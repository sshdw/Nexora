//! Conversation context-stats command: thin IPC translation over the
//! existing [`crate::application::context_stats`] service (read-only stats +
//! diff render scope; no agent behavior change).
//!
//! No business logic or repository access lives here.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::State;

use crate::application::context_stats::{self, ConversationContextStats};
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Read-only per-conversation token/cost breakdown for the Context panel.
///
/// Token sums are `0` with `has_token_data = false` ("n/a") where no
/// persisted usage exists; nothing is ever estimated silently.
#[tauri::command]
pub(crate) fn conversation_context_stats(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<ConversationContextStats, CommandError> {
    context_stats::conversation_context_stats(db.inner(), conversation_id).map_err(Into::into)
}
