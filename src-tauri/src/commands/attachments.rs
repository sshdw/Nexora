//! Tauri commands over the existing [`AttachmentService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer attachment service and converts its
//! classified errors into safe [`CommandError`] values. No file content is
//! read, copied, or uploaded here (FR-008 local-file reference semantics).

use tauri::State;

use crate::application::attachments::AttachmentService;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::attachments::Attachment;

use super::error::CommandError;

/// Attach a local-file reference to a conversation as a draft attachment and
/// return the persisted attachment.
#[tauri::command]
pub(crate) fn attach_file(
    conversation_id: i64,
    file_name: String,
    file_path: String,
    file_size_bytes: Option<i64>,
    mime_type: Option<String>,
    db: State<'_, Database>,
) -> Result<Attachment, CommandError> {
    AttachmentService::new(db.inner())
        .attach(
            conversation_id,
            &file_name,
            &file_path,
            file_size_bytes,
            mime_type.as_deref(),
        )
        .map_err(Into::into)
}

/// List the attachments owned by one conversation.
#[tauri::command]
pub(crate) fn list_attachments(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<Attachment>, CommandError> {
    AttachmentService::new(db.inner())
        .list(conversation_id)
        .map_err(Into::into)
}

/// Remove an attachment (no-op-safe: removing an unknown id is reported as
/// not-found by the service).
#[tauri::command]
pub(crate) fn remove_attachment(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    AttachmentService::new(db.inner()).remove(id).map_err(Into::into)
}