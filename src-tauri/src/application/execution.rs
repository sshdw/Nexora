//! Provider request execution service: the provider-independent execution
//! boundary of the AI layer (SRS FR-003, FR-004, FR-014; ROADMAP.md Phase 3 —
//! AI Providers; ARCHITECTURE.md §5, §7).
//!
//! This module defines the provider-independent AI layer contract
//! (ARCHITECTURE.md §7): a request is selected and executed through a single,
//! provider-agnostic boundary ([`ProviderExecutor`]) until a classified result
//! is propagated. It sits in the application layer (ARCHITECTURE.md §5:
//! request orchestration) and composes the existing
//! [`ProviderService`](crate::application::providers::ProviderService) for
//! provider metadata and the existing [`CredentialStore`] for credentials.
//! It invents no provider-specific networking, request format, or API behavior:
//! the concrete provider implementation is a later Phase 3 task that fulfills
//! [`ProviderExecutor`].
//!
//! A request is executed only after:
//!   1. the requested provider is resolved through the Provider Metadata
//!      Service (ARCHITECTURE.md §7: provider selection);
//!   2. the provider is verified to be configured and to have stored
//!      credentials (FR-014: missing credentials are detected before a request
//!      is sent; DATABASE.md §7.5 availability);
//!   3. the credential is read from the existing [`CredentialStore`], only at
//!      the moment of execution;
//!   4. the request is delegated to the [`ProviderExecutor`].
//!
//! # Security
//!
//! Per ARCHITECTURE.md §9, §11, §12 and DATABASE.md §14, credential values are
//! NEVER persisted to `SQLite`, written to the logs, or included in any returned
//! metadata or error message. The credential is read directly into this
//! service's execution call and passed only to the executor that performs the
//! network request; it is dropped when the call returns. [`RequestError`] and
//! [`ExecutorError`] deliberately carry no secret payload (ARCHITECTURE.md §10:
//! classified errors).

use std::sync::Arc;

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::anthropic::{
    AnthropicExecutor, PROVIDER_NAME as ANTHROPIC_PROVIDER_NAME,
};
use crate::infrastructure::providers::credentials::{CredentialError, CredentialStore};
use crate::infrastructure::providers::gemini::{
    GeminiExecutor, PROVIDER_NAME as GEMINI_PROVIDER_NAME,
};
use crate::infrastructure::providers::openai::{
    OpenAiExecutor, NVIDIA_ENDPOINT, NVIDIA_NAME, OPENCODE_ZEN_ENDPOINT, OPENCODE_ZEN_NAME,
    OPENROUTER_ENDPOINT, OPENROUTER_NAME, PROVIDER_NAME, XKIRO_ENDPOINT, XKIRO_NAME,
};
use serde::{Deserialize, Serialize};

use super::providers::{ProviderError, ProviderService};
use crate::application::agent::control::CancellationToken;

/// Application-layer result shared by request execution operations, unifying
/// orchestration, persistence, and credential failures.
pub(crate) type Result<T> = std::result::Result<T, RequestError>;

/// A tool definition available to the model for one request (function calling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolDefinition {
    /// Function name as exposed to the model.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// A structured tool call returned by the assistant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolCall {
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
pub(crate) struct AiToolResult {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiRequest {
    /// Internal name of the provider to use (DATABASE.md §7.5).
    pub provider: String,
    /// Model identifier requested for this request (FR-004 model selection).
    pub model: String,
    /// Conversation content to send, in chronological order.
    pub messages: Vec<AiMessage>,
    /// Tools available to the model for this request. Empty means text-only.
    pub tools: Vec<ToolDefinition>,
    /// Optional wall-clock bound on the single blocking HTTP round trip
    /// (Task 3.2). `None` keeps the historical unbounded behavior; the
    /// blocking client cannot be interrupted mid-flight, so executors honour
    /// this via their HTTP client's per-request timeout when set.
    pub request_timeout: Option<std::time::Duration>,
}

/// A single turn of conversation content carried by an [`AiRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiMessage {
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
    pub(crate) fn composed_content(&self) -> String {
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
pub(crate) enum AiAttachmentPayload {
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
pub(crate) struct AiAttachment {
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
pub(crate) enum AiRole {
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
pub(crate) struct TokenUsage {
    /// Input (prompt) tokens billed for this turn.
    pub input_tokens: u64,
    /// Output (completion) tokens billed for this turn.
    pub output_tokens: u64,
}

/// Static, locally-curated description of one model (provider-runtime core).
///
/// This is a plain domain struct: it carries the documented context window
/// plus capability flags and performs no network I/O. The hardcoded model
/// lists stay in `infrastructure::providers::{openai,anthropic,gemini}` and
/// are aggregated by `infrastructure::providers::supported_providers`; this
/// struct only projects them (plus the existing
/// `application::context_stats::context_limit_for`) into one value. There is
/// no live catalog fetch.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelInfo {
    /// Model identifier as listed for its provider.
    pub name: String,
    /// Documented context window for this model.
    pub context_limit: u64,
    /// Whether the model accepts function-calling tools.
    pub supports_tools: bool,
    /// Whether the model accepts image input.
    pub supports_vision: bool,
    /// Whether the model accepts a structured-output (`json`) mode.
    pub supports_json: bool,
    /// Whether the model can stream chunks. Always `false` while the runtime
    /// is sync-only; a future streaming task flips this per provider without
    /// changing the sync path.
    pub supports_streaming: bool,
}

impl ModelInfo {
    /// Project this model's static flags into [`ModelCapabilities`].
    ///
    /// Pure projection: no network, no catalog, local-first intact.
    #[must_use]
    pub(crate) fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tool_call: self.supports_tools,
            vision: self.supports_vision,
            json: self.supports_json,
            streaming: self.supports_streaming,
        }
    }
}

/// Pure capability projection of a [`ModelInfo`].
///
/// Computed on the local side from curated flags only — never fetched over
/// the network.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelCapabilities {
    /// The model can be invoked with tools (`tool_call` capability).
    pub tool_call: bool,
    /// The model accepts image input.
    pub vision: bool,
    /// The model accepts a structured-output mode.
    pub json: bool,
    /// The model can stream chunks.
    pub streaming: bool,
}

/// Pure capability projection of an [`AiResponse`].
///
/// Computed on the response side from the already-received value — no
/// network, local-first intact. `tool_call` is true iff the response carried
/// at least one structured tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResponseCapabilities {
    /// The response carried at least one tool call.
    pub tool_call: bool,
}

/// Per-request model selection (provider-runtime core).
///
/// Carries the requested model plus optional overrides. Missing values fall
/// back through [`Self::with_canonical_limits`], which resolves the
/// documented window via the existing
/// `application::context_stats::context_limit_for` — the hardcoded constants
/// stay in the providers, this struct only defaults from them. There is no
/// live catalog fetch and no cross-provider fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModelConfig {
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
    pub(crate) fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            context_limit: None,
            temperature: None,
            max_tokens: None,
        }
    }

    /// Set an explicit context-window override.
    #[must_use]
    pub(crate) fn with_context_limit(mut self, limit: Option<u64>) -> Self {
        self.context_limit = limit;
        self
    }

    /// Set an explicit sampling-temperature override.
    #[must_use]
    pub(crate) fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    /// Set an explicit output-token cap override.
    #[must_use]
    pub(crate) fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Fill a missing [`Self::context_limit`] from the canonical local
    /// source: the existing `context_limit_for(provider, model)`.
    ///
    /// An explicit limit is never overwritten. Temperature and `max_tokens`
    /// have no canonical local source, so they are left untouched.
    #[must_use]
    pub(crate) fn with_canonical_limits(self, provider: &str) -> Self {
        if self.context_limit.is_some() {
            return self;
        }
        let limit = crate::application::context_stats::context_limit_for(
            Some(provider),
            Some(self.model_name.as_str()),
        );
        Self {
            context_limit: Some(limit),
            ..self
        }
    }

    /// Effective context window: the explicit override when set, otherwise
    /// the canonical local value for (`provider`, [`Self::model_name`]).
    #[must_use]
    pub(crate) fn effective_context_limit(&self, provider: &str) -> u64 {
        self.context_limit.unwrap_or_else(|| {
            crate::application::context_stats::context_limit_for(
                Some(provider),
                Some(self.model_name.as_str()),
            )
        })
    }
}

/// Resolve the static [`ModelInfo`] for (`provider`, `model`).
///
/// The context window comes from the existing `context_limit_for`; the
/// capability flags come from membership in the hardcoded
/// `supported_providers()` aggregation (the single source of truth for which
/// models may be configured). Listed models are tool/vision/json capable and
/// non-streaming (sync-only runtime); unlisted IDs get conservative `false`
/// flags so tool-gating drops them when tools are required. No network, no
/// fallback to another provider.
#[must_use]
pub(crate) fn model_info_for(provider: &str, model: &str) -> ModelInfo {
    let context_limit =
        crate::application::context_stats::context_limit_for(Some(provider), Some(model));
    let listed = crate::infrastructure::providers::supported_providers()
        .iter()
        .any(|entry| entry.name == provider && entry.models.iter().any(|id| id == model));
    ModelInfo {
        name: model.to_string(),
        context_limit,
        supports_tools: listed,
        supports_vision: listed,
        supports_json: listed,
        supports_streaming: false,
    }
}

/// List the static [`ModelInfo`] values for `provider` in display order.
///
/// Returns an empty vector for an unknown provider — never a fallback to
/// another provider's models.
#[must_use]
pub(crate) fn models_for_provider(provider: &str) -> Vec<ModelInfo> {
    crate::infrastructure::providers::supported_providers()
        .iter()
        .find(|entry| entry.name == provider)
        .map(|entry| {
            entry
                .models
                .iter()
                .map(|model| model_info_for(provider, model))
                .collect()
        })
        .unwrap_or_default()
}

/// Keep only tool-capable models.
///
/// Pure local filter over [`ModelInfo::supports_tools`]; used when a request
/// carries tools so a non-`tool_call` model is never offered for it.
#[must_use]
pub(crate) fn tool_capable_models(models: &[ModelInfo]) -> Vec<ModelInfo> {
    models
        .iter()
        .filter(|info| info.supports_tools)
        .cloned()
        .collect()
}

/// List model names for `provider` in display order.
///
/// When `require_tools` is true, non-`tool_call` models are filtered out
/// (local analog of a recommended-models projection: text + tool-calling
/// only). When false, every listed model is returned. Unknown providers yield
/// an empty vector — no cross-provider fallback, no network.
#[must_use]
pub(crate) fn recommended_models(provider: &str, require_tools: bool) -> Vec<String> {
    let infos = models_for_provider(provider);
    let kept: Vec<ModelInfo> = if require_tools {
        tool_capable_models(&infos)
    } else {
        infos
    };
    kept.into_iter().map(|info| info.name).collect()
}

/// A provider-independent AI response (ARCHITECTURE.md §7).
///
/// Carries the assistant's text plus the model that produced it so the caller
/// can record which model actually responded. No secret or provider-specific
/// detail is included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AiResponse {
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
    pub(crate) fn capabilities(&self) -> ResponseCapabilities {
        ResponseCapabilities {
            tool_call: !self.tool_calls.is_empty(),
        }
    }
}

/// Error raised by a [`ProviderExecutor`] while executing a request.
///
/// Deliberately carries **no secret payload** and **no response-body
/// content**: it classifies only the failure *category*, so a formatted
/// [`ExecutorError`] can never leak a credential or payload
/// (ARCHITECTURE.md §9, §11). The boundary now carries the classified
/// categories computed by each provider.
#[derive(Debug)]
pub(crate) enum ExecutorError {
    /// The provider could not be reached (network or timeout).
    Network,
    /// The provider rate limited the request (HTTP 429), with the
    /// provider's `Retry-After` value when it was a valid integer of
    /// seconds.
    RateLimited { retry_after_secs: Option<u64> },
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
    /// dispatch: the runner maps this to `AgentError::Cancelled`, so no model
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

/// Maximum attempts for one provider HTTP send: the initial try plus up to two
/// retries (bounded retry for 429/5xx and retryable network errors).
pub(crate) const MAX_SEND_ATTEMPTS: u32 = 3;

/// Upper bound honored for a provider `Retry-After` hint (seconds).
pub(crate) const MAX_RETRY_DELAY_SECS: u64 = 30;

/// Base backoff for a retryable failure without a usable `Retry-After` hint
/// (milliseconds): doubled per consecutive retry, ±25% jitter, capped at
/// [`MAX_RETRY_DELAY_SECS`]. The first retry waits ~2 s, the second ~4 s.
pub(crate) const RETRY_BASE_DELAY_MS: u64 = 2_000;

/// Returns true iff `status` is retryable: HTTP 429 or any 5xx. All other
/// statuses (including 400/401/402/403/404) are never retried.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
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
pub(crate) fn backoff_delay(retry_index: u32) -> std::time::Duration {
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
pub(crate) fn retry_delay(retry_after_secs: Option<u64>, retry_index: u32) -> std::time::Duration {
    match retry_after_secs {
        Some(secs) => std::time::Duration::from_secs(secs.min(MAX_RETRY_DELAY_SECS)),
        None => backoff_delay(retry_index),
    }
}

/// Provider-independent execution boundary (ARCHITECTURE.md §7).
///
/// A provider-specific implementation of this trait performs the actual
/// network request for one provider and normalizes the result into the common
/// [`AiResponse`] boundary. This abstraction keeps the application independent
/// of provider-specific behavior; concrete implementations belong to later
/// Phase 3 tasks and are intentionally absent from the repository today.
///
/// `credential` is the provider credential read from the [`CredentialStore`]
/// immediately before execution. Implementations must never log, persist, or
/// embed `credential` into any error they return (ARCHITECTURE.md §9, §11,
/// §12).
///
/// `token` is the run's [`CancellationToken`]: implementations must abort a
/// cancelled in-flight request promptly (well under the wall-clock
/// `request_timeout`) and report [`ExecutorError::Cancelled`], and must abort
/// a pending retry backoff immediately. Callers without a run (plain chat)
/// pass a fresh token that never fires.
pub(crate) trait ProviderExecutor {
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

/// Resolves a concrete [`ProviderExecutor`] from a provider's internal name.
///
/// This is the wiring point between the provider-independent execution layer
/// and the concrete provider implementations (ROADMAP.md Phase 3). It owns no
/// credentials and never touches the [`CredentialStore`]: it holds only the
/// registered executors and resolves them by `name`. Credential acquisition and
/// validation remain the responsibility of [`RequestExecutionService`]. An
/// unregistered `name` resolves to [`None`] — there is deliberately no fallback
/// to an arbitrary available provider.
pub(crate) struct ExecutorRegistry {
    executors: Vec<(&'static str, Arc<dyn ProviderExecutor + Send + Sync>)>,
}

impl ExecutorRegistry {
    /// Build a registry that has every supported concrete provider registered.
    ///
    /// The `openai`, `anthropic`, and `gemini` provider executors are
    /// registered here, each under its internal provider name. Additional
    /// providers are registered here as they ship.
    pub(crate) fn new() -> Self {
        Self {
            executors: vec![
                (PROVIDER_NAME, Arc::new(OpenAiExecutor::new())),
                (ANTHROPIC_PROVIDER_NAME, Arc::new(AnthropicExecutor::new())),
                (GEMINI_PROVIDER_NAME, Arc::new(GeminiExecutor::new())),
                // Additional OpenAI-compatible providers share `OpenAiExecutor`.
                (
                    XKIRO_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        XKIRO_NAME,
                        XKIRO_ENDPOINT.to_string(),
                    )),
                ),
                (
                    OPENROUTER_NAME,
                    Arc::new(OpenAiExecutor::compatible_with_headers(
                        OPENROUTER_NAME,
                        OPENROUTER_ENDPOINT.to_string(),
                        &[
                            ("HTTP-Referer", "https://github.com/sshdw/Nexora"),
                            ("X-Title", "Nexora"),
                        ],
                    )),
                ),
                (
                    NVIDIA_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        NVIDIA_NAME,
                        NVIDIA_ENDPOINT.to_string(),
                    )),
                ),
                (
                    OPENCODE_ZEN_NAME,
                    Arc::new(OpenAiExecutor::compatible(
                        OPENCODE_ZEN_NAME,
                        OPENCODE_ZEN_ENDPOINT.to_string(),
                    )),
                ),
            ],
        }
    }

    /// Resolve the executor registered for `name`, if any.
    ///
    /// Returns [`None`] when no executor is registered for `name`; no fallback
    /// or automatic provider selection is performed.
    pub(crate) fn resolve(&self, name: &str) -> Option<&(dyn ProviderExecutor + Send + Sync)> {
        self.executors
            .iter()
            .find(|(registered, _)| *registered == name)
            .map(|(_, executor)| executor.as_ref())
    }

    /// Resolve the executor registered for `name` as an owned shared handle
    /// (Task 5.1): the agent-run bridge moves the executor into the spawned
    /// run thread, which outlives the registry that resolved it.
    ///
    /// Returns [`None`] when no executor is registered for `name` — no
    /// fallback, exactly like [`Self::resolve`].
    pub(crate) fn resolve_owned(
        &self,
        name: &str,
    ) -> Option<Arc<dyn ProviderExecutor + Send + Sync>> {
        self.executors
            .iter()
            .find(|(registered, _)| *registered == name)
            .map(|(_, executor)| Arc::clone(executor))
    }
}

/// Application-layer service orchestrating an AI request from provider
/// selection to execution.
///
/// Wraps the existing [`ProviderService`] for provider metadata, composes the
/// existing [`CredentialStore`] for credentials, and resolves the concrete
/// executor through the [`ExecutorRegistry`]. It is deliberately focused on
/// orchestration and contains no provider-specific behavior: once a provider is
/// resolved, its credential is available, and a registered executor is found,
/// the request is delegated to that provider-independent [`ProviderExecutor`].
pub(crate) struct RequestExecutionService<'a> {
    provider: ProviderService<'a>,
    executors: ExecutorRegistry,
}

impl<'a> RequestExecutionService<'a> {
    /// Create a service over the shared application [`Database`] with the
    /// supported providers registered in the built-in [`ExecutorRegistry`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            provider: ProviderService::new(db),
            executors: ExecutorRegistry::new(),
        }
    }

    /// Resolve the credential for `provider` (Task 5.1 factoring): the
    /// provider-metadata lookup plus the FR-014 credential checks that
    /// [`Self::execute`] performs, exposed so the agent-run bridge can
    /// resolve the credential before spawning the run thread. The value is
    /// never persisted, logged, or placed in any returned metadata.
    ///
    /// # Errors
    ///
    /// Same classification as [`Self::execute`] steps 1–3.
    pub(crate) fn resolve_credential(&self, provider: &str) -> Result<String> {
        // 1. Resolve the requested provider through the Provider Metadata
        //    Service (ARCHITECTURE.md §7: provider selection).
        let provider = self
            .provider
            .read_by_name(provider)
            .map_err(RequestError::from)?
            .ok_or_else(|| RequestError::UnknownProvider {
                name: provider.to_string(),
            })?;

        // 2. The provider must be available: configured and credentialed
        //    (DATABASE.md §7.5; FR-014 detects a missing credential before a
        //    request is sent). An unreachable keyring is not a definitively
        //    missing credential, so it is classified separately as an
        //    unavailable provider.
        match CredentialStore::exists(&provider.name) {
            Err(CredentialError::StorageUnavailable) => {
                return Err(RequestError::ProviderUnavailable {
                    name: provider.name,
                });
            }
            Err(err) => return Err(RequestError::Credential(err)),
            Ok(false) => {
                return Err(RequestError::MissingCredentials {
                    name: provider.name,
                })
            }
            Ok(true) => {}
        }

        // 3. Obtain the credential only now, immediately before execution, and
        //    only from the existing CredentialStore (FR-014). The value is
        //    never persisted, logged, or placed in any returned metadata.
        match CredentialStore::read(&provider.name) {
            Ok(Some(secret)) => Ok(secret),
            Ok(None) => Err(RequestError::MissingCredentials {
                name: provider.name,
            }),
            Err(CredentialError::StorageUnavailable) => Err(RequestError::ProviderUnavailable {
                name: provider.name,
            }),
            Err(err) => Err(RequestError::Credential(err)),
        }
    }

    /// Execute `request`.
    ///
    /// Resolves the requested provider through the Provider Metadata Service,
    /// verifies it is configured and has stored credentials, reads the
    /// credential from the [`CredentialStore`] only for the duration of the
    /// call, and delegates execution to the [`ProviderExecutor`].
    ///
    /// # Errors
    ///
    /// Returns [`RequestError::UnknownProvider`] when no provider with the
    /// request's `provider` name is configured; [`RequestError::MissingCredentials`]
    /// when the provider exists but has no stored credential;
    /// [`RequestError::ProviderUnavailable`] when the secure keyring cannot be
    /// reached to determine or obtain the credential;
    /// [`RequestError::ExecutorUnavailable`] when the provider is configured but
    /// has no registered executor; [`RequestError::Execution`]
    /// when the provider fails to fulfil the request; or
    /// [`RequestError::Database`] when the provider lookup fails.
    pub(crate) fn execute(&self, request: &AiRequest) -> Result<AiResponse> {
        // 1–3. Provider row, availability, and credential (shared with the
        //     Task 5.1 agent-run bridge through `resolve_credential`).
        let credential = self.resolve_credential(&request.provider)?;

        // 4. Resolve the concrete executor for this provider through the
        //    registry. A provider whose metadata exists but has no registered
        //    executor cannot fulfil the request; it fails explicitly with a
        //    classified error rather than falling back to another provider.
        let executor = self.executors.resolve(&request.provider).ok_or_else(|| {
            RequestError::ExecutorUnavailable {
                name: request.provider.clone(),
            }
        })?;

        // 5. Delegate to the provider-independent boundary. The credential is
        //    moved only into this call and dropped when it returns. Plain
        //    chat has no run to cancel, so it executes under a fresh token
        //    that never fires; cancellation is an agent-run concern.
        let idle_token = CancellationToken::new();
        executor
            .execute(request, &credential, &idle_token)
            .map_err(|err| RequestError::Execution {
                name: request.provider.clone(),
                message: err.to_string(),
            })
    }
}

/// Classified errors raised by request execution.
///
/// Unifies orchestration failures and persistence failures. No variant carries
/// a credential or other secret value, so formatting a [`RequestError`] never
/// writes a secret to the logs (ARCHITECTURE.md §9, §10, §11).
#[derive(Debug)]
pub(crate) enum RequestError {
    /// No provider with the requested `name` is configured.
    UnknownProvider {
        /// The requested provider internal name.
        name: String,
    },
    /// The provider is configured but its credential could not be obtained
    /// because the OS secure keyring is unreachable.
    ProviderUnavailable {
        /// The provider internal name.
        name: String,
    },
    /// The provider is configured but has no stored credential (FR-014).
    MissingCredentials {
        /// The provider internal name.
        name: String,
    },
    /// The provider is configured but no executor is registered for its
    /// internal name, so the request cannot be executed.
    ExecutorUnavailable {
        /// The provider internal name.
        name: String,
    },
    /// The provider failed to fulfil the request; `message` carries the
    /// classified error text (never a credential or payload).
    Execution {
        /// The provider internal name.
        name: String,
        /// The classified provider error text.
        message: String,
    },
    /// A credential-store failure other than an unreachable keyring.
    Credential(CredentialError),
    /// A provider metadata lookup failure.
    Database(DatabaseError),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { name } => {
                write!(f, "the AI provider '{name}' is not configured")
            }
            Self::ProviderUnavailable { name } => write!(
                f,
                "the AI provider '{name}' is unavailable: its credentials could not \
                 be reached in the OS secure keyring"
            ),
            Self::MissingCredentials { name } => write!(
                f,
                "the AI provider '{name}' has no stored credentials (FR-014)"
            ),
            Self::ExecutorUnavailable { name } => {
                write!(f, "the AI provider '{name}' has no registered executor")
            }
            Self::Execution { name, message } => {
                write!(f, "the AI provider '{name}' failed: {message}")
            }
            Self::Credential(err) => write!(f, "{err}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownProvider { .. }
            | Self::ProviderUnavailable { .. }
            | Self::MissingCredentials { .. }
            | Self::ExecutorUnavailable { .. }
            | Self::Execution { .. } => None,
            Self::Credential(err) => Some(err),
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for RequestError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<ProviderError> for RequestError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Database(err) => Self::Database(err),
            ProviderError::Credential(err) => Self::Credential(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn registry_resolves_openai() {
        let registry = ExecutorRegistry::new();
        // The OpenAI executor is registered under the internal `openai` name.
        assert!(registry.resolve(PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_anthropic() {
        let registry = ExecutorRegistry::new();
        // The Anthropic executor is registered under the internal `anthropic`
        // name, independently of the OpenAI registration.
        assert!(registry.resolve(ANTHROPIC_PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_gemini() {
        let registry = ExecutorRegistry::new();
        // The Gemini executor is registered under the internal `gemini` name,
        // independently of the OpenAI and Anthropic registrations.
        assert!(registry.resolve(GEMINI_PROVIDER_NAME).is_some());
    }

    #[test]
    fn registry_resolves_xkiro() {
        let registry = ExecutorRegistry::new();
        assert!(registry.resolve("xkiro").is_some());
    }

    #[test]
    fn registry_resolves_openrouter() {
        let registry = ExecutorRegistry::new();
        assert!(registry.resolve("openrouter").is_some());
    }

    #[test]
    fn registry_resolves_nvidia() {
        let registry = ExecutorRegistry::new();
        assert!(registry.resolve("nvidia").is_some());
    }

    #[test]
    fn registry_resolves_opencode_zen() {
        let registry = ExecutorRegistry::new();
        assert!(registry.resolve("opencode_zen").is_some());
    }

    #[test]
    fn registry_compatible_providers_do_not_shadow_openai() {
        let registry = ExecutorRegistry::new();
        assert!(registry.resolve("openai").is_some());
        assert!(registry.resolve("xkiro").is_some());
        assert!(registry.resolve("openrouter").is_some());
        assert!(registry.resolve("nvidia").is_some());
        assert!(registry.resolve("opencode_zen").is_some());
        assert!(registry.resolve("not-a-provider").is_none());
    }

    #[test]
    fn registry_does_not_resolve_unknown_provider() {
        let registry = ExecutorRegistry::new();
        // No executor is registered for an unregistered name; there is no
        // fallback to an arbitrary available provider.
        assert!(registry.resolve("not-a-provider").is_none());
    }

    #[test]
    fn request_service_uses_registry_without_reading_credentials() {
        // Constructing the service touches no keyring and no network: the
        // registry only maps names to executors, and credential acquisition
        // stays outside it (nothing here supplies or reads a credential).
        let db = Database::new(Connection::open_in_memory().expect("in-memory db"));
        let service = RequestExecutionService::new(&db);

        // The service resolves through its registry: `openai` is registered...
        assert!(service.executors.resolve(PROVIDER_NAME).is_some());
        // ...and an unknown provider name does not resolve to a silent fallback.
        assert!(service.executors.resolve("ghost").is_none());
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
    fn request_execution_display_includes_classified_message() {
        let err = RequestError::Execution {
            name: "openai".into(),
            message: "boom".into(),
        };
        assert_eq!(err.to_string(), "the AI provider 'openai' failed: boom");
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
    fn model_config_canonical_defaulting_fills_missing_limit() {
        use crate::application::context_stats::context_limit_for;
        let provider = "openai";
        let model = "gpt-5.6-terra";
        let config = ModelConfig::new(model).with_canonical_limits(provider);
        assert_eq!(
            config.context_limit,
            Some(context_limit_for(Some(provider), Some(model)))
        );
        assert_eq!(config.model_name, model);
        // No canonical source for temperature / max_tokens: untouched.
        assert_eq!(config.temperature, None);
        assert_eq!(config.max_tokens, None);
    }

    #[test]
    fn model_config_explicit_limit_is_preserved() {
        let config = ModelConfig::new("gpt-5.6-terra")
            .with_context_limit(Some(11_000))
            .with_canonical_limits("openai");
        assert_eq!(config.context_limit, Some(11_000));
    }

    #[test]
    fn model_config_effective_limit_matches_canonical() {
        use crate::application::context_stats::context_limit_for;
        let config = ModelConfig::new("claude-sonnet-5");
        assert_eq!(
            config.effective_context_limit("anthropic"),
            context_limit_for(Some("anthropic"), Some("claude-sonnet-5"))
        );
        let explicit = ModelConfig::new("claude-sonnet-5").with_context_limit(Some(7_000));
        assert_eq!(explicit.effective_context_limit("anthropic"), 7_000);
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
    fn model_info_for_listed_model_is_tool_capable_without_streaming() {
        // Query the hardcoded aggregation instead of duplicating IDs: the
        // first listed model of the first non-empty provider must resolve to
        // a fully-capable (except streaming) entry.
        let providers = crate::infrastructure::providers::supported_providers();
        let entry = providers
            .iter()
            .find(|entry| !entry.models.is_empty())
            .expect("at least one provider lists models");
        let info = model_info_for(entry.name.as_str(), entry.models[0].as_str());
        assert_eq!(info.name, entry.models[0]);
        assert!(info.context_limit > 0);
        assert!(info.supports_tools);
        assert!(info.supports_vision);
        assert!(info.supports_json);
        // Sync-only runtime: streaming stays false until a streaming task lands.
        assert!(!info.supports_streaming);
    }

    #[test]
    fn model_info_for_unknown_model_is_conservative() {
        let info = model_info_for("openai", "no-such-model-xyz");
        assert_eq!(info.name, "no-such-model-xyz");
        assert!(info.context_limit > 0);
        assert!(!info.supports_tools);
        assert!(!info.supports_vision);
        assert!(!info.supports_json);
        assert!(!info.supports_streaming);
    }

    #[test]
    fn model_capabilities_project_toolcall_flag() {
        let info = ModelInfo {
            name: "m".to_string(),
            context_limit: 8_000,
            supports_tools: true,
            supports_vision: false,
            supports_json: true,
            supports_streaming: false,
        };
        let caps = info.capabilities();
        assert!(caps.tool_call);
        assert!(!caps.vision);
        assert!(caps.json);
        assert!(!caps.streaming);
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
    fn tool_capable_models_filters_synthetic_mix() {
        let capable = ModelInfo {
            name: "capable".to_string(),
            context_limit: 8_000,
            supports_tools: true,
            supports_vision: true,
            supports_json: true,
            supports_streaming: false,
        };
        let legacy = ModelInfo {
            name: "legacy".to_string(),
            context_limit: 8_000,
            supports_tools: false,
            supports_vision: false,
            supports_json: false,
            supports_streaming: false,
        };
        let filtered = tool_capable_models(&[capable.clone(), legacy]);
        assert_eq!(filtered, vec![capable]);
        assert!(tool_capable_models(&[]).is_empty());
    }

    #[test]
    fn recommended_models_gates_tools_without_cross_provider_fallback() {
        // Unknown provider: empty in both modes, never another provider's list.
        assert!(recommended_models("ghost", false).is_empty());
        assert!(recommended_models("ghost", true).is_empty());
        // Known provider: plain mode lists every hardcoded ID in order...
        let all = recommended_models("openai", false);
        assert!(!all.is_empty());
        assert_eq!(
            all,
            models_for_provider("openai")
                .iter()
                .map(|info| info.name.clone())
                .collect::<Vec<_>>()
        );
        // ...while tools-required mode keeps only `tool_call` models (today
        // every listed ID is tool-capable, so the subset equals the full
        // list; the synthetic test above proves non-tool IDs are dropped).
        let gated = recommended_models("openai", true);
        assert_eq!(gated, all);
        for name in &gated {
            assert!(model_info_for("openai", name).supports_tools);
        }
    }

    #[test]
    fn models_for_provider_unknown_is_empty_no_fallback() {
        assert!(models_for_provider("ghost").is_empty());
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
            messages: Vec::new(),
            tools: Vec::new(),
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
}
