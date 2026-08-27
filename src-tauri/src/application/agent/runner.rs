//! Agent execution service: the multi-step agent `ReAct` loop (ROADMAP.md
//! Phase 3 — Task 3.1).
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
//! provider payloads. Every returned tool call — including unknown tools,
//! malformed arguments, and failing invocations — is dispatched through
//! [`ToolRegistry`] and converted into a textual observation message that is
//! appended to the conversation history for the next model turn.
//!
//! # Termination
//!
//! The loop finishes successfully when a provider response carries no tool
//! calls and usable final assistant content (AC-2). It terminates
//! deterministically once `max_iterations` model turns are exhausted, and it
//! propagates provider failures as classified [`AgentError`] values without
//! panicking (AC-9, AC-10). No adaptive budgeting lives here (Task 3.2).
//!
//! # Observation representation
//!
//! The provider-independent boundary (`AiRequest` / `AiMessage`) deliberately
//! has no tool-role message today, so observations are recorded in the
//! chronological history as [`AiRole::User`] messages carrying an explicit
//! `[tool '<name>' result]` fence plus the call identifier. This invents no
//! new wire format: every observation crosses the exact existing
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
//! # Observation representation
//!
//! The provider-independent boundary (`AiRequest` / `AiMessage`) deliberately
//! has no tool-role message today, so observations are recorded in the
//! chronological history as [`AiRole::User`] messages carrying an explicit
//! `[tool '<name>' result]` fence plus the call identifier. This invents no
//! new wire format: every observation crosses the exact existing
//! provider-neutral text channel that executors already render.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{
    AiMessage, AiRequest, AiRole, ExecutorError, ProviderExecutor, ToolCall,
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

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Classified agent-loop failure. Carries no secret payload and never embeds
/// credential material (ARCHITECTURE.md §9, §11): the provider variant wraps
/// the already-classified [`ExecutorError`].
#[derive(Debug)]
pub(crate) enum AgentError {
    /// The provider failed to fulfil one of the loop's requests.
    Provider(ExecutorError),
    /// The iteration budget was exhausted before the model produced a final
    /// answer. With no [`RunControl`] attached this aborts outright; with one
    /// attached the run first parked at the boundary awaiting `extend_steps`
    /// and only aborts if it was instead cancelled.
    BudgetExhausted(usize),
    /// The provider returned neither tool calls nor usable final content.
    EmptyResponse,
    /// A user presented the run via [`RunControl::cancel`] (or cancellation
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
            Self::BudgetExhausted(_) | Self::EmptyResponse | Self::Cancelled => None,
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
/// owns no conversation state between [`Self::run`] calls and performs no
/// persistence, approval gating, or cancellation (later tasks).
pub(crate) struct AgentRunner<'a> {
    executor: &'a dyn ProviderExecutor,
    workspace_root: PathBuf,
    max_iterations: usize,
    /// Optional governance handle (Task 3.2). When `None` the loop keeps the
    /// exact deterministic Task 3.1 semantics; `pause`/`resume`/`extend_steps`
    /// are no-ops and cancellation never fires.
    control: Option<RunControl>,
    /// Optional governance-event channel (Task 3.2); Milestone 5 bridges it to
    /// Tauri events. Delivery is best-effort.
    event_sender: Option<Sender<AgentRunEvent>>,
    /// Per-request timeout applied to every provider round trip (Task 3.2).
    request_timeout: Duration,
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
            event_sender: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
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
        self.control = Some(control);
        self
    }

    /// Attach the governance-event channel (Task 3.2). Emissions are
    /// best-effort: a receiver that stopped draining never blocks the run.
    #[must_use]
    pub(crate) fn with_event_sender(mut self, tx: Sender<AgentRunEvent>) -> Self {
        self.event_sender = Some(tx);
        self
    }

    /// Override the default per-request HTTP timeout (Task 3.2).
    #[must_use]
    pub(crate) fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
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
    pub(crate) fn run(
        &self,
        provider: &str,
        model: &str,
        credential: &str,
        user_request: &str,
    ) -> Result<String, AgentError> {
        let tools = ToolRegistry::definitions();
        // A control never cancelled the plan: when the runner has no attached
        // control it dispatches tools through a never-firing token so the
        // undisputed Task 3.1 behaviour is preserved exactly.
        let idle_token = CancellationToken::new();
        let control = self.control.as_ref();
        let base = self.max_iterations;
        let mut steps_taken: usize = 0;
        let mut messages = vec![AiMessage {
            role: AiRole::User,
            content: user_request.to_string(),
            attachments: Vec::new(),
        }];

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
            let response = self.executor.execute(&request, credential)?;
            steps_taken += 1;

            self.check_cancellation(control)?;

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

            // Preserve the assistant's own narration turn (when present) in
            // chronological order before its observations.
            if !response.content.trim().is_empty() {
                messages.push(AiMessage {
                    role: AiRole::Assistant,
                    content: response.content,
                    attachments: Vec::new(),
                });
            }

            // AC-6: never drop a call — every returned call is dispatched and
            // observed. Failures are rendered through `ToolError`'s Display
            // (`Error: ...`) so the model can recover on the next turn.
            let token: &CancellationToken = control.map_or(&idle_token, RunControl::token);
            for call in &response.tool_calls {
                self.check_cancellation(control)?;
                let observation = match ToolRegistry::execute_with_cancellation(
                    call,
                    &self.workspace_root,
                    token,
                ) {
                    Ok(output) => output,
                    Err(tool_error) => tool_error.to_string(),
                };
                messages.push(observation_message(call, &observation));
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

/// Build the observation [`AiMessage`] for one dispatched tool call.
///
/// Recorded as a `User` turn fenced by an explicit header so the next model
/// invocation can reason over the output (AC-5) using only the existing
/// provider-neutral message shape.
fn observation_message(call: &ToolCall, observation: &str) -> AiMessage {
    AiMessage {
        role: AiRole::User,
        attachments: Vec::new(),
        content: format!(
            "[tool '{}' (id {}) result]\n{observation}",
            call.name, call.id
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::AiResponse;
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
        }
    }

    #[allow(clippy::needless_pass_by_value)] // JSON literals read best at call sites
    fn call_tool(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    fn raw_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
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
        // History of the second request keeps the original user turn and adds
        // exactly the fenced observation (AC-5).
        assert_eq!(requests[1].messages[0].content, "create notes");
        assert_eq!(requests[1].messages.len(), 2);
        assert_eq!(
            requests[1].messages[1].content,
            "[tool 'write_file' (id c1) result]\nSuccessfully wrote 10 bytes to 'notes.txt'"
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
        // user + assistant narration + three observations in original order.
        let tail = &requests[1].messages[requests[1].messages.len() - 3..];
        assert_eq!(tail.len(), 3);
        assert_eq!(
            tail[0].content,
            "[tool 'write_file' (id a) result]\nSuccessfully wrote 1 bytes to 'one.txt'"
        );
        assert_eq!(
            tail[1].content,
            "[tool 'write_file' (id b) result]\nSuccessfully wrote 1 bytes to 'two.txt'"
        );
        assert_eq!(tail[2].content, "[tool 'read_file' (id c) result]\n1");
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
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "try").expect("finish");
        assert_eq!(answer, "recovered");

        assert_eq!(fake.requests.borrow()[1].messages[0].content, "try");
        let observation = &fake.requests.borrow()[1].messages[1].content;
        assert!(
            observation.contains("[tool 'does_not_exist' (id u1) result]"),
            "observation header missing: {observation}"
        );
        assert!(observation.contains("unknown tool"));
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
            }),
            Ok(text_response("handled")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "x").expect("finish");
        assert_eq!(answer, "handled");

        let observation = &fake.requests.borrow()[1].messages[1].content;
        assert!(observation.contains("invalid arguments"));
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
            }),
            Ok(text_response("kept going")),
        ]);
        let runner = AgentRunner::new(&fake, &ws);

        let answer = runner.run("openai", "m", "cred", "sneak").expect("finish");
        assert_eq!(answer, "kept going");

        let observation = &fake.requests.borrow()[1].messages[1].content;
        assert!(observation.contains("outside workspace"));
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
}
