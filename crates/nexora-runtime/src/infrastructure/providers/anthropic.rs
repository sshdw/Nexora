//! Anthropic provider integration: a concrete [`ProviderExecutor`] (ROADMAP.md
//! Phase 3 вЂ” AI Providers; ARCHITECTURE.md В§7).
//!
//! Translates the provider-independent [`AiRequest`] into an Anthropic
//! non-streaming Messages request (`messages` endpoint,
//! `https://api.anthropic.com/v1/messages`), sends it over HTTPS using the
//! crate's HTTP facility (`reqwest`), and normalizes the response into the
//! provider-independent [`AiResponse`].
//!
//! Isolation: every Anthropic-specific wire type lives in this module. Nothing
//! Anthropic-specific crosses the boundary; the caller only ever sees
//! [`AiRequest`]/[`AiResponse`]/[`ExecutorError`] (the contract lives in
//! `nexora_core::provider`, NEX-ARCH-002).
//!
//! # Anthropic-specific contract notes
//!
//! - **Authentication:** Anthropic requires the API key in the `x-api-key`
//!   header (with an `anthropic-version` header), not a bearer token. The
//!   credential is placed only in headers via `reqwest`, never in the body.
//! - **System messages:** Anthropic routes instructions through a top-level
//!   `system` parameter rather than an in-`messages` entry, so [`AiRole::System`]
//!   is mapped to that field and is never emitted as a `user`/`assistant`
//!   message. The caller-supplied model is forwarded unchanged.
//! - **Required `max_tokens`:** the Messages API requires `max_tokens`; a fixed
//!   default is supplied (not a selectable model feature).
//!
//! # Security (ARCHITECTURE.md В§9, В§11, В§12)
//!
//! - The credential is supplied by the caller for the duration of the call
//!   only; it is never persisted, logged, or returned.
//! - The credential is sent only in the `x-api-key` header via `reqwest`, never
//!   in the body.
//! - Failed responses are read and classified by HTTP status **plus the
//!   `Retry-After` header and common JSON body shapes**; only the failure
//!   *category* ever reaches an error or log — never the credential, the
//!   request payload, or raw body text.
//! - All failures collapse to the single provider-independent
//!   [`ExecutorError::Failure`]; the internal [`AnthropicError`] classification
//!   (authentication, invalid request, provider/network, unexpected response) is
//!   recorded in the logs by category only.

// The module docs reference product/brand names (Anthropic, Messages) that the
// `doc_markdown` pedantic lint flags as needing backticks. Allow it locally.
#![allow(clippy::doc_markdown)]

use nexora_core::provider::{
    AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, CancellationToken,
    ExecutorError, ProviderExecutor, ReasoningEffort, StreamEvent, TokenUsage, ToolCall,
};
// `AiAttachment`, `ToolDefinition`, and `AiToolResult` are referenced only by
// this module's unit tests.
#[cfg(test)]
use nexora_core::provider::{AiAttachment, AiToolResult, ModelConfig, ToolDefinition};

use std::sync::Arc;

use super::transport::{
    Credential, HttpClient, PostOutcome, PostRequest, StreamOutcome, StreamPostError,
};
use crate::async_spawner::AsyncSpawner;

use serde::{Deserialize, Serialize};

/// Anthropic's Messages endpoint.
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = nexora_core::catalog::anthropic::PROVIDER_NAME;

/// `anthropic-version` header value required by the Messages API. This is the
/// only GA version string published by Anthropic; an unrecognized version is
/// rejected before authentication, so every request would fail even with a
/// valid API key.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic's Messages API requires `max_tokens`; a generous default is
/// supplied so the request is always valid without inventing model-selection
/// behavior.
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Anthropic models currently supported by the provider (DATABASE.md В§7.5:
/// model lists are hardcoded in the MVP and managed by the application layer).
///
/// The selected model is passed through unchanged and is never validated
/// against this list at runtime (never silently substituted, never rejected):
/// this set documents the currently supported models and anchors the
/// model-selection surface so the UI can present only supported choices.
///
/// Pricing is governed by the application-layer pricing policy table
/// (DATABASE.md В§7.8); the first entry
/// is the provider default consumed as `models[0]` by the selection surface
/// (`claude-3-5-sonnet-20240620`, the previous sole entry, was retired
/// October 28, 2025).
#[cfg(test)]
pub(crate) const SUPPORTED_MODELS: &[&str] = nexora_core::catalog::anthropic::SUPPORTED_MODELS;

/// Concrete [`ProviderExecutor`] for Anthropic.
///
/// Stateless over the shared cancellable transport client so connections
/// are pooled across requests; the per-request credential and request payload
/// are passed into each [`ProviderExecutor::execute`] call and dropped on
/// return.
pub(crate) struct AnthropicExecutor {
    client: HttpClient,
    endpoint: String,
}

impl AnthropicExecutor {
    /// Create an executor targeting the Anthropic production endpoint.
    pub(crate) fn new(spawner: Arc<dyn AsyncSpawner>) -> Self {
        Self::with_endpoint(ENDPOINT.to_string(), spawner)
    }

    /// Create an executor targeting an explicit `endpoint` (used by tests to
    /// exercise the full request/response path without a live Anthropic
    /// service).
    fn with_endpoint(endpoint: String, spawner: Arc<dyn AsyncSpawner>) -> Self {
        Self {
            client: HttpClient::new(spawner),
            endpoint,
        }
    }

    /// Translate, send, and normalize one request.
    ///
    /// Returns a provider-independent [`AiResponse`] on success, or a
    /// classified [`AnthropicError`] describing the failure category.
    fn run(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, AnthropicError> {
        let body = anthropic_request(request);
        let wire = serde_json::to_vec(&body).map_err(|_| AnthropicError::UnexpectedResponse)?;
        let outcome = self
            .client
            .post(
                token,
                &PostRequest {
                    url: self.endpoint.clone(),
                    credential: Credential::Header {
                        name: "x-api-key",
                        value: credential,
                    },
                    extra_headers: &[("anthropic-version", ANTHROPIC_VERSION)],
                    extra_headers_owned: &[],
                    body: &wire,
                    timeout: request.request_timeout,
                },
            )
            .map_err(|err| {
                if matches!(err, ExecutorError::Cancelled) {
                    AnthropicError::Cancelled
                } else {
                    AnthropicError::Network
                }
            })?;
        match outcome {
            PostOutcome::Success(bytes) => {
                let response: AnthropicResponse = serde_json::from_slice(&bytes)
                    .map_err(|_| AnthropicError::UnexpectedResponse)?;
                to_ai_response(response)
            }
            PostOutcome::Failure(snapshot) => {
                if snapshot.is_context_length_exceeded {
                    Err(AnthropicError::ContextLengthExceeded)
                } else {
                    Err(classify_status(snapshot.status, snapshot.retry_after_secs))
                }
            }
        }
    }

    fn run_stream(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<AiResponse, AnthropicError> {
        let mut body = anthropic_request(request);
        body.stream = Some(true);
        let wire = serde_json::to_vec(&body).map_err(|_| AnthropicError::UnexpectedResponse)?;
        let mut acc = AnthropicStreamAccumulator::new(&request.model);
        let outcome = self.client.post_stream(
            token,
            &PostRequest {
                url: self.endpoint.clone(),
                credential: Credential::Header {
                    name: "x-api-key",
                    value: credential,
                },
                extra_headers: &[("anthropic-version", ANTHROPIC_VERSION)],
                extra_headers_owned: &[],
                body: &wire,
                timeout: request.request_timeout,
            },
            &mut |frame| {
                let data = frame.data.trim();
                if data.is_empty() {
                    return Ok(());
                }
                let ev: AnthropicSsePayload =
                    serde_json::from_str(data).map_err(|_| AnthropicError::UnexpectedResponse)?;
                acc.apply_event(frame.event.as_deref(), ev, on_event)
            },
        );
        match outcome {
            Ok(StreamOutcome::Completed) => acc.finish(),
            Ok(StreamOutcome::Failure(snapshot)) => {
                if snapshot.is_context_length_exceeded {
                    Err(AnthropicError::ContextLengthExceeded)
                } else {
                    Err(classify_status(snapshot.status, snapshot.retry_after_secs))
                }
            }
            Err(StreamPostError::Transport(ExecutorError::Cancelled)) => {
                Err(AnthropicError::Cancelled)
            }
            Err(StreamPostError::Transport(_)) => Err(AnthropicError::Network),
            Err(StreamPostError::Handler(err)) => Err(err),
        }
    }

    fn map_error(error: AnthropicError) -> ExecutorError {
        // Record only the classification category; never the credential
        // or request payload (ARCHITECTURE.md §9, §11).
        log::warn!("anthropic request failed: {error}");
        match error {
            AnthropicError::InvalidRequest => ExecutorError::InvalidRequest,
            AnthropicError::ContextLengthExceeded => ExecutorError::ContextLengthExceeded,
            AnthropicError::Authentication => ExecutorError::Authentication,
            AnthropicError::PaymentRequired => ExecutorError::PaymentRequired,
            AnthropicError::Network => ExecutorError::Network,
            AnthropicError::RateLimited { retry_after_secs } => {
                ExecutorError::RateLimited { retry_after_secs }
            }
            AnthropicError::ProviderUnavailable => ExecutorError::ProviderUnavailable,
            AnthropicError::UnexpectedResponse => ExecutorError::UnexpectedResponse,
            AnthropicError::Provider => ExecutorError::Failure,
            AnthropicError::Cancelled => ExecutorError::Cancelled,
        }
    }
}

impl ProviderExecutor for AnthropicExecutor {
    fn execute(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        self.run(request, credential, token)
            .map_err(Self::map_error)
    }

    fn execute_stream_events(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<AiResponse, ExecutorError> {
        self.run_stream(request, credential, token, on_event)
            .map_err(Self::map_error)
    }
}

/// Anthropic Messages request body, mapped from the provider-independent
/// [`AiRequest`].
///
/// `system` is a top-level Anthropic-specific parameter and is omitted (via
/// `skip_serializing_if`) when no system message is present, so the wire shape
/// matches Anthropic's contract in both cases.
///
/// `tools` carries Anthropic-native tool definitions (`name`/`description`/
/// `input_schema`) only when `AiRequest.tools` is non-empty; an empty list is
/// omitted entirely to keep the wire payload byte-compatible with plain chat.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    system: Vec<AnthropicSystemBlock>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<AnthropicWireTool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    output_config: Option<AnthropicOutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    stream: Option<bool>,
}

/// Anthropic prompt-caching breakpoint marker (`{"type": "ephemeral"}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct AnthropicCacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

impl AnthropicCacheControl {
    const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

/// One top-level `system` text block with optional prompt caching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

#[derive(Debug, Serialize)]
struct AnthropicOutputConfig {
    effort: &'static str,
}

/// Anthropic-native tool definition (Messages `tools` array).
///
/// Mirrors the Anthropic wire contract:
/// `{ "name", "description", "input_schema": <JSON Schema>, "cache_control"? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AnthropicWireTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<AnthropicCacheControl>,
}

/// One Anthropic chat message. Only `user` and `assistant` roles are valid
/// inside `messages`; system instructions are routed to the top-level `system`
/// field instead.
///
/// `content` is a plain string for the common no-attachment case and a block
/// array only when the turn carries binary attachments (FR-008).
#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

/// Anthropic Messages `content` values: a plain string, or an array of
/// content blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicBlock>),
}

/// One Anthropic content block: text, a base64 image, a base64 PDF
/// document, a `tool_use` invocation, or a `tool_result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlock {
    /// A plain text segment.
    Text { text: String },
    /// An image supplied as inline base64 data (FR-008).
    Image { source: AnthropicSource },
    /// A PDF document supplied as inline base64 data (FR-008).
    Document { source: AnthropicSource },
    /// A model tool invocation (assistant turn).
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The result of one tool invocation (tool-result turn).
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// The base64 data source shared by image and document blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AnthropicSource {
    /// Always `"base64"` for this integration (serialized as `"type"`).
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
}

/// Translate a provider-independent request into an Anthropic Messages request.
///
/// The selected model is passed through unchanged вЂ” it is never silently
/// substituted (FR-004). [`AiRole::System`] messages are aggregated into the
/// top-level `system` parameter; `user` and `assistant` messages map to the
/// corresponding Anthropic `messages.role` and remain in chronological order.
fn anthropic_request(request: &AiRequest) -> AnthropicRequest {
    let system = collect_system(&request.messages)
        .map(|text| {
            vec![AnthropicSystemBlock {
                kind: "text",
                text,
                cache_control: Some(AnthropicCacheControl::ephemeral()),
            }]
        })
        .unwrap_or_default();
    let messages: Vec<AnthropicMessage> = request
        .messages
        .iter()
        .filter(|message| message.role != AiRole::System)
        .map(|message| AnthropicMessage {
            role: match message.role {
                // User turns and tool results are both user-role on the wire;
                // a tool result additionally carries a tool_result block
                // (Anthropic contract).
                AiRole::User | AiRole::Tool => "user",
                // System is filtered out above and mapped to the top-level
                // `system` field by `collect_system`; this arm is unreachable in
                // practice but keeps the match exhaustive.
                AiRole::Assistant => "assistant",
                AiRole::System => "system",
            }
            .to_string(),
            // Attachment payloads are rendered per the Anthropic contract:
            // inline text file contents become part of the turn text; base64
            // images and PDFs become `image` / `document` blocks (FR-008).
            content: anthropic_content(message),
        })
        .collect();
    let tool_count = request.tools.len();
    let tools: Vec<AnthropicWireTool> = request
        .tools
        .iter()
        .enumerate()
        .map(|(idx, tool)| AnthropicWireTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
            // Place the ephemeral cache breakpoint on the final tool so the
            // entire tool-definition prefix is cached across turns (S11-09).
            cache_control: (idx + 1 == tool_count).then(AnthropicCacheControl::ephemeral),
        })
        .collect();
    let max_tokens = request
        .model_config
        .as_ref()
        .and_then(|cfg| cfg.max_tokens)
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let output_config = request
        .model_config
        .as_ref()
        .and_then(|cfg| cfg.effort.as_deref())
        .and_then(ReasoningEffort::parse)
        .map(|effort| AnthropicOutputConfig {
            effort: match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
                ReasoningEffort::XHigh => "max",
            },
        });
    AnthropicRequest {
        model: request.model.clone(),
        max_tokens,
        messages,
        system,
        tools,
        output_config,
        stream: None,
    }
}

/// Build the Anthropic `content` value for one message.
///
/// Plain string when the turn carries no binary attachments (identical to the
/// pre-FR-008 wire shape); otherwise a text block followed by one base64
/// `image` or `document` block per binary attachment.
fn anthropic_content(message: &AiMessage) -> AnthropicContent {
    // Assistant agent turn with structured tool calls: an optional text block
    // (only when narration is non-empty) followed by one `tool_use` block per
    // call, answering them in order.
    if message.role == AiRole::Assistant && !message.tool_calls.is_empty() {
        let mut blocks: Vec<AnthropicBlock> = Vec::new();
        if !message.content.trim().is_empty() {
            blocks.push(AnthropicBlock::Text {
                text: message.content.clone(),
            });
        }
        for call in &message.tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::default()));
            blocks.push(AnthropicBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input,
            });
        }
        return AnthropicContent::Blocks(blocks);
    }
    // Tool result turn (serialized as a user message with a tool_result block).
    if message.role == AiRole::Tool {
        let result = message.tool_result.as_ref().expect("tool result present");
        return AnthropicContent::Blocks(vec![AnthropicBlock::ToolResult {
            tool_use_id: result.call_id.clone(),
            content: result.content.clone(),
        }]);
    }
    let mut blocks: Vec<AnthropicBlock> = Vec::new();
    for attachment in &message.attachments {
        let AiAttachmentPayload::Base64(data) = &attachment.payload else {
            continue;
        };
        let media_type = attachment
            .mime_type
            .as_deref()
            .unwrap_or("application/octet-stream")
            .to_string();
        let source = AnthropicSource {
            kind: "base64",
            data: data.clone(),
            media_type: media_type.clone(),
        };
        blocks.push(if media_type == "application/pdf" {
            AnthropicBlock::Document { source }
        } else {
            AnthropicBlock::Image { source }
        });
    }
    if blocks.is_empty() {
        AnthropicContent::Text(message.composed_content())
    } else {
        blocks.insert(
            0,
            AnthropicBlock::Text {
                text: message.composed_content(),
            },
        );
        AnthropicContent::Blocks(blocks)
    }
}

/// Aggregate every [`AiRole::System`] message into the Anthropic top-level
/// `system` parameter.
///
/// Anthropic expects a single `system` value; when the provider-independent
/// request carries multiple system turns they are joined with a blank line.
/// Returns [`None`] (and is omitted from the wire payload) when there are no
/// system messages.
fn collect_system(messages: &[AiMessage]) -> Option<String> {
    let combined: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == AiRole::System)
        .map(|message| message.content.as_str())
        .collect();
    if combined.is_empty() {
        None
    } else {
        Some(combined.join("\n\n"))
    }
}

/// A normalized Anthropic Messages response.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    model: String,
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::struct_field_names)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// One content block from an Anthropic response.
///
/// Non-streaming completions produce `text` blocks (`{type:"text", text:"..."}`)
/// and tool invocations produce `tool_use` blocks
/// (`{type:"tool_use", id, name, input}`); unknown block types are ignored.
/// The `type` discriminator is preserved so extraction can dispatch by kind.
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

/// Normalize a successful Anthropic response into the provider-independent
/// [`AiResponse`].
///
/// Text blocks are concatenated in wire order to produce `AiResponse.content`;
/// `tool_use` blocks map in order to `AiResponse.tool_calls` with
/// `arguments` as a JSON string of the `input` object. A tool-only response
/// yields `content == ""`; a text-only response yields an empty `tool_calls`
/// vector. If the response contains neither extractable text nor a `tool_use`
/// block (empty `content`, no `text`/`tool_use` block, or blocks missing
/// required fields), this returns [`AnthropicError::UnexpectedResponse`]
/// rather than inventing a partial response shape.
fn to_ai_response(response: AnthropicResponse) -> Result<AiResponse, AnthropicError> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in response.content {
        match block.kind.as_str() {
            "text" => {
                if let Some(text) = block.text {
                    text_parts.push(text);
                }
            }
            "tool_use" => {
                let id = block.id.ok_or(AnthropicError::UnexpectedResponse)?;
                let name = block.name.ok_or(AnthropicError::UnexpectedResponse)?;
                let input = block
                    .input
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
                let arguments = serde_json::to_string(&input)
                    .map_err(|_| AnthropicError::UnexpectedResponse)?;
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                    // Anthropic responses carry no reasoning signature.
                    thought_signature: None,
                });
            }
            _ => {}
        }
    }
    if text_parts.is_empty() && tool_calls.is_empty() {
        return Err(AnthropicError::UnexpectedResponse);
    }
    let content = if text_parts.is_empty() {
        String::new()
    } else {
        text_parts.join("")
    };
    let usage = response.usage.and_then(|u| {
        let total_input = u
            .input_tokens
            .saturating_add(u.cache_read_input_tokens)
            .saturating_add(u.cache_creation_input_tokens);
        if total_input == 0 && u.output_tokens == 0 {
            None
        } else {
            Some(TokenUsage {
                input_tokens: total_input,
                output_tokens: u.output_tokens,
                cached_tokens: u.cache_read_input_tokens,
            })
        }
    });
    Ok(AiResponse {
        content,
        model: response.model,
        tool_calls,
        usage,
    })
}

#[derive(Debug, Deserialize)]
struct AnthropicSsePayload {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    message: Option<AnthropicSseMessage>,
    #[serde(default)]
    content_block: Option<AnthropicSseContentBlock>,
    #[serde(default)]
    delta: Option<AnthropicSseDelta>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    #[serde(default)]
    error: Option<AnthropicSseError>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseDelta {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseError {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Debug)]
struct PendingAnthropicTool {
    tool_index: usize,
    id: String,
    name: String,
    arguments: String,
}

struct AnthropicStreamAccumulator {
    model: String,
    text: String,
    saw_text_or_tool: bool,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_creation_tokens: u64,
    next_tool_index: usize,
    active_tools: std::collections::BTreeMap<usize, PendingAnthropicTool>,
    finished_tools: Vec<PendingAnthropicTool>,
}

impl AnthropicStreamAccumulator {
    fn new(requested_model: &str) -> Self {
        Self {
            model: requested_model.to_string(),
            text: String::new(),
            saw_text_or_tool: false,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            next_tool_index: 0,
            active_tools: std::collections::BTreeMap::new(),
            finished_tools: Vec::new(),
        }
    }

    fn apply_content_block_start(
        &mut self,
        block_idx: usize,
        block: AnthropicSseContentBlock,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<(), AnthropicError> {
        match block.kind.as_str() {
            "text" => {
                if let Some(t) = block.text.filter(|s| !s.is_empty()) {
                    self.saw_text_or_tool = true;
                    self.text.push_str(&t);
                    on_event(StreamEvent::TextDelta(t));
                }
            }
            "thinking" => {
                if let Some(t) = block.thinking.filter(|s| !s.is_empty()) {
                    on_event(StreamEvent::ThinkingDelta(t));
                }
            }
            "tool_use" => {
                self.saw_text_or_tool = true;
                let id = block.id.ok_or(AnthropicError::UnexpectedResponse)?;
                let name = block.name.ok_or(AnthropicError::UnexpectedResponse)?;
                let tool_index = self.next_tool_index;
                self.next_tool_index += 1;
                on_event(StreamEvent::ToolCallDelta {
                    index: tool_index,
                    id: Some(id.clone()),
                    name: Some(name.clone()),
                    arguments_delta: None,
                });
                self.active_tools.insert(
                    block_idx,
                    PendingAnthropicTool {
                        tool_index,
                        id,
                        name,
                        arguments: String::new(),
                    },
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_content_block_delta(
        &mut self,
        block_idx: usize,
        delta: AnthropicSseDelta,
        on_event: &dyn Fn(StreamEvent),
    ) {
        match delta.kind.as_deref().unwrap_or_default() {
            "text_delta" => {
                if let Some(t) = delta.text.filter(|s| !s.is_empty()) {
                    self.saw_text_or_tool = true;
                    self.text.push_str(&t);
                    on_event(StreamEvent::TextDelta(t));
                }
            }
            "thinking_delta" => {
                if let Some(t) = delta.thinking.filter(|s| !s.is_empty()) {
                    on_event(StreamEvent::ThinkingDelta(t));
                }
            }
            "input_json_delta" => {
                if let Some(partial) = delta.partial_json {
                    if let Some(pending) = self.active_tools.get_mut(&block_idx) {
                        pending.arguments.push_str(&partial);
                        if !partial.is_empty() {
                            on_event(StreamEvent::ToolCallDelta {
                                index: pending.tool_index,
                                id: None,
                                name: None,
                                arguments_delta: Some(partial),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn apply_event(
        &mut self,
        sse_event: Option<&str>,
        payload: AnthropicSsePayload,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<(), AnthropicError> {
        let kind = payload.kind.as_deref().or(sse_event).unwrap_or_default();
        match kind {
            "error" => {
                if payload
                    .error
                    .as_ref()
                    .and_then(|e| e.kind.as_deref())
                    .is_some_and(|k| k == "overloaded_error")
                {
                    return Err(AnthropicError::ProviderUnavailable);
                }
                return Err(AnthropicError::Provider);
            }
            "message_start" => {
                if let Some(msg) = payload.message {
                    if let Some(model) = msg.model.filter(|m| !m.is_empty()) {
                        self.model = model;
                    }
                    if let Some(usage) = msg.usage {
                        self.input_tokens = self.input_tokens.max(usage.input_tokens);
                        self.output_tokens = self.output_tokens.max(usage.output_tokens);
                        self.cached_tokens = self.cached_tokens.max(usage.cache_read_input_tokens);
                        self.cache_creation_tokens = self
                            .cache_creation_tokens
                            .max(usage.cache_creation_input_tokens);
                    }
                }
            }
            "content_block_start" => {
                if let (Some(block_idx), Some(block)) = (payload.index, payload.content_block) {
                    self.apply_content_block_start(block_idx, block, on_event)?;
                }
            }
            "content_block_delta" => {
                if let (Some(block_idx), Some(delta)) = (payload.index, payload.delta) {
                    self.apply_content_block_delta(block_idx, delta, on_event);
                }
            }
            "content_block_stop" => {
                if let Some(block_idx) = payload.index {
                    if let Some(pending) = self.active_tools.remove(&block_idx) {
                        self.finished_tools.push(pending);
                    }
                }
            }
            "message_delta" => {
                if let Some(usage) = payload.usage {
                    self.input_tokens = self.input_tokens.max(usage.input_tokens);
                    self.output_tokens = self.output_tokens.max(usage.output_tokens);
                    self.cached_tokens = self.cached_tokens.max(usage.cache_read_input_tokens);
                    self.cache_creation_tokens = self
                        .cache_creation_tokens
                        .max(usage.cache_creation_input_tokens);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self) -> Result<AiResponse, AnthropicError> {
        for (_, pending) in self.active_tools {
            self.finished_tools.push(pending);
        }
        self.finished_tools.sort_by_key(|t| t.tool_index);
        let tool_calls: Vec<ToolCall> = self
            .finished_tools
            .into_iter()
            .map(|t| ToolCall {
                id: t.id,
                name: t.name,
                arguments: if t.arguments.is_empty() {
                    "{}".to_string()
                } else {
                    t.arguments
                },
                thought_signature: None,
            })
            .collect();
        if !self.saw_text_or_tool && tool_calls.is_empty() && self.text.is_empty() {
            return Err(AnthropicError::UnexpectedResponse);
        }
        let total_input = self
            .input_tokens
            .saturating_add(self.cached_tokens)
            .saturating_add(self.cache_creation_tokens);
        let usage = if total_input == 0 && self.output_tokens == 0 {
            None
        } else {
            Some(TokenUsage {
                input_tokens: total_input,
                output_tokens: self.output_tokens,
                cached_tokens: self.cached_tokens,
            })
        };
        Ok(AiResponse {
            content: self.text,
            model: self.model,
            tool_calls,
            usage,
        })
    }
}

/// Classify a non-success HTTP status into a secret-free failure category.
///
/// Mirrors the unified [`super::transport::map_http_error`] table so every
/// provider maps a status to the same category: 401/403 auth, 402 payment,
/// 413 context, 400/404 invalid (the 400-classifier promotes window language
/// via the snapshot flag before this runs), 429 rate-limit, 5xx unavailable.
///
/// `retry_after_secs` is the merged provider `Retry-After` hint: the response
/// header first, then common JSON body shapes (see
/// [`super::transport::extract_retry_after`]), capped upstream. It is carried
/// only by the 429 [`AnthropicError::RateLimited`] category.
fn classify_status(status: u16, retry_after_secs: Option<u64>) -> AnthropicError {
    // Single-table delegation: the unified transport mapping owns the
    // status→category table (the snapshot context flag is handled by the
    // caller, so `false` here still covers a bare 413 via the shared table).
    match super::transport::map_http_error(status, retry_after_secs, false) {
        ExecutorError::Authentication => AnthropicError::Authentication,
        ExecutorError::PaymentRequired => AnthropicError::PaymentRequired,
        ExecutorError::ContextLengthExceeded => AnthropicError::ContextLengthExceeded,
        ExecutorError::InvalidRequest => AnthropicError::InvalidRequest,
        ExecutorError::RateLimited { retry_after_secs } => {
            AnthropicError::RateLimited { retry_after_secs }
        }
        ExecutorError::ProviderUnavailable => AnthropicError::ProviderUnavailable,
        _ => AnthropicError::Provider,
    }
}

/// Classified Anthropic failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] вЂ” this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug, Clone, Copy)]
enum AnthropicError {
    /// The Anthropic endpoint rejected the request as malformed (HTTP 400) or
    /// addressed an unknown model/route (HTTP 404).
    InvalidRequest,
    /// The prompt exceeds the model context window: a client-error status
    /// whose body names the context window or token limit. Never retried;
    /// the agent loop treats it as a compaction trigger.
    ContextLengthExceeded,
    /// The credential was rejected (HTTP 401).
    Authentication,
    /// The provider reported insufficient credits/quota (HTTP 402): the
    /// credential is valid but the account cannot pay for this call.
    PaymentRequired,
    /// A network/transport failure (connection refused, DNS, timeout, ...).
    Network,
    /// The provider rate limited the request (HTTP 429), carrying the
    /// provider's `Retry-After` hint when it was a valid integer.
    RateLimited { retry_after_secs: Option<u64> },
    /// The provider is unavailable or overloaded (HTTP 5xx).
    ProviderUnavailable,
    /// A provider-side error (HTTP 4xx other than 400/401, 5xx, 429, ...).
    Provider,
    /// The response was not a recognizable message completion (e.g. missing
    /// text content or malformed JSON).
    UnexpectedResponse,
    /// The run was cancelled before the request completed: the in-flight
    /// attempt was aborted and no response was consumed.
    Cancelled,
}

impl std::fmt::Display for AnthropicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(f, "the Anthropic request was invalid (400)"),
            Self::ContextLengthExceeded => {
                write!(f, "the Anthropic prompt exceeds the model context window")
            }
            Self::Authentication => write!(f, "Anthropic rejected the credential (401)"),
            Self::PaymentRequired => write!(
                f,
                "provider reported insufficient credits/quota (HTTP 402); \
                 top up or switch to a free-tier ID"
            ),
            Self::Network => write!(f, "Anthropic network or transport failure"),
            Self::RateLimited { .. } => write!(f, "Anthropic rate limit (429)"),
            Self::ProviderUnavailable => write!(f, "Anthropic unavailable (5xx)"),
            Self::Provider => write!(f, "Anthropic provider failure"),
            Self::UnexpectedResponse => write!(f, "Anthropic returned an unexpected response"),
            Self::Cancelled => write!(f, "Anthropic request cancelled before completion"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_spawner::test_spawner;

    fn sample_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![
                AiMessage {
                    role: AiRole::System,
                    content: "You are a helpful assistant.".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::User,
                    content: "Hello".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: "Hi there".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
            ]),
            tools: std::sync::Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        }
    }

    #[test]
    fn system_message_is_mapped_to_top_level_field() {
        let body = anthropic_request(&sample_request());
        // The system instruction is extracted into the top-level `system`
        // parameter with ephemeral prompt caching, not emitted as a message.
        assert_eq!(body.system.len(), 1);
        assert_eq!(body.system[0].kind, "text");
        assert_eq!(body.system[0].text, "You are a helpful assistant.");
        assert_eq!(
            body.system[0].cache_control,
            Some(AnthropicCacheControl::ephemeral())
        );
        assert!(body.messages.iter().all(|m| m.role != "system"));
    }

    #[test]
    fn user_message_is_mapped_to_messages() {
        let body = anthropic_request(&sample_request());
        let user = body
            .messages
            .iter()
            .find(|m| m.role == "user")
            .expect("a user message is present");
        assert_eq!(user.content, AnthropicContent::Text("Hello".to_string()));
    }

    #[test]
    fn assistant_message_is_mapped_to_messages() {
        let body = anthropic_request(&sample_request());
        let assistant = body
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("an assistant message is present");
        assert_eq!(
            assistant.content,
            AnthropicContent::Text("Hi there".to_string())
        );
    }

    #[test]
    fn selected_model_is_preserved() {
        let body = anthropic_request(&sample_request());
        // The caller-supplied model is forwarded unchanged, never substituted.
        assert_eq!(body.model, "claude-sonnet-5");
        assert_eq!(body.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn request_serialization_matches_anthropic_contract() {
        let json = serde_json::to_string(&anthropic_request(&sample_request())).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");

        // Model + required max_tokens present.
        assert_eq!(value["model"], "claude-sonnet-5");
        assert_eq!(value["max_tokens"], DEFAULT_MAX_TOKENS);

        // System content is a top-level cached text block array, not a message.
        assert_eq!(
            value["system"],
            serde_json::json!([{
                "type": "text",
                "text": "You are a helpful assistant.",
                "cache_control": { "type": "ephemeral" }
            }])
        );
        let has_system_role = value["messages"]
            .as_array()
            .expect("messages is an array")
            .iter()
            .any(|m| m["role"] == "system");
        assert!(!has_system_role, "no system role inside messages");

        // User and assistant messages remain in chronological order.
        let roles: Vec<&str> = value["messages"]
            .as_array()
            .expect("messages is an array")
            .iter()
            .map(|m| m["role"].as_str().expect("role is a string"))
            .collect();
        assert_eq!(roles, vec!["user", "assistant"]);
    }

    #[test]
    fn request_omits_system_when_absent() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: std::sync::Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let body = anthropic_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        assert!(value.get("system").is_none());
        assert!(body.system.is_empty());
    }

    #[test]
    fn multiple_system_messages_are_joined() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![
                AiMessage {
                    role: AiRole::System,
                    content: "First rule.".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::System,
                    content: "Second rule.".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
            ]),
            tools: std::sync::Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let body = anthropic_request(&request);
        assert_eq!(body.system.len(), 1);
        assert_eq!(body.system[0].text, "First rule.\n\nSecond rule.");
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        assert_eq!(
            value["system"],
            serde_json::json!([{
                "type": "text",
                "text": "First rule.\n\nSecond rule.",
                "cache_control": { "type": "ephemeral" }
            }])
        );
    }

    #[test]
    fn tool_use_without_input_maps_to_empty_object_string() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "tool_use".to_string(),
                text: None,
                id: Some("toolu_no_args".to_string()),
                name: Some("no_args_tool".to_string()),
                input: None,
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("no-args maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].id, "toolu_no_args");
        assert_eq!(ai.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn successful_response_is_normalized() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "text".to_string(),
                text: Some("pong".to_string()),
                id: None,
                name: None,
                input: None,
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("response converts");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "claude-sonnet-5");
    }

    #[test]
    fn response_without_content_is_unexpected() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: Vec::new(),
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(AnthropicError::UnexpectedResponse)
        ));
    }

    #[test]
    fn response_block_without_text_is_unexpected() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "text".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
            }],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(AnthropicError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(
            classify_status(400, None),
            AnthropicError::InvalidRequest
        ));
        // 404 (unknown model/route) is invalid like 400, matching OpenAI.
        assert!(matches!(
            classify_status(404, None),
            AnthropicError::InvalidRequest
        ));
        assert!(matches!(
            classify_status(401, None),
            AnthropicError::Authentication
        ));
        assert!(matches!(
            classify_status(403, None),
            AnthropicError::Authentication
        ));
        // 429 carries the Retry-After hint (None when absent/not parseable).
        assert!(matches!(
            classify_status(429, None),
            AnthropicError::RateLimited {
                retry_after_secs: None
            }
        ));
        assert!(matches!(
            classify_status(500, None),
            AnthropicError::ProviderUnavailable
        ));
        assert!(matches!(
            classify_status(503, None),
            AnthropicError::ProviderUnavailable
        ));
        // Other 4xx remain the catch-all provider failure.
        assert!(matches!(
            classify_status(422, None),
            AnthropicError::Provider
        ));
    }

    #[test]
    fn executor_maps_failure_to_boundary_failure() {
        let executor = AnthropicExecutor {
            client: HttpClient::new(test_spawner()),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
        };
        // The boundary surfaces the classified category (here: network), never an
        // Anthropic-specific or secret-bearing type.
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::Network)));
    }

    #[test]
    fn credential_never_appears_in_returned_error() {
        let executor = AnthropicExecutor {
            client: HttpClient::new(test_spawner()),
            endpoint: "http://127.0.0.1:1".to_string(),
        };
        let credential = "sk-secret-example";
        let err = executor
            .execute(&sample_request(), credential, &CancellationToken::new())
            .expect_err("unreachable endpoint must fail");
        // The classified boundary error carries a fixed, secret-free message.
        assert_eq!(
            format!("{err}"),
            "the AI provider could not be reached (network or timeout)"
        );
        assert!(!format!("{err}").contains(credential));
    }

    #[test]
    fn run_classifies_network_failure() {
        let executor =
            AnthropicExecutor::with_endpoint("http://127.0.0.1:1".to_string(), test_spawner());
        let result = executor.run(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(AnthropicError::Network)));
    }

    #[test]
    fn run_classifies_authentication_failure() {
        let (endpoint, _captured, server) = spawn_server(401, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.run(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(AnthropicError::Authentication)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_invalid_request_failure() {
        let (endpoint, _captured, server) = spawn_server(400, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.run(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(AnthropicError::InvalidRequest)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_provider_failure() {
        let (endpoint, _captured, server) = spawn_server(422, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.run(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(AnthropicError::Provider)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_unknown_model_failure_as_invalid_request() {
        // 404 (unknown model/route) surfaces as invalid request, matching the
        // OpenAI-compatible path.
        let (endpoint, _captured, server) = spawn_server(404, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.run(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(AnthropicError::InvalidRequest)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn binary_attachments_serialize_as_anthropic_blocks() {
        let mut request = sample_request();
        std::sync::Arc::get_mut(&mut request.messages)
            .expect("test builds the sole owner")
            .push(AiMessage {
                role: AiRole::User,
                content: "What is in these files?".to_string(),
                attachments: vec![
                    AiAttachment {
                        file_name: "chart.png".to_string(),
                        file_size_bytes: Some(4),
                        mime_type: Some("image/png".to_string()),
                        payload: AiAttachmentPayload::Base64("cG5nIQ==".to_string()),
                    },
                    AiAttachment {
                        file_name: "paper.pdf".to_string(),
                        file_size_bytes: Some(5),
                        mime_type: Some("application/pdf".to_string()),
                        payload: AiAttachmentPayload::Base64("JVBERi0=".to_string()),
                    },
                ],
                tool_calls: Vec::new(),
                tool_result: None,
            });
        let json = serde_json::to_string(&anthropic_request(&request)).expect("serialize");

        // Text block first, then a base64 image block and a base64 PDF
        // document block вЂ” per the Messages API content-block contract.
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"type\":\"image\""));
        assert!(json.contains("\"type\":\"document\""));
        assert!(json.contains("\"media_type\":\"image/png\""));
        assert!(json.contains("\"media_type\":\"application/pdf\""));
        assert!(json.contains("cG5nIQ=="));
        // No filesystem path can appear: the boundary never carries one.
        assert!(!json.contains("/tmp/"));
    }

    #[test]
    fn text_attachments_are_inlined_into_the_turn_text() {
        let mut request = sample_request();
        std::sync::Arc::get_mut(&mut request.messages)
            .expect("test builds the sole owner")
            .push(AiMessage {
                role: AiRole::User,
                content: "Summarize".to_string(),
                attachments: vec![AiAttachment {
                    file_name: "notes.txt".to_string(),
                    file_size_bytes: Some(5),
                    mime_type: Some("text/plain".to_string()),
                    payload: AiAttachmentPayload::Text("revenue rose 12 percent".to_string()),
                }],
                tool_calls: Vec::new(),
                tool_result: None,
            });
        let body = anthropic_request(&request);
        let user = body
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("user message");
        let AnthropicContent::Text(text) = &user.content else {
            panic!("text-only attachments keep the plain string wire shape");
        };
        assert!(text.contains("begin attached file contents"));
        assert!(text.contains("revenue rose 12 percent"));
    }

    #[test]
    fn executor_round_trips_through_local_server() {
        let body = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#;
        let (endpoint, _captured, server) = spawn_server(200, body);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "claude-sonnet-5");
    }

    #[test]
    fn request_authenticates_with_x_api_key_header() {
        let body = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#;
        let (endpoint, captured, server) = spawn_server(200, body);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let credential = "sk-secret-example";
        executor
            .execute(&sample_request(), credential, &CancellationToken::new())
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        let request_text = String::from_utf8(
            captured
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("server captured the request"),
        )
        .expect("utf8 request");

        // The credential is sent only as the x-api-key header, never as a bearer
        // token, and the required version header is present.
        assert!(
            request_text.contains("x-api-key: sk-secret-example"),
            "x-api-key header must carry the credential: {request_text}"
        );
        assert!(
            request_text.contains("anthropic-version: 2023-06-01"),
            "anthropic-version header must be set: {request_text}"
        );
        assert!(
            !request_text.contains("authorization: bearer"),
            "must not use bearer auth: {request_text}"
        );
    }

    #[test]
    fn request_with_tools_serializes_with_native_shape() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "Use the tool".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: std::sync::Arc::new(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather for a location".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            }]),
            model_config: None,
            request_timeout: None,
        };
        let json = serde_json::to_string(&anthropic_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let tools = value
            .get("tools")
            .expect("tools present")
            .as_array()
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        // Anthropic native wire shape: name / description / input_schema (not
        // OpenAI's type/function nesting).
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get the weather for a location");
        assert!(tools[0]["input_schema"]["properties"]["location"].is_object());
        assert!(!json.contains("\"type\":\"function\""));
    }

    #[test]
    fn request_without_tools_omits_tools_key() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: std::sync::Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let json = serde_json::to_string(&anthropic_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            value.get("tools").is_none(),
            "tools key must be absent when empty"
        );
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn response_with_tool_use_maps_to_ai_response() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "tool_use".to_string(),
                text: None,
                id: Some("toolu_123".to_string()),
                name: Some("get_weather".to_string()),
                input: Some(serde_json::json!({"location":"Paris"})),
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("tool_use response maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.model, "claude-sonnet-5");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].id, "toolu_123");
        assert_eq!(ai.tool_calls[0].name, "get_weather");
        assert_eq!(ai.tool_calls[0].arguments, "{\"location\":\"Paris\"}");
    }

    #[test]
    fn response_with_content_and_tool_use_maps_both() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![
                ContentBlock {
                    kind: "text".to_string(),
                    text: Some("I will call the tool ".to_string()),
                    id: None,
                    name: None,
                    input: None,
                },
                ContentBlock {
                    kind: "tool_use".to_string(),
                    text: None,
                    id: Some("toolu_456".to_string()),
                    name: Some("search".to_string()),
                    input: Some(serde_json::json!({"query":"test"})),
                },
            ],
            usage: None,
        };
        let ai = to_ai_response(response).expect("text + tool_use maps");
        assert_eq!(ai.content, "I will call the tool ");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].name, "search");
        assert_eq!(ai.tool_calls[0].arguments, "{\"query\":\"test\"}");
    }

    #[test]
    fn plain_text_response_without_tools_still_maps_correctly() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "text".to_string(),
                text: Some("Hello to you too.".to_string()),
                id: None,
                name: None,
                input: None,
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("plain text maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "claude-sonnet-5");
        assert!(ai.tool_calls.is_empty());
    }

    #[test]
    fn tool_only_response_yields_empty_content() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![
                ContentBlock {
                    kind: "tool_use".to_string(),
                    text: None,
                    id: Some("toolu_1".to_string()),
                    name: Some("get_weather".to_string()),
                    input: Some(serde_json::json!({"location":"Paris"})),
                },
                ContentBlock {
                    kind: "tool_use".to_string(),
                    text: None,
                    id: Some("toolu_2".to_string()),
                    name: Some("get_time".to_string()),
                    input: Some(serde_json::json!({})),
                },
            ],
            usage: None,
        };
        let ai = to_ai_response(response).expect("tool-only maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.tool_calls.len(), 2);
        assert_eq!(ai.tool_calls[0].id, "toolu_1");
        assert_eq!(ai.tool_calls[1].id, "toolu_2");
    }

    #[test]
    fn multiple_text_blocks_are_concatenated() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![
                ContentBlock {
                    kind: "text".to_string(),
                    text: Some("Hello ".to_string()),
                    id: None,
                    name: None,
                    input: None,
                },
                ContentBlock {
                    kind: "text".to_string(),
                    text: Some("world".to_string()),
                    id: None,
                    name: None,
                    input: None,
                },
            ],
            usage: None,
        };
        let ai = to_ai_response(response).expect("concatenated text maps");
        assert_eq!(ai.content, "Hello world");
        assert!(ai.tool_calls.is_empty());
    }

    #[test]
    fn response_without_text_and_without_tool_use_is_unexpected() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "text".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
            }],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(AnthropicError::UnexpectedResponse)
        ));
    }

    /// The native tool round-trip serializes as Anthropic content blocks: an
    /// assistant turn with a `tool_use` block per call (`input` as an object
    /// parsed from the arguments string) and a tool result as a user turn
    /// carrying a `tool_result` block.
    #[test]
    fn tool_round_trip_request_serializes_native_blocks() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: std::sync::Arc::new(vec![
                AiMessage {
                    role: AiRole::User,
                    content: "List files".to_string(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_7".to_string(),
                        name: "list_directory".to_string(),
                        arguments: r#"{"path":"."}"#.to_string(),
                        thought_signature: None,
                    }],
                    tool_result: None,
                },
                AiMessage {
                    role: AiRole::Tool,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: Some(AiToolResult {
                        call_id: "call_7".to_string(),
                        name: "list_directory".to_string(),
                        content: "a.txt".to_string(),
                    }),
                },
            ]),
            tools: std::sync::Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        };
        let body = anthropic_request(&request);
        let value: serde_json::Value =
            serde_json::to_value(&body).expect("request body serializes");

        assert_eq!(
            value["messages"][1],
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "call_7",
                        "name": "list_directory",
                        "input": { "path": "." }
                    }
                ]
            })
        );
        assert_eq!(
            value["messages"][2],
            serde_json::json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_7",
                        "content": "a.txt"
                    }
                ]
            })
        );
    }

    /// Timeouts are transport failures and therefore retried per the bounded
    /// attempt cap (not fired once): three 200 ms attempts with the computed
    /// backoff between them, then a classified `Network` failure. The server
    /// accepts exactly three connections and answers none, so the hit count
    /// proves the bound and the elapsed time proves the backoff.
    #[test]
    fn request_timeout_is_threaded_through_send() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
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
                // Answer nothing: every attempt must hit the per-request
                // timeout instead of completing.
            }
        });
        let executor = AnthropicExecutor::with_endpoint(format!("http://{addr}"), test_spawner());
        let mut request = sample_request();
        request.request_timeout = Some(Duration::from_millis(200));
        let start = std::time::Instant::now();
        let result = executor.execute(&request, "sk-secret-example", &CancellationToken::new());
        let elapsed = start.elapsed();
        server.join().expect("server thread joins");
        assert!(
            matches!(result, Err(ExecutorError::Network)),
            "expected timeout to surface as ExecutorError::Network, got {result:?}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "a 200 ms timeout must be retried per the 3-attempt bound"
        );
        // Two computed backoffs separate the three attempts: [1.5, 2.5] s +
        // [3, 5] s on top of the 3 × 200 ms timeouts.
        assert!(
            elapsed >= Duration::from_millis(4_500),
            "retried timeouts must wait the computed backoff, elapsed={elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "retried timeouts must stay bounded, elapsed={elapsed:?}"
        );
    }

    #[test]
    fn status_429_maps_to_rate_limited_with_retry_after() {
        // Bounded retry answers all three attempts; `Retry-After: 0` keeps the
        // test fast while exercising the header-carry path.
        let (endpoint, _count, server) = spawn_sequence_server(vec![
            (429, String::new(), Some("0".to_string())),
            (429, String::new(), Some("0".to_string())),
            (429, String::new(), Some("0".to_string())),
        ]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: Some(0)
            })
        ));
        let _ = server.join();

        // Case 2: 429 without the header -> None (all three attempts).
        let (endpoint, _count, server) = spawn_sequence_server(vec![
            (429, String::new(), None),
            (429, String::new(), None),
            (429, String::new(), None),
        ]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: None
            })
        ));
        server.join().expect("server thread joins");
    }

    #[test]
    fn status_5xx_maps_to_provider_unavailable() {
        for status in [500, 503] {
            // Bounded retry serves all three attempts with the same status.
            let (endpoint, _count, server) = spawn_sequence_server(vec![
                (status, String::new(), None),
                (status, String::new(), None),
                (status, String::new(), None),
            ]);
            let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
            let result = executor.execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            );
            assert!(
                matches!(result, Err(ExecutorError::ProviderUnavailable)),
                "status {status} should map to ProviderUnavailable, got {result:?}"
            );
            server.join().expect("server thread joins");
        }
    }

    #[test]
    fn other_client_errors_still_surface_as_failure() {
        let (endpoint, _captured, server) = spawn_server(422, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::Failure)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn status_404_maps_to_invalid_request() {
        // Unknown model/route surfaces as invalid request, matching the
        // OpenAI-compatible path.
        let (endpoint, _captured, server) = spawn_server(404, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        assert!(matches!(result, Err(ExecutorError::InvalidRequest)));
        server.join().expect("server thread joins");
    }

    /// Spawn a local HTTP server that reads the request headers, returns a
    /// response with the given `status`/`response_body`, and forwards the
    /// captured request bytes to the caller. Mirrors the OpenAI local-server
    /// test approach; no live provider is contacted.
    fn spawn_server(
        status: u16,
        response_body: &str,
    ) -> (
        String,
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let response_body = response_body.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
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
            let _ = tx.send(raw.clone());
            let response = format!(
                "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                reason(status),
                response_body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });
        (format!("http://{addr}"), rx, server)
    }

    /// Map an HTTP status code to a minimal reason phrase for the status line.
    fn reason(status: u16) -> &'static str {
        match status {
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            _ => "OK",
        }
    }
    #[test]
    fn usage_present_maps_to_token_usage() {
        let json = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":12,"output_tokens":34}}"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).expect("parse");
        let usage = parsed.usage.clone().expect("usage present");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 34);
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("ai usage");
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.output_tokens, 34);
    }

    #[test]
    fn usage_absent_maps_to_none() {
        let json = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"hi"}]}"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).expect("parse");
        assert!(parsed.usage.is_none());
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_zero_zero_maps_to_none() {
        let json = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":0,"output_tokens":0}}"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_partial_zero_maps_to_some() {
        let json = r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":0,"output_tokens":5}}"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("some");
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 5);
    }

    /// Spawn a local HTTP server that serves the scripted `responses` in order,
    /// one per accepted connection, and counts accepted connections.
    fn spawn_sequence_server(
        responses: Vec<(u16, String, Option<String>)>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count_clone = std::sync::Arc::clone(&count);
        let server = std::thread::spawn(move || {
            for (status, body, retry_after) in responses {
                let (mut stream, _) = listener.accept().expect("accept connection");
                count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut raw = Vec::new();
                let mut buf = [0u8; 1024];
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
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
                let retry_header = retry_after
                    .map(|value| format!("Retry-After: {value}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\n{retry_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    reason(status),
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), count, server)
    }

    #[test]
    fn retry_429_then_200_succeeds() {
        use std::sync::atomic::Ordering;
        let success =
            r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (429, String::new(), Some("0".to_string())),
            (200, success, None),
        ]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("429-then-200 must succeed");
        server.join().expect("server thread joins");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "claude-sonnet-5");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_503_then_200_succeeds() {
        use std::sync::atomic::Ordering;
        let success =
            r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (503, String::new(), Some("0".to_string())),
            (200, success, None),
        ]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let ai = executor
            .execute(
                &sample_request(),
                "sk-secret-example",
                &CancellationToken::new(),
            )
            .expect("503-then-200 must succeed");
        server.join().expect("server thread joins");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "claude-sonnet-5");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retry_400_is_never_retried() {
        use std::sync::atomic::Ordering;
        let (endpoint, count, server) = spawn_sequence_server(vec![(400, String::new(), None)]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(matches!(result, Err(ExecutorError::InvalidRequest)));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn status_400_with_context_body_maps_to_context_length_exceeded() {
        use std::sync::atomic::Ordering;
        // A 400 naming an over-long prompt is a compaction trigger, not a
        // malformed request — and, like every 400, it fires exactly once.
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200000 tokens > 100000 maximum"}}"#.to_string();
        let (endpoint, count, server) = spawn_sequence_server(vec![(400, body, None)]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(
            matches!(result, Err(ExecutorError::ContextLengthExceeded)),
            "context-length 400 must classify as ContextLengthExceeded, got {result:?}"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_attempts_are_capped_at_three() {
        use std::sync::atomic::Ordering;
        // `Retry-After: 0` keeps this a cap proof, not a timing proof: the
        // attempt bound and classification are what matter here, and a zero
        // hint retries immediately. Real-backoff timing is proven once in the
        // transport `retry_after_*` test instead of slept out per provider.
        let (endpoint, count, server) = spawn_sequence_server(vec![
            (503, String::new(), Some("0".to_string())),
            (503, String::new(), Some("0".to_string())),
            (503, String::new(), Some("0".to_string())),
        ]);
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let result = executor.execute(
            &sample_request(),
            "sk-secret-example",
            &CancellationToken::new(),
        );
        server.join().expect("server thread joins");
        assert!(matches!(result, Err(ExecutorError::ProviderUnavailable)));
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn execute_stream_events_emits_anthropic_text_thinking_and_tool_deltas() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured_srv = std::sync::Arc::clone(&captured);

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let mut header_end = None;
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if header_end.is_none() {
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                    }
                }
                if let Some(end) = header_end {
                    let hdrs = String::from_utf8_lossy(&raw[..end]);
                    let len = hdrs
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + len {
                        *captured_srv.lock().expect("lock") =
                            String::from_utf8_lossy(&raw[end..end + len]).to_string();
                        break;
                    }
                }
            }
            let sse = concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"step 1\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"list_dir\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\".\\\"}\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            stream.write_all(resp.as_bytes()).expect("write");
        });

        let endpoint = format!("http://{addr}/v1/messages");
        let executor = AnthropicExecutor::with_endpoint(endpoint, test_spawner());
        let mut req = sample_request();
        req.model_config =
            Some(ModelConfig::new("claude-sonnet-5").with_effort(Some("xhigh".to_string())));
        let events = std::sync::Mutex::new(Vec::new());
        let resp = executor
            .execute_stream_events(&req, "sk-secret", &CancellationToken::new(), &|ev| {
                events.lock().expect("lock").push(ev);
            })
            .expect("anthropic stream ok");
        server.join().expect("join");

        let sent: serde_json::Value =
            serde_json::from_str(&captured.lock().expect("lock")).expect("json");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["output_config"]["effort"], "max");

        assert_eq!(resp.content, "Done.");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_1");
        assert_eq!(resp.tool_calls[0].name, "list_dir");
        assert_eq!(resp.tool_calls[0].arguments, "{\"path\":\".\"}");
        assert_eq!(
            resp.usage,
            Some(TokenUsage {
                input_tokens: 12,
                output_tokens: 9,
                cached_tokens: 0
            })
        );
        let evs = events.into_inner().expect("lock");
        assert!(evs.contains(&StreamEvent::ThinkingDelta("step 1".to_string())));
        assert!(evs.contains(&StreamEvent::TextDelta("Done.".to_string())));
    }

    #[test]
    fn s11_09_anthropic_cache_control_serialized_on_system_and_last_tool() {
        let mut req = sample_request();
        req.tools = std::sync::Arc::new(vec![
            ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "edit_file".to_string(),
                description: "Edit a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ]);
        let body = anthropic_request(&req);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");

        assert_eq!(value["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(value["tools"][0].get("cache_control").is_none());
        assert_eq!(value["tools"][1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn s11_09_anthropic_parses_cached_tokens_non_streaming_and_streaming() {
        let json = r#"{
            "model": "claude-sonnet-5",
            "content": [{"type": "text", "text": "cached reply"}],
            "usage": {
                "input_tokens": 200,
                "cache_read_input_tokens": 1500,
                "cache_creation_input_tokens": 300,
                "output_tokens": 45
            }
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        assert_eq!(
            ai.usage,
            Some(TokenUsage {
                input_tokens: 2000,
                output_tokens: 45,
                cached_tokens: 1500,
            })
        );

        let mut acc = AnthropicStreamAccumulator::new("claude-sonnet-5");
        let start_ev: AnthropicSsePayload = serde_json::from_str(
            r#"{"type":"message_start","message":{"model":"claude-sonnet-5","usage":{"input_tokens":100,"cache_read_input_tokens":900,"output_tokens":1}}}"#,
        )
        .expect("start");
        let delta_text: AnthropicSsePayload = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        )
        .expect("delta");
        let end_ev: AnthropicSsePayload =
            serde_json::from_str(r#"{"type":"message_delta","usage":{"output_tokens":15}}"#)
                .expect("end");
        acc.apply_event(Some("message_start"), start_ev, &|_| {})
            .unwrap();
        acc.apply_event(Some("content_block_delta"), delta_text, &|_| {})
            .unwrap();
        acc.apply_event(Some("message_delta"), end_ev, &|_| {})
            .unwrap();
        let streamed = acc.finish().expect("finish");
        assert_eq!(
            streamed.usage,
            Some(TokenUsage {
                input_tokens: 1000,
                output_tokens: 15,
                cached_tokens: 900,
            })
        );
    }
}
