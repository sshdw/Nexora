//! Per-tool-call dispatch pipeline (split from `runner`).

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::permissions::{self, PermissionOutcome, PermissionStore};
use crate::application::agent::persistence::{ActiveRunRecord, StepProvenance};
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{AiMessage, AiRole, ToolCall};

use super::errors::AgentError;
use super::prompts::{
    classify_outcome, denied_tool_message, thought_signature_trace, tool_message,
};

/// Read-only view of runner state for one dispatch batch.
///
/// Assembled by the run loop; the pipeline never mutates runner state
/// through it (message and step sinks travel as explicit `&mut` params).
pub(crate) struct DispatchCtx<'a> {
    pub workspace_root: &'a Path,
    pub token: &'a CancellationToken,
    pub control: Option<&'a RunControl>,
    pub approval_gate: Option<&'a ApprovalGate>,
    pub permission_store: Option<&'a PermissionStore>,
    pub sender: Option<&'a Sender<AgentRunEvent>>,
}

/// Emit a governance event on the optional channel, best-effort.
pub(crate) fn emit(sender: Option<&Sender<AgentRunEvent>>, event: AgentRunEvent) {
    if let Some(tx) = sender {
        let _ = tx.send(event);
    }
}

/// Return `Err(AgentError::Cancelled)` when cancellation was observed.
pub(crate) fn check_cancellation(
    control: Option<&RunControl>,
    sender: Option<&Sender<AgentRunEvent>>,
) -> Result<(), AgentError> {
    if matches!(control, Some(c) if c.is_cancelled()) {
        emit(sender, AgentRunEvent::Cancelled);
        return Err(AgentError::Cancelled);
    }
    Ok(())
}

/// Honour a pending user pause at this step boundary. Emits `Paused`,
/// blocks until `resume` (emitting `Resumed`) or `cancel` (aborting);
/// cancelling while paused wakes the loop (no deadlock).
pub(crate) fn honor_pause(
    control: Option<&RunControl>,
    sender: Option<&Sender<AgentRunEvent>>,
) -> Result<(), AgentError> {
    let Some(c) = control else {
        return Ok(());
    };
    if !c.pause_pending() {
        return Ok(());
    }
    emit(sender, AgentRunEvent::Paused);
    if c.wait_while_paused() {
        emit(sender, AgentRunEvent::Resumed);
        Ok(())
    } else {
        emit(sender, AgentRunEvent::Cancelled);
        Err(AgentError::Cancelled)
    }
}

/// Dispatch every returned tool call through the per-tool-call pipeline:
/// session-sticky group auto-resolve, persistent permission rules,
/// approval-gate parking, execution and observation recording.
pub(crate) fn dispatch_tool_calls(
    ctx: &DispatchCtx<'_>,
    calls: &[ToolCall],
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
) -> Result<(), AgentError> {
    // AC-6: never drop a call — every returned call is dispatched and
    // observed. Failures are rendered through `ToolError`'s Display
    // (`Error: ...`) so the model can recover on the next turn.
    let batch_groups: Vec<String> = calls
        .iter()
        .map(|call| {
            let path = permissions::extract_path(&call.name, &call.arguments);
            permissions::group_key("coding", &call.name, path.as_deref())
        })
        .collect();
    for call in calls {
        // Trace-level flow marker: presence + length only, never value.
        log::trace!(
            "{}",
            thought_signature_trace(&call.id, call.thought_signature.as_ref())
        );
        check_cancellation(ctx.control, ctx.sender)?;
        dispatch_one(ctx, call, messages, record, &batch_groups)?;
    }
    Ok(())
}

fn dispatch_one(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    batch_groups: &[String],
) -> Result<(), AgentError> {
    let request_path = permissions::extract_path(&call.name, &call.arguments);
    let call_group_key = permissions::group_key("coding", &call.name, request_path.as_deref());
    let call_group_size = batch_groups
        .iter()
        .filter(|key| *key == &call_group_key)
        .count();
    if handle_sticky_group_decision(ctx, call, messages, record, &call_group_key) {
        return Ok(());
    }
    if handle_permission_store_decision(ctx, call, messages, record, request_path.as_deref()) {
        return Ok(());
    }
    if !park_for_approval(
        ctx,
        call,
        messages,
        record,
        &call_group_key,
        call_group_size,
    )? {
        return Ok(());
    }
    execute_tool_call(ctx, call, messages, record, &call_group_key);
    Ok(())
}

fn handle_sticky_group_decision(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
) -> bool {
    // M1-core: session-sticky group auto-resolve (run lifetime).
    if let Some(gate) = ctx.approval_gate {
        if let Some(sticky) = gate.group_decision(group_key) {
            let approved = matches!(sticky, ApprovalDecision::Approved);
            if let Some(rec) = record.as_mut() {
                if approved {
                    rec.approval_with_provenance(call, true, StepProvenance::user(Some(group_key)));
                } else {
                    rec.approval_denied_by_group(call, group_key);
                }
            }
            if !approved {
                messages.push(denied_tool_message(call));
                return true;
            }
            let dispatch_started = Instant::now();
            let outcome =
                ToolRegistry::execute_with_cancellation(call, ctx.workspace_root, ctx.token);
            let dispatch_ms =
                i64::try_from(dispatch_started.elapsed().as_millis()).unwrap_or(i64::MAX);
            let (observation, tool_status) = classify_outcome(outcome, ctx.token);
            messages.push(tool_message(call, &observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    tool_status,
                    Some(dispatch_ms),
                    StepProvenance::user(Some(group_key)),
                );
            }
            return true;
        }
    }
    false
}

fn handle_permission_store_decision(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    request_path: Option<&str>,
) -> bool {
    // M1-core: persistent rules before the gate. Unknown tools skip the store.
    if permissions::is_known_tool(&call.name) {
        if let Some(store) = ctx.permission_store {
            if let Some(outcome) = store.decide(
                "coding",
                &call.name,
                request_path,
                crate::application::agent::approval::RiskClass::classify(&call.name),
            ) {
                let mode = ctx
                    .approval_gate
                    .map_or(AutonomyMode::Supervised, ApprovalGate::mode);
                match outcome {
                    PermissionOutcome::Deny { rule_id, .. } => {
                        if let Some(rec) = record.as_mut() {
                            rec.approval_with_provenance(
                                call,
                                false,
                                StepProvenance::rule(rule_id),
                            );
                        }
                        messages.push(denied_tool_message(call));
                        return true;
                    }
                    PermissionOutcome::Allow { rule_id } => {
                        if !matches!(mode, AutonomyMode::Supervised) {
                            let dispatch_started = Instant::now();
                            let outcome = ToolRegistry::execute_with_cancellation(
                                call,
                                ctx.workspace_root,
                                ctx.token,
                            );
                            let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis())
                                .unwrap_or(i64::MAX);
                            let (observation, tool_status) = classify_outcome(outcome, ctx.token);
                            messages.push(tool_message(call, &observation));
                            if let Some(rec) = record.as_mut() {
                                rec.tool_call_with_provenance(
                                    call,
                                    &observation,
                                    tool_status,
                                    Some(dispatch_ms),
                                    StepProvenance::rule(rule_id),
                                );
                            }
                            return true;
                        }
                    }
                    PermissionOutcome::Ask { rule_id } => {
                        if matches!(mode, AutonomyMode::FullAutonomous) {
                            let dispatch_started = Instant::now();
                            let outcome = ToolRegistry::execute_with_cancellation(
                                call,
                                ctx.workspace_root,
                                ctx.token,
                            );
                            let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis())
                                .unwrap_or(i64::MAX);
                            let (observation, tool_status) = classify_outcome(outcome, ctx.token);
                            messages.push(tool_message(call, &observation));
                            if let Some(rec) = record.as_mut() {
                                rec.tool_call_with_provenance(
                                    call,
                                    &observation,
                                    tool_status,
                                    Some(dispatch_ms),
                                    StepProvenance::rule(rule_id),
                                );
                            }
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn park_for_approval(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
    group_size: usize,
) -> Result<bool, AgentError> {
    // Task 4.1: approval gate evaluated at the per-tool-call
    // boundary, before dispatch. Auto paths execute exactly as
    // before; denied calls become a controlled observation and the
    // loop continues; cancellation while parked aborts.
    if let Some(gate) = ctx.approval_gate {
        if gate.needs_approval(call) {
            // INVARIANT: once ApprovalRequested is emitted, a pending entry for that call_id exists,
            // so a concurrent resolve cannot hit NoPendingApproval вЂ” the race is closed by construction.
            gate.prepare_pending_with_group(call, Some(group_key.to_owned()));
            emit(
                ctx.sender,
                AgentRunEvent::ApprovalRequested {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    group_key: Some(group_key.to_owned()),
                    group_size,
                },
            );
            let Ok(decision) = gate.request_approval(call) else {
                // Task 4.2: cancellation ended the parked wait РІР‚вЂќ
                // record the `cancelled` approval step (D12).
                if let Some(rec) = record.as_mut() {
                    rec.approval_cancelled(call);
                }
                emit(ctx.sender, AgentRunEvent::Cancelled);
                return Err(AgentError::Cancelled);
            };
            let approved = matches!(decision, ApprovalDecision::Approved);
            // Task 4.2: record the parked approval decision (D12).
            if let Some(rec) = record.as_mut() {
                rec.approval_with_provenance(call, approved, StepProvenance::user(Some(group_key)));
            }
            emit(
                ctx.sender,
                AgentRunEvent::ApprovalResolved {
                    call_id: call.id.clone(),
                    approved,
                },
            );
            if !approved {
                messages.push(AiMessage {
                    role: AiRole::Tool,
                    content: String::new(),
                    attachments: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_result: Some(crate::application::execution::AiToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: "Error: tool execution was denied by the user".to_string(),
                    }),
                });
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn execute_tool_call(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
) {
    // Task 4.2: the dispatched call (approved or ungated) is
    // recorded with its raw arguments, observation, and outcome
    // (D12). A cancellation observed by the tool records as
    // `cancelled`; everything else is `succeeded` or `failed`.
    let dispatch_started = Instant::now();
    let outcome = ToolRegistry::execute_with_cancellation(call, ctx.workspace_root, ctx.token);
    let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (observation, tool_status) = match outcome {
        Ok(output) if ctx.token.is_cancelled() => (output, "cancelled"),
        Ok(output) => (output, "succeeded"),
        Err(tool_error) if ctx.token.is_cancelled() => (tool_error.to_string(), "cancelled"),
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
        // M1-core provenance: parked-then-approved tool calls inherit
        // `user`; ladder-auto executions are `system`.
        let provenance = if ctx
            .approval_gate
            .is_some_and(|gate| gate.needs_approval(call))
        {
            StepProvenance::user(Some(group_key))
        } else {
            StepProvenance::system()
        };
        rec.tool_call_with_provenance(
            call,
            &observation,
            tool_status,
            Some(dispatch_ms),
            provenance,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::persistence::RunRecorder;
    use crate::application::agent::prompts::AGENT_SYSTEM_PROMPT;
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use crate::application::execution::AiResponse;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::agent_runs::AgentRunRepository;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// Two calls in one response: the Assistant message carries both calls
    /// and the two Tool messages follow in the same order вЂ” the wire ordering
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
        // Tool{observation}] вЂ” the model sees its own call and the result.
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
        // [System, User, Assistant{narration + 3 calls}, Tool, Tool, Tool] вЂ”
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

    // -----------------------------------------------------------------------
    // Three-tier approval gate (Task 4.1)
    // -----------------------------------------------------------------------

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
            gate_for_driver.set_mode(AutonomyMode::FullAutonomous);
            // Ordering matters (approval.rs:139 вЂ” an already-parked request
            // is NOT auto-resolved by `set_mode`): the mode must be Full
            // BEFORE `respond` wakes the runner, so w2 necessarily sees Full
            // even if the runner executes w1 and evaluates w2 before this
            // driver thread is rescheduled. Parked w1 still requires its
            // own `respond`, so w1 semantics are unchanged.
            gate_for_driver.respond("w1", ApprovalDecision::Approved);
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
                    Ok(AgentRunEvent::ApprovalRequested { call_id, .. }) if call_id == "w2" => {
                        // Race relic on loaded CI: w2 parked before observing
                        // the mode switch. Approve it and keep draining.
                        gate_for_driver.respond("w2", ApprovalDecision::Approved);
                        seen += 1;
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
            // Immediate resolve вЂ” must succeed; the race is closed by construction
            // because prepare_pending ran before the emit.
            let resolved = gate_for_driver.respond("race-1", ApprovalDecision::Approved);
            assert!(
                resolved,
                "immediate respond must succeed вЂ” race closed by construction"
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
    // M1-core permission rules + grouping
    // -----------------------------------------------------------------------

    use crate::application::agent::permissions::{PermissionStore, RuleEffect};

    #[test]
    fn supervised_ignores_allow_rules() {
        let db = in_memory_database();
        let ws = temp_workspace();
        crate::application::agent::permissions::insert_rule(
            &db,
            "coding",
            "write_file",
            None,
            RuleEffect::Allow,
            10,
        )
        .expect("insert allow");
        let store = PermissionStore::load(&db);
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
                    serde_json::json!({"path": "supervised.txt", "content": "1"}),
                )],
                usage: None,
            }),
            Ok(text_response("ok")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_permission_store(store)
            .with_run_recorder(RunRecorder::new(&db))
            .with_event_sender(tx);
        let driver = thread::spawn(move || {
            // Allow is ignored under Supervised: the call must still park.
            let ev = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("ApprovalRequested");
            assert!(
                matches!(ev, AgentRunEvent::ApprovalRequested { .. }),
                "Allow + Supervised still parks, got {ev:?}"
            );
            assert!(gate_for_driver.respond("w1", ApprovalDecision::Approved));
            rx.recv_timeout(Duration::from_secs(5))
                .expect("ApprovalResolved");
            rx.recv_timeout(Duration::from_secs(5)).expect("Completed")
        });
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "ok");
        driver.join().expect("driver joins");
        assert_eq!(fs::read_to_string(ws.join("supervised.txt")).unwrap(), "1");
        // Parked approval carries user provenance, not rule.
        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list")[0];
        let steps = runs.list_steps(run.id).expect("steps");
        let approval = steps
            .iter()
            .find(|s| s.kind == "approval")
            .expect("approval step");
        assert_eq!(approval.decided_by.as_deref(), Some("user"));
        assert_eq!(approval.rule_id, None);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn full_autonomous_deny_rule_still_denies() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let deny_id = crate::application::agent::permissions::insert_rule(
            &db,
            "coding",
            "write_file",
            None,
            RuleEffect::Deny,
            10,
        )
        .expect("insert deny");
        let store = PermissionStore::load(&db);
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "blocked.txt", "content": "x"}),
                )],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(ApprovalGate::new(AutonomyMode::FullAutonomous))
            .with_permission_store(store)
            .with_run_recorder(RunRecorder::new(&db))
            .with_event_sender(tx);
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "recovered");
        // Never dispatched, never parked (no approval-branch events; the
        // terminal Completed event still fires).
        assert!(
            fs::read_to_string(ws.join("blocked.txt")).is_err(),
            "deny must not dispatch"
        );
        let mut saw_approval_branch = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(
                ev,
                AgentRunEvent::ApprovalRequested { .. } | AgentRunEvent::ApprovalResolved { .. }
            ) {
                saw_approval_branch = true;
            }
        }
        assert!(
            !saw_approval_branch,
            "deny floor parks nothing and emits no approval event"
        );
        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list")[0];
        let steps = runs.list_steps(run.id).expect("steps");
        let approval = steps
            .iter()
            .find(|s| s.kind == "approval")
            .expect("approval step");
        assert_eq!(approval.status.as_deref(), Some("denied"));
        assert_eq!(approval.rule_id, Some(deny_id));
        assert_eq!(approval.decided_by.as_deref(), Some("rule"));
        assert!(
            !steps.iter().any(|s| s.kind == "tool_call"),
            "denied call is never dispatched"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn allow_rule_skips_park_with_rule_provenance() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let allow_id = crate::application::agent::permissions::insert_rule(
            &db,
            "coding",
            "write_file",
            None,
            RuleEffect::Allow,
            10,
        )
        .expect("insert allow");
        let store = PermissionStore::load(&db);
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "allowed.txt", "content": "ok"}),
                )],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(ApprovalGate::new(AutonomyMode::SemiAutonomous))
            .with_permission_store(store)
            .with_run_recorder(RunRecorder::new(&db))
            .with_event_sender(tx);
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "done");
        assert_eq!(fs::read_to_string(ws.join("allowed.txt")).unwrap(), "ok");
        // No park: no ApprovalRequested event.
        let mut saw_approval = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AgentRunEvent::ApprovalRequested { .. }) {
                saw_approval = true;
            }
        }
        assert!(!saw_approval, "Allow rule must skip the park");
        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list")[0];
        let steps = runs.list_steps(run.id).expect("steps");
        let tool = steps
            .iter()
            .find(|s| s.kind == "tool_call")
            .expect("tool step");
        assert_eq!(tool.rule_id, Some(allow_id));
        assert_eq!(tool.decided_by.as_deref(), Some("rule"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn denied_by_rule_never_dispatches_and_keeps_verbatim_observation() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let deny_id = crate::application::agent::permissions::insert_rule(
            &db,
            "coding",
            "write_file",
            None,
            RuleEffect::Deny,
            10,
        )
        .expect("insert deny");
        let store = PermissionStore::load(&db);
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![approval_call(
                    "w1",
                    "write_file",
                    serde_json::json!({"path": "nope.txt", "content": "x"}),
                )],
                usage: None,
            }),
            Ok(text_response("recovered")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(ApprovalGate::new(AutonomyMode::SemiAutonomous))
            .with_permission_store(store)
            .with_run_recorder(RunRecorder::new(&db));
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "recovered");
        assert!(fs::read_to_string(ws.join("nope.txt")).is_err());
        // Model-facing denial string is byte-exact.
        let requests = fake.requests.borrow();
        assert_eq!(requests.len(), 2);
        let history = &requests[1].messages;
        let result = history[3].tool_result.as_ref().expect("tool result");
        assert_eq!(
            result.content,
            "Error: tool execution was denied by the user"
        );
        // Ledger keeps rule provenance.
        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list")[0];
        let steps = runs.list_steps(run.id).expect("steps");
        let approval = steps
            .iter()
            .find(|s| s.kind == "approval")
            .expect("approval");
        assert_eq!(approval.status.as_deref(), Some("denied"));
        let observation = approval.observation.as_deref().unwrap_or("");
        assert!(
            observation.starts_with("denied by rule:"),
            "observation {observation:?} must start with denied by rule:"
        );
        assert_eq!(approval.rule_id, Some(deny_id));
        assert_eq!(approval.decided_by.as_deref(), Some("rule"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn group_scope_second_call_auto_resolves_with_shared_group_key() {
        let db = in_memory_database();
        let ws = temp_workspace();
        let gate = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        let gate_for_driver = gate.clone();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "m".to_string(),
                tool_calls: vec![
                    approval_call(
                        "w1",
                        "write_file",
                        serde_json::json!({"path": "g/a.txt", "content": "1"}),
                    ),
                    approval_call(
                        "w2",
                        "write_file",
                        serde_json::json!({"path": "g/b.txt", "content": "2"}),
                    ),
                ],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_run_recorder(RunRecorder::new(&db))
            .with_event_sender(tx);
        let driver = thread::spawn(move || {
            // First park resolves with group scope; the second same-group call
            // must auto-resolve without a second park.
            let mut requested = 0;
            loop {
                match rx.recv_timeout(Duration::from_secs(5)).expect("event") {
                    AgentRunEvent::ApprovalRequested { call_id, .. } => {
                        requested += 1;
                        assert_eq!(call_id, "w1", "only the first call may park");
                        assert!(gate_for_driver.respond_with_scope(
                            "w1",
                            ApprovalDecision::Approved,
                            Some("group")
                        ));
                    }
                    AgentRunEvent::Completed { .. } => break,
                    _ => {}
                }
                assert!(requested <= 1, "second call must not park");
            }
            assert_eq!(requested, 1, "exactly one park for the group");
        });
        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "done");
        driver.join().expect("driver joins");
        assert_eq!(fs::read_to_string(ws.join("g/a.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(ws.join("g/b.txt")).unwrap(), "2");
        let runs = AgentRunRepository::new(&db);
        let run = &runs.list_runs_by_started_at_desc().expect("list")[0];
        let steps = runs.list_steps(run.id).expect("steps");
        let approvals: Vec<_> = steps.iter().filter(|s| s.kind == "approval").collect();
        assert_eq!(
            approvals.len(),
            2,
            "both group decisions are ledger artifacts, got {approvals:?}"
        );
        assert_eq!(
            approvals[0].group_key, approvals[1].group_key,
            "group steps share one group_key"
        );
        assert!(approvals[0].group_key.is_some(), "group_key must be set");
        let _ = fs::remove_dir_all(&ws);
    }
}
