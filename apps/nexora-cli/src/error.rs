//! Secret-free CLI error carrier and output discipline.
//!
//! Every diagnostic this binary prints goes through [`CliError::report`],
//! which writes one prefixed line to **stderr**. Text output (the default for
//! `providers`/`models`/`config`) goes to stdout and carries no ANSI escapes.
//! Messages are curated here: a caller-supplied key or value is never echoed
//! verbatim. [`safe_key_label`] additionally refuses to print anything that
//! looks like credential material, so a hostile `config set` cannot smuggle
//! key bytes into stderr (brief §11, §15; issue #162 redaction acceptance).

use nexora_core::error::{CoreErrorKind, ExitCode};

/// A classified, secret-free CLI failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliError {
    /// Stable taxonomy category from the shared core.
    pub(crate) kind: CoreErrorKind,
    /// Curated message — never raw user input or a driver error.
    pub(crate) message: String,
}

impl CliError {
    /// A caller/validation error (exit `2` in the shared table).
    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self {
            kind: CoreErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    /// A `SQLite` failure, with a fixed message that cannot quote a path.
    pub(crate) fn database() -> Self {
        Self {
            kind: CoreErrorKind::Database,
            message: "a database operation failed; no setting was changed".to_string(),
        }
    }

    /// An OS keyring failure, with no key material or raw error text.
    ///
    /// Reserved for the hidden-input credential setup flow (a later phase):
    /// the phase-3 read-only surface only probes credential *presence* and
    /// reports `unknown` when the keyring is unreachable, so it never surfaces
    /// this error today. Kept here so the shared credential error category
    /// (exit `3`) has exactly one CLI carrier.
    #[allow(dead_code)]
    pub(crate) fn credential() -> Self {
        Self {
            kind: CoreErrorKind::Credential,
            message: "the OS credential store could not be reached; no secret was read or written"
                .to_string(),
        }
    }

    /// The numeric process exit status, from the one shared table.
    #[must_use]
    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::for_kind(self.kind)
    }

    /// Print the diagnostic to stderr. Never prints a value or a raw OS error.
    pub(crate) fn report(&self) {
        eprintln!("nexora: {}", self.message);
    }
}

/// Markers that identify credential material in a caller-supplied string.
const SECRET_MARKERS: [&str; 9] = [
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "AKIA",
    "xoxb-",
    "xoxp-",
    "BEGIN PRIVATE KEY",
];

/// Return a label safe to echo in a diagnostic.
///
/// A normal dotted setting key is printed verbatim so the rejection is
/// actionable. Anything containing a secret marker, whitespace, control
/// character, or unusual punctuation becomes `[redacted]` — the diagnostic
/// stays loud without reproducing hostile argv bytes.
#[must_use]
pub(crate) fn safe_key_label(key: &str) -> String {
    let looks_secret = SECRET_MARKERS.iter().any(|marker| key.contains(marker));
    let printable = !key.is_empty()
        && key.len() <= 200
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if looks_secret || !printable {
        "[redacted]".to_string()
    } else {
        key.to_string()
    }
}

/// Convert a shared config rejection into a CLI diagnostic.
///
/// The shared gate's `Rejection::message()` echoes the rejected *value* for
/// out-of-domain writes (that is the Desktop wording). The CLI deliberately
/// does not: a value can look like a secret even under a harmless key, so the
/// CLI surfaces the category and the safe key label only.
#[must_use]
pub(crate) fn from_rejection(rejection: &nexora_core::config::Rejection) -> CliError {
    use nexora_core::config::RejectionReason;

    match &rejection.reason {
        RejectionReason::UnknownKey { key, .. } => CliError::validation(format!(
            "setting key '{}' is not supported (unknown or secret-adjacent)",
            safe_key_label(key)
        )),
        RejectionReason::InvalidValue { key, .. } => CliError::validation(format!(
            "value for '{}' is not valid; check the allowed values in `nexora config --help`",
            safe_key_label(key)
        )),
        RejectionReason::InvalidMcpServers { key } => CliError::validation(format!(
            "value for '{}' is not a valid MCP server list",
            safe_key_label(key)
        )),
    }
}

/// Build a rejection for a key the CLI refuses before it reaches the shared
/// gate (for example `config get` on a key with no readable value domain).
#[must_use]
pub(crate) fn unknown_key(key: &str) -> CliError {
    CliError::validation(format!(
        "setting key '{}' is not supported (unknown or secret-adjacent)",
        safe_key_label(key)
    ))
}

/// Sanitize a config surface before rendering it in `config get`.
///
/// Values under allow-listed keys are non-secret by construction. This pass is
/// defence-in-depth: if a future key ever holds key-like material, the CLI
/// still never prints the bytes.
#[must_use]
pub(crate) fn redact_value(value: &str) -> String {
    if SECRET_MARKERS.iter().any(|marker| value.contains(marker)) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}
