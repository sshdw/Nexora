//! Run snapshots with rollback and explicit checkpoints (WS-C.1).
//!
//! In-memory run-scoped accessories built on the WS-B.3 pipeline API (same
//! rule as the WS-B.2 audit trail: no new tables). Any run position can be
//! captured, inspected, and restored to a prior stage without re-execution:
//!
//! - [`RunSnapshot`] is pure data: run id, stage index plus the stage's
//!   bound role, lifecycle state, budget consumption counters, the audit
//!   length marker, and the capture timestamp (Unix seconds). It never
//!   carries message content, credentials, or SQL.
//! - [`SnapshotStore::capture`] snapshots the caller's pipeline position at
//!   any stage; [`SnapshotStore::checkpoint`] additionally files it under a
//!   caller-chosen name (explicit user intent); [`SnapshotStore::rollback`]
//!   restores the stage plus the budget counters with monotonic budgets
//!   (rollback never refunds: the restored counters are the element-wise
//!   maximum of the snapshot and the current consumption).
//! - Every capture, checkpoint, and rollback appends one fixed-vocabulary
//!   entry (`snapshot_captured` / `checkpoint_saved` / `rolled_back`) to the
//!   [`AuditLog`] on the genuine lifecycle edge at the point — the same
//!   pattern as [`Pipeline::advance`](super::pipeline::Pipeline::advance).
//!   The witness edge goes through
//!   [`transition`](super::lifecycle::transition) via [`AuditLog::record`]:
//!   an illegal edge fails secret-free and mutates nothing. Rolling back is
//!   an event, never a state: no new [`RunState`](super::lifecycle::RunState)
//!   variant exists.
//! - Checkpoint names never enter the trail or the errors (they may carry
//!   hostile content), so unknown names fail loudly with a fixed-vocabulary
//!   [`SnapshotError`] that echoes nothing.
//!
//! The store never truncates the [`AuditLog`] (append-only) and never touches
//! the runner: the caller applies the returned [`Restored`] position to its
//! own [`Pipeline`](super::pipeline::Pipeline) (via
//! [`rewind`](super::pipeline::Pipeline::rewind)) and budget counters.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::governance::{AuditEvent, AuditLog};
use super::lifecycle::{LifecycleError, RunState};
use super::roles::AgentRole;

// ---------------------------------------------------------------------------
// Snapshot data
// ---------------------------------------------------------------------------

/// One captured run position (WS-C.1): pure data, secret-free by
/// construction. No message content, no credentials, no SQL — only ids,
/// fixed-vocabulary enums, integer counters, and a Unix-seconds timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunSnapshot {
    /// Owning run (`agent_runs.id` vocabulary).
    pub run_id: i64,
    /// Entered pipeline stages at capture (`Pipeline::entered().len()`).
    pub stage_index: usize,
    /// Role bound to the stage at capture (`PipelineStage::role`).
    pub role: AgentRole,
    /// Lifecycle state at capture.
    pub state: RunState,
    /// Consumed model turns at capture.
    pub steps_taken: usize,
    /// Consumed spend at capture, micro-USD.
    pub spent_micro_usd: u64,
    /// Audit length marker: [`AuditLog::len`] just after the capture entry.
    pub audit_len: usize,
    /// Capture timestamp, Unix seconds.
    pub captured_at: i64,
}

/// Restored run position returned by rollback (WS-C.1): the snapshot stage
/// plus the monotonic budget counters the caller must resume with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Restored {
    /// Entered pipeline stages to resume from.
    pub stage_index: usize,
    /// Stage-bound role to resume with.
    pub role: AgentRole,
    /// Lifecycle state to resume with.
    pub state: RunState,
    /// Consumed model turns to resume with (never below the snapshot).
    pub steps_taken: usize,
    /// Consumed spend to resume with, micro-USD (never below the snapshot).
    pub spent_micro_usd: u64,
}

/// Caller pipeline position filed by capture/checkpoint (WS-C.1):
/// `stage_index` is the entered prefix length
/// (`Pipeline::entered().len()`), `role` the stage-bound role, and the rest
/// the caller's current lifecycle state and budget consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunPosition {
    /// Entered pipeline stages at capture.
    pub stage_index: usize,
    /// Role bound to the stage at capture.
    pub role: AgentRole,
    /// Lifecycle state at capture.
    pub state: RunState,
    /// Consumed model turns at capture.
    pub steps_taken: usize,
    /// Consumed spend at capture, micro-USD.
    pub spent_micro_usd: u64,
}

/// Caller consumption at the rollback point (WS-C.1): the position rollback
/// must not reach past, and the floor the monotonic restore keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResumePoint {
    /// Entered pipeline stages now.
    pub stage_index: usize,
    /// Consumed model turns now.
    pub steps_taken: usize,
    /// Consumed spend now, micro-USD.
    pub spent_micro_usd: u64,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// In-memory snapshot accessory for one run: the ordered snapshots plus the
/// checkpoint name index.
///
/// `Debug` renders counts only — checkpoint names may carry hostile content
/// and must never leak through formatting.
pub(crate) struct SnapshotStore {
    run_id: i64,
    snapshots: Vec<RunSnapshot>,
    checkpoints: HashMap<String, usize>,
    /// The `snapshots` feature-flag gate: disabled stores refuse every record
    /// path with [`SnapshotError::Disabled`]. Enabled by default, so existing
    /// callers behave exactly as before.
    enabled: bool,
}

impl std::fmt::Debug for SnapshotStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotStore")
            .field("run_id", &self.run_id)
            .field("snapshots", &self.snapshots)
            .field("checkpoints", &self.checkpoints.len())
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl SnapshotStore {
    /// Empty store for one run.
    #[must_use]
    pub(crate) fn new(run_id: i64) -> Self {
        Self {
            run_id,
            snapshots: Vec::new(),
            checkpoints: HashMap::new(),
            enabled: true,
        }
    }

    /// Gate the WS-C.1 record paths behind the `snapshots` feature flag.
    /// Enabled (the default) keeps the current behavior; disabled makes the
    /// store fully inert — capture, checkpoint, and rollback refuse with the
    /// fixed-vocabulary [`SnapshotError::Disabled`] and append nothing, which
    /// reproduces the pre-2.0 behavior (no snapshots).
    #[must_use]
    pub(crate) fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Whether the WS-C.1 record paths are gated on.
    #[must_use]
    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Owning run id.
    #[must_use]
    pub(crate) const fn run_id(&self) -> i64 {
        self.run_id
    }

    /// Number of captured snapshots.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether nothing has been captured yet.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Captured snapshots in capture order.
    #[must_use]
    pub(crate) fn snapshots(&self) -> &[RunSnapshot] {
        &self.snapshots
    }

    /// Capture the caller's pipeline position at any stage.
    ///
    /// The witness edge `from → to` is the genuine lifecycle edge at the
    /// point and goes through [`transition`](super::lifecycle::transition)
    /// via the audit append.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Disabled`] when the `snapshots` flag gates
    /// the store off (appends nothing, captures nothing), or the secret-free
    /// [`SnapshotError::Lifecycle`] when the witness edge is illegal
    /// (appends nothing and captures nothing).
    pub(crate) fn capture(
        &mut self,
        log: &AuditLog,
        from: RunState,
        to: RunState,
        position: RunPosition,
    ) -> Result<usize, SnapshotError> {
        if !self.enabled {
            return Err(SnapshotError::Disabled);
        }
        log.record(from, to, AuditEvent::SnapshotCaptured)
            .map_err(SnapshotError::Lifecycle)?;
        Ok(self.push_snapshot(position, log.len()))
    }

    /// Capture under a caller-chosen checkpoint name (explicit user intent).
    ///
    /// Re-checkpointing a name moves it to the newest snapshot; earlier
    /// snapshots stay retained. The name is stored in memory only — it never
    /// enters the audit trail or any error.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::EmptyCheckpointName`] for an empty name,
    /// [`SnapshotError::Disabled`] when the `snapshots` flag gates the store
    /// off (appends nothing, captures nothing), or the secret-free
    /// [`SnapshotError::Lifecycle`] when the witness edge is illegal
    /// (appends nothing, captures nothing).
    pub(crate) fn checkpoint(
        &mut self,
        log: &AuditLog,
        from: RunState,
        to: RunState,
        name: &str,
        position: RunPosition,
    ) -> Result<usize, SnapshotError> {
        if name.is_empty() {
            return Err(SnapshotError::EmptyCheckpointName);
        }
        if !self.enabled {
            return Err(SnapshotError::Disabled);
        }
        log.record(from, to, AuditEvent::CheckpointSaved)
            .map_err(SnapshotError::Lifecycle)?;
        let index = self.push_snapshot(position, log.len());
        self.checkpoints.insert(name.to_string(), index);
        Ok(index)
    }

    /// Resolve a checkpoint name to its snapshot index.
    ///
    /// # Errors
    ///
    /// Returns the fixed-vocabulary [`SnapshotError::UnknownCheckpoint`] for
    /// unknown names — the rejected name is never echoed.
    pub(crate) fn resolve_checkpoint(&self, name: &str) -> Result<usize, SnapshotError> {
        self.checkpoints
            .get(name)
            .copied()
            .ok_or(SnapshotError::UnknownCheckpoint)
    }

    /// Borrowed checkpoint index for read-only views (WS-D.1): `(name,
    /// snapshot_index)` pairs ordered by snapshot index.
    ///
    /// Takes `&self` only, so inspectors cannot mutate the store through it.
    /// Names are caller-chosen labels (explicit user intent); they never enter
    /// the audit trail or errors, only this borrowed view.
    #[must_use]
    pub(crate) fn checkpoints(&self) -> Vec<(&str, usize)> {
        let mut indexed: Vec<(&str, usize)> = self
            .checkpoints
            .iter()
            .map(|(name, &index)| (name.as_str(), index))
            .collect();
        indexed.sort_by_key(|&(_, index)| index);
        indexed
    }

    /// Roll back to snapshot `index`.
    ///
    /// The witness edge `from → to` is the genuine lifecycle edge at the
    /// rollback point (e.g. the park-resume edge the reset resumes on) and
    /// goes through [`transition`](super::lifecycle::transition) via the
    /// audit append. Only backward targets are legal: the snapshot stage
    /// must not reach past the current stage and its audit marker must not
    /// reach past the current trail. Budgets are monotonic: the restored
    /// counters are the element-wise maximum, so rollback never refunds.
    /// The caller applies `restored.stage_index` to its pipeline (via
    /// [`rewind`](super::pipeline::Pipeline::rewind)).
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Disabled`] when the `snapshots` flag gates
    /// the store off (appends nothing),
    /// [`SnapshotError::UnknownSnapshot`] for an out-of-range index,
    /// [`SnapshotError::IllegalTarget`] for a forward target, or the
    /// secret-free [`SnapshotError::Lifecycle`] when the witness edge is
    /// illegal (appends nothing).
    pub(crate) fn rollback(
        &self,
        log: &AuditLog,
        from: RunState,
        to: RunState,
        index: usize,
        current: ResumePoint,
    ) -> Result<Restored, SnapshotError> {
        if !self.enabled {
            return Err(SnapshotError::Disabled);
        }
        let snapshot =
            self.snapshots
                .get(index)
                .copied()
                .ok_or(SnapshotError::UnknownSnapshot {
                    requested: index,
                    count: self.snapshots.len(),
                })?;
        if snapshot.stage_index > current.stage_index || snapshot.audit_len > log.len() {
            return Err(SnapshotError::IllegalTarget {
                target_stage: snapshot.stage_index,
                current_stage: current.stage_index,
                target_audit: snapshot.audit_len,
                current_audit: log.len(),
            });
        }
        log.record(from, to, AuditEvent::RolledBack)
            .map_err(SnapshotError::Lifecycle)?;
        Ok(Restored {
            stage_index: snapshot.stage_index,
            role: snapshot.role,
            state: snapshot.state,
            steps_taken: current.steps_taken.max(snapshot.steps_taken),
            spent_micro_usd: current.spent_micro_usd.max(snapshot.spent_micro_usd),
        })
    }

    /// Roll back to the snapshot filed under `name`.
    ///
    /// # Errors
    ///
    /// Returns the fixed-vocabulary [`SnapshotError::UnknownCheckpoint`] for
    /// unknown names (never echoed), or the target/edge errors of
    /// [`Self::rollback`].
    pub(crate) fn rollback_to_checkpoint(
        &self,
        log: &AuditLog,
        from: RunState,
        to: RunState,
        name: &str,
        current: ResumePoint,
    ) -> Result<Restored, SnapshotError> {
        let index = self.resolve_checkpoint(name)?;
        self.rollback(log, from, to, index, current)
    }

    /// File one validated snapshot position; returns its index.
    fn push_snapshot(&mut self, position: RunPosition, audit_len: usize) -> usize {
        self.snapshots.push(RunSnapshot {
            run_id: self.run_id,
            stage_index: position.stage_index,
            role: position.role,
            state: position.state,
            steps_taken: position.steps_taken,
            spent_micro_usd: position.spent_micro_usd,
            audit_len,
            captured_at: now_unix_seconds(),
        });
        self.snapshots.len().saturating_sub(1)
    }
}

/// Wall-clock capture time, Unix seconds. Falls back to `0` only when the
/// clock reports before the epoch.
fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Secret-free snapshot failure: fixed vocabulary and integer counters only —
/// checkpoint names, message content, credentials, and SQL never appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotError {
    /// Snapshot index out of range: carries only counters.
    UnknownSnapshot {
        /// Requested snapshot index.
        requested: usize,
        /// Snapshots captured so far.
        count: usize,
    },
    /// Unknown checkpoint name: fixed vocabulary, never echoes the name.
    UnknownCheckpoint,
    /// Checkpoint name is empty: explicit intent needs a name.
    EmptyCheckpointName,
    /// Rollback target reaches past the current position (forward restore or
    /// an audit marker from another trail): carries only stage and audit
    /// counters.
    IllegalTarget {
        /// Snapshot stage index.
        target_stage: usize,
        /// Current stage index.
        current_stage: usize,
        /// Snapshot audit length marker.
        target_audit: usize,
        /// Current audit trail length.
        current_audit: usize,
    },
    /// The witness lifecycle edge is illegal: the wrapped secret-free
    /// [`LifecycleError`].
    Lifecycle(LifecycleError),
    /// The `snapshots` feature flag gates the store off: every record path
    /// refuses without appending or capturing (pre-2.0: no snapshots).
    Disabled,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSnapshot { requested, count } => write!(
                f,
                "unknown run snapshot (requested {requested} with {count} captured)"
            ),
            Self::UnknownCheckpoint => write!(f, "unknown snapshot checkpoint"),
            Self::EmptyCheckpointName => write!(f, "snapshot checkpoint name is empty"),
            Self::IllegalTarget {
                target_stage,
                current_stage,
                target_audit,
                current_audit,
            } => {
                // Either trigger is reported with its own facts: a stage
                // reach-past names the stages, an audit-marker reach-past
                // (e.g. a marker from another trail) names the audit lengths.
                if target_stage > current_stage {
                    write!(
                        f,
                        "illegal snapshot rollback target (target stage {target_stage} past current stage {current_stage})"
                    )
                } else {
                    write!(
                        f,
                        "illegal snapshot rollback target (target audit {target_audit} past current audit {current_audit})"
                    )
                }
            }
            Self::Lifecycle(err) => write!(f, "{err}"),
            Self::Disabled => write!(f, "run snapshots are disabled"),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownSnapshot { .. }
            | Self::UnknownCheckpoint
            | Self::EmptyCheckpointName
            | Self::Disabled
            | Self::IllegalTarget { .. } => None,
            Self::Lifecycle(err) => Some(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::governance::AuditLog;
    use super::super::lifecycle::RunState;
    use super::super::pipeline::{Pipeline, PipelineStage};
    use super::*;

    fn capture_at(
        store: &mut SnapshotStore,
        log: &AuditLog,
        pipeline: &Pipeline,
        state: RunState,
        steps_taken: usize,
        spent_micro_usd: u64,
    ) -> usize {
        let position = RunPosition {
            stage_index: pipeline.entered().len(),
            role: pipeline.next().unwrap_or(PipelineStage::Review).role(),
            state,
            steps_taken,
            spent_micro_usd,
        };
        // Witness edge reuses a legal edge (genuine-edge pattern from
        // `Pipeline::advance`): capture appends without changing state.
        let (from, to) = match state {
            RunState::AwaitingApproval | RunState::AwaitingBudget => (state, RunState::Running),
            _ => (RunState::Queued, RunState::Running),
        };
        store
            .capture(log, from, to, position)
            .expect("capture on a legal witness edge")
    }

    #[test]
    fn capture_restore_round_trip_rewinds_pipeline() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();
        let mut store = SnapshotStore::new(7);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(store.run_id(), 7);

        pipeline
            .advance(&log, RunState::Queued, RunState::Running)
            .expect("enter plan");
        let first = capture_at(&mut store, &log, &pipeline, RunState::Running, 1, 100);
        assert_eq!(first, 0);

        pipeline
            .advance(&log, RunState::Running, RunState::AwaitingApproval)
            .expect("enter act");
        let second = capture_at(
            &mut store,
            &log,
            &pipeline,
            RunState::AwaitingApproval,
            2,
            250,
        );
        assert_eq!(second, 1);

        pipeline
            .advance(&log, RunState::AwaitingApproval, RunState::Running)
            .expect("enter review");
        assert!(pipeline.is_complete());

        let snapshot = store.snapshots()[first];
        assert_eq!(snapshot.run_id, 7);
        assert_eq!(snapshot.stage_index, 1);
        assert_eq!(snapshot.role, PipelineStage::Act.role());
        assert_eq!(snapshot.state, RunState::Running);
        assert_eq!(snapshot.steps_taken, 1);
        assert_eq!(snapshot.spent_micro_usd, 100);
        assert!(snapshot.captured_at > 0);

        // Rolling back keeps consumption monotonic (3 turns / 400 micro spent
        // since the snapshot) while restoring the stage position.
        let restored = store
            .rollback(
                &log,
                RunState::Running,
                RunState::AwaitingApproval,
                first,
                ResumePoint {
                    stage_index: pipeline.entered().len(),
                    steps_taken: 3,
                    spent_micro_usd: 400,
                },
            )
            .expect("backward rollback");
        assert_eq!(restored.stage_index, 1);
        assert_eq!(restored.role, PipelineStage::Act.role());
        assert_eq!(restored.state, RunState::Running);
        assert_eq!(restored.steps_taken, 3);
        assert_eq!(restored.spent_micro_usd, 400);

        pipeline
            .rewind(restored.stage_index)
            .expect("rewind to snapshot");
        assert_eq!(pipeline.next(), Some(PipelineStage::Act));
        assert_eq!(pipeline.entered().len(), 1);
        // The rewound stage re-enters in order without re-execution history.
        pipeline
            .advance(&log, RunState::AwaitingApproval, RunState::Running)
            .expect("re-enter act");
        assert_eq!(pipeline.entered().len(), 2);
    }

    #[test]
    fn rollback_never_refunds_budget() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(3);
        let index = store
            .capture(
                &log,
                RunState::Queued,
                RunState::Running,
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 2,
                    spent_micro_usd: 1_000,
                },
            )
            .expect("capture");
        assert_eq!(store.len(), 1);

        // Consumption grew past the snapshot: the restore keeps the higher
        // counters (no refund).
        let restored = store
            .rollback(
                &log,
                RunState::Running,
                RunState::Paused,
                index,
                ResumePoint {
                    stage_index: 2,
                    steps_taken: 5,
                    spent_micro_usd: 5_000,
                },
            )
            .expect("rollback keeps consumption");
        assert_eq!(restored.steps_taken, 5);
        assert_eq!(restored.spent_micro_usd, 5_000);

        // Consumption at the snapshot: the restore keeps the snapshot floor.
        let floored = store
            .rollback(
                &log,
                RunState::Paused,
                RunState::Running,
                index,
                ResumePoint {
                    stage_index: 2,
                    steps_taken: 0,
                    spent_micro_usd: 0,
                },
            )
            .expect("rollback floors at snapshot");
        assert_eq!(floored.steps_taken, 2);
        assert_eq!(floored.spent_micro_usd, 1_000);

        // The snapshot itself is unchanged by either rollback.
        assert_eq!(store.snapshots()[index].steps_taken, 2);
        assert_eq!(store.snapshots()[index].spent_micro_usd, 1_000);
    }

    #[test]
    fn checkpoint_naming_resolves_and_moves() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(11);

        let first = store
            .checkpoint(
                &log,
                RunState::Queued,
                RunState::Running,
                "stable",
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect("checkpoint");
        assert_eq!(store.resolve_checkpoint("stable"), Ok(first));

        // Re-checkpointing the name moves it; the earlier snapshot stays.
        let second = store
            .checkpoint(
                &log,
                RunState::Running,
                RunState::AwaitingApproval,
                "stable",
                RunPosition {
                    stage_index: 2,
                    role: PipelineStage::Review.role(),
                    state: RunState::AwaitingApproval,
                    steps_taken: 2,
                    spent_micro_usd: 20,
                },
            )
            .expect("re-checkpoint moves");
        assert_ne!(first, second);
        assert_eq!(store.resolve_checkpoint("stable"), Ok(second));
        assert_eq!(store.len(), 2);

        let restored = store
            .rollback_to_checkpoint(
                &log,
                RunState::AwaitingApproval,
                RunState::Running,
                "stable",
                ResumePoint {
                    stage_index: 2,
                    steps_taken: 9,
                    spent_micro_usd: 99,
                },
            )
            .expect("named rollback");
        assert_eq!(restored.stage_index, 2);
        assert_eq!(restored.steps_taken, 9);
        assert_eq!(restored.spent_micro_usd, 99);

        assert_eq!(
            store
                .checkpoint(
                    &log,
                    RunState::Queued,
                    RunState::Running,
                    "",
                    RunPosition {
                        stage_index: 0,
                        role: PipelineStage::Plan.role(),
                        state: RunState::Running,
                        steps_taken: 0,
                        spent_micro_usd: 0,
                    },
                )
                .expect_err("empty name must fail"),
            SnapshotError::EmptyCheckpointName
        );
        assert_eq!(
            format!("{}", SnapshotError::EmptyCheckpointName),
            "snapshot checkpoint name is empty"
        );
    }

    #[test]
    fn unknown_checkpoint_errors_loudly_without_echo() {
        let log = AuditLog::new();
        let store = SnapshotStore::new(5);
        let hostile = "sk-live-hostile-id SELECT * FROM users WHERE '1'='1";
        let err = store
            .resolve_checkpoint(hostile)
            .expect_err("unknown checkpoint must fail");
        assert_eq!(err, SnapshotError::UnknownCheckpoint);
        assert_eq!(format!("{err}"), "unknown snapshot checkpoint");

        let err = store
            .rollback_to_checkpoint(
                &log,
                RunState::Queued,
                RunState::Running,
                hostile,
                ResumePoint {
                    stage_index: 0,
                    steps_taken: 0,
                    spent_micro_usd: 0,
                },
            )
            .expect_err("named rollback must fail");
        assert_eq!(err, SnapshotError::UnknownCheckpoint);
        // The witness edge was never consumed: unknown names append nothing.
        assert!(log.is_empty());
    }

    #[test]
    fn illegal_rollback_targets_rejected() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(9);
        let first = store
            .capture(
                &log,
                RunState::Queued,
                RunState::Running,
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect("capture");
        assert_eq!(first, 0);

        // Out-of-range index carries counters only.
        let err = store
            .rollback(
                &log,
                RunState::Queued,
                RunState::Running,
                99,
                ResumePoint {
                    stage_index: 1,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect_err("unknown index must fail");
        assert_eq!(
            err,
            SnapshotError::UnknownSnapshot {
                requested: 99,
                count: 1
            }
        );
        assert_eq!(
            format!("{err}"),
            "unknown run snapshot (requested 99 with 1 captured)"
        );

        // Forward restore is illegal: the snapshot stage reaches past now.
        let err = store
            .rollback(
                &log,
                RunState::Queued,
                RunState::Running,
                first,
                ResumePoint {
                    stage_index: 0,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect_err("forward rollback must fail");
        assert_eq!(
            err,
            SnapshotError::IllegalTarget {
                target_stage: 1,
                current_stage: 0,
                target_audit: 1,
                current_audit: 1,
            }
        );
        assert_eq!(
            format!("{err}"),
            "illegal snapshot rollback target (target stage 1 past current stage 0)"
        );
    }

    #[test]
    fn audit_marker_beyond_trail_rejected_with_audit_facts() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(9);
        let index = store
            .capture(
                &log,
                RunState::Queued,
                RunState::Running,
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect("capture");
        let marker = store.snapshots()[index].audit_len;
        assert!(marker > 0);

        // Same stage position, but a shorter trail (e.g. another run's log):
        // only the audit marker reaches past, and the message names it.
        let short = AuditLog::new();
        let err = store
            .rollback(
                &short,
                RunState::Queued,
                RunState::Running,
                index,
                ResumePoint {
                    stage_index: 1,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect_err("audit reach-past must fail");
        assert_eq!(
            err,
            SnapshotError::IllegalTarget {
                target_stage: 1,
                current_stage: 1,
                target_audit: marker,
                current_audit: 0,
            }
        );
        assert_eq!(
            format!("{err}"),
            format!(
                "illegal snapshot rollback target (target audit {marker} past current audit 0)"
            )
        );
        assert!(short.is_empty());
    }

    #[test]
    fn illegal_witness_edges_append_nothing() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(9);
        let first = store
            .capture(
                &log,
                RunState::Queued,
                RunState::Running,
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect("capture");
        assert_eq!(store.len(), 1);

        // Illegal witness edges fail through transition() and append nothing.
        let before = log.len();
        let err = store
            .rollback(
                &log,
                RunState::Cancelled,
                RunState::Running,
                first,
                ResumePoint {
                    stage_index: 1,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect_err("illegal edge must fail");
        assert!(matches!(err, SnapshotError::Lifecycle(_)));
        assert_eq!(
            format!("{err}"),
            "illegal agent run transition from 'cancelled' to 'running'"
        );
        assert_eq!(log.len(), before);

        // Illegal captures append nothing either.
        let before = log.len();
        let err = store
            .capture(
                &log,
                RunState::Cancelled,
                RunState::Running,
                RunPosition {
                    stage_index: 1,
                    role: PipelineStage::Act.role(),
                    state: RunState::Running,
                    steps_taken: 1,
                    spent_micro_usd: 10,
                },
            )
            .expect_err("illegal capture edge must fail");
        assert!(matches!(err, SnapshotError::Lifecycle(_)));
        assert_eq!(log.len(), before);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn audit_entries_ordered_with_new_vocabulary() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();
        let mut store = SnapshotStore::new(21);

        pipeline
            .advance(&log, RunState::Queued, RunState::Running)
            .expect("enter plan");
        capture_at(&mut store, &log, &pipeline, RunState::Running, 1, 10);
        pipeline
            .advance(&log, RunState::Running, RunState::AwaitingApproval)
            .expect("enter act");
        store
            .checkpoint(
                &log,
                RunState::Running,
                RunState::AwaitingApproval,
                "stable",
                RunPosition {
                    stage_index: pipeline.entered().len(),
                    role: PipelineStage::Review.role(),
                    state: RunState::AwaitingApproval,
                    steps_taken: 2,
                    spent_micro_usd: 20,
                },
            )
            .expect("checkpoint");
        store
            .rollback(
                &log,
                RunState::AwaitingApproval,
                RunState::Running,
                0,
                ResumePoint {
                    stage_index: pipeline.entered().len(),
                    steps_taken: 2,
                    spent_micro_usd: 20,
                },
            )
            .expect("rollback");

        let entries = log.entries();
        let events: Vec<&str> = entries.iter().map(|entry| entry.event).collect();
        assert_eq!(
            events,
            vec![
                "stage_entered",
                "snapshot_captured",
                "stage_entered",
                "checkpoint_saved",
                "rolled_back",
            ]
        );
        // Gap-free append order across the composed pipeline + snapshot trail.
        for (index, entry) in entries.iter().enumerate() {
            let expected = u64::try_from(index).unwrap_or(u64::MAX);
            assert_eq!(entry.seq, expected, "seq gap-free at {index}");
        }
        // New snapshot events ride legal witness edges.
        assert_eq!((entries[1].from, entries[1].to), ("queued", "running"));
        assert_eq!(
            (entries[4].from, entries[4].to),
            ("awaiting_approval", "running"),
        );
    }

    #[test]
    fn snapshots_stay_secret_free_with_hostile_names() {
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(13);
        let hostile_name = "sk-live-hostile-id SELECT * FROM users; credential=sk-admin-secret";
        store
            .checkpoint(
                &log,
                RunState::Queued,
                RunState::Running,
                hostile_name,
                RunPosition {
                    stage_index: 0,
                    role: PipelineStage::Plan.role(),
                    state: RunState::Running,
                    steps_taken: 0,
                    spent_micro_usd: 0,
                },
            )
            .expect("hostile name still files");

        let audit_dump = format!("{:?}", log.entries());
        for hostile in [
            "sk-live-hostile-id",
            "sk-admin-secret",
            "SELECT",
            "credential",
            "api_key",
            "exfiltrate",
        ] {
            assert!(
                !audit_dump.to_lowercase().contains(hostile),
                "trail must not echo checkpoint names, found {hostile:?}"
            );
        }
        assert!(audit_dump.contains("checkpoint_saved"));

        // Errors never echo the rejected name either.
        let err = store
            .resolve_checkpoint("sk-live-other SELECT hostile")
            .expect_err("unknown");
        let rendered = format!("{err} {store:?}");
        for hostile in ["sk-live-other", "select", "hostile"] {
            assert!(
                !rendered.to_lowercase().contains(hostile),
                "errors and store Debug must not echo names, found {hostile:?}"
            );
        }
    }

    #[test]
    fn rewind_beyond_entered_rejected_secret_free() {
        let mut pipeline = Pipeline::default_pipeline();
        let err = pipeline
            .rewind(4)
            .expect_err("rewind past entered must fail");
        assert_eq!(
            err,
            super::super::pipeline::PipelineError::IllegalRewind {
                requested: 4,
                entered: 0
            }
        );
        assert_eq!(
            format!("{err}"),
            "pipeline rewind out of range (requested 4 with 0 entered)"
        );
        assert_eq!(pipeline.next(), Some(PipelineStage::Plan));
    }

    #[test]
    fn disabled_store_is_inert_and_secret_free() {
        // Flag OFF reproduces the pre-2.0 behavior: no captures, no
        // checkpoints, no rollbacks, no audit appends — every record path
        // refuses with fixed vocabulary.
        let log = AuditLog::new();
        let mut store = SnapshotStore::new(7).with_enabled(false);
        assert!(!store.is_enabled());
        assert!(SnapshotStore::new(7).is_enabled());
        let position = RunPosition {
            stage_index: 0,
            role: PipelineStage::Plan.role(),
            state: RunState::Running,
            steps_taken: 0,
            spent_micro_usd: 0,
        };
        for outcome in [
            store.capture(&log, RunState::Queued, RunState::Running, position),
            store.checkpoint(
                &log,
                RunState::Queued,
                RunState::Running,
                "stable",
                position,
            ),
        ] {
            assert_eq!(
                outcome.expect_err("disabled store refuses"),
                SnapshotError::Disabled
            );
        }
        assert_eq!(
            store
                .rollback(
                    &log,
                    RunState::Queued,
                    RunState::Running,
                    0,
                    ResumePoint {
                        stage_index: 0,
                        steps_taken: 0,
                        spent_micro_usd: 0
                    },
                )
                .expect_err("disabled rollback refuses"),
            SnapshotError::Disabled
        );
        let disabled = SnapshotError::Disabled;
        assert_eq!(format!("{disabled}"), "run snapshots are disabled");
        assert!(log.is_empty(), "disabled paths append nothing");
        assert!(store.is_empty(), "disabled paths capture nothing");
        // The empty name still fails first (caller intent, not the gate).
        assert_eq!(
            store
                .checkpoint(&log, RunState::Queued, RunState::Running, "", position)
                .expect_err("empty name fails"),
            SnapshotError::EmptyCheckpointName
        );
    }
}
