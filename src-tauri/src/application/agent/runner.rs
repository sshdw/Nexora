//! Agent execution service: the multi-step agent `ReAct` loop (ROADMAP.md
//! Phase 3 РІР‚вЂќ Task 3.1).
//!
//! [`AgentRunner`] orchestrates the existing provider-independent execution
//! layer ([`ProviderExecutor`]) and the existing native workspace tools
//! ([`ToolRegistry`]) into a deterministic request/tool-call loop:
//!
//! ```text
//! user request -> LLM request (+ tool definitions)
//!              -> response
//!                 |--- final text ----------------> finish
//!                 |--- tool_calls -> ToolRegistry -> observations
//!                                   -> LLM request -> repeat
//! ```
//!
//! The runner owns only orchestration: it never executes shell commands,
//! touches the filesystem outside the configured workspace root, or formats
//! provider payloads. Every returned tool call РІР‚вЂќ including unknown tools,
//! malformed arguments, and failing invocations РІР‚вЂќ is dispatched through
//! [`ToolRegistry`] and converted into a native tool-result message that is
//! appended to the conversation history for the next model turn.
//!
//! # Termination & Governance
//!
//! The loop finishes successfully when a provider response carries no tool
//! calls and usable final assistant content (AC-2). It terminates
//! deterministically once `max_iterations` model turns are exhausted, and it
//! propagates provider failures as classified [`AgentError`] values without
//! panicking (AC-9, AC-10).
//!
//! Task 3.2 layers user-controllable governance on top of that: an attached
//! [`RunControl`] exposes adaptive step budgets (`extend_steps`), user
//! pause/resume, and instant cancellation backed by a [`CancellationToken`]
//! that reaches running tool processes. When no control is attached the loop
//! keeps the exact deterministic Task 3.1 behaviour.
//!
//! Task 4.1 layers the HD-3 autonomy ladder on top of that: an attached
//! [`ApprovalGate`] decides per tool risk class and [`AutonomyMode`] whether
//! a call executes automatically or parks until the user approves or denies
//! it. Approved calls dispatch exactly as before; denied calls become a
//! controlled observation (`Error: tool execution was denied by the user`)
//! and the loop continues. When no gate is attached the loop keeps the exact
//! pre-4.1 behaviour.
//!
//! Task 4.2 layers opt-in persistence on top of that: with an attached
//! [`RunRecorder`] ([`AgentRunner::with_run_recorder`]) the run is persisted
//! to `agent_runs` (DATABASE.md Р’В§7.8) from start to termination on every exit
//! path, and each model turn, dispatched tool call, and parked approval
//! decision is appended to `agent_steps` (Р’В§7.9, D12) РІР‚вЂќ all best-effort, so
//! persistence failures never panic the loop and never change the run's
//! semantics. When no recorder is attached the loop keeps the exact pre-4.2
//! behaviour and writes nothing.
//!
//! # Observation representation (native tool round-trip)
//!
//! The provider-independent boundary models tool turns natively: the
//! assistant's own tool calls are appended as an [`AiRole::Assistant`]
//! message carrying `tool_calls` (including the provider-opaque
//! `thought_signature` pass-through), and every observation is appended as
//! an [`AiRole::Tool`] message carrying the `call_id`, `name`, and text of
//! the result. Each executor translates these into its provider-native
//! format (Gemini `functionCall`/`functionResponse`, `OpenAI`
//! `tool_calls`/role `tool`, Anthropic `tool_use`/`tool_result`), so the
//! model always sees its own calls and the results they produced. Unlike the
//! historical plain-user-text fence, these roles are in-flight only: they
//! are never persisted (DATABASE.md В§7.2 stays user/assistant/system).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::application::agent::action_memory::ActionSummary;
use crate::application::agent::approval::ApprovalGate;
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::permissions::PermissionStore;
use crate::application::agent::persistence::{
    mode_to_column, ActiveRunRecord, RunRecorder, DEFAULT_RECORDED_MODE,
};
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{
    AiMessage, AiRequest, AiRole, ExecutorError, ProviderExecutor,
};

use super::budget;
pub(crate) use super::budget::{DEFAULT_MAX_ITERATIONS, DEFAULT_REQUEST_TIMEOUT};
use super::dispatch;
pub(crate) use super::errors::AgentError;
use super::prompts;

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Deterministic single-run agent executor over the existing AI abstractions.
///
/// Wraps one [`ProviderExecutor`] reference and the workspace root that bounds
/// [`ToolRegistry`] filesystem access. The runner is reusable across runs; it
/// owns no conversation state between [`Self::run`] calls. Governance
/// (pause/resume, budgets, cancellation), approval gating, and run
/// persistence are applied only when a [`RunControl`], [`ApprovalGate`], or
/// [`RunRecorder`] is attached; otherwise the loop keeps the exact
/// deterministic pre-3.2/4.1/4.2 behaviour.
pub(crate) struct AgentRunner<'a> {
    executor: &'a dyn ProviderExecutor,
    workspace_root: PathBuf,
    max_iterations: usize,
    /// Optional governance handle (Task 3.2). When `None` the loop keeps the
    /// exact deterministic Task 3.1 semantics; `pause`/`resume`/`extend_steps`
    /// are no-ops and cancellation never fires.
    control: Option<RunControl>,
    /// Optional three-tier approval gate (Task 4.1). When `None` the loop keeps
    /// the exact deterministic pre-4.1 behaviour; no approval is ever required.
    approval_gate: Option<ApprovalGate>,
    /// Optional persistent permission rules (M1-core). When `None` every call
    /// falls back to the ladder; every run is implicitly `"coding"` (M2).
    permission_store: Option<PermissionStore>,
    /// Optional governance-event channel (Task 3.2); Milestone 5 bridges it to
    /// Tauri events. Delivery is best-effort.
    event_sender: Option<Sender<AgentRunEvent>>,
    /// Per-request timeout applied to every provider round trip (Task 3.2).
    request_timeout: Duration,
    /// Opt-in run recorder (Task 4.2). When `None` nothing is persisted and
    /// the loop keeps the exact pre-4.2 behaviour; when attached, the run and
    /// its structured steps are persisted to `agent_runs` / `agent_steps`
    /// (DATABASE.md Р’В§7.8, Р’В§7.9) best-effort.
    recorder: Option<RunRecorder<'a>>,
    /// Opt-in spend limit in micro-USD (Task 4.3). `None` means no financial
    /// guard; the loop keeps the exact pre-4.3 behaviour.
    spend_limit_micro_usd: Option<u64>,
    /// Prior conversation turns carried into the next run (agent memory
    /// slice). Empty by default; applied via [`Self::with_history`].
    prior_messages: Vec<AiMessage>,
    /// Prior runs' compressed action trace (Layer-2 action memory). `None`
    /// by default; applied via [`Self::with_action_summary`].
    action_summary: Option<ActionSummary>,
}

impl<'a> AgentRunner<'a> {
    /// Create a runner over `executor`, confining all tool filesystem access
    /// to `workspace_root`.
    pub(crate) fn new(executor: &'a dyn ProviderExecutor, workspace_root: &Path) -> Self {
        Self {
            executor,
            workspace_root: workspace_root.to_path_buf(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
            control: None,
            approval_gate: None,
            permission_store: None,
            event_sender: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            recorder: None,
            spend_limit_micro_usd: None,
            prior_messages: Vec::new(),
            action_summary: None,
        }
    }

    /// Override the fixed per-run iteration bound (AC-9). A bound of zero
    /// makes every run terminate immediately with budget exhaustion.
    #[must_use]
    pub(crate) fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    /// Attach a [`RunControl`] so the run can be paused, resumed, extended,
    /// or cancelled by the user (Task 3.2). Cloned cheaply; every clone
    /// governs this runner.
    #[must_use]
    pub(crate) fn with_control(mut self, control: RunControl) -> Self {
        if let Some(gate) = &self.approval_gate {
            gate.set_token(control.token().clone());
        }
        self.control = Some(control);
        self
    }

    /// Attach an [`ApprovalGate`] so the run enforces the HD-3 autonomy
    /// ladder (Task 4.1). When no gate is attached the loop keeps the exact
    /// pre-4.1 deterministic behaviour. Cloned cheaply; every clone governs
    /// this runner. If a `RunControl` is already attached, the gate is wired
    /// to share its cancellation token so `cancel()` while parked on an
    /// approval aborts with `AgentError::Cancelled` without deadlock.
    #[must_use]
    pub(crate) fn with_approval_gate(mut self, gate: ApprovalGate) -> Self {
        if let Some(control) = &self.control {
            gate.set_token(control.token().clone());
        }
        self.approval_gate = Some(gate);
        self
    }

    /// Attach the persistent permission store (M1-core). The store is a
    /// snapshot loaded at run start; every run is implicitly `"coding"`.
    #[must_use]
    pub(crate) fn with_permission_store(mut self, store: PermissionStore) -> Self {
        self.permission_store = Some(store);
        self
    }

    /// Attach the governance-event channel (Task 3.2). Emissions are
    /// best-effort: a receiver that stopped draining never blocks the run.
    #[must_use]
    pub(crate) fn with_event_sender(mut self, tx: Sender<AgentRunEvent>) -> Self {
        self.event_sender = Some(tx);
        self
    }

    /// Attach the opt-in run recorder (Task 4.2): the run and its structured
    /// steps are persisted to `agent_runs` / `agent_steps` (DATABASE.md
    /// Р’В§7.8, Р’В§7.9) best-effort. When no recorder is attached the loop keeps
    /// the exact pre-4.2 behaviour and writes nothing. The recorded mode is
    /// the attached [`ApprovalGate`]'s current [`AutonomyMode`], or
    /// [`DEFAULT_RECORDED_MODE`] without a gate; `conversation_id` stays
    /// `NULL` until the Task 5.1 IPC layer wires runs to conversations.
    #[must_use]
    pub(crate) fn with_run_recorder(mut self, recorder: RunRecorder<'a>) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// Override the default per-request HTTP timeout (Task 3.2).
    #[must_use]
    pub(crate) fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Attach an opt-in spend limit in micro-USD (Task 4.3). `None` (the
    /// default) means no financial guard; the loop keeps the exact pre-4.3
    /// behaviour and `tool_calls` byte-for-byte tests stay green.
    #[must_use]
    pub(crate) fn with_spend_limit(mut self, micro_usd: u64) -> Self {
        self.spend_limit_micro_usd = Some(micro_usd);
        self
    }

    /// Carry prior conversation turns into the run (agent memory slice).
    /// The history is windowed to [`history::DEFAULT_HISTORY_WINDOW`] at
    /// loop start; an empty history keeps the exact pre-slice behaviour.
    #[must_use]
    pub(crate) fn with_history(mut self, history: Vec<AiMessage>) -> Self {
        self.prior_messages = history;
        self
    }

    /// Carry prior runs' compressed action trace into the run (Layer-2
    /// action memory). `None` (the default) leaves the system prompt
    /// byte-identical; an empty summary likewise appends nothing.
    #[must_use]
    pub(crate) fn with_action_summary(mut self, summary: ActionSummary) -> Self {
        self.action_summary = Some(summary);
        self
    }

    /// Execute the `ReAct` loop for one user request.
    ///
    /// Sends the initial request augmented with the [`ToolRegistry`]
    /// definitions (AC-3), dispatches every returned tool call through the
    /// registry (AC-4), appends each outcome to the conversation history as an
    /// observation (AC-5), and repeats until the model answers without tool
    /// calls (AC-1, AC-2). Multiple tool calls in one response are all handled
    /// (AC-6); unknown tools and failing executions become controlled error
    /// observations rather than aborts or panics (AC-7, AC-8).
    ///
    /// When a [`RunControl`] is attached (Task 3.2) the loop additionally
    /// honours user pause/resume at step boundaries, parks at an exhausted
    /// budget awaiting `extend_steps` or `cancel`, and aborts promptly on
    /// cancellation (including reaching running tool processes via the shared
    /// [`CancellationToken`]). Without a control the loop is byte-for-byte
    /// deterministic (Task 3.1).
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::EmptyResponse`] when a response contains neither
    /// tool calls nor usable content; [`AgentError::BudgetExhausted`] when
    /// the step budget is exhausted without a final answer (deterministically
    /// when no [`RunControl`] is attached); [`AgentError::Cancelled`] when a
    /// user cancels; [`AgentError::Provider`] when any underlying request
    /// fails.
    ///
    /// Task 4.2: when a [`RunRecorder`] is attached, the run is persisted to
    /// `agent_runs` from start to termination on every exit path, and each
    /// model turn, dispatched tool call, and parked approval decision is
    /// appended to `agent_steps` (DATABASE.md Р’В§7.8, Р’В§7.9) РІР‚вЂќ all best-effort,
    /// so persistence failures never change the run's semantics.
    pub(crate) fn run(
        &self,
        provider: &str,
        model: &str,
        credential: &str,
        user_request: &str,
    ) -> Result<String, AgentError> {
        // Task 4.2: opt-in run persistence. When a recorder is attached the
        // run row is inserted before the first model turn; the recorded mode
        // is the gate's current mode, or DEFAULT_RECORDED_MODE without a
        // gate (documented in `persistence`).
        let mut record = self.recorder.as_ref().map(|recorder| {
            let mode = self
                .approval_gate
                .as_ref()
                .map_or(DEFAULT_RECORDED_MODE, |gate| mode_to_column(gate.mode()));
            ActiveRunRecord::start(*recorder, model, mode)
        });
        let mut spent_micro_usd: u64 = 0;
        let result = self.react_loop(
            provider,
            model,
            credential,
            user_request,
            record.as_mut(),
            &mut spent_micro_usd,
        );
        if let Some(rec) = record.as_ref() {
            rec.finalize(&result, spent_micro_usd, self.spend_limit_micro_usd);
        }
        result
    }

    /// The deterministic `ReAct` loop proper (Task 3.1 semantics with the
    /// Task 3.2 governance and Task 4.1 approval layers), optionally
    /// recording each model turn, dispatched tool call, and parked approval
    /// decision into `record` (Task 4.2).
    #[allow(clippy::too_many_lines)]
    fn react_loop(
        &self,
        provider: &str,
        model: &str,
        credential: &str,
        user_request: &str,
        mut record: Option<&mut ActiveRunRecord<'_>>,
        spent_micro_usd: &mut u64,
    ) -> Result<String, AgentError> {
        let tools = ToolRegistry::definitions();
        // A control never cancelled the plan: when the runner has no attached
        // control it dispatches tools through a never-firing token so the
        // undisputed Task 3.1 behaviour is preserved exactly.
        let idle_token = CancellationToken::new();
        let control = self.control.as_ref();
        let base = self.max_iterations;
        let mut steps_taken: usize = 0;
        // History opens with the fixed agent system prompt, the retained
        // conversation tail and the user request (assembled in `prompts`).
        let mut messages = prompts::build_initial_messages(
            &self.prior_messages,
            self.action_summary.as_ref(),
            user_request,
        );

        loop {
            // ---- Step boundary: governance gates before the next LLM turn ----

            // Cancellation is the highest-priority gate: it is checked before
            // any LLM work, again after every provider call, and between tool
            // dispatches so a cancellation never waits for further work.
            dispatch::check_cancellation(control, self.event_sender.as_ref())?;
            dispatch::honor_pause(control, self.event_sender.as_ref())?;
            budget::honor_allowance(control, base, steps_taken, self.event_sender.as_ref())?;

            let request = AiRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                messages: messages.clone(),
                tools: tools.clone(),
                request_timeout: Some(self.request_timeout),
            };
            let turn_started = Instant::now();
            // The run's cancellation token travels into the provider call so
            // an in-flight HTTP attempt aborts promptly instead of running to
            // the wall-clock timeout. A call cancelled in flight reports
            // `ExecutorError::Cancelled`: emit the governance event and abort
            // without recording a model turn or dispatching any tool call вЂ”
            // an abandoned call must never dispatch.
            let token: &CancellationToken = control.map_or(&idle_token, RunControl::token);
            let response = match self.executor.execute(&request, credential, token) {
                Ok(response) => response,
                Err(ExecutorError::Cancelled) => {
                    dispatch::emit(self.event_sender.as_ref(), AgentRunEvent::Cancelled);
                    return Err(AgentError::Cancelled);
                }
                Err(other) => return Err(other.into()),
            };
            steps_taken += 1;

            // Task 4.2: record the completed model turn (D12) with its
            // provider round-trip duration.
            if let Some(rec) = record.as_mut() {
                let duration_ms =
                    i64::try_from(turn_started.elapsed().as_millis()).unwrap_or(i64::MAX);
                rec.model_turn(&response.content, Some(duration_ms));
            }

            dispatch::check_cancellation(control, self.event_sender.as_ref())?;

            budget::check_spend_guard(
                model,
                response.usage,
                self.spend_limit_micro_usd,
                record.is_some(),
                spent_micro_usd,
                self.event_sender.as_ref(),
            )?;

            if response.tool_calls.is_empty() {
                // AC-2: no tool calls means the model is done. Usable final
                // content must be present; anything else is a controlled
                // failure rather than a silently empty success.
                if response.content.trim().is_empty() {
                    return Err(AgentError::EmptyResponse);
                }
                dispatch::emit(
                    self.event_sender.as_ref(),
                    AgentRunEvent::Completed { steps: steps_taken },
                );
                return Ok(response.content);
            }

            // The model's own turn вЂ” narration plus every returned tool call вЂ”
            // is appended unconditionally so the model always sees what it
            // called; the individual observations follow as Tool messages.
            messages.push(AiMessage {
                role: AiRole::Assistant,
                content: response.content,
                attachments: Vec::new(),
                tool_calls: response.tool_calls.clone(),
                tool_result: None,
            });

            // AC-6: dispatch every returned call through the per-tool-call
            // pipeline (permission rules, approval gate, execution and
            // observation recording) in `dispatch`.
            let ctx = dispatch::DispatchCtx {
                workspace_root: &self.workspace_root,
                token,
                control,
                approval_gate: self.approval_gate.as_ref(),
                permission_store: self.permission_store.as_ref(),
                sender: self.event_sender.as_ref(),
            };
            dispatch::dispatch_tool_calls(&ctx, &response.tool_calls, &mut messages, &mut record)?;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::fs;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;

    use crate::application::agent::approval::{ApprovalDecision, AutonomyMode};
    use crate::application::execution::{AiResponse, ExecutorError};

    #[test]
    fn immediate_final_text_finishes_without_second_iteration() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("all done"))]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner
            .run("openai", "gpt-test", "unused", "hello")
            .expect("run should finish");

        assert_eq!(answer, "all done");
        // Exactly one model turn: no extra request may be issued after the
        // final answer (AC-2).
        assert_eq!(fake.requests.borrow().len(), 1);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn initial_request_exposes_registry_definitions() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let runner = AgentRunner::new(&fake, &ws);

        runner.run("openai", "m", "cred", "hi").expect("finish");

        let first = &fake.requests.borrow()[0];
        let expected = ToolRegistry::definitions();
        assert_eq!(first.tools.len(), expected.len());
        for (sent, exp) in first.tools.iter().zip(&expected) {
            assert_eq!(sent, exp);
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn non_agent_chat_request_shape_is_unaffected_by_runner() {
        // Regression guard (AC-12): a plain text-only request built exactly as
        // the existing non-agent chat flow builds it carries no tools, and the
        // registry definitions exist only on runner-built requests.
        let plain = AiRequest {
            provider: "openai".to_string(),
            model: "m".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "just chat".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }],
            tools: Vec::new(),
            request_timeout: None,
        };
        assert!(plain.tools.is_empty());
        assert!(!ToolRegistry::definitions().is_empty());

        let fake = FakeExecutor::new(vec![Ok(text_response("plain reply"))]);
        let response = fake
            .execute(&plain, "cred", &CancellationToken::new())
            .expect("plain execute");
        assert_eq!(response.content, "plain reply");
        assert!(response.tool_calls.is_empty());
        assert_eq!(fake.requests.borrow().len(), 1);
    }

    // -----------------------------------------------------------------------
    // Opt-in run persistence (Task 4.2)
    // -----------------------------------------------------------------------

    use crate::application::agent::persistence::RunRecorder;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;

    #[test]
    fn no_recorder_persists_nothing() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(tool_step("a")), Ok(text_response("done"))]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "q").expect("finish");
        assert_eq!(answer, "done");

        // The database is available but no recorder was attached, so the
        // pre-4.2 behaviour persists nothing (the unrecorded path stays
        // byte-for-byte unchanged).
        let runs = AgentRunRepository::new(&db);
        assert!(
            runs.list_runs_by_started_at_desc()
                .expect("list runs")
                .is_empty(),
            "no agent_runs rows without an attached recorder"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_completed_run_with_gap_free_steps() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("final answer"))]);
        let runner = AgentRunner::new(&fake, &ws).with_run_recorder(RunRecorder::new(&db));

        let answer = runner.run("openai", "m", "cred", "q").expect("finish");
        assert_eq!(answer, "final answer");

        let runs = AgentRunRepository::new(&db);
        let all = runs.list_runs_by_started_at_desc().expect("list runs");
        assert_eq!(all.len(), 1, "one run row per run");
        let run = &all[0];
        assert_eq!(run.status, "completed");
        assert_eq!(run.final_content.as_deref(), Some("final answer"));
        assert_eq!(run.model, "m");
        assert_eq!(run.conversation_id, None, "NULL until Task 5.1 (D50)");
        assert_eq!(run.mode, "supervised", "documented default without a gate");
        assert!(run.finished_at.is_some(), "finalize stamps the time");
        assert_eq!(run.error, None);

        let steps = runs.list_steps(run.id).expect("list steps");
        assert!(
            steps.iter().any(|s| s.kind == "model_turn"),
            "the model turn is recorded (D12)"
        );
        let step_count = i64::try_from(steps.len()).expect("step count fits");
        let seqs: Vec<i64> = steps.iter().map(|s| s.seq).collect();
        assert_eq!(
            seqs,
            (1..=step_count).collect::<Vec<_>>(),
            "seq strictly increasing without gaps on the happy path"
        );
        assert_eq!(
            run.total_steps, step_count,
            "total_steps counts the recorded steps (D12)"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_dispatched_tool_call_with_succeeded_status() {
        let db = in_memory_database();
        let ws = temp_workspace();
        fs::write(ws.join("exists.txt"), "hello").expect("write fixture");
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![call_tool(
                    "r1",
                    "read_file",
                    serde_json::json!({"path": "exists.txt"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_run_recorder(RunRecorder::new(&db));

        runner.run("openai", "m", "cred", "q").expect("finish");

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        let steps = runs.list_steps(run.id).expect("list steps");
        let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["model_turn", "tool_call", "model_turn"]);
        let call_step = &steps[1];
        assert_eq!(call_step.tool_name.as_deref(), Some("read_file"));
        assert_eq!(call_step.status.as_deref(), Some("succeeded"));
        assert_eq!(
            call_step.arguments.as_deref(),
            Some("{\"path\":\"exists.txt\"}"),
            "raw JSON arguments exactly as provider-supplied"
        );
        assert_eq!(call_step.observation.as_deref(), Some("hello"));
        assert!(call_step.duration_ms.is_some());
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_denied_approval_and_run_still_completes() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_for_driver = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "x.txt", "content": "1"}),
                )],
                usage: None,
            }),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_run_recorder(RunRecorder::new(&db))
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            rx.recv_timeout(Duration::from_secs(5))
                .expect("ApprovalRequested");
            assert!(gate_for_driver.respond("w1", ApprovalDecision::Denied));
            rx.recv_timeout(Duration::from_secs(5))
                .expect("ApprovalResolved");
            rx.recv_timeout(Duration::from_secs(5)).expect("Completed")
        });
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "ok");
        driver.join().expect("driver joins");

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        assert_eq!(run.status, "completed");
        let steps = runs.list_steps(run.id).expect("list steps");
        let approval_steps: Vec<_> = steps.iter().filter(|s| s.kind == "approval").collect();
        assert_eq!(approval_steps.len(), 1, "the parked decision is recorded");
        assert_eq!(approval_steps[0].status.as_deref(), Some("denied"));
        assert_eq!(approval_steps[0].tool_name.as_deref(), Some("write_file"));
        assert!(
            !steps.iter().any(|s| s.kind == "tool_call"),
            "a denied call is never dispatched, so no tool_call step exists"
        );
        assert!(
            fs::read_to_string(ws.join("x.txt")).is_err(),
            "denied tool must not have executed"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_cancelled_run() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("never"))]);
        let control = RunControl::new();
        control.cancel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_control(control)
            .with_run_recorder(RunRecorder::new(&db));

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("cancelled");
        assert!(matches!(err, AgentError::Cancelled));

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        assert_eq!(run.status, "cancelled");
        assert_eq!(run.final_content, None);
        assert_eq!(run.error, None, "cancellation is not a classified error");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_budget_exhausted_run() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("later")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_run_recorder(RunRecorder::new(&db));

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("exhausted");
        assert!(matches!(err, AgentError::BudgetExhausted(2)));

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        assert_eq!(run.status, "budget_exhausted");
        assert_eq!(
            run.total_steps, 4,
            "two model turns and two dispatched tool calls"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_persists_error_run_for_provider_failure_without_panic() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Err(ExecutorError::Failure)]);
        let runner = AgentRunner::new(&fake, &ws).with_run_recorder(RunRecorder::new(&db));

        let err = runner.run("openai", "m", "cred", "q").expect_err("fails");
        assert!(matches!(err, AgentError::Provider(_)));

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        assert_eq!(run.status, "error");
        assert_eq!(
            run.error.as_deref(),
            Some("the AI provider failed to fulfil the request"),
            "classified error text, no secrets"
        );
        assert_eq!(run.final_content, None);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn recorder_records_the_gate_mode_when_attached() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("done"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(ApprovalGate::new(AutonomyMode::SemiAutonomous))
            .with_run_recorder(RunRecorder::new(&db));

        runner.run("openai", "m", "cred", "q").expect("finish");

        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list runs")[0];
        assert_eq!(run.mode, "semi_autonomous", "the gate's mode is recorded");
        let _ = fs::remove_dir_all(&ws);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{AgentError, AgentRunner};
    use crate::application::agent::approval::ApprovalGate;
    use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
    use crate::application::execution::{
        AiMessage, AiRequest, AiResponse, AiRole, ExecutorError, ProviderExecutor, TokenUsage,
        ToolCall,
    };

    pub(crate) static COUNTER: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn temp_workspace() -> PathBuf {
        let base = std::env::temp_dir();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!(
            "nexora-runner-test-{pid}-{id}-{nanos}",
            pid = std::process::id(),
            id = id,
            nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp workspace");
        canonical_workspace(&dir)
    }

    /// Canonicalize a freshly created temp workspace so the returned root is
    /// already in the form the file tools compare against. On Windows the
    /// temp dir can sit behind a junction, 8.3 short name, or an alternate
    /// separator/drive-letter/case spelling (notably on CI runners); the
    /// tools' canonical re-check then rejects the non-canonical root with
    /// `PathTraversal`. Resolving once here keeps every downstream
    /// `resolve_path`/`is_within_workspace` comparison canonical-vs-canonical.
    /// The `\\?\` verbatim prefix is stripped so paths stay readable and
    /// comparable with non-verbatim joins.
    pub(crate) fn canonical_workspace(dir: &Path) -> PathBuf {
        let canon = dir.canonicalize().expect("canonicalize temp workspace");
        let text = canon.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        canon
    }

    pub(crate) fn text_response(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)] // JSON literals read best at call sites
    pub(crate) fn call_tool(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    pub(crate) fn raw_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    /// Scripted [`ProviderExecutor`] fake: replays prepared responses in order
    /// and records every incoming request. Never performs network I/O.
    pub(crate) struct FakeExecutor {
        pub(crate) steps: RefCell<std::vec::IntoIter<Result<AiResponse, ExecutorError>>>,
        pub(crate) requests: RefCell<Vec<AiRequest>>,
    }

    impl FakeExecutor {
        pub(crate) fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: RefCell::new(steps.into_iter()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProviderExecutor for FakeExecutor {
        fn execute(
            &self,
            request: &AiRequest,
            _credential: &str,
            _token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.requests.borrow_mut().push(request.clone());
            self.steps
                .borrow_mut()
                .next()
                .expect("fake executor script exhausted")
        }
    }

    pub(crate) fn user_message(content: &str) -> AiMessage {
        AiMessage {
            role: AiRole::User,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    pub(crate) fn assistant_message(content: &str) -> AiMessage {
        AiMessage {
            role: AiRole::Assistant,
            content: content.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }
    }

    /// A scripted executor that can force the runner to park *between* turns:
    /// at `block_at` (the 0-based execute index) it signals `entered` and then
    /// spins until `release` while the test drives governance, then returns
    /// the next scripted response. Deterministic and network-free.
    pub(crate) struct GatedExecutor {
        pub(crate) steps: RefCell<std::vec::IntoIter<Result<AiResponse, ExecutorError>>>,
        pub(crate) requests: RefCell<Vec<AiRequest>>,
        pub(crate) block_at: usize,
        pub(crate) entered: Arc<AtomicBool>,
        pub(crate) release: Arc<AtomicBool>,
    }

    impl GatedExecutor {
        pub(crate) fn new(
            steps: Vec<Result<AiResponse, ExecutorError>>,
            block_at: usize,
        ) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let entered = Arc::new(AtomicBool::new(false));
            let release = Arc::new(AtomicBool::new(false));
            (
                Self {
                    steps: RefCell::new(steps.into_iter()),
                    requests: RefCell::new(Vec::new()),
                    block_at,
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                },
                entered,
                release,
            )
        }
    }

    impl ProviderExecutor for GatedExecutor {
        fn execute(
            &self,
            request: &AiRequest,
            _credential: &str,
            _token: &CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.requests.borrow_mut().push(request.clone());
            let idx = self.requests.borrow().len() - 1;
            if idx == self.block_at {
                self.entered.store(true, Ordering::SeqCst);
                while !self.release.load(Ordering::SeqCst) {
                    std::hint::spin_loop();
                }
            }
            self.steps
                .borrow_mut()
                .next()
                .expect("gated executor script exhausted")
        }
    }

    /// Wait (bounded) until `flag` becomes true.
    pub(crate) fn wait_flag(flag: &AtomicBool) {
        let start = Instant::now();
        while !flag.load(Ordering::SeqCst) {
            std::hint::spin_loop();
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "flag never became true in time"
            );
        }
    }

    /// A scripted non-terminal turn that only produces a tool call.
    pub(crate) fn tool_step(id: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "m".to_string(),
            tool_calls: vec![call_tool(id, "list_directory", serde_json::json!({}))],
            usage: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn approval_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn run_with_gate(
        ws: &std::path::Path,
        gate: ApprovalGate,
        steps: Vec<Result<AiResponse, ExecutorError>>,
        control: Option<RunControl>,
    ) -> (Result<String, AgentError>, Vec<AgentRunEvent>) {
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(steps);
        let mut runner = AgentRunner::new(&fake, ws)
            .with_approval_gate(gate)
            .with_event_sender(tx);
        if let Some(c) = control {
            runner = runner.with_control(c);
        }
        let res = runner.run("openai", "m", "cred", "q");
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        (res, events)
    }

    pub(crate) fn usage_response(content: &str, input: u64, output: u64) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: Some(TokenUsage {
                input_tokens: input,
                output_tokens: output,
            }),
        }
    }

    pub(crate) fn usage_tool_response(id: &str, input: u64, output: u64) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![call_tool(id, "list_directory", serde_json::json!({}))],
            usage: Some(TokenUsage {
                input_tokens: input,
                output_tokens: output,
            }),
        }
    }
}
