//! The provider-independent AI execution boundary, shared by every shell
//! (NEX-ARCH-002 / review finding AC-101).
//!
//! Before this module existed, the boundary lived in the desktop adapter
//! (`src-tauri/src/application/execution.rs`) while the concrete provider
//! executors lived in its infrastructure layer — so the two layers imported
//! each other's types and formed a dependency cycle. This module lifts the
//! *boundary* into the interface-free core so the dependency arrow points one
//! way only:
//!
//! ```text
//! src-tauri infrastructure/providers ──implements──▶ nexora_core::provider
//! src-tauri application/execution   ──consumes────▶ nexora_core::provider
//! ```
//!
//! Contained — data, contracts, and pure policy only:
//!
//! - [`AiRequest`]/[`AiResponse`] and their message/tool/attachment payload
//!   types: the wire-neutral request/response shapes every executor
//!   normalizes into.
//! - [`ProviderExecutor`]: the provider-agnostic execution contract.
//! - [`ProviderExecutorResolver`] / [`ExecutorRegistry`]: the name → executor
//!   resolution seam. The *concrete* registry (which provider executor backs
//!   which name) stays with the desktop composition root — this crate never
//!   names a concrete provider, sends no HTTP, and touches no keyring.
//! - [`CredentialSource`] / [`CredentialError`]: the credential-availability
//!   seam. The OS keyring implementation stays with the infrastructure layer.
//! - [`ExecutorError`] and the bounded retry policy ([`should_retry`],
//!   [`retry_delay`], [`backoff_delay`]): pure classification and math.
//! - [`CancellationToken`]: the cooperative cancellation flag shared all the
//!   way down into executors and tool executions.
//!
//! **Not** contained: any concrete provider behavior, HTTP transport, keyring
//! access, model catalogs, or pricing. Those stay with the adapters that own
//! them, exactly as in phase 1 — this module adds only the *contract* both
//! adapters can agree on without importing each other.
//!
//! ## Dependency rule
//!
//! Unchanged from the crate rule: no Tauri, React, Ratatui, or Clap. The
//! module needs only `serde`/`serde_json` (stable wire shapes) and `std`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Cancellation token
// ---------------------------------------------------------------------------

/// Cooperative cancellation flag shared across the execution stack.
///
/// `std`-only (`Arc<AtomicBool>`), cheap to clone, polled by the runner
/// around every provider call, between tool dispatches, and inside the
/// command-execution wait loop so a user cancellation reaches running tool
/// processes promptly.
///
/// NEX-TASK-002: the flag additionally records a shutdown origin
/// ([`Self::shutdown`]): a shutdown-cancel wakes every parked wait exactly
/// like [`Self::cancel`], but parked approval waits observe the origin and
/// record the honest shutdown vocabulary instead of the user-cancel text.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Create a token in the not-cancelled state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the token cancelled. Never blocks.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Mark the token cancelled *by application shutdown* (NEX-TASK-002).
    /// Wakes every parked wait exactly like [`Self::cancel`]; parked
    /// approval waits additionally observe this origin and record the
    /// shutdown vocabulary (never the user-cancel text). Never blocks.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancel();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Whether the cancellation came from application shutdown
    /// ([`Self::shutdown`]) rather than a user cancel.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Request/response boundary data
// ---------------------------------------------------------------------------

/// A tool definition available to the model for one request (function calling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Function name as exposed to the model.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// A structured tool call returned by the assistant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned identifier for this call.
    pub id: String,
    /// Name of the tool invoked.
    pub name: String,
    /// Raw JSON string of arguments for the call.
    pub arguments: String,
    /// Provider-opaque reasoning signature (Gemini 3 thought signatures),
    /// pass-through only: never logged, never persisted, never parsed.
    #[serde(default)]
    pub thought_signature: Option<String>,
}

/// The execution result of one dispatched tool call, carried back to the
/// provider in the provider-native response format ([`AiRole::Tool`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiToolResult {
    /// The identifier of the tool call this result answers.
    pub call_id: String,
    /// Name of the tool that produced this result.
    pub name: String,
    /// Textual observation produced by the tool (success or error).
    pub content: String,
}

/// A provider-independent AI request (ARCHITECTURE.md §7).
///
/// The boundary deliberately carries no provider-specific structure: it
/// identifies the provider and model to use (FR-004) and the conversation
/// content to send (FR-003). Provider-specific formatting is the
/// responsibility of a [`ProviderExecutor`] implementation.
///
/// `messages` and `tools` are reference-counted (NEX-PERF-003): the agent
/// run loop rebuilds this request every model turn, and deep-cloning the
/// whole history plus the tool schema per turn is `O(turns² × history)`.
/// Sharing the buffers with `Arc` makes per-turn assembly `O(1)` — the loop
/// moves its owned `Vec` into an `Arc` for the synchronous provider call
/// and reclaims sole ownership before the next mutation. `Arc<Vec<…>>`
/// (rather than `Arc<[…]>`) is what lets the loop move the buffer back out
/// without cloning. Cloning the request itself (e.g. test captures) is
/// equally cheap: only the counters move.
///
/// `Eq` is intentionally not derived: [`ModelConfig`] carries an `f32`
/// temperature, so only `PartialEq` holds.
#[derive(Debug, Clone, PartialEq)]
pub struct AiRequest {
    /// Internal name of the provider to use (DATABASE.md §7.5).
    pub provider: String,
    /// Model identifier requested for this request (FR-004 model selection).
    pub model: String,
    /// Conversation content to send, in chronological order (shared, never
    /// deep-cloned per turn — see the struct docs).
    pub messages: Arc<Vec<AiMessage>>,
    /// Tools available to the model for this request. Empty means text-only.
    /// Shared for the same reason as `messages` (fixed for the whole run).
    pub tools: Arc<Vec<ToolDefinition>>,
    /// Per-request model selection overrides (temperature / output-token cap).
    /// `None` keeps the historical wire shape: no `temperature` or
    /// `max_tokens` member is sent. Executors serialize the overrides only
    /// where the endpoint supports them.
    pub model_config: Option<ModelConfig>,
    /// Optional wall-clock bound on the single blocking HTTP round trip
    /// (Task 3.2). `None` keeps the historical unbounded behavior; the
    /// blocking client cannot be interrupted mid-flight, so executors honour
    /// this via their HTTP client's per-request timeout when set.
    pub request_timeout: Option<std::time::Duration>,
}

/// A single turn of conversation content carried by an [`AiRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiMessage {
    /// Whether this message is authored by the user or the assistant.
    pub role: AiRole,
    /// The message text.
    pub content: String,
    /// Local-file references attached to this turn (FR-008; DATABASE.md
    /// §7.4). Metadata only: no file path and no file content is carried —
    /// the existing attachment model is a local-file reference, so only the
    /// display name, size, and media type cross the provider-independent
    /// boundary.
    pub attachments: Vec<AiAttachment>,
    /// Structured tool calls made by the assistant. Non-empty only on an
    /// [`AiRole::Assistant`] agent turn; every call is answered by a matching
    /// [`AiRole::Tool`] message before the next model turn.
    pub tool_calls: Vec<ToolCall>,
    /// Execution result of the dispatched tool call. Some only when
    /// `role == AiRole::Tool`; the textual content of such messages is an
    /// empty string — the observation lives in [`AiToolResult::content`].
    pub tool_result: Option<AiToolResult>,
}

impl AiMessage {
    /// Textual content for this turn, including inline text-file contents
    /// (FR-008).
    ///
    /// Text-decoded attachment payloads are inlined between explicit fences so
    /// the model can actually answer questions about the file. Base64 payloads
    /// (images / PDFs) are *not* dumped into the text — they are carried as
    /// provider-native structured parts by each executor — and are only
    /// acknowledged by name here.
    ///
    /// This is the single rendering point shared by every executor; it invents
    /// no provider-specific structure and never includes a filesystem path.
    #[must_use]
    pub fn composed_content(&self) -> String {
        if self.attachments.is_empty() {
            return self.content.clone();
        }
        let mut composed = self.content.clone();
        for attachment in &self.attachments {
            composed.push_str("\n\n[Attached file: ");
            composed.push_str(&attachment.file_name);
            composed.push(']');
            match &attachment.payload {
                AiAttachmentPayload::Text(text) => {
                    composed.push_str("\n--- begin attached file contents ---\n");
                    composed.push_str(text);
                    composed.push_str("\n--- end attached file contents ---");
                }
                AiAttachmentPayload::Base64(_) => {
                    // Binary payloads travel as provider-native parts; the
                    // text only acknowledges their presence.
                    composed.push_str(" (binary content attached)");
                }
            }
        }
        composed
    }
}

/// Processed content of one attached local file, ready for inclusion in a
/// provider request (FR-008).
///
/// Built by the application layer at request-construction time from the
/// stored `file_path`; the boundary deliberately never sees the path itself,
/// so no executor can leak it into a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiAttachmentPayload {
    /// The file was valid UTF-8 text; carried decoded for direct inlining.
    Text(String),
    /// Raw file bytes, base64-encoded for a provider-native inline part
    /// (image or PDF document block). The MIME type on [`AiAttachment`]
    /// identifies the encoded media.
    Base64(String),
}

/// A local-file reference attached to an [`AiMessage`] (FR-008; DATABASE.md
/// §7.4). Carries display metadata plus processed content only — deliberately
/// no `file_path`: the absolute local path is machine-local state that never
/// crosses the provider-independent boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiAttachment {
    /// Display name (`attachments.file_name`).
    pub file_name: String,
    /// File size in bytes (`attachments.file_size_bytes`), when recorded.
    pub file_size_bytes: Option<i64>,
    /// Media type (`attachments.mime_type`), when recorded.
    pub mime_type: Option<String>,
    /// Processed file content ready for provider transmission.
    pub payload: AiAttachmentPayload,
}

/// Author of an [`AiMessage`], mirroring the `messages.role` domain (DATABASE.md
/// §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRole {
    /// A system/instruction message that guides the assistant.
    System,
    /// An end-user message.
    User,
    /// An assistant (AI) response.
    Assistant,
    /// Role strictly for in-flight agent requests (tool results); never
    /// persisted (DATABASE.md §7.2 remains user/assistant/system).
    Tool,
}

/// Token usage for one provider response (Task 4.3).
///
/// `None` on [`AiResponse::usage`] means the provider response carried no
/// usage block (streaming or usage-less response) — this is not an error and
/// is counted as $0 by the spend guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input (prompt) tokens billed for this turn.
    pub input_tokens: u64,
    /// Output (completion) tokens billed for this turn.
    pub output_tokens: u64,
}

/// Per-request model selection (provider-runtime core).
///
/// Carries the requested model plus optional overrides. Filling a missing
/// context-window default from a canonical local source is application-layer
/// policy (it needs the adapters' model catalogs), so it is deliberately not
/// implemented here: the boundary carries only what the caller resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Requested model identifier.
    pub model_name: String,
    /// Explicit context-window override, if any.
    pub context_limit: Option<u64>,
    /// Explicit sampling-temperature override, if any.
    pub temperature: Option<f32>,
    /// Explicit output-token cap override, if any.
    pub max_tokens: Option<u32>,
}

impl ModelConfig {
    /// Create a config with no overrides.
    #[must_use]
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            context_limit: None,
            temperature: None,
            max_tokens: None,
        }
    }

    /// Set an explicit context-window override.
    #[must_use]
    pub fn with_context_limit(mut self, limit: Option<u64>) -> Self {
        self.context_limit = limit;
        self
    }

    /// Set an explicit sampling-temperature override.
    #[must_use]
    pub fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set an explicit output-token cap override.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

/// Pure capability projection of a received [`AiResponse`].
///
/// Computed on the response side from the already-received value — no
/// network, local-first intact. `tool_call` is true iff the response carried
/// at least one structured tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseCapabilities {
    /// The response carried at least one tool call.
    pub tool_call: bool,
}

/// A provider-independent AI response (ARCHITECTURE.md §7).
///
/// Carries the assistant's text plus the model that produced it so the caller
/// can record which model actually responded. No secret or provider-specific
/// detail is included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiResponse {
    /// Assistant response text.
    pub content: String,
    /// Model that produced the response.
    pub model: String,
    /// Structured tool calls returned by the assistant, if any.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage for this turn, if the provider reported it.
    pub usage: Option<TokenUsage>,
}

impl AiResponse {
    /// Project response-side capabilities from the received value.
    ///
    /// Pure projection: `tool_call` is true iff at least one structured tool
    /// call was returned. No network, local-first intact.
    #[must_use]
    pub fn capabilities(&self) -> ResponseCapabilities {
        ResponseCapabilities {
            tool_call: !self.tool_calls.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// Executor error classification
// ---------------------------------------------------------------------------

/// Error raised by a [`ProviderExecutor`] while executing a request.
///
/// Deliberately carries **no secret payload** and **no response-body
/// content**: it classifies only the failure *category*, so a formatted
/// [`ExecutorError`] can never leak a credential or payload
/// (ARCHITECTURE.md §9, §11). The boundary now carries the classified
/// categories computed by each provider.
#[derive(Debug)]
pub enum ExecutorError {
    /// The provider could not be reached (network or timeout).
    Network,
    /// The provider rate limited the request (HTTP 429), with the
    /// provider's `Retry-After` value when it was a valid integer of
    /// seconds.
    RateLimited {
        /// The provider's `Retry-After` hint in seconds, when parseable.
        retry_after_secs: Option<u64>,
    },
    /// The provider is unavailable or overloaded (HTTP 5xx).
    ProviderUnavailable,
    /// The provider reported insufficient credits/quota (HTTP 402): the
    /// credential is valid but the account cannot pay for this call
    /// (surfaced on the OpenAI-compatible path, e.g. `OpenRouter`).
    PaymentRequired,
    /// The provider rejected the stored credential (HTTP 401/403).
    Authentication,
    /// The provider rejected the request as invalid (HTTP 400).
    InvalidRequest,
    /// The provider rejected the request because the prompt exceeds the
    /// model context window. Detected from the error body/message text at
    /// the provider boundary (a 400/404/413/422 naming the context window
    /// or token limit); never retried, and handled by the agent loop as a
    /// compaction trigger instead of a terminal failure.
    ContextLengthExceeded,
    /// The provider returned an unexpected response.
    UnexpectedResponse,
    /// The provider could not fulfil the request (catch-all).
    Failure,
    /// The provider request was cancelled via the run's [`CancellationToken`]
    /// before it completed. Abandoned tool calls on this path must never
    /// dispatch: the runner maps this to a cancelled-run error, so no model
    /// turn is recorded and no tool call is dispatched.
    Cancelled,
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => {
                write!(
                    f,
                    "the AI provider could not be reached (network or timeout)"
                )
            }
            Self::RateLimited {
                retry_after_secs: None,
            } => {
                write!(
                    f,
                    "the AI provider rate limit was hit (HTTP 429); wait before retrying"
                )
            }
            Self::RateLimited {
                retry_after_secs: Some(secs),
            } => write!(
                f,
                "the AI provider rate limit was hit (HTTP 429); retry after {secs} seconds"
            ),
            Self::ProviderUnavailable => {
                write!(f, "the AI provider is unavailable or overloaded (HTTP 5xx)")
            }
            Self::PaymentRequired => {
                write!(
                    f,
                    "provider reported insufficient credits/quota (HTTP 402); \
                     top up or switch to a free-tier ID"
                )
            }
            Self::Authentication => {
                write!(
                    f,
                    "the AI provider rejected the stored credential (HTTP 401/403)"
                )
            }
            Self::InvalidRequest => {
                write!(
                    f,
                    "the AI provider rejected the request as invalid (HTTP 400)"
                )
            }
            Self::ContextLengthExceeded => {
                write!(
                    f,
                    "the AI provider rejected the request: prompt exceeds the model context window"
                )
            }
            Self::UnexpectedResponse => {
                write!(f, "the AI provider returned an unexpected response")
            }
            Self::Failure => write!(f, "the AI provider failed to fulfil the request"),
            Self::Cancelled => write!(
                f,
                "the AI provider request was cancelled before it completed"
            ),
        }
    }
}

impl std::error::Error for ExecutorError {}

// ---------------------------------------------------------------------------
// Bounded retry policy (pure)
// ---------------------------------------------------------------------------

/// Maximum attempts for one provider HTTP send: the initial try plus up to two
/// retries (bounded retry for 429/5xx and retryable network errors).
pub const MAX_SEND_ATTEMPTS: u32 = 3;

/// Upper bound honored for a provider `Retry-After` hint (seconds).
pub const MAX_RETRY_DELAY_SECS: u64 = 30;

/// Base backoff for a retryable failure without a usable `Retry-After` hint
/// (milliseconds): doubled per consecutive retry, ±25% jitter, capped at
/// [`MAX_RETRY_DELAY_SECS`]. The first retry waits ~2 s, the second ~4 s.
pub const RETRY_BASE_DELAY_MS: u64 = 2_000;

/// Returns true iff `status` is retryable: HTTP 429 or any 5xx. All other
/// statuses (including 400/401/402/403/404) are never retried.
#[must_use]
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// Returns true iff a classified [`ExecutorError`] is worth retrying.
///
/// Retry table (bounded by [`MAX_SEND_ATTEMPTS`], waited with jittered
/// [`backoff_delay`] or the capped `Retry-After` hint via [`retry_delay`]):
/// - retry: `RateLimited` (429), `ProviderUnavailable` (5xx), `Network`
///   (refused/DNS/timeout/reset);
/// - never: `Authentication` (401/403), `ContextLengthExceeded` (413 and the
///   400-classifier), usage failures (`InvalidRequest`, `PaymentRequired`,
///   `UnexpectedResponse`, `Failure`), and `Cancelled`.
///
/// There is deliberately no cross-provider fallback: a terminal failure for
/// one provider never tries another provider.
#[must_use]
pub fn should_retry(error: &ExecutorError) -> bool {
    matches!(
        error,
        ExecutorError::RateLimited { .. }
            | ExecutorError::ProviderUnavailable
            | ExecutorError::Network
    )
}

/// Computed backoff for the `retry_index`-th retry (0-based: 0 is the wait
/// before the second attempt): `min(base * 2^retry_index, 30 s)` with ±25%
/// jitter. Always strictly positive and never above [`MAX_RETRY_DELAY_SECS`].
///
/// The jitter source is a process-wide atomic counter mixed with the wall
/// clock (`splitmix64`); it needs no RNG crate and its output is confined to
/// `[0.75, 1.25]` by construction, so only bounds — never exact values — are
/// asserted. `retry_index` saturates at 5 (2^5 × 2 s already exceeds the cap),
/// so the shift can never overflow.
#[must_use]
pub fn backoff_delay(retry_index: u32) -> std::time::Duration {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    static JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

    let shift = retry_index.min(5);
    let base_ms = RETRY_BASE_DELAY_MS.saturating_mul(1 << shift);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    let count = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut mixed = nanos.wrapping_add(count.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    // splitmix64: avalanche the counter/clock mix into a uniform u64.
    mixed = mixed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = mixed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Jitter factor in [0.75, 1.25]: 75 + (z % 51) percent, integer math —
    // no float casts, no precision loss.
    let percent = 75 + (z % 51);
    let jittered_ms = base_ms.saturating_mul(percent) / 100;
    Duration::from_millis(
        jittered_ms
            .min(MAX_RETRY_DELAY_SECS.saturating_mul(1_000))
            .max(1),
    )
}

/// Effective wait before the `retry_index`-th retry: an explicit provider
/// `Retry-After` hint (header first, then body — already merged by the caller)
/// is honored verbatim up to [`MAX_RETRY_DELAY_SECS`]; a missing hint falls
/// back to the computed [`backoff_delay`], which is never zero.
///
/// An explicit `0` is honored as `0` (the provider asked for an immediate
/// retry); only a *missing* hint computes a backoff.
#[must_use]
pub fn retry_delay(retry_after_secs: Option<u64>, retry_index: u32) -> std::time::Duration {
    match retry_after_secs {
        Some(secs) => std::time::Duration::from_secs(secs.min(MAX_RETRY_DELAY_SECS)),
        None => backoff_delay(retry_index),
    }
}

// ---------------------------------------------------------------------------
// Executor contract and resolution seam
// ---------------------------------------------------------------------------

/// Internal name of the user-configured OpenAI-compatible endpoint
/// (DATABASE.md §7.5). Its base URL, organization, headers, and tool support
/// live in user settings rather than in a static provider entry, so its
/// executor is built per request from the resolved configuration via
/// [`ProviderExecutorResolver::compat_executor`].
pub const OPENAI_COMPAT_NAME: &str = "openai_compat";

/// Endpoint settings for the user-configured OpenAI-compatible endpoint,
/// reduced to the values an executor needs to send one request.
///
/// Deliberately carries **no secret**: the API key is keyed by
/// [`OPENAI_COMPAT_NAME`] in the OS keyring and travels only as the
/// call-scoped `credential` argument. Every field here is non-sensitive
/// endpoint metadata. Validation of the *stored* settings stays with the
/// adapter that persists them; executors re-validate fail-closed before any
/// network activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatEndpoint {
    /// Fully-qualified endpoint URL to send to (already normalized/trimmed
    /// by the caller from the stored base URL).
    pub endpoint: String,
    /// Optional organization identifier (sent as a provider-specific header
    /// by the OpenAI-compatible path).
    pub organization: Option<String>,
    /// Optional extra request headers (`name`, `value` pairs).
    pub headers: Vec<(String, String)>,
    /// Whether the endpoint accepts function-calling tools. When `false`,
    /// tools are never serialized (graceful degrade).
    pub supports_tools: bool,
}

/// Provider-independent execution boundary (ARCHITECTURE.md §7).
///
/// A provider-specific implementation of this trait performs the actual
/// network request for one provider and normalizes the result into the common
/// [`AiResponse`] boundary. Concrete implementations belong to the
/// infrastructure layer of each adapter; the application layer only ever sees
/// this contract.
///
/// `credential` is the provider credential read from the [`CredentialSource`]
/// immediately before execution. Implementations must never log, persist, or
/// embed `credential` into any error they return (ARCHITECTURE.md §9, §11,
/// §12).
///
/// `token` is the run's [`CancellationToken`]: implementations must abort a
/// cancelled in-flight request promptly (well under the wall-clock
/// `request_timeout`) and report [`ExecutorError::Cancelled`], and must abort
/// a pending retry backoff immediately. Callers without a run (plain chat)
/// pass a fresh token that never fires.
pub trait ProviderExecutor {
    /// Execute `request` against the provider using `credential`.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutorError::Failure`] when the provider cannot fulfil the
    /// request. The error never contains the credential or other secrets.
    fn execute(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> std::result::Result<AiResponse, ExecutorError>;

    /// Forward-compatible streaming shape: run the current sync [`Self::execute`]
    /// path, then deliver the full text once through `on_text`.
    ///
    /// This default keeps every existing caller and the current event channel
    /// untouched while giving a future streaming task a stable override point:
    /// per-provider executors will override this with real chunk delivery
    /// without changing the sync path. It performs no network itself beyond
    /// what [`Self::execute`] does, and `on_text` runs synchronously on the
    /// caller's thread.
    ///
    /// # Errors
    ///
    /// Same as [`Self::execute`].
    fn execute_stream(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_text: &dyn Fn(&str),
    ) -> std::result::Result<AiResponse, ExecutorError> {
        let response = self.execute(request, credential, token)?;
        on_text(response.content.as_str());
        Ok(response)
    }
}

/// Shared handle to one concrete executor. Executors are plain synchronous
/// objects; the explicit `Send + Sync` bound is what lets one executor be
/// shared across the run threads that resolve it.
pub type SharedExecutor = Arc<dyn ProviderExecutor + Send + Sync>;

/// Resolves concrete [`ProviderExecutor`]s from a provider's internal name.
///
/// This is the wiring seam between the provider-independent execution layer
/// and the concrete provider implementations (ROADMAP.md Phase 3). The
/// composition root of each adapter (the desktop `lib.rs` setup) builds an
/// implementation over its concrete executors and installs it into the
/// application service; the application layer never names a concrete
/// provider.
///
/// Implementations own no credentials and never touch a [`CredentialSource`]:
/// they hold only registered executors and resolve them by `name`.
/// Credential acquisition and validation remain the responsibility of the
/// calling service. An unregistered `name` resolves to [`None`] — there is
/// deliberately no fallback to an arbitrary available provider.
pub trait ProviderExecutorResolver: Send + Sync {
    /// Resolve the executor registered for `name` as an owned shared handle
    /// (the agent-run bridge moves the executor into the spawned run thread,
    /// which outlives the resolver that resolved it).
    ///
    /// Returns [`None`] when no executor is registered for `name` — no
    /// fallback, exactly like a lookup against an empty registry.
    fn resolve(&self, name: &str) -> Option<SharedExecutor>;

    /// Build the executor for the user-configured OpenAI-compatible endpoint
    /// ([`OPENAI_COMPAT_NAME`]) from `config`.
    ///
    /// The caller must validate the stored settings first; the executor
    /// re-validates fail-closed on every send. Unlike [`Self::resolve`] this
    /// is constructed per request from configuration — the endpoint carries
    /// no static registry entry.
    fn compat_executor(&self, config: &CompatEndpoint) -> SharedExecutor;
}

/// A name → executor map — the generic half of every
/// [`ProviderExecutorResolver`].
///
/// The registry itself is provider-free: it maps internal names to executors
/// that were registered into it. Each adapter's composition root registers
/// its concrete executors; tests register stubs. Resolution never falls back:
/// an unknown name yields [`None`].
#[derive(Default)]
pub struct ExecutorRegistry {
    executors: Vec<(&'static str, SharedExecutor)>,
}

impl ExecutorRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `executor` under the internal provider `name`, consuming the
    /// registry (builder style) so a composition root can chain registrations.
    ///
    /// A later registration under an already-registered name wins: names are
    /// resolved in registration order and the first match is returned, so
    /// append-only composition (as every call site uses) never shadows.
    #[must_use]
    pub fn with_executor(mut self, name: &'static str, executor: SharedExecutor) -> Self {
        self.executors.push((name, executor));
        self
    }

    /// Whether `name` resolves to a registered executor.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }
}

impl ProviderExecutorResolver for ExecutorRegistry {
    fn resolve(&self, name: &str) -> Option<SharedExecutor> {
        self.executors
            .iter()
            .find(|(registered, _)| *registered == name)
            .map(|(_, executor)| Arc::clone(executor))
    }

    fn compat_executor(&self, _config: &CompatEndpoint) -> SharedExecutor {
        // The generic registry holds no concrete provider wiring, so it
        // cannot build a user-configured-endpoint executor. Composition
        // roots install a resolver that implements this method concretely;
        // this default exists only so the generic registry stays usable for
        // name-resolution-only seams (tests, health probes). The failure is
        // classified and secret-free, exactly like any other rejected
        // request.
        Arc::new(UnsupportedCompatExecutor)
    }
}

/// Executor used when a registry without concrete provider wiring is asked
/// for a user-configured-endpoint executor. Every request fails closed with
/// a classified, secret-free error; it never sends anything.
#[derive(Debug)]
struct UnsupportedCompatExecutor;

impl ProviderExecutor for UnsupportedCompatExecutor {
    fn execute(
        &self,
        _request: &AiRequest,
        _credential: &str,
        _token: &CancellationToken,
    ) -> std::result::Result<AiResponse, ExecutorError> {
        Err(ExecutorError::InvalidRequest)
    }
}

// ---------------------------------------------------------------------------
// Credential seam
// ---------------------------------------------------------------------------

/// Errors raised while reading provider credential availability.
///
/// Deliberately carries **no secret payload**: the variants identify only the
/// failure category, so a formatted [`CredentialError`] can never leak a
/// credential into the logs (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub enum CredentialError {
    /// The operating-system secure keyring could not be reached, was locked,
    /// or is unsupported on this system.
    StorageUnavailable,
    /// A credential's value or stored representation could not be written or
    /// read as expected.
    Invalid,
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StorageUnavailable => {
                write!(
                    f,
                    "provider credential store is unavailable (OS secure keyring)"
                )
            }
            Self::Invalid => write!(
                f,
                "provider credential could not be read or written in the OS secure keyring"
            ),
        }
    }
}

impl std::error::Error for CredentialError {}

/// Reads provider credential availability and values from the secure store.
///
/// The application layer depends on this seam (FR-014: missing credentials
/// are detected before a request is sent); the OS keyring implementation
/// stays with the infrastructure layer. Implementations must never log or
/// embed a credential value in any error they return.
pub trait CredentialSource: Send + Sync {
    /// Report whether `provider` has a stored credential.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] when the secure store
    /// cannot be reached to determine availability; this is distinct from a
    /// definitively missing credential, which resolves to [`Ok(false)`].
    fn exists(&self, provider: &str) -> std::result::Result<bool, CredentialError>;

    /// Read the credential stored under `provider`.
    ///
    /// Returns [`Some`] containing the secret when present, or [`None`] when
    /// no credential exists for `provider`.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::StorageUnavailable`] if the secure store
    /// is unavailable, or [`CredentialError::Invalid`] if the stored
    /// credential cannot be decoded.
    fn read(&self, provider: &str) -> std::result::Result<Option<String>, CredentialError>;
}

// ---------------------------------------------------------------------------
// Tests (moved with the boundary from the desktop adapter)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_starts_not_cancelled_and_reports_shutdown_origin() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        assert!(!token.is_shutdown());
        token.shutdown();
        assert!(token.is_cancelled(), "shutdown implies cancellation");
        assert!(token.is_shutdown(), "the origin is recorded");
        let plain = CancellationToken::new();
        plain.cancel();
        assert!(plain.is_cancelled());
        assert!(!plain.is_shutdown(), "a plain cancel is not a shutdown");
    }

    #[test]
    fn token_clone_shares_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled(), "clones govern the same flag");
    }

    #[test]
    fn composed_content_without_attachments_is_unchanged() {
        let message = AiMessage {
            role: AiRole::User,
            content: "plain text".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        };
        assert_eq!(message.composed_content(), "plain text");
    }

    #[test]
    fn composed_content_appends_attachment_references() {
        let message = AiMessage {
            role: AiRole::User,
            content: "Summarize".to_string(),
            attachments: vec![AiAttachment {
                file_name: "notes.txt".to_string(),
                file_size_bytes: Some(2048),
                mime_type: Some("text/plain".to_string()),
                payload: AiAttachmentPayload::Text("file body line".to_string()),
            }],
            tool_calls: Vec::new(),
            tool_result: None,
        };
        assert_eq!(
            message.composed_content(),
            "Summarize\n\n[Attached file: notes.txt]\n\
             --- begin attached file contents ---\n\
             file body line\n\
             --- end attached file contents ---"
        );
    }

    #[test]
    fn composed_content_acknowledges_binary_attachments_without_dumping_them() {
        let message = AiMessage {
            role: AiRole::User,
            content: "Look".to_string(),
            attachments: vec![AiAttachment {
                file_name: "report.pdf".to_string(),
                file_size_bytes: Some(7),
                mime_type: Some("application/pdf".to_string()),
                payload: AiAttachmentPayload::Base64("Zm9vYmFy".to_string()),
            }],
            tool_calls: Vec::new(),
            tool_result: None,
        };
        let composed = message.composed_content();
        assert_eq!(
            composed,
            "Look\n\n[Attached file: report.pdf] (binary content attached)"
        );
        // The base64 data itself never enters the text channel.
        assert!(!composed.contains("Zm9vYmFy"));
    }

    #[test]
    fn executor_error_display_texts_are_distinct() {
        assert_eq!(
            ExecutorError::Network.to_string(),
            "the AI provider could not be reached (network or timeout)"
        );
        assert_eq!(
            ExecutorError::RateLimited {
                retry_after_secs: None
            }
            .to_string(),
            "the AI provider rate limit was hit (HTTP 429); wait before retrying"
        );
        assert_eq!(
            ExecutorError::RateLimited {
                retry_after_secs: Some(30)
            }
            .to_string(),
            "the AI provider rate limit was hit (HTTP 429); retry after 30 seconds"
        );
        assert_eq!(
            ExecutorError::ProviderUnavailable.to_string(),
            "the AI provider is unavailable or overloaded (HTTP 5xx)"
        );
        assert_eq!(
            ExecutorError::PaymentRequired.to_string(),
            "provider reported insufficient credits/quota (HTTP 402); \
             top up or switch to a free-tier ID"
        );
        assert_eq!(
            ExecutorError::Authentication.to_string(),
            "the AI provider rejected the stored credential (HTTP 401/403)"
        );
        assert_eq!(
            ExecutorError::InvalidRequest.to_string(),
            "the AI provider rejected the request as invalid (HTTP 400)"
        );
        assert_eq!(
            ExecutorError::UnexpectedResponse.to_string(),
            "the AI provider returned an unexpected response"
        );
        assert_eq!(
            ExecutorError::Failure.to_string(),
            "the AI provider failed to fulfil the request"
        );
        assert_eq!(
            ExecutorError::Cancelled.to_string(),
            "the AI provider request was cancelled before it completed"
        );
    }

    #[test]
    fn retryable_status_is_only_429_and_5xx() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(402));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
        assert!(!is_retryable_status(422));
    }

    #[test]
    fn retry_delay_caps_retry_after_at_30s() {
        use std::time::Duration;
        assert_eq!(MAX_SEND_ATTEMPTS, 3);
        assert_eq!(MAX_RETRY_DELAY_SECS, 30);
        // An explicit provider hint is honored verbatim up to the cap: an
        // explicit 0 stays 0 (immediate retry, as before).
        assert_eq!(retry_delay(Some(0), 0), Duration::from_secs(0));
        assert_eq!(retry_delay(Some(5), 0), Duration::from_secs(5));
        assert_eq!(retry_delay(Some(5), 1), Duration::from_secs(5));
        assert_eq!(retry_delay(Some(30), 0), Duration::from_secs(30));
        assert_eq!(retry_delay(Some(120), 0), Duration::from_secs(30));
        assert_eq!(retry_delay(Some(9999), 2), Duration::from_secs(30));
        // A missing hint no longer waits zero: it computes the jittered
        // backoff for the retry index (bounds asserted exactly in
        // `default_backoff_is_nonzero_with_jitter_bounds`).
        for index in 0..3 {
            let delay = retry_delay(None, index);
            assert!(delay > Duration::from_secs(0));
            assert!(delay <= Duration::from_secs(30));
        }
    }

    #[test]
    fn default_backoff_is_nonzero_with_jitter_bounds() {
        // The computed backoff for retry index `i` is base 2 s × 2^i with
        // ±25% jitter, capped at 30 s, always strictly positive. Jitter is
        // nondeterministic by design, so only deterministic bounds are
        // asserted — never exact equality.
        for index in 0..8 {
            let uncapped_ms = RETRY_BASE_DELAY_MS.saturating_mul(1 << index.min(5));
            // Either side of the jitter band saturates at the cap: past the
            // knee the delay pins at exactly 30 s.
            let lower_ms = (uncapped_ms.saturating_mul(75) / 100).min(30_000);
            let upper_ms = (uncapped_ms.saturating_mul(125) / 100).min(30_000);
            let delay = backoff_delay(index);
            assert!(
                delay > std::time::Duration::from_secs(0),
                "backoff for retry {index} must be nonzero, got {delay:?}"
            );
            assert!(
                delay >= std::time::Duration::from_millis(lower_ms)
                    && delay <= std::time::Duration::from_millis(upper_ms),
                "backoff for retry {index} must lie in [0.75x, 1.25x] of \
                 {uncapped_ms}ms capped at 30 s, got {delay:?}"
            );
            assert!(
                delay <= std::time::Duration::from_secs(MAX_RETRY_DELAY_SECS),
                "backoff for retry {index} must never exceed the 30 s cap"
            );
        }
        // Spot bounds for the attempts the bounded retry loop actually uses:
        // first retry ~2 s, second ~4 s.
        let first = backoff_delay(0);
        assert!(first >= std::time::Duration::from_millis(1_500));
        assert!(first <= std::time::Duration::from_millis(2_500));
        let second = backoff_delay(1);
        assert!(second >= std::time::Duration::from_secs(3));
        assert!(second <= std::time::Duration::from_secs(5));
    }

    #[test]
    fn should_retry_retries_only_rate_limit_server_and_network() {
        // Retry: rate-limit (with or without hint), server outage, transport.
        assert!(should_retry(&ExecutorError::RateLimited {
            retry_after_secs: Some(2)
        }));
        assert!(should_retry(&ExecutorError::RateLimited {
            retry_after_secs: None
        }));
        assert!(should_retry(&ExecutorError::ProviderUnavailable));
        assert!(should_retry(&ExecutorError::Network));
        // Never: auth, context, and usage failures — retrying burns quota or
        // loops on a request that can never succeed.
        assert!(!should_retry(&ExecutorError::Authentication));
        assert!(!should_retry(&ExecutorError::ContextLengthExceeded));
        assert!(!should_retry(&ExecutorError::InvalidRequest));
        assert!(!should_retry(&ExecutorError::PaymentRequired));
        assert!(!should_retry(&ExecutorError::UnexpectedResponse));
        assert!(!should_retry(&ExecutorError::Failure));
        assert!(!should_retry(&ExecutorError::Cancelled));
    }

    #[test]
    fn executor_error_stays_category_only_and_secret_free() {
        // Same pattern as the provider secret-hygiene test: every boundary
        // category formats to fixed text that can never echo a credential,
        // URL secret, request payload, or raw body.
        const SECRET_SENTINELS: [&str; 5] = [
            "sk-",
            "secret",
            "api_key",
            "sk-live-sentinel-12345",
            "bearer",
        ];
        let errors = [
            ExecutorError::Network,
            ExecutorError::RateLimited {
                retry_after_secs: None,
            },
            ExecutorError::RateLimited {
                retry_after_secs: Some(7),
            },
            ExecutorError::ProviderUnavailable,
            ExecutorError::PaymentRequired,
            ExecutorError::Authentication,
            ExecutorError::InvalidRequest,
            ExecutorError::ContextLengthExceeded,
            ExecutorError::UnexpectedResponse,
            ExecutorError::Failure,
            ExecutorError::Cancelled,
        ];
        for error in &errors {
            let message = error.to_string();
            assert!(!message.is_empty(), "category text must be non-empty");
            for sentinel in SECRET_SENTINELS {
                assert!(
                    !message.to_lowercase().contains(sentinel),
                    "category text must stay secret-free, found {sentinel:?} in {message:?}"
                );
            }
        }
        // The credential value itself never appears, even verbatim.
        let credential = "sk-live-sentinel-12345";
        for error in &errors {
            assert!(
                !error.to_string().contains(credential),
                "credential must never appear in {error:?}"
            );
            assert!(
                !format!("{error:?}").contains(credential),
                "credential must never appear in debug {error:?}"
            );
        }
    }

    #[test]
    fn model_config_builders_set_overrides() {
        let config = ModelConfig::new("m")
            .with_temperature(Some(0.5))
            .with_max_tokens(Some(512));
        assert_eq!(config.temperature, Some(0.5));
        assert_eq!(config.max_tokens, Some(512));
        assert_eq!(config.context_limit, None);
    }

    #[test]
    fn response_capabilities_reflect_tool_calls() {
        let plain = AiResponse {
            content: "hi".to_string(),
            model: "m".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        };
        assert!(!plain.capabilities().tool_call);
        let with_tools = AiResponse {
            content: String::new(),
            model: "m".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_0".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
                thought_signature: None,
            }],
            usage: None,
        };
        assert!(with_tools.capabilities().tool_call);
    }

    #[test]
    fn executor_stream_default_delegates_to_execute() {
        struct StubExecutor {
            response: AiResponse,
        }
        impl ProviderExecutor for StubExecutor {
            fn execute(
                &self,
                _request: &AiRequest,
                _credential: &str,
                _token: &CancellationToken,
            ) -> std::result::Result<AiResponse, ExecutorError> {
                Ok(self.response.clone())
            }
        }
        let request = AiRequest {
            provider: "openai".to_string(),
            model: "m".to_string(),
            messages: Arc::new(Vec::new()),
            tools: Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let stub = StubExecutor {
            response: AiResponse {
                content: "hello".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            },
        };
        let seen = std::cell::RefCell::new(Vec::new());
        let out = stub
            .execute_stream(&request, "cred", &CancellationToken::new(), &|chunk| {
                seen.borrow_mut().push(chunk.to_string());
            })
            .expect("stream default succeeds");
        assert_eq!(out.content, "hello");
        assert_eq!(seen.borrow().as_slice(), ["hello"]);
    }

    /// NEX-PERF-003: `AiRequest` shares its message/tool buffers instead of
    /// deep-cloning them per turn. Cloning a request (what the run loop and
    /// the test executors do every turn) must alias the same allocations —
    /// `Arc::ptr_eq` on both buffers — so per-turn assembly is `O(1)`.
    #[test]
    fn ai_request_clone_shares_message_and_tool_buffers() {
        let messages = Arc::new(vec![AiMessage {
            role: AiRole::User,
            content: "hello".to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }]);
        let tools = Arc::new(vec![ToolDefinition {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: serde_json::json!({}),
        }]);
        let first = AiRequest {
            provider: "openai".to_string(),
            model: "m".to_string(),
            messages: Arc::clone(&messages),
            tools: Arc::clone(&tools),
            model_config: None,
            request_timeout: None,
        };
        let second = first.clone();
        assert!(
            Arc::ptr_eq(&first.messages, &second.messages),
            "request clones must share the message buffer"
        );
        assert!(
            Arc::ptr_eq(&first.tools, &second.tools),
            "request clones must share the tool buffer"
        );
        assert_eq!(first, second, "shared requests still compare by value");
    }

    #[test]
    fn registry_resolves_registered_names_without_fallback() {
        struct StubExecutor;
        impl ProviderExecutor for StubExecutor {
            fn execute(
                &self,
                _request: &AiRequest,
                _credential: &str,
                _token: &CancellationToken,
            ) -> std::result::Result<AiResponse, ExecutorError> {
                Err(ExecutorError::Failure)
            }
        }
        let registry = ExecutorRegistry::new()
            .with_executor("openai", Arc::new(StubExecutor))
            .with_executor("gemini", Arc::new(StubExecutor));
        assert!(registry.resolve("openai").is_some());
        assert!(registry.resolve("gemini").is_some());
        // No executor is registered for an unregistered name; there is no
        // fallback to an arbitrary available provider.
        assert!(registry.resolve("ghost-provider").is_none());
        assert!(!registry.contains("ghost-provider"));
        assert!(registry.contains("openai"));
        // An empty registry resolves nothing at all.
        assert!(ExecutorRegistry::new().resolve("openai").is_none());
    }

    #[test]
    fn empty_registry_compat_executor_fails_closed_without_sending() {
        // The generic registry has no concrete provider wiring: its compat
        // executor rejects every request with a classified error instead of
        // silently sending to a guessed target.
        let registry = ExecutorRegistry::new();
        let executor = registry.compat_executor(&CompatEndpoint {
            endpoint: "https://example.invalid/v1".to_string(),
            organization: None,
            headers: Vec::new(),
            supports_tools: false,
        });
        let request = AiRequest {
            provider: OPENAI_COMPAT_NAME.to_string(),
            model: "m".to_string(),
            messages: Arc::new(Vec::new()),
            tools: Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let outcome = executor.execute(&request, "cred", &CancellationToken::new());
        assert!(matches!(outcome, Err(ExecutorError::InvalidRequest)));
    }
}
