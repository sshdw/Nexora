//! Agent run bridge (Task 5.1): threads the synchronous [`AgentRunner`]
//! onto a dedicated run thread, streams every run event to the frontend,
//! and tracks active runs.
//!
//! # Threading model
//!
//! `AgentRunner::run` is synchronous and long-lived (it parks on user pauses,
//! budget boundaries, and approval gates), so it must never execute on the
//! IPC thread. [`start_run`] therefore:
//!
//! 1. claims the conversation (DP-4: at most one active run per conversation,
//!    parallel across conversations — rejected with
//!    [`AgentRunError::RunAlreadyActive`] otherwise);
//! 2. pre-creates the `agent_runs` row through the run recorder (with the
//!    conversation link, D50) so the run id is known synchronously;
//! 3. registers the run in the [`AgentRunRegistry`];
//! 4. spawns one **run thread** (executes `run()`, persists the assistant
//!    message on success, forwards the terminal `RunFinished` payload,
//!    releases the registry entry on every exit path) and one
//!    **forwarder thread** (drains the run's `mpsc` channel into
//!    [`AgentRunHost::emit`] until the channel disconnects — the disconnect
//!    *is* the drain guarantee — and only then emits the terminal frame, so
//!    `Finished` is always the last frame).
//!
//! The runner and the recorder both send on the same channel, and both send
//! from the run thread (recorder methods are called by the runner), so
//! governance and step events are totally ordered by emission.
//!
//! # Tauri independence
//!
//! The bridge never names a Tauri type: the shell layer supplies an
//! [`AgentRunHost`] implementation (event emission + assistant-message
//! persistence). The Tauri adapter lives in `commands/agent.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
use super::control::{AgentRunEvent, RunControl};
use super::persistence::{mode_to_column, terminal_outcome, RunRecorder};
use super::runner::AgentRunner;
use crate::application::conversations::ConversationService;
use crate::application::execution::{ExecutorRegistry, ProviderExecutor, RequestError};
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentRunRepository, AgentStep};

// ---------------------------------------------------------------------------
// Frames (the wire shape of `agent-run-event`)
// ---------------------------------------------------------------------------

/// One `agent-run-event` frame (Task 5.1 design §2.4): a single Tauri event
/// name whose payload discriminates step, governance, and terminal frames,
/// each tagged with the owning `run_id`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RunFrame {
    /// One successfully persisted step (from `AgentRunEvent::StepRecorded`).
    Step {
        /// The run this frame belongs to.
        run_id: i64,
        /// The step payload.
        event: StepEventFrame,
    },
    /// One governance event (everything the runner emits).
    Governance {
        /// The run this frame belongs to.
        run_id: i64,
        /// The governance payload.
        event: AgentRunEvent,
    },
    /// The terminal frame: delivered by the forwarder only after the run-event
    /// channel is fully drained, so it is always the last frame of a run.
    Finished {
        /// The run this frame belongs to.
        run_id: i64,
        /// The terminal payload.
        event: RunFinished,
    },
}

/// Step payload of a [`RunFrame::Step`] frame: exactly the persisted
/// `agent_steps` columns (minus the `run_id`, which the frame carries).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StepEventFrame {
    /// 1-based sequence, identical to `agent_steps.seq`.
    pub seq: i64,
    /// `'model_turn' | 'tool_call' | 'approval'`.
    pub kind: String,
    /// Tool name; `None` for `model_turn`.
    pub tool_name: Option<String>,
    /// Raw JSON arguments exactly as provider-supplied.
    pub arguments: Option<String>,
    /// Model-turn content / tool output / denial or approval text.
    pub observation: Option<String>,
    /// `'succeeded' | 'failed' | 'denied' | 'cancelled'` (tool/approval only).
    pub status: Option<String>,
    /// Step duration in milliseconds, when known.
    pub duration_ms: Option<i64>,
}

/// Terminal payload of a [`RunFrame::Finished`] frame. The status/error
/// mapping is the recorder's own [`terminal_outcome`], so the UI, the live
/// stream, and `agent_runs` cannot disagree.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunFinished {
    /// The conversation the run belongs to (also on the run's persisted row;
    /// carried here so the frontend can route the frame without a lookup).
    pub conversation_id: i64,
    /// `'completed' | 'cancelled' | 'budget_exhausted' |
    /// 'spend_limit_exceeded' | 'error'`.
    pub status: String,
    /// Final assistant text (`completed` only).
    pub final_content: Option<String>,
    /// Classified error text (`error` only; never a secret).
    pub error: Option<String>,
}

impl RunFrame {
    /// Wrap a channel event into its frame. `StepRecorded` events become
    /// step frames (their own `run_id` is authoritative); every other event
    /// becomes a governance frame tagged with the bridging `run_id`.
    #[must_use]
    pub(crate) fn with_event(run_id: i64, event: AgentRunEvent) -> Self {
        match event {
            AgentRunEvent::StepRecorded {
                run_id: event_run_id,
                seq,
                kind,
                tool_name,
                arguments,
                observation,
                status,
                duration_ms,
            } => Self::Step {
                run_id: event_run_id,
                event: StepEventFrame {
                    seq,
                    kind,
                    tool_name,
                    arguments,
                    observation,
                    status,
                    duration_ms,
                },
            },
            other => Self::Governance {
                run_id,
                event: other,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Host abstraction (the shell side of the bridge)
// ---------------------------------------------------------------------------

/// Everything the bridge needs from the shell (Tauri) layer, without Tauri
/// types. Implemented in `commands/agent.rs` over the `AppHandle`.
pub(crate) trait AgentRunHost: Send + Sync + 'static {
    /// Emit one frame to the frontend. Best-effort: a failed emission is
    /// logged by the implementation and never affects the run.
    fn emit(&self, frame: &RunFrame);

    /// Persist the final assistant message after a successful run — the same
    /// [`ConversationService`] path as plain chat (DP-7). Best-effort: a
    /// failure is logged and never changes the run outcome (the final answer
    /// remains available on the `agent_runs` row and in the stream).
    fn persist_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    );
}

// ---------------------------------------------------------------------------
// Active-run registry (managed Tauri state)
// ---------------------------------------------------------------------------

/// One active run's user-controllable handles. Cheap clones over the same
/// underlying state as the handles the run thread attached to its runner:
/// `cancel` wakes every parked wait through the shared cancellation token,
/// and `gate.respond` resolves an approval park.
#[derive(Debug, Clone)]
pub(crate) struct ActiveAgentRun {
    /// The conversation this run belongs to (DP-4 uniqueness key).
    pub(crate) conversation_id: i64,
    /// Cancel/extend handle.
    pub(crate) control: RunControl,
    /// Approval gate handle (`respond` resolves a park).
    pub(crate) gate: ApprovalGate,
}

/// Active-run registry: managed Tauri state mapping `run_id` to the handles
/// of the in-flight run, plus the per-conversation claim set that enforces
/// DP-4 synchronously (a second `start` for the same conversation is
/// rejected even before the run thread registers its entry).
#[derive(Debug, Default)]
pub(crate) struct AgentRunRegistry {
    runs: Mutex<HashMap<i64, ActiveAgentRun>>,
    claimed_conversations: Mutex<HashSet<i64>>,
}

/// Outcome of a registry approval resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveOutcome {
    /// The pending approval was resolved.
    Resolved,
    /// The run is not (or no longer) active.
    RunNotActive,
    /// The run is active but has no pending approval for that `call_id`.
    NoPendingApproval,
}

impl AgentRunRegistry {
    /// Synchronously claim a conversation slot (DP-4). Returns `false` when
    /// the conversation already has an active or starting run.
    fn claim_conversation(&self, conversation_id: i64) -> bool {
        self.claimed_conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(conversation_id)
    }

    /// Release a conversation claim (setup failure path).
    fn unclaim_conversation(&self, conversation_id: i64) {
        self.claimed_conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&conversation_id);
    }

    /// Register a started run under its id. The conversation claim must
    /// already be held (taken by [`start_run`]).
    fn register(&self, run_id: i64, entry: ActiveAgentRun) {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run_id, entry);
    }

    /// Release a terminated run: drop its handles and unclaim its
    /// conversation. Called by the run thread on every exit path, so a
    /// finished run is immediately reusable for a new run in the same
    /// conversation.
    fn release(&self, run_id: i64) {
        let entry = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&run_id);
        if let Some(entry) = entry {
            self.unclaim_conversation(entry.conversation_id);
        }
    }

    /// Cancel a run (DP-3: works from *every* state — running, approval-
    /// parked, or budget-parked — because `RunControl::cancel` wakes all
    /// parked waits through the shared cancellation token).
    ///
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn cancel(&self, run_id: i64) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.control.cancel();
                true
            }
            None => false,
        }
    }

    /// Resolve a parked approval. See [`ResolveOutcome`] for the outcomes.
    #[must_use]
    pub(crate) fn resolve(&self, run_id: i64, call_id: &str, approved: bool) -> ResolveOutcome {
        let runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = runs.get(&run_id) else {
            return ResolveOutcome::RunNotActive;
        };
        let decision = if approved {
            ApprovalDecision::Approved
        } else {
            ApprovalDecision::Denied
        };
        if entry.gate.respond(call_id, decision) {
            ResolveOutcome::Resolved
        } else {
            ResolveOutcome::NoPendingApproval
        }
    }

    /// Grant additional iterations to a budget-parked (or running) run.
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn extend(&self, run_id: i64, extra_steps: usize) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.control.extend_steps(extra_steps);
                true
            }
            None => false,
        }
    }

    /// Whether a run is currently registered (test seam).
    #[cfg(test)]
    fn is_active(&self, run_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&run_id)
    }
}

// ---------------------------------------------------------------------------
// Start request / errors
// ---------------------------------------------------------------------------

/// A validated agent-run start request. The credential is resolved by the
/// caller (the IPC layer, via `RequestExecutionService::resolve_credential`)
/// and only ever lives inside the spawned run thread — it never crosses IPC
/// and never enters an event frame, log line, or error message.
#[derive(Debug, Clone)]
pub(crate) struct AgentRunRequest {
    /// Conversation to run in (also persisted onto the `agent_runs` row).
    pub conversation_id: i64,
    /// The user request; persisted as the user message before the spawn.
    pub user_request: String,
    /// Provider internal name (must resolve to a registered executor).
    pub provider: String,
    /// Model name within the provider.
    pub model: String,
    /// Keyring credential for the provider (pre-resolved by the caller).
    pub credential: String,
    /// Iteration-budget override (test seam; `None` = runner default).
    pub(crate) max_iterations: Option<usize>,
}

/// Classified failures of [`start_run`] (pre-spawn only: once the run thread
/// is spawned, outcomes flow through the event stream instead).
#[derive(Debug)]
pub(crate) enum AgentRunError {
    /// The conversation does not exist.
    ConversationNotFound {
        /// The missing conversation id.
        id: i64,
    },
    /// Another run is already active for this conversation (DP-4).
    RunAlreadyActive {
        /// The busy conversation id.
        conversation_id: i64,
    },
    /// Provider/credential resolution failed (FR-014 classifications).
    Request(RequestError),
    /// The `agent_runs` row could not be created before spawning.
    RunNotPersisted,
    /// A run/forwarder thread could not be spawned.
    ThreadSpawn(String),
    /// Setup persistence failed.
    Database(crate::infrastructure::database::DatabaseError),
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

/// Start one agent run (Task 5.1 design §2-§3): claim the conversation
/// (DP-4), persist the user message, create the linked `agent_runs` row,
/// register the run, and spawn the run + forwarder threads. Returns the
/// `run_id` immediately; the run's outcome flows exclusively through the
/// event stream.
///
/// # Errors
///
/// See [`AgentRunError`]. On any pre-spawn failure the conversation claim is
/// released and no thread is left behind.
///
/// `registry` and `request` are taken by value deliberately: the bridge owns
/// the start request, and the registry handle is a cheap `Arc` that is
/// shared into the spawned threads.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn start_run(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
) -> Result<i64, AgentRunError> {
    ExecutorRegistry::new()
        .resolve_owned(&request.provider)
        .ok_or_else(|| {
            AgentRunError::Request(RequestError::ExecutorUnavailable {
                name: request.provider.clone(),
            })
        })?;

    // DP-4: claim synchronously so a second start for the same conversation
    // is rejected even before the run thread registers its entry.
    if !registry.claim_conversation(request.conversation_id) {
        return Err(AgentRunError::RunAlreadyActive {
            conversation_id: request.conversation_id,
        });
    }

    let started = start_run_claimed(db, &registry, host, executor, workspace_root, &request);
    if started.is_err() {
        registry.unclaim_conversation(request.conversation_id);
    }
    started
}

/// The post-claim setup: user message, run row, registration, spawn.
fn start_run_claimed(
    db: &Database,
    registry: &Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: &AgentRunRequest,
) -> Result<i64, AgentRunError> {
    // Persist the user message BEFORE spawning (design §3.2): a crash can
    // never lose it, and it appears in the thread immediately. No assistant
    // message is ever created unless the run later succeeds (plain-chat
    // doctrine). Empty content is rejected by the `messages` schema CHECK,
    // exactly as in plain chat.
    match ConversationService::new(db)
        .persist_user_message(request.conversation_id, &request.user_request)
    {
        Ok(_) => {}
        Err(crate::application::conversations::ConversationError::NotFound { id }) => {
            return Err(AgentRunError::ConversationNotFound { id });
        }
        Err(crate::application::conversations::ConversationError::Database(err)) => {
            return Err(AgentRunError::Database(err));
        }
        Err(other) => {
            log::error!("agent run setup: unexpected user-message error: {other}");
            return Err(AgentRunError::Request(RequestError::Execution {
                name: request.provider.clone(),
            }));
        }
    }

    // Create the linked run row (D50) through the recorder so the run id is
    // known synchronously; the spawned run adopts it (no second insert).
    let gate = ApprovalGate::new(AutonomyMode::SemiAutonomous);
    let control = RunControl::new();
    let mode = mode_to_column(AutonomyMode::SemiAutonomous);
    let Some(run_id) = RunRecorder::new(db)
        .with_conversation(request.conversation_id)
        .create_run_row(&request.model, mode)
    else {
        return Err(AgentRunError::RunNotPersisted);
    };

    registry.register(
        run_id,
        ActiveAgentRun {
            conversation_id: request.conversation_id,
            control: control.clone(),
            gate: gate.clone(),
        },
    );

    spawn_run(
        Arc::clone(registry),
        host,
        db.clone(),
        executor,
        workspace_root,
        run_id,
        control,
        gate,
        request.clone(),
    )?;
    Ok(run_id)
}

/// Spawn the run thread and the forwarder thread (design §2.1).
///
/// `request`/`host` are consumed by the thread closures; the owned handles are
/// the bridge's contract, and the arity mirrors the thread boundaries each
/// value is destined for.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn spawn_run(
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    db: Database,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    run_id: i64,
    control: RunControl,
    gate: ApprovalGate,
    request: AgentRunRequest,
) -> Result<(), AgentRunError> {
    let (tx, rx): (Sender<AgentRunEvent>, Receiver<AgentRunEvent>) = mpsc::channel();
    // Terminal-frame channel: the run thread sends the `RunFinished` payload
    // here (never directly to the host) so the forwarder emits it only AFTER
    // the run-event channel disconnects — `Finished` is therefore the last
    // frame of every run, no matter how many events were buffered.
    let (finish_tx, finish_rx): (Sender<RunFinished>, Receiver<RunFinished>) = mpsc::channel();

    // Run thread: executes the ReAct loop, persists the assistant message on
    // success, forwards the terminal frame, and releases the registry entry on
    // every exit path (DP-9: a panic leaks the entry until app exit — the
    // runner is panic-free by design).
    let run_registry = Arc::clone(&registry);
    let run_host = Arc::clone(&host);
    let run_request = request;
    std::thread::Builder::new()
        .name(format!("agent-run-{run_id}"))
        .spawn(move || {
            // The recorder borrows this thread's sender clone; the runner owns
            // the recorder, so the borrow lives exactly as long as the loop.
            let outcome = {
                let tx_for_recorder = tx.clone();
                let recorder = RunRecorder::new(&db)
                    .with_run_id(run_id)
                    .with_events(&tx_for_recorder);
                let mut runner = AgentRunner::new(executor.as_ref(), &workspace_root)
                    .with_control(control)
                    .with_approval_gate(gate)
                    .with_event_sender(tx_for_recorder.clone());
                if let Some(max_iterations) = run_request.max_iterations {
                    runner = runner.with_max_iterations(max_iterations);
                }
                runner.with_run_recorder(recorder).run(
                    &run_request.provider,
                    &run_request.model,
                    &run_request.credential,
                    &run_request.user_request,
                )
            };

            // Assistant message only on success — never a fake assistant
            // message on failure (plain-chat doctrine).
            if let Ok(content) = &outcome {
                run_host.persist_assistant_message(
                    run_request.conversation_id,
                    content,
                    &run_request.provider,
                    &run_request.model,
                );
            }

            let (status, final_content, error) = terminal_outcome(&outcome);
            // Best-effort: if the send fails the run is already finished and
            // the terminal state is persisted on the `agent_runs` row.
            let _ = finish_tx.send(RunFinished {
                conversation_id: run_request.conversation_id,
                status: status.to_string(),
                final_content,
                error,
            });
            run_registry.release(run_id);
            // `tx` and `finish_tx` drop at scope end: the run-event channel
            // disconnects (forwarder flushes to Finished) and the terminal
            // channel disconnects after the frame is consumed.
        })
        .map_err(|err| {
            registry.release(run_id);
            AgentRunError::ThreadSpawn(err.to_string())
        })?;

    // Forwarder thread: drains the run-event channel into the host until
    // disconnect — the disconnect IS the drain guarantee — and only then
    // forwards the terminal frame from the finish channel, so `Finished` is
    // the last frame of every run.
    let forward_host = host;
    std::thread::Builder::new()
        .name(format!("agent-run-forwarder-{run_id}"))
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                forward_host.emit(&RunFrame::with_event(run_id, event));
            }
            // `rx` disconnected: every buffered run event has been emitted.
            // The run thread sent the terminal frame before dropping its
            // senders (and before releasing the registry entry), so it is
            // either buffered or in flight here — never lost.
            while let Ok(event) = finish_rx.recv() {
                forward_host.emit(&RunFrame::Finished { run_id, event });
            }
        })
        .map_err(|err| {
            // No forwarder means no event stream: abort the run and clean up.
            let _ = registry.cancel(run_id);
            registry.release(run_id);
            AgentRunError::ThreadSpawn(err.to_string())
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Rehydration helpers (application-layer wrappers over the run repository)
// ---------------------------------------------------------------------------

/// List the runs of one conversation, newest first (`started_at` DESC).
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_runs_for_conversation(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<AgentRun>, crate::infrastructure::database::DatabaseError> {
    AgentRunRepository::new(db).list_runs_by_conversation(conversation_id)
}

/// List the steps of one run, `seq` ascending (gap-free per CF-01).
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_steps_for_run(
    db: &Database,
    run_id: i64,
) -> Result<Vec<AgentStep>, crate::infrastructure::database::DatabaseError> {
    AgentRunRepository::new(db).list_steps(run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use crate::application::execution::{AiResponse, ExecutorError, ToolCall};

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
            _request: &crate::application::execution::AiRequest,
            _credential: &str,
        ) -> Result<AiResponse, ExecutorError> {
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    /// Fake host: records emitted frames on a channel and assistant
    /// persistence calls for assertions.
    struct FakeHost {
        frames_tx: Sender<RunFrame>,
        db: Database,
        persisted: Mutex<Vec<(i64, String, String, String)>>,
    }

    impl AgentRunHost for FakeHost {
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
            self.persisted.lock().expect("persisted lock").push((
                conversation_id,
                content.to_string(),
                provider.to_string(),
                model.to_string(),
            ));
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
            }],
            usage: None,
        }
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nexora-agent-bridge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace dir");
        dir
    }

    fn collect_frames(rx: &Receiver<RunFrame>) -> Vec<RunFrame> {
        let mut frames = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(frame) => {
                    let finished = matches!(frame, RunFrame::Finished { .. });
                    frames.push(frame);
                    if finished {
                        return frames;
                    }
                }
                Err(_) => return frames,
            }
        }
    }

    fn setup(tag: &str) -> (Database, PathBuf, Receiver<RunFrame>, Arc<FakeHost>) {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace(tag);
        let (tx, rx) = std::sync::mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        (db, workspace, rx, host)
    }

    fn request(conversation_id: i64, content: &str) -> AgentRunRequest {
        AgentRunRequest {
            conversation_id,
            user_request: content.to_string(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            credential: "sk-secret-test-credential".to_string(),
            max_iterations: None,
        }
    }

    /// Happy path: steps stream in seq order, `Finished` is the last frame
    /// with `completed` + final content, the assistant message is persisted
    /// through the shared path, and the registry entry is released. Also a
    /// leak-negative check: no frame ever contains the credential.
    #[test]
    fn start_run_streams_steps_and_finishes_completed() {
        let (db, workspace, rx, host) = setup("happy");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("t", "read_file")),
                Ok(text_response("final answer")),
            ])),
            workspace,
            request(conversation_id, "do things"),
        )
        .expect("start");

        let frames = collect_frames(&rx);
        assert!(
            !frames.is_empty(),
            "at least the terminal frame must arrive"
        );
        // `Finished` is the LAST frame (drain guarantee).
        let RunFrame::Finished {
            run_id: frame_run,
            event,
        } = frames.last().expect("frames")
        else {
            panic!("last frame must be Finished");
        };
        assert_eq!(*frame_run, run_id);
        assert_eq!(event.conversation_id, conversation_id);
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("final answer"));

        // Step frames are strictly seq-ordered starting at 1.
        let seqs: Vec<i64> = frames
            .iter()
            .filter_map(|frame| match frame {
                RunFrame::Step { event, .. } => Some(event.seq),
                _ => None,
            })
            .collect();
        assert_eq!(
            seqs,
            (1..=seqs.len())
                .map(i64::try_from)
                .collect::<Result<Vec<_>, _>>()
                .expect("seq count fits in i64")
        );

        // Assistant message persisted through the shared path (DP-7).
        assert_eq!(
            host.persisted.lock().expect("lock").as_slice(),
            [(
                conversation_id,
                "final answer".to_string(),
                "openai".to_string(),
                "test-model".to_string()
            )]
        );

        // Registry cleanup on the success exit path.
        assert!(!registry.is_active(run_id), "run released after finish");

        // Credential never enters any frame.
        for frame in &frames {
            let serialized = serde_json::to_string(frame).expect("serialize");
            assert!(
                !serialized.contains("sk-secret-test-credential"),
                "credential leaked into frame: {serialized}"
            );
        }

        // Rehydration: the persisted row agrees with the stream.
        let runs = list_runs_for_conversation(&db, conversation_id).expect("list runs");
        let run = runs.iter().find(|run| run.id == run_id).expect("run row");
        assert_eq!(run.status, "completed");
        assert_eq!(run.final_content.as_deref(), Some("final answer"));
        assert_eq!(run.conversation_id, Some(conversation_id));
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        assert_eq!(
            steps.iter().map(|step| step.seq).collect::<Vec<_>>(),
            seqs,
            "persisted seqs match the streamed seqs"
        );
        let _ = std::fs::remove_dir_all(temp_workspace("happy"));
    }

    /// Approval park: the mutating tool parks the run (DP-3), a second start
    /// for the same conversation is rejected (DP-4), `resolve` unblocks the
    /// park, and the run then completes.
    #[test]
    fn approval_park_is_resolved_and_duplicate_start_is_rejected() {
        let (db, workspace, rx, host) = setup("approval");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("after approval")),
            ])),
            workspace,
            request(conversation_id, "mutate something"),
        )
        .expect("start");

        // Wait for the approval park.
        let call_id = loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { call_id, .. },
                ..
            } = frame
            {
                break call_id;
            }
        };

        // DP-4: a second concurrent start for the same conversation must be
        // rejected while the first is parked.
        let second = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("no"))])),
            temp_workspace("approval-second"),
            request(conversation_id, "second"),
        );
        assert!(
            matches!(second, Err(AgentRunError::RunAlreadyActive { .. })),
            "duplicate conversation start must be rejected, got: {second:?}"
        );

        // Resolve the park through the registry (the IPC command's path).
        assert_eq!(
            registry.resolve(run_id, &call_id, true),
            ResolveOutcome::Resolved
        );

        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("after approval"));
        assert!(!registry.is_active(run_id));
        let _ = std::fs::remove_dir_all(temp_workspace("approval"));
        let _ = std::fs::remove_dir_all(temp_workspace("approval-second"));
    }

    /// Cancel from an approval park aborts the run (`cancelled` terminal) and
    /// releases the registry entry (DP-3: cancel works from every state).
    #[test]
    fn cancel_from_approval_park_aborts_the_run() {
        let (db, workspace, rx, host) = setup("cancel");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("never reached")),
            ])),
            workspace,
            request(conversation_id, "cancel me"),
        )
        .expect("start");

        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::ApprovalRequested { .. },
                    ..
                }
            ) {
                break;
            }
        }

        assert!(registry.cancel(run_id), "cancel must reach the active run");
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "cancelled");
        assert_eq!(event.final_content, None);
        assert!(
            !registry.is_active(run_id),
            "registry entry released on the cancelled exit path"
        );
        // No assistant message on a failed/cancelled run (doctrine).
        assert!(host.persisted.lock().expect("lock").is_empty());
        let _ = std::fs::remove_dir_all(temp_workspace("cancel"));
    }

    /// Budget park: exhaustion parks the run; `extend` continues it to
    /// completion (DP-5).
    #[test]
    fn budget_park_is_continued_via_extend() {
        let (db, workspace, rx, host) = setup("budget");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let mut req = request(conversation_id, "loop a bit");
        req.max_iterations = Some(1);
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("l", "list_directory")),
                Ok(text_response("continued")),
            ])),
            workspace,
            req,
        )
        .expect("start");

        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("budget frame");
            if matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::BudgetExhausted { .. },
                    ..
                }
            ) {
                break;
            }
        }

        assert!(registry.extend(run_id, 2), "extend must reach the run");
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("continued"));
        assert!(!registry.is_active(run_id));
        let _ = std::fs::remove_dir_all(temp_workspace("budget"));
    }

    /// Provider failure: `Finished { status: "error" }`, no assistant message,
    /// registry released, and the classified error text carries no secret.
    #[test]
    fn provider_error_finishes_with_error_and_cleans_up() {
        let (db, workspace, rx, host) = setup("error");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Err(ExecutorError::Failure)])),
            workspace,
            request(conversation_id, "explode"),
        )
        .expect("start");

        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "error");
        assert_eq!(event.final_content, None);
        assert!(event.error.is_some());
        let serialized = serde_json::to_string(&frames).expect("serialize");
        assert!(!serialized.contains("sk-secret-test-credential"));
        assert!(!registry.is_active(run_id));
        assert!(host.persisted.lock().expect("lock").is_empty());
        let _ = std::fs::remove_dir_all(temp_workspace("error"));
    }
}
