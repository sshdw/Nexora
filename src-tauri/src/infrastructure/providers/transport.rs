//! Shared cancellable provider HTTP transport (agent-transport-retry).
//!
//! Every provider executor ([`super::openai`], [`super::anthropic`],
//! [`super::gemini`], including the OpenAI-compatible path) sends its
//! non-streaming request through this single helper instead of a per-file
//! blocking loop. It owns:
//!
//! - the shared async `reqwest` client (connection pooling is retained: one
//!   client per executor, reused across requests);
//! - prompt cancellation: the run's [`CancellationToken`] is polled while an
//!   attempt is in flight and between retries, so a cancelled run observes
//!   the abort in milliseconds, never after the wall-clock `request_timeout`;
//! - honest retries: initial attempt plus bounded retries ([`MAX_SEND_ATTEMPTS`]
//!   total) for 429/5xx ([`is_retryable_status`]) and retryable network
//!   errors (refused/DNS/timeout/reset); 400/401/402/403/404 fire exactly
//!   once. `Retry-After` is honored from the response header first, then from
//!   common JSON body shapes, capped at [`MAX_RETRY_DELAY_SECS`]; a missing
//!   hint falls back to the jittered [`backoff_delay`], never zero;
//! - error-body reads: failure bodies are read (bounded) so `Retry-After`
//!   fields can be extracted. Bodies never leave this module except inside
//!   the classification snapshot the provider maps to a category-only
//!   [`ExecutorError`]: no credential, no request payload, and no raw body
//!   text ever reaches an error, a log line, or an event.
//!
//! # Mechanics (no new crates, no `unsafe`, no direct tokio dependency)
//!
//! The calling thread (agent run thread, or the runtime blocking pool for
//! plain chat) never blocks inside an async context: each attempt is spawned
//! onto the Tauri async runtime via [`tauri::async_runtime::spawn`] and the
//! driver waits on an `std::sync::mpsc` channel with short `recv_timeout`
//! polls against the token. On cancellation the spawned task is aborted via
//! the runtime join handle — dropping the in-flight request future aborts
//! the socket — and [`ExecutorError::Cancelled`] is returned immediately.
//! Retry-backoff waits sleep in small slices against the token for the same
//! reason. `unsafe_code = "forbid"` holds: no raw wakers are constructed.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::application::agent::control::CancellationToken;
use crate::application::execution::{
    is_retryable_status, retry_delay, ExecutorError, MAX_SEND_ATTEMPTS,
};

/// Upper bound on an error body kept for `Retry-After` extraction (bytes).
/// Provider diagnostics are small; the cap bounds memory against a hostile
/// or malfunctioning server without changing classification (a body beyond
/// the cap falls back to the header hint or the computed backoff).
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1_024;

/// How often the driver polls the channel / cancellation token while an
/// attempt is in flight. Bounds cancel-observation latency at ~10 ms, far
/// under the ~2 s budget, without spinning.
const FLIGHT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Slice length for cancellable retry-backoff waits. Same latency bound as
/// [`FLIGHT_POLL_INTERVAL`], kept separate so each bound reads at its use.
const BACKOFF_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Provider credential placement for one POST. The value is borrowed for the
/// call only and never stored, logged, or returned.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Credential<'a> {
    /// `Authorization: Bearer <value>` (`OpenAI` and the `OpenAI`-compatible path).
    Bearer(&'a str),
    /// A provider API-key header (`x-api-key`, `x-goog-api-key`, ...).
    Header {
        /// Header name.
        name: &'static str,
        /// Key value (borrowed, never stored).
        value: &'a str,
    },
}

/// Owned credential placement for the spawned attempt task (the runtime
/// requires `'static` futures, so the borrowed [`Credential`] is copied here;
/// the copy lives only for the attempt).
#[derive(Debug, Clone)]
enum OwnedCredential {
    /// `Authorization: Bearer <value>`.
    Bearer(String),
    /// A provider API-key header.
    Header {
        /// Header name.
        name: &'static str,
        /// Key value.
        value: String,
    },
}

impl<'a> From<Credential<'a>> for OwnedCredential {
    fn from(credential: Credential<'a>) -> Self {
        match credential {
            Credential::Bearer(value) => Self::Bearer(value.to_owned()),
            Credential::Header { name, value } => Self::Header {
                name,
                value: value.to_owned(),
            },
        }
    }
}
/// One non-streaming provider POST, fully described so every provider shares
/// the retry loop byte-for-byte.
#[derive(Debug, Clone)]
pub(crate) struct PostRequest<'a> {
    /// Fully-qualified endpoint URL (Gemini embeds `{model}:generateContent`).
    pub url: String,
    /// Credential placement.
    pub credential: Credential<'a>,
    /// Static extra headers applied after auth (`OpenRouter` `HTTP-Referer`/`X-Title`).
    pub extra_headers: &'a [(&'static str, &'static str)],
    /// Pre-serialized JSON body; cloned per attempt (attempts ≤ 3).
    pub body: &'a [u8],
    /// Per-attempt wall-clock bound (the runner's default); `None` preserves
    /// the historical unbounded behavior.
    pub timeout: Option<Duration>,
}

/// Failure snapshot for provider classification: status plus the merged
/// `Retry-After` (response header first, then body field). Providers map this
/// to a category-only [`ExecutorError`]. The raw error body itself is consumed
/// inside the transport (bounded read for `Retry-After` extraction) and never
/// stored or surfaced: no credential, request payload, or body text ever
/// reaches an error, a log line, or an event.
#[derive(Debug, Clone)]
pub(crate) struct ErrorSnapshot {
    /// HTTP status code of the terminal failure.
    pub status: u16,
    /// Merged `Retry-After` hint in seconds (header wins over body), already
    /// capped upstream by [`retry_delay`]; meaningful for 429.
    pub retry_after_secs: Option<u64>,
}

/// Outcome of [`HttpClient::post`]: success bytes for the provider to parse,
/// or a terminal failure snapshot for the provider to classify. Transport
/// failures surface as `Err`: [`ExecutorError::Network`] (retries exhausted)
/// or [`ExecutorError::Cancelled`] (token fired).
#[derive(Debug)]
pub(crate) enum PostOutcome {
    /// 2xx with the full response body.
    Success(Vec<u8>),
    /// Non-retryable status, or a retryable status with attempts exhausted.
    Failure(ErrorSnapshot),
}

/// Shared async HTTP client (one per executor, reused across requests).
pub(crate) struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// Build the shared client (rustls, connection pooling).
    pub(crate) fn new() -> Self {
        Self {
            inner: reqwest::Client::new(),
        }
    }

    /// Send `request` with cancellable bounded retries (see module docs).
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError::Network`] when every attempt failed at the
    /// transport, or [`ExecutorError::Cancelled`] when `token` fired
    /// in-flight or during backoff. Both are secret-free categories.
    pub(crate) fn post(
        &self,
        token: &CancellationToken,
        request: &PostRequest<'_>,
    ) -> Result<PostOutcome, ExecutorError> {
        // Fast path: never start work for an already-cancelled run.
        if token.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let mut completed: u32 = 0;
        loop {
            match self.send_one_attempt(token, request) {
                AttemptResult::Cancelled => return Err(ExecutorError::Cancelled),
                AttemptResult::Network { retryable } => {
                    if retryable && completed + 1 < MAX_SEND_ATTEMPTS {
                        completed += 1;
                        if !sleep_cancellable(token, retry_delay(None, completed - 1)) {
                            return Err(ExecutorError::Cancelled);
                        }
                        continue;
                    }
                    return Err(ExecutorError::Network);
                }
                AttemptResult::Success(body) => return Ok(PostOutcome::Success(body)),
                AttemptResult::FailureStatus {
                    status,
                    header_retry_after,
                    body,
                } => {
                    let merged = header_retry_after.or_else(|| extract_retry_after(&body));
                    if is_retryable_status(status) && completed + 1 < MAX_SEND_ATTEMPTS {
                        completed += 1;
                        if !sleep_cancellable(token, retry_delay(merged, completed - 1)) {
                            return Err(ExecutorError::Cancelled);
                        }
                        continue;
                    }
                    return Ok(PostOutcome::Failure(ErrorSnapshot {
                        status,
                        retry_after_secs: merged,
                    }));
                }
            }
        }
    }

    /// Run one attempt on the runtime, aborting promptly on cancellation.
    fn send_one_attempt(
        &self,
        token: &CancellationToken,
        request: &PostRequest<'_>,
    ) -> AttemptResult {
        let client = self.inner.clone();
        let url = request.url.clone();
        let credential = OwnedCredential::from(request.credential);
        let extra: Vec<(&'static str, &'static str)> = request.extra_headers.to_vec();
        let body: Vec<u8> = request.body.to_vec();
        let timeout = request.timeout;
        let (tx, rx) = mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            let outcome = attempt_once(&client, &url, credential, &extra, body, timeout).await;
            let _ = tx.send(outcome);
        });
        loop {
            match rx.recv_timeout(FLIGHT_POLL_INTERVAL) {
                Ok(outcome) => {
                    // A result racing a concurrent cancel still counts as
                    // cancelled: an abandoned call must never dispatch.
                    if token.is_cancelled() {
                        return AttemptResult::Cancelled;
                    }
                    return outcome;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if token.is_cancelled() {
                        // Drop the in-flight request future: the socket
                        // aborts instead of running to the wall-clock timeout.
                        handle.abort();
                        return AttemptResult::Cancelled;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The task died without sending (runtime panic path):
                    // fail closed as a non-retryable transport error rather
                    // than hanging or inventing a response.
                    if token.is_cancelled() {
                        return AttemptResult::Cancelled;
                    }
                    return AttemptResult::Network { retryable: false };
                }
            }
        }
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Owned per-attempt result, sent back to the driver thread.
#[derive(Debug)]
enum AttemptResult {
    /// Token fired before or during the attempt.
    Cancelled,
    /// Transport failure with its retryability pre-classified.
    Network {
        /// True for refused/DNS/timeout/reset-style errors.
        retryable: bool,
    },
    /// 2xx with the full response body.
    Success(Vec<u8>),
    /// Non-2xx with the header hint and bounded body.
    FailureStatus {
        /// HTTP status code.
        status: u16,
        /// `Retry-After` response header parsed as integer seconds.
        header_retry_after: Option<u64>,
        /// Bounded raw error body.
        body: Vec<u8>,
    },
}

/// One HTTP round trip on the runtime: build, send, read.
/// Never touches the token (the driver owns cancellation).
async fn attempt_once(
    client: &reqwest::Client,
    url: &str,
    credential: OwnedCredential,
    extra_headers: &[(&'static str, &'static str)],
    body: Vec<u8>,
    timeout: Option<Duration>,
) -> AttemptResult {
    let mut builder = client.post(url);
    builder = match &credential {
        OwnedCredential::Bearer(value) => builder.bearer_auth(value),
        OwnedCredential::Header { name, value } => builder.header(*name, value.as_str()),
    };
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    // Pre-serialized JSON wire bytes; `.json()` would serialize identically,
    // but the bytes are built once per request and cloned per attempt.
    builder = builder
        .header("content-type", "application/json")
        .body(body);
    if let Some(bound) = timeout {
        builder = builder.timeout(bound);
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(err) => {
            return AttemptResult::Network {
                retryable: is_retryable_network_error(&err),
            };
        }
    };
    let status = response.status().as_u16();
    if response.status().is_success() {
        match response.bytes().await {
            Ok(bytes) => return AttemptResult::Success(bytes.to_vec()),
            // A stream torn mid-body is reset-class: retryable.
            Err(_) => return AttemptResult::Network { retryable: true },
        }
    }
    let header_retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let body = match response.bytes().await {
        Ok(bytes) => bounded_body(bytes.as_ref()),
        // An unreadable error body still classifies by status; the header
        // hint (if any) and the computed backoff carry the retry.
        Err(_) => Vec::new(),
    };
    AttemptResult::FailureStatus {
        status,
        header_retry_after,
        body,
    }
}

/// Copy up to [`MAX_ERROR_BODY_BYTES`] for field extraction.
fn bounded_body(body: &[u8]) -> Vec<u8> {
    let len = body.len().min(MAX_ERROR_BODY_BYTES);
    body[..len].to_vec()
}

/// True for retryable transport failures: refused connections, DNS failures,
/// timeouts, and torn streams. Builder errors (programming bugs: bad URL,
/// invalid headers) fail fast instead of burning backoff.
fn is_retryable_network_error(err: &reqwest::Error) -> bool {
    // `is_request` covers hyper dispatch/reset errors; `is_body` covers a
    // stream torn while reading. Decode errors cannot occur here (bodies are
    // consumed as raw bytes), and status errors cannot occur (statuses are
    // read, never raised via `error_for_status`).
    err.is_connect() || err.is_timeout() || err.is_body() || err.is_request()
}

/// Body field names honored as `Retry-After` hints (integer seconds).
const RETRY_AFTER_FIELDS: [&str; 4] = [
    "retry_after",
    "retry-after",
    "retryAfter",
    "retry_after_seconds",
];

/// Extract a `Retry-After` hint (integer seconds) from common JSON error-body
/// shapes: a top-level or `error`-nested body field (see
/// [`RETRY_AFTER_FIELDS`]) holding an integer or an integer string. Anything
/// else (floats, HTTP dates, embedded message text) yields `None` and the
/// caller falls back to the header or computed backoff.
pub(crate) fn extract_retry_after(body: &[u8]) -> Option<u64> {
    if body.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    for scope in [
        &value,
        value.get("error").unwrap_or(&serde_json::Value::Null),
    ] {
        let object = scope.as_object()?;
        for field in RETRY_AFTER_FIELDS {
            if let Some(found) = object.get(field).and_then(as_u64_seconds) {
                return Some(found);
            }
        }
    }
    None
}

/// Read an integer-seconds value: JSON integers, or strings holding one
/// (`"2"`, `" 2 "`). Floats and dates are deliberately not honored.
fn as_u64_seconds(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Merge header and body hints for tests and documentation: the response
/// header wins over the body field; either may be absent. Test-only: the
/// production loop merges inline.
#[cfg(test)]
pub(crate) fn effective_retry_after(header_secs: Option<u64>, body: &[u8]) -> Option<u64> {
    header_secs.or_else(|| extract_retry_after(body))
}

/// Sleep `duration` in slices, returning `false` the moment `token` fires so
/// a cancel during backoff aborts immediately instead of sleeping it out.
fn sleep_cancellable(token: &CancellationToken, duration: Duration) -> bool {
    let deadline = std::time::Instant::now() + duration;
    loop {
        if token.is_cancelled() {
            return false;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return true;
        }
        let remaining = deadline - now;
        std::thread::sleep(remaining.min(BACKOFF_POLL_INTERVAL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn idle_token() -> CancellationToken {
        CancellationToken::new()
    }

    fn post_to(url: String, body: &'static [u8]) -> PostRequest<'static> {
        // Test bodies are `'static` literals, so no leaking is involved.
        PostRequest {
            url,
            credential: Credential::Bearer("sk-secret-example"),
            extra_headers: &[],
            body,
            timeout: None,
        }
    }

    /// Serve scripted `(status, body, retry-after-header)` responses in order,
    /// counting accepted connections.
    fn spawn_scripted(
        responses: Vec<(u16, Vec<u8>, Option<String>)>,
    ) -> (String, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let server = std::thread::spawn(move || {
            for (status, body, retry_after) in responses {
                let (mut stream, _) = listener.accept().expect("accept connection");
                seen.fetch_add(1, Ordering::SeqCst);
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&buf[..n]);
                            if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                // Drain any pipelined request body so a persistent client
                // cannot stall the next accept on this single-threaded loop.
                let reason = match status {
                    400 => "Bad Request",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "OK",
                };
                let header = retry_after
                    .map(|value| format!("Retry-After: {value}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                     {header}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), count, server)
    }

    #[test]
    fn retry_after_header_and_body_are_honored_and_capped() {
        let client = HttpClient::new();

        // Header `2` is honored verbatim: two backoffs of exactly 2 s across
        // the three bounded attempts (~4 s total, well under any wall clock).
        let (endpoint, count, server) =
            spawn_scripted(vec![(429, Vec::new(), Some("2".to_string())); 3]);
        let started = std::time::Instant::now();
        let outcome = client
            .post(&idle_token(), &post_to(endpoint, b"{}"))
            .expect("transport resolves");
        let elapsed = started.elapsed();
        let PostOutcome::Failure(snapshot) = outcome else {
            panic!("exhausted 429s must surface as a failure snapshot");
        };
        assert_eq!(snapshot.status, 429);
        assert_eq!(snapshot.retry_after_secs, Some(2));
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert!(
            elapsed >= Duration::from_secs(4),
            "header Retry-After: 2 must be honored (~4 s over two backoffs), waited {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "honored backoff must stay near the hint, waited {elapsed:?}"
        );
        server.join().expect("server thread joins");

        // A body field is honored the same way when the header is absent.
        let body = br#"{"error":{"message":"slow down","retry_after":1}}"#.to_vec();
        let (endpoint, count, server) = spawn_scripted(vec![(429, body, None); 3]);
        let started = std::time::Instant::now();
        let outcome = client
            .post(&idle_token(), &post_to(endpoint, b"{}"))
            .expect("transport resolves");
        let elapsed = started.elapsed();
        let PostOutcome::Failure(snapshot) = outcome else {
            panic!("exhausted 429s must surface as a failure snapshot");
        };
        assert_eq!(snapshot.retry_after_secs, Some(1));
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert!(
            elapsed >= Duration::from_secs(2),
            "body retry_after: 1 must be honored (~2 s over two backoffs), waited {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "honored backoff must stay near the hint, waited {elapsed:?}"
        );
        server.join().expect("server thread joins");

        // Pure cap and precedence checks (no 30 s sleeps): 9999 caps at 30 s,
        // the header wins over the body, and a lone body field is honored.
        assert_eq!(
            retry_delay(effective_retry_after(Some(9999), b"{}"), 0),
            Duration::from_secs(30)
        );
        assert_eq!(
            effective_retry_after(Some(7), br#"{"retry_after":1}"#),
            Some(7)
        );
        assert_eq!(
            effective_retry_after(None, br#"{"error":{"retryAfter":3}}"#),
            Some(3)
        );
        assert_eq!(effective_retry_after(None, b"not json"), None);
        // The merged hint, like the header, is capped rather than slept raw.
        assert_eq!(
            retry_delay(effective_retry_after(None, br#"{"retry_after":9999}"#), 0),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn extract_retry_after_reads_common_shapes_only() {
        assert_eq!(extract_retry_after(b""), None);
        assert_eq!(extract_retry_after(b"not json"), None);
        assert_eq!(extract_retry_after(br#"{"ok":true}"#), None);
        assert_eq!(extract_retry_after(br#"{"retry_after":5}"#), Some(5));
        assert_eq!(extract_retry_after(br#"{"retry-after":6}"#), Some(6));
        assert_eq!(extract_retry_after(br#"{"retryAfter":7}"#), Some(7));
        assert_eq!(
            extract_retry_after(br#"{"retry_after_seconds":8}"#),
            Some(8)
        );
        assert_eq!(
            extract_retry_after(br#"{"error":{"retry_after":9}}"#),
            Some(9)
        );
        assert_eq!(extract_retry_after(br#"{"retry_after":"11"}"#), Some(11));
        // Floats, dates, and message-embedded delays are not hints.
        assert_eq!(extract_retry_after(br#"{"retry_after":2.5}"#), None);
        assert_eq!(
            extract_retry_after(br#"{"retry_after":"Wed, 21 Oct 2015 07:28:00 GMT"}"#),
            None
        );
        assert_eq!(
            extract_retry_after(br#"{"error":{"message":"try again in 2s"}}"#),
            None
        );
    }

    #[test]
    fn non_retryable_snapshot_returns_without_retry() {
        let client = HttpClient::new();
        let (endpoint, count, server) = spawn_scripted(vec![(400, b"{}".to_vec(), None)]);
        let outcome = client
            .post(&idle_token(), &post_to(endpoint, b"{}"))
            .expect("transport resolves");
        let PostOutcome::Failure(snapshot) = outcome else {
            panic!("400 must surface as a failure snapshot");
        };
        assert_eq!(snapshot.status, 400);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.join().expect("server thread joins");
    }

    #[test]
    fn cancelled_token_aborts_before_any_attempt() {
        let client = HttpClient::new();
        let (endpoint, count, server) = spawn_scripted(vec![(200, b"{}".to_vec(), None)]);
        let token = CancellationToken::new();
        token.cancel();
        let result = client.post(&token, &post_to(endpoint, b"{}"));
        assert!(
            matches!(result, Err(ExecutorError::Cancelled)),
            "pre-cancelled token must abort, got {result:?}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(server);
    }
}
