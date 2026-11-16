//! Tauri commands over the existing [`AttachmentService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer attachment service and converts its
//! classified errors into safe [`CommandError`] values. No file content is
//! handled by the command itself; bounded copying stays in the service.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::attachments::{AttachmentService, AttachmentView};
use crate::application::managed_attachments::AttachmentStore;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::attachments::Attachment;

use super::error::CommandError;

/// Admit and copy a local file into a conversation-owned draft attachment and
/// return the persisted attachment.
///
/// The file size is derived backend-side from the picked file's own metadata
/// (SEC-01) — the renderer supplies only the dialog-picked `file_path`, the
/// display `file_name`, and the optional `mime_type` hint.
#[tauri::command]
pub(crate) async fn attach_file(
    conversation_id: i64,
    file_name: String,
    file_path: String,
    mime_type: Option<String>,
    db: State<'_, Database>,
    store: State<'_, AttachmentStore>,
) -> Result<Attachment, CommandError> {
    let db = db.inner().clone();
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        AttachmentService::with_store(&db, store)
            .attach(
                conversation_id,
                &file_name,
                &file_path,
                mime_type.as_deref(),
            )
            .map_err(CommandError::from)
    })
    .await
    .map_err(|_| {
        CommandError::new(
            super::error::ErrorKind::Request,
            "attachment could not be added",
        )
    })?
}

/// List the attachments owned by one conversation.
#[tauri::command]
pub(crate) fn list_attachments(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AttachmentView>, CommandError> {
    AttachmentService::new(db.inner())
        .list_views(conversation_id)
        .map_err(Into::into)
}

/// Remove an attachment (no-op-safe: removing an unknown id is reported as
/// not-found by the service).
#[tauri::command]
pub(crate) fn remove_attachment(
    id: i64,
    db: State<'_, Database>,
    store: State<'_, AttachmentStore>,
) -> Result<(), CommandError> {
    AttachmentService::with_store(db.inner(), store.inner().clone())
        .remove(id)
        .map_err(Into::into)
}
