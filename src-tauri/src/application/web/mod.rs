//! Hardened web fetch & HTML-to-Markdown foundation (Stage 12 PR 5).
//!
//! This module provides the network-safety, bounded HTTP fetch, and
//! self-contained HTML-to-Markdown extraction primitives that future web tools
//! (`web_fetch`, `web_search`) build on.
//!
//! Hard invariants:
//! - Strictly fail-closed SSRF protection: model-supplied URLs may never target
//!   loopback, private, link-local, metadata, CGNAT, or NAT64 addresses, and
//!   redirects are strictly disabled (`Policy::none()`).
//! - Layered resource bounds: `Content-Length` precheck, `1 MiB` streamed raw
//!   body cap, `2 MiB` decompressed body cap, `5s` connect / `20s` total /
//!   `10s` inter-chunk timeouts, cooperative [`CancellationToken`], and
//!   `20_000`-char (clamped up to `48_000`) post-conversion output cap.
//! - Secret-free errors: [`WebError`] carries only fixed-vocabulary categories
//!   and numeric HTTP status codes — never URLs, hostnames, userinfo, keys, or
//!   response payloads.

pub(crate) mod extract;
pub(crate) mod fetch;
pub(crate) mod safety;
pub(crate) mod search;

use std::fmt;
use std::time::Duration;

use crate::application::agent::control::CancellationToken;

use extract::{extract_markdown, truncate_utf8_with_marker, ExtractOptions};
use fetch::{fetch_url, FetchOptions};

/// Maximum accepted length of a web fetch URL (bytes).
pub(crate) const MAX_WEB_URL_LEN: usize = 2048;

/// Default TCP/TLS connect timeout for web fetches (research §7.3).
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default total end-to-end timeout for a single web fetch (research §7.3).
pub(crate) const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Default maximum idle time between streamed body chunks (research §7.3).
pub(crate) const DEFAULT_INTER_CHUNK_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard cap on raw wire bytes downloaded for one web fetch (`1 MiB`).
pub(crate) const MAX_RAW_RESPONSE_BYTES: usize = 1_048_576;

/// Hard cap on decompressed bytes for one web fetch (`2 MiB`).
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 2_097_152;

/// Default maximum extracted Markdown length in Unicode scalar values.
pub(crate) const DEFAULT_MAX_EXTRACTED_CHARS: usize = 20_000;

/// Upper bound on caller-configurable extracted character cap.
pub(crate) const MAX_EXTRACTED_CHARS: usize = 48_000;

/// Maximum HTML element nesting depth before deeper elements are bounded.
pub(crate) const MAX_HTML_NESTING_DEPTH: usize = 128;

/// Maximum list indentation nesting levels (`2` spaces per level).
pub(crate) const MAX_LIST_INDENT_LEVELS: usize = 6;

/// Suffix appended whenever extracted output is truncated to a char cap.
pub(crate) const TRUNCATED_MARKER: &str = "\n\n[truncated]";

/// Fixed non-identifying `User-Agent` sent on web fetch requests.
pub(crate) const WEB_FETCH_USER_AGENT: &str = "Nexora-Web/2.0";

/// Secret-free web fetch and extraction error taxonomy.
///
/// Never carries URLs, hostnames, userinfo, credentials, headers, or body
/// fragments — every human-readable message is compile-time fixed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebError {
    /// The URL failed syntactic, scheme, userinfo, control-char, or secret checks.
    InvalidUrl(&'static str),
    /// The URL targets a blocked loopback, private, link-local, metadata, or NAT64 host.
    BlockedHost,
    /// DNS resolution failed or yielded no usable addresses.
    DnsResolutionFailed,
    /// The remote server returned an HTTP 3xx redirect (strictly disallowed).
    RedirectDisallowed,
    /// The response `Content-Type` is missing or outside the text/HTML allowlist.
    UnsupportedMediaType,
    /// The response `Content-Encoding` is unsupported or nested.
    UnsupportedContentEncoding,
    /// The raw response exceeded the streamed byte limit (or advertised `Content-Length`).
    ResponseTooLarge,
    /// The decompressed response exceeded the decompressed byte limit.
    DecompressedTooLarge,
    /// The compressed response stream could not be decoded.
    DecompressionFailed,
    /// The remote server returned a non-success HTTP status code.
    HttpStatus { status: u16 },
    /// The fetch exceeded its connect, inter-chunk, or total timeout.
    Timeout,
    /// The fetch was cancelled via [`CancellationToken`].
    Cancelled,
    /// A network or TLS transport failure occurred.
    Transport(&'static str),
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(msg) | Self::Transport(msg) => write!(f, "{msg}"),
            Self::BlockedHost => write!(
                f,
                "web URL targets a blocked loopback, private, link-local, metadata, or non-routable host"
            ),
            Self::DnsResolutionFailed => write!(f, "web URL host could not be resolved safely"),
            Self::RedirectDisallowed => {
                write!(f, "web fetch does not follow HTTP redirects")
            }
            Self::UnsupportedMediaType => {
                write!(f, "web response media type is not supported text or HTML")
            }
            Self::UnsupportedContentEncoding => {
                write!(f, "web response uses an unsupported content encoding")
            }
            Self::ResponseTooLarge => {
                write!(f, "web response exceeded the maximum download size limit")
            }
            Self::DecompressedTooLarge => {
                write!(
                    f,
                    "web response exceeded the maximum decompressed size limit"
                )
            }
            Self::DecompressionFailed => {
                write!(f, "web response could not be decompressed")
            }
            Self::HttpStatus { status } => {
                write!(f, "web server returned HTTP status {status}")
            }
            Self::Timeout => write!(f, "web fetch timed out"),
            Self::Cancelled => write!(f, "web fetch was cancelled"),
        }
    }
}

impl std::error::Error for WebError {}

/// Result of fetching a web URL and extracting bounded Markdown/text content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchedDocument {
    /// Validated destination host (without userinfo or path).
    pub(crate) host: String,
    /// HTTP 2xx status code returned by the origin server.
    pub(crate) status: u16,
    /// Normalized MIME media type (e.g. `text/html`, `text/plain`).
    pub(crate) mime_type: String,
    /// Extracted Markdown (for HTML/XHTML) or bounded plain text.
    pub(crate) content: String,
    /// Whether `content` was truncated and suffixed with [`TRUNCATED_MARKER`].
    pub(crate) truncated: bool,
    /// Raw compressed wire bytes read.
    pub(crate) raw_bytes: usize,
    /// Decompressed UTF-8/lossy body byte count before extraction.
    pub(crate) decompressed_bytes: usize,
}

/// Fetch `raw_url` with full SSRF and resource protections and convert HTML
/// responses into bounded Markdown (or bound `text/plain` directly).
///
/// # Errors
/// Returns a secret-free [`WebError`] if URL validation, DNS verification,
/// network transport, MIME/size limits, decompression, or cancellation fails.
pub(crate) fn fetch_and_extract(
    raw_url: &str,
    fetch_options: &FetchOptions,
    extract_options: ExtractOptions,
    token: &CancellationToken,
) -> Result<FetchedDocument, WebError> {
    let fetched = fetch_url(raw_url, fetch_options, token)?;
    let (content, truncated) = if fetched.mime_type == "text/plain" {
        truncate_utf8_with_marker(&fetched.body_text, extract_options.clamped_max_chars())
    } else {
        let extracted = extract_markdown(&fetched.body_text, extract_options);
        (extracted.markdown, extracted.truncated)
    };
    Ok(FetchedDocument {
        host: fetched.host,
        status: fetched.status,
        mime_type: fetched.mime_type,
        content,
        truncated,
        raw_bytes: fetched.raw_bytes,
        decompressed_bytes: fetched.decompressed_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::error::{CommandError, ErrorKind};

    #[test]
    fn web_error_messages_and_command_error_mapping_are_secret_free() {
        const FORBIDDEN_SENTINELS: [&str; 6] = [
            "http://",
            "https://",
            "sk-",
            "bearer",
            "password",
            "127.0.0.1",
        ];

        let cases = [
            (
                WebError::InvalidUrl("web URL must not include embedded credentials"),
                ErrorKind::InvalidInput,
            ),
            (WebError::BlockedHost, ErrorKind::InvalidInput),
            (WebError::DnsResolutionFailed, ErrorKind::Request),
            (WebError::RedirectDisallowed, ErrorKind::Request),
            (WebError::UnsupportedMediaType, ErrorKind::UnsupportedFormat),
            (
                WebError::UnsupportedContentEncoding,
                ErrorKind::UnsupportedFormat,
            ),
            (WebError::ResponseTooLarge, ErrorKind::Request),
            (WebError::DecompressedTooLarge, ErrorKind::Request),
            (WebError::DecompressionFailed, ErrorKind::Request),
            (WebError::HttpStatus { status: 503 }, ErrorKind::Request),
            (WebError::Timeout, ErrorKind::Request),
            (WebError::Cancelled, ErrorKind::Request),
            (
                WebError::Transport("web fetch failed due to a network error"),
                ErrorKind::Request,
            ),
        ];

        for (err, expected_kind) in cases {
            let text = err.to_string();
            assert!(!text.is_empty());
            let mapped = CommandError::from(err);
            assert_eq!(mapped.kind, expected_kind);
            assert_eq!(mapped.message, text);
            let lower = mapped.message.to_ascii_lowercase();
            for sentinel in FORBIDDEN_SENTINELS {
                assert!(
                    !lower.contains(sentinel),
                    "WebError message must stay secret/URL-free, found {sentinel:?} in {text:?}"
                );
            }
        }
    }
}
