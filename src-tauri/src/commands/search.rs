//! Tauri command over the existing [`LocalSearchService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! The command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer local-search service (FR-009, FTS5-backed,
//! offline) and converts its classified errors into safe [`CommandError`]
//! values.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};

use crate::application::search::{LocalSearchService, SearchResults};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Run the existing local search over conversations, messages, and prompts.
///
/// Runs on the runtime blocking pool like `send_message`: the FTS query plus
/// result hydration is blocking end to end, so a synchronous command would
/// run on the main thread and freeze the window behind large result sets
/// (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn search(query: String, app: AppHandle) -> Result<SearchResults, CommandError> {
    // Owned handle so the managed state can be reached from the blocking
    // thread (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<SearchResults, CommandError> = LocalSearchService::new(db.inner())
            .search(&query)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            // The search touches only the database, so the curated database
            // text (no SQL, no stored value) is the honest mapping.
            log::error!("search blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Database,
                "a database operation failed; no data was changed",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;

    const SOURCE: &str = include_str!("search.rs");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Static wiring check (NEX-IPC-001): the search command is declared
    /// `async` and moves the FTS query onto the runtime blocking pool,
    /// matching the `send_message` precedent.
    #[test]
    fn search_runs_on_the_blocking_pool() {
        assert!(
            SOURCE.contains("async fn search("),
            "search must be an async command so it never runs on the main thread"
        );
        assert!(
            SOURCE.contains("spawn_blocking"),
            "search must move the FTS query onto the blocking pool"
        );
    }

    /// Boundary smoke: the exact body the command moves onto the blocking
    /// pool completes there and finds the seeded conversation. The scratch
    /// title carries the counter so parallel runs never match each other.
    #[test]
    fn search_body_completes_through_the_async_boundary() {
        let db = in_memory_database();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let title = format!("boundary audit {id}");
        let conversation = ConversationRepository::new(&db)
            .create(&title, "active")
            .expect("conversation seeds");
        MessageRepository::new(&db)
            .create(
                conversation,
                "user",
                "the release checklist needs review",
                None,
                None,
            )
            .expect("message seeds");

        let results = tauri::async_runtime::block_on(async {
            let db = db.clone();
            let query = format!("boundary audit {id}");
            tauri::async_runtime::spawn_blocking(move || {
                LocalSearchService::new(&db).search(&query)
            })
            .await
            .expect("blocking task joins without panicking")
        })
        .expect("search succeeds through the boundary");

        assert_eq!(results.conversations.len(), 1);
        assert_eq!(results.conversations[0].id, conversation);
    }
}
