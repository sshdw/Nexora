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
use super::control::{AgentRunEvent, RunControl};
use super::governance::{audit, AuditEvent, AuditLog};
use super::lifecycle::{observe_transition, RunState};
use super::permissions::RunPreset;
use super::persistence::{mode_to_column, terminal_outcome, RunRecorder};
use super::runner::AgentRunner;
use crate::application::context_stats::context_limit_for;
use crate::application::conversations::ConversationService;
use crate::application::execution::{AiMessage, ProviderExecutor, RequestError};
use crate::application::routing::{RoutingService, TaskKind};
use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentRunRepository, AgentStep};

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
    /// Provider/credential resolution failed (FR-014 classifications).
    Request(RequestError),
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

/// Resolve the persisted autonomy mode from `app_settings` (`agent.autonomy`),
/// defaulting to `SemiAutonomous` when unset or invalid.
#[must_use]
pub(crate) fn resolve_autonomy_mode(db: &Database) -> AutonomyMode {
    let svc = SettingsService::new(db);
    match svc.read("agent.autonomy") {
        Ok(Some(value)) => parse_autonomy_mode(Some(value.as_str())),
        _ => AutonomyMode::SemiAutonomous,
    }
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

/// Resolve the persisted run preset from `app_settings` (`agent.preset`),
/// defaulting to `Coding` when unset or invalid.
#[must_use]
pub(crate) fn resolve_preset(db: &Database) -> RunPreset {
    let svc = SettingsService::new(db);
    match svc.read(PRESET_KEY) {
        Ok(Some(value)) => parse_preset(Some(value.as_str())),
        _ => RunPreset::Coding,
    }
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
pub(crate) fn resolve_approval_timeout(db: &Database) -> std::time::Duration {
    let svc = SettingsService::new(db);
    match svc.read(APPROVAL_TIMEOUT_KEY) {
        Ok(Some(value)) => match value.as_str().trim().parse::<u64>() {
            Ok(secs) if (MIN_APPROVAL_TIMEOUT_SECS..=MAX_APPROVAL_TIMEOUT_SECS).contains(&secs) => {
                std::time::Duration::from_secs(secs)
            }
            _ => std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
        },
        _ => std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
    }
}
/// Setting key backing the per-run spend guard (`agent.spend_limit_micro_usd`).
pub(crate) use nexora_core::config::SPEND_LIMIT_KEY;

/// Resolve the persisted per-run spend limit in micro-USD, mirroring
/// [`resolve_autonomy_mode`]: `None` means "no limit" and preserves today's
/// behaviour. A stored value yields `Some` only when it parses as `u64` and
/// is greater than zero; absent, unparseable, zero, or unreadable values
/// all resolve to `None`.
#[must_use]
pub(crate) fn resolve_spend_limit(db: &Database) -> Option<u64> {
    let svc = SettingsService::new(db);
    match svc.read(SPEND_LIMIT_KEY) {
        Ok(Some(value)) => match value.as_str().trim().parse::<u64>() {
            Ok(limit) if limit > 0 => Some(limit),
            _ => None,
        },
        _ => None,
    }
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
    start_run_scoped(
        db,
        registry,
        host,
        executor,
        workspace_root,
        request,
        mode,
        preset,
        super::access::AccessContext::root(db),
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
    if super::tools::path_safety::check_private(&workspace_root).is_err() {
        return Err(AgentRunError::WorkspaceProtected);
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
#[allow(clippy::too_many_arguments)]
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
) -> Result<i64, AgentRunError> {
    // Load the persisted history BEFORE persisting the current user
    // message (agent memory slice): after the persist the current turn is
    // already in the table, so loading first is what makes it appear exactly
    // once in the run's context.
    let history: Vec<AiMessage> = match ConversationService::new(db)
        .agent_history(request.conversation_id, &request.provider)
    {
        Ok(history) => history,
        Err(crate::application::conversations::ConversationError::NotFound { id }) => {
            return Err(AgentRunError::ConversationNotFound { id });
        }
        Err(other) => {
            log::warn!("agent run setup: history load failed, continuing empty: {other}");
            Vec::new()
        }
    };

    // Load the prior action trace AFTER the text history (which stays first)
    // and BEFORE persisting the current user message. The current run row is
    // created below, so no live row exists yet here — and any live row is
    // still excluded defensively. Best-effort: any failure keeps the run
    // going with an empty trace.
    let action_summary = load_action_summary(db, request.conversation_id);

    // Persist the user message BEFORE spawning (design §3.2): a crash can
    // never lose it, and it appears in the thread immediately. No assistant
    // message is ever created unless the run later succeeds (plain-chat
    // doctrine). Empty content is rejected by the `messages` schema CHECK,
    // exactly as in plain chat.
    match ConversationService::new(db)
        .persist_user_message(request.conversation_id, &request.user_request)
    {
        Ok(_) => {}
        Err(crate::application::conversations::ConversationError::NotFound { id }) => {
            return Err(AgentRunError::ConversationNotFound { id });
        }
        Err(crate::application::conversations::ConversationError::Database(err)) => {
            return Err(AgentRunError::Database(err));
        }
        Err(other) => {
            log::error!("agent run setup: unexpected user-message error: {other}");
            return Err(AgentRunError::Request(RequestError::Execution {
                name: request.provider.clone(),
                message: other.to_string(),
            }));
        }
    }

    // Create the linked run row (D50) through the recorder so the run id is
    // known synchronously; the spawned run adopts it (no second insert).
    let checkpoint_store = host.checkpoint_store();
    let gate = ApprovalGate::new(mode)
        .with_access(access)
        .with_checkpoints(checkpoint_store.clone());
    gate.bind_workspace(&workspace_root);
    // NEX-AGENT-004: every park on this run expires on the configured
    // deadline, so an unanswered approval can never hold the conversation
    // claim forever.
    gate.set_timeout(resolve_approval_timeout(db));
    let control = RunControl::new();
    let mode_column = mode_to_column(mode);
    let Some(run_id) = RunRecorder::new(db)
        .with_conversation(request.conversation_id)
        .create_run_row(&request.model, mode_column)
    else {
        return Err(AgentRunError::RunNotPersisted);
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
    let run_registry = Arc::clone(&registry);
    let run_host = Arc::clone(&host);
    let run_request = request;
    let run_handle = std::thread::Builder::new()
        .name(format!("agent-run-{run_id}"))
        .spawn(move || {
            // The recorder borrows this thread's sender clone; the runner owns
            // the recorder, so the borrow lives exactly as long as the loop.
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
                // Task T4 wiring: the run's model window enables the proactive
                // compaction trigger (`usable_context_tokens` + `decide`);
                // without it the trigger stays dormant (limit `0`) and only
                // reactive overflow recovery runs.
                let context_limit = context_limit_for(
                    Some(run_request.provider.as_str()),
                    Some(run_request.model.as_str()),
                );
                let mut runner = AgentRunner::new(executor.as_ref(), &workspace_root)
                    .with_control(control)
                    .with_approval_gate(gate)
                    .with_preset(preset)
                    .with_permission_store(permission_store)
                    .with_history(history)
                    .with_action_summary(action_summary)
                    .with_context_limit(context_limit)
                    .with_run_flags(resolved_flags.run_flags())
                    .with_audit_log(Arc::clone(&audit_log))
                    .with_event_sender(tx_for_recorder.clone());
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
            // message on failure (plain-chat doctrine).
            if let Ok(content) = &outcome {
                run_host.persist_assistant_message(
                    run_request.conversation_id,
                    content,
                    &run_request.provider,
                    &run_request.model,
                );
            }

            let (status, final_content, error) = terminal_outcome(&outcome);
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
                notify_durable(
                    &forward_db,
                    forward_host.as_ref(),
                    run_id,
                    forward_conversation_id,
                    "terminal",
                );
                forward_host.emit(&RunFrame::Finished { run_id, event });
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

/// Bounded DTO for durable history. Original identities are retained, with
/// exactly the same step previews as the existing history path.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurableRunEvent {
    pub event_id: i64,
    pub conversation_id: i64,
    pub run_id: i64,
    pub kind: String,
    pub step_id: Option<i64>,
    pub schema_version: i64,
    pub created_at: i64,
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
        assert!(events.iter().all(|e| e.run_id == run_id));
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
}
