//! Application import services, split by domain (AC-207, review §3.2): two
//! unrelated responsibilities that shared one file now have one file each.
//!
//! - [`conversations`] — conversation JSON import (FR-011), including the
//!   SEC-01 backend-side document read.
//! - [`setup`] — VS Code settings, MCP servers, and portable setup documents
//!   (WS-E.2).
//!
//! The plumbing shared by both halves lives here: the classified
//! [`ImportError`], the [`Result`] alias, the document size cap
//! ([`MAX_IMPORT_JSON_BYTES`]), and the shared decode/validate helpers. The
//! re-exports below keep every `crate::application::import::…` path stable
//! for the command layer.

mod conversations;
mod setup;

// The re-exports keep every original single-file path
// (`crate::application::import::<item>`) valid for the command layer.
// `unused_imports` is allowed because reachability — not a current consumer —
// is the point, mirroring the `#![allow(dead_code)]` rationale in
// `application/mod.rs`.
#[allow(unused_imports)]
pub(crate) use conversations::{
    read_import_document, ImportConversation, ImportDocument, ImportMessage, ImportService,
    IMPORT_FUTURE_SKEW_SECS,
};
#[allow(unused_imports)]
pub(crate) use setup::{
    is_importable_setting, DeniedEntry, ImportedEntry, McpServerEntry, McpServerList,
    SetupImportReport, SetupImportService, DENY_INVALID_RECORD, DENY_SECRET_VALUE,
    DENY_UNSUPPORTED_VALUE, MAX_MCP_SERVERS, MCP_SERVERS_KEY, SETTINGS_VALUE_MAX_LEN,
};

use crate::infrastructure::database::DatabaseError;

/// Hard cap on an import document (bytes). Pinned from the bounded-read
/// family: the 64 KiB [`MAX_ERROR_BODY_BYTES`](crate::infrastructure::providers::transport::MAX_ERROR_BODY_BYTES)
/// sniff precedent for hostile input, scaled to a document budget — the full
/// bounded input is secrets-scanned, so the scan cost stays bounded too.
pub(crate) const MAX_IMPORT_JSON_BYTES: usize = 16 * 1024 * 1024;

/// Application-layer result shared by import operations, unifying
/// validation and persistence failures.
pub(crate) type Result<T> = std::result::Result<T, ImportError>;

/// Return [`Ok`] when `condition` is true, otherwise an
/// [`ImportError::InvalidData`] carrying `reason`.
fn ensure_valid(condition: bool, reason: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(ImportError::InvalidData(reason.into()))
    }
}

/// Distinguish malformed JSON from well-formed JSON with invalid structure.
/// JSON syntax/EOF errors are [`ImportError::InvalidJson`]; missing or
/// wrongly-typed fields are [`ImportError::InvalidData`] with fixed
/// vocabulary (NEX-SEC-011: the `serde_json` detail can quote document
/// content, so it is never carried into the reason).
fn classify_error(err: serde_json::Error) -> ImportError {
    match err.classify() {
        serde_json::error::Category::Data => ImportError::InvalidData(
            "import document has an invalid field type or structure".to_string(),
        ),
        _ => ImportError::InvalidJson(err),
    }
}

/// Classified errors raised by conversation import (FR-011).
///
/// No variant carries a credential or other secret value, so formatting an
/// [`ImportError`] never writes a secret to the logs (ARCHITECTURE.md §9,
/// §11).
#[derive(Debug)]
pub(crate) enum ImportError {
    /// The input is not valid JSON.
    InvalidJson(serde_json::Error),
    /// The document's `format` value is not the Phase 8.1 format
    /// ([`EXPORT_FORMAT`](crate::application::export::EXPORT_FORMAT)).
    UnsupportedFormat {
        /// The `format` value found in the document.
        format: String,
    },
    /// The document's `version` is not supported
    /// ([`EXPORT_VERSION`](crate::application::export::EXPORT_VERSION)).
    UnsupportedVersion {
        /// The `version` value found in the document.
        version: i64,
    },
    /// The document violates the Phase 8.1 format or the schema constraints.
    InvalidData(String),
    /// A persistence or transaction failure from a repository.
    Database(DatabaseError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // NEX-SEC-011: fixed vocabulary only — the `serde_json` detail
            // and the document's `format` value can quote document content,
            // so neither is rendered here (the typed variants still carry
            // them for programmatic matching in tests).
            Self::InvalidJson(_) => write!(f, "invalid JSON document"),
            Self::UnsupportedFormat { .. } => {
                write!(f, "unsupported import format")
            }
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported import version {version}")
            }
            Self::InvalidData(reason) => write!(f, "invalid import document: {reason}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(err) => Some(err),
            Self::Database(err) => Some(err),
            Self::UnsupportedFormat { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidData(_) => None,
        }
    }
}

impl From<DatabaseError> for ImportError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}
