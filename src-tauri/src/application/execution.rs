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

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::anthropic::{
    AnthropicExecutor, PROVIDER_NAME as ANTHROPIC_PROVIDER_NAME,
};
use crate::infrastructure::providers::credentials::{CredentialError, CredentialStore};
use crate::infrastructure::providers::gemini::{
    GeminiExecutor, PROVIDER_NAME as GEMINI_PROVIDER_NAME,
};
use crate::infrastructure::providers::openai::{OpenAiExecutor, PROVIDER_NAME};
use serde::{Deserialize, Serialize};

use super::providers::{ProviderError, ProviderService};

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

/// Error raised by a [`ProviderExecutor`] while executing a request.
///
/// Deliberately carries **no secret payload** and **no provider-specific
/// detail**: it classifies only the failure category so a formatted
/// [`ExecutorError`] can never leak a credential (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub(crate) enum ExecutorError {
    /// The provider could not fulfil the request (network, authentication, or
    /// provider-side failure).
    Failure,
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failure => write!(f, "the AI provider failed to fulfil the request"),
        }
    }
}

impl std::error::Error for ExecutorError {}

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
    ) -> std::result::Result<AiResponse, ExecutorError>;
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
    executors: Vec<(&'static str, Box<dyn ProviderExecutor>)>,
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
                (PROVIDER_NAME, Box::new(OpenAiExecutor::new())),
                (ANTHROPIC_PROVIDER_NAME, Box::new(AnthropicExecutor::new())),
                (GEMINI_PROVIDER_NAME, Box::new(GeminiExecutor::new())),
            ],
        }
    }

    /// Resolve the executor registered for `name`, if any.
    ///
    /// Returns [`None`] when no executor is registered for `name`; no fallback
    /// or automatic provider selection is performed.
    pub(crate) fn resolve(&self, name: &str) -> Option<&dyn ProviderExecutor> {
        self.executors
            .iter()
            .find(|(registered, _)| *registered == name)
            .map(|(_, executor)| executor.as_ref())
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
        // 1. Resolve the requested provider through the Provider Metadata
        //    Service (ARCHITECTURE.md §7: provider selection).
        let provider = self
            .provider
            .read_by_name(&request.provider)
            .map_err(RequestError::from)?
            .ok_or_else(|| RequestError::UnknownProvider {
                name: request.provider.clone(),
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
        let credential = match CredentialStore::read(&provider.name) {
            Ok(Some(secret)) => secret,
            Ok(None) => {
                return Err(RequestError::MissingCredentials {
                    name: provider.name,
                })
            }
            Err(CredentialError::StorageUnavailable) => {
                return Err(RequestError::ProviderUnavailable {
                    name: provider.name,
                });
            }
            Err(err) => return Err(RequestError::Credential(err)),
        };

        // 4. Resolve the concrete executor for this provider through the
        //    registry. A provider whose metadata exists but has no registered
        //    executor cannot fulfil the request; it fails explicitly with a
        //    classified error rather than falling back to another provider.
        let executor = self.executors.resolve(&provider.name).ok_or_else(|| {
            RequestError::ExecutorUnavailable {
                name: provider.name.clone(),
            }
        })?;

        // 5. Delegate to the provider-independent boundary. The credential is
        //    moved only into this call and dropped when it returns.
        executor
            .execute(request, &credential)
            .map_err(|_| RequestError::Execution {
                name: provider.name,
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
    /// The provider failed to fulfil the request (network, authentication, or
    /// provider-side failure).
    Execution {
        /// The provider internal name.
        name: String,
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
            Self::Execution { name } => {
                write!(f, "the AI provider '{name}' failed to fulfil the request")
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
        };
        let composed = message.composed_content();
        assert_eq!(
            composed,
            "Look\n\n[Attached file: report.pdf] (binary content attached)"
        );
        // The base64 data itself never enters the text channel.
        assert!(!composed.contains("Zm9vYmFy"));
    }
}
