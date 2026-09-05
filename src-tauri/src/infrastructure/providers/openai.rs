//! OpenAI provider integration: a concrete [`ProviderExecutor`] (ROADMAP.md
//! Phase 3 вЂ” AI Providers; ARCHITECTURE.md В§7).
//!
//! This is the first concrete provider implementation behind the
//! provider-independent execution boundary. It translates a provider-agnostic
//! [`AiRequest`] into a non-streaming OpenAI Chat Completions request, sends it
//! over HTTPS using the crate's HTTP facility (`reqwest`), and normalizes the
//! response into the provider-independent [`AiResponse`].
//!
//! Isolation: every OpenAI-specific wire type lives in this module. Nothing
//! OpenAI-specific is exposed through [`application::execution`]; the boundary
//! only ever sees [`AiRequest`]/[`AiResponse`]/[`ExecutorError`]. The same
//! boundary can later host Anthropic, Gemini, DeepSeek, Kimi, or Grok without
//! touching this contract.
//!
//! # Security (ARCHITECTURE.md В§9, В§11, В§12)
//!
//! - The credential is supplied by the caller from the [`CredentialStore`] for
//!   the duration of the call only; it is never persisted, logged, or returned.
//! - The credential is sent only in the `Authorization` header via
//!   `reqwest`'s `bearer_auth`, never in the body.
//! - Failed responses are classified by HTTP status **without reading the
//!   error body**, so a provider diagnostic can never leak the credential or
//!   the request payload into an error or log.
//! - All failures collapse to the single provider-independent
//!   [`ExecutorError::Failure`]; the internal [`OpenAiError`] classification
//!   (authentication, invalid request, provider/network, unexpected response)
//!   is recorded in the logs by category only.

// The module docs deliberately reference product/brand names (OpenAI,
// Anthropic, Gemini, DeepSeek, Kimi, Grok, Chat Completions), which the
// `doc_markdown` pedantic lint flags as needing backticks. Allow it locally.
#![allow(clippy::doc_markdown)]

use crate::application::execution::{
    AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, ExecutorError, ProviderExecutor,
};
// `AiAttachment` is referenced only by this module's unit tests.
#[cfg(test)]
use crate::application::execution::AiAttachment;

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OpenAI's Chat Completions endpoint.
const ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// Internal provider name (DATABASE.md В§7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = "openai";
/// User-facing provider label.
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "OpenAI";

/// OpenAI models currently supported by the provider (DATABASE.md В§7.5: model
/// lists are hardcoded in the MVP and managed by the application layer).
///
/// The selected model is passed through unchanged and is never validated
/// against this list at runtime (never silently substituted, never rejected):
/// this set documents the currently supported models and anchors the
/// model-selection surface so the UI can present only supported choices.
///
/// Pricing is governed by the policy table in
/// `crate::application::agent::pricing` (DATABASE.md В§7.8); the first entry
/// is the provider default consumed as `models[0]` by the selection surface.
pub(crate) const SUPPORTED_MODELS: &[&str] = &[
    // Default: best balance of cost and capability.
    "gpt-5.6-terra",
    // Fast/cheap tier.
    "gpt-5.6-luna",
    // Best-quality flagship tier.
    "gpt-5.6-sol",
];
///
/// Stateless over the shared `reqwest` blocking client so it can be shared
/// across requests; the per-request credential and request payload are passed
/// into each [`ProviderExecutor::execute`] call and dropped on return.
pub(crate) struct OpenAiExecutor {
    client: reqwest::blocking::Client,
    endpoint: String,
}

impl OpenAiExecutor {
    /// Create an executor targeting the OpenAI production endpoint.
    pub(crate) fn new() -> Self {
        Self::with_endpoint(ENDPOINT.to_string())
    }

    /// Create an executor targeting an explicit `endpoint` (used by tests,
    /// including the command-layer threading regression test, to exercise
    /// the full request/response path without a live OpenAI service).
    pub(crate) fn with_endpoint(endpoint: String) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            endpoint,
        }
    }

    /// Translate, send, and normalize one request.
    ///
    /// Returns a provider-independent [`AiResponse`] on success, or a
    /// classified [`OpenAiError`] describing the failure category.
    fn run(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, OpenAiError> {
        let body = chat_completion_request(request);
        let response = send(
            &self.client,
            &self.endpoint,
            credential,
            &body,
            request.request_timeout,
        )?;
        to_ai_response(response)
    }
}

impl ProviderExecutor for OpenAiExecutor {
    fn execute(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, ExecutorError> {
        match self.run(request, credential) {
            Ok(response) => Ok(response),
            Err(error) => {
                // Record only the classification category; never the credential
                // or request payload (ARCHITECTURE.md В§9, В§11).
                log::warn!("openai request failed: {error}");
                Err(match error {
                    OpenAiError::InvalidRequest => ExecutorError::InvalidRequest,
                    OpenAiError::Authentication => ExecutorError::Authentication,
                    OpenAiError::Network => ExecutorError::Network,
                    OpenAiError::RateLimited { retry_after_secs } => {
                        ExecutorError::RateLimited { retry_after_secs }
                    }
                    OpenAiError::ProviderUnavailable => ExecutorError::ProviderUnavailable,
                    OpenAiError::UnexpectedResponse => ExecutorError::UnexpectedResponse,
                    OpenAiError::Provider => ExecutorError::Failure,
                })
            }
        }
    }
}
/// OpenAI request body mapped from the provider-independent [`AiRequest`].
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tools: Vec<OpenAiWireTool>,
}

#[derive(Debug, Serialize)]
struct OpenAiWireTool {
    r#type: &'static str,
    function: OpenAiWireFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiWireFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

/// One OpenAI chat message.
///
/// `content` is serialized as `null` for assistant turns that carry only
/// `tool_calls` (OpenAI contract); regular turns keep the plain-string or
/// parts form of the pre-FR-008 / FR-008 shapes. Tool-result turns set
/// `tool_call_id` and a text content.
#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    // Serialized explicitly (None renders as JSON null) so assistant
    // tool-call turns carry `"content": null` per the OpenAI contract.
    content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiReqWireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// OpenAI assistant `tool_calls` entry (request direction).
#[derive(Debug, Serialize)]
struct OpenAiReqWireToolCall {
    id: String,
    r#type: &'static str,
    function: OpenAiReqWireFunctionCall,
}

/// OpenAI assistant `function` payload: `name` plus the raw `arguments`
/// string exactly as the model produced it.
#[derive(Debug, Serialize)]
struct OpenAiReqWireFunctionCall {
    name: String,
    arguments: String,
}

/// OpenAI Chat Completions `content` values: a plain string, or an array of
/// text/image parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}
/// One OpenAI content part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiContentPart {
    /// A plain text segment.
    Text { text: String },
    /// An image supplied as a base64 data URI (OpenAI has no inline PDF or
    /// arbitrary-binary input in Chat Completions).
    ImageUrl { image_url: OpenAiImageUrl },
}

/// The data-URI wrapper of an OpenAI image part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OpenAiImageUrl {
    url: String,
}

/// Translate a provider-independent request into an OpenAI request body.
///
/// The selected model is passed through unchanged вЂ” it is never silently
/// substituted (FR-004). System, user, and assistant messages map to the
/// corresponding OpenAI `role`.
fn chat_completion_request(request: &AiRequest) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: request.model.clone(),
        messages: request.messages.iter().map(openai_message).collect(),
        tools: request
            .tools
            .iter()
            .map(|tool| OpenAiWireTool {
                r#type: "function",
                function: OpenAiWireFunction {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect(),
    }
}

/// Map one provider-independent message to an OpenAI chat message.
///
// Attachment payloads are rendered per the OpenAI contract: inline text file
// contents become part of the turn text; base64 images become `image_url`
// data-URI parts (FR-008).
fn openai_message(message: &AiMessage) -> OpenAiMessage {
    // Assistant agent turn with structured tool calls: content is `null` and
    // the calls ride the `tool_calls` array with verbatim argument strings.
    if message.role == AiRole::Assistant && !message.tool_calls.is_empty() {
        return OpenAiMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(
                message
                    .tool_calls
                    .iter()
                    .map(|call| OpenAiReqWireToolCall {
                        id: call.id.clone(),
                        r#type: "function",
                        function: OpenAiReqWireFunctionCall {
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
        };
    }
    // Tool result turn: role "tool" answering one call by id.
    if message.role == AiRole::Tool {
        let result = message.tool_result.as_ref().expect("tool result present");
        return OpenAiMessage {
            role: "tool".to_string(),
            content: Some(OpenAiContent::Text(result.content.clone())),
            tool_calls: None,
            tool_call_id: Some(result.call_id.clone()),
        };
    }
    let mut parts: Vec<OpenAiContentPart> = Vec::new();
    for attachment in &message.attachments {
        if let AiAttachmentPayload::Base64(data) = &attachment.payload {
            let mime = attachment
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            parts.push(OpenAiContentPart::ImageUrl {
                image_url: OpenAiImageUrl {
                    url: format!("data:{mime};base64,{data}"),
                },
            });
        }
    }
    // Plain string when there is nothing structural to send: the wire format
    // of attachment-free requests stays exactly as before.
    let content = if parts.is_empty() {
        OpenAiContent::Text(message.composed_content())
    } else {
        parts.insert(
            0,
            OpenAiContentPart::Text {
                text: message.composed_content(),
            },
        );
        OpenAiContent::Parts(parts)
    };
    OpenAiMessage {
        role: match message.role {
            AiRole::System => "system",
            AiRole::User => "user",
            AiRole::Assistant => "assistant",
            // Handled above; unreachable here but keeps the match exhaustive.
            AiRole::Tool => "tool",
        }
        .to_string(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// A normalized OpenAI Chat Completions response.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    model: String,
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiWireToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiWireToolCall {
    id: String,
    // Wire field consumed by serde, retained for OpenAI `type:"function"` compatibility.
    #[allow(dead_code)]
    r#type: String,
    function: OpenAiWireFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAiWireFunctionCall {
    name: String,
    arguments: String,
}
/// Perform the non-streaming HTTPS request.
///
/// The credential is placed only in the `Authorization` header. A non-success
/// response is classified by status without reading its body.
///
/// `request_timeout` bounds the single blocking round trip (Task 3.2): the
/// blocking client cannot be interrupted mid-flight, so the honest bound is a
/// wall-clock timeout applied via `RequestBuilder::timeout`. `None` preserves
/// the historical unbounded behavior byte-for-byte.
fn send(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    credential: &str,
    body: &ChatCompletionRequest,
    request_timeout: Option<Duration>,
) -> Result<ChatCompletionResponse, OpenAiError> {
    let mut builder = client.post(endpoint).bearer_auth(credential).json(body);
    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }
    let response = builder.send().map_err(|_| OpenAiError::Network)?;

    let status = response.status();
    if status.is_success() {
        response
            .json::<ChatCompletionResponse>()
            .map_err(|_| OpenAiError::UnexpectedResponse)
    } else {
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        Err(classify_status(status.as_u16(), retry_after_secs))
    }
}

/// Normalize a successful OpenAI response into the provider-independent
/// [`AiResponse`], preserving the model that actually responded.
fn to_ai_response(response: ChatCompletionResponse) -> Result<AiResponse, OpenAiError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(OpenAiError::UnexpectedResponse)?;
    let wire_tool_calls = choice.message.tool_calls.unwrap_or_default();
    let tool_calls: Vec<crate::application::execution::ToolCall> = wire_tool_calls
        .into_iter()
        .map(|call| crate::application::execution::ToolCall {
            id: call.id,
            name: call.function.name,
            arguments: call.function.arguments,
            // OpenAI responses carry no reasoning signature.
            thought_signature: None,
        })
        .collect();
    let content = if let Some(text) = choice.message.content {
        text
    } else if tool_calls.is_empty() {
        return Err(OpenAiError::UnexpectedResponse);
    } else {
        String::new()
    };
    let usage = response.usage.and_then(|u| {
        if u.prompt_tokens == 0 && u.completion_tokens == 0 {
            None
        } else {
            Some(crate::application::execution::TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
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

/// Classify a non-success HTTP status into a secret-free failure category.
///
/// `retry_after_secs` is the provider's `Retry-After` header parsed as integer
/// seconds (absent or HTTP-date values are `None`) and is carried only by the
/// 429 [`OpenAiError::RateLimited`] category.
fn classify_status(status: u16, retry_after_secs: Option<u64>) -> OpenAiError {
    match status {
        400 => OpenAiError::InvalidRequest,
        401 | 403 => OpenAiError::Authentication,
        429 => OpenAiError::RateLimited { retry_after_secs },
        s if s >= 500 => OpenAiError::ProviderUnavailable,
        _ => OpenAiError::Provider,
    }
}

/// Classified OpenAI failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] вЂ” this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug)]
enum OpenAiError {
    /// The OpenAI endpoint rejected the request as malformed (HTTP 400).
    InvalidRequest,
    /// A network/transport failure (connection refused, DNS, timeout, ...).
    Network,
    /// The provider rate limited the request (HTTP 429), carrying the
    /// provider's `Retry-After` hint when it was a valid integer.
    RateLimited { retry_after_secs: Option<u64> },
    /// The provider is unavailable or overloaded (HTTP 5xx).
    ProviderUnavailable,
    /// The credential was rejected (HTTP 401).
    Authentication,
    /// A network failure or a provider-side error (HTTP 5xx, 429, ...).
    Provider,
    /// The response was not a recognizable chat completion (e.g. missing
    /// content or malformed JSON).
    UnexpectedResponse,
}

impl std::fmt::Display for OpenAiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest => write!(f, "the OpenAI request was invalid (400)"),
            Self::Network => write!(f, "OpenAI network or transport failure"),
            Self::RateLimited { .. } => write!(f, "OpenAI rate limit (429)"),
            Self::ProviderUnavailable => write!(f, "OpenAI unavailable (5xx)"),
            Self::Authentication => write!(f, "OpenAI rejected the credential (401)"),
            Self::Provider => write!(f, "OpenAI provider or network failure"),
            Self::UnexpectedResponse => write!(f, "OpenAI returned an unexpected response"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> AiRequest {
        AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![
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
            ],
            tools: Vec::new(),
            request_timeout: None,
        }
    }

    #[test]
    fn request_translates_roles_and_model() {
        let body = chat_completion_request(&sample_request());
        // The selected model is passed through unchanged, never substituted.
        assert_eq!(body.model, "gpt-5.6-terra");
        let roles: Vec<&str> = body.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["system", "user", "assistant"]);
        assert_eq!(
            body.messages[0].content,
            Some(OpenAiContent::Text(
                "You are a helpful assistant.".to_string()
            ))
        );
        assert_eq!(
            body.messages[1].content,
            Some(OpenAiContent::Text("Hello".to_string()))
        );
        assert_eq!(
            body.messages[2].content,
            Some(OpenAiContent::Text("Hi there".to_string()))
        );
        // Messages remain in chronological order.
        assert_eq!(body.messages.len(), 3);
    }

    #[test]
    fn maps_user_and_assistant_roles_without_system() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-sol".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "ping".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: Vec::new(),
            request_timeout: None,
        };
        let body = chat_completion_request(&request);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(
            body.messages[0].content,
            Some(OpenAiContent::Text("ping".to_string()))
        );
    }

    #[test]
    fn response_maps_to_ai_response() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello to you too.".to_string()),
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("valid response maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gpt-5.6-terra");
    }

    #[test]
    fn response_without_content_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn response_without_choices_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![],
            usage: None,
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(
            classify_status(400, None),
            OpenAiError::InvalidRequest
        ));
        assert!(matches!(
            classify_status(401, None),
            OpenAiError::Authentication
        ));
        assert!(matches!(
            classify_status(403, None),
            OpenAiError::Authentication
        ));
        // 429 carries the Retry-After hint (None when absent/not parseable).
        assert!(matches!(
            classify_status(429, None),
            OpenAiError::RateLimited {
                retry_after_secs: None
            }
        ));
        assert!(matches!(
            classify_status(500, None),
            OpenAiError::ProviderUnavailable
        ));
        assert!(matches!(
            classify_status(503, None),
            OpenAiError::ProviderUnavailable
        ));
        // Other 4xx remain the catch-all provider failure.
        assert!(matches!(classify_status(404, None), OpenAiError::Provider));
    }

    #[test]
    fn executor_maps_every_classified_failure_to_boundary_failure() {
        let executor = OpenAiExecutor {
            client: reqwest::blocking::Client::new(),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
        };
        // The boundary surfaces the classified category (here: network), never an
        // OpenAI-specific or secret-bearing type.
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(ExecutorError::Network)));
    }

    #[test]
    fn binary_attachments_serialize_as_openai_image_parts() {
        let mut request = sample_request();
        request.messages.push(AiMessage {
            role: AiRole::User,
            content: "What is in this image?".to_string(),
            attachments: vec![AiAttachment {
                file_name: "chart.png".to_string(),
                file_size_bytes: Some(4),
                mime_type: Some("image/png".to_string()),
                payload: AiAttachmentPayload::Base64("cG5nIQ==".to_string()),
            }],
            tool_calls: Vec::new(),
            tool_result: None,
        });
        let json = serde_json::to_string(&chat_completion_request(&request)).expect("serialize");

        // Text part plus an image_url part carrying a base64 data URI, per
        // the Chat Completions multimodal content contract.
        assert!(json.contains("\"type\":\"image_url\""));
        assert!(json.contains("data:image/png;base64,cG5nIQ=="));
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
            tool_calls: Vec::new(),
            tool_result: None,
        });
        let body = chat_completion_request(&request);
        let user = body
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("user message");
        let OpenAiContent::Text(text) = user.content.as_ref().expect("content present") else {
            panic!("text-only attachments keep the plain string wire shape");
        };
        assert!(text.contains("revenue rose 12 percent"));
    }

    #[test]
    fn executor_round_trips_through_local_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

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
            // A minimal valid OpenAI-style success body.
            let body = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });

        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let ai = executor
            .execute(&sample_request(), "sk-secret-example")
            .expect("round trip succeeds");
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gpt-5.6-terra");
    }

    #[test]
    fn status_429_maps_to_rate_limited_with_retry_after() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Case 1: 429 with a valid integer Retry-After header.
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
            let response = "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 30\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: Some(30)
            })
        ));
        let _ = server.join();

        // Case 2: 429 without the header -> None.
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
            let response = "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(
            result,
            Err(ExecutorError::RateLimited {
                retry_after_secs: None
            })
        ));
        let _ = server.join();
    }

    #[test]
    fn status_5xx_maps_to_provider_unavailable() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        for status in [500, 503] {
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
                let response = format!(
                    "HTTP/1.1 {status} Server Error\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
            let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
            let result = executor.execute(&sample_request(), "sk-secret-example");
            assert!(
                matches!(result, Err(ExecutorError::ProviderUnavailable)),
                "status {status} should map to ProviderUnavailable, got {result:?}"
            );
            let _ = server.join();
        }
    }

    #[test]
    fn other_client_errors_still_surface_as_failure() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

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
            let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(ExecutorError::Failure)));
        let _ = server.join();
    }

    #[test]
    fn request_with_tools_serializes_with_function_type() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Use the tool".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
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
        let json = serde_json::to_string(&chat_completion_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        // Tools key must be present with correct wire shape.
        let tools = value
            .get("tools")
            .expect("tools present")
            .as_array()
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(
            tools[0]["function"]["description"],
            "Get the weather for a location"
        );
        assert!(tools[0]["function"]["parameters"]["properties"]["location"].is_object());
    }

    #[test]
    fn request_without_tools_omits_tools_key() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: Vec::new(),
            request_timeout: None,
        };
        let json = serde_json::to_string(&chat_completion_request(&request)).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            value.get("tools").is_none(),
            "tools key must be absent when empty"
        );
        // Byte-for-byte backward compatible: no tools key at all.
        assert!(!json.contains("\"tools\""));
    }

    #[test]
    fn response_with_tool_calls_maps_to_ai_response() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: Some(vec![OpenAiWireToolCall {
                        id: "call_123".to_string(),
                        r#type: "function".to_string(),
                        function: OpenAiWireFunctionCall {
                            name: "get_weather".to_string(),
                            arguments: "{\"location\":\"Paris\"}".to_string(),
                        },
                    }]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("valid tool call response maps");
        // Content defaults to empty string when only tool calls are present.
        assert_eq!(ai.content, "");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].id, "call_123");
        assert_eq!(ai.tool_calls[0].name, "get_weather");
        assert_eq!(ai.tool_calls[0].arguments, "{\"location\":\"Paris\"}");
    }

    #[test]
    fn response_with_content_and_tool_calls_maps_both() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("I will call the tool".to_string()),
                    tool_calls: Some(vec![OpenAiWireToolCall {
                        id: "call_456".to_string(),
                        r#type: "function".to_string(),
                        function: OpenAiWireFunctionCall {
                            name: "search".to_string(),
                            arguments: "{\"query\":\"test\"}".to_string(),
                        },
                    }]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("response with content and tool calls maps");
        assert_eq!(ai.content, "I will call the tool");
        assert_eq!(ai.tool_calls.len(), 1);
        assert_eq!(ai.tool_calls[0].name, "search");
    }

    #[test]
    fn plain_text_response_without_tools_still_maps_correctly() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello to you too.".to_string()),
                    tool_calls: None,
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("plain text response maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gpt-5.6-terra");
        assert!(
            ai.tool_calls.is_empty(),
            "plain text must have empty tool_calls"
        );
    }

    #[test]
    fn response_with_empty_tool_calls_and_content_maps_as_text() {
        let response = ChatCompletionResponse {
            model: "gpt-5.6-terra".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Just text".to_string()),
                    tool_calls: Some(vec![]),
                },
            }],
            usage: None,
        };
        let ai = to_ai_response(response).expect("empty tool_calls with content maps");
        assert_eq!(ai.content, "Just text");
        assert!(ai.tool_calls.is_empty());
    }

    /// The native tool round-trip serializes as the OpenAI shapes: an
    /// assistant turn with `"content": null` plus a `tool_calls` array
    /// carrying the verbatim argument string, and a tool-result turn with
    /// `role: "tool"`, `tool_call_id`, and the observation as text content.
    #[test]
    fn tool_round_trip_request_serializes_native_shapes() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![
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
                    tool_calls: vec![crate::application::execution::ToolCall {
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
                    tool_result: Some(crate::application::execution::AiToolResult {
                        call_id: "call_7".to_string(),
                        name: "list_directory".to_string(),
                        content: "a.txt".to_string(),
                    }),
                },
            ],
            tools: Vec::new(),
            request_timeout: None,
        };
        let body = chat_completion_request(&request);
        let value: serde_json::Value =
            serde_json::to_value(&body).expect("request body serializes");

        assert_eq!(
            value["messages"][1],
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_7",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\".\"}"
                        }
                    }
                ]
            })
        );
        assert_eq!(
            value["messages"][2],
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_7",
                "content": "a.txt"
            })
        );
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
            let body = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let mut request = sample_request();
        request.request_timeout = Some(Duration::from_millis(200));
        let start = std::time::Instant::now();
        let result = executor.execute(&request, "sk-secret-example");
        let elapsed = start.elapsed();
        // Must be a classified boundary failure (timeout surfaces as Network).
        assert!(
            matches!(result, Err(ExecutorError::Network)),
            "expected timeout to surface as ExecutorError::Network, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout should fire quickly, elapsed={elapsed:?}"
        );
        let _ = server.join();
    }
    #[test]
    fn usage_present_maps_to_token_usage() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let usage = parsed.usage.clone().expect("usage present");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("ai usage");
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn usage_absent_maps_to_none() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}]}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        assert!(parsed.usage.is_none());
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none());
    }

    #[test]
    fn usage_zero_zero_maps_to_none() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":0,"completion_tokens":0}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        assert!(ai.usage.is_none(), "0,0 should map to None per spec");
    }

    #[test]
    fn usage_partial_zero_maps_to_some() {
        let json = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"hi"}}],"usage":{"prompt_tokens":0,"completion_tokens":5}}"#;
        let parsed: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let ai = to_ai_response(parsed).expect("to_ai");
        let u = ai.usage.expect("some");
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 5);
    }
}
