//! Active-run registry (managed Tauri state): tracks in-flight agent runs and
//! enforces DP-4 (at most one active run per conversation). Moved verbatim
//! from [`super::service`] (S3 split); the bridge re-exports the names so
//! existing `service::` paths keep resolving.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
use super::control::RunControl;

// ---------------------------------------------------------------------------
// Active-run registry (managed Tauri state)
// ---------------------------------------------------------------------------

/// One active run's user-controllable handles. Cheap clones over the same
/// underlying state as the handles the run thread attached to its runner:
/// `cancel` wakes every parked wait through the shared cancellation token,
/// and `gate.respond` resolves an approval park.
#[derive(Debug, Clone)]
pub(crate) struct ActiveAgentRun {
    /// The conversation this run belongs to (DP-4 uniqueness key).
    pub(crate) conversation_id: i64,
    /// Cancel/extend handle.
    pub(crate) control: RunControl,
    /// Approval gate handle (`respond` resolves a park).
    pub(crate) gate: ApprovalGate,
}

/// Active-run registry: managed Tauri state mapping `run_id` to the handles
/// of the in-flight run, plus the per-conversation claim set that enforces
/// DP-4 synchronously (a second `start` for the same conversation is
/// rejected even before the run thread registers its entry).
///
/// NEX-TASK-002: the registry additionally retains the run + forwarder
/// thread [`JoinHandle`](std::thread::JoinHandle)s so orderly shutdown can
/// cancel every active run and join its threads with a bounded deadline
/// instead of dropping the handles. Handles are *not* removed by
/// [`Self::release`]: the run thread calls `release` on its own exit path
/// and must never take its own handle (shutdown would lose it). Finished
/// handles are reaped (joined + removed) by [`Self::reap_finished`], called
/// on every [`Self::track_threads`] and by the shutdown join.
#[derive(Debug, Default)]
pub(crate) struct AgentRunRegistry {
    runs: Mutex<HashMap<i64, ActiveAgentRun>>,
    claimed_conversations: Mutex<HashSet<i64>>,
    threads: Mutex<HashMap<i64, Vec<std::thread::JoinHandle<()>>>>,
}

/// Outcome of a registry approval resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveOutcome {
    /// The pending approval was resolved.
    Resolved,
    /// The run is not (or no longer) active.
    RunNotActive,
    /// The run is active but has no pending approval for that `call_id`.
    NoPendingApproval,
}

impl AgentRunRegistry {
    /// Synchronously claim a conversation slot (DP-4). Returns `false` when
    /// the conversation already has an active or starting run.
    pub(crate) fn claim_conversation(&self, conversation_id: i64) -> bool {
        self.claimed_conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(conversation_id)
    }

    /// Release a conversation claim (setup failure path).
    pub(crate) fn unclaim_conversation(&self, conversation_id: i64) {
        self.claimed_conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&conversation_id);
    }

    /// Register a started run under its id. The conversation claim must
    /// already be held (taken by [`super::service::start_run`]).
    pub(crate) fn register(&self, run_id: i64, entry: ActiveAgentRun) {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run_id, entry);
    }

    /// Release a terminated run: drop its handles and unclaim its
    /// conversation. Called by the run thread on every exit path, so a
    /// finished run is immediately reusable for a new run in the same
    /// conversation.
    ///
    /// NEX-TASK-002: the retained thread handles are deliberately *not*
    /// removed here (see the struct docs); [`Self::reap_finished`] and
    /// [`Self::join_threads_until`] own them.
    pub(crate) fn release(&self, run_id: i64) {
        let entry = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&run_id);
        if let Some(entry) = entry {
            self.unclaim_conversation(entry.conversation_id);
        }
    }

    /// Cancel a run (DP-3: works from *every* state — running, approval-
    /// parked, or budget-parked — because `RunControl::cancel` wakes all
    /// parked waits through the shared cancellation token).
    ///
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn cancel(&self, run_id: i64) -> bool {
        // Same shape as `shutdown_cancel` below: clone the entry, drop the
        // runs-map guard, then signal outside the critical section so the
        // map is never held across `notify_all`. Note that `RunControl::cancel`
        // takes the *governor* mutex internally — that is its own critical
        // section, unrelated to this map lock, and dropping the map guard
        // first is what keeps the two locks from nesting. Do not "simplify"
        // `RunControl::cancel` back to a bare atomic store: notifying without
        // the governor mutex loses the wakeup when it lands between a waiter's
        // park predicate and its condvar enqueue, wedging the run forever
        // (#256).
        let entry = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
            .cloned();
        match entry {
            Some(entry) => {
                entry.control.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancel a run by application shutdown (NEX-TASK-002). Wakes every
    /// parked wait exactly like [`Self::cancel`], and additionally marks the
    /// shutdown origin on the shared token so a parked approval records the
    /// honest shutdown vocabulary instead of the user-cancel text.
    ///
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn shutdown_cancel(&self, run_id: i64) -> bool {
        // Scope the map lock to the lookup: clone the entry, then drop
        // the guard before the control wake so the map is never held
        // across `notify_all`.
        let entry = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
            .cloned();
        match entry {
            Some(entry) => {
                entry.control.shutdown();
                true
            }
            None => false,
        }
    }

    /// Snapshot the ids of all currently active runs (orderly-shutdown
    /// seam: cancel-then-join iterates a snapshot so per-run completion
    /// during the sweep cannot disturb the iteration).
    #[must_use]
    pub(crate) fn active_run_ids(&self) -> Vec<i64> {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    /// Cancel every active run by application shutdown (NEX-TASK-002; see
    /// [`Self::shutdown_cancel`]). Returns how many runs were cancelled.
    pub(crate) fn shutdown_cancel_all(&self) -> usize {
        self.active_run_ids()
            .iter()
            .filter(|run_id| self.shutdown_cancel(**run_id))
            .count()
    }

    /// Retain the spawned thread handles for `run_id` (NEX-TASK-002): the
    /// run thread plus the forwarder thread. Finished handles of *other*
    /// runs are reaped first so the map stays bounded by the live set.
    pub(crate) fn track_threads(&self, run_id: i64, handles: Vec<std::thread::JoinHandle<()>>) {
        self.reap_finished();
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run_id, handles);
    }

    /// Join and remove every already-finished retained handle (non-blocking).
    /// Returns how many handles are still live.
    pub(crate) fn reap_finished(&self) -> usize {
        let mut threads = self
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let finished: Vec<i64> = threads
            .iter()
            .filter_map(|(run_id, handles)| {
                handles
                    .iter()
                    .all(std::thread::JoinHandle::is_finished)
                    .then_some(*run_id)
            })
            .collect();
        for run_id in finished {
            if let Some(handles) = threads.remove(&run_id) {
                for handle in handles {
                    if handle.join().is_err() {
                        // A panicked run thread: its registry entry was never
                        // released and its row stays `running` — the startup
                        // sweep (crash fallback) reconciles it. Never panic
                        // the reaper for a dead thread.
                        log::warn!("run {run_id}: retained thread panicked");
                    }
                }
            }
        }
        threads.values().map(Vec::len).sum()
    }

    /// Join retained thread handles until none remain or `deadline` passes
    /// (NEX-TASK-002 bounded quit: never hangs the shutdown path). Returns
    /// how many handles are still unjoined past the deadline; the process
    /// exit reclaims those threads.
    pub(crate) fn join_threads_until(&self, deadline: std::time::Instant) -> usize {
        loop {
            let live = self.reap_finished();
            if live == 0 || std::time::Instant::now() >= deadline {
                return live;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// How many thread handles are currently retained (test seam).
    #[must_use]
    pub(crate) fn tracked_thread_count(&self) -> usize {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(Vec::len)
            .sum()
    }

    /// Resolve a parked approval. See [`ResolveOutcome`] for the outcomes.
    #[must_use]
    pub(crate) fn resolve(&self, run_id: i64, call_id: &str, approved: bool) -> ResolveOutcome {
        self.resolve_with_scope(run_id, call_id, approved, None).0
    }

    /// Resolve with M1-core scope (`None`/`"single"` = one call;
    /// `"group"` = rest of the group, session-sticky for the run).
    /// Returns the outcome plus pending `(tool_name, group_key)` when resolved.
    #[must_use]
    pub(crate) fn resolve_with_scope(
        &self,
        run_id: i64,
        call_id: &str,
        approved: bool,
        scope: Option<&str>,
    ) -> (ResolveOutcome, Option<(String, Option<String>)>) {
        let runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = runs.get(&run_id) else {
            return (ResolveOutcome::RunNotActive, None);
        };
        let decision = if approved {
            ApprovalDecision::Approved
        } else {
            ApprovalDecision::Denied
        };
        let scope_group = matches!(scope, Some("group"));
        // Capture pending metadata before the decision clears it.
        let pending = entry.gate.pending_info(call_id);
        if entry.gate.respond_with_scope(call_id, decision, scope) {
            // `respond_with_scope` already recorded the sticky verdict when
            // `scope == "group"`; nothing further to do here.
            let _ = scope_group;
            (ResolveOutcome::Resolved, pending)
        } else {
            (ResolveOutcome::NoPendingApproval, None)
        }
    }

    /// Read-only peek at a pending approval's `(tool_name, group_key)` without
    /// resolving it (NEX-AGENT-023 scope confirmation): the confirm echo is
    /// validated against this snapshot *before* the park is consumed, so a
    /// mismatch fails closed with the park intact. Returns `None` when the
    /// run is not active or has no such pending approval.
    #[must_use]
    pub(crate) fn pending_scope_info(
        &self,
        run_id: i64,
        call_id: &str,
    ) -> Option<(String, Option<String>)> {
        let runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = runs.get(&run_id)?;
        entry.gate.pending_info(call_id)
    }

    /// Grant additional iterations to a budget-parked (or running) run.
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn extend(&self, run_id: i64, extra_steps: usize) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.control.extend_steps(extra_steps);
                true
            }
            None => false,
        }
    }

    /// Whether the run's worker thread is currently parked at the exhausted
    /// step budget (awaiting `extend_steps` or `cancel`). Returns `false`
    /// when the run is not active. Polled by the task loop's terminal-wait
    /// so a budget-parked step run — whose persisted row stays `'running'`
    /// until extended or cancelled — is surfaced instead of polled forever.
    #[must_use]
    pub(crate) fn is_budget_parked(&self, run_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
            .is_some_and(|entry| entry.control.is_budget_parked())
    }

    /// Change the autonomy mode of an active run (Task 5.2, DP-AUTONOMY).
    /// A parked approval is never auto-resolved by a mode switch.
    /// Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn set_mode(&self, run_id: i64, mode: AutonomyMode) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.gate.set_mode(mode);
                true
            }
            None => false,
        }
    }

    /// Pause an active run (Task 5.2, DP-PAUSE). Takes effect at the next
    /// step boundary. Returns `false` when the run is not active.
    #[must_use]
    pub(crate) fn pause(&self, run_id: i64) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.control.pause();
                true
            }
            None => false,
        }
    }

    /// Resume a paused run (Task 5.2, DP-PAUSE). Returns `false` when the run
    /// is not active.
    #[must_use]
    pub(crate) fn resume(&self, run_id: i64) -> bool {
        match self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&run_id)
        {
            Some(entry) => {
                entry.control.resume();
                true
            }
            None => false,
        }
    }

    /// Whether a run is currently registered (test seam).
    #[cfg(test)]
    pub(crate) fn is_active(&self, run_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&run_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn registry_set_mode_pause_resume_happy_and_unknown() {
        let registry = AgentRunRegistry::default();
        // Unknown runs -> false / NotActive
        assert!(!registry.set_mode(
            9999,
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        ));
        assert!(!registry.pause(9999));
        assert!(!registry.resume(9999));
        assert_eq!(
            registry.resolve(9999, "any", true),
            ResolveOutcome::RunNotActive
        );
        assert!(!registry.extend(9999, 1));
        // Manual registration for happy path (parallel-safe, no threads)
        let reg = AgentRunRegistry::default();
        let gate = crate::application::agent::approval::ApprovalGate::new(
            crate::application::agent::approval::AutonomyMode::Supervised,
        );
        let control = crate::application::agent::control::RunControl::new();
        reg.register(
            42,
            ActiveAgentRun {
                conversation_id: 1,
                control: control.clone(),
                gate: gate.clone(),
            },
        );
        assert!(reg.is_active(42));
        // set_mode
        assert!(reg.set_mode(
            42,
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        ));
        assert_eq!(
            gate.mode(),
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        );
        // pause/resume
        assert!(reg.pause(42));
        assert!(control.pause_pending());
        assert!(reg.resume(42));
        assert!(!control.pause_pending());
    }

    #[test]
    fn registry_set_mode_does_not_resolve_parked_approval() {
        let gate = crate::application::agent::approval::ApprovalGate::new(
            crate::application::agent::approval::AutonomyMode::Supervised,
        );
        let control = crate::application::agent::control::RunControl::new();
        let reg = AgentRunRegistry::default();
        reg.register(
            101,
            ActiveAgentRun {
                conversation_id: 1,
                control: control.clone(),
                gate: gate.clone(),
            },
        );
        // Park an approval in a thread
        let call = crate::application::execution::ToolCall {
            id: "parked-1".to_string(),
            name: "write_file".to_string(),
            arguments: "{}".to_string(),
            thought_signature: None,
        };
        let gate2 = gate.clone();
        let handle = std::thread::spawn(move || gate2.request_approval(&call));
        // Wait until parked
        let start = std::time::Instant::now();
        while !gate.has_pending_for("parked-1") {
            assert!(start.elapsed() < Duration::from_secs(2), "not parked");
            std::thread::yield_now();
        }
        // Switch mode while parked: must not auto-resolve
        assert!(reg.set_mode(
            101,
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        ));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!handle.is_finished(), "mode switch must not auto-resolve");
        assert!(gate.has_pending_for("parked-1"));
        // Now resolve normally
        assert_eq!(reg.resolve(101, "parked-1", true), ResolveOutcome::Resolved);
        let decision = handle.join().expect("join").expect("approved");
        assert_eq!(
            decision,
            crate::application::agent::approval::ApprovalDecision::Approved
        );
        // Resolve again should be NoPendingApproval
        assert_eq!(
            reg.resolve(101, "parked-1", true),
            ResolveOutcome::NoPendingApproval
        );
    }

    #[test]
    fn pending_scope_info_peeks_without_consuming() {
        // NEX-AGENT-023: the scope-confirmation peek observes the pending
        // tool/group metadata without resolving the park.
        let gate = crate::application::agent::approval::ApprovalGate::new(
            crate::application::agent::approval::AutonomyMode::Supervised,
        );
        let control = crate::application::agent::control::RunControl::new();
        let reg = AgentRunRegistry::default();
        reg.register(
            77,
            ActiveAgentRun {
                conversation_id: 1,
                control,
                gate: gate.clone(),
            },
        );
        assert_eq!(reg.pending_scope_info(9999, "nope"), None);
        assert_eq!(reg.pending_scope_info(77, "nope"), None);
        let call = crate::application::execution::ToolCall {
            id: "scoped-1".to_string(),
            name: "write_file".to_string(),
            arguments: "{}".to_string(),
            thought_signature: None,
        };
        gate.prepare_pending_with_group(&call, Some("coding:write_file:docs".to_string()));
        assert_eq!(
            reg.pending_scope_info(77, "scoped-1"),
            Some((
                "write_file".to_string(),
                Some("coding:write_file:docs".to_string())
            ))
        );
        // The park is untouched: the pending entry (with no decision yet)
        // survives the peek.
        assert!(
            gate.has_pending_for("scoped-1"),
            "peek must not consume the park"
        );
    }

    #[test]
    fn shutdown_cancel_marks_shutdown_origin() {
        // #181 behavior-preservation pin: `shutdown_cancel` marks the
        // shutdown origin on the shared token, unlike plain `cancel` which
        // leaves the origin clear. Scope note: the narrowed lock scope
        // itself (lookup-clone-drop-then-signal) is not observable from
        // these outcome asserts — it is covered by inspection — so this
        // test pins only the observable origin behavior. Outcome asserts
        // only, never `elapsed`.
        let reg = AgentRunRegistry::default();
        assert!(!reg.shutdown_cancel(9999));
        let gate = crate::application::agent::approval::ApprovalGate::new(
            crate::application::agent::approval::AutonomyMode::Supervised,
        );
        let control = crate::application::agent::control::RunControl::new();
        reg.register(
            42,
            ActiveAgentRun {
                conversation_id: 1,
                control: control.clone(),
                gate: gate.clone(),
            },
        );
        assert!(reg.shutdown_cancel(42));
        assert!(control.is_cancelled());
        assert!(
            control.token().is_shutdown(),
            "shutdown_cancel must mark the shutdown origin"
        );
        // Idempotent while the run stays registered.
        assert!(reg.shutdown_cancel(42));
        // Plain cancel marks cancelled but never the shutdown origin.
        let control2 = crate::application::agent::control::RunControl::new();
        reg.register(
            43,
            ActiveAgentRun {
                conversation_id: 2,
                control: control2.clone(),
                gate,
            },
        );
        assert!(reg.cancel(43));
        assert!(control2.is_cancelled());
        assert!(
            !control2.token().is_shutdown(),
            "plain cancel must not mark the shutdown origin"
        );
    }
}
