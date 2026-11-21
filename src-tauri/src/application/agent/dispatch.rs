//! Per-tool-call dispatch pipeline (split from `runner`).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, ParkEnd, RiskClass};
use crate::application::agent::checkpoints::{CheckpointHook, CheckpointRunContext};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::governance::{audit, AuditEvent, AuditLog, GateOutcome};
use crate::application::agent::injection;
use crate::application::agent::permissions::{self, PermissionOutcome, PermissionStore, RunPreset};
use crate::application::agent::persistence::{step_preview_opt, ActiveRunRecord, StepProvenance};
use crate::application::agent::question::{AskQuestionArgs, QuestionRunContext, QuestionWaitEnd};
use crate::application::agent::roles::TaskCapability;
use crate::application::agent::subagents::{SubagentManager, MAX_SUBAGENT_DEPTH};
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{AiMessage, AiRole, ToolCall};

use super::errors::AgentError;
use super::lifecycle::{observe_transition, RunState};
use super::prompts::{
    classify_outcome, denied_tool_message, thought_signature_trace, tool_message, tool_message_raw,
    trusted_denial_message, DOCUMENT_SHELL_DENIAL,
};

/// Wrap one tool observation for context: the WS-C.2 envelope when the
/// `injection` flag is on (and unconditionally for external MCP tools), or the
/// raw pre-2.0 shape when it is off for native tools.
fn observation_message(ctx: &DispatchCtx<'_>, call: &ToolCall, observation: &str) -> AiMessage {
    if ctx.injection_enabled || permissions::is_mcp_tool(&call.name) {
        tool_message(call, observation)
    } else {
        tool_message_raw(call, observation)
    }
}

fn is_dispatchable_tool(ctx: &DispatchCtx<'_>, name: &str) -> bool {
    permissions::is_known_tool(name)
        || permissions::is_subagent_tool(name)
        || (permissions::is_mcp_tool(name) && ctx.mcp_runtime.is_some_and(|rt| rt.has_tool(name)))
}

/// Read-only view of runner state for one dispatch batch.
///
/// Assembled by the run loop; the pipeline never mutates runner state
/// through it (message and step sinks travel as explicit `&mut` params).
#[derive(Clone, Copy)]
pub(crate) struct DispatchCtx<'a> {
    pub workspace_root: &'a Path,
    pub token: &'a CancellationToken,
    pub control: Option<&'a RunControl>,
    pub approval_gate: Option<&'a ApprovalGate>,
    pub permission_store: Option<&'a PermissionStore>,
    /// Run preset (T5): the schema filter and the structural shell ban both
    /// key off this. `Coding` preserves the exact pre-T5 pipeline.
    pub preset: RunPreset,
    pub sender: Option<&'a Sender<AgentRunEvent>>,
    /// Optional run-scoped audit trail (WS-B.2): every approval park records
    /// its gate decision here, secret-free. `None` keeps the exact pre-B.2
    /// pipeline.
    pub audit: Option<&'a AuditLog>,
    /// Optional task-scoped capability gate (`P34`): when `Some`, a known tool
    /// outside the capability's subset becomes a controlled denial observation
    /// before the store, the ladder, and execution.
    pub capability: Option<TaskCapability>,
    /// The `injection` feature flag (WS-C.2 gate): enveloped observations
    /// when `true` (current behavior); raw observations when `false`
    /// (pre-2.0 shape). The flag gates the envelope only — the marker-scan
    /// approval hold below always applies (NEX-SEC-007), so disabling the
    /// flag can never silently turn a tripped scan into an auto-approve.
    /// `true` by default, so runs constructed without flags behave exactly
    /// as before.
    pub injection_enabled: bool,
    /// Optional PR 2B pre/post mutation checkpoint context. When `Some`,
    /// `write_file` and `edit_file` persist fail-closed pre-mutation blobs
    /// tagged with `turn_index` and link `step_id` when recorded.
    pub checkpoints: Option<&'a CheckpointRunContext>,
    /// Zero-based turn index within the current run (`0, 1, ...`).
    pub turn_index: usize,
    /// Optional interactive question context for `ask_user_question`.
    pub question_ctx: Option<&'a QuestionRunContext>,
    /// Optional run-scoped user-added MCP tool runtime (`M18` / `P26`).
    pub mcp_runtime: Option<&'a dyn crate::application::mcp::McpToolRuntime>,
    /// Optional parent-run subagent manager (`M19` / `P27`). Always `None`
    /// inside subagents (`subagent_depth >= 1`).
    pub subagent_manager: Option<&'a Arc<SubagentManager>>,
    /// Nesting depth (`0` for top-level parent run, `1` for child subagents).
    pub subagent_depth: u8,
    /// Active subagent identifier when running inside a child subagent.
    pub subagent_id: Option<&'a str>,
    /// Monotonic step sequence counter for child `AgentRunEvent::SubagentStep` frames.
    pub subagent_step_seq: Option<&'a AtomicUsize>,
}

/// Emit a governance event on the optional channel, best-effort.
pub(crate) fn emit(sender: Option<&Sender<AgentRunEvent>>, event: AgentRunEvent) {
    if let Some(tx) = sender {
        let _ = tx.send(event);
    }
}

/// Maximum tool calls dispatched from one model turn (NEX-SEC-008).
///
/// The step budget counts model turns, so one turn returning an unbounded
/// batch could otherwise execute without bound. Calls past the cap are never
/// executed: each becomes a fixed-vocabulary denial observation (secret-free:
/// fixed text plus the cap only) and the loop continues, so every returned
/// call is still observed exactly once (AC-6) while execution stays bounded.
/// Far above legitimate batches (tests and practice use a handful), so
/// in-budget runs behave exactly as before.
pub(crate) const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// Return `Err(AgentError::Cancelled)` when cancellation was observed.
pub(crate) fn check_cancellation(
    control: Option<&RunControl>,
    sender: Option<&Sender<AgentRunEvent>>,
) -> Result<(), AgentError> {
    if matches!(control, Some(c) if c.is_cancelled()) {
        observe_transition(RunState::Running, RunState::Cancelled);
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
    observe_transition(RunState::Running, RunState::Paused);
    emit(sender, AgentRunEvent::Paused);
    if c.wait_while_paused() {
        observe_transition(RunState::Paused, RunState::Running);
        emit(sender, AgentRunEvent::Resumed);
        Ok(())
    } else {
        observe_transition(RunState::Paused, RunState::Cancelled);
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
    // NEX-SEC-008: bound the batch — only the first `MAX_TOOL_CALLS_PER_TURN`
    // calls dispatch; the rest become fixed-vocabulary denial observations
    // below, so execution per turn is capped while every call stays observed.
    let (dispatchable, over_cap) = calls.split_at(calls.len().min(MAX_TOOL_CALLS_PER_TURN));
    if let Some(gate) = ctx.approval_gate {
        gate.bind_workspace(ctx.workspace_root);
    }
    let preset = ctx.preset.as_str();
    let batch_groups: Vec<String> = dispatchable
        .iter()
        .map(|call| {
            let path = permissions::extract_path(&call.name, &call.arguments);
            let fallback = permissions::group_key(preset, &call.name, path.as_deref());
            ctx.approval_gate
                .map_or(fallback.clone(), |gate| gate.group_key(call, &fallback))
        })
        .collect();
    for call in dispatchable {
        // Trace-level flow marker: presence + length only, never value.
        log::trace!(
            "{}",
            thought_signature_trace(&call.id, call.thought_signature.as_ref())
        );
        check_cancellation(ctx.control, ctx.sender)?;
        if let Some(rec) = record.as_ref() {
            rec.checkpoint()?;
        }
        dispatch_one(ctx, call, messages, record, &batch_groups)?;
        if let Some(rec) = record.as_ref() {
            rec.checkpoint()?;
        }
    }
    for call in over_cap {
        // Over-cap calls never execute, park, or touch the store/ladder:
        // a fixed-vocabulary denial observation (fixed text plus the cap —
        // never the call content) keeps the AC-6 pairing while bounding the
        // turn. Host-generated fixed vocabulary like the document shell ban,
        // so it bypasses the untrusted envelope via `trusted_denial_message`.
        let observation = format!(
            "Error: tool batch exceeds the per-turn limit of {MAX_TOOL_CALLS_PER_TURN} calls; \
             this call was not executed"
        );
        messages.push(trusted_denial_message(call, &observation));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                &observation,
                "denied",
                None,
                StepProvenance::system(),
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // Keep the ordered hard ceilings and three execution branches reviewable together.
fn dispatch_one(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    batch_groups: &[String],
) -> Result<(), AgentError> {
    if !is_dispatchable_tool(ctx, &call.name) {
        let observation = super::tools::ToolError::UnknownTool(call.name.clone()).to_string();
        messages.push(observation_message(ctx, call, &observation));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                &observation,
                "failed",
                None,
                StepProvenance::system(),
            );
        }
        return Ok(());
    }
    // Every execution branch below shares this live server-owned ceiling.
    let refreshed_store = ctx.approval_gate.and_then(|gate| gate.access().store());
    let refreshed = DispatchCtx {
        permission_store: refreshed_store.as_ref().or(ctx.permission_store),
        ..*ctx
    };
    let ctx = &refreshed;
    if let Some(gate) = ctx.approval_gate {
        gate.bind_policy(ctx.preset, ctx.permission_store);
    }
    if call.name == "execute_command"
        && ctx
            .approval_gate
            .is_some_and(|gate| gate.access().sandbox_enabled())
    {
        super::sandbox::refresh(ctx.workspace_root);
    }
    let allowed = ctx.approval_gate.map_or_else(
        || matches!(RiskClass::classify(&call.name), RiskClass::ReadOnly),
        |gate| gate.access().allows(call),
    );
    if !allowed {
        let observation = "Error: tool is blocked by the effective access profile";
        messages.push(trusted_denial_message(call, observation));
        emit_subagent_tool_step(ctx, call, "denied", Some(observation));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                observation,
                "denied",
                None,
                StepProvenance::system(),
            );
        }
        return Ok(());
    }
    // M19 / P27 fail-closed max-depth=1 gate: subagents can NEVER spawn or
    // control subagents even if they smuggle a tool call outside the schema.
    if permissions::is_subagent_tool(&call.name) {
        if ctx.subagent_depth >= MAX_SUBAGENT_DEPTH {
            let observation =
                "Error: subagent tools are not available (maximum subagent nesting depth of 1 reached)";
            messages.push(trusted_denial_message(call, observation));
            emit_subagent_tool_step(ctx, call, "denied", Some(observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    observation,
                    "denied",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        }
        let Some(manager) = ctx.subagent_manager else {
            let observation = "Error: subagent tools require an active parent run subagent manager";
            messages.push(trusted_denial_message(call, observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    observation,
                    "denied",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        };
        let dispatch_started = Instant::now();
        let outcome = manager.dispatch_tool(&call.name, &call.arguments)?;
        let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let status = if outcome.starts_with("Error:") {
            "failed"
        } else {
            "succeeded"
        };
        messages.push(observation_message(ctx, call, &outcome));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                &outcome,
                status,
                Some(dispatch_ms),
                StepProvenance::system(),
            );
        }
        return Ok(());
    }
    if is_dispatchable_tool(ctx, &call.name) {
        let boundary = ctx.approval_gate.map_or_else(
            || {
                super::access::AccessContext::fixed(super::access::AccessProfile::ReadOnly)
                    .identity(ctx.workspace_root, call)
            },
            |gate| gate.access().identity(ctx.workspace_root, call),
        );
        if let Err(error) = boundary {
            let observation = error.to_string();
            messages.push(trusted_denial_message(call, &observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    "denied",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        }
    }
    // T5 structural shell ban (deny-floor): a document run never exposes
    // `execute_command`, so it is denied deterministically here — before the
    // sticky-group path, the permission store (even an `allow` rule must not
    // pass), and the approval ladder. No `ApprovalRequested` is emitted and
    // nothing parks: the denial is a controlled observation and the loop
    // continues. Unknown tools keep their existing controlled-observation
    // path below.
    if ctx.preset == RunPreset::Document && call.name == "execute_command" {
        messages.push(trusted_denial_message(call, DOCUMENT_SHELL_DENIAL));
        emit_subagent_tool_step(ctx, call, "denied", Some(DOCUMENT_SHELL_DENIAL));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                DOCUMENT_SHELL_DENIAL,
                "denied",
                None,
                StepProvenance::system(),
            );
        }
        return Ok(());
    }
    // P34 structural capability gate: a capability-scoped run never exposes
    // tools outside its subset, so such a call is denied deterministically
    // before the sticky-group path, permission store, and approval ladder.
    if let Some(capability) = ctx.capability {
        if is_dispatchable_tool(ctx, &call.name) && !capability.allows_tool(&call.name) {
            let observation = format!(
                "Error: tool '{}' is not available under task capability '{}'",
                call.name,
                capability.as_str()
            );
            messages.push(trusted_denial_message(call, &observation));
            emit_subagent_tool_step(ctx, call, "denied", Some(&observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    "denied",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        }
    }
    // M18 / Invariant 4 fail-closed gate: external MCP tools can NEVER execute
    // without an active interactive ApprovalGate (never auto-approved).
    if permissions::is_mcp_tool(&call.name) && ctx.approval_gate.is_none() {
        messages.push(denied_tool_message(call));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                "Error: tool execution was denied by the user",
                "denied",
                None,
                StepProvenance::system(),
            );
        }
        return Ok(());
    }
    if call.name == "ask_user_question" {
        return dispatch_ask_user_question(ctx, call, messages, record);
    }
    let request_path = permissions::extract_path(&call.name, &call.arguments);
    let fallback = permissions::group_key(ctx.preset.as_str(), &call.name, request_path.as_deref());
    let call_group_key = ctx
        .approval_gate
        .map_or(fallback.clone(), |gate| gate.group_key(call, &fallback));
    let call_group_size = batch_groups
        .iter()
        .filter(|key| *key == &call_group_key)
        .count();
    // WS-C.2 injection hold: the latest tool observation trips the Reviewer
    // marker scan, so this call parks through the existing gate even when
    // the ladder, the sticky group, or the store would auto-execute it.
    // No new policy: the #65 ladder and its recorded decisions are reused
    // unchanged (`park_call`); without a gate there is nobody to approve,
    // so the call fails closed with the frozen denial observation.
    // NEX-SEC-007: the hold is NOT gated by the `injection` flag. The flag
    // switches enveloped vs raw observations only; the scan runs either way,
    // so turning the flag off can never silently disable the hold into an
    // auto-approve. The scan itself (#182 structural shapes) and the
    // single-prompt path below are unchanged.
    // NEX-AGENT-006: when the hold below prompts and the user approves this
    // exact call, the ladder must not prompt a second time for the same call
    // (single prompt path). An explicit `Ask` rule still parks afterwards:
    // recorded "ask me" consent is a separate policy from the hold.
    let mut hold_approved = false;
    if latest_tool_observation(messages)
        .is_some_and(|observation| injection::scan_observation(observation).is_some())
    {
        if ctx.approval_gate.is_some() {
            if !park_call(
                ctx,
                call,
                messages,
                record,
                &call_group_key,
                call_group_size,
                None,
            )? {
                return Ok(());
            }
            hold_approved = true;
        } else {
            messages.push(denied_tool_message(call));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    "Error: tool execution was denied by the user",
                    "denied",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        }
    }
    let store_action =
        handle_permission_store_decision(ctx, call, messages, record, request_path.as_deref());
    if matches!(store_action, StoreAction::Continue { .. })
        && handle_sticky_group_decision(ctx, call, messages, record, &call_group_key)
    {
        return Ok(());
    }
    match store_action {
        StoreAction::Done => return Ok(()),
        // NEX-AGENT-006 single prompt path: the hold already prompted for
        // this exact call and the user approved it, so the ladder must not
        // park it again — fall through to execution. (`considered_rule` is
        // dropped with the skipped park: with no park there is no approval
        // step to name it on. An explicit `Ask` rule still parks via
        // `ParkForAsk` below: recorded "ask me" consent is a separate policy
        // from the hold, not the ladder.)
        StoreAction::Continue { .. } if hold_approved => {}
        action => {
            if !dispatch_store_action(
                ctx,
                call,
                messages,
                record,
                &call_group_key,
                call_group_size,
                action,
            )? {
                return Ok(());
            }
        }
    }
    execute_tool_call(ctx, call, messages, record, &call_group_key);
    Ok(())
}

fn dispatch_ask_user_question(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
) -> Result<(), AgentError> {
    let args = match AskQuestionArgs::parse(&call.arguments) {
        Ok(parsed) => parsed,
        Err(msg) => {
            let observation = super::tools::ToolError::InvalidArguments(msg).to_string();
            messages.push(observation_message(ctx, call, &observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    "failed",
                    None,
                    StepProvenance::system(),
                );
            }
            return Ok(());
        }
    };
    let Some(qctx) = ctx.question_ctx else {
        let observation =
            "Error: ask_user_question requires an active interactive run context".to_string();
        messages.push(observation_message(ctx, call, &observation));
        if let Some(rec) = record.as_mut() {
            rec.tool_call_with_provenance(
                call,
                &observation,
                "failed",
                None,
                StepProvenance::system(),
            );
        }
        return Ok(());
    };
    match qctx.gate.ask_and_wait(
        &qctx.db,
        qctx.run_id,
        qctx.conversation_id,
        &call.id,
        &args,
        ctx.sender,
    ) {
        Ok(answer) => {
            let observation = serde_json::to_string(&answer)
                .unwrap_or_else(|_| "{\"selected_options\":[],\"free_text\":null}".to_string());
            messages.push(observation_message(ctx, call, &observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    "succeeded",
                    None,
                    StepProvenance::user(None),
                );
            }
            Ok(())
        }
        Err(QuestionWaitEnd::Cancelled) => {
            observe_transition(RunState::Running, RunState::Cancelled);
            emit(ctx.sender, AgentRunEvent::Cancelled);
            Err(AgentError::Cancelled)
        }
        Err(QuestionWaitEnd::Database(msg)) => {
            let observation = format!("Error: failed to persist question: {msg}");
            messages.push(observation_message(ctx, call, &observation));
            if let Some(rec) = record.as_mut() {
                rec.tool_call_with_provenance(
                    call,
                    &observation,
                    "failed",
                    None,
                    StepProvenance::system(),
                );
            }
            Ok(())
        }
    }
}

/// Continue dispatch after the permission store (NEX-AGENT-012): park
/// unconditionally for an `Ask` rule, or run the approval ladder otherwise.
/// Returns `Ok(true)` when the call may proceed, `Ok(false)` when it becomes
/// a denial observation, and `Err(Cancelled)` when cancellation ended a park.
fn dispatch_store_action(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
    group_size: usize,
    action: StoreAction,
) -> Result<bool, AgentError> {
    match action {
        StoreAction::Done => Ok(true),
        StoreAction::ParkForAsk { rule_id } => {
            // An `Ask` rule records explicit "ask me" consent for this
            // pattern, so it parks unconditionally — even where the ladder
            // would auto-execute. The eventual approval step names the
            // asking rule (honest surfacing).
            park_call(ctx, call, messages, record, group_key, group_size, rule_id)
        }
        StoreAction::Continue { considered_rule } => park_for_approval(
            ctx,
            call,
            messages,
            record,
            group_key,
            group_size,
            considered_rule,
        ),
    }
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
            // NEX-SEC-001 / M18 Invariant 4: group consent for a previous shell
            // or MCP call cannot authorize this call. Keep denials sticky, but
            // let approved shell/MCP calls reach the store and per-call consent
            // path. Exact-call injection-hold consent is tracked separately by
            // `hold_approved`.
            if approved
                && matches!(
                    RiskClass::classify(&call.name),
                    RiskClass::DangerousShell | RiskClass::ExternalMcp
                )
            {
                return false;
            }
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
            if record.as_ref().is_some_and(|rec| rec.checkpoint().is_err()) {
                return true;
            }
            execute_and_record_bound(
                ctx,
                call,
                messages,
                record,
                StepProvenance::user(Some(group_key)),
            );
            return true;
        }
    }
    false
}

/// What the permission store decided for one tool call (NEX-AGENT-012: a
/// stored rule never upgrades autonomy on its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreAction {
    /// The store fully handled the call (denied, executed via an
    /// authorizing `Allow`, or fail-closed with no gate to ask).
    Done,
    /// An `Ask` rule matched: park unconditionally for an explicit decision,
    /// naming the asking rule on the eventual approval step.
    ParkForAsk { rule_id: Option<i64> },
    /// Fall through to the approval ladder. `considered_rule` names an
    /// `Allow` rule that matched but did not authorize execution, so the
    /// eventual park honestly records which rule caused it (`None` when no
    /// rule was considered).
    Continue { considered_rule: Option<i64> },
}

fn handle_permission_store_decision(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    _request_path: Option<&str>,
) -> StoreAction {
    // M1-core: persistent rules before the gate. Unknown tools skip the store.
    if is_dispatchable_tool(ctx, &call.name) {
        if let Some(store) = ctx.permission_store {
            if let Some(outcome) = store.decide_call(ctx.preset.as_str(), call, ctx.workspace_root)
            {
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
                        return StoreAction::Done;
                    }
                    PermissionOutcome::Allow { rule_id } => {
                        // NEX-AGENT-012: a stored `Allow` never upgrades
                        // autonomy. It authorizes execution only where the
                        // ladder already auto-executes — and never for the
                        // shell or external MCP tools (NEX-SEC-001 / Invariant 4
                        // deny-floor: even an `allow` row must not pass).
                        // Elsewhere the call falls through to the ladder park
                        // below, honestly naming the considered rule.
                        let ladder_auto = ctx
                            .approval_gate
                            .as_ref()
                            .is_some_and(|gate| !gate.needs_approval(call));
                        let never_auto = matches!(
                            RiskClass::classify(&call.name),
                            RiskClass::DangerousShell | RiskClass::ExternalMcp
                        );
                        if ladder_auto
                            && !never_auto
                            && !ctx
                                .approval_gate
                                .is_some_and(|gate| gate.has_current_consent(call))
                        {
                            execute_and_record_bound(
                                ctx,
                                call,
                                messages,
                                record,
                                StepProvenance::rule(rule_id),
                            );
                            return StoreAction::Done;
                        }
                        log::debug!(
                            "permission store: allow rule {} matched for '{}' but does not authorize execution in this mode; parking for consent",
                            rule_id.unwrap_or(-1),
                            call.name
                        );
                        return StoreAction::Continue {
                            considered_rule: rule_id,
                        };
                    }
                    PermissionOutcome::Ask { rule_id } => {
                        // When the gate itself already permits unprompted
                        // `GeneralShell` under a `Verified` native sandbox, let
                        // the v7 baseline seed (`priority = 50`) yield to the
                        // ladder while preserving unconditional parking for all
                        // user-created/explicit `Ask` rules.
                        if call.name == "execute_command"
                            && store.is_seeded_default_shell_ask(rule_id)
                            && ctx
                                .approval_gate
                                .as_ref()
                                .is_some_and(|gate| !gate.needs_approval(call))
                        {
                            return StoreAction::Continue {
                                considered_rule: rule_id,
                            };
                        }
                        // NEX-AGENT-012: an `Ask` rule records explicit "ask
                        // me" consent for this pattern, so it always parks —
                        // in every mode, including `FullAutonomous` (the
                        // NEX-SEC-001 deny-floor principle: a mode must not
                        // silently upgrade a recorded ask into execute).
                        // Without a gate nobody can approve, so fail closed
                        // with the frozen denial observation.
                        if ctx.approval_gate.is_none() {
                            messages.push(denied_tool_message(call));
                            if let Some(rec) = record.as_mut() {
                                rec.approval_with_provenance(
                                    call,
                                    false,
                                    StepProvenance::rule(rule_id),
                                );
                            }
                            return StoreAction::Done;
                        }
                        return StoreAction::ParkForAsk { rule_id };
                    }
                }
            }
        }
    }
    StoreAction::Continue {
        considered_rule: None,
    }
}

fn park_for_approval(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
    group_size: usize,
    considered_rule: Option<i64>,
) -> Result<bool, AgentError> {
    // Task 4.1: approval gate evaluated at the per-tool-call
    // boundary, before dispatch. Auto paths execute exactly as
    // before; denied calls become a controlled observation and the
    // loop continues; cancellation while parked aborts. `considered_rule`
    // names a permission rule that matched but did not authorize execution
    // (NEX-AGENT-012 honest surfacing on the eventual approval step).
    if let Some(gate) = ctx.approval_gate {
        if gate.needs_approval(call) {
            return park_call(
                ctx,
                call,
                messages,
                record,
                group_key,
                group_size,
                considered_rule,
            );
        }
    }
    Ok(true)
}

/// Park `call` unconditionally through the existing approval gate.
///
/// Task 4.1 ladder decisions (`park_for_approval`) and the WS-C.2 injection
/// hold share this body: the caller decides *whether* to park
/// (`needs_approval` for the ladder, a tripped marker scan for the hold);
/// this function only performs the park — the `AwaitingApproval`
/// transition, the secret-free audit entries, the `ApprovalRequested`
/// emission, the blocking resolve, and the recorded gate decision.
/// Returns `Ok(true)` when the call may proceed (approved), `Ok(false)`
/// when it becomes a denial observation, and `Err(Cancelled)` when
/// cancellation ended the wait. All three outcomes resolve to a recorded
/// decision (WS-B.2 invariant); the #65 ladder itself is reused unchanged.
/// Abort a parked call whose wait ended without a user decision
/// (NEX-AGENT-004): record the honest step, transition
/// `AwaitingApproval → Cancelled`, audit the edge, and emit the plain
/// `Cancelled` event. [`ParkEnd::Cancelled`] names the user cancel;
/// [`ParkEnd::TimedOut`] names the expired deadline (never the user-cancel
/// vocabulary), and [`ParkEnd::Shutdown`] names the application shutdown
/// (NEX-TASK-002, never the user-cancel vocabulary either) — all three
/// still abort as `Cancelled`, the only honest schema terminal. The run
/// thread then unwinds and releases the conversation claim, so a new run
/// can start. Always returns [`AgentError::Cancelled`].
fn abort_parked_call(
    ctx: &DispatchCtx<'_>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    call: &ToolCall,
    end: ParkEnd,
    timeout: Duration,
) -> AgentError {
    match end {
        ParkEnd::Cancelled => {
            // Task 4.2: cancellation ended the parked wait —
            // record the `cancelled` approval step (D12).
            if let Some(rec) = record.as_mut() {
                rec.approval_cancelled(call);
            }
            observe_transition(RunState::AwaitingApproval, RunState::Cancelled);
            // WS-B.2: every park resolves to a recorded gate decision —
            // cancellation included.
            audit(
                ctx.audit,
                RunState::AwaitingApproval,
                RunState::Cancelled,
                AuditEvent::ApprovalCancelled,
            );
        }
        ParkEnd::TimedOut => {
            // NEX-AGENT-004: the park deadline elapsed with no decision.
            // The recorded step and audit edge name the timeout: an expired
            // deadline is neither a user denial nor a user cancel.
            if let Some(rec) = record.as_mut() {
                rec.approval_timed_out(call, timeout);
            }
            observe_transition(RunState::AwaitingApproval, RunState::Cancelled);
            audit(
                ctx.audit,
                RunState::AwaitingApproval,
                RunState::Cancelled,
                AuditEvent::ApprovalTimedOut,
            );
        }
        ParkEnd::Shutdown => {
            // NEX-TASK-002: application shutdown ended the parked wait.
            // The recorded step and audit edge name the shutdown: a quit is
            // neither a user denial nor a user cancel.
            if let Some(rec) = record.as_mut() {
                rec.approval_shutdown_cancelled(call);
            }
            observe_transition(RunState::AwaitingApproval, RunState::Cancelled);
            audit(
                ctx.audit,
                RunState::AwaitingApproval,
                RunState::Cancelled,
                AuditEvent::ApprovalShutdown,
            );
        }
    }
    emit(ctx.sender, AgentRunEvent::Cancelled);
    AgentError::Cancelled
}

fn park_call(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
    group_size: usize,
    considered_rule: Option<i64>,
) -> Result<bool, AgentError> {
    if let Some(gate) = ctx.approval_gate {
        let parked_identity = gate.identity(call);
        let request_id = gate.prepare_pending_with_group(call, Some(group_key.to_owned()));
        // INVARIANT: once ApprovalRequested is emitted, a pending entry for that call_id exists,
        // so a concurrent resolve cannot hit NoPendingApproval вЂ” the race is closed by construction.
        super::lifecycle::observe_transition(
            super::lifecycle::RunState::Running,
            super::lifecycle::RunState::AwaitingApproval,
        );
        // WS-B.2: the park is recorded on the run-scoped audit trail
        // (secret-free: fixed vocabulary only, no call content).
        audit(
            ctx.audit,
            super::lifecycle::RunState::Running,
            super::lifecycle::RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        );
        emit(
            ctx.sender,
            AgentRunEvent::ApprovalRequested {
                metadata: gate.metadata(call),
                call_id: request_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                group_key: Some(group_key.to_owned()),
                group_size,
                timeout_secs: gate.timeout().as_secs(),
            },
        );
        // The hold parks unconditionally: `park_until_resolved` reuses the
        // gate's pending/response machinery but skips the ladder fast-path,
        // which would otherwise auto-approve (and swallow) the hold in
        // auto-approve modes.
        let decision = match gate.park_until_resolved(call) {
            Ok(decision) => decision,
            // The wait ended with no user decision — cancellation or the
            // NEX-AGENT-004 park deadline. Abort honestly (never relabelled
            // as a user denial); see `abort_parked_call`.
            Err(end) => return Err(abort_parked_call(ctx, record, call, end, gate.timeout())),
        };
        let invalidated = decision == ApprovalDecision::Invalidated
            || !gate.access().allows(call)
            || gate.identity(call) != parked_identity;
        let approved = decision == ApprovalDecision::Approved && !invalidated;
        // Task 4.2: record the parked approval decision (D12). A rule that
        // matched but did not authorize execution stays named on the step
        // (NEX-AGENT-012 honest surfacing: the user decided, the rule caused
        // the park).
        if let Some(rec) = record.as_mut() {
            rec.approval_with_provenance(
                call,
                approved,
                if invalidated {
                    StepProvenance::system()
                } else {
                    StepProvenance::user_with_rule(Some(group_key), considered_rule)
                },
            );
        }
        if let Some(rec) = record.as_ref() {
            rec.checkpoint()?;
        }
        super::lifecycle::observe_transition(
            super::lifecycle::RunState::AwaitingApproval,
            super::lifecycle::RunState::Running,
        );
        // WS-B.2: the gate decision is recorded on the run-scoped audit
        // trail (fixed `approved` / `denied` vocabulary, no call content).
        // The #65 ladder itself is reused unchanged — never bypassed, never
        // remapped.
        let outcome = if approved {
            GateOutcome::Approved
        } else {
            GateOutcome::Denied
        };
        if let Some(event) = AuditEvent::for_gate(outcome) {
            audit(
                ctx.audit,
                super::lifecycle::RunState::AwaitingApproval,
                super::lifecycle::RunState::Running,
                event,
            );
        }
        emit(
            ctx.sender,
            AgentRunEvent::ApprovalResolved {
                call_id: request_id.clone(),
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
                    content: if invalidated {
                        "Error: approval was invalidated by an execution-context change"
                    } else {
                        "Error: tool execution was denied by the user"
                    }
                    .to_string(),
                }),
            });
            return Ok(false);
        }
    }
    Ok(true)
}

/// Latest tool observation in `messages`, if any.
///
/// WS-C.2 injection hold input: the most recent `Tool` result content
/// (enveloped or otherwise) is what the next dispatched call would build on,
/// so a tripped marker scan here parks that call. Pure view — `None` when no
/// tool has produced output yet.
fn latest_tool_observation(messages: &[AiMessage]) -> Option<&str> {
    messages.iter().rev().find_map(|message| {
        if message.role == AiRole::Tool {
            message
                .tool_result
                .as_ref()
                .map(|result| result.content.as_str())
        } else {
            None
        }
    })
}

fn execute_bound(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    hook: Option<&mut CheckpointHook<'_>>,
) -> Result<String, super::tools::ToolError> {
    if permissions::is_mcp_tool(&call.name) {
        let Some(gate) = ctx.approval_gate else {
            return Err(super::tools::ToolError::ConsentInvalidated);
        };
        let Some(runtime) = ctx.mcp_runtime else {
            return Err(super::tools::ToolError::UnknownTool(call.name.clone()));
        };
        return gate.execute_authorized(call, ctx.workspace_root, |_target, started| {
            started();
            runtime
                .call_tool(&call.name, &call.arguments, ctx.token)
                .map_err(super::tools::ToolError::Io)
        });
    }
    if call.name == "execute_command" && ctx.sender.is_some() {
        let mut sink = |update: super::tools::CommandStreamUpdate| {
            let event = match update {
                super::tools::CommandStreamUpdate::Started { command } => {
                    AgentRunEvent::CommandOutput {
                        call_id: call.id.clone(),
                        command: Some(command),
                        stream: "stdout".to_string(),
                        chunk: String::new(),
                        truncated: false,
                        killed: false,
                    }
                }
                super::tools::CommandStreamUpdate::Chunk { stream, text } => {
                    AgentRunEvent::CommandOutput {
                        call_id: call.id.clone(),
                        command: None,
                        stream: stream.to_string(),
                        chunk: text,
                        truncated: false,
                        killed: false,
                    }
                }
                super::tools::CommandStreamUpdate::Done { truncated, killed } => {
                    AgentRunEvent::CommandOutput {
                        call_id: call.id.clone(),
                        command: None,
                        stream: "status".to_string(),
                        chunk: String::new(),
                        truncated,
                        killed,
                    }
                }
            };
            emit(ctx.sender, event);
        };
        return ToolRegistry::execute_agent_with_checkpoints_streaming(
            call,
            ctx.workspace_root,
            ctx.token,
            ctx.approval_gate,
            hook,
            Some(&mut sink),
        );
    }

    ToolRegistry::execute_agent_with_checkpoints(
        call,
        ctx.workspace_root,
        ctx.token,
        ctx.approval_gate,
        hook,
    )
}

fn execute_and_record_bound(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    valid_provenance: StepProvenance<'_>,
) {
    let mut hook = ctx
        .checkpoints
        .map(|cp_ctx| CheckpointHook::new(cp_ctx, ctx.turn_index));
    let dispatch_started = Instant::now();
    let outcome = execute_bound(ctx, call, hook.as_mut());
    let captured_checkpoint_id = hook.as_ref().and_then(CheckpointHook::captured_id);
    let invalidated = matches!(outcome, Err(super::tools::ToolError::ConsentInvalidated));
    let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let (observation, tool_status) = classify_outcome(outcome, ctx.token);
    messages.push(observation_message(ctx, call, &observation));
    emit_subagent_tool_step(ctx, call, tool_status, Some(&observation));
    if let Some(rec) = record.as_mut() {
        let provenance = if invalidated {
            StepProvenance::system()
        } else {
            valid_provenance
        };
        let step_id = rec.tool_call_with_provenance(
            call,
            &observation,
            tool_status,
            Some(dispatch_ms),
            provenance,
        );
        if let (Some(cp_ctx), Some(cp_id), Some(sid)) =
            (ctx.checkpoints, captured_checkpoint_id, step_id)
        {
            if let Err(err) = cp_ctx.link_step(cp_id, sid) {
                log::warn!("file checkpoint step linkage failed: {}", err.summary());
            }
        }
    }
}

fn execute_tool_call(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    group_key: &str,
) {
    let user_approved = ctx
        .approval_gate
        .is_some_and(|gate| gate.has_current_consent(call));
    let valid_provenance = if user_approved {
        StepProvenance::user(Some(group_key))
    } else {
        StepProvenance::system()
    };
    execute_and_record_bound(ctx, call, messages, record, valid_provenance);
}

fn emit_subagent_tool_step(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    status: &str,
    observation: Option<&str>,
) {
    let (Some(subagent_id), Some(seq_counter)) = (ctx.subagent_id, ctx.subagent_step_seq) else {
        return;
    };
    let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let target = permissions::extract_path(&call.name, &call.arguments).or_else(|| {
        serde_json::from_str::<serde_json::Value>(&call.arguments)
            .ok()
            .and_then(|v| {
                v.get("path")
                    .or_else(|| v.get("pattern"))
                    .or_else(|| v.get("command"))
                    .and_then(serde_json::Value::as_str)
                    .map(|s| injection::redact_secrets(s).0)
            })
    });
    let redacted_obs = observation.map(|text| injection::redact_secrets(text).0);
    let (preview, _, _) = step_preview_opt(redacted_obs.as_deref());
    emit(
        ctx.sender,
        AgentRunEvent::SubagentStep {
            subagent_id: subagent_id.to_string(),
            seq,
            kind: "tool_call".to_string(),
            tool_name: Some(call.name.clone()),
            target,
            status: Some(status.to_string()),
            observation: preview,
        },
    );
}

// AC-302: the inline test mass (2,596 lines) moved verbatim to
// `dispatch/tests.rs`; same module path `dispatch::tests`, wired via #[path].
#[cfg(test)]
#[path = "dispatch/tests.rs"]
mod tests;
