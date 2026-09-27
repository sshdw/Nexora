//! Staged execution pipeline: ordered stages bound to roles (WS-B.3).
//!
//! The default run flows `plan → act → review`. Each stage binds one
//! [`AgentRole`] (fixed per stage); the lifecycle segment is recorded per
//! entry — [`Pipeline::advance`] takes the genuine lifecycle edge at the
//! point, gates it on [`super::lifecycle::transition`], and appends a fixed-vocabulary
//! stage entry to the [`AuditLog`] atomically. An illegal edge fails with the
//! secret-free [`LifecycleError`] and advances nothing; a skipped stage fails
//! with the secret-free [`PipelineError`] and appends nothing.
//!
//! Per-stage budget slices are sub-[`RunBudget`]s: [`slice_budget`] resolves
//! caps through [`RunBudget`] composition (never duplication), rejecting any
//! slice that exceeds the parent cap.
//!
//! The pipeline changes no run behavior on its own: it is orchestration data
//! that WS-C (snapshots/checkpoints) builds on.

use super::governance::{AuditEvent, AuditLog, RunBudget};
use super::lifecycle::{LifecycleError, RunState};
use super::roles::AgentRole;

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

/// One ordered pipeline stage (WS-B.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PipelineStage {
    /// Plan the work (bound to [`AgentRole::Planner`]).
    Plan,
    /// Do the work (bound to [`AgentRole::Executor`]).
    Act,
    /// Review the work (bound to [`AgentRole::Reviewer`]).
    Review,
}

/// Default per-run stage list: `plan → act → review`.
pub(crate) const DEFAULT_STAGES: [PipelineStage; 3] = [
    PipelineStage::Plan,
    PipelineStage::Act,
    PipelineStage::Review,
];

impl PipelineStage {
    /// Canonical stage name (`plan` / `act` / `review`): fixed audit
    /// vocabulary, never content.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Act => "act",
            Self::Review => "review",
        }
    }

    /// Role bound to this stage.
    #[must_use]
    pub(crate) const fn role(self) -> AgentRole {
        match self {
            Self::Plan => AgentRole::Planner,
            Self::Act => AgentRole::Executor,
            Self::Review => AgentRole::Reviewer,
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Ordered stage list for one run plus its recorded lifecycle segments.
///
/// Advancing is strictly in order: only the next stage may be entered, so a
/// skipped or repeated stage is rejected and appends nothing. Every advance
/// goes through [`super::lifecycle::transition`] via the audit append, keeping the WS-B.1
/// state model and the WS-B.2 decision trail as the single source of truth.
#[derive(Debug)]
pub(crate) struct Pipeline {
    stages: Vec<PipelineStage>,
    entered: Vec<(PipelineStage, RunState, RunState)>,
}

impl Pipeline {
    /// Default `plan → act → review` pipeline.
    #[must_use]
    pub(crate) fn default_pipeline() -> Self {
        Self {
            stages: DEFAULT_STAGES.to_vec(),
            entered: Vec::new(),
        }
    }

    /// Custom ordered stage list for one run.
    #[must_use]
    pub(crate) fn new(stages: Vec<PipelineStage>) -> Self {
        Self {
            stages,
            entered: Vec::new(),
        }
    }

    /// Ordered stage list.
    #[must_use]
    pub(crate) fn stages(&self) -> &[PipelineStage] {
        &self.stages
    }

    /// Next stage to enter, or `None` once complete.
    #[must_use]
    pub(crate) fn next(&self) -> Option<PipelineStage> {
        self.stages.get(self.entered.len()).copied()
    }

    /// Whether every stage has been entered.
    #[must_use]
    pub(crate) fn is_complete(&self) -> bool {
        self.entered.len() >= self.stages.len()
    }

    /// Recorded entries: stage plus the genuine lifecycle segment it was
    /// entered on, in entry order.
    #[must_use]
    pub(crate) fn entered(&self) -> &[(PipelineStage, RunState, RunState)] {
        &self.entered
    }

    /// Rewind the entered prefix to `target_len` (WS-C.1 snapshot rollback).
    ///
    /// Truncates the entered tail so [`Self::next`] re-offers the snapshot
    /// stage. The audit trail stays append-only: rollback appends a
    /// `rolled_back` entry and never truncates.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError::IllegalRewind`] when `target_len` reaches
    /// past the entered prefix (counters only, secret-free).
    pub(crate) fn rewind(&mut self, target_len: usize) -> Result<(), PipelineError> {
        if target_len > self.entered.len() {
            return Err(PipelineError::IllegalRewind {
                requested: target_len,
                entered: self.entered.len(),
            });
        }
        self.entered.truncate(target_len);
        Ok(())
    }

    /// Enter the next ordered stage on lifecycle edge `from → to`.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError::AlreadyComplete`] when every stage is
    /// entered, or [`PipelineError::Lifecycle`] when the edge is illegal —
    /// both append nothing and advance nothing.
    pub(crate) fn advance(
        &mut self,
        log: &AuditLog,
        from: RunState,
        to: RunState,
    ) -> Result<PipelineStage, PipelineError> {
        let stage = self.next().ok_or(PipelineError::AlreadyComplete)?;
        self.enter(stage, log, from, to)
    }

    /// Enter `stage` on lifecycle edge `from → to`. Only the next ordered
    /// stage may be entered: skips and repeats are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError::OutOfOrder`] when `stage` is not next (appends
    /// nothing), [`PipelineError::AlreadyComplete`] when every stage is
    /// entered, or [`PipelineError::Lifecycle`] when the edge is illegal
    /// (appends nothing, advances nothing).
    pub(crate) fn advance_to(
        &mut self,
        stage: PipelineStage,
        log: &AuditLog,
        from: RunState,
        to: RunState,
    ) -> Result<PipelineStage, PipelineError> {
        let Some(next) = self.next() else {
            return Err(PipelineError::AlreadyComplete);
        };
        if stage != next {
            return Err(PipelineError::OutOfOrder {
                expected: next.as_str(),
                found: stage.as_str(),
            });
        }
        self.enter(stage, log, from, to)
    }

    fn enter(
        &mut self,
        stage: PipelineStage,
        log: &AuditLog,
        from: RunState,
        to: RunState,
    ) -> Result<PipelineStage, PipelineError> {
        // Illegal edges are an expected caller error here (not a model bug):
        // `record` gates on `transition`, so the append itself rejects the
        // edge and the stage never advances. No `debug_assert`: tests drive
        // this path on purpose.
        log.record(
            from,
            to,
            AuditEvent::StageEntered {
                stage: stage.as_str(),
            },
        )
        .map_err(PipelineError::Lifecycle)?;
        self.entered.push((stage, from, to));
        Ok(stage)
    }
}

// ---------------------------------------------------------------------------
// Budget slices
// ---------------------------------------------------------------------------

/// Resolve a per-stage budget slice under a parent cap (WS-B.3).
///
/// Both caps compose [`RunBudget`] (never duplicate it): slice caps at or
/// below the parent pass, anything above fails. `None` is unbounded — a
/// bounded parent rejects an unbounded slice, an unbounded parent accepts
/// any slice. An attached parent cost hook is intentionally not inherited:
/// slices resolve caps only (the hook stays an opaque WS-B.2 placeholder).
///
/// # Errors
///
/// Returns the secret-free [`BudgetError`] when the slice exceeds the parent
/// cap on either dimension.
pub(crate) fn slice_budget(
    parent: RunBudget,
    max_steps: usize,
    spend_limit_micro_usd: Option<u64>,
) -> Result<RunBudget, BudgetError> {
    if max_steps > parent.max_steps() {
        return Err(BudgetError::StepsExceedParent {
            requested: max_steps,
            parent: parent.max_steps(),
        });
    }
    let spend_ok = match (parent.spend_limit_micro_usd(), spend_limit_micro_usd) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(limit), Some(requested)) => requested <= limit,
    };
    if !spend_ok {
        return Err(BudgetError::SpendExceedsParent {
            requested: spend_limit_micro_usd,
            parent: parent.spend_limit_micro_usd(),
        });
    }
    Ok(RunBudget::new(max_steps, spend_limit_micro_usd))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Secret-free pipeline failure: fixed stage vocabulary and counters only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineError {
    /// A non-next stage was entered (skip or repeat): carries only the fixed
    /// expected/found stage names.
    OutOfOrder {
        /// Fixed name of the next ordered stage.
        expected: &'static str,
        /// Fixed name of the attempted stage.
        found: &'static str,
    },
    /// Every stage is already entered.
    AlreadyComplete,
    /// Rewind target beyond the entered prefix (WS-C.1 snapshot rollback):
    /// carries only counters, never content.
    IllegalRewind {
        /// Requested entered length.
        requested: usize,
        /// Entered stages so far.
        entered: usize,
    },
    /// The lifecycle edge at the advance point is illegal: the wrapped
    /// secret-free [`LifecycleError`].
    Lifecycle(LifecycleError),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfOrder { expected, found } => write!(
                f,
                "pipeline stage out of order: expected '{expected}', found '{found}'"
            ),
            Self::AlreadyComplete => write!(f, "pipeline already complete"),
            Self::IllegalRewind { requested, entered } => write!(
                f,
                "pipeline rewind out of range (requested {requested} with {entered} entered)"
            ),
            Self::Lifecycle(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OutOfOrder { .. } | Self::AlreadyComplete | Self::IllegalRewind { .. } => None,
            Self::Lifecycle(err) => Some(err),
        }
    }
}

/// Secret-free budget-slice failure: operational counters only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetError {
    /// Slice step cap exceeds the parent step cap.
    StepsExceedParent {
        /// Requested slice step cap.
        requested: usize,
        /// Parent step cap.
        parent: usize,
    },
    /// Slice spend cap exceeds the parent spend cap (`None` = unbounded).
    SpendExceedsParent {
        /// Requested slice spend cap, micro-USD (`None` = unbounded).
        requested: Option<u64>,
        /// Parent spend cap, micro-USD (`None` = unbounded).
        parent: Option<u64>,
    },
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StepsExceedParent { requested, parent } => write!(
                f,
                "pipeline budget slice exceeds parent step cap ({requested} above {parent})"
            ),
            Self::SpendExceedsParent { requested, parent } => write!(
                f,
                "pipeline budget slice exceeds parent spend cap ({requested:?} above {parent:?})"
            ),
        }
    }
}

impl std::error::Error for BudgetError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::roles::ALL_ROLES;

    #[test]
    fn default_pipeline_binds_stages_in_order() {
        let pipeline = Pipeline::default_pipeline();
        assert_eq!(pipeline.stages(), DEFAULT_STAGES);
        assert_eq!(
            DEFAULT_STAGES.map(PipelineStage::as_str),
            ["plan", "act", "review"]
        );
        assert_eq!(PipelineStage::Plan.role(), AgentRole::Planner);
        assert_eq!(PipelineStage::Act.role(), AgentRole::Executor);
        assert_eq!(PipelineStage::Review.role(), AgentRole::Reviewer);
        // Every bound role is one of the six typed roles.
        for stage in DEFAULT_STAGES {
            assert!(
                ALL_ROLES.contains(&stage.role()),
                "{stage:?} binds a typed role"
            );
        }
        assert_eq!(pipeline.next(), Some(PipelineStage::Plan));
        assert!(!pipeline.is_complete());
        assert!(pipeline.entered().is_empty());
    }

    #[test]
    fn advance_in_order_records_stage_entries() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();

        assert_eq!(
            pipeline
                .advance(&log, RunState::Queued, RunState::Running)
                .expect("enter plan"),
            PipelineStage::Plan
        );
        assert_eq!(
            pipeline
                .advance(&log, RunState::Running, RunState::AwaitingApproval)
                .expect("enter act"),
            PipelineStage::Act
        );
        assert_eq!(
            pipeline
                .advance(&log, RunState::AwaitingApproval, RunState::Running)
                .expect("enter review"),
            PipelineStage::Review
        );
        assert!(pipeline.is_complete());
        assert_eq!(pipeline.next(), None);

        assert_eq!(
            pipeline.entered(),
            &[
                (PipelineStage::Plan, RunState::Queued, RunState::Running),
                (
                    PipelineStage::Act,
                    RunState::Running,
                    RunState::AwaitingApproval
                ),
                (
                    PipelineStage::Review,
                    RunState::AwaitingApproval,
                    RunState::Running
                ),
            ]
        );

        let entries = log.entries();
        assert_eq!(entries.len(), 3);
        for (index, (entry, stage)) in entries.iter().zip(["plan", "act", "review"]).enumerate() {
            assert_eq!(entry.seq, u64::try_from(index).unwrap_or(u64::MAX));
            assert_eq!(entry.event, "stage_entered");
            assert_eq!(entry.stage, Some(stage));
        }
        assert_eq!((entries[0].from, entries[0].to), ("queued", "running"));
        assert_eq!(
            (entries[1].from, entries[1].to),
            ("running", "awaiting_approval")
        );
        assert_eq!(
            (entries[2].from, entries[2].to),
            ("awaiting_approval", "running")
        );
    }

    #[test]
    fn stage_skip_and_repeat_are_rejected_without_append() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();

        // Skipping straight to review is rejected ...
        let err = pipeline
            .advance_to(
                PipelineStage::Review,
                &log,
                RunState::Queued,
                RunState::Running,
            )
            .expect_err("skip must fail");
        assert_eq!(
            err,
            PipelineError::OutOfOrder {
                expected: "plan",
                found: "review"
            }
        );
        assert_eq!(
            format!("{err}"),
            "pipeline stage out of order: expected 'plan', found 'review'"
        );
        assert!(log.is_empty());
        assert_eq!(pipeline.next(), Some(PipelineStage::Plan));

        pipeline
            .advance_to(
                PipelineStage::Plan,
                &log,
                RunState::Queued,
                RunState::Running,
            )
            .expect("enter plan");
        // ... skipping act is rejected ...
        assert_eq!(
            pipeline
                .advance_to(
                    PipelineStage::Review,
                    &log,
                    RunState::Running,
                    RunState::AwaitingApproval
                )
                .expect_err("skip must fail"),
            PipelineError::OutOfOrder {
                expected: "act",
                found: "review"
            }
        );
        pipeline
            .advance_to(
                PipelineStage::Act,
                &log,
                RunState::Running,
                RunState::AwaitingApproval,
            )
            .expect("enter act");
        // ... and so is repeating plan once act is entered.
        assert_eq!(
            pipeline
                .advance_to(
                    PipelineStage::Plan,
                    &log,
                    RunState::AwaitingApproval,
                    RunState::Running
                )
                .expect_err("repeat must fail"),
            PipelineError::OutOfOrder {
                expected: "review",
                found: "plan"
            }
        );
        pipeline
            .advance(&log, RunState::AwaitingApproval, RunState::Running)
            .expect("enter review");
        assert_eq!(
            pipeline
                .advance(&log, RunState::Running, RunState::AwaitingApproval)
                .expect_err("complete pipeline rejects"),
            PipelineError::AlreadyComplete
        );
        // Only the three ordered entries were appended.
        let stages: Vec<Option<&str>> = log.entries().iter().map(|entry| entry.stage).collect();
        assert_eq!(stages, vec![Some("plan"), Some("act"), Some("review")]);
    }

    #[test]
    fn illegal_lifecycle_edge_rejects_advance_and_appends_nothing() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();

        // Terminal states have no exits: the stage change goes through
        // transition() and fails without appending or advancing.
        let err = pipeline
            .advance(&log, RunState::Cancelled, RunState::Running)
            .expect_err("illegal edge must fail");
        assert!(matches!(err, PipelineError::Lifecycle(_)));
        assert_eq!(
            format!("{err}"),
            "illegal agent run transition from 'cancelled' to 'running'"
        );
        assert!(log.is_empty(), "illegal edges append nothing");
        assert_eq!(pipeline.next(), Some(PipelineStage::Plan));
        assert!(pipeline.entered().is_empty());

        // A legal edge still advances afterwards.
        pipeline
            .advance(&log, RunState::Queued, RunState::Running)
            .expect("legal edge advances");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn budget_slice_enforces_parent_cap() {
        let parent = RunBudget::new(10, Some(1_000_000));

        let slice = slice_budget(parent, 4, Some(250_000)).expect("within caps");
        assert_eq!(slice.max_steps(), 4);
        assert_eq!(slice.spend_limit_micro_usd(), Some(250_000));
        // Exact caps are not an exceed.
        let exact = slice_budget(parent, 10, Some(1_000_000)).expect("exact caps pass");
        assert_eq!(exact.max_steps(), 10);

        let err = slice_budget(parent, 11, Some(100)).expect_err("steps exceed");
        assert_eq!(
            err,
            BudgetError::StepsExceedParent {
                requested: 11,
                parent: 10
            }
        );

        let err = slice_budget(parent, 5, Some(2_000_000)).expect_err("spend exceeds");
        assert_eq!(
            err,
            BudgetError::SpendExceedsParent {
                requested: Some(2_000_000),
                parent: Some(1_000_000)
            }
        );

        // An unbounded slice under a bounded parent would exceed the cap.
        assert_eq!(
            slice_budget(parent, 5, None).expect_err("unguarded slice exceeds"),
            BudgetError::SpendExceedsParent {
                requested: None,
                parent: Some(1_000_000)
            }
        );

        // An unbounded parent accepts any slice.
        let free = RunBudget::new(8, None);
        let guarded = slice_budget(free, 3, Some(500)).expect("bounded slice under free parent");
        assert_eq!(guarded.spend_limit_micro_usd(), Some(500));
        let unguarded = slice_budget(free, 8, None).expect("free under free");
        assert_eq!(unguarded.spend_limit_micro_usd(), None);

        for sentinel in ["sk-", "credential", "SELECT", "api_key"] {
            let dump = format!(
                "{:?}",
                slice_budget(parent, 99, Some(99_000_000)).expect_err("must fail")
            );
            assert!(
                !dump.to_lowercase().contains(sentinel),
                "slice errors stay secret-free, found {sentinel:?}"
            );
        }
    }

    #[test]
    fn stage_entries_stay_secret_free() {
        let log = AuditLog::new();
        let mut pipeline = Pipeline::default_pipeline();
        pipeline
            .advance(&log, RunState::Queued, RunState::Running)
            .expect("plan");
        pipeline
            .advance(&log, RunState::Running, RunState::AwaitingApproval)
            .expect("act");
        let dump = format!("{:?}", log.entries());
        for hostile in [
            "sk-live",
            "credential",
            "SELECT",
            "api_key",
            "passwd",
            "exfiltrate",
        ] {
            assert!(
                !dump.to_lowercase().contains(hostile),
                "stage entries carry fixed vocabulary only, found {hostile:?}"
            );
        }
        assert!(dump.contains("stage_entered"));
    }
}
