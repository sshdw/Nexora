//! Tauri commands over the existing [`PromptLibraryService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer prompt-library service and converts its
//! classified errors into safe [`CommandError`] values.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::prompts::PromptLibraryService;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::messages::Message;
use crate::infrastructure::repository::prompts::Prompt;

use super::error::CommandError;

/// Create a new prompt and return its schema-assigned `id`.
#[tauri::command]
pub(crate) fn create_prompt(
    title: String,
    content: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    PromptLibraryService::new(db.inner())
        .create(&title, &content)
        .map_err(Into::into)
}

/// List every prompt in the library.
#[tauri::command]
pub(crate) fn list_prompts(db: State<'_, Database>) -> Result<Vec<Prompt>, CommandError> {
    PromptLibraryService::new(db.inner())
        .list()
        .map_err(Into::into)
}

/// Update an existing prompt's `title` / `content`.
#[tauri::command]
pub(crate) fn update_prompt(
    id: i64,
    title: String,
    content: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    PromptLibraryService::new(db.inner())
        .update(id, &title, &content)
        .map_err(Into::into)
}

/// Delete a prompt from the library (a no-op when the id is unknown).
#[tauri::command]
pub(crate) fn delete_prompt(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    PromptLibraryService::new(db.inner())
        .delete(id)
        .map_err(Into::into)
}

/// Insert a prompt's content into a conversation as a user message.
#[tauri::command]
pub(crate) fn insert_prompt_into_conversation(
    prompt_id: i64,
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Message, CommandError> {
    PromptLibraryService::new(db.inner())
        .insert_into_conversation(prompt_id, conversation_id)
        .map_err(Into::into)
}
