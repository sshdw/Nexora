//! OpenAI provider integration: a concrete [`ProviderExecutor`] (ROADMAP.md
//! Phase 3 — AI Providers; ARCHITECTURE.md §7).
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
//! # Security (ARCHITECTURE.md §9, §11, §12)
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
    AiMessage, AiRequest, AiResponse, AiRole, ExecutorError, ProviderExecutor,
};

use serde::{Deserialize, Serialize};

/// OpenAI's Chat Completions endpoint.
const ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// Internal provider name (DATABASE.md §7.5); the keyring namespace key.
pub(crate) const PROVIDER_NAME: &str = "openai";
/// User-facing provider label.
pub(crate) const PROVIDER_DISPLAY_NAME: &str = "OpenAI";

/// Concrete [`ProviderExecutor`] for OpenAI.
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

    /// Create an executor targeting an explicit `endpoint` (used by tests to
    /// exercise the full request/response path without a live OpenAI service).
    fn with_endpoint(endpoint: String) -> Self {
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
        let response = send(&self.client, &self.endpoint, credential, &body)?;
        to_ai_response(response)
    }
}

impl ProviderExecutor for OpenAiExecutor {
    fn execute(&self, request: &AiRequest, credential: &str) -> Result<AiResponse, ExecutorError> {
        match self.run(request, credential) {
            Ok(response) => Ok(response),
            Err(error) => {
                // Record only the classification category; never the credential
                // or request payload (ARCHITECTURE.md §9, §11).
                log::warn!("openai request failed: {error}");
                Err(ExecutorError::Failure)
            }
        }
    }
}
/// OpenAI request body mapped from the provider-independent [`AiRequest`].
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
}

/// One OpenAI chat message.
#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

/// Translate a provider-independent request into an OpenAI request body.
///
/// The selected model is passed through unchanged — it is never silently
/// substituted (FR-004). System, user, and assistant messages map to the
/// corresponding OpenAI `role`.
fn chat_completion_request(request: &AiRequest) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: request.model.clone(),
        messages: request.messages.iter().map(openai_message).collect(),
    }
}

/// Map one provider-independent message to an OpenAI chat message.
fn openai_message(message: &AiMessage) -> OpenAiMessage {
    OpenAiMessage {
        role: match message.role {
            AiRole::System => "system",
            AiRole::User => "user",
            AiRole::Assistant => "assistant",
        }
        .to_string(),
        content: message.content.clone(),
    }
}

/// A normalized OpenAI Chat Completions response.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    model: String,
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}
/// Perform the non-streaming HTTPS request.
///
/// The credential is placed only in the `Authorization` header. A non-success
/// response is classified by status without reading its body.
fn send(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    credential: &str,
    body: &ChatCompletionRequest,
) -> Result<ChatCompletionResponse, OpenAiError> {
    let response = client
        .post(endpoint)
        .bearer_auth(credential)
        .json(body)
        .send()
        .map_err(|_| OpenAiError::Provider)?;

    let status = response.status();
    if status.is_success() {
        response
            .json::<ChatCompletionResponse>()
            .map_err(|_| OpenAiError::UnexpectedResponse)
    } else {
        Err(classify_status(status.as_u16()))
    }
}

/// Normalize a successful OpenAI response into the provider-independent
/// [`AiResponse`], preserving the model that actually responded.
fn to_ai_response(response: ChatCompletionResponse) -> Result<AiResponse, OpenAiError> {
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or(OpenAiError::UnexpectedResponse)?;
    Ok(AiResponse {
        content,
        model: response.model,
    })
}

/// Classify a non-success HTTP status into a secret-free failure category.
fn classify_status(status: u16) -> OpenAiError {
    match status {
        400 => OpenAiError::InvalidRequest,
        401 => OpenAiError::Authentication,
        _ => OpenAiError::Provider,
    }
}

/// Classified OpenAI failure categories (secret-free).
///
/// These identify only the failure *category*; no credential, authorization
/// header, or request payload is ever stored. The provider-independent boundary
/// exposes only [`ExecutorError::Failure`] — this richer classification exists
/// so diagnostics can distinguish failure classes in the logs.
#[derive(Debug)]
enum OpenAiError {
    /// The OpenAI endpoint rejected the request as malformed (HTTP 400).
    InvalidRequest,
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
            model: "gpt-4o-mini".to_string(),
            messages: vec![
                AiMessage {
                    role: AiRole::System,
                    content: "You are a helpful assistant.".to_string(),
                },
                AiMessage {
                    role: AiRole::User,
                    content: "Hello".to_string(),
                },
                AiMessage {
                    role: AiRole::Assistant,
                    content: "Hi there".to_string(),
                },
            ],
        }
    }

    #[test]
    fn request_translates_roles_and_model() {
        let body = chat_completion_request(&sample_request());
        // The selected model is passed through unchanged, never substituted.
        assert_eq!(body.model, "gpt-4o-mini");
        let roles: Vec<&str> = body.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["system", "user", "assistant"]);
        assert_eq!(body.messages[0].content, "You are a helpful assistant.");
        assert_eq!(body.messages[1].content, "Hello");
        assert_eq!(body.messages[2].content, "Hi there");
        // Messages remain in chronological order.
        assert_eq!(body.messages.len(), 3);
    }

    #[test]
    fn maps_user_and_assistant_roles_without_system() {
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-4o".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "ping".to_string(),
            }],
        };
        let body = chat_completion_request(&request);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.messages[0].content, "ping");
    }

    #[test]
    fn response_maps_to_ai_response() {
        let response = ChatCompletionResponse {
            model: "gpt-4o-mini".to_string(),
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("Hello to you too.".to_string()),
                },
            }],
        };
        let ai = to_ai_response(response).expect("valid response maps");
        assert_eq!(ai.content, "Hello to you too.");
        assert_eq!(ai.model, "gpt-4o-mini");
    }

    #[test]
    fn response_without_content_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-4o-mini".to_string(),
            choices: vec![Choice {
                message: ResponseMessage { content: None },
            }],
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn response_without_choices_is_unexpected() {
        let response = ChatCompletionResponse {
            model: "gpt-4o-mini".to_string(),
            choices: vec![],
        };
        assert!(matches!(
            to_ai_response(response),
            Err(OpenAiError::UnexpectedResponse)
        ));
    }

    #[test]
    fn statuses_classify_without_secrets() {
        assert!(matches!(classify_status(400), OpenAiError::InvalidRequest));
        assert!(matches!(classify_status(401), OpenAiError::Authentication));
        for status in [403, 429, 500, 502, 503, 504] {
            assert!(matches!(classify_status(status), OpenAiError::Provider));
        }
    }

    #[test]
    fn executor_maps_every_classified_failure_to_boundary_failure() {
        let executor = OpenAiExecutor {
            client: reqwest::blocking::Client::new(),
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable -> network failure
        };
        // The boundary must only ever surface ExecutorError::Failure, never an
        // OpenAI-specific or secret-bearing type.
        let result = executor.execute(&sample_request(), "sk-secret-example");
        assert!(matches!(result, Err(ExecutorError::Failure)));
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
            let body = r#"{"model":"gpt-4o-mini","choices":[{"message":{"content":"pong"}}]}"#;
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
        assert_eq!(ai.model, "gpt-4o-mini");
    }
}
