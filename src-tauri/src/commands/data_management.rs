//! Tauri commands over the existing [`DataManagementService`]
//! (Phase 10.2 вЂ” Tauri Command Layer; Phase 9 вЂ” Data Management).
//!
//! Every destructive operation requires the caller to supply the explicit
//! confirmation phrase (FR-013; AC-5). The command forwards the supplied
//! `confirmation` verbatim to the existing service, which refuses to run
//! without the exact [`CONFIRMATION`](crate::application::data_management::CONFIRMATION)
//! phrase вЂ” the explicit-confirmation requirement is therefore preserved
//! unchanged. No crashes, cascade deletions, or FTS reindexing happen here:
//! they are delegated to the existing service and database.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::data_management::DataManagementService;
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Permanently delete one conversation (and the messages/attachments that
/// cascade from it). Requires explicit `confirmation`.
#[tauri::command]
pub(crate) fn delete_conversation_permanently(
    id: i64,
    confirmation: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .delete_conversation(id, &confirmation)
        .map_err(Into::into)
}

/// Permanently delete one prompt. Requires explicit `confirmation`.
#[tauri::command]
pub(crate) fn delete_prompt_permanently(
    id: i64,
    confirmation: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .delete_prompt(id, &confirmation)
        .map_err(Into::into)
}

/// Clear all local application data (conversations, messages, attachments,
/// prompts, provider metadata, settings). Requires explicit `confirmation`.
#[tauri::command]
pub(crate) fn clear_application_data(
    confirmation: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .clear(&confirmation)
        .map_err(Into::into)
}
