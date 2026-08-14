//! Tauri command over the existing [`LocalSearchService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! The command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer local-search service (FR-009, FTS5-backed,
//! offline) and converts its classified errors into safe [`CommandError`]
//! values.

use tauri::State;

use crate::application::search::{LocalSearchService, SearchResults};
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Run the existing local search over conversations, messages, and prompts.
#[tauri::command]
pub(crate) fn search(
    query: String,
    db: State<'_, Database>,
) -> Result<SearchResults, CommandError> {
    LocalSearchService::new(db.inner())
        .search(&query)
        .map_err(Into::into)
}