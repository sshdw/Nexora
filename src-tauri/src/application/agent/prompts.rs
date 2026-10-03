//! Agent system prompts and message constructors (split from `runner`).

use crate::application::agent::action_memory::{self, ActionSummary};
use crate::application::agent::assembly;
use crate::application::agent::compaction;
use crate::application::agent::control::CancellationToken;
use crate::application::agent::history;
use crate::application::agent::injection;
use crate::application::agent::lifecycle::RunState;
use crate::application::agent::pipeline::PipelineStage;
use crate::application::execution::{AiMessage, AiRole};

/// Fixed system prompt for Windows hosts (the primary target).
pub(crate) const AGENT_SYSTEM_PROMPT_WINDOWS: &str = "You are Nexora, a desktop agent working on the user's machine.\n\nEnvironment:\n- The operating system is Windows; execute_command runs each command through cmd.exe, so Unix shell utilities such as ls, cat or grep are unavailable - use their Windows equivalents (dir, type, findstr).\n- The file tools read_file, write_file and list_directory operate inside a dedicated agent workspace directory. Relative paths resolve against the workspace, paths outside it are rejected, and execute_command runs with the workspace as its current directory.\n\nWorkflow:\n- Call a tool whenever the task needs one. Every call you make comes back as a tool result that you must use to continue.\n- A turn that only calls tools is not a final answer: when the task is done, reply to the user directly, without tool calls.\n- If a tool returns an error, read it, fix the arguments or choose another approach; never repeat an identical failing call.\n- Reply in the user's language.";

/// Fixed system prompt for POSIX hosts; the Environment section states the
/// shell accordingly.
pub(crate) const AGENT_SYSTEM_PROMPT_POSIX: &str = "You are Nexora, a desktop agent working on the user's machine.\n\nEnvironment:\n- execute_command runs each command through the POSIX shell (sh).\n- The file tools read_file, write_file and list_directory operate inside a dedicated agent workspace directory. Relative paths resolve against the workspace, paths outside it are rejected, and execute_command runs with the workspace as its current directory.\n\nWorkflow:\n- Call a tool whenever the task needs one. Every call you make comes back as a tool result that you must use to continue.\n- A turn that only calls tools is not a final answer: when the task is done, reply to the user directly, without tool calls.\n- If a tool returns an error, read it, fix the arguments or choose another approach; never repeat an identical failing call.\n- Reply in the user's language.";

/// The fixed agent system prompt assembled for this build target.
#[cfg(windows)]
pub(crate) const AGENT_SYSTEM_PROMPT: &str = AGENT_SYSTEM_PROMPT_WINDOWS;

/// The fixed agent system prompt assembled for this build target.
#[cfg(not(windows))]
pub(crate) const AGENT_SYSTEM_PROMPT: &str = AGENT_SYSTEM_PROMPT_POSIX;

/// Frozen model-facing structural denial (T5): document runs never expose
/// the shell, so a smuggled `execute_command` call becomes this controlled
/// observation instead of parking for approval or executing.
pub(crate) const DOCUMENT_SHELL_DENIAL: &str =
    "Error: execute_command is not available in the document preset";

/// Frozen model-facing denial observation (M1-core): provenance travels in
/// the persisted step only.
pub(crate) fn denied_tool_message(call: &crate::application::execution::ToolCall) -> AiMessage {
    AiMessage {
        role: AiRole::Tool,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: Some(crate::application::execution::AiToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: "Error: tool execution was denied by the user".to_string(),
        }),
    }
}

/// Wrap a tool observation as a native `Tool` message.
///
/// WS-C.2: the observation is untrusted tool output, so it always enters the
/// context inside the fenced [`injection`] envelope (tool label + fixed
/// vocabulary, body byte-identical). The only raw path is the flag-gated
/// [`tool_message_raw`] (the `injection` flag off reproduces the pre-2.0
/// shape); trusted fixed-vocabulary denials bypass this constructor via
/// [`trusted_denial_message`] or [`denied_tool_message`].
pub(crate) fn tool_message(
    call: &crate::application::execution::ToolCall,
    observation: &str,
) -> AiMessage {
    AiMessage {
        role: AiRole::Tool,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: Some(crate::application::execution::AiToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: injection::envelope_tool_output(&call.name, observation),
        }),
    }
}

/// Wrap a tool observation as a native `Tool` message, unenveloped.
///
/// Pre-2.0 shape: the raw observation without the untrusted fence. Used only
/// when the `injection` feature flag is off; with the flag on every tool
/// observation enters through [`tool_message`]. Trusted fixed-vocabulary
/// denials keep bypassing both constructors via [`trusted_denial_message`].
pub(crate) fn tool_message_raw(
    call: &crate::application::execution::ToolCall,
    observation: &str,
) -> AiMessage {
    AiMessage {
        role: AiRole::Tool,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: Some(crate::application::execution::AiToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: observation.to_string(),
        }),
    }
}

/// Wrap a trusted system denial as a native `Tool` message, unenveloped.
///
/// Only for fixed-vocabulary denials the host generates itself (the document
/// shell ban, the role-subset gate): unlike [`tool_message`] the text is not
/// tool output, so it must not wear the untrusted fence. Callers pass frozen
/// constants or fixed-vocabulary formats only — never tool output.
pub(crate) fn trusted_denial_message(
    call: &crate::application::execution::ToolCall,
    denial: &str,
) -> AiMessage {
    AiMessage {
        role: AiRole::Tool,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: Some(crate::application::execution::AiToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: denial.to_string(),
        }),
    }
}

/// Classify a dispatch outcome into `(observation, status)` honouring
/// cancellation.
pub(crate) fn classify_outcome(
    outcome: Result<String, crate::application::agent::tools::ToolError>,
    token: &CancellationToken,
) -> (String, &'static str) {
    match outcome {
        Ok(output) if token.is_cancelled() => (output, "cancelled"),
        Ok(output) => (output, "succeeded"),
        Err(tool_error) if token.is_cancelled() => (tool_error.to_string(), "cancelled"),
        Err(tool_error) => (tool_error.to_string(), "failed"),
    }
}

/// Window selection for [`build_initial_messages_budgeted`]: who the run is
/// for, under which model window, and at which pipeline stage.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssemblyBudget<'a> {
    /// Internal provider name (window resolution).
    pub provider: &'a str,
    /// Model identifier (window resolution + tool gating).
    pub model: &'a str,
    /// Model window override (`None` resolves via `context_limit_for`,
    /// `Some(0)` disables budgeting — the legacy byte-identical path).
    pub context_limit: Option<u64>,
    /// Lifecycle state framing the stage section (fixed vocabulary only).
    pub state: RunState,
    /// Current pipeline stage (`None` omits the stage section).
    pub stage: Option<PipelineStage>,
}

impl AssemblyBudget<'_> {
    /// Unbounded budget with no stage: reproduces the legacy assembly
    /// byte-for-byte.
    #[must_use]
    pub(crate) fn unbounded() -> AssemblyBudget<'static> {
        AssemblyBudget {
            provider: "",
            model: "",
            context_limit: Some(0),
            state: RunState::Running,
            stage: None,
        }
    }
}

/// Compose the system content for `dropped` omitted history messages plus the
/// optional action trace through the single assembly path ([`assembly`]).
///
/// With no omission, no trace, no stage, and an unbounded window this returns
/// the fixed system prompt verbatim — the legacy byte-identical shape.
fn system_content_for(
    dropped: usize,
    action_summary: Option<&ActionSummary>,
    budget: &AssemblyBudget<'_>,
) -> String {
    let mut task_role = if dropped == 0 {
        String::new()
    } else {
        history::omitted_note(dropped)
    };
    // Layer-2 action memory: the prior action trace follows the Layer-1
    // note, separated by a blank line. An empty summary appends zero
    // bytes, so runs without prior actions keep the exact prompt.
    // WS-C.2: the trace replays prior tool observations (untrusted text),
    // so the block enters inside the untrusted fence, never raw.
    if let Some(summary) = action_summary {
        if let Some(note) = action_memory::system_note(summary) {
            if !task_role.is_empty() {
                task_role.push_str("\n\n");
            }
            task_role.push_str(&injection::envelope_prior_actions(&note));
        }
    }
    let input = assembly::AssemblyInput {
        provider: budget.provider,
        model: budget.model,
        context_limit: budget.context_limit,
        state: budget.state,
        stage: budget.stage,
        system_identity: AGENT_SYSTEM_PROMPT,
        task_role: &task_role,
        tool_outputs: &[],
        history_summary: None,
    };
    let assembled = assembly::assemble(&input);
    let mut content = assembled
        .section(assembly::SectionKind::SystemIdentity)
        .unwrap_or("")
        .to_string();
    for kind in [
        assembly::SectionKind::TaskRole,
        assembly::SectionKind::StageContext,
    ] {
        if let Some(text) = assembled.section(kind) {
            if !text.is_empty() {
                content.push_str("\n\n");
                content.push_str(text);
            }
        }
    }
    content
}

/// Assemble the opening message sequence for a run: the fixed agent system
/// prompt, the retained conversation tail (agent memory slice), the prior
/// action trace and the user request.
pub(crate) fn build_initial_messages(
    prior_messages: &[AiMessage],
    action_summary: Option<&ActionSummary>,
    user_request: &str,
) -> Vec<AiMessage> {
    build_initial_messages_budgeted(
        prior_messages,
        action_summary,
        user_request,
        &AssemblyBudget::unbounded(),
    )
}

/// Assemble the opening message sequence under a model window.
///
/// The history opens with the fixed agent system prompt, the retained
/// conversation tail (agent memory slice), and the user request; after every
/// tool turn the assistant's own calls and each tool's result are appended
/// natively (see module docs). Under a bounded window the carried history
/// shrinks oldest-first (re-aligned to a user turn, with the omission note
/// recount) while the system prompt and the current request are pinned — the
/// request tail-truncates only when it alone overflows the window. With an
/// unbounded budget and no attached stage the output is byte-identical to
/// [`build_initial_messages`].
pub(crate) fn build_initial_messages_budgeted(
    prior_messages: &[AiMessage],
    action_summary: Option<&ActionSummary>,
    user_request: &str,
    budget: &AssemblyBudget<'_>,
) -> Vec<AiMessage> {
    let windowed = history::window(prior_messages, history::DEFAULT_HISTORY_WINDOW);
    let resolved =
        assembly::resolve_context_limit(budget.provider, budget.model, budget.context_limit);
    let usable = compaction::usable_context_tokens(resolved);
    let mut extra_dropped = 0_usize;
    let mut request_text = user_request.to_string();
    loop {
        // Oldest-first drops within the carried history. The start is
        // re-aligned to a user turn only while dropping (mirroring
        // `history::window`); a fitting history keeps its head verbatim —
        // in-run assistant/tool exchanges open mid-turn by construction.
        let mut start = extra_dropped.min(windowed.messages.len());
        if start > 0 {
            while start < windowed.messages.len() && windowed.messages[start].role != AiRole::User {
                start += 1;
            }
        }
        let dropped_total = windowed.dropped.saturating_add(start);
        let system_content = system_content_for(dropped_total, action_summary, budget);
        let retained = &windowed.messages[start..];
        let total = compaction::estimate_tokens(&system_content)
            .saturating_add(assembly::messages_size_tokens(retained))
            .saturating_add(compaction::estimate_tokens(&request_text));
        if resolved == 0 || usable == 0 || total <= usable {
            return render_opening(system_content, retained, &request_text);
        }
        if start < windowed.messages.len() {
            extra_dropped = start.saturating_add(1);
            continue;
        }
        // The carried history is empty and the pinned pair still overflows:
        // truncate the current request tail (never removed) until it fits.
        let system_tokens = compaction::estimate_tokens(&system_content);
        request_text =
            assembly::truncate_tail_to_budget(&request_text, usable.saturating_sub(system_tokens));
        return render_opening(system_content, retained, &request_text);
    }
}

/// Render the opening `[System, ..history.., User]` sequence.
fn render_opening(
    system_content: String,
    retained: &[AiMessage],
    request_text: &str,
) -> Vec<AiMessage> {
    let mut messages = Vec::with_capacity(retained.len() + 2);
    messages.push(AiMessage {
        role: AiRole::System,
        content: system_content,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: None,
    });
    messages.extend(retained.iter().cloned());
    messages.push(AiMessage {
        role: AiRole::User,
        content: request_text.to_string(),
        attachments: Vec::new(),
        tool_calls: Vec::new(),
        tool_result: None,
    });
    messages
}

/// Format a secret-free trace line for a thought signature.
///
/// Reports only presence (`present=true/false`) and byte length (`len=N`)
/// per tool call so signature flow leaves a trace in logs; the opaque value
/// itself is never formatted, logged, or returned.
pub(crate) fn thought_signature_trace(call_id: &str, signature: Option<&String>) -> String {
    let (present, len) = match signature {
        Some(value) if !value.is_empty() => (true, value.len()),
        _ => (false, 0),
    };
    format!("agent thought_signature call_id={call_id} present={present} len={len}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::action_memory::{self, ActionSummary};
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use crate::application::execution::{AiResponse, ToolCall};
    use std::fs;

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

    /// An empty action summary appends zero bytes: the system prompt stays
    /// byte-for-byte identical to the Layer-1 output.
    #[test]
    fn with_empty_action_summary_leaves_system_prompt_byte_identical() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("done"))]);
        let runner = AgentRunner::new(&fake, &ws).with_action_summary(ActionSummary {
            lines: Vec::new(),
            omitted_runs: 0,
            omitted_steps: 0,
        });

        runner.run("openai", "m", "cred", "hi").expect("finish");

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 1);
        let messages = &requests[0].messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, AiRole::System);
        assert_eq!(messages[0].content, AGENT_SYSTEM_PROMPT);
        let _ = fs::remove_dir_all(&ws);
    }

    /// A non-empty action summary is appended after the system prompt,
    /// separated by a blank line, with the current turn last.
    #[test]
    fn with_action_summary_appends_trace_after_system_prompt() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("done"))]);
        let runner =
            AgentRunner::new(&fake, &ws).with_action_summary(action_memory::summarize(&[(
                12,
                vec![action_memory::AgentStepView {
                    tool_name: "read_file".to_string(),
                    arguments: r#"{"path": "a.txt"}"#.to_string(),
                    observation: "content".to_string(),
                    status: "succeeded".to_string(),
                }],
            )]));

        runner.run("openai", "m", "cred", "hi").expect("finish");

        let requests = fake.requests.borrow();
        let system = &requests[0].messages[0].content;
        assert!(
            system.starts_with(AGENT_SYSTEM_PROMPT),
            "trace follows the system prompt"
        );
        assert!(system.contains("Prior action trace"), "{system}");
        assert!(system.contains("run 12: read_file(a.txt)"), "{system}");
        assert_eq!(
            requests[0].messages.last().expect("current turn").content,
            "hi"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    /// `with_history` carries prior turns into the first request as
    /// [System, ..history.., User(current)]; with no truncation the system
    /// prompt stays byte-for-byte exact.
    #[test]
    fn with_history_prepends_prior_turns_before_current_request() {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("done"))]);
        let history = vec![
            user_message("earlier question"),
            assistant_message("earlier answer"),
        ];
        let runner = AgentRunner::new(&fake, &ws).with_history(history);

        runner
            .run("openai", "m", "cred", "current question")
            .expect("finish");

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 1);
        let messages = &requests[0].messages;
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, AiRole::System);
        assert_eq!(messages[0].content, AGENT_SYSTEM_PROMPT);
        assert_eq!(messages[1].role, AiRole::User);
        assert_eq!(messages[1].content, "earlier question");
        assert_eq!(messages[2].role, AiRole::Assistant);
        assert_eq!(messages[2].content, "earlier answer");
        assert_eq!(messages[3].role, AiRole::User);
        assert_eq!(messages[3].content, "current question");
        let _ = fs::remove_dir_all(&ws);
    }

    /// A history longer than the window is truncated: the system prompt
    /// carries the omission note and the dropped messages never reach the
    /// request.
    #[test]
    fn with_history_beyond_window_truncates_with_omission_note() {
        use crate::application::agent::history as agent_history;

        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![Ok(text_response("done"))]);
        let mut history = Vec::new();
        for i in 0..agent_history::DEFAULT_HISTORY_WINDOW + 5 {
            history.push(user_message(&format!("question {i}")));
            history.push(assistant_message(&format!("answer {i}")));
        }
        let dropped_question = history[0].content.clone();
        let runner = AgentRunner::new(&fake, &ws).with_history(history);

        runner
            .run("openai", "m", "cred", "new question")
            .expect("finish");

        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 1);
        let messages = &requests[0].messages;
        assert_eq!(messages[0].role, AiRole::System);
        assert!(
            messages[0].content.starts_with(AGENT_SYSTEM_PROMPT),
            "truncated runs keep the agent system prompt first"
        );
        assert!(
            messages[0].content.len() > AGENT_SYSTEM_PROMPT.len(),
            "truncated runs append the omission note"
        );
        assert!(
            !messages.iter().any(|m| m.content == dropped_question),
            "dropped history must be absent from the request"
        );
        assert_eq!(
            messages.last().expect("current turn").content,
            "new question"
        );
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

    /// The trace helper reports presence + length per call and never the
    /// opaque value itself (secret hygiene).
    #[test]
    fn thought_signature_trace_reports_presence_never_value() {
        let secret = "sig-runner-secret".to_string();
        let secret_len = secret.len();
        let present = thought_signature_trace("c9", Some(&secret));
        assert!(
            present.contains("present=true"),
            "trace line reports presence: {present}"
        );
        assert!(
            present.contains(&format!("len={secret_len}")),
            "trace line reports length: {present}"
        );
        assert!(
            !present.contains(&secret),
            "trace line must never carry the value"
        );
        let absent = thought_signature_trace("c9", None);
        assert!(
            absent.contains("present=false"),
            "trace line reports absence: {absent}"
        );
        assert!(
            absent.contains("len=0"),
            "absent signature has zero length: {absent}"
        );
    }

    /// The unbounded budgeted build reproduces the legacy assembly
    /// byte-for-byte, including window truncation and the action trace.
    #[test]
    fn budgeted_unbounded_matches_legacy_byte_for_byte() {
        use crate::application::agent::history as agent_history;

        let mut history = Vec::new();
        for i in 0..agent_history::DEFAULT_HISTORY_WINDOW + 5 {
            history.push(user_message(&format!("question {i}")));
            history.push(assistant_message(&format!("answer {i}")));
        }
        let summary = action_memory::summarize(&[(
            12,
            vec![action_memory::AgentStepView {
                tool_name: "read_file".to_string(),
                arguments: r#"{"path": "a.txt"}"#.to_string(),
                observation: "content".to_string(),
                status: "succeeded".to_string(),
            }],
        )]);

        let legacy = build_initial_messages(&history, Some(&summary), "new question");
        let budgeted = build_initial_messages_budgeted(
            &history,
            Some(&summary),
            "new question",
            &AssemblyBudget::unbounded(),
        );
        assert_eq!(
            budgeted, legacy,
            "unbounded budgeted build stays legacy-identical"
        );

        // Empty-input shape matches too.
        assert_eq!(
            build_initial_messages_budgeted(&[], None, "hi", &AssemblyBudget::unbounded()),
            build_initial_messages(&[], None, "hi")
        );
    }

    /// A tiny explicit window drops the oldest carried history first while
    /// the system prompt and the current request stay pinned — and the whole
    /// opening fits the window.
    #[test]
    fn budgeted_tiny_limit_drops_oldest_history_first_and_fits() {
        let mut history = Vec::new();
        for i in 0..6 {
            history.push(user_message(&format!("OLD-{i}-{}", "q".repeat(4_000))));
            history.push(assistant_message(&format!("A-{i}-{}", "a".repeat(4_000))));
        }
        // 28_000 tokens of window, 20_000 reserved: 8_000 usable. Twelve
        // ~1.2k-token turns overflow it, so the oldest pair must drop while
        // the newest survives.
        let budget = AssemblyBudget {
            provider: "openai",
            model: "gpt-5.6-terra",
            context_limit: Some(28_000),
            state: RunState::Running,
            stage: None,
        };
        let messages = build_initial_messages_budgeted(&history, None, "current question", &budget);

        assert_eq!(messages[0].role, AiRole::System);
        assert!(
            messages[0].content.starts_with(AGENT_SYSTEM_PROMPT),
            "system identity stays pinned first"
        );
        assert_eq!(
            messages.last().expect("current turn").content,
            "current question",
            "the current request is never dropped"
        );
        assert!(
            !messages.iter().any(|m| m.content.contains("OLD-0")),
            "the oldest history drops first"
        );
        assert!(
            messages.iter().any(|m| m.content.contains("OLD-5")),
            "the newest history survives"
        );
        assert!(
            messages
                .iter()
                .position(|m| m.content.contains("OLD-5"))
                .expect("newest present")
                > 1,
            "the retained window stays user-aligned behind the system prompt"
        );
        let total = crate::application::agent::compaction::estimate_tokens(&messages[0].content)
            .saturating_add(assembly::messages_size_tokens(&messages[1..]));
        assert!(
            total <= 8_000,
            "the budgeted opening fits the usable window, got {total}"
        );
    }

    /// An attached stage appends fixed-vocabulary stage context after the
    /// legacy system content — and nothing else changes.
    #[test]
    fn budgeted_stage_appends_fixed_vocabulary_after_legacy_content() {
        let history = vec![user_message("earlier question")];
        let legacy = build_initial_messages(&history, None, "current question");
        let budget = AssemblyBudget {
            provider: "openai",
            model: "gpt-5.6-terra",
            context_limit: Some(0),
            state: RunState::Running,
            stage: Some(crate::application::agent::pipeline::PipelineStage::Act),
        };
        let messages = build_initial_messages_budgeted(&history, None, "current question", &budget);

        assert_eq!(messages.len(), legacy.len());
        assert_eq!(
            &messages[1..],
            &legacy[1..],
            "history and request untouched"
        );
        let expected = format!(
            "{}\n\n{}",
            legacy[0].content,
            crate::application::agent::assembly::stage_context_text(
                crate::application::agent::pipeline::PipelineStage::Act,
                RunState::Running,
            )
        );
        assert_eq!(messages[0].content, expected);
        assert!(messages[0].content.contains("act"));
        assert!(messages[0].content.contains("executor"));
        assert!(messages[0].content.contains("running"));
    }
}
