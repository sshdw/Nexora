//! Tauri commands over the existing [`ExportService`] / [`ImportService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer export / import services (FR-010, FR-011)
//! and converts their classified errors into safe [`CommandError`] values.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::export::{ExportService, SetupExportService};
use crate::application::import::{ImportService, SetupImportReport, SetupImportService};
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Export one conversation to its JSON document (returns the document text).
#[tauri::command]
pub(crate) fn export_conversation(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<String, CommandError> {
    ExportService::new(db.inner())
        .serialize(conversation_id)
        .map_err(Into::into)
}

/// Export one conversation and write the document to the given `path`.
#[tauri::command]
pub(crate) fn export_conversation_to_file(
    conversation_id: i64,
    path: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ExportService::new(db.inner())
        .export_to_file(conversation_id, std::path::Path::new(&path))
        .map_err(Into::into)
}

/// Import a conversation from an existing export document; returns the new
/// conversation's `id`.
#[tauri::command]
pub(crate) fn import_conversation(
    json: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    ImportService::new(db.inner())
        .import(&json)
        .map_err(Into::into)
}

/// Import a VS Code `settings.json` document (WS-E.2); returns the per-key
/// report (`imported` / `skipped` / `denied`). Only the mappable subset
/// translates — everything else is reported as skipped, never guessed.
#[tauri::command]
pub(crate) fn import_vscode_settings(
    json: String,
    db: State<'_, Database>,
) -> Result<SetupImportReport, CommandError> {
    SetupImportService::new(db.inner())
        .import_vscode(&json)
        .map_err(Into::into)
}

/// Import an MCP servers document (`{ "mcpServers": { ... } }`, WS-E.2);
/// returns the per-server report. Validated servers are stored under the
/// `mcp.servers` setting; secret-like entries are denied per server with a
/// secret-free reason while valid servers still import.
#[tauri::command]
pub(crate) fn import_mcp_servers(
    json: String,
    db: State<'_, Database>,
) -> Result<SetupImportReport, CommandError> {
    SetupImportService::new(db.inner())
        .import_mcp(&json)
        .map_err(Into::into)
}

/// Export the current Nexora setup (settings plus routing profiles and
/// feature flags) to its portable JSON document (WS-E.2); returns the
/// document text.
#[tauri::command]
pub(crate) fn export_setup(db: State<'_, Database>) -> Result<String, CommandError> {
    SetupExportService::new(db.inner())
        .serialize_setup()
        .map_err(Into::into)
}

/// Export the current Nexora setup and write the document to the given
/// `path` (WS-E.2). The destination goes through the same JSON-only
/// allowlist and path validation as conversation exports.
#[tauri::command]
pub(crate) fn export_setup_to_file(
    path: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    SetupExportService::new(db.inner())
        .export_setup_to_file(std::path::Path::new(&path))
        .map_err(Into::into)
}

/// Import a portable Nexora setup document (WS-E.2); returns the per-key
/// report. Allowlisted keys with in-domain values are written (`null`
/// clears a key); unknown keys are skipped; out-of-domain values are denied
/// with fixed-vocabulary reasons.
#[tauri::command]
pub(crate) fn import_setup(
    json: String,
    db: State<'_, Database>,
) -> Result<SetupImportReport, CommandError> {
    SetupImportService::new(db.inner())
        .import_setup(&json)
        .map_err(Into::into)
}
