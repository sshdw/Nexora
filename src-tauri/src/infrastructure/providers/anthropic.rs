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
//! Anthropic-specific is exposed through [`application::execution`]; the
//! boundary only ever sees [`AiRequest`]/[`AiResponse`]/[`ExecutorError`].
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
//! - The credential is supplied by the caller from the [`CredentialStore`] for
//!   the duration of the call only; it is never persisted, logged, or returned.
//! - The credential is sent only in the `x-api-key` header via `reqwest`, never
//!   in the body.
//! - Failed responses are classified by HTTP status **without reading the error
//!   body**, so a provider diagnostic can never leak the credential or the
//!   request payload into an error or log.
//! - All failures collapse to the single provider-independent
//!   [`ExecutorError::Failure`]; the internal [`AnthropicError`] classification
//!   (authentication, invalid request, provider/network, unexpected response) is
//!   recorded in the logs by category only.

// The module docs reference product/brand names (Anthropic, Messages) that the
// `doc_markdown` pedantic lint flags as needing backticks. Allow it locally.
#![allow(clippy::doc_markdown)]

use crate::application::execution::{
    AiAttachment, AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, ExecutorError, ProviderExecutor,
};

use serde::{Deserialize, Serialize};

/// Anthropic's Messages endpoint.
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

/// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = "anthropic";

/// User-facing provider label.
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "Anthropic";

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
/// Ordered per Nexora-AI-Model-Catalog-August-2026.md В§10/В§11; the first entry
/// is the provider default consumed as `models[0]` by the selection surface
/// (`claude-3-5-sonnet-20240620`, the previous sole entry, was retired
/// October 28, 2025).
pub(crate) const SUPPORTED_MODELS: &[&str] = &[
    // Default: best cost/quality balance, 1M context.
    "claude-sonnet-5",
    // Fast/cheap tier.
    "claude-haiku-4-5-20251001",
    // Best-quality flagship tier.
    "claude-opus-4-8",
];

/// Concrete [`ProviderExecutor`] for Anthropic.
///
/// Stateless over the shared `reqwest` blocking client so it can be shared
/// across requests; the per-request credential and request payload are passed
/// into each [`ProviderExecutor::execute`] call and dropped on return.
pub(crate) struct AnthropicExecutor {
    client: reqwest::blocking::Client,
    endpoint: String,
}

impl AnthropicExecutor {
    /// Create an executor targeting the Anthropic production endpoint.
    pub(crate) fn new() -> Self {
        Self::with_endpoint(ENDPOINT.to_string())
    }

    /// Create an executor targeting an explicit `endpoint` (used by tests to
    /// exercise the full request/response path without a live Anthropic
    /// service).
    fn with_endpoint(endpoint: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            endpoint,
        }
    }

    /// Translate, send, and normalize one request.
    ///
    /// Returns a provider-independent [`AiResponse`] on success, or a
    /// classified [`AnthropicError`] describing the failure category.
    fn run(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, AnthropicError> {
        let body = anthropic_request(request);
        let response = send(&self.client, &self.endpoint, credential, &body)?;
        to_ai_response(response)
    }
}

impl ProviderExecutor for AnthropicExecutor {
    fn execute(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, ExecutorError> {
        match self.run(request, credential) {
            Ok(response) => Ok(response),
            Err(error) => {
                // Record only the classification category; never the credential
                // or request payload (ARCHITECTURE.md В§9, В§11).
                log::warn!("anthropic request failed: {error}");
                Err(ExecutorError::Failure)
            }
        }
    }
}

/// Anthropic Messages request body, mapped from the provider-independent
/// [`AiRequest`].
///
/// `system` is a top-level Anthropic-specific parameter and is omitted (via
/// `skip_serializing_if`) when no system message is present, so the wire shape
/// matches Anthropic's contract in both cases.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
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

/// One Anthropic content block: text, a base64 image, or a base64 PDF
/// document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlock {
    /// A plain text segment.
    Text { text: String },
    /// An image supplied as inline base64 data (FR-008).
    Image { source: AnthropicSource },
    /// A PDF document supplied as inline base64 data (FR-008).
    Document { source: AnthropicSource },
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
    let system = collect_system(&request.messages);
    let messages: Vec<AnthropicMessage> = request
        .messages
        .iter()
        .filter(|message| message.role != AiRole::System)
        .map(|message| AnthropicMessage {
            role: match message.role {
                AiRole::User => "user",
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
    AnthropicRequest {
        model: request.model.clone(),
        max_tokens: DEFAULT_MAX_TOKENS,
        messages,
        system,
    }
}

/// Build the Anthropic `content` value for one message.
///
/// Plain string when the turn carries no binary attachments (identical to the
/// pre-FR-008 wire shape); otherwise a text block followed by one base64
/// `image` or `document` block per binary attachment.
fn anthropic_content(message: &AiMessage) -> AnthropicContent {
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
}

/// One content block from an Anthropic response. Non-streaming text
/// completions produce `text` blocks; the `type` discriminator is preserved so
/// only `text` blocks are extracted.
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Normalize a successful Anthropic response into the provider-independent
/// [`AiResponse`].
///
/// The assistant's text is taken from the first `text` content block. If the
/// response contains no extractable text (empty `content`, no `text` block, or
/// a block missing `text`), this returns [`AnthropicError::UnexpectedResponse`]
/// rather than inventing a partial response shape.
fn to_ai_response(response: AnthropicResponse) -> Result<AiResponse, AnthropicError> {
    let content = response
        .content
        .into_iter()
        .find(|block| block.kind == "text")
        .and_then(|block| block.text)
        .ok_or(AnthropicError::UnexpectedResponse)?;
    Ok(AiResponse {
        content,
        model: response.model,
    })
}

/// Perform the non-streaming HTTPS request.
///
/// The credential is placed only in the `x-api-key` header (plus the required
/// `anthropic-version` header). A non-success response is classified by status
/// without reading its body, so provider diagnostics can never leak the
/// credential or request payload.
fn send(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    credential: &str,
    body: &AnthropicRequest,
) -> Result<AnthropicResponse, AnthropicError> {
    let response = client
        .post(endpoint)
        .header("x-api-key", credential)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(body)
        .send()
        .map_err(|_| AnthropicError::Network)?;

    let status = response.status();
    if status.is_success() {
        response
            .json::<AnthropicResponse>()
            .map_err(|_| AnthropicError::UnexpectedResponse)
    } else {
        Err(classify_status(status.as_u16()))
    }
}

/// Classify a non-success HTTP status into a secret-free failure category.
fn classify_status(status: u16) -> AnthropicError {
    match status {
        400 => AnthropicError::InvalidRequest,
        401 => AnthropicError::Authentication,
        _ => AnthropicError::Provider,
    }
}

/// Classified Anthropic failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] вЂ” this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug)]
enum AnthropicError {
    /// The Anthropic endpoint rejected the request as malformed (HTTP 400).
    InvalidRequest,
    /// The credential was rejected (HTTP 401).
    Authentication,
    /// A network/transport failure (connection refused, DNS, timeout, ...).
    Network,
    /// A provider-side error (HTTP 4xx other than 400/401, 5xx, 429, ...).
    Provider,
    /// The response was not a recognizable message completion (e.g. missing
    /// text content or malformed JSON).
    UnexpectedResponse,
}

impl std::fmt::Display for AnthropicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(f, "the Anthropic request was invalid (400)"),
            Self::Authentication => write!(f, "Anthropic rejected the credential (401)"),
            Self::Network => write!(f, "Anthropic network or transport failure"),
            Self::Provider => write!(f, "Anthropic provider failure"),
            Self::UnexpectedResponse => write!(f, "Anthropic returned an unexpected response"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "claude-sonnet-5".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::System,
                    content: "You are a helpful assistant.".to_string(),                    attachments: Vec::new(),
                },
                AiMessage {
                    role: AiRole::User,
                    content: "Hello".to_string(),                    attachments: Vec::new(),
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: "Hi there".to_string(),                    attachments: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn system_message_is_mapped_to_top_level_field() {
        let body = anthropic_request(&sample_request());
        // The system instruction is extracted into the top-level `system`
        // parameter, not emitted as a message.
        assert_eq!(body.system.as_deref(), Some("You are a helpful assistant."));
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
        assert_eq!(assistant.content, AnthropicContent::Text("Hi there".to_string()));
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

        // System content is a top-level field, not a message.
        assert_eq!(value["system"], "You are a helpful assistant.");
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
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),                attachments: Vec::new(),
            }],
        };
        let body = anthropic_request(&request);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).expect("serialize")).expect("parse");
        assert!(value.get("system").is_none());
        assert!(body.system.is_none());
    }

    #[test]
    fn successful_response_is_normalized() {
        let response = AnthropicResponse {
            model: "claude-sonnet-5".to_string(),
            content: vec![ContentBlock {
                kind: "text".to_string(),
                text: Some("pong".to_string()),
            }],
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
            }],
        };
        assert!(matches!(
            to_ai_response(response),
            Err(AnthropicError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(
            classify_status(400),
            AnthropicError::InvalidRequest
        ));
        assert!(matches!(
            classify_status(401),
            AnthropicError::Authentication
        ));
        for status in [403, 404, 429, 500, 502, 503, 504] {
            assert!(
                matches!(classify_status(status), AnthropicError::Provider),
                "status {status} should classify as provider failure"
            );
        }
    }

    #[test]
    fn executor_maps_failure_to_boundary_failure() {
        let executor = AnthropicExecutor {
            client: reqwest::blocking::Client::new(),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
        };
        // The boundary must only ever surface ExecutorError::Failure, never an
        // Anthropic-specific or secret-bearing type.
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(ExecutorError::Failure)));
    }

    #[test]
    fn credential_never_appears_in_returned_error() {
        let executor = AnthropicExecutor {
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
        let executor = AnthropicExecutor::with_endpoint("http://127.0.0.1:1".to_string());
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(AnthropicError::Network)));
    }

    #[test]
    fn run_classifies_authentication_failure() {
        let (endpoint, _captured, server) = spawn_server(401, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(AnthropicError::Authentication)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_invalid_request_failure() {
        let (endpoint, _captured, server) = spawn_server(400, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(AnthropicError::InvalidRequest)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn run_classifies_provider_failure() {
        let (endpoint, _captured, server) = spawn_server(500, "");
        let executor = AnthropicExecutor::with_endpoint(endpoint);
        let result = executor.run(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(AnthropicError::Provider)));
        server.join().expect("server thread joins");
    }

    #[test]
    fn binary_attachments_serialize_as_anthropic_blocks() {
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
        let body =
            r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#;
        let (endpoint, _captured, server) = spawn_server(200, body);
        let executor = AnthropicExecutor::with_endpoint(endpoint);
        let ai = executor
            .execute(&sample_request(), "sk-secret-example")
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "claude-sonnet-5");
    }

    #[test]
    fn request_authenticates_with_x_api_key_header() {
        let body =
            r#"{"model":"claude-sonnet-5","content":[{"type":"text","text":"pong"}]}"#;
        let (endpoint, captured, server) = spawn_server(200, body);
        let executor = AnthropicExecutor::with_endpoint(endpoint);
        let credential = "sk-secret-example";
        executor
            .execute(&sample_request(), credential)
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
}
