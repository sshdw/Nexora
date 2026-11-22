//! Command error boundary: converts application-layer classified errors into
//! safe, serializable, secret-free responses for the frontend (Phase 10.2 —
//! Tauri Command Layer).
//!
//! Every Tauri command returns [`Result<T, CommandError>`]. The error carries a
//! stable machine-readable [`ErrorKind`] and a human-readable `message`, and is
//! the single translation point from the application/infrastructure error
//! types. It deliberately never exposes a credential, raw SQL, a file path, or
//! a conversation/prompt payload (ARCHITECTURE.md §9, §11; DATABASE.md §14).
//!
//! # Shared taxonomy (issue #160)
//!
//! [`ErrorKind`] and [`CommandError`] now live in the Tauri-free
//! `nexora-core` crate (`crates/nexora-core/src/error.rs`) so the CLI can
//! produce the identical taxonomy and exit codes without linking the window
//! application. They are re-exported here under their original names, so every
//! existing `use super::error::{CommandError, ErrorKind}` and every frontend
//! payload stay byte-identical.
//!
//! What stayed here is exactly the business logic: the `From` impls that map
//! this app's classified errors into the taxonomy. The taxonomy and its
//! exit-code table moved; the translation did not. One mapping left with its
//! types: `CredentialError` moved to `nexora_core::provider` with the
//! execution seam (NEX-ARCH-002), so its `From` impl for the taxonomy moved
//! into `nexora-core` too (an orphan-rules `From` impl for two core types
//! cannot live here) — the curated, secret-free messages are unchanged.

// Only these two application error types are still referenced by the module's
// unit tests after the `From` impls moved to `nexora_runtime`.
#[cfg(test)]
use crate::application::execution::RequestError;
#[cfg(test)]
use crate::infrastructure::providers::chatgpt_oauth::ChatGptOAuthError;

/// Stable, secret-free error categories the frontend can branch on.
///
/// Shared with the CLI through `nexora-core` (issue #160) so both surfaces
/// classify a failure identically. Re-exported under its original name: the
/// serialized shape and every call site are unchanged.
pub(crate) use nexora_core::error::CoreErrorKind as ErrorKind;

/// Serializable, secret-free command error returned to the frontend.
///
/// Shared with the CLI through `nexora-core` (issue #160). The `From` impls
/// below remain here — mapping this application's classified errors into the
/// taxonomy is business logic and did not move.
pub(crate) use nexora_core::error::CoreError as CommandError;

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

// [error mapping moved to nexora_runtime::command_error_mapping]

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::ExecutorError;

    /// A provider call cancelled in flight reaches the frontend through the
    /// classified chain (`ExecutorError::Cancelled` → `RequestError::Execution`
    /// → [`CommandError`]): the mapping is total over the error taxonomy, the
    /// kind stays `Request`, and the message is the fixed category text —
    /// never a credential, payload, or body fragment.
    #[test]
    fn cancelled_execution_maps_to_secret_free_request_error() {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        let err = CommandError::from(RequestError::Execution {
            name: "openai".into(),
            message: ExecutorError::Cancelled.to_string(),
        });
        assert_eq!(err.kind, ErrorKind::Request);
        assert_eq!(
            err.message,
            "the AI provider 'openai' failed: \
             the AI provider request was cancelled before it completed"
        );
        for sentinel in SECRET_SENTINELS {
            assert!(
                !err.message.to_lowercase().contains(sentinel),
                "cancelled mapping must stay secret-free, found {sentinel:?}"
            );
        }
    }

    /// Raw `SQLite` failures stay secret-free across the command boundary
    /// (NEX-SEC-011): the frontend text is fixed, and the failure carries no
    /// SQL or stored value even when the rusqlite detail quotes both.
    #[test]
    fn database_error_mapping_is_secret_free_and_classified() {
        const SENTINELS: [&str; 4] = ["sk-", "secret", "select", "credential"];
        let raw = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(1),
            Some("SELECT secret FROM users WHERE api_key='sk-live-sentinel-1'".to_string()),
        );
        let err = CommandError::from(crate::infrastructure::database::DatabaseError::Sqlite(raw));
        assert_eq!(err.kind, ErrorKind::Database);
        assert_eq!(
            err.message,
            "a database operation failed; no data was changed"
        );
        for sentinel in SENTINELS {
            assert!(
                !err.message.to_lowercase().contains(sentinel),
                "database mapping must stay secret-free, found {sentinel:?}"
            );
        }
        // The typed lock and gap errors map through the same fixed text.
        for typed in [
            crate::infrastructure::database::DatabaseError::Lock("boom".to_string()),
            crate::infrastructure::database::DatabaseError::SchemaGap { missing: 6 },
        ] {
            let err = CommandError::from(typed);
            assert_eq!(err.kind, ErrorKind::Database);
            assert_eq!(
                err.message,
                "a database operation failed; no data was changed"
            );
        }
    }

    #[test]
    fn chatgpt_oauth_error_mapping_is_classified_and_secret_free() {
        let keyring_err = CommandError::from(ChatGptOAuthError::KeyringUnavailable);
        assert_eq!(keyring_err.kind, ErrorKind::Credential);

        let endpoint_err = CommandError::from(ChatGptOAuthError::InvalidEndpoint);
        assert_eq!(endpoint_err.kind, ErrorKind::InvalidInput);

        for req_err in [
            ChatGptOAuthError::StateMismatch,
            ChatGptOAuthError::CallbackTimeout,
            ChatGptOAuthError::TokenExchangeFailed,
            ChatGptOAuthError::TokenRefreshFailed,
            ChatGptOAuthError::TokenRefreshTransient,
            ChatGptOAuthError::ReauthorizationRequired,
        ] {
            let mapped = CommandError::from(req_err);
            assert_eq!(mapped.kind, ErrorKind::Request);
            let lower = mapped.message.to_lowercase();
            assert!(!lower.contains("sk-"));
            assert!(!lower.contains("bearer"));
        }
    }
}
