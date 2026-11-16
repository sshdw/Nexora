//! Conversation context-stats command: thin IPC translation over the
//! existing [`crate::application::context_stats`] service (read-only stats +
//! diff render scope; no agent behavior change).
//!
//! No business logic or repository access lives here.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::application::agent::service::{self, AgentRunHost, CompactionOutcomeView};
use crate::application::context_stats::{self, ConversationContextStats};
use crate::application::execution::RequestExecutionService;
use crate::infrastructure::database::Database;

use super::agent::{ManagedRegistry, TauriAgentHost};
use super::error::{CommandError, ErrorKind};

/// Read-only per-conversation token/cost breakdown for the Context panel.
///
/// Token sums are `0` with `has_token_data = false` ("n/a") where no
/// persisted usage exists; nothing is ever estimated silently.
#[tauri::command]
pub(crate) fn conversation_context_stats(
    conversation_id: i64,
    connection_id: Option<String>,
    model_id: Option<String>,
    db: State<'_, Database>,
) -> Result<ConversationContextStats, CommandError> {
    context_stats::conversation_context_stats_for_selection(
        db.inner(),
        conversation_id,
        connection_id.as_deref(),
        model_id.as_deref(),
    )
    .map_err(Into::into)
}

/// Manually compact conversation context (`/compact` or `ContextRing` `Compact`
/// button), preserving Goal, Active Plan, Key Decisions, and Touched Files.
#[tauri::command]
pub(crate) async fn compact_conversation_context(
    conversation_id: i64,
    connection_id: Option<String>,
    model_id: Option<String>,
    app: AppHandle,
    db: State<'_, Database>,
    registry: State<'_, ManagedRegistry>,
) -> Result<CompactionOutcomeView, CommandError> {
    let db_cloned = db.inner().clone();
    let registry_cloned = Arc::clone(registry.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let host: Arc<dyn AgentRunHost> = Arc::new(TauriAgentHost::new(app, db_cloned.clone()));
        let exec_svc = RequestExecutionService::new(&db_cloned);
        let owned_target = match (
            connection_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            model_id.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        ) {
            (Some(conn), Some(model)) => {
                if let (Ok(cred), Ok(exec)) = (
                    exec_svc.resolve_credential(conn),
                    exec_svc.resolve_executor_for(conn),
                ) {
                    Some((exec, conn.to_string(), model.to_string(), cred))
                } else {
                    None
                }
            }
            _ => None,
        };
        let target_ref = owned_target.as_ref().map(|(exec, conn, model, cred)| {
            let exec_ref: &dyn crate::application::execution::ProviderExecutor = exec.as_ref();
            (exec_ref, conn.as_str(), model.as_str(), cred.as_str())
        });
        service::compact_conversation_context(
            &db_cloned,
            Some(registry_cloned.as_ref()),
            Some(host.as_ref()),
            target_ref,
            conversation_id,
            connection_id.as_deref(),
            model_id.as_deref(),
        )
        .map_err(CommandError::from)
    })
    .await
    .map_err(|_| CommandError::new(ErrorKind::Request, "context compaction task failed"))?
}
