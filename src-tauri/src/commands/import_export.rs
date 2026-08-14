//! Tauri commands over the existing [`ExportService`] / [`ImportService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer export / import services (FR-010, FR-011)
//! and converts their classified errors into safe [`CommandError`] values.

use tauri::State;

use crate::application::export::ExportService;
use crate::application::import::ImportService;
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