//! Stable, interface-free error taxonomy + exit-code mapping.
//!
//! This module is the *single source of truth* for "what kind of failure
//! happened" and "what exit code does that kind produce". It was extracted
//! verbatim from the desktop command-error boundary
//! (`src-tauri/src/commands/error.rs`, issue #160) so that Desktop and CLI
//! cannot grow two divergent error taxonomies (brief §2.3: one shared core,
//! adapters on top).
//!
//! ## What lives here, and what deliberately does not
//!
//! - [`CoreErrorKind`] — the stable, machine-readable taxonomy. Every
//!   consumer (the Tauri IPC boundary, the CLI exit code, future TUI event
//!   rendering) branches on this one enum.
//! - [`CoreError`] — the secret-free `{kind, message}` carrier. It is
//!   deliberately *data only*: how a given application error maps into it
//!   stays in the adapter that owns that error type, so no business logic
//!   moved.
//! - [`ExitCode`] — the versioned exit-code table (brief §9.4).
//!
//! ## Secret hygiene (non-negotiable)
//!
//! This taxonomy is safe to render in a terminal, serialize to a CLI JSON
//! payload, or forward over Tauri IPC **only because of how it is
//! constructed**, not because of anything enforced here:
//!
//! - a `message` is curated text, never a raw `Display` of a driver error;
//! - no credential, authorization header, raw SQL, bound parameter, stored
//!   prompt/message payload, or filesystem path may appear in a `message`
//!   (ARCHITECTURE.md §9, §11; NEX-SEC-011);
//! - the enum carries no payload at all, so it cannot smuggle data even by
//!   accident;
//! - `Display` for [`CoreError`] is the fixed `"{kind:?}: {message}"` shape.
//!
//! [`crate::config`] extends the same rule to config values; `doctor` and the
//! CLI exit path rely on both.

use std::fmt::Display;

/// Version of the exit-code table (brief §9.4).
///
/// Frozen at `1`. The numeric values of [`ExitCode`] are a *public contract*:
/// scripts branch on them. Growing the taxonomy may append new variants, but
/// an existing variant must keep its number for the lifetime of the major CLI
/// version. Any change to a number requires bumping this constant and a
/// CHANGELOG entry.
pub const EXIT_CODE_TABLE_VERSION: u32 = 1;

/// Stable, secret-free error categories every surface can branch on.
///
/// Mirrors the desktop's pre-extraction `ErrorKind` one-for-one, including the
/// `camelCase` wire names, so the serialized IPC payload is byte-identical to
/// what Desktop has always sent (zero behavior delta on extraction).
///
/// The variants carry no data by construction: a category is a label, never a
/// carrier for a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoreErrorKind {
    /// The referenced entity does not exist.
    NotFound,
    /// A caller-supplied value was rejected (length, empty, ...).
    InvalidInput,
    /// A persistence / `SQLite` operation failed.
    Database,
    /// The OS secure keyring could not satisfy a credential operation.
    Credential,
    /// AI request execution failed (unknown/unavailable provider, missing
    /// credentials, provider failure, ...).
    Request,
    /// A destructive operation was invoked without a valid server-side
    /// confirmation.
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
    /// The request conflicts with state held elsewhere (an edit lock owned
    /// by another surface).
    Conflict,
}

impl CoreErrorKind {
    /// Every variant, in declaration order.
    ///
    /// Used by the table self-check so a newly added variant cannot be
    /// forgotten by [`ExitCode::for_kind`].
    pub const ALL: [Self; 13] = [
        Self::NotFound,
        Self::InvalidInput,
        Self::Database,
        Self::Credential,
        Self::Request,
        Self::ConfirmationRequired,
        Self::Serialization,
        Self::InvalidJson,
        Self::UnsupportedFormat,
        Self::UnsupportedVersion,
        Self::InvalidData,
        Self::Io,
        Self::Conflict,
    ];
}

/// Serializable, secret-free error value returned across every Nexora surface.
///
/// `message` is curated, fixed-vocabulary text. It never contains a
/// credential, raw SQL, a bound value, a stored prompt/message payload, or a
/// filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreError {
    /// Stable machine-readable category, safe to surface and branch on.
    pub kind: CoreErrorKind,
    /// Human-readable message. Never contains a credential, raw SQL, or a
    /// stored payload (prompt / message content, file path).
    pub message: String,
}

impl CoreError {
    /// Build a classified error with a curated `message`.
    pub fn new(kind: CoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The exit code this error produces (brief §9.4).
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        ExitCode::for_kind(self.kind)
    }
}

impl Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CoreError {}

impl From<crate::provider::CredentialError> for CoreError {
    /// Map the provider credential-store failure categories into the shared
    /// taxonomy. The bridge lives here (not in an adapter) because both sides
    /// of the conversion are core types: `CredentialError` moved to
    /// [`crate::provider`] with the execution seam (NEX-ARCH-002), and an
    /// orphan-rules `From` impl for two foreign types cannot exist in the
    /// adapters. The messages are the same curated, secret-free texts the
    /// desktop adapter used before the move.
    fn from(err: crate::provider::CredentialError) -> Self {
        match err {
            crate::provider::CredentialError::StorageUnavailable => Self::new(
                CoreErrorKind::Credential,
                "the provider credential store is unavailable (OS secure keyring)",
            ),
            crate::provider::CredentialError::Invalid => Self::new(
                CoreErrorKind::Credential,
                "the provider credential could not be read or written in the OS secure keyring",
            ),
        }
    }
}

/// Process exit codes (brief §9.4).
///
/// Documented codes instead of a bare `1` for every outcome. The numeric
/// values are frozen by [`EXIT_CODE_TABLE_VERSION`]: automation branches on
/// them, so a number never changes once published — a new failure mode gets a
/// *new* number.
///
/// | Code | Variant | Meaning |
/// |---|---|---|
/// | 0 | `Ok` | success |
/// | 1 | `Internal` | unexpected/internal failure |
/// | 2 | `Usage` | user or validation error (bad flag, bad config value) |
/// | 3 | `NotFound` | the referenced entity does not exist |
/// | 4 | `Conflict` | state held elsewhere (edit lock, concurrent owner) |
/// | 5 | `Database` | persistence / `SQLite` failure |
/// | 6 | `Credential` | OS keyring unavailable, or the credential is invalid |
/// | 7 | `Request` | provider / network / auth failure talking to a provider |
/// | 8 | `ApprovalRequired` | destructive action needs confirmation, or it was denied |
/// | 9 | `Serialization` | document could not be encoded or decoded |
/// | 10 | `Io` | filesystem I/O failure |
/// | 11 | `Budget` | reserved: step-budget / spend-limit exhaustion |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ExitCode {
    /// Successful completion.
    Ok = 0,
    /// Unexpected or internal failure.
    Internal = 1,
    /// User / validation error.
    Usage = 2,
    /// The referenced entity does not exist.
    NotFound = 3,
    /// Conflicting state owned elsewhere.
    Conflict = 4,
    /// Persistence failure.
    Database = 5,
    /// Keyring unavailable, or the credential is invalid.
    Credential = 6,
    /// Provider / network / auth failure.
    Request = 7,
    /// Confirmation required, or the action was denied.
    ApprovalRequired = 8,
    /// Serialization failure.
    Serialization = 9,
    /// Filesystem I/O failure.
    Io = 10,
    /// Reserved for step-budget / spend-limit exhaustion.
    ///
    /// Not reachable from [`ExitCode::for_kind`] yet: budget exhaustion is an
    /// *agent-runner* outcome, and no application error maps to it in this
    /// phase. The slot is reserved so the later agent slice (brief §3.1)
    /// binds a number without renumbering the published table.
    Budget = 11,
}

impl ExitCode {
    /// The numeric value handed to the OS on process exit.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Map a taxonomy category onto its published exit code.
    ///
    /// Total by construction: every [`CoreErrorKind`] has a code, and the
    /// table self-check test fails if a new variant is ever added without one.
    #[must_use]
    pub const fn for_kind(kind: CoreErrorKind) -> Self {
        match kind {
            CoreErrorKind::NotFound => Self::NotFound,
            CoreErrorKind::InvalidInput
            | CoreErrorKind::InvalidJson
            | CoreErrorKind::UnsupportedFormat
            | CoreErrorKind::UnsupportedVersion
            | CoreErrorKind::InvalidData => Self::Usage,
            CoreErrorKind::Database => Self::Database,
            CoreErrorKind::Credential => Self::Credential,
            CoreErrorKind::Request => Self::Request,
            CoreErrorKind::ConfirmationRequired => Self::ApprovalRequired,
            CoreErrorKind::Serialization => Self::Serialization,
            CoreErrorKind::Io => Self::Io,
            CoreErrorKind::Conflict => Self::Conflict,
        }
    }
}

impl Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

// ---------------------------------------------------------------------------
// CLI process exit contract (brief §9.4, NEXORA-CLI-PLAN.md §5)
// ---------------------------------------------------------------------------

/// Version of the CLI process exit-code table.
///
/// This is a *separate*, stable contract from [`EXIT_CODE_TABLE_VERSION`] and
/// [`ExitCode`]. The latter is the shared error-category table (0–11) used by
/// the desktop IPC boundary. The brief §9.4 / plan §5 exit codes are the
/// numbers a shell sees when the `nexora` binary exits (0–5); scripts branch on
/// them, so they are frozen here and never renumbered. Two tables, two
/// versions, on purpose: changing one must not silently rewrite the other.
pub const CLI_EXIT_CODE_TABLE_VERSION: u32 = 1;

/// Process exit codes of the `nexora` CLI.
///
/// | Code | Variant | Meaning |
/// |---|---|---|
/// | 0 | `Ok` | success |
/// | 1 | `Internal` | unexpected/internal failure (DB, config, I/O, ...) |
/// | 2 | `Usage` | user / validation error (bad flag, bad provider/model) |
/// | 3 | `Provider` | provider / network / auth failure |
/// | 4 | `Cancelled` | approval denied or run cancelled |
/// | 5 | `Budget` | step-budget / spend-limit exhausted |
///
/// Every CLI process exit — including commands added before this table — goes
/// through this enum. In particular a DB/config failure maps to `Internal`
/// (`1`), never to `Budget` (`5`): the two concepts are unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum CliExitCode {
    /// Successful completion.
    Ok = 0,
    /// Unexpected or internal failure (persistence, config, serialization, I/O).
    Internal = 1,
    /// User / validation error (bad flag, bad provider or model).
    Usage = 2,
    /// Provider / network / credential failure.
    Provider = 3,
    /// Approval denied, or the run was cancelled before completion.
    Cancelled = 4,
    /// Step budget or spend limit exhausted.
    Budget = 5,
}

impl CliExitCode {
    /// The numeric value handed to the OS on process exit.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Map a shared error category onto the CLI exit contract.
    ///
    /// Total by construction over [`CoreErrorKind::ALL`]. Validation-shaped
    /// kinds are usage (`2`); provider/credential kinds are provider (`3`);
    /// confirmation is cancellation (`4`); everything else — notably
    /// persistence/config `Database` — is internal (`1`).
    #[must_use]
    pub const fn for_kind(kind: CoreErrorKind) -> Self {
        match kind {
            CoreErrorKind::InvalidInput
            | CoreErrorKind::InvalidJson
            | CoreErrorKind::UnsupportedFormat
            | CoreErrorKind::UnsupportedVersion
            | CoreErrorKind::InvalidData => Self::Usage,
            CoreErrorKind::Credential | CoreErrorKind::Request => Self::Provider,
            CoreErrorKind::ConfirmationRequired => Self::Cancelled,
            CoreErrorKind::NotFound
            | CoreErrorKind::Database
            | CoreErrorKind::Serialization
            | CoreErrorKind::Io
            | CoreErrorKind::Conflict => Self::Internal,
        }
    }

    /// Map a concrete provider executor failure onto the CLI exit contract.
    ///
    /// A provider call cancelled in flight is user cancellation (`4`), not a
    /// provider failure. A prompt that exceeds the model context window is an
    /// internal failure (`1`) unless the runner reclassifies it as a budget
    /// outcome — this choice is pinned in `cli_exit_code_table_is_pinned`.
    #[must_use]
    pub const fn for_executor_error(error: &crate::provider::ExecutorError) -> Self {
        use crate::provider::ExecutorError;
        match error {
            ExecutorError::Cancelled => Self::Cancelled,
            ExecutorError::ContextLengthExceeded => Self::Internal,
            ExecutorError::Network
            | ExecutorError::RateLimited { .. }
            | ExecutorError::ProviderUnavailable
            | ExecutorError::PaymentRequired
            | ExecutorError::Authentication
            | ExecutorError::InvalidRequest
            | ExecutorError::UnexpectedResponse
            | ExecutorError::Failure => Self::Provider,
        }
    }
}

/// Terminal outcome of a CLI-launched run, mapped onto [`CliExitCode`].
///
/// Kept deliberately small and secret-free: it is a classification label, not
/// a carrier for step counts, spend amounts, or provider payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliRunOutcome {
    /// The run finished successfully.
    Completed,
    /// An unexpected/internal failure ended the run.
    Failed,
    /// Run history could not be persisted.
    PersistenceFailure,
    /// A provider call failed.
    Provider,
    /// The context window was exhausted after recovery attempts.
    ContextExhausted,
    /// The user cancelled the run.
    Cancelled,
    /// An approval was denied in non-interactive mode.
    ApprovalDenied,
    /// The step budget was exhausted.
    BudgetExhausted,
    /// The spend limit was exceeded.
    SpendLimitExceeded,
}

impl CliExitCode {
    /// Map a run's terminal outcome onto the CLI exit contract.
    #[must_use]
    pub const fn for_run_outcome(outcome: CliRunOutcome) -> Self {
        match outcome {
            CliRunOutcome::Completed => Self::Ok,
            CliRunOutcome::Failed
            | CliRunOutcome::PersistenceFailure
            | CliRunOutcome::ContextExhausted => Self::Internal,
            CliRunOutcome::Provider => Self::Provider,
            CliRunOutcome::Cancelled | CliRunOutcome::ApprovalDenied => Self::Cancelled,
            CliRunOutcome::BudgetExhausted | CliRunOutcome::SpendLimitExceeded => Self::Budget,
        }
    }

    /// Map an approval outcome onto the CLI exit contract.
    ///
    /// In non-interactive mode an approval request is never auto-granted: a
    /// denial or a cancel both resolve to `4`, with an explicit how-to-repeat
    /// hint printed by the CLI adapter.
    #[must_use]
    pub const fn for_approval(approved: bool) -> Self {
        if approved {
            Self::Ok
        } else {
            Self::Cancelled
        }
    }
}

impl Display for CliExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_maps_to_a_code() {
        for kind in CoreErrorKind::ALL {
            // Exhaustive today; the match in `for_kind` is the compiler-level
            // guard, and this asserts the mapping is never `Internal`.
            assert_ne!(
                ExitCode::for_kind(kind),
                ExitCode::Internal,
                "{kind:?} must not fall back to the generic internal code"
            );
        }
    }

    #[test]
    fn kind_to_exit_code_table_is_pinned() {
        // The published contract. Changing a number here is a breaking change
        // for scripts and MUST accompany an EXIT_CODE_TABLE_VERSION bump.
        let expected = [
            (CoreErrorKind::NotFound, 3),
            (CoreErrorKind::InvalidInput, 2),
            (CoreErrorKind::Database, 5),
            (CoreErrorKind::Credential, 6),
            (CoreErrorKind::Request, 7),
            (CoreErrorKind::ConfirmationRequired, 8),
            (CoreErrorKind::Serialization, 9),
            (CoreErrorKind::InvalidJson, 2),
            (CoreErrorKind::UnsupportedFormat, 2),
            (CoreErrorKind::UnsupportedVersion, 2),
            (CoreErrorKind::InvalidData, 2),
            (CoreErrorKind::Io, 10),
            (CoreErrorKind::Conflict, 4),
        ];
        for (kind, code) in expected {
            assert_eq!(
                ExitCode::for_kind(kind).code(),
                code,
                "exit code for {kind:?} changed"
            );
        }
    }

    #[test]
    fn exit_code_numbers_are_unique_and_stable() {
        // No two categories may collapse onto one code except through an
        // intentional grouping; the grouping below is the documented one.
        assert_eq!(EXIT_CODE_TABLE_VERSION, 1);
        assert_eq!(ExitCode::Ok.code(), 0);
        assert_eq!(ExitCode::Internal.code(), 1);
        assert_eq!(ExitCode::Usage.code(), 2);
        // `Budget` is reserved and currently unmapped: it must stay distinct
        // from every mapped code so the later agent slice cannot silently
        // collide with an existing outcome.
        assert_eq!(ExitCode::Budget.code(), 11);
        assert!(CoreErrorKind::ALL
            .iter()
            .all(|kind| ExitCode::for_kind(*kind) != ExitCode::Budget));
    }

    #[test]
    fn error_display_is_the_pinned_shape() {
        let err = CoreError::new(
            CoreErrorKind::InvalidInput,
            "value 'x' is not a valid 'appearance.theme' setting",
        );
        assert_eq!(
            err.to_string(),
            "InvalidInput: value 'x' is not a valid 'appearance.theme' setting"
        );
        assert_eq!(err.exit_code(), ExitCode::Usage);
    }

    #[test]
    fn wire_names_are_camel_case_and_match_the_desktop_payload() {
        // Desktop's frontend branches on these exact strings; the extraction
        // must not rename a single one.
        let expected = [
            (CoreErrorKind::NotFound, "notFound"),
            (CoreErrorKind::InvalidInput, "invalidInput"),
            (CoreErrorKind::Database, "database"),
            (CoreErrorKind::Credential, "credential"),
            (CoreErrorKind::Request, "request"),
            (CoreErrorKind::ConfirmationRequired, "confirmationRequired"),
            (CoreErrorKind::Serialization, "serialization"),
            (CoreErrorKind::InvalidJson, "invalidJson"),
            (CoreErrorKind::UnsupportedFormat, "unsupportedFormat"),
            (CoreErrorKind::UnsupportedVersion, "unsupportedVersion"),
            (CoreErrorKind::InvalidData, "invalidData"),
            (CoreErrorKind::Io, "io"),
            (CoreErrorKind::Conflict, "conflict"),
        ];
        for (kind, wire) in expected {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
        }
    }

    #[test]
    fn error_serializes_with_stable_field_names() {
        let err = CoreError::new(
            CoreErrorKind::Database,
            "a database operation failed; no data was changed",
        );
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["kind"], "database");
        assert_eq!(
            json["message"],
            "a database operation failed; no data was changed"
        );
        let round: CoreError = serde_json::from_value(json).expect("deserialize");
        assert_eq!(round, err);
    }

    #[test]
    fn error_strings_stay_secret_free() {
        // The taxonomy itself cannot smuggle data: a message is whatever the
        // adapter curated. This test pins the contract that the *shared*
        // constructors never template untrusted material, and guards the
        // documented sentinels used across the repo's secret-hygiene tests.
        let sentinels = [
            "sk-",
            "bearer ",
            "api_key",
            "api-key",
            "authorization",
            "select ",
            "insert ",
            "ghp_",
        ];
        let err = CoreError::new(CoreErrorKind::Request, "the AI provider is not configured");
        let text = err.to_string().to_lowercase();
        for sentinel in sentinels {
            assert!(
                !text.contains(sentinel),
                "shared error text must not contain {sentinel:?}: {text}"
            );
        }
    }

    // --- CLI process exit contract (brief §9.4 / plan §5) -------------------

    #[test]
    fn cli_exit_code_table_is_pinned() {
        // The published shell-facing contract. These exact numbers are what
        // automation branches on; changing one is a breaking change that MUST
        // accompany a CLI_EXIT_CODE_TABLE_VERSION bump.
        assert_eq!(CLI_EXIT_CODE_TABLE_VERSION, 1);
        assert_eq!(CliExitCode::Ok.code(), 0);
        assert_eq!(CliExitCode::Internal.code(), 1);
        assert_eq!(CliExitCode::Usage.code(), 2);
        assert_eq!(CliExitCode::Provider.code(), 3);
        assert_eq!(CliExitCode::Cancelled.code(), 4);
        assert_eq!(CliExitCode::Budget.code(), 5);

        // The two tables are independent: both happen to be at version 1, but
        // the shared error table's numbers (0–11) must not leak into the CLI
        // contract (0–5).
        assert_eq!(CLI_EXIT_CODE_TABLE_VERSION, 1);
        assert_eq!(EXIT_CODE_TABLE_VERSION, 1);
        assert_eq!(ExitCode::Database.code(), 5);
        assert_eq!(CliExitCode::for_kind(CoreErrorKind::Database).code(), 1);
        assert_ne!(ExitCode::Budget.code(), CliExitCode::Budget.code());
    }

    #[test]
    fn cli_exit_code_core_kind_mapping_is_pinned() {
        let expected = [
            (CoreErrorKind::InvalidInput, 2),
            (CoreErrorKind::InvalidJson, 2),
            (CoreErrorKind::UnsupportedFormat, 2),
            (CoreErrorKind::UnsupportedVersion, 2),
            (CoreErrorKind::InvalidData, 2),
            (CoreErrorKind::Credential, 3),
            (CoreErrorKind::Request, 3),
            (CoreErrorKind::ConfirmationRequired, 4),
            (CoreErrorKind::NotFound, 1),
            (CoreErrorKind::Database, 1),
            (CoreErrorKind::Serialization, 1),
            (CoreErrorKind::Io, 1),
            (CoreErrorKind::Conflict, 1),
        ];
        assert_eq!(expected.len(), CoreErrorKind::ALL.len());
        for (kind, code) in expected {
            assert_eq!(
                CliExitCode::for_kind(kind).code(),
                code,
                "CLI exit code for {kind:?} changed"
            );
        }
    }

    #[test]
    fn cli_exit_code_run_outcome_mapping_is_pinned() {
        let expected = [
            (CliRunOutcome::Completed, 0),
            (CliRunOutcome::Failed, 1),
            (CliRunOutcome::PersistenceFailure, 1),
            (CliRunOutcome::Provider, 3),
            (CliRunOutcome::ContextExhausted, 1),
            (CliRunOutcome::Cancelled, 4),
            (CliRunOutcome::ApprovalDenied, 4),
            (CliRunOutcome::BudgetExhausted, 5),
            (CliRunOutcome::SpendLimitExceeded, 5),
        ];
        for (outcome, code) in expected {
            assert_eq!(
                CliExitCode::for_run_outcome(outcome).code(),
                code,
                "CLI exit code for {outcome:?} changed"
            );
        }
        // Owner decision: `ContextExhausted` is internal unless core
        // classifies it as budget. It is internal here — pin that choice.
        assert_eq!(
            CliExitCode::for_run_outcome(CliRunOutcome::ContextExhausted),
            CliExitCode::Internal
        );
        assert_ne!(
            CliExitCode::for_run_outcome(CliRunOutcome::ContextExhausted),
            CliExitCode::Budget
        );
    }

    #[test]
    fn cli_exit_code_executor_error_and_approval_mapping_is_pinned() {
        use crate::provider::ExecutorError;
        assert_eq!(
            CliExitCode::for_executor_error(&ExecutorError::Authentication).code(),
            3
        );
        assert_eq!(
            CliExitCode::for_executor_error(&ExecutorError::Network).code(),
            3
        );
        assert_eq!(
            CliExitCode::for_executor_error(&ExecutorError::Cancelled).code(),
            4
        );
        assert_eq!(
            CliExitCode::for_executor_error(&ExecutorError::ContextLengthExceeded).code(),
            1
        );
        // Non-interactive never auto-approves: only an explicit grant maps to
        // success; denial and cancellation both map to 4.
        assert_eq!(CliExitCode::for_approval(true).code(), 0);
        assert_eq!(CliExitCode::for_approval(false).code(), 4);
    }
}
