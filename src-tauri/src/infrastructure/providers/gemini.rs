//! Google Gemini provider integration: a concrete [`ProviderExecutor`]
//! (ROADMAP.md Phase 3 вЂ” AI Providers; ARCHITECTURE.md В§7).
//!
//! Translates the provider-independent [`AiRequest`] into a non-streaming
//! Google AI Studio `generateContent` request (`models.generateContent`,
//! `https://generativelanguage.googleapis.com/v1beta`), sends it over HTTPS
//! using the crate's HTTP facility (`reqwest`), and normalizes the response
//! into the provider-independent [`AiResponse`].
//!
//! Isolation: every Gemini-specific wire type lives in this module. Nothing
//! Gemini-specific is exposed through [`application::execution`]; the
//! boundary only ever sees [`AiRequest`]/[`AiResponse`]/[`ExecutorError`].
//!
//! # Gemini-specific contract notes
//!
//! - **Authentication:** Gemini accepts the API key in the `x-goog-api-key`
//!   header (Google AI Studio API key). The credential is placed only in that
//!   header via `reqwest`, never in the body or URL.
//! - **Endpoint shape:** `generateContent` embeds the model in the path
//!   (`{base}/models/{model}:generateContent`), so the caller-supplied model
//!   (FR-004 model selection) is never a body field вЂ” it selects both the
//!   endpoint and the model used for the request.
//! - **Roles:** Gemini contents use the roles `user` and `model` (not
//!   `assistant`). Instructions are routed through the top-level
//!   `systemInstruction` parameter rather than a `system` content entry, so
//!   [`AiRole::System`] is mapped to that field and never emitted as a
//!   `user`/`model` message. The response model is reported from the
//!   `modelVersion` field when present, falling back to the requested model.
//!
//! # Security (ARCHITECTURE.md В§9, В§11, В§12)
//!
//! - The credential is supplied by the caller from the [`CredentialStore`] for
//!   the duration of the call only; it is never persisted, logged, or returned.
//! - The credential is sent only in the `x-goog-api-key` header via `reqwest`,
//!   never in the body or URL.
//! - Failed responses are classified by HTTP status **without reading the error
//!   body**, so a provider diagnostic can never leak the credential or the
//!   request payload into an error or log.
//! - All failures collapse to the single provider-independent
//!   [`ExecutorError::Failure`]; the internal [`GeminiError`] classification
//!   (authentication, invalid request, provider/network, unexpected response) is
//!   recorded in the logs by category only.

// The module docs reference product/brand names (Gemini, Google AI Studio,
// generateContent) that the `doc_markdown` pedantic lint flags as needing
// backticks. Allow it locally.
#![allow(clippy::doc_markdown)]

use crate::application::execution::{
    AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, ExecutorError, ProviderExecutor,
};
// `AiAttachment` is referenced only by this module's unit tests.
#[cfg(test)]
use crate::application::execution::AiAttachment;

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Google AI Studio (Gemini API) base URL root, without the per-model
/// `generateContent` path (the model is appended per request).
const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = "gemini";

/// User-facing provider label.
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "Gemini";

/// Gemini models currently supported by the provider (DATABASE.md В§7.5:
/// model lists are hardcoded in the MVP and managed by the application layer).
///
/// The selected model is passed through unchanged and is never validated
/// against this list at runtime (never silently substituted, never rejected):
/// this set documents the currently supported models and anchors the
/// model-selection tests.
///
/// Pricing is governed by the policy table in
/// `crate::application::agent::pricing` (DATABASE.md В§7.8); the first entry
/// is the provider default consumed as `models[0]` by the selection surface.
/// The retired IDs this list replaces: `gemini-1.5-pro`, `gemini-1.5-flash`
/// (shut down), and `gemini-2.0-flash` (retired June 1, 2026).
pub(crate) const SUPPORTED_MODELS: &[&str] = &[
    // Default: GA, current-generation, balanced cost/quality.
    "gemini-3.6-flash",
    // Fast/cheap tier.
    "gemini-3.1-flash-lite",
    // Best-quality reasoning tier (still Preview status upstream).
    "gemini-3.1-pro-preview",
];

/// Concrete [`ProviderExecutor`] for Google Gemini.
///
/// Stateless over the shared `reqwest` blocking client so it can be shared
/// across requests; the per-request credential and request payload are passed
/// into each [`ProviderExecutor::execute`] call and dropped on return.
pub(crate) struct GeminiExecutor {
    client: reqwest::blocking::Client,
    endpoint: String,
}

impl GeminiExecutor {
    /// Create an executor targeting the Google AI Studio production endpoint.
    pub(crate) fn new() -> Self {
        Self::with_endpoint(ENDPOINT.to_string())
    }

    /// Create an executor targeting an explicit `endpoint` base (used by tests
    /// to exercise the full request/response path without a live Google
    /// service; the per-model `generateContent` path is appended by `send`).
    fn with_endpoint(endpoint: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            endpoint,
        }
    }

    /// Translate, send, and normalize one request.
    ///
    /// Returns a provider-independent [`AiResponse`] on success, or a
    /// classified [`GeminiError`] describing the failure category.
    fn run(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, GeminiError> {
        let body = generate_content_request(request);
        let response = send(
            &self.client,
            &self.endpoint,
            &request.model,
            credential,
            &body,
            request.request_timeout,
        )?;
        to_ai_response(response, &request.model)
    }
}

impl ProviderExecutor for GeminiExecutor {
    fn execute(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, ExecutorError> {
        match self.run(request, credential) {
            Ok(response) => Ok(response),
            Err(error) => {
                // Record only the classification category; never the credential
                // or request payload (ARCHITECTURE.md В§9, В§11).
                log::warn!("gemini request failed: {error}");
                Err(ExecutorError::Failure)
            }
        }
    }
}

/// Gemini `generateContent` request body, mapped from the provider-independent
/// [`AiRequest`].
///
/// `systemInstruction` is a top-level Gemini-specific parameter and is omitted
/// (via `skip_serializing_if`) when no system message is present, so the wire
/// shape matches Gemini's contract in both cases.
///
/// `tools` carries Gemini-native function declarations only when
/// `AiRequest.tools` is non-empty; an empty list is omitted entirely to keep
/// the wire payload byte-compatible with plain chat.
#[derive(Debug, Serialize)]
struct GenerateContentRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<GeminiTool>,
}

/// Gemini-native tool container (`tools` array element).
///
/// The `generateContent` REST contract nests function declarations under
/// `tools[].functionDeclarations[]`; this struct models that single-element
/// container. All `AiRequest.tools` are emitted as one `functionDeclarations`
/// array to preserve declaration order without inventing multiple tool entries.
#[derive(Debug, Serialize)]
struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

/// Gemini-native function declaration.
///
/// Mirrors the `generateContent` declaration contract:
/// `{ "name", "description", "parameters": <JSON Schema> }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// One Gemini `contents` entry. Only `user` and `model` roles are valid inside
/// `contents`; instructions are routed to the top-level `systemInstruction`
/// field instead.
#[derive(Debug, Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

/// One Gemini content part: either a text segment or an inline base64 data
/// part (images / PDFs, FR-008). Exactly one field is set per part.
#[derive(Debug, Serialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Inline base64 payload (serialized as `inlineData`), mirroring the
    /// generateContent REST contract.
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    inline_data: Option<GeminiInlineData>,
}

/// Inline base64 data of one Gemini part (FR-008).
#[derive(Debug, Serialize)]
struct GeminiInlineData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    data: String,
}

/// Top-level Gemini system instruction (mapped from [`AiRole::System`]).
#[derive(Debug, Serialize)]
struct SystemInstruction {
    parts: Vec<GeminiPart>,
}

/// Translate a provider-independent request into a Gemini `generateContent`
/// request.
///
/// The selected model is passed through unchanged вЂ” it is never silently
/// substituted (FR-004) вЂ” and is embedded in the endpoint path by `send`.
/// [`AiRole::System`] messages are aggregated into the top-level
/// `systemInstruction` parameter; `user` and `assistant` messages map to the
/// corresponding Gemini `contents.role` (`user`/`model`) and remain in
/// chronological order.
fn generate_content_request(request: &AiRequest) -> GenerateContentRequest {
    let system = collect_system(&request.messages);
    let contents: Vec<GeminiContent> = request
        .messages
        .iter()
        .filter(|message| message.role != AiRole::System)
        .map(|message| GeminiContent {
            role: match message.role {
                AiRole::User => "user",
                // System is filtered out above and mapped to the top-level
                // `systemInstruction` field by `collect_system`; this arm is
                // unreachable in practice but keeps the match exhaustive.
                AiRole::Assistant => "model",
                AiRole::System => "system",
            }
            .to_string(),
            // Attachment payloads are rendered per the Gemini contract: inline
            // text file contents become a text part; base64 images and PDFs
            // become `inlineData` parts (FR-008).
            parts: gemini_parts(message),
        })
        .collect();
    let tools = if request.tools.is_empty() {
        Vec::new()
    } else {
        vec![GeminiTool {
            function_declarations: request
                .tools
                .iter()
                .map(|tool| GeminiFunctionDeclaration {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    // Reduce the full JSON Schema to the OpenAPI subset Gemini
                    // accepts; the verbatim schema stays the shared source of
                    // truth for the other providers.
                    parameters: to_gemini_schema(&tool.parameters),
                })
                .collect(),
        }]
    };
    GenerateContentRequest {
        contents,
        system_instruction: system.map(|text| SystemInstruction {
            parts: vec![GeminiPart {
                text: Some(text),
                inline_data: None,
            }],
        }),
        tools,
    }
}

/// Reduce a provider-independent JSON Schema to the subset of OpenAPI 3.0 that
/// Gemini's `functionDeclarations[].parameters` accepts.
/// Unknown keys are DROPPED, never renamed and never rejected: the wire type is
/// parsed as proto3 JSON, where any unknown field name fails the whole request.
fn to_gemini_schema(schema: &serde_json::Value) -> serde_json::Value {
    /// Keys Gemini's function-declaration `Schema` accepts (OpenAPI 3.0
    /// subset); everything else is dropped before it can reach the wire.
    const ALLOWED: [&str; 11] = [
        "type",
        "format",
        "description",
        "nullable",
        "enum",
        "properties",
        "items",
        "anyOf",
        "maxItems",
        "required",
        "propertyOrdering",
    ];

    // A non-object (null / array / scalar) is never a valid Schema object;
    // an empty Schema always is.
    let Some(object) = schema.as_object() else {
        return serde_json::json!({});
    };

    let mut reduced = serde_json::Map::new();
    for (key, value) in object {
        match key.as_str() {
            // `properties` is a map of name -> Schema; reduce every value.
            "properties" => {
                if let Some(properties) = value.as_object() {
                    let mapped: serde_json::Map<String, serde_json::Value> = properties
                        .iter()
                        .map(|(name, property)| (name.clone(), to_gemini_schema(property)))
                        .collect();
                    reduced.insert("properties".to_string(), serde_json::Value::Object(mapped));
                }
            }
            // `items` is a single Schema; a non-object form is dropped.
            "items" => {
                if value.is_object() {
                    reduced.insert("items".to_string(), to_gemini_schema(value));
                }
            }
            // `anyOf` is an array of Schemas; a non-array form is dropped.
            "anyOf" => {
                if let Some(alternatives) = value.as_array() {
                    reduced.insert(
                        "anyOf".to_string(),
                        serde_json::Value::Array(
                            alternatives.iter().map(to_gemini_schema).collect(),
                        ),
                    );
                }
            }
            // `required` survives only as a non-empty array of strings.
            "required" => {
                if let Some(required) = value.as_array() {
                    let all_strings = required.iter().all(serde_json::Value::is_string);
                    if !required.is_empty() && all_strings {
                        reduced.insert("required".to_string(), value.clone());
                    }
                }
            }
            // Plain allow-listed keys pass through verbatim.
            key if ALLOWED.contains(&key) => {
                reduced.insert(key.to_string(), value.clone());
            }
            // Unknown keys (additionalProperties, minimum, maximum, const,
            // propertyNames, $schema, $defs, ...) are dropped silently.
            _ => {}
        }
    }
    serde_json::Value::Object(reduced)
}

/// Build the Gemini parts for one message (FR-008).
///
/// One text part carries the turn content with inline text-file contents;
/// each base64 attachment additionally becomes an `inlineData` part.
fn gemini_parts(message: &AiMessage) -> Vec<GeminiPart> {
    let mut parts = vec![GeminiPart {
        text: Some(message.composed_content()),
        inline_data: None,
    }];
    for attachment in &message.attachments {
        let AiAttachmentPayload::Base64(data) = &attachment.payload else {
            continue;
        };
        parts.push(GeminiPart {
            text: None,
            inline_data: Some(GeminiInlineData {
                mime_type: attachment
                    .mime_type
                    .as_deref()
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                data: data.clone(),
            }),
        });
    }
    parts
}

/// Aggregate every [`AiRole::System`] message into the Gemini top-level
/// `systemInstruction` parameter.
///
/// Gemini expects a single `systemInstruction` value; when the
/// provider-independent request carries multiple system turns they are joined
/// with a blank line. Returns [`None`] (and the parameter is omitted from the
/// wire payload) when there are no system messages.
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

/// A normalized Gemini `generateContent` response.
///
/// `modelVersion` is optional: it reports the model that produced the response
/// when the service includes it, and falls back to the requested model
/// otherwise (see [`to_ai_response`]).
#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    #[serde(rename = "modelVersion")]
    model_version: Option<String>,
    candidates: Vec<Candidate>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u64,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u64,
}

/// One generation candidate from a Gemini response.
#[derive(Debug, Deserialize)]
struct Candidate {
    content: CandidateContent,
}

/// The `content` of a Gemini response candidate.
#[derive(Debug, Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

/// One response part. Text completions produce `text` parts; non-text parts
/// (for example `inlineData`) carry no text and are skipped. Tool invocations
/// produce `functionCall` parts (`{functionCall:{name, args}}`).
#[derive(Debug, Deserialize)]
struct ResponsePart {
    text: Option<String>,
    #[serde(rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
}

/// Gemini-native function call inside a response part.
#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: Option<serde_json::Value>,
}

/// Normalize a successful Gemini response into the provider-independent
/// [`AiResponse`].
///
/// Text parts are concatenated in wire order to produce `AiResponse.content`;
/// `functionCall` parts map in order to `AiResponse.tool_calls` with
/// `arguments` as a JSON string of `args`. Gemini does not assign a per-call
/// id, so this uses a deterministic index-based id `call_{n}` (`n` = zero-based
/// position among function calls in the response). A tool-only response yields
/// `content == ""`; a text-only response yields an empty `tool_calls` vector.
/// If the response contains neither extractable text nor a `functionCall`
/// (no candidates, no `parts`, no `text`/`functionCall`), this returns
/// [`GeminiError::UnexpectedResponse`] rather than inventing a partial response
/// shape.
///
/// The model reported in the response is the service's `modelVersion` when
/// present; `requested_model` (the model selected for the request, FR-004) is
/// used when the service omits it.
///
/// # Errors
///
/// Returns [`GeminiError::UnexpectedResponse`] when no assistant text nor
/// tool call can be extracted from the response.
fn to_ai_response(
    response: GenerateContentResponse,
    requested_model: &str,
) -> Result<AiResponse, GeminiError> {
    let candidate = response
        .candidates
        .into_iter()
        .next()
        .ok_or(GeminiError::UnexpectedResponse)?;
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<crate::application::execution::ToolCall> = Vec::new();
    for part in candidate.content.parts {
        if let Some(text) = part.text {
            text_parts.push(text);
        }
        if let Some(call) = part.function_call {
            let index = tool_calls.len();
            let id = format!("call_{index}");
            let args_value = call
                .args
                .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
            let arguments =
                serde_json::to_string(&args_value).map_err(|_| GeminiError::UnexpectedResponse)?;
            tool_calls.push(crate::application::execution::ToolCall {
                id,
                name: call.name,
                arguments,
            });
        }
    }
    if text_parts.is_empty() && tool_calls.is_empty() {
        return Err(GeminiError::UnexpectedResponse);
    }
    let content = if text_parts.is_empty() {
        String::new()
    } else {
        text_parts.join("")
    };
    let usage = response.usage_metadata.and_then(|u| {
        if u.prompt_token_count == 0 && u.candidates_token_count == 0 {
            None
        } else {
            Some(crate::application::execution::TokenUsage {
                input_tokens: u.prompt_token_count,
                output_tokens: u.candidates_token_count,
            })
        }
    });
    Ok(AiResponse {
        content,
        model: response
            .model_version
            .unwrap_or_else(|| requested_model.to_string()),
        tool_calls,
        usage,
    })
}

/// Build the `generateContent` endpoint URL for `model` under `endpoint`.
///
/// The Gemini API embeds the model in the path:
/// `{base}/models/{model}:generateContent`.
fn generate_content_url(endpoint: &str, model: &str) -> String {
    format!("{endpoint}/models/{model}:generateContent")
}

/// Perform the non-streaming HTTPS request.
///
/// The credential is placed only in the `x-goog-api-key` header (the Google AI
/// Studio API key authentication), never in the body or URL. A non-success
/// response is classified by status without reading its body, so provider
/// diagnostics can never leak the credential or request payload.
///
/// `request_timeout` bounds the single blocking round trip (Task 3.2): the
/// blocking client cannot be interrupted mid-flight, so the honest bound is a
/// wall-clock timeout applied via `RequestBuilder::timeout`. `None` preserves
/// the historical unbounded behavior byte-for-byte.
fn send(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    model: &str,
    credential: &str,
    body: &GenerateContentRequest,
    request_timeout: Option<Duration>,
) -> Result<GenerateContentResponse, GeminiError> {
    let mut builder = client
        .post(generate_content_url(endpoint, model))
        .header("x-goog-api-key", credential)
        .json(body);
    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }
    let response = builder.send().map_err(|_| GeminiError::Network)?;

    let status = response.status();
    if status.is_success() {
        response
            .json::<GenerateContentResponse>()
            .map_err(|_| GeminiError::UnexpectedResponse)
    } else {
        Err(classify_status(status.as_u16()))
    }
}

/// Classify a non-success HTTP status into a secret-free failure category.
///
/// Google reports credential problems as **401 UNAUTHENTICATED** (missing or
/// malformed key) and **403 PERMISSION_DENIED** (the key lacks access: wrong
/// key, restricted/unrestricted-key policy rejection, or the API is disabled
/// for its project), so both map to [`GeminiError::Authentication`]. Note that
/// Google also returns **400 INVALID_ARGUMENT for an invalid API key**
/// (`reason: "API_KEY_INVALID"`); a 400 therefore cannot be distinguished from
/// a genuinely malformed request without reading the error body, which this
/// integration deliberately avoids (secret hygiene), so it stays classified as
/// [`GeminiError::InvalidRequest`] with a message that names both likely causes.
fn classify_status(status: u16) -> GeminiError {
    match status {
        400 => GeminiError::InvalidRequest,
        // Credential/access problems (see above): actionable for the user.
        401 | 403 => GeminiError::Authentication,
        _ => GeminiError::Provider,
    }
}

/// Classified Gemini failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] вЂ” this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug)]
enum GeminiError {
    /// The Gemini endpoint rejected the request as malformed (HTTP 400).
    InvalidRequest,
    /// The credential was rejected (HTTP 401).
    Authentication,
    /// A network/transport failure (connection refused, DNS, timeout, ...).
    Network,
    /// A provider-side error (HTTP 4xx other than 400/401, 5xx, 429, ...).
    Provider,
    /// The response was not a recognizable generation (e.g. missing text
    /// content or malformed JSON).
    UnexpectedResponse,
}

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(
                f,
                "the Gemini request was rejected as invalid (400); check the selected \
                 model and that the stored Gemini API key is valid and not restricted \
                 (Google reports invalid keys as 400)"
            ),
            Self::Authentication => write!(
                f,
                "Gemini rejected the stored credential or its access (401/403)"
            ),
            Self::Network => write!(f, "Gemini network or transport failure"),
            Self::Provider => write!(f, "Gemini provider failure"),
            Self::UnexpectedResponse => write!(f, "Gemini returned an unexpected response"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::System,
                    content: "You are a helpful assistant.".to_string(),
                    attachments: Vec::new(),
                },
                AiMessage {
                    role: AiRole::User,
                    content: "Hello".to_string(),
                    attachments: Vec::new(),
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: "Hi there".to_string(),
                    attachments: Vec::new(),
                },
            ],
            tools: Vec::new(),
            request_timeout: None,
        }
    }

    #[test]
    fn supported_models_include_sample_model() {
        let request = sample_request();
        // The model selected in tests is one of the currently supported
        // Gemini models (DATABASE.md В§7.5: hardcoded model lists).
        assert!(SUPPORTED_MODELS.contains(&request.model.as_str()));
    }

    #[test]
    fn system_message_is_mapped_to_system_instruction() {
        let body = generate_content_request(&sample_request());
        // The system instruction is extracted into the top-level
        // `systemInstruction` parameter, not emitted as a content entry.
        let instruction = body
            .system_instruction
            .as_ref()
            .expect("a system instruction is present");
        assert_eq!(
            instruction.parts[0].text.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert!(body.contents.iter().all(|c| c.role != "system"));
    }

    #[test]
    fn multiple_system_messages_are_joined() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::System,
                    content: "First rule.".to_string(),
                    attachments: Vec::new(),
                },
                AiMessage {
                    role: AiRole::System,
                    content: "Second rule.".to_string(),
                    attachments: Vec::new(),
                },
            ],
            tools: Vec::new(),
            request_timeout: None,
        };
        let body = generate_content_request(&request);
        assert_eq!(
            body.system_instruction.expect("system present").parts[0]
                .text
                .as_deref(),
            Some("First rule.\n\nSecond rule.")
        );
    }

    #[test]
    fn contents_hold_no_system_role_and_only_user_and_model() {
        let body = generate_content_request(&sample_request());
        let roles: Vec<&str> = body.contents.iter().map(|c| c.role.as_str()).collect();
        // User -> user, assistant -> model, chronological order preserved.
        assert_eq!(roles, vec!["user", "model"]);
        assert_eq!(body.contents[0].parts[0].text.as_deref(), Some("Hello"));
        assert_eq!(body.contents[1].parts[0].text.as_deref(), Some("Hi there"));
    }

    #[test]
    fn system_instruction_is_omitted_when_absent() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
            }],
            tools: Vec::new(),
            request_timeout: None,
        };
        let body = generate_content_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        assert!(value.get("systemInstruction").is_none());
        assert!(body.system_instruction.is_none());
    }

    #[test]
    fn request_serialization_matches_gemini_contract() {
        let json =
            serde_json::to_string(&generate_content_request(&sample_request())).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");

        // System content is a top-level systemInstruction, not a content entry,
        // and contents carry user/model roles with text parts.
        assert_eq!(
            value["systemInstruction"]["parts"][0]["text"],
            "You are a helpful assistant."
        );
        let roles: Vec<&str> = value["contents"]
            .as_array()
            .expect("contents is an array")
            .iter()
            .map(|c| c["role"].as_str().expect("role is a string"))
            .collect();
        assert_eq!(roles, vec!["user", "model"]);
        assert_eq!(value["contents"][0]["parts"][0]["text"], "Hello");
        assert_eq!(value["contents"][1]["parts"][0]["text"], "Hi there");

        // The Gemini wire contract has no "assistant" or "system" content role.
        assert!(!json.contains("\"assistant\""));
        assert!(!json.contains("\"system\""));
    }

    #[test]
    fn endpoint_embeds_the_selected_model() {
        // The model selection (FR-004) drives the generateContent path.
        assert_eq!(
            generate_content_url("http://127.0.0.1:1", "gemini-3.6-flash"),
            "http://127.0.0.1:1/models/gemini-3.6-flash:generateContent"
        );
    }

    #[test]
    fn successful_response_is_normalized() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: Some("pong".to_string()),
                        function_call: None,
                    }],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("valid response maps");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gemini-3.6-flash");
    }

    #[test]
    fn model_version_falls_back_to_requested_model() {
        let response = GenerateContentResponse {
            model_version: None,
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: Some("pong".to_string()),
                        function_call: None,
                    }],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("valid response maps");
        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gemini-3.6-flash");
    }

    #[test]
    fn response_without_candidates_is_unexpected() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: Vec::new(),
            usage_metadata: None,
        };
        assert!(matches!(
            to_ai_response(response, "gemini-3.6-flash"),
            Err(GeminiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn candidate_without_text_part_is_unexpected() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: None,
                        function_call: None,
                    }],
                },
            }],
            usage_metadata: None,
        };
        assert!(matches!(
            to_ai_response(response, "gemini-3.6-flash"),
            Err(GeminiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(classify_status(400), GeminiError::InvalidRequest));
        // Credential problems: 401 UNAUTHENTICATED and 403 PERMISSION_DENIED.
        assert!(matches!(classify_status(401), GeminiError::Authentication));
        assert!(matches!(classify_status(403), GeminiError::Authentication));
        for status in [404, 429, 500, 502, 503, 504] {
            assert!(
                matches!(classify_status(status), GeminiError::Provider),
                "status {status} should classify as provider failure"
            );
        }
    }

    #[test]
    fn run_classifies_permission_denied_failure() {
        // Google answers a key without access (wrong key, restricted-key policy,
        // disabled API) with 403 PERMISSION_DENIED; it must surface as an
        // authentication problem, not a generic provider failure.
        let (endpoint, _captured, server) = spawn_server(403, "");
        let executor = GeminiExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(GeminiError::Authentication)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn executor_maps_failure_to_boundary_failure() {
        let executor = GeminiExecutor {
            client: reqwest::blocking::Client::new(),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
        };
        // The boundary must only ever surface ExecutorError::Failure, never a
        // Gemini-specific or secret-bearing type.
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(ExecutorError::Failure)));
    }

    #[test]
    fn credential_never_appears_in_returned_error() {
        let executor = GeminiExecutor {
            client: reqwest::blocking::Client::new(),
            endpoint: "http://127.0.0.1:1".to_string(),
        };
        let credential = "sk-secret-example";
        let err = executor
            .execute(&sample_request(), credential)
            .expect_err("unreachable endpoint must fail");
        // The collapsed boundary error carries a fixed, secret-free message.
        assert_eq!(
            format!("{err}"),
            "the AI provider failed to fulfil the request"
        );
        assert!(!format!("{err}").contains(credential));
    }

    #[test]
    fn run_classifies_network_failure() {
        let executor = GeminiExecutor::with_endpoint("http://127.0.0.1:1".to_string());
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(GeminiError::Network)));
    }

    #[test]
    fn run_classifies_authentication_failure() {
        let (endpoint, _captured, server) = spawn_server(401, "");
        let executor = GeminiExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(GeminiError::Authentication)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_invalid_request_failure() {
        let (endpoint, _captured, server) = spawn_server(400, "");
        let executor = GeminiExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(GeminiError::InvalidRequest)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn credential_is_sent_in_header_not_body_or_url_and_model_is_selected() {
        let success_body = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"pong"}]}}]}"#;
        let (endpoint, captured, server) = spawn_server(200, success_body);
        let executor = GeminiExecutor::with_endpoint(endpoint);
        executor
            .run(&sample_request(), "sk-secret-example")
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        let request_text = String::from_utf8(
            captured
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("server captured the request"),
        )
        .expect("utf8 request");

        // The credential is sent only as the x-goog-api-key header, never as a
        // bearer token or as a `key=` query parameter on the endpoint URL.
        assert!(
            request_text.contains("x-goog-api-key: sk-secret-example"),
            "x-goog-api-key header must carry the credential: {request_text}"
        );
        assert!(
            !request_text.contains("authorization: bearer"),
            "must not use bearer auth: {request_text}"
        );
        let request_line = request_text.lines().next().unwrap_or_default();
        assert!(
            !request_line.contains("key="),
            "the credential must not appear in the URL: {request_line}"
        );

        // The selected model is embedded in the generateContent endpoint path
        // (FR-004 model selection passes through unchanged).
        assert!(
            request_line.contains("POST /models/gemini-3.6-flash:generateContent"),
            "request line must embed the selected model: {request_line}"
        );

        // The wire body carries no credential value.
        let body_json =
            serde_json::to_string(&generate_content_request(&sample_request())).expect("serialize");
        assert!(
            !body_json.contains("sk-secret-example"),
            "the body must not contain the credential"
        );
    }

    #[test]
    fn executor_round_trips_through_local_server() {
        let success_body = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"pong"}]}}]}"#;
        let (endpoint, _captured, server) = spawn_server(200, success_body);
        let executor = GeminiExecutor::with_endpoint(endpoint);
        let ai = executor
            .execute(&sample_request(), "sk-secret-example")
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gemini-3.6-flash");
    }

    #[test]
    fn binary_attachments_serialize_as_gemini_inline_data_parts() {
        let mut request = sample_request();
        request.messages.push(AiMessage {
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
        });
        let json = serde_json::to_string(&generate_content_request(&request)).expect("serialize");

        // A text part plus one `inlineData` part per binary attachment,
        // per the generateContent REST contract (camelCase field names).
        assert!(json.contains("\"inlineData\":{"));
        assert!(json.contains("\"mimeType\":\"image/png\""));
        assert!(json.contains("\"mimeType\":\"application/pdf\""));
        assert!(json.contains("cG5nIQ=="));
        // No filesystem path can appear: the boundary never carries one.
        assert!(!json.contains("/tmp/"));
    }

    #[test]
    fn text_attachments_are_inlined_into_the_turn_text() {
        let mut request = sample_request();
        request.messages.push(AiMessage {
            role: AiRole::User,
            content: "Summarize".to_string(),
            attachments: vec![AiAttachment {
                file_name: "notes.txt".to_string(),
                file_size_bytes: Some(5),
                mime_type: Some("text/plain".to_string()),
                payload: AiAttachmentPayload::Text("revenue rose 12 percent".to_string()),
            }],
        });
        let body = generate_content_request(&request);
        let user = body
            .contents
            .iter()
            .rev()
            .find(|c| c.role == "user")
            .expect("user contents");
        assert_eq!(user.parts.len(), 1);
        assert_eq!(
            user.parts[0].text.as_deref(),
            Some("Summarize\n\n[Attached file: notes.txt]\n--- begin attached file contents ---\nrevenue rose 12 percent\n--- end attached file contents ---")
        );
    }

    #[test]
    fn request_with_tools_serializes_with_native_shape() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Use the tool".to_string(),
                attachments: Vec::new(),
            }],
            tools: vec![crate::application::execution::ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather for a location".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            }],
            request_timeout: None,
        };
        let json = serde_json::to_string(&generate_content_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let tools = value
            .get("tools")
            .expect("tools present")
            .as_array()
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        let decls = tools[0]
            .get("functionDeclarations")
            .expect("functionDeclarations present")
            .as_array()
            .expect("functionDeclarations array");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["description"], "Get the weather for a location");
        assert!(decls[0]["parameters"]["properties"]["location"].is_object());
    }

    #[test]
    fn request_without_tools_omits_tools_key() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
            }],
            tools: Vec::new(),
            request_timeout: None,
        };
        let json = serde_json::to_string(&generate_content_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            value.get("tools").is_none(),
            "tools key must be absent when empty"
        );
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn response_with_function_call_maps_to_ai_response() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name: "get_weather".to_string(),
                            args: Some(serde_json::json!({"location":"Paris"})),
                        }),
                    }],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("functionCall maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.model, "gemini-3.6-flash");
        assert_eq!(ai.tool_calls.len(), 1);
        // Deterministic index-based id (no random UUIDs).
        assert_eq!(ai.tool_calls[0].id, "call_0");
        assert_eq!(ai.tool_calls[0].name, "get_weather");
        assert_eq!(ai.tool_calls[0].arguments, "{\"location\":\"Paris\"}");
    }

    #[test]
    fn response_with_content_and_function_call_maps_both() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![
                        ResponsePart {
                            text: Some("I will call the tool ".to_string()),
                            function_call: None,
                        },
                        ResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                name: "search".to_string(),
                                args: Some(serde_json::json!({"query":"test"})),
                            }),
                        },
                    ],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("text + functionCall maps");
        assert_eq!(ai.content, "I will call the tool ");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].name, "search");
        assert_eq!(ai.tool_calls[0].arguments, "{\"query\":\"test\"}");
    }

    #[test]
    fn plain_text_response_without_tools_still_maps_correctly() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: Some("Hello to you too.".to_string()),
                        function_call: None,
                    }],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("plain text maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gemini-3.6-flash");
        assert!(ai.tool_calls.is_empty());
    }

    #[test]
    fn tool_only_response_yields_empty_content() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![
                        ResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                name: "get_weather".to_string(),
                                args: Some(serde_json::json!({"location":"Paris"})),
                            }),
                        },
                        ResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                name: "get_time".to_string(),
                                args: Some(serde_json::json!({})),
                            }),
                        },
                    ],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("tool-only maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.tool_calls.len(), 2);
        // Deterministic ids in wire order.
        assert_eq!(ai.tool_calls[0].id, "call_0");
        assert_eq!(ai.tool_calls[1].id, "call_1");
        assert_eq!(ai.tool_calls[0].name, "get_weather");
        assert_eq!(ai.tool_calls[1].name, "get_time");
    }

    #[test]
    fn multiple_text_parts_are_concatenated() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![
                        ResponsePart {
                            text: Some("Hello ".to_string()),
                            function_call: None,
                        },
                        ResponsePart {
                            text: Some("world".to_string()),
                            function_call: None,
                        },
                    ],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("concatenated text maps");
        assert_eq!(ai.content, "Hello world");
        assert!(ai.tool_calls.is_empty());
    }

    #[test]
    fn request_timeout_is_threaded_through_send() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");
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
            std::thread::sleep(Duration::from_secs(2));
            let body = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"pong"}]}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = GeminiExecutor::with_endpoint(format!("http://{addr}"));
        let mut request = sample_request();
        request.request_timeout = Some(Duration::from_millis(200));
        let start = std::time::Instant::now();
        let result = executor.execute(&request, "sk-secret-example");
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(ExecutorError::Failure)),
            "expected timeout to surface as ExecutorError::Failure, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout should fire quickly, elapsed={elapsed:?}"
        );
        let _ = server.join();
    }

    #[test]
    fn function_call_without_args_maps_to_empty_object_string() {
        let response = GenerateContentResponse {
            model_version: Some("gemini-3.6-flash".to_string()),
            candidates: vec![Candidate {
                content: CandidateContent {
                    parts: vec![ResponsePart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name: "no_args_tool".to_string(),
                            args: None,
                        }),
                    }],
                },
            }],
            usage_metadata: None,
        };
        let ai = to_ai_response(response, "gemini-3.6-flash").expect("no-args maps");
        assert_eq!(ai.content, "");
        assert_eq!(ai.tool_calls[0].arguments, "{}");
    }

    /// Spawn a local HTTP server that reads the request headers, returns a
    /// response with the given `status`/`response_body`, and forwards the
    /// captured request bytes to the caller. Mirrors the OpenAI/Anthropic
    /// local-server test approach; no live provider is contacted.
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
        let json = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20}}"#;
        let parsed: GenerateContentResponse = serde_json::from_str(json).expect("parse");
        let usage = parsed.usage_metadata.clone().expect("usage present");
        assert_eq!(usage.prompt_token_count, 10);
        assert_eq!(usage.candidates_token_count, 20);
        let ai = to_ai_response(parsed, "gemini-3.6-flash").expect("to_ai");
        let u = ai.usage.expect("ai usage");
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn usage_absent_maps_to_none() {
        let json = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#;
        let parsed: GenerateContentResponse = serde_json::from_str(json).expect("parse");
        assert!(parsed.usage_metadata.is_none());
        let ai = to_ai_response(parsed, "gemini-3.6-flash").expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_zero_zero_maps_to_none() {
        let json = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":0,"candidatesTokenCount":0}}"#;
        let parsed: GenerateContentResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed, "gemini-3.6-flash").expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_partial_zero_maps_to_some() {
        let json = r#"{"modelVersion":"gemini-3.6-flash","candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"promptTokenCount":0,"candidatesTokenCount":5}}"#;
        let parsed: GenerateContentResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed, "gemini-3.6-flash").expect("to_ai");
        let u = ai.usage.expect("some");
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 5);
    }

    // ---- Gemini tool-schema reduction (v1.0.1) ---------------------------
    //
    // `ToolRegistry::definitions()` (src/application/agent/tools.rs:100, 119,
    // 144, 163) emits full JSON Schema objects (`additionalProperties`,
    // `minimum`), but Gemini parses `tools[].functionDeclarations[].parameters`
    // as proto3 JSON and rejects any unknown field name with 400
    // INVALID_ARGUMENT. `to_gemini_schema` is the Gemini-only wire adapter.

    /// True when `key` appears anywhere inside `value` (objects or arrays).
    fn contains_key_recursive(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key)
                    || map.values().any(|inner| contains_key_recursive(inner, key))
            }
            serde_json::Value::Array(items) => {
                items.iter().any(|inner| contains_key_recursive(inner, key))
            }
            _ => false,
        }
    }

    /// An [`AiRequest`] carrying the four real workspace tool definitions.
    fn tool_request(tools: Vec<crate::application::execution::ToolDefinition>) -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gemini-3.6-flash".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Run the tests".to_string(),
                attachments: Vec::new(),
            }],
            tools,
            request_timeout: None,
        }
    }

    /// Test (a): the four real tool definitions, serialized through
    /// `generate_content_request`, contain none of the JSON-Schema keys Gemini
    /// rejects; read_file's parameters equal the exact reduced shape; the
    /// empty `required` of list_directory is dropped entirely.
    #[test]
    fn agent_tool_declarations_are_reduced_to_the_gemini_subset() {
        let request = tool_request(crate::application::agent::tools::ToolRegistry::definitions());
        let body = generate_content_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        let declarations = value["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("four function declarations");
        assert_eq!(declarations.len(), 4, "all four tools are declared");

        for declaration in declarations {
            let parameters = &declaration["parameters"];
            for forbidden in [
                "additionalProperties",
                "minimum",
                "maximum",
                "const",
                "propertyNames",
                "$schema",
                "$defs",
            ] {
                assert!(
                    !contains_key_recursive(parameters, forbidden),
                    "declaration '{}' leaked forbidden key '{forbidden}': {parameters}",
                    declaration["name"]
                );
            }
        }

        // read_file: type / description / properties / non-empty required
        // survive unchanged; minimum and additionalProperties are gone.
        let read_file = &declarations[1];
        assert_eq!(read_file["name"], "read_file");
        let expected = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file relative to workspace root"
                },
                "offset_lines": {
                    "type": "integer",
                    "description": "Line offset to start reading from (0-indexed)"
                },
                "limit_lines": {
                    "type": "integer",
                    "description": "Maximum number of lines to return"
                }
            },
            "required": ["path"]
        });
        assert_eq!(
            read_file["parameters"], expected,
            "read_file parameters must equal the exact reduced shape"
        );

        // list_directory: `required: []` is dropped entirely (tools.rs:175).
        let list_directory = &declarations[3];
        assert_eq!(list_directory["name"], "list_directory");
        assert!(
            list_directory["parameters"].get("required").is_none(),
            "list_directory must not carry a 'required' key"
        );

        // Non-empty required arrays of the other tools survive verbatim.
        assert_eq!(
            declarations[0]["parameters"]["required"],
            serde_json::json!(["command"]),
            "execute_command keeps its required array"
        );
        assert_eq!(
            declarations[2]["parameters"]["required"],
            serde_json::json!(["path", "content"]),
            "write_file keeps its required array"
        );
    }

    /// Test (b): the reduction is a Gemini wire adapter only. The shared
    /// `AiRequest.tools` payload — the exact value the OpenAI
    /// (`chat_completion_request`, openai.rs:190) and Anthropic
    /// (`anthropic_request`, anthropic.rs:235) body builders serialize
    /// verbatim into their `tools` arrays — keeps the full JSON Schema, and
    /// `generate_content_request` never mutates it.
    #[test]
    fn other_providers_keep_the_schema_verbatim() {
        let definitions = crate::application::agent::tools::ToolRegistry::definitions();
        let shared = definitions.clone();

        // The shared source of truth still carries the keys Gemini rejects;
        // OpenAI and Anthropic serialize exactly this value.
        for definition in &shared {
            assert!(
                definition.parameters.get("additionalProperties").is_some(),
                "shared definition '{}' lost additionalProperties",
                definition.name
            );
        }
        let read_file = &shared[1];
        assert_eq!(read_file.name, "read_file");
        assert!(
            contains_key_recursive(&read_file.parameters, "minimum"),
            "shared read_file schema lost its 'minimum' constraints"
        );

        // Driving the definitions through the Gemini body builder leaves the
        // shared value untouched, and the reduction appears only in the
        // Gemini wire body (a passthrough would leak the keys below).
        let request = tool_request(definitions);
        let body = generate_content_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        let declarations = value["tools"][0]["functionDeclarations"]
            .as_array()
            .expect("declarations");
        for declaration in declarations {
            assert!(
                !contains_key_recursive(&declaration["parameters"], "additionalProperties"),
                "gemini body for '{}' still carries additionalProperties: {}",
                declaration["name"],
                declaration["parameters"]
            );
        }
        for definition in &shared {
            assert!(
                definition.parameters.get("additionalProperties").is_some(),
                "shared definition '{}' was mutated by the gemini builder",
                definition.name
            );
        }
    }

    /// Test (c): a plain chat request (empty tools) serializes without a
    /// `tools` key at all — no empty array may reach the wire.
    #[test]
    fn chat_requests_still_omit_the_tools_field() {
        let json =
            serde_json::to_string(&generate_content_request(&sample_request())).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            value.get("tools").is_none(),
            "chat body must not contain a 'tools' key: {value}"
        );
    }
}
