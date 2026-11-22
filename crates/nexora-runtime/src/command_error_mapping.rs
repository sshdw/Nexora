//! Error-to-taxonomy mapping for the shared backend.
//!
//! These `From<AppError> for CoreError` impls used to live in the desktop
//! command layer. After the application/infrastructure extraction both the
//! source error type and the target `CoreError` are foreign to the desktop
//! crate, so the orphan rule forbids the impl there. The impls are moved
//! verbatim to the crate that owns the source errors; the fixed, secret-free
//! messages and every classification are unchanged. Call sites still write
//! `CommandError::from(err)` — trait impls need no import.

use nexora_core::error::{CoreError as CommandError, CoreErrorKind as ErrorKind};

use crate::application::agent::checkpoints::CheckpointError;
use crate::application::agent::plan::PlanDecisionError;
use crate::application::agent::service::AgentRunError;
use crate::application::agent::tasks::TaskError;
use crate::application::attachments::AttachmentError;
use crate::application::compat::CompatError;
use crate::application::confirmations::ConfirmationError;
use crate::application::conversations::ConversationError;
use crate::application::data_management::DataManagementError;
use crate::application::debt::DebtError;
use crate::application::dep_inventory::DepInventoryError;
use crate::application::execution::RequestError;
use crate::application::export::ExportError;
use crate::application::github::GitHubError;
use crate::application::import::ImportError;
use crate::application::mcp::McpError;
use crate::application::privacy::PrivacyError;
use crate::application::project_dir::ProjectDirError;
use crate::application::prompt_context::ContextError;
use crate::application::prompts::PromptLibraryError;
use crate::application::providers::ProviderError;
use crate::application::pty_terminal::PtyError;
use crate::application::refactor_apply::RefactorApplyError;
use crate::application::release::ReleaseError;
use crate::application::repo_audit::RepoAuditError;
use crate::application::search::SearchError;
use crate::application::settings::ScopedSaveError;
use crate::application::system::SystemError;
use crate::application::version_control::{
    CommitExplainError, CommitMessageError, VersionControlError,
};
use crate::infrastructure::providers::chatgpt_oauth::ChatGptOAuthError;

impl From<ProviderError> for CommandError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::Database(inner) => Self::from(inner),
            ProviderError::Credential(inner) => Self::from(inner),
        }
    }
}

impl From<CompatError> for CommandError {
    fn from(err: CompatError) -> Self {
        match err {
            // Stored endpoint data failed validation: a caller-supplied value
            // was rejected. The reason is already a secret-free category.
            CompatError::InvalidStoredData { reason } => Self::new(ErrorKind::InvalidInput, reason),
            CompatError::Database(inner) => Self::from(inner),
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
            RequestError::InvalidCompatEndpoint { reason } => {
                Self::new(ErrorKind::InvalidInput, reason)
            }
            RequestError::Execution { name, message } => Self::new(
                ErrorKind::Request,
                format!("the AI provider '{name}' failed: {message}"),
            ),
            RequestError::Credential(inner) => Self::from(inner),
            RequestError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<ConversationError> for CommandError {
    fn from(err: ConversationError) -> Self {
        match err {
            ConversationError::AttachmentLimit => Self::new(ErrorKind::InvalidInput, "at most 10 files / 50 MiB per message"),
            ConversationError::NotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            ConversationError::UnknownAttachment { id } => Self::new(
                ErrorKind::InvalidInput,
                format!("attachment {id} does not exist"),
            ),
            ConversationError::ForeignAttachment { id } => Self::new(
                ErrorKind::InvalidInput,
                format!("attachment {id} is not a pending draft of this conversation"),
            ),
            ConversationError::AttachmentUnreadable { name } => Self::new(
                ErrorKind::InvalidInput,
                format!("could not read attached file '{name}' from disk"),
            ),
            ConversationError::AttachmentTooLarge { name, size_bytes, max_bytes } => Self::new(
                ErrorKind::InvalidInput,
                format!(
                    "attached file '{name}' is too large ({size_bytes} bytes; the limit is {max_bytes} bytes)"
                ),
            ),
            ConversationError::AttachmentNotText { name } => Self::new(
                ErrorKind::InvalidInput,
                format!(
                    "attached file '{name}' is not decodable text and has no supported representation"
                ),
            ),
            ConversationError::AttachmentUnsupported { name, provider } => Self::new(
                ErrorKind::InvalidInput,
                format!("the AI provider '{provider}' cannot accept attached file '{name}' of this type"),
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
            PromptLibraryError::ConversationNotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
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
            AttachmentError::AttachmentNotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("attachment {id} does not exist"),
            ),
            AttachmentError::InvalidInput { field, reason } => Self::new(
                ErrorKind::InvalidInput,
                format!("invalid {field}: {reason}"),
            ),
            AttachmentError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<RepoAuditError> for CommandError {
    fn from(err: RepoAuditError) -> Self {
        match err {
            // The workspace path may contain a user name, so even the
            // fixed text names no path.
            RepoAuditError::InvalidRoot => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for audit",
            ),
            RepoAuditError::Io => Self::new(
                ErrorKind::Io,
                "the repository audit could not read the workspace",
            ),
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

impl From<SystemError> for CommandError {
    fn from(err: SystemError) -> Self {
        match err {
            // The curated database text names no SQL and no stored value;
            // the raw detail stays in the server log only.
            SystemError::Database(inner) => Self::from(inner),
            // The backup directory or snapshot file could not be written.
            // The path itself is never echoed: it may carry a user name.
            SystemError::Io => Self::new(
                ErrorKind::Io,
                "the pre-update snapshot could not be written",
            ),
        }
    }
}

impl From<PrivacyError> for CommandError {
    fn from(err: PrivacyError) -> Self {
        match err {
            // The curated database text names no SQL and no stored value;
            // the raw detail stays in the server log only.
            PrivacyError::Database(inner) => Self::from(inner),
            // The destructive-action confirmation pattern, like the
            // refactor apply and data management (never the raw backend
            // text).
            PrivacyError::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before the ledger can be wiped",
            ),
            // Recording outside the fixed kind vocabulary is a caller bug,
            // never user input: fixed text, no kind echoed.
            PrivacyError::UnknownKind => {
                Self::new(ErrorKind::InvalidInput, "unknown ledger event kind")
            }
        }
    }
}

impl From<DepInventoryError> for CommandError {
    fn from(err: DepInventoryError) -> Self {
        match err {
            // The workspace path may contain a user name, so even the
            // fixed text names no path.
            DepInventoryError::InvalidRoot => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for inventory",
            ),
            DepInventoryError::Io => Self::new(
                ErrorKind::Io,
                "the dependency inventory could not read the workspace",
            ),
        }
    }
}

impl From<RefactorApplyError> for CommandError {
    fn from(err: RefactorApplyError) -> Self {
        match err {
            RefactorApplyError::NotARepository => Self::new(
                ErrorKind::InvalidInput,
                "the workspace is not inside a git repository",
            ),
            // The destructive-action confirmation pattern, like the git
            // writes and data management (never the raw backend text, so no
            // path or content can leak).
            RefactorApplyError::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before a refactor can be applied",
            ),
            RefactorApplyError::UnsafeKind => Self::new(
                ErrorKind::InvalidInput,
                "this finding kind is never auto-applied; only confirmed dead-code removals apply",
            ),
            RefactorApplyError::InvalidPath => {
                Self::new(ErrorKind::InvalidInput, "the file path is invalid")
            }
            RefactorApplyError::UncleanFile => Self::new(
                ErrorKind::InvalidInput,
                "the file has uncommitted changes or is untracked; commit or revert first",
            ),
            RefactorApplyError::InvalidRange => {
                Self::new(ErrorKind::InvalidInput, "the line range is invalid")
            }
            RefactorApplyError::ContentMismatch => Self::new(
                ErrorKind::InvalidInput,
                "the target lines no longer match a public item declaration",
            ),
            // Never log the underlying git detail: workspace content may
            // carry user-written secrets, so even server-side logging stays
            // fixed.
            RefactorApplyError::GitFailed => Self::new(ErrorKind::Io, "the git operation failed"),
            RefactorApplyError::Io => {
                Self::new(ErrorKind::Io, "the file could not be read or written")
            }
        }
    }
}

impl From<VersionControlError> for CommandError {
    fn from(err: VersionControlError) -> Self {
        match err {
            VersionControlError::NotARepository => Self::new(
                ErrorKind::InvalidInput,
                "the workspace is not inside a git repository",
            ),
            VersionControlError::InvalidPath => {
                Self::new(ErrorKind::InvalidInput, "the file path is invalid")
            }
            VersionControlError::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before this git write can run",
            ),
            VersionControlError::InvalidMessage => {
                Self::new(ErrorKind::InvalidInput, "the commit message is invalid")
            }
            VersionControlError::InvalidRemote => {
                Self::new(ErrorKind::InvalidInput, "the git remote is not allowed")
            }
            VersionControlError::InvalidBranch => {
                Self::new(ErrorKind::InvalidInput, "the branch name is invalid")
            }
            VersionControlError::BranchNotMerged => Self::new(
                ErrorKind::InvalidInput,
                "the branch has unmerged commits; confirm force delete to remove it",
            ),
            VersionControlError::InvalidInput => {
                Self::new(ErrorKind::InvalidInput, "the git request input is invalid")
            }
            VersionControlError::MergeInProgress => Self::new(
                ErrorKind::InvalidInput,
                "a merge or conflict resolution is currently in progress",
            ),
            // Never log the underlying git detail: diff content may carry
            // user-written secrets, so even server-side logging stays fixed.
            VersionControlError::GitFailed => Self::new(ErrorKind::Io, "the git operation failed"),
        }
    }
}

impl From<CommitMessageError> for CommandError {
    fn from(err: CommitMessageError) -> Self {
        match err {
            // The inner failure is already secret-free fixed vocabulary.
            CommitMessageError::VersionControl(inner) => Self::from(inner),
            CommitMessageError::Request(inner) => Self::from(inner),
        }
    }
}

impl From<GitHubError> for CommandError {
    fn from(err: GitHubError) -> Self {
        match err {
            // Same text as the version-control mapping: the workspace has no
            // usable repository behind it.
            GitHubError::NotARepository => Self::new(
                ErrorKind::InvalidInput,
                "the workspace is not inside a git repository",
            ),
            // The origin URL itself is never echoed: it may carry a user
            // name, so even the fixed text names no remote.
            GitHubError::NoGitHubRemote => Self::new(
                ErrorKind::InvalidInput,
                "the workspace origin is not a github.com repository",
            ),
            GitHubError::InvalidState => {
                Self::new(ErrorKind::InvalidInput, "the state filter is invalid")
            }
            // Transport, timeout, parse, unexpected statuses, and non-quota
            // refusals: the request could not be completed. The token and the
            // response body never reach the text.
            GitHubError::RequestFailed | GitHubError::Forbidden => Self::new(
                ErrorKind::Request,
                "the GitHub request could not be completed",
            ),
            GitHubError::Unauthorized => {
                Self::new(ErrorKind::Request, "the stored GitHub token was rejected")
            }
            GitHubError::NotFound => {
                Self::new(ErrorKind::NotFound, "the GitHub repository was not found")
            }
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
            ExportError::InvalidPath { reason } => Self::new(
                ErrorKind::InvalidInput,
                format!("the export path is invalid: {reason}"),
            ),
            // SEC-03: the caller supplied no live single-use id minted for
            // this export; fixed vocabulary, never echoed.
            ExportError::ConfirmationRequired => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before this export can run",
            ),
        }
    }
}

impl From<ImportError> for CommandError {
    fn from(err: ImportError) -> Self {
        match err {
            ImportError::InvalidJson(_) => Self::new(
                ErrorKind::InvalidJson,
                "the import document is not valid JSON",
            ),
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

impl From<ConfirmationError> for CommandError {
    fn from(err: ConfirmationError) -> Self {
        match err {
            // The scope value itself is never echoed: it is caller-supplied.
            ConfirmationError::InvalidScope => {
                Self::new(ErrorKind::InvalidInput, "the confirmation scope is invalid")
            }
        }
    }
}

impl From<DataManagementError> for CommandError {
    fn from(err: DataManagementError) -> Self {
        match err {
            DataManagementError::AttachmentCleanup => Self::new(
                ErrorKind::Database,
                "attachment cleanup failed; retry deletion",
            ),
            DataManagementError::ConfirmationRequired => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before destructive data-management operations",
            ),
            DataManagementError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<ProjectDirError> for CommandError {
    fn from(err: ProjectDirError) -> Self {
        match err {
            ProjectDirError::OutsideWorkspace => {
                Self::new(ErrorKind::InvalidInput, "the path is outside the workspace")
            }
            ProjectDirError::NotInitialized => Self::new(
                ErrorKind::InvalidInput,
                "the workspace project directory is not initialized",
            ),
            ProjectDirError::UnsupportedVersion { version } => Self::new(
                ErrorKind::UnsupportedVersion,
                format!("the workspace project directory uses an unsupported version ({version})"),
            ),
            ProjectDirError::InvalidManifest => Self::new(
                ErrorKind::InvalidData,
                "the workspace project manifest is invalid",
            ),
            ProjectDirError::InvalidProfile => {
                Self::new(ErrorKind::InvalidData, "the workspace profile is invalid")
            }
            ProjectDirError::InvalidFlags => Self::new(
                ErrorKind::InvalidData,
                "the workspace flags file is invalid",
            ),
            ProjectDirError::InvalidConfig => Self::new(
                ErrorKind::InvalidData,
                "the workspace configuration file is invalid",
            ),
            ProjectDirError::Io => Self::new(
                ErrorKind::Io,
                "a workspace project directory operation failed",
            ),
        }
    }
}

impl From<ScopedSaveError> for CommandError {
    fn from(err: ScopedSaveError) -> Self {
        match err {
            ScopedSaveError::InvalidInput(msg) | ScopedSaveError::SecurityBlocked(msg) => {
                Self::new(ErrorKind::InvalidInput, msg)
            }
            ScopedSaveError::WorkspaceRequired => Self::new(
                ErrorKind::InvalidInput,
                "select a project workspace folder before saving project settings",
            ),
            ScopedSaveError::Database(e) => Self::from(e),
            ScopedSaveError::ProjectDir(e) => Self::from(e),
        }
    }
}

impl From<CheckpointError> for CommandError {
    fn from(err: CheckpointError) -> Self {
        match err {
            CheckpointError::NotFound => Self::new(
                ErrorKind::NotFound,
                "the requested file checkpoint was not found",
            ),
            CheckpointError::ExternalConflict => Self::new(
                ErrorKind::Conflict,
                "the target file was modified externally since this checkpoint was created",
            ),
            CheckpointError::BlobCorrupt => Self::new(
                ErrorKind::InvalidData,
                "the stored checkpoint backup failed integrity verification",
            ),
            CheckpointError::BlobMissing => Self::new(
                ErrorKind::NotFound,
                "the stored checkpoint backup is missing from disk",
            ),
            CheckpointError::InvalidPath | CheckpointError::PathEscape => Self::new(
                ErrorKind::InvalidInput,
                "the checkpoint target path is invalid or outside the workspace",
            ),
            CheckpointError::WorkspaceUnavailable => Self::new(
                ErrorKind::InvalidInput,
                "the checkpoint workspace directory is unavailable",
            ),
            CheckpointError::StoreInsideWorkspace => Self::new(
                ErrorKind::InvalidInput,
                "the checkpoint store must be located outside the workspace",
            ),
            CheckpointError::Database(inner) => Self::from(inner),
            CheckpointError::Io(_) => {
                Self::new(ErrorKind::Io, "the checkpoint file operation failed")
            }
        }
    }
}

impl From<McpError> for CommandError {
    fn from(err: McpError) -> Self {
        match err {
            McpError::NotFound(_) => Self::new(ErrorKind::NotFound, err.to_string()),
            McpError::InvalidConfig(_) | McpError::InvalidUrl(_) => {
                Self::new(ErrorKind::InvalidInput, err.to_string())
            }
            McpError::KeyringUnavailable => Self::new(ErrorKind::Credential, err.to_string()),
            McpError::Database(inner) => Self::from(inner),
            McpError::Transport(_)
            | McpError::Protocol(_)
            | McpError::JsonRpcError { .. }
            | McpError::HttpStatus { .. }
            | McpError::Unauthorized
            | McpError::Timeout
            | McpError::Cancelled
            | McpError::ResponseTooLarge
            | McpError::OAuthStateMismatch
            | McpError::OAuthTimeout
            | McpError::OAuthFailed(_) => Self::new(ErrorKind::Request, err.to_string()),
        }
    }
}

impl From<ChatGptOAuthError> for CommandError {
    fn from(err: ChatGptOAuthError) -> Self {
        match err {
            ChatGptOAuthError::KeyringUnavailable => {
                Self::new(ErrorKind::Credential, err.to_string())
            }
            ChatGptOAuthError::InvalidEndpoint => {
                Self::new(ErrorKind::InvalidInput, err.to_string())
            }
            ChatGptOAuthError::LoopbackBindFailed
            | ChatGptOAuthError::BrowserOpenFailed
            | ChatGptOAuthError::CallbackTimeout
            | ChatGptOAuthError::StateMismatch
            | ChatGptOAuthError::AuthorizationDenied
            | ChatGptOAuthError::MissingCode
            | ChatGptOAuthError::TokenExchangeFailed
            | ChatGptOAuthError::TokenRefreshFailed
            | ChatGptOAuthError::TokenRefreshTransient
            | ChatGptOAuthError::ReauthorizationRequired
            | ChatGptOAuthError::ResponseTooLarge
            | ChatGptOAuthError::NotConnected
            | ChatGptOAuthError::OtherInstance
            | ChatGptOAuthError::RevocationUnconfirmed
            | ChatGptOAuthError::Cancelled => Self::new(ErrorKind::Request, err.to_string()),
        }
    }
}

impl From<AgentRunError> for CommandError {
    fn from(err: AgentRunError) -> Self {
        match err {
            AgentRunError::WorkspaceProtected => Self::new(
                ErrorKind::InvalidInput,
                "the selected workspace is a protected private store",
            ),
            AgentRunError::ConversationNotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            AgentRunError::RunAlreadyActive { conversation_id } => Self::new(
                ErrorKind::InvalidInput,
                format!("an agent run is already active for conversation {conversation_id}"),
            ),
            AgentRunError::RunNotFound { run_id } => Self::new(
                ErrorKind::NotFound,
                format!("agent run {run_id} does not exist"),
            ),
            AgentRunError::RunNotInterrupted { status } => Self::new(
                ErrorKind::InvalidInput,
                format!("only interrupted agent runs can be continued (status: {status})"),
            ),
            AgentRunError::Request(inner) => Self::from(inner),
            AgentRunError::Attachment(inner) => Self::from(inner),
            AgentRunError::RunNotPersisted => {
                Self::new(ErrorKind::Database, "the agent run could not be persisted")
            }
            AgentRunError::ThreadSpawn(_) => {
                Self::new(ErrorKind::Request, "the agent run could not be started")
            }
            AgentRunError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<PlanDecisionError> for CommandError {
    fn from(err: PlanDecisionError) -> Self {
        match err {
            PlanDecisionError::PlanNotFound => Self::new(
                ErrorKind::NotFound,
                "the requested plan record does not exist",
            ),
            PlanDecisionError::PlanIncomplete => Self::new(
                ErrorKind::InvalidInput,
                "the plan run has not produced a completed plan yet",
            ),
            PlanDecisionError::AlreadyDecided(status) => Self::new(
                ErrorKind::InvalidInput,
                format!("the plan decision has already been recorded ({status})"),
            ),
            PlanDecisionError::InvalidDecision => Self::new(
                ErrorKind::InvalidInput,
                "invalid plan decision; expected accepted, rejected, or custom_reply",
            ),
            PlanDecisionError::EmptyCustomReply => Self::new(
                ErrorKind::InvalidInput,
                "a non-empty custom reply is required",
            ),
            PlanDecisionError::MissingLaunchTarget => Self::new(
                ErrorKind::InvalidInput,
                "a provider and model are required to start implementation",
            ),
            PlanDecisionError::StartRun(inner) => Self::from(inner),
            PlanDecisionError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<DebtError> for CommandError {
    fn from(err: DebtError) -> Self {
        match err {
            DebtError::ItemNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("no debt item with id {id}"))
            }
            DebtError::InvalidInput { message } => Self::new(ErrorKind::InvalidInput, message),
            // The workspace path may contain a user name, so even the
            // fixed text names no path (same mapping as `repo_audit`).
            DebtError::Audit(RepoAuditError::InvalidRoot) => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for audit",
            ),
            DebtError::Audit(RepoAuditError::Io) => Self::new(
                ErrorKind::Io,
                "the repository audit could not read the workspace",
            ),
            DebtError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<crate::application::flags::FlagError> for CommandError {
    fn from(err: crate::application::flags::FlagError) -> Self {
        match err {
            crate::application::flags::FlagError::InvalidDocument { .. } => Self::new(
                ErrorKind::InvalidData,
                "the workspace flags file is invalid",
            ),
            crate::application::flags::FlagError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<ContextError> for CommandError {
    fn from(error: ContextError) -> Self {
        let kind = match error {
            ContextError::Settings => ErrorKind::Database,
            ContextError::ReadUnavailable | ContextError::NoDiff => ErrorKind::Io,
            _ => ErrorKind::InvalidInput,
        };
        Self::new(kind, error.to_string())
    }
}

impl From<ReleaseError> for CommandError {
    fn from(err: ReleaseError) -> Self {
        match err {
            // The workspace path may contain a user name, so even the
            // fixed text names no path (same mapping as `repo_audit`).
            ReleaseError::InvalidRoot => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for release",
            ),
            ReleaseError::Io => Self::new(
                ErrorKind::Io,
                "the release status could not read the workspace",
            ),
            // The curated database text names no SQL and no stored value;
            // the raw detail stays in the server log only.
            ReleaseError::Database(inner) => Self::from(inner),
            // Fixed validation vocabulary that never echoes the rejected
            // value; the destructive-action confirmation pattern for the
            // unconfirmed call (like the terminal and the refactor apply).
            ReleaseError::InvalidInput { message } => Self::new(ErrorKind::InvalidInput, message),
            ReleaseError::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before filing a GitHub issue",
            ),
            // The `gh` CLI surface: honest, actionable, fixed vocabulary —
            // no credential material, no path, no CLI stderr text.
            ReleaseError::GhMissing => Self::new(
                ErrorKind::Io,
                "the GitHub CLI (gh) is not installed or not on PATH",
            ),
            ReleaseError::GhNotSignedIn => Self::new(
                ErrorKind::Request,
                "the GitHub CLI is not signed in — run `gh auth login` in a terminal",
            ),
            ReleaseError::NoGitHubRepo => Self::new(
                ErrorKind::InvalidInput,
                "the workspace is not filed under a github.com repository",
            ),
            ReleaseError::GhFailed => {
                Self::new(ErrorKind::Request, "the GitHub issue could not be created")
            }
        }
    }
}

impl From<TaskError> for CommandError {
    fn from(err: TaskError) -> Self {
        match err {
            TaskError::TaskNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("no task with id {id}"))
            }
            TaskError::AlreadyRunning { task_id } => Self::new(
                ErrorKind::InvalidInput,
                format!("task {task_id} already has an active run"),
            ),
            TaskError::InvalidInput { message } | TaskError::InvalidLock { message } => {
                Self::new(ErrorKind::InvalidInput, message)
            }
            TaskError::RunStart { message } => Self::new(ErrorKind::Request, message),
            TaskError::LockHeld {
                task_id,
                holder,
                expires_at,
            } => Self::new(
                ErrorKind::Conflict,
                format!("task {task_id} is locked by {holder} until {expires_at}"),
            ),
            TaskError::Database(inner) => Self::from(inner),
        }
    }
}

impl From<PtyError> for CommandError {
    fn from(err: PtyError) -> Self {
        match err {
            PtyError::InvalidWorkingDirectory => Self::new(
                ErrorKind::InvalidInput,
                "the workspace directory is not available for the terminal",
            ),
            PtyError::SpawnFailed => Self::new(
                ErrorKind::Io,
                "the interactive terminal session could not be started",
            ),
            PtyError::SessionNotFound => Self::new(
                ErrorKind::NotFound,
                "the terminal session is no longer active",
            ),
            PtyError::WriteFailed => Self::new(
                ErrorKind::Io,
                "could not write input to the terminal session",
            ),
            PtyError::ResizeFailed => {
                Self::new(ErrorKind::Io, "could not resize the terminal session")
            }
            PtyError::InternalLock => {
                Self::new(ErrorKind::Io, "the terminal session state is unavailable")
            }
        }
    }
}

impl From<crate::application::terminal::TerminalError> for CommandError {
    fn from(err: crate::application::terminal::TerminalError) -> Self {
        use crate::application::terminal::TerminalError as Source;
        match err {
            Source::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before a terminal command can run",
            ),
            Source::InvalidInput(message) => Self::new(ErrorKind::InvalidInput, message),
            // The service message is already fixed vocabulary (the tool
            // path's detail is classified away, never echoed).
            Source::Execution(message) => Self::new(ErrorKind::Io, message),
            Source::Timeout => Self::new(
                ErrorKind::Io,
                "the terminal command timed out before it completed",
            ),
            Source::Cancelled => Self::new(ErrorKind::Io, "the terminal command was stopped"),
            Source::AlreadyRunning => Self::new(
                ErrorKind::InvalidInput,
                "a terminal command is already running — stop it first",
            ),
        }
    }
}

impl From<crate::application::terminal::ExplainError> for CommandError {
    fn from(err: crate::application::terminal::ExplainError) -> Self {
        use crate::application::terminal::ExplainError as Source;
        match err {
            // Both sides stay secret-free: the failed output is never echoed.
            Source::InvalidInput => Self::new(
                ErrorKind::InvalidInput,
                "the failed output to explain is invalid",
            ),
            Source::Request(inner) => Self::from(inner),
        }
    }
}

impl From<CommitExplainError> for CommandError {
    fn from(err: CommitExplainError) -> Self {
        match err {
            // Both sides stay secret-free: the commit diff is never echoed.
            CommitExplainError::InvalidInput => Self::new(
                ErrorKind::InvalidInput,
                "the commit diff to explain is invalid",
            ),
            CommitExplainError::Request(inner) => Self::from(inner),
        }
    }
}
