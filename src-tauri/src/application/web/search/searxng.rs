//! `SearXNG` self-hosted search backend (Stage 12 PR 6).
//!
//! Endpoint validation invariant:
//! - Base URL must be either `https://` targeting a public routable host, OR
//!   `http://` targeting an explicit loopback host (`localhost`, `127.0.0.1`, `[::1]`).
//! - Never allows plaintext `http://` to non-loopback hosts, embedded credentials,
//!   fragments, control chars, backslashes, secret-like tokens, or non-loopback
//!   private/RFC1918/metadata/NAT64 addresses.
//! - Every returned search result URL is still validated through strict public-only
//!   [`validate_fetch_url`](crate::application::web::safety::validate_fetch_url).

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use serde::Deserialize;
use url::{Host, Url};

use crate::application::agent::control::CancellationToken;
use crate::application::agent::injection::contains_secret;
use crate::application::web::safety::{is_public_web_ip, resolve_and_verify_web_host};
use crate::application::web::{MAX_WEB_URL_LEN, WEB_FETCH_USER_AGENT};

use super::{
    clamp_max_results, normalize_and_filter_results, percent_encode, send_pinned_search_request,
    SearchBackend, SearchError, SearchHttpMethod, SearchHttpRequest, SearchHttpResponse, WebResult,
    DEFAULT_SEARCH_TIMEOUT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedSearxngUrl {
    pub(crate) url: Url,
    pub(crate) host: String,
    pub(crate) is_loopback: bool,
}

fn raw_authority(raw: &str) -> Option<&str> {
    let sep = raw.find("://")?;
    let rest = &raw[sep + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn raw_authority_contains_at(raw: &str) -> bool {
    raw_authority(raw).is_some_and(|authority| authority.contains('@'))
}

fn raw_authority_has_empty_port(raw: &str) -> bool {
    let Some(authority) = raw_authority(raw) else {
        return false;
    };
    if let Some(after_bracket) = authority.strip_prefix('[') {
        after_bracket
            .split_once(']')
            .is_some_and(|(_, port_part)| port_part == ":")
    } else {
        authority.ends_with(':')
    }
}

/// Validate a user-configured `SearXNG` base URL: HTTPS (public host) or
/// explicit loopback HTTP/HTTPS (`localhost`, `127.0.0.1`, `[::1]`).
///
/// # Errors
/// Returns a secret-free [`SearchError::InvalidConfig`].
#[allow(clippy::too_many_lines)] // Keep all URL/scheme/host/SSRF checks reviewable in one function.
pub(crate) fn validate_searxng_base_url(raw: &str) -> Result<ValidatedSearxngUrl, SearchError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_WEB_URL_LEN {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL must be non-empty and at most 2048 bytes",
        ));
    }
    if trimmed.chars().any(char::is_control) || trimmed.contains('\\') {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL contains forbidden characters",
        ));
    }
    if raw_authority_contains_at(trimmed) {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL must not include embedded credentials",
        ));
    }
    if raw_authority_has_empty_port(trimmed) {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL port must be a valid TCP port number",
        ));
    }
    if contains_secret(trimmed) {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL appears to contain embedded secret material",
        ));
    }

    let parsed = Url::parse(trimmed)
        .map_err(|_| SearchError::InvalidConfig("SearXNG URL is not a valid absolute URL"))?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL must not include embedded credentials",
        ));
    }
    if parsed.fragment().is_some() || parsed.query().is_some() {
        return Err(SearchError::InvalidConfig(
            "SearXNG base URL must not include a query or fragment",
        ));
    }

    let Some(host) = parsed.host() else {
        return Err(SearchError::InvalidConfig(
            "SearXNG URL must specify a host",
        ));
    };

    let (host_label, is_loopback) = match host {
        Host::Ipv4(ip) => {
            if ip.is_loopback() {
                (ip.to_string(), true)
            } else if is_public_web_ip(IpAddr::V4(ip)) {
                (ip.to_string(), false)
            } else {
                return Err(SearchError::InvalidConfig(
                    "SearXNG URL host must be HTTPS public or explicit loopback",
                ));
            }
        }
        Host::Ipv6(ip) => {
            if ip.is_loopback() {
                (ip.to_string(), true)
            } else if is_public_web_ip(IpAddr::V6(ip)) {
                (ip.to_string(), false)
            } else {
                return Err(SearchError::InvalidConfig(
                    "SearXNG URL host must be HTTPS public or explicit loopback",
                ));
            }
        }
        Host::Domain(domain) => {
            let lower = domain.trim_end_matches('.').to_ascii_lowercase();
            if lower == "localhost" {
                (lower, true)
            } else if !lower.contains('.')
                || matches!(
                    lower.rsplit('.').next(),
                    Some(
                        "localhost"
                            | "local"
                            | "internal"
                            | "arpa"
                            | "localdomain"
                            | "lan"
                            | "corp"
                    )
                )
                || lower == "metadata.google.internal"
                || lower == "metadata.goog"
            {
                return Err(SearchError::InvalidConfig(
                    "SearXNG URL host must be HTTPS public or explicit loopback",
                ));
            } else {
                (lower, false)
            }
        }
    };

    match parsed.scheme() {
        "https" => {}
        "http" if is_loopback => {}
        _ => {
            return Err(SearchError::InvalidConfig(
                "SearXNG URL must use https:// (or http:// only for localhost/loopback)",
            ));
        }
    }

    Ok(ValidatedSearxngUrl {
        url: parsed,
        host: host_label,
        is_loopback,
    })
}

fn resolve_searxng_addrs(validated: &ValidatedSearxngUrl) -> Result<Vec<SocketAddr>, SearchError> {
    if validated.is_loopback {
        let port = validated.url.port_or_known_default().unwrap_or(80);
        let addrs: Vec<SocketAddr> = (validated.host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| SearchError::Transport("SearXNG loopback host could not be resolved"))?
            .filter(|addr| addr.ip().is_loopback())
            .collect();
        if addrs.is_empty() {
            return Err(SearchError::InvalidConfig(
                "SearXNG loopback host did not resolve to a loopback address",
            ));
        }
        Ok(addrs)
    } else {
        resolve_and_verify_web_host(&validated.url).map_err(SearchError::from)
    }
}

pub(crate) struct SearxngBackend {
    validated: ValidatedSearxngUrl,
}

impl SearxngBackend {
    /// Create a `SearXNG` backend after validating its base URL.
    ///
    /// # Errors
    /// Returns [`SearchError::InvalidConfig`] when the base URL is invalid.
    pub(crate) fn new(base_url: &str) -> Result<Self, SearchError> {
        let validated = validate_searxng_base_url(base_url)?;
        Ok(Self { validated })
    }

    fn build_search_url(&self, query: &str) -> Result<Url, SearchError> {
        let base = self.validated.url.as_str().trim_end_matches('/');
        let full = format!("{base}/search?q={}&format=json", percent_encode(query));
        Url::parse(&full)
            .map_err(|_| SearchError::InvalidConfig("SearXNG search URL could not be constructed"))
    }
}

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngItem>,
}

#[derive(Deserialize)]
struct SearxngItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

impl SearchBackend for SearxngBackend {
    fn name(&self) -> &'static str {
        "searxng"
    }

    fn search(
        &self,
        query: &str,
        max_results: usize,
        token: &CancellationToken,
    ) -> Result<Vec<WebResult>, SearchError> {
        let cap = clamp_max_results(max_results);
        let target_url = self.build_search_url(query)?;
        let addrs = resolve_searxng_addrs(&self.validated)?;
        let headers = [("Accept", "application/json")];

        let resp = send_pinned_search_request(
            &SearchHttpRequest {
                url: &target_url,
                resolved_addrs: &addrs,
                method: SearchHttpMethod::Get,
                user_agent: WEB_FETCH_USER_AGENT,
                headers: &headers,
                body: None,
                timeout: DEFAULT_SEARCH_TIMEOUT,
            },
            token,
        )?;
        parse_searxng_response(&resp, cap)
    }
}

pub(crate) fn parse_searxng_response(
    resp: &SearchHttpResponse,
    max_results: usize,
) -> Result<Vec<WebResult>, SearchError> {
    if resp.status == 429 {
        return Err(SearchError::RateLimitedOrBlocked(
            "SearXNG instance rate-limited the search request",
        ));
    }
    if !(200..300).contains(&resp.status) {
        return Err(SearchError::HttpStatus {
            status: resp.status,
        });
    }
    let parsed: SearxngResponse = serde_json::from_str(&resp.body)
        .map_err(|_| SearchError::ParseFailed("SearXNG returned invalid JSON"))?;
    let raw = parsed
        .results
        .into_iter()
        .map(|item| WebResult {
            title: item.title,
            url: item.url,
            snippet: item.content,
        })
        .collect();
    Ok(normalize_and_filter_results(raw, max_results))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn validates_https_public_and_loopback_http_only() {
        assert!(validate_searxng_base_url("https://search.example.org").is_ok());
        assert!(validate_searxng_base_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_searxng_base_url("http://localhost:8888").is_ok());
        assert!(validate_searxng_base_url("http://[::1]:8080").is_ok());
        assert!(validate_searxng_base_url("https://localhost:8443").is_ok());

        for bad in [
            "",
            "http://search.example.org",
            "https://192.168.1.10",
            "http://192.168.1.10:8080",
            "https://169.254.169.254",
            "https://user:pass@search.example.org",
            "https://search.example.org?foo=bar",
            "https://search.example.org#frag",
            "https://internal.local",
            "ftp://localhost:8080",
            "https://example.com:/secret",
            "https://example.com:",
            "https://[::1]:",
            "http://localhost:/",
        ] {
            assert!(
                validate_searxng_base_url(bad).is_err(),
                "SearXNG URL {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn searxng_loopback_live_roundtrip_with_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock searxng");
        let addr = listener.local_addr().expect("addr");
        let body = r#"{"results":[{"title":"Docs","url":"https://docs.rs/","content":"Crate docs"},{"title":"Private","url":"http://127.0.0.1/secret","content":"blocked"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        let backend = SearxngBackend::new(&format!("http://{addr}")).expect("valid loopback");
        let results = backend
            .search("rust serde", 5, &CancellationToken::new())
            .expect("searxng loopback search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Docs");
        assert_eq!(results[0].url, "https://docs.rs/");
        assert_eq!(results[0].snippet, "Crate docs");
    }
}
