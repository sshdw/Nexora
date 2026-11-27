//! Web search backends: keyless `DuckDuckGo` default, optional keyed
//! Tavily / Brave / Exa (OS keyring only), and optional `SearXNG`
//! (HTTPS-or-loopback validated) (Stage 12 PR 6).
//!
//! Hard invariants:
//! - Every returned `WebResult.url` is re-validated through
//!   [`crate::application::web::safety::validate_fetch_url`] so search engines
//!   can never smuggle `javascript:`, `file:`, loopback, or private-network URLs.
//! - API keys for Tavily, Brave, and Exa are read exclusively from
//!   [`CredentialStore`](crate::infrastructure::providers::credentials::CredentialStore)
//!   and never appear in errors, logs, or IPC payloads.
//! - All outbound HTTP requests pin verified DNS, disable proxies (`.no_proxy()`),
//!   disable redirects (`Policy::none()`), enforce connect (`5s`) and total (`15s`)
//!   timeouts, bound raw/decompressed response sizes, and honor [`CancellationToken`].

pub(crate) mod brave;
pub(crate) mod ddg;
pub(crate) mod exa;
pub(crate) mod searxng;
pub(crate) mod tavily;

use std::collections::HashSet;
use std::fmt::{self, Write as _};
use std::net::SocketAddr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH};
use url::{Host, Url};

use crate::application::agent::control::CancellationToken;
use crate::infrastructure::providers::credentials::{CredentialError, CredentialStore};

use super::fetch::decompress_body;
use super::safety::{resolve_and_verify_web_host, validate_fetch_url};
use super::{WebError, MAX_DECOMPRESSED_BYTES, MAX_RAW_RESPONSE_BYTES};

/// Default number of search results returned per call.
pub(crate) const DEFAULT_SEARCH_RESULTS: usize = 5;

/// Minimum number of search results configurable/requested.
pub(crate) const MIN_SEARCH_RESULTS: usize = 1;

/// Maximum number of search results returned per call (research §3.1: 5–8 cap).
pub(crate) const MAX_SEARCH_RESULTS: usize = 8;

/// Maximum search query length in bytes.
pub(crate) const MAX_SEARCH_QUERY_LEN: usize = 500;

/// Maximum title length in Unicode scalar values after normalization.
const MAX_RESULT_TITLE_CHARS: usize = 200;

/// Maximum snippet length in Unicode scalar values after normalization.
const MAX_RESULT_SNIPPET_CHARS: usize = 500;

/// Default TCP/TLS connect timeout for search backends (research §7.3).
pub(crate) const DEFAULT_SEARCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default total request timeout for search backends (research §7.3).
pub(crate) const DEFAULT_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Polling cadence while streaming search responses.
const SEARCH_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Bounded channel capacity for streamed search response chunks.
const SEARCH_CHANNEL_CAPACITY: usize = 4;

/// Fixed observation string when a search yields zero valid results.
pub(crate) const NO_RESULTS_TEXT: &str = "No results";

/// Keyring provider entry names (under service `"nexora"`), kept distinct from
/// AI model provider names.
pub(crate) const WEB_SEARCH_KEYRING_TAVILY: &str = "web_search_tavily";
pub(crate) const WEB_SEARCH_KEYRING_BRAVE: &str = "web_search_brave";
pub(crate) const WEB_SEARCH_KEYRING_EXA: &str = "web_search_exa";

/// Supported web search backend identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SearchBackendKind {
    #[default]
    Ddg,
    Tavily,
    Brave,
    Exa,
    Searxng,
}

impl SearchBackendKind {
    /// Parse a persisted backend identifier (`"ddg"`, `"tavily"`, `"brave"`, `"exa"`, `"searxng"`).
    #[must_use]
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ddg" => Some(Self::Ddg),
            "tavily" => Some(Self::Tavily),
            "brave" => Some(Self::Brave),
            "exa" => Some(Self::Exa),
            "searxng" => Some(Self::Searxng),
            _ => None,
        }
    }

    /// Canonical persisted identifier.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ddg => "ddg",
            Self::Tavily => "tavily",
            Self::Brave => "brave",
            Self::Exa => "exa",
            Self::Searxng => "searxng",
        }
    }

    /// Keyring entry name for keyed backends (`None` for keyless backends).
    #[must_use]
    pub(crate) const fn keyring_name(self) -> Option<&'static str> {
        match self {
            Self::Tavily => Some(WEB_SEARCH_KEYRING_TAVILY),
            Self::Brave => Some(WEB_SEARCH_KEYRING_BRAVE),
            Self::Exa => Some(WEB_SEARCH_KEYRING_EXA),
            Self::Ddg | Self::Searxng => None,
        }
    }

    /// Primary destination host surfaced in approval metadata for `web_search`.
    #[must_use]
    pub(crate) fn target_host(self, searxng_url: Option<&str>) -> String {
        match self {
            Self::Ddg => "html.duckduckgo.com".to_string(),
            Self::Tavily => "api.tavily.com".to_string(),
            Self::Brave => "api.search.brave.com".to_string(),
            Self::Exa => "api.exa.ai".to_string(),
            Self::Searxng => searxng_url
                .and_then(|raw| searxng::validate_searxng_base_url(raw).ok())
                .map_or_else(|| "searxng (unconfigured)".to_string(), |v| v.host),
        }
    }
}

/// Normalized, SSRF-validated web search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebResult {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
}

/// Secret-free search error taxonomy.
///
/// Never echoes API keys, URLs, query parameters, headers, or body payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchError {
    /// Query was empty, oversized, or contained control characters.
    InvalidQuery(&'static str),
    /// Backend configuration (e.g. `SearXNG` URL) is missing or invalid.
    InvalidConfig(&'static str),
    /// A keyed backend is selected without a configured OS keyring API key.
    MissingApiKey(&'static str),
    /// OS keyring could not be accessed.
    CredentialStoreUnavailable,
    /// Backend returned CAPTCHA, bot challenge, HTTP 202/403/429, or rate limit.
    RateLimitedOrBlocked(&'static str),
    /// Remote server returned an unexpected non-2xx HTTP status.
    HttpStatus { status: u16 },
    /// Response body could not be parsed as expected HTML/JSON.
    ParseFailed(&'static str),
    /// Search timed out.
    Timeout,
    /// Search was cancelled via [`CancellationToken`].
    Cancelled,
    /// Network, TLS, or response-size failure.
    Transport(&'static str),
    /// Underlying web safety/fetch error.
    Web(WebError),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(msg)
            | Self::InvalidConfig(msg)
            | Self::MissingApiKey(msg)
            | Self::RateLimitedOrBlocked(msg)
            | Self::ParseFailed(msg)
            | Self::Transport(msg) => write!(f, "{msg}"),
            Self::CredentialStoreUnavailable => {
                write!(
                    f,
                    "web search API key could not be read from the OS keyring"
                )
            }
            Self::HttpStatus { status } => {
                write!(f, "web search backend returned HTTP status {status}")
            }
            Self::Timeout => write!(f, "web search request timed out"),
            Self::Cancelled => write!(f, "web search request was cancelled"),
            Self::Web(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<WebError> for SearchError {
    fn from(err: WebError) -> Self {
        match err {
            WebError::Timeout => Self::Timeout,
            WebError::Cancelled => Self::Cancelled,
            other => Self::Web(other),
        }
    }
}

/// Common search backend contract.
pub(crate) trait SearchBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn search(
        &self,
        query: &str,
        max_results: usize,
        token: &CancellationToken,
    ) -> Result<Vec<WebResult>, SearchError>;
}

/// Validate and normalize a search query before dispatching to any backend.
///
/// # Errors
/// Returns [`SearchError::InvalidQuery`] for empty, oversized, or control-char
/// queries.
pub(crate) fn validate_query(raw: &str) -> Result<String, SearchError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SearchError::InvalidQuery("search query must not be empty"));
    }
    if trimmed.len() > MAX_SEARCH_QUERY_LEN {
        return Err(SearchError::InvalidQuery(
            "search query exceeds maximum length of 500 bytes",
        ));
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n' | '\r'))
    {
        return Err(SearchError::InvalidQuery(
            "search query contains forbidden control characters",
        ));
    }
    let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(SearchError::InvalidQuery("search query must not be empty"));
    }
    Ok(normalized)
}

/// Clamp requested result count to the deterministic `1..=8` bound.
#[must_use]
pub(crate) fn clamp_max_results(requested: usize) -> usize {
    requested.clamp(MIN_SEARCH_RESULTS, MAX_SEARCH_RESULTS)
}

/// Re-validate every result URL through strict web SSRF safety, normalize text,
/// deduplicate by canonical URL, and enforce the deterministic result cap.
#[must_use]
pub(crate) fn normalize_and_filter_results(
    raw: Vec<WebResult>,
    max_results: usize,
) -> Vec<WebResult> {
    let cap = clamp_max_results(max_results);
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(cap.min(raw.len()));
    for item in raw {
        if out.len() >= cap {
            break;
        }
        let Ok(validated) = validate_fetch_url(item.url.trim()) else {
            continue;
        };
        let canonical_url = validated.as_url().as_str().to_string();
        if !seen.insert(canonical_url.clone()) {
            continue;
        }
        let title = collapse_and_bound(&item.title, MAX_RESULT_TITLE_CHARS);
        let snippet = collapse_and_bound(&item.snippet, MAX_RESULT_SNIPPET_CHARS);
        let title = if title.is_empty() {
            validated.host().to_string()
        } else {
            title
        };
        out.push(WebResult {
            title,
            url: canonical_url,
            snippet,
        });
    }
    out
}

/// Format normalized results into a deterministic model-facing observation.
/// Empty results always render as `"No results"`.
#[must_use]
pub(crate) fn format_results(results: &[WebResult]) -> String {
    if results.is_empty() {
        return NO_RESULTS_TEXT.to_string();
    }
    let mut out = String::new();
    for (idx, item) in results.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let _ = write!(out, "{}. {} — {}", idx + 1, item.title, item.url);
        if !item.snippet.is_empty() {
            out.push_str("\n   ");
            out.push_str(&item.snippet);
        }
    }
    out
}

fn collapse_and_bound(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for (count, ch) in collapsed.chars().enumerate() {
        if count >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

/// Execute a web search through the selected backend, reading keyed API keys
/// strictly from the OS keyring.
///
/// # Errors
/// Returns a secret-free [`SearchError`].
pub(crate) fn run_search(
    backend: SearchBackendKind,
    query: &str,
    max_results: usize,
    searxng_url: Option<&str>,
    token: &CancellationToken,
) -> Result<Vec<WebResult>, SearchError> {
    if token.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let normalized_query = validate_query(query)?;
    let cap = clamp_max_results(max_results);
    match backend {
        SearchBackendKind::Ddg => ddg::DdgBackend::new().search(&normalized_query, cap, token),
        SearchBackendKind::Tavily => {
            let api_key = read_search_key(
                WEB_SEARCH_KEYRING_TAVILY,
                "Tavily search requires an API key configured in Settings -> Agent -> Web access",
            )?;
            tavily::TavilyBackend::new(api_key).search(&normalized_query, cap, token)
        }
        SearchBackendKind::Brave => {
            let api_key = read_search_key(
                WEB_SEARCH_KEYRING_BRAVE,
                "Brave search requires an API key configured in Settings -> Agent -> Web access",
            )?;
            brave::BraveBackend::new(api_key).search(&normalized_query, cap, token)
        }
        SearchBackendKind::Exa => {
            let api_key = read_search_key(
                WEB_SEARCH_KEYRING_EXA,
                "Exa search requires an API key configured in Settings -> Agent -> Web access",
            )?;
            exa::ExaBackend::new(api_key).search(&normalized_query, cap, token)
        }
        SearchBackendKind::Searxng => {
            let base_url = searxng_url.map(str::trim).filter(|s| !s.is_empty()).ok_or(
                SearchError::InvalidConfig(
                    "SearXNG search requires a validated HTTPS or loopback base URL",
                ),
            )?;
            searxng::SearxngBackend::new(base_url)?.search(&normalized_query, cap, token)
        }
    }
}

fn read_search_key(keyring_name: &str, missing_msg: &'static str) -> Result<String, SearchError> {
    match CredentialStore::read(keyring_name) {
        Ok(Some(secret)) if !secret.trim().is_empty() => Ok(secret.trim().to_string()),
        Ok(_) => Err(SearchError::MissingApiKey(missing_msg)),
        Err(CredentialError::StorageUnavailable | CredentialError::Invalid) => {
            Err(SearchError::CredentialStoreUnavailable)
        }
    }
}

/// Parse a keyed search backend name accepted by credential IPC commands.
#[must_use]
pub(crate) fn keyring_name_for_keyed_backend(backend: &str) -> Option<&'static str> {
    match backend.trim().to_ascii_lowercase().as_str() {
        "tavily" => Some(WEB_SEARCH_KEYRING_TAVILY),
        "brave" => Some(WEB_SEARCH_KEYRING_BRAVE),
        "exa" => Some(WEB_SEARCH_KEYRING_EXA),
        _ => None,
    }
}

/// Percent-encode a string for URL query / `application/x-www-form-urlencoded`.
#[must_use]
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(hex_upper(byte >> 4));
            out.push(hex_upper(byte & 0x0f));
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'A' + (nibble - 10)),
        _ => '0',
    }
}

/// Percent-decode a URL query value (`+` treated as space).
#[must_use]
pub(crate) fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = from_hex(bytes[i + 1])?;
                let lo = from_hex(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared hardened HTTP transport for search backends
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(crate) enum SearchHttpMethod {
    Get,
    PostForm,
    PostJson,
}

pub(crate) struct SearchHttpRequest<'a> {
    pub(crate) url: &'a Url,
    pub(crate) resolved_addrs: &'a [SocketAddr],
    pub(crate) method: SearchHttpMethod,
    pub(crate) user_agent: &'a str,
    pub(crate) headers: &'a [(&'static str, &'a str)],
    pub(crate) body: Option<&'a str>,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchHttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
}

enum SearchStreamMsg {
    Headers {
        status: u16,
        content_encoding: Option<String>,
        content_length: Option<u64>,
    },
    Chunk(Vec<u8>),
    Eof,
    Error(SearchError),
}

/// Resolve and execute a strictly public-routable pinned HTTP request for
/// built-in public search APIs (DDG, Tavily, Brave, Exa).
///
/// # Errors
/// Returns a secret-free [`SearchError`].
pub(crate) fn send_public_search_request(
    raw_url: &str,
    method: SearchHttpMethod,
    user_agent: &str,
    headers: &[(&'static str, &str)],
    body: Option<&str>,
    token: &CancellationToken,
) -> Result<SearchHttpResponse, SearchError> {
    if token.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let validated = validate_fetch_url(raw_url)?;
    let resolved = resolve_and_verify_web_host(validated.as_url())?;
    send_pinned_search_request(
        &SearchHttpRequest {
            url: validated.as_url(),
            resolved_addrs: &resolved,
            method,
            user_agent,
            headers,
            body,
            timeout: DEFAULT_SEARCH_TIMEOUT,
        },
        token,
    )
}

/// Execute a pinned, redirect-disabled, proxy-disabled, byte-bounded HTTP
/// request against already-validated socket addresses.
///
/// # Errors
/// Returns a secret-free [`SearchError`].
pub(crate) fn send_pinned_search_request(
    req: &SearchHttpRequest<'_>,
    token: &CancellationToken,
) -> Result<SearchHttpResponse, SearchError> {
    if token.is_cancelled() {
        return Err(SearchError::Cancelled);
    }
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DEFAULT_SEARCH_CONNECT_TIMEOUT)
        .timeout(req.timeout)
        .user_agent(req.user_agent);

    if let Some(Host::Domain(domain)) = req.url.host() {
        builder = builder.resolve_to_addrs(domain, req.resolved_addrs);
    }

    let client = builder
        .build()
        .map_err(|_| SearchError::Transport("web search HTTP client could not be initialized"))?;

    let target_url = req.url.clone();
    let method = req.method;
    let owned_headers: Vec<(&'static str, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (*k, (*v).to_string()))
        .collect();
    let owned_body = req.body.map(ToOwned::to_owned);

    let (tx, rx) = mpsc::sync_channel::<SearchStreamMsg>(SEARCH_CHANNEL_CAPACITY);
    let handle = tauri::async_runtime::spawn(async move {
        stream_search_task(client, target_url, method, owned_headers, owned_body, tx).await;
    });

    let collected = collect_search_response(req.timeout, token, &rx);
    if collected.is_err() {
        handle.abort();
    }
    collected
}

async fn stream_search_task(
    client: reqwest::Client,
    url: Url,
    method: SearchHttpMethod,
    headers: Vec<(&'static str, String)>,
    body: Option<String>,
    tx: mpsc::SyncSender<SearchStreamMsg>,
) {
    let mut req_builder = match method {
        SearchHttpMethod::Get => client.get(url),
        SearchHttpMethod::PostForm => client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded"),
        SearchHttpMethod::PostJson => client.post(url).header("Content-Type", "application/json"),
    };
    req_builder = req_builder.header("Accept-Encoding", "gzip, deflate, identity");
    for (name, value) in headers {
        req_builder = req_builder.header(name, value);
    }
    if let Some(payload) = body {
        req_builder = req_builder.body(payload);
    }

    let mut response = match req_builder.send().await {
        Ok(resp) => resp,
        Err(err) => {
            let mapped = if err.is_timeout() {
                SearchError::Timeout
            } else if err.is_redirect() {
                SearchError::Web(WebError::RedirectDisallowed)
            } else if err.is_connect() {
                SearchError::Transport("web search backend could not be reached")
            } else {
                SearchError::Transport("web search request failed due to a network error")
            };
            let _ = tx.send(SearchStreamMsg::Error(mapped));
            return;
        }
    };

    let status = response.status().as_u16();
    let content_encoding = response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let content_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    if tx
        .send(SearchStreamMsg::Headers {
            status,
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
                if tx.send(SearchStreamMsg::Chunk(chunk.to_vec())).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = tx.send(SearchStreamMsg::Eof);
                return;
            }
            Err(err) => {
                let mapped = if err.is_timeout() {
                    SearchError::Timeout
                } else {
                    SearchError::Transport("web search response stream interrupted")
                };
                let _ = tx.send(SearchStreamMsg::Error(mapped));
                return;
            }
        }
    }
}

fn collect_search_response(
    timeout: Duration,
    token: &CancellationToken,
    rx: &mpsc::Receiver<SearchStreamMsg>,
) -> Result<SearchHttpResponse, SearchError> {
    let deadline = Instant::now() + timeout;
    let mut status: Option<u16> = None;
    let mut content_encoding: Option<String> = None;
    let mut raw_body = Vec::new();

    loop {
        if token.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(SearchError::Timeout);
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(SEARCH_POLL_INTERVAL);
        match rx.recv_timeout(wait) {
            Ok(SearchStreamMsg::Headers {
                status: s,
                content_encoding: ce,
                content_length,
            }) => {
                if (300..400).contains(&s) {
                    return Err(SearchError::Web(WebError::RedirectDisallowed));
                }
                if let Some(len) = content_length {
                    if len > u64::try_from(MAX_RAW_RESPONSE_BYTES).unwrap_or(u64::MAX) {
                        return Err(SearchError::Web(WebError::ResponseTooLarge));
                    }
                }
                status = Some(s);
                content_encoding = ce;
            }
            Ok(SearchStreamMsg::Chunk(chunk)) => {
                if raw_body.len().saturating_add(chunk.len()) > MAX_RAW_RESPONSE_BYTES {
                    return Err(SearchError::Web(WebError::ResponseTooLarge));
                }
                raw_body.extend_from_slice(&chunk);
            }
            Ok(SearchStreamMsg::Eof) => {
                let decompressed = decompress_body(
                    &raw_body,
                    content_encoding.as_deref(),
                    MAX_DECOMPRESSED_BYTES,
                )?;
                return Ok(SearchHttpResponse {
                    status: status.unwrap_or(500),
                    body: String::from_utf8_lossy(&decompressed).into_owned(),
                });
            }
            Ok(SearchStreamMsg::Error(err)) => return Err(err),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if token.is_cancelled() {
                    return Err(SearchError::Cancelled);
                }
                return Err(SearchError::Transport("web search task ended unexpectedly"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::error::{CommandError, ErrorKind};

    #[test]
    fn format_results_returns_fixed_no_results_on_empty() {
        assert_eq!(format_results(&[]), "No results");
    }

    #[test]
    fn normalize_and_filter_enforces_cap_and_drops_unsafe_urls() {
        let raw = vec![
            WebResult {
                title: "Local".into(),
                url: "http://127.0.0.1/admin".into(),
                snippet: "blocked".into(),
            },
            WebResult {
                title: "JS".into(),
                url: "javascript:alert(1)".into(),
                snippet: "blocked".into(),
            },
            WebResult {
                title: "Metadata".into(),
                url: "http://169.254.169.254/latest".into(),
                snippet: "blocked".into(),
            },
            WebResult {
                title: "  Safe   Result  ".into(),
                url: "https://example.com/a".into(),
                snippet: "  hello\nworld  ".into(),
            },
            WebResult {
                title: "Duplicate".into(),
                url: "https://example.com/a".into(),
                snippet: "dup".into(),
            },
            WebResult {
                title: "Second".into(),
                url: "https://rust-lang.org/docs".into(),
                snippet: "docs".into(),
            },
        ];

        let filtered = normalize_and_filter_results(raw, 8);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].title, "Safe Result");
        assert_eq!(filtered[0].url, "https://example.com/a");
        assert_eq!(filtered[0].snippet, "hello world");
        assert_eq!(filtered[1].url, "https://rust-lang.org/docs");
    }

    #[test]
    fn clamp_max_results_enforces_deterministic_bounds() {
        assert_eq!(clamp_max_results(0), 1);
        assert_eq!(clamp_max_results(5), 5);
        assert_eq!(clamp_max_results(8), 8);
        assert_eq!(clamp_max_results(100), 8);
    }

    #[test]
    fn search_error_messages_and_command_mapping_are_secret_free() {
        let cases = [
            (
                SearchError::InvalidQuery("search query must not be empty"),
                ErrorKind::InvalidInput,
            ),
            (
                SearchError::InvalidConfig("invalid SearXNG URL"),
                ErrorKind::InvalidInput,
            ),
            (
                SearchError::MissingApiKey("Tavily search requires an API key"),
                ErrorKind::Request,
            ),
            (
                SearchError::CredentialStoreUnavailable,
                ErrorKind::Credential,
            ),
            (
                SearchError::RateLimitedOrBlocked("rate limited"),
                ErrorKind::Request,
            ),
            (SearchError::HttpStatus { status: 502 }, ErrorKind::Request),
            (SearchError::ParseFailed("bad json"), ErrorKind::Request),
            (SearchError::Timeout, ErrorKind::Request),
            (SearchError::Cancelled, ErrorKind::Request),
            (SearchError::Transport("network error"), ErrorKind::Request),
        ];

        for (err, expected_kind) in cases {
            let mapped = CommandError::from(err);
            assert_eq!(mapped.kind, expected_kind);
            let lower = mapped.message.to_ascii_lowercase();
            for forbidden in [
                "http://",
                "https://",
                "sk-",
                "tvly-",
                "bearer ",
                "127.0.0.1",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "SearchError leaked {forbidden:?}: {}",
                    mapped.message
                );
            }
        }
    }
}
