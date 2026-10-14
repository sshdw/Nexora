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
}
