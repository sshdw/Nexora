//! Safe continuation context builder for interrupted agent runs (`M11` / `P06`).
//!
//! Converts persisted [`AgentStep`] and [`RunQuestion`] records from a prior
//! `interrupted` run into provider-independent [`AiMessage`] history without
//! re-running completed tool calls, while treating any incomplete/in-flight tool
//! calls, unmatched approvals, or cancelled questions as explicitly interrupted /
//! cancelled so they are never automatically re-dispatched.

use crate::application::agent::prompts::{denied_tool_message, tool_message, tool_message_raw};
use crate::application::execution::{AiMessage, AiRole, ToolCall};
use crate::infrastructure::repository::agent_runs::AgentStep;
use crate::infrastructure::repository::run_questions::RunQuestion;

const INTERRUPTED_TOOL_OBSERVATION: &str =
    "[Interrupted] Tool execution did not complete before the prior run was interrupted; \
     it was NOT completed and has NOT been automatically re-dispatched. Re-evaluate the \
     current workspace state and invoke a tool only if still required.";

const INTERRUPTED_APPROVAL_OBSERVATION: &str =
    "[Interrupted] Tool approval or execution was interrupted or cancelled before completion; \
     the tool did NOT run and was NOT automatically re-dispatched.";

const INTERRUPTED_QUESTION_OBSERVATION: &str =
    "[Interrupted] Clarifying question was interrupted or cancelled before receiving an answer.";

/// Reconstructed continuation history and prompt for resuming an interrupted run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinuationPlan {
    /// Prior assistant tool-call batches and observations reconstructed from persisted steps.
    pub extra_messages: Vec<AiMessage>,
    /// Synthetic in-memory user prompt guiding continuation (never persisted as a user chat turn).
    pub continuation_prompt: String,
    /// Number of completed `tool_call` steps preserved as already executed.
    pub completed_tool_calls: usize,
    /// Number of incomplete/cancelled tool calls, unmatched approvals, or cancelled questions.
    pub interrupted_steps: usize,
}

/// Build the continuation context from an interrupted run's persisted steps and questions.
#[derive(Default)]
struct ResumeCounters {
    completed_tool_calls: usize,
    interrupted_steps: usize,
    recorded_question_calls: usize,
}

#[must_use]
pub(crate) fn build_continuation_messages(
    prior_steps: &[AgentStep],
    prior_questions: &[RunQuestion],
    injection_enabled: bool,
    is_plan: bool,
) -> ContinuationPlan {
    let batches = group_steps_by_turn(prior_steps);
    let mut extra_messages = Vec::new();
    let mut counters = ResumeCounters::default();

    for (narration, steps) in batches {
        append_turn_messages(
            narration,
            &steps,
            injection_enabled,
            &mut counters,
            &mut extra_messages,
        );
    }

    append_unrecorded_questions(
        prior_questions,
        injection_enabled,
        &mut counters,
        &mut extra_messages,
    );

    let continuation_prompt = if is_plan {
        "Continue this interrupted read-only Plan run from the preserved steps above. \
         Do NOT re-run already completed read tool calls unless needed to inspect updated state. \
         Any incomplete/cancelled tool call or question above was NOT executed. \
         Finish producing the final implementation plan."
            .to_string()
    } else {
        "Continue this interrupted run from the preserved steps above. \
         Completed tool calls listed above have already executed and must NOT be re-run needlessly. \
         Any incomplete/cancelled tool calls, pending approvals, or pending questions did NOT complete \
         and were NOT automatically re-dispatched."
            .to_string()
    };

    ContinuationPlan {
        extra_messages,
        continuation_prompt,
        completed_tool_calls: counters.completed_tool_calls,
        interrupted_steps: counters.interrupted_steps,
    }
}

fn group_steps_by_turn<'a>(
    prior_steps: &'a [AgentStep],
) -> Vec<(Option<&'a str>, Vec<&'a AgentStep>)> {
    let mut batches: Vec<(Option<&'a str>, Vec<&'a AgentStep>)> = Vec::new();
    for step in prior_steps {
        if step.kind == "model_turn" {
            let narration = step
                .observation
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            batches.push((narration, Vec::new()));
        } else if let Some((_, steps)) = batches.last_mut() {
            steps.push(step);
        } else {
            batches.push((None, vec![step]));
        }
    }
    batches
}

fn build_step_tool_call(step: &AgentStep) -> ToolCall {
    let tool_name = step
        .tool_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown_tool");
    ToolCall {
        id: format!("resume-step-{}", step.seq),
        name: tool_name.to_string(),
        arguments: step.arguments.clone().unwrap_or_else(|| "{}".to_string()),
        thought_signature: None,
    }
}

fn append_turn_messages(
    narration: Option<&str>,
    steps: &[&AgentStep],
    injection_enabled: bool,
    counters: &mut ResumeCounters,
    extra_messages: &mut Vec<AiMessage>,
) {
    let mut assistant_calls: Vec<ToolCall> = Vec::new();
    let mut tool_results: Vec<AiMessage> = Vec::new();

    for (idx, step) in steps.iter().enumerate() {
        match step.kind.as_str() {
            "tool_call" => {
                let call = build_step_tool_call(step);
                if call.name == "ask_user_question" {
                    counters.recorded_question_calls += 1;
                }
                let msg = match step.status.as_deref() {
                    Some("succeeded" | "failed") => {
                        counters.completed_tool_calls += 1;
                        let obs = step.observation.as_deref().unwrap_or("");
                        wrap_observation(injection_enabled, &call, obs)
                    }
                    Some("denied") => {
                        counters.completed_tool_calls += 1;
                        let obs = step
                            .observation
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Error: tool execution was denied.");
                        wrap_observation(injection_enabled, &call, obs)
                    }
                    _ => {
                        // `None`, `"cancelled"`, or any non-terminal state stays interrupted/cancelled.
                        counters.interrupted_steps += 1;
                        wrap_observation(injection_enabled, &call, INTERRUPTED_TOOL_OBSERVATION)
                    }
                };
                assistant_calls.push(call);
                tool_results.push(msg);
            }
            "approval" => {
                if approval_has_matching_tool_call(steps, idx, step) {
                    continue;
                }
                let call = build_step_tool_call(step);
                if step.status.as_deref() == Some("denied") {
                    counters.completed_tool_calls += 1;
                    assistant_calls.push(call.clone());
                    tool_results.push(denied_tool_message(&call));
                } else {
                    // Unmatched `succeeded` (crashed mid-tool before step persisted),
                    // `cancelled`, or `None`: must never auto-dispatch or carry approval forward.
                    counters.interrupted_steps += 1;
                    assistant_calls.push(call.clone());
                    tool_results.push(wrap_observation(
                        injection_enabled,
                        &call,
                        INTERRUPTED_APPROVAL_OBSERVATION,
                    ));
                }
            }
            _ => {}
        }
    }

    if !assistant_calls.is_empty() {
        extra_messages.push(AiMessage {
            role: AiRole::Assistant,
            content: narration.unwrap_or("").to_string(),
            attachments: Vec::new(),
            tool_calls: assistant_calls,
            tool_result: None,
        });
        extra_messages.extend(tool_results);
    } else if let Some(text) = narration {
        extra_messages.push(AiMessage {
            role: AiRole::Assistant,
            content: text.to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        });
    }
}

fn append_unrecorded_questions(
    prior_questions: &[RunQuestion],
    injection_enabled: bool,
    counters: &mut ResumeCounters,
    extra_messages: &mut Vec<AiMessage>,
) {
    // Account for `run_questions` rows that never produced a completed `ask_user_question` tool step
    // (for example, runs interrupted while parked waiting for user input).
    let unrecorded_questions: Vec<&RunQuestion> = prior_questions
        .iter()
        .skip(counters.recorded_question_calls)
        .collect();
    if unrecorded_questions.is_empty() {
        return;
    }

    let mut q_calls = Vec::with_capacity(unrecorded_questions.len());
    let mut q_results = Vec::with_capacity(unrecorded_questions.len());
    for q in unrecorded_questions {
        let args = serde_json::json!({ "question": q.question }).to_string();
        let call = ToolCall {
            id: format!("resume-question-{}", q.id),
            name: "ask_user_question".to_string(),
            arguments: args,
            thought_signature: None,
        };
        let msg = if q.status == "answered" {
            counters.completed_tool_calls += 1;
            let answer = format_answered_question(q);
            wrap_observation(injection_enabled, &call, &answer)
        } else {
            counters.interrupted_steps += 1;
            wrap_observation(injection_enabled, &call, INTERRUPTED_QUESTION_OBSERVATION)
        };
        q_calls.push(call);
        q_results.push(msg);
    }
    extra_messages.push(AiMessage {
        role: AiRole::Assistant,
        content: String::new(),
        attachments: Vec::new(),
        tool_calls: q_calls,
        tool_result: None,
    });
    extra_messages.extend(q_results);
}

fn approval_has_matching_tool_call(
    steps: &[&AgentStep],
    approval_idx: usize,
    approval: &AgentStep,
) -> bool {
    steps.get(approval_idx + 1).is_some_and(|next| {
        next.kind == "tool_call"
            && next.tool_name == approval.tool_name
            && next.arguments == approval.arguments
    })
}

fn wrap_observation(injection_enabled: bool, call: &ToolCall, observation: &str) -> AiMessage {
    if injection_enabled {
        tool_message(call, observation)
    } else {
        tool_message_raw(call, observation)
    }
}

fn format_answered_question(question: &RunQuestion) -> String {
    let mut parts = Vec::new();
    if !question.selected_options.is_empty() {
        parts.push(format!(
            "selected: {}",
            question.selected_options.join(", ")
        ));
    }
    if let Some(text) = question
        .free_text
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        parts.push(format!("reply: {text}"));
    }
    if parts.is_empty() {
        "User acknowledged the question.".to_string()
    } else {
        format!("User answered: {}", parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(
        seq: i64,
        kind: &str,
        tool_name: Option<&str>,
        arguments: Option<&str>,
        observation: Option<&str>,
        status: Option<&str>,
    ) -> AgentStep {
        AgentStep {
            id: seq,
            run_id: 42,
            seq,
            kind: kind.to_string(),
            tool_name: tool_name.map(str::to_string),
            arguments: arguments.map(str::to_string),
            observation: observation.map(str::to_string),
            status: status.map(str::to_string),
            started_at: 100 + seq,
            duration_ms: Some(12),
            rule_id: None,
            group_key: None,
            decided_by: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn completed_tools_are_preserved_and_incomplete_tools_marked_interrupted() {
        let steps = vec![
            step(1, "model_turn", None, None, Some("Inspecting files"), None),
            step(
                2,
                "tool_call",
                Some("read_file"),
                Some(r#"{"path":"README.md"}"#),
                Some("hello readme"),
                Some("succeeded"),
            ),
            step(
                3,
                "approval",
                Some("write_file"),
                Some(r#"{"path":"out.txt","content":"hi"}"#),
                Some("approved"),
                Some("succeeded"),
            ),
            // Crash occurred after approval succeeded, before write_file tool_call step ran:
            step(4, "model_turn", None, None, Some("Running shell"), None),
            step(
                5,
                "tool_call",
                Some("execute_command"),
                Some(r#"{"command":"cargo test"}"#),
                None,
                Some("cancelled"),
            ),
        ];

        let plan = build_continuation_messages(&steps, &[], false, false);
        assert_eq!(plan.completed_tool_calls, 1);
        assert_eq!(plan.interrupted_steps, 2);
        // Turn 1: Assistant([read_file, write_file]) + Tool(read_file) + Tool(write_file interrupted)
        // Turn 2: Assistant([execute_command]) + Tool(execute_command interrupted)
        assert_eq!(plan.extra_messages.len(), 5);
        assert_eq!(plan.extra_messages[0].role, AiRole::Assistant);
        assert_eq!(plan.extra_messages[0].tool_calls.len(), 2);
        assert_eq!(plan.extra_messages[1].role, AiRole::Tool);
        assert_eq!(
            plan.extra_messages[1]
                .tool_result
                .as_ref()
                .expect("result")
                .content,
            "hello readme"
        );
        assert!(plan.extra_messages[2]
            .tool_result
            .as_ref()
            .expect("result")
            .content
            .contains("[Interrupted]"));
        assert!(plan.extra_messages[4]
            .tool_result
            .as_ref()
            .expect("result")
            .content
            .contains("[Interrupted]"));
    }

    #[test]
    fn matched_approval_before_completed_tool_call_does_not_duplicate_step() {
        let steps = vec![
            step(1, "model_turn", None, None, Some("Writing file"), None),
            step(
                2,
                "approval",
                Some("write_file"),
                Some(r#"{"path":"a.txt","content":"x"}"#),
                Some("approved"),
                Some("succeeded"),
            ),
            step(
                3,
                "tool_call",
                Some("write_file"),
                Some(r#"{"path":"a.txt","content":"x"}"#),
                Some("wrote 1 byte"),
                Some("succeeded"),
            ),
        ];

        let plan = build_continuation_messages(&steps, &[], true, false);
        assert_eq!(plan.completed_tool_calls, 1);
        assert_eq!(plan.interrupted_steps, 0);
        assert_eq!(plan.extra_messages.len(), 2);
        assert_eq!(plan.extra_messages[0].tool_calls.len(), 1);
        assert!(plan.extra_messages[1]
            .tool_result
            .as_ref()
            .expect("result")
            .content
            .contains("wrote 1 byte"));
    }

    #[test]
    fn cancelled_run_questions_emit_interrupted_observation() {
        let q = RunQuestion {
            id: 7,
            run_id: 42,
            conversation_id: 9,
            call_id: "call_q1".to_string(),
            question: "Which env?".to_string(),
            options: vec!["dev".to_string(), "prod".to_string()],
            multi_select: false,
            allow_free_text: true,
            status: "cancelled".to_string(),
            selected_options: Vec::new(),
            free_text: None,
            created_at: 100,
            answered_at: None,
        };

        let plan = build_continuation_messages(&[], &[q], false, true);
        assert_eq!(plan.completed_tool_calls, 0);
        assert_eq!(plan.interrupted_steps, 1);
        assert_eq!(plan.extra_messages.len(), 2);
        assert_eq!(
            plan.extra_messages[0].tool_calls[0].name,
            "ask_user_question"
        );
        assert!(plan.extra_messages[1]
            .tool_result
            .as_ref()
            .expect("result")
            .content
            .contains("[Interrupted]"));
        assert!(plan.continuation_prompt.contains("Plan run"));
    }
}
