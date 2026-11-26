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

use nexora_core::provider::{
    is_retryable_status, retry_delay, CancellationToken, ExecutorError, MAX_RETRY_DELAY_SECS,
    MAX_SEND_ATTEMPTS,
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
    /// Runtime-configured extra headers applied after [`Self::extra_headers`]
    /// (the user-configured OpenAI-compatible endpoint headers). Borrowed for
    /// the call only; validated by the owning executor before sending.
    pub extra_headers_owned: &'a [(String, String)],
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
    /// True when a client-error status (400/404/413/422) carries an error
    /// body naming the context window or token limit. Providers map this to
    /// a category-only context-length error; the raw body itself is consumed
    /// here and never stored or surfaced.
    pub is_context_length_exceeded: bool,
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

/// Secret-free transport failure category for connection diagnostic probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportProbeError {
    Timeout,
    TlsError,
    NotRunning,
    DnsOrProxy,
    Cancelled,
}

/// One parsed Server-Sent Event frame (`event:` optional + joined `data:` lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseFrame {
    /// Optional `event:` field value for this frame.
    pub event: Option<String>,
    /// Payload lines joined with `\n`.
    pub data: String,
}

/// Outcome of [`HttpClient::post_stream`].
#[derive(Debug)]
pub(crate) enum StreamOutcome {
    /// HTTP 2xx stream ran to EOF.
    Completed,
    /// Non-2xx response after pre-stream retries (never leaks raw body).
    Failure(ErrorSnapshot),
}

/// Error returned by [`HttpClient::post_stream`], preserving caller callback errors.
#[derive(Debug)]
pub(crate) enum StreamPostError<E> {
    /// Transport/cancellation error.
    Transport(ExecutorError),
    /// Frame consumer rejected or terminated the stream with provider error `E`.
    Handler(E),
}

/// Incremental UTF-8 Server-Sent Events parser handling partial TCP chunk boundaries.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data_lines: Vec<String>,
}

impl SseParser {
    /// Create a fresh SSE parser.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of raw bytes and emit any complete SSE frames.
    pub(crate) fn feed<E>(
        &mut self,
        chunk: &[u8],
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> Result<(), E> {
        self.buffer.extend_from_slice(chunk);
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line_bytes = self.buffer.drain(..=pos).collect::<Vec<u8>>();
            // Drop trailing '\n' and optional '\r'.
            if matches!(line_bytes.last(), Some(b'\n')) {
                line_bytes.pop();
            }
            if matches!(line_bytes.last(), Some(b'\r')) {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, on_frame)?;
        }
        Ok(())
    }

    /// Flush any buffered final line/frame at stream EOF.
    pub(crate) fn finish<E>(
        &mut self,
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> Result<(), E> {
        if !self.buffer.is_empty() {
            let mut line_bytes = std::mem::take(&mut self.buffer);
            if matches!(line_bytes.last(), Some(b'\r')) {
                line_bytes.pop();
            }
            let line = String::from_utf8_lossy(&line_bytes);
            self.process_line(&line, on_frame)?;
        }
        self.flush_frame(on_frame)
    }

    fn process_line<E>(
        &mut self,
        line: &str,
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> Result<(), E> {
        if line.is_empty() {
            return self.flush_frame(on_frame);
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((f, rest)) => (f, rest.strip_prefix(' ').unwrap_or(rest)),
            None => (line, ""),
        };
        match field {
            "event" => {
                self.event = Some(value.to_string());
            }
            "data" => {
                self.data_lines.push(value.to_string());
            }
            _ => {}
        }
        Ok(())
    }

    fn flush_frame<E>(
        &mut self,
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.data_lines.is_empty() {
            self.event = None;
            return Ok(());
        }
        let frame = SseFrame {
            event: self.event.take(),
            data: std::mem::take(&mut self.data_lines).join("\n"),
        };
        on_frame(frame)
    }
}

/// Shared async HTTP client (one per executor, reused across requests).
pub(crate) struct HttpClient {
    inner: reqwest::Client,
    stream_attempts: u32,
}

impl HttpClient {
    /// Build the shared client (rustls, connection pooling).
    pub(crate) fn new() -> Self {
        Self {
            inner: reqwest::Client::new(),
            stream_attempts: MAX_SEND_ATTEMPTS,
        }
    }

    /// Fix the set of hosts to which a subscription bearer can be sent.
    pub(crate) fn single_stream_attempt() -> Option<Self> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()
            .map(|inner| Self {
                inner,
                stream_attempts: 1,
            })
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
                        is_context_length_exceeded: body_signals_context_length(status, &body),
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
        let extra_owned: Vec<(String, String)> = request.extra_headers_owned.to_vec();
        let body: Vec<u8> = request.body.to_vec();
        let timeout = request.timeout;
        let (tx, rx) = mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            let outcome = attempt_once(
                &client,
                &url,
                credential,
                &extra,
                &extra_owned,
                body,
                timeout,
            )
            .await;
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

    /// Perform a cancellable GET request for zero-token metadata endpoints.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError::Network`] on transport failures or
    /// [`ExecutorError::Cancelled`] when `token` fires.
    pub(crate) fn get_json(
        &self,
        token: &CancellationToken,
        url: &str,
        credential: Option<Credential<'_>>,
        extra_headers: &[(&'static str, &'static str)],
        extra_headers_owned: &[(String, String)],
        timeout: Option<Duration>,
    ) -> Result<PostOutcome, ExecutorError> {
        if token.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        let client = self.inner.clone();
        let url_owned = url.to_string();
        let owned_cred = credential.map(OwnedCredential::from);
        let extra = extra_headers.to_vec();
        let extra_owned = extra_headers_owned.to_vec();
        let (tx, rx) = mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            let outcome = get_once(
                &client,
                &url_owned,
                owned_cred,
                &extra,
                &extra_owned,
                timeout,
            )
            .await;
            let _ = tx.send(outcome);
        });
        loop {
            match rx.recv_timeout(FLIGHT_POLL_INTERVAL) {
                Ok(outcome) => {
                    if token.is_cancelled() {
                        return Err(ExecutorError::Cancelled);
                    }
                    return match outcome {
                        AttemptResult::Cancelled => Err(ExecutorError::Cancelled),
                        AttemptResult::Network { .. } => Err(ExecutorError::Network),
                        AttemptResult::Success(body) => Ok(PostOutcome::Success(body)),
                        AttemptResult::FailureStatus {
                            status,
                            header_retry_after,
                            body,
                        } => Ok(PostOutcome::Failure(ErrorSnapshot {
                            status,
                            retry_after_secs: header_retry_after
                                .or_else(|| extract_retry_after(&body)),
                            is_context_length_exceeded: body_signals_context_length(status, &body),
                        })),
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if token.is_cancelled() {
                        handle.abort();
                        return Err(ExecutorError::Cancelled);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if token.is_cancelled() {
                        return Err(ExecutorError::Cancelled);
                    }
                    return Err(ExecutorError::Network);
                }
            }
        }
    }

    /// Send a single bounded `GET` diagnostic probe with both connect and
    /// request timeout, classifying transport errors into a secret-free
    /// [`TransportProbeError`] without ever surfacing URLs or raw bodies.
    pub(crate) fn get_json_probe(
        &self,
        token: &CancellationToken,
        url: &str,
        credential: Option<Credential<'_>>,
        extra_headers: &[(&'static str, &'static str)],
        extra_headers_owned: &[(String, String)],
        timeout: Duration,
    ) -> Result<PostOutcome, TransportProbeError> {
        if token.is_cancelled() {
            return Err(TransportProbeError::Cancelled);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| self.inner.clone());
        let url_owned = url.to_string();
        let owned_cred = credential.map(OwnedCredential::from);
        let extra = extra_headers.to_vec();
        let extra_owned = extra_headers_owned.to_vec();
        let (tx, rx) = mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            let outcome = get_once_probe(
                &client,
                &url_owned,
                owned_cred,
                &extra,
                &extra_owned,
                timeout,
            )
            .await;
            let _ = tx.send(outcome);
        });
        loop {
            match rx.recv_timeout(FLIGHT_POLL_INTERVAL) {
                Ok(outcome) => {
                    if token.is_cancelled() {
                        return Err(TransportProbeError::Cancelled);
                    }
                    return outcome;
                }
                Err(RecvTimeoutError::Timeout) => {
                    if token.is_cancelled() {
                        handle.abort();
                        return Err(TransportProbeError::Cancelled);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if token.is_cancelled() {
                        return Err(TransportProbeError::Cancelled);
                    }
                    return Err(TransportProbeError::DnsOrProxy);
                }
            }
        }
    }

    /// Send an SSE streaming `POST` request with deterministic retry/backoff on
    /// pre-stream errors (`429`/`5xx`/network) and immediate cancellation drop.
    ///
    /// Once HTTP 2xx headers arrive and streaming starts, no further retries are
    /// attempted so partial deltas are never duplicated.
    ///
    /// # Errors
    ///
    /// Returns [`StreamPostError::Transport`] on network/cancellation failure,
    /// or [`StreamPostError::Handler`] if `on_frame` returns `Err(E)`.
    pub(crate) fn post_stream<E>(
        &self,
        token: &CancellationToken,
        request: &PostRequest<'_>,
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> Result<StreamOutcome, StreamPostError<E>> {
        if token.is_cancelled() {
            return Err(StreamPostError::Transport(ExecutorError::Cancelled));
        }
        let mut completed: u32 = 0;
        loop {
            match self.stream_one_attempt(token, request, on_frame) {
                StreamAttemptOutcome::Cancelled => {
                    return Err(StreamPostError::Transport(ExecutorError::Cancelled));
                }
                StreamAttemptOutcome::Handler(err) => {
                    return Err(StreamPostError::Handler(err));
                }
                StreamAttemptOutcome::PreNetwork { retryable } => {
                    if retryable && completed + 1 < self.stream_attempts {
                        completed += 1;
                        if !sleep_cancellable(token, retry_delay(None, completed - 1)) {
                            return Err(StreamPostError::Transport(ExecutorError::Cancelled));
                        }
                        continue;
                    }
                    return Err(StreamPostError::Transport(ExecutorError::Network));
                }
                StreamAttemptOutcome::PreFailureStatus {
                    status,
                    header_retry_after,
                    body,
                } => {
                    let merged = header_retry_after.or_else(|| extract_retry_after(&body));
                    if is_retryable_status(status) && completed + 1 < self.stream_attempts {
                        completed += 1;
                        if !sleep_cancellable(token, retry_delay(merged, completed - 1)) {
                            return Err(StreamPostError::Transport(ExecutorError::Cancelled));
                        }
                        continue;
                    }
                    return Ok(StreamOutcome::Failure(ErrorSnapshot {
                        status,
                        retry_after_secs: merged,
                        is_context_length_exceeded: body_signals_context_length(status, &body),
                    }));
                }
                StreamAttemptOutcome::StreamNetwork => {
                    return Err(StreamPostError::Transport(ExecutorError::Network));
                }
                StreamAttemptOutcome::Completed => return Ok(StreamOutcome::Completed),
            }
        }
    }

    fn stream_one_attempt<E>(
        &self,
        token: &CancellationToken,
        request: &PostRequest<'_>,
        on_frame: &mut dyn FnMut(SseFrame) -> Result<(), E>,
    ) -> StreamAttemptOutcome<E> {
        let client = self.inner.clone();
        let url = request.url.clone();
        let credential = OwnedCredential::from(request.credential);
        let extra: Vec<(&'static str, &'static str)> = request.extra_headers.to_vec();
        let extra_owned: Vec<(String, String)> = request.extra_headers_owned.to_vec();
        let body: Vec<u8> = request.body.to_vec();
        let timeout = request.timeout;
        let (tx, rx) = mpsc::channel();
        let handle = tauri::async_runtime::spawn(async move {
            stream_attempt_task(
                &client,
                &url,
                credential,
                &extra,
                &extra_owned,
                body,
                timeout,
                &tx,
            )
            .await;
        });

        let mut parser = SseParser::new();
        let mut started = false;
        loop {
            match rx.recv_timeout(FLIGHT_POLL_INTERVAL) {
                Ok(StreamChunkMsg::Started) => {
                    if token.is_cancelled() {
                        handle.abort();
                        return StreamAttemptOutcome::Cancelled;
                    }
                    started = true;
                }
                Ok(StreamChunkMsg::Chunk(bytes)) => {
                    if token.is_cancelled() {
                        handle.abort();
                        return StreamAttemptOutcome::Cancelled;
                    }
                    if let Err(err) = parser.feed(&bytes, on_frame) {
                        handle.abort();
                        return StreamAttemptOutcome::Handler(err);
                    }
                }
                Ok(StreamChunkMsg::Eof) => {
                    if token.is_cancelled() {
                        return StreamAttemptOutcome::Cancelled;
                    }
                    if let Err(err) = parser.finish(on_frame) {
                        return StreamAttemptOutcome::Handler(err);
                    }
                    return StreamAttemptOutcome::Completed;
                }
                Ok(StreamChunkMsg::PreNetwork { retryable }) => {
                    if token.is_cancelled() {
                        return StreamAttemptOutcome::Cancelled;
                    }
                    return StreamAttemptOutcome::PreNetwork { retryable };
                }
                Ok(StreamChunkMsg::PreFailureStatus {
                    status,
                    header_retry_after,
                    body,
                }) => {
                    if token.is_cancelled() {
                        return StreamAttemptOutcome::Cancelled;
                    }
                    return StreamAttemptOutcome::PreFailureStatus {
                        status,
                        header_retry_after,
                        body,
                    };
                }
                Ok(StreamChunkMsg::StreamNetwork) => {
                    if token.is_cancelled() {
                        return StreamAttemptOutcome::Cancelled;
                    }
                    return if started {
                        StreamAttemptOutcome::StreamNetwork
                    } else {
                        StreamAttemptOutcome::PreNetwork { retryable: true }
                    };
                }
                Err(RecvTimeoutError::Timeout) => {
                    if token.is_cancelled() {
                        handle.abort();
                        return StreamAttemptOutcome::Cancelled;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if token.is_cancelled() {
                        return StreamAttemptOutcome::Cancelled;
                    }
                    return if started {
                        StreamAttemptOutcome::StreamNetwork
                    } else {
                        StreamAttemptOutcome::PreNetwork { retryable: false }
                    };
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

/// Messages sent from a streaming runtime task to the driver thread.
#[derive(Debug)]
enum StreamChunkMsg {
    Started,
    Chunk(Vec<u8>),
    Eof,
    PreNetwork {
        retryable: bool,
    },
    PreFailureStatus {
        status: u16,
        header_retry_after: Option<u64>,
        body: Vec<u8>,
    },
    StreamNetwork,
}

/// Internal per-attempt outcome for [`HttpClient::post_stream`].
enum StreamAttemptOutcome<E> {
    Cancelled,
    Completed,
    Handler(E),
    PreNetwork {
        retryable: bool,
    },
    PreFailureStatus {
        status: u16,
        header_retry_after: Option<u64>,
        body: Vec<u8>,
    },
    StreamNetwork,
}

fn apply_request_auth_headers(
    mut builder: reqwest::RequestBuilder,
    credential: Option<&OwnedCredential>,
    extra_headers: &[(&'static str, &'static str)],
    extra_headers_owned: &[(String, String)],
    timeout: Option<Duration>,
) -> reqwest::RequestBuilder {
    if let Some(cred) = credential {
        builder = match cred {
            OwnedCredential::Bearer(value) => builder.bearer_auth(value),
            OwnedCredential::Header { name, value } => builder.header(*name, value.as_str()),
        };
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    for (name, value) in extra_headers_owned {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if let Some(bound) = timeout {
        builder = builder.timeout(bound);
    }
    builder
}

fn parse_retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|secs| secs.min(MAX_RETRY_DELAY_SECS))
}

async fn get_once(
    client: &reqwest::Client,
    url: &str,
    credential: Option<OwnedCredential>,
    extra_headers: &[(&'static str, &'static str)],
    extra_headers_owned: &[(String, String)],
    timeout: Option<Duration>,
) -> AttemptResult {
    let builder = apply_request_auth_headers(
        client.get(url).header("accept", "application/json"),
        credential.as_ref(),
        extra_headers,
        extra_headers_owned,
        timeout,
    );
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
        return match response.bytes().await {
            Ok(bytes) => AttemptResult::Success(bytes.to_vec()),
            Err(_) => AttemptResult::Network { retryable: true },
        };
    }
    let header_retry_after = parse_retry_after_header(response.headers());
    let body = match response.bytes().await {
        Ok(bytes) => bounded_body(bytes.as_ref()),
        Err(_) => Vec::new(),
    };
    AttemptResult::FailureStatus {
        status,
        header_retry_after,
        body,
    }
}

async fn get_once_probe(
    client: &reqwest::Client,
    url: &str,
    credential: Option<OwnedCredential>,
    extra_headers: &[(&'static str, &'static str)],
    extra_headers_owned: &[(String, String)],
    timeout: Duration,
) -> Result<PostOutcome, TransportProbeError> {
    let builder = apply_request_auth_headers(
        client.get(url).header("accept", "application/json"),
        credential.as_ref(),
        extra_headers,
        extra_headers_owned,
        Some(timeout),
    );
    let response = match builder.send().await {
        Ok(response) => response,
        Err(err) => return Err(classify_reqwest_probe_error(&err)),
    };
    let status = response.status().as_u16();
    if response.status().is_success() {
        return match response.bytes().await {
            Ok(bytes) => Ok(PostOutcome::Success(bytes.to_vec())),
            Err(err) => Err(classify_reqwest_probe_error(&err)),
        };
    }
    let header_retry_after = parse_retry_after_header(response.headers());
    let body = match response.bytes().await {
        Ok(bytes) => bounded_body(bytes.as_ref()),
        Err(_) => Vec::new(),
    };
    Ok(PostOutcome::Failure(ErrorSnapshot {
        status,
        retry_after_secs: header_retry_after.or_else(|| extract_retry_after(&body)),
        is_context_length_exceeded: body_signals_context_length(status, &body),
    }))
}

fn classify_reqwest_probe_error(err: &reqwest::Error) -> TransportProbeError {
    let mut io_kind = None;
    let mut lowered_chain = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(source) = cur {
        if io_kind.is_none() {
            if let Some(io_err) = source.downcast_ref::<std::io::Error>() {
                io_kind = Some(io_err.kind());
            }
        }
        // Inspect the source chain strictly inside this private classifier;
        // the string is dropped immediately and never returned or logged.
        lowered_chain.push(' ');
        lowered_chain.push_str(&source.to_string().to_ascii_lowercase());
        cur = source.source();
    }
    classify_probe_error_details(err.is_timeout(), err.is_connect(), io_kind, &lowered_chain)
}

/// Pure, secret-free classifier mapping transport error signals to a
/// [`TransportProbeError`] category.
pub(crate) fn classify_probe_error_details(
    is_timeout: bool,
    _is_connect: bool,
    io_kind: Option<std::io::ErrorKind>,
    lowered_chain: &str,
) -> TransportProbeError {
    if is_timeout || matches!(io_kind, Some(std::io::ErrorKind::TimedOut)) {
        return TransportProbeError::Timeout;
    }
    if matches!(io_kind, Some(std::io::ErrorKind::ConnectionRefused))
        || lowered_chain.contains("connection refused")
        || lowered_chain.contains("actively refused")
    {
        return TransportProbeError::NotRunning;
    }
    if lowered_chain.contains("timed out") || lowered_chain.contains("timeout") {
        return TransportProbeError::Timeout;
    }
    if lowered_chain.contains("tls")
        || lowered_chain.contains("ssl")
        || lowered_chain.contains("certificate")
        || lowered_chain.contains("handshake")
        || lowered_chain.contains("rustls")
        || lowered_chain.contains("unknown issuer")
        || lowered_chain.contains("invalid peer")
    {
        return TransportProbeError::TlsError;
    }
    TransportProbeError::DnsOrProxy
}

#[allow(clippy::too_many_arguments)]
async fn stream_attempt_task(
    client: &reqwest::Client,
    url: &str,
    credential: OwnedCredential,
    extra_headers: &[(&'static str, &'static str)],
    extra_headers_owned: &[(String, String)],
    body: Vec<u8>,
    timeout: Option<Duration>,
    tx: &mpsc::Sender<StreamChunkMsg>,
) {
    let builder = apply_request_auth_headers(
        client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body),
        Some(&credential),
        extra_headers,
        extra_headers_owned,
        timeout,
    );
    let mut response = match builder.send().await {
        Ok(response) => response,
        Err(err) => {
            let _ = tx.send(StreamChunkMsg::PreNetwork {
                retryable: is_retryable_network_error(&err),
            });
            return;
        }
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let header_retry_after = parse_retry_after_header(response.headers());
        let body = match response.bytes().await {
            Ok(bytes) => bounded_body(bytes.as_ref()),
            Err(_) => Vec::new(),
        };
        let _ = tx.send(StreamChunkMsg::PreFailureStatus {
            status,
            header_retry_after,
            body,
        });
        return;
    }
    if tx.send(StreamChunkMsg::Started).is_err() {
        return;
    }
    loop {
        match response.chunk().await {
            Ok(Some(bytes)) => {
                if tx.send(StreamChunkMsg::Chunk(bytes.to_vec())).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = tx.send(StreamChunkMsg::Eof);
                return;
            }
            Err(_) => {
                let _ = tx.send(StreamChunkMsg::StreamNetwork);
                return;
            }
        }
    }
}

/// One HTTP round trip on the runtime: build, send, read.
/// Never touches the token (the driver owns cancellation).
async fn attempt_once(
    client: &reqwest::Client,
    url: &str,
    credential: OwnedCredential,
    extra_headers: &[(&'static str, &'static str)],
    extra_headers_owned: &[(String, String)],
    body: Vec<u8>,
    timeout: Option<Duration>,
) -> AttemptResult {
    // Pre-serialized JSON wire bytes; `.json()` would serialize identically,
    // but the bytes are built once per request and cloned per attempt.
    let builder = apply_request_auth_headers(
        client
            .post(url)
            .header("content-type", "application/json")
            .body(body),
        Some(&credential),
        extra_headers,
        extra_headers_owned,
        timeout,
    );
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
    let header_retry_after = parse_retry_after_header(response.headers());
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
/// caller falls back to the header or computed backoff. Returned hints are
/// capped at [`MAX_RETRY_DELAY_SECS`] so a hostile or malfunctioning server
/// cannot inflate the backoff or the surfaced category.
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
                return Some(found.min(MAX_RETRY_DELAY_SECS));
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

/// Statuses whose bodies may carry a context-length report: malformed or
/// oversized requests (400/404/413/422). Retryable statuses (429/5xx) never
/// count: their bodies describe rate limits or outages, not the window.
fn is_context_length_status(status: u16) -> bool {
    matches!(status, 400 | 404 | 413 | 422)
}

/// Substrings naming an exhausted model context window, matched
/// ASCII-case-insensitively against the raw error body. The list stays narrow
/// on purpose: a genuine malformed request (unknown model, bad schema) names
/// none of these and keeps its `InvalidRequest` classification.
const CONTEXT_LENGTH_MARKERS: &[&str] = &[
    "context_length_exceeded",
    "context-length-exceeded",
    "context length",
    "maximum context",
    "max context",
    "context window",
    "prompt is too long",
    "prompt too long",
    "prompt_too_long",
    "too many tokens",
    "token limit",
    "maximum tokens",
    "input too long",
    "input exceeds",
    "exceeds the maximum",
    "exceeded maximum",
];

/// True when `status` is a client-error shape and `body` names the context
/// window or token limit. Category-only: the body text is scanned here and
/// only the boolean leaves this module.
pub(crate) fn body_signals_context_length(status: u16, body: &[u8]) -> bool {
    if !is_context_length_status(status) || body.is_empty() {
        return false;
    }
    let text = String::from_utf8_lossy(body);
    let bytes = text.as_bytes();
    for marker in CONTEXT_LENGTH_MARKERS {
        let needle = marker.as_bytes();
        if needle.len() > bytes.len() {
            continue;
        }
        // ASCII case-insensitive substring search without allocating a
        // lowered copy of the whole body (bodies are bounded, markers tiny).
        for start in 0..=bytes.len() - needle.len() {
            let mut matched = true;
            for (offset, &expected) in needle.iter().enumerate() {
                if bytes[start + offset].to_ascii_lowercase() != expected {
                    matched = false;
                    break;
                }
            }
            if matched {
                return true;
            }
        }
    }
    false
}

/// Strip secrets from `url` before it can reach error text, logs, or events.
///
/// Removes embedded userinfo (`user:pass@`) and drops the query and fragment
/// (`?...` / `#...`), keeping only scheme, host (plus port), and path. The
/// transport never surfaces URLs in errors at all — this is the single
/// sanitizer applied on any path where a URL must be rendered for diagnostics,
/// so a secret-bearing endpoint can never leak through error text.
pub(crate) fn sanitize_url(url: &str) -> String {
    // Split the scheme without allocating a lowered copy of the whole URL.
    let (scheme, after_scheme) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    // Authority runs to the first path/query/fragment separator.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let after_authority = &after_scheme[authority_end..];
    // Drop userinfo: keep only what follows the last `@`.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // Keep only the path: cut at the first query/fragment separator.
    let path_end = after_authority
        .find(['?', '#'])
        .unwrap_or(after_authority.len());
    let path = &after_authority[..path_end];
    match scheme {
        Some(scheme) => format!("{scheme}://{host_port}{path}"),
        None => format!("{host_port}{path}"),
    }
}

/// Unified HTTP status → [`ExecutorError`] category table.
///
/// Single reference mapping shared by every provider executor
/// (`OpenAI`, `Anthropic`, `Gemini`, and the `OpenAI`-compatible path): each
/// provider's own `classify_status` delegates to this table so a status means
/// the same category everywhere. There is deliberately no cross-provider
/// fallback — an unknown provider resolves to `None` and never tries another
/// provider.
///
/// Mapping:
/// - 401/403 → `Authentication` (credential rejected or forbidden);
/// - 402 → `PaymentRequired` (valid credential, insufficient credits/quota);
/// - 413 → `ContextLengthExceeded` (payload too large is always a window
///   overflow, even without body language);
/// - 400/404 → `InvalidRequest`, unless `is_context_length_exceeded` (the
///   400-classifier: a client-error body naming the window or token limit)
///   promotes to `ContextLengthExceeded`;
/// - 429 → `RateLimited`, carrying the merged `Retry-After` hint capped at
///   [`MAX_RETRY_DELAY_SECS`];
/// - 5xx → `ProviderUnavailable`;
/// - anything else (including 422 without window language) → `Failure`.
///
/// Category-only and secret-free: the status, the capped hint, and the
/// precomputed context flag are the only inputs — no credential, URL, request
/// payload, or raw body text.
pub(crate) fn map_http_error(
    status: u16,
    retry_after_secs: Option<u64>,
    is_context_length_exceeded: bool,
) -> ExecutorError {
    if is_context_length_exceeded {
        return ExecutorError::ContextLengthExceeded;
    }
    match status {
        401 | 403 => ExecutorError::Authentication,
        402 => ExecutorError::PaymentRequired,
        // Payload-too-large is a window overflow by definition.
        413 => ExecutorError::ContextLengthExceeded,
        400 | 404 => ExecutorError::InvalidRequest,
        429 => ExecutorError::RateLimited {
            retry_after_secs: retry_after_secs.map(|secs| secs.min(MAX_RETRY_DELAY_SECS)),
        },
        500..=599 => ExecutorError::ProviderUnavailable,
        _ => ExecutorError::Failure,
    }
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
            extra_headers_owned: &[],
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
    fn context_length_body_flags_snapshot_without_retry() {
        // A 400 naming the context window flags the snapshot for
        // context-length classification — and, like every non-retryable
        // status, fires exactly once (no elapsed assert: this is a
        // classification proof, not a timing proof).
        let client = HttpClient::new();
        let body = br#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 128000 tokens."}}"#.to_vec();
        let (endpoint, count, server) = spawn_scripted(vec![(400, body, None)]);
        let outcome = client
            .post(&idle_token(), &post_to(endpoint, b"{}"))
            .expect("transport resolves");
        let PostOutcome::Failure(snapshot) = outcome else {
            panic!("400 must surface as a failure snapshot");
        };
        assert_eq!(snapshot.status, 400);
        assert!(snapshot.is_context_length_exceeded);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.join().expect("server thread joins");

        // A plain 400 without window language stays unflagged.
        let (endpoint, count, server) = spawn_scripted(vec![(400, b"{}".to_vec(), None)]);
        let outcome = client
            .post(&idle_token(), &post_to(endpoint, b"{}"))
            .expect("transport resolves");
        let PostOutcome::Failure(snapshot) = outcome else {
            panic!("400 must surface as a failure snapshot");
        };
        assert!(!snapshot.is_context_length_exceeded);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.join().expect("server thread joins");
    }

    #[test]
    fn body_signals_context_length_matches_window_language_only() {
        // Positive shapes across providers (case-insensitive).
        for body in [
            br#"{"code":"context_length_exceeded"}"#.as_slice(),
            br#"{"error":"Maximum context length exceeded"}"#.as_slice(),
            br#"{"message":"prompt is too long"}"#.as_slice(),
            br#"{"message":"Too many tokens for this model"}"#.as_slice(),
            br#"{"message":"input exceeds the maximum allowed"}"#.as_slice(),
        ] {
            assert!(
                body_signals_context_length(400, body),
                "must flag: {}",
                String::from_utf8_lossy(body)
            );
        }
        // Negative shapes: genuine malformed requests, rate limits, outages.
        for body in [
            br#"{"error":"unknown model 'gpt-9'}"#.as_slice(),
            br#"{"error":"invalid_request_error: bad schema"}"#.as_slice(),
            br#"{"error":"rate limit exceeded, retry later"}"#.as_slice(),
            b"{}".as_slice(),
            b"".as_slice(),
        ] {
            assert!(
                !body_signals_context_length(400, body),
                "must not flag: {}",
                String::from_utf8_lossy(body)
            );
        }
        // Retryable statuses never count, even with window language.
        assert!(!body_signals_context_length(
            429,
            b"maximum context length is fine, slow down"
        ));
        assert!(!body_signals_context_length(500, b"context window intact"));
        // A bare parameter name is not an overflow report: a missing
        // `max_tokens` field is a malformed request, not an exhausted window.
        assert!(!body_signals_context_length(
            400,
            br#"{"error":"max_tokens parameter is required"}"#
        ));
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

    #[test]
    fn unified_status_table_maps_each_status_to_category() {
        // 401/403 → Authentication.
        assert!(matches!(
            map_http_error(401, None, false),
            ExecutorError::Authentication
        ));
        assert!(matches!(
            map_http_error(403, None, false),
            ExecutorError::Authentication
        ));
        // 402 → PaymentRequired.
        assert!(matches!(
            map_http_error(402, None, false),
            ExecutorError::PaymentRequired
        ));
        // 413 → ContextLengthExceeded even without body language.
        assert!(matches!(
            map_http_error(413, None, false),
            ExecutorError::ContextLengthExceeded
        ));
        // 400/404 → InvalidRequest without window language ...
        assert!(matches!(
            map_http_error(400, None, false),
            ExecutorError::InvalidRequest
        ));
        assert!(matches!(
            map_http_error(404, None, false),
            ExecutorError::InvalidRequest
        ));
        // ... and ContextLengthExceeded with it (the 400-classifier).
        assert!(matches!(
            map_http_error(400, None, true),
            ExecutorError::ContextLengthExceeded
        ));
        assert!(matches!(
            map_http_error(404, None, true),
            ExecutorError::ContextLengthExceeded
        ));
        // 429 → RateLimited, carrying the hint.
        assert!(matches!(
            map_http_error(429, Some(2), false),
            ExecutorError::RateLimited {
                retry_after_secs: Some(2)
            }
        ));
        assert!(matches!(
            map_http_error(429, None, false),
            ExecutorError::RateLimited {
                retry_after_secs: None
            }
        ));
        // 5xx → ProviderUnavailable.
        for status in [500, 502, 503, 599] {
            assert!(
                matches!(
                    map_http_error(status, None, false),
                    ExecutorError::ProviderUnavailable
                ),
                "status {status} must map to ProviderUnavailable"
            );
        }
        // Anything else (including a bare 422) → Failure.
        assert!(matches!(
            map_http_error(422, None, false),
            ExecutorError::Failure
        ));
        assert!(matches!(
            map_http_error(418, None, false),
            ExecutorError::Failure
        ));
        // A context flag promotes even a 422 to the window category.
        assert!(matches!(
            map_http_error(422, None, true),
            ExecutorError::ContextLengthExceeded
        ));
    }

    #[test]
    fn retry_after_hints_are_capped_at_30s() {
        // Body hints cap rather than pass through raw.
        assert_eq!(extract_retry_after(br#"{"retry_after":9999}"#), Some(30));
        assert_eq!(
            extract_retry_after(br#"{"error":{"retryAfter":120}}"#),
            Some(30)
        );
        // Small hints pass through verbatim.
        assert_eq!(extract_retry_after(br#"{"retry_after":5}"#), Some(5));
        // The unified mapper caps the carried hint the same way.
        assert!(matches!(
            map_http_error(429, Some(9999), false),
            ExecutorError::RateLimited {
                retry_after_secs: Some(30)
            }
        ));
        assert!(matches!(
            map_http_error(429, Some(7), false),
            ExecutorError::RateLimited {
                retry_after_secs: Some(7)
            }
        ));
    }

    #[test]
    fn sanitize_url_strips_userinfo_query_and_fragment() {
        assert_eq!(
            sanitize_url("https://user:pass@example.com/v1/chat?key=sk-live-sentinel-1#frag"),
            "https://example.com/v1/chat"
        );
        assert_eq!(
            sanitize_url("https://example.com/v1/chat?key=secret"),
            "https://example.com/v1/chat"
        );
        assert_eq!(
            sanitize_url("https://user@example.com/"),
            "https://example.com/"
        );
        assert_eq!(
            sanitize_url("https://example.com/v1/chat"),
            "https://example.com/v1/chat"
        );
        // A secret planted in userinfo, query, or fragment never survives.
        for url in [
            "https://sk-live-sentinel-2@example.com/v1",
            "https://example.com/v1?api_key=sk-live-sentinel-3",
            "https://example.com/v1#sk-live-sentinel-4",
        ] {
            let clean = sanitize_url(url);
            for sentinel in [
                "sk-live-sentinel-2",
                "sk-live-sentinel-3",
                "sk-live-sentinel-4",
            ] {
                // Only the planted URL's own sentinel must be absent; the
                // assertion below checks the matching one via `url.contains`.
                if url.contains(sentinel) {
                    assert!(
                        !clean.contains(sentinel),
                        "sanitized URL must not leak {sentinel:?}: {clean:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn sanitized_url_never_leaks_into_category_text() {
        // The boundary only ever carries categories: formatting every unified
        // category must never echo a URL-planted secret, even when the caller
        // sanitizes first (defense in depth — raw URLs never reach errors).
        const SECRET: &str = "sk-live-sentinel-9";
        let raw = format!("https://user:{SECRET}@example.com/v1?key={SECRET}");
        let clean = sanitize_url(&raw);
        assert!(!clean.contains(SECRET));
        for error in [
            map_http_error(401, None, false),
            map_http_error(402, None, false),
            map_http_error(413, None, false),
            map_http_error(400, None, false),
            map_http_error(429, Some(1), false),
            map_http_error(500, None, false),
            map_http_error(422, None, false),
            ExecutorError::Network,
            ExecutorError::Cancelled,
        ] {
            let message = format!("{error} {clean}");
            assert!(
                !message.contains(SECRET),
                "category text plus sanitized URL must stay secret-free: {message:?}"
            );
        }
    }

    #[test]
    fn sse_parser_handles_split_chunks_crlf_comments_and_multiline_data() {
        let mut parser = SseParser::new();
        let mut frames = Vec::new();
        let part1 = b": keepalive\r\nevent: content_block_de";
        let part2 = b"lta\r\ndata: {\"a\":1}\r\ndata: {\"b\":2}\r\n\r\ndata: tail";
        parser
            .feed::<()>(part1, &mut |frame| {
                frames.push(frame);
                Ok(())
            })
            .expect("part1 parses");
        assert!(frames.is_empty());
        parser
            .feed::<()>(part2, &mut |frame| {
                frames.push(frame);
                Ok(())
            })
            .expect("part2 parses");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("content_block_delta"));
        assert_eq!(frames[0].data, "{\"a\":1}\n{\"b\":2}");
        parser
            .finish::<()>(&mut |frame| {
                frames.push(frame);
                Ok(())
            })
            .expect("finish flushes tail");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].event, None);
        assert_eq!(frames[1].data, "tail");
    }

    #[test]
    fn post_stream_retries_pre_stream_429_and_aborts_mid_stream_on_cancel() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("addr");
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_server = Arc::clone(&attempts);
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut raw = Vec::new();
                let mut buf = [0u8; 512];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let n = attempts_server.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    let body = r#"{"error":"rate limit"}"#;
                    let resp = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.write_all(b"data: first\n\n");
                    let _ = stream.flush();
                    // Hold socket open until client cancels and closes.
                    std::thread::sleep(Duration::from_millis(600));
                }
            }
        });

        let client = HttpClient::new();
        let token = CancellationToken::new();
        let token_cancel = token.clone();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_cb = Arc::clone(&seen);
        let outcome = client.post_stream::<()>(
            &token,
            &post_to(format!("http://{addr}/stream"), b"{}"),
            &mut |frame| {
                seen_cb.lock().expect("lock").push(frame.data);
                token_cancel.cancel();
                Ok(())
            },
        );
        assert!(matches!(
            outcome,
            Err(StreamPostError::Transport(ExecutorError::Cancelled))
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(seen.lock().expect("lock").as_slice(), ["first"]);
        server.join().expect("join server");
    }
}
