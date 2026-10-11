//! Three-tier approval gate (ROADMAP.md Phase 4 — Task 4.1).
//!
//! The HD-3 autonomy ladder decides, per tool risk class and selected
//! [`AutonomyMode`], whether a tool call executes automatically or parks the
//! run until the user approves or denies it. The gate is a cheap cloneable
//! handle (`Arc<Mutex<_>> + Condvar`, std-only, poison-safe) mirroring
//! [`crate::application::agent::control::RunControl`] conventions: every clone
//! governs the same underlying run, `mode()` is readable and `set_mode()`
//! changeable at runtime, `request_approval` auto-decides per the ladder or
//! parks until `respond` (or cancellation), and cancellation shares the run's
//! [`CancellationToken`] so the Task 3.2 no-deadlock guarantee extends to
//! approval waits.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::application::agent::control::CancellationToken;
use crate::application::execution::ToolCall;

/// Default approval-park deadline (NEX-AGENT-004): a parked run waits this
/// long for a user decision before the park expires and the run is
/// cancelled. Minutes-scale so a user who steps away briefly is unaffected,
/// while a dropped event or a closed frontend can never lock the
/// conversation forever. Overridable per gate via [`ApprovalGate::with_timeout`]
/// and per deployment via the `agent.approval_timeout_secs` setting
/// ([`super::service::APPROVAL_TIMEOUT_KEY`]).
pub(crate) const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_mins(10);

// ---------------------------------------------------------------------------
// Risk classification and autonomy ladder
// ---------------------------------------------------------------------------

/// Risk class of a workspace tool, conservative by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskClass {
    /// `read_file`, `list_directory`, `search_files`.
    ReadOnly,
    /// `write_file`, `edit_file`, and any unknown tool.
    Mutating,
    /// `execute_command` only: an unconfined shell (NEX-SEC-001). The
    /// command string, its network access, and its file reach cannot be
    /// statically classified, so the shell always requires explicit user
    /// consent — even in `FullAutonomous`.
    DangerousShell,
}

impl RiskClass {
    /// Classify a tool by name.
    #[must_use]
    pub(crate) fn classify(name: &str) -> Self {
        match name {
            "read_file" | "list_directory" | "search_files" => Self::ReadOnly,
            "execute_command" => Self::DangerousShell,
            _ => Self::Mutating,
        }
    }
}

/// Autonomy ladder (coordinator ruling, Task 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutonomyMode {
    /// Every tool call requires user approval, including reads.
    Supervised,
    /// `ReadOnly` executes automatically; `Mutating` requires approval.
    SemiAutonomous,
    /// Everything executes automatically, bounded only by the Task 3.2
    /// governor/timeout safeguards.
    FullAutonomous,
}

/// Decision for an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    Approved,
    Denied,
}

/// How a [`ApprovalGate::park_until_resolved`] wait ended without a user
/// decision (NEX-AGENT-004). Kept distinct from [`ApprovalDecision`] on
/// purpose: an expired deadline is neither a user denial nor a user cancel,
/// so every consumer must name the timeout honestly instead of relabelling
/// it as user action. NEX-TASK-002 adds the same honesty for shutdown: a
/// shutdown-cancel is neither a user denial nor a user cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParkEnd {
    /// Cancellation was observed on the shared token.
    Cancelled,
    /// The park deadline elapsed with no decision and no cancellation.
    TimedOut,
    /// Application shutdown was observed on the shared token
    /// (NEX-TASK-002): the wait ends like [`Self::Cancelled`] (the run
    /// still aborts as `Cancelled`, the only honest schema terminal) but
    /// the recorded step names the shutdown, never the user-cancel text.
    Shutdown,
}

/// Fixed-vocabulary rendering of a park deadline for timeout observations
/// and notices (whole seconds, minutes above a minute, millis below a
/// second so short test deadlines still read honestly).
#[must_use]
pub(crate) fn format_timeout(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs >= 1 {
        format!("{secs}s")
    } else {
        format!("{}ms", timeout.as_millis())
    }
}

// ---------------------------------------------------------------------------
// Gate state
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PendingApproval {
    id: String,
    decision: Option<ApprovalDecision>,
    /// Tool name for the pending call (M1-core grouping).
    tool_name: String,
    /// Session-sticky group key for the pending call (M1-core).
    group_key: Option<String>,
}

#[derive(Debug)]
struct GateState {
    mode: AutonomyMode,
    pending: Option<PendingApproval>,
    /// Session-sticky group decisions for this run (M1-core): further
    /// same-group parks auto-resolve identically without parking.
    group_decisions: std::collections::HashMap<String, ApprovalDecision>,
    /// Park deadline for every [`ApprovalGate::park_until_resolved`] wait
    /// (NEX-AGENT-004). Shared by all clones; set once per run from the
    /// `agent.approval_timeout_secs` setting.
    timeout: Duration,
}

// ---------------------------------------------------------------------------
// Gate handle
// ---------------------------------------------------------------------------

/// Cheap cloneable approval handle. Mirrors `RunControl` concurrency patterns:
/// `Arc<Mutex<state>> + Condvar`, std-only, poison-safe. All clones share the
/// same mode, pending request, and cancellation token when wired through the
/// runner's builder (which shares the run's `CancellationToken`).
#[derive(Debug, Clone)]
pub(crate) struct ApprovalGate {
    token: Arc<Mutex<CancellationToken>>,
    state: Arc<Mutex<GateState>>,
    signal: Arc<Condvar>,
}

impl ApprovalGate {
    /// Create a gate in `mode` with a fresh cancellation token.
    pub(crate) fn new(mode: AutonomyMode) -> Self {
        Self {
            token: Arc::new(Mutex::new(CancellationToken::new())),
            state: Arc::new(Mutex::new(GateState {
                mode,
                pending: None,
                group_decisions: std::collections::HashMap::new(),
                timeout: DEFAULT_APPROVAL_TIMEOUT,
            })),
            signal: Arc::new(Condvar::new()),
        }
    }

    /// Create a gate in `mode` sharing `token` (the run's token). Prefer this
    /// when wiring through `AgentRunner` so cancellation while parked aborts
    /// with `Cancelled` and wakes without deadlock.
    pub(crate) fn with_token(mode: AutonomyMode, token: CancellationToken) -> Self {
        Self {
            token: Arc::new(Mutex::new(token)),
            state: Arc::new(Mutex::new(GateState {
                mode,
                pending: None,
                group_decisions: std::collections::HashMap::new(),
                timeout: DEFAULT_APPROVAL_TIMEOUT,
            })),
            signal: Arc::new(Condvar::new()),
        }
    }

    /// Override the park deadline (NEX-AGENT-004). Prefer this in tests for
    /// a short deadline; production uses
    /// [`super::service::resolve_approval_timeout`] plus [`Self::set_timeout`].
    #[must_use]
    pub(crate) fn with_timeout(self, timeout: Duration) -> Self {
        self.set_timeout(timeout);
        self
    }

    /// Change the park deadline at runtime. Affects parks started after the
    /// change; a park already waiting recomputes nothing (its deadline was
    /// fixed at park entry). Shared by all clones.
    pub(crate) fn set_timeout(&self, timeout: Duration) {
        self.lock_state().timeout = timeout;
    }

    /// Current park deadline.
    #[must_use]
    pub(crate) fn timeout(&self) -> Duration {
        self.lock_state().timeout
    }

    fn lock_state(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_token(&self) -> MutexGuard<'_, CancellationToken> {
        self.token.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Current autonomy mode.
    #[must_use]
    pub(crate) fn mode(&self) -> AutonomyMode {
        self.lock_state().mode
    }

    /// Change the autonomy mode at runtime. Affects the next tool-call
    /// decision; a request already parked is not auto-resolved.
    pub(crate) fn set_mode(&self, mode: AutonomyMode) {
        self.lock_state().mode = mode;
        // No need to wake a parked approval: the pending decision is still
        // required and mode only affects the *next* call. Notifying is
        // harmless, so we avoid it to keep semantics explicit.
    }

    /// Share the run's cancellation token with this gate. All clones see the
    /// update because `token` is behind a shared `Arc<Mutex<_>>`. The runner
    /// calls this when wiring `RunControl` and `ApprovalGate` together.
    pub(crate) fn set_token(&self, token: CancellationToken) {
        *self.lock_token() = token;
    }

    /// The token currently shared by this gate.
    #[must_use]
    pub(crate) fn token_cloned(&self) -> CancellationToken {
        self.lock_token().clone()
    }

    /// Whether cancellation has been requested on the shared token.
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock_token().is_cancelled()
    }

    /// Whether the shared token was cancelled by application shutdown
    /// (NEX-TASK-002) rather than by a user cancel.
    #[must_use]
    pub(crate) fn is_shutdown(&self) -> bool {
        self.lock_token().is_shutdown()
    }

    /// How a cancelled wait ended: [`ParkEnd::Shutdown`] when the shutdown
    /// origin is set, [`ParkEnd::Cancelled`] otherwise. Every cancel
    /// observation in [`Self::park_until_resolved`] funnels through here so
    /// the origin can never be relabelled by accident.
    fn cancel_end(&self) -> ParkEnd {
        if self.is_shutdown() {
            ParkEnd::Shutdown
        } else {
            ParkEnd::Cancelled
        }
    }

    /// Cancel the shared token and wake every waiter parked in
    /// `request_approval`. Never blocks.
    pub(crate) fn cancel(&self) {
        self.lock_token().cancel();
        self.signal.notify_all();
    }

    /// Whether `call` would require user approval under the current mode and
    /// risk class. Does not park.
    ///
    /// NEX-SEC-001 deny-floor: `DangerousShell` (`execute_command`) always
    /// requires explicit consent, even in `FullAutonomous`.
    #[must_use]
    pub(crate) fn needs_approval(&self, call: &ToolCall) -> bool {
        let risk = RiskClass::classify(&call.name);
        let mode = self.mode();
        match mode {
            AutonomyMode::Supervised => true,
            AutonomyMode::SemiAutonomous => {
                matches!(risk, RiskClass::Mutating | RiskClass::DangerousShell)
            }
            AutonomyMode::FullAutonomous => {
                matches!(risk, RiskClass::DangerousShell)
            }
        }
    }

    /// Pre-register a pending entry for `call` so that an `ApprovalRequested`
    /// emission that follows can be resolved immediately without a race.
    /// Called by the runner before emitting the event; does not notify.
    pub(crate) fn prepare_pending(&self, call: &ToolCall) {
        self.prepare_pending_with_group(call, None);
    }

    /// Pre-register a pending entry with M1-core grouping metadata.
    pub(crate) fn prepare_pending_with_group(&self, call: &ToolCall, group_key: Option<String>) {
        let mut state = self.lock_state();
        match state.pending.as_ref() {
            Some(existing) if existing.id == call.id => {
                // Already prepared for this call — preserve any early decision.
            }
            _ => {
                state.pending = Some(PendingApproval {
                    id: call.id.clone(),
                    decision: None,
                    tool_name: call.name.clone(),
                    group_key,
                });
            }
        }
    }

    /// Decide for `call` per the autonomy ladder, or park until
    /// `respond(request_id, decision)` or cancellation. Returns
    /// `Err(())` when cancellation aborted the wait or the park deadline
    /// expired (the runner maps this to `AgentError::Cancelled`).
    pub(crate) fn request_approval(&self, call: &ToolCall) -> Result<ApprovalDecision, ()> {
        // Fast-path: auto-approved without ever creating a pending entry.
        if !self.needs_approval(call) {
            // A `prepare_pending` may have pre-registered this exact call id
            // before the mode was switched to auto-approve. That stale entry
            // would linger until the next different-id approval overwrote it
            // (hotfix-review backlog, classified BENIGN). Clear it here so a
            // late `respond` for the stale id resolves nothing (Task 6.2).
            let mut state = self.lock_state();
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == call.id)
            {
                state.pending = None;
            }
            return Ok(ApprovalDecision::Approved);
        }

        self.park_until_resolved(call).map_err(|_| ())
    }

    /// Park unconditionally until `respond(request_id, decision)`,
    /// cancellation, or the park deadline (WS-C.2 injection hold).
    ///
    /// The same pending/response/cancellation machinery as
    /// [`Self::request_approval`], minus the ladder fast-path: the caller
    /// has already decided a park is required (a tripped injection scan),
    /// so an auto-approve mode must not swallow it. Returns
    /// `Err(ParkEnd::Cancelled)` when a user cancellation aborted the wait,
    /// `Err(ParkEnd::Shutdown)` when application shutdown aborted it
    /// (NEX-TASK-002), and `Err(ParkEnd::TimedOut)` when the deadline elapsed
    /// with no decision (NEX-AGENT-004) — every park arm (Ask parks, ladder parks,
    /// group-sticky parks, injection holds) funnels through this one wait,
    /// so the deadline covers them all.
    pub(crate) fn park_until_resolved(&self, call: &ToolCall) -> Result<ApprovalDecision, ParkEnd> {
        // Create pending entry, reusing an early-prepared one for the same
        // call id so an early decision (respond before park) is not clobbered.
        {
            let mut state = self.lock_state();
            match state.pending.as_ref() {
                Some(existing) if existing.id == call.id => {
                    // Reuse the pending prepared by `prepare_pending`; keep decision if already set.
                }
                _ => {
                    // If a previous pending somehow remains for a different id
                    // (should not happen in the sequential per-tool-call runner loop),
                    // replace it conservatively.
                    state.pending = Some(PendingApproval {
                        id: call.id.clone(),
                        decision: None,
                        tool_name: call.name.clone(),
                        group_key: None,
                    });
                }
            }
        }

        // Park until a decision arrives, cancellation fires, or the deadline
        // elapses. Use a short timeout so a cancellation that only sets the
        // shared token (without notifying *this* gate's condvar, e.g. via
        // `RunControl::cancel`) still wakes promptly: the loop re-checks
        // `is_cancelled` each timeout. This extends the Task 3.2 no-deadlock
        // guarantee to approval waits even when only the token is shared.
        // The wait is additionally bounded by the park deadline fixed at
        // entry (NEX-AGENT-004): an unanswered approval can never block the
        // run thread — and hold the conversation claim — forever.
        let deadline = Instant::now() + self.timeout();
        let mut state = self.lock_state();
        loop {
            if self.is_cancelled() {
                state.pending = None;
                return Err(self.cancel_end());
            }
            if let Some(pending) = state.pending.as_ref() {
                if pending.id == call.id {
                    if let Some(decision) = pending.decision {
                        let result = decision;
                        state.pending = None;
                        return Ok(result);
                    }
                } else {
                    // Pending id mismatch: treat as cancelled/overwritten.
                    // Should not happen in sequential dispatch.
                }
            } else {
                // Pending cleared externally (e.g. cancellation).
                if self.is_cancelled() {
                    return Err(self.cancel_end());
                }
            }
            // Deadline check before sleeping: expiry clears the pending entry
            // (a late `respond` for this id then resolves nothing) and ends
            // the wait as a timeout, never as a user decision.
            let now = Instant::now();
            if now >= deadline {
                state.pending = None;
                // Shutdown may have landed after the loop-top `is_cancelled`
                // check (NEX-TASK-002): name the shutdown, never a timeout.
                if self.is_shutdown() {
                    return Err(ParkEnd::Shutdown);
                }
                return Err(ParkEnd::TimedOut);
            }
            // Wait with timeout to poll external cancellation, capped by the
            // remaining deadline so expiry is observed promptly.
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(20));
            let (next, _timeout) = self
                .signal
                .wait_timeout(state, wait)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }

    /// Resolve a parked approval. Returns `true` when a pending request with
    /// `request_id` existed and was resolved.
    pub(crate) fn respond(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        self.respond_with_scope(request_id, decision, None)
    }

    /// Resolve a parked approval with an M1-core scope: `None`/`"single"`
    /// resolves one call; `"group"` additionally records the decision as the
    /// session-sticky verdict for the pending call's group key (run lifetime).
    /// Returns `true` when a pending request existed and was resolved.
    /// The pending entry is kept until `request_approval` consumes it; the
    /// group verdict is stored atomically under the same lock so the runner
    /// cannot wake before the sticky is visible.
    pub(crate) fn respond_with_scope(
        &self,
        request_id: &str,
        decision: ApprovalDecision,
        scope: Option<&str>,
    ) -> bool {
        let mut state = self.lock_state();
        if let Some(pending) = state.pending.as_mut() {
            if pending.id == request_id && pending.decision.is_none() {
                pending.decision = Some(decision);
                if scope == Some("group") {
                    if let Some(group_key) = pending.group_key.clone() {
                        state.group_decisions.insert(group_key, decision);
                    }
                }
                self.signal.notify_all();
                return true;
            }
        }
        false
    }

    /// Session-sticky verdict for `group_key`, if a group-scope resolution
    /// recorded one in this run.
    pub(crate) fn group_decision(&self, group_key: &str) -> Option<ApprovalDecision> {
        self.lock_state().group_decisions.get(group_key).copied()
    }

    /// Pending tool/group metadata for `request_id` (M1-core command bridge).
    pub(crate) fn pending_info(&self, request_id: &str) -> Option<(String, Option<String>)> {
        let state = self.lock_state();
        state.pending.as_ref().and_then(|pending| {
            if pending.id == request_id {
                Some((pending.tool_name.clone(), pending.group_key.clone()))
            } else {
                None
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn has_pending_for(&self, id: &str) -> bool {
        let state = self.lock_state();
        match state.pending.as_ref() {
            Some(pending) => pending.id == id && pending.decision.is_none(),
            None => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn has_any_pending(&self) -> bool {
        self.lock_state().pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
            thought_signature: None,
        }
    }

    fn wait_until_parked(gate: &ApprovalGate, id: &str) {
        let start = Instant::now();
        while !gate.has_pending_for(id) {
            assert!(
                start.elapsed() <= Duration::from_secs(2),
                "timed out waiting for gate to park id={id}"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn risk_classification_covers_all_four_tools() {
        assert_eq!(RiskClass::classify("read_file"), RiskClass::ReadOnly);
        assert_eq!(RiskClass::classify("list_directory"), RiskClass::ReadOnly);
        assert_eq!(RiskClass::classify("write_file"), RiskClass::Mutating);
        // NEX-SEC-001: the shell is its own risk class, never plain Mutating.
        assert_eq!(
            RiskClass::classify("execute_command"),
            RiskClass::DangerousShell
        );
        // Unknown tools are conservative.
        assert_eq!(RiskClass::classify("does_not_exist"), RiskClass::Mutating);
        assert_eq!(RiskClass::classify(""), RiskClass::Mutating);
    }

    #[test]
    fn risk_class_maps_new_tools() {
        assert_eq!(RiskClass::classify("search_files"), RiskClass::ReadOnly);
        assert_eq!(RiskClass::classify("edit_file"), RiskClass::Mutating);
    }

    #[test]
    fn mode_is_readable_and_changeable_at_runtime() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        assert_eq!(gate.mode(), AutonomyMode::Supervised);
        gate.set_mode(AutonomyMode::FullAutonomous);
        assert_eq!(gate.mode(), AutonomyMode::FullAutonomous);
        // Clones share the same mode.
        let other = gate.clone();
        other.set_mode(AutonomyMode::SemiAutonomous);
        assert_eq!(gate.mode(), AutonomyMode::SemiAutonomous);
    }

    #[test]
    fn full_mode_x_risk_matrix_all_six_behaviors() {
        // Supervised: both read and mutating need approval.
        let sup = ApprovalGate::new(AutonomyMode::Supervised);
        assert!(sup.needs_approval(&tool_call("1", "read_file")));
        assert!(sup.needs_approval(&tool_call("2", "list_directory")));
        assert!(sup.needs_approval(&tool_call("3", "write_file")));
        assert!(sup.needs_approval(&tool_call("4", "execute_command")));

        // Semi: reads auto, mutating needs approval.
        let semi = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        assert!(!semi.needs_approval(&tool_call("1", "read_file")));
        assert!(!semi.needs_approval(&tool_call("2", "list_directory")));
        assert!(semi.needs_approval(&tool_call("3", "write_file")));
        assert!(semi.needs_approval(&tool_call("4", "execute_command")));

        // Full: everything auto except the NEX-SEC-001 shell deny-floor.
        let full = ApprovalGate::new(AutonomyMode::FullAutonomous);
        assert!(!full.needs_approval(&tool_call("1", "read_file")));
        assert!(!full.needs_approval(&tool_call("2", "list_directory")));
        assert!(!full.needs_approval(&tool_call("3", "write_file")));
        assert!(full.needs_approval(&tool_call("4", "execute_command")));
    }

    #[test]
    fn shell_deny_floor_mode_x_tool_matrix() {
        // NEX-SEC-001 table: every mode x every tool. The shell
        // (`execute_command`) parks in all three modes; every non-shell
        // outcome is exactly the pre-fix ladder behavior.
        let modes = [
            AutonomyMode::Supervised,
            AutonomyMode::SemiAutonomous,
            AutonomyMode::FullAutonomous,
        ];
        let tools = [
            "read_file",
            "list_directory",
            "search_files",
            "write_file",
            "edit_file",
            "execute_command",
            "does_not_exist",
        ];
        for mode in modes {
            let gate = ApprovalGate::new(mode);
            for tool in tools {
                let needs = gate.needs_approval(&tool_call("t", tool));
                let expected = match mode {
                    AutonomyMode::Supervised => true,
                    AutonomyMode::SemiAutonomous => {
                        !matches!(RiskClass::classify(tool), RiskClass::ReadOnly)
                    }
                    AutonomyMode::FullAutonomous => tool == "execute_command",
                };
                assert_eq!(needs, expected, "mode={mode:?} tool={tool}");
            }
        }
    }

    #[test]
    fn auto_approved_calls_return_immediately_without_parking() {
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        let call = tool_call("auto", "read_file");
        let decision = gate.request_approval(&call).expect("auto approved");
        assert_eq!(decision, ApprovalDecision::Approved);
        assert!(!gate.has_any_pending());
        // Semi read also auto.
        let semi = ApprovalGate::new(AutonomyMode::SemiAutonomous);
        let read = tool_call("r", "list_directory");
        assert_eq!(
            semi.request_approval(&read).expect("auto"),
            ApprovalDecision::Approved
        );
        assert!(!semi.has_any_pending());
    }

    #[test]
    fn parked_approval_resolves_via_respond() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("c1", "read_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call).expect("approved"));
        wait_until_parked(&gate, "c1");
        assert!(gate.respond("c1", ApprovalDecision::Approved));
        let decision = handle.join().expect("thread joins");
        assert_eq!(decision, ApprovalDecision::Approved);
        assert!(!gate.has_any_pending());
    }

    #[test]
    fn denied_decision_is_delivered() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("c2", "write_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call).expect("denied"));
        wait_until_parked(&gate, "c2");
        assert!(gate.respond("c2", ApprovalDecision::Denied));
        let decision = handle.join().expect("join");
        assert_eq!(decision, ApprovalDecision::Denied);
    }

    #[test]
    fn wrong_request_id_does_not_resolve() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("real", "write_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call).expect("approved"));
        wait_until_parked(&gate, "real");
        assert!(!gate.respond("wrong_id", ApprovalDecision::Approved));
        assert!(gate.has_pending_for("real"));
        // Still parked, now respond correctly.
        assert!(gate.respond("real", ApprovalDecision::Approved));
        let decision = handle.join().expect("join");
        assert_eq!(decision, ApprovalDecision::Approved);
    }

    #[test]
    fn cancel_while_parked_aborts_with_err_and_no_deadlock() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("wait", "execute_command");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call));
        wait_until_parked(&gate, "wait");
        gate.cancel();
        let res = handle.join().expect("join");
        assert!(res.is_err(), "cancellation must abort with Err");
        // Gate is reusable after cancellation: next auto call still works if mode allows.
        gate.set_mode(AutonomyMode::FullAutonomous);
        let auto_call = tool_call("next", "write_file");
        assert_eq!(
            gate.request_approval(&auto_call)
                .expect("auto after cancel"),
            ApprovalDecision::Approved
        );
    }

    #[test]
    fn park_deadline_expires_without_respond_and_clears_pending() {
        // NEX-AGENT-004: an unanswered park must terminate on its own. The
        // short deadline is a promptness bound, not a wall-clock margin —
        // the test asserts termination + outcome + cleanup, never `elapsed`.
        let gate =
            ApprovalGate::new(AutonomyMode::Supervised).with_timeout(Duration::from_millis(100));
        assert_eq!(gate.timeout(), Duration::from_millis(100));
        let call = tool_call("expire", "write_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.park_until_resolved(&call));
        wait_until_parked(&gate, "expire");
        let res = handle.join().expect("parked thread terminates on expiry");
        assert_eq!(res, Err(ParkEnd::TimedOut));
        assert!(
            !gate.has_any_pending(),
            "expired park clears pending so a late respond resolves nothing"
        );
        assert!(
            !gate.respond("expire", ApprovalDecision::Approved),
            "late respond after expiry must not resolve"
        );
        // A decision that arrives before the deadline still wins.
        let gate = ApprovalGate::new(AutonomyMode::Supervised).with_timeout(Duration::from_mins(1));
        let call = tool_call("early", "write_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.park_until_resolved(&call));
        wait_until_parked(&gate, "early");
        assert!(gate.respond("early", ApprovalDecision::Denied));
        assert_eq!(handle.join().expect("join"), Ok(ApprovalDecision::Denied));
    }

    #[test]
    fn set_timeout_is_shared_by_clones_and_covers_forced_parks() {
        // The deadline lives in the shared state: every clone — and the
        // ladder-bypassing forced park — observes the same timeout.
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        gate.set_timeout(Duration::from_millis(100));
        assert_eq!(gate.clone().timeout(), Duration::from_millis(100));
        let call = tool_call("forced-expire", "read_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.park_until_resolved(&call));
        wait_until_parked(&gate, "forced-expire");
        assert_eq!(handle.join().expect("join"), Err(ParkEnd::TimedOut));
    }

    #[test]
    fn shared_cancellation_token_wiring_wakes_parked_wait() {
        // Simulate RunControl sharing: gate shares the run's token.
        let token = CancellationToken::new();
        let gate = ApprovalGate::with_token(AutonomyMode::Supervised, token.clone());
        let call = tool_call("shared", "read_file");
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call));
        wait_until_parked(&gate, "shared");
        // Cancelling via the shared token (as RunControl::cancel does) must wake.
        token.cancel();
        let res = handle.join().expect("join");
        assert!(res.is_err());
    }

    #[test]
    fn set_token_sharing_affects_all_clones() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate_clone = gate.clone();
        let token = CancellationToken::new();
        gate.set_token(token.clone());
        // Both clones see the same cancellation.
        let call = tool_call("x", "write_file");
        let gate2 = gate_clone.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call));
        wait_until_parked(&gate, "x");
        token.cancel();
        let res = handle.join().expect("join");
        assert!(res.is_err());
        assert!(gate.is_cancelled());
        assert!(gate_clone.is_cancelled());
    }

    #[test]
    fn runtime_mode_switch_changes_next_decision() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let read = tool_call("r1", "read_file");
        assert!(gate.needs_approval(&read));
        gate.set_mode(AutonomyMode::FullAutonomous);
        assert!(!gate.needs_approval(&read));
        gate.set_mode(AutonomyMode::SemiAutonomous);
        assert!(!gate.needs_approval(&read));
        assert!(gate.needs_approval(&tool_call("w1", "write_file")));
        // Already-parked request is not auto-resolved by mode switch.
        let gate2 = gate.clone();
        gate.set_mode(AutonomyMode::Supervised);
        let call = tool_call("park", "write_file");
        let handle = thread::spawn(move || gate2.request_approval(&call).expect("approved"));
        wait_until_parked(&gate, "park");
        // Switch mode while parked: should not resolve pending.
        gate.set_mode(AutonomyMode::FullAutonomous);
        assert!(!handle.is_finished(), "mode switch must not auto-resolve");
        assert!(gate.has_pending_for("park"));
        assert!(gate.respond("park", ApprovalDecision::Approved));
        let decision = handle.join().expect("join");
        assert_eq!(decision, ApprovalDecision::Approved);
    }

    #[test]
    fn cheap_clone_handle_shares_state() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let other = gate.clone();
        other.set_mode(AutonomyMode::FullAutonomous);
        assert_eq!(gate.mode(), AutonomyMode::FullAutonomous);
        // Respond via clone resolves wait on original.
        let call = tool_call("c", "write_file");
        gate.set_mode(AutonomyMode::Supervised);
        let g2 = gate.clone();
        let h = thread::spawn(move || g2.request_approval(&call).expect("ok"));
        wait_until_parked(&gate, "c");
        assert!(other.respond("c", ApprovalDecision::Denied));
        assert_eq!(h.join().unwrap(), ApprovalDecision::Denied);
    }

    #[test]
    fn gate_is_poison_safe() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let gate2 = gate.clone();
        // Poison the mutex by panicking while holding it.
        let _ = thread::spawn(move || {
            let _guard = gate2.state.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        // Next operation must recover, not deadlock.
        gate.set_mode(AutonomyMode::FullAutonomous);
        assert_eq!(gate.mode(), AutonomyMode::FullAutonomous);
        let call = tool_call("p", "read_file");
        // Should still auto-approve despite poisoning.
        assert_eq!(
            gate.request_approval(&call).expect("recovered"),
            ApprovalDecision::Approved
        );
    }

    #[test]
    fn prepare_pending_early_resolve_is_consumed_without_parking() {
        // Guard: if request_approval clobbers the early decision, this test
        // must fail (it would park forever instead of returning immediately).
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("early-1", "write_file");
        gate.prepare_pending(&call);
        assert!(gate.has_pending_for("early-1"));
        assert!(gate.respond("early-1", ApprovalDecision::Approved));
        // Decision is now set; has_pending_for is false but has_any_pending true (decision Some).
        assert!(!gate.has_pending_for("early-1"));
        assert!(gate.has_any_pending());
        // request_approval must consume the early decision without parking.
        // Use a channel with timeout to detect a clobber-bug that would park.
        let gate2 = gate.clone();
        let call2 = call.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = gate2.request_approval(&call2);
            let _ = tx.send(res);
        });
        let res = rx.recv_timeout(Duration::from_secs(1)).expect(
            "request_approval must return immediately — clobbered early decision would park",
        );
        let decision = res.expect("approved");
        assert_eq!(decision, ApprovalDecision::Approved);
        assert!(!gate.has_any_pending(), "pending cleared after consumption");
    }

    #[test]
    fn prepare_pending_normal_park_and_resolve_still_works() {
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("normal-1", "write_file");
        gate.prepare_pending(&call);
        assert!(gate.has_pending_for("normal-1"));
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.request_approval(&call).expect("approved"));
        wait_until_parked(&gate, "normal-1");
        assert!(!handle.is_finished(), "should be parked");
        assert!(gate.respond("normal-1", ApprovalDecision::Approved));
        let decision = handle.join().expect("join");
        assert_eq!(decision, ApprovalDecision::Approved);
        assert!(!gate.has_any_pending());
    }

    #[test]
    fn fast_path_clears_stale_prepared_pending() {
        // Hotfix-review backlog cleanup (Task 6.2): prepare → switch to
        // FullAutonomous → request_approval must auto-approve AND the stale
        // pre-registered pending must be gone afterwards. Observable via a
        // subsequent respond(same_id) returning false.
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let call = tool_call("stale-1", "write_file");
        gate.prepare_pending(&call);
        assert!(gate.has_pending_for("stale-1"));
        gate.set_mode(AutonomyMode::FullAutonomous);
        let decision = gate.request_approval(&call).expect("auto-approved");
        assert_eq!(decision, ApprovalDecision::Approved);
        assert!(
            !gate.has_any_pending(),
            "stale prepared pending must be cleared by the fast-path"
        );
        assert!(
            !gate.respond("stale-1", ApprovalDecision::Denied),
            "a late respond for the stale id must not resolve anything"
        );
    }

    #[test]
    fn park_until_resolved_parks_even_when_ladder_auto_approves() {
        // WS-C.2 injection hold: `park_until_resolved` reuses the
        // pending/response machinery but skips the ladder fast-path, so a
        // FullAutonomous gate (which `request_approval` would auto-approve)
        // still parks until `respond`.
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        let call = tool_call("forced-1", "read_file");
        // The ladder itself auto-approves this call ...
        assert_eq!(
            gate.request_approval(&call).expect("ladder auto"),
            ApprovalDecision::Approved
        );
        // ... but the forced park blocks for a real decision.
        let gate2 = gate.clone();
        let handle = thread::spawn(move || gate2.park_until_resolved(&call).expect("resolved"));
        wait_until_parked(&gate, "forced-1");
        assert!(!handle.is_finished(), "forced park must not auto-resolve");
        assert!(gate.respond("forced-1", ApprovalDecision::Denied));
        let decision = handle.join().expect("join");
        assert_eq!(decision, ApprovalDecision::Denied);
        assert!(!gate.has_any_pending());
    }

    #[test]
    fn expired_deadline_with_shutdown_flag_names_shutdown_not_timeout() {
        // NEX-TASK-002 race pin (#181): when the shutdown origin is set, an
        // already-expired deadline must end as Shutdown, never TimedOut —
        // through both the loop-top cancel check and the deadline-expiry
        // branch. Zero timeout = expired at park entry; single-threaded, so
        // no scheduling dependence. Outcome asserts only, never `elapsed`.
        let token = CancellationToken::new();
        token.shutdown();
        let gate =
            ApprovalGate::with_token(AutonomyMode::Supervised, token).with_timeout(Duration::ZERO);
        let call = tool_call("shutdown-expired", "write_file");
        assert_eq!(gate.park_until_resolved(&call), Err(ParkEnd::Shutdown));
        assert!(
            !gate.has_any_pending(),
            "shutdown expiry clears pending like every other park end"
        );
        // Non-shutdown paths are unchanged: plain expiry is still a timeout.
        let plain = ApprovalGate::new(AutonomyMode::Supervised).with_timeout(Duration::ZERO);
        let call = tool_call("plain-expired", "write_file");
        assert_eq!(plain.park_until_resolved(&call), Err(ParkEnd::TimedOut));
        // A user cancel (no shutdown origin) is still a cancel, not a timeout.
        let cancelled_token = CancellationToken::new();
        cancelled_token.cancel();
        let cancelled = ApprovalGate::with_token(AutonomyMode::Supervised, cancelled_token)
            .with_timeout(Duration::ZERO);
        let call = tool_call("cancelled-expired", "write_file");
        assert_eq!(
            cancelled.park_until_resolved(&call),
            Err(ParkEnd::Cancelled)
        );
    }
}
