//! Command error boundary: converts application-layer classified errors into
//! safe, serializable, secret-free responses for the frontend (Phase 10.2 —
//! Tauri Command Layer).
//!
//! Every Tauri command returns [`Result<T, CommandError>`]. The error carries a
//! stable machine-readable [`ErrorKind`] and a human-readable `message`, and is
//! the single translation point from the application/infrastructure error
//! types. It deliberately never exposes a credential, raw SQL, a file path, or
//! a conversation/prompt payload (ARCHITECTURE.md §9, §11; DATABASE.md §14).

use serde::Serialize;

use crate::application::attachments::AttachmentError;
use crate::application::conversations::ConversationError;
use crate::application::data_management::DataManagementError;
use crate::application::execution::RequestError;
use crate::application::export::ExportError;
use crate::application::import::ImportError;
use crate::application::prompts::PromptLibraryError;
use crate::application::providers::ProviderError;
use crate::application::search::SearchError;
use crate::infrastructure::database::DatabaseError;
use crate::infrastructure::providers::credentials::CredentialError;

/// Stable, secret-free error categories the frontend can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ErrorKind {
    /// The referenced entity does not exist.
    NotFound,
    /// A caller-supplied value was rejected (length, empty, ...).
    InvalidInput,
    /// A persistence / SQLite operation failed.
    Database,
    /// The OS secure keyring could not satisfy a credential operation.
    Credential,
    /// AI request execution failed (unknown/unavailable provider, missing
    /// credentials, provider failure, ...).
    Request,
    /// A destructive operation was invoked without the required explicit
    /// confirmation phrase.
    ConfirmationRequired,
    /// A document could not be serialized or deserialized.
    Serialization,
    /// The input was not valid JSON.
    InvalidJson,
    /// The input used an unsupported document format.
    UnsupportedFormat,
    /// The input used an unsupported document version.
    UnsupportedVersion,
    /// The input carried invalid document data.
    InvalidData,
    /// A filesystem I/O operation failed.
    Io,
}

/// Serializable, secret-free command error returned to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandError {
    /// Stable machine-readable category, safe to surface and branch on.
    pub kind: ErrorKind,
    /// Human-readable message. Never contains a credential, raw SQL, or a
    /// stored payload (prompt / message content, file path).
    pub message: String,
}

impl CommandError {
    /// Build a classified command error with a curated `message`.
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CommandError {}

/// Curated, secret-free message for an unclassified SQLite failure. The raw
/// rusqlite detail is deliberately not surfaced to avoid leaking SQL or a
/// stored value; the underlying failure is logged instead.
impl From<DatabaseError> for CommandError {
    fn from(err: DatabaseError) -> Self {
        log::error!("command failed with database error: {err}");
        Self::new(
            ErrorKind::Database,
            "a database operation failed; no data was changed".to_string(),
        )
    }
}

impl From<CredentialError> for CommandError {
    fn from(err: CredentialError) -> Self {
        match err {
            CredentialError::StorageUnavailable => Self::new(
                ErrorKind::Credential,
                "the provider credential store is unavailable (OS secure keyring)",
            ),
            CredentialError::Invalid => Self::new(
                ErrorKind::Credential,
                "the provider credential could not be read or written in the OS secure keyring",
            ),
        }
    }
}
impl From<ProviderError> for CommandError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Database(inner) => Self::from(inner),
            ProviderError::Credential(inner) => Self::from(inner),
        }
    }
}

impl From<RequestError> for CommandError {
    fn from(err: RequestError) -> Self {
        match err {
            RequestError::UnknownProvider { name } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' is not configured"),
            ),
            RequestError::ProviderUnavailable { name } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' is not available"),
            ),
            RequestError::MissingCredentials { name } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' has no stored credentials"),
            ),
            RequestError::ExecutorUnavailable { name } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' has no registered executor"),
            ),
            RequestError::Execution { name } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' failed to fulfil the request"),
            ),
            RequestError::Credential(inner) => Self::from(inner),
            RequestError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<ConversationError> for CommandError {
    fn from(err: ConversationError) -> Self {
        match err {
            ConversationError::NotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            ConversationError::UnexpectedMessageRole { role } => {
                log::error!("conversation history contains invalid message role '{role}'");
                Self::new(
                    ErrorKind::Database,
                    "the conversation history contains an invalid stored message role",
                )
            }
            ConversationError::Request(inner) => Self::from(inner),
            ConversationError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<PromptLibraryError> for CommandError {
    fn from(err: PromptLibraryError) -> Self {
        match err {
            PromptLibraryError::PromptNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("prompt {id} does not exist"))
            }
            PromptLibraryError::ConversationNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("conversation {id} does not exist"))
            }
            PromptLibraryError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<AttachmentError> for CommandError {
    fn from(err: AttachmentError) -> Self {
        match err {
            AttachmentError::ConversationNotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            AttachmentError::AttachmentNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("attachment {id} does not exist"))
            }
            AttachmentError::InvalidInput { field, reason } => Self::new(
                ErrorKind::InvalidInput,
                format!("invalid {field}: {reason}"),
            ),
            AttachmentError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<SearchError> for CommandError {
    fn from(err: SearchError) -> Self {
        match err {
            SearchError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<ExportError> for CommandError {
    fn from(err: ExportError) -> Self {
        match err {
            ExportError::NotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            ExportError::Database(inner) => Self::from(inner),
            ExportError::Serialization(inner) => {
                log::error!("conversation export serialization failed: {inner}");
                Self::new(
                    ErrorKind::Serialization,
                    "the conversation could not be serialized for export",
                )
            }
            ExportError::Io(_) => Self::new(
                ErrorKind::Io,
                "the exported conversation could not be written to disk",
            ),
        }
    }
}

impl From<ImportError> for CommandError {
    fn from(err: ImportError) -> Self {
        match err {
            ImportError::InvalidJson(_) => {
                Self::new(ErrorKind::InvalidJson, "the import document is not valid JSON")
            }
            ImportError::UnsupportedFormat { .. } => Self::new(
                ErrorKind::UnsupportedFormat,
                "the import document uses an unsupported format",
            ),
            ImportError::UnsupportedVersion { version } => Self::new(
                ErrorKind::UnsupportedVersion,
                format!("the import document uses an unsupported version ({version})"),
            ),
            ImportError::InvalidData(reason) => Self::new(
                ErrorKind::InvalidData,
                format!("the import document is invalid: {reason}"),
            ),
            ImportError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<DataManagementError> for CommandError {
    fn from(err: DataManagementError) -> Self {
        match err {
            DataManagementError::ConfirmationRequired => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before destructive data-management operations",
            ),
            DataManagementError::Database(inner) => Self::from(inner),
        }
    }
}