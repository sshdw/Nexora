//! Strict SSRF validation and DNS-pinned HTTP fetching for web tools.
//!
//! Security policy (strictly fail-closed; stronger than MCP's loopback-allowing
//! policy):
//! - Only `https://` and `http://` schemes are accepted.
//! - Loopback (`localhost`, `127.0.0.0/8`, `::1`), RFC1918 private networks,
//!   link-local / cloud metadata (`169.254.0.0/16`, `fe80::/10`), CGNAT
//!   (`100.64.0.0/10`), ULA (`fc00::/7`), NAT64 (`64:ff9b::/96`,
//!   `64:ff9b:1::/48`), IPv4-mapped/compatible/translated IPv6, 6to4 private,
//!   Teredo, discard, unspecified, multicast, dotless, and `.local`/`.internal`
//!   hosts are unconditionally rejected.
//! - Embedded credentials (`user:pass@host`), fragments, control characters,
//!   oversized URLs, secret-like tokens, and backslash parser differentials
//!   (`http://127.0.0.1\@1.1.1.1`) are rejected before parsing or connecting.
//! - DNS resolution reuses [`verify_resolved_host`] + [`is_public_routable_ip`]
//!   from `application::mcp::url_safety`, applies additional web-only IPv6/NAT64
//!   guards, and pins the verified socket addresses onto the HTTP client with
//!   redirects strictly disabled (`reqwest::redirect::Policy::none()`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use url::{Host, Url};

use crate::application::agent::control::CancellationToken;
use crate::application::agent::injection::contains_secret;
use crate::application::mcp::url_safety::{
    is_public_routable_ip, verify_resolved_host, UrlRejectReason,
};

use super::{
    WebError, DEFAULT_CONNECT_TIMEOUT, DEFAULT_INTER_CHUNK_TIMEOUT, DEFAULT_TOTAL_TIMEOUT,
    MAX_RAW_RESPONSE_BYTES, MAX_WEB_URL_LEN, WEB_FETCH_USER_AGENT,
};

/// Polling cadence while awaiting streamed chunks so [`CancellationToken`] and
/// inter-chunk / total timeouts fire promptly without busy-spinning.
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Bounded channel depth between the async fetch task and the synchronous reader
/// to enforce backpressure and cap in-flight memory.
const STREAM_CHANNEL_CAPACITY: usize = 4;

/// A syntactically and SSRF-validated public web URL ready for DNS-pinned fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedFetchUrl {
    url: Url,
    host: String,
}

impl ValidatedFetchUrl {
    /// Underlying parsed [`Url`].
    #[must_use]
    pub(crate) fn as_url(&self) -> &Url {
        &self.url
    }

    /// Validated destination host string (lowercase domain or IP literal).
    #[must_use]
    pub(crate) fn host(&self) -> &str {
        &self.host
    }
}

/// Policy knobs for [`fetch_with_pinned_dns`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PinnedFetchOptions {
    pub(crate) connect_timeout: Duration,
    pub(crate) total_timeout: Duration,
    pub(crate) inter_chunk_timeout: Duration,
    pub(crate) max_raw_bytes: usize,
    pub(crate) allowed_mime_check: fn(&str) -> bool,
}

impl Default for PinnedFetchOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            inter_chunk_timeout: DEFAULT_INTER_CHUNK_TIMEOUT,
            max_raw_bytes: MAX_RAW_RESPONSE_BYTES,
            allowed_mime_check: super::fetch::is_allowed_mime_type,
        }
    }
}

/// Raw HTTP response collected by [`fetch_with_pinned_dns`] after passing status,
/// redirect, MIME, `Content-Length`, and streamed byte-cap checks.
#[derive(Debug, Clone)]
pub(crate) struct RawHttpResponse {
    pub(crate) host: String,
    pub(crate) status: u16,
    pub(crate) mime_type: String,
    pub(crate) content_encoding: Option<String>,
    pub(crate) raw_body: Vec<u8>,
}

/// True when `ip` is a public, globally routable web IP address.
///
/// Reuses [`is_public_routable_ip`] from `application::mcp::url_safety` and
/// additionally rejects IPv6 NAT64 (`64:ff9b::/96`, `64:ff9b:1::/48`),
/// deprecated IPv4-compatible (`::a.b.c.d`), IPv4-translated (`::ffff:0:0/96`),
/// 6to4 (`2002::/16`) wrapping a non-public IPv4, Teredo (`2001:0000::/32`),
/// and discard-only (`100::/64`) ranges.
#[must_use]
pub(crate) fn is_public_web_ip(ip: IpAddr) -> bool {
    if !is_public_routable_ip(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(_) => true,
        IpAddr::V6(v6) => !is_blocked_special_ipv6(v6),
    }
}

fn is_blocked_special_ipv6(ip: Ipv6Addr) -> bool {
    let segs = ip.segments();
    // NAT64 well-known prefix (RFC 6052: 64:ff9b::/96)
    if segs[0] == 0x0064
        && segs[1] == 0xff9b
        && segs[2] == 0
        && segs[3] == 0
        && segs[4] == 0
        && segs[5] == 0
    {
        return true;
    }
    // NAT64 local-use prefix (RFC 8215: 64:ff9b:1::/48)
    if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2] == 0x0001 {
        return true;
    }
    // Deprecated IPv4-compatible IPv6 (::a.b.c.d, where first 96 bits are 0)
    if segs[0] == 0 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 && segs[4] == 0 && segs[5] == 0
    {
        return true;
    }
    // IPv4-translated IPv6 (RFC 2765: ::ffff:0:0/96)
    if segs[0] == 0
        && segs[1] == 0
        && segs[2] == 0
        && segs[3] == 0
        && segs[4] == 0xffff
        && segs[5] == 0
    {
        return true;
    }
    // 6to4 (RFC 3056: 2002::/16) — extract embedded IPv4 and verify public
    if segs[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segs[1] >> 8) as u8,
            (segs[1] & 0xff) as u8,
            (segs[2] >> 8) as u8,
            (segs[2] & 0xff) as u8,
        );
        if !is_public_routable_ip(IpAddr::V4(embedded)) {
            return true;
        }
    }
    // Teredo tunneling (RFC 4380: 2001:0000::/32)
    if segs[0] == 0x2001 && segs[1] == 0x0000 {
        return true;
    }
    // Discard-only address block (RFC 6666: 100::/64)
    if segs[0] == 0x0100 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 {
        return true;
    }
    false
}

fn is_blocked_web_domain(domain: &str) -> bool {
    let trimmed = domain.trim_end_matches('.');
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Reject single-label / dotless hostnames (e.g. `localhost`, `metadata`, `db`).
    if !lower.contains('.') {
        return true;
    }
    if lower == "metadata.google.internal" || lower == "metadata.goog" {
        return true;
    }
    matches!(
        lower.rsplit('.').next(),
        Some("localhost" | "local" | "internal" | "arpa" | "localdomain" | "lan" | "corp")
    )
}

/// Check the raw authority slice (between `://` and the first `/`, `?`, or `#`)
/// for `@` so userinfo smuggling and parser differentials are rejected before
/// URL normalization.
fn raw_authority_contains_at(raw: &str) -> bool {
    let Some(scheme_sep) = raw.find("://") else {
        return false;
    };
    let rest = &raw[scheme_sep + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest[..authority_end].contains('@')
}

/// Validate a web fetch URL strictly (syntactic, parser-differential, scheme,
/// secret, and SSRF checks).
///
/// Unlike MCP URL validation, loopback (`localhost`, `127.0.0.1`, `[::1]`) is
/// **never** permitted here.
///
/// # Errors
/// Returns a secret-free [`WebError`] if any validation or SSRF invariant fails.
pub(crate) fn validate_fetch_url(raw: &str) -> Result<ValidatedFetchUrl, WebError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_WEB_URL_LEN {
        return Err(WebError::InvalidUrl(
            "web URL must be non-empty and at most 2048 bytes",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(WebError::InvalidUrl(
            "web URL contains forbidden control characters",
        ));
    }
    // Reject backslashes anywhere in the URL to eliminate WHATWG-vs-RFC3986
    // authority parser differentials such as `http://127.0.0.1\@1.1.1.1`.
    if trimmed.contains('\\') {
        return Err(WebError::InvalidUrl(
            "web URL contains forbidden backslash characters",
        ));
    }
    if raw_authority_contains_at(trimmed) {
        return Err(WebError::InvalidUrl(
            "web URL must not include embedded credentials",
        ));
    }
    if contains_secret(trimmed) {
        return Err(WebError::InvalidUrl(
            "web URL appears to contain embedded secret material",
        ));
    }

    let parsed = Url::parse(trimmed)
        .map_err(|_| WebError::InvalidUrl("web URL is not a valid absolute URL"))?;

    match parsed.scheme() {
        "https" | "http" => {}
        _ => {
            return Err(WebError::InvalidUrl("web URL must use https:// or http://"));
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(WebError::InvalidUrl(
            "web URL must not include embedded credentials",
        ));
    }
    if parsed.fragment().is_some() {
        return Err(WebError::InvalidUrl("web URL must not include a fragment"));
    }

    let Some(host) = parsed.host() else {
        return Err(WebError::InvalidUrl("web URL must specify a host"));
    };

    let host_label = match host {
        Host::Ipv4(ip) => {
            if !is_public_web_ip(IpAddr::V4(ip)) {
                return Err(WebError::BlockedHost);
            }
            ip.to_string()
        }
        Host::Ipv6(ip) => {
            if !is_public_web_ip(IpAddr::V6(ip)) {
                return Err(WebError::BlockedHost);
            }
            ip.to_string()
        }
        Host::Domain(domain) => {
            if is_blocked_web_domain(domain) {
                return Err(WebError::BlockedHost);
            }
            // Belt-and-suspenders: if a domain parses as a literal IP, check it.
            if let Ok(ip) = domain.parse::<IpAddr>() {
                if !is_public_web_ip(ip) {
                    return Err(WebError::BlockedHost);
                }
            }
            domain.to_ascii_lowercase()
        }
    };

    Ok(ValidatedFetchUrl {
        url: parsed,
        host: host_label,
    })
}

/// Resolve a validated public web URL via DNS and verify every returned socket
/// address obeys the strict web SSRF policy (preventing DNS rebinding to
/// loopback, private, metadata, or NAT64 addresses).
///
/// Reuses [`verify_resolved_host`] from `application::mcp::url_safety`, while
/// strictly blocking localhost beforehand and applying [`is_public_web_ip`] to
/// every resolved address.
///
/// # Errors
/// Returns [`WebError::BlockedHost`] or [`WebError::DnsResolutionFailed`].
pub(crate) fn resolve_and_verify_web_host(url: &Url) -> Result<Vec<SocketAddr>, WebError> {
    match url.host() {
        None => return Err(WebError::InvalidUrl("web URL must specify a host")),
        Some(Host::Domain(domain)) if is_blocked_web_domain(domain) => {
            return Err(WebError::BlockedHost);
        }
        Some(Host::Ipv4(ip)) if !is_public_web_ip(IpAddr::V4(ip)) => {
            return Err(WebError::BlockedHost);
        }
        Some(Host::Ipv6(ip)) if !is_public_web_ip(IpAddr::V6(ip)) => {
            return Err(WebError::BlockedHost);
        }
        Some(_) => {}
    }

    let addrs = verify_resolved_host(url).map_err(map_mcp_reject_reason)?;
    if addrs.is_empty() {
        return Err(WebError::DnsResolutionFailed);
    }
    for addr in &addrs {
        if !is_public_web_ip(addr.ip()) {
            return Err(WebError::BlockedHost);
        }
    }
    Ok(addrs)
}

fn map_mcp_reject_reason(reason: UrlRejectReason) -> WebError {
    match reason {
        UrlRejectReason::PrivateOrBlockedHost => WebError::BlockedHost,
        UrlRejectReason::DnsResolutionFailed => WebError::DnsResolutionFailed,
        UrlRejectReason::EmptyOrTooLong
        | UrlRejectReason::ControlChars
        | UrlRejectReason::SecretLike
        | UrlRejectReason::InvalidSyntax
        | UrlRejectReason::UserinfoForbidden
        | UrlRejectReason::FragmentForbidden
        | UrlRejectReason::SchemeNotAllowed
        | UrlRejectReason::HostMissing => WebError::InvalidUrl("web URL failed safety validation"),
    }
}

/// Build a hardened blocking HTTP client pinned to a single verified DNS
/// resolution (strict web SSRF verification with no loopback or NAT64,
/// redirects disabled, bounded timeouts, and connect-time DNS pinning).
///
/// # Errors
/// Returns a secret-free [`WebError`] if DNS verification fails or the client
/// cannot be constructed.
pub(crate) fn build_pinned_blocking_client(
    validated: &ValidatedFetchUrl,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, WebError> {
    let url = validated.as_url();
    let resolved = resolve_and_verify_web_host(url)?;
    let mut builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(timeout)
        .user_agent(WEB_FETCH_USER_AGENT);

    if let Some(Host::Domain(domain)) = url.host() {
        builder = builder.resolve_to_addrs(domain, &resolved);
    }

    builder
        .build()
        .map_err(|_| WebError::Transport("web HTTP client could not be initialized"))
}

enum FetchStreamMsg {
    Headers {
        status: u16,
        content_type: Option<String>,
        content_encoding: Option<String>,
        content_length: Option<u64>,
    },
    Chunk(Vec<u8>),
    Eof,
    TransportError(WebError),
}

fn classify_reqwest_error(err: &reqwest::Error) -> WebError {
    if err.is_timeout() {
        return WebError::Timeout;
    }
    if err.is_redirect() {
        return WebError::RedirectDisallowed;
    }
    if err.is_connect() {
        return WebError::Transport("web fetch could not connect to the remote host");
    }
    WebError::Transport("web fetch failed due to a network error")
}

/// Perform a DNS-pinned, redirect-disabled, bounded HTTP GET for `validated`.
///
/// Enforces:
/// - DNS resolution + strict public-IP verification + connect-time DNS pinning
/// - Redirects strictly disabled (`Policy::none()`)
/// - Connect timeout (`5s`), total timeout (`20s`), and inter-chunk idle
///   timeout (`10s`)
/// - Cooperative [`CancellationToken`] polling with immediate task abort
/// - MIME allowlist check and `Content-Length` precheck before body streaming
/// - Hard streamed byte cap (`1 MiB`) even when `Content-Length` lies or is
///   omitted (`Transfer-Encoding: chunked`)
///
/// # Errors
/// Returns a secret-free [`WebError`].
pub(crate) fn fetch_with_pinned_dns(
    validated: &ValidatedFetchUrl,
    options: &PinnedFetchOptions,
    token: &CancellationToken,
) -> Result<RawHttpResponse, WebError> {
    if token.is_cancelled() {
        return Err(WebError::Cancelled);
    }
    let resolved = resolve_and_verify_web_host(validated.as_url())?;
    execute_pinned_request(
        validated.as_url(),
        validated.host(),
        &resolved,
        options,
        token,
    )
}

fn execute_pinned_request(
    url: &Url,
    host_label: &str,
    resolved_addrs: &[SocketAddr],
    options: &PinnedFetchOptions,
    token: &CancellationToken,
) -> Result<RawHttpResponse, WebError> {
    if token.is_cancelled() {
        return Err(WebError::Cancelled);
    }

    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(options.connect_timeout)
        .timeout(options.total_timeout)
        .user_agent(WEB_FETCH_USER_AGENT);

    if let Some(Host::Domain(domain)) = url.host() {
        builder = builder.resolve_to_addrs(domain, resolved_addrs);
    }

    let client = builder
        .build()
        .map_err(|_| WebError::Transport("web HTTP client could not be initialized"))?;

    let target_url = url.clone();
    let (tx, rx) = mpsc::sync_channel::<FetchStreamMsg>(STREAM_CHANNEL_CAPACITY);
    let handle = tauri::async_runtime::spawn(async move {
        stream_fetch_task(&client, target_url, &tx).await;
    });

    let result = collect_streamed_response(host_label, options, token, &rx);
    if result.is_err() {
        handle.abort();
    }
    result
}

async fn stream_fetch_task(
    client: &reqwest::Client,
    url: Url,
    tx: &mpsc::SyncSender<FetchStreamMsg>,
) {
    let response_result = client
        .get(url)
        .header(
            "Accept",
            "text/html, application/xhtml+xml, text/plain;q=0.9",
        )
        .header("Accept-Encoding", "gzip, deflate, identity")
        .send()
        .await;

    let mut response = match response_result {
        Ok(resp) => resp,
        Err(err) => {
            let _ = tx.send(FetchStreamMsg::TransportError(classify_reqwest_error(&err)));
            return;
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let content_encoding = response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let content_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| response.content_length());

    if tx
        .send(FetchStreamMsg::Headers {
            status,
            content_type,
            content_encoding,
            content_length,
        })
        .is_err()
    {
        return;
    }

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if !chunk.is_empty() && tx.send(FetchStreamMsg::Chunk(chunk.to_vec())).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = tx.send(FetchStreamMsg::Eof);
                return;
            }
            Err(err) => {
                let _ = tx.send(FetchStreamMsg::TransportError(classify_reqwest_error(&err)));
                return;
            }
        }
    }
}

fn collect_streamed_response(
    host_label: &str,
    options: &PinnedFetchOptions,
    token: &CancellationToken,
    rx: &mpsc::Receiver<FetchStreamMsg>,
) -> Result<RawHttpResponse, WebError> {
    let started_at = Instant::now();
    let mut last_activity_at = started_at;
    let mut headers_received = false;
    let mut status: u16 = 0;
    let mut mime_type = String::new();
    let mut content_encoding: Option<String> = None;
    let mut raw_body: Vec<u8> = Vec::new();

    loop {
        if token.is_cancelled() {
            return Err(WebError::Cancelled);
        }
        if started_at.elapsed() >= options.total_timeout {
            return Err(WebError::Timeout);
        }
        if headers_received && last_activity_at.elapsed() >= options.inter_chunk_timeout {
            return Err(WebError::Timeout);
        }

        match rx.recv_timeout(STREAM_POLL_INTERVAL) {
            Ok(FetchStreamMsg::Headers {
                status: resp_status,
                content_type,
                content_encoding: resp_encoding,
                content_length,
            }) => {
                if (300..400).contains(&resp_status) {
                    return Err(WebError::RedirectDisallowed);
                }
                if !(200..300).contains(&resp_status) {
                    return Err(WebError::HttpStatus {
                        status: resp_status,
                    });
                }

                let normalized_mime = content_type
                    .as_deref()
                    .and_then(super::fetch::extract_mime_type)
                    .ok_or(WebError::UnsupportedMediaType)?;
                if !(options.allowed_mime_check)(&normalized_mime) {
                    return Err(WebError::UnsupportedMediaType);
                }

                if let Some(advertised_len) = content_length {
                    let max_u64 = u64::try_from(options.max_raw_bytes).unwrap_or(u64::MAX);
                    if advertised_len > max_u64 {
                        return Err(WebError::ResponseTooLarge);
                    }
                }

                headers_received = true;
                last_activity_at = Instant::now();
                status = resp_status;
                mime_type = normalized_mime;
                content_encoding = resp_encoding;
            }
            Ok(FetchStreamMsg::Chunk(chunk)) => {
                last_activity_at = Instant::now();
                if raw_body.len().saturating_add(chunk.len()) > options.max_raw_bytes {
                    return Err(WebError::ResponseTooLarge);
                }
                raw_body.extend_from_slice(&chunk);
            }
            Ok(FetchStreamMsg::Eof) => {
                return Ok(RawHttpResponse {
                    host: host_label.to_owned(),
                    status,
                    mime_type,
                    content_encoding,
                    raw_body,
                });
            }
            Ok(FetchStreamMsg::TransportError(err)) => return Err(err),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if token.is_cancelled() {
                    return Err(WebError::Cancelled);
                }
                return Err(WebError::Transport(
                    "web fetch stream terminated unexpectedly",
                ));
            }
        }
    }
}

/// Test-only helper that exercises the exact `execute_pinned_request` pipeline
/// against an explicit loopback mock server (`127.0.0.1` / `::1`) without ever
/// weakening production [`validate_fetch_url`] or [`resolve_and_verify_web_host`].
#[cfg(test)]
pub(crate) fn fetch_loopback_for_test(
    addr: SocketAddr,
    path: &str,
    options: &PinnedFetchOptions,
    token: &CancellationToken,
) -> Result<RawHttpResponse, WebError> {
    assert!(
        addr.ip().is_loopback(),
        "fetch_loopback_for_test may only target loopback addresses"
    );
    let raw = format!("http://{addr}{path}");
    let url = Url::parse(&raw).expect("valid loopback test URL");
    execute_pinned_request(&url, &addr.ip().to_string(), &[addr], options, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_https_and_http_urls() {
        let ok_cases = [
            ("https://example.com/docs/page?q=1", "example.com"),
            ("http://example.org/", "example.org"),
            ("https://93.184.216.34/index.html", "93.184.216.34"),
            ("http://1.1.1.1/status", "1.1.1.1"),
            (
                "https://[2606:2800:220:1:248:1893:25c8:1946]/",
                "2606:2800:220:1:248:1893:25c8:1946",
            ),
        ];
        for (raw, expected_host) in ok_cases {
            let validated = validate_fetch_url(raw)
                .unwrap_or_else(|err| panic!("expected {raw} to be valid, got {err:?}"));
            assert_eq!(validated.host(), expected_host);
        }
    }

    #[test]
    fn rejects_loopback_private_link_local_cgnat_nat64_and_metadata_hosts() {
        let blocked = [
            "http://127.0.0.1/",
            "https://127.0.0.1:8443/secret",
            "http://127.0.0.2/admin",
            "http://localhost:8080/",
            "https://localhost/",
            "https://sub.localhost/api",
            "http://[::1]:8080/",
            "https://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "https://169.254.169.254/computeMetadata/v1",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://0.0.0.0/",
            "http://100.64.0.1/",
            "http://100.127.255.254/",
            // Decimal, octal, hex, shorthand IPv4 literals (normalized by WHATWG URL parser)
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://0x7f000001/",
            "http://127.1/",
            "http://2852039166/", // 169.254.169.254 in decimal
            // IPv4-mapped, IPv4-compatible, IPv4-translated, NAT64, 6to4, Teredo, ULA, link-local
            "https://[::ffff:127.0.0.1]/",
            "https://[::ffff:169.254.169.254]/",
            "https://[::127.0.0.1]/",
            "https://[::ffff:0:127.0.0.1]/",
            "https://[64:ff9b::127.0.0.1]/",
            "https://[64:ff9b::169.254.169.254]/",
            "https://[64:ff9b::1.1.1.1]/",
            "https://[64:ff9b:1::1]/",
            "https://[2002:7f00:0001::1]/",
            "https://[2002:a9fe:a9fe::1]/",
            "https://[2001:0000:4136:e378:8000:63bf:3fff:fdd2]/",
            "https://[100::1]/",
            "https://[fe80::1]/",
            "https://[fc00::1]/",
            "https://[fd12:3456:789a::1]/",
            // Metadata / internal / dotless domains
            "https://metadata.google.internal/computeMetadata/v1",
            "https://metadata.goog/",
            "https://service.internal/",
            "https://printer.local/",
            "http://metadata/",
            "http://intranet/",
        ];
        for raw in blocked {
            let err = validate_fetch_url(raw)
                .expect_err(&format!("expected SSRF host to be blocked: {raw}"));
            assert_eq!(
                err,
                WebError::BlockedHost,
                "expected BlockedHost for {raw}, got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_userinfo_fragments_control_chars_backslash_differentials_and_secrets() {
        let oversized = format!("https://example.com/{}", "a".repeat(MAX_WEB_URL_LEN));
        let invalid = [
            "",
            "   ",
            oversized.as_str(),
            "ftp://example.com/file",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<h1>hi</h1>",
            "https://user:pass@example.com/secret",
            "https://user@example.com/",
            "https://example.com/page#section",
            "https://example.com/line\nbreak",
            "https://example.com/tab\tchar",
            "https://example.com/null\0byte",
            // Backslash / @ parser differentials
            r"http://127.0.0.1\@1.1.1.1/",
            r"https://1.1.1.1\@127.0.0.1/",
            r"https://example.com\foo",
            // Embedded secret material
            "https://example.com/?api_key=sk-live-secret-1234567890abcdef",
        ];
        for raw in invalid {
            let err =
                validate_fetch_url(raw).expect_err(&format!("expected URL to be rejected: {raw}"));
            assert!(
                matches!(err, WebError::InvalidUrl(_)),
                "expected InvalidUrl for {raw}, got {err:?}"
            );
            let msg = err.to_string();
            assert!(!msg.contains("127.0.0.1"));
            assert!(!msg.contains("sk-live"));
            assert!(!msg.contains("user:pass"));
        }
    }

    #[test]
    fn resolve_and_verify_blocks_loopback_even_if_called_directly() {
        let loopback = Url::parse("http://127.0.0.1:8080/").expect("parse");
        assert_eq!(
            resolve_and_verify_web_host(&loopback),
            Err(WebError::BlockedHost)
        );
        let localhost = Url::parse("http://localhost:8080/").expect("parse");
        assert_eq!(
            resolve_and_verify_web_host(&localhost),
            Err(WebError::BlockedHost)
        );
        let nat64 = Url::parse("https://[64:ff9b::1.1.1.1]/").expect("parse");
        assert_eq!(
            resolve_and_verify_web_host(&nat64),
            Err(WebError::BlockedHost)
        );
    }

    #[test]
    fn is_public_web_ip_blocks_nat64_and_allows_public_v4_and_v6() {
        assert!(is_public_web_ip(IpAddr::V4(Ipv4Addr::new(
            93, 184, 216, 34
        ))));
        assert!(is_public_web_ip(IpAddr::V6(
            "2606:2800:220:1:248:1893:25c8:1946".parse().expect("ipv6")
        )));
        assert!(!is_public_web_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_public_web_ip(IpAddr::V6(
            "64:ff9b::93.184.216.34".parse().expect("nat64")
        )));
        assert!(!is_public_web_ip(IpAddr::V6(
            "64:ff9b:1::1".parse().expect("nat64 local")
        )));
        assert!(!is_public_web_ip(IpAddr::V6(
            "::127.0.0.1".parse().expect("v4compat")
        )));
    }

    #[test]
    fn build_pinned_blocking_client_succeeds_for_public_ip_literal() {
        let validated = validate_fetch_url("https://93.184.216.34/index.html").expect("valid");
        let client = build_pinned_blocking_client(&validated, Duration::from_secs(5));
        assert!(client.is_ok());
    }
}
