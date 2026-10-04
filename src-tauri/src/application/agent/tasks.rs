//! Task manager + autonomous mode: user-defined task lists with
//! agent-executable steps, and a bounded autonomous loop
//! (plan → act → verify → report) running inside the existing agent engine.
//!
//! [`TaskService`] owns task CRUD validation over
//! [`crate::infrastructure::repository::agent_tasks::AgentTaskRepository`]:
//! tasks carry a title, an optional description, an ordered step list, and
//! the provider/model pair the autonomous runs use. When no conversation is
//! supplied at creation, the service creates one backing conversation
//! (`Task: <title>`) through [`crate::application::conversations`] —
//! conversations + agent runs are the substrate, so every autonomous step
//! executes as one ordinary agent run in that conversation and its history
//! stays inspectable there.
//!
//! [`spawn_task_run`] drives the autonomous loop on a dedicated thread: for
//! each step (plan) it starts one agent run through the existing
//! [`crate::application::agent::service::start_run`] bridge (act), polls the
//! persisted `agent_runs` row until it reaches a terminal status (verify),
//! records the per-step result, and finally composes the task report
//! (report). There is deliberately exactly one agent-execution call on this
//! path — [`service::start_run`] — so budgets, approval gates, and spend
//! limits are honored by construction, never bypassed:
//!
//! - the step budget parks the run on `BudgetExhausted` exactly as for a
//!   manual run; the loop never auto-extends — a parked step run stops the
//!   loop with an honest report (the parked run is cancelled first so the
//!   conversation stays reusable) instead of polling its `'running'` row
//!   forever (NEX-AGENT-001);
//! - the approval gate parks mutating tool calls per the persisted autonomy
//!   mode (resolved through the existing `resolve_agent_approval` command);
//! - the per-run spend guard terminates the run on `SpendLimitExceeded`
//!   (the loop records the step as failed and stops — it never retries past
//!   the guard).
//!
//! Boundedness: task creation rejects more than [`TASK_MAX_STEPS`] steps and
//! `max_steps` outside `1..=TASK_MAX_STEPS`; the loop additionally executes
//! at most `max_steps` steps and then marks the remainder `skipped`, so no
//! infinite loop is possible. A wall-clock [`TASK_LOOP_DEADLINE`] bounds the
//! whole loop as a safety net (NEX-AGENT-001): a step run that never reaches
//! a terminal status and never parks observably stops the loop with an
//! honest report instead of polling forever. [`TaskRegistry`] carries the per-task
//! cancellation flag (plus the current step's `run_id` so `stop` also aborts
//! the in-flight agent run through the existing
//! [`AgentRunRegistry::cancel`] path — the cancellation-token precedent).
//! Progress and the terminal report surface through [`TaskEvent`] frames on
//! the `agent-task-event` stream; the persisted rows stay the source of
//! truth.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::approval::AutonomyMode;
use super::permissions::RunPreset;
use super::service::{self, AgentRunHost, AgentRunRegistry, AgentRunRequest};
use crate::application::conversations::{ConversationError, ConversationService};
use crate::application::execution::ProviderExecutor;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;
use crate::infrastructure::repository::agent_tasks::{
    AgentTask, AgentTaskRepository, AgentTaskStep,
};

// ---------------------------------------------------------------------------
// Bounds (mirror the v8 schema CHECKs; validated before any write)
// ---------------------------------------------------------------------------

/// Hard cap on steps per task and on `max_steps` (v8 `CHECK (max_steps <=
/// 25)`). The autonomous loop additionally enforces this at runtime, so the
/// cap holds even for rows written before a bounds change.
pub(crate) const TASK_MAX_STEPS: i64 = 25;

/// Longest accepted task title (v8 `CHECK (length(title) <= 200)`).
pub(crate) const TASK_TITLE_MAX_LEN: usize = 200;

/// Longest accepted task description (v8 `CHECK (length(description) <=
/// 4000)`).
pub(crate) const TASK_DESCRIPTION_MAX_LEN: usize = 4000;

/// Longest accepted step title (v8 `CHECK (length(title) <= 500)`).
pub(crate) const STEP_TITLE_MAX_LEN: usize = 500;

/// Longest per-step `result` persisted (agent `final_content` is unbounded;
/// the task row keeps an excerpt so one verbose step cannot bloat the task).
pub(crate) const STEP_RESULT_MAX_CHARS: usize = 4000;

/// Longest per-step excerpt carried into the composed report.
pub(crate) const REPORT_EXCERPT_MAX_CHARS: usize = 1000;

/// Poll interval while waiting for a step's agent run to terminate.
const RUN_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Consecutive `agent_runs` read failures tolerated while polling before the
/// step is failed (a persistently unreadable database must terminate the
/// loop, not spin it forever).
const MAX_CONSECUTIVE_READ_ERRORS: u32 = 150;

/// Overall wall-clock bound on one autonomous loop, from loop start to the
/// terminal report (NEX-AGENT-001 safety net). Generous by design:
/// legitimate tasks finish far sooner; only a genuinely stuck loop — a step
/// run that never reaches a terminal status and never parks observably —
/// trips it, and then the loop stops with an honest report instead of
/// polling forever.
const TASK_LOOP_DEADLINE: Duration = Duration::from_mins(30);

/// Consecutive polls observing the in-memory budget park before the loop
/// treats the step as parked (NEX-AGENT-001). A single poll could win a race
/// against a user `extend_steps` landing just as the run parks; requiring a
/// short streak keeps the loop honest without hanging on a true park.
const PARK_CONFIRM_POLLS: u32 = 3;

// ---------------------------------------------------------------------------
// Events (`agent-task-event` frames)
// ---------------------------------------------------------------------------

/// One `agent-task-event` frame: task/step lifecycle for the `TaskPanel`.
/// Secret-free by construction — ids, sequence numbers, and
/// fixed-vocabulary statuses only (never step results or report text; the UI
/// reloads those through the list commands).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TaskEvent {
    /// The autonomous loop started (`task_id` only).
    Started {
        /// The task whose loop started.
        task_id: i64,
    },
    /// One step started executing (`task_id` + 1-based `seq`).
    StepStarted {
        /// The task the step belongs to.
        task_id: i64,
        /// 1-based step sequence.
        seq: i64,
    },
    /// One step reached a terminal step status (`task_id` + `seq` +
    /// fixed-vocabulary `status`).
    StepFinished {
        /// The task the step belongs to.
        task_id: i64,
        /// 1-based step sequence.
        seq: i64,
        /// `'completed' | 'failed' | 'skipped' | 'cancelled'`.
        status: String,
    },
    /// The loop terminated (`task_id` + fixed-vocabulary `status`).
    Finished {
        /// The finished task.
        task_id: i64,
        /// `'completed' | 'failed' | 'cancelled'`.
        status: String,
    },
}

// ---------------------------------------------------------------------------
// Errors (secret-free: ids and fixed vocabulary only)
// ---------------------------------------------------------------------------

/// Classified failures of the task service and the autonomous loop.
#[derive(Debug)]
pub(crate) enum TaskError {
    /// No task with this id exists.
    TaskNotFound {
        /// The missing task id.
        id: i64,
    },
    /// A loop is already active for this task.
    AlreadyRunning {
        /// The busy task id.
        task_id: i64,
    },
    /// Caller-supplied input failed validation (fixed-vocabulary message).
    InvalidInput {
        /// What was wrong (fixed vocabulary, never caller content).
        message: String,
    },
    /// The step's agent run could not be started (fixed-vocabulary message;
    /// never the provider error text, which may carry payload fragments).
    RunStart {
        /// Fixed-vocabulary reason.
        message: String,
    },
    /// Persistence failed.
    Database(DatabaseError),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TaskNotFound { id } => write!(f, "no task with id {id}"),
            Self::AlreadyRunning { task_id } => {
                write!(f, "task {task_id} already has an active run")
            }
            Self::InvalidInput { message } | Self::RunStart { message } => {
                write!(f, "{message}")
            }
            Self::Database(err) => write!(f, "task persistence failed: {err}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry (active autonomous loops: cancel flag + current run id)
// ---------------------------------------------------------------------------

/// One active autonomous loop's handles: the cooperative cancel flag the
/// loop polls, and the current step's agent `run_id` (so `stop` aborts the
/// in-flight run through [`AgentRunRegistry::cancel`], the same
/// cancellation-token path as `cancel_agent_run`).
#[derive(Debug, Default)]
pub(crate) struct ActiveTaskRun {
    cancel: AtomicBool,
    run_id: Mutex<Option<i64>>,
}

/// Active-task registry: managed Tauri state mapping `task_id` to the
/// handles of the in-flight autonomous loop. Mirrors the agent
/// [`AgentRunRegistry`] contract: at most one active loop per task.
#[derive(Debug, Default)]
pub(crate) struct TaskRegistry {
    runs: Mutex<HashMap<i64, Arc<ActiveTaskRun>>>,
}

impl TaskRegistry {
    /// Claim a task slot. Returns `None` when a loop is already active.
    pub(crate) fn claim(&self, task_id: i64) -> Option<Arc<ActiveTaskRun>> {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        if runs.contains_key(&task_id) {
            return None;
        }
        let entry = Arc::new(ActiveTaskRun::default());
        runs.insert(task_id, Arc::clone(&entry));
        Some(entry)
    }

    /// Release a terminated loop (every exit path of the loop thread).
    pub(crate) fn release(&self, task_id: i64) {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&task_id);
    }

    /// Whether a loop is currently registered for `task_id`.
    #[must_use]
    pub(crate) fn is_active(&self, task_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&task_id)
    }

    /// Request cancellation: sets the flag the loop polls and reports the
    /// current step's agent `run_id` (if any) so the caller can abort it.
    /// The `active` flag is false when no loop is registered.
    pub(crate) fn request_cancel(&self, task_id: i64) -> (bool, Option<i64>) {
        let runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = runs.get(&task_id) else {
            return (false, None);
        };
        entry.cancel.store(true, Ordering::SeqCst);
        let run_id = *entry.run_id.lock().unwrap_or_else(PoisonError::into_inner);
        (true, run_id)
    }

    /// Whether cancellation was requested for `task_id` (false when no loop
    /// is active).
    #[must_use]
    pub(crate) fn is_cancelled(&self, task_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&task_id)
            .is_some_and(|entry| entry.cancel.load(Ordering::SeqCst))
    }

    /// Record the current step's agent `run_id` (cleared with `None` when
    /// the step terminates).
    pub(crate) fn set_run_id(&self, task_id: i64, run_id: Option<i64>) {
        if let Some(entry) = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&task_id)
        {
            *entry.run_id.lock().unwrap_or_else(PoisonError::into_inner) = run_id;
        }
    }
}

// ---------------------------------------------------------------------------
// Service (validation + CRUD orchestration)
// ---------------------------------------------------------------------------

/// Task CRUD over the shared [`Database`]. Validation mirrors the v8 CHECKs
/// so invalid input fails here with fixed vocabulary instead of as raw
/// `SQLite` errors.
pub(crate) struct TaskService<'a> {
    db: &'a Database,
}

impl<'a> TaskService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Validate a task title (non-empty after trimming, within the bound).
    fn validate_title(title: &str) -> Result<String, TaskError> {
        let trimmed = title.trim().to_string();
        if trimmed.is_empty() || trimmed.chars().count() > TASK_TITLE_MAX_LEN {
            return Err(TaskError::InvalidInput {
                message: "the task title must be 1..200 characters".to_string(),
            });
        }
        Ok(trimmed)
    }

    /// Validate an optional description (empty maps to `None`).
    fn validate_description(description: Option<&str>) -> Result<Option<String>, TaskError> {
        let Some(text) = description else {
            return Ok(None);
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.chars().count() > TASK_DESCRIPTION_MAX_LEN {
            return Err(TaskError::InvalidInput {
                message: "the task description must be at most 4000 characters".to_string(),
            });
        }
        Ok(Some(trimmed.to_string()))
    }

    /// Validate the step list (1..=`TASK_MAX_STEPS` non-empty titles).
    fn validate_steps(steps: &[String]) -> Result<Vec<String>, TaskError> {
        let count = i64::try_from(steps.len()).unwrap_or(i64::MAX);
        if steps.is_empty() || count > TASK_MAX_STEPS {
            return Err(TaskError::InvalidInput {
                message: "the task must define 1..25 steps".to_string(),
            });
        }
        let mut clean = Vec::with_capacity(steps.len());
        for step in steps {
            let trimmed = step.trim().to_string();
            if trimmed.is_empty() || trimmed.chars().count() > STEP_TITLE_MAX_LEN {
                return Err(TaskError::InvalidInput {
                    message: "each step title must be 1..500 characters".to_string(),
                });
            }
            clean.push(trimmed);
        }
        Ok(clean)
    }

    /// Resolve the effective step cap: explicit `max_steps` must land in
    /// `1..=TASK_MAX_STEPS`; absent means one pass over the step list.
    fn resolve_max_steps(max_steps: Option<i64>, step_count: usize) -> Result<i64, TaskError> {
        match max_steps {
            None => Ok(i64::try_from(step_count).unwrap_or(TASK_MAX_STEPS)),
            Some(cap) if (1..=TASK_MAX_STEPS).contains(&cap) => Ok(cap),
            Some(_) => Err(TaskError::InvalidInput {
                message: "the step cap must be 1..25".to_string(),
            }),
        }
    }

    /// Create a task with its steps. When `conversation_id` is `None`, a
    /// backing conversation (`Task: <title>`) is created first so the
    /// autonomous loop's agent runs have a conversation to execute inside
    /// of. Returns the task `id` (steps are stored `seq` 1-based in order).
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::InvalidInput`] for out-of-bounds titles,
    /// descriptions, step lists, or caps, and [`TaskError::Database`] for
    /// persistence failures (including a missing explicit conversation).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        conversation_id: Option<i64>,
        provider: Option<&str>,
        model: Option<&str>,
        steps: &[String],
        max_steps: Option<i64>,
    ) -> Result<i64, TaskError> {
        let title = Self::validate_title(title)?;
        let description = Self::validate_description(description)?;
        let steps = Self::validate_steps(steps)?;
        let cap = Self::resolve_max_steps(max_steps, steps.len())?;
        let provider = non_empty(provider);
        let model = non_empty(model);

        let conversation_id = if let Some(id) = conversation_id {
            Some(id)
        } else {
            let backing = format!("Task: {}", title.chars().take(100).collect::<String>());
            match ConversationService::new(self.db).create(&backing) {
                Ok(id) => Some(id),
                Err(ConversationError::Database(inner)) => {
                    return Err(TaskError::Database(inner));
                }
                Err(other) => {
                    log::warn!("task create: backing conversation failed: {other}");
                    return Err(TaskError::RunStart {
                        message: "the task conversation could not be created".to_string(),
                    });
                }
            }
        };

        let repo = AgentTaskRepository::new(self.db);
        let id = repo
            .create_task(
                &title,
                description.as_deref(),
                conversation_id,
                provider.as_deref(),
                model.as_deref(),
                cap,
            )
            .map_err(TaskError::Database)?;
        for (index, step) in steps.iter().enumerate() {
            let seq = i64::try_from(index + 1).unwrap_or(i64::MAX);
            repo.append_step(id, seq, step)
                .map_err(TaskError::Database)?;
        }
        Ok(id)
    }

    /// List all tasks, most recently active first.
    ///
    /// # Errors
    ///
    /// Propagates [`DatabaseError`].
    pub(crate) fn list_tasks(&self) -> Result<Vec<AgentTask>, DatabaseError> {
        AgentTaskRepository::new(self.db).list_tasks()
    }

    /// Read one task by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] when no task exists; propagates
    /// [`DatabaseError`] on read failure.
    pub(crate) fn read_task(&self, id: i64) -> Result<AgentTask, TaskError> {
        AgentTaskRepository::new(self.db)
            .read_task(id)
            .map_err(TaskError::Database)?
            .ok_or(TaskError::TaskNotFound { id })
    }

    /// List one task's steps, `seq` ascending.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] when no task exists; propagates
    /// [`DatabaseError`] on read failure.
    pub(crate) fn list_steps(&self, task_id: i64) -> Result<Vec<AgentTaskStep>, TaskError> {
        self.read_task(task_id)?;
        AgentTaskRepository::new(self.db)
            .list_steps(task_id)
            .map_err(TaskError::Database)
    }

    /// Update a pending task's title/description. Running tasks are rejected:
    /// edits mid-loop would race the report the loop composes.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for out-of-bounds text or for edits to a
    /// running task; propagates [`DatabaseError`] on write failure.
    pub(crate) fn update_task(
        &self,
        id: i64,
        title: &str,
        description: Option<&str>,
    ) -> Result<(), TaskError> {
        let task = self.read_task(id)?;
        if task.status == "running" {
            return Err(TaskError::InvalidInput {
                message: "a running task cannot be edited".to_string(),
            });
        }
        let title = Self::validate_title(title)?;
        let description = Self::validate_description(description)?;
        AgentTaskRepository::new(self.db)
            .update_task(id, &title, description.as_deref())
            .map_err(TaskError::Database)
    }

    /// Delete a task (its steps cascade via the schema). Deleting a running
    /// task is rejected: stop it first so the loop thread never writes to a
    /// deleted row.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::TaskNotFound`] for unknown ids,
    /// [`TaskError::InvalidInput`] for deletes of a running task; propagates
    /// [`DatabaseError`] on write failure.
    pub(crate) fn delete_task(&self, id: i64, registry: &TaskRegistry) -> Result<(), TaskError> {
        let task = self.read_task(id)?;
        if task.status == "running" || registry.is_active(id) {
            return Err(TaskError::InvalidInput {
                message: "stop the task before deleting it".to_string(),
            });
        }
        AgentTaskRepository::new(self.db)
            .delete_task(id)
            .map_err(TaskError::Database)
    }
}

/// Map empty/whitespace-only optionals to `None` (an explicit empty provider
/// is "unset", never a provider named `""`).
fn non_empty(value: Option<&str>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

// ---------------------------------------------------------------------------
// Autonomous loop (plan → act → verify → report)
// ---------------------------------------------------------------------------

/// Owned inputs the loop thread needs. Taken by value deliberately: the
/// command layer resolves everything up front and moves the bundle into the
/// spawned thread (the credential lives only there — never in an event, log,
/// or error — mirroring [`AgentRunRequest`]).
#[derive(Clone)]
pub(crate) struct TaskRunConfig {
    /// Provider executor the step runs dispatch through (the run path
    /// resolves the provider name to this same registry).
    pub executor: Arc<dyn ProviderExecutor + Send + Sync>,
    /// Workspace root bounding the step runs' tool access.
    pub workspace_root: PathBuf,
    /// Keyring credential for the provider (pre-resolved by the caller).
    pub credential: String,
    /// Autonomy mode for the step runs (persisted setting, DP-AUTONOMY).
    pub mode: AutonomyMode,
    /// Tool preset for the step runs (persisted setting, T5).
    pub preset: RunPreset,
    /// Per-run spend limit in micro-USD (`None` = no guard).
    pub spend_limit_micro_usd: Option<u64>,
}

/// Claim `task_id` and spawn its autonomous loop thread. Returns immediately;
/// progress flows through `emit` (`agent-task-event` frames) and the
/// persisted rows.
///
/// # Errors
///
/// Returns [`TaskError::TaskNotFound`] for unknown ids,
/// [`TaskError::AlreadyRunning`] when a loop is already active, and
/// [`TaskError::InvalidInput`] when the task has no steps or no
/// provider/model pair (choose both before running).
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub(crate) fn spawn_task_run(
    db: &Database,
    agent_registry: Arc<AgentRunRegistry>,
    task_registry: Arc<TaskRegistry>,
    host: Arc<dyn AgentRunHost>,
    emit: Arc<dyn Fn(TaskEvent) + Send + Sync>,
    task_id: i64,
    config: TaskRunConfig,
) -> Result<(), TaskError> {
    spawn_task_run_with_deadline(
        db,
        agent_registry,
        task_registry,
        host,
        emit,
        task_id,
        config,
        Instant::now() + TASK_LOOP_DEADLINE,
    )
}

/// Deadline-parameterized [`spawn_task_run`]: production passes the default
/// [`TASK_LOOP_DEADLINE`]; tests pass short bounds to prove the loop
/// terminates without waiting out the production bound.
///
/// # Errors
///
/// Same as [`spawn_task_run`].
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub(crate) fn spawn_task_run_with_deadline(
    db: &Database,
    agent_registry: Arc<AgentRunRegistry>,
    task_registry: Arc<TaskRegistry>,
    host: Arc<dyn AgentRunHost>,
    emit: Arc<dyn Fn(TaskEvent) + Send + Sync>,
    task_id: i64,
    config: TaskRunConfig,
    deadline: Instant,
) -> Result<(), TaskError> {
    let service = TaskService::new(db);
    let task = service.read_task(task_id)?;
    if task_registry.is_active(task_id) || task.status == "running" {
        return Err(TaskError::AlreadyRunning { task_id });
    }
    let steps = service.list_steps(task_id)?;
    if steps.is_empty() {
        return Err(TaskError::InvalidInput {
            message: "the task has no steps to run".to_string(),
        });
    }
    let (Some(provider), Some(model)) = (task.provider.clone(), task.model.clone()) else {
        return Err(TaskError::InvalidInput {
            message: "choose a provider and model before running the task".to_string(),
        });
    };
    let Some(conversation_id) = task.conversation_id else {
        return Err(TaskError::InvalidInput {
            message: "the task has no conversation to run in".to_string(),
        });
    };
    if task_registry.claim(task_id).is_none() {
        return Err(TaskError::AlreadyRunning { task_id });
    }

    let thread_db = db.clone();
    let release_registry = Arc::clone(&task_registry);
    std::thread::Builder::new()
        .name(format!("agent-task-{task_id}"))
        .spawn(move || {
            run_task_loop_with_deadline(
                &thread_db,
                &agent_registry,
                &task_registry,
                &host,
                &emit,
                task_id,
                conversation_id,
                &provider,
                &model,
                &steps,
                &config,
                deadline,
            );
            task_registry.release(task_id);
        })
        .map_err(|_| {
            release_registry.release(task_id);
            TaskError::RunStart {
                message: "the task run could not be started".to_string(),
            }
        })?;
    Ok(())
}

/// Owned context for one loop pass: every borrow the step executor needs,
/// bundled so the per-step helpers stay within the argument-count lint.
struct LoopCtx<'a> {
    db: &'a Database,
    agent_registry: &'a Arc<AgentRunRegistry>,
    task_registry: &'a Arc<TaskRegistry>,
    host: &'a Arc<dyn AgentRunHost>,
    emit: &'a Arc<dyn Fn(TaskEvent) + Send + Sync>,
    task_id: i64,
    conversation_id: i64,
    provider: &'a str,
    model: &'a str,
    config: &'a TaskRunConfig,
    /// Absolute wall-clock bound for the whole loop (NEX-AGENT-001 safety
    /// net): the step wait stops with [`RunOutcome::TimedOut`] past it.
    deadline: Instant,
}

/// How one step ended for the loop driver.
enum StepControl {
    /// The loop may advance to the next step.
    Continue,
    /// The loop must stop: terminal reason plus whether the task itself was
    /// cancelled (a task-level stop finalizes `cancelled`; anything else
    /// fails the task).
    Stop {
        /// Fixed-vocabulary reason recorded in the report.
        reason: &'static str,
        /// Whether the task itself was cancelled.
        cancelled: bool,
    },
}

impl LoopCtx<'_> {
    /// Execute one step: plan (mark the step, build its prompt) → act (one
    /// [`service::start_run`] agent run) → verify (poll the persisted run
    /// row to a terminal status) → record (persist the step result).
    fn execute_step(
        &self,
        step: &AgentTaskStep,
        seq: i64,
        step_number: usize,
        total: i64,
        prior: &mut Vec<String>,
    ) -> StepControl {
        let repo = AgentTaskRepository::new(self.db);
        let _ = repo.mark_step_running(self.task_id, seq);
        let _ = repo.mark_task_progress(self.task_id, seq, None);
        (self.emit)(TaskEvent::StepStarted {
            task_id: self.task_id,
            seq,
        });

        let prompt = build_step_prompt(
            &task_title(self.db, self.task_id),
            &step.title,
            step_number,
            total,
            prior,
        );
        let Ok(run_id) = service::start_run(
            self.db,
            Arc::clone(self.agent_registry),
            Arc::clone(self.host),
            Arc::clone(&self.config.executor),
            self.config.workspace_root.clone(),
            AgentRunRequest {
                conversation_id: self.conversation_id,
                user_request: prompt,
                provider: self.provider.to_string(),
                model: self.model.to_string(),
                credential: self.config.credential.clone(),
                max_iterations: None,
                spend_limit_micro_usd: self.config.spend_limit_micro_usd,
            },
            self.config.mode,
            self.config.preset,
        ) else {
            // `start_run` failures are pre-spawn only (unknown conversation,
            // busy conversation, provider/credential, persistence, thread
            // spawn): fixed vocabulary, never the classified provider text.
            self.record_failed(seq, None, "the step run could not be started");
            return StepControl::Stop {
                reason: "a step run could not be started",
                cancelled: false,
            };
        };

        self.task_registry.set_run_id(self.task_id, Some(run_id));
        let _ = repo.mark_task_progress(self.task_id, seq, Some(run_id));
        let outcome = wait_for_run_terminal(
            &AgentRunRepository::new(self.db),
            self.agent_registry,
            self.task_registry,
            self.task_id,
            run_id,
            self.deadline,
        );
        self.task_registry.set_run_id(self.task_id, None);
        self.apply_outcome(step, seq, run_id, outcome, prior)
    }

    /// Verify → record: persist one terminated step run's outcome and map it
    /// onto the loop control (completed continues; every other terminal
    /// stops the loop — budgets and spend guards are honored as stops, never
    /// retried or bypassed).
    fn apply_outcome(
        &self,
        step: &AgentTaskStep,
        seq: i64,
        run_id: i64,
        outcome: RunOutcome,
        prior: &mut Vec<String>,
    ) -> StepControl {
        match outcome {
            RunOutcome::Completed(content) => {
                let excerpt = truncate_chars(&content, STEP_RESULT_MAX_CHARS);
                let _ = AgentTaskRepository::new(self.db).mark_step_finished(
                    self.task_id,
                    seq,
                    "completed",
                    Some(&excerpt),
                    Some(run_id),
                );
                prior.push(format!(
                    "Step {seq} ({}): {}",
                    step.title,
                    truncate_chars(&content, REPORT_EXCERPT_MAX_CHARS)
                ));
                (self.emit)(TaskEvent::StepFinished {
                    task_id: self.task_id,
                    seq,
                    status: "completed".to_string(),
                });
                StepControl::Continue
            }
            RunOutcome::BudgetExhausted => {
                // The step budget parked the run: honored, never
                // auto-extended. The user extends the parked run via
                // `extend_agent_run` (its id is on the task row) or stops
                // the task; the loop itself stops here.
                self.record_failed(seq, Some(run_id), "the step stopped at the run step budget");
                StepControl::Stop {
                    reason: "stopped at the run step budget",
                    cancelled: false,
                }
            }
            RunOutcome::Parked => {
                // The step hit its iteration budget and parked awaiting a
                // budget decision; the loop never auto-extends, so stop here
                // instead of polling the `'running'` row forever
                // (NEX-AGENT-001). Cancel the parked run first: the wake
                // finalizes its persisted row and frees the conversation for
                // future runs. Re-check the park immediately before the
                // destructive cancel: an `extend_steps` landing in the
                // return→cancel gap clears the in-memory park and wakes the
                // runner with fresh allowance, and cancelling then would
                // kill a healthy extended run — keep polling to the fresh
                // terminal instead of stopping.
                if !self.agent_registry.is_budget_parked(run_id) {
                    self.task_registry.set_run_id(self.task_id, Some(run_id));
                    let outcome = wait_for_run_terminal(
                        &AgentRunRepository::new(self.db),
                        self.agent_registry,
                        self.task_registry,
                        self.task_id,
                        run_id,
                        self.deadline,
                    );
                    self.task_registry.set_run_id(self.task_id, None);
                    return self.apply_outcome(step, seq, run_id, outcome, prior);
                }
                let _ = self.agent_registry.cancel(run_id);
                self.record_failed(
                    seq,
                    Some(run_id),
                    "the step run parked at the run step budget",
                );
                StepControl::Stop {
                    reason: "stopped at the run step budget (parked)",
                    cancelled: false,
                }
            }
            RunOutcome::TimedOut => {
                // The overall task deadline tripped while the step run was
                // neither terminal nor observably parked. Cancel best-effort
                // (wakes budget and approval parks; a provider call blocked
                // mid-flight still runs to its request timeout) and stop
                // with an honest report.
                let _ = self.agent_registry.cancel(run_id);
                self.record_failed(
                    seq,
                    Some(run_id),
                    "the step run exceeded the task time bound",
                );
                StepControl::Stop {
                    reason: "exceeded the task time bound",
                    cancelled: false,
                }
            }
            RunOutcome::SpendLimited => {
                self.record_failed(seq, Some(run_id), "the step stopped at the spend limit");
                StepControl::Stop {
                    reason: "stopped at the spend limit",
                    cancelled: false,
                }
            }
            RunOutcome::Errored(message) => {
                self.record_failed(seq, Some(run_id), &message);
                StepControl::Stop {
                    reason: "a step run failed",
                    cancelled: false,
                }
            }
            RunOutcome::Cancelled => {
                let _ = AgentTaskRepository::new(self.db).mark_step_finished(
                    self.task_id,
                    seq,
                    "cancelled",
                    None,
                    Some(run_id),
                );
                (self.emit)(TaskEvent::StepFinished {
                    task_id: self.task_id,
                    seq,
                    status: "cancelled".to_string(),
                });
                // A task-level stop finalizes `cancelled`; an externally
                // cancelled run (e.g. from the conversation view) fails the
                // task instead.
                StepControl::Stop {
                    reason: "a step run was cancelled",
                    cancelled: self.task_registry.is_cancelled(self.task_id),
                }
            }
        }
    }

    /// Persist a failed step and emit its terminal frame.
    fn record_failed(&self, seq: i64, run_id: Option<i64>, message: &str) {
        let _ = AgentTaskRepository::new(self.db).mark_step_finished(
            self.task_id,
            seq,
            "failed",
            Some(message),
            run_id,
        );
        (self.emit)(TaskEvent::StepFinished {
            task_id: self.task_id,
            seq,
            status: "failed".to_string(),
        });
    }
}

/// The bounded autonomous loop proper (runs on the spawned thread):
/// plan (mark the step, build its prompt) → act (one
/// [`service::start_run`] agent run) → verify (poll the persisted run row to
/// a terminal status) → report (per-step result + composed task report).
/// At most `max_steps` steps execute; the rest are marked `skipped`.
// The loop thread receives one bundle per boundary by construction (the
// spawned-thread handoff mirrors `service::spawn_run`, which allows the
// same lint for the same reason).
#[allow(clippy::too_many_arguments)]
fn run_task_loop(
    db: &Database,
    agent_registry: &Arc<AgentRunRegistry>,
    task_registry: &Arc<TaskRegistry>,
    host: &Arc<dyn AgentRunHost>,
    emit: &Arc<dyn Fn(TaskEvent) + Send + Sync>,
    task_id: i64,
    conversation_id: i64,
    provider: &str,
    model: &str,
    steps: &[AgentTaskStep],
    config: &TaskRunConfig,
) {
    run_task_loop_with_deadline(
        db,
        agent_registry,
        task_registry,
        host,
        emit,
        task_id,
        conversation_id,
        provider,
        model,
        steps,
        config,
        Instant::now() + TASK_LOOP_DEADLINE,
    );
}

/// Deadline-parameterized [`run_task_loop`]: production passes the default
/// [`TASK_LOOP_DEADLINE`]; tests pass short bounds to prove the loop
/// terminates without waiting out the production bound.
// The loop thread receives one bundle per boundary by construction (the
// spawned-thread handoff mirrors `service::spawn_run`, which allows the
// same lint for the same reason).
#[allow(clippy::too_many_arguments)]
fn run_task_loop_with_deadline(
    db: &Database,
    agent_registry: &Arc<AgentRunRegistry>,
    task_registry: &Arc<TaskRegistry>,
    host: &Arc<dyn AgentRunHost>,
    emit: &Arc<dyn Fn(TaskEvent) + Send + Sync>,
    task_id: i64,
    conversation_id: i64,
    provider: &str,
    model: &str,
    steps: &[AgentTaskStep],
    config: &TaskRunConfig,
    deadline: Instant,
) {
    let repo = AgentTaskRepository::new(db);
    // Reset any previous attempt's step states, then bound the pass. The
    // runtime `min` is load-bearing: even a row whose `max_steps` predates a
    // bounds change can never execute more than TASK_MAX_STEPS steps.
    let _ = repo.reset_steps(task_id);
    let max_steps = steps
        .len()
        .min(task_max_steps(db, task_id))
        .min(usize::try_from(TASK_MAX_STEPS).unwrap_or(usize::MAX));
    let total = i64::try_from(steps.len()).unwrap_or(i64::MAX);
    if repo.mark_task_running(task_id, total).is_err() {
        return;
    }
    emit(TaskEvent::Started { task_id });

    let ctx = LoopCtx {
        db,
        agent_registry,
        task_registry,
        host,
        emit,
        task_id,
        conversation_id,
        provider,
        model,
        config,
        deadline,
    };
    let mut prior: Vec<String> = Vec::new();
    let mut stop_reason: Option<&str> = None;
    let mut task_cancelled = false;

    for (index, step) in steps.iter().take(max_steps).enumerate() {
        let seq = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if task_registry.is_cancelled(task_id) {
            task_cancelled = true;
            break;
        }
        // Overall wall-clock safety net (NEX-AGENT-001): never start another
        // step past the task deadline — stop with an honest report instead.
        if Instant::now() >= deadline {
            stop_reason = Some("exceeded the task time bound");
            break;
        }
        match ctx.execute_step(step, seq, index + 1, total, &mut prior) {
            StepControl::Continue => {}
            StepControl::Stop { reason, cancelled } => {
                stop_reason = Some(reason);
                task_cancelled = cancelled;
                break;
            }
        }
    }

    // Steps beyond the pass never execute: mark them `skipped` so the report
    // and the panel distinguish "not attempted" from "pending".
    for (index, step) in steps.iter().enumerate() {
        let seq = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if index >= max_steps || stop_reason.is_some() || task_cancelled {
            let current = repo
                .list_steps(task_id)
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.seq == seq))
                .map(|s| s.status);
            if current.as_deref() == Some("pending") {
                let _ = repo.mark_step_finished(task_id, step.seq, "skipped", None, None);
                emit(TaskEvent::StepFinished {
                    task_id,
                    seq,
                    status: "skipped".to_string(),
                });
            }
        }
    }

    let finished = repo.list_steps(task_id).unwrap_or_default();
    let status = if task_cancelled {
        "cancelled"
    } else if stop_reason.is_none() && finished.iter().all(|s| s.status == "completed") {
        "completed"
    } else {
        "failed"
    };
    let report = compose_report(
        &task_title(db, task_id),
        status,
        &finished,
        stop_reason,
        max_steps,
        steps.len(),
    );
    let _ = repo.finalize_task(task_id, status, &report);
    emit(TaskEvent::Finished {
        task_id,
        status: status.to_string(),
    });
}

/// Terminal outcome of one step's agent run, resolved by polling the
/// persisted `agent_runs` row. Content/error text come only from the
/// persisted row (classified, secret-free by the recorder's own mapping).
enum RunOutcome {
    /// `completed` with the final assistant text.
    Completed(String),
    /// `budget_exhausted`: the step budget parked the run (honored as a
    /// stop — the loop never auto-extends).
    BudgetExhausted,
    /// The step run parked at its iteration budget (persisted `'running'`
    /// until extended or cancelled — and the loop never auto-extends), so
    /// the wait stopped rather than polling forever (NEX-AGENT-001). The
    /// loop cancels the parked run and records the step failed.
    Parked,
    /// The overall task `deadline` tripped while the step run was neither
    /// terminal nor observably parked (NEX-AGENT-001 safety net). The loop
    /// cancels best-effort and records the step failed.
    TimedOut,
    /// `spend_limit_exceeded`: the spend guard tripped (honored as a stop).
    SpendLimited,
    /// `error` (or an unreadable row): classified error text.
    Errored(String),
    /// `cancelled`.
    Cancelled,
}

/// Poll the persisted run row until it leaves `running`. A requested task
/// stop aborts the in-flight run first (through
/// [`AgentRunRegistry::cancel`]); the poll then observes its `cancelled`
/// terminal like any other terminal.
///
/// Two NEX-AGENT-001 guards keep the poll bounded (terminal rows resolve
/// exactly as before — neither guard changes completed/budget/spend/
/// cancelled/error behavior):
///
/// - a step run parked at its iteration budget (persisted `'running'`
///   forever until extended or cancelled) surfaces as
///   [`RunOutcome::Parked`] once the in-memory park is confirmed across
///   [`PARK_CONFIRM_POLLS`] consecutive polls;
/// - the overall task `deadline` surfaces as [`RunOutcome::TimedOut`].
fn wait_for_run_terminal(
    runs: &AgentRunRepository<'_>,
    agent_registry: &Arc<AgentRunRegistry>,
    task_registry: &Arc<TaskRegistry>,
    task_id: i64,
    run_id: i64,
    deadline: Instant,
) -> RunOutcome {
    let mut read_errors: u32 = 0;
    let mut parked_polls: u32 = 0;
    loop {
        if task_registry.is_cancelled(task_id) {
            let _ = agent_registry.cancel(run_id);
        }
        if agent_registry.is_budget_parked(run_id) {
            parked_polls += 1;
            if parked_polls >= PARK_CONFIRM_POLLS {
                return RunOutcome::Parked;
            }
        } else {
            parked_polls = 0;
        }
        if Instant::now() >= deadline {
            return RunOutcome::TimedOut;
        }
        match runs.read_run(run_id) {
            Ok(Some(run)) => {
                read_errors = 0;
                match run.status.as_str() {
                    "completed" => {
                        return RunOutcome::Completed(run.final_content.unwrap_or_default());
                    }
                    "budget_exhausted" => return RunOutcome::BudgetExhausted,
                    "spend_limit_exceeded" => return RunOutcome::SpendLimited,
                    "cancelled" => return RunOutcome::Cancelled,
                    "error" => {
                        return RunOutcome::Errored(
                            run.error
                                .unwrap_or_else(|| "the step run failed".to_string()),
                        );
                    }
                    _ => {}
                }
            }
            Ok(None) => {
                return RunOutcome::Errored("the step run was not found".to_string());
            }
            Err(err) => {
                log::warn!("task {task_id}: run poll failed, continuing: {err}");
                read_errors += 1;
                if read_errors >= MAX_CONSECUTIVE_READ_ERRORS {
                    return RunOutcome::Errored("the step run could not be read".to_string());
                }
            }
        }
        std::thread::sleep(RUN_POLL_INTERVAL);
    }
}

/// Read the task's `max_steps` (falls back to the hard cap when the row is
/// unreadable — bounding first, failing never).
fn task_max_steps(db: &Database, task_id: i64) -> usize {
    let fallback = usize::try_from(TASK_MAX_STEPS).unwrap_or(usize::MAX);
    AgentTaskRepository::new(db)
        .read_task(task_id)
        .ok()
        .flatten()
        .map_or(fallback, |task| {
            usize::try_from(task.max_steps).unwrap_or(usize::MAX)
        })
}

/// Read the task title for prompts/reports (falls back to the id when the
/// row is unreadable — best-effort display text only).
fn task_title(db: &Database, task_id: i64) -> String {
    AgentTaskRepository::new(db)
        .read_task(task_id)
        .ok()
        .flatten()
        .map_or_else(|| format!("task {task_id}"), |task| task.title)
}

/// Build one step's agent prompt: the plan → act → verify → report loop in
/// miniature. The step runs inside the task's backing conversation, so prior
/// steps' persisted messages are already in context; `prior` carries only
/// the compacted excerpts of this loop's earlier steps.
fn build_step_prompt(
    task_title: &str,
    step_title: &str,
    step_number: usize,
    total_steps: i64,
    prior: &[String],
) -> String {
    let mut prompt = format!(
        "Autonomous task \"{task_title}\" — step {step_number} of {total_steps}: {step_title}.\n\
         Plan the step, act with the available tools, verify the outcome, then report back: \
         end your turn with a concise report of what this step achieved (or what blocked it)."
    );
    if !prior.is_empty() {
        prompt.push_str("\nEarlier steps this run completed:\n");
        for line in prior {
            prompt.push_str(line);
            prompt.push('\n');
        }
    }
    prompt
}

/// Compose the terminal task report: status line plus one line per step
/// (status + excerpt). Fixed vocabulary; step text is the loop's own
/// recorded results.
fn compose_report(
    title: &str,
    status: &str,
    steps: &[AgentTaskStep],
    stop_reason: Option<&str>,
    max_steps: usize,
    defined: usize,
) -> String {
    let done = steps.iter().filter(|s| s.status == "completed").count();
    let mut report = format!(
        "Task \"{title}\" — {status}: {done} of {} steps completed.",
        steps.len()
    );
    if let Some(reason) = stop_reason {
        let _ = write!(report, " Loop {reason}.");
    }
    if defined > max_steps {
        let _ = write!(
            report,
            " Step cap: executed {max_steps} of {defined} defined steps."
        );
    }
    for step in steps {
        let excerpt = step
            .result
            .as_deref()
            .map(|result| truncate_chars(result, REPORT_EXCERPT_MAX_CHARS))
            .unwrap_or_default();
        if excerpt.is_empty() {
            let _ = write!(report, "\n{}. [{}] {}", step.seq, step.status, step.title);
        } else {
            let _ = write!(
                report,
                "\n{}. [{}] {} — {}",
                step.seq, step.status, step.title, excerpt
            );
        }
    }
    report
}

/// Truncate text to `limit` characters (never splitting mid-scalar), marking
/// the cut. Empty input stays empty (no marker on untouched text).
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…[truncated]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use crate::application::agent::control::CancellationToken;
    use crate::application::execution::{AiRequest, AiResponse, ExecutorError};

    /// Scripted executor: pops one response per provider call, counts calls.
    struct ScriptedExecutor {
        steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
        calls: Mutex<usize>,
    }

    impl ScriptedExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("calls lock")
        }
    }

    impl ProviderExecutor for ScriptedExecutor {
        fn execute(
            &self,
            _request: &AiRequest,
            _credential: &str,
            _token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            *self.calls.lock().expect("calls lock") += 1;
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    /// No-op run host: task progress flows through `TaskEvent`, not run
    /// frames; assistant persistence still lands in the backing conversation
    /// through the real host in production (here: dropped).
    struct QuietHost;

    impl AgentRunHost for QuietHost {
        fn emit(&self, _frame: &super::super::service::RunFrame) {}
        fn persist_assistant_message(
            &self,
            _conversation_id: i64,
            _content: &str,
            _provider: &str,
            _model: &str,
        ) {
        }
    }

    fn text_response(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    fn priced_text_response(content: &str, input_tokens: u64, output_tokens: u64) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: Some(crate::application::execution::TokenUsage {
                input_tokens,
                output_tokens,
            }),
        }
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nexora-agent-task-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace dir");
        crate::application::agent::tools::test_support::canonical_workspace(&dir)
    }

    struct LoopHarness {
        db: Database,
        agent_registry: Arc<AgentRunRegistry>,
        task_registry: Arc<TaskRegistry>,
        host: Arc<dyn AgentRunHost>,
        events: Arc<Mutex<Vec<TaskEvent>>>,
        emit: Arc<dyn Fn(TaskEvent) + Send + Sync>,
        workspace: PathBuf,
        executor: Arc<ScriptedExecutor>,
    }

    impl LoopHarness {
        fn new(tag: &str, scripts: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            let db = crate::infrastructure::database::in_memory_database();
            let events: Arc<Mutex<Vec<TaskEvent>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&events);
            let emit: Arc<dyn Fn(TaskEvent) + Send + Sync> =
                Arc::new(move |event| sink.lock().expect("events lock").push(event));
            let executor = Arc::new(ScriptedExecutor::new(scripts));
            Self {
                db,
                agent_registry: Arc::new(AgentRunRegistry::default()),
                task_registry: Arc::new(TaskRegistry::default()),
                host: Arc::new(QuietHost),
                events,
                emit,
                workspace: temp_workspace(tag),
                executor,
            }
        }

        fn config(&self, spend_limit: Option<u64>) -> TaskRunConfig {
            TaskRunConfig {
                executor: Arc::clone(&self.executor) as Arc<dyn ProviderExecutor + Send + Sync>,
                workspace_root: self.workspace.clone(),
                credential: "sk-secret-test-credential".to_string(),
                mode: AutonomyMode::SemiAutonomous,
                preset: RunPreset::Coding,
                spend_limit_micro_usd: spend_limit,
            }
        }

        fn create_task(&self, steps: &[&str], max_steps: Option<i64>) -> i64 {
            TaskService::new(&self.db)
                .create_task(
                    "task",
                    Some("goal"),
                    None,
                    Some("openai"),
                    Some("test-model"),
                    &steps.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    max_steps,
                )
                .expect("create task")
        }

        /// Drive the asynchronous spawn synchronously: claim + join the loop
        /// thread by reimplementing the spawn's body would duplicate it, so
        /// instead spawn for real and poll the registry until released.
        fn run_to_completion(&self, task_id: i64, spend_limit: Option<u64>) {
            spawn_task_run(
                &self.db,
                Arc::clone(&self.agent_registry),
                Arc::clone(&self.task_registry),
                Arc::clone(&self.host),
                Arc::clone(&self.emit),
                task_id,
                self.config(spend_limit),
            )
            .expect("spawn");
            let start = std::time::Instant::now();
            while self.task_registry.is_active(task_id) {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(30),
                    "task loop must terminate"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn create_validates_titles_steps_and_caps() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let one = vec!["step".to_string()];

        assert!(service
            .create_task("", None, None, None, None, &one, None)
            .is_err());
        assert!(service
            .create_task(&"t".repeat(201), None, None, None, None, &one, None)
            .is_err());
        assert!(
            service
                .create_task("t", None, None, None, None, &[], None)
                .is_err(),
            "a task needs at least one step"
        );
        let many = vec!["s".to_string(); usize::try_from(TASK_MAX_STEPS).unwrap_or(usize::MAX) + 1];
        assert!(
            service
                .create_task("t", None, None, None, None, &many, None)
                .is_err(),
            "more than TASK_MAX_STEPS steps must be rejected"
        );
        assert!(service
            .create_task("t", None, None, None, None, &one, Some(0))
            .is_err());
        assert!(service
            .create_task("t", None, None, None, None, &one, Some(26))
            .is_err());
        assert!(
            service
                .create_task("t", None, None, None, None, &[String::new()], None)
                .is_err(),
            "empty step titles must be rejected"
        );
        let long_desc = "d".repeat(TASK_DESCRIPTION_MAX_LEN + 1);
        assert!(service
            .create_task("t", Some(&long_desc), None, None, None, &one, None)
            .is_err());
    }

    #[test]
    fn create_wires_a_backing_conversation_when_absent() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let id = service
            .create_task("t", None, None, None, None, &["s".to_string()], None)
            .expect("create");
        let task = service.read_task(id).expect("read");
        assert!(
            task.conversation_id.is_some(),
            "a backing conversation must be wired"
        );
    }

    #[test]
    fn update_rejects_running_tasks_and_delete_rejects_active_loops() {
        let harness = LoopHarness::new("guard", vec![Ok(text_response("done"))]);
        let service = TaskService::new(&harness.db);
        let id = harness.create_task(&["one"], None);
        service
            .update_task(id, "renamed", None)
            .expect("pending edit allowed");
        assert_eq!(service.read_task(id).expect("read").title, "renamed");

        // Simulate a running task row: edits and deletes are rejected while
        // the loop owns it.
        AgentTaskRepository::new(&harness.db)
            .mark_task_running(id, 1)
            .expect("mark running");
        assert!(service.update_task(id, "x", None).is_err());
        assert!(service.delete_task(id, &harness.task_registry).is_err());
        assert!(matches!(
            service.read_task(9999).expect_err("missing"),
            TaskError::TaskNotFound { .. }
        ));
    }

    #[test]
    fn duplicate_spawn_is_rejected() {
        let harness = LoopHarness::new("dup", vec![Ok(text_response("done"))]);
        let id = harness.create_task(&["one"], None);
        assert!(harness.task_registry.claim(id).is_some());
        let err = spawn_task_run(
            &harness.db,
            Arc::clone(&harness.agent_registry),
            Arc::clone(&harness.task_registry),
            Arc::clone(&harness.host),
            Arc::clone(&harness.emit),
            id,
            harness.config(None),
        )
        .expect_err("second claim must fail");
        assert!(matches!(err, TaskError::AlreadyRunning { .. }));
        harness.task_registry.release(id);
    }

    #[test]
    fn loop_completes_steps_and_composes_a_report() {
        let harness = LoopHarness::new(
            "happy",
            vec![
                Ok(text_response("first done")),
                Ok(text_response("second done")),
            ],
        );
        let id = harness.create_task(&["first", "second"], None);
        harness.run_to_completion(id, None);

        let service = TaskService::new(&harness.db);
        let task = service.read_task(id).expect("read");
        assert_eq!(task.status, "completed");
        assert_eq!(task.current_step, 2);
        assert_eq!(task.total_steps, 2);
        let report = task.report.expect("report");
        assert!(
            report.contains("completed: 2 of 2 steps completed"),
            "{report}"
        );
        assert!(report.contains("first done"), "{report}");

        let steps = service.list_steps(id).expect("steps");
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().all(|s| s.status == "completed"));
        assert_eq!(steps[0].result.as_deref(), Some("first done"));
        assert!(
            steps.iter().all(|s| s.run_id.is_some()),
            "each step links its run"
        );

        // Credential never enters an event frame.
        let serialized =
            serde_json::to_string(&*harness.events.lock().expect("lock")).expect("serialize");
        assert!(!serialized.contains("sk-secret-test-credential"));
        assert_eq!(harness.executor.calls(), 2);
        let _ = std::fs::remove_dir_all(temp_workspace("happy"));
    }

    #[test]
    fn loop_enforces_the_step_cap_and_skips_the_rest() {
        // Three defined steps with a cap of two: the third never executes.
        let harness = LoopHarness::new(
            "cap",
            vec![Ok(text_response("one done")), Ok(text_response("two done"))],
        );
        let id = harness.create_task(&["one", "two", "three"], Some(2));
        harness.run_to_completion(id, None);

        let service = TaskService::new(&harness.db);
        let task = service.read_task(id).expect("read");
        assert_eq!(task.status, "failed", "a cap-stopped task is not complete");
        assert!(
            task.report.as_deref().unwrap_or("").contains("Step cap"),
            "report notes the cap"
        );
        let steps = service.list_steps(id).expect("steps");
        assert_eq!(
            steps.iter().map(|s| s.status.as_str()).collect::<Vec<_>>(),
            vec!["completed", "completed", "skipped"]
        );
        assert_eq!(harness.executor.calls(), 2, "the capped step never runs");
        let _ = std::fs::remove_dir_all(temp_workspace("cap"));
    }

    #[test]
    fn loop_honors_the_spend_guard_without_bypass() {
        // One micro-USD limit against a priced turn: the spend guard trips
        // inside the existing run path, and the loop stops failed — it never
        // retries past the guard.
        let harness =
            LoopHarness::new("spend", vec![Ok(priced_text_response("rich answer", 1, 1))]);
        let id = harness.create_task(&["spend"], None);
        harness.run_to_completion(id, Some(1));

        let service = TaskService::new(&harness.db);
        let task = service.read_task(id).expect("read");
        assert_eq!(task.status, "failed");
        let steps = service.list_steps(id).expect("steps");
        assert_eq!(steps[0].status, "failed");
        assert!(
            steps[0]
                .result
                .as_deref()
                .unwrap_or("")
                .contains("spend limit"),
            "step records the guard trip, got {:?}",
            steps[0].result
        );
        assert_eq!(harness.executor.calls(), 1, "no retry past the guard");
        let _ = std::fs::remove_dir_all(temp_workspace("spend"));
    }

    #[test]
    fn loop_records_provider_errors_and_stops() {
        let harness = LoopHarness::new("err", vec![Err(ExecutorError::Failure)]);
        let id = harness.create_task(&["boom", "never"], None);
        harness.run_to_completion(id, None);

        let service = TaskService::new(&harness.db);
        let task = service.read_task(id).expect("read");
        assert_eq!(task.status, "failed");
        let steps = service.list_steps(id).expect("steps");
        assert_eq!(steps[0].status, "failed");
        assert_eq!(steps[1].status, "skipped");
        let _ = std::fs::remove_dir_all(temp_workspace("err"));
    }

    #[test]
    fn spawn_succeeds_after_orphan_sweep() {
        // NEX-TASK-001: a quit mid-task leaves `status='running'`, bricking
        // the task (spawn/update/delete all refuse). The startup sweep fails
        // the orphan so the task is runnable again.
        let harness = LoopHarness::new("sweep", vec![Ok(text_response("recovered"))]);
        let service = TaskService::new(&harness.db);
        let id = harness.create_task(&["one"], None);
        AgentTaskRepository::new(&harness.db)
            .mark_task_running(id, 1)
            .expect("simulate a crash mid-task");
        assert!(
            matches!(
                spawn_task_run(
                    &harness.db,
                    Arc::clone(&harness.agent_registry),
                    Arc::clone(&harness.task_registry),
                    Arc::clone(&harness.host),
                    Arc::clone(&harness.emit),
                    id,
                    harness.config(None),
                )
                .expect_err("orphaned running task must refuse spawn"),
                TaskError::AlreadyRunning { .. }
            ),
            "pre-sweep spawn must be refused"
        );
        let swept = AgentTaskRepository::new(&harness.db)
            .fail_orphaned_running_tasks("task interrupted by application shutdown")
            .expect("sweep");
        assert_eq!(swept, 1);
        assert_eq!(service.read_task(id).expect("read").status, "failed");
        harness.run_to_completion(id, None);
        assert_eq!(
            service.read_task(id).expect("read").status,
            "completed",
            "the swept task runs cleanly again"
        );
        let _ = std::fs::remove_dir_all(temp_workspace("sweep"));
    }

    /// One read-only tool-call response (auto-approved in `SemiAutonomous`):
    /// enough of these in a row exhaust the default step budget (10) and
    /// park the step run instead of terminating it.
    fn tool_list_response(id: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![crate::application::execution::ToolCall {
                id: id.to_string(),
                name: "list_directory".to_string(),
                arguments: "{}".to_string(),
                thought_signature: None,
            }],
            usage: None,
        }
    }

    #[test]
    fn loop_stops_when_step_parks_at_budget() {
        // NEX-AGENT-001 integration: more tool turns than the step budget
        // allows, so the step run parks at its iteration budget (persisted
        // `'running'` forever until extended or cancelled — and the loop
        // never auto-extends). The loop must surface the in-memory park and
        // stop with an honest report instead of polling forever.
        let scripts: Vec<Result<AiResponse, ExecutorError>> = (0..12)
            .map(|n| Ok(tool_list_response(&format!("park-{n}"))))
            .collect();
        let harness = LoopHarness::new("park", scripts);
        let id = harness.create_task(&["grind"], None);
        harness.run_to_completion(id, None);

        let service = TaskService::new(&harness.db);
        let task = service.read_task(id).expect("read");
        assert_eq!(task.status, "failed");
        assert!(
            task.report
                .as_deref()
                .unwrap_or("")
                .contains("run step budget"),
            "report stays honest about the budget, got {:?}",
            task.report
        );
        let steps = service.list_steps(id).expect("steps");
        assert_eq!(steps[0].status, "failed");
        assert!(
            steps[0].result.as_deref().unwrap_or("").contains("parked"),
            "step records the park, got {:?}",
            steps[0].result
        );
        assert_eq!(
            harness.executor.calls(),
            10,
            "the budget parks the run before an 11th turn"
        );

        // The loop cancels the parked run: its persisted row reaches a
        // terminal status (no leaked parked thread) and the conversation is
        // reusable for future runs.
        let run_id = steps[0].run_id.expect("step links its run");
        let start = std::time::Instant::now();
        let run = loop {
            let row = AgentRunRepository::new(&harness.db)
                .read_run(run_id)
                .expect("read run")
                .expect("exists");
            if row.status != "running" {
                break row;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the cancelled park must finalize promptly"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(run.status, "cancelled");
        let _ = std::fs::remove_dir_all(temp_workspace("park"));
    }

    #[test]
    fn parked_outcome_with_cleared_park_keeps_polling() {
        // Return→cancel race guard: a `Parked` outcome whose in-memory park
        // already cleared (an `extend_steps` landed in the gap) must not
        // cancel or stop — the wait resumes to the fresh terminal instead.
        // Deterministic: the run was never registered (never parked) and its
        // row is already `completed`, so the re-poll resolves immediately.
        let harness = LoopHarness::new("park-race", Vec::new());
        let id = harness.create_task(&["grind"], None);
        let steps = TaskService::new(&harness.db).list_steps(id).expect("steps");
        let conversation_id = TaskService::new(&harness.db)
            .read_task(id)
            .expect("read")
            .conversation_id
            .expect("conv");
        let run_id = AgentRunRepository::new(&harness.db)
            .create_run(None, "m", "supervised")
            .expect("run");
        AgentRunRepository::new(&harness.db)
            .finalize_run(
                run_id,
                "completed",
                1,
                Some("fresh result"),
                None,
                None,
                None,
            )
            .expect("finalize");
        let config = harness.config(None);
        let ctx = LoopCtx {
            db: &harness.db,
            agent_registry: &harness.agent_registry,
            task_registry: &harness.task_registry,
            host: &harness.host,
            emit: &harness.emit,
            task_id: id,
            conversation_id,
            provider: "openai",
            model: "test-model",
            config: &config,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        };
        let mut prior = Vec::new();
        let seq = steps[0].seq;
        match ctx.apply_outcome(&steps[0], seq, run_id, RunOutcome::Parked, &mut prior) {
            StepControl::Continue => {}
            StepControl::Stop { reason, .. } => {
                panic!("a cleared park must keep polling, not stop ({reason})")
            }
        }
        assert_eq!(prior.len(), 1, "the fresh terminal is recorded");
        let steps = TaskService::new(&harness.db).list_steps(id).expect("steps");
        assert_eq!(steps[0].status, "completed");
        let _ = std::fs::remove_dir_all(temp_workspace("park-race"));
    }

    /// Provider executor whose calls block forever: a step run that never
    /// terminates benignly and never parks observably.
    struct BlockingExecutor;

    impl ProviderExecutor for BlockingExecutor {
        fn execute(
            &self,
            _request: &AiRequest,
            _credential: &str,
            token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            // Block until the loop's deadline trips and cancels the run:
            // honour the token so the harness run thread exits in-test
            // instead of parking forever (one leaked thread + registry
            // entry per test-process run otherwise).
            while !token.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(ExecutorError::Failure)
        }
    }

    #[test]
    fn loop_enforces_overall_deadline() {
        // NEX-AGENT-001 safety net: the step run never terminates (provider
        // call blocks forever), so neither a persisted terminal nor a park
        // ever arrives. The overall task deadline must still bring the task
        // to a terminal status within a bounded time (short test cap — the
        // production bound is far longer).
        let db = crate::infrastructure::database::in_memory_database();
        let agent_registry = Arc::new(AgentRunRegistry::default());
        let task_registry = Arc::new(TaskRegistry::default());
        let host: Arc<dyn AgentRunHost> = Arc::new(QuietHost);
        let events: Arc<Mutex<Vec<TaskEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let emit: Arc<dyn Fn(TaskEvent) + Send + Sync> =
            Arc::new(move |event| sink.lock().expect("events lock").push(event));
        let workspace = temp_workspace("deadline");
        let id = TaskService::new(&db)
            .create_task(
                "task",
                Some("goal"),
                None,
                Some("openai"),
                Some("test-model"),
                &["stuck".to_string()],
                None,
            )
            .expect("create task");
        let config = TaskRunConfig {
            executor: Arc::new(BlockingExecutor),
            workspace_root: workspace.clone(),
            credential: "sk-secret-test-credential".to_string(),
            mode: AutonomyMode::SemiAutonomous,
            preset: RunPreset::Coding,
            spend_limit_micro_usd: None,
        };
        spawn_task_run_with_deadline(
            &db,
            Arc::clone(&agent_registry),
            Arc::clone(&task_registry),
            Arc::clone(&host),
            Arc::clone(&emit),
            id,
            config,
            std::time::Instant::now() + std::time::Duration::from_secs(3),
        )
        .expect("spawn");

        let start = std::time::Instant::now();
        let task = loop {
            let task = TaskService::new(&db).read_task(id).expect("read");
            if task.status == "failed" || task.status == "completed" || task.status == "cancelled" {
                break task;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(30),
                "the task loop must hit the deadline and terminate"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(task.status, "failed");
        assert!(
            task.report.as_deref().unwrap_or("").contains("time bound"),
            "report stays honest about the deadline, got {:?}",
            task.report
        );
        let start = std::time::Instant::now();
        while task_registry.is_active(id) {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the loop thread must release the task"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn wait_detects_budget_park_without_polling_forever() {
        // NEX-AGENT-001 unit probe: a run row stuck at `'running'` whose
        // control is budget-parked must surface `Parked` quickly — the
        // exact case that used to poll forever.
        use crate::application::agent::approval::ApprovalGate;
        use crate::application::agent::control::RunControl;
        use crate::application::agent::registry::ActiveAgentRun;

        let db = crate::infrastructure::database::in_memory_database();
        let runs = AgentRunRepository::new(&db);
        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        let agent_registry = Arc::new(AgentRunRegistry::default());
        let task_registry = Arc::new(TaskRegistry::default());
        let control = RunControl::new();
        agent_registry.register(
            run_id,
            ActiveAgentRun {
                conversation_id: 1,
                control: control.clone(),
                gate: ApprovalGate::new(AutonomyMode::Supervised),
            },
        );
        let parked = control.clone();
        let handle = std::thread::spawn(move || {
            assert!(!parked.wait_for_allowance(0, 0), "cancel ends park");
        });

        let start = std::time::Instant::now();
        while !control.is_budget_parked() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "the control must park"
            );
            std::thread::yield_now();
        }
        let outcome = wait_for_run_terminal(
            &runs,
            &agent_registry,
            &task_registry,
            7,
            run_id,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        assert!(
            matches!(outcome, RunOutcome::Parked),
            "a budget park must surface, not poll forever"
        );
        control.cancel();
        handle.join().expect("parked wait joins after cancel");
    }

    #[test]
    fn wait_enforces_deadline_on_stuck_running_row() {
        // NEX-AGENT-001 unit probe: a `'running'` row with no observable
        // park and an expired deadline returns immediately — never polls.
        let db = crate::infrastructure::database::in_memory_database();
        let runs = AgentRunRepository::new(&db);
        let run_id = runs.create_run(None, "m", "supervised").expect("run");
        let agent_registry = Arc::new(AgentRunRegistry::default());
        let task_registry = Arc::new(TaskRegistry::default());
        let outcome = wait_for_run_terminal(
            &runs,
            &agent_registry,
            &task_registry,
            7,
            run_id,
            std::time::Instant::now(),
        );
        assert!(
            matches!(outcome, RunOutcome::TimedOut),
            "an expired deadline must surface immediately"
        );
    }

    #[test]
    fn pre_cancelled_loop_runs_nothing_and_finalizes_cancelled() {
        let harness = LoopHarness::new("stop", vec![Ok(text_response("never"))]);
        let id = harness.create_task(&["one"], None);
        // Claim then cancel before the loop starts: the loop must observe the
        // flag at the first step boundary and run zero agent runs.
        assert!(harness.task_registry.claim(id).is_some());
        let _ = harness.task_registry.request_cancel(id);
        let db = harness.db.clone();
        let service = TaskService::new(&db);
        let conversation_id = service
            .read_task(id)
            .expect("read")
            .conversation_id
            .expect("conv");
        let steps = service.list_steps(id).expect("steps");
        let config = harness.config(None);
        run_task_loop(
            &db,
            &harness.agent_registry,
            &harness.task_registry,
            &harness.host,
            &harness.emit,
            id,
            conversation_id,
            "openai",
            "test-model",
            &steps,
            &config,
        );
        harness.task_registry.release(id);

        let service = TaskService::new(&harness.db);
        assert_eq!(service.read_task(id).expect("read").status, "cancelled");
        assert_eq!(
            harness.executor.calls(),
            0,
            "no agent run may start after stop"
        );
        let _ = std::fs::remove_dir_all(temp_workspace("stop"));
    }

    #[test]
    fn registry_cancel_sets_the_flag_and_returns_the_run() {
        let registry = TaskRegistry::default();
        assert!(!registry.is_active(1));
        assert_eq!(registry.request_cancel(1), (false, None));
        let entry = registry.claim(1).expect("claim");
        assert!(registry.is_active(1));
        assert!(!registry.is_cancelled(1));
        registry.set_run_id(1, Some(77));
        assert_eq!(registry.request_cancel(1), (true, Some(77)));
        assert!(registry.is_cancelled(1));
        assert!(registry.claim(1).is_none(), "claimed tasks reject re-claim");
        drop(entry);
        registry.release(1);
        assert!(!registry.is_active(1));
    }

    #[test]
    fn step_prompt_carries_plan_act_verify_report_shape() {
        let prompt = build_step_prompt("T", "do it", 1, 2, &[]);
        assert!(prompt.contains("step 1 of 2: do it"));
        assert!(prompt.contains("Plan"));
        assert!(prompt.contains("verify"));
        assert!(prompt.contains("report"));
    }

    #[test]
    fn truncate_marks_cuts_and_leaves_short_text() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        let cut = truncate_chars("abcdef", 3);
        assert_eq!(cut, "abc…[truncated]");
    }

    /// Static wiring check: every agent execution in the autonomous loop
    /// goes through the existing run-path bridge — the loop must never
    /// construct the runner directly (which would skip the approval, budget,
    /// and spend wiring the bridge owns). The runner needle is built with
    /// `concat!` so this test's own source never matches it verbatim.
    #[test]
    fn autonomous_loop_routes_all_agent_work_through_service_bridge() {
        const SOURCE: &str = include_str!("tasks.rs");
        // Count non-test occurrences: the production loop body must call the
        // bridge (the one spawn in `spawn_task_run` plus this assertion's own
        // literal are the only other mentions).
        let bridge_calls = SOURCE.matches("service::start_run(").count();
        assert!(
            bridge_calls >= 2,
            "the loop must drive agent work through service::start_run, found {bridge_calls}"
        );
        let runner_needle = concat!("AgentRunner", "::new");
        let runner_uses = SOURCE.matches(runner_needle).count();
        assert_eq!(
            runner_uses, 0,
            "the loop must never name the runner constructor (it would skip the bridge wiring)"
        );
    }
}
