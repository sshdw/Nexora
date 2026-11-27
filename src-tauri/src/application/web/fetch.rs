//! Bounded web fetch pipeline: MIME allowlist, `Content-Length` precheck,
//! `1 MiB` streamed wire cap, `2 MiB` decompressed cap, timeouts, and
//! [`CancellationToken`] support (Stage 12 PR 5).

use std::io::Read;
use std::time::Duration;

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use crate::application::agent::control::CancellationToken;

use super::safety::{
    fetch_with_pinned_dns, validate_fetch_url, PinnedFetchOptions, RawHttpResponse,
};
use super::{
    WebError, DEFAULT_CONNECT_TIMEOUT, DEFAULT_INTER_CHUNK_TIMEOUT, DEFAULT_TOTAL_TIMEOUT,
    MAX_DECOMPRESSED_BYTES, MAX_RAW_RESPONSE_BYTES,
};

/// Configuration for [`fetch_url`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct FetchOptions {
    /// Connect timeout (`5s` default).
    pub(crate) connect_timeout: Duration,
    /// Total request timeout (`20s` default).
    pub(crate) total_timeout: Duration,
    /// Inter-chunk idle timeout (`10s` default).
    pub(crate) inter_chunk_timeout: Duration,
    /// Raw wire byte cap (`1 MiB` default).
    pub(crate) max_raw_bytes: usize,
    /// Decompressed byte cap (`2 MiB` default).
    pub(crate) max_decompressed_bytes: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            inter_chunk_timeout: DEFAULT_INTER_CHUNK_TIMEOUT,
            max_raw_bytes: MAX_RAW_RESPONSE_BYTES,
            max_decompressed_bytes: MAX_DECOMPRESSED_BYTES,
        }
    }
}

impl FetchOptions {
    fn to_pinned_options(self) -> PinnedFetchOptions {
        PinnedFetchOptions {
            connect_timeout: self.connect_timeout,
            total_timeout: self.total_timeout,
            inter_chunk_timeout: self.inter_chunk_timeout,
            max_raw_bytes: self.max_raw_bytes,
            allowed_mime_check: is_allowed_mime_type,
        }
    }
}

/// Decoded HTTP text/HTML response returned by [`fetch_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FetchResult {
    /// Validated destination host (without userinfo or path).
    pub(crate) host: String,
    /// HTTP 2xx status code.
    pub(crate) status: u16,
    /// Normalized allowlisted MIME type (`text/html`, `text/plain`, or
    /// `application/xhtml+xml`).
    pub(crate) mime_type: String,
    /// Decompressed body decoded cleanly as UTF-8 (invalid byte sequences are
    /// replaced with U+FFFD rather than failing or panicking).
    pub(crate) body_text: String,
    /// Raw wire bytes transferred.
    pub(crate) raw_bytes: usize,
    /// Decompressed byte length.
    pub(crate) decompressed_bytes: usize,
}

/// Extract and normalize the MIME media type (essence) from a `Content-Type`
/// header value (stripping parameters such as `; charset=utf-8`).
#[must_use]
pub(crate) fn extract_mime_type(content_type: &str) -> Option<String> {
    let essence = content_type.split(';').next()?.trim().to_ascii_lowercase();
    if essence.is_empty() {
        None
    } else {
        Some(essence)
    }
}

/// Strict MIME allowlist for web fetches.
#[must_use]
pub(crate) fn is_allowed_mime_type(mime: &str) -> bool {
    matches!(mime, "text/html" | "text/plain" | "application/xhtml+xml")
}

/// Fetch `raw_url` with full SSRF validation, DNS pinning, redirect refusal,
/// MIME allowlist, `Content-Length` precheck, `1 MiB` streamed wire cap,
/// `2 MiB` decompressed cap, timeouts, and cooperative cancellation.
///
/// # Errors
/// Returns a secret-free [`WebError`].
pub(crate) fn fetch_url(
    raw_url: &str,
    options: &FetchOptions,
    token: &CancellationToken,
) -> Result<FetchResult, WebError> {
    if token.is_cancelled() {
        return Err(WebError::Cancelled);
    }
    let validated = validate_fetch_url(raw_url)?;
    let pinned_opts = options.to_pinned_options();
    let raw = fetch_with_pinned_dns(&validated, &pinned_opts, token)?;
    finalize_raw_response(raw, options.max_decompressed_bytes)
}

fn finalize_raw_response(
    raw: RawHttpResponse,
    max_decompressed_bytes: usize,
) -> Result<FetchResult, WebError> {
    let raw_bytes = raw.raw_body.len();
    let decompressed = decompress_body(
        &raw.raw_body,
        raw.content_encoding.as_deref(),
        max_decompressed_bytes,
    )?;
    let decompressed_bytes = decompressed.len();
    let body_text = String::from_utf8_lossy(&decompressed).into_owned();

    Ok(FetchResult {
        host: raw.host,
        status: raw.status,
        mime_type: raw.mime_type,
        body_text,
        raw_bytes,
        decompressed_bytes,
    })
}

/// Decompress `raw_body` according to `Content-Encoding`, strictly enforcing
/// `max_decompressed_bytes` to defeat decompression bombs.
///
/// # Errors
/// Returns [`WebError::UnsupportedContentEncoding`],
/// [`WebError::DecompressedTooLarge`], or [`WebError::DecompressionFailed`].
pub(crate) fn decompress_body(
    raw_body: &[u8],
    content_encoding: Option<&str>,
    max_decompressed_bytes: usize,
) -> Result<Vec<u8>, WebError> {
    let encodings: Vec<String> = content_encoding
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "identity")
        .collect();

    if encodings.is_empty() {
        if raw_body.len() > max_decompressed_bytes {
            return Err(WebError::DecompressedTooLarge);
        }
        return Ok(raw_body.to_vec());
    }

    // Reject stacked/nested encodings (e.g. `gzip, gzip`) to avoid multi-layer bombs.
    if encodings.len() > 1 {
        return Err(WebError::UnsupportedContentEncoding);
    }

    match encodings[0].as_str() {
        "gzip" | "x-gzip" => read_bounded(GzDecoder::new(raw_body), max_decompressed_bytes),
        "deflate" => match read_bounded(ZlibDecoder::new(raw_body), max_decompressed_bytes) {
            Ok(bytes) => Ok(bytes),
            Err(WebError::DecompressedTooLarge) => Err(WebError::DecompressedTooLarge),
            Err(_) => read_bounded(DeflateDecoder::new(raw_body), max_decompressed_bytes),
        },
        _ => Err(WebError::UnsupportedContentEncoding),
    }
}

fn read_bounded<R: Read>(reader: R, max_bytes: usize) -> Result<Vec<u8>, WebError> {
    let limit_u64 = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut out = Vec::new();
    let mut limited = reader.take(limit_u64);
    limited
        .read_to_end(&mut out)
        .map_err(|_| WebError::DecompressionFailed)?;
    if out.len() > max_bytes {
        return Err(WebError::DecompressedTooLarge);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    use flate2::write::{GzEncoder, ZlibEncoder};
    use flate2::Compression;

    use super::*;
    use crate::application::web::extract::{extract_markdown, ExtractOptions};
    use crate::application::web::safety::fetch_loopback_for_test;

    fn spawn_single_response_server(raw_response: Vec<u8>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req_buf = [0u8; 2048];
                let _ = stream.read(&mut req_buf);
                let _ = stream.write_all(&raw_response);
                let _ = stream.flush();
            }
        });
        addr
    }

    fn fetch_test_server(
        addr: SocketAddr,
        options: &FetchOptions,
        token: &CancellationToken,
    ) -> Result<FetchResult, WebError> {
        let raw = fetch_loopback_for_test(addr, "/test", &options.to_pinned_options(), token)?;
        finalize_raw_response(raw, options.max_decompressed_bytes)
    }

    #[test]
    fn production_fetch_url_strictly_blocks_loopback_server() {
        let addr = spawn_single_response_server(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        );
        let url = format!("http://{addr}/test");
        let err = fetch_url(&url, &FetchOptions::default(), &CancellationToken::new())
            .expect_err("production fetch_url must block loopback");
        assert_eq!(err, WebError::BlockedHost);
    }

    #[test]
    fn fetches_html_plain_and_xhtml_and_extracts_markdown() {
        let body = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_single_response_server(resp.into_bytes());
        let fetched = fetch_test_server(addr, &FetchOptions::default(), &CancellationToken::new())
            .expect("html fetch succeeds");
        assert_eq!(fetched.status, 200);
        assert_eq!(fetched.mime_type, "text/html");
        let md = extract_markdown(&fetched.body_text, ExtractOptions::default());
        assert_eq!(md.markdown, "# Hello\n\nWorld");

        let plain_resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nplain".to_vec();
        let plain_addr = spawn_single_response_server(plain_resp);
        let plain = fetch_test_server(
            plain_addr,
            &FetchOptions::default(),
            &CancellationToken::new(),
        )
        .expect("plain fetch succeeds");
        assert_eq!(plain.mime_type, "text/plain");
        assert_eq!(plain.body_text, "plain");

        let xhtml_resp = b"HTTP/1.1 200 OK\r\nContent-Type: application/xhtml+xml\r\nContent-Length: 10\r\nConnection: close\r\n\r\n<p>hi</p>\n".to_vec();
        let xhtml_addr = spawn_single_response_server(xhtml_resp);
        let xhtml = fetch_test_server(
            xhtml_addr,
            &FetchOptions::default(),
            &CancellationToken::new(),
        )
        .expect("xhtml fetch succeeds");
        assert_eq!(xhtml.mime_type, "application/xhtml+xml");
    }

    #[test]
    fn rejects_redirects_without_following() {
        let resp = b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let addr = spawn_single_response_server(resp);
        let err = fetch_test_server(addr, &FetchOptions::default(), &CancellationToken::new())
            .expect_err("redirect must be rejected");
        assert_eq!(err, WebError::RedirectDisallowed);
    }

    #[test]
    fn rejects_non_html_and_missing_mime_types() {
        for header in [
            "Content-Type: application/pdf\r\n",
            "Content-Type: image/png\r\n",
            "Content-Type: application/octet-stream\r\n",
            "",
        ] {
            let resp = format!(
                "HTTP/1.1 200 OK\r\n{header}Content-Length: 4\r\nConnection: close\r\n\r\ndata"
            );
            let addr = spawn_single_response_server(resp.into_bytes());
            let err = fetch_test_server(addr, &FetchOptions::default(), &CancellationToken::new())
                .expect_err("disallowed MIME must be rejected");
            assert_eq!(err, WebError::UnsupportedMediaType);
        }
    }

    #[test]
    fn rejects_honest_oversized_content_length_before_reading_body() {
        let opts = FetchOptions {
            max_raw_bytes: 64,
            ..FetchOptions::default()
        };
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 999999\r\nConnection: close\r\n\r\n".to_vec();
        let addr = spawn_single_response_server(resp);
        let err = fetch_test_server(addr, &opts, &CancellationToken::new())
            .expect_err("oversized Content-Length must be rejected");
        assert_eq!(err, WebError::ResponseTooLarge);
    }

    #[test]
    fn rejects_lying_small_content_length_and_chunked_oversize() {
        let opts = FetchOptions {
            max_raw_bytes: 32,
            ..FetchOptions::default()
        };

        // Case 1: Transfer-Encoding: chunked with body > 32 bytes (no Content-Length)
        let chunk_payload = "x".repeat(40);
        let chunked_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            chunk_payload.len(),
            chunk_payload
        );
        let addr1 = spawn_single_response_server(chunked_resp.into_bytes());
        assert_eq!(
            fetch_test_server(addr1, &opts, &CancellationToken::new()),
            Err(WebError::ResponseTooLarge)
        );

        // Case 2: No Content-Length, raw EOF-delimited body > 32 bytes
        let eof_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{}",
            "y".repeat(64)
        );
        let addr2 = spawn_single_response_server(eof_resp.into_bytes());
        assert_eq!(
            fetch_test_server(addr2, &opts, &CancellationToken::new()),
            Err(WebError::ResponseTooLarge)
        );

        // Case 3: Lying small Content-Length with chunked transfer encoding
        // (RFC 9112: Transfer-Encoding overrides Content-Length)
        let lying_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            chunk_payload.len(),
            chunk_payload
        );
        let addr3 = spawn_single_response_server(lying_resp.into_bytes());
        let res3 = fetch_test_server(addr3, &opts, &CancellationToken::new());
        // Either reqwest rejects the malformed TE+CL framing as Transport or our cap rejects as ResponseTooLarge;
        // it must NEVER succeed with an oversized body.
        assert!(res3.is_err(), "lying CL + oversized chunked must fail");
    }

    #[test]
    fn decompresses_gzip_and_deflate_and_blocks_decompression_bomb() {
        // Valid gzip
        let plain = b"<p>Compressed hello</p>";
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(plain).expect("gz write");
        let gz_bytes = gz.finish().expect("gz finish");
        let mut resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gz_bytes.len()
        )
        .into_bytes();
        resp.extend_from_slice(&gz_bytes);
        let addr = spawn_single_response_server(resp);
        let ok = fetch_test_server(addr, &FetchOptions::default(), &CancellationToken::new())
            .expect("gzip fetch succeeds");
        assert_eq!(ok.body_text, "<p>Compressed hello</p>");

        // Valid deflate (zlib)
        let mut zl = ZlibEncoder::new(Vec::new(), Compression::default());
        zl.write_all(b"deflated").expect("zlib write");
        let zl_bytes = zl.finish().expect("zlib finish");
        let decompressed = decompress_body(&zl_bytes, Some("deflate"), 1024).expect("deflate ok");
        assert_eq!(decompressed, b"deflated");

        // Decompression bomb: tiny compressed payload expanding past max_decompressed_bytes
        let bomb_plain = vec![b'A'; 4096];
        let mut bomb_gz = GzEncoder::new(Vec::new(), Compression::best());
        bomb_gz.write_all(&bomb_plain).expect("bomb write");
        let bomb_bytes = bomb_gz.finish().expect("bomb finish");
        assert!(bomb_bytes.len() < 128, "bomb should compress very small");

        let mut bomb_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bomb_bytes.len()
        )
        .into_bytes();
        bomb_resp.extend_from_slice(&bomb_bytes);
        let bomb_addr = spawn_single_response_server(bomb_resp);
        let opts = FetchOptions {
            max_raw_bytes: 1024,
            max_decompressed_bytes: 512,
            ..FetchOptions::default()
        };
        assert_eq!(
            fetch_test_server(bomb_addr, &opts, &CancellationToken::new()),
            Err(WebError::DecompressedTooLarge)
        );

        // Unsupported / stacked encodings
        assert_eq!(
            decompress_body(b"abc", Some("br"), 1024),
            Err(WebError::UnsupportedContentEncoding)
        );
        assert_eq!(
            decompress_body(&gz_bytes, Some("gzip, gzip"), 1024),
            Err(WebError::UnsupportedContentEncoding)
        );
    }

    #[test]
    fn handles_non_utf8_bytes_lossily_without_panicking() {
        let mut resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".to_vec();
        resp.extend_from_slice(&[b'h', b'i', 0xff, 0xfe, b'!']);
        let addr = spawn_single_response_server(resp);
        let fetched = fetch_test_server(addr, &FetchOptions::default(), &CancellationToken::new())
            .expect("non-utf8 fetch succeeds lossily");
        assert!(fetched.body_text.starts_with("hi"));
        assert!(fetched.body_text.ends_with('!'));
        assert!(fetched.body_text.contains('\u{FFFD}'));
    }

    #[test]
    fn enforces_inter_chunk_timeout_and_cancellation() {
        // Pre-cancelled token immediately returns Cancelled
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            fetch_url("https://example.com/", &FetchOptions::default(), &cancelled),
            Err(WebError::Cancelled)
        );

        // Server sends headers + first chunk, then stalls past inter_chunk_timeout
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stall server");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n",
                );
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(600));
            }
        });

        let opts = FetchOptions {
            connect_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(5),
            inter_chunk_timeout: Duration::from_millis(120),
            ..FetchOptions::default()
        };
        assert_eq!(
            fetch_test_server(addr, &opts, &CancellationToken::new()),
            Err(WebError::Timeout)
        );

        // Mid-stream cancellation aborts promptly
        let listener2 = TcpListener::bind("127.0.0.1:0").expect("bind cancel server");
        let addr2 = listener2.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener2.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n",
                );
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(600));
            }
        });
        let token2 = CancellationToken::new();
        let token_trigger = token2.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            token_trigger.cancel();
        });
        assert_eq!(
            fetch_test_server(addr2, &FetchOptions::default(), &token2),
            Err(WebError::Cancelled)
        );
    }
}
