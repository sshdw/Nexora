//! Tauri commands over the existing [`ConversationService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer conversation service and converts its
//! classified errors into safe [`CommandError`] values. No new business logic
//! or repository access lives here.

use tauri::State;

use crate::application::conversations::ConversationService;
use crate::application::execution::AiResponse;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::conversations::Conversation;
use crate::infrastructure::repository::messages::Message;

use super::error::CommandError;

/// Create a new conversation and return its schema-assigned `id`.
#[tauri::command]
pub(crate) fn create_conversation(
    title: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    ConversationService::new(db.inner())
        .create(&title)
        .map_err(Into::into)
}

/// List all conversations (the conversation history / sidebar list).
#[tauri::command]
pub(crate) fn list_conversations(
    db: State<'_, Database>,
) -> Result<Vec<Conversation>, CommandError> {
    ConversationService::new(db.inner()).list().map_err(Into::into)
}

/// Read the message history for one conversation.
#[tauri::command]
pub(crate) fn conversation_history(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<Message>, CommandError> {
    ConversationService::new(db.inner())
        .history(conversation_id)
        .map_err(Into::into)
}

/// Rename a conversation.
#[tauri::command]
pub(crate) fn rename_conversation(
    id: i64,
    title: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .rename(id, &title)
        .map_err(Into::into)
}

/// Archive a conversation.
#[tauri::command]
pub(crate) fn archive_conversation(
    id: i64,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .archive(id)
        .map_err(Into::into)
}

/// Restore an archived conversation to the active state.
#[tauri::command]
pub(crate) fn restore_conversation(
    id: i64,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .restore(id)
        .map_err(Into::into)
}

/// Delete a conversation and the messages/attachments that cascade from it.
#[tauri::command]
pub(crate) fn delete_conversation(
    id: i64,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .delete(id)
        .map_err(Into::into)
}

/// Send a user message to a conversation and return the normalized AI
/// response, which is also persisted as the assistant message. Any draft
/// attachment ids supplied are linked to the created user message before the
/// request is executed (FR-008).
#[tauri::command]
pub(crate) fn send_message(
    conversation_id: i64,
    content: String,
    provider: String,
    model: String,
    attachment_ids: Vec<i64>,
    db: State<'_, Database>,
) -> Result<AiResponse, CommandError> {
    ConversationService::new(db.inner())
        .send_message(
            conversation_id,
            &content,
            &provider,
            &model,
            &attachment_ids,
        )
        .map_err(Into::into)
}