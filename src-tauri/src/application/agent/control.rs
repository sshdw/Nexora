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
///
/// NEX-TASK-002: the flag additionally records a shutdown origin
/// ([`Self::shutdown`]): a shutdown-cancel wakes every parked wait exactly
/// like [`Self::cancel`], but parked approval waits observe the origin and
/// record the honest shutdown vocabulary instead of the user-cancel text.
#[derive(Debug, Clone)]
pub(crate) struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
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
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the token cancelled. Never blocks.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Mark the token cancelled *by application shutdown* (NEX-TASK-002).
    /// Wakes every parked wait exactly like [`Self::cancel`]; parked
    /// approval waits additionally observe this origin and record the
    /// shutdown vocabulary (never the user-cancel text). Never blocks.
    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancel();
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Whether the cancellation came from application shutdown
    /// ([`Self::shutdown`]) rather than a user cancel.
    #[must_use]
    pub(crate) fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
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
    /// A context compaction began: the run folds older turns into a summary
    /// (Task T4). `reason` is `"threshold"` (proactive, usage crossed
    /// `COMPACTION_THRESHOLD`) or `"overflow"` (reactive, the provider
    /// reported `ContextLengthExceeded`); `messages` is the in-run history
    /// length being compacted.
    CompactionStarted {
        /// `"threshold"` or `"overflow"`.
        reason: String,
        /// In-run history length at compaction time.
        messages: usize,
    },
    /// A context compaction finished: the head was folded into a summary.
    /// `summarized` counts the folded messages, `retained` the verbatim
    /// tail; the summary itself travels as a normal user message, so no
    /// step is recorded and the persisted `seq` contract is untouched.
    CompactionFinished {
        /// `"threshold"` or `"overflow"`.
        reason: String,
        /// Messages folded into the summary.
        summarized: usize,
        /// Messages retained verbatim after the summary.
        retained: usize,
    },
    /// A context compaction failed (empty plan or a failed summarizer call):
    /// the run continues uncompacted and never fails for this alone.
    CompactionFailed {
        /// `"threshold"` or `"overflow"`.
        reason: String,
    },
    /// A tool call is awaiting user approval (Task 4.1).
    ApprovalRequested {
        /// Provider-assigned identifier for the pending tool call.
        call_id: String,
        /// Tool name.
        name: String,
        /// Raw JSON arguments for the call.
        arguments: String,
        /// Session-sticky group key (`preset:tool:parent-or-*`, M1-core).
        /// Additive: existing fields are untouched.
        #[serde(default)]
        group_key: Option<String>,
        /// Number of same-group calls in the current batch (M1-core).
        /// Additive: existing fields are untouched.
        #[serde(default)]
        group_size: usize,
        /// Park deadline in seconds at emission time (NEX-AGENT-004): the UI
        /// surfaces this as a countdown/notice so an unanswered approval is
        /// never a silent hang. Additive: defaults to `0` (unknown deadline,
        /// no notice) for older emitters.
        #[serde(default)]
        timeout_secs: u64,
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
    ///
    /// NEX-PERF-003: `observation` is a *bounded preview* of the sanitized
    /// stored text (see `persistence::MAX_STEP_PREVIEW_CHARS`), never the
    /// full output — a 200-turn run must not retain megabytes over IPC and
    /// in the render tree. `observation_len` is the byte-exact total of the
    /// full sanitized text (`None` exactly when `observation` is `None`);
    /// `observation_truncated` tells the UI whether the preview was cut.
    /// The full text stays reachable on demand through
    /// `get_agent_step_observation(step_id)`; `step_id` is the
    /// `agent_steps.id` surrogate key that command takes.
    StepRecorded {
        /// Surrogate key of the persisted `agent_steps` row (`id`).
        step_id: i64,
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
        /// Bounded preview of the model-turn content / tool output / denial
        /// or approval text (sanitized, exactly as stored up to the cut).
        observation: Option<String>,
        /// Byte-exact length of the full sanitized observation text;
        /// `None` exactly when `observation` is `None`.
        observation_len: Option<u64>,
        /// Whether the preview was cut (`true` exactly when the stored text
        /// exceeds the preview bound).
        observation_truncated: bool,
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
/// which wake promptly on [`Self::cancel`]. Mutators take the governor mutex
/// only for their own critical section, so they block no caller for longer
/// than a few field writes.
#[derive(Debug, Clone)]
pub(crate) struct RunControl {
    token: CancellationToken,
    state: Arc<Mutex<GovernorState>>,
    signal: Arc<Condvar>,
    /// Set while a runner thread is parked inside
    /// [`Self::wait_for_allowance`] (budget exhausted, awaiting
    /// `extend_steps` or `cancel`). Lock-free so external pollers (e.g. the
    /// task loop's terminal-wait) can observe the park without contending
    /// the governor mutex. Shared by all clones.
    budget_parked: Arc<AtomicBool>,
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
            budget_parked: Arc::new(AtomicBool::new(false)),
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
        #[cfg(test)]
        pre_wait_probe::hold_window();
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
    ///
    /// The token is mutated under the governor mutex, which is what makes the
    /// wake loss-free: a waiter only ever *gives up* that mutex by becoming a
    /// condvar waiter (enqueue and unlock are atomic), so a notifier either
    /// runs entirely before the waiter's predicate check — which then observes
    /// the flag — or entirely after the waiter is enqueued, which then
    /// receives this notification. Notifying without the mutex (issue #256)
    /// leaves the window between check and enqueue open and wedges a paused
    /// run forever.
    pub(crate) fn cancel(&self) {
        {
            let _guard = self.lock_state();
            self.token.cancel();
        }
        self.signal.notify_all();
    }

    /// Request cancellation by application shutdown (NEX-TASK-002). Wakes
    /// every waiter exactly like [`Self::cancel`], and additionally marks
    /// the shutdown origin on the shared token so parked approval waits
    /// record the honest shutdown vocabulary instead of the user-cancel
    /// text. Never blocks a caller beyond a governor critical section.
    pub(crate) fn shutdown(&self) {
        {
            let _guard = self.lock_state();
            self.token.shutdown();
        }
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
        if taken < base.saturating_add(state.extra_steps) {
            return !self.is_cancelled();
        }
        self.budget_parked.store(true, Ordering::SeqCst);
        while taken >= base.saturating_add(state.extra_steps) && !self.is_cancelled() {
            state = self.wait_on_signal(state);
        }
        self.budget_parked.store(false, Ordering::SeqCst);
        !self.is_cancelled()
    }

    /// Whether a runner thread is currently parked in
    /// [`Self::wait_for_allowance`]. Observed by the task loop's
    /// terminal-wait so a budget-parked step run (whose persisted row stays
    /// `'running'`) is surfaced instead of polled forever.
    #[must_use]
    pub(crate) fn is_budget_parked(&self) -> bool {
        self.budget_parked.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Test-only park-window probe (never compiled outside test builds)
// ---------------------------------------------------------------------------

/// Widens the nanosecond-wide window between a waiter's park predicate and its
/// condvar enqueue so a lost-wakeup regression test can drive the interleaving
/// deterministically instead of winning a nanosecond coin flip.
///
/// The probe only observes; it cannot manufacture the bug. What it guarantees
/// is that a parking waiter holds the governor mutex — with its predicate
/// already evaluated — for long enough for a concurrent `cancel` to *attempt*:
/// a `cancel` that mutates the token without the mutex returns inside the
/// window and its notification is dropped (the waiter then sleeps forever),
/// while a `cancel` that mutates the token under the mutex cannot return at
/// all until the waiter parks, so the notification is always delivered.
#[cfg(test)]
mod pre_wait_probe {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    // Armed per waiting thread, so an armed probe can never latch onto an
    // unrelated park running in parallel under libtest.
    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    /// Published by the parking waiter while it still holds the governor mutex,
    /// immediately before its condvar enqueue.
    static IN_WINDOW: AtomicBool = AtomicBool::new(false);
    /// Set by the notifier immediately before it calls `cancel`.
    static ATTEMPTED: AtomicBool = AtomicBool::new(false);
    /// Set by the notifier after its `cancel` call returned.
    static DONE: AtomicBool = AtomicBool::new(false);

    /// How long the notifier waits for the waiter to reach the window.
    const NOTIFIER_BOUND: Duration = Duration::from_secs(10);
    /// Grace after `ATTEMPTED` before the waiter concludes the notifier is
    /// blocked on the governor mutex and parks to let it through. A lock-free
    /// `cancel` returns in nanoseconds, so this is four orders of magnitude
    /// more slack than the unfixed path needs to be observed.
    const BLOCKED_GRACE: Duration = Duration::from_millis(200);
    /// Poll spacing for the bounded window waits.
    const POLL: Duration = Duration::from_millis(1);

    /// Arms the probe on the calling thread for one park.
    pub(crate) struct ProbeGuard;

    impl ProbeGuard {
        #[must_use]
        pub(crate) fn arm() -> Self {
            ARMED.with(|armed| armed.set(true));
            Self
        }
    }

    impl Drop for ProbeGuard {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.set(false));
        }
    }

    /// Clears the handshake before a scenario arms the probe.
    pub(crate) fn reset() {
        IN_WINDOW.store(false, Ordering::Release);
        ATTEMPTED.store(false, Ordering::Release);
        DONE.store(false, Ordering::Release);
    }

    /// Notifier side: wait (bounded) for the waiter to enter the window.
    pub(crate) fn await_window() {
        let deadline = Instant::now() + NOTIFIER_BOUND;
        while !IN_WINDOW.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Notifier side: announce the imminent `cancel`.
    pub(crate) fn notify_attempted() {
        ATTEMPTED.store(true, Ordering::Release);
    }

    /// Notifier side: announce that the `cancel` call returned.
    pub(crate) fn notify_done() {
        DONE.store(true, Ordering::Release);
    }

    /// Waiter side: hold the window open until the notifier has either
    /// completed its `cancel` (unfixed: the notification was dropped) or is
    /// demonstrably blocked on the governor mutex this thread still holds
    /// (fixed: the waiter must park to let it through).
    pub(crate) fn hold_window() {
        if !ARMED.with(Cell::get) {
            return;
        }
        IN_WINDOW.store(true, Ordering::Release);
        let mut blocked_deadline: Option<Instant> = None;
        loop {
            if DONE.load(Ordering::Acquire) {
                break;
            }
            let now = Instant::now();
            match blocked_deadline {
                Some(deadline) if now >= deadline => break,
                None if ATTEMPTED.load(Ordering::Acquire) => {
                    blocked_deadline = Some(now + BLOCKED_GRACE);
                }
                None | Some(_) => {}
            }
            std::thread::sleep(POLL);
        }
        IN_WINDOW.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;

    /// Every assertion below is bounded: a lost wakeup must surface as a test
    /// failure in seconds, never as a parked thread that wedges the run.
    const BOUND: Duration = Duration::from_secs(10);

    /// The probe handshake is process-wide, so the two scenarios must not
    /// overlap (libtest runs tests in parallel).
    static PROBE_LOCK: Mutex<()> = Mutex::new(());

    /// Parks a fresh control with `park`, fires exactly one `cancel` from
    /// another thread inside the check-to-enqueue window, and requires the
    /// waiter to observe the cancellation within [`BOUND`].
    fn cancel_wakes_parked_waiter(park: fn(&RunControl) -> bool) {
        let _serial = PROBE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        pre_wait_probe::reset();

        let control = RunControl::new();
        let (tx, rx) = channel();
        let waiter_control = control.clone();
        let waiter = thread::spawn(move || {
            let probe = pre_wait_probe::ProbeGuard::arm();
            let resumed = park(&waiter_control);
            drop(probe);
            let _ = tx.send(resumed);
        });

        let notifier_control = control.clone();
        let notifier = thread::spawn(move || {
            pre_wait_probe::await_window();
            pre_wait_probe::notify_attempted();
            notifier_control.cancel();
            pre_wait_probe::notify_done();
        });

        // A lost wakeup parks the waiter thread forever, so the bound is the
        // assertion: expiry means cancellation was requested and never
        // observed (issue #256).
        let resumed = rx.recv_timeout(BOUND).expect(
            "a parked waiter must observe the cancellation; a lost wakeup parks it forever",
        );
        assert!(!resumed, "cancellation must end the park, never resume it");

        notifier.join().expect("notifier joins");
        waiter.join().expect("waiter joins");
    }

    /// Regression for the lost-wakeup deadlock of issue #256: a user-paused run
    /// that is cancelled while its runner sits between the park predicate and
    /// the condvar enqueue must still observe the cancellation.
    #[test]
    fn cancel_wakes_a_waiter_parked_on_a_user_pause() {
        cancel_wakes_parked_waiter(|control| {
            control.pause();
            control.wait_while_paused()
        });
    }

    /// Same race on the other production park: a run over its step allowance
    /// that is cancelled while parked awaiting `extend_steps`.
    #[test]
    fn cancel_wakes_a_waiter_parked_at_the_budget_boundary() {
        cancel_wakes_parked_waiter(|control| control.wait_for_allowance(1, 1));
    }

    /// Sibling notifiers mutate `GovernorState` under the governor mutex before
    /// notifying, so a `resume` cannot be lost either. Pinned here so the
    /// asymmetry with `cancel` (issue #256) cannot return: `extend_steps` moves
    /// state while leaving the run paused, and only `resume` ends the park.
    #[test]
    fn resume_wakes_a_paused_waiter_that_extend_steps_did_not_release() {
        let control = RunControl::new();
        control.pause();
        let (tx, rx) = channel();
        let waiter_control = control.clone();
        let waiter = thread::spawn(move || {
            let _ = tx.send(waiter_control.wait_while_paused());
        });
        control.extend_steps(1);
        assert!(control.pause_pending(), "extend must not lift a user pause");
        assert_eq!(control.extra_steps(), 1, "extend must be recorded");
        control.resume();
        assert!(
            rx.recv_timeout(BOUND)
                .expect("resume wakes the parked waiter"),
            "the waiter resumes when the pause is lifted"
        );
        waiter.join().expect("waiter joins");
    }
}
