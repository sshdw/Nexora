//! Agent run bridge (Task 5.1): threads the synchronous [`AgentRunner`]
//! onto a dedicated run thread, streams every run event to the frontend,
//! and tracks active runs.
//!
//! # Threading model
//!
//! `AgentRunner::run` is synchronous and long-lived (it parks on user pauses,
//! budget boundaries, and approval gates), so it must never execute on the
//! IPC thread. [`start_run`] therefore:
//!
//! 1. claims the conversation (DP-4: at most one active run per conversation,
//!    parallel across conversations — rejected with
//!    [`AgentRunError::RunAlreadyActive`] otherwise);
//! 2. pre-creates the `agent_runs` row through the run recorder (with the
//!    conversation link, D50) so the run id is known synchronously;
//! 3. registers the run in the [`AgentRunRegistry`];
//! 4. spawns one **run thread** (executes `run()`, persists the assistant
//!    message on success, forwards the terminal `RunFinished` payload,
//!    releases the registry entry on every exit path) and one
//!    **forwarder thread** (drains the run's `mpsc` channel into
//!    [`AgentRunHost::emit`] until the channel disconnects — the disconnect
//!    *is* the drain guarantee — and only then emits the terminal frame, so
//!    `Finished` is always the last frame).
//!
//! The runner and the recorder both send on the same channel, and both send
//! from the run thread (recorder methods are called by the runner), so
//! governance and step events are totally ordered by emission.
//!
//! # Tauri independence
//!
//! The bridge never names a Tauri type: the shell layer supplies an
//! [`AgentRunHost`] implementation (event emission + assistant-message
//! persistence). The Tauri adapter lives in `commands/agent.rs`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use serde::Serialize;

use super::action_memory::{self, ActionSummary, AgentStepView};
use super::approval::{ApprovalGate, AutonomyMode};
use super::checkpoints::{CheckpointRunContext, CheckpointStore};
use super::compaction::{self, CompactionReason};
use super::control::{AgentRunEvent, CancellationToken, RunControl};
use super::governance::{audit, AuditEvent, AuditLog};
use super::lifecycle::{observe_transition, RunState};
use super::permissions::RunPreset;
use super::persistence::{mode_to_column, terminal_outcome, RunRecorder};
use super::question::QuestionRunContext;
use super::runner::AgentRunner;
#[cfg(test)]
use crate::application::context_stats::context_limit_for;
use crate::application::context_stats::conversation_context_stats_for_selection;
use crate::application::conversations::ConversationService;
use crate::application::execution::{
    AiMessage, AiRequest, AiRole, ModelConfig, ProviderExecutor, ReasoningEffort, RequestError,
};
use crate::application::routing::{RoutingService, TaskKind};
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_runs::{
    AgentRun, AgentRunRepository, AgentStep, NewConversationCompaction, RunSource,
};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::MessageRepository;
use crate::infrastructure::repository::run_plans::{RunPlan, RunPlanRepository};
use crate::infrastructure::repository::run_questions::RunQuestionRepository;

// S3 split: the run registry lives in `super::registry`; re-exported here so
// existing `service::{AgentRunRegistry, ActiveAgentRun, ResolveOutcome}`
// paths (commands, e2e/stress tests) keep resolving with no other file edits.
pub(crate) use super::registry::{ActiveAgentRun, AgentRunRegistry, ResolveOutcome};

// ---------------------------------------------------------------------------
// Frames (the wire shape of `agent-run-event`)
// ---------------------------------------------------------------------------

/// One `agent-run-event` frame (Task 5.1 design §2.4): a single Tauri event
/// name whose payload discriminates step, governance, and terminal frames,
/// each tagged with the owning `run_id`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RunFrame {
    /// Best-effort notification of an already committed identity. Consumers
    /// reconcile with the conversation cursor API; this never executes work.
    Durable {
        run_id: i64,
        conversation_id: i64,
        event_id: i64,
    },
    /// One successfully persisted step (from `AgentRunEvent::StepRecorded`).
    Step {
        /// The run this frame belongs to.
        run_id: i64,
        /// The owning conversation, supplied by the service forwarder.
        conversation_id: i64,
        /// The step payload.
        event: StepEventFrame,
    },
    /// One governance event (everything the runner emits).
    Governance {
        /// The run this frame belongs to.
        run_id: i64,
        /// The owning conversation, supplied by the service forwarder.
        conversation_id: i64,
        /// The governance payload.
        event: AgentRunEvent,
    },
    /// The terminal frame: delivered by the forwarder only after the run-event
    /// channel is fully drained, so it is always the last frame of a run.
    Finished {
        /// The run this frame belongs to.
        run_id: i64,
        /// The terminal payload.
        event: RunFinished,
    },
}

/// Step payload of a [`RunFrame::Step`] frame: exactly the persisted
/// `agent_steps` columns (minus the `run_id`, which the frame carries) —
/// except `observation`, which is a *bounded preview* of the stored text
/// (NEX-PERF-003), never the full output. The full sanitized text stays
/// reachable on demand through `get_agent_step_observation(step_id)`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StepEventFrame {
    /// Surrogate key of the persisted `agent_steps` row (`id`) — the
    /// on-demand full-text fetch key.
    pub step_id: i64,
    /// 1-based sequence, identical to `agent_steps.seq`.
    pub seq: i64,
    /// `'model_turn' | 'tool_call' | 'approval'`.
    pub kind: String,
    /// Tool name; `None` for `model_turn`.
    pub tool_name: Option<String>,
    /// Raw JSON arguments exactly as provider-supplied.
    pub arguments: Option<String>,
    /// Bounded preview of the model-turn content / tool output / denial or
    /// approval text (sanitized, exactly as stored up to the cut).
    pub observation: Option<String>,
    /// Byte-exact length of the full sanitized observation text;
    /// `None` exactly when `observation` is `None`.
    pub observation_len: Option<u64>,
    /// Whether the preview was cut (`true` exactly when the stored text
    /// exceeds the preview bound).
    pub observation_truncated: bool,
    /// `'succeeded' | 'failed' | 'denied' | 'cancelled'` (tool/approval only).
    pub status: Option<String>,
    /// Step duration in milliseconds, when known.
    pub duration_ms: Option<i64>,
}

/// One `agent_steps` row as served to the UI (NEX-PERF-003): the full
/// persisted metadata plus a *bounded preview* of the observation — never
/// the full output. The preview is derived by the same
/// [`super::persistence::step_preview`] the live `StepRecorded` frames use,
/// so rehydration and the live stream agree byte-for-byte. The full
/// sanitized text stays reachable on demand through
/// [`get_step_observation_for`]. No migration: the preview is derived at
/// read time, never stored.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentStepListItem {
    /// Surrogate primary key (`id`) — the on-demand full-text fetch key.
    pub id: i64,
    /// Owning run (`run_id`).
    pub run_id: i64,
    /// 1-based step sequence within the run (`seq`).
    pub seq: i64,
    /// Step kind (`kind`).
    pub kind: String,
    /// Tool name (`tool_name`), `None` for `model_turn` steps.
    pub tool_name: Option<String>,
    /// Raw JSON arguments exactly as provider-supplied (`arguments`).
    pub arguments: Option<String>,
    /// Bounded preview of the tool output / denial text / approval decision
    /// (`observation`, sanitized as stored up to the cut).
    pub observation: Option<String>,
    /// Byte-exact length of the full sanitized observation text;
    /// `None` exactly when `observation` is `None`.
    pub observation_len: Option<u64>,
    /// Whether the preview was cut (`true` exactly when the stored text
    /// exceeds the preview bound).
    pub observation_truncated: bool,
    /// Tool call outcome (`status`).
    pub status: Option<String>,
    /// Step start timestamp (`started_at`).
    pub started_at: i64,
    /// Step duration in milliseconds (`duration_ms`).
    pub duration_ms: Option<i64>,
    /// Authoring permission rule (`rule_id`, M1-core).
    pub rule_id: Option<i64>,
    /// Session-sticky group key (`group_key`, M1-core).
    pub group_key: Option<String>,
    /// Who decided the step (`decided_by`, M1-core).
    pub decided_by: Option<String>,
}

impl AgentStepListItem {
    /// Project one persisted row onto its bounded list shape.
    fn from_row(step: &AgentStep) -> Self {
        let (preview, total_bytes, truncated) =
            super::persistence::step_preview_opt(step.observation.as_deref());
        Self {
            id: step.id,
            run_id: step.run_id,
            seq: step.seq,
            kind: step.kind.clone(),
            tool_name: step.tool_name.clone(),
            arguments: step.arguments.clone(),
            observation: preview,
            observation_len: total_bytes,
            observation_truncated: truncated,
            status: step.status.clone(),
            started_at: step.started_at,
            duration_ms: step.duration_ms,
            rule_id: step.rule_id,
            group_key: step.group_key.clone(),
            decided_by: step.decided_by.clone(),
        }
    }
}

/// Terminal payload of a [`RunFrame::Finished`] frame. The status/error
/// mapping is the recorder's own [`terminal_outcome`], so the UI, the live
/// stream, and `agent_runs` cannot disagree.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunFinished {
    /// The conversation the run belongs to (also on the run's persisted row;
    /// carried here so the frontend can route the frame without a lookup).
    pub conversation_id: i64,
    /// `'completed' | 'cancelled' | 'budget_exhausted' |
    /// 'spend_limit_exceeded' | 'error'`.
    pub status: String,
    /// Final assistant text (`completed` only).
    pub final_content: Option<String>,
    /// Classified error text (`error` only; never a secret).
    pub error: Option<String>,
}

impl RunFrame {
    /// Wrap a channel event into its frame. `StepRecorded` events become
    /// step frames (their own `run_id` is authoritative); every other event
    /// becomes a governance frame tagged with the bridging `run_id`.
    #[must_use]
    pub(crate) fn with_event(run_id: i64, conversation_id: i64, event: AgentRunEvent) -> Self {
        match event {
            AgentRunEvent::StepRecorded {
                step_id,
                run_id: event_run_id,
                seq,
                kind,
                tool_name,
                arguments,
                observation,
                observation_len,
                observation_truncated,
                status,
                duration_ms,
            } => Self::Step {
                run_id: event_run_id,
                conversation_id,
                event: StepEventFrame {
                    step_id,
                    seq,
                    kind,
                    tool_name,
                    arguments,
                    observation,
                    observation_len,
                    observation_truncated,
                    status,
                    duration_ms,
                },
            },
            other => Self::Governance {
                run_id,
                conversation_id,
                event: other,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Host abstraction (the shell side of the bridge)
// ---------------------------------------------------------------------------

/// Everything the bridge needs from the shell (Tauri) layer, without Tauri
/// types. Implemented in `commands/agent.rs` over the `AppHandle`.
pub(crate) trait AgentRunHost: Send + Sync + 'static {
    /// Emit one frame to the frontend. Best-effort: a failed emission is
    /// logged by the implementation and never affects the run.
    fn emit(&self, frame: &RunFrame);

    /// Persist the final assistant message after a successful run — the same
    /// [`ConversationService`] path as plain chat (DP-7). Best-effort: a
    /// failure is logged and never changes the run outcome (the final answer
    /// remains available on the `agent_runs` row and in the stream).
    fn persist_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    );

    /// Optional verified content-addressed checkpoint store (`<app_data>/checkpoints`).
    /// Returning `None` keeps fail-closed `PerCommandConsent` gating.
    fn checkpoint_store(&self) -> Option<CheckpointStore> {
        None
    }

    /// Invoked on the forwarder thread right after `RunFrame::Finished` is
    /// emitted when `status == "completed"`, allowing the host to auto-advance
    /// the conversation's FIFO work queue.
    fn on_run_completed(&self, _conversation_id: i64, _registry: Arc<AgentRunRegistry>) {}
}

// ---------------------------------------------------------------------------
// Start request / errors
// ---------------------------------------------------------------------------

/// A validated agent-run start request. The credential is resolved by the
/// caller (the IPC layer, via `RequestExecutionService::resolve_credential`)
/// and only ever lives inside the spawned run thread — it never crosses IPC
/// and never enters an event frame, log line, or error message.
#[derive(Debug, Clone)]
pub(crate) struct AgentRunRequest {
    /// Conversation to run in (also persisted onto the `agent_runs` row).
    pub conversation_id: i64,
    /// The user request; persisted as the user message before the spawn.
    pub user_request: String,
    /// Provider internal name (must resolve to a registered executor).
    pub provider: String,
    /// Model name within the provider.
    pub model: String,
    /// Keyring credential for the provider (pre-resolved by the caller).
    pub credential: String,
    /// Iteration-budget override (test seam; `None` = runner default).
    pub(crate) max_iterations: Option<usize>,
    /// Spend-limit override in micro-USD (test seam; `None` = no guard).
    pub(crate) spend_limit_micro_usd: Option<u64>,
}

/// Optional launch customizations shared by normal runs, Plan runs (`M09`),
/// plan-approved implementation runs, and safe interrupted continuations (`M11`).
#[derive(Debug, Clone)]
pub(crate) struct RunLaunchOptions {
    /// Enforce user-initiated read-only Plan mode (`AccessProfile::ReadOnly`).
    pub is_plan: bool,
    /// Additional prior messages injected after persisted conversation history
    /// (never persisted into `messages`).
    pub extra_history: Vec<AiMessage>,
    /// Whether `request.user_request` should be persisted to `messages` (`false`
    /// for `Yes` plan confirmation and explicit `continue_run`).
    pub persist_user_message: bool,
    /// Draft attachment IDs admitted and linked with this run's user turn.
    pub attachment_ids: Vec<i64>,
    /// Exact server-admitted context included on this turn only.
    pub prompt_context: Option<String>,
    /// Original owner of inherited history and tool restrictions.
    pub source: Option<RunLaunchSource>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RunLaunchSource {
    Continue(RunSource),
    Plan(RunSource),
}

impl RunLaunchSource {
    fn run(self) -> RunSource {
        match self {
            Self::Continue(source) | Self::Plan(source) => source,
        }
    }
}

impl Default for RunLaunchOptions {
    fn default() -> Self {
        Self {
            is_plan: false,
            extra_history: Vec::new(),
            persist_user_message: true,
            attachment_ids: Vec::new(),
            prompt_context: None,
            source: None,
        }
    }
}

/// Enriched `agent_runs` projection returned to UI callers, combining persisted
/// `AgentRun` columns with `run_plans` (`M09`) and `run_continuations` (`M11`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentRunSummary {
    #[serde(flatten)]
    pub run: AgentRun,
    pub is_plan: bool,
    pub plan_content: Option<String>,
    pub plan_decision_status: Option<String>,
    pub plan_decision_reply: Option<String>,
    pub plan_implemented_run_id: Option<i64>,
    pub continued_from_run_id: Option<i64>,
}

/// Classified failures of [`start_run`] (pre-spawn only: once the run thread
/// is spawned, outcomes flow through the event stream instead).
#[derive(Debug)]
pub(crate) enum AgentRunError {
    /// A private store cannot be selected as an agent workspace.
    WorkspaceProtected,
    /// The conversation does not exist.
    ConversationNotFound {
        /// The missing conversation id.
        id: i64,
    },
    /// Another run is already active for this conversation (DP-4).
    RunAlreadyActive {
        /// The busy conversation id.
        conversation_id: i64,
    },
    /// The requested parent run for continuation does not exist.
    RunNotFound {
        /// The missing run id.
        run_id: i64,
    },
    /// A continuation was requested for a run whose status is not `'interrupted'`.
    RunNotInterrupted {
        /// Current persisted run status.
        status: String,
    },
    /// Provider/credential resolution failed (FR-014 classifications).
    Request(RequestError),
    /// Shared attachment admission failed before any run was spawned.
    Attachment(crate::application::conversations::ConversationError),
    /// The `agent_runs` row could not be created before spawning.
    RunNotPersisted,
    /// A run/forwarder thread could not be spawned.
    ThreadSpawn(String),
    /// Setup persistence failed.
    Database(crate::infrastructure::database::DatabaseError),
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

/// Start one agent run (Task 5.1 design §2-§3): claim the conversation
/// (DP-4), persist the user message, create the linked `agent_runs` row,
/// register the run, and spawn the run + forwarder threads. Returns the
/// `run_id` immediately; the run's outcome flows exclusively through the
/// event stream.
///
/// # Errors
///
/// See [`AgentRunError`]. On any pre-spawn failure the conversation claim is
/// released and no thread is left behind.
///
/// `registry` and `request` are taken by value deliberately: the bridge owns
/// the start request, and the registry handle is a cheap `Arc` that is
/// shared into the spawned threads.
/// Parse a persisted autonomy string into [`AutonomyMode`], defaulting to
/// `SemiAutonomous` for missing, empty, or legacy-invalid values (Task 5.2,
/// DP-AUTONOMY — matches current hardcoded behavior).
#[must_use]
#[allow(clippy::match_same_arms)]
pub(crate) fn parse_autonomy_mode(value: Option<&str>) -> AutonomyMode {
    match value {
        Some("supervised") => AutonomyMode::Supervised,
        Some("full_autonomous") => AutonomyMode::FullAutonomous,
        Some("semi_autonomous") => AutonomyMode::SemiAutonomous,
        _ => AutonomyMode::SemiAutonomous,
    }
}

/// Resolve the autonomy mode through `Project (.nexora/) > Personal (SQLite) > Default`,
/// enforcing the untrusted-repo ceiling so `.nexora/` can restrict but never widen autonomy.
#[must_use]
pub(crate) fn resolve_autonomy_mode_for_workspace(
    db: &Database,
    workspace_root: Option<&Path>,
) -> AutonomyMode {
    let snap = crate::application::settings::resolve_settings(db, workspace_root, None);
    if snap.config_notice.is_some() {
        return AutonomyMode::Supervised;
    }
    let raw = snap
        .entries
        .get(nexora_core::config::AUTONOMY_KEY)
        .and_then(|entry| entry.value.as_deref());
    parse_autonomy_mode(raw)
}

/// Resolve the persisted autonomy mode from `app_settings` (`agent.autonomy`)
/// plus any active project `.nexora/` restriction, defaulting to `SemiAutonomous`.
#[must_use]
pub(crate) fn resolve_autonomy_mode(db: &Database) -> AutonomyMode {
    resolve_autonomy_mode_for_workspace(db, None)
}

/// Setting key backing the run preset (`agent.preset`).
pub(crate) const PRESET_KEY: &str = "agent.preset";

/// Parse a persisted preset string into [`RunPreset`], defaulting to
/// `Coding` for missing, empty, or invalid values (T5 — mirrors the autonomy
/// pattern above: the default preserves today's behavior everywhere).
#[must_use]
pub(crate) fn parse_preset(value: Option<&str>) -> RunPreset {
    match value {
        Some("document") => RunPreset::Document,
        _ => RunPreset::Coding,
    }
}

/// Resolve the run preset through `Project (.nexora/) > Personal (SQLite) > Default`.
#[must_use]
pub(crate) fn resolve_preset_for_workspace(
    db: &Database,
    workspace_root: Option<&Path>,
) -> RunPreset {
    let raw = crate::application::settings::resolve_effective_setting(
        db,
        workspace_root,
        PRESET_KEY,
        None,
    );
    parse_preset(raw.as_deref())
}

/// Resolve the persisted run preset from `app_settings` (`agent.preset`) plus
/// any active project `.nexora/` override, defaulting to `Coding`.
#[must_use]
pub(crate) fn resolve_preset(db: &Database) -> RunPreset {
    resolve_preset_for_workspace(db, None)
}

/// Setting key backing the approval-park deadline
/// (`agent.approval_timeout_secs`, NEX-AGENT-004).
pub(crate) use nexora_core::config::APPROVAL_TIMEOUT_KEY;

/// Default approval-park deadline in seconds (10 minutes — mirrors
/// [`super::approval::DEFAULT_APPROVAL_TIMEOUT`]; the `u64` form is what the
/// settings store resolves).
pub(crate) const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 10 * 60;

/// Bounds for a stored approval-park deadline in seconds: below one second
/// the deadline is indistinguishable from "never park"; above a day it stops
/// being a deadline. Out-of-domain values fall back to the default (the
/// command gate in `commands/settings.rs` enforces the same domain, so the
/// two agree exactly).
pub(crate) use nexora_core::config::{MAX_APPROVAL_TIMEOUT_SECS, MIN_APPROVAL_TIMEOUT_SECS};

/// Resolve the approval-park deadline from `app_settings`
/// (`agent.approval_timeout_secs`), defaulting to
/// [`DEFAULT_APPROVAL_TIMEOUT_SECS`] when unset, unparseable, or outside
/// [`MIN_APPROVAL_TIMEOUT_SECS`]..=[`MAX_APPROVAL_TIMEOUT_SECS`] — mirroring
/// [`resolve_spend_limit`]: an unreadable setting degrades to the default
/// and never fails the run.
#[must_use]
pub(crate) fn resolve_approval_timeout_for_workspace(
    db: &Database,
    workspace_root: Option<&Path>,
) -> std::time::Duration {
    let raw = crate::application::settings::resolve_effective_setting(
        db,
        workspace_root,
        APPROVAL_TIMEOUT_KEY,
        None,
    );
    match raw {
        Some(value) => match value.as_str().trim().parse::<u64>() {
            Ok(secs) if (MIN_APPROVAL_TIMEOUT_SECS..=MAX_APPROVAL_TIMEOUT_SECS).contains(&secs) => {
                std::time::Duration::from_secs(secs)
            }
            _ => std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
        },
        None => std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
    }
}

#[must_use]
pub(crate) fn resolve_approval_timeout(db: &Database) -> std::time::Duration {
    resolve_approval_timeout_for_workspace(db, None)
}
/// Setting key backing the per-run spend guard (`agent.spend_limit_micro_usd`).
pub(crate) use nexora_core::config::SPEND_LIMIT_KEY;

#[must_use]
pub(crate) fn resolve_effective_effort_for_workspace(
    db: &Database,
    workspace_root: Option<&Path>,
    provider: &str,
    model: &str,
) -> Option<String> {
    let raw = crate::application::settings::resolve_effective_setting(
        db,
        workspace_root,
        nexora_core::config::SELECTED_EFFORT_KEY,
        None,
    )?;
    let parsed = ReasoningEffort::parse(&raw)?;
    let caps = crate::application::capabilities::resolve_model_capabilities(db, provider, model);
    let candidate = parsed.as_str();
    caps.supported_efforts
        .iter()
        .any(|e| e == candidate)
        .then(|| candidate.to_string())
}

#[must_use]
pub(crate) fn resolve_effective_effort(
    db: &Database,
    provider: &str,
    model: &str,
) -> Option<String> {
    resolve_effective_effort_for_workspace(db, None, provider, model)
}

/// Resolve the persisted per-run spend limit in micro-USD through
/// `Project (.nexora/) > Personal > Default`, mirroring [`resolve_autonomy_mode`].
#[must_use]
pub(crate) fn resolve_spend_limit_for_workspace(
    db: &Database,
    workspace_root: Option<&Path>,
) -> Option<u64> {
    let raw = crate::application::settings::resolve_effective_setting(
        db,
        workspace_root,
        SPEND_LIMIT_KEY,
        None,
    )?;
    match raw.as_str().trim().parse::<u64>() {
        Ok(limit) if limit > 0 => Some(limit),
        _ => None,
    }
}

/// Resolve the persisted per-run spend limit in micro-USD, mirroring
/// [`resolve_autonomy_mode`]: `None` means "no limit" and preserves today's
/// behaviour.
#[must_use]
pub(crate) fn resolve_spend_limit(db: &Database) -> Option<u64> {
    resolve_spend_limit_for_workspace(db, None)
}

/// Canonical routing-profile load for the run path (closes #81).
///
/// Resolves the agent profile through
/// [`RoutingService::resolve`] (workspace → global → default — never a
/// direct `load`) and surfaces a present-but-invalid workspace file on the
/// run's audit trail with the fixed-vocabulary
/// [`AuditEvent::RoutingFallback`] event on the genuine `Queued → Running`
/// setup edge. A corrupt global value is reported with fixed vocabulary and
/// an unreadable settings tier degrades silently: the run never fails for
/// routing, and the request's explicit provider/model always win.
pub(crate) fn note_routing_fallback(db: &Database, workspace_root: &Path, log: &AuditLog) {
    match RoutingService::new(db).resolve(Some(workspace_root), TaskKind::Agent) {
        Ok(resolved) => {
            if resolved.notice.is_some() {
                audit(
                    Some(log),
                    RunState::Queued,
                    RunState::Running,
                    AuditEvent::RoutingFallback,
                );
            }
        }
        Err(err) => {
            log::warn!("agent run setup: routing profile unreadable, continuing: {err}");
        }
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub(crate) fn start_run(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
) -> Result<i64, AgentRunError> {
    start_run_with_options(
        db,
        registry,
        host,
        executor,
        workspace_root,
        request,
        mode,
        preset,
        RunLaunchOptions::default(),
    )
}

/// Start one agent run with explicit launch options (`is_plan`, continuation parent, extra history).
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub(crate) fn start_run_with_options(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
    options: RunLaunchOptions,
) -> Result<i64, AgentRunError> {
    let access = super::access::AccessContext::root(db).with_workspace(&workspace_root);
    start_run_scoped_with_options(
        db,
        registry,
        host,
        executor,
        workspace_root,
        request,
        mode,
        preset,
        access,
        options,
    )
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub(crate) fn start_run_with_parent(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
    parent: &super::access::AccessContext,
) -> Result<i64, AgentRunError> {
    start_run_scoped(
        db,
        registry,
        host,
        executor,
        workspace_root,
        request,
        mode,
        preset,
        parent.child(parent.profile()),
    )
}

/// Explicitly continue an `interrupted` run (`M11` / `P06`) without re-running completed
/// tool calls or restoring stale approval caches.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
pub(crate) fn continue_run(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    run_id: i64,
    provider: String,
    model: String,
    credential: String,
    mode: AutonomyMode,
    preset: RunPreset,
) -> Result<i64, AgentRunError> {
    if super::tools::path_safety::check_private(&workspace_root).is_err() {
        return Err(AgentRunError::WorkspaceProtected);
    }
    let repo = AgentRunRepository::new(db);
    let (parent, source) = repo
        .read_run_with_source(run_id)
        .map_err(AgentRunError::Database)?
        .ok_or(AgentRunError::RunNotFound { run_id })?;
    if !parent.interrupted && parent.status != "interrupted" {
        return Err(AgentRunError::RunNotInterrupted {
            status: parent.status,
        });
    }
    let source = source.ok_or(AgentRunError::ConversationNotFound { id: 0 })?;
    let conversation_id = source.conversation_id;

    let prior_steps = repo.list_steps(run_id).map_err(AgentRunError::Database)?;
    let q_repo = RunQuestionRepository::new(db);
    let prior_questions: Vec<_> = q_repo
        .list_for_conversation(conversation_id)
        .map_err(AgentRunError::Database)?
        .into_iter()
        .filter(|q| q.run_id == run_id)
        .collect();
    let is_plan = RunPlanRepository::new(db)
        .read_by_run(run_id)
        .map_err(AgentRunError::Database)?
        .is_some();
    let injection_enabled = crate::application::flags::FlagService::new(db)
        .resolve_all(Some(&workspace_root))
        .run_flags()
        .injection;
    let plan = super::resume::build_continuation_messages(
        &prior_steps,
        &prior_questions,
        injection_enabled,
        is_plan,
    );
    let spend_limit_micro_usd = resolve_spend_limit_for_workspace(db, Some(&workspace_root));

    start_run_with_options(
        db,
        registry,
        host,
        executor,
        workspace_root,
        AgentRunRequest {
            conversation_id,
            user_request: plan.continuation_prompt,
            provider,
            model,
            credential,
            max_iterations: None,
            spend_limit_micro_usd,
        },
        mode,
        preset,
        RunLaunchOptions {
            is_plan,
            extra_history: plan.extra_messages,
            persist_user_message: false,
            attachment_ids: Vec::new(),
            source: Some(RunLaunchSource::Continue(source)),
            prompt_context: None,
        },
    )
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn start_run_scoped(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
    access: super::access::AccessContext,
) -> Result<i64, AgentRunError> {
    start_run_scoped_with_options(
        db,
        registry,
        host,
        executor,
        workspace_root,
        request,
        mode,
        preset,
        access,
        RunLaunchOptions::default(),
    )
}

#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn start_run_scoped_with_options(
    db: &Database,
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
    access: super::access::AccessContext,
    options: RunLaunchOptions,
) -> Result<i64, AgentRunError> {
    if super::tools::path_safety::check_private(&workspace_root).is_err() {
        return Err(AgentRunError::WorkspaceProtected);
    }
    if options.is_plan {
        access.set_plan(true);
    }
    // Executor availability is the caller's contract: every call site resolves
    // the executor through `execution::resolve_executor` first (agent command,
    // task loop bridge, e2e smoke) and fails with the same classified
    // `ExecutorUnavailable` before this bridge is reached, then hands the
    // resolved executor in. Re-checking here would consult the process
    // registry rather than the injected executor — a vacuous check — so it is
    // deliberately not repeated (NEX-ARCH-002: the resolver is installed by
    // the composition root; unit tests inject stubs instead).

    // DP-4: claim synchronously so a second start for the same conversation
    // is rejected even before the run thread registers its entry.
    if !registry.claim_conversation(request.conversation_id) {
        return Err(AgentRunError::RunAlreadyActive {
            conversation_id: request.conversation_id,
        });
    }

    let started = start_run_claimed(
        db,
        &registry,
        host,
        executor,
        workspace_root,
        &request,
        mode,
        preset,
        access,
        options,
    );
    if started.is_err() {
        registry.unclaim_conversation(request.conversation_id);
    } else {
        // Local usage ledger (counts only): one best-effort tick per
        // started run — a ledger failure never fails the start.
        crate::application::privacy::record(db, "agent_run");
    }
    started
}

/// Load the Layer-2 action trace for `conversation_id` (best-effort): the
/// newest [`action_memory::MAX_PRIOR_RUNS`] non-live runs with their `seq`
/// -ordered steps, mapped onto [`AgentStepView`] and compressed by
/// [`action_memory::summarize`].
///
/// Any failure yields an empty trace and the run continues. Only counts are
/// logged — argument/observation content travels in-memory only, never into
/// logs and never beyond the prompt.
fn load_action_summary(db: &Database, conversation_id: i64) -> ActionSummary {
    let repo = AgentRunRepository::new(db);
    let runs = match repo.list_runs_by_conversation(conversation_id) {
        Ok(runs) => runs,
        Err(err) => {
            log::warn!("agent run setup: action trace load failed, continuing empty: {err}");
            return action_memory::summarize(&[]);
        }
    };
    let mut steps_by_run: Vec<(i64, Vec<AgentStepView>)> = Vec::new();
    for run in runs
        .iter()
        .filter(|run| run.status != "running")
        .take(action_memory::MAX_PRIOR_RUNS)
    {
        match repo.list_steps(run.id) {
            Ok(steps) => steps_by_run.push((
                run.id,
                steps
                    .into_iter()
                    .map(|step| AgentStepView {
                        tool_name: step.tool_name.unwrap_or_default(),
                        arguments: step.arguments.unwrap_or_default(),
                        observation: step.observation.unwrap_or_default(),
                        status: step.status.unwrap_or_default(),
                    })
                    .collect(),
            )),
            Err(err) => {
                log::warn!("agent run setup: action trace load failed, continuing empty: {err}");
                return action_memory::summarize(&[]);
            }
        }
    }
    action_memory::summarize(&steps_by_run)
}

/// The post-claim setup: user message, run row, registration, spawn.
// Keep pre-spawn persistence and failure handling in one launch path.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn start_run_claimed(
    db: &Database,
    registry: &Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    request: &AgentRunRequest,
    mode: AutonomyMode,
    preset: RunPreset,
    access: super::access::AccessContext,
    options: RunLaunchOptions,
) -> Result<i64, AgentRunError> {
    // Load the persisted history BEFORE persisting the current user
    // message (agent memory slice): after the persist the current turn is
    // already in the table, so loading first is what makes it appear exactly
    // once in the run's context.
    let mut history: Vec<AiMessage> = match ConversationService::new(db)
        .agent_history(request.conversation_id, &request.provider)
    {
        Ok(history) => history,
        Err(crate::application::conversations::ConversationError::NotFound { id }) => {
            return Err(AgentRunError::ConversationNotFound { id });
        }
        // Lifecycle coordination must fail closed before any provider request.
        Err(crate::application::conversations::ConversationError::Database(
            err @ crate::infrastructure::database::DatabaseError::Io(_),
        )) => {
            return Err(AgentRunError::Database(err));
        }
        Err(other) => {
            log::warn!("agent run setup: history load failed, continuing empty: {other}");
            Vec::new()
        }
    };
    history.extend(options.extra_history);

    // Load the prior action trace AFTER the text history (which stays first)
    // and BEFORE persisting the current user message. The current run row is
    // created below, so no live row exists yet here — and any live row is
    // still excluded defensively. Best-effort: any failure keeps the run
    // going with an empty trace.
    let action_summary = load_action_summary(db, request.conversation_id);

    // Prepare and link current attachments synchronously before spawning.
    // Current bytes are appended by the runner exactly once, after history.
    let current_attachments = if options.persist_user_message && options.source.is_none() {
        ConversationService::new(db)
            .persist_user_message_with_attachments(
                request.conversation_id,
                &request.user_request,
                &request.provider,
                &request.model,
                &options.attachment_ids,
            )
            .map(|(_, attachments)| attachments)
            .map_err(|err| match err {
                crate::application::conversations::ConversationError::NotFound { id } => {
                    AgentRunError::ConversationNotFound { id }
                }
                crate::application::conversations::ConversationError::Database(err) => {
                    AgentRunError::Database(err)
                }
                other => AgentRunError::Attachment(other),
            })?
    } else {
        Vec::new()
    };

    // Create the linked run row (D50) before spawning the run that adopts it.
    let checkpoint_store = host.checkpoint_store();
    let gate = ApprovalGate::new(mode)
        .with_access(access)
        .with_checkpoints(checkpoint_store.clone());
    gate.bind_workspace(&workspace_root);
    // NEX-AGENT-004: every park on this run expires on the configured
    // deadline, so an unanswered approval can never hold the conversation
    // claim forever.
    gate.set_timeout(resolve_approval_timeout_for_workspace(
        db,
        Some(&workspace_root),
    ));
    let control = RunControl::new();
    let mode_column = mode_to_column(mode);
    let prompt_no_shell = options.source.is_some_and(|source| source.run().no_shell)
        || options.prompt_context.is_some();
    let run_id = if options.is_plan || prompt_no_shell || options.source.is_some() {
        AgentRunRepository::new(db)
            .create_run_with(
                Some(request.conversation_id),
                &request.model,
                mode_column,
                |tx, run_id| {
                    if let Some(source) = options.source {
                        source.run().verify_in(tx, request.conversation_id)?;
                        match source {
                            RunLaunchSource::Continue(source) => {
                                RunQuestionRepository::cancel_pending_for_run_in(
                                    tx,
                                    source.run_id,
                                )?;
                                AgentRunRepository::record_continuation_in(tx, run_id, source)?;
                            }
                            RunLaunchSource::Plan(source) => {
                                RunPlanRepository::verify_pending_in(tx, source)?;
                                if options.persist_user_message {
                                    MessageRepository::create_in_transaction(
                                        tx,
                                        request.conversation_id,
                                        "user",
                                        &request.user_request,
                                        None,
                                        None,
                                    )?;
                                    ConversationRepository::touch_updated_at(
                                        tx,
                                        request.conversation_id,
                                    )?;
                                }
                            }
                        }
                    }
                    if prompt_no_shell {
                        super::super::prompt_context::mark_run_no_shell(tx, run_id)?;
                    }
                    if options.is_plan {
                        RunPlanRepository::mark_started_in_transaction(
                            tx,
                            run_id,
                            request.conversation_id,
                        )?;
                    }
                    Ok(())
                },
            )
            .map_err(AgentRunError::Database)?
    } else {
        RunRecorder::new(db)
            .with_conversation(request.conversation_id)
            .create_run_row(&request.model, mode_column)
            .ok_or(AgentRunError::RunNotPersisted)?
    };
    let preset = if prompt_no_shell {
        RunPreset::Document
    } else {
        preset
    };

    observe_transition(RunState::Queued, RunState::Running);
    registry.register(
        run_id,
        ActiveAgentRun {
            conversation_id: request.conversation_id,
            control: control.clone(),
            gate: gate.clone(),
        },
    );

    notify_durable(db, host.as_ref(), run_id, request.conversation_id, "start");
    spawn_run(
        Arc::clone(registry),
        host,
        db.clone(),
        executor,
        workspace_root,
        run_id,
        control,
        gate,
        preset,
        request.clone(),
        history,
        action_summary,
        checkpoint_store,
        options.is_plan,
        current_attachments,
        options.prompt_context,
    )?;
    Ok(run_id)
}

/// Spawn the run thread and the forwarder thread (design §2.1).
///
/// `request`/`host` are consumed by the thread closures; the owned handles are
/// the bridge's contract, and the arity mirrors the thread boundaries each
/// value is destined for.
// Keep the owned run/forwarder thread setup and commit-notification boundary
// together; splitting these closures would obscure their channel lifetimes.
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]
fn spawn_run(
    registry: Arc<AgentRunRegistry>,
    host: Arc<dyn AgentRunHost>,
    db: Database,
    executor: Arc<dyn ProviderExecutor + Send + Sync>,
    workspace_root: PathBuf,
    run_id: i64,
    control: RunControl,
    gate: ApprovalGate,
    preset: RunPreset,
    request: AgentRunRequest,
    history: Vec<AiMessage>,
    action_summary: ActionSummary,
    checkpoint_store: Option<CheckpointStore>,
    is_plan: bool,
    current_attachments: Vec<crate::application::execution::AiAttachment>,
    prompt_context: Option<String>,
) -> Result<(), AgentRunError> {
    let (tx, rx): (Sender<AgentRunEvent>, Receiver<AgentRunEvent>) = mpsc::channel();
    // Terminal-frame channel: the run thread sends the `RunFinished` payload
    // here (never directly to the host) so the forwarder emits it only AFTER
    // the run-event channel disconnects — `Finished` is therefore the last
    // frame of every run, no matter how many events were buffered.
    let (finish_tx, finish_rx): (Sender<RunFinished>, Receiver<RunFinished>) = mpsc::channel();

    // Run thread: executes the ReAct loop, persists the assistant message on
    // success, forwards the terminal frame, and releases the registry entry on
    // every exit path (DP-9: a panic leaks the entry until app exit — the
    // runner is panic-free by design).
    let forward_db = db.clone();
    let forward_conversation_id = request.conversation_id;
    let forward_registry = Arc::clone(&registry);
    let run_registry = Arc::clone(&registry);
    let run_host = Arc::clone(&host);
    let run_request = request;
    let run_handle = std::thread::Builder::new()
        .name(format!("agent-run-{run_id}"))
        .spawn(move || {
            // The recorder borrows this thread's sender clone; the runner owns
            // the recorder, so the borrow lives exactly as long as the loop.
            let qctx = QuestionRunContext {
                db: db.clone(),
                run_id,
                conversation_id: run_request.conversation_id,
                gate: control.question_gate().clone(),
            };
            let outcome = {
                let tx_for_recorder = tx.clone();
                let recorder = RunRecorder::new(&db)
                    .with_run_id(run_id)
                    .with_conversation(run_request.conversation_id)
                    .with_events(&tx_for_recorder);
                let permission_store = super::permissions::PermissionStore::load(&db);
                // 2.0 flags: resolve the workspace → global → default gates
                // once per run (best-effort; unreadable tiers degrade to the
                // current behavior). An invalid workspace flags file is
                // reported with fixed vocabulary and never fails the run.
                let resolved_flags = crate::application::flags::FlagService::new(&db)
                    .resolve_all(Some(&workspace_root));
                if let Some(notice) = resolved_flags.notice {
                    log::warn!("agent run setup: {notice}");
                }
                // Canonical routing-profile load (#81): the agent profile is
                // resolved through `RoutingService::resolve` (never a direct
                // `load`), and a present-but-invalid workspace file is
                // surfaced on the run's audit trail with the fixed-vocabulary
                // `routing_fallback` event. The run's explicit
                // provider/model always win; the profile load only validates
                // and reports. Nothing here ever fails the run.
                let audit_log = Arc::new(super::governance::AuditLog::new());
                note_routing_fallback(&db, &workspace_root, &audit_log);
                // Task T4 / M16 wiring: resolve authoritative model window from
                // `(connection_id, model_id)` capabilities (`ManualOverride >
                // ApiMetadata > BuiltInCatalog > Unknown(0)`). Unknown stays `0`
                // so proactive threshold compaction stays dormant.
                let context_limit = crate::application::capabilities::resolve_model_capabilities(
                    &db,
                    run_request.provider.as_str(),
                    run_request.model.as_str(),
                )
                .context_window
                .map_or(0, u64::from);
                let active_plan_text = RunPlanRepository::new(&db)
                    .list_for_conversation(run_request.conversation_id)
                    .ok()
                    .and_then(|plans| {
                        plans.into_iter().rev().find_map(|p| {
                            matches!(
                                p.decision_status.as_str(),
                                "accepted" | "custom_reply" | "pending"
                            )
                            .then_some(p.plan_content)
                            .flatten()
                        })
                    });
                let mcp_cancel = control.token().clone();
                let mut runner = AgentRunner::new(executor.as_ref(), &workspace_root)
                    .with_plan(is_plan)
                    .with_control(control)
                    .with_approval_gate(gate)
                    .with_question_context(qctx)
                    .with_preset(preset)
                    .with_permission_store(permission_store)
                    .with_history(history)
                    .with_attachments(current_attachments)
                    .with_prompt_context(prompt_context)
                    .with_action_summary(action_summary)
                    .with_context_limit(context_limit)
                    .with_run_flags(resolved_flags.run_flags())
                    .with_audit_log(Arc::clone(&audit_log))
                    .with_event_sender(tx_for_recorder.clone())
                    .with_stream_deltas(true);
                if let Some(plan_text) = active_plan_text {
                    runner = runner.with_active_plan(plan_text);
                }
                if !is_plan {
                    if let Some(mcp_runtime) =
                        crate::application::mcp::LiveMcpRuntime::discover(&db, &mcp_cancel)
                    {
                        runner = runner.with_mcp_runtime(Arc::new(mcp_runtime));
                    }
                }
                match crate::application::project_dir::discover_instructions(&workspace_root, None)
                {
                    Ok(instructions) => {
                        if !instructions.skipped_escapes.is_empty() {
                            log::warn!(
                                "agent run setup: skipped symlink-escaping instruction file(s)"
                            );
                        }
                        if !instructions.combined.is_empty() {
                            runner = runner.with_project_instructions(instructions.combined);
                        }
                    }
                    Err(err) => {
                        log::warn!("agent run setup: instruction discovery skipped: {err}");
                    }
                }
                if let Some(effort) = resolve_effective_effort_for_workspace(
                    &db,
                    Some(&workspace_root),
                    &run_request.provider,
                    &run_request.model,
                ) {
                    runner = runner.with_model_config(
                        ModelConfig::new(&run_request.model).with_effort(Some(effort)),
                    );
                }
                if let Some(max_iterations) = run_request.max_iterations {
                    runner = runner.with_max_iterations(max_iterations);
                }
                if let Some(limit) = run_request.spend_limit_micro_usd {
                    runner = runner.with_spend_limit(limit);
                }
                if let Some(store) = checkpoint_store.as_ref() {
                    if store.verify_for_workspace(&workspace_root).is_ok() {
                        runner = runner.with_checkpoints(CheckpointRunContext::new(
                            Arc::new(db.clone()),
                            store.clone(),
                            run_request.conversation_id,
                            run_id,
                        ));
                    }
                }
                runner.with_run_recorder(recorder).run(
                    &run_request.provider,
                    &run_request.model,
                    &run_request.credential,
                    &run_request.user_request,
                )
            };

            // Assistant message only on success — never a fake assistant
            // message on failure (plain-chat doctrine). Plan runs persist the
            // structured plan in `run_plans` + `agent_runs.final_content` and emit
            // `PlanReady` rather than duplicating an assistant bubble below it.
            if let Ok(content) = &outcome {
                if is_plan {
                    match RunPlanRepository::new(&db).create(
                        run_id,
                        run_request.conversation_id,
                        content,
                    ) {
                        Ok(plan) => {
                            let _ = tx.send(AgentRunEvent::PlanReady { plan });
                        }
                        Err(err) => {
                            log::warn!("agent run plan persistence failed (run {run_id}): {err}");
                        }
                    }
                } else {
                    run_host.persist_assistant_message(
                        run_request.conversation_id,
                        content,
                        &run_request.provider,
                        &run_request.model,
                    );
                }
            }

            let (status, final_content, error) = terminal_outcome(&outcome);
            let _ = RunQuestionRepository::new(&db).cancel_pending_for_run(run_id);
            // Best-effort: if the send fails the run is already finished and
            // the terminal state is persisted on the `agent_runs` row.
            let _ = finish_tx.send(RunFinished {
                conversation_id: run_request.conversation_id,
                status: status.to_string(),
                final_content,
                error,
            });
            run_registry.release(run_id);
            // `tx` and `finish_tx` drop at scope end: the run-event channel
            // disconnects (forwarder flushes to Finished) and the terminal
            // channel disconnects after the frame is consumed.
        })
        .map_err(|err| {
            registry.release(run_id);
            AgentRunError::ThreadSpawn(err.to_string())
        })?;

    // Forwarder thread: drains the run-event channel into the host until
    // disconnect — the disconnect IS the drain guarantee — and only then
    // forwards the terminal frame from the finish channel, so `Finished` is
    // the last frame of every run.
    let forward_host = host;
    let forward_handle = std::thread::Builder::new()
        .name(format!("agent-run-forwarder-{run_id}"))
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                if let AgentRunEvent::StepRecorded { seq, .. } = &event {
                    notify_durable(
                        &forward_db,
                        forward_host.as_ref(),
                        run_id,
                        forward_conversation_id,
                        &format!("step:{seq}"),
                    );
                } else if let AgentRunEvent::ContextCompacted {
                    event_id: Some(event_id),
                    ..
                } = &event
                {
                    forward_host.emit(&RunFrame::Durable {
                        run_id,
                        conversation_id: forward_conversation_id,
                        event_id: *event_id,
                    });
                }
                forward_host.emit(&RunFrame::with_event(
                    run_id,
                    forward_conversation_id,
                    event,
                ));
            }
            // `rx` disconnected: every buffered run event has been emitted.
            // The run thread sent the terminal frame before dropping its
            // senders (and before releasing the registry entry), so it is
            // either buffered or in flight here — never lost.
            while let Ok(event) = finish_rx.recv() {
                let completed = event.status == "completed";
                notify_durable(
                    &forward_db,
                    forward_host.as_ref(),
                    run_id,
                    forward_conversation_id,
                    "terminal",
                );
                forward_host.emit(&RunFrame::Finished { run_id, event });
                if completed {
                    forward_host
                        .on_run_completed(forward_conversation_id, Arc::clone(&forward_registry));
                }
            }
        });
    let forward_handle = match forward_handle {
        Ok(handle) => handle,
        Err(err) => {
            // No forwarder means no event stream: abort the run and clean up.
            // The already-spawned run thread was cancelled and exits
            // promptly — retain its handle so shutdown can still join it.
            registry.track_threads(run_id, vec![run_handle]);
            let _ = registry.cancel(run_id);
            registry.release(run_id);
            return Err(AgentRunError::ThreadSpawn(err.to_string()));
        }
    };
    // NEX-TASK-002: retain both handles so orderly shutdown can join them.
    registry.track_threads(run_id, vec![run_handle, forward_handle]);
    Ok(())
}

/// Errors returned by [`clarify_run`].
#[derive(Debug)]
pub(crate) enum ClarifyRunError {
    EmptyMessage,
    RunNotActive,
    Database(crate::infrastructure::database::DatabaseError),
}

/// Enqueue a live `Clarify now` steering message onto an active run and persist
/// the user message into the owning conversation.
pub(crate) fn clarify_run(
    db: &Database,
    registry: &AgentRunRegistry,
    run_id: i64,
    content: &str,
) -> Result<i64, ClarifyRunError> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(ClarifyRunError::EmptyMessage);
    }
    let Some(conversation_id) = registry.push_steering(run_id, trimmed.to_string()) else {
        return Err(ClarifyRunError::RunNotActive);
    };
    if let Err(err) = ConversationService::new(db).persist_user_message(conversation_id, trimmed) {
        log::warn!("clarify_run: failed to persist user message: {err}");
        if let crate::application::conversations::ConversationError::Database(db_err) = err {
            return Err(ClarifyRunError::Database(db_err));
        }
    }
    Ok(conversation_id)
}

// ---------------------------------------------------------------------------
// Rehydration helpers (application-layer wrappers over the run repository)
// ---------------------------------------------------------------------------

/// List the runs of one conversation, newest first (`started_at` DESC).
///
/// Unpaged. The rehydration command uses
/// [`list_runs_for_conversation_paged`] (AC-102/ADD-08) so a long history
/// cannot fan the read out unbounded.
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_runs_for_conversation(
    db: &Database,
    conversation_id: i64,
) -> Result<Vec<AgentRun>, crate::infrastructure::database::DatabaseError> {
    AgentRunRepository::new(db).list_runs_by_conversation(conversation_id)
}

/// Paged variant of [`list_runs_for_conversation`] (AC-102/ADD-08): `limit`
/// bounds the page, `before_run_id` is the exclusive keyset cursor (the
/// previous page's last run id). `None` keeps the historical unbounded
/// behavior.
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_runs_for_conversation_paged(
    db: &Database,
    conversation_id: i64,
    limit: Option<i64>,
    before_run_id: Option<i64>,
) -> Result<Vec<AgentRun>, crate::infrastructure::database::DatabaseError> {
    AgentRunRepository::new(db).list_runs_by_conversation_paged(
        conversation_id,
        limit,
        before_run_id,
    )
}

/// Paged conversation run listing enriched with `run_plans` (`M09`) and
/// `run_continuations` (`M11`) metadata.
pub(crate) fn list_runs_for_conversation_summary(
    db: &Database,
    conversation_id: i64,
    limit: Option<i64>,
    before_run_id: Option<i64>,
) -> Result<Vec<AgentRunSummary>, crate::infrastructure::database::DatabaseError> {
    let runs = list_runs_for_conversation_paged(db, conversation_id, limit, before_run_id)?;
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let plans_by_run: std::collections::HashMap<i64, RunPlan> = RunPlanRepository::new(db)
        .list_for_conversation(conversation_id)?
        .into_iter()
        .map(|plan| (plan.run_id, plan))
        .collect();
    let continuations_by_run: std::collections::HashMap<i64, i64> = AgentRunRepository::new(db)
        .list_continuations_for_conversation(conversation_id)?
        .into_iter()
        .collect();

    Ok(runs
        .into_iter()
        .map(|run| {
            let plan = plans_by_run.get(&run.id);
            let continued_from_run_id = continuations_by_run.get(&run.id).copied();
            AgentRunSummary {
                is_plan: plan.is_some(),
                plan_content: plan.and_then(|p| p.plan_content.clone()),
                plan_decision_status: plan.map(|p| p.decision_status.clone()),
                plan_decision_reply: plan.and_then(|p| p.decision_reply.clone()),
                plan_implemented_run_id: plan.and_then(|p| p.implemented_run_id),
                continued_from_run_id,
                run,
            }
        })
        .collect())
}

/// Bounded DTO for durable history. Original identities are retained, with
/// exactly the same step previews as the existing history path.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurableRunEvent {
    pub event_id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub kind: String,
    pub step_id: Option<i64>,
    pub schema_version: i64,
    pub created_at: i64,
    pub payload: Option<String>,
    pub step: Option<AgentStepListItem>,
    pub terminal: Option<RunFinished>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurableRunEventPage {
    pub events: Vec<DurableRunEvent>,
    pub next_cursor: i64,
    pub high_water_event_id: i64,
    pub has_more: bool,
}

/// Read-only reconciliation. Does not resolve credentials or restore work.
pub(crate) fn list_conversation_run_events(
    db: &Database,
    conversation_id: i64,
    after_event_id: i64,
    limit: i64,
) -> Result<DurableRunEventPage, crate::infrastructure::database::DatabaseError> {
    let page = AgentRunRepository::new(db).list_conversation_run_events(
        conversation_id,
        after_event_id,
        limit,
    )?;
    Ok(DurableRunEventPage {
        events: page
            .events
            .into_iter()
            .map(|event| DurableRunEvent {
                event_id: event.event_id,
                conversation_id: event.conversation_id,
                run_id: event.run_id,
                kind: event.kind,
                step_id: event.step_id,
                schema_version: event.schema_version,
                created_at: event.created_at,
                payload: event.payload,
                step: event.step.as_ref().map(AgentStepListItem::from_row),
                terminal: event.terminal.map(|run| {
                    let final_content = run
                        .final_content
                        .as_deref()
                        .map(super::persistence::history_preview);
                    RunFinished {
                        conversation_id,
                        status: if run.interrupted {
                            "interrupted".to_string()
                        } else {
                            run.status
                        },
                        final_content,
                        error: run
                            .error
                            .as_deref()
                            .map(super::persistence::history_preview),
                    }
                }),
            })
            .collect(),
        next_cursor: page.next_cursor,
        high_water_event_id: page.high_water_event_id,
        has_more: page.has_more,
    })
}

fn notify_durable(
    db: &Database,
    host: &dyn AgentRunHost,
    run_id: i64,
    conversation_id: i64,
    suffix: &str,
) {
    if let Ok(Some(event_id)) =
        AgentRunRepository::new(db).event_id_for_source(&format!("run:{run_id}:{suffix}"))
    {
        host.emit(&RunFrame::Durable {
            run_id,
            conversation_id,
            event_id,
        });
    }
}

/// Outcome returned by manual conversation compaction (`compact_conversation_context`, M16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct CompactionOutcomeView {
    /// `"compacted"` | `"scheduled"` | `"nothing_to_compact"`.
    pub status: String,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub event_id: Option<i64>,
    pub reason: String,
    pub goal: String,
    pub active_plan: String,
    pub key_decisions: String,
    pub touched_files: Vec<String>,
    pub summary: String,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub summarized_messages: usize,
    pub retained_messages: usize,
    pub created_at: i64,
}

fn extract_step_touched_path(arguments: Option<&str>) -> Option<String> {
    let args = arguments?;
    let val = serde_json::from_str::<serde_json::Value>(args).ok()?;
    for key in ["path", "file_path", "target", "file"] {
        if let Some(s) = val.get(key).and_then(serde_json::Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Trigger manual context compaction (`/compact` or `ContextRing` `Compact` button).
///
/// - If an active run exists for `conversation_id`, schedules manual compaction
///   at the next step boundary (`status = "scheduled"`).
/// - Otherwise compacts the persisted uncompacted history into a structured
///   summary preserving **Goal**, **Active Plan**, **Key Decisions**, and
///   **Touched Files / Open Tasks**, persists `conversation_compactions` +
///   durable `context_compacted` event, and emits `RunFrame::Durable` when
///   `host` is provided.
///
/// # Errors
///
/// Returns [`AgentRunError::ConversationNotFound`] when `conversation_id` does
/// not exist or [`AgentRunError::Database`] when persistence fails.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(crate) fn compact_conversation_context(
    db: &Database,
    registry: Option<&AgentRunRegistry>,
    host: Option<&dyn AgentRunHost>,
    executor_target: Option<(&dyn ProviderExecutor, &str, &str, &str)>,
    conversation_id: i64,
    connection_id: Option<&str>,
    model_id: Option<&str>,
) -> Result<CompactionOutcomeView, AgentRunError> {
    if !ConversationRepository::new(db)
        .exists(conversation_id)
        .map_err(AgentRunError::Database)?
    {
        return Err(AgentRunError::ConversationNotFound {
            id: conversation_id,
        });
    }

    if let Some(active_run_id) =
        registry.and_then(|reg| reg.active_run_for_conversation(conversation_id))
    {
        if registry.is_some_and(|reg| reg.request_compaction(active_run_id)) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            return Ok(CompactionOutcomeView {
                status: "scheduled".to_string(),
                conversation_id,
                run_id: Some(active_run_id),
                event_id: None,
                reason: CompactionReason::Manual.as_str().to_string(),
                goal: String::new(),
                active_plan: String::new(),
                key_decisions: String::new(),
                touched_files: Vec::new(),
                summary: String::new(),
                before_tokens: 0,
                after_tokens: 0,
                summarized_messages: 0,
                retained_messages: 0,
                created_at: now,
            });
        }
    }

    let messages = MessageRepository::new(db)
        .list_by_conversation(conversation_id)
        .map_err(AgentRunError::Database)?;
    let repo = AgentRunRepository::new(db);
    let latest_compaction = repo
        .latest_compaction(conversation_id)
        .map_err(AgentRunError::Database)?;
    let watermark_id = latest_compaction.as_ref().map_or(0, |c| c.after_message_id);

    let active_plan_text = RunPlanRepository::new(db)
        .list_for_conversation(conversation_id)
        .ok()
        .and_then(|plans| {
            plans.into_iter().rev().find_map(|p| {
                matches!(
                    p.decision_status.as_str(),
                    "accepted" | "custom_reply" | "pending"
                )
                .then_some(p.plan_content)
                .flatten()
            })
        });

    let runs = repo
        .list_runs_by_conversation(conversation_id)
        .map_err(AgentRunError::Database)?;
    let mut touched_files: Vec<String> = Vec::new();
    for run in runs.iter().take(5) {
        if let Ok(steps) = repo.list_steps(run.id) {
            for step in steps {
                if step.kind == "tool_call" {
                    if let Some(path) = extract_step_touched_path(step.arguments.as_deref()) {
                        if !touched_files.contains(&path) {
                            touched_files.push(path);
                        }
                    }
                }
            }
        }
    }

    let post_watermark: Vec<_> = messages.iter().filter(|m| m.id > watermark_id).collect();
    if post_watermark.is_empty() && active_plan_text.is_none() && touched_files.is_empty() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        return Ok(CompactionOutcomeView {
            status: "nothing_to_compact".to_string(),
            conversation_id,
            run_id: None,
            event_id: None,
            reason: CompactionReason::Manual.as_str().to_string(),
            goal: String::new(),
            active_plan: String::new(),
            key_decisions: String::new(),
            touched_files: Vec::new(),
            summary: String::new(),
            before_tokens: 0,
            after_tokens: 0,
            summarized_messages: 0,
            retained_messages: 0,
            created_at: now,
        });
    }

    let before_tokens =
        conversation_context_stats_for_selection(db, conversation_id, connection_id, model_id)
            .map_or(0, |s| s.category_breakdown.total_accounted_tokens);

    // Keep the newest exchange (up to last 2 messages when >= 3 exist; otherwise fold older).
    let retain_count = if post_watermark.len() >= 3 {
        2
    } else {
        usize::from(post_watermark.len() == 2)
    };
    let fold_len = post_watermark.len().saturating_sub(retain_count);
    let fold_slice = &post_watermark[..fold_len];
    let after_message_id = fold_slice
        .last()
        .map(|m| m.id)
        .or_else(|| post_watermark.last().map(|m| m.id))
        .unwrap_or(watermark_id);

    let mut fold_ai: Vec<AiMessage> = Vec::new();
    if let Some(prev) = &latest_compaction {
        fold_ai.push(AiMessage {
            role: AiRole::User,
            content: prev.summary.clone(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        });
    }
    for msg in fold_slice {
        fold_ai.push(AiMessage {
            role: if msg.role == "assistant" {
                AiRole::Assistant
            } else {
                AiRole::User
            },
            content: msg.content.clone(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        });
    }
    if fold_ai.is_empty() {
        for msg in &post_watermark {
            fold_ai.push(AiMessage {
                role: if msg.role == "assistant" {
                    AiRole::Assistant
                } else {
                    AiRole::User
                },
                content: msg.content.clone(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            });
        }
    }

    let structured = compaction::extract_structured_summary(
        &fold_ai,
        active_plan_text.as_deref(),
        &touched_files,
    );
    let llm_raw = executor_target.and_then(|(executor, provider, model, credential)| {
        let prompt = compaction::summary_prompt_for_slice(&fold_ai);
        let req = AiRequest {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: prompt,
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: Arc::new(Vec::new()),
            model_config: None,
            request_timeout: Some(std::time::Duration::from_secs(30)),
        };
        let token = CancellationToken::new();
        executor
            .execute(&req, credential, &token)
            .ok()
            .map(|r| r.content)
            .filter(|s| !s.trim().is_empty())
    });
    let summary =
        compaction::merge_summary_with_preserved_sections(llm_raw.as_deref(), &structured);

    let retained_messages = post_watermark.len().saturating_sub(fold_len);
    let summarized_messages = fold_len.max(usize::from(!fold_ai.is_empty()));
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let next_seq = repo
        .compaction_count(conversation_id)
        .map_err(AgentRunError::Database)?
        + 1;
    let source_key = format!("conv:{conversation_id}:compaction:{next_seq}:{nonce}");

    let retained_tokens: u64 = post_watermark[fold_len..]
        .iter()
        .map(|m| compaction::estimate_tokens(&m.content))
        .sum();
    let summary_tokens = compaction::estimate_tokens(compaction::SUMMARY_PREFIX)
        .saturating_add(compaction::estimate_tokens(&summary));
    let after_tokens = summary_tokens.saturating_add(retained_tokens);

    let payload_json = serde_json::json!({
        "reason": CompactionReason::Manual.as_str(),
        "goal": structured.goal,
        "active_plan": structured.active_plan,
        "key_decisions": structured.key_decisions,
        "touched_files": structured.touched_files,
        "before_tokens": before_tokens,
        "after_tokens": after_tokens,
        "summarized": summarized_messages,
        "retained": retained_messages,
    })
    .to_string();

    let input = NewConversationCompaction {
        conversation_id,
        run_id: None,
        reason: CompactionReason::Manual.as_str(),
        after_message_id,
        goal: &structured.goal,
        active_plan: &structured.active_plan,
        key_decisions: &structured.key_decisions,
        touched_files: &structured.touched_files,
        summary: &summary,
        before_tokens,
        after_tokens,
        summarized_messages: u64::try_from(summarized_messages).unwrap_or(u64::MAX),
        retained_messages: u64::try_from(retained_messages).unwrap_or(u64::MAX),
    };
    let (_compaction_id, event_id) = repo
        .record_compaction(&input, &source_key, &payload_json)
        .map_err(AgentRunError::Database)?;

    if let Some(h) = host {
        h.emit(&RunFrame::Durable {
            run_id: 0,
            conversation_id,
            event_id,
        });
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    Ok(CompactionOutcomeView {
        status: "compacted".to_string(),
        conversation_id,
        run_id: None,
        event_id: Some(event_id),
        reason: CompactionReason::Manual.as_str().to_string(),
        goal: structured.goal,
        active_plan: structured.active_plan,
        key_decisions: structured.key_decisions,
        touched_files: structured.touched_files,
        summary,
        before_tokens,
        after_tokens,
        summarized_messages,
        retained_messages,
        created_at: now,
    })
}

/// List the steps of one run, `seq` ascending (gap-free per CF-01).
///
/// NEX-PERF-003: the returned items carry a *bounded preview* of each
/// observation plus its byte-exact total — never the full output — so
/// rehydration stays bounded no matter how many turns a run took. The full
/// sanitized text of any step stays reachable through
/// [`get_step_observation_for`]. Callers that need full text for the model
/// (action memory) read the repository directly and are unaffected.
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_steps_for_run(
    db: &Database,
    run_id: i64,
) -> Result<Vec<AgentStepListItem>, crate::infrastructure::database::DatabaseError> {
    list_steps_for_run_paged(db, run_id, None, None)
}

/// Paged variant of [`list_steps_for_run`] (AC-102/ADD-08): `limit` bounds
/// the page, `before_seq` is an exclusive upper-bound cursor on the run's
/// `seq` ordering. `None` keeps the historical full-listing behavior.
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn list_steps_for_run_paged(
    db: &Database,
    run_id: i64,
    limit: Option<i64>,
    before_seq: Option<i64>,
) -> Result<Vec<AgentStepListItem>, crate::infrastructure::database::DatabaseError> {
    Ok(AgentRunRepository::new(db)
        .list_steps_paged(run_id, limit, before_seq)?
        .iter()
        .map(AgentStepListItem::from_row)
        .collect())
}

/// Read the full sanitized `observation` of one `agent_steps` row by its
/// surrogate `id` (NEX-PERF-003 on-demand fetch behind the
/// `get_agent_step_observation` command).
///
/// Returns `Ok(None)` when no step with `step_id` exists (the command maps
/// this to an honest secret-free not-found error). The text is exactly what
/// the recorder stored — sanitized at insert — so this never re-exposes
/// pre-redaction content.
///
/// # Errors
///
/// Propagates [`crate::infrastructure::database::DatabaseError`].
pub(crate) fn get_step_observation_for(
    db: &Database,
    step_id: i64,
) -> Result<Option<String>, crate::infrastructure::database::DatabaseError> {
    AgentRunRepository::new(db).read_step_observation(step_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::application::execution::{AiResponse, ExecutorError, ToolCall};
    use crate::application::settings::SettingsService;

    /// Scripted executor: pops one response per provider call.
    struct ScriptedExecutor {
        steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
    }

    impl ScriptedExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
            }
        }
    }

    impl ProviderExecutor for ScriptedExecutor {
        fn execute(
            &self,
            _request: &crate::application::execution::AiRequest,
            _credential: &str,
            _token: &crate::application::agent::control::CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    /// Fake host: records emitted frames on a channel and assistant
    /// persistence calls for assertions.
    struct FakeHost {
        frames_tx: Sender<RunFrame>,
        db: Database,
        persisted: Mutex<Vec<(i64, String, String, String)>>,
    }

    impl AgentRunHost for FakeHost {
        fn emit(&self, frame: &RunFrame) {
            if let RunFrame::Durable {
                event_id,
                conversation_id,
                ..
            } = frame
            {
                let page =
                    list_conversation_run_events(&self.db, *conversation_id, event_id - 1, 1)
                        .unwrap();
                assert_eq!(
                    page.events[0].event_id, *event_id,
                    "notification follows commit"
                );
            }
            let _ = self.frames_tx.send(frame.clone());
        }

        fn persist_assistant_message(
            &self,
            conversation_id: i64,
            content: &str,
            provider: &str,
            model: &str,
        ) {
            self.persisted.lock().expect("persisted lock").push((
                conversation_id,
                content.to_string(),
                provider.to_string(),
                model.to_string(),
            ));
        }
    }

    fn text_response(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    fn tool_response(name: &str, tool: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: format!("{name}-1"),
                name: tool.to_string(),
                arguments: "{}".to_string(),
                thought_signature: None,
            }],
            usage: None,
        }
    }

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nexora-agent-bridge-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("workspace dir");
        // Canonical-vs-canonical: on Windows the temp dir can sit behind an
        // 8.3 short name (notably `RUNNER~1` on CI runners); the tools'
        // canonical re-check then rejects the raw root with `PathTraversal`.
        // Same one-time resolve as the tools/runner test supports.
        crate::application::agent::tools::test_support::canonical_workspace(&dir)
    }

    fn collect_frames(rx: &Receiver<RunFrame>) -> Vec<RunFrame> {
        let mut frames = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(frame) => {
                    let finished = matches!(frame, RunFrame::Finished { .. });
                    frames.push(frame);
                    if finished {
                        return frames;
                    }
                }
                Err(_) => return frames,
            }
        }
    }

    fn setup(tag: &str) -> (Database, PathBuf, Receiver<RunFrame>, Arc<FakeHost>) {
        let db = crate::infrastructure::database::in_memory_database();
        let workspace = temp_workspace(tag);
        let (tx, rx) = std::sync::mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        (db, workspace, rx, host)
    }

    fn request(conversation_id: i64, content: &str) -> AgentRunRequest {
        AgentRunRequest {
            conversation_id,
            user_request: content.to_string(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            credential: "sk-secret-test-credential".to_string(),
            max_iterations: None,
            spend_limit_micro_usd: None,
        }
    }

    #[test]
    fn journal_production_start_cursor_roundtrip_and_duplicate_delivery() {
        let (db, workspace, rx, host) = setup("journal-roundtrip");
        let conversation_id = ConversationService::new(&db).create("conv").unwrap();
        let run_id = start_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response(&format!(
                "sk-abcdefghijklmnopqrstuv {}",
                "x".repeat(4000)
            )))])),
            workspace,
            request(conversation_id, "q"),
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .unwrap();
        let frames = collect_frames(&rx);
        let live_ids: Vec<_> = frames
            .iter()
            .filter_map(|f| {
                if let RunFrame::Durable { event_id, .. } = f {
                    Some(*event_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(live_ids.len(), 3);
        let mut cursor = 0;
        let mut events = Vec::new();
        loop {
            let page = list_conversation_run_events(&db, conversation_id, cursor, 1).unwrap();
            cursor = page.next_cursor;
            events.extend(page.events);
            if !page.has_more {
                break;
            }
        }
        assert_eq!(
            events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
            live_ids
        );
        assert!(events.iter().all(|e| e.run_id == Some(run_id)));
        let step = events[1].step.as_ref().unwrap();
        assert!(step.observation_truncated);
        assert!(!step
            .observation
            .as_ref()
            .unwrap()
            .contains("sk-abcdefghijklmnopqrstuv"));
        assert_eq!(
            get_step_observation_for(&db, step.id)
                .unwrap()
                .unwrap()
                .len() as u64,
            step.observation_len.unwrap()
        );
        assert_eq!(list_steps_for_run(&db, run_id).unwrap()[0].id, step.id);
        let mut merged = std::collections::BTreeSet::new();
        for id in live_ids
            .iter()
            .chain(live_ids.iter())
            .chain(events.iter().map(|e| &e.event_id))
        {
            merged.insert(*id);
        }
        assert_eq!(merged.len(), 3);
        assert!(
            list_conversation_run_events(&db, conversation_id + 1, 0, 100)
                .unwrap()
                .events
                .is_empty()
        );
        let encoded = serde_json::to_string(&events).unwrap();
        assert!(!encoded.contains("sk-abcdefghijklmnopqrstuv"));
        assert!(
            encoded.len() < 8000,
            "durable output uses bounded projections"
        );
    }

    #[test]
    fn journal_production_start_rejects_checkpoint_before_dispatch() {
        let (db, workspace, _rx, host) = setup("journal-start-failure");
        let conversation_id = ConversationService::new(&db).create("conv").unwrap();
        db.lock().unwrap().execute_batch("CREATE TRIGGER inject_journal_failure BEFORE INSERT ON durable_run_events BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let executor = Arc::new(ScriptedExecutor::new(vec![Ok(text_response(
            "never requested",
        ))]));
        let result = start_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host as Arc<dyn AgentRunHost>,
            executor.clone(),
            workspace,
            request(conversation_id, "q"),
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        );
        assert!(matches!(result, Err(AgentRunError::RunNotPersisted)));
        assert_eq!(executor.steps.lock().unwrap().len(), 1);
        assert!(list_runs_for_conversation(&db, conversation_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn journal_terminal_failure_has_no_durable_ack_or_assistant_success() {
        let (db, workspace, rx, host) = setup("journal-terminal-failure");
        let conversation_id = ConversationService::new(&db).create("conv").unwrap();
        db.lock().unwrap().execute_batch("CREATE TRIGGER inject_journal_failure BEFORE INSERT ON durable_run_events WHEN NEW.kind='run_finished' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let run_id = start_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host.clone() as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("answer"))])),
            workspace,
            request(conversation_id, "q"),
            AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .unwrap();
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().unwrap() else {
            panic!("finished")
        };
        assert_eq!(event.status, "error");
        assert!(event
            .error
            .as_ref()
            .unwrap()
            .contains("history could not be saved"));
        assert!(host.persisted.lock().unwrap().is_empty());
        assert_eq!(
            AgentRunRepository::new(&db)
                .read_run(run_id)
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
        let page = list_conversation_run_events(&db, conversation_id, 0, 100).unwrap();
        assert_eq!(page.events.len(), 2);
        assert!(page.events.iter().all(|e| e.kind != "run_finished"));
        assert_eq!(
            frames
                .iter()
                .filter(|f| matches!(f, RunFrame::Durable { .. }))
                .count(),
            2
        );
    }

    /// Happy path: steps stream in seq order, `Finished` is the last frame
    /// with `completed` + final content, the assistant message is persisted
    /// through the shared path, and the registry entry is released. Also a
    /// leak-negative check: no frame ever contains the credential.
    #[test]
    fn start_run_streams_steps_and_finishes_completed() {
        let (db, workspace, rx, host) = setup("happy");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("t", "read_file")),
                Ok(text_response("final answer")),
            ])),
            workspace,
            request(conversation_id, "do things"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        let frames = collect_frames(&rx);
        assert!(
            !frames.is_empty(),
            "at least the terminal frame must arrive"
        );
        // `Finished` is the LAST frame (drain guarantee).
        let RunFrame::Finished {
            run_id: frame_run,
            event,
        } = frames.last().expect("frames")
        else {
            panic!("last frame must be Finished");
        };
        assert_eq!(*frame_run, run_id);
        assert_eq!(event.conversation_id, conversation_id);
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("final answer"));

        // Step frames are strictly seq-ordered starting at 1.
        let seqs: Vec<i64> = frames
            .iter()
            .filter_map(|frame| match frame {
                RunFrame::Step { event, .. } => Some(event.seq),
                _ => None,
            })
            .collect();
        assert_eq!(
            seqs,
            (1..=seqs.len())
                .map(i64::try_from)
                .collect::<Result<Vec<_>, _>>()
                .expect("seq count fits in i64")
        );

        // Assert the real service forwarder's serialized transport identity,
        // for both persisted steps and transient governance (not durable hints).
        for kind in ["step", "governance"] {
            let live_frames: Vec<_> = frames
                .iter()
                .map(|frame| serde_json::to_value(frame).expect("serialize"))
                .filter(|frame| frame["type"] == kind)
                .collect();
            assert!(!live_frames.is_empty(), "missing {kind} frames");
            for frame in live_frames {
                assert_eq!(frame["conversation_id"], conversation_id);
                assert_eq!(frame["run_id"], run_id);
            }
        }

        // Assistant message persisted through the shared path (DP-7).
        assert_eq!(
            host.persisted.lock().expect("lock").as_slice(),
            [(
                conversation_id,
                "final answer".to_string(),
                "openai".to_string(),
                "test-model".to_string()
            )]
        );

        // Registry cleanup on the success exit path.
        assert!(!registry.is_active(run_id), "run released after finish");

        // Credential never enters any frame.
        for frame in &frames {
            let serialized = serde_json::to_string(frame).expect("serialize");
            assert!(
                !serialized.contains("sk-secret-test-credential"),
                "credential leaked into frame: {serialized}"
            );
        }

        // Rehydration: the persisted row agrees with the stream.
        let runs = list_runs_for_conversation(&db, conversation_id).expect("list runs");
        let run = runs.iter().find(|run| run.id == run_id).expect("run row");
        assert_eq!(run.status, "completed");
        assert_eq!(run.final_content.as_deref(), Some("final answer"));
        assert_eq!(run.conversation_id, Some(conversation_id));
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        assert_eq!(
            steps.iter().map(|step| step.seq).collect::<Vec<_>>(),
            seqs,
            "persisted seqs match the streamed seqs"
        );
        let _ = std::fs::remove_dir_all(temp_workspace("happy"));
    }

    /// Approval park: the mutating tool parks the run (DP-3), a second start
    /// for the same conversation is rejected (DP-4), `resolve` unblocks the
    /// park, and the run then completes.
    #[test]
    fn approval_park_is_resolved_and_duplicate_start_is_rejected() {
        let (db, workspace, rx, host) = setup("approval");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("after approval")),
            ])),
            workspace,
            request(conversation_id, "mutate something"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        // Wait for the approval park.
        let call_id = loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { call_id, .. },
                ..
            } = frame
            {
                break call_id;
            }
        };

        // DP-4: a second concurrent start for the same conversation must be
        // rejected while the first is parked.
        let second = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("no"))])),
            temp_workspace("approval-second"),
            request(conversation_id, "second"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        );
        assert!(
            matches!(second, Err(AgentRunError::RunAlreadyActive { .. })),
            "duplicate conversation start must be rejected, got: {second:?}"
        );

        // Resolve the park through the registry (the IPC command's path).
        assert_eq!(
            registry.resolve(run_id, &call_id, true),
            ResolveOutcome::Resolved
        );

        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("after approval"));
        assert!(!registry.is_active(run_id));
        let _ = std::fs::remove_dir_all(temp_workspace("approval"));
        let _ = std::fs::remove_dir_all(temp_workspace("approval-second"));
    }

    /// Production start and IPC resolver: group consent for A cannot dispatch B.
    #[test]
    #[allow(clippy::too_many_lines)] // Keep the ordered consent/marker/history proof together.
    fn shell_group_consent_requires_a_separate_production_resolution_for_each_call() {
        use crate::application::agent::permissions::resolve_approval;

        for resolution in ["approve", "deny", "cancel"] {
            let (db, workspace, rx, host) = setup(&format!("shell-group-{resolution}"));
            let registry = Arc::new(AgentRunRegistry::default());
            let conversation_id = ConversationService::new(&db).create("conv").unwrap();
            let calls: Vec<_> = [("a", "a.txt"), ("b", "b.txt")]
                .into_iter()
                .map(|(id, file)| ToolCall {
                    id: id.to_string(),
                    name: "execute_command".to_string(),
                    arguments:
                        serde_json::json!({"command": format!("echo {id}>>{file}"), "cwd": "."})
                            .to_string(),
                    thought_signature: None,
                })
                .collect();
            let run_id = start_run(
                &db,
                Arc::clone(&registry),
                host as Arc<dyn AgentRunHost>,
                Arc::new(ScriptedExecutor::new(vec![
                    Ok(AiResponse {
                        tool_calls: calls.clone(),
                        ..text_response("")
                    }),
                    Ok(text_response("done")),
                ])),
                workspace.clone(),
                request(conversation_id, "two shell calls"),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
            )
            .expect("start");
            let mut groups = Vec::new();
            for expected in ["a", "b"] {
                let (call_id, group) = loop {
                    let frame = rx
                        .recv_timeout(Duration::from_secs(10))
                        .expect("pending shell approval");
                    match frame {
                        RunFrame::Governance {
                            event:
                                AgentRunEvent::ApprovalRequested {
                                    call_id, group_key, ..
                                },
                            ..
                        } => break (call_id, group_key),
                        RunFrame::Finished { .. } => {
                            panic!("shell {expected} finished without its own approval")
                        }
                        _ => {}
                    }
                };
                let pending = registry
                    .live_run_snapshot(conversation_id)
                    .unwrap()
                    .pending_approval
                    .unwrap();
                assert_eq!(pending.call_id, call_id);
                assert_eq!(
                    pending.arguments,
                    calls[usize::from(expected == "b")].arguments
                );
                groups.push(group);
                assert!(
                    !workspace.join("b.txt").exists(),
                    "B must not dispatch before its own consent"
                );
                if expected == "a" {
                    assert!(!workspace.join("a.txt").exists());
                    assert!(
                        resolve_approval(&registry, &db, run_id, &call_id, true, Some("group"))
                            .is_err(),
                        "shell group scope is rejected"
                    );
                    resolve_approval(&registry, &db, run_id, &call_id, true, None)
                        .expect("single approval A");
                } else {
                    assert_eq!(
                        std::fs::read_to_string(workspace.join("a.txt"))
                            .unwrap()
                            .trim(),
                        "a"
                    );
                    let steps = list_steps_for_run(&db, run_id).unwrap();
                    let approvals: Vec<_> = steps.iter().filter(|s| s.kind == "approval").collect();
                    assert_eq!(approvals.len(), 1, "no fabricated group approval for B");
                    assert_eq!(
                        approvals[0].arguments.as_deref(),
                        Some(calls[0].arguments.as_str())
                    );
                    match resolution {
                        "approve" => {
                            resolve_approval(&registry, &db, run_id, &call_id, true, None).unwrap();
                        }
                        "deny" => {
                            resolve_approval(&registry, &db, run_id, &call_id, false, None)
                                .unwrap();
                        }
                        "cancel" => assert!(registry.cancel(run_id)),
                        _ => unreachable!(),
                    }
                }
            }
            assert!(groups[0].is_some());
            assert_ne!(
                groups[0], groups[1],
                "distinct shell calls have distinct identities"
            );
            let frames = collect_frames(&rx);
            let RunFrame::Finished { event, .. } = frames.last().expect("terminal frame") else {
                panic!("terminal frame missing");
            };
            let cancelled = resolution == "cancel";
            let approved = resolution == "approve";
            assert_eq!(
                event.status,
                if cancelled { "cancelled" } else { "completed" }
            );
            assert!(!registry.is_active(run_id));
            assert!(
                !frames.iter().any(|frame| matches!(
                    frame,
                    RunFrame::Governance {
                        event: AgentRunEvent::ApprovalRequested { .. },
                        ..
                    }
                )),
                "no duplicate prompt"
            );
            let steps = list_steps_for_run(&db, run_id).unwrap();
            let approvals: Vec<_> = steps.iter().filter(|s| s.kind == "approval").collect();
            assert_eq!(approvals.len(), 2);
            assert_eq!(approvals[0].status.as_deref(), Some("succeeded"));
            assert_eq!(
                approvals[1].status.as_deref(),
                Some(if cancelled {
                    "cancelled"
                } else if approved {
                    "succeeded"
                } else {
                    "denied"
                })
            );
            assert_eq!(
                approvals[1].arguments.as_deref(),
                Some(calls[1].arguments.as_str())
            );
            if !cancelled {
                assert_eq!(approvals[1].decided_by.as_deref(), Some("user"));
                assert_eq!(approvals[1].group_key, groups[1]);
                assert!(
                    approvals[1].rule_id.is_some(),
                    "seeded shell Ask stays named"
                );
            }
            let executions: Vec<_> = steps.iter().filter(|s| s.kind == "tool_call").collect();
            assert_eq!(executions.len(), if approved { 2 } else { 1 });
            assert!(executions
                .iter()
                .all(|s| s.status.as_deref() == Some("succeeded")));
            if approved {
                assert_eq!(
                    std::fs::read_to_string(workspace.join("b.txt"))
                        .unwrap()
                        .lines()
                        .collect::<Vec<_>>(),
                    ["b"]
                );
                assert_eq!(executions[1].decided_by.as_deref(), Some("user"));
            } else {
                assert!(!workspace.join("b.txt").exists());
            }
        }
    }

    /// Cancel from an approval park aborts the run (`cancelled` terminal) and
    /// releases the registry entry (DP-3: cancel works from every state).
    #[test]
    fn cancel_from_approval_park_aborts_the_run() {
        let (db, workspace, rx, host) = setup("cancel");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("never reached")),
            ])),
            workspace,
            request(conversation_id, "cancel me"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::ApprovalRequested { .. },
                    ..
                }
            ) {
                break;
            }
        }

        assert!(registry.cancel(run_id), "cancel must reach the active run");
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "cancelled");
        assert_eq!(event.final_content, None);
        assert!(
            !registry.is_active(run_id),
            "registry entry released on the cancelled exit path"
        );
        // No assistant message on a failed/cancelled run (doctrine).
        assert!(host.persisted.lock().expect("lock").is_empty());
        let _ = std::fs::remove_dir_all(temp_workspace("cancel"));
    }

    /// NEX-AGENT-004: an approval park with no `respond` expires on the
    /// configured deadline — the run terminates `cancelled`, the registry
    /// entry is released, and the conversation is unclaimed so a new run
    /// can start. The 1s test deadline is a promptness bound, not a
    /// wall-clock margin: the test asserts termination + outcome + cleanup,
    /// never `elapsed`.
    #[test]
    fn unanswered_approval_park_expires_and_releases_the_conversation() {
        let (db, workspace, rx, host) = setup("park-timeout");
        crate::application::settings::SettingsService::new(&db)
            .write(APPROVAL_TIMEOUT_KEY, Some("1"))
            .expect("write timeout");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("never reached")),
            ])),
            workspace,
            request(conversation_id, "expire me"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        // Wait for the approval park; the event carries the deadline so the
        // UI can surface a countdown/notice.
        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { timeout_secs, .. },
                ..
            } = frame
            {
                assert_eq!(timeout_secs, 1, "event carries the configured deadline");
                break;
            }
        }

        // No `respond`, no `cancel`: the deadline expires on its own and the
        // run terminates `cancelled` with the claim released.
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "cancelled");
        assert_eq!(event.final_content, None);
        assert!(
            !registry.is_active(run_id),
            "registry entry released on the timeout exit path"
        );
        // No assistant message on a failed/cancelled run (doctrine).
        assert!(host.persisted.lock().expect("lock").is_empty());

        // The recorded approval step names the timeout honestly: status
        // `cancelled` (the schema terminal), observation naming the expiry —
        // never the user-cancel vocabulary.
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        let approval = steps
            .iter()
            .find(|step| step.kind == "approval")
            .expect("timeout records an approval step");
        assert_eq!(approval.status.as_deref(), Some("cancelled"));
        let observation = approval.observation.as_deref().unwrap_or_default();
        assert!(
            observation.contains("timed out"),
            "timeout step must name the timeout, got: {observation}"
        );
        assert!(
            !observation.contains("by the user"),
            "timeout step must not claim a user cancel, got: {observation}"
        );

        // The conversation is unclaimed: a new run can start immediately.
        let second = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("fresh"))])),
            temp_workspace("park-timeout-second"),
            request(conversation_id, "second"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        );
        assert!(
            second.is_ok(),
            "conversation must accept a new run after timeout, got: {second:?}"
        );
        let second_id = second.expect("start");
        // The second run is a scripted single-response chat with no park, so
        // it completes on its own and its exit path releases the registry
        // entry BEFORE the forwarder emits `Finished` (the terminal frame is
        // forwarded only after the run thread's senders drop, which happens
        // after `release`). Waiting for that frame synchronizes with the
        // run's exit; an unconditional `assert!(registry.cancel(..))` here
        // lost that race intermittently — `cancel` returned false on the
        // entry the run had already self-released.
        let second_frames = collect_frames(&rx);
        let RunFrame::Finished { run_id, event } = second_frames.last().expect("second run frames")
        else {
            panic!("second run must terminate with a Finished frame");
        };
        assert_eq!(
            *run_id, second_id,
            "Finished frame belongs to the second run"
        );
        assert_eq!(
            event.status, "completed",
            "second run completes on the released conversation"
        );
        assert!(
            !registry.is_active(second_id),
            "registry entry released by the second run's own exit path"
        );
        let _ = std::fs::remove_dir_all(temp_workspace("park-timeout"));
        let _ = std::fs::remove_dir_all(temp_workspace("park-timeout-second"));
    }

    /// `resolve_approval_timeout`: unset/unreadable degrades to the default;
    /// in-domain values resolve; out-of-domain values fall back.
    #[test]
    fn resolve_approval_timeout_defaults_and_clamps() {
        let db = crate::infrastructure::database::in_memory_database();
        let svc = crate::application::settings::SettingsService::new(&db);
        assert_eq!(
            resolve_approval_timeout(&db),
            Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS)
        );
        svc.write(APPROVAL_TIMEOUT_KEY, Some("30")).expect("write");
        assert_eq!(resolve_approval_timeout(&db), Duration::from_secs(30));
        for invalid in ["0", "-5", "abc", "", "  ", "1.5", "86401", "99999999999"] {
            svc.write(APPROVAL_TIMEOUT_KEY, Some(invalid))
                .expect("write");
            assert_eq!(
                resolve_approval_timeout(&db),
                Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
                "out-of-domain value must fall back: {invalid:?}"
            );
        }
        svc.delete(APPROVAL_TIMEOUT_KEY).expect("delete");
        assert_eq!(
            resolve_approval_timeout(&db),
            Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS)
        );
    }

    /// Budget park: exhaustion parks the run; `extend` continues it to
    /// completion (DP-5).
    #[test]
    fn budget_park_is_continued_via_extend() {
        let (db, workspace, rx, host) = setup("budget");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let mut req = request(conversation_id, "loop a bit");
        req.max_iterations = Some(1);
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("l", "list_directory")),
                Ok(text_response("continued")),
            ])),
            workspace,
            req,
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("budget frame");
            if matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::BudgetExhausted { .. },
                    ..
                }
            ) {
                break;
            }
        }

        assert!(registry.extend(run_id, 2), "extend must reach the run");
        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("continued"));
        assert!(!registry.is_active(run_id));
        let _ = std::fs::remove_dir_all(temp_workspace("budget"));
    }

    /// Provider failure: `Finished { status: "error" }`, no assistant message,
    /// registry released, and the classified error text carries no secret.
    #[test]
    fn provider_error_finishes_with_error_and_cleans_up() {
        let (db, workspace, rx, host) = setup("error");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Err(ExecutorError::Failure)])),
            workspace,
            request(conversation_id, "explode"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");

        let frames = collect_frames(&rx);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last frame must be Finished");
        };
        assert_eq!(event.status, "error");
        assert_eq!(event.final_content, None);
        assert!(event.error.is_some());
        let serialized = serde_json::to_string(&frames).expect("serialize");
        assert!(!serialized.contains("sk-secret-test-credential"));
        assert!(!registry.is_active(run_id));
        assert!(host.persisted.lock().expect("lock").is_empty());
        let _ = std::fs::remove_dir_all(temp_workspace("error"));
    }

    #[test]
    fn parse_autonomy_mode_defaults_to_semi() {
        use crate::application::agent::approval::AutonomyMode;
        assert_eq!(
            parse_autonomy_mode(Some("supervised")),
            AutonomyMode::Supervised
        );
        assert_eq!(
            parse_autonomy_mode(Some("semi_autonomous")),
            AutonomyMode::SemiAutonomous
        );
        assert_eq!(
            parse_autonomy_mode(Some("full_autonomous")),
            AutonomyMode::FullAutonomous
        );
        // Invalid, None, empty all default to semi
        assert_eq!(parse_autonomy_mode(None), AutonomyMode::SemiAutonomous);
        assert_eq!(parse_autonomy_mode(Some("")), AutonomyMode::SemiAutonomous);
        assert_eq!(
            parse_autonomy_mode(Some("garbage")),
            AutonomyMode::SemiAutonomous
        );
        assert_eq!(
            parse_autonomy_mode(Some("SemiAutonomous")),
            AutonomyMode::SemiAutonomous
        );
    }

    #[test]
    fn resolve_autonomy_mode_reads_setting_and_defaults() {
        let db = crate::infrastructure::database::in_memory_database();
        // Default when unset
        assert_eq!(
            resolve_autonomy_mode(&db),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous
        );
        // Supervised
        crate::application::settings::SettingsService::new(&db)
            .write("agent.autonomy", Some("supervised"))
            .expect("write");
        assert_eq!(
            resolve_autonomy_mode(&db),
            crate::application::agent::approval::AutonomyMode::Supervised
        );
        // Full
        crate::application::settings::SettingsService::new(&db)
            .write("agent.autonomy", Some("full_autonomous"))
            .expect("write");
        assert_eq!(
            resolve_autonomy_mode(&db),
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        );
        // Invalid legacy defaults to semi
        crate::application::settings::SettingsService::new(&db)
            .write("agent.autonomy", Some("legacy"))
            .expect("write");
        assert_eq!(
            resolve_autonomy_mode(&db),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous
        );
        // Clearing restores default
        crate::application::settings::SettingsService::new(&db)
            .delete("agent.autonomy")
            .expect("delete");
        assert_eq!(
            resolve_autonomy_mode(&db),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous
        );
    }

    #[test]
    fn parse_preset_defaults_to_coding() {
        assert_eq!(parse_preset(Some("coding")), RunPreset::Coding);
        assert_eq!(parse_preset(Some("document")), RunPreset::Document);
        // Invalid, None, empty all default to coding
        assert_eq!(parse_preset(None), RunPreset::Coding);
        assert_eq!(parse_preset(Some("")), RunPreset::Coding);
        assert_eq!(parse_preset(Some("garbage")), RunPreset::Coding);
        assert_eq!(parse_preset(Some("Document")), RunPreset::Coding);
    }

    #[test]
    fn resolve_preset_reads_setting_and_defaults() {
        let db = crate::infrastructure::database::in_memory_database();
        // Default when unset
        assert_eq!(resolve_preset(&db), RunPreset::Coding);
        // Document
        crate::application::settings::SettingsService::new(&db)
            .write(PRESET_KEY, Some("document"))
            .expect("write");
        assert_eq!(resolve_preset(&db), RunPreset::Document);
        // Coding
        crate::application::settings::SettingsService::new(&db)
            .write(PRESET_KEY, Some("coding"))
            .expect("write");
        assert_eq!(resolve_preset(&db), RunPreset::Coding);
        // Invalid legacy defaults to coding
        crate::application::settings::SettingsService::new(&db)
            .write(PRESET_KEY, Some("legacy"))
            .expect("write");
        assert_eq!(resolve_preset(&db), RunPreset::Coding);
        // Clearing restores default
        crate::application::settings::SettingsService::new(&db)
            .delete(PRESET_KEY)
            .expect("delete");
        assert_eq!(resolve_preset(&db), RunPreset::Coding);
    }

    #[test]
    fn resolve_spend_limit_unset_is_none() {
        let db = crate::infrastructure::database::in_memory_database();
        assert_eq!(resolve_spend_limit(&db), None);
    }

    #[test]
    fn resolve_spend_limit_parses_positive_and_rejects_other_values() {
        let db = crate::infrastructure::database::in_memory_database();
        let svc = crate::application::settings::SettingsService::new(&db);
        svc.write(SPEND_LIMIT_KEY, Some("250000")).expect("write");
        assert_eq!(resolve_spend_limit(&db), Some(250_000));
        for invalid in ["0", "abc", "", "-5", "  "] {
            svc.write(SPEND_LIMIT_KEY, Some(invalid)).expect("write");
            assert_eq!(
                resolve_spend_limit(&db),
                None,
                "value {invalid:?} must resolve to no limit"
            );
        }
        svc.delete(SPEND_LIMIT_KEY).expect("delete");
        assert_eq!(resolve_spend_limit(&db), None);
    }

    #[test]
    fn resolve_spend_limit_round_trip_through_settings_service() {
        let db = crate::infrastructure::database::in_memory_database();
        crate::application::settings::SettingsService::new(&db)
            .write(SPEND_LIMIT_KEY, Some("1000000"))
            .expect("write");
        assert_eq!(resolve_spend_limit(&db), Some(1_000_000));
        crate::application::settings::SettingsService::new(&db)
            .delete(SPEND_LIMIT_KEY)
            .expect("delete");
        assert_eq!(resolve_spend_limit(&db), None);
    }

    #[test]
    fn start_run_persists_resolved_mode() {
        let (db, workspace, _rx, host) = setup("mode-persist");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db).create("conv").expect("conv");
        // Write setting to supervised
        crate::application::settings::SettingsService::new(&db)
            .write("agent.autonomy", Some("supervised"))
            .expect("write");
        let mode = resolve_autonomy_mode(&db);
        assert_eq!(
            mode,
            crate::application::agent::approval::AutonomyMode::Supervised
        );
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("done"))])),
            workspace.clone(),
            request(conversation_id, "hello"),
            mode,
            RunPreset::Coding,
        )
        .expect("start");
        // Verify persisted row has mode column = supervised
        let runs = list_runs_for_conversation(&db, conversation_id).expect("list");
        let run = runs.iter().find(|r| r.id == run_id).expect("run");
        assert_eq!(run.mode, "supervised");
        let _ = std::fs::remove_dir_all(temp_workspace("mode-persist"));
    }

    /// Task T4 wiring: `spawn_run` resolves the model window via
    /// [`context_limit_for`] (the same call the run thread makes), so the
    /// proactive trigger has a usable window; a `0` limit (unwired) or
    /// unknown usage keeps it dormant. Reactive overflow recovery is
    /// untouched (covered by the runner/compaction suites).
    #[test]
    fn wired_context_limit_activates_proactive_compaction_and_zero_stays_dormant() {
        use crate::application::agent::compaction::{
            usable_context_tokens, ContextAction, ContextGovernor,
        };
        use crate::application::execution::{AiMessage, AiRole, TokenUsage};

        fn user(content: &str) -> AiMessage {
            AiMessage {
                role: AiRole::User,
                content: content.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }
        }

        // Foldable history (> 2 messages) so `decide` reaches the trigger.
        let history = vec![user("one"), user("two"), user("three")];

        // Wired: resolve exactly as `spawn_run` does, for one model per
        // provider. Every wired limit yields a usable window and an
        // over-threshold observation compacts on `Threshold`.
        for (provider, model) in [
            ("openai", "gpt-5.6-terra"),
            ("anthropic", "claude-sonnet-5"),
            ("gemini", "gemini-3.6-flash"),
        ] {
            let wired = context_limit_for(Some(provider), Some(model));
            let usable = usable_context_tokens(wired);
            assert!(
                usable > 0,
                "wired limit for {provider}/{model} must leave a usable window"
            );
            let mut governor = ContextGovernor::new();
            governor.observe(Some(TokenUsage {
                input_tokens: usable.saturating_add(1),
                output_tokens: 0,
            }));
            match governor.decide(&history, wired) {
                ContextAction::Compact(reason) => assert_eq!(
                    reason.as_str(),
                    "threshold",
                    "wired {provider}/{model} must compact on Threshold"
                ),
                other => panic!("wired {provider}/{model} must Compact, got {other:?}"),
            }
        }

        // Dormant: limit `0` (no wired window) always proceeds, even at
        // maximum reported usage.
        let wired_openai = context_limit_for(Some("openai"), Some("gpt-5.6-terra"));
        assert!(usable_context_tokens(wired_openai) > 0);
        let mut dormant = ContextGovernor::new();
        dormant.observe(Some(TokenUsage {
            input_tokens: u64::MAX,
            output_tokens: 0,
        }));
        assert_eq!(
            dormant.decide(&history, 0),
            ContextAction::Proceed,
            "limit 0 must keep the proactive trigger dormant"
        );

        // Dormant: unknown usage (`None`) never triggers, even when wired.
        let mut unknown = ContextGovernor::new();
        unknown.observe(None);
        assert_eq!(
            unknown.decide(&history, wired_openai),
            ContextAction::Proceed,
            "unknown usage must never trigger"
        );
    }

    /// Local per-turn delay wrapper: sleeps before each provider turn so the
    /// run thread stays alive long enough for the pause handshake below.
    struct DelayedExecutor {
        inner: ScriptedExecutor,
        delay: Duration,
    }

    impl DelayedExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>, delay: Duration) -> Self {
            Self {
                inner: ScriptedExecutor::new(steps),
                delay,
            }
        }
    }

    impl ProviderExecutor for DelayedExecutor {
        fn execute(
            &self,
            request: &crate::application::execution::AiRequest,
            credential: &str,
            token: &crate::application::agent::control::CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            std::thread::sleep(self.delay);
            self.inner.execute(request, credential, token)
        }
    }

    #[test]
    fn pause_resume_round_trip_allows_run_to_continue_to_completion() {
        let (db, workspace, rx, host) = setup("pause-resume");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db).create("conv").expect("conv");
        // Deterministic pause handshake: per-turn delay keeps the run thread
        // alive so `pause` can land before the run finishes on fast runners.
        let mut req = request(conversation_id, "pause me");
        req.max_iterations = Some(10);
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(DelayedExecutor::new(
                vec![
                    Ok(tool_response("a", "list_directory")),
                    Ok(tool_response("b", "list_directory")),
                    Ok(text_response("done after pause")),
                ],
                Duration::from_millis(200),
            )),
            workspace,
            req,
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        // Retry pause while the run is still active; fail fast if it finished first.
        let mut paused = false;
        for _ in 0..50 {
            if registry.pause(run_id) {
                paused = true;
                break;
            }
            assert!(
                registry.is_active(run_id),
                "run finished before pause could land"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(paused, "pause never landed while run was active");
        // Wait for the Paused governance frame, keeping drained frames.
        let mut frames = Vec::new();
        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("paused frame");
            let is_paused = matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::Paused,
                    ..
                }
            );
            frames.push(frame);
            if is_paused {
                break;
            }
        }
        // Now resume: run should continue
        assert!(registry.resume(run_id));
        let rest = collect_frames(&rx);
        frames.extend(rest);
        let RunFrame::Finished { event, .. } = frames.last().expect("frames") else {
            panic!("last must be finished");
        };
        assert_eq!(event.status, "completed");
        assert_eq!(event.final_content.as_deref(), Some("done after pause"));
        // After completion, pause/resume on inactive should be false
        assert!(!registry.pause(run_id));
        assert!(!registry.resume(run_id));
        assert!(!registry.set_mode(
            run_id,
            crate::application::agent::approval::AutonomyMode::Supervised
        ));
        let _ = std::fs::remove_dir_all(temp_workspace("pause-resume"));
    }

    #[test]
    fn inactive_run_after_completion_returns_not_found_for_all_controls() {
        let (db, workspace, rx, host) = setup("inactive-controls");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db).create("conv").expect("conv");
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("quick"))])),
            workspace,
            request(conversation_id, "hi"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last().unwrap(), RunFrame::Finished { .. }));
        // Now all controls should report inactive
        assert!(!registry.cancel(run_id));
        assert!(!registry.pause(run_id));
        assert!(!registry.resume(run_id));
        assert!(!registry.extend(run_id, 1));
        assert_eq!(
            registry.resolve(run_id, "any", true),
            ResolveOutcome::RunNotActive
        );
        assert!(!registry.set_mode(
            run_id,
            crate::application::agent::approval::AutonomyMode::FullAutonomous
        ));
        let _ = std::fs::remove_dir_all(temp_workspace("inactive-controls"));
    }

    /// Entry-point proof (M1-core reachability gate, T1 pattern): a deny rule
    /// persisted before `start_run` denies through the production bridge —
    /// `list_agent_steps` shows the denied approval with its `rule_id`.
    #[test]
    fn entry_point_deny_rule_run_shows_denied_with_rule_id() {
        let (db, workspace, rx, host) = setup("entry-deny");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("entry-deny")
            .expect("conversation");
        crate::application::agent::permissions::insert_rule(
            &db,
            "coding",
            "write_file",
            None,
            crate::application::agent::permissions::RuleEffect::Deny,
            10,
        )
        .expect("insert deny rule");
        let inserted_id: i64 = db
            .lock()
            .expect("lock")
            .query_row(
                "SELECT id FROM permission_rules WHERE tool_pattern = 'write_file'",
                [],
                |row| row.get(0),
            )
            .expect("read rule id");
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response("w", "write_file")),
                Ok(text_response("recovered")),
            ])),
            workspace,
            request(conversation_id, "deny me"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last().unwrap(), RunFrame::Finished { .. }));
        // Reachability: the service bridge consulted the store (no park).
        assert!(
            !frames.iter().any(|frame| matches!(
                frame,
                RunFrame::Governance {
                    event: AgentRunEvent::ApprovalRequested { .. },
                    ..
                }
            )),
            "deny-rule runs never park"
        );
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        let approval = steps
            .iter()
            .find(|step| step.kind == "approval")
            .expect("approval step");
        assert_eq!(approval.status.as_deref(), Some("denied"));
        assert_eq!(approval.rule_id, Some(inserted_id));
        assert_eq!(approval.decided_by.as_deref(), Some("rule"));
        assert!(approval
            .observation
            .as_deref()
            .unwrap_or("")
            .starts_with("denied by rule:"));
        assert!(!steps.iter().any(|step| step.kind == "tool_call"));
        let _ = std::fs::remove_dir_all(temp_workspace("entry-deny"));
    }

    /// Entry-point proof (grouping): a group-scope resolve through the
    /// registry makes the second same-group park auto-resolve with a shared
    /// `group_key`.
    #[test]
    fn entry_point_group_scope_second_call_shares_group_key() {
        let (db, workspace, rx, host) = setup("entry-group");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("entry-group")
            .expect("conversation");
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(AiResponse {
                    content: String::new(),
                    model: "test-model".to_string(),
                    tool_calls: vec![
                        ToolCall {
                            id: "g1".to_string(),
                            name: "write_file".to_string(),
                            arguments: serde_json::json!({"path": "grp/a.txt", "content": "1"})
                                .to_string(),
                            thought_signature: None,
                        },
                        ToolCall {
                            id: "g2".to_string(),
                            name: "write_file".to_string(),
                            arguments: serde_json::json!({"path": "grp/a.txt", "content": "1"})
                                .to_string(),
                            thought_signature: None,
                        },
                    ],
                    usage: None,
                }),
                Ok(text_response("done")),
            ])),
            workspace,
            request(conversation_id, "group me"),
            crate::application::agent::approval::AutonomyMode::SemiAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        // First park resolves with group scope through the registry (the IPC
        // command's path); the second same-group call auto-resolves.
        let call_id = loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("approval frame");
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { call_id, .. },
                ..
            } = frame
            {
                break call_id;
            }
        };
        let (outcome, _) = registry.resolve_with_scope(run_id, &call_id, true, Some("group"));
        assert_eq!(outcome, ResolveOutcome::Resolved);
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last().unwrap(), RunFrame::Finished { .. }));
        // The first park was consumed by the wait loop above; the collected
        // tail must contain no further park (the second group call
        // auto-resolves without parking).
        let tail_parks = frames
            .iter()
            .filter(|frame| {
                matches!(
                    frame,
                    RunFrame::Governance {
                        event: AgentRunEvent::ApprovalRequested { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(tail_parks, 0, "second group call must not park");
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        let approvals: Vec<_> = steps
            .iter()
            .filter(|step| step.kind == "approval")
            .collect();
        assert_eq!(approvals.len(), 2);
        assert_eq!(approvals[0].group_key, approvals[1].group_key);
        assert!(approvals[0].group_key.is_some());
        let _ = std::fs::remove_dir_all(temp_workspace("entry-group"));
    }

    /// Entry-point proof for the new tools: a run whose scripted provider
    /// returns `edit_file` then `search_files` calls completes, the edit
    /// lands on disk, the spilled search observation lists as a bounded
    /// preview, and its full text is reachable via the on-demand fetch.
    #[test]
    #[allow(clippy::too_many_lines)] // Keep consent, file mutation and spilled-observation assertions in one integration fixture.
    fn new_tools_run_end_to_end_with_spilled_observation() {
        use std::fmt::Write as _;
        let (db, workspace, rx, host) = setup("newtools");
        std::fs::write(workspace.join("target.txt"), "alpha beta gamma").expect("seed target");
        // 200 long matching lines push the search observation past the 20KB
        // budget so the spill notice path is exercised end to end.
        let mut corpus = String::new();
        for i in 0..200 {
            let _ = writeln!(corpus, "haystack marker line {i:03} {}", "z".repeat(140));
        }
        std::fs::write(workspace.join("corpus.txt"), &corpus).expect("seed corpus");

        let edit_call = tool_call_with(
            "edit-1",
            "edit_file",
            &serde_json::json!({"path": "target.txt", "old_text": "beta", "new_text": "BETA"}),
        );
        let search_call = tool_call_with(
            "search-1",
            "search_files",
            &serde_json::json!({"pattern": "haystack marker", "max_matches": 200}),
        );
        let scripted = ScriptedCalls::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![edit_call],
                usage: None,
            }),
            Ok(AiResponse {
                content: String::new(),
                model: "test-model".to_string(),
                tool_calls: vec![search_call],
                usage: None,
            }),
            Ok(text_response("done")),
        ]);

        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(scripted),
            workspace.clone(),
            request(conversation_id, "edit then search"),
            crate::application::agent::approval::AutonomyMode::FullAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        let mut frames = Vec::new();
        loop {
            let frame = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("bounded completion");
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { ref call_id, .. },
                ..
            } = frame
            {
                assert_eq!(
                    registry.resolve(run_id, call_id, true),
                    ResolveOutcome::Resolved
                );
            }
            let finished = matches!(frame, RunFrame::Finished { .. });
            frames.push(frame);
            if finished {
                break;
            }
        }
        assert!(
            matches!(frames.last().unwrap(), RunFrame::Finished { .. }),
            "run must complete"
        );

        // The edit landed on disk through the service/registry path.
        // Both tool observations are persisted and visible via list steps.
        let steps = list_steps_for_run(&db, run_id).expect("steps");
        let tool_steps: Vec<_> = steps.iter().filter(|s| s.kind == "tool_call").collect();
        let step_debug: Vec<_> = tool_steps
            .iter()
            .map(|s| {
                let obs = s.observation.clone().unwrap_or_default();
                // First line only: full observations (e.g. spilled search
                // output) would flood the failure log.
                let short: String = obs.lines().next().unwrap_or("").chars().take(200).collect();
                (s.tool_name.clone(), s.status.clone(), short)
            })
            .collect();
        assert_eq!(
            std::fs::read_to_string(workspace.join("target.txt")).unwrap(),
            "alpha BETA gamma",
            "edit must land on disk; tool steps: {step_debug:?}"
        );

        assert_eq!(tool_steps.len(), 2);
        assert_eq!(tool_steps[0].tool_name.as_deref(), Some("edit_file"));
        assert_eq!(tool_steps[0].status.as_deref(), Some("succeeded"));
        assert_eq!(tool_steps[1].tool_name.as_deref(), Some("search_files"));
        assert_eq!(tool_steps[1].status.as_deref(), Some("succeeded"));
        // NEX-PERF-003: the list item carries a bounded preview, never the
        // full output; the spill notice lives in the full text behind the
        // on-demand fetch (`get_step_observation_for`).
        let search_item = tool_steps[1];
        assert!(
            search_item.observation_truncated,
            "large search observation must list as a truncated preview"
        );
        assert!(
            search_item.observation_len.unwrap_or(0) > 20 * 1024,
            "total names the full shaped bytes"
        );
        let search_observation = get_step_observation_for(&db, search_item.id)
            .expect("full-text fetch succeeds")
            .expect("step exists");
        assert!(
            search_observation.contains("[truncated: "),
            "large search observation must carry the spill notice"
        );
        assert!(search_observation.contains("full output spilled to "));
        // The spilled file exists and holds the full pre-truncation bytes.
        let start = search_observation.find("spilled to ").expect("notice") + "spilled to ".len();
        let end = search_observation.rfind(']').expect("notice end");
        let spill_path = std::path::PathBuf::from(search_observation[start..end].trim());
        let spilled = std::fs::read(&spill_path).expect("spill file exists");
        assert!(spilled.len() > 20 * 1024, "spill holds the full output");
        assert!(String::from_utf8_lossy(&spilled).contains("haystack marker line 199"));
        let _ = std::fs::remove_dir_all(temp_workspace("newtools"));
    }

    /// Scripted executor honouring per-call tool arguments (unlike
    /// [`ScriptedExecutor`], whose helper fixes arguments to `{}`).
    struct ScriptedCalls {
        steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
    }

    impl ScriptedCalls {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
            }
        }
    }

    impl ProviderExecutor for ScriptedCalls {
        fn execute(
            &self,
            _request: &crate::application::execution::AiRequest,
            _credential: &str,
            _token: &crate::application::agent::control::CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    fn tool_call_with(id: &str, name: &str, arguments: &serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            thought_signature: None,
        }
    }

    // -----------------------------------------------------------------------
    // Canonical routing-profile load (#81): the run path resolves through
    // `RoutingService::resolve` and surfaces the invalid-workspace notice on
    // the run's audit trail.
    // -----------------------------------------------------------------------

    /// Seed an invalid workspace agent profile (present file, bad content).
    fn seed_invalid_workspace_profile(ws: &std::path::Path) {
        crate::application::project_dir::init_nexora_dir(ws).expect("init succeeds");
        std::fs::write(
            ws.join(".nexora").join("profiles").join("agent.json"),
            "not json at all sk-test-sentinel-77ae",
        )
        .expect("seed invalid profile");
    }

    #[test]
    fn routing_fallback_notice_is_surfaced_on_the_run_audit_trail() {
        let db = crate::infrastructure::database::in_memory_database();
        let ws = temp_workspace("routing-notice");
        seed_invalid_workspace_profile(&ws);
        let log = AuditLog::new();
        note_routing_fallback(&db, &ws, &log);
        let entries = log.entries();
        assert_eq!(entries.len(), 1, "one fallback entry, got {entries:?}");
        let entry = &entries[0];
        assert_eq!((entry.from, entry.to), ("queued", "running"));
        assert_eq!(entry.event, "routing_fallback");
        // Payload-free: the hostile file content never enters the trail.
        let dump = format!("{entries:?}");
        assert!(!dump.contains("sk-test-sentinel-77ae"));
        assert!(!dump.contains("agent.json"));
        let _ = std::fs::remove_dir_all(temp_workspace("routing-notice"));
    }

    #[test]
    fn routing_without_workspace_file_appends_nothing() {
        let db = crate::infrastructure::database::in_memory_database();
        let ws = temp_workspace("routing-clean");
        crate::application::project_dir::init_nexora_dir(&ws).expect("init succeeds");
        let log = AuditLog::new();
        note_routing_fallback(&db, &ws, &log);
        assert!(log.is_empty(), "absent file resolves silently");
        let _ = std::fs::remove_dir_all(temp_workspace("routing-clean"));
    }

    #[test]
    fn routing_with_corrupt_global_never_fails_the_run() {
        let db = crate::infrastructure::database::in_memory_database();
        crate::application::settings::SettingsService::new(&db)
            .write(crate::application::routing::AGENT_PROFILE_KEY, Some("[]"))
            .expect("seed corrupt global");
        let ws = temp_workspace("routing-corrupt");
        let log = AuditLog::new();
        // A corrupt global value is reported, never silently replaced — and
        // the run setup never fails for routing.
        note_routing_fallback(&db, &ws, &log);
        assert!(log.is_empty(), "corrupt global appends no fallback entry");
        let _ = std::fs::remove_dir_all(temp_workspace("routing-corrupt"));
    }

    /// Static wiring check: the production run path must resolve routing
    /// through `RoutingService::resolve` (the canonical load — never a direct
    /// `load`), surface the notice via `note_routing_fallback`, and attach
    /// the run-scoped audit trail to the runner. The spawn closure needs
    /// threads and managed state, so this source check pins the delegation
    /// instead (same pattern as the `start_agent_run` bridge check).
    #[test]
    fn run_path_resolves_routing_through_resolve_not_load() {
        const SOURCE: &str = include_str!("service.rs");
        assert!(
            SOURCE.contains("RoutingService::new(db).resolve("),
            "spawn_run must resolve routing through the canonical resolve()"
        );
        assert!(
            SOURCE.contains("note_routing_fallback(&db, &workspace_root, &audit_log)"),
            "spawn_run must surface the routing notice on the run audit trail"
        );
        assert!(
            SOURCE.contains(".with_audit_log("),
            "spawn_run must attach the run-scoped audit trail to the runner"
        );
        assert!(
            SOURCE.contains(".with_run_flags("),
            "spawn_run must apply the resolved feature flags to the runner"
        );
    }

    /// NEX-PERF-003: rehydration lists bounded previews (never full outputs)
    /// with byte-exact totals, and the full sanitized text of any listed
    /// step is reachable through the on-demand fetch by the listed id.
    #[test]
    fn list_steps_preview_bounded_and_full_text_fetchable_by_id() {
        let (db, workspace, rx, host) = setup("preview-list");
        let registry = Arc::new(AgentRunRegistry::default());
        let conversation_id = ConversationService::new(&db)
            .create("conv")
            .expect("conversation");

        // A 10KB file: the real `read_file` tool returns it whole, so the
        // stored observation exceeds the preview bound end to end.
        std::fs::write(workspace.join("big.txt"), "v".repeat(10_000)).expect("seed big");
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            Arc::clone(&host) as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(AiResponse {
                    content: String::new(),
                    model: "test-model".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "big-1".to_string(),
                        name: "read_file".to_string(),
                        arguments: r#"{"path": "big.txt"}"#.to_string(),
                        thought_signature: None,
                    }],
                    usage: None,
                }),
                Ok(text_response("done")),
            ])),
            workspace,
            request(conversation_id, "read big"),
            crate::application::agent::approval::AutonomyMode::FullAutonomous,
            RunPreset::Coding,
        )
        .expect("start");
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last().unwrap(), RunFrame::Finished { .. }));

        let items = list_steps_for_run(&db, run_id).expect("list");
        assert!(!items.is_empty(), "steps must list");
        for item in &items {
            if let Some(preview) = item.observation.as_deref() {
                assert!(
                    preview.chars().count()
                        <= super::super::persistence::MAX_STEP_PREVIEW_CHARS + 64,
                    "list preview must stay bounded (seq {})",
                    item.seq
                );
            }
        }
        // The tool step lists truncated with a byte-exact total, and the
        // fetch behind its id recovers the full stored bytes.
        let tool_item = items
            .iter()
            .find(|item| item.kind == "tool_call")
            .expect("tool step lists");
        assert!(
            tool_item.observation_truncated,
            "10KB observation must list truncated"
        );
        assert_eq!(tool_item.observation_len, Some(10_000));
        assert_eq!(
            get_step_observation_for(&db, tool_item.id)
                .expect("fetch succeeds")
                .as_deref(),
            Some("v".repeat(10_000).as_str()),
            "fetch recovers the exact stored bytes"
        );
        // The step frame for the same run carries the same bounded shape:
        // preview + byte-exact total + fetch key.
        let step_frames: Vec<_> = frames
            .iter()
            .filter_map(|frame| match frame {
                RunFrame::Step { event, .. } => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(step_frames.len(), items.len(), "one frame per listed step");
        for (frame, item) in step_frames.iter().zip(items.iter()) {
            assert_eq!(frame.step_id, item.id);
            assert_eq!(frame.observation, item.observation);
            assert_eq!(frame.observation_len, item.observation_len);
            assert_eq!(frame.observation_truncated, item.observation_truncated);
        }
        let _ = std::fs::remove_dir_all(temp_workspace("preview-list"));
    }

    /// NEX-PERF-003: the on-demand fetch returns the exact stored text for a
    /// known step id and an honest `None` for an unknown one (the command
    /// maps `None` to a secret-free not-found error carrying only the id).
    #[test]
    fn step_observation_fetch_round_trips_and_unknown_id_is_none() {
        use crate::infrastructure::repository::agent_runs::AgentRunRepository;
        let db = crate::infrastructure::database::in_memory_database();
        let repo = AgentRunRepository::new(&db);
        let run_id = repo.create_run(None, "m", "supervised").expect("run");
        let step_id = repo
            .append_step(
                run_id,
                1,
                "tool_call",
                Some("read_file"),
                Some("{}"),
                Some("full stored observation"),
                Some("succeeded"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("step");
        assert_eq!(
            get_step_observation_for(&db, step_id)
                .expect("fetch succeeds")
                .as_deref(),
            Some("full stored observation")
        );
        assert_eq!(
            get_step_observation_for(&db, step_id + 10_000).expect("fetch succeeds"),
            None,
            "unknown step id must read as None, never an error or a guess"
        );
    }
    #[test]
    fn access_profiles_production_start_enforces_ceilings_above_legacy_allow_rules() {
        for profile in ["read_only", "standard", "supervised"] {
            let (db, workspace, rx, host) = setup(&format!("access-profile-{profile}"));
            SettingsService::new(&db)
                .write(super::super::access::PROFILE_KEY, Some(profile))
                .unwrap();
            super::super::permissions::insert_rule(
                &db,
                "coding",
                "*",
                None,
                super::super::permissions::RuleEffect::Allow,
                0,
            )
            .unwrap();
            let registry = Arc::new(AgentRunRegistry::default());
            let conversation_id = ConversationService::new(&db)
                .create("synthetic profile run")
                .unwrap();
            let mut response = tool_response("file", "write_file");
            response.tool_calls[0].arguments =
                r#"{"path":"approved.txt","content":"synthetic"}"#.into();
            let mut shell = tool_response("shell", "execute_command");
            shell.tool_calls[0].arguments =
                r#"{"command":"echo synthetic>>shell-marker.txt","cwd":"."}"#.into();
            let run_id = start_run(
                &db,
                registry.clone(),
                host as Arc<dyn AgentRunHost>,
                Arc::new(ScriptedExecutor::new(vec![
                    Ok(response),
                    Ok(shell),
                    Ok(text_response("done")),
                ])),
                workspace.clone(),
                request(conversation_id, "q"),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
            )
            .unwrap();
            let mut approvals = 0;
            loop {
                let frame = rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("bounded run completion");
                match frame {
                    RunFrame::Governance {
                        event:
                            AgentRunEvent::ApprovalRequested {
                                call_id,
                                name,
                                metadata,
                                ..
                            },
                        ..
                    } => {
                        approvals += 1;
                        assert_ne!(profile, "read_only");
                        assert!(!metadata.checkpoints_available);
                        assert_eq!(metadata.sandbox_posture, "PerCommandConsent");
                        if name == "execute_command" {
                            assert!(!metadata.allow_for_run);
                            assert_eq!(
                                registry
                                    .resolve_with_scope(run_id, &call_id, true, Some("group"))
                                    .0,
                                ResolveOutcome::NoPendingApproval
                            );
                        }
                        assert_eq!(
                            registry.resolve(run_id, &call_id, name != "execute_command"),
                            ResolveOutcome::Resolved
                        );
                    }
                    RunFrame::Finished { event, .. } => {
                        assert_eq!(event.status, "completed");
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(approvals, if profile == "read_only" { 0 } else { 2 });
            assert_eq!(
                workspace.join("approved.txt").exists(),
                profile != "read_only"
            );
            assert!(!workspace.join("shell-marker.txt").exists());
            std::fs::remove_dir_all(workspace).unwrap();
        }
    }
    #[test]
    fn server_parent_plan_ceiling_reaches_production_child_start() {
        let (db, workspace, rx, host) = setup("access-parent-plan");
        let parent = super::super::access::AccessContext::root(&db);
        parent.set_plan(true);
        let conversation_id = ConversationService::new(&db).create("child").unwrap();
        let mut response = tool_response("file", "write_file");
        response.tool_calls[0].arguments = r#"{"path":"blocked.txt","content":"x"}"#.into();
        start_run_with_parent(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host as Arc<dyn AgentRunHost>,
            Arc::new(ScriptedExecutor::new(vec![
                Ok(response),
                Ok(text_response("done")),
            ])),
            workspace.clone(),
            request(conversation_id, "q"),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
            &parent,
        )
        .unwrap();
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last(), Some(RunFrame::Finished { .. })));
        assert!(!frames.iter().any(|frame| matches!(
            frame,
            RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { .. },
                ..
            }
        )));
        assert!(!workspace.join("blocked.txt").exists());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn private_workspace_is_rejected_before_provider_or_context_work() {
        let (db, workspace, _rx, host) = setup("protected-root");
        let private = workspace.join(".ssh");
        std::fs::create_dir(&private).unwrap();
        let conversation_id = ConversationService::new(&db)
            .create("private root")
            .unwrap();
        let executor = Arc::new(ScriptedExecutor::new(vec![Ok(text_response(
            "must not execute",
        ))]));
        let result = start_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host as Arc<dyn AgentRunHost>,
            executor.clone(),
            private,
            request(conversation_id, "q"),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
        );
        assert!(matches!(result, Err(AgentRunError::WorkspaceProtected)));
        assert_eq!(executor.steps.lock().unwrap().len(), 1);
        assert!(list_runs_for_conversation(&db, conversation_id)
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(workspace).unwrap();
    }

    struct CapturingScriptedExecutor {
        responses: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
        captured: Mutex<Vec<crate::application::execution::AiRequest>>,
    }

    impl CapturingScriptedExecutor {
        fn new(responses: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                captured: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProviderExecutor for CapturingScriptedExecutor {
        fn execute(
            &self,
            request: &crate::application::execution::AiRequest,
            _credential: &str,
            _token: &crate::application::agent::control::CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.captured.lock().unwrap().push(request.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected model turn")
        }
    }

    #[test]
    fn prompt_context_is_current_turn_data_and_sets_the_document_ceiling() {
        let (db, workspace, rx, host) = setup("prompt-context-doc");
        let conversations = ConversationService::new(&db);
        let conversation_id = conversations.create("prompt context").unwrap();
        let registry = Arc::new(AgentRunRegistry::default());
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
            "Done",
        ))]));
        let run_id = start_run_with_options(
            &db,
            Arc::clone(&registry),
            host.clone(),
            executor.clone(),
            workspace.clone(),
            request(conversation_id, "Take a look"),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
            RunLaunchOptions {
                prompt_context: Some("[{\"kind\":\"skill\",\"source\":\"fixture\",\"text\":\"private context marker\"}]".into()),
                ..RunLaunchOptions::default()
            },
        ).unwrap();
        collect_frames(&rx);
        {
            let requests = executor.captured.lock().unwrap();
            let first = &requests[0];
            assert!(first
                .messages
                .iter()
                .any(|message| message.content.contains("private context marker")));
            assert!(!first
                .tools
                .iter()
                .any(|tool| tool.name == "execute_command"));
        }
        assert!(super::super::super::prompt_context::run_no_shell(&db, run_id).unwrap());
        assert!(conversations
            .agent_history(conversation_id, "openai")
            .unwrap()
            .iter()
            .all(|message| !message.content.contains("private context marker")));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_context_document_ceiling_is_enforced_after_restart_and_continue() {
        use super::super::super::prompt_context::{mark_run_no_shell, run_no_shell};
        use crate::infrastructure::database::open;
        let workspace = temp_workspace("prompt-context-restart");
        let path = workspace.join("run-test.db");
        let interrupted = {
            let db = Database::new(open(&path).unwrap());
            let conversation = ConversationService::new(&db).create("restart").unwrap();
            AgentRunRepository::new(&db)
                .create_run_with(
                    Some(conversation),
                    "test-model",
                    "full_autonomous",
                    mark_run_no_shell,
                )
                .unwrap()
        };
        let db = Database::new(open(&path).unwrap());
        let runs = AgentRunRepository::new(&db);
        assert!(run_no_shell(&db, interrupted).unwrap());
        assert_eq!(
            runs.fail_orphaned_running_runs("interrupted test").unwrap(),
            1
        );
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        let continuation = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(tool_response("unexpected", "execute_command")),
            Ok(text_response("Continued")),
        ]));
        let next = continue_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host,
            continuation.clone(),
            workspace.clone(),
            interrupted,
            "openai".to_owned(),
            "test-model".to_owned(),
            "synthetic".to_owned(),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
        )
        .unwrap();
        let frames = collect_frames(&rx);
        assert!(matches!(frames.last(), Some(RunFrame::Finished { .. })));
        assert!(run_no_shell(&db, next).unwrap());
        let captured = continuation.captured.lock().unwrap();
        assert!(!captured[0]
            .tools
            .iter()
            .any(|tool| tool.name == "execute_command"));
        assert!(runs.list_steps(next).unwrap().iter().any(|step| {
            step.tool_name.as_deref() == Some("execute_command")
                && step.status.as_deref() == Some("denied")
        }));
        drop(db);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_context_policy_failure_rolls_back_the_run_before_it_is_observable() {
        use crate::infrastructure::database::open;
        let workspace = temp_workspace("prompt-context-policy-rollback");
        let path = workspace.join("run-test.db");
        let conversation = {
            let db = Database::new(open(&path).unwrap());
            let conversation = ConversationService::new(&db).create("rollback").unwrap();
            db.lock()
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER reject_prompt_policy BEFORE INSERT ON app_settings
                 WHEN NEW.key GLOB 'composer.no_shell.*'
                 BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;",
                )
                .unwrap();
            let (tx, rx) = mpsc::channel();
            let host = Arc::new(FakeHost {
                frames_tx: tx,
                db: db.clone(),
                persisted: Mutex::new(Vec::new()),
            });
            let executor = Arc::new(CapturingScriptedExecutor::new(Vec::new()));
            let registry = Arc::new(AgentRunRegistry::default());
            let result = start_run_with_options(
                &db,
                Arc::clone(&registry),
                host,
                executor.clone(),
                workspace.clone(),
                request(conversation, "Test rollback"),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
                RunLaunchOptions {
                    prompt_context: Some("[]".into()),
                    ..RunLaunchOptions::default()
                },
            );
            assert!(matches!(result, Err(AgentRunError::Database(_))));
            assert!(executor.captured.lock().unwrap().is_empty());
            assert!(rx.try_recv().is_err());
            assert!(list_runs_for_conversation(&db, conversation)
                .unwrap()
                .is_empty());
            assert!(registry.claim_conversation(conversation));
            conversation
        };
        let db = Database::new(open(&path).unwrap());
        assert!(list_runs_for_conversation(&db, conversation)
            .unwrap()
            .is_empty());
        assert_eq!(
            AgentRunRepository::new(&db)
                .fail_orphaned_running_runs("test restart")
                .unwrap(),
            0
        );
        drop(db);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_context_policy_is_deleted_with_its_run_and_never_reused() {
        use crate::infrastructure::repository::conversations::ConversationRepository;
        let db = crate::infrastructure::database::in_memory_database();
        let conversations = ConversationRepository::new(&db);
        let runs = AgentRunRepository::new(&db);

        for delete_conversation in [true, false] {
            let conversation = conversations.create("first owner", "active").unwrap();
            let marked = runs
                .create_run_with(
                    Some(conversation),
                    "m",
                    "supervised",
                    AgentRunRepository::mark_no_shell,
                )
                .unwrap();
            assert!(runs.no_shell(marked).unwrap());
            if delete_conversation {
                conversations.delete(conversation).unwrap();
            } else {
                runs.delete_run(marked).unwrap();
            }
            let markers: i64 = db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM app_settings WHERE key GLOB 'composer.no_shell.*'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(markers, 0);

            db.lock().unwrap().execute(
                "INSERT INTO app_settings (key, value) VALUES ('composer.no_shell.' || ?1, 'true')",
                [marked],
            ).unwrap();
            let replacement = conversations.create("new owner", "active").unwrap();
            let plain = runs
                .create_run(Some(replacement), "m", "supervised")
                .unwrap();
            assert_eq!(plain, marked);
            assert!(!runs.no_shell(plain).unwrap());
            runs.delete_run(plain).unwrap();
            let restricted = runs
                .create_run_with(
                    Some(replacement),
                    "m",
                    "supervised",
                    AgentRunRepository::mark_no_shell,
                )
                .unwrap();
            assert_eq!(restricted, marked);
            assert!(runs.no_shell(restricted).unwrap());
            conversations.delete(replacement).unwrap();
        }
    }

    #[test]
    fn plan_start_marker_failure_rolls_back_run_and_prompt_policy() {
        let (db, workspace, rx, host) = setup("plan-marker-rollback");
        let conversation = ConversationService::new(&db)
            .create("Plan rollback")
            .unwrap();
        db.lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_plan_start BEFORE INSERT ON run_plans
                 BEGIN SELECT RAISE(ABORT, 'synthetic Plan marker failure'); END;",
            )
            .unwrap();
        let registry = Arc::new(AgentRunRegistry::default());
        let executor = Arc::new(CapturingScriptedExecutor::new(Vec::new()));
        for prompt_context in [None, Some("[]".into())] {
            let result = start_run_with_options(
                &db,
                Arc::clone(&registry),
                host.clone(),
                executor.clone(),
                workspace.clone(),
                request(conversation, "Plan a change"),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
                RunLaunchOptions {
                    is_plan: true,
                    prompt_context,
                    ..RunLaunchOptions::default()
                },
            );
            assert!(matches!(result, Err(AgentRunError::Database(_))));
            assert!(executor.captured.lock().unwrap().is_empty());
            assert!(rx.try_recv().is_err());
            assert!(list_runs_for_conversation(&db, conversation)
                .unwrap()
                .is_empty());
            assert!(list_conversation_run_events(&db, conversation, 0, 100)
                .unwrap()
                .events
                .is_empty());
            assert!(registry.claim_conversation(conversation));
            registry.unclaim_conversation(conversation);
        }
        let no_shell_markers: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key GLOB 'composer.no_shell.*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(no_shell_markers, 0);
        let _ = std::fs::remove_dir_all(workspace);
    }

    fn start_plans_without_terminal_checkpoint(path: &Path, workspace: &Path) -> [(i64, bool); 2] {
        use crate::infrastructure::database::open;

        let db = Database::new(open(path).unwrap());
        // Leave the committed run in its pre-terminal state for the startup sweep.
        db.lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_plan_finish BEFORE INSERT ON durable_run_events
                 WHEN NEW.kind = 'run_finished'
                 BEGIN SELECT RAISE(ABORT, 'synthetic interruption'); END;",
            )
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        [false, true].map(|selected_context| {
            let conversation = ConversationService::new(&db)
                .create("Plan restart")
                .unwrap();
            let run_id = start_run_with_options(
                &db,
                Arc::new(AgentRunRegistry::default()),
                host.clone(),
                Arc::new(ScriptedExecutor::new(vec![Err(ExecutorError::Failure)])),
                workspace.to_path_buf(),
                request(conversation, "Plan a change"),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
                RunLaunchOptions {
                    is_plan: true,
                    prompt_context: selected_context.then(|| "[]".into()),
                    ..RunLaunchOptions::default()
                },
            )
            .unwrap();
            assert!(matches!(
                collect_frames(&rx).last(),
                Some(RunFrame::Finished { .. })
            ));
            (run_id, selected_context)
        })
    }

    #[test]
    fn interrupted_plan_remains_read_only_after_restart_and_continue() {
        use super::super::super::prompt_context::run_no_shell;
        use crate::infrastructure::database::open;
        use crate::infrastructure::repository::run_plans::PLAN_STATUS_NONE;

        let workspace = temp_workspace("plan-restart");
        let path = workspace.join("run-test.db");
        let parent_runs = start_plans_without_terminal_checkpoint(&path, &workspace);

        let db = Database::new(open(&path).unwrap());
        let runs = AgentRunRepository::new(&db);
        assert_eq!(
            runs.fail_orphaned_running_runs("interrupted test").unwrap(),
            2
        );
        db.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_plan_finish")
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        for (parent, selected_context) in parent_runs {
            let plans = RunPlanRepository::new(&db);
            assert_eq!(
                plans.read_by_run(parent).unwrap().unwrap().decision_status,
                PLAN_STATUS_NONE
            );
            assert_eq!(run_no_shell(&db, parent).unwrap(), selected_context);

            let mut write = tool_response("write", "write_file");
            write.tool_calls[0].arguments =
                r#"{"path":"must_not_exist.txt","content":"blocked"}"#.into();
            let executor = Arc::new(CapturingScriptedExecutor::new(vec![
                Ok(write),
                Ok(text_response("Continued plan")),
            ]));
            let next = continue_run(
                &db,
                Arc::new(AgentRunRegistry::default()),
                host.clone(),
                executor.clone(),
                workspace.clone(),
                parent,
                "openai".into(),
                "test-model".into(),
                "synthetic".into(),
                AutonomyMode::FullAutonomous,
                RunPreset::Coding,
            )
            .unwrap();
            assert!(matches!(
                collect_frames(&rx).last(),
                Some(RunFrame::Finished { .. })
            ));
            let captured = executor.captured.lock().unwrap();
            let tools = &captured[0].tools;
            assert!(tools.iter().any(|tool| tool.name == "read_file"));
            assert!(tools.iter().all(|tool| !matches!(
                tool.name.as_str(),
                "write_file" | "edit_file" | "execute_command"
            )));
            assert!(!workspace.join("must_not_exist.txt").exists());
            assert_eq!(run_no_shell(&db, next).unwrap(), selected_context);
        }
        drop(host);
        drop(db);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_context_authority_is_inherited_by_accepted_plan_implementation() {
        use super::super::super::prompt_context::run_no_shell;
        use super::super::plan::{decide_run_plan, PlanDecision};
        let (db, workspace, rx, host) = setup("prompt-context-accepted-plan");
        let conversation = ConversationService::new(&db)
            .create("restricted plan")
            .unwrap();
        let registry = Arc::new(AgentRunRegistry::default());
        let plan = start_run_with_options(
            &db,
            Arc::clone(&registry),
            host.clone(),
            Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
                "## Plan\n1. Update the text.",
            ))])),
            workspace.clone(),
            request(conversation, "Plan a change"),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
            RunLaunchOptions {
                is_plan: true,
                prompt_context: Some("[]".into()),
                ..RunLaunchOptions::default()
            },
        )
        .unwrap();
        collect_frames(&rx);
        assert!(run_no_shell(&db, plan).unwrap());
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
            "Implemented",
        ))]));
        let implementation = decide_run_plan(
            &db,
            plan,
            &PlanDecision::Accepted,
            Some(test_plan_launch_target(
                &workspace,
                executor.clone(),
                &host,
                &registry,
            )),
        )
        .unwrap()
        .launched_run_id
        .unwrap();
        collect_frames(&rx);
        assert!(run_no_shell(&db, implementation).unwrap());
        let captured = executor.captured.lock().unwrap();
        assert!(!captured[0]
            .tools
            .iter()
            .any(|tool| tool.name == "execute_command"));
        assert!(captured[0]
            .tools
            .iter()
            .any(|tool| tool.name == "write_file"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn plan_run_enforces_read_only_even_with_allow_rule_and_emits_plan_ready() {
        use crate::infrastructure::repository::permission_rules::{
            PermissionRulesRepository, RuleEffect,
        };
        let (db, workspace, rx, host) = setup("plan-readonly");
        let conversation_id = ConversationService::new(&db)
            .create("plan readonly")
            .unwrap();
        PermissionRulesRepository::new(&db)
            .insert("coding", "write_file", Some("*"), RuleEffect::Allow, 10)
            .expect("insert allow rule");

        let blocked_write = AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: "pw-1".to_string(),
                name: "write_file".to_string(),
                arguments: r#"{"path":"must_not_exist.txt","content":"boom"}"#.to_string(),
                thought_signature: None,
            }],
            usage: None,
        };
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(blocked_write),
            Ok(text_response("## Plan\n1. Inspect code\n2. Done")),
        ]));
        let registry = Arc::new(AgentRunRegistry::default());

        let run_id = start_run_with_options(
            &db,
            Arc::clone(&registry),
            host.clone() as Arc<dyn AgentRunHost>,
            executor.clone(),
            workspace.clone(),
            request(conversation_id, "Make a plan"),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
            RunLaunchOptions {
                is_plan: true,
                ..RunLaunchOptions::default()
            },
        )
        .expect("plan run starts");

        let frames = collect_frames(&rx);
        assert!(
            !workspace.join("must_not_exist.txt").exists(),
            "Plan run must enforce ReadOnly and block file mutation even under FullAutonomous + Allow rule"
        );
        assert!(
            host.persisted.lock().unwrap().is_empty(),
            "Plan run must not duplicate assistant bubble via persist_assistant_message"
        );

        let saw_plan_ready = frames.iter().any(|f| {
            matches!(
                f,
                RunFrame::Governance {
                    event: AgentRunEvent::PlanReady { plan },
                    ..
                } if plan.run_id == run_id && plan.decision_status == "pending"
            )
        });
        assert!(saw_plan_ready, "expected PlanReady frame: {frames:?}");

        let captured = executor.captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        let tool_names: Vec<&str> = captured[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !tool_names.contains(&"write_file") && !tool_names.contains(&"execute_command"),
            "Plan run must only advertise read-only tools, got {tool_names:?}"
        );
        assert!(captured[0].messages[0].content.contains("Plan Mode"));

        let summaries = list_runs_for_conversation_summary(&db, conversation_id, None, None)
            .expect("list summary");
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].is_plan);
        assert_eq!(
            summaries[0].plan_decision_status.as_deref(),
            Some("pending")
        );
        assert_eq!(
            summaries[0].plan_content.as_deref(),
            Some("## Plan\n1. Inspect code\n2. Done")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    fn launch_test_plan_run(
        db: &Database,
        registry: &Arc<AgentRunRegistry>,
        host: &Arc<FakeHost>,
        workspace: &Path,
        conversation_id: i64,
        rx: &mpsc::Receiver<RunFrame>,
        plan_markdown: &str,
    ) -> i64 {
        let plan_exec = Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
            plan_markdown,
        ))]));
        let plan_run_id = start_run_with_options(
            db,
            Arc::clone(registry),
            host.clone() as Arc<dyn AgentRunHost>,
            plan_exec,
            workspace.to_path_buf(),
            request(conversation_id, plan_markdown),
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
            RunLaunchOptions {
                is_plan: true,
                ..RunLaunchOptions::default()
            },
        )
        .expect("start plan run");
        let _ = collect_frames(rx);
        plan_run_id
    }

    fn test_plan_launch_target(
        workspace: &Path,
        executor: Arc<dyn ProviderExecutor + Send + Sync>,
        host: &Arc<FakeHost>,
        registry: &Arc<AgentRunRegistry>,
    ) -> super::super::plan::PlanLaunchTarget {
        super::super::plan::PlanLaunchTarget {
            workspace_root: workspace.to_path_buf(),
            provider: "openai".to_string(),
            model: "test-model".to_string(),
            credential: "sk-secret".to_string(),
            mode: AutonomyMode::FullAutonomous,
            preset: RunPreset::Coding,
            spend_limit_micro_usd: None,
            executor,
            host: host.clone() as Arc<dyn AgentRunHost>,
            registry: Arc::clone(registry),
        }
    }

    fn drain_approvals_to_finish(
        rx: &mpsc::Receiver<RunFrame>,
        registry: &AgentRunRegistry,
        run_id: i64,
    ) -> bool {
        let mut saw_approval_park = false;
        while let Ok(frame) = rx.recv_timeout(Duration::from_secs(10)) {
            if let RunFrame::Governance {
                event: AgentRunEvent::ApprovalRequested { call_id, .. },
                ..
            } = &frame
            {
                saw_approval_park = true;
                let _ = registry.resolve(run_id, call_id, true);
            }
            if matches!(frame, RunFrame::Finished { .. }) {
                return saw_approval_park;
            }
        }
        panic!("run {run_id} did not finish before timeout");
    }

    fn seed_interrupted_parent_run(db: &Database, conversation_id: i64) -> i64 {
        let repo = AgentRunRepository::new(db);
        let parent_run_id = repo
            .create_run(Some(conversation_id), "test-model", "supervised")
            .expect("create parent run");
        repo.append_step(
            parent_run_id,
            1,
            "model_turn",
            None,
            None,
            Some("Reading notes.txt"),
            None,
            Some(5),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        repo.append_step(
            parent_run_id,
            2,
            "tool_call",
            Some("read_file"),
            Some(r#"{"path":"notes.txt"}"#),
            Some("hello world"),
            Some("succeeded"),
            Some(4),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        // Unmatched approval (crashed before write_file ran)
        repo.append_step(
            parent_run_id,
            3,
            "approval",
            Some("write_file"),
            Some(r#"{"path":"resumed.txt","content":"old"}"#),
            Some("approved"),
            Some("succeeded"),
            Some(1),
            None,
            None,
            Some("user"),
            None,
            None,
        )
        .unwrap();
        // Interrupted tool_call step
        repo.append_step(
            parent_run_id,
            4,
            "tool_call",
            Some("execute_command"),
            Some(r#"{"command":"echo hi"}"#),
            None,
            Some("cancelled"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            repo.fail_orphaned_running_runs("interrupted by shutdown")
                .unwrap(),
            1
        );
        parent_run_id
    }

    #[test]
    fn plan_decision_yes_starts_implementation_without_duplicate_user_msg() {
        use super::super::plan::{decide_run_plan, PlanDecision};
        let (db, workspace, rx, host) = setup("plan-decision-yes");
        let conversation_id = ConversationService::new(&db)
            .create("plan yes conv")
            .unwrap();
        let registry = Arc::new(AgentRunRegistry::default());

        let plan_run_1 = launch_test_plan_run(
            &db,
            &registry,
            &host,
            &workspace,
            conversation_id,
            &rx,
            "## Plan A\n1. Create out.txt",
        );

        let write_call = AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: "impl-1".to_string(),
                name: "write_file".to_string(),
                arguments: r#"{"path":"out.txt","content":"implemented"}"#.to_string(),
                thought_signature: None,
            }],
            usage: None,
        };
        let impl_exec = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(write_call),
            Ok(text_response("Implemented!")),
        ]));
        let yes_result = decide_run_plan(
            &db,
            plan_run_1,
            &PlanDecision::Accepted,
            Some(test_plan_launch_target(
                &workspace,
                impl_exec.clone(),
                &host,
                &registry,
            )),
        )
        .expect("accept plan");
        let impl_id_1 = yes_result.launched_run_id.expect("launched run id");
        assert_eq!(yes_result.plan.decision_status, "accepted");
        assert_eq!(yes_result.plan.implemented_run_id, Some(impl_id_1));
        drain_approvals_to_finish(&rx, &registry, impl_id_1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("out.txt")).unwrap(),
            "implemented"
        );
        let impl_req = &impl_exec.captured.lock().unwrap()[0];
        assert!(
            impl_req.messages.iter().any(|m| m.role
                == crate::application::execution::AiRole::Assistant
                && m.content.contains("Plan A")),
            "accepted implementation run must receive plan content in history"
        );
        // `Yes` button must not add a fake user message to `messages` table.
        let persisted_msgs = ConversationService::new(&db)
            .history(conversation_id)
            .unwrap();
        assert_eq!(
            persisted_msgs.iter().filter(|m| m.role == "user").count(),
            1
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn plan_decision_custom_reply_starts_implementation_with_instructions() {
        use super::super::plan::{decide_run_plan, PlanDecision};
        let (db, workspace, rx, host) = setup("plan-decision-reply");
        let conversation_id = ConversationService::new(&db)
            .create("plan reply conv")
            .unwrap();
        let registry = Arc::new(AgentRunRegistry::default());

        let plan_run_2 = launch_test_plan_run(
            &db,
            &registry,
            &host,
            &workspace,
            conversation_id,
            &rx,
            "## Plan B\n1. Alt approach",
        );
        let custom_exec = Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
            "Followed custom reply",
        ))]));
        let reply_result = decide_run_plan(
            &db,
            plan_run_2,
            &PlanDecision::CustomReply("Use strict mocks instead".to_string()),
            Some(test_plan_launch_target(
                &workspace,
                custom_exec.clone(),
                &host,
                &registry,
            )),
        )
        .expect("custom reply");
        assert!(reply_result.launched_run_id.is_some());
        assert_eq!(reply_result.plan.decision_status, "custom_reply");
        assert_eq!(
            reply_result.plan.decision_reply.as_deref(),
            Some("Use strict mocks instead")
        );
        let _ = collect_frames(&rx);
        let reply_request = &custom_exec.captured.lock().unwrap()[0];
        assert!(reply_request
            .messages
            .iter()
            .any(|m| m.content.contains("Plan B")));
        assert!(reply_request
            .messages
            .iter()
            .any(|m| m.role == crate::application::execution::AiRole::User
                && m.content.contains("Use strict mocks instead")));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn continue_run_skips_completed_tools_marks_incomplete_interrupted_and_revalidates_m04() {
        let (db, workspace, rx, host) = setup("continue-interrupted");
        std::fs::write(workspace.join("notes.txt"), "hello world").unwrap();
        let conversation_id = ConversationService::new(&db)
            .create("continue interrupted")
            .unwrap();
        ConversationService::new(&db)
            .persist_user_message(conversation_id, "Update repo")
            .unwrap();
        let parent_run_id = seed_interrupted_parent_run(&db, conversation_id);

        let new_write = AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: "res-w1".to_string(),
                name: "write_file".to_string(),
                arguments: r#"{"path":"resumed.txt","content":"fresh"}"#.to_string(),
                thought_signature: None,
            }],
            usage: None,
        };
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(new_write),
            Ok(text_response("Continued cleanly")),
        ]));
        let registry = Arc::new(AgentRunRegistry::default());

        let cont_id = continue_run(
            &db,
            Arc::clone(&registry),
            host.clone() as Arc<dyn AgentRunHost>,
            executor.clone(),
            workspace.clone(),
            parent_run_id,
            "openai".to_string(),
            "test-model".to_string(),
            "sk-secret".to_string(),
            AutonomyMode::Supervised,
            RunPreset::Coding,
        )
        .expect("continue_run launches");

        // Parent's stale `write_file` approval MUST NOT authorize the resumed `write_file` call:
        // Supervised mode must park through M04 ApprovalGate and wait for fresh user decision!
        let saw_approval_park = drain_approvals_to_finish(&rx, &registry, cont_id);
        assert!(
            saw_approval_park,
            "resumed mutating tool call must re-validate through M04 ApprovalGate with zero stale cache"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("resumed.txt")).unwrap(),
            "fresh"
        );

        let captured = executor.captured.lock().unwrap();
        let first_req = &captured[0];
        assert!(first_req.messages.iter().any(|m| {
            m.tool_result
                .as_ref()
                .is_some_and(|r| r.name == "read_file" && r.content.contains("hello world"))
        }));
        assert!(first_req.messages.iter().any(|m| {
            m.tool_result
                .as_ref()
                .is_some_and(|r| r.name == "write_file" && r.content.contains("[Interrupted]"))
        }));
        assert!(first_req.messages.iter().any(|m| {
            m.tool_result
                .as_ref()
                .is_some_and(|r| r.name == "execute_command" && r.content.contains("[Interrupted]"))
        }));
        drop(captured);

        let summaries = list_runs_for_conversation_summary(&db, conversation_id, None, None)
            .expect("summaries");
        let cont_summary = summaries.iter().find(|r| r.run.id == cont_id).unwrap();
        assert_eq!(cont_summary.continued_from_run_id, Some(parent_run_id));
        assert_eq!(
            AgentRunRepository::new(&db)
                .list_steps(parent_run_id)
                .unwrap()
                .len(),
            4
        );

        // Non-interrupted completed run cannot be continued
        let err = continue_run(
            &db,
            Arc::clone(&registry),
            host as Arc<dyn AgentRunHost>,
            executor,
            workspace.clone(),
            cont_id,
            "openai".to_string(),
            "test-model".to_string(),
            "sk-secret".to_string(),
            AutonomyMode::Supervised,
            RunPreset::Coding,
        )
        .expect_err("completed run cannot continue");
        assert!(matches!(err, AgentRunError::RunNotInterrupted { .. }));

        let _ = std::fs::remove_dir_all(workspace);
    }
    #[test]
    fn agent_admits_and_links_current_attachments_and_replays_them_in_later_runs() {
        let (db, workspace, rx, host) = setup("managed-attachments");
        let service = ConversationService::new(&db);
        let conversation = service.create("attached agent").unwrap();
        let source = workspace.join("note.rs");
        std::fs::write(&source, "fn main() { /* context */ }").unwrap();
        let store = crate::application::managed_attachments::AttachmentStore::open(
            workspace.join("app-data/attachments"),
        )
        .unwrap();
        let attachment =
            crate::application::attachments::AttachmentService::with_store(&db, store.clone())
                .attach(conversation, "note.rs", &source.to_string_lossy(), None)
                .unwrap();
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(text_response("first answer")),
            Ok(text_response("second answer")),
        ]));
        let registry = Arc::new(AgentRunRegistry::default());
        let mut first = request(conversation, "inspect attached code");
        first.model = crate::infrastructure::providers::catalog::openai::SUPPORTED_MODELS[0].into();
        start_run_with_options(
            &db,
            Arc::clone(&registry),
            host.clone(),
            executor.clone(),
            workspace.clone(),
            first,
            AutonomyMode::Supervised,
            RunPreset::Coding,
            RunLaunchOptions {
                is_plan: true,
                attachment_ids: vec![attachment.id],
                ..RunLaunchOptions::default()
            },
        )
        .unwrap();
        collect_frames(&rx);
        assert!(
            crate::infrastructure::repository::attachments::AttachmentRepository::new(&db)
                .read(attachment.id)
                .unwrap()
                .unwrap()
                .message_id
                .is_some()
        );
        std::fs::remove_file(&source).unwrap();
        let mut second = request(conversation, "remember that file");
        second.model =
            crate::infrastructure::providers::catalog::openai::SUPPORTED_MODELS[0].into();
        start_run(
            &db,
            registry,
            host,
            executor.clone(),
            workspace.clone(),
            second,
            AutonomyMode::Supervised,
            RunPreset::Coding,
        )
        .unwrap();
        collect_frames(&rx);
        let captured = executor.captured.lock().unwrap();
        let first_user = captured[0]
            .messages
            .iter()
            .find(|m| m.content == "inspect attached code")
            .unwrap();
        assert_eq!(first_user.attachments.len(), 1);
        assert_eq!(
            first_user.attachments[0].payload,
            crate::application::execution::AiAttachmentPayload::Text(
                "fn main() { /* context */ }".into()
            )
        );
        let replayed = captured[1]
            .messages
            .iter()
            .find(|m| m.content == "inspect attached code")
            .unwrap();
        assert_eq!(replayed.attachments, first_user.attachments);
        assert!(captured[1].messages.last().unwrap().attachments.is_empty());
        drop(captured);
        drop(store);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn rejected_agent_attachment_preserves_draft_and_never_spawns_a_run() {
        let (db, workspace, _rx, host) = setup("attachment-capability-rejection");
        let conversation = ConversationService::new(&db)
            .create("attachment guard")
            .unwrap();
        let source = workspace.join("note.txt");
        std::fs::write(&source, "source").unwrap();
        let store = crate::application::managed_attachments::AttachmentStore::open(
            workspace.join("app-data/attachments"),
        )
        .unwrap();
        let attachment = crate::application::attachments::AttachmentService::with_store(&db, store)
            .attach(conversation, "note.txt", &source.to_string_lossy(), None)
            .unwrap();
        let executor = Arc::new(CapturingScriptedExecutor::new(Vec::new()));
        let registry = Arc::new(AgentRunRegistry::default());
        let result = start_run_with_options(
            &db,
            Arc::clone(&registry),
            host,
            executor.clone(),
            workspace.clone(),
            request(conversation, "inspect"),
            AutonomyMode::Supervised,
            RunPreset::Coding,
            RunLaunchOptions {
                attachment_ids: vec![attachment.id],
                ..RunLaunchOptions::default()
            },
        );
        assert!(matches!(
            result,
            Err(AgentRunError::Attachment(
                crate::application::conversations::ConversationError::AttachmentUnsupported { .. }
            ))
        ));
        assert!(executor.captured.lock().unwrap().is_empty());
        assert!(ConversationService::new(&db)
            .history(conversation)
            .unwrap()
            .is_empty());
        assert!(list_runs_for_conversation(&db, conversation)
            .unwrap()
            .is_empty());
        assert_eq!(
            crate::infrastructure::repository::attachments::AttachmentRepository::new(&db)
                .read(attachment.id)
                .unwrap()
                .unwrap()
                .message_id,
            None
        );
        assert!(registry.claim_conversation(conversation));
        assert_eq!(std::fs::read(source).unwrap(), b"source");
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn lifecycle_lock_failure_preserves_agent_draft_and_releases_claim() {
        let workspace = temp_workspace("attachment-lifecycle-failure");
        let app_data = workspace.join("app-data");
        std::fs::create_dir(&app_data).unwrap();
        let db = Database::new(
            crate::infrastructure::database::open(app_data.join("nexora.db")).unwrap(),
        );
        let conversation = ConversationService::new(&db)
            .create("lock failure")
            .unwrap();
        let original = workspace.join("original.txt");
        std::fs::write(&original, b"original").unwrap();
        let store = crate::application::managed_attachments::AttachmentStore::open(
            app_data.join("attachments"),
        )
        .unwrap();
        let copy = store
            .copy(conversation, "original.txt", b"original")
            .unwrap();
        let attachments =
            crate::infrastructure::repository::attachments::AttachmentRepository::new(&db);
        let attachment = attachments
            .create(
                conversation,
                "original.txt",
                &copy.to_string_lossy(),
                Some(8),
                Some("text/plain"),
            )
            .unwrap();
        std::fs::create_dir(app_data.join("nexora.db.attachments.lock")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let host = Arc::new(FakeHost {
            frames_tx: tx,
            db: db.clone(),
            persisted: Mutex::new(Vec::new()),
        });
        let executor = Arc::new(CapturingScriptedExecutor::new(Vec::new()));
        let registry = Arc::new(AgentRunRegistry::default());
        let error = start_run_with_options(
            &db,
            Arc::clone(&registry),
            host.clone(),
            executor.clone(),
            workspace.clone(),
            request(conversation, "guarded secret content"),
            AutonomyMode::Supervised,
            RunPreset::Coding,
            RunLaunchOptions {
                attachment_ids: vec![attachment],
                ..RunLaunchOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, AgentRunError::Database(_)));
        let safe = crate::commands::error::CommandError::from(error);
        assert_eq!(
            safe.message,
            "a database operation failed; no data was changed"
        );
        assert!(executor.captured.lock().unwrap().is_empty());
        assert!(host.persisted.lock().unwrap().is_empty());
        assert!(rx.try_recv().is_err());
        assert!(ConversationService::new(&db)
            .history(conversation)
            .unwrap()
            .is_empty());
        assert!(list_runs_for_conversation(&db, conversation)
            .unwrap()
            .is_empty());
        assert_eq!(
            attachments.read(attachment).unwrap().unwrap().message_id,
            None
        );
        assert!(
            registry.claim_conversation(conversation),
            "failed setup must release its claim"
        );
        registry.unclaim_conversation(conversation);
        assert_eq!(
            crate::application::managed_attachments::read(&copy).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        drop(host);
        drop(db);
        drop(store);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn continue_replays_managed_attachment_history_without_a_second_user_commit() {
        let (db, workspace, rx, host) = setup("continue-attachments");
        let service = ConversationService::new(&db);
        let conversation = service.create("continue attachment").unwrap();
        let source = workspace.join("note.txt");
        std::fs::write(&source, "preserved context").unwrap();
        let store = crate::application::managed_attachments::AttachmentStore::open(
            workspace.join("app-data/attachments"),
        )
        .unwrap();
        let attachment = crate::application::attachments::AttachmentService::with_store(&db, store)
            .attach(conversation, "note.txt", &source.to_string_lossy(), None)
            .unwrap();
        let model = crate::infrastructure::providers::catalog::openai::SUPPORTED_MODELS[0];
        service
            .persist_user_message_with_attachments(
                conversation,
                "original request",
                "openai",
                model,
                &[attachment.id],
            )
            .unwrap();
        let parent = seed_interrupted_parent_run(&db, conversation);
        let executor = Arc::new(CapturingScriptedExecutor::new(vec![Ok(text_response(
            "resumed",
        ))]));
        continue_run(
            &db,
            Arc::new(AgentRunRegistry::default()),
            host,
            executor.clone(),
            workspace.clone(),
            parent,
            "openai".into(),
            model.into(),
            "test-key".into(),
            AutonomyMode::Supervised,
            RunPreset::Coding,
        )
        .unwrap();
        collect_frames(&rx);
        let captured = executor.captured.lock().unwrap();
        let prior = captured[0]
            .messages
            .iter()
            .find(|m| m.content == "original request")
            .unwrap();
        assert_eq!(prior.attachments.len(), 1);
        assert_eq!(
            prior.attachments[0].payload,
            crate::application::execution::AiAttachmentPayload::Text("preserved context".into())
        );
        assert_eq!(
            service
                .history(conversation)
                .unwrap()
                .iter()
                .filter(|m| m.role == "user")
                .count(),
            1
        );
        drop(captured);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    fn seed_manual_compaction_fixture(db: &Database, conv_id: i64) {
        let messages_repo = MessageRepository::new(db);
        for (role, text) in [
            ("user", "Build M16 context compaction"),
            (
                "assistant",
                "We decided to use durable_run_events for compaction state.",
            ),
            ("user", "Add ContextRing popover breakdown"),
            ("assistant", "Working on ContextPanel next."),
        ] {
            messages_repo
                .create(conv_id, role, text, None, None)
                .unwrap();
        }
        let runs_repo = AgentRunRepository::new(db);
        let run_id = runs_repo
            .create_run(Some(conv_id), "gpt-5.6-terra", "supervised")
            .unwrap();
        runs_repo
            .append_step(
                run_id,
                1,
                "tool_call",
                Some("write_file"),
                Some(r#"{"path":"src/components/ContextRing.tsx"}"#),
                Some("wrote file"),
                Some("succeeded"),
                Some(4),
                None,
                None,
                None,
                Some(500),
                Some(120),
            )
            .unwrap();
        runs_repo
            .finalize_run(run_id, "completed", Some("ok"), None, None, None)
            .unwrap();
        let plan_run_id = runs_repo
            .create_run(Some(conv_id), "gpt-5.6-terra", "supervised")
            .unwrap();
        let plans = RunPlanRepository::new(db);
        plans.mark_started(plan_run_id, conv_id).unwrap();
        plans
            .create(
                plan_run_id,
                conv_id,
                "Phase 1: Backend accounting; Phase 2: Ring UI",
            )
            .unwrap();
        plans
            .decide(plan_run_id, "accepted", None, Some(run_id))
            .unwrap();
    }

    #[test]
    fn manual_compaction_preserves_goal_plan_decisions_files_and_persists_durable_event() {
        let (db, workspace, _rx, host) = setup("manual-compaction");
        let conv_svc = ConversationService::new(&db);
        let conv_id = conv_svc.create("Compaction test").unwrap();
        seed_manual_compaction_fixture(&db, conv_id);

        let outcome = compact_conversation_context(
            &db,
            None,
            Some(host.as_ref()),
            None,
            conv_id,
            Some("openai"),
            Some("gpt-5.6-terra"),
        )
        .expect("manual compaction succeeds");
        assert_eq!(outcome.status, "compacted");
        assert_eq!(outcome.reason, "manual");
        assert!(outcome.event_id.is_some());
        assert!(outcome.goal.contains("Build M16 context compaction"));
        assert!(outcome.active_plan.contains("Backend accounting"));
        assert!(outcome.key_decisions.contains("durable_run_events"));
        assert!(outcome
            .touched_files
            .contains(&"src/components/ContextRing.tsx".to_string()));

        // Durable journal contains `context_compacted`.
        let page = list_conversation_run_events(&db, conv_id, 0, 100).unwrap();
        assert!(
            page.events
                .iter()
                .any(|e| e.kind == "context_compacted" && e.event_id == outcome.event_id.unwrap()),
            "durable journal must record context_compacted"
        );

        // Stats reflect compaction count and timestamp.
        let stats = conversation_context_stats_for_selection(
            &db,
            conv_id,
            Some("openai"),
            Some("gpt-5.6-terra"),
        )
        .unwrap();
        assert_eq!(stats.compaction_count, 1);
        assert!(stats.last_compacted_at.is_some());

        // Subsequent agent_history injects the structured compaction summary.
        let history = conv_svc.agent_history(conv_id, "openai").unwrap();
        assert!(history[0]
            .content
            .contains(crate::application::agent::compaction::SUMMARY_PREFIX));
        assert!(history[0].content.contains("Build M16 context compaction"));

        drop(conv_svc);
        drop(host);
        drop(db);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn in_run_auto_compaction_persists_durable_context_compacted_event() {
        let (db, workspace, rx, host) = setup("auto-compaction-durable");
        let conv_id = ConversationService::new(&db)
            .create("Auto compact")
            .unwrap();
        let messages_repo = MessageRepository::new(&db);
        for i in 0..4 {
            messages_repo
                .create(
                    conv_id,
                    "user",
                    &format!("Goal item {i} {}", "q".repeat(1200)),
                    None,
                    None,
                )
                .unwrap();
            messages_repo
                .create(
                    conv_id,
                    "assistant",
                    &format!("We decided step {i} {}", "a".repeat(1200)),
                    None,
                    None,
                )
                .unwrap();
        }
        // Set manual capability override so context_window = 30_000 (20k reserve -> 10k usable -> 80% = 8k).
        crate::application::capabilities::set_manual_override(
            &db,
            "openai",
            "gpt-5.6-terra",
            Some(crate::application::capabilities::ModelCapabilityOverride {
                context_window: Some(30_000),
                ..Default::default()
            }),
        )
        .unwrap();

        let executor = Arc::new(CapturingScriptedExecutor::new(vec![
            Ok(AiResponse {
                content: String::new(),
                model: "gpt-5.6-terra".into(),
                tool_calls: vec![crate::application::execution::ToolCall {
                    id: "tc-1".into(),
                    name: "list_directory".into(),
                    arguments: "{}".into(),
                    thought_signature: None,
                }],
                usage: Some(crate::application::execution::TokenUsage {
                    input_tokens: 9_000,
                    output_tokens: 100,
                }),
            }),
            Ok(text_response("1. Goal\nGoal item 0")),
            Ok(text_response("final compacted answer")),
        ]));
        let registry = Arc::new(AgentRunRegistry::default());
        let run_id = start_run(
            &db,
            Arc::clone(&registry),
            host,
            executor,
            workspace.clone(),
            AgentRunRequest {
                conversation_id: conv_id,
                user_request: "Finish the task".into(),
                provider: "openai".into(),
                model: "gpt-5.6-terra".into(),
                credential: "test-key".into(),
                max_iterations: None,
                spend_limit_micro_usd: None,
            },
            AutonomyMode::FullAutonomous,
            RunPreset::Coding,
        )
        .unwrap();

        let frames = collect_frames(&rx);
        assert!(frames.iter().any(|f| matches!(
            f,
            RunFrame::Governance {
                event: AgentRunEvent::ContextCompacted { reason, .. },
                ..
            } if reason == "threshold"
        )));

        let page = list_conversation_run_events(&db, conv_id, 0, 100).unwrap();
        assert!(
            page.events
                .iter()
                .any(|e| e.kind == "context_compacted" && e.run_id == Some(run_id)),
            "in-run 80%/20k-reserve compaction must persist context_compacted in durable_run_events"
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }
}
