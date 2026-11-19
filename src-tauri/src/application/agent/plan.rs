//! User-initiated `Plan` decision flow (`M09` / `P04`).
//!
//! Handles listing persisted [`RunPlan`] records and resolving the user's
//! *"Start implementation?"* decision (`Yes` / `No` / `Custom reply`), either
//! dismissing the plan cleanly (`No`) or launching an implementation run with
//! the approved plan content injected into prior context (`Yes` / `Custom reply`).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::permissions::RunPreset;
use crate::application::execution::{AiMessage, AiRole, ProviderExecutor};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::run_plans::{
    RunPlan, RunPlanRepository, PLAN_STATUS_ACCEPTED, PLAN_STATUS_CUSTOM_REPLY, PLAN_STATUS_NONE,
    PLAN_STATUS_PENDING, PLAN_STATUS_REJECTED,
};

use super::registry::AgentRunRegistry;
use super::service::{
    start_run_with_options, AgentRunError, AgentRunHost, AgentRunRequest, RunLaunchOptions,
    RunLaunchSource,
};

/// Parsed user decision for a completed Plan run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanDecision {
    /// User clicked `Yes`: exit Plan mode and start implementation.
    Accepted,
    /// User clicked `No`: decline/dismiss without starting implementation.
    Rejected,
    /// User entered custom instructions (`Custom reply`) to steer/execute implementation.
    CustomReply(String),
}

impl PlanDecision {
    /// Parse a wire decision string + optional custom reply text.
    ///
    /// # Errors
    /// Returns [`PlanDecisionError::InvalidDecision`] for unrecognized variants or
    /// [`PlanDecisionError::EmptyCustomReply`] when `custom_reply` has empty text.
    pub(crate) fn parse(decision: &str, reply: Option<String>) -> Result<Self, PlanDecisionError> {
        match decision.trim() {
            "accepted" | "yes" => Ok(Self::Accepted),
            "rejected" | "no" => Ok(Self::Rejected),
            "custom_reply" => {
                let text = reply.unwrap_or_default();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(PlanDecisionError::EmptyCustomReply);
                }
                Ok(Self::CustomReply(trimmed.to_string()))
            }
            _ => Err(PlanDecisionError::InvalidDecision),
        }
    }
}

/// Outcome returned after applying a user decision to a [`RunPlan`].
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DecidePlanResult {
    /// Updated `run_plans` record after persisting the decision.
    pub plan: RunPlan,
    /// Newly launched implementation run id for `Yes` / `Custom reply`, `None` for `No`.
    pub launched_run_id: Option<i64>,
}

/// Parameters required when a decision (`Yes` / `Custom reply`) starts an implementation run.
pub(crate) struct PlanLaunchTarget {
    pub workspace_root: PathBuf,
    pub provider: String,
    pub model: String,
    pub credential: String,
    pub mode: AutonomyMode,
    pub preset: RunPreset,
    pub spend_limit_micro_usd: Option<u64>,
    pub executor: Arc<dyn ProviderExecutor + Send + Sync>,
    pub host: Arc<dyn AgentRunHost>,
    pub registry: Arc<AgentRunRegistry>,
}

/// Classified failures when resolving a Plan decision.
#[derive(Debug)]
pub(crate) enum PlanDecisionError {
    PlanNotFound,
    PlanIncomplete,
    AlreadyDecided(String),
    InvalidDecision,
    EmptyCustomReply,
    MissingLaunchTarget,
    StartRun(AgentRunError),
    Database(DatabaseError),
}

impl From<DatabaseError> for PlanDecisionError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl From<AgentRunError> for PlanDecisionError {
    fn from(value: AgentRunError) -> Self {
        Self::StartRun(value)
    }
}

/// List all persisted Plan records for `conversation_id`, oldest first.
///
/// # Errors
/// Propagates any [`DatabaseError`].
pub(crate) fn list_run_plans(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<RunPlan>, DatabaseError> {
    RunPlanRepository::new(db).list_for_conversation(conversation_id)
}

/// Apply the user's decision (`Yes` / `No` / `Custom reply`) to `run_id`'s pending plan.
///
/// For `Yes` and `Custom reply`, launches a non-plan implementation run under the
/// conversation's active Access profile with the approved plan injected as context
/// BEFORE marking the plan row decided, so pre-spawn failures keep the prompt pending.
///
/// # Errors
/// Returns [`PlanDecisionError`] if the plan is missing, incomplete, already decided,
/// invalid, or if starting the implementation run fails.
pub(crate) fn decide_run_plan(
    db: &Database,
    run_id: i64,
    decision: &PlanDecision,
    launch_target: Option<PlanLaunchTarget>,
) -> Result<DecidePlanResult, PlanDecisionError> {
    let repo = RunPlanRepository::new(db);
    let (existing, source) = repo
        .read_by_run_with_source(run_id)?
        .ok_or(PlanDecisionError::PlanNotFound)?;

    if existing.decision_status == PLAN_STATUS_NONE || existing.plan_content.is_none() {
        return Err(PlanDecisionError::PlanIncomplete);
    }
    if existing.decision_status != PLAN_STATUS_PENDING {
        return Err(PlanDecisionError::AlreadyDecided(existing.decision_status));
    }

    match decision {
        PlanDecision::Rejected => {
            let updated = repo
                .decide_for_source(source, PLAN_STATUS_REJECTED, None, None)?
                .ok_or_else(|| {
                    PlanDecisionError::AlreadyDecided(PLAN_STATUS_REJECTED.to_string())
                })?;
            Ok(DecidePlanResult {
                plan: updated,
                launched_run_id: None,
            })
        }
        PlanDecision::Accepted | PlanDecision::CustomReply(_) => {
            let target = launch_target.ok_or(PlanDecisionError::MissingLaunchTarget)?;
            let plan_text = existing.plan_content.as_deref().unwrap_or_default();
            let approved_plan_msg = AiMessage {
                role: AiRole::Assistant,
                content: format!("Proposed implementation plan:\n\n{plan_text}"),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            };

            let (status, reply_opt, user_request, persist_user_message) = match decision {
                PlanDecision::Accepted => (
                    PLAN_STATUS_ACCEPTED,
                    None,
                    "Proceed with implementing the approved plan.".to_string(),
                    false,
                ),
                PlanDecision::CustomReply(reply) => (
                    PLAN_STATUS_CUSTOM_REPLY,
                    Some(reply.as_str()),
                    reply.clone(),
                    true,
                ),
                PlanDecision::Rejected => unreachable!(),
            };

            let launched_run_id = start_run_with_options(
                db,
                target.registry,
                target.host,
                target.executor,
                target.workspace_root,
                AgentRunRequest {
                    conversation_id: existing.conversation_id,
                    user_request,
                    provider: target.provider,
                    model: target.model,
                    credential: target.credential,
                    max_iterations: None,
                    spend_limit_micro_usd: target.spend_limit_micro_usd,
                },
                target.mode,
                target.preset,
                RunLaunchOptions {
                    is_plan: false,
                    extra_history: vec![approved_plan_msg],
                    persist_user_message,
                    attachment_ids: Vec::new(),
                    source: Some(RunLaunchSource::Plan(source)),
                    prompt_context: None,
                },
            )?;

            let updated = repo
                .decide_for_source(source, status, reply_opt, Some(launched_run_id))?
                .unwrap_or(RunPlan {
                    decision_status: status.to_string(),
                    decision_reply: reply_opt.map(str::to_string),
                    implemented_run_id: Some(launched_run_id),
                    ..existing
                });

            Ok(DecidePlanResult {
                plan: updated,
                launched_run_id: Some(launched_run_id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plan_decision_validates_variants_and_empty_custom_reply() {
        assert_eq!(
            PlanDecision::parse("accepted", None).unwrap(),
            PlanDecision::Accepted
        );
        assert_eq!(
            PlanDecision::parse("yes", None).unwrap(),
            PlanDecision::Accepted
        );
        assert_eq!(
            PlanDecision::parse("rejected", None).unwrap(),
            PlanDecision::Rejected
        );
        assert_eq!(
            PlanDecision::parse("no", None).unwrap(),
            PlanDecision::Rejected
        );
        assert_eq!(
            PlanDecision::parse("custom_reply", Some("  use mocks  ".to_string())).unwrap(),
            PlanDecision::CustomReply("use mocks".to_string())
        );
        assert!(matches!(
            PlanDecision::parse("custom_reply", Some("   ".to_string())),
            Err(PlanDecisionError::EmptyCustomReply)
        ));
        assert!(matches!(
            PlanDecision::parse("custom_reply", None),
            Err(PlanDecisionError::EmptyCustomReply)
        ));
        assert!(matches!(
            PlanDecision::parse("maybe", None),
            Err(PlanDecisionError::InvalidDecision)
        ));
    }

    #[test]
    fn rejected_decision_updates_row_without_starting_run() {
        let db = crate::infrastructure::database::in_memory_database();
        let conv_id = crate::application::conversations::ConversationService::new(&db)
            .create("plan reject test")
            .expect("create conv");
        let run_id = crate::infrastructure::repository::agent_runs::AgentRunRepository::new(&db)
            .create_run(Some(conv_id), "gpt-5", "supervised")
            .expect("create run");
        RunPlanRepository::new(&db)
            .create(run_id, conv_id, "## Plan\n1. Step")
            .expect("create plan");

        let res = decide_run_plan(&db, run_id, &PlanDecision::Rejected, None).expect("reject");
        assert_eq!(res.launched_run_id, None);
        assert_eq!(res.plan.decision_status, PLAN_STATUS_REJECTED);
        assert!(res.plan.decided_at.is_some());

        // Second decision must fail with AlreadyDecided
        let err = decide_run_plan(&db, run_id, &PlanDecision::Rejected, None)
            .expect_err("double decide rejected");
        assert!(matches!(err, PlanDecisionError::AlreadyDecided(_)));
    }
}
