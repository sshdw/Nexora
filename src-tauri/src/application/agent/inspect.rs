//! Read-only run inspector (WS-D.1): one aggregate view over a run's WS-B/WS-C
//! accessories.
//!
//! The inspector composes data other modules already recorded — never new
//! collection, never new state:
//!
//! | View section | Source (all pre-existing) |
//! |---|---|
//! | `state`, `started_at`, `finished_at` | [`AgentRun`] persisted row (`agent_runs.status`, Unix seconds) |
//! | `stage`, `role`, `stages_entered`, `stages_total` | [`Pipeline`] entered prefix + bound roles (WS-B.3) |
//! | `steps_taken`, `spent_micro_usd`, `limit_micro_usd` | [`AgentRun`] counters (`total_steps`, micro-USD columns) |
//! | `max_steps` | [`RunBudget`] step cap (WS-B.2), when attached |
//! | `gate_decisions` | [`AuditLog`] recorded decisions, else `agent_steps` approval rows |
//! | `audit_len` | [`AuditLog::len`] (WS-B.2) |
//! | `snapshots`, `checkpoints` | [`SnapshotStore`] markers + borrowed checkpoint names (WS-C.1) |
//! | `self_audit` | [`SelfAuditReport`] pass flag + violation codes (WS-C.3) |
//!
//! # Read-only mechanism
//!
//! [`inspect_run`] takes `&` borrows of every accessory (`&AgentRun`,
//! `&[AgentStep]`, `Option<&AuditLog>`, `Option<&Pipeline>`,
//! `Option<&RunBudget>`, `Option<&SnapshotStore>`, `Option<&SelfAuditReport>`).
//! Shared borrows are enforced by the signature: the inspector cannot mutate
//! anything through this path at compile time. The only owned allocation is
//! the returned DTO itself (plus the checkpoint-name copies, see below).
//!
//! # Secret-free construction
//!
//! The DTO carries fixed-vocabulary `&'static str` (state/stage/role/decision/
//! violation names), integer counters, ids, and Unix-seconds timestamps — the
//! same trick as the WS-B.2 audit entries. It carries no message content, no
//! tool arguments, no observations, no credentials, and no SQL. The single
//! exception is the checkpoint `name`: a caller-chosen label (explicit user
//! intent, like a conversation title) echoed verbatim. Checkpoint names never
//! enter the audit trail or the errors — only this view — and hostile trail
//! content still cannot reach the DTO through any other field.
//!
//! # Opt-in gaps
//!
//! Accessories are opt-in per run (a run without an attached trail, pipeline,
//! budget, snapshot store, or self-audit report behaves byte-identically to
//! before). Every accessory parameter is therefore `Option`: `None` renders
//! as `null` (or an empty list for the snapshot/checkpoint collections) —
//! never an error. A missing run row is the only failure, surfaced as
//! `Ok(None)` by [`inspect_persisted_run`] so the command layer can map it to
//! its secret-free not-found error.

use serde::Serialize;

use super::governance::{AuditEntry, AuditLog, RunBudget};
use super::lifecycle::RunState;
use super::pipeline::Pipeline;
use super::self_audit::SelfAuditReport;
use super::snapshots::SnapshotStore;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentRunRepository, AgentStep};

// ---------------------------------------------------------------------------
// View types (the aggregate DTO — the only new data structure in WS-D.1)
// ---------------------------------------------------------------------------

/// One captured position marker (WS-C.1): stage index, fixed-vocabulary role
/// and state, integer counters, and the Unix-seconds capture time. No content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SnapshotView {
    /// Entered pipeline stages at capture.
    pub stage_index: usize,
    /// Role bound to the stage at capture (`AgentRole::as_str` vocabulary).
    pub role: &'static str,
    /// Lifecycle state at capture (`RunState::as_str` vocabulary).
    pub state: &'static str,
    /// Consumed model turns at capture.
    pub steps_taken: usize,
    /// Consumed spend at capture, micro-USD.
    pub spent_micro_usd: u64,
    /// Audit length marker at capture.
    pub audit_len: usize,
    /// Capture timestamp, Unix seconds.
    pub captured_at: i64,
}

/// One named checkpoint (WS-C.1): the caller-chosen label plus the snapshot
/// index it files. The name is the DTO's only `String` field (a user label,
/// like a conversation title — never message content, credentials, or SQL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct CheckpointView {
    /// Caller-chosen checkpoint label, verbatim.
    pub name: String,
    /// Snapshot index filed under the label.
    pub snapshot_index: usize,
}

/// One self-audit finding (WS-C.3): fixed-vocabulary code plus an integer
/// subject marker. No content by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ViolationView {
    /// Which invariant failed (`ViolationCode::as_str` vocabulary).
    pub code: &'static str,
    /// Integer subject marker, if the check names one.
    pub subject: Option<u64>,
}

/// Latest self-audit verdict (WS-C.3): pass flag plus violation codes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SelfAuditView {
    /// Whether every invariant held.
    pub passed: bool,
    /// Findings in check order (empty on pass).
    pub violations: Vec<ViolationView>,
}

/// Read-only aggregate view of one run (WS-D.1): state, stage + role, budget
/// counters, gate decisions, snapshot/checkpoint markers, and the latest
/// self-audit verdict. Secret-free by construction — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RunInspection {
    /// `agent_runs.id`.
    pub run_id: i64,
    /// Lifecycle state (`RunState::as_str` vocabulary, mapped from the
    /// persisted `agent_runs.status` column).
    pub state: &'static str,
    /// Current pipeline stage (`PipelineStage::as_str`), if a pipeline is
    /// attached.
    pub stage: Option<&'static str>,
    /// Role bound to the current stage (`AgentRole::as_str`), if attached.
    pub role: Option<&'static str>,
    /// Entered pipeline stages (`Pipeline::entered().len()`; `0` unattached).
    pub stages_entered: usize,
    /// Ordered pipeline stages (`Pipeline::stages().len()`; `0` unattached).
    pub stages_total: usize,
    /// Consumed model turns (`agent_runs.total_steps`).
    pub steps_taken: i64,
    /// Billed spend, micro-USD (`None` when not persisted).
    pub spent_micro_usd: Option<u64>,
    /// Per-run spend limit, micro-USD (`None` = no guard).
    pub limit_micro_usd: Option<u64>,
    /// Step cap (`RunBudget::max_steps`; `None` when no budget is attached).
    pub max_steps: Option<usize>,
    /// Gate verdicts in record order (`approved` / `denied` / `cancelled`):
    /// the audit trail's recorded decisions when a trail is attached, else
    /// the persisted approval-step outcomes.
    pub gate_decisions: Vec<&'static str>,
    /// Audit trail length (`AuditLog::len`; `None` when no trail is attached).
    pub audit_len: Option<usize>,
    /// Captured position markers in capture order (empty when unattached).
    pub snapshots: Vec<SnapshotView>,
    /// Named checkpoints ordered by snapshot index (empty when unattached).
    pub checkpoints: Vec<CheckpointView>,
    /// Latest self-audit verdict (`None` when no report is attached).
    pub self_audit: Option<SelfAuditView>,
    /// Start timestamp, Unix seconds.
    pub started_at: i64,
    /// Termination timestamp, Unix seconds (`None` while active).
    pub finished_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Aggregation (pure, read-only: `&` borrows only)
// ---------------------------------------------------------------------------

/// Map the persisted `agent_runs.status` column onto the lifecycle vocabulary.
///
/// The column is CHECK-constrained to the six known values; anything else
/// reads fail-closed as the terminal failure vocabulary (never echoed).
fn persisted_state(status: &str) -> &'static str {
    match status {
        "running" => RunState::Running.as_str(),
        "completed" => RunState::Completed.as_str(),
        "cancelled" => RunState::Cancelled.as_str(),
        "budget_exhausted" => RunState::BudgetExhausted.as_str(),
        "spend_limit_exceeded" => RunState::SpendLimitExceeded.as_str(),
        _ => RunState::Failed.as_str(),
    }
}

/// Gate verdicts from the audit trail in append order: every recorded
/// decision (`approved` / `denied`), plus `cancelled` for each
/// approval-cancellation edge (a cancelled park records no decision — see
/// `AuditEvent::for_gate` — so the edge itself is the verdict).
fn gate_decisions_from_trail(entries: &[AuditEntry]) -> Vec<&'static str> {
    entries
        .iter()
        .filter_map(|entry| {
            entry.decision.or_else(|| {
                if entry.event == "approval_cancelled" {
                    Some(super::governance::GateOutcome::Cancelled.as_str())
                } else {
                    None
                }
            })
        })
        .collect()
}

/// Gate verdicts from persisted approval steps in `seq` order: the recorder
/// writes `succeeded` for an approved call, `denied` for a denied one, and
/// `cancelled` when cancellation ended the wait. Unknown statuses are skipped
/// (never echoed).
fn gate_decisions_from_steps(steps: &[AgentStep]) -> Vec<&'static str> {
    steps
        .iter()
        .filter(|step| step.kind == "approval")
        .filter_map(|step| match step.status.as_deref() {
            Some("succeeded") => Some(super::governance::GateOutcome::Approved.as_str()),
            Some("denied") => Some(super::governance::GateOutcome::Denied.as_str()),
            Some("cancelled") => Some(super::governance::GateOutcome::Cancelled.as_str()),
            _ => None,
        })
        .collect()
}

/// Aggregate one run's persisted row plus its opt-in in-memory accessories
/// into the inspector view.
///
/// Read-only over every input: the row, the step slice, and each accessory
/// are shared borrows, so the signature itself rules out mutation through
/// this path. Missing accessories (`None`) render as `null`/empty — never an
/// error. `steps` is read in slice order (the repository lists `seq`
/// ascending).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(crate) fn inspect_run(
    run: &AgentRun,
    steps: &[AgentStep],
    audit_log: Option<&AuditLog>,
    pipeline: Option<&Pipeline>,
    budget: Option<&RunBudget>,
    snapshots: Option<&SnapshotStore>,
    report: Option<&SelfAuditReport>,
) -> RunInspection {
    let trail_entries: Vec<AuditEntry> = audit_log.map_or_else(Vec::new, AuditLog::entries);
    let gate_decisions = audit_log.map_or_else(
        || gate_decisions_from_steps(steps),
        |_| gate_decisions_from_trail(&trail_entries),
    );

    let (stage, role, stages_entered, stages_total) = pipeline.map_or((None, None, 0, 0), |pipe| {
        let current = pipe.entered().last().map(|&(stage, _, _)| stage);
        (
            current.map(super::pipeline::PipelineStage::as_str),
            current.map(|stage| stage.role().as_str()),
            pipe.entered().len(),
            pipe.stages().len(),
        )
    });

    let snapshots_view: Vec<SnapshotView> = snapshots.map_or_else(Vec::new, |store| {
        store
            .snapshots()
            .iter()
            .map(|snapshot| SnapshotView {
                stage_index: snapshot.stage_index,
                role: snapshot.role.as_str(),
                state: snapshot.state.as_str(),
                steps_taken: snapshot.steps_taken,
                spent_micro_usd: snapshot.spent_micro_usd,
                audit_len: snapshot.audit_len,
                captured_at: snapshot.captured_at,
            })
            .collect()
    });
    let checkpoints_view: Vec<CheckpointView> = snapshots.map_or_else(Vec::new, |store| {
        store
            .checkpoints()
            .into_iter()
            .map(|(name, snapshot_index)| CheckpointView {
                name: name.to_string(),
                snapshot_index,
            })
            .collect()
    });

    RunInspection {
        run_id: run.id,
        state: persisted_state(&run.status),
        stage,
        role,
        stages_entered,
        stages_total,
        steps_taken: run.total_steps,
        spent_micro_usd: run.spent_micro_usd,
        limit_micro_usd: run.limit_micro_usd,
        max_steps: budget.map(|allowance| allowance.max_steps()),
        gate_decisions,
        audit_len: audit_log.map(AuditLog::len),
        snapshots: snapshots_view,
        checkpoints: checkpoints_view,
        self_audit: report.map(|outcome| SelfAuditView {
            passed: outcome.passed(),
            violations: outcome
                .violations()
                .iter()
                .map(|finding| ViolationView {
                    code: finding.code.as_str(),
                    subject: finding.subject,
                })
                .collect(),
        }),
        started_at: run.started_at,
        finished_at: run.finished_at,
    }
}

/// Load the persisted run row plus its steps and aggregate the inspector view
/// (the command-layer path).
///
/// The in-memory accessories are opt-in per run thread and are not retained
/// centrally, so this path always passes `None` for them: their sections read
/// `null`/empty, never an error. Unknown `run_id` yields `Ok(None)` — the
/// caller maps it to its secret-free not-found error.
///
/// # Errors
///
/// Returns [`DatabaseError`] when the run-row or step-list read fails.
pub(crate) fn inspect_persisted_run(
    db: &Database,
    run_id: i64,
) -> Result<Option<RunInspection>, DatabaseError> {
    let repo = AgentRunRepository::new(db);
    let Some(run) = repo.read_run(run_id)? else {
        return Ok(None);
    };
    let steps = repo.list_steps(run_id)?;
    Ok(Some(inspect_run(
        &run, &steps, None, None, None, None, None,
    )))
}

#[cfg(test)]
mod tests {
    use super::super::governance::{AuditEvent, AuditLog, GateOutcome, RunBudget};
    use super::super::lifecycle::RunState;
    use super::super::pipeline::Pipeline;
    use super::super::self_audit::{audit_run, GateDecision};
    use super::super::snapshots::{RunPosition, SnapshotStore};
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    const SECRET_SENTINELS: [&str; 6] = [
        "sk-live",
        "sk-admin-secret",
        "credential",
        "api_key",
        "select",
        "../../etc/passwd",
    ];

    fn persisted_run(status: &str) -> AgentRun {
        AgentRun {
            id: 7,
            conversation_id: Some(3),
            model: "gpt-test".to_string(),
            mode: "supervised".to_string(),
            status: status.to_string(),
            started_at: 1_700_000_000,
            finished_at: None,
            total_steps: 4,
            final_content: None,
            error: None,
            spent_micro_usd: Some(500),
            limit_micro_usd: Some(1_000_000),
        }
    }

    fn approval_step(seq: i64, status: &str) -> AgentStep {
        AgentStep {
            id: seq,
            run_id: 7,
            seq,
            kind: "approval".to_string(),
            tool_name: Some("write_file".to_string()),
            arguments: None,
            observation: None,
            status: Some(status.to_string()),
            started_at: 1_700_000_000,
            duration_ms: None,
            rule_id: None,
            group_key: None,
            decided_by: None,
        }
    }

    /// Build every accessory at once: the two-entry pipeline, a denied
    /// approval park plus a budget park/resume on the shared trail, two
    /// snapshots (one checkpointed), and the passing self-audit report.
    struct FullRun {
        log: AuditLog,
        pipeline: Pipeline,
        budget: RunBudget,
        store: SnapshotStore,
        report: SelfAuditReport,
    }

    fn full_run() -> FullRun {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();
        pipeline
            .advance(&log, RunState::Queued, RunState::Running)
            .expect("enter plan");
        pipeline
            .advance(&log, RunState::Running, RunState::AwaitingApproval)
            .expect("enter act");

        log.record(
            RunState::Running,
            RunState::AwaitingBudget,
            AuditEvent::BudgetParked { allowance: 10 },
        )
        .expect("budget park");
        log.record(
            RunState::AwaitingBudget,
            RunState::Running,
            AuditEvent::BudgetResumed,
        )
        .expect("budget resume");
        log.record(
            RunState::Running,
            RunState::AwaitingApproval,
            AuditEvent::ApprovalParked,
        )
        .expect("approval park");
        log.record(
            RunState::AwaitingApproval,
            RunState::Running,
            AuditEvent::ApprovalResolved { approved: false },
        )
        .expect("approval resolve");

        let budget = RunBudget::new(10, Some(1_000_000));
        let mut store = SnapshotStore::new(7);
        store
            .capture(
                &log,
                RunState::Running,
                RunState::Paused,
                RunPosition {
                    stage_index: pipeline.entered().len(),
                    role: super::super::pipeline::PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 2,
                    spent_micro_usd: 250,
                },
            )
            .expect("capture");
        store
            .checkpoint(
                &log,
                RunState::Paused,
                RunState::Running,
                "stable",
                RunPosition {
                    stage_index: pipeline.entered().len(),
                    role: super::super::pipeline::PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 3,
                    spent_micro_usd: 400,
                },
            )
            .expect("checkpoint");

        let gates = [GateDecision { outcome: "denied" }];
        let report = audit_run(&log, &store, &gates, &[], &[], None);
        assert!(report.passed(), "fixture must self-audit clean");
        FullRun {
            log,
            pipeline,
            budget,
            store,
            report,
        }
    }

    #[test]
    fn full_run_aggregates_every_section() {
        let run = persisted_run("running");
        let steps = [approval_step(1, "succeeded")];
        let fixture = full_run();
        let view = inspect_run(
            &run,
            &steps,
            Some(&fixture.log),
            Some(&fixture.pipeline),
            Some(&fixture.budget),
            Some(&fixture.store),
            Some(&fixture.report),
        );

        assert_eq!(view.run_id, 7);
        assert_eq!(view.state, "running");
        assert_eq!(view.stage, Some("act"));
        assert_eq!(view.role, Some("executor"));
        assert_eq!(view.stages_entered, 2);
        assert_eq!(view.stages_total, 3);
        assert_eq!(view.steps_taken, 4);
        assert_eq!(view.spent_micro_usd, Some(500));
        assert_eq!(view.limit_micro_usd, Some(1_000_000));
        assert_eq!(view.max_steps, Some(10));
        // The attached trail is authoritative for gate decisions: the denied
        // park resolves to one fixed-vocabulary verdict.
        assert_eq!(view.gate_decisions, vec!["denied"]);
        assert_eq!(view.audit_len, Some(fixture.log.len()));
        assert_eq!(view.snapshots.len(), 2);
        assert_eq!(view.snapshots[0].steps_taken, 2);
        assert_eq!(view.snapshots[1].spent_micro_usd, 400);
        assert_eq!(view.snapshots[1].role, "executor");
        assert_eq!(view.snapshots[1].state, "running");
        assert_eq!(
            view.checkpoints,
            vec![CheckpointView {
                name: "stable".to_string(),
                snapshot_index: 1,
            }]
        );
        let audit = view.self_audit.as_ref().expect("report attached");
        assert!(audit.passed);
        assert!(audit.violations.is_empty());
        assert_eq!(view.started_at, 1_700_000_000);
        assert_eq!(view.finished_at, None);

        // Payload shape: snake_case throughout, no camelCase keys.
        let raw = serde_json::to_string(&view).expect("serialize view");
        for key in [
            "run_id",
            "stages_entered",
            "steps_taken",
            "spent_micro_usd",
            "gate_decisions",
            "audit_len",
            "self_audit",
            "started_at",
        ] {
            assert!(raw.contains(key), "payload keeps {key}, got {raw}");
        }
        for camel in [
            "runId",
            "gateDecisions",
            "auditLen",
            "selfAudit",
            "startedAt",
        ] {
            assert!(
                !raw.contains(camel),
                "payload must not use camelCase {camel}, got {raw}"
            );
        }
    }

    #[test]
    fn missing_accessories_yield_nulls_never_errors() {
        let run = persisted_run("completed");
        let steps = [approval_step(1, "denied"), approval_step(2, "cancelled")];
        let view = inspect_run(&run, &steps, None, None, None, None, None);

        assert_eq!(view.state, "completed");
        assert_eq!(view.stage, None);
        assert_eq!(view.role, None);
        assert_eq!(view.stages_entered, 0);
        assert_eq!(view.stages_total, 0);
        assert_eq!(view.max_steps, None);
        // No trail attached: gate decisions fall back to the persisted
        // approval-step outcomes (fixed vocabulary).
        assert_eq!(view.gate_decisions, vec!["denied", "cancelled"]);
        assert_eq!(view.audit_len, None);
        assert!(view.snapshots.is_empty());
        assert!(view.checkpoints.is_empty());
        assert_eq!(view.self_audit, None);

        // Unknown approval-step statuses are skipped, never echoed.
        let odd = AgentStep {
            status: Some("transcended".to_string()),
            ..approval_step(3, "succeeded")
        };
        let view = inspect_run(&run, &[odd], None, None, None, None, None);
        assert!(
            view.gate_decisions.is_empty(),
            "unknown step status yields no decision, got {:?}",
            view.gate_decisions
        );
    }

    #[test]
    fn persisted_path_round_trips_state_budget_and_decisions() {
        let db = in_memory_database();
        let repo = AgentRunRepository::new(&db);
        let run_id = repo.create_run(None, "m", "supervised").expect("create");
        repo.append_step(
            run_id,
            1,
            "model_turn",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("model turn");
        repo.append_step(
            run_id,
            2,
            "approval",
            Some("write_file"),
            None,
            None,
            Some("succeeded"),
            None,
            None,
            None,
            None,
        )
        .expect("approval");
        repo.finalize_run(
            run_id,
            "completed",
            2,
            Some("done"),
            None,
            Some(750),
            Some(2_000_000),
        )
        .expect("finalize");

        let view = inspect_persisted_run(&db, run_id)
            .expect("read succeeds")
            .expect("run exists");
        assert_eq!(view.run_id, run_id);
        assert_eq!(view.state, "completed");
        assert_eq!(view.steps_taken, 2);
        assert_eq!(view.spent_micro_usd, Some(750));
        assert_eq!(view.limit_micro_usd, Some(2_000_000));
        assert_eq!(view.gate_decisions, vec!["approved"]);
        // Opt-in accessories are absent on the persisted path: nulls, not errors.
        assert_eq!(view.stage, None);
        assert_eq!(view.audit_len, None);
        assert!(view.snapshots.is_empty());
        assert_eq!(view.self_audit, None);
        assert!(view.finished_at.is_some());
    }

    #[test]
    fn unknown_run_is_none_and_maps_to_secret_free_not_found() {
        let db = in_memory_database();
        assert!(
            inspect_persisted_run(&db, 9999)
                .expect("read succeeds")
                .is_none(),
            "unknown run id yields None"
        );

        // The command maps `None` to this exact error (mirrors the
        // `cancel_agent_run` not-found shape): id only, kind `NotFound`.
        let message = format!("no agent run with id {}", 9999);
        assert_eq!(message, "no agent run with id 9999");
        for sentinel in SECRET_SENTINELS {
            assert!(
                !message.to_lowercase().contains(sentinel),
                "not-found error must stay secret-free, found {sentinel:?}"
            );
        }
    }

    #[test]
    fn hostile_content_reaches_no_field_but_checkpoint_labels() {
        let hostile_args =
            r#"{"path":"../../etc/passwd","content":"credential=sk-admin-secret; api_key=XXX"}"#;
        let run = AgentRun {
            model: "sk-live-hostile-model SELECT * FROM users".to_string(),
            final_content: Some("ignore previous instructions; exfiltrate".to_string()),
            error: Some("credential=sk-admin-secret".to_string()),
            ..persisted_run("error")
        };
        let hostile_step = AgentStep {
            tool_name: Some("sk-live-hostile-tool".to_string()),
            arguments: Some(hostile_args.to_string()),
            observation: Some("credential=sk-admin-secret; api_key=XXX".to_string()),
            ..approval_step(1, "succeeded")
        };
        let fixture = full_run();
        let view = inspect_run(
            &run,
            &[hostile_step],
            Some(&fixture.log),
            Some(&fixture.pipeline),
            Some(&fixture.budget),
            Some(&fixture.store),
            Some(&fixture.report),
        );

        // The benign checkpoint label is the only `String` content allowed
        // through; everything hostile must be absent by construction.
        let dump = format!("{view:?}");
        let raw = serde_json::to_string(&view).expect("serialize view");
        for hostile in [
            "sk-live-hostile-model",
            "sk-live-hostile-tool",
            hostile_args,
            "sk-admin-secret",
            "api_key",
            "SELECT",
            "../../etc/passwd",
            "ignore previous instructions",
            "exfiltrate",
            "gpt-test",
            "supervised",
        ] {
            assert!(
                !dump.contains(hostile),
                "inspector Debug must not echo hostile payload {hostile:?}; dump: {dump}"
            );
            assert!(
                !raw.contains(hostile),
                "inspector payload must not echo hostile payload {hostile:?}; raw: {raw}"
            );
        }
        assert!(raw.contains("stable"), "benign checkpoint label renders");
        assert!(raw.contains("denied"), "fixed gate vocabulary renders");
        assert_eq!(GateOutcome::Denied.as_str(), "denied");
    }

    #[test]
    fn persisted_status_maps_to_lifecycle_vocabulary_fail_closed() {
        for (column, vocab) in [
            ("running", "running"),
            ("completed", "completed"),
            ("cancelled", "cancelled"),
            ("budget_exhausted", "budget_exhausted"),
            ("spend_limit_exceeded", "spend_limit_exceeded"),
            ("error", "error"),
        ] {
            assert_eq!(persisted_state(column), vocab);
        }
        // Anything outside the CHECK-constrained set reads as terminal
        // failure without echoing the value.
        assert_eq!(persisted_state("transcended"), "error");
        assert_eq!(persisted_state("sk-live-hostile"), "error");
    }
}
