//! Stress autonomy suite (Task 6.2).
//!
//! Proves the autonomy stack (runner + governance + approval gate + bridge +
//! persistence) holds under sustained and concurrent load, using the same
//! real-stack doctrine as the Task 6.1 e2e suite: real file-backed `SQLite`
//! with
//! real migrations, real `ConversationService`, real `ToolRegistry` against a
//! real temporary workspace, real settings, real run/forwarder threads, and
//! the real spend-guard pricing — with fakes only for the provider executor
//! (deterministic script) and the event host (in-process channel).
//!
//! Every test is prefixed `stress_` so `cargo test stress_` selects exactly
//! this suite. Every wait is hard-bounded (recv deadlines / loop deadlines),
//! every temp root is unique per test (parallel-safe by construction), and no
//! test touches the network.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::control::AgentRunEvent;
use crate::application::agent::pricing::cost_for_usage;
use crate::application::agent::service::{
    start_run, AgentRunHost, AgentRunRegistry, AgentRunRequest, ResolveOutcome, RunFinished,
    RunFrame,
};
use crate::application::conversations::ConversationService;
use crate::application::execution::{
    AiRequest, AiResponse, ExecutorError, ProviderExecutor, TokenUsage, ToolCall,
};
use crate::infrastructure::database::{open, Database};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;

// ---------------------------------------------------------------------------
// Shared counter for unique temp paths (parallel-safe)
// ---------------------------------------------------------------------------

static STRESS_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_suffix(tag: &str) -> String {
    let id = STRESS_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!("{tag}-{}-{}-{}", std::process::id(), id, nanos)
}

fn stress_db(tag: &str) -> (Database, PathBuf) {
    let dir = std::env::temp_dir();
    let name = format!("nexora-stress-db-{}.db", unique_suffix(tag));
    let path = dir.join(name);
    let conn = open(&path).expect("open file-backed stress database");
    (Database::new(conn), path)
}

fn stress_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nexora-stress-ws-{}", unique_suffix(tag)));
    std::fs::create_dir_all(&dir).expect("create stress workspace");
    dir
}

fn cleanup_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm", ".wal", ".shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
    }
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

fn cleanup_workspace(ws: &Path) {
    let _ = std::fs::remove_dir_all(ws);
}

fn create_conversation(db: &Database, title: &str) -> i64 {
    ConversationService::new(db)
        .create(title)
        .expect("create conversation")
}

// ---------------------------------------------------------------------------
// Deterministic scripted provider executor (no network)
// ---------------------------------------------------------------------------

struct ScriptedExecutor {
    steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
    requests: Mutex<Vec<AiRequest>>,
}

impl ScriptedExecutor {
    fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }
}

impl ProviderExecutor for ScriptedExecutor {
    fn execute(&self, request: &AiRequest, _credential: &str) -> Result<AiResponse, ExecutorError> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        self.steps
            .lock()
            .expect("steps lock")
            .pop_front()
            .expect("scripted executor exhausted")
    }
}

// ---------------------------------------------------------------------------
// In-process frame host (fakes only the Tauri emission, keeps persistence real)
// ---------------------------------------------------------------------------

struct StressHost {
    frames_tx: std::sync::mpsc::Sender<RunFrame>,
    db: Database,
}

impl AgentRunHost for StressHost {
    fn emit(&self, frame: &RunFrame) {
        let _ = self.frames_tx.send(frame.clone());
    }

    fn persist_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    ) {
        let _ = ConversationService::new(&self.db).persist_assistant_message(
            conversation_id,
            content,
            provider,
            model,
        );
    }
}

// ---------------------------------------------------------------------------
// AiResponse factories
// ---------------------------------------------------------------------------

fn usage_of(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
    }
}

fn text_response(content: &str, usage: Option<TokenUsage>) -> AiResponse {
    AiResponse {
        content: content.to_string(),
        model: "test-model".to_string(),
        tool_calls: Vec::new(),
        usage,
    }
}

#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
fn tool_response(
    id: &str,
    name: &str,
    args: serde_json::Value,
    usage: Option<TokenUsage>,
) -> Result<AiResponse, ExecutorError> {
    Ok(AiResponse {
        content: String::new(),
        model: "test-model".to_string(),
        tool_calls: vec![ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }],
        usage,
    })
}

// ---------------------------------------------------------------------------
// Frame helpers (every wait hard-bounded)
// ---------------------------------------------------------------------------

fn recv_deadline(rx: &Receiver<RunFrame>, deadline: Instant) -> RunFrame {
    let Some(budget) = deadline.checked_duration_since(Instant::now()) else {
        panic!("timed out waiting for a run frame (deadline exceeded)");
    };
    rx.recv_timeout(budget)
        .expect("run event channel dropped before a frame arrived")
}

fn wait_finished_bounded(rx: &Receiver<RunFrame>, bound: Duration) -> (RunFinished, Duration) {
    let start = Instant::now();
    let deadline = start + bound;
    loop {
        let frame = recv_deadline(rx, deadline);
        if let RunFrame::Finished { event, .. } = frame {
            return (event, start.elapsed());
        }
    }
}

fn step_seqs(frames: &[RunFrame]) -> Vec<i64> {
    frames
        .iter()
        .filter_map(|f| match f {
            RunFrame::Step { event, .. } => Some(event.seq),
            _ => None,
        })
        .collect()
}

fn assert_contiguous(seqs: &[i64], context: &str) {
    let expected: Vec<i64> = (1..=i64::try_from(seqs.len()).expect("len fits in i64")).collect();
    assert_eq!(seqs, expected, "{context}: seqs must be 1..N gap-free");
}

/// Probe registry release through the public resolve surface: a released run
/// reports `RunNotActive`.
fn run_released(registry: &AgentRunRegistry, run_id: i64) -> bool {
    matches!(
        registry.resolve(run_id, "stress-release-probe", true),
        ResolveOutcome::RunNotActive
    )
}

fn wait_released(registry: &AgentRunRegistry, run_id: i64, deadline: Instant) {
    while !run_released(registry, run_id) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for run {run_id} to be released from the registry"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// Start helper
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn start_stress_run(
    db: &Database,
    registry: &Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    ws: PathBuf,
    conversation_id: i64,
    user_request: &str,
    max_iterations: Option<usize>,
    spend_limit_micro_usd: Option<u64>,
    mode: AutonomyMode,
) -> i64 {
    start_run(
        db,
        Arc::clone(registry),
        host,
        executor,
        ws,
        AgentRunRequest {
            conversation_id,
            user_request: user_request.to_string(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            credential: "sk-stress".to_string(),
            max_iterations,
            spend_limit_micro_usd,
        },
        mode,
    )
    .expect("start stress run")
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Scenario 1: sustained long `ReAct` run through the real stack (over 250
/// scripted turns).
#[test]
#[allow(clippy::too_many_lines)]
fn stress_long_react_200_plus_turns() {
    const TOOL_TURNS: usize = 250;
    const HARD_BOUND: Duration = Duration::from_mins(1);
    let suite_started = Instant::now();

    let (db, db_path) = stress_db("long-react");
    let ws = stress_workspace("long-react-ws");
    std::fs::write(ws.join("seed.txt"), "stress seed").expect("seed file");
    let conversation_id = create_conversation(&db, "stress long react");

    // 250 tool turns (read_file, escalating input usage) + 1 final turn.
    let mut script: Vec<Result<AiResponse, ExecutorError>> = Vec::new();
    let mut expected_spent: u64 = 0;
    for i in 0..TOOL_TURNS {
        let tokens = u64::try_from(i + 1).expect("turn count fits u64");
        let usage = usage_of(tokens, 1);
        expected_spent = expected_spent.saturating_add(cost_for_usage(usage));
        script.push(tool_response(
            &format!("read-{i}"),
            "read_file",
            serde_json::json!({ "path": "seed.txt" }),
            Some(usage),
        ));
    }
    let final_usage = usage_of(1, 1);
    expected_spent = expected_spent.saturating_add(cost_for_usage(final_usage));
    script.push(Ok(text_response("all done", Some(final_usage))));

    let registry = Arc::new(AgentRunRegistry::default());
    let (tx, rx) = channel();
    let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
        frames_tx: tx,
        db: db.clone(),
    });
    let executor: Arc<dyn ProviderExecutor + Send + Sync> = Arc::new(ScriptedExecutor::new(script));

    let run_id = start_stress_run(
        &db,
        &registry,
        host,
        Arc::clone(&executor),
        ws.clone(),
        conversation_id,
        "read the seed file 250 times, then summarize",
        Some(500),
        None,
        AutonomyMode::FullAutonomous,
    );

    let deadline = Instant::now() + HARD_BOUND;
    let mut frames = Vec::new();
    loop {
        let frame = recv_deadline(&rx, deadline);
        let finished = matches!(frame, RunFrame::Finished { .. });
        frames.push(frame);
        if finished {
            break;
        }
    }

    // Terminal frame: last, completed, final content preserved.
    match frames.last().expect("frames") {
        RunFrame::Finished { run_id: fid, event } => {
            assert_eq!(*fid, run_id);
            assert_eq!(event.status, "completed", "run must complete");
            assert_eq!(event.final_content.as_deref(), Some("all done"));
            assert_eq!(event.conversation_id, conversation_id);
        }
        other => panic!("last frame must be Finished, got {other:?}"),
    }

    // Streamed steps: gap-free 1..N; every turn accounted (250 model_turn +
    // 250 tool_call + 1 final model_turn).
    let seqs = step_seqs(&frames);
    assert_contiguous(&seqs, "long react");
    let expected_steps = TOOL_TURNS * 2 + 1;
    assert_eq!(
        seqs.len(),
        expected_steps,
        "total streamed steps must equal turns + tool calls"
    );

    // Persisted steps must match the stream exactly.
    let steps = AgentRunRepository::new(&db)
        .list_steps(run_id)
        .expect("list steps");
    let persisted: Vec<i64> = steps.iter().map(|s| s.seq).collect();
    assert_eq!(persisted, seqs, "persisted seqs must match the stream");

    // agent_runs row: completed, exact total_steps, monotonic spend
    // accumulation (the final sum is only reachable if every turn's cost
    // accumulated in order).
    let run = AgentRunRepository::new(&db)
        .read_run(run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(run.status, "completed");
    assert_eq!(
        run.total_steps,
        i64::try_from(expected_steps).expect("fits"),
        "run.total_steps must equal the step count"
    );
    assert_eq!(
        run.spent_micro_usd,
        Some(expected_spent),
        "spend must equal the exact per-turn integer sum"
    );
    assert_eq!(run.spent_micro_usd, Some(163_155), "hard-coded exact sum");

    let elapsed = suite_started.elapsed();
    assert!(
        elapsed < HARD_BOUND,
        "long react run exceeded its hard wall-time bound: {elapsed:?}"
    );
    cleanup_db(&db_path);
    cleanup_workspace(&ws);
}

/// Scenario 2: three cancellations in one test — (a) mid `execute_command`,
/// (b) approval-parked, (c) right after a spend trip.
#[test]
#[allow(clippy::too_many_lines)]
fn stress_cancel_under_load() {
    // (a) Cancel while a long-running execute_command is in flight: the child
    // must be killed promptly, the run must end `cancelled`, and the registry
    // must release the conversation.
    {
        let (db, db_path) = stress_db("cancel-command");
        let ws = stress_workspace("cancel-command-ws");
        let conversation_id = create_conversation(&db, "stress cancel command");
        let long_cmd = if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        };
        let script = vec![
            tool_response(
                "cmd-1",
                "execute_command",
                serde_json::json!({ "command": long_cmd }),
                None,
            ),
            Ok(text_response("never reached", None)),
        ];
        let registry = Arc::new(AgentRunRegistry::default());
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "run a long command",
            Some(10),
            None,
            AutonomyMode::FullAutonomous,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        // The model_turn step is emitted right before the tool dispatch.
        loop {
            let frame = recv_deadline(&rx, deadline);
            if matches!(frame, RunFrame::Step { .. }) {
                break;
            }
        }
        // Give the child process time to spawn, then cancel.
        std::thread::sleep(Duration::from_millis(800));
        assert!(registry.cancel(run_id), "active run must cancel");
        let (finished, elapsed) = wait_finished_bounded(&rx, Duration::from_secs(10));
        assert_eq!(finished.status, "cancelled");
        assert_eq!(finished.conversation_id, conversation_id);
        // Child killed fast: the tool poll interval is 50ms and the cancel
        // drain grace is 2×250ms, so the whole unwind must stay well under
        // the ~1s tool bound plus scheduling slack.
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel-while-command must unwind fast, took {elapsed:?}"
        );
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
        let run = AgentRunRepository::new(&db)
            .read_run(run_id)
            .expect("read run")
            .expect("run exists");
        assert_eq!(run.status, "cancelled");
        cleanup_db(&db_path);
        cleanup_workspace(&ws);
    }

    // (b) Cancel while parked on an approval.
    {
        let (db, db_path) = stress_db("cancel-approval");
        let ws = stress_workspace("cancel-approval-ws");
        let conversation_id = create_conversation(&db, "stress cancel approval");
        let script = vec![
            tool_response(
                "wf-1",
                "write_file",
                serde_json::json!({ "path": "cancel-b.txt", "content": "x" }),
                None,
            ),
            Ok(text_response("never reached", None)),
        ];
        let registry = Arc::new(AgentRunRegistry::default());
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "write a file",
            Some(10),
            None,
            AutonomyMode::Supervised,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let frame = recv_deadline(&rx, deadline);
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { .. },
                ..
            } = &frame
            {
                break;
            }
        }
        assert!(registry.cancel(run_id), "approval-parked run must cancel");
        let (finished, elapsed) = wait_finished_bounded(&rx, Duration::from_secs(10));
        assert_eq!(finished.status, "cancelled");
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel-while-parked must unwind fast, took {elapsed:?}"
        );
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
        cleanup_db(&db_path);
        cleanup_workspace(&ws);
    }

    // (c) Cancel immediately after a spend trip: the trip already decided the
    // terminal outcome, so the run must end `spend_limit_exceeded` with the
    // exact tripped spend persisted and the registry released.
    {
        let (db, db_path) = stress_db("cancel-spend");
        let ws = stress_workspace("cancel-spend-ws");
        std::fs::write(ws.join("seed.txt"), "seed").expect("seed file");
        let conversation_id = create_conversation(&db, "stress cancel spend");
        // usage (1,1) costs exactly 30 micro-USD/turn; limit 20 trips on turn 1.
        let script = vec![tool_response(
            "read-0",
            "read_file",
            serde_json::json!({ "path": "seed.txt" }),
            Some(usage_of(1, 1)),
        )];
        let registry = Arc::new(AgentRunRegistry::default());
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "trip the spend guard",
            Some(10),
            Some(20),
            AutonomyMode::FullAutonomous,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let frame = recv_deadline(&rx, deadline);
            if let RunFrame::Governance {
                event:
                    AgentRunEvent::SpendLimitExceeded {
                        spent_micro,
                        limit_micro,
                    },
                ..
            } = &frame
            {
                assert_eq!(*spent_micro, 30);
                assert_eq!(*limit_micro, 20);
                break;
            }
        }
        // Cancel the instant the trip is observed: whether it lands before or
        // after release is a benign race, so its return value is not asserted.
        let _ = registry.cancel(run_id);
        let (finished, _elapsed) = wait_finished_bounded(&rx, Duration::from_secs(10));
        assert_eq!(
            finished.status, "spend_limit_exceeded",
            "a completed spend trip wins over a concurrent cancel"
        );
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
        let run = AgentRunRepository::new(&db)
            .read_run(run_id)
            .expect("read run")
            .expect("run exists");
        assert_eq!(run.status, "spend_limit_exceeded");
        assert_eq!(run.spent_micro_usd, Some(30));
        cleanup_db(&db_path);
        cleanup_workspace(&ws);
    }
}

/// Scenario 3: ≥8 conversations run concurrently with mixed scripts
/// (approve / deny / budget-extend / plain); every run reaches its expected
/// terminal status and the registry ends empty.
#[test]
#[allow(clippy::too_many_lines)]
fn stress_parallel_runs_across_conversations() {
    use std::thread;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Approve,
        Deny,
        BudgetExtend,
        Plain,
    }

    let (db, db_path) = stress_db("parallel");
    let ws = stress_workspace("parallel-ws");
    std::fs::write(ws.join("seed.txt"), "seed").expect("seed file");
    let registry = Arc::new(AgentRunRegistry::default());
    let deadline = Instant::now() + Duration::from_mins(1);

    let kinds = [
        Kind::Approve,
        Kind::Deny,
        Kind::BudgetExtend,
        Kind::Plain,
        Kind::Approve,
        Kind::Deny,
        Kind::BudgetExtend,
        Kind::Plain,
    ];

    // Start every run first (synchronous DP-4 claim per conversation), then
    // drive them all concurrently.
    let mut run_ids = Vec::new();
    let mut started = Vec::new();
    for (idx, kind) in kinds.iter().enumerate() {
        let conversation_id = create_conversation(&db, &format!("stress parallel {idx}"));
        let script: Vec<Result<AiResponse, ExecutorError>> = match kind {
            Kind::Approve | Kind::Deny => vec![
                tool_response(
                    &format!("wf-{idx}"),
                    "write_file",
                    serde_json::json!({ "path": format!("out-{idx}.txt"), "content": "hi" }),
                    None,
                ),
                Ok(text_response("done", None)),
            ],
            Kind::BudgetExtend => (0..3)
                .map(|i| {
                    tool_response(
                        &format!("read-{idx}-{i}"),
                        "read_file",
                        serde_json::json!({ "path": "seed.txt" }),
                        None,
                    )
                })
                .chain(std::iter::once(Ok(text_response("extended done", None))))
                .collect(),
            Kind::Plain => vec![Ok(text_response("plain done", None))],
        };
        let (mode, max_iterations) = match kind {
            Kind::Approve | Kind::Deny => (AutonomyMode::Supervised, None),
            Kind::BudgetExtend => (AutonomyMode::FullAutonomous, Some(1)),
            Kind::Plain => (AutonomyMode::FullAutonomous, None),
        };
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "parallel scenario",
            max_iterations,
            None,
            mode,
        );
        run_ids.push(run_id);
        started.push((run_id, conversation_id, rx, *kind));
    }

    let handles: Vec<_> = started
        .into_iter()
        .map(|(run_id, conversation_id, rx, kind)| {
            let registry = Arc::clone(&registry);
            let db = db.clone();
            thread::spawn(move || {
                let mut frames = Vec::new();
                let mut budget_parks = 0usize;
                loop {
                    let frame = recv_deadline(&rx, deadline);
                    match &frame {
                        RunFrame::Governance { event, .. } => match event {
                            AgentRunEvent::ApprovalRequested { call_id, .. } => {
                                let approve = matches!(kind, Kind::Approve);
                                assert_eq!(
                                    registry.resolve(run_id, call_id, approve),
                                    ResolveOutcome::Resolved,
                                    "fixed-mode approval must resolve cleanly"
                                );
                            }
                            AgentRunEvent::BudgetExhausted { .. } => {
                                budget_parks += 1;
                                assert!(
                                    registry.extend(run_id, 1),
                                    "budget extend must hit an active run"
                                );
                            }
                            _ => {}
                        },
                        RunFrame::Finished { event, .. } => {
                            assert_eq!(event.status, "completed", "{kind:?} must complete");
                            assert_eq!(event.conversation_id, conversation_id);
                            frames.push(frame);
                            break;
                        }
                        RunFrame::Step { .. } => {}
                    }
                    frames.push(frame);
                }

                let seqs = step_seqs(&frames);
                assert_contiguous(&seqs, &format!("{kind:?} run {run_id}"));
                let expected_steps = match kind {
                    // Approve: 2 model turns + 1 parked approval + 1 dispatched
                    // tool call. Deny: 2 model turns + 1 denied approval (the
                    // denied call is never dispatched).
                    Kind::Approve => 4,
                    Kind::Deny => 3,
                    Kind::BudgetExtend => 7,
                    Kind::Plain => 1,
                };
                assert_eq!(seqs.len(), expected_steps, "{kind:?} step count");
                if matches!(kind, Kind::BudgetExtend) {
                    assert!(budget_parks >= 1, "budget-extend run must park");
                }
                if matches!(kind, Kind::Deny) {
                    let steps = AgentRunRepository::new(&db)
                        .list_steps(run_id)
                        .expect("list steps");
                    assert!(
                        steps
                            .iter()
                            .any(|s| s.kind == "approval" && s.status.as_deref() == Some("denied")),
                        "denied run must record a denied approval step"
                    );
                }
                wait_released(&registry, run_id, deadline);
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("parallel driver thread joins");
    }
    // Registry must be empty: every run released its entry.
    for run_id in &run_ids {
        assert!(
            run_released(&registry, *run_id),
            "run {run_id} still registered after the parallel wave"
        );
    }
    cleanup_db(&db_path);
    cleanup_workspace(&ws);
}

/// Scenario 4: one run whose autonomy mode is cycled
/// supervised↔semi↔full mid-run by a storm driver; no deadlock, no lost
/// approval, run completes, bounded.
#[test]
#[allow(clippy::too_many_lines)]
fn stress_mode_switch_storm() {
    use std::thread;

    let (db, db_path) = stress_db("mode-storm");
    let ws = stress_workspace("mode-storm-ws");
    let conversation_id = create_conversation(&db, "stress mode storm");

    // 3 mutating tool turns + final text. Depending on when the mode storm
    // lands, each call either parks (and is approved) or auto-executes.
    let script = vec![
        tool_response(
            "wf-0",
            "write_file",
            serde_json::json!({ "path": "storm-0.txt", "content": "0" }),
            None,
        ),
        tool_response(
            "wf-1",
            "write_file",
            serde_json::json!({ "path": "storm-1.txt", "content": "1" }),
            None,
        ),
        tool_response(
            "wf-2",
            "write_file",
            serde_json::json!({ "path": "storm-2.txt", "content": "2" }),
            None,
        ),
        Ok(text_response("storm done", None)),
    ];
    let registry = Arc::new(AgentRunRegistry::default());
    let (tx, rx) = channel();
    let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
        frames_tx: tx,
        db: db.clone(),
    });
    let run_id = start_stress_run(
        &db,
        &registry,
        host,
        Arc::new(ScriptedExecutor::new(script)),
        ws.clone(),
        conversation_id,
        "survive the mode storm",
        Some(20),
        None,
        AutonomyMode::Supervised,
    );

    // Storm driver: cycles the mode every few milliseconds until the run ends.
    let done = Arc::new(AtomicBool::new(false));
    let storm_done = Arc::clone(&done);
    let storm_registry = Arc::clone(&registry);
    let storm = thread::spawn(move || {
        let modes = [
            AutonomyMode::Supervised,
            AutonomyMode::SemiAutonomous,
            AutonomyMode::FullAutonomous,
        ];
        let mut i = 0usize;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !storm_done.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "mode storm exceeded its bound");
            assert!(
                storm_registry.set_mode(run_id, modes[i % modes.len()]),
                "storm set_mode must hit the active run"
            );
            i += 1;
            thread::sleep(Duration::from_millis(5));
        }
    });

    // Frame driver: approve every requested approval; tolerate the benign
    // fast-path auto-approve race (pending already consumed →
    // NoPendingApproval, which is exactly the hotfix-cleanup scenario).
    let mut frames = Vec::new();
    let mut requested = 0usize;
    let mut resolved_events = 0usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let frame = recv_deadline(&rx, deadline);
        match &frame {
            RunFrame::Governance { event, .. } => match event {
                AgentRunEvent::ApprovalRequested { call_id, .. } => {
                    requested += 1;
                    let outcome = registry.resolve(run_id, call_id, true);
                    assert!(
                        matches!(
                            outcome,
                            ResolveOutcome::Resolved | ResolveOutcome::NoPendingApproval
                        ),
                        "requested approval must resolve or be auto-approved, got {outcome:?}"
                    );
                }
                AgentRunEvent::ApprovalResolved { approved, .. } => {
                    assert!(approved, "storm approvals must be approved, never denied");
                    resolved_events += 1;
                }
                _ => {}
            },
            RunFrame::Finished { event, .. } => {
                assert_eq!(event.status, "completed", "storm run must complete");
                assert_eq!(event.final_content.as_deref(), Some("storm done"));
                frames.push(frame);
                break;
            }
            RunFrame::Step { .. } => {}
        }
        frames.push(frame);
    }
    done.store(true, Ordering::Relaxed);
    storm.join().expect("storm thread joins");

    // No lost approvals: every request got exactly one resolution.
    assert_eq!(
        requested, resolved_events,
        "every ApprovalRequested must have exactly one ApprovalResolved"
    );
    let seqs = step_seqs(&frames);
    assert_contiguous(&seqs, "mode storm");
    // 3 model turns with tool calls + 3 tool calls + 1 final model turn,
    // plus one `approval` step per parked (non-auto-approved) call — the
    // exact count depends on where the storm landed, so derive it.
    let approval_steps = frames
        .iter()
        .filter_map(|f| match f {
            RunFrame::Step { event, .. } if event.kind == "approval" => Some(1),
            _ => None,
        })
        .sum::<usize>();
    assert_eq!(
        seqs.len(),
        7 + approval_steps,
        "storm run step count (7 + parked approval steps)"
    );
    wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
    let run = AgentRunRepository::new(&db)
        .read_run(run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(run.status, "completed");
    cleanup_db(&db_path);
    cleanup_workspace(&ws);
}

/// Scenario 5: park → extend → park → extend → … → complete (≥3 extends),
/// final status `completed`, never `budget_exhausted`.
#[test]
fn stress_budget_extend_loop() {
    let (db, db_path) = stress_db("budget-extend");
    let ws = stress_workspace("budget-extend-ws");
    std::fs::write(ws.join("seed.txt"), "seed").expect("seed file");
    let conversation_id = create_conversation(&db, "stress budget extend");

    // 3 tool turns + final; max_iterations=1 → park after every tool turn.
    let script = (0..3)
        .map(|i| {
            tool_response(
                &format!("read-{i}"),
                "read_file",
                serde_json::json!({ "path": "seed.txt" }),
                None,
            )
        })
        .chain(std::iter::once(Ok(text_response(
            "extended to the end",
            None,
        ))))
        .collect();

    let registry = Arc::new(AgentRunRegistry::default());
    let (tx, rx) = channel();
    let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
        frames_tx: tx,
        db: db.clone(),
    });
    let run_id = start_stress_run(
        &db,
        &registry,
        host,
        Arc::new(ScriptedExecutor::new(script)),
        ws.clone(),
        conversation_id,
        "keep extending the budget",
        Some(1),
        None,
        AutonomyMode::FullAutonomous,
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut frames = Vec::new();
    let mut extends = 0usize;
    let mut parks = 0usize;
    loop {
        let frame = recv_deadline(&rx, deadline);
        match &frame {
            RunFrame::Governance {
                event: AgentRunEvent::BudgetExhausted { .. },
                ..
            } => {
                parks += 1;
                assert!(registry.extend(run_id, 1), "extend must hit the parked run");
                extends += 1;
            }
            RunFrame::Finished { event, .. } => {
                assert_eq!(event.status, "completed", "must complete, never exhaust");
                assert_eq!(event.final_content.as_deref(), Some("extended to the end"));
                frames.push(frame);
                break;
            }
            _ => {}
        }
        frames.push(frame);
    }
    assert!(
        extends >= 3,
        "must have parked-and-extended at least 3 times, got {extends}"
    );
    assert_eq!(
        parks, extends,
        "each park is answered by exactly one extend"
    );
    let seqs = step_seqs(&frames);
    assert_contiguous(&seqs, "budget extend");
    assert_eq!(seqs.len(), 7, "3 tool turns + 3 tool calls + 1 final turn");
    wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
    let run = AgentRunRepository::new(&db)
        .read_run(run_id)
        .expect("read run")
        .expect("run exists");
    assert_eq!(run.status, "completed", "final status must be completed");
    assert_ne!(run.status, "budget_exhausted");
    cleanup_db(&db_path);
    cleanup_workspace(&ws);
}

/// Scenario 6: ≥100 tiny-usage turns; persisted `spent_micro_usd` equals the
/// exact integer sum, and the boundary turn trips strictly per
/// `spent > limit` semantics.
#[test]
#[allow(clippy::too_many_lines)]
fn stress_spend_accumulation_many_small_turns() {
    // usage (1,1) costs exactly 5 (input) + 25 (output) = 30 micro-USD/turn
    // under the policy pricing (ceiling divisions land exactly).
    const PER_TURN: u64 = 30;

    // Run A — accumulation: 100 tool turns + 1 final turn, no limit.
    {
        let (db, db_path) = stress_db("spend-accum");
        let ws = stress_workspace("spend-accum-ws");
        std::fs::write(ws.join("seed.txt"), "seed").expect("seed file");
        let conversation_id = create_conversation(&db, "stress spend accumulation");
        let turns = 100usize;
        let mut script: Vec<Result<AiResponse, ExecutorError>> = (0..turns)
            .map(|i| {
                tool_response(
                    &format!("read-{i}"),
                    "read_file",
                    serde_json::json!({ "path": "seed.txt" }),
                    Some(usage_of(1, 1)),
                )
            })
            .collect();
        script.push(Ok(text_response("accumulated", Some(usage_of(1, 1)))));
        let registry = Arc::new(AgentRunRegistry::default());
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "accumulate tiny spend",
            Some(200),
            None,
            AutonomyMode::FullAutonomous,
        );
        let (finished, _) = wait_finished_bounded(&rx, Duration::from_mins(1));
        assert_eq!(finished.status, "completed");
        let expected = PER_TURN * u64::try_from(turns + 1).expect("fits");
        let run = AgentRunRepository::new(&db)
            .read_run(run_id)
            .expect("read run")
            .expect("run exists");
        assert_eq!(run.status, "completed");
        assert_eq!(
            run.spent_micro_usd,
            Some(expected),
            "persisted spend must equal the exact integer sum 30×{turns}+30"
        );
        assert_eq!(run.spent_micro_usd, Some(3_030), "hard-coded exact sum");
        let steps = AgentRunRepository::new(&db)
            .list_steps(run_id)
            .expect("list steps");
        assert_eq!(
            steps.len(),
            turns * 2 + 1,
            "100 tool turns + 100 tool calls + 1 final turn"
        );
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
        cleanup_db(&db_path);
        cleanup_workspace(&ws);
    }

    // Run B — boundary trip: limit 900 = 30×30, so turn 30 (spent == 900)
    // must NOT trip (strict `spent > limit`), and turn 31 (spent == 930) must.
    {
        const LIMIT: u64 = 900;
        let (db, db_path) = stress_db("spend-boundary");
        let ws = stress_workspace("spend-boundary-ws");
        std::fs::write(ws.join("seed.txt"), "seed").expect("seed file");
        let conversation_id = create_conversation(&db, "stress spend boundary");
        let script: Vec<Result<AiResponse, ExecutorError>> = (0..31)
            .map(|i| {
                tool_response(
                    &format!("read-{i}"),
                    "read_file",
                    serde_json::json!({ "path": "seed.txt" }),
                    Some(usage_of(1, 1)),
                )
            })
            .collect();
        // Executor is kept concrete so the exact request count is observable.
        let executor = Arc::new(ScriptedExecutor::new(script));
        let registry = Arc::new(AgentRunRegistry::default());
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            executor.clone(),
            ws.clone(),
            conversation_id,
            "trip exactly at the boundary+1",
            Some(200),
            Some(LIMIT),
            AutonomyMode::FullAutonomous,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let frame = recv_deadline(&rx, deadline);
            if let RunFrame::Governance {
                event:
                    AgentRunEvent::SpendLimitExceeded {
                        spent_micro,
                        limit_micro,
                    },
                ..
            } = &frame
            {
                assert_eq!(*limit_micro, LIMIT);
                assert_eq!(
                    *spent_micro,
                    LIMIT + PER_TURN,
                    "trip must carry the boundary turn's cost"
                );
                break;
            }
        }
        let (finished, _) = wait_finished_bounded(&rx, Duration::from_secs(10));
        assert_eq!(finished.status, "spend_limit_exceeded");
        let run = AgentRunRepository::new(&db)
            .read_run(run_id)
            .expect("read run")
            .expect("run exists");
        assert_eq!(run.status, "spend_limit_exceeded");
        assert_eq!(run.spent_micro_usd, Some(LIMIT + PER_TURN));
        // Strictness proof: the executor saw exactly 31 turns — turn 30
        // (spent == 900 == limit) did NOT trip; turn 31 (930 > 900) did.
        assert_eq!(
            executor.request_count(),
            31,
            "exactly 31 model turns may be requested"
        );
        // Turns 1..30 each dispatch a read_file (tool_call step); the
        // tripping turn 31 is recorded as a model_turn before the guard
        // fires and dispatches nothing: 31 + 30 = 61 steps.
        let steps = AgentRunRepository::new(&db)
            .list_steps(run_id)
            .expect("list steps");
        assert_eq!(steps.len(), 61, "31 model turns + 30 dispatched tool calls");
        assert_eq!(
            steps.iter().filter(|s| s.kind == "model_turn").count(),
            31,
            "every turn including the tripping one is a model_turn step"
        );
        assert_eq!(
            steps.iter().filter(|s| s.kind == "tool_call").count(),
            30,
            "turns 1..30 dispatch; the tripping turn 31 does not"
        );
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
        cleanup_db(&db_path);
        cleanup_workspace(&ws);
    }
}

/// Scenario 7: hammer `start_run` on ONE active conversation from several
/// threads → `RunAlreadyActive` every time except the first; after the run
/// finishes, a new start succeeds.
#[test]
#[allow(clippy::too_many_lines)]
fn stress_duplicate_start_under_concurrency() {
    use std::thread;

    let (db, db_path) = stress_db("dup-start");
    let ws = stress_workspace("dup-start-ws");
    let conversation_id = create_conversation(&db, "stress duplicate start");
    let registry = Arc::new(AgentRunRegistry::default());
    let deadline = Instant::now() + Duration::from_secs(30);

    // Run 1 parks on an approval — a stable active state to hammer against.
    {
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let script = vec![
            tool_response(
                "wf-1",
                "write_file",
                serde_json::json!({ "path": "dup.txt", "content": "hi" }),
                None,
            ),
            Ok(text_response("first run done", None)),
        ];
        let run_id = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(script)),
            ws.clone(),
            conversation_id,
            "park on approval",
            Some(10),
            None,
            AutonomyMode::Supervised,
        );

        // Hammer start from 8 threads × 3 attempts while run 1 is active.
        let mut hammer_join = Vec::new();
        for t in 0..8 {
            let registry = Arc::clone(&registry);
            let db = db.clone();
            let ws = ws.clone();
            let (res_tx, res_rx) = channel::<bool>();
            hammer_join.push((res_rx, thread::spawn(move || {
                for a in 0..3 {
                    let attempt = t * 100 + a;
                    let script = vec![Ok(text_response("rejected", None))];
                    let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
                        frames_tx: channel().0,
                        db: db.clone(),
                    });
                    let outcome = start_run(
                        &db,
                        Arc::clone(&registry),
                        host,
                        Arc::new(ScriptedExecutor::new(script)),
                        ws.clone(),
                        AgentRunRequest {
                            conversation_id,
                            user_request: "duplicate attempt".to_string(),
                            provider: "openai".to_string(),
                            model: "test-model".to_string(),
                            credential: "sk-stress".to_string(),
                            max_iterations: None,
                            spend_limit_micro_usd: None,
                        },
                        AutonomyMode::FullAutonomous,
                    );
                    let rejected = matches!(
                        outcome,
                        Err(crate::application::agent::service::AgentRunError::RunAlreadyActive { .. })
                    );
                    assert!(
                        rejected,
                        "attempt {attempt} on an active conversation must be rejected"
                    );
                    let _ = res_tx.send(rejected);
                }
            })));
        }
        let mut rejections = 0usize;
        for (res_rx, handle) in hammer_join {
            for _ in 0..3 {
                assert!(res_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("hammer result"));
                rejections += 1;
            }
            handle.join().expect("hammer thread joins");
        }
        assert_eq!(rejections, 24, "every duplicate start must be rejected");

        // Finish run 1: approve, expect completed.
        let call_id;
        loop {
            let frame = recv_deadline(&rx, deadline);
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { call_id: id, .. },
                ..
            } = &frame
            {
                call_id = id.clone();
                break;
            }
        }
        assert_eq!(
            registry.resolve(run_id, &call_id, true),
            ResolveOutcome::Resolved
        );
        let (finished, _) = wait_finished_bounded(&rx, Duration::from_secs(10));
        assert_eq!(finished.status, "completed");
        assert_eq!(finished.final_content.as_deref(), Some("first run done"));
        wait_released(&registry, run_id, Instant::now() + Duration::from_secs(10));
    }

    // After the run finished, a new start on the same conversation succeeds.
    {
        let (tx, rx) = channel();
        let host: Arc<dyn AgentRunHost> = Arc::new(StressHost {
            frames_tx: tx,
            db: db.clone(),
        });
        let run_id2 = start_stress_run(
            &db,
            &registry,
            host,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response(
                "second run done",
                None,
            ))])),
            ws.clone(),
            conversation_id,
            "start again after release",
            Some(10),
            None,
            AutonomyMode::FullAutonomous,
        );
        let (finished, _) = wait_finished_bounded(&rx, Duration::from_secs(30));
        assert_eq!(finished.status, "completed");
        assert_eq!(finished.final_content.as_deref(), Some("second run done"));
        wait_released(&registry, run_id2, Instant::now() + Duration::from_secs(10));
    }

    cleanup_db(&db_path);
    cleanup_workspace(&ws);
}
