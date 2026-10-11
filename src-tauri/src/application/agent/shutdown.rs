//! Orderly agent-subsystem shutdown (NEX-TASK-002): cancel + join on exit.
//!
//! Run/forwarder/task-loop threads are spawned detached by design (the agent
//! engine is std-thread based), but their [`JoinHandle`](std::thread::JoinHandle)s
//! are retained in the registries, so quitting mid-run can unwind honestly
//! instead of leaving persisted rows stuck at `running`:
//!
//! 1. cancel the terminal session (its executor kills the in-flight child
//!    through the shared token — no orphans — reusing the existing
//!    `cancel_active` path);
//! 2. shutdown-cancel every active task loop (its in-flight step run is
//!    shutdown-cancelled through the agent registry, the same
//!    cancellation-token path as `stop_task_run`);
//! 3. shutdown-cancel every remaining active agent run (covers direct runs
//!    and any step run whose task already released — idempotent);
//! 4. join the retained task-loop handles, then the run/forwarder handles,
//!    against one shared deadline.
//!
//! The bound: [`SHUTDOWN_JOIN_DEADLINE`] caps the whole join (seconds-scale,
//! then the quit proceeds — a stuck thread can delay shutdown by at most
//! the deadline, never hang it). The startup sweep stays the crash fallback
//! for anything that never unwound (a panicked thread, a kill -9).
//!
//! Outcome vocabulary: shutdown-cancel flows through the existing `Cancelled`
//! plumbing, so run rows finalize `cancelled` and task rows finalize
//! `cancelled` exactly like a user stop — except parked approvals, which
//! observe the shutdown origin on the shared token and record
//! `approval cancelled by application shutdown; run cancelled`
//! (`decided_by='system'`), never the user-cancel text (same honesty rule
//! as the NEX-AGENT-004 park deadline).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::registry::AgentRunRegistry;
use super::tasks::TaskRegistry;
use crate::application::terminal::TerminalRegistry;

/// How long orderly shutdown waits for run/task threads to unwind after
/// cancelling them (NEX-TASK-002 bounded quit). Seconds-scale on purpose:
/// cooperative cancellation unwinds a parked run in milliseconds (token +
/// condvar wake) and a mid-tool run within one executor poll; the deadline
/// only binds a genuinely stuck thread — and then the quit proceeds anyway.
pub(crate) const SHUTDOWN_JOIN_DEADLINE: Duration = Duration::from_secs(5);

/// Outcome of one [`shutdown_agent_subsystem`] pass. All counts, no content:
/// secret-free by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShutdownSummary {
    /// Active agent runs that were shutdown-cancelled.
    pub runs_cancelled: usize,
    /// Active task loops that were shutdown-cancelled.
    pub tasks_cancelled: usize,
    /// Whether a terminal session was active and cancelled (its child is
    /// killed through the shared token by the existing cancel path).
    pub terminal_cancelled: bool,
    /// Retained run/forwarder handles still live past the deadline.
    pub runs_unjoined: usize,
    /// Retained task-loop handles still live past the deadline.
    pub tasks_unjoined: usize,
}

/// Cancel every active run/task/terminal session and join the retained
/// threads with a bounded deadline (NEX-TASK-002 orderly path).
///
/// Never blocks past `deadline`; never panics (a missing registry entry
/// between snapshot and cancel is just a finished thread). Each thread
/// finalizes its own persisted rows on the cancel unwind, so after this
/// returns with zero unjoined handles no `agent_runs`/`agent_tasks` row is
/// left `running` by an orderly quit.
pub(crate) fn shutdown_agent_subsystem(
    agent_registry: &AgentRunRegistry,
    task_registry: &TaskRegistry,
    terminal_registry: &TerminalRegistry,
    deadline: Duration,
) -> ShutdownSummary {
    let quit_by = Instant::now() + deadline;

    // 1. Terminal first: kill the in-flight child (if any) through the
    // shared token so it cannot outlive the quit.
    let terminal_cancelled = terminal_registry.cancel_active();

    // 2. Task loops: flag each loop and shutdown-cancel its in-flight step
    // run (when the snapshot names one). The loop's terminal-wait observes
    // the flag and unwinds to a `cancelled` task row.
    let mut tasks_cancelled = 0;
    for (_task_id, run_id) in task_registry.request_shutdown_cancel_all() {
        tasks_cancelled += 1;
        if let Some(run_id) = run_id {
            let _ = agent_registry.shutdown_cancel(run_id);
        }
    }

    // 3. Remaining agent runs (direct runs plus any step run whose task
    // snapshot missed it — shutdown-cancel is idempotent).
    let runs_cancelled = agent_registry.shutdown_cancel_all();

    // 4. Join with the shared bound: task loops depend on their step runs'
    // rows finalizing, and runs finalize on the cancel unwind, so join runs
    // before the loops that poll them.
    let runs_unjoined = agent_registry.join_threads_until(quit_by);
    let tasks_unjoined = task_registry.join_threads_until(quit_by);

    ShutdownSummary {
        runs_cancelled,
        tasks_cancelled,
        terminal_cancelled,
        runs_unjoined,
        tasks_unjoined,
    }
}

/// Convenience wrapper for the Tauri exit handler: the managed registries
/// are held as `Arc`s, and [`SHUTDOWN_JOIN_DEADLINE`] is the production
/// bound. Returns the pass summary for the shutdown log line.
pub(crate) fn shutdown_managed(
    agent_registry: &Arc<AgentRunRegistry>,
    task_registry: &Arc<TaskRegistry>,
    terminal_registry: &Arc<TerminalRegistry>,
) -> ShutdownSummary {
    shutdown_agent_subsystem(
        agent_registry,
        task_registry,
        terminal_registry,
        SHUTDOWN_JOIN_DEADLINE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Mutex;

    use super::super::approval::AutonomyMode;
    use super::super::control::{AgentRunEvent, CancellationToken};
    use super::super::permissions::RunPreset;
    use super::super::service::{self, AgentRunHost, AgentRunRegistry, AgentRunRequest, RunFrame};
    use super::super::tasks::{spawn_task_run, TaskRegistry, TaskRunConfig, TaskService};
    use crate::application::conversations::ConversationService;
    use crate::application::execution::{
        AiRequest, AiResponse, ExecutorError, ProviderExecutor, ToolCall,
    };
    use crate::application::terminal::TerminalRegistry;
    use crate::infrastructure::database::Database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;

    /// Executor that blocks inside the provider call until the run token
    /// fires, then reports the cancellation honestly (the runner maps this
    /// to `AgentError::Cancelled`, exactly like a mid-flight provider call
    /// aborted by a user cancel).
    struct BlockingExecutor;

    impl ProviderExecutor for BlockingExecutor {
        fn execute(
            &self,
            _request: &AiRequest,
            _credential: &str,
            token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            while !token.is_cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(ExecutorError::Cancelled)
        }
    }

    /// Scripted executor: pops one response per provider call.
    struct ScriptedExecutor {
        steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
    }

    impl ScriptedExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
            }
        }
    }

    impl ProviderExecutor for ScriptedExecutor {
        fn execute(
            &self,
            _request: &AiRequest,
            _credential: &str,
            _token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    /// Fake host: records emitted frames on a channel for assertions.
    struct FakeHost {
        frames_tx: Sender<RunFrame>,
    }

    impl AgentRunHost for FakeHost {
        fn emit(&self, frame: &RunFrame) {
            let _ = self.frames_tx.send(frame.clone());
        }

        fn persist_assistant_message(
            &self,
            _conversation_id: i64,
            _content: &str,
            _provider: &str,
            _model: &str,
        ) {
        }
    }

    /// No-op run host for task tests: task progress flows through the
    /// task-event sink, not run frames.
    struct QuietHost;

    impl AgentRunHost for QuietHost {
        fn emit(&self, _frame: &RunFrame) {}
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

    fn tool_response(name: &str, tool: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: format!("{name}-1"),
                name: tool.to_string(),
                arguments: "{}".to_string(),
                thought_signature: None,
            }],
            usage: None,
        }
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexora-agent-shutdown-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace dir");
        crate::application::agent::tools::test_support::canonical_workspace(&dir)
    }

    fn request(conversation_id: i64, content: &str) -> AgentRunRequest {
        AgentRunRequest {
            conversation_id,
            user_request: content.to_string(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            credential: "sk-secret-test-credential".to_string(),
            max_iterations: None,
            spend_limit_micro_usd: None,
        }
    }

    /// Generous shutdown bound for tests: long enough that a cooperative
    /// thread always unwinds first, so the assertions observe terminal
    /// states — never a bound on the thing being measured. Outcome asserts
    /// only, never `elapsed`.
    const TEST_SHUTDOWN_BOUND: Duration = Duration::from_secs(10);

    fn collect_finished(
        rx: &Receiver<RunFrame>,
        run_id: i64,
    ) -> super::super::service::RunFinished {
        // Generous receive bound (prior-task pattern): the run already
        // unwound under shutdown; this only drains the buffered frames.
        for _ in 0..1000 {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(RunFrame::Finished { run_id: id, event }) if id == run_id => return event,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("run {run_id}: no Finished frame after shutdown");
    }

    fn wait_for_approval_park(rx: &Receiver<RunFrame>, run_id: i64) {
        // Generous receive bound (prior-task pattern): asserts the park
        // happened, never how fast.
        for _ in 0..1000 {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(RunFrame::Governance {
                    run_id: id,
                    event: AgentRunEvent::ApprovalRequested { .. },
                }) if id == run_id => return,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("run {run_id}: never parked for approval");
    }

    fn run_status(db: &Database, run_id: i64) -> String {
        AgentRunRepository::new(db)
            .read_run(run_id)
            .expect("read run")
            .expect("run row must exist")
            .status
    }

    /// NEX-TASK-002 acceptance: start a run, trigger shutdown, assert the
    /// run row is terminal and the retained threads joined.
    #[test]
    fn shutdown_cancels_blocked_run_and_joins_threads() {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace("blocked-run");
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost { frames_tx: tx });
        let agent = Arc::new(AgentRunRegistry::default());
        let tasks = Arc::new(TaskRegistry::default());
        let terminal = Arc::new(TerminalRegistry::new());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = service::start_run(
            &db,
            Arc::clone(&agent),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(BlockingExecutor),
            workspace,
            request(conversation_id, "block me"),
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        assert!(
            agent.is_active(run_id),
            "run must be active before shutdown"
        );
        assert_eq!(
            agent.tracked_thread_count(),
            2,
            "run + forwarder handles retained"
        );

        let summary = shutdown_agent_subsystem(&agent, &tasks, &terminal, TEST_SHUTDOWN_BOUND);

        assert_eq!(summary.runs_cancelled, 1);
        assert_eq!(summary.tasks_cancelled, 0);
        assert!(!summary.terminal_cancelled);
        assert_eq!(
            summary.runs_unjoined, 0,
            "run + forwarder threads must join"
        );
        assert_eq!(summary.tasks_unjoined, 0);
        assert!(
            !agent.is_active(run_id),
            "registry entry released on the shutdown exit path"
        );
        assert_eq!(agent.tracked_thread_count(), 0, "finished handles reaped");
        let finished = collect_finished(&rx, run_id);
        assert_eq!(finished.status, "cancelled");
        assert_eq!(
            run_status(&db, run_id),
            "cancelled",
            "run row must be terminal"
        );
    }

    /// Shutdown-cancel through an approval park records the shutdown
    /// vocabulary (`decided_by='system'`, observation naming the shutdown) —
    /// never the user-cancel text (same honesty rule as NEX-AGENT-004).
    #[test]
    fn shutdown_releases_approval_park_with_shutdown_vocabulary() {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace("approval-park");
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost { frames_tx: tx });
        let agent = Arc::new(AgentRunRegistry::default());
        let tasks = Arc::new(TaskRegistry::default());
        let terminal = Arc::new(TerminalRegistry::new());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = service::start_run(
            &db,
            Arc::clone(&agent),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("never reached")),
            ])),
            workspace,
            request(conversation_id, "park me"),
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        wait_for_approval_park(&rx, run_id);

        let summary = shutdown_agent_subsystem(&agent, &tasks, &terminal, TEST_SHUTDOWN_BOUND);

        assert_eq!(summary.runs_cancelled, 1);
        assert_eq!(summary.runs_unjoined, 0);
        let finished = collect_finished(&rx, run_id);
        assert_eq!(finished.status, "cancelled");
        assert_eq!(run_status(&db, run_id), "cancelled");
        let steps = service::list_steps_for_run(&db, run_id).expect("steps");
        let approval = steps
            .iter()
            .find(|step| step.kind == "approval")
            .expect("shutdown records an approval step");
        assert_eq!(approval.status.as_deref(), Some("cancelled"));
        assert_eq!(approval.decided_by.as_deref(), Some("system"));
        let observation = approval.observation.as_deref().unwrap_or_default();
        assert!(
            observation.contains("shutdown"),
            "shutdown step must name the shutdown, got: {observation}"
        );
        assert!(
            !observation.contains("by the user"),
            "shutdown step must not claim a user cancel, got: {observation}"
        );
    }

    /// Shutdown-cancel releases a budget park too (NEX-TASK-002 complements
    /// the #119 park deadline: parks must also unwind on shutdown-cancel).
    #[test]
    fn shutdown_releases_budget_park() {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace("budget-park");
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost { frames_tx: tx });
        let agent = Arc::new(AgentRunRegistry::default());
        let tasks = Arc::new(TaskRegistry::default());
        let terminal = Arc::new(TerminalRegistry::new());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let mut parked_request = request(conversation_id, "park on budget");
        parked_request.max_iterations = Some(0);
        let run_id = service::start_run(
            &db,
            Arc::clone(&agent),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("never"))])),
            workspace,
            parked_request,
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        // Bounded wait for the in-memory park (fixed polls, outcome assert —
        // never `elapsed`).
        let mut parked = false;
        for _ in 0..500 {
            if agent.is_budget_parked(run_id) {
                parked = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(parked, "run must reach the budget park before shutdown");

        let summary = shutdown_agent_subsystem(&agent, &tasks, &terminal, TEST_SHUTDOWN_BOUND);

        assert_eq!(summary.runs_cancelled, 1);
        assert_eq!(summary.runs_unjoined, 0);
        let finished = collect_finished(&rx, run_id);
        assert_eq!(finished.status, "cancelled");
        assert_eq!(run_status(&db, run_id), "cancelled");
    }

    /// #150 Ask-always-parks: several parks outstanding at quit must all
    /// unwind — shutdown must not hang on them and none may stay `running`.
    #[test]
    fn shutdown_cancels_every_parked_run() {
        let db = crate::infrastructure::database::in_memory_database();
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost { frames_tx: tx });
        let agent = Arc::new(AgentRunRegistry::default());
        let tasks = Arc::new(TaskRegistry::default());
        let terminal = Arc::new(TerminalRegistry::new());
        let mut run_ids = Vec::new();
        for (index, tag) in ["park-a", "park-b"].iter().enumerate() {
            let conversation_id = ConversationService::new(&db)
                .create(format!("conv-{index}").as_str())
                .expect("conversation");
            let run_id = service::start_run(
                &db,
                Arc::clone(&agent),
                Arc::clone(&host) as Arc<dyn AgentRunHost>,
                Arc::new(ScriptedExecutor::new(vec![
                    Ok(tool_response("w", "write_file")),
                    Ok(text_response("never reached")),
                ])),
                temp_workspace(tag),
                request(conversation_id, "park me"),
                AutonomyMode::SemiAutonomous,
                RunPreset::Coding,
            )
            .expect("start");
            run_ids.push(run_id);
        }
        for run_id in &run_ids {
            wait_for_approval_park(&rx, *run_id);
        }

        let summary = shutdown_agent_subsystem(&agent, &tasks, &terminal, TEST_SHUTDOWN_BOUND);

        assert_eq!(summary.runs_cancelled, 2);
        assert_eq!(summary.runs_unjoined, 0);
        assert_eq!(agent.tracked_thread_count(), 0);
        // Drain once for both runs: each run's forwarder emits its own
        // `Finished`, and the frames can interleave, so a per-run drain
        // could consume (and discard) the other run's terminal frame.
        let mut finished: HashMap<i64, super::super::service::RunFinished> = HashMap::new();
        for _ in 0..2000 {
            if finished.len() == run_ids.len() {
                break;
            }
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(RunFrame::Finished { run_id, event }) => {
                    finished.insert(run_id, event);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            finished.len(),
            run_ids.len(),
            "both runs must emit Finished after shutdown"
        );
        for run_id in run_ids {
            let event = finished.get(&run_id).expect("Finished frame");
            assert_eq!(event.status, "cancelled");
            assert_eq!(run_status(&db, run_id), "cancelled");
        }
    }

    /// Task rows are not stuck at `running` after orderly shutdown: the
    /// loop unwinds to `cancelled` and its thread joins.
    #[test]
    fn shutdown_finalizes_task_loop_cancelled() {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace("task-shutdown");
        let agent = Arc::new(AgentRunRegistry::default());
        let tasks = Arc::new(TaskRegistry::default());
        let terminal = Arc::new(TerminalRegistry::new());
        let host: Arc<dyn AgentRunHost> = Arc::new(QuietHost);
        let sink: Arc<dyn Fn(super::super::tasks::TaskEvent) + Send + Sync> = Arc::new(|_| {});
        let task_id = TaskService::new(&db)
            .create_task(
                "task",
                Some("goal"),
                None,
                Some("openai"),
                Some("test-model"),
                &["step one".to_string()],
                None,
            )
            .expect("create task");
        spawn_task_run(
            &db,
            Arc::clone(&agent),
            Arc::clone(&tasks),
            Arc::clone(&host),
            Arc::clone(&sink),
            task_id,
            TaskRunConfig {
                executor: Arc::new(BlockingExecutor) as Arc<dyn ProviderExecutor + Send + Sync>,
                workspace_root: workspace,
                credential: "sk-secret-test-credential".to_string(),
                mode: AutonomyMode::SemiAutonomous,
                preset: RunPreset::Coding,
                spend_limit_micro_usd: None,
            },
        )
        .expect("spawn");
        // Bounded wait until the loop owns the task (fixed polls, outcome
        // assert — never `elapsed`).
        let mut running = false;
        for _ in 0..500 {
            if TaskService::new(&db)
                .read_task(task_id)
                .expect("read task")
                .status
                == "running"
            {
                running = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(running, "task must be running before shutdown");
        assert!(tasks.is_active(task_id));
        assert_eq!(tasks.tracked_thread_count(), 1, "loop handle retained");

        let summary = shutdown_agent_subsystem(&agent, &tasks, &terminal, TEST_SHUTDOWN_BOUND);

        assert_eq!(summary.tasks_cancelled, 1);
        assert_eq!(summary.tasks_unjoined, 0, "loop thread must join");
        assert_eq!(summary.runs_unjoined, 0);
        assert!(!tasks.is_active(task_id));
        assert_eq!(tasks.tracked_thread_count(), 0);
        let task = TaskService::new(&db).read_task(task_id).expect("read task");
        assert_eq!(task.status, "cancelled", "task row must be terminal");
        let steps = TaskService::new(&db).list_steps(task_id).expect("steps");
        assert!(
            steps
                .iter()
                .all(|step| step.status != "pending" && step.status != "running"),
            "no step may be stuck non-terminal"
        );
        if let Some(conversation_id) = task.conversation_id {
            for run in AgentRunRepository::new(&db)
                .list_runs_by_conversation(conversation_id)
                .expect("runs")
            {
                assert_ne!(
                    run.status, "running",
                    "no step run may be stuck at running after shutdown"
                );
            }
        }
    }

    /// A stuck thread (one that ignores cancellation) cannot hang the quit:
    /// the bounded join still returns and reports the unjoined handle. The
    /// assertion is on the outcome (returned + reported), never on `elapsed`.
    #[test]
    fn stuck_thread_does_not_hang_shutdown() {
        let agent = AgentRunRegistry::default();
        let tasks = TaskRegistry::default();
        let terminal = TerminalRegistry::new();
        let (stuck_tx, stuck_rx) = mpsc::channel::<()>();
        let stuck = std::thread::Builder::new()
            .name("shutdown-test-stuck".to_string())
            .spawn(move || {
                // Ignores cancellation entirely: blocks until the sender is
                // dropped, which happens only after shutdown returned.
                let _ = stuck_rx.recv();
            })
            .expect("spawn stuck thread");
        agent.track_threads(7, vec![stuck]);

        let summary =
            shutdown_agent_subsystem(&agent, &tasks, &terminal, Duration::from_millis(250));

        assert_eq!(summary.runs_unjoined, 1, "stuck handle must be reported");
        assert_eq!(summary.tasks_unjoined, 0);
        // Release the stuck thread; its detached handle dies with the test
        // process if it is still unwinding.
        drop(stuck_tx);
    }
}
