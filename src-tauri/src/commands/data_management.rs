//! Tauri commands over the existing [`DataManagementService`]
//! (Phase 10.2 — Tauri Command Layer; Phase 9 — Data Management).
//!
//! Every destructive operation requires a live single-use confirmation id
//! minted for the data-management scope by the `request_confirmation` command
//! (FR-013; AC-5; NEX-SEC-004). The command forwards the supplied
//! `confirmation_id` verbatim to the existing service, which consumes it
//! atomically and refuses to run on a forged, expired, reused, or
//! cross-scope id — the server-side confirmation requirement is therefore
//! enforced backend-side, not on a caller-controlled constant. No crashes,
//! cascade deletions, or FTS reindexing happen here: they are delegated to
//! the existing service and database.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::confirmations::ManagedConfirmations;
use crate::application::data_management::DataManagementService;
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Permanently delete one conversation (and the messages/attachments that
/// cascade from it). Requires a live `confirmation_id` minted for the
/// data-management scope.
#[tauri::command]
pub(crate) fn delete_conversation_permanently(
    id: i64,
    confirmation_id: String,
    db: State<'_, Database>,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .delete_conversation(id, &confirmation_id, &confirmations)
        .map_err(Into::into)
}

/// Permanently delete one prompt. Requires a live `confirmation_id` minted
/// for the data-management scope.
#[tauri::command]
pub(crate) fn delete_prompt_permanently(
    id: i64,
    confirmation_id: String,
    db: State<'_, Database>,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .delete_prompt(id, &confirmation_id, &confirmations)
        .map_err(Into::into)
}

/// Clear all local application data (conversations, messages, attachments,
/// prompts, provider metadata, settings). Requires a live `confirmation_id`
/// minted for the data-management scope.
#[tauri::command]
pub(crate) fn clear_application_data(
    confirmation_id: String,
    db: State<'_, Database>,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<(), CommandError> {
    DataManagementService::new(db.inner())
        .clear(&confirmation_id, &confirmations)
        .map_err(Into::into)
}
