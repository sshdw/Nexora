//! Enforceable run budgets, approval checkpoints, and the secret-free audit
//! trail (WS-B.2).
//!
//! This module plugs into the [`super::lifecycle::RunState`] model from WS-B.1
//! without changing execution behavior for runs that stay within budget:
//!
//! - [`RunBudget`] resolves the opaque [`super::lifecycle::BudgetHandles`]
//!   contract (step cap + spend cap) into the values the enforcement points
//!   consume. Cost accounting stays in abstract micro-USD units; the optional
//!   [`CostHook`] is an opaque placeholder for future pricing sources — no
//!   live pricing, no network. Enforcement itself still flows through the
//!   existing terminal paths (`budget::honor_allowance`,
//!   `budget::check_spend_guard` → `BudgetExhausted` / `SpendLimitExceeded`).
//! - [`GateOutcome`] models the approval checkpoint at each
//!   `AwaitingApproval` entry. The checkpoint reuses the #65
//!   ladder (`ApprovalGate::needs_approval` over `RiskClass`) unchanged —
//!   this type only names the decision, never remaps it.
//! - [`AuditLog`] is an append-only, run-scoped, in-memory accessory to the
//!   persistence row (no new table). Every entry carries only lifecycle state
//!   names and fixed vocabulary plus integer counters — never tool
//!   arguments, message content, credentials, or SQL. Every append is gated
//!   on [`super::lifecycle::transition`]: an illegal state change is a
//!   [`super::lifecycle::LifecycleError`] and appends nothing.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::application::execution::TokenUsage;

use super::budget::DEFAULT_MAX_ITERATIONS;
use super::lifecycle::{transition, BudgetHandles, LifecycleError, RunState};
use super::pricing;

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Opaque cost hook: maps one turn's token usage onto abstract micro-USD
/// cost units. A placeholder for future pricing sources — never live pricing,
/// never network. `None` (the default) keeps the existing policy-rate
/// accounting (`pricing::cost_for_model_usage`).
pub(crate) type CostHook = fn(TokenUsage) -> u64;

/// Enforceable run budget: step cap + spend cap in abstract units, resolved
/// from the lifecycle [`BudgetHandles`] contract.
///
/// The struct gates consumption, never model choice (routing profiles from
/// #69 resolve models independently). The runner builds one per run from its
/// configured caps and hands it to the existing enforcement points, so an
/// over-budget run reaches `BudgetExhausted` / `SpendLimitExceeded` through
/// the pre-existing terminal paths while an in-budget run behaves
/// byte-identically to before.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunBudget {
    max_steps: usize,
    spend_limit_micro_usd: Option<u64>,
    cost_hook: Option<CostHook>,
}

impl RunBudget {
    /// Resolve `max_steps` / `spend_limit_micro_usd` directly.
    #[must_use]
    pub(crate) const fn new(max_steps: usize, spend_limit_micro_usd: Option<u64>) -> Self {
        Self {
            max_steps,
            spend_limit_micro_usd,
            cost_hook: None,
        }
    }

    /// Resolve the lifecycle contract handles: an absent step cap falls back
    /// to the runner default; an absent spend cap means no guard.
    #[must_use]
    pub(crate) fn from_handles(handles: BudgetHandles) -> Self {
        Self {
            max_steps: handles.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS),
            spend_limit_micro_usd: handles.spend_limit_micro_usd,
            cost_hook: None,
        }
    }

    /// Attach an opaque cost hook (tests / future pricing sources). `None`
    /// keeps the existing policy-rate accounting.
    #[must_use]
    pub(crate) const fn with_cost_hook(mut self, hook: CostHook) -> Self {
        self.cost_hook = Some(hook);
        self
    }

    /// Step cap consumed by the allowance check.
    #[must_use]
    pub(crate) const fn max_steps(self) -> usize {
        self.max_steps
    }

    /// Spend cap consumed by the spend guard (`None` = no guard).
    #[must_use]
    pub(crate) const fn spend_limit_micro_usd(self) -> Option<u64> {
        self.spend_limit_micro_usd
    }

    /// Whether the hook override is attached (introspection for tests).
    #[must_use]
    pub(crate) const fn has_cost_hook(self) -> bool {
        self.cost_hook.is_some()
    }

    /// Step-cap trip predicate: `taken` model turns against the cap.
    #[must_use]
    pub(crate) const fn steps_exhausted(self, taken: usize) -> bool {
        taken >= self.max_steps
    }

    /// Bill one turn in abstract micro-USD: the hook when attached, otherwise
    /// the existing policy-rate accounting (known-free models bill $0).
    #[must_use]
    pub(crate) fn cost_for(self, model: &str, usage: TokenUsage) -> u64 {
        if let Some(hook) = self.cost_hook {
            hook(usage)
        } else {
            pricing::cost_for_model_usage(model, usage)
        }
    }
}

// ---------------------------------------------------------------------------
// Gate checkpoints
// ---------------------------------------------------------------------------

/// Approval checkpoint outcome: the recorded verdict of one
/// `AwaitingApproval` park.
///
/// The park itself is decided by the #65 ladder
/// (`ApprovalGate::needs_approval` over `RiskClass`, reused unchanged); this
/// enum only names the decision so every park resolves to exactly one
/// recorded gate outcome — approved and denied both continue the loop,
/// cancellation aborts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateOutcome {
    /// The parked tool call was approved and dispatched.
    Approved,
    /// The parked tool call was denied; it becomes a controlled observation.
    Denied,
    /// Cancellation ended the parked wait.
    Cancelled,
}

impl GateOutcome {
    /// Fixed vocabulary for the audit trail.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

/// Fixed-vocabulary audit event. Variants carry integer counters only —
/// never tool names, arguments, message content, credentials, or SQL — so a
/// hostile payload echoed through the run cannot leak into the trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuditEvent {
    /// Parked on a policy-flagged tool call (`Running → AwaitingApproval`).
    ApprovalParked,
    /// Park resolved with a gate decision (`AwaitingApproval → Running`).
    ApprovalResolved {
        /// Verdict; rendered as the fixed `approved` / `denied` vocabulary.
        approved: bool,
    },
    /// Cancellation ended the parked wait (`AwaitingApproval → Cancelled`).
    ApprovalCancelled,
    /// Step budget exhausted with a control attached; parked for extension
    /// (`Running → AwaitingBudget`).
    BudgetParked {
        /// Allowance in effect when the park fired.
        allowance: usize,
    },
    /// Extension raised the allowance (`AwaitingBudget → Running`).
    BudgetResumed,
    /// Cancellation ended the budget wait (`AwaitingBudget → Cancelled`).
    BudgetCancelled,
    /// Step budget exhausted without a control: terminal
    /// (`Running → BudgetExhausted`).
    BudgetExhausted {
        /// Allowance in effect when the terminal fired.
        allowance: usize,
    },
    /// Spend guard tripped: terminal (`Running → SpendLimitExceeded`).
    SpendTripped {
        /// Accumulated spend when the guard fired, micro-USD.
        spent_micro: u64,
        /// Limit in effect when the guard fired, micro-USD.
        limit_micro: u64,
    },
}

impl AuditEvent {
    /// Fixed event vocabulary for the audit trail.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ApprovalParked => "approval_parked",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::ApprovalCancelled => "approval_cancelled",
            Self::BudgetParked { .. } => "budget_parked",
            Self::BudgetResumed => "budget_resumed",
            Self::BudgetCancelled => "budget_cancelled",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::SpendTripped { .. } => "spend_tripped",
        }
    }

    /// Build the resolve event for a gate outcome. Returns `None` for
    /// [`GateOutcome::Cancelled`], which records as [`Self::ApprovalCancelled`]
    /// instead (a distinct lifecycle edge).
    #[must_use]
    pub(crate) const fn for_gate(outcome: GateOutcome) -> Option<Self> {
        match outcome {
            GateOutcome::Approved => Some(Self::ApprovalResolved { approved: true }),
            GateOutcome::Denied => Some(Self::ApprovalResolved { approved: false }),
            GateOutcome::Cancelled => None,
        }
    }
}

/// One append-only audit entry: lifecycle edge plus fixed vocabulary.
/// Constructible only through [`AuditLog::record`], which gates on
/// [`transition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuditEntry {
    /// Append order (0-based, gap-free while the log is only appended to).
    pub seq: u64,
    /// Lifecycle state at entry (`RunState::as_str` vocabulary).
    pub from: &'static str,
    /// Lifecycle state after entry (`RunState::as_str` vocabulary).
    pub to: &'static str,
    /// Fixed event vocabulary (`AuditEvent::name`).
    pub event: &'static str,
    /// Gate verdict vocabulary (`approved` / `denied`), if the event carries
    /// a gate decision.
    pub decision: Option<&'static str>,
    /// Budget allowance in effect, if the event carries one.
    pub allowance: Option<usize>,
    /// Accumulated spend in micro-USD, if the event carries one.
    pub spent_micro: Option<u64>,
    /// Spend limit in micro-USD, if the event carries one.
    pub limit_micro: Option<u64>,
}

impl AuditEntry {
    fn new(seq: u64, from: RunState, to: RunState, event: AuditEvent) -> Self {
        let (decision, allowance, spent_micro, limit_micro) = match event {
            AuditEvent::ApprovalResolved { approved } => (
                Some(if approved {
                    GateOutcome::Approved.as_str()
                } else {
                    GateOutcome::Denied.as_str()
                }),
                None,
                None,
                None,
            ),
            AuditEvent::BudgetParked { allowance } | AuditEvent::BudgetExhausted { allowance } => {
                (None, Some(allowance), None, None)
            }
            AuditEvent::SpendTripped {
                spent_micro,
                limit_micro,
            } => (None, None, Some(spent_micro), Some(limit_micro)),
            AuditEvent::ApprovalParked
            | AuditEvent::ApprovalCancelled
            | AuditEvent::BudgetResumed
            | AuditEvent::BudgetCancelled => (None, None, None, None),
        };
        Self {
            seq,
            from: from.as_str(),
            to: to.as_str(),
            event: event.name(),
            decision,
            allowance,
            spent_micro,
            limit_micro,
        }
    }
}

/// Append-only, run-scoped, in-memory audit trail: the accessory to the
/// persistence row (no new table).
///
/// Entries are ordered by append (`seq`) and secret-free by construction —
/// the log accepts only [`RunState`]s and [`AuditEvent`]s, so there is no
/// channel through which content, credentials, or SQL could enter. Every
/// append goes through [`transition`]: an illegal state change returns the
/// secret-free [`LifecycleError`] and appends nothing.
#[derive(Debug, Default)]
pub(crate) struct AuditLog {
    entries: Mutex<Vec<AuditEntry>>,
}

impl AuditLog {
    /// Empty trail for one run.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<AuditEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Append one lifecycle edge. Enforces the transition table: illegal
    /// edges fail with [`LifecycleError`] and append nothing.
    pub(crate) fn record(
        &self,
        from: RunState,
        to: RunState,
        event: AuditEvent,
    ) -> Result<(), LifecycleError> {
        transition(from, to)?;
        let mut entries = self.lock();
        let seq = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        entries.push(AuditEntry::new(seq, from, to, event));
        Ok(())
    }

    /// Ordered snapshot of the trail.
    #[must_use]
    pub(crate) fn entries(&self) -> Vec<AuditEntry> {
        self.lock().clone()
    }

    /// Number of appended entries.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing has been appended yet.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// Best-effort audit append at an enforcement point: legal edges are
/// recorded; an illegal edge (a model bug) is logged — and `debug_assert`ed —
/// but never alters the run's outcome, mirroring `observe_transition`.
pub(crate) fn audit(log: Option<&AuditLog>, from: RunState, to: RunState, event: AuditEvent) {
    if let Some(trail) = log {
        if trail.record(from, to, event).is_err() {
            log::warn!(
                "agent audit: illegal transition from '{}' to '{}' for event '{}'",
                from.as_str(),
                to.as_str(),
                event.name()
            );
            debug_assert!(
                transition(from, to).is_ok(),
                "illegal agent audit transition from '{}' to '{}'",
                from.as_str(),
                to.as_str()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
    use crate::application::agent::control::RunControl;
    use crate::application::agent::runner::test_support::*;
    use crate::application::agent::runner::AgentRunner;
    use crate::application::execution::AiResponse;
    use std::fs;
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    // -----------------------------------------------------------------------
    // Budget struct
    // -----------------------------------------------------------------------

    #[test]
    fn budget_from_handles_wires_both_caps() {
        let full = RunBudget::from_handles(BudgetHandles {
            max_iterations: Some(7),
            spend_limit_micro_usd: Some(250_000),
        });
        assert_eq!(full.max_steps(), 7);
        assert_eq!(full.spend_limit_micro_usd(), Some(250_000));
        assert!(!full.has_cost_hook());

        // Absent caps fall back: runner default for steps, no guard for spend.
        let bare = RunBudget::from_handles(BudgetHandles {
            max_iterations: None,
            spend_limit_micro_usd: None,
        });
        assert_eq!(bare.max_steps(), DEFAULT_MAX_ITERATIONS);
        assert_eq!(bare.spend_limit_micro_usd(), None);

        assert!(full.steps_exhausted(7));
        assert!(!full.steps_exhausted(6));
    }

    #[test]
    fn cost_hook_overrides_pricing_in_abstract_units() {
        // Opaque placeholder hook: bills a flat abstract-unit cost per turn,
        // independent of the policy rate and without any network.
        let hooked = RunBudget::new(10, Some(1_000_000)).with_cost_hook(|_| 999_999_999);
        assert!(hooked.has_cost_hook());
        let usage = crate::application::execution::TokenUsage {
            input_tokens: 1,
            output_tokens: 0,
        };
        assert_eq!(hooked.cost_for("openai", usage), 999_999_999);

        // Without a hook the policy rate applies (1 input token bills > 0 and
        // known-free models bill $0) — existing accounting untouched.
        let plain = RunBudget::new(10, Some(1_000_000));
        assert_eq!(
            plain.cost_for("openai", usage),
            plain.cost_for("openai", usage)
        );
        assert!(plain.cost_for("openai", usage) > 0);
        assert_eq!(plain.cost_for("model-free", usage), 0);
    }

    // -----------------------------------------------------------------------
    // Budget exhaustion pins both terminals via transition()
    // -----------------------------------------------------------------------

    #[test]
    fn step_cap_pins_budget_exhausted_terminal() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let step = || {
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![call_tool("loop", "list_directory", serde_json::json!({}))],
                usage: None,
            })
        };
        let fake = FakeExecutor::new(vec![step(), step()]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_budget(BudgetHandles {
                max_iterations: Some(1),
                spend_limit_micro_usd: None,
            })
            .with_audit_log(Arc::clone(&log));

        let err = runner
            .run("openai", "m", "cred", "loop")
            .expect_err("must exhaust");
        assert!(
            matches!(
                err,
                crate::application::agent::runner::AgentError::BudgetExhausted(1)
            ),
            "expected BudgetExhausted(1), got {err:?}"
        );
        // The terminal is reached through the lifecycle table ...
        assert!(transition(RunState::Running, RunState::BudgetExhausted).is_ok());
        // ... and the trail records exactly that edge with the allowance.
        let entries = log.entries();
        assert_eq!(entries.len(), 1, "one terminal edge, got {entries:?}");
        assert_eq!(entries[0].from, "running");
        assert_eq!(entries[0].to, "budget_exhausted");
        assert_eq!(entries[0].event, "budget_exhausted");
        assert_eq!(entries[0].allowance, Some(1));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn spend_cap_pins_spend_limit_exceeded_terminal() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        // 200k input tokens bills 1M micro at the policy rate; the limit
        // trips on the second turn (2M > 1.5M).
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 200_000, 0)),
            Ok(usage_response("done", 200_000, 0)),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_budget(BudgetHandles {
                max_iterations: None,
                spend_limit_micro_usd: Some(1_500_000),
            })
            .with_audit_log(Arc::clone(&log));

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must trip");
        assert!(
            matches!(
                err,
                crate::application::agent::runner::AgentError::SpendLimitExceeded { .. }
            ),
            "expected SpendLimitExceeded, got {err:?}"
        );
        assert!(transition(RunState::Running, RunState::SpendLimitExceeded).is_ok());
        let entries = log.entries();
        assert_eq!(entries.len(), 1, "one terminal edge, got {entries:?}");
        assert_eq!(entries[0].from, "running");
        assert_eq!(entries[0].to, "spend_limit_exceeded");
        assert_eq!(entries[0].event, "spend_tripped");
        assert_eq!(entries[0].spent_micro, Some(2_000_000));
        assert_eq!(entries[0].limit_micro, Some(1_500_000));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn in_budget_run_records_no_trips_and_behaves_identically() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let fake = FakeExecutor::new(vec![
            Ok(usage_tool_response("a", 10, 0)),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(5)
            .with_spend_limit(10_000_000)
            .with_audit_log(Arc::clone(&log));

        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        assert_eq!(answer, "done");
        assert_eq!(fake.requests.borrow().len(), 2);
        // No budget/gate trip fired: the trail stays empty and the run is
        // byte-identical to the no-audit behavior.
        assert!(
            log.is_empty(),
            "in-budget run appends nothing, got {:?}",
            log.entries()
        );
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Gates: park / resolve / cancel paths with recorded decisions
    // -----------------------------------------------------------------------

    fn supervised_two_tool_script() -> (FakeExecutor, std::path::PathBuf) {
        let ws = temp_workspace();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("g1")),
            Ok(tool_step("g2")),
            Ok(text_response("done")),
        ]);
        (fake, ws)
    }

    #[test]
    fn gate_park_and_approve_records_decision() {
        let (fake, ws) = supervised_two_tool_script();
        let log = Arc::new(AuditLog::new());
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let driver_gate = gate.clone();
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_audit_log(Arc::clone(&log));

        let driver = thread::spawn(move || {
            for id in ["g1", "g2"] {
                let start = std::time::Instant::now();
                while !driver_gate.has_pending_for(id) {
                    assert!(
                        start.elapsed() <= Duration::from_secs(5),
                        "timed out waiting for park {id}"
                    );
                    thread::yield_now();
                }
                assert!(driver_gate.respond(id, ApprovalDecision::Approved));
            }
        });

        let answer = runner.run("openai", "m", "cred", "q").expect("completes");
        driver.join().expect("driver joins");
        assert_eq!(answer, "done");

        // Every park has its recorded decision, in append order.
        let events: Vec<(&str, Option<&str>)> = log
            .entries()
            .iter()
            .map(|entry| (entry.event, entry.decision))
            .collect();
        assert_eq!(
            events,
            vec![
                ("approval_parked", None),
                ("approval_resolved", Some("approved")),
                ("approval_parked", None),
                ("approval_resolved", Some("approved")),
            ],
            "every park resolves to a recorded decision"
        );
        for entry in log.entries() {
            match entry.event {
                "approval_parked" => {
                    assert_eq!((entry.from, entry.to), ("running", "awaiting_approval"));
                }
                "approval_resolved" => {
                    assert_eq!((entry.from, entry.to), ("awaiting_approval", "running"));
                }
                other => panic!("unexpected audit event {other:?}"),
            }
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn gate_deny_records_denied_decision_and_continues() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let driver_gate = gate.clone();
        let fake = FakeExecutor::new(vec![Ok(tool_step("d1")), Ok(text_response("done"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_audit_log(Arc::clone(&log));

        let driver = thread::spawn(move || {
            let start = std::time::Instant::now();
            while !driver_gate.has_pending_for("d1") {
                assert!(
                    start.elapsed() <= Duration::from_secs(5),
                    "timed out waiting for park"
                );
                thread::yield_now();
            }
            assert!(driver_gate.respond("d1", ApprovalDecision::Denied));
        });

        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("denial continues");
        driver.join().expect("driver joins");
        assert_eq!(answer, "done");

        let events: Vec<(&str, Option<&str>)> = log
            .entries()
            .iter()
            .map(|entry| (entry.event, entry.decision))
            .collect();
        assert_eq!(
            events,
            vec![
                ("approval_parked", None),
                ("approval_resolved", Some("denied")),
            ]
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn gate_cancel_records_cancelled_decision() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let driver_gate = gate.clone();
        let fake = FakeExecutor::new(vec![Ok(tool_step("c1"))]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate.clone())
            .with_audit_log(Arc::clone(&log));

        let driver = thread::spawn(move || {
            let start = std::time::Instant::now();
            while !driver_gate.has_pending_for("c1") {
                assert!(
                    start.elapsed() <= Duration::from_secs(5),
                    "timed out waiting for park"
                );
                thread::yield_now();
            }
            gate.cancel();
        });

        let err = runner
            .run("openai", "m", "cred", "q")
            .expect_err("must cancel");
        driver.join().expect("driver joins");
        assert!(
            matches!(
                err,
                crate::application::agent::runner::AgentError::Cancelled
            ),
            "expected Cancelled, got {err:?}"
        );

        let entries = log.entries();
        assert_eq!(entries.len(), 2, "park + cancel, got {entries:?}");
        assert_eq!(entries[0].event, "approval_parked");
        assert_eq!(
            (entries[0].from, entries[0].to),
            ("running", "awaiting_approval")
        );
        assert_eq!(entries[1].event, "approval_cancelled");
        assert_eq!(
            (entries[1].from, entries[1].to),
            ("awaiting_approval", "cancelled")
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn gate_outcome_maps_to_audit_events() {
        assert_eq!(
            AuditEvent::for_gate(GateOutcome::Approved),
            Some(AuditEvent::ApprovalResolved { approved: true })
        );
        assert_eq!(
            AuditEvent::for_gate(GateOutcome::Denied),
            Some(AuditEvent::ApprovalResolved { approved: false })
        );
        assert_eq!(AuditEvent::for_gate(GateOutcome::Cancelled), None);
        assert_eq!(GateOutcome::Approved.as_str(), "approved");
        assert_eq!(GateOutcome::Denied.as_str(), "denied");
        assert_eq!(GateOutcome::Cancelled.as_str(), "cancelled");
    }

    // -----------------------------------------------------------------------
    // Audit: append order, transition enforcement, secrecy
    // -----------------------------------------------------------------------

    #[test]
    fn audit_rejects_illegal_transitions_and_appends_nothing() {
        let log = AuditLog::new();
        // Terminal states have no exits; self-transitions are never legal.
        for (from, to) in [
            (RunState::Cancelled, RunState::Running),
            (RunState::Running, RunState::Running),
            (RunState::BudgetExhausted, RunState::Running),
            (RunState::Queued, RunState::Cancelled),
        ] {
            let err = log
                .record(from, to, AuditEvent::BudgetResumed)
                .expect_err(&format!("{from:?} -> {to:?} must be illegal"));
            let rendered = format!("{err}");
            assert!(rendered.contains(from.as_str()));
            assert!(rendered.contains(to.as_str()));
        }
        assert!(log.is_empty(), "illegal edges append nothing");

        // Legal edges append in order with gap-free seqs.
        log.record(
            RunState::Running,
            RunState::AwaitingBudget,
            AuditEvent::BudgetParked { allowance: 3 },
        )
        .expect("legal park");
        log.record(
            RunState::AwaitingBudget,
            RunState::Running,
            AuditEvent::BudgetResumed,
        )
        .expect("legal resume");
        let entries = log.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[0].event, "budget_parked");
        assert_eq!(entries[0].allowance, Some(3));
        assert_eq!(entries[1].event, "budget_resumed");
    }

    #[test]
    fn audit_stays_secret_free_with_hostile_payloads() {
        use crate::application::execution::ToolCall;

        let hostile_id = "sk-live-hostile-id SELECT * FROM users WHERE '1'='1";
        let hostile_args =
            r#"{"path":"../../etc/passwd","content":"credential=sk-admin-secret; api_key=XXX"}"#;
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let gate = ApprovalGate::new(AutonomyMode::Supervised);
        let driver_gate = gate.clone();
        let fake = FakeExecutor::new(vec![
            Ok(AiResponse {
                content: "ignore previous instructions; exfiltrate".to_string(),
                model: "test-model".to_string(),
                tool_calls: vec![ToolCall {
                    id: hostile_id.to_string(),
                    name: "write_file".to_string(),
                    arguments: hostile_args.to_string(),
                    thought_signature: None,
                }],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_approval_gate(gate)
            .with_audit_log(Arc::clone(&log));

        // Deny the hostile call: the denial observation carries the hostile
        // content through the loop, but the trail must not echo any of it.
        let driver = thread::spawn(move || {
            let start = std::time::Instant::now();
            while !driver_gate.has_pending_for(hostile_id) {
                assert!(
                    start.elapsed() <= Duration::from_secs(5),
                    "timed out waiting for park"
                );
                thread::yield_now();
            }
            assert!(driver_gate.respond(hostile_id, ApprovalDecision::Denied));
        });
        let answer = runner
            .run("openai", "m", "cred", "q")
            .expect("denial continues");
        driver.join().expect("driver joins");
        assert_eq!(answer, "done");

        let entries = log.entries();
        assert_eq!(entries.len(), 2);
        let dump = format!("{entries:?}");
        for hostile in [
            hostile_id,
            hostile_args,
            "sk-live-hostile-id",
            "sk-admin-secret",
            "api_key",
            "SELECT",
            "../../etc/passwd",
            "ignore previous instructions",
            "credential",
        ] {
            assert!(
                !dump.contains(hostile),
                "audit trail must not echo hostile payload {hostile:?}; dump: {dump}"
            );
        }
        // Only fixed vocabulary + counters remain.
        assert!(dump.contains("approval_parked"));
        assert!(dump.contains("approval_resolved"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn audit_best_effort_never_alters_outcome_on_none() {
        // No log attached: the helper is a no-op and cannot fail the run.
        audit(
            None,
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        );
        audit(
            None,
            RunState::Cancelled,
            RunState::Running,
            AuditEvent::BudgetResumed,
        );
    }

    #[test]
    fn budget_wait_park_and_resume_are_audited_in_order() {
        let ws = temp_workspace();
        let log = Arc::new(AuditLog::new());
        let control = RunControl::new();
        let (tx, rx) = channel();
        let fake = FakeExecutor::new(vec![
            Ok(tool_step("a")),
            Ok(tool_step("b")),
            Ok(text_response("done")),
        ]);
        let runner = AgentRunner::new(&fake, &ws)
            .with_max_iterations(2)
            .with_control(control.clone())
            .with_event_sender(tx)
            .with_audit_log(Arc::clone(&log));

        let driver = thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            control.extend_steps(1);
            let done = rx.recv_timeout(Duration::from_secs(5)).expect("event");
            (first, done)
        });

        let answer = runner.run("openai", "m", "cred", "go").expect("completes");
        let (first, done) = driver.join().expect("driver joins");
        assert_eq!(
            first,
            crate::application::agent::control::AgentRunEvent::BudgetExhausted { max_steps: 2 }
        );
        assert_eq!(
            done,
            crate::application::agent::control::AgentRunEvent::Completed { steps: 3 }
        );
        assert_eq!(answer, "done");

        let events: Vec<&str> = log.entries().iter().map(|entry| entry.event).collect();
        assert_eq!(events, vec!["budget_parked", "budget_resumed"]);
        let entries = log.entries();
        assert_eq!(
            (entries[0].from, entries[0].to),
            ("running", "awaiting_budget")
        );
        assert_eq!(
            (entries[1].from, entries[1].to),
            ("awaiting_budget", "running")
        );
        assert_eq!(entries[0].allowance, Some(2));
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        let _ = fs::remove_dir_all(&ws);
    }
}
