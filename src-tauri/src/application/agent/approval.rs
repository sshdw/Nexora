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
use std::time::Duration;

use crate::application::agent::control::CancellationToken;
use crate::application::execution::ToolCall;

// ---------------------------------------------------------------------------
// Risk classification and autonomy ladder
// ---------------------------------------------------------------------------

/// Risk class of a workspace tool, conservative by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskClass {
    /// `read_file`, `list_directory`.
    ReadOnly,
    /// `write_file`, `execute_command`, and any unknown tool. Every shell
    /// command counts as mutating because the shell cannot be statically
    /// classified.
    Mutating,
}

impl RiskClass {
    /// Classify a tool by name.
    #[must_use]
    pub(crate) fn classify(name: &str) -> Self {
        match name {
            "read_file" | "list_directory" => Self::ReadOnly,
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

// ---------------------------------------------------------------------------
// Gate state
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PendingApproval {
    id: String,
    decision: Option<ApprovalDecision>,
}

#[derive(Debug)]
struct GateState {
    mode: AutonomyMode,
    pending: Option<PendingApproval>,
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
            })),
            signal: Arc::new(Condvar::new()),
        }
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

    /// Cancel the shared token and wake every waiter parked in
    /// `request_approval`. Never blocks.
    pub(crate) fn cancel(&self) {
        self.lock_token().cancel();
        self.signal.notify_all();
    }

    /// Whether `call` would require user approval under the current mode and
    /// risk class. Does not park.
    #[must_use]
    pub(crate) fn needs_approval(&self, call: &ToolCall) -> bool {
        let risk = RiskClass::classify(&call.name);
        let mode = self.mode();
        match mode {
            AutonomyMode::Supervised => true,
            AutonomyMode::SemiAutonomous => matches!(risk, RiskClass::Mutating),
            AutonomyMode::FullAutonomous => false,
        }
    }

    /// Decide for `call` per the autonomy ladder, or park until
    /// `respond(request_id, decision)` or cancellation. Returns
    /// `Err(())` when cancellation aborted the wait (the runner maps this to
    /// `AgentError::Cancelled`).
    pub(crate) fn request_approval(&self, call: &ToolCall) -> Result<ApprovalDecision, ()> {
        // Fast-path: auto-approved without ever creating a pending entry.
        if !self.needs_approval(call) {
            return Ok(ApprovalDecision::Approved);
        }

        // Create pending entry.
        {
            let mut state = self.lock_state();
            // If a previous pending somehow remains (should not happen in the
            // sequential per-tool-call runner loop), replace it conservatively.
            state.pending = Some(PendingApproval {
                id: call.id.clone(),
                decision: None,
            });
        }

        // Park until a decision arrives or cancellation fires. Use a short
        // timeout so a cancellation that only sets the shared token (without
        // notifying *this* gate's condvar, e.g. via `RunControl::cancel`)
        // still wakes promptly: the loop re-checks `is_cancelled` each
        // timeout. This extends the Task 3.2 no-deadlock guarantee to
        // approval waits even when only the token is shared.
        let mut state = self.lock_state();
        loop {
            if self.is_cancelled() {
                state.pending = None;
                return Err(());
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
                    return Err(());
                }
            }
            // Wait with timeout to poll external cancellation.
            let (next, _timeout) = self
                .signal
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }

    /// Resolve a parked approval. Returns `true` when a pending request with
    /// `request_id` existed and was resolved.
    pub(crate) fn respond(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let mut state = self.lock_state();
        if let Some(pending) = state.pending.as_mut() {
            if pending.id == request_id && pending.decision.is_none() {
                pending.decision = Some(decision);
                self.signal.notify_all();
                return true;
            }
        }
        false
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
        assert_eq!(RiskClass::classify("execute_command"), RiskClass::Mutating);
        // Unknown tools are conservative.
        assert_eq!(RiskClass::classify("does_not_exist"), RiskClass::Mutating);
        assert_eq!(RiskClass::classify(""), RiskClass::Mutating);
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

        // Full: everything auto.
        let full = ApprovalGate::new(AutonomyMode::FullAutonomous);
        assert!(!full.needs_approval(&tool_call("1", "read_file")));
        assert!(!full.needs_approval(&tool_call("2", "list_directory")));
        assert!(!full.needs_approval(&tool_call("3", "write_file")));
        assert!(!full.needs_approval(&tool_call("4", "execute_command")));
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
}
