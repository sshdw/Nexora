//! Per-tool-call dispatch pipeline (split from `runner`).

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, ParkEnd, RiskClass};
use crate::application::agent::control::{AgentRunEvent, CancellationToken, RunControl};
use crate::application::agent::governance::{audit, AuditEvent, AuditLog, GateOutcome};
use crate::application::agent::injection;
use crate::application::agent::permissions::{self, PermissionOutcome, PermissionStore, RunPreset};
use crate::application::agent::persistence::{ActiveRunRecord, StepProvenance};
use crate::application::agent::roles::AgentRole;
use crate::application::agent::tools::ToolRegistry;
use crate::application::execution::{AiMessage, AiRole, ToolCall};

use super::errors::AgentError;
use super::lifecycle::{observe_transition, RunState};
use super::prompts::{
    classify_outcome, denied_tool_message, thought_signature_trace, tool_message, tool_message_raw,
    trusted_denial_message, DOCUMENT_SHELL_DENIAL,
};

/// Wrap one tool observation for context: the WS-C.2 envelope when the
/// `injection` flag is on, the raw pre-2.0 shape when it is off.
fn observation_message(ctx: &DispatchCtx<'_>, call: &ToolCall, observation: &str) -> AiMessage {
    if ctx.injection_enabled {
        tool_message(call, observation)
    } else {
        tool_message_raw(call, observation)
    }
}

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
    /// Run preset (T5): the schema filter and the structural shell ban both
    /// key off this. `Coding` preserves the exact pre-T5 pipeline.
    pub preset: RunPreset,
    pub sender: Option<&'a Sender<AgentRunEvent>>,
    /// Optional run-scoped audit trail (WS-B.2): every approval park records
    /// its gate decision here, secret-free. `None` keeps the exact pre-B.2
    /// pipeline.
    pub audit: Option<&'a AuditLog>,
    /// Optional role-scoped tool subset (WS-B.3): when `Some`, a known tool
    /// outside the role's subset becomes a controlled denial observation
    /// before the store, the ladder, and execution (mirroring the T5 document
    /// shell ban). `None` keeps the exact pre-B.3 pipeline.
    pub role: Option<AgentRole>,
    /// The `injection` feature flag (WS-C.2 gate): enveloped observations
    /// when `true` (current behavior); raw observations when `false`
    /// (pre-2.0 shape). The flag gates the envelope only — the marker-scan
    /// approval hold below always applies (NEX-SEC-007), so disabling the
    /// flag can never silently turn a tripped scan into an auto-approve.
    /// `true` by default, so runs constructed without flags behave exactly
    /// as before.
    pub injection_enabled: bool,
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
    let preset = ctx.preset.as_str();
    let batch_groups: Vec<String> = dispatchable
        .iter()
        .map(|call| {
            let path = permissions::extract_path(&call.name, &call.arguments);
            permissions::group_key(preset, &call.name, path.as_deref())
        })
        .collect();
    for call in dispatchable {
        // Trace-level flow marker: presence + length only, never value.
        log::trace!(
            "{}",
            thought_signature_trace(&call.id, call.thought_signature.as_ref())
        );
        check_cancellation(ctx.control, ctx.sender)?;
        dispatch_one(ctx, call, messages, record, &batch_groups)?;
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

fn dispatch_one(
    ctx: &DispatchCtx<'_>,
    call: &ToolCall,
    messages: &mut Vec<AiMessage>,
    record: &mut Option<&mut ActiveRunRecord<'_>>,
    batch_groups: &[String],
) -> Result<(), AgentError> {
    // T5 structural shell ban (deny-floor): a document run never exposes
    // `execute_command`, so it is denied deterministically here — before the
    // sticky-group path, the permission store (even an `allow` rule must not
    // pass), and the approval ladder. No `ApprovalRequested` is emitted and
    // nothing parks: the denial is a controlled observation and the loop
    // continues. Unknown tools keep their existing controlled-observation
    // path below.
    if ctx.preset == RunPreset::Document && call.name == "execute_command" {
        messages.push(trusted_denial_message(call, DOCUMENT_SHELL_DENIAL));
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
    // WS-B.3 structural role gate: a role-scoped run never exposes tools
    // outside the role's subset, so such a call is denied deterministically
    // here — before the sticky-group path, the permission store, and the
    // approval ladder. Unknown tools skip this gate and keep their existing
    // controlled-observation path below. Both interpolated values are fixed
    // vocabulary (a known tool name, a canonical role name), so the denial
    // stays secret-free by construction.
    if let Some(role) = ctx.role {
        if permissions::is_known_tool(&call.name) && !role.allows_tool(&call.name) {
            let observation = format!(
                "Error: tool '{}' is not available to agent role '{}'",
                call.name,
                role.as_str()
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
            return Ok(());
        }
    }
    let request_path = permissions::extract_path(&call.name, &call.arguments);
    let call_group_key =
        permissions::group_key(ctx.preset.as_str(), &call.name, request_path.as_deref());
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
    if handle_sticky_group_decision(ctx, call, messages, record, &call_group_key) {
        return Ok(());
    }
    match handle_permission_store_decision(ctx, call, messages, record, request_path.as_deref()) {
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
            messages.push(observation_message(ctx, call, &observation));
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
    request_path: Option<&str>,
) -> StoreAction {
    // M1-core: persistent rules before the gate. Unknown tools skip the store.
    if permissions::is_known_tool(&call.name) {
        if let Some(store) = ctx.permission_store {
            if let Some(outcome) = store.decide(
                ctx.preset.as_str(),
                &call.name,
                request_path,
                RiskClass::classify(&call.name),
            ) {
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
                        // shell (NEX-SEC-001 deny-floor: even a legacy shell
                        // `allow` row must not pass). Elsewhere the call
                        // falls through to the ladder park below, honestly
                        // naming the considered rule.
                        let ladder_auto = ctx
                            .approval_gate
                            .as_ref()
                            .is_some_and(|gate| !gate.needs_approval(call));
                        let shell =
                            matches!(RiskClass::classify(&call.name), RiskClass::DangerousShell);
                        if ladder_auto && !shell {
                            let dispatch_started = Instant::now();
                            let outcome = ToolRegistry::execute_with_cancellation(
                                call,
                                ctx.workspace_root,
                                ctx.token,
                            );
                            let dispatch_ms = i64::try_from(dispatch_started.elapsed().as_millis())
                                .unwrap_or(i64::MAX);
                            let (observation, tool_status) = classify_outcome(outcome, ctx.token);
                            messages.push(observation_message(ctx, call, &observation));
                            if let Some(rec) = record.as_mut() {
                                rec.tool_call_with_provenance(
                                    call,
                                    &observation,
                                    tool_status,
                                    Some(dispatch_ms),
                                    StepProvenance::rule(rule_id),
                                );
                            }
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
        gate.prepare_pending_with_group(call, Some(group_key.to_owned()));
        emit(
            ctx.sender,
            AgentRunEvent::ApprovalRequested {
                call_id: call.id.clone(),
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
        let approved = matches!(decision, ApprovalDecision::Approved);
        // Task 4.2: record the parked approval decision (D12). A rule that
        // matched but did not authorize execution stays named on the step
        // (NEX-AGENT-012 honest surfacing: the user decided, the rule caused
        // the park).
        if let Some(rec) = record.as_mut() {
            rec.approval_with_provenance(
                call,
                approved,
                StepProvenance::user_with_rule(Some(group_key), considered_rule),
            );
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
    messages.push(observation_message(ctx, call, &observation));
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

// AC-302: the inline test mass (2,596 lines) moved verbatim to
// `dispatch/tests.rs`; same module path `dispatch::tests`, wired via #[path].
#[cfg(test)]
#[path = "dispatch/tests.rs"]
mod tests;
