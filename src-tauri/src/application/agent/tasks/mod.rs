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

mod prompts;
mod registry;
mod run_loop;
mod service;

use std::time::Duration;

use serde::Serialize;

use crate::infrastructure::database::DatabaseError;

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
// Kanban edit locks (P2 kanban+locks)
// ---------------------------------------------------------------------------

/// Default lock time-to-live in seconds. Every lock carries a deadline (the
/// v11 `CHECK (expires_at > acquired_at)`); the default keeps a forgotten
/// lock to two minutes, never forever (NEX-AGENT-004 lesson).
pub(crate) const TASK_LOCK_TTL_SECS: i64 = 120;

/// Shortest accepted lock TTL in seconds (heartbeats stay meaningful; a
/// sub-10s lock would expire before the UI can use it).
pub(crate) const TASK_LOCK_TTL_MIN_SECS: i64 = 10;

/// Longest accepted lock TTL in seconds (ten minutes caps how long a crashed
/// surface can block a task; steal-on-expiry always bounds the wait).
pub(crate) const TASK_LOCK_TTL_MAX_SECS: i64 = 600;

/// Longest accepted lock holder label (v11 `CHECK (length(holder) <= 64)`).
pub(crate) const TASK_LOCK_HOLDER_MAX_LEN: usize = 64;

/// Kanban-move vocabulary: the statuses a lock-guarded move may set. The
/// loop owns `'running'` (enter it only through `start_task_run`), so the
/// board never writes it — moving a running task is rejected outright.
const KANBAN_MOVE_STATUSES: [&str; 4] = ["pending", "completed", "failed", "cancelled"];

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
    /// A live edit lock by another holder blocks the acquisition. Carries
    /// only the task id, the holder label, and the deadline (Unix seconds)
    /// so the UI can report who holds the task and when the lock lapses.
    LockHeld {
        /// The contested task id.
        task_id: i64,
        /// The holder label on the live lock row.
        holder: String,
        /// The live row's deadline (Unix seconds).
        expires_at: i64,
    },
    /// The presented lock proof failed: no lock row, a token mismatch, or an
    /// expired row (fixed-vocabulary message — never the stored token).
    InvalidLock {
        /// What was wrong (fixed vocabulary, never caller content).
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
            Self::InvalidInput { message }
            | Self::RunStart { message }
            | Self::InvalidLock { message } => {
                write!(f, "{message}")
            }
            Self::LockHeld {
                task_id,
                holder,
                expires_at,
            } => {
                write!(f, "task {task_id} is locked by {holder} until {expires_at}")
            }
            Self::Database(err) => write!(f, "task persistence failed: {err}"),
        }
    }
}

// The re-exports keep every original single-file path
// (`crate::application::agent::tasks::<item>`) valid for `lib.rs` and the
// command layer; `unused_imports` is allowed because reachability — not a
// current consumer — is the point (mirrors the `#![allow(dead_code)]`
// rationale in `application/mod.rs`).
#[allow(unused_imports)]
pub(crate) use registry::{ActiveTaskRun, TaskRegistry};
#[allow(unused_imports)]
pub(crate) use run_loop::{spawn_task_run, spawn_task_run_with_deadline, TaskRunConfig};
#[allow(unused_imports)]
pub(crate) use service::TaskService;

// The loop-wait and truncation helpers moved to siblings; the tests below and
// the loop itself reach them through these imports (AC-207 split).
#[cfg(test)]
mod tests {
    use super::prompts::{build_step_prompt, truncate_chars};
    use super::run_loop::{run_task_loop, wait_for_run_terminal, LoopCtx, RunOutcome, StepControl};
    use super::*;
    use crate::application::agent::approval::AutonomyMode;
    use crate::application::agent::permissions::RunPreset;
    use crate::application::agent::service::{AgentRunHost, AgentRunRegistry};
    use crate::application::execution::ProviderExecutor;
    use crate::infrastructure::database::Database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use crate::infrastructure::repository::agent_tasks::AgentTaskRepository;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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

    fn lock_test_task(service: &TaskService<'_>) -> i64 {
        service
            .create_task("t", None, None, None, None, &["s".to_string()], None)
            .expect("task")
    }

    #[test]
    fn lock_acquire_denies_contention_with_holder_info() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let id = lock_test_task(&service);

        let first = service
            .acquire_lock(id, "kanban", None)
            .expect("first acquire");
        assert_eq!(first.holder, "kanban");
        assert!(!first.token.is_empty());
        assert!(first.expires_at > first.acquired_at);

        // Same holder re-enters idempotently with the same token, refreshing
        // the deadline so a re-acquire just before expiry hands back a
        // live lock rather than a nearly-dead one.
        let reentry = service.acquire_lock(id, "kanban", None).expect("re-entry");
        assert_eq!(reentry.token, first.token);
        assert!(
            reentry.expires_at >= first.expires_at,
            "re-entry must not hand back a deader lock"
        );

        // Another holder is denied with the live holder + deadline attached.
        let err = service
            .acquire_lock(id, "list", None)
            .expect_err("contended acquire");
        match err {
            TaskError::LockHeld {
                task_id,
                holder,
                expires_at,
            } => {
                assert_eq!(task_id, id);
                assert_eq!(holder, "kanban");
                assert_eq!(expires_at, reentry.expires_at);
            }
            other => panic!("expected LockHeld, got {other:?}"),
        }

        assert!(matches!(
            service
                .acquire_lock(9999, "kanban", None)
                .expect_err("missing"),
            TaskError::TaskNotFound { .. }
        ));
        assert!(
            service.acquire_lock(id, "  ", None).is_err(),
            "blank holder must be rejected"
        );
        assert!(
            service.acquire_lock(id, "kanban", Some(5)).is_err(),
            "TTL below 10s must be rejected"
        );
        assert!(
            service.acquire_lock(id, "kanban", Some(601)).is_err(),
            "TTL above 600s must be rejected"
        );
    }

    #[test]
    fn expired_lock_is_stealable_and_heartbeat_extends_live_locks() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let id = lock_test_task(&service);

        // Seed an already-expired row directly (no sleeping): the next
        // acquire must steal it rather than report contention.
        AgentTaskRepository::new(&db)
            .acquire_task_lock(id, "stale", "stale-token", 1000, 1010)
            .expect("seed expired");
        let stolen = service
            .acquire_lock(id, "kanban", None)
            .expect("steal after expiry");
        assert_eq!(stolen.holder, "kanban");
        assert_ne!(stolen.token, "stale-token");

        // Heartbeat extends the live lock.
        let refreshed = service
            .heartbeat_lock(id, &stolen.token, None)
            .expect("heartbeat");
        assert_eq!(refreshed.token, stolen.token);
        assert!(
            refreshed.expires_at >= stolen.expires_at,
            "heartbeat must not shorten the deadline"
        );

        // Wrong tokens and unknown tasks fail honestly.
        assert!(
            service.heartbeat_lock(id, "wrong-token", None).is_err(),
            "mismatched heartbeat must fail"
        );
        assert!(
            service.heartbeat_lock(9999, &stolen.token, None).is_err(),
            "heartbeat on a missing task must fail"
        );
    }

    #[test]
    fn release_and_move_require_the_live_token() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let id = lock_test_task(&service);

        // Moving without any lock changes nothing.
        assert!(
            service.move_task(id, "completed", "nope").is_err(),
            "move without a lock must fail"
        );
        assert_eq!(service.read_task(id).expect("read").status, "pending");

        let lock = service.acquire_lock(id, "kanban", None).expect("acquire");

        // Wrong token: rejected, status untouched.
        assert!(
            service.move_task(id, "completed", "wrong-token").is_err(),
            "move with a wrong token must fail"
        );
        assert_eq!(service.read_task(id).expect("read").status, "pending");

        // Unknown and loop-owned statuses are rejected even with a token.
        assert!(service.move_task(id, "transcended", &lock.token).is_err());
        assert!(service.move_task(id, "running", &lock.token).is_err());

        // Valid move persists.
        service
            .move_task(id, "completed", &lock.token)
            .expect("move");
        assert_eq!(service.read_task(id).expect("read").status, "completed");

        // Release with the wrong token fails; with the live token succeeds,
        // and the spent token no longer authorizes moves.
        assert!(service.release_lock(id, "wrong-token").is_err());
        service.release_lock(id, &lock.token).expect("release");
        assert!(
            service.move_task(id, "failed", &lock.token).is_err(),
            "a released token must not authorize moves"
        );
        assert!(service.release_lock(id, &lock.token).is_err());
    }

    #[test]
    fn move_rejects_tasks_owned_by_the_loop() {
        let db = crate::infrastructure::database::in_memory_database();
        let service = TaskService::new(&db);
        let id = lock_test_task(&service);
        AgentTaskRepository::new(&db)
            .mark_task_running(id, 1)
            .expect("mark running");
        let lock = service.acquire_lock(id, "kanban", None).expect("acquire");
        let err = service
            .move_task(id, "completed", &lock.token)
            .expect_err("move a running task");
        assert!(
            matches!(err, TaskError::InvalidInput { .. }),
            "loop-owned tasks stay out of the kanban write path"
        );
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
            .finalize_run(run_id, "completed", Some("fresh result"), None, None, None)
            .expect("finalize");
        let config = harness.config(None);
        let ctx = LoopCtx {
            access: crate::application::agent::access::AccessContext::root(&harness.db),
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
        const SOURCE: &str = include_str!("run_loop.rs");
        // AC-207 split: the guard scans `run_loop.rs` (the loop's home file),
        // so this assertion's own literal no longer self-counts — the needle
        // is built with `concat!` and the one real bridge call must remain.
        let bridge_needle = concat!("service::start", "_run_with_parent(");
        let bridge_calls = SOURCE.matches(bridge_needle).count();
        assert!(
            bridge_calls >= 1,
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
