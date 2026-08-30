//! Step governor and cancellation controller (ROADMAP.md Phase 3 — Task 3.2).
//!
//! This module provides the user-controllable governance surface attached to
//! [`crate::application::agent::runner::AgentRunner`]:
//!
//! - [`CancellationToken`]: a cheaply cloneable, std-only cooperative
//!   cancellation flag shared all the way down into long-running tool
//!   executions (`ToolRegistry::execute_with_cancellation`).
//! - [`RunControl`]: a cheap cloneable handle exposing `cancel`, `pause`,
//!   `resume`, and `extend_steps`. Internally `Arc<Mutex<…>> + Condvar` —
//!   deliberately std-only, no tokio — so `cancel` reliably wakes a loop
//!   parked at a pause or budget boundary (no deadlock).
//!
//! Governance semantics implemented by the runner on top of these handles:
//!
//! - One step is accounted per LLM turn against the configured maximum.
//! - Exhaustion at a step boundary parks the loop until `extend_steps(n)`
//!   continues it or `cancel()` aborts it; `resume()` alone grants no steps.
//! - `pause()` takes effect at the next step boundary and is lifted by
//!   `resume()` or terminated by `cancel()`.
//!
//! [`AgentRunEvent`] values are delivered through an optional
//! `std::sync::mpsc::Sender` wired to the Tauri layer in Milestone 5.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use serde::Serialize;

// ---------------------------------------------------------------------------
// Cancellation token
// ---------------------------------------------------------------------------

/// Cooperative cancellation flag shared across the run stack.
///
/// `std`-only (`Arc<AtomicBool>`), cheap to clone, polled by the runner
/// around every provider call, between tool dispatches, and inside the
/// command-execution wait loop so a user cancellation reaches running tool
/// processes promptly.
#[derive(Debug, Clone)]
pub(crate) struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Create a token in the not-cancelled state.
    pub(crate) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the token cancelled. Never blocks.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Governance events
// ---------------------------------------------------------------------------

/// Milestone-governance event emitted over the optional run-event channel.
///
/// Delivery is best-effort: a receiver that stopped draining the channel
/// never blocks or aborts the run. Task 5.1 extends this enum with
/// [`AgentRunEvent::StepRecorded`], emitted by the run recorder (not the
/// runner) after each *successfully persisted* `agent_steps` row, so live
/// events and persisted steps share one code path and one total order.
///
/// Serialization is internal-tagged (`{"kind": "...", ...field values}`)
/// with `snake_case` variant names; the Task 5.1 bridge wraps events in the
/// `agent-run-event` Tauri frame alongside `run_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentRunEvent {
    /// The run parked at a step boundary because the user paused it.
    Paused,
    /// The user resumed a paused run.
    Resumed,
    /// The configured step budget was exhausted at a step boundary; the run
    /// parks until `extend_steps` (continue) or `cancel` (abort).
    BudgetExhausted {
        /// Effective step allowance at the moment of exhaustion.
        max_steps: usize,
    },
    /// The spend guard tripped: billed spend exceeded the configured limit
    /// (Task 4.3). `spent_micro` includes the tripping turn's cost.
    SpendLimitExceeded {
        /// Billed spend at the moment of the trip, micro-USD.
        spent_micro: u64,
        /// Configured per-run limit, micro-USD.
        limit_micro: u64,
    },
    /// A tool call is awaiting user approval (Task 4.1).
    ApprovalRequested {
        /// Provider-assigned identifier for the pending tool call.
        call_id: String,
        /// Tool name.
        name: String,
        /// Raw JSON arguments for the call.
        arguments: String,
    },
    /// A parked approval was resolved (Task 4.1).
    ApprovalResolved {
        /// Provider-assigned identifier for the resolved call.
        call_id: String,
        /// Whether the user approved the call.
        approved: bool,
    },
    /// A user cancellation was observed; the run aborted with
    /// `AgentError::Cancelled`.
    Cancelled,
    /// The run produced a final answer after `steps` accounted model turns.
    Completed { steps: usize },
    /// One successfully persisted agent step, streamed to the UI (Task 5.1).
    /// Emitted by the recorder immediately after the `agent_steps` insert
    /// succeeds; `seq` is exactly the persisted value (CF-01-aligned: a
    /// failed insert emits nothing and its `seq` is reused by the retry).
    /// Emitted on the run thread, so governance and step events on the same
    /// channel are totally ordered by emission.
    StepRecorded {
        /// The run this step belongs to.
        run_id: i64,
        /// 1-based sequence within the run, identical to `agent_steps.seq`.
        seq: i64,
        /// `'model_turn' | 'tool_call' | 'approval'` (schema CHECK values).
        kind: String,
        /// Tool name; `None` for `model_turn`.
        tool_name: Option<String>,
        /// Raw JSON arguments exactly as provider-supplied.
        arguments: Option<String>,
        /// Model-turn content / tool output / denial or approval text.
        observation: Option<String>,
        /// `'succeeded' | 'failed' | 'denied' | 'cancelled'` (tool/approval
        /// only).
        status: Option<String>,
        /// Step duration in milliseconds, when known.
        duration_ms: Option<i64>,
    },
}

// ---------------------------------------------------------------------------
// Run control handle
// ---------------------------------------------------------------------------

/// Mutable governor state shared by every [`RunControl`] clone.
///
/// Wrapped whole (never primitive-per-field) inside one mutex so pause and
/// budget decisions observe a consistent snapshot.
#[derive(Debug, Default)]
struct GovernorState {
    /// Set by `pause`, cleared by `resume`.
    paused: bool,
    /// Cumulative extra steps granted via `extend_steps`.
    extra_steps: usize,
}

/// User-facing governance handle attached to an
/// [`AgentRunner`](super::runner::AgentRunner) via `.with_control(...)`.
///
/// Cloning is cheap: every clone governs the same underlying run state. All
/// methods are non-blocking except the waiters consumed by the runner
/// ([`Self::wait_while_paused`], [`Self::wait_for_allowance`]), both of
/// which wake promptly on [`Self::cancel`].
#[derive(Debug, Clone)]
pub(crate) struct RunControl {
    token: CancellationToken,
    state: Arc<Mutex<GovernorState>>,
    signal: Arc<Condvar>,
}

impl Default for RunControl {
    fn default() -> Self {
        Self::new()
    }
}

impl RunControl {
    /// Create a fresh control: running, budget-neutral, not cancelled.
    pub(crate) fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            state: Arc::new(Mutex::new(GovernorState::default())),
            signal: Arc::new(Condvar::new()),
        }
    }

    /// Lock the shared state, recovering from poisoning (a panic in another
    /// governing thread must not wedge the governor forever).
    fn lock_state(&self) -> MutexGuard<'_, GovernorState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_on_signal<'a>(
        &self,
        guard: MutexGuard<'a, GovernorState>,
    ) -> MutexGuard<'a, GovernorState> {
        self.signal
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The cancellation token distributed to tools for this run.
    #[must_use]
    pub(crate) fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Request immediate cancellation. Wakes every waiter: a loop parked in a
    /// user pause or at an exhausted-budget boundary observes the token and
    /// aborts instead of deadlocking.
    pub(crate) fn cancel(&self) {
        self.token.cancel();
        self.signal.notify_all();
    }

    /// Request a user pause. Takes effect at the runner's next step boundary;
    /// `resume` ends it, `cancel` terminates the run outright.
    pub(crate) fn pause(&self) {
        {
            self.lock_state().paused = true;
        }
        self.signal.notify_all();
    }

    /// Lift a user pause. During an exhausted-budget wait this grants no
    /// steps: only [`Self::extend_steps`] lets a parked-over-budget run
    /// continue.
    pub(crate) fn resume(&self) {
        {
            self.lock_state().paused = false;
        }
        self.signal.notify_all();
    }

    /// Grant `n` additional steps, effective whether the loop is still
    /// running or already parked at the budget-exhausted boundary.
    pub(crate) fn extend_steps(&self, n: usize) {
        {
            let mut state = self.lock_state();
            state.extra_steps = state.extra_steps.saturating_add(n);
        }
        self.signal.notify_all();
    }

    /// Whether a user pause is pending (has not yet been honoured at a step
    /// boundary).
    #[must_use]
    pub(crate) fn pause_pending(&self) -> bool {
        self.lock_state().paused
    }

    /// Blocks while the run is user-paused. Returns `false` when cancellation
    /// ended the wait; otherwise the pause was lifted by `resume`.
    pub(crate) fn wait_while_paused(&self) -> bool {
        let mut state = self.lock_state();
        while state.paused && !self.is_cancelled() {
            state = self.wait_on_signal(state);
        }
        !self.is_cancelled()
    }

    /// Cumulative extra steps granted so far.
    #[must_use]
    pub(crate) fn extra_steps(&self) -> usize {
        self.lock_state().extra_steps
    }

    /// Effective allowance for a run whose fixed budget is `base`.
    #[must_use]
    pub(crate) fn allowance(&self, base: usize) -> usize {
        base.saturating_add(self.extra_steps())
    }

    /// Park until the allowance rises above `taken` (via
    /// [`Self::extend_steps`]) or the run is cancelled. `resume` alone never
    /// satisfies the predicate — it only ends a user pause.
    ///
    /// Returns `false` when cancellation ended the wait.
    pub(crate) fn wait_for_allowance(&self, base: usize, taken: usize) -> bool {
        let mut state = self.lock_state();
        while taken >= base.saturating_add(state.extra_steps) && !self.is_cancelled() {
            state = self.wait_on_signal(state);
        }
        !self.is_cancelled()
    }
}
