//! Autonomous task loop: spawn, plan → act → verify → report, deadline.
//!
//! Split from `tasks.rs` (AC-207): the spawned loop thread, its
//! step-execution context, and the spawn/wait plumbing live here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::prompts::{build_step_prompt, compose_report, truncate_chars};
use super::registry::TaskRegistry;
use super::service::TaskService;
use super::{
    TaskError, TaskEvent, MAX_CONSECUTIVE_READ_ERRORS, PARK_CONFIRM_POLLS,
    REPORT_EXCERPT_MAX_CHARS, RUN_POLL_INTERVAL, STEP_RESULT_MAX_CHARS, TASK_LOOP_DEADLINE,
    TASK_MAX_STEPS,
};

use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::permissions::RunPreset;
use crate::application::agent::service::{self, AgentRunHost, AgentRunRegistry, AgentRunRequest};
use crate::application::execution::ProviderExecutor;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_runs::AgentRunRepository;
use crate::infrastructure::repository::agent_tasks::{AgentTaskRepository, AgentTaskStep};

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
    let track_registry = Arc::clone(&release_registry);
    let handle = std::thread::Builder::new()
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
    // NEX-TASK-002: retain the loop handle so orderly shutdown can join it.
    track_registry.track_thread(task_id, handle);
    Ok(())
}

/// Owned context for one loop pass: every borrow the step executor needs,
/// bundled so the per-step helpers stay within the argument-count lint.
pub(super) struct LoopCtx<'a> {
    pub(super) db: &'a Database,
    pub(super) agent_registry: &'a Arc<AgentRunRegistry>,
    pub(super) task_registry: &'a Arc<TaskRegistry>,
    pub(super) host: &'a Arc<dyn AgentRunHost>,
    pub(super) emit: &'a Arc<dyn Fn(TaskEvent) + Send + Sync>,
    pub(super) task_id: i64,
    pub(super) conversation_id: i64,
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) config: &'a TaskRunConfig,
    /// Absolute wall-clock bound for the whole loop (NEX-AGENT-001 safety
    /// net): the step wait stops with [`RunOutcome::TimedOut`] past it.
    pub(super) deadline: Instant,
}

/// How one step ended for the loop driver.
pub(super) enum StepControl {
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
    pub(super) fn apply_outcome(
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
pub(super) fn run_task_loop(
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
pub(super) enum RunOutcome {
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
pub(super) fn wait_for_run_terminal(
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
