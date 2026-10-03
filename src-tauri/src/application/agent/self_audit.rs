//! Run self-audit: a read-only safety-net review of a run's own trail
//! (WS-C.3, closes WS-C).
//!
//! The pass composes the verdicts other modules already recorded — the
//! [`AuditLog`](super::governance::AuditLog) entries, the
//! [`SnapshotStore`](super::snapshots::SnapshotStore) markers, and the
//! caller-supplied gate/scan/permission verdict lists — and reports
//! pass/fail with a secret-free violation list. It never re-scans content,
//! never duplicates another module's logic, and never mutates anything: the
//! only write is the opt-in [`record_report`] outcome append, which goes
//! through [`transition`](super::lifecycle::transition) like every other
//! audit append.
//!
//! Input verdict vocabulary is fixed (`approved` / `denied` / `cancelled`,
//! `boundary` / `imperative` / `mimicry`); anything outside it fails closed
//! with [`ViolationCode::UnknownVerdict`] without echoing the unknown value.
//! [`Violation`] carries a code plus integer counters only, so hostile trail
//! content cannot leak through the report. The verdict input types render
//! [`std::fmt::Debug`] without their string fields for the same reason
//! (mirroring [`SnapshotStore`](super::snapshots::SnapshotStore)'s
//! counts-only `Debug`).
//!
//! Without this pass attached a run behaves byte-identically to before
//! (opt-in like the WS-B.2 audit trail): the runner is untouched.

use std::fmt::{Debug, Display, Formatter, Result as FmtResult};

use super::governance::{AuditEvent, AuditLog};
use super::lifecycle::{LifecycleError, RunState};
use super::snapshots::SnapshotStore;

// ---------------------------------------------------------------------------
// Violation vocabulary
// ---------------------------------------------------------------------------

/// Fixed-vocabulary self-audit violation codes. Secret-free by construction:
/// rendering a code names only the invariant, never trail content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViolationCode {
    /// A park (`approval_parked` / `budget_parked`) has no matching recorded
    /// decision — in the trail or in the gate-decision list.
    UnresolvedPark,
    /// The run claims a terminal state while a park is still open (or the
    /// trail still points at an `Awaiting*` state).
    TerminalParked,
    /// Snapshot budget counters regress across captures, or a snapshot audit
    /// marker reaches past the trail.
    BudgetRegression,
    /// A recorded envelope-scan verdict is neither parked nor cleared.
    UnresolvedScan,
    /// A denied tool call was executed afterwards (audit-order cross-check).
    ExecutedAfterDeny,
    /// A recorded verdict outside the fixed vocabulary — fails closed without
    /// echoing the unknown value.
    UnknownVerdict,
}

impl ViolationCode {
    /// Fixed vocabulary for reports and tests.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UnresolvedPark => "unresolved_park",
            Self::TerminalParked => "terminal_parked",
            Self::BudgetRegression => "budget_regression",
            Self::UnresolvedScan => "unresolved_scan",
            Self::ExecutedAfterDeny => "executed_after_deny",
            Self::UnknownVerdict => "unknown_verdict",
        }
    }
}

/// One self-audit finding: a fixed-vocabulary code plus integer counters
/// only. There is deliberately no string payload — hostile content in the
/// trail or in a verdict cannot echo through a violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Violation {
    /// Which invariant failed (fixed vocabulary).
    pub code: ViolationCode,
    /// Integer subject marker (verdict index, open-park count, snapshot
    /// index), if the check names one.
    pub subject: Option<u64>,
}

impl Display for Violation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self.subject {
            Some(subject) => write!(
                formatter,
                "self-audit violation: {} (subject #{subject})",
                self.code.as_str()
            ),
            None => write!(formatter, "self-audit violation: {}", self.code.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// Recorded verdicts (produced by dispatch/runner, consumed read-only)
// ---------------------------------------------------------------------------

/// One recorded gate decision aligned to an approval park: the fixed
/// `approved` / `denied` / `cancelled` vocabulary from
/// [`GateOutcome`](super::governance::GateOutcome). `Debug` omits the string
/// field so a hostile verdict value can never leak through formatting.
pub(crate) struct GateDecision {
    /// Gate verdict vocabulary (`approved` / `denied` / `cancelled`).
    pub outcome: &'static str,
}

impl Debug for GateDecision {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str("GateDecision { .. }")
    }
}

/// One recorded envelope-scan verdict (WS-C.2): the signal family plus how it
/// resolved. Consumed as recorded — this pass never calls
/// [`scan_observation`](super::injection::scan_observation) itself.
/// `Debug` renders only the resolution booleans, never the signal string.
pub(crate) struct ScanVerdict {
    /// Signal family vocabulary (`boundary` / `imperative` / `mimicry`).
    pub signal: &'static str,
    /// Whether the hit parked through the approval gate.
    pub parked: bool,
    /// Whether the hit was cleared (reviewer decision / benign re-check).
    pub cleared: bool,
}

impl Debug for ScanVerdict {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter
            .debug_struct("ScanVerdict")
            .field("parked", &self.parked)
            .field("cleared", &self.cleared)
            .finish_non_exhaustive()
    }
}

/// One recorded permission verdict with its execution outcome and audit-order
/// markers. `Debug` renders only the booleans, never identifiers.
pub(crate) struct PermissionVerdict {
    /// Whether the permission store denied the call.
    pub denied: bool,
    /// Whether the call was executed anyway.
    pub executed: bool,
    /// Audit seq of the deny decision, if the producer tracks one.
    pub deny_seq: Option<u64>,
    /// Audit seq of the execution, if the producer tracks one.
    pub execute_seq: Option<u64>,
}

impl Debug for PermissionVerdict {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter
            .debug_struct("PermissionVerdict")
            .field("denied", &self.denied)
            .field("executed", &self.executed)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Self-audit outcome: pass when no violation was found, fail otherwise.
/// `Debug` is safe to log: codes are fixed vocabulary, subjects are counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelfAuditReport {
    violations: Vec<Violation>,
}

impl SelfAuditReport {
    /// Whether every invariant held.
    #[must_use]
    pub(crate) const fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// Findings in check order (empty on pass).
    #[must_use]
    pub(crate) fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Whether the report contains a violation with `code`.
    #[must_use]
    pub(crate) fn has_code(&self, code: ViolationCode) -> bool {
        self.violations.iter().any(|finding| finding.code == code)
    }
}

// ---------------------------------------------------------------------------
// The pass (pure, read-only)
// ---------------------------------------------------------------------------

/// Recorded scan-signal vocabulary (`InjectionSignal::as_str`).
const KNOWN_SIGNALS: &[&str] = &["boundary", "imperative", "mimicry"];

/// Recorded gate-outcome vocabulary (`GateOutcome::as_str`).
const KNOWN_OUTCOMES: &[&str] = &["approved", "denied", "cancelled"];

/// Trail park/resolution tallies.
struct ParkCounts {
    approval_parked: u64,
    approval_resolved: u64,
    budget_parked: u64,
    budget_resolved: u64,
}

fn count_parks(entries: &[super::governance::AuditEntry]) -> ParkCounts {
    let mut counts = ParkCounts {
        approval_parked: 0,
        approval_resolved: 0,
        budget_parked: 0,
        budget_resolved: 0,
    };
    for entry in entries {
        match entry.event {
            "approval_parked" => counts.approval_parked = counts.approval_parked.saturating_add(1),
            "approval_resolved" | "approval_cancelled" => {
                counts.approval_resolved = counts.approval_resolved.saturating_add(1);
            }
            "budget_parked" => counts.budget_parked = counts.budget_parked.saturating_add(1),
            "budget_resumed" | "budget_cancelled" => {
                counts.budget_resolved = counts.budget_resolved.saturating_add(1);
            }
            _ => {}
        }
    }
    counts
}

/// Every park resolves to a recorded decision: trail parks against trail
/// resolutions, and approval parks against the gate-decision list.
fn check_parks(violations: &mut Vec<Violation>, counts: &ParkCounts, gates: &[GateDecision]) {
    if counts.approval_parked > counts.approval_resolved {
        violations.push(Violation {
            code: ViolationCode::UnresolvedPark,
            subject: Some(
                counts
                    .approval_parked
                    .saturating_sub(counts.approval_resolved),
            ),
        });
    }
    if counts.budget_parked > counts.budget_resolved {
        violations.push(Violation {
            code: ViolationCode::UnresolvedPark,
            subject: Some(counts.budget_parked.saturating_sub(counts.budget_resolved)),
        });
    }
    let gate_count = u64::try_from(gates.len()).unwrap_or(u64::MAX);
    if counts.approval_parked > gate_count {
        violations.push(Violation {
            code: ViolationCode::UnresolvedPark,
            subject: Some(counts.approval_parked.saturating_sub(gate_count)),
        });
    }
}

/// No unresolved `Awaiting*` at terminal: an open park (or a trail still
/// pointing at a park state) contradicts a claimed terminal state.
fn check_terminal(
    violations: &mut Vec<Violation>,
    entries: &[super::governance::AuditEntry],
    counts: &ParkCounts,
    terminal: Option<RunState>,
) {
    if !terminal.is_some_and(RunState::is_terminal) {
        return;
    }
    let approval_open = counts
        .approval_parked
        .saturating_sub(counts.approval_resolved);
    let budget_open = counts.budget_parked.saturating_sub(counts.budget_resolved);
    let trail_still_parked = entries
        .last()
        .is_some_and(|entry| entry.to == "awaiting_approval" || entry.to == "awaiting_budget");
    if approval_open > 0 || budget_open > 0 || trail_still_parked {
        violations.push(Violation {
            code: ViolationCode::TerminalParked,
            subject: Some(approval_open.saturating_add(budget_open)),
        });
    }
}

/// Snapshot budget monotonicity: capture-ordered counters never regress
/// (rollback restores the element-wise maximum), and no audit marker may
/// reach past the trail.
fn check_snapshots(violations: &mut Vec<Violation>, snapshots: &SnapshotStore, trail_len: usize) {
    let mut previous_steps: Option<usize> = None;
    let mut previous_spent: Option<u64> = None;
    for (index, snapshot) in snapshots.snapshots().iter().enumerate() {
        let subject = Some(u64::try_from(index).unwrap_or(u64::MAX));
        if previous_steps.is_some_and(|previous| snapshot.steps_taken < previous)
            || previous_spent.is_some_and(|previous| snapshot.spent_micro_usd < previous)
            || snapshot.audit_len > trail_len
        {
            violations.push(Violation {
                code: ViolationCode::BudgetRegression,
                subject,
            });
        }
        previous_steps = Some(snapshot.steps_taken);
        previous_spent = Some(snapshot.spent_micro_usd);
    }
}

/// Envelope scan verdicts: every recorded hit resolved (parked or cleared);
/// unknown signal vocabulary fails closed without echo.
fn check_scans(violations: &mut Vec<Violation>, scans: &[ScanVerdict]) {
    for (index, scan) in scans.iter().enumerate() {
        let subject = Some(u64::try_from(index).unwrap_or(u64::MAX));
        if !KNOWN_SIGNALS.contains(&scan.signal) {
            violations.push(Violation {
                code: ViolationCode::UnknownVerdict,
                subject,
            });
        } else if !(scan.parked || scan.cleared) {
            violations.push(Violation {
                code: ViolationCode::UnresolvedScan,
                subject,
            });
        }
    }
}

/// Permission denials honored: a denied call executed afterwards is a
/// violation. The audit-order markers decide order — execution after the
/// deny confirms it; absent markers fail closed; execution recorded at or
/// before the deny is not an execute-after-deny.
fn check_permissions(violations: &mut Vec<Violation>, permissions: &[PermissionVerdict]) {
    for (index, verdict) in permissions.iter().enumerate() {
        if verdict.denied && verdict.executed {
            let executed_after_deny = match (verdict.deny_seq, verdict.execute_seq) {
                (Some(deny), Some(executed)) => executed > deny,
                (Some(_) | None, None) | (None, Some(_)) => true,
            };
            if executed_after_deny {
                violations.push(Violation {
                    code: ViolationCode::ExecutedAfterDeny,
                    subject: Some(u64::try_from(index).unwrap_or(u64::MAX)),
                });
            }
        }
    }
}

/// Fail-closed on unknown gate verdicts (never echoed).
fn check_gate_vocabulary(violations: &mut Vec<Violation>, gates: &[GateDecision]) {
    for (index, gate) in gates.iter().enumerate() {
        if !KNOWN_OUTCOMES.contains(&gate.outcome) {
            violations.push(Violation {
                code: ViolationCode::UnknownVerdict,
                subject: Some(u64::try_from(index).unwrap_or(u64::MAX)),
            });
        }
    }
}

/// Review a run's own trail against the security invariants.
///
/// Read-only over every input: the audit entries are snapshotted, the
/// snapshot store is borrowed, and the verdict lists are only matched
/// against fixed vocabulary (never re-scanned, never echoed). Returns
/// [`SelfAuditReport::passed`] on a clean run, or one [`Violation`] per
/// failed check otherwise.
///
/// Checks, in order: every park has a recorded decision (trail + gate
/// list); no open park at a claimed terminal; snapshot budget monotonicity;
/// every scan verdict resolved (parked or cleared); every permission denial
/// honored (execute-after-deny via the audit-order markers); every verdict
/// inside the fixed vocabulary (fail-closed on unknown).
#[must_use]
pub(crate) fn audit_run(
    log: &AuditLog,
    snapshots: &SnapshotStore,
    gates: &[GateDecision],
    scans: &[ScanVerdict],
    permissions: &[PermissionVerdict],
    terminal: Option<RunState>,
) -> SelfAuditReport {
    let mut violations: Vec<Violation> = Vec::new();
    let entries = log.entries();
    let counts = count_parks(&entries);
    check_parks(&mut violations, &counts, gates);
    check_terminal(&mut violations, &entries, &counts, terminal);
    check_snapshots(&mut violations, snapshots, entries.len());
    check_scans(&mut violations, scans);
    check_permissions(&mut violations, permissions);
    check_gate_vocabulary(&mut violations, gates);
    SelfAuditReport { violations }
}

// ---------------------------------------------------------------------------
// Outcome recording (opt-in write path)
// ---------------------------------------------------------------------------

/// Append the self-audit outcome to the trail on the caller's genuine edge.
///
/// Pass appends one `self_audit_passed` entry; failure appends one
/// `self_audit_failed` entry per violation (fixed vocabulary, secret-free)
/// and the caller terminates the run through the existing `Failed` terminal
/// — no new terminal state. The edge goes through
/// [`transition`](super::lifecycle::transition) via [`AuditLog::record`]:
/// pass typically records on `(Running, Completed)`, failure on
/// `(Running, Failed)`.
///
/// This is the `self_audit` feature-flag gate in its enabled form; see
/// [`record_report_gated`] for the flag-off behavior.
///
/// # Errors
///
/// Returns the secret-free [`LifecycleError`] when the witness edge is
/// illegal; appends nothing.
pub(crate) fn record_report(
    log: &AuditLog,
    from: RunState,
    to: RunState,
    report: &SelfAuditReport,
) -> Result<(), LifecycleError> {
    record_report_gated(log, from, to, report, true).map(|_| ())
}

/// Append the self-audit outcome only when `enabled` (the `self_audit`
/// feature flag): disabled reproduces the pre-2.0 behavior (the pass stays
/// purely read-only — nothing is appended) and reports `Ok(false)`, while
/// enabled behaves exactly like [`record_report`] and reports `Ok(true)`.
///
/// # Errors
///
/// Returns the secret-free [`LifecycleError`] when enabled and the witness
/// edge is illegal; appends nothing.
pub(crate) fn record_report_gated(
    log: &AuditLog,
    from: RunState,
    to: RunState,
    report: &SelfAuditReport,
    enabled: bool,
) -> Result<bool, LifecycleError> {
    if !enabled {
        return Ok(false);
    }
    if report.passed() {
        log.record(from, to, AuditEvent::SelfAuditPassed)?;
    } else {
        for _ in report.violations() {
            log.record(from, to, AuditEvent::SelfAuditFailed)?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::governance::{AuditEvent, AuditLog};
    use super::super::lifecycle::RunState;
    use super::super::roles::AgentRole;
    use super::super::snapshots::RunPosition;
    use super::*;

    fn position(steps: usize, spent: u64) -> RunPosition {
        RunPosition {
            stage_index: 0,
            role: AgentRole::Executor,
            state: RunState::Running,
            steps_taken: steps,
            spent_micro_usd: spent,
        }
    }

    fn capture(store: &mut SnapshotStore, log: &AuditLog, steps: usize, spent: u64) {
        store
            .capture(
                log,
                RunState::Queued,
                RunState::Running,
                position(steps, spent),
            )
            .expect("capture on a legal witness edge");
    }

    fn park_approve(log: &AuditLog) {
        log.record(
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        )
        .expect("legal park");
        log.record(
            RunState::AwaitingApproval,
            RunState::Running,
            AuditEvent::ApprovalResolved { approved: true },
        )
        .expect("legal resolve");
    }

    fn clean_inputs() -> (AuditLog, SnapshotStore) {
        (AuditLog::new(), SnapshotStore::new(1))
    }

    fn approved_gate() -> Vec<GateDecision> {
        vec![GateDecision {
            outcome: "approved",
        }]
    }

    #[test]
    fn clean_run_passes_and_records_passed() {
        let (log, mut store) = clean_inputs();
        park_approve(&log);
        capture(&mut store, &log, 1, 100);
        capture(&mut store, &log, 2, 250);
        let scans = vec![
            ScanVerdict {
                signal: "boundary",
                parked: true,
                cleared: false,
            },
            ScanVerdict {
                signal: "imperative",
                parked: false,
                cleared: true,
            },
        ];
        let permissions = vec![
            PermissionVerdict {
                denied: false,
                executed: true,
                deny_seq: None,
                execute_seq: Some(4),
            },
            PermissionVerdict {
                denied: true,
                executed: false,
                deny_seq: Some(1),
                execute_seq: None,
            },
        ];
        let report = audit_run(
            &log,
            &store,
            &approved_gate(),
            &scans,
            &permissions,
            Some(RunState::Completed),
        );
        assert!(report.passed(), "clean run passes, got {report:?}");
        assert!(report.violations().is_empty());

        record_report(&log, RunState::Running, RunState::Completed, &report)
            .expect("pass records on the completion edge");
        let entries = log.entries();
        assert_eq!(
            entries.last().expect("outcome entry").event,
            "self_audit_passed"
        );
    }

    #[test]
    fn self_audit_is_read_only() {
        let (log, mut store) = clean_inputs();
        park_approve(&log);
        capture(&mut store, &log, 1, 100);
        let log_len = log.len();
        let store_len = store.len();
        let report = audit_run(
            &log,
            &store,
            &approved_gate(),
            &[],
            &[],
            Some(RunState::Completed),
        );
        assert!(report.passed());
        assert_eq!(log.len(), log_len, "the pass must not append");
        assert_eq!(store.len(), store_len, "the pass must not capture");
    }

    #[test]
    fn unresolved_approval_park_fails_with_its_code() {
        let (log, store) = clean_inputs();
        log.record(
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        )
        .expect("legal park");
        let report = audit_run(&log, &store, &[], &[], &[], None);
        assert!(!report.passed());
        assert!(report.has_code(ViolationCode::UnresolvedPark));

        record_report(&log, RunState::Running, RunState::Failed, &report)
            .expect("failure records on the Failed edge");
        let entries = log.entries();
        assert_eq!(
            entries.last().expect("outcome entry").event,
            "self_audit_failed"
        );
        assert_eq!(entries.last().expect("outcome entry").to, "error");
    }

    #[test]
    fn unresolved_budget_park_fails_with_its_code() {
        let (log, store) = clean_inputs();
        log.record(
            RunState::Running,
            RunState::AwaitingBudget,
            AuditEvent::BudgetParked { allowance: 2 },
        )
        .expect("legal park");
        let report = audit_run(&log, &store, &[], &[], &[], None);
        assert!(report.has_code(ViolationCode::UnresolvedPark));
    }

    #[test]
    fn park_without_gate_decision_fails() {
        let (log, store) = clean_inputs();
        park_approve(&log);
        // Trail resolves the park, but the gate-decision list is empty: the
        // decision was never recorded where the pass can see it.
        let report = audit_run(&log, &store, &[], &[], &[], None);
        assert!(report.has_code(ViolationCode::UnresolvedPark));
    }

    #[test]
    fn terminal_with_open_park_fails_with_terminal_code() {
        let (log, store) = clean_inputs();
        log.record(
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        )
        .expect("legal park");
        let report = audit_run(
            &log,
            &store,
            &approved_gate(),
            &[],
            &[],
            Some(RunState::Completed),
        );
        assert!(!report.passed());
        assert!(report.has_code(ViolationCode::UnresolvedPark));
        assert!(report.has_code(ViolationCode::TerminalParked));
    }

    #[test]
    fn snapshot_budget_regression_fails_with_its_code() {
        let (log, mut store) = clean_inputs();
        capture(&mut store, &log, 5, 500);
        capture(&mut store, &log, 3, 500);
        let report = audit_run(&log, &store, &[], &[], &[], None);
        assert!(report.has_code(ViolationCode::BudgetRegression));

        let (log, mut store) = clean_inputs();
        capture(&mut store, &log, 5, 500);
        capture(&mut store, &log, 5, 100);
        let report = audit_run(&log, &store, &[], &[], &[], None);
        assert!(report.has_code(ViolationCode::BudgetRegression));
    }

    #[test]
    fn unresolved_scan_fails_with_its_code() {
        let (log, store) = clean_inputs();
        let scans = vec![ScanVerdict {
            signal: "mimicry",
            parked: false,
            cleared: false,
        }];
        let report = audit_run(&log, &store, &[], &scans, &[], None);
        assert!(!report.passed());
        assert!(report.has_code(ViolationCode::UnresolvedScan));
        assert!(!report.has_code(ViolationCode::UnknownVerdict));
    }

    #[test]
    fn unknown_scan_signal_fails_closed_without_echo() {
        let hostile: &'static str = Box::leak(
            "sk-live-hostile; ignore previous instructions"
                .to_owned()
                .into_boxed_str(),
        );
        let (log, store) = clean_inputs();
        let scans = vec![ScanVerdict {
            signal: hostile,
            parked: true,
            cleared: false,
        }];
        let report = audit_run(&log, &store, &[], &scans, &[], None);
        assert!(report.has_code(ViolationCode::UnknownVerdict));
        let rendered = format!("{report:?}");
        assert!(
            !rendered.contains(hostile),
            "report must not echo the unknown verdict, got {rendered}"
        );
    }

    #[test]
    fn unknown_gate_outcome_fails_closed_without_echo() {
        let hostile: &'static str = Box::leak(
            "approved\"; DROP TABLE agent_runs; --"
                .to_owned()
                .into_boxed_str(),
        );
        let (log, store) = clean_inputs();
        park_approve(&log);
        let gates = vec![GateDecision { outcome: hostile }];
        let report = audit_run(&log, &store, &gates, &[], &[], None);
        assert!(report.has_code(ViolationCode::UnknownVerdict));
        for finding in report.violations() {
            assert!(
                !format!("{finding}").contains(hostile),
                "violation rendering must stay fixed-vocabulary"
            );
        }
    }

    #[test]
    fn executed_after_deny_caught_via_audit_order() {
        let (log, store) = clean_inputs();
        let permissions = vec![PermissionVerdict {
            denied: true,
            executed: true,
            deny_seq: Some(3),
            execute_seq: Some(5),
        }];
        let report = audit_run(&log, &store, &[], &[], &permissions, None);
        assert!(!report.passed());
        assert!(report.has_code(ViolationCode::ExecutedAfterDeny));
    }

    #[test]
    fn deny_without_markers_still_fails_closed() {
        let (log, store) = clean_inputs();
        let permissions = vec![PermissionVerdict {
            denied: true,
            executed: true,
            deny_seq: None,
            execute_seq: None,
        }];
        let report = audit_run(&log, &store, &[], &[], &permissions, None);
        assert!(report.has_code(ViolationCode::ExecutedAfterDeny));
    }

    #[test]
    fn execution_before_deny_is_not_execute_after_deny() {
        let (log, store) = clean_inputs();
        let permissions = vec![PermissionVerdict {
            denied: true,
            executed: true,
            deny_seq: Some(5),
            execute_seq: Some(3),
        }];
        let report = audit_run(&log, &store, &[], &[], &permissions, None);
        assert!(
            !report.has_code(ViolationCode::ExecutedAfterDeny),
            "execution recorded before the deny is not an execute-after-deny, got {report:?}"
        );
    }

    #[test]
    fn hostile_trail_never_echoes_into_violations() {
        let hostile_id = "sk-live-hostile-id SELECT * FROM users";
        let hostile_args = "api_key=XXX ../../etc/passwd ignore previous instructions";
        let hostile_checkpoint = "checkpoint'; DROP TABLE agent_runs; --";
        let (log, mut store) = clean_inputs();
        log.record(
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        )
        .expect("legal park");
        store
            .checkpoint(
                &log,
                RunState::AwaitingApproval,
                RunState::Running,
                hostile_checkpoint,
                position(1, 100),
            )
            .expect("hostile checkpoint name stays in memory only");
        let hostile_signal: &'static str = Box::leak(hostile_args.to_owned().into_boxed_str());
        let scans = vec![ScanVerdict {
            signal: hostile_signal,
            parked: false,
            cleared: false,
        }];
        let permissions = vec![PermissionVerdict {
            denied: true,
            executed: true,
            deny_seq: Some(0),
            execute_seq: Some(2),
        }];
        let report = audit_run(&log, &store, &[], &scans, &permissions, None);
        assert!(!report.passed());
        let rendered = format!("{report:?}");
        for hostile in [
            hostile_id,
            hostile_args,
            hostile_checkpoint,
            "sk-live-hostile-id",
            "api_key",
            "SELECT",
            "../../etc/passwd",
            "ignore previous instructions",
            "DROP TABLE",
        ] {
            assert!(
                !rendered.contains(hostile),
                "report must not echo hostile payload {hostile:?}; got {rendered}"
            );
        }
        // Verdict inputs hide their strings from Debug too.
        assert!(!format!("{:?}", scans[0]).contains(hostile_args));
        assert!(!format!("{:?}", permissions[0]).contains(hostile_args));
        // Failure entries on the Failed edge carry fixed vocabulary only.
        record_report(&log, RunState::Running, RunState::Failed, &report).expect("failure records");
        let dump = format!("{:?}", log.entries());
        for hostile in [hostile_args, hostile_checkpoint, "DROP TABLE"] {
            assert!(
                !dump.contains(hostile),
                "trail must not echo hostile payload {hostile:?}"
            );
        }
    }

    #[test]
    fn record_on_illegal_edge_fails_secret_free_and_appends_nothing() {
        let (log, store) = clean_inputs();
        park_approve(&log);
        let before = log.len();
        let report = audit_run(
            &log,
            &store,
            &approved_gate(),
            &[],
            &[],
            Some(RunState::Completed),
        );
        assert!(report.passed());
        let err = record_report(&log, RunState::Cancelled, RunState::Running, &report)
            .expect_err("terminal states have no exits");
        let rendered = format!("{err}");
        assert!(rendered.contains("cancelled"));
        assert!(rendered.contains("running"));
        assert_eq!(log.len(), before, "illegal edges append nothing");
    }

    #[test]
    fn violation_codes_are_fixed_vocabulary() {
        assert_eq!(ViolationCode::UnresolvedPark.as_str(), "unresolved_park");
        assert_eq!(ViolationCode::TerminalParked.as_str(), "terminal_parked");
        assert_eq!(
            ViolationCode::BudgetRegression.as_str(),
            "budget_regression"
        );
        assert_eq!(ViolationCode::UnresolvedScan.as_str(), "unresolved_scan");
        assert_eq!(
            ViolationCode::ExecutedAfterDeny.as_str(),
            "executed_after_deny"
        );
        assert_eq!(ViolationCode::UnknownVerdict.as_str(), "unknown_verdict");
        let finding = Violation {
            code: ViolationCode::ExecutedAfterDeny,
            subject: Some(2),
        };
        assert_eq!(
            format!("{finding}"),
            "self-audit violation: executed_after_deny (subject #2)"
        );
        let bare = Violation {
            code: ViolationCode::UnresolvedPark,
            subject: None,
        };
        assert_eq!(format!("{bare}"), "self-audit violation: unresolved_park");
    }

    #[test]
    fn gated_record_off_appends_nothing_and_on_matches_record() {
        // Flag OFF reproduces the pre-2.0 behavior: the pass stays purely
        // read-only. Flag ON behaves exactly like `record_report`.
        let (log, mut store) = clean_inputs();
        park_approve(&log);
        capture(&mut store, &log, 1, 100);
        let report = audit_run(
            &log,
            &store,
            &approved_gate(),
            &[],
            &[],
            Some(RunState::Completed),
        );
        assert!(report.passed());
        let before = log.len();
        assert!(
            !record_report_gated(&log, RunState::Running, RunState::Completed, &report, false)
                .expect("disabled record succeeds"),
            "disabled record reports nothing appended"
        );
        assert_eq!(log.len(), before, "disabled record appends nothing");
        assert!(
            record_report_gated(&log, RunState::Running, RunState::Completed, &report, true)
                .expect("enabled record succeeds"),
            "enabled record reports the append"
        );
        assert_eq!(log.len(), before + 1);
        let entries = log.entries();
        assert_eq!(
            entries.last().expect("outcome entry").event,
            "self_audit_passed"
        );
    }
}
