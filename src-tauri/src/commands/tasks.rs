//! Task-manager IPC commands: the Tauri side of task CRUD and the autonomous
//! loop.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves managed
//! state, delegates to the application-layer task service
//! ([`crate::application::agent::tasks`]), and maps classified errors into
//! secret-free [`CommandError`] values ([`super::error`] doctrine — no
//! credentials, raw SQL, or task payloads in error text).
//!
//! # Command shapes
//!
//! - `create_task { title, description?, conversationId?, provider?,
//!   model?, steps: string[], maxSteps? } → id` — creates the task with its
//!   ordered steps (a backing `Task: <title>` conversation is created when
//!   `conversationId` is absent); rejects empty titles, empty step lists,
//!   more than 25 steps, and caps outside 1..25.
//! - `list_tasks {} → AgentTask[]` — all tasks, most recently active first.
//! - `list_task_steps { taskId } → AgentTaskStep[]` — one task's steps,
//!   `seq` ascending.
//! - `update_task { taskId, title, description? }` — rename/edit a
//!   non-running task.
//! - `delete_task { taskId }` — delete a non-running task (steps cascade).
//! - `start_task_run { taskId }` — claim the task and spawn the autonomous
//!   loop thread (plan → act → verify → report); progress streams via the
//!   `agent-task-event` Tauri event.
//! - `stop_task_run { taskId } → bool` — stop the active loop (and abort its
//!   in-flight agent run); `false` means no loop was active.
//!
//! # Secrets
//!
//! `start_task_run` resolves the provider credential *inside the backend*
//! via the existing [`RequestExecutionService::resolve_credential`] path and
//! moves it straight into the spawned loop thread. It never crosses IPC, is
//! never serialized, logged, or placed in a [`TaskEvent`], and is dropped
//! when the thread ends.
//!
//! # Budgets and approvals
//!
//! Every step executes through
//! [`crate::application::agent::service::start_run`], so the persisted
//! autonomy mode, run preset, per-run spend limit, step budget, and approval
//! gate apply to autonomous steps exactly as to manual runs — the loop never
//! auto-extends a parked budget and never auto-resolves an approval. A
//! budget-parked step stops the loop (its run id stays on the task row for
//! `extend_agent_run`); a spend-guard trip fails the step and stops the
//! loop; `stop_task_run` aborts the in-flight run through the existing
//! registry cancel path.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. agent.rs.)
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::application::agent::service::{self, AgentRunHost};
use crate::application::agent::tasks::{
    self, TaskError, TaskEvent, TaskRegistry, TaskRunConfig, TaskService,
};
use crate::application::execution::{ExecutorRegistry, RequestExecutionService};
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_tasks::{AgentTask, AgentTaskStep};

use super::agent::ManagedRegistry;
use super::error::{CommandError, ErrorKind};

/// Managed task-registry state is an [`Arc`] so commands can clone an owned
/// handle into `spawn_blocking`/the loop thread without borrowing the
/// managed value.
pub(crate) type ManagedTaskRegistry = Arc<TaskRegistry>;

/// Create a task with its ordered steps (`create_task` shape above).
/// Create a task command: 8 IPC inputs by Tauri-command construction (all
/// task fields plus managed state), mirroring the other multi-field commands.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn create_task(
    title: String,
    description: Option<String>,
    conversation_id: Option<i64>,
    provider: Option<String>,
    model: Option<String>,
    steps: Vec<String>,
    max_steps: Option<i64>,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    TaskService::new(db.inner())
        .create_task(
            &title,
            description.as_deref(),
            conversation_id,
            provider.as_deref(),
            model.as_deref(),
            &steps,
            max_steps,
        )
        .map_err(CommandError::from)
}

/// List all tasks, most recently active first.
#[tauri::command]
pub(crate) fn list_tasks(db: State<'_, Database>) -> Result<Vec<AgentTask>, CommandError> {
    TaskService::new(db.inner())
        .list_tasks()
        .map_err(CommandError::from)
}

/// List one task's steps, `seq` ascending.
#[tauri::command]
pub(crate) fn list_task_steps(
    task_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AgentTaskStep>, CommandError> {
    TaskService::new(db.inner())
        .list_steps(task_id)
        .map_err(CommandError::from)
}

/// Rename/edit a non-running task.
#[tauri::command]
pub(crate) fn update_task(
    task_id: i64,
    title: String,
    description: Option<String>,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    TaskService::new(db.inner())
        .update_task(task_id, &title, description.as_deref())
        .map_err(CommandError::from)
}

/// Delete a non-running task (its steps cascade via the schema).
#[tauri::command]
pub(crate) fn delete_task(
    task_id: i64,
    db: State<'_, Database>,
    registry: State<'_, ManagedTaskRegistry>,
) -> Result<(), CommandError> {
    TaskService::new(db.inner())
        .delete_task(task_id, &registry)
        .map_err(CommandError::from)
}

/// Start the autonomous loop for `task_id` (shape and guarantees above).
///
/// Performs fast local work only (`SQLite` + keyring reads + thread spawn)
/// on the runtime's blocking pool — exactly like `start_agent_run`
/// (BUG-005 doctrine) — and returns immediately; the loop runs on its
/// dedicated thread and reports through `agent-task-event`.
#[tauri::command]
pub(crate) async fn start_task_run(app: AppHandle, task_id: i64) -> Result<(), CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let registry = handle.state::<ManagedRegistry>();
        let task_registry = handle.state::<ManagedTaskRegistry>();
        let db_client = db.inner().clone();
        let db_ref = &db_client;
        let registry_arc = Arc::clone(registry.inner());
        let task_registry_arc = Arc::clone(task_registry.inner());

        // 1. Load the task for its provider/model pair (fixed vocabulary
        //    when absent — the loop's own spawn re-validates everything).
        let task = TaskService::new(db_ref)
            .read_task(task_id)
            .map_err(CommandError::from)?;
        let (Some(provider), Some(_model)) = (task.provider.clone(), task.model.clone()) else {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                "choose a provider and model before running the task",
            ));
        };

        // 2. Resolve the credential inside the backend (FR-014 path shared
        //    with plain chat and agent runs). The value lives only inside
        //    the loop thread.
        let credential = RequestExecutionService::new(db_ref)
            .resolve_credential(&provider)
            .map_err(CommandError::from)?;

        // 3. Resolve the provider executor (no fallback; same registry plain
        //    chat and agent runs use).
        let executor = ExecutorRegistry::new()
            .resolve_owned(&provider)
            .ok_or_else(|| {
                CommandError::from(
                    crate::application::execution::RequestError::ExecutorUnavailable {
                        name: provider.clone(),
                    },
                )
            })?;

        let root: PathBuf = super::agent::workspace_root(&handle, db_ref)?;
        let emitter = handle_state_emitter(&handle);
        let host: Arc<dyn AgentRunHost> =
            Arc::new(super::agent::TauriAgentHost::new(handle, db_client.clone()));
        let config = TaskRunConfig {
            executor,
            workspace_root: root,
            credential,
            mode: service::resolve_autonomy_mode(db_ref),
            preset: service::resolve_preset(db_ref),
            spend_limit_micro_usd: service::resolve_spend_limit(db_ref),
        };
        tasks::spawn_task_run(
            db_ref,
            registry_arc,
            task_registry_arc,
            host,
            emitter,
            task_id,
            config,
        )
        .map_err(CommandError::from)
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("start_task_run blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the task run could not be started",
            ))
        }
    }
}

/// Clone an `Arc` emitter over the `AppHandle` for the loop thread: each
/// [`TaskEvent`] becomes one `agent-task-event` frame (best-effort — a
/// missing frontend listener never affects the loop).
fn handle_state_emitter(handle: &AppHandle) -> Arc<dyn Fn(TaskEvent) + Send + Sync> {
    let app = handle.clone();
    Arc::new(move |event: TaskEvent| {
        if let Err(err) = app.emit("agent-task-event", event) {
            log::warn!("task run bridge: frame emission failed: {err}");
        }
    })
}

/// Stop the active loop for `task_id` (and abort its in-flight agent run).
/// Returns whether a loop was active (and is now cancelled); `false` means
/// nothing was running. Unknown tasks fail with a secret-free not-found
/// error carrying only the id.
#[tauri::command]
pub(crate) fn stop_task_run(
    task_id: i64,
    db: State<'_, Database>,
    registry: State<'_, ManagedTaskRegistry>,
    agent_registry: State<'_, ManagedRegistry>,
) -> Result<bool, CommandError> {
    // Unknown tasks fail here (id only) rather than silently reporting
    // "nothing running" for a task that never existed.
    TaskService::new(db.inner())
        .read_task(task_id)
        .map_err(CommandError::from)?;
    let (active, run_id) = registry.request_cancel(task_id);
    if !active {
        return Ok(false);
    }
    if let Some(run_id) = run_id {
        let _ = agent_registry.cancel(run_id);
    }
    Ok(true)
}

impl From<TaskError> for CommandError {
    fn from(err: TaskError) -> Self {
        match err {
            TaskError::TaskNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("no task with id {id}"))
            }
            TaskError::AlreadyRunning { task_id } => Self::new(
                ErrorKind::InvalidInput,
                format!("task {task_id} already has an active run"),
            ),
            TaskError::InvalidInput { message } => Self::new(ErrorKind::InvalidInput, message),
            TaskError::RunStart { message } => Self::new(ErrorKind::Request, message),
            TaskError::Database(inner) => Self::from(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    /// Negative-control: the task command mapping must never surface a
    /// secret, even when the source error carries raw backend output. The
    /// production `RunStart`/`InvalidInput` messages are fixed vocabulary
    /// (the loop never forwards provider text), pinned exactly here.
    #[test]
    fn task_error_mapping_is_secret_free() {
        let cases = [
            TaskError::TaskNotFound { id: 7 },
            TaskError::AlreadyRunning { task_id: 7 },
            TaskError::InvalidInput {
                message: "the task title must be 1..200 characters".to_string(),
            },
            TaskError::RunStart {
                message: "the step run could not be started".to_string(),
            },
            TaskError::Database(crate::infrastructure::database::DatabaseError::Lock(
                "sk-".into(),
            )),
        ];
        for case in cases {
            let mapped: CommandError = case.into();
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }

    #[test]
    fn task_error_kinds_are_classified() {
        assert_eq!(
            CommandError::from(TaskError::TaskNotFound { id: 1 }).kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            CommandError::from(TaskError::AlreadyRunning { task_id: 1 }).kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            CommandError::from(TaskError::InvalidInput {
                message: "x".to_string()
            })
            .kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            CommandError::from(TaskError::RunStart {
                message: "x".to_string()
            })
            .kind,
            ErrorKind::Request
        );
    }

    /// Static wiring check: the production `start_task_run` command must
    /// route through the task service's spawn (which owns the claim + loop
    /// thread) and must not construct the agent runner directly.
    #[test]
    fn start_task_run_routes_through_task_service() {
        const SOURCE: &str = include_str!("tasks.rs");
        assert!(
            SOURCE.contains("tasks::spawn_task_run("),
            "start_task_run must delegate to the task service spawn"
        );
        let runner_needle = concat!("AgentRunner", "::new");
        assert!(
            !SOURCE.contains(runner_needle),
            "commands/tasks.rs must not construct the runner directly"
        );
    }
}
