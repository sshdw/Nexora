//! Agent execution service: the multi-step agent `ReAct` loop (ROADMAP.md
//! Phase 3 вЂ” Task 3.1).
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
//! provider payloads. Every returned tool call вЂ” including unknown tools,
//! malformed arguments, and failing invocations вЂ” is dispatched through
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
//! to `agent_runs` (DATABASE.md В§7.8) from start to termination on every exit
//! path, and each model turn, dispatched tool call, and parked approval
//! decision is appended to `agent_steps` (В§7.9, D12) вЂ” all best-effort, so
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
//! are never persisted (DATABASE.md §7.2 stays user/assistant/system).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::application::agent::approval::{ApprovalDecision, ApprovalGate};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::persistence::{
    mode_to_column, ActiveRunRecord, RunRecorder, DEFAULT_RECORDED_MODE,
};
use crate::application::agent::pricing;
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{
    AiMessage, AiRequest, AiRole, ExecutorError, ProviderExecutor,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default upper bound on consecutive model turns executed by one run.
///
/// A repeatedly tool-calling model cannot loop forever: after this many
/// iterations the run terminates deterministically with
/// [`AgentError::BudgetExhausted`] (AC-9). This is the fixed base bound;
/// adaptive budgets extend it via [`RunControl::extend_steps`] (Task 3.2).
pub(crate) const DEFAULT_MAX_ITERATIONS: usize = 10;

/// Default wall-clock timeout bound applied to each blocking provider
/// request emitted by the runner (Task 3.2).
///
/// The blocking `reqwest` client cannot be interrupted mid-flight, so the
/// honest bound for "terminate running LLM HTTP requests" is a per-request
/// timeout. The provider-independent [`AiRequest`] carries
/// `request_timeout: Option<Duration>`; the runner always sets it to this
/// default unless overridden via [`AgentRunner::with_request_timeout`].
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::new(120, 0);

/// Fixed system prompt for Windows hosts (the primary target).
const AGENT_SYSTEM_PROMPT_WINDOWS: &str = "You are Nexora, a desktop agent working on the user's machine.\n\nEnvironment:\n- The operating system is Windows; execute_command runs each command through cmd.exe, so Unix shell utilities such as ls, cat or grep are unavailable - use their Windows equivalents (dir, type, findstr).\n- The file tools read_file, write_file and list_directory operate inside a dedicated agent workspace directory. Relative paths resolve against the workspace, paths outside it are rejected, and execute_command runs with the workspace as its current directory.\n\nWorkflow:\n- Call a tool whenever the task needs one. Every call you make comes back as a tool result that you must use to continue.\n- A turn that only calls tools is not a final answer: when the task is done, reply to the user directly, without tool calls.\n- If a tool returns an error, read it, fix the arguments or choose another approach; never repeat an identical failing call.\n- Reply in the user's language.";

/// Fixed system prompt for POSIX hosts; the Environment section states the
/// shell accordingly.
const AGENT_SYSTEM_PROMPT_POSIX: &str = "You are Nexora, a desktop agent working on the user's machine.\n\nEnvironment:\n- execute_command runs each command through the POSIX shell (sh).\n- The file tools read_file, write_file and list_directory operate inside a dedicated agent workspace directory. Relative paths resolve against the workspace, paths outside it are rejected, and execute_command runs with the workspace as its current directory.\n\nWorkflow:\n- Call a tool whenever the task needs one. Every call you make comes back as a tool result that you must use to continue.\n- A turn that only calls tools is not a final answer: when the task is done, reply to the user directly, without tool calls.\n- If a tool returns an error, read it, fix the arguments or choose another approach; never repeat an identical failing call.\n- Reply in the user's language.";

/// The fixed agent system prompt assembled for this build target.
#[cfg(windows)]
const AGENT_SYSTEM_PROMPT: &str = AGENT_SYSTEM_PROMPT_WINDOWS;

/// The fixed agent system prompt assembled for this build target.
#[cfg(not(windows))]
const AGENT_SYSTEM_PROMPT: &str = AGENT_SYSTEM_PROMPT_POSIX;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Classified agent-loop failure. Carries no secret payload and never embeds
/// credential material (ARCHITECTURE.md В§9, В§11): the provider variant wraps
/// the already-classified [`ExecutorError`].
#[derive(Debug)]
pub(crate) enum AgentError {
    /// The provider failed to fulfil one of the loop's requests. The
    /// classified [`ExecutorError`] passes through verbatim to the run
    /// error text (its Display is rendered unchanged).
    Provider(ExecutorError),
    /// The iteration budget was exhausted before the model produced a final
    /// answer. With no [`RunControl`] attached this aborts outright; with one
    /// attached the run first parked at the boundary awaiting `extend_steps`
    /// and only aborts if it was instead cancelled.
    BudgetExhausted(usize),
    /// The spend guard tripped: billed spend exceeded the configured per-run
    /// limit (Task 4.3). `spent_micro` includes the tripping turn's cost.
    SpendLimitExceeded { spent_micro: u64, limit_micro: u64 },
    /// The provider returned neither tool calls nor usable final content.
    EmptyResponse,
    /// A user cancelled the run via [`RunControl::cancel`] (or cancellation
    /// was observed during a tool execution).
    Cancelled,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(err) => write!(f, "{err}"),
            Self::BudgetExhausted(max) => write!(
                f,
                "agent stopped: reached the {max}-step limit without a final answer"
            ),
            Self::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => write!(
                f,
                "agent stopped: spend limit exceeded (spent {spent_micro} micro-USD of {limit_micro} micro-USD)"
            ),
            Self::EmptyResponse => {
                write!(f, "agent stopped: the model returned an empty response")
            }
            Self::Cancelled => write!(f, "agent stopped: cancelled by the user"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(err) => Some(err),
            Self::BudgetExhausted(_)
            | Self::SpendLimitExceeded { .. }
            | Self::EmptyResponse
            | Self::Cancelled => None,
        }
    }
}

impl From<ExecutorError> for AgentError {
    fn from(err: ExecutorError) -> Self {
        Self::Provider(err)
    }
}

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
    /// Optional governance-event channel (Task 3.2); Milestone 5 bridges it to
    /// Tauri events. Delivery is best-effort.
    event_sender: Option<Sender<AgentRunEvent>>,
    /// Per-request timeout applied to every provider round trip (Task 3.2).
    request_timeout: Duration,
    /// Opt-in run recorder (Task 4.2). When `None` nothing is persisted and
    /// the loop keeps the exact pre-4.2 behaviour; when attached, the run and
    /// its structured steps are persisted to `agent_runs` / `agent_steps`
    /// (DATABASE.md В§7.8, В§7.9) best-effort.
    recorder: Option<RunRecorder<'a>>,
    /// Opt-in spend limit in micro-USD (Task 4.3). `None` means no financial
    /// guard; the loop keeps the exact pre-4.3 behaviour.
    spend_limit_micro_usd: Option<u64>,
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
            event_sender: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            recorder: None,
            spend_limit_micro_usd: None,
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

    /// Attach the governance-event channel (Task 3.2). Emissions are
    /// best-effort: a receiver that stopped draining never blocks the run.
    #[must_use]
    pub(crate) fn with_event_sender(mut self, tx: Sender<AgentRunEvent>) -> Self {
        self.event_sender = Some(tx);
        self
    }

    /// Attach the opt-in run recorder (Task 4.2): the run and its structured
    /// steps are persisted to `agent_runs` / `agent_steps` (DATABASE.md
    /// В§7.8, В§7.9) best-effort. When no recorder is attached the loop keeps
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
    /// appended to `agent_steps` (DATABASE.md В§7.8, В§7.9) вЂ” all best-effort,
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
        // History opens with the fixed agent system prompt plus the user
        // request; after every tool turn the assistant's own calls and each
        // tool's result are appended natively (see module docs).
        let mut messages = vec![
            AiMessage {
                role: AiRole::System,
                content: AGENT_SYSTEM_PROMPT.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            },
            AiMessage {
                role: AiRole::User,
                content: user_request.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            },
        ];

        loop {
            // ---- Step boundary: governance gates before the next LLM turn ----

            // Cancellation is the highest-priority gate: it is checked before
            // any LLM work, again after every provider call, and between tool
            // dispatches so a cancellation never waits for further work.
            self.check_cancellation(control)?;
            self.honor_pause(control)?;
            self.honor_allowance(control, base, steps_taken)?;

            let request = AiRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                messages: messages.clone(),
                tools: tools.clone(),
                request_timeout: Some(self.request_timeout),
            };
            let turn_started = Instant::now();
            let response = self.executor.execute(&request, credential)?;
            steps_taken += 1;

            // Task 4.2: record the completed model turn (D12) with its
            // provider round-trip duration.
            if let Some(rec) = record.as_mut() {
                let duration_ms =
                    i64::try_from(turn_started.elapsed().as_millis()).unwrap_or(i64::MAX);
                rec.model_turn(&response.content, Some(duration_ms));
            }

            self.check_cancellation(control)?;

            // Task 4.3: spend guard — accumulate billed cost for this turn
            // (only when a consumer exists) and trip if the limit is exceeded.
            // Usage absent is counted as $0 (count-as-known).
            if let Some(usage) = response.usage {
                if self.spend_limit_micro_usd.is_some() || record.is_some() {
                    let cost = pricing::cost_for_usage(usage);
                    *spent_micro_usd = spent_micro_usd.saturating_add(cost);
                    if let Some(limit) = self.spend_limit_micro_usd {
                        if *spent_micro_usd > limit {
                            self.emit(AgentRunEvent::SpendLimitExceeded {
                                spent_micro: *spent_micro_usd,
                                limit_micro: limit,
                            });
                            return Err(AgentError::SpendLimitExceeded {
                                spent_micro: *spent_micro_usd,
                                limit_micro: limit,
                            });
                        }
                    }
                }
            }

            if response.tool_calls.is_empty() {
                // AC-2: no tool calls means the model is done. Usable final
                // content must be present; anything else is a controlled
                // failure rather than a silently empty success.
                if response.content.trim().is_empty() {
                    return Err(AgentError::EmptyResponse);
                }
                self.emit(AgentRunEvent::Completed { steps: steps_taken });
                return Ok(response.content);
            }

            // The model's own turn — narration plus every returned tool call —
            // is appended unconditionally so the model always sees what it
            // called; the individual observations follow as Tool messages.
            messages.push(AiMessage {
                role: AiRole::Assistant,
                content: response.content,
                attachments: Vec::new(),
                tool_calls: response.tool_calls.clone(),
                tool_result: None,
            });

            // AC-6: never drop a call вЂ” every returned call is dispatched and
            // observed. Failures are rendered through `ToolError`'s Display
            // (`Error: ...`) so the model can recover on the next turn.
            let token: &CancellationToken = control.map_or(&idle_token, RunControl::token);
            for call in &response.tool_calls {
                self.check_cancellation(control)?;
                // Task 4.1: approval gate evaluated at the per-tool-call
                // boundary, before dispatch. Auto paths execute exactly as
                // before; denied calls become a controlled observation and the
                // loop continues; cancellation while parked aborts.
                if let Some(gate) = &self.approval_gate {
                    if gate.needs_approval(call) {
                        // INVARIANT: once ApprovalRequested is emitted, a pending entry for that call_id exists,
                        // so a concurrent resolve cannot hit NoPendingApproval — the race is closed by construction.
                        gate.prepare_pending(call);
                        self.emit(AgentRunEvent::ApprovalRequested {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        });
                        let Ok(decision) = gate.request_approval(call) else {
                            // Task 4.2: cancellation ended the parked wait вЂ”
                            // record the `cancelled` approval step (D12).
                            if let Some(rec) = record.as_mut() {
                                rec.approval_cancelled(call);
                            }
                            self.emit(AgentRunEvent::Cancelled);
                            return Err(AgentError::Cancelled);
                        };
                        let approved = matches!(decision, ApprovalDecision::Approved);
                        // Task 4.2: record the parked approval decision (D12).
                        if let Some(rec) = record.as_mut() {
                            rec.approval(call, approved);
                        }
                        self.emit(AgentRunEvent::ApprovalResolved {
                            call_id: call.id.clone(),
                            approved,
                        });
                        if !approved {
                            messages.push(AiMessage {
                                role: AiRole::Tool,
                                content: String::new(),
                                attachments: Vec::new(),
                                tool_calls: Vec::new(),
                                tool_result: Some(crate::application::execution::AiToolResult {
                                    call_id: call.id.clone(),
                                    name: call.name.clone(),
                                    content: "Error: tool execution was denied by the user"
                                        .to_string(),
                                }),
                            });
                            continue;
                        }
                    }
                }
                // Task 4.2: the dispatched call (approved or ungated) is
                // recorded with its raw arguments, observation, and outcome
                // (D12). A cancellation observed by the tool records as
                // `cancelled`; everything else is `succeeded` or `failed`.
                let dispatch_started = Instant::now();
                let outcome =
                    ToolRegistry::execute_with_cancellation(call, &self.workspace_root, token);
                let dispatch_ms =
                    i64::try_from(dispatch_started.elapsed().as_millis()).unwrap_or(i64::MAX);
                let (observation, tool_status) = match outcome {
                    Ok(output) if token.is_cancelled() => (output, "cancelled"),
                    Ok(output) => (output, "succeeded"),
                    Err(tool_error) if token.is_cancelled() => {
                        (tool_error.to_string(), "cancelled")
                    }
                    Err(tool_error) => (tool_error.to_string(), "failed"),
                };
                messages.push(AiMessage {
                    role: AiRole::Tool,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: Some(crate::application::execution::AiToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: observation.clone(),
                    }),
                });
                if let Some(rec) = record.as_mut() {
                    rec.tool_call(call, &observation, tool_status, Some(dispatch_ms));
                }
            }
        }
    }

    /// Emit a governance event on the optional channel, best-effort.
    fn emit(&self, event: AgentRunEvent) {
        if let Some(tx) = &self.event_sender {
            let _ = tx.send(event);
        }
    }

    /// Return `Err(AgentError::Cancelled)` when cancellation was observed.
    fn check_cancellation(&self, control: Option<&RunControl>) -> Result<(), AgentError> {
        if matches!(control, Some(c) if c.is_cancelled()) {
            self.emit(AgentRunEvent::Cancelled);
            return Err(AgentError::Cancelled);
        }
        Ok(())
    }

    /// Honour a pending user pause at this step boundary. Emits `Paused`,
    /// blocks until `resume` (emitting `Resumed`) or `cancel` (aborting);
    /// cancelling while paused wakes the loop (no deadlock).
    fn honor_pause(&self, control: Option<&RunControl>) -> Result<(), AgentError> {
        let Some(c) = control else {
            return Ok(());
        };
        if !c.pause_pending() {
            return Ok(());
        }
        self.emit(AgentRunEvent::Paused);
        if c.wait_while_paused() {
            self.emit(AgentRunEvent::Resumed);
            Ok(())
        } else {
            self.emit(AgentRunEvent::Cancelled);
            Err(AgentError::Cancelled)
        }
    }

    /// Honour the step budget at this boundary.
    ///
    /// With a control attached, exhaustion parks the loop on
    /// `wait_for_allowance` until `extend_steps` continues it or `cancel`
    /// aborts it (`resume` alone grants no steps). Without a control, the
    /// Task 3.1 deterministic behaviour is preserved: exhaustion returns
    /// `AgentError::BudgetExhausted` immediately.
    fn honor_allowance(
        &self,
        control: Option<&RunControl>,
        base: usize,
        taken: usize,
    ) -> Result<(), AgentError> {
        let Some(c) = control else {
            if taken >= base {
                return Err(AgentError::BudgetExhausted(base));
            }
            return Ok(());
        };
        let allowance = c.allowance(base);
        if taken < allowance {
            return Ok(());
        }
        self.emit(AgentRunEvent::BudgetExhausted {
            max_steps: allowance,
        });
        if c.wait_for_allowance(base, taken) {
            Ok(())
        } else {
            self.emit(AgentRunEvent::Cancelled);
            Err(AgentError::Cancelled)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::{AiResponse, ToolCall};
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
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
        dir
    }

    fn text_response(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)] // JSON literals read best at call sites
    fn call_tool(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    fn raw_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    /// Scripted [`ProviderExecutor`] fake: replays prepared responses in order
    /// and records every incoming request. Never performs network I/O.
    struct FakeExecutor {
        steps: RefCell<std::vec::IntoIter<Result<AiResponse, ExecutorError>>>,
        requests: RefCell<Vec<AiRequest>>,
    }

    impl FakeExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
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
        ) -> Result<AiResponse, ExecutorError> {
            self.requests.borrow_mut().push(request.clone());
            self.steps
                .borrow_mut()
                .next()
                .expect("fake executor script exhausted")
        }
    }

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

    /// Every request the runner emits opens with the fixed agent system
    /// prompt (exact equality, including the target-specific Environment
    /// section chosen at compile time).
    #[test]
    fn every_request_starts_with_the_agent_system_prompt() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![ToolCall {
                    id: "s1".to_string(),
                    name: "list_directory".to_string(),
                    arguments: "{}".to_string(),
                    thought_signature: None,
                }],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        runner.run("openai", "m", "cred", "hello").expect("finish");

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request.messages[0].role, AiRole::System);
            assert_eq!(request.messages[0].content, AGENT_SYSTEM_PROMPT);
        }
        // The prompt text is the compile-time target variant.
        #[cfg(windows)]
        assert_eq!(AGENT_SYSTEM_PROMPT, AGENT_SYSTEM_PROMPT_WINDOWS);
        #[cfg(not(windows))]
        assert_eq!(AGENT_SYSTEM_PROMPT, AGENT_SYSTEM_PROMPT_POSIX);
        let _ = fs::remove_dir_all(&ws);
    }

    /// After one tool turn the second request's history is exactly
    /// [System, User, Assistant{narration, call verbatim incl.
    /// `thought_signature`}, `Tool{call_id, name, observation}`].
    #[test]
    fn second_request_history_carries_assistant_calls_and_tool_results() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: "Let me check.".to_string(),
                model: "m".to_string(),
                tool_calls: vec![ToolCall {
                    id: "c9".to_string(),
                    name: "list_directory".to_string(),
                    arguments: "{\"path\":\".\"}".to_string(),
                    thought_signature: Some("sig-abc".to_string()),
                }],
                usage: None,
            }),
            Ok(text_response("listed")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        runner
            .run("openai", "m", "cred", "list it")
            .expect("finish");

        let history = &fake.requests.borrow()[1].messages;
        assert_eq!(history.len(), 4);

        assert_eq!(
            history[0],
            AiMessage {
                role: AiRole::System,
                content: AGENT_SYSTEM_PROMPT.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }
        );
        assert_eq!(
            history[1],
            AiMessage {
                role: AiRole::User,
                content: "list it".to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }
        );
        assert_eq!(
            history[2],
            AiMessage {
                role: AiRole::Assistant,
                content: "Let me check.".to_string(),
                attachments: Vec::new(),
                tool_calls: vec![ToolCall {
                    id: "c9".to_string(),
                    name: "list_directory".to_string(),
                    arguments: "{\"path\":\".\"}".to_string(),
                    thought_signature: Some("sig-abc".to_string()),
                }],
                tool_result: None,
            }
        );
        assert_eq!(history[3].role, AiRole::Tool);
        assert_eq!(history[3].content, "");
        let result = history[3].tool_result.as_ref().expect("result present");
        assert_eq!(result.call_id, "c9");
        assert_eq!(result.name, "list_directory");
        let _ = fs::remove_dir_all(&ws);
    }

    /// Two calls in one response: the Assistant message carries both calls
    /// and the two Tool messages follow in the same order — the wire ordering
    /// contract (all function calls first, then all results).
    #[test]
    fn parallel_tool_calls_keep_call_then_result_ordering() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![
                    ToolCall {
                        id: "p1".to_string(),
                        name: "read_file".to_string(),
                        arguments: "{\"path\":\"a.txt\"}".to_string(),
                        thought_signature: Some("sig-first".to_string()),
                    },
                    ToolCall {
                        id: "p2".to_string(),
                        name: "read_file".to_string(),
                        arguments: "{\"path\":\"b.txt\"}".to_string(),
                        // Parallel calls: only the first carries a signature.
                        thought_signature: None,
                    },
                ],
                usage: None,
            }),
            Ok(text_response("both read")),
        ]);
        fs::write(ws.join("a.txt"), "alpha").expect("seed a");
        fs::write(ws.join("b.txt"), "beta").expect("seed b");
        let runner = AgentRunner::new(&fake, &ws);

        runner
            .run("openai", "m", "cred", "read both")
            .expect("finish");

        let history = &fake.requests.borrow()[1].messages;
        assert_eq!(history.len(), 5);
        // All function calls first, in order, signatures verbatim.
        assert_eq!(history[2].role, AiRole::Assistant);
        assert_eq!(history[2].tool_calls.len(), 2);
        assert_eq!(history[2].tool_calls[0].id, "p1");
        assert_eq!(
            history[2].tool_calls[0].thought_signature.as_deref(),
            Some("sig-first")
        );
        assert_eq!(history[2].tool_calls[1].id, "p2");
        assert_eq!(history[2].tool_calls[1].thought_signature, None);
        // Then all results, in the same call order.
        assert_eq!(history[3].role, AiRole::Tool);
        assert_eq!(history[3].tool_result.as_ref().expect("p1").call_id, "p1");
        assert_eq!(
            history[3].tool_result.as_ref().expect("p1").content,
            "alpha"
        );
        assert_eq!(history[4].role, AiRole::Tool);
        assert_eq!(history[4].tool_result.as_ref().expect("p2").call_id, "p2");
        assert_eq!(history[4].tool_result.as_ref().expect("p2").content, "beta");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn single_tool_call_executes_and_observation_feeds_back_to_final_answer() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool(
                    "c1",
                    "write_file",
                    serde_json::json!({
                        "path": "notes.txt",
                        "content": "react-loop"
                    }),
                )],
                usage: None,
            }),
            Ok(text_response("wrote notes.txt")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner
            .run("openai", "m", "cred", "create notes")
            .expect("finish");
        assert_eq!(answer, "wrote notes.txt");

        // The tool really executed inside the workspace.
        assert_eq!(
            fs::read_to_string(ws.join("notes.txt")).expect("file"),
            "react-loop"
        );

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 2);
        // History of the second request: [System, User, Assistant{calls},
        // Tool{observation}] — the model sees its own call and the result.
        let history = &requests[1].messages;
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, AiRole::System);
        assert_eq!(history[0].content, AGENT_SYSTEM_PROMPT);
        assert_eq!(history[1].role, AiRole::User);
        assert_eq!(history[1].content, "create notes");
        assert_eq!(history[2].role, AiRole::Assistant);
        assert_eq!(history[2].content, "");
        assert_eq!(history[2].tool_calls.len(), 1);
        assert_eq!(history[2].tool_calls[0].id, "c1");
        assert_eq!(history[2].tool_calls[0].name, "write_file");
        assert_eq!(history[3].role, AiRole::Tool);
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert_eq!(result.call_id, "c1");
        assert_eq!(result.name, "write_file");
        assert_eq!(
            result.content,
            "--- a/notes.txt\n+++ b/notes.txt\n@@ -0,0 +1,1 @@\n+react-loop\n"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn multiple_tool_calls_in_one_response_are_all_handled() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: "planning two writes".to_string(),
                model: "test-model".to_string(),
                tool_calls: vec![
                    call_tool(
                        "a",
                        "write_file",
                        serde_json::json!({
                            "path": "one.txt", "content": "1"
                        }),
                    ),
                    call_tool(
                        "b",
                        "write_file",
                        serde_json::json!({
                            "path": "two.txt", "content": "2"
                        }),
                    ),
                    call_tool("c", "read_file", serde_json::json!({"path": "one.txt"})),
                ],
                usage: None,
            }),
            Ok(text_response("did everything")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "go").expect("finish");
        assert_eq!(answer, "did everything");

        // All three calls actually executed, none dropped (AC-6).
        assert_eq!(fs::read_to_string(ws.join("one.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(ws.join("two.txt")).unwrap(), "2");

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 2);
        // [System, User, Assistant{narration + 3 calls}, Tool, Tool, Tool] —
        // results follow the calls in original order (AC-6).
        let history = &requests[1].messages;
        assert_eq!(history.len(), 6);
        assert_eq!(history[0].role, AiRole::System);
        assert_eq!(history[1].role, AiRole::User);
        assert_eq!(history[2].role, AiRole::Assistant);
        assert_eq!(history[2].content, "planning two writes");
        assert_eq!(history[2].tool_calls.len(), 3);
        let tail = &history[3..];
        assert_eq!(tail[0].role, AiRole::Tool);
        assert_eq!(tail[0].tool_result.as_ref().expect("a").call_id, "a");
        assert_eq!(
            tail[0].tool_result.as_ref().expect("a").content,
            "--- a/one.txt\n+++ b/one.txt\n@@ -0,0 +1,1 @@\n+1\n"
        );
        assert_eq!(tail[1].tool_result.as_ref().expect("b").call_id, "b");
        assert_eq!(
            tail[1].tool_result.as_ref().expect("b").content,
            "--- a/two.txt\n+++ b/two.txt\n@@ -0,0 +1,1 @@\n+2\n"
        );
        assert_eq!(tail[2].tool_result.as_ref().expect("c").call_id, "c");
        assert_eq!(tail[2].tool_result.as_ref().expect("c").name, "read_file");
        assert_eq!(tail[2].tool_result.as_ref().expect("c").content, "1");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn unknown_tool_becomes_controlled_observation_and_loop_continues() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![raw_call("u1", "does_not_exist", "{}")],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "try").expect("finish");
        assert_eq!(answer, "recovered");

        let history = &fake.requests.borrow()[1].messages;
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, AiRole::System);
        assert_eq!(history[0].content, AGENT_SYSTEM_PROMPT);
        assert_eq!(history[1].role, AiRole::User);
        assert_eq!(history[1].content, "try");
        assert_eq!(history[2].role, AiRole::Assistant);
        assert_eq!(history[2].tool_calls[0].id, "u1");
        assert_eq!(history[3].role, AiRole::Tool);
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert_eq!(result.call_id, "u1");
        assert_eq!(result.name, "does_not_exist");
        assert!(
            result.content.contains("unknown tool"),
            "unknown-tool observation missing: {}",
            result.content
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn malformed_arguments_become_error_observation_without_panic() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![raw_call("bad", "write_file", "not json at all")],
                usage: None,
            }),
            Ok(text_response("handled")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "x").expect("finish");
        assert_eq!(answer, "handled");

        let history = &fake.requests.borrow()[1].messages;
        assert_eq!(history.len(), 4);
        assert_eq!(history[3].role, AiRole::Tool);
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert!(
            result.content.contains("invalid arguments"),
            "malformed-arguments observation missing: {}",
            result.content
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn tool_execution_failure_is_an_observation_and_run_stays_controlled() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool(
                    "esc",
                    "read_file",
                    serde_json::json!({
                        "path": "../../outside.txt"
                    }),
                )],
                usage: None,
            }),
            Ok(text_response("kept going")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "sneak").expect("finish");
        assert_eq!(answer, "kept going");

        let history = &fake.requests.borrow()[1].messages;
        assert_eq!(history[3].role, AiRole::Tool);
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert!(
            result.content.contains("outside workspace"),
            "escape observation missing: {}",
            result.content
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn iteration_budget_exhaustion_terminates_deterministically() {
        let ws = temp_workspace();
        // Always demands another tool call: would loop forever unbounded.
        let step = || {
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool("loop", "list_directory", serde_json::json!({}))],
                usage: None,
            })
        };
        let fake = FakeExecutor::new(vec![step(), step(), step()]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(3);

        let err = runner
            .run("openai", "m", "cred", "loop")
            .expect_err("must exhaust");
        match err {
            AgentError::BudgetExhausted(3) => {}
            other => panic!("expected BudgetExhausted(3), got: {other:?}"),
        }
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn zero_iteration_budget_terminates_before_any_model_turn() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("never reached"))]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(0);

        assert!(matches!(
            runner
                .run("openai", "m", "cred", "q")
                .expect_err("exhausted"),
            AgentError::BudgetExhausted(0)
        ));
        assert!(fake.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn provider_failure_is_propagated_as_classified_error() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Err(ExecutorError::Failure)]);
        let runner = AgentRunner::new(&fake, &ws);

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("provider failed");
        assert!(matches!(err, AgentError::Provider(_)));
        // Exactly one attempt: failures are not retried here.
        assert_eq!(fake.requests.borrow().len(), 1);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn mid_loop_provider_failure_leaves_no_partial_success() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool("t", "list_directory", serde_json::json!({}))],
                usage: None,
            }),
            Err(ExecutorError::Failure),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("second turn fails");
        assert!(matches!(err, AgentError::Provider(_)));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn empty_text_without_tool_calls_is_a_controlled_failure() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("   "))]);
        let runner = AgentRunner::new(&fake, &ws);

        assert!(matches!(
            runner
                .run("openai", "m", "cred", "q")
                .expect_err("empty answer"),
            AgentError::EmptyResponse
        ));
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
        let response = fake.execute(&plain, "cred").expect("plain execute");
        assert_eq!(response.content, "plain reply");
        assert!(response.tool_calls.is_empty());
        assert_eq!(fake.requests.borrow().len(), 1);
    }

    /// A scripted executor that can force the runner to park *between* turns:
    /// at `block_at` (the 0-based execute index) it signals `entered` and then
    /// spins until `release` while the test drives governance, then returns
    /// the next scripted response. Deterministic and network-free.
    struct GatedExecutor {
        steps: RefCell<std::vec::IntoIter<Result<AiResponse, ExecutorError>>>,
        requests: RefCell<Vec<AiRequest>>,
        block_at: usize,
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl GatedExecutor {
        fn new(
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
    fn wait_flag(flag: &AtomicBool) {
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
    fn tool_step(id: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "m".to_string(),
            tool_calls: vec![call_tool(id, "list_directory", serde_json::json!({}))],
            usage: None,
        }
    }

    // -----------------------------------------------------------------------
    // Step governor & cancellation (Task 3.2)
    // -----------------------------------------------------------------------

    #[test]
    fn control_activity_is_seen_by_attached_run_control() {
        // The handle is cheaply cloneable and every clone governs the same
        // underlying state.
        let control = RunControl::new();
        let other = control.clone();
        other.extend_steps(4);
        assert_eq!(control.extra_steps(), 4);
        assert_eq!(control.allowance(10), 14);
        assert!(!control.is_cancelled());
        other.cancel();
        assert!(control.is_cancelled());
    }

    #[test]
    fn fixed_budget_without_control_still_hard_stops_deterministically() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("later")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_max_iterations(2);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must exhaust");
        assert!(matches!(err, AgentError::BudgetExhausted(2)));
        // Exactly the fixed allowance ran; no silent continuation.
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn exhausted_budget_extend_continues_then_completes() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.extend_steps(1);
            let done = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, done)
        });

        let answer = runner.run("openai", "m", "cred", "go").expect("completes");
        let (first, done) = driver.join().expect("driver joins");
        assert_eq!(
            first,
            AgentRunEvent::BudgetExhausted { max_steps: 2 },
            "first governance event must be the exhaustion"
        );
        assert_eq!(done, AgentRunEvent::Completed { steps: 3 });
        assert_eq!(answer, "done");
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn exhausted_budget_cancel_aborts_with_cancelled() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(tool_step("a"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(1)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.cancel();
            let second = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, second)
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        let (first, second) = driver.join().expect("driver joins");
        assert_eq!(first, AgentRunEvent::BudgetExhausted { max_steps: 1 });
        assert_eq!(second, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn resume_does_not_end_an_exhausted_budget_wait() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let fake = FakeExecutor::new(vec![Ok(tool_step("a")), Ok(text_response("step"))]);
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(1)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            // resume() alone must NOT grant any steps while parked over budget.
            control.resume();
            // Still parked: no further event (=> no Completed) unless extended.
            let stale = rx.recv_timeout(Duration::from_millis(300));
            assert!(
                stale.is_err(),
                "resume() must not unpark an exhausted-budget wait: got {stale:?}"
            );
            control.extend_steps(1);
            let done = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, done)
        });

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("completes after extension");
        let (first, done) = driver.join().expect("driver joins");
        assert_eq!(first, AgentRunEvent::BudgetExhausted { max_steps: 1 });
        assert_eq!(done, AgentRunEvent::Completed { steps: 2 });
        assert_eq!(answer, "step");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn pause_then_resume_mid_run_emits_events_and_continues() {
        let ws = temp_workspace();
        let control = RunControl::new();
        // Pause before the run starts: it will park at the very first step
        // boundary.
        control.pause();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(3)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let paused = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.resume();
            let resumed = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            let completed = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (paused, resumed, completed)
        });

        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        let (paused, resumed, completed) = driver.join().expect("driver joins");
        assert_eq!(paused, AgentRunEvent::Paused);
        assert_eq!(resumed, AgentRunEvent::Resumed);
        assert_eq!(completed, AgentRunEvent::Completed { steps: 3 });
        assert_eq!(answer, "ok");
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancelling_while_paused_wakes_the_loop_without_deadlock() {
        let ws = temp_workspace();
        let control = RunControl::new();
        control.pause();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(text_response("never"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            let paused = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.cancel();
            let cancelled = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (paused, cancelled)
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        let (paused, cancelled) = driver.join().expect("driver joins");
        assert_eq!(paused, AgentRunEvent::Paused);
        assert_eq!(cancelled, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        // Zero model turns: nothing ran after the parked pause.
        assert!(fake.requests.borrow().is_empty());
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancellation_between_provider_calls_stops_further_work() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let (tx, rx) = channel();
        let (gated, entered, release) = GatedExecutor::new(
            vec![
                // Turn 1: dispatch one real workspace write.
                Ok(AiResponse {
                    content: String::new(),
                    model: "m".to_string(),
                    tool_calls: vec![call_tool(
                        "w1",
                        "write_file",
                        serde_json::json!({ "path": "a.txt", "content": "1" }),
                    )],
                    usage: None,
                }),
                // Turn 2 (block_at = 1): parks until the test cancels, then
                // yields a fresh tool call that must never run.
                Ok(AiResponse {
                    content: String::new(),
                    model: "m".to_string(),
                    tool_calls: vec![call_tool(
                        "w2",
                        "write_file",
                        serde_json::json!({ "path": "b.txt", "content": "2" }),
                    )],
                    usage: None,
                }),
                Ok(text_response("never")),
            ],
            1,
        );
        let runner = AgentRunner::new(&gated, &ws)
            .with_max_iterations(3)
            .with_control(control.clone())
            .with_event_sender(tx);

        let driver = thread::spawn(move || {
            // Wait until the second provider call is actually mid-flight.
            wait_flag(&entered);
            control.cancel();
            // Release the blocked second call so the runner can observe cancel.
            release.store(true, Ordering::SeqCst);
            // The runner aborts with the Cancelled event.
            rx.recv_timeout(Duration::from_secs(5)).expect("event")
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        // Cancellation surfaces after the in-flight call returns; the pending
        // tool call of turn 2 must never be dispatched.
        let cancelled = driver.join().expect("driver joins");
        assert_eq!(cancelled, AgentRunEvent::Cancelled);
        assert!(matches!(err, AgentError::Cancelled));
        assert_eq!(gated.requests.borrow().len(), 2, "only two LLM turns ran");
        assert!(
            fs::read_to_string(ws.join("a.txt")).is_ok(),
            "turn-1 tool ran"
        );
        assert!(
            fs::read_to_string(ws.join("b.txt")).is_err(),
            "turn-2 tool must not run after cancellation"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn runner_requests_carry_default_request_timeout_and_override_flows_through() {
        let ws = temp_workspace();
        let default_fake = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let default_runner = AgentRunner::new(&default_fake, &ws);
        default_runner
            .run("openai", "m", "cred", "q")
            .expect("finish");
        let captured = &default_fake.requests.borrow()[0];
        assert_eq!(
            captured.request_timeout,
            Some(DEFAULT_REQUEST_TIMEOUT),
            "runner applies its configurable default timeout to every request"
        );
        let _ = fs::remove_dir_all(&ws);

        // A custom timeout overrides it.
        let custom_fake = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let custom = Duration::from_secs(7);
        let custom_runner = AgentRunner::new(&custom_fake, &ws).with_request_timeout(custom);
        custom_runner
            .run("openai", "m", "cred", "q")
            .expect("finish");
        assert_eq!(
            custom_fake.requests.borrow()[0].request_timeout,
            Some(custom)
        );
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Three-tier approval gate (Task 4.1)
    // -----------------------------------------------------------------------

    use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};

    #[allow(clippy::needless_pass_by_value)]
    fn approval_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    #[allow(dead_code)]
    fn run_with_gate(
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

    #[test]
    fn approval_matrix_supervised_read_requires_approval() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "r1",
                    "read_file",
                    serde_json::json!({"path": "exists.txt"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        // Create a file to read.
        fs::write(ws.join("exists.txt"), "hello").expect("write");
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let ev = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("ApprovalRequested");
            assert!(
                matches!(ev, AgentRunEvent::ApprovalRequested { call_id, name, .. } if call_id=="r1" && name=="read_file")
            );
            assert!(gate_for_driver.respond("r1", ApprovalDecision::Approved));
            let ev2 = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("ApprovalResolved");
            assert!(
                matches!(ev2, AgentRunEvent::ApprovalResolved { call_id, approved } if call_id=="r1" && approved)
            );
            rx.recv_timeout(Duration::from_secs(5)).expect("Completed")
        });
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "done");
        let completed = driver.join().expect("driver");
        assert_eq!(completed, AgentRunEvent::Completed { steps: 2 });
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_matrix_supervised_mutating_requires_approval() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "out.txt", "content": "hi"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let ev = rx.recv_timeout(Duration::from_secs(5)).expect("requested");
            assert!(
                matches!(ev, AgentRunEvent::ApprovalRequested { call_id, .. } if call_id=="w1")
            );
            gate_for_driver.respond("w1", ApprovalDecision::Approved);
            let ev2 = rx.recv_timeout(Duration::from_secs(5)).expect("resolved");
            assert!(matches!(
                ev2,
                AgentRunEvent::ApprovalResolved { approved: true, .. }
            ));
            rx.recv_timeout(Duration::from_secs(5)).expect("completed")
        });
        runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(fs::read_to_string(ws.join("out.txt")).expect("file"), "hi");
        let completed = driver.join().expect("driver");
        assert_eq!(completed, AgentRunEvent::Completed { steps: 2 });
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_matrix_semi_read_auto_approved_no_events() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        let (tx, rx) = channel();
        fs::write(ws.join("a.txt"), "content").expect("write");
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "r1",
                    "read_file",
                    serde_json::json!({"path": "a.txt"}),
                )],
                usage: None,
            }),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_event_sender(tx);
        let answer = runner.run("openai", "m", "cred", "q").expect("auto");
        assert_eq!(answer, "ok");
        // No approval events for auto path; only Completed may be present.
        let mut saw_approval = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(
                ev,
                AgentRunEvent::ApprovalRequested { .. } | AgentRunEvent::ApprovalResolved { .. }
            ) {
                saw_approval = true;
            }
        }
        assert!(!saw_approval, "auto path must not emit approval events");
        // File still there, tool executed.
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "content");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_matrix_semi_mutating_requires_approval() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "execute_command",
                    serde_json::json!({"command": "echo hi"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let ev = rx.recv_timeout(Duration::from_secs(5)).expect("requested");
            assert!(
                matches!(ev, AgentRunEvent::ApprovalRequested { name, .. } if name=="execute_command")
            );
            gate_for_driver.respond("w1", ApprovalDecision::Approved);
            rx.recv_timeout(Duration::from_secs(5)).expect("resolved");
            rx.recv_timeout(Duration::from_secs(5)).expect("completed")
        });
        runner.run("openai", "m", "cred", "q").expect("completes");
        driver.join().expect("driver");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_matrix_full_read_auto() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call("r1", "list_directory", serde_json::json!({}))],
                usage: None,
            }),
            Ok(text_response("listed")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_approval_gate(gate);
        let answer = runner.run("openai", "m", "cred", "q").expect("auto");
        assert_eq!(answer, "listed");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_matrix_full_mutating_auto() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "auto.txt", "content": "x"}),
                )],
                usage: None,
            }),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_approval_gate(gate);
        runner.run("openai", "m", "cred", "q").expect("auto");
        assert_eq!(fs::read_to_string(ws.join("auto.txt")).unwrap(), "x");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn denied_call_becomes_observation_and_loop_continues() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "should_not_exist.txt", "content": "bad"}),
                )],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let ev = rx.recv_timeout(Duration::from_secs(5)).expect("requested");
            assert!(matches!(ev, AgentRunEvent::ApprovalRequested { .. }));
            gate_for_driver.respond("w1", ApprovalDecision::Denied);
            let ev2 = rx.recv_timeout(Duration::from_secs(5)).expect("resolved");
            assert!(matches!(
                ev2,
                AgentRunEvent::ApprovalResolved {
                    approved: false,
                    ..
                }
            ));
            rx.recv_timeout(Duration::from_secs(5)).expect("completed")
        });
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "recovered");
        driver.join().expect("driver");
        // Denied tool must not have executed.
        assert!(fs::read_to_string(ws.join("should_not_exist.txt")).is_err());
        // The denied observation was fed to the next LLM turn as a Tool
        // message with the verbatim denial text.
        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 2);
        let history = &requests[1].messages;
        assert_eq!(history[3].role, AiRole::Tool);
        let result = history[3]
            .tool_result
            .as_ref()
            .expect("tool result present");
        assert_eq!(
            result.content, "Error: tool execution was denied by the user",
            "denied observation must be verbatim"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancel_while_awaiting_approval_aborts_with_cancelled_no_deadlock() {
        let ws = temp_workspace();
        let control = RunControl::new();
        let gate = ApprovalGate::with_token(AutonomyMode::Supervised, control.token().clone());
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(AiResponse {
            content: String::new(),
            model: "m".to_string(),
            tool_calls: vec![approval_call(
                "c1",
                "write_file",
                serde_json::json!({"path": "x.txt", "content": "y"}),
            )],
            usage: None,
        })]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_control(control.clone())
            .with_approval_gate(gate)
            .with_event_sender(tx);
        let driver = thread::spawn(move || {
            let ev = rx.recv_timeout(Duration::from_secs(5)).expect("requested");
            assert!(matches!(ev, AgentRunEvent::ApprovalRequested { .. }));
            control.cancel();
            rx.recv_timeout(Duration::from_secs(5)).expect("cancelled")
        });
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("cancelled");
        assert!(matches!(err, AgentError::Cancelled));
        let cancelled = driver.join().expect("driver");
        assert_eq!(cancelled, AgentRunEvent::Cancelled);
        // No file should have been written; no further LLM work.
        assert!(fs::read_to_string(ws.join("x.txt")).is_err());
        assert_eq!(fake.requests.borrow().len(), 1);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn cancel_via_gate_while_parked_also_aborts() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![Ok(AiResponse {
            content: String::new(),
            model: "m".to_string(),
            tool_calls: vec![approval_call(
                "c1",
                "read_file",
                serde_json::json!({"path": "a.txt"}),
            )],
            usage: None,
        })]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let _ = rx.recv_timeout(Duration::from_secs(5)).expect("requested");
            gate_for_driver.cancel();
            rx.recv_timeout(Duration::from_secs(5)).expect("cancelled")
        });
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("cancelled");
        assert!(matches!(err, AgentError::Cancelled));
        driver.join().expect("driver");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn runtime_mode_switch_mid_run_changes_next_decision() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "first.txt", "content": "1"}),
                )],
                usage: None,
            }),
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w2",
                    "write_file",
                    serde_json::json!({"path": "second.txt", "content": "2"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        // Hang fuse: `run` must stay on this thread (`AgentRunner` holds
        // `&dyn ProviderExecutor`, which is not `Send`, so it cannot move to
        // a spawned thread without touching production bounds), so a watchdog
        // cancels the gate if the run has not finished within 10s. A parked
        // run then aborts instead of hanging the suite forever. The watchdog
        // stands down the moment the run reports back, so a passing run
        // costs no extra seconds.
        let fuse_fired = Arc::new(AtomicBool::new(false));
        let (finish_tx, finish_rx) = channel();
        let watchdog = {
            let fuse_fired = fuse_fired.clone();
            let gate = gate.clone();
            thread::spawn(move || {
                if finish_rx.recv_timeout(Duration::from_secs(10)).is_err() {
                    fuse_fired.store(true, Ordering::SeqCst);
                    gate.cancel();
                }
            })
        };
        // Every driver failure cancels first: otherwise a parked run would
        // never return and the suite would hang on `run` below.
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            // First tool requires approval in Supervised.
            let Ok(ev1) = rx.recv_timeout(Duration::from_secs(5)) else {
                gate_for_driver.cancel();
                panic!("first requested: timed out");
            };
            let w1_requested =
                matches!(&ev1, AgentRunEvent::ApprovalRequested { call_id, .. } if call_id == "w1");
            if !w1_requested {
                gate_for_driver.cancel();
                panic!("first event must request approval for w1, got {ev1:?}");
            }
            gate_for_driver.respond("w1", ApprovalDecision::Approved);
            // Switch immediately: the earliest moment the next tool can see
            // Full, with no `recv` in between that would let w2 park while
            // still Supervised and strand `run` on a second approval.
            gate_for_driver.set_mode(AutonomyMode::FullAutonomous);
            // Do not assume the next event is Completed: drain until
            // `Completed { steps: 3 }`, skipping ApprovalResolved / step /
            // tool events. Cap the drain so a flood cannot loop.
            let mut seen = 0;
            loop {
                if seen >= 32 {
                    gate_for_driver.cancel();
                    panic!("too many events without Completed");
                }
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(AgentRunEvent::Completed { steps }) => {
                        assert_eq!(steps, 3);
                        break;
                    }
                    Ok(AgentRunEvent::Cancelled) => {
                        gate_for_driver.cancel();
                        panic!("unexpected terminal: Cancelled");
                    }
                    Ok(_) => {
                        seen += 1;
                    }
                    Err(_) => {
                        gate_for_driver.cancel();
                        panic!("timed out waiting for Completed");
                    }
                }
            }
        });
        let result = runner.run("openai", "m", "cred", "q");
        let _ = finish_tx.send(());
        watchdog.join().expect("watchdog joins");
        assert!(!fuse_fired.load(Ordering::SeqCst), "run hung");
        let answer = result.expect("completes");
        assert_eq!(answer, "done");
        driver.join().expect("driver");
        assert_eq!(fs::read_to_string(ws.join("first.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(ws.join("second.txt")).unwrap(), "2");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn no_gate_default_path_unchanged() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "no_gate.txt", "content": "ok"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "done");
        assert_eq!(fs::read_to_string(ws.join("no_gate.txt")).unwrap(), "ok");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_gate_builder_is_additive_and_cloneable() {
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let control = RunControl::new();
        let fake = FakeExecutor::new(vec![Ok(text_response("hi"))]);
        // Order: control then gate.
        let runner1 = AgentRunner::new(&fake, &ws)
            .with_control(control.clone())
            .with_approval_gate(gate.clone());
        let fake2 = FakeExecutor::new(vec![Ok(text_response("hi"))]);
        // Order: gate then control.
        let runner2 = AgentRunner::new(&fake2, &ws)
            .with_approval_gate(gate.clone())
            .with_control(control.clone());
        // Both should be constructible and behave identically for auto path.
        // FullAutonomous gate with no approval needed should complete regardless of order.
        let full_gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        let fake3 = FakeExecutor::new(vec![Ok(text_response("ok"))]);
        let r = AgentRunner::new(&fake3, &ws)
            .with_control(control)
            .with_approval_gate(full_gate);
        let ans = r.run("openai", "m", "cred", "q").expect("ok");
        assert_eq!(ans, "ok");
        drop(runner1);
        drop(runner2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn approval_immediate_resolve_after_requested_proceeds_without_race() {
        // Deterministic regression for the emit-before-park race: upon receiving
        // ApprovalRequested, immediately resolve via respond (no sleep) and expect
        // the call to proceed as approved.
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "race-1",
                    "write_file",
                    serde_json::json!({"path": "immediate.txt", "content": "immediate ok"}),
                )],
                usage: None,
            }),
            Ok(text_response("done after immediate approve")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate_clone)
            .with_event_sender(tx);
        let gate_for_driver = gate.clone();
        let driver = thread::spawn(move || {
            let ev = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("ApprovalRequested");
            assert!(
                matches!(ev, AgentRunEvent::ApprovalRequested { call_id, .. } if call_id == "race-1")
            );
            // Immediate resolve — must succeed; the race is closed by construction
            // because prepare_pending ran before the emit.
            let resolved = gate_for_driver.respond("race-1", ApprovalDecision::Approved);
            assert!(
                resolved,
                "immediate respond must succeed — race closed by construction"
            );
            let ev2 = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("ApprovalResolved");
            assert!(matches!(
                ev2,
                AgentRunEvent::ApprovalResolved { approved: true, .. }
            ));
            rx.recv_timeout(Duration::from_secs(5)).expect("Completed")
        });
        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("run completes after immediate approval");
        assert_eq!(answer, "done after immediate approve");
        assert_eq!(
            fs::read_to_string(ws.join("immediate.txt")).expect("file written"),
            "immediate ok"
        );
        driver.join().expect("driver");
        let _ = fs::remove_dir_all(&ws);
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
    // -----------------------------------------------------------------------
    // Spend guard (Task 4.3)
    // -----------------------------------------------------------------------

    use crate::application::execution::TokenUsage;

    fn usage_response(content: &str, input: u64, output: u64) -> AiResponse {
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

    fn usage_tool_response(id: &str, input: u64, output: u64) -> AiResponse {
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

    #[test]
    fn spend_guard_trips_exactly_on_exceed() {
        let ws = temp_workspace();
        // Each turn costs 1_000_000 micro (200_000 input tokens * 5_000_000 / 1M)
        let cheap = |id| usage_tool_response(id, 200_000, 0);
        let fake = FakeExecutor::new(vec![
            Ok(cheap("a")),
            Ok(cheap("b")),
            Ok(usage_response("final", 200_000, 0)),
        ]);
        let limit = 2_500_000u64; // 2.5M, so 2*1M=2M under, 3*1M=3M over
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(limit)
            .with_event_sender(tx);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must trip");
        match err {
            AgentError::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(spent_micro, 3_000_000);
                assert_eq!(limit_micro, limit);
            }
            other => panic!("expected SpendLimitExceeded, got {other:?}"),
        }
        // Event payload correct
        let ev = rx.recv_timeout(Duration::from_secs(2)).expect("event");
        assert_eq!(
            ev,
            AgentRunEvent::SpendLimitExceeded {
                spent_micro: 3_000_000,
                limit_micro: limit
            }
        );
        // Two tool calls ran (first two turns), third was final but tripped before return
        assert_eq!(fake.requests.borrow().len(), 3);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_no_limit_behaves_identical() {
        let ws = temp_workspace();
        // Same script as above, but no limit — must complete normally
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![call_tool("a", "list_directory", serde_json::json!({}))],
                usage: Some(TokenUsage {
                    input_tokens: 200_000,
                    output_tokens: 0,
                }),
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);
        let ans = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(ans, "done");
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_usage_none_adds_zero() {
        let ws = temp_workspace();
        // First turn: usage None (cost 0), second: cheap 1M, limit 500k -> second trips
        // Actually first None adds 0, spent 0, second 1M >500k trips
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![call_tool("a", "list_directory", serde_json::json!({}))],
                usage: None,
            }),
            Ok(usage_tool_response("b", 200_000, 0)),
            Ok(text_response("never")),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_spend_limit(500_000);
        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("trips on second");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        // Only 2 turns ran (first None + second that tripped)
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_event_and_error_payload_correct() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        // 400k *5M/1M =2_000_000
        let limit = 1_000_000u64;
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(limit)
            .with_event_sender(tx);
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        match &err {
            AgentError::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(*spent_micro, 2_000_000);
                assert_eq!(*limit_micro, limit);
                // Display contains integers, no secrets
                let s = format!("{err}");
                assert!(s.contains("2000000"));
                assert!(s.contains("1000000"));
            }
            _ => panic!("wrong error"),
        }
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        match ev {
            AgentRunEvent::SpendLimitExceeded {
                spent_micro,
                limit_micro,
            } => {
                assert_eq!(spent_micro, 2_000_000);
                assert_eq!(limit_micro, limit);
            }
            _ => panic!("wrong event"),
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_recorder_persists_status_and_spend() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(1_000_000)
            .with_run_recorder(RunRecorder::new(&db));
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        let runs = AgentRunRepository::new(&db);
        let run = runs.list_runs_by_started_at_desc().expect("list")[0].clone();
        assert_eq!(run.status, "spend_limit_exceeded");
        assert_eq!(run.spent_micro_usd, Some(2_000_000));
        assert_eq!(run.limit_micro_usd, Some(1_000_000));
        assert_eq!(run.error, None);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_non_recorded_still_emits_event() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(usage_response("hi", 400_000, 0))]);
        let (tx, rx) = channel();
        let runner = AgentRunner::new(&fake, &ws)
            .with_spend_limit(1_000_000)
            .with_event_sender(tx);
        let err = runner.run("openai", "m", "cred", "q").expect_err("trips");
        assert!(matches!(err, AgentError::SpendLimitExceeded { .. }));
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ev, AgentRunEvent::SpendLimitExceeded { .. }));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_step_governor_untouched() {
        let ws = temp_workspace();
        // BudgetExhausted should still happen when max_iterations hit, even with a spend limit that is not tripped
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 10, 0)), // cost tiny 50 micro
            Ok(usage_tool_response("b", 10, 0)),
            Ok(text_response("later")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_spend_limit(10_000_000); // high, not tripped
        let err = runner.run("openai", "m", "cred", "q").expect_err("budget");
        assert!(matches!(err, AgentError::BudgetExhausted(2)));
        assert_eq!(fake.requests.borrow().len(), 2);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_guard_exactly_at_limit_does_not_trip() {
        let ws = temp_workspace();
        // Cost 1M per turn, limit 2M, two turns exactly at limit -> should complete
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 200_000, 0)),
            Ok(usage_response("done", 200_000, 0)),
        ]);
        let runner = AgentRunner::new(&fake, &ws).with_spend_limit(2_000_000);
        let ans = runner
            .run("openai", "m", "cred", "q")
            .expect("at limit completes");
        assert_eq!(ans, "done");
        let _ = fs::remove_dir_all(&ws);
    }
}
