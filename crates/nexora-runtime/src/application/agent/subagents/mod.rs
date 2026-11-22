//! Automatic visible task-scoped subagents (`M19` / `P27`, `S11-07`, `S11-08`).
//!
//! Each parent agent run (`depth == 0`) may own one [`SubagentManager`] that
//! orchestrates task-scoped child runs with strict hard invariants:
//!
//! - **Bounded concurrency**: default `3`, configurable `1..=6`
//!   (`agent.subagent_max_concurrency`).
//! - **Max depth = 1**: child runners execute at `depth = 1` with NO
//!   `SubagentManager` attached; subagent tool schemas are omitted and
//!   `dispatch.rs` fails closed if a subagent attempts to invoke one.
//! - **Parent model default**: when `model` is omitted, the child ALWAYS uses
//!   the parent's model (NEVER auto-escalating to a pricier model). Explicit
//!   `model` + `effort` overrides stay on the parent provider and validate
//!   `effort` against the `M06` capability catalog's `supported_efforts`.
//! - **Lifetime access ceiling (`child_access <= parent_current_access`)**:
//!   every child derives its [`AccessContext`] from `parent_access.child(...)`
//!   so live parent downgrades, Plan mode, Document preset, and parent
//!   cancellation/autonomy downgrades propagate immediately.
//! - **Parallel write isolation (`S11-08`)**: optional temporary `git worktree`
//!   and branch for writing subagents with serialized `git apply --check` /
//!   `git apply` transfer on completion (failing cleanly on conflicts) and
//!   mandatory cleanup on completion, kill, error, and `Drop`. Read-only
//!   subagents always share the parent workspace without copying.

pub(crate) mod worktree;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::application::capabilities::resolve_model_capabilities_opt_db;
use crate::application::execution::{AiMessage, ModelConfig, ProviderExecutor, ReasoningEffort};
use crate::application::flags::RunFlags;
use crate::infrastructure::database::Database;

use super::approval::{ApprovalDecision, ApprovalGate, AutonomyMode};
use super::control::{AgentRunEvent, RunControl};
use super::errors::AgentError;
use super::permissions::{PermissionStore, RunPreset};
use super::persistence::step_preview_opt;
use super::roles::TaskCapability;
use super::runner::AgentRunner;
use worktree::{PatchTransferOutcome, WorktreeIsolation};

pub(crate) use nexora_core::config::{
    DEFAULT_SUBAGENT_MAX_CONCURRENCY, MAX_SUBAGENT_MAX_CONCURRENCY, MIN_SUBAGENT_MAX_CONCURRENCY,
    SUBAGENT_MAX_CONCURRENCY_KEY,
};

/// Maximum nesting depth: only top-level runs (`depth == 0`) may spawn subagents.
pub(crate) const MAX_SUBAGENT_DEPTH: u8 = 1;

/// Default maximum LLM turns per subagent invocation.
pub(crate) const DEFAULT_SUBAGENT_MAX_TURNS: usize = 12;
/// Hard ceiling on per-subagent LLM turns.
pub(crate) const MAX_SUBAGENT_MAX_TURNS: usize = 30;

/// Default wall-clock timeout per subagent invocation, in seconds.
pub(crate) const DEFAULT_SUBAGENT_TIMEOUT_SECS: u64 = 180;
/// Minimum wall-clock timeout accepted for a subagent invocation, in seconds.
pub(crate) const MIN_SUBAGENT_TIMEOUT_SECS: u64 = 5;
/// Maximum wall-clock timeout accepted for a subagent invocation, in seconds.
pub(crate) const MAX_SUBAGENT_TIMEOUT_SECS: u64 = 900;

/// Default wait timeout for `wait_subagent` when omitted, in seconds.
pub(crate) const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 300;

/// Clamp a configured subagent concurrency value strictly into `1..=6`.
#[must_use]
pub(crate) fn clamp_max_concurrency(value: usize) -> usize {
    value.clamp(MIN_SUBAGENT_MAX_CONCURRENCY, MAX_SUBAGENT_MAX_CONCURRENCY)
}

/// Lifecycle state of one managed subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl SubagentStatus {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }

    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// One bounded, secret-free step record emitted by a subagent and streamed
/// to the parent run UI via `AgentRunEvent::SubagentStep`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SubagentStepRecord {
    pub seq: usize,
    pub kind: String,
    pub tool_name: Option<String>,
    pub target: Option<String>,
    pub status: Option<String>,
    pub observation: Option<String>,
}

/// UI-facing snapshot of one subagent (`AgentRunEvent::SubagentUpdated` and
/// `LiveRunState::subagents`). Secret-free by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SubagentSnapshot {
    pub subagent_id: String,
    pub title: String,
    pub status: String,
    pub model: String,
    pub effort: Option<String>,
    pub access: String,
    pub worktree_isolated: bool,
    pub steps_taken: usize,
    pub max_turns: usize,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub patch_applied: bool,
    pub patch_conflict: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<SubagentStepRecord>,
}

/// Parent-run context needed by [`SubagentManager`] to launch bounded children.
pub(crate) struct SubagentManagerConfig {
    pub parent_run_id: i64,
    pub db: Option<Database>,
    pub executor: Arc<dyn ProviderExecutor + Send + Sync>,
    pub workspace_root: PathBuf,
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub credential: String,
    pub parent_control: RunControl,
    pub parent_gate: ApprovalGate,
    pub parent_preset: RunPreset,
    pub parent_is_plan: bool,
    pub permission_store: Option<PermissionStore>,
    pub project_instructions: Option<String>,
    pub run_flags: RunFlags,
    pub event_sender: Option<Sender<AgentRunEvent>>,
    pub max_concurrency: usize,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    task: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    use_worktree: Option<bool>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    max_turns: Option<usize>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SendMessageArgs {
    subagent_id: String,
    message: String,
    #[serde(default)]
    max_turns: Option<usize>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct WaitArgs {
    #[serde(default)]
    subagent_id: Option<String>,
    #[serde(default)]
    subagent_ids: Option<Vec<String>>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct KillArgs {
    subagent_id: String,
}

#[allow(clippy::struct_excessive_bools)]
struct SubagentEntry {
    id: String,
    title: String,
    status: SubagentStatus,
    model: String,
    effort: Option<String>,
    requested_capability: TaskCapability,
    effective_capability: TaskCapability,
    use_worktree: bool,
    worktree_isolated: bool,
    max_turns: usize,
    timeout_secs: u64,
    steps_taken: usize,
    started_at: i64,
    finished_at: Option<i64>,
    result: Option<String>,
    result_summary: Option<String>,
    error: Option<String>,
    patch_applied: bool,
    patch_conflict: bool,
    changed_files: Vec<String>,
    steps: Vec<SubagentStepRecord>,
    history: Vec<AiMessage>,
    control: RunControl,
    gate: ApprovalGate,
    killed: bool,
    step_seq: Arc<AtomicUsize>,
}

impl SubagentEntry {
    fn snapshot(&self, include_steps: bool) -> SubagentSnapshot {
        SubagentSnapshot {
            subagent_id: self.id.clone(),
            title: self.title.clone(),
            status: self.status.as_str().to_string(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            access: self.effective_capability.as_str().to_string(),
            worktree_isolated: self.worktree_isolated,
            steps_taken: self.steps_taken,
            max_turns: self.max_turns,
            started_at: self.started_at,
            finished_at: self.finished_at,
            result_summary: self.result_summary.clone(),
            error: self.error.clone(),
            patch_applied: self.patch_applied,
            patch_conflict: self.patch_conflict,
            steps: if include_steps {
                self.steps.clone()
            } else {
                Vec::new()
            },
        }
    }
}

#[derive(Default)]
struct ManagerState {
    next_seq: usize,
    order: Vec<String>,
    entries: HashMap<String, SubagentEntry>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl ManagerState {
    fn active_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.status == SubagentStatus::Running)
            .count()
    }
}

/// Run-scoped manager coordinating all subagents spawned by one parent run.
pub(crate) struct SubagentManager {
    config: SubagentManagerConfig,
    max_concurrency: usize,
    state: Mutex<ManagerState>,
    changed: Condvar,
    patch_apply_lock: Mutex<()>,
}

impl std::fmt::Debug for SubagentManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentManager")
            .field("parent_run_id", &self.config.parent_run_id)
            .field("max_concurrency", &self.max_concurrency)
            .finish_non_exhaustive()
    }
}

impl SubagentManager {
    /// Create a new run-scoped subagent manager with concurrency clamped to `1..=6`.
    #[must_use]
    pub(crate) fn new(config: SubagentManagerConfig) -> Arc<Self> {
        let max_concurrency = clamp_max_concurrency(config.max_concurrency);
        Arc::new(Self {
            config,
            max_concurrency,
            state: Mutex::new(ManagerState::default()),
            changed: Condvar::new(),
            patch_apply_lock: Mutex::new(()),
        })
    }

    /// Effective max concurrency (`1..=6`, default `3`).
    #[must_use]
    pub(crate) const fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// Snapshot all subagents in creation order (for live rehydration / UI).
    #[must_use]
    pub(crate) fn snapshots(&self) -> Vec<SubagentSnapshot> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .order
            .iter()
            .filter_map(|id| guard.entries.get(id).map(|e| e.snapshot(true)))
            .collect()
    }

    /// Route an approval response for a parked child subagent tool call.
    #[must_use]
    pub(crate) fn resolve_approval(
        &self,
        call_id: &str,
        decision: ApprovalDecision,
        scope: Option<&str>,
    ) -> bool {
        let gates: Vec<ApprovalGate> = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .entries
                .values()
                .filter(|e| e.status == SubagentStatus::Running)
                .map(|e| e.gate.clone())
                .collect()
        };
        for gate in gates {
            if gate.respond_with_scope(call_id, decision, scope) {
                return true;
            }
        }
        false
    }

    /// Read-only peek at a parked child subagent approval's
    /// `(tool_name, group_key)` without resolving it, so
    /// `group:confirm:<scope>` persistent approval validation succeeds for
    /// child subagents.
    #[must_use]
    pub(crate) fn pending_scope_info(&self, call_id: &str) -> Option<(String, Option<String>)> {
        let gates: Vec<ApprovalGate> = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .entries
                .values()
                .filter(|e| e.status == SubagentStatus::Running)
                .map(|e| e.gate.clone())
                .collect()
        };
        gates
            .into_iter()
            .find_map(|gate| gate.pending_info(call_id))
    }

    /// Return the first unresolved pending approval held by any running child
    /// subagent (so `inspect_run_live` remains truthful while a child parks).
    #[must_use]
    pub(crate) fn pending_approval(&self) -> Option<super::approval::PendingApprovalSnapshot> {
        let gates: Vec<ApprovalGate> = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .entries
                .values()
                .filter(|e| e.status == SubagentStatus::Running)
                .map(|e| e.gate.clone())
                .collect()
        };
        gates.into_iter().find_map(|gate| gate.pending_snapshot())
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn child_mode_for_test(&self, subagent_id: &str) -> Option<AutonomyMode> {
        let gate = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.entries.get(subagent_id).map(|e| e.gate.clone())
        }?;
        Some(gate.mode())
    }

    /// Propagate a parent autonomy-mode change to all active child gates
    /// (ensuring child autonomy never exceeds parent autonomy).
    pub(crate) fn propagate_mode(&self, mode: AutonomyMode) {
        let gates: Vec<ApprovalGate> = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.entries.values().map(|e| e.gate.clone()).collect()
        };
        for gate in gates {
            gate.set_mode(mode);
        }
    }

    /// Kill one individual subagent without cancelling the parent run.
    /// Returns `true` if `subagent_id` existed.
    ///
    /// Worktree cleanup is owned exclusively by the child turn thread so
    /// `git worktree remove` never races mid-flight against an active child.
    pub(crate) fn kill_subagent(&self, subagent_id: &str) -> bool {
        let (control, snapshot) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(entry) = guard.entries.get_mut(subagent_id) else {
                return false;
            };
            entry.killed = true;
            let control = entry.control.clone();
            if entry.status == SubagentStatus::Running {
                entry.status = SubagentStatus::Cancelled;
                entry.finished_at = Some(now_unix_secs());
                if entry.error.is_none() {
                    entry.error = Some("Stopped by user".to_string());
                }
            }
            let snapshot = entry.snapshot(false);
            self.changed.notify_all();
            (control, snapshot)
        };
        control.cancel();
        self.emit_event(AgentRunEvent::SubagentUpdated { subagent: snapshot });
        true
    }

    /// Cancel all running subagents (called when parent run cancels or ends).
    /// Each child turn owns its temporary worktree and cleans it up upon exit.
    pub(crate) fn cancel_all(&self, shutdown: bool) {
        let (controls, snapshots) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut controls = Vec::new();
            let mut snapshots = Vec::new();
            for entry in guard.entries.values_mut() {
                if entry.status == SubagentStatus::Running {
                    entry.killed = true;
                    entry.status = SubagentStatus::Cancelled;
                    entry.finished_at = Some(now_unix_secs());
                    if entry.error.is_none() {
                        entry.error = Some(if shutdown {
                            "Cancelled by application shutdown".to_string()
                        } else {
                            "Cancelled with parent run".to_string()
                        });
                    }
                    controls.push(entry.control.clone());
                    snapshots.push(entry.snapshot(false));
                }
            }
            self.changed.notify_all();
            (controls, snapshots)
        };
        for control in controls {
            if shutdown {
                control.shutdown();
            } else {
                control.cancel();
            }
        }
        for snapshot in snapshots {
            self.emit_event(AgentRunEvent::SubagentUpdated { subagent: snapshot });
        }
    }

    /// Cancel any still-running subagents and join all child worker threads
    /// cleanly (called when the parent run exits). Each joined child thread
    /// cleans up its owned worktree before returning.
    pub(crate) fn shutdown_and_join(&self) {
        self.cancel_all(false);
        let handles = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut guard.threads)
        };
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Dispatch one parent subagent tool call (`spawn_subagent`,
    /// `send_subagent_message`, `wait_subagent`, `kill_subagent`,
    /// `list_subagents`).
    pub(crate) fn dispatch_tool(
        self: &Arc<Self>,
        name: &str,
        arguments: &str,
    ) -> Result<String, AgentError> {
        match name {
            "spawn_subagent" => self.handle_spawn(arguments),
            "send_subagent_message" => self.handle_send_message(arguments),
            "wait_subagent" => self.handle_wait(arguments),
            "kill_subagent" => Ok(self.handle_kill(arguments)),
            "list_subagents" => Ok(self.handle_list()),
            _ => Ok(format!("Error: unknown subagent tool '{name}'")),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_spawn(self: &Arc<Self>, arguments: &str) -> Result<String, AgentError> {
        let args: SpawnArgs = match serde_json::from_str(arguments) {
            Ok(parsed) => parsed,
            Err(err) => return Ok(format!("Error: invalid spawn_subagent arguments: {err}")),
        };
        let task = args.task.trim();
        if task.is_empty() {
            return Ok("Error: spawn_subagent requires a non-empty 'task'".to_string());
        }

        let requested_cap = match args.access.as_deref() {
            None => TaskCapability::ReadOnly,
            Some(raw) => match TaskCapability::parse(raw.trim()) {
                Ok(cap) => cap,
                Err(_) => {
                    return Ok(
                        "Error: invalid 'access'; expected one of: read_only, workspace_write, full_access"
                            .to_string(),
                    );
                }
            },
        };
        let effective_cap = self.effective_capability_for(requested_cap);

        let (model, effort) = match resolve_model_and_effort(
            self.config.db.as_ref(),
            &self.config.provider,
            &self.config.model,
            self.config.effort.as_deref(),
            args.model.as_deref(),
            args.effort.as_deref(),
        ) {
            Ok(pair) => pair,
            Err(err) => return Ok(format!("Error: {err}")),
        };

        let max_turns = args
            .max_turns
            .unwrap_or(DEFAULT_SUBAGENT_MAX_TURNS)
            .clamp(1, MAX_SUBAGENT_MAX_TURNS);
        let timeout_secs = args
            .timeout_secs
            .unwrap_or(DEFAULT_SUBAGENT_TIMEOUT_SECS)
            .clamp(MIN_SUBAGENT_TIMEOUT_SECS, MAX_SUBAGENT_TIMEOUT_SECS);
        let requested_worktree = args.use_worktree.unwrap_or(false);
        let effective_use_worktree = requested_worktree && effective_cap.allows_writes();

        let title = derive_title(args.title.as_deref(), task);

        // Reserve slot & check concurrency atomically. Constructing `child_gate`
        // from `parent_gate.mode()` under `self.state` serializes spawn against
        // `propagate_mode` so a concurrent `agent_set_mode` cannot slip between
        // reading the parent's mode and inserting the child entry.
        let (subagent_id, worktree, snapshot) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = guard.active_count();
            if active >= self.max_concurrency {
                return Ok(format!(
                    "Error: maximum concurrent subagents ({}) reached ({active} active); \
                     wait for or kill an existing subagent before spawning another",
                    self.max_concurrency
                ));
            }
            guard.next_seq += 1;
            let id = format!("sub-{}", guard.next_seq);

            // Create isolated worktree only for writing subagents when requested.
            let worktree = if effective_use_worktree {
                match WorktreeIsolation::create(
                    &self.config.workspace_root,
                    self.config.parent_run_id,
                    &id,
                ) {
                    Ok(wt) => Some(wt),
                    Err(err) => return Ok(format!("Error: {err}")),
                }
            } else {
                None
            };
            let worktree_isolated = worktree.is_some();

            let child_control = RunControl::new();
            let child_access = self
                .config
                .parent_gate
                .access()
                .child(effective_cap.to_access_profile());
            let child_gate = ApprovalGate::new(self.config.parent_gate.mode())
                .with_access(child_access)
                .with_timeout(self.config.parent_gate.timeout());
            child_gate.set_token(child_control.token().clone());

            let entry = SubagentEntry {
                id: id.clone(),
                title,
                status: SubagentStatus::Running,
                model: model.clone(),
                effort: effort.clone(),
                requested_capability: requested_cap,
                effective_capability: effective_cap,
                use_worktree: effective_use_worktree,
                worktree_isolated,
                max_turns,
                timeout_secs,
                steps_taken: 0,
                started_at: now_unix_secs(),
                finished_at: None,
                result: None,
                result_summary: None,
                error: None,
                patch_applied: false,
                patch_conflict: false,
                changed_files: Vec::new(),
                steps: Vec::new(),
                history: Vec::new(),
                control: child_control,
                gate: child_gate,
                killed: false,
                step_seq: Arc::new(AtomicUsize::new(0)),
            };
            let snapshot = entry.snapshot(false);
            guard.order.push(id.clone());
            guard.entries.insert(id.clone(), entry);
            (id, worktree, snapshot)
        };

        self.emit_event(AgentRunEvent::SubagentUpdated { subagent: snapshot });
        self.launch_child_turn(
            &subagent_id,
            task.to_string(),
            Vec::new(),
            max_turns,
            timeout_secs,
            worktree,
        );

        if args.wait.unwrap_or(false) {
            return self.wait_for_ids(&[subagent_id], timeout_secs.saturating_add(15));
        }

        let payload = serde_json::json!({
            "subagent_id": subagent_id,
            "status": "running",
            "model": model,
            "effort": effort,
            "access": effective_cap.as_str(),
            "worktree_isolated": effective_use_worktree,
            "max_turns": max_turns,
            "timeout_secs": timeout_secs,
        });
        Ok(payload.to_string())
    }

    #[allow(clippy::too_many_lines)]
    fn handle_send_message(self: &Arc<Self>, arguments: &str) -> Result<String, AgentError> {
        let args: SendMessageArgs = match serde_json::from_str(arguments) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Ok(format!(
                    "Error: invalid send_subagent_message arguments: {err}"
                ));
            }
        };
        let message = args.message.trim();
        if message.is_empty() {
            return Ok("Error: send_subagent_message requires a non-empty 'message'".to_string());
        }

        let (history, max_turns, timeout_secs, snapshot, worktree) = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = guard.active_count();
            let Some(entry) = guard.entries.get_mut(&args.subagent_id) else {
                return Ok(format!("Error: unknown subagent '{}'", args.subagent_id));
            };
            if entry.killed {
                return Ok(format!(
                    "Error: subagent '{}' was killed and cannot be resumed",
                    args.subagent_id
                ));
            }
            if entry.status == SubagentStatus::Running {
                return Ok(format!(
                    "Error: subagent '{}' is still running; call wait_subagent first",
                    args.subagent_id
                ));
            }
            if active >= self.max_concurrency {
                return Ok(format!(
                    "Error: maximum concurrent subagents ({}) reached ({active} active)",
                    self.max_concurrency
                ));
            }

            // Re-evaluate live parent access ceiling before resuming.
            let effective_cap = self.effective_capability_for(entry.requested_capability);
            let effective_use_worktree = entry.use_worktree && effective_cap.allows_writes();
            let worktree = if effective_use_worktree {
                match WorktreeIsolation::create(
                    &self.config.workspace_root,
                    self.config.parent_run_id,
                    &entry.id,
                ) {
                    Ok(wt) => Some(wt),
                    Err(err) => return Ok(format!("Error: {err}")),
                }
            } else {
                None
            };

            let child_control = RunControl::new();
            let child_access = self
                .config
                .parent_gate
                .access()
                .child(effective_cap.to_access_profile());
            let child_gate = ApprovalGate::new(self.config.parent_gate.mode())
                .with_access(child_access)
                .with_timeout(self.config.parent_gate.timeout());
            child_gate.set_token(child_control.token().clone());

            let max_turns = args
                .max_turns
                .unwrap_or(entry.max_turns)
                .clamp(1, MAX_SUBAGENT_MAX_TURNS);
            let timeout_secs = args
                .timeout_secs
                .unwrap_or(entry.timeout_secs)
                .clamp(MIN_SUBAGENT_TIMEOUT_SECS, MAX_SUBAGENT_TIMEOUT_SECS);

            entry.effective_capability = effective_cap;
            entry.worktree_isolated = worktree.is_some();
            entry.control = child_control;
            entry.gate = child_gate;
            entry.status = SubagentStatus::Running;
            entry.finished_at = None;
            entry.result = None;
            entry.result_summary = None;
            entry.error = None;
            entry.patch_applied = false;
            entry.patch_conflict = false;
            entry.changed_files.clear();
            entry.max_turns = max_turns;
            entry.timeout_secs = timeout_secs;

            let history = entry.history.clone();
            let snapshot = entry.snapshot(false);
            (history, max_turns, timeout_secs, snapshot, worktree)
        };

        self.emit_event(AgentRunEvent::SubagentUpdated { subagent: snapshot });
        self.launch_child_turn(
            &args.subagent_id,
            message.to_string(),
            history,
            max_turns,
            timeout_secs,
            worktree,
        );

        if args.wait.unwrap_or(true) {
            return self.wait_for_ids(&[args.subagent_id], timeout_secs.saturating_add(15));
        }
        Ok(serde_json::json!({
            "subagent_id": args.subagent_id,
            "status": "running",
        })
        .to_string())
    }

    fn handle_wait(&self, arguments: &str) -> Result<String, AgentError> {
        let args: WaitArgs = if arguments.trim().is_empty() {
            WaitArgs {
                subagent_id: None,
                subagent_ids: None,
                timeout_secs: None,
            }
        } else {
            match serde_json::from_str(arguments) {
                Ok(parsed) => parsed,
                Err(err) => return Ok(format!("Error: invalid wait_subagent arguments: {err}")),
            }
        };

        let target_ids: Vec<String> = if let Some(id) = args.subagent_id {
            vec![id]
        } else if let Some(ids) = args.subagent_ids {
            ids
        } else {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .order
                .iter()
                .filter(|id| {
                    guard
                        .entries
                        .get(*id)
                        .is_some_and(|e| e.status == SubagentStatus::Running)
                })
                .cloned()
                .collect()
        };

        if target_ids.is_empty() {
            return Ok(serde_json::json!({ "subagents": [] }).to_string());
        }

        let wait_secs = args
            .timeout_secs
            .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS)
            .clamp(1, MAX_SUBAGENT_TIMEOUT_SECS);
        self.wait_for_ids(&target_ids, wait_secs)
    }

    fn wait_for_ids(&self, target_ids: &[String], timeout_secs: u64) -> Result<String, AgentError> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        for id in target_ids {
            if !guard.entries.contains_key(id) {
                return Ok(format!("Error: unknown subagent '{id}'"));
            }
        }

        loop {
            if self.config.parent_control.is_cancelled() {
                drop(guard);
                self.cancel_all(self.config.parent_control.token().is_shutdown());
                return Err(AgentError::Cancelled);
            }
            let all_done = target_ids
                .iter()
                .all(|id| guard.entries.get(id).is_none_or(|e| e.status.is_terminal()));
            if all_done {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let slice = (deadline - now).min(Duration::from_millis(50));
            let (next_guard, _) = self
                .changed
                .wait_timeout(guard, slice)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next_guard;
        }

        let results: Vec<serde_json::Value> = target_ids
            .iter()
            .filter_map(|id| {
                let entry = guard.entries.get(id)?;
                Some(serde_json::json!({
                    "subagent_id": entry.id,
                    "title": entry.title,
                    "status": entry.status.as_str(),
                    "model": entry.model,
                    "effort": entry.effort,
                    "access": entry.effective_capability.as_str(),
                    "worktree_isolated": entry.worktree_isolated,
                    "steps_taken": entry.steps_taken,
                    "result": entry.result,
                    "error": entry.error,
                    "patch_applied": entry.patch_applied,
                    "patch_conflict": entry.patch_conflict,
                    "changed_files": entry.changed_files,
                }))
            })
            .collect();

        Ok(serde_json::json!({ "subagents": results }).to_string())
    }

    fn handle_kill(&self, arguments: &str) -> String {
        let args: KillArgs = match serde_json::from_str(arguments) {
            Ok(parsed) => parsed,
            Err(err) => return format!("Error: invalid kill_subagent arguments: {err}"),
        };
        if self.kill_subagent(&args.subagent_id) {
            serde_json::json!({
                "subagent_id": args.subagent_id,
                "killed": true,
                "status": "cancelled",
            })
            .to_string()
        } else {
            format!("Error: unknown subagent '{}'", args.subagent_id)
        }
    }

    fn handle_list(&self) -> String {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active_count = guard.active_count();
        let items: Vec<SubagentSnapshot> = guard
            .order
            .iter()
            .filter_map(|id| guard.entries.get(id).map(|e| e.snapshot(false)))
            .collect();
        serde_json::json!({
            "max_concurrency": self.max_concurrency,
            "active_count": active_count,
            "subagents": items,
        })
        .to_string()
    }

    fn effective_capability_for(&self, requested: TaskCapability) -> TaskCapability {
        if self.config.parent_is_plan {
            return TaskCapability::ReadOnly;
        }
        let live_profile = self.config.parent_gate.access().profile();
        let mut effective = requested.cap_by_access_profile(live_profile);
        if self.config.parent_preset == RunPreset::Document
            && effective == TaskCapability::FullAccess
        {
            effective = TaskCapability::WorkspaceWrite;
        }
        effective
    }

    fn emit_event(&self, event: AgentRunEvent) {
        if let Some(tx) = &self.config.event_sender {
            let _ = tx.send(event);
        }
    }

    fn launch_child_turn(
        self: &Arc<Self>,
        subagent_id: &str,
        prompt: String,
        prior_history: Vec<AiMessage>,
        max_turns: usize,
        timeout_secs: u64,
        worktree: Option<WorktreeIsolation>,
    ) {
        let Some((model, effort, effective_cap, child_control, child_gate, step_seq, aborted)) = ({
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.entries.get(subagent_id).map(|entry| {
                (
                    entry.model.clone(),
                    entry.effort.clone(),
                    entry.effective_capability,
                    entry.control.clone(),
                    entry.gate.clone(),
                    Arc::clone(&entry.step_seq),
                    entry.killed || entry.control.is_cancelled(),
                )
            })
        }) else {
            if let Some(wt) = worktree {
                wt.cleanup();
            }
            return;
        };

        if aborted {
            if let Some(wt) = worktree {
                wt.cleanup();
            }
            return;
        }

        let mgr = Arc::clone(self);
        let id_owned = subagent_id.to_string();
        let handle = std::thread::Builder::new()
            .name(format!(
                "subagent-{}-{subagent_id}",
                self.config.parent_run_id
            ))
            .spawn(move || {
                mgr.run_child_turn(
                    &id_owned,
                    &prompt,
                    prior_history,
                    &model,
                    effort.as_deref(),
                    effective_cap,
                    &child_control,
                    child_gate,
                    step_seq,
                    worktree,
                    max_turns,
                    timeout_secs,
                );
            });

        if let Ok(join_handle) = handle {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.threads.push(join_handle);
        } else {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = guard.entries.get_mut(subagent_id) {
                entry.status = SubagentStatus::Failed;
                entry.finished_at = Some(now_unix_secs());
                entry.error = Some("Failed to spawn subagent worker thread".to_string());
                let snap = entry.snapshot(false);
                self.changed.notify_all();
                drop(guard);
                self.emit_event(AgentRunEvent::SubagentUpdated { subagent: snap });
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn run_child_turn(
        self: &Arc<Self>,
        subagent_id: &str,
        prompt: &str,
        prior_history: Vec<AiMessage>,
        model: &str,
        effort: Option<&str>,
        effective_cap: TaskCapability,
        child_control: &RunControl,
        child_gate: ApprovalGate,
        step_seq: Arc<AtomicUsize>,
        worktree: Option<WorktreeIsolation>,
        max_turns: usize,
        timeout_secs: u64,
    ) {
        let child_workspace: PathBuf = worktree.as_ref().map_or_else(
            || self.config.workspace_root.clone(),
            |wt| wt.workspace_path().to_path_buf(),
        );
        // Condvar-backed watchdog so timeout cancels the child promptly and
        // exits immediately when the child completes early.
        let done_signal = Arc::new((Mutex::new(false), Condvar::new()));
        let timed_out = Arc::new(AtomicBool::new(false));
        let watchdog_handle = {
            let signal = Arc::clone(&done_signal);
            let timed_out_flag = Arc::clone(&timed_out);
            let cancel_control = child_control.clone();
            let parent_control = self.config.parent_control.clone();
            std::thread::Builder::new()
                .name(format!("subagent-watch-{subagent_id}"))
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                    let (lock, cvar) = &*signal;
                    let mut finished = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while !*finished {
                        if parent_control.is_cancelled() {
                            cancel_control.cancel();
                            break;
                        }
                        let now = Instant::now();
                        if now >= deadline {
                            timed_out_flag.store(true, Ordering::SeqCst);
                            cancel_control.cancel();
                            break;
                        }
                        let slice = (deadline - now).min(Duration::from_millis(50));
                        let (next, _) = cvar
                            .wait_timeout(finished, slice)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        finished = next;
                    }
                })
                .ok()
        };

        // Isolated child event channel: forwards ONLY approvals and
        // SubagentStep records to the parent event stream — never leaks child
        // Completed/Cancelled/StepRecorded frames onto the parent run.
        let (child_tx, child_rx) = mpsc::channel::<AgentRunEvent>();
        let forwarder_handle = {
            let mgr = Arc::clone(self);
            let id_copy = subagent_id.to_string();
            std::thread::Builder::new()
                .name(format!("subagent-fwd-{subagent_id}"))
                .spawn(move || {
                    while let Ok(ev) = child_rx.recv() {
                        match ev {
                            AgentRunEvent::ApprovalRequested { .. }
                            | AgentRunEvent::ApprovalResolved { .. } => {
                                mgr.emit_event(ev);
                            }
                            AgentRunEvent::SubagentStep {
                                subagent_id: ref sid,
                                seq,
                                ref kind,
                                ref tool_name,
                                ref target,
                                ref status,
                                ref observation,
                            } if sid == &id_copy => {
                                let snap = {
                                    let mut guard = mgr
                                        .state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    if let Some(entry) = guard.entries.get_mut(&id_copy) {
                                        if kind == "model_turn" {
                                            entry.steps_taken += 1;
                                        }
                                        entry.steps.push(SubagentStepRecord {
                                            seq,
                                            kind: kind.clone(),
                                            tool_name: tool_name.clone(),
                                            target: target.clone(),
                                            status: status.clone(),
                                            observation: observation.clone(),
                                        });
                                        Some(entry.snapshot(false))
                                    } else {
                                        None
                                    }
                                };
                                mgr.emit_event(ev);
                                if let Some(subagent) = snap {
                                    mgr.emit_event(AgentRunEvent::SubagentUpdated { subagent });
                                }
                            }
                            _ => {}
                        }
                    }
                })
                .ok()
        };

        let history_sink = Arc::new(Mutex::new(Vec::<AiMessage>::new()));
        // Keep the parent's RunPreset unchanged so `coding`-scoped Deny/Ask
        // permission rules remain enforced on `ReadOnly` / `WorkspaceWrite`
        // subagents; disallowed tools are hidden via `ToolRegistry` and denied
        // structurally via `TaskCapability`.
        let child_preset = self.config.parent_preset;
        let mut runner = AgentRunner::new(self.config.executor.as_ref(), &child_workspace)
            .with_plan(effective_cap == TaskCapability::ReadOnly)
            .with_capability(effective_cap)
            .with_subagent_depth(MAX_SUBAGENT_DEPTH)
            .with_subagent_id(subagent_id.to_string())
            .with_subagent_step_seq(step_seq)
            .with_history_sink(Arc::clone(&history_sink))
            .with_max_iterations(max_turns)
            .with_control(child_control.clone())
            .with_approval_gate(child_gate)
            .with_preset(child_preset)
            .with_history(prior_history)
            .with_run_flags(self.config.run_flags)
            .with_event_sender(child_tx);

        if let Some(store) = &self.config.permission_store {
            runner = runner.with_permission_store(store.clone());
        }
        if let Some(instructions) = &self.config.project_instructions {
            runner = runner.with_project_instructions(instructions.clone());
        }
        if let Some(eff) = effort {
            runner = runner
                .with_model_config(ModelConfig::new(model).with_effort(Some(eff.to_string())));
        }

        let outcome = runner.run(
            &self.config.provider,
            model,
            &self.config.credential,
            prompt,
        );
        // Drop `runner` so `child_tx` disconnects and the forwarder drains cleanly.
        drop(runner);

        // Stop watchdog promptly.
        {
            let (lock, cvar) = &*done_signal;
            let mut finished = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *finished = true;
            cvar.notify_all();
        }
        if let Some(h) = watchdog_handle {
            let _ = h.join();
        }
        if let Some(h) = forwarder_handle {
            let _ = h.join();
        }

        // Transfer isolated worktree patch ONLY on clean Ok completion (never on
        // cancel/timeout/kill/error), then unconditionally clean up worktree.
        let was_timed_out = timed_out.load(Ordering::SeqCst);
        let mut patch_applied = false;
        let mut patch_conflict = false;
        let mut patch_error: Option<String> = None;
        let mut changed_files = Vec::new();

        let is_killed = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.entries.get(subagent_id).is_none_or(|e| e.killed)
        };

        if let Some(wt) = worktree {
            if outcome.is_ok()
                && !was_timed_out
                && !is_killed
                && !child_control.is_cancelled()
                && !self.config.parent_control.is_cancelled()
            {
                match wt.apply_to_parent(&self.patch_apply_lock) {
                    PatchTransferOutcome::NoChanges => {}
                    PatchTransferOutcome::Applied {
                        changed_files: files,
                    } => {
                        patch_applied = true;
                        changed_files = files;
                    }
                    PatchTransferOutcome::Conflict {
                        message,
                        changed_files: files,
                    } => {
                        patch_conflict = true;
                        patch_error = Some(message);
                        changed_files = files;
                    }
                }
            }
            wt.cleanup();
        }

        let captured_history = history_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        let final_snapshot = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let snap = if let Some(entry) = guard.entries.get_mut(subagent_id) {
                if !captured_history.is_empty() {
                    entry.history = captured_history;
                }
                entry.patch_applied = patch_applied;
                entry.patch_conflict = patch_conflict;
                entry.changed_files = changed_files;
                entry.finished_at = Some(now_unix_secs());

                if entry.killed {
                    entry.status = SubagentStatus::Cancelled;
                    if entry.error.is_none() {
                        entry.error = Some("Stopped".to_string());
                    }
                } else if was_timed_out {
                    entry.status = SubagentStatus::TimedOut;
                    entry.error = Some(format!("Subagent timed out after {timeout_secs}s"));
                } else {
                    match outcome {
                        Ok(text) => {
                            let (redacted, _) = super::injection::redact_secrets(&text);
                            let (preview, _, _) = step_preview_opt(Some(&redacted));
                            entry.result = Some(redacted);
                            entry.result_summary = preview;
                            if patch_conflict {
                                entry.status = SubagentStatus::Failed;
                                entry.error = patch_error;
                            } else {
                                entry.status = SubagentStatus::Completed;
                            }
                        }
                        Err(AgentError::Cancelled) => {
                            entry.status = SubagentStatus::Cancelled;
                            if entry.error.is_none() {
                                entry.error = Some("Subagent cancelled".to_string());
                            }
                        }
                        Err(AgentError::BudgetExhausted(max_iterations)) => {
                            entry.status = SubagentStatus::Failed;
                            entry.error = Some(format!(
                                "Subagent reached max_turns limit ({max_iterations})"
                            ));
                        }
                        Err(err) => {
                            entry.status = SubagentStatus::Failed;
                            entry.error = Some(err.to_string());
                        }
                    }
                }
                Some(entry.snapshot(false))
            } else {
                None
            };
            self.changed.notify_all();
            snap
        };

        if let Some(subagent) = final_snapshot {
            self.emit_event(AgentRunEvent::SubagentUpdated { subagent });
        }
    }
}

impl Drop for SubagentManager {
    fn drop(&mut self) {
        self.cancel_all(false);
    }
}

/// Resolve the subagent's `(model, effort)` strictly defaulting to the parent
/// model (NEVER auto-escalating to a pricier model) and validating any
/// explicit `effort` override against the `M06` catalog's `supported_efforts`.
pub(crate) fn resolve_model_and_effort(
    db: Option<&Database>,
    provider: &str,
    parent_model: &str,
    parent_effort: Option<&str>,
    requested_model: Option<&str>,
    requested_effort: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let model = match requested_model.map(str::trim).filter(|m| !m.is_empty()) {
        None => parent_model.to_string(),
        Some(explicit) => {
            if explicit.len() > nexora_core::config::MAX_MODEL_LEN
                || explicit.chars().any(char::is_control)
            {
                return Err("invalid subagent model override".to_string());
            }
            explicit.to_string()
        }
    };

    let caps = resolve_model_capabilities_opt_db(db, provider, &model);
    let effort = match requested_effort.map(str::trim).filter(|e| !e.is_empty()) {
        Some(raw) => {
            let parsed = ReasoningEffort::parse(raw).ok_or_else(|| {
                format!("invalid effort '{raw}'; expected one of: low, medium, high, xhigh")
            })?;
            if caps.supported_efforts.is_empty() {
                return Err(format!(
                    "model '{model}' does not support reasoning effort overrides"
                ));
            }
            if !caps
                .supported_efforts
                .iter()
                .any(|supported| supported == parsed.as_str())
            {
                return Err(format!(
                    "effort '{}' is not supported by model '{}' (supported: {})",
                    parsed.as_str(),
                    model,
                    caps.supported_efforts.join(", ")
                ));
            }
            Some(parsed.as_str().to_string())
        }
        None => parent_effort.and_then(|pe| {
            let parsed = ReasoningEffort::parse(pe)?;
            caps.supported_efforts
                .iter()
                .any(|supported| supported == parsed.as_str())
                .then(|| parsed.as_str().to_string())
        }),
    };

    Ok((model, effort))
}

fn derive_title(explicit: Option<&str>, task: &str) -> String {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return truncate_chars(raw, 80);
    }
    let first_line = task.lines().next().unwrap_or(task).trim();
    truncate_chars(first_line, 72)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
