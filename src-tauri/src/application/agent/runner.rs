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
//! provider-neutral text channel that executors already render.

use std::path::{Path, PathBuf};

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
/// [`AgentError::BudgetExhausted`] (AC-9). This is a fixed bound by design;
/// adaptive budgets belong to Task 3.2.
pub(crate) const DEFAULT_MAX_ITERATIONS: usize = 10;

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
    /// answer.
    BudgetExhausted(usize),
    /// The provider returned neither tool calls nor usable final content.
    EmptyResponse,
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
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(err) => Some(err),
            Self::BudgetExhausted(_) | Self::EmptyResponse => None,
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
}

impl<'a> AgentRunner<'a> {
    /// Create a runner over `executor`, confining all tool filesystem access
    /// to `workspace_root`.
    pub(crate) fn new(executor: &'a dyn ProviderExecutor, workspace_root: &Path) -> Self {
        Self {
            executor,
            workspace_root: workspace_root.to_path_buf(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
        }
    }

    /// Override the fixed per-run iteration bound (AC-9). A bound of zero
    /// makes every run terminate immediately with budget exhaustion.
    #[must_use]
    pub(crate) fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
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
    /// # Errors
    ///
    /// Returns [`AgentError::EmptyResponse`] when a response contains neither
    /// tool calls nor usable content; [`AgentError::BudgetExhausted`] when
    /// `max_iterations` model turns complete without a final answer;
    /// [`AgentError::Provider`] when any underlying request fails.
    pub(crate) fn run(
        &self,
        provider: &str,
        model: &str,
        credential: &str,
        user_request: &str,
    ) -> Result<String, AgentError> {
        let tools = ToolRegistry::definitions();
        let mut messages = vec![AiMessage {
            role: AiRole::User,
            content: user_request.to_string(),
            attachments: Vec::new(),
        }];

        for _ in 0..self.max_iterations {
            let request = AiRequest {
                provider: provider.to_string(),
                model: model.to_string(),
                messages: messages.clone(),
                tools: tools.clone(),
            };
            let response = self.executor.execute(&request, credential)?;

            if response.tool_calls.is_empty() {
                // AC-2: no tool calls means the model is done. Usable final
                // content must be present; anything else is a controlled
                // failure rather than a silently empty success.
                if response.content.trim().is_empty() {
                    return Err(AgentError::EmptyResponse);
                }
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
            for call in &response.tool_calls {
                let observation = match ToolRegistry::execute(call, &self.workspace_root) {
                    Ok(output) => output,
                    Err(tool_error) => tool_error.to_string(),
                };
                messages.push(observation_message(call, &observation));
            }
        }

        Err(AgentError::BudgetExhausted(self.max_iterations))
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        };
        assert!(plain.tools.is_empty());
        assert!(!ToolRegistry::definitions().is_empty());

        let fake = FakeExecutor::new(vec![Ok(text_response("plain reply"))]);
        let response = fake.execute(&plain, "cred").expect("plain execute");
        assert_eq!(response.content, "plain reply");
        assert!(response.tool_calls.is_empty());
        assert_eq!(fake.requests.borrow().len(), 1);
    }
}
