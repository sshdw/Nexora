//! Agent-run IPC commands (Task 5.1): the Tauri side of the run bridge.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves managed
//! state, delegates to the application-layer bridge
//! ([`crate::application::agent::service`]), and maps classified errors into
//! secret-free [`CommandError`] values ([`super::error`] doctrine — no
//! credentials, raw SQL, or message payloads in error text).
//!
//! # Secrets
//!
//! `start_agent_run` resolves the provider credential *inside the backend*
//! via the existing [`RequestExecutionService::resolve_credential`] path
//! (the same one plain chat's `execute` performs) and moves it straight into
//! the spawned run thread. It never crosses IPC, is never serialized, logged,
//! or placed in a [`RunFrame`], and is dropped when the thread ends.
//!
//! # Threading
//!
//! `start_agent_run` performs fast local work only (`SQLite` + keyring reads +
//! thread spawn) and returns `{ run_id }` immediately; everything long-lived
//! runs on the dedicated run/forwarder threads owned by the registry
//! (design §2.1). The synchronous setup is moved onto the runtime's blocking
//! pool exactly like `send_message` (BUG-005 doctrine,
//! [`super::conversations::send_message`]).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::application::agent::approval::AutonomyMode;
use crate::application::agent::permissions::{self, PermissionRule, RuleEffect};
use crate::application::agent::service::{
    self, AgentRunError, AgentRunHost, AgentRunRegistry, AgentRunRequest, ResolveOutcome, RunFrame,
};
use crate::application::conversations::ConversationService;
use crate::application::execution::{ExecutorRegistry, RequestError, RequestExecutionService};
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::agent_runs::{AgentRun, AgentStep};

use super::error::{CommandError, ErrorKind};

/// Managed registry state is an [`Arc`] so commands can clone an owned handle
/// into `spawn_blocking`/the run bridge without borrowing the managed value.
pub(crate) type ManagedRegistry = Arc<AgentRunRegistry>;

/// The shell side of the bridge (design §2.3): emits `agent-run-event` frames
/// through the `AppHandle` and persists the assistant message through the
/// same [`ConversationService`] path as plain chat (DP-7). The bridge itself
/// never names a Tauri type.
pub(crate) struct TauriAgentHost {
    app: AppHandle,
    db: Database,
}

impl AgentRunHost for TauriAgentHost {
    fn emit(&self, frame: &RunFrame) {
        let payload = frame.clone();
        if let Err(err) = self.app.emit("agent-run-event", payload) {
            // Best-effort, exactly like the recorder: a missing/unreachable
            // frontend listener must never affect the run.
            log::warn!("agent run bridge: frame emission failed: {err}");
        }
    }

    fn persist_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    ) {
        let outcome = ConversationService::new(&self.db).persist_assistant_message(
            conversation_id,
            content,
            provider,
            model,
        );
        if let Err(err) = outcome {
            // Best-effort: the final answer remains available on the
            // `agent_runs` row and in the stream.
            log::warn!("agent run bridge: assistant persistence failed: {err}");
        }
    }
}

impl TauriAgentHost {
    fn new(app: AppHandle, db: Database) -> Self {
        Self { app, db }
    }
}

/// The per-run agent workspace root: the stored `agent.workspace_root`
/// setting when it names an existing directory, else the default
/// `agent_workspace` subdirectory of the app-data dir (the pre-picker
/// behavior). Created on demand. Anchor: `commands/agent.rs::workspace_root`.
fn workspace_root(app: &AppHandle, db: &Database) -> Result<PathBuf, CommandError> {
    let base = app.path().app_data_dir().map_err(|err| {
        CommandError::new(
            ErrorKind::Io,
            format!("the application data directory is unavailable: {err}"),
        )
    })?;
    let fallback = base.join("agent_workspace");
    let resolved = crate::application::workspace::resolve_workspace_root(db, &fallback);
    if resolved != fallback {
        return Ok(resolved);
    }
    std::fs::create_dir_all(&fallback).map_err(|_| {
        CommandError::new(ErrorKind::Io, "the agent workspace could not be created")
    })?;
    Ok(fallback)
}

/// Response returned immediately by [`start_agent_run`]: the run is fully
/// owned by the registry and its outcome flows exclusively through the
/// `agent-run-event` stream.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct StartAgentRunResponse {
    pub run_id: i64,
}

/// Start one opt-in agent run for `conversation_id` (design §3): resolve the
/// provider/credential backend-side, persist the user message, create the
/// linked `agent_runs` row, register the run, and spawn the run + forwarder
/// threads. Returns `{ run_id }` immediately; the run's outcome flows
/// exclusively through the `agent-run-event` stream.
///
/// # Errors
///
/// Classified [`CommandError`]s for unknown conversations, an already-active
/// run in the same conversation (DP-4), unknown/missing-credential providers
/// (FR-014), or persistence/thread failures. No secrets ever cross IPC.
#[tauri::command]
pub(crate) async fn start_agent_run(
    app: AppHandle,
    conversation_id: i64,
    content: String,
    provider: String,
    model: String,
) -> Result<StartAgentRunResponse, CommandError> {
    if content.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "the user message must not be empty",
        ));
    }
    if provider.trim().is_empty() || model.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "a provider and model are required",
        ));
    }

    // Owned handle so managed state can be reached from the blocking thread
    // (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let registry = handle.state::<ManagedRegistry>();
        let db_client = db.inner().clone();
        let db_ref = &db_client;
        // Materialize the registry handle before `handle` moves below.
        let registry_arc = Arc::clone(registry.inner());

        // 1. Resolve the credential inside the backend (FR-014 path shared
        //    with plain chat). The value lives only inside the run thread.
        let credential = RequestExecutionService::new(db_ref)
            .resolve_credential(&provider)
            .map_err(CommandError::from)?;

        // 2. Resolve the provider executor (no fallback; same registry plain
        //    chat uses).
        let executor = ExecutorRegistry::new()
            .resolve_owned(&provider)
            .ok_or_else(|| {
                CommandError::from(RequestError::ExecutorUnavailable {
                    name: provider.clone(),
                })
            })?;

        let root = workspace_root(&handle, db_ref)?;
        let host: Arc<dyn AgentRunHost> = Arc::new(TauriAgentHost::new(handle, db_client.clone()));
        // Resolve autonomy mode from settings (DP-AUTONOMY): default
        // semi_autonomous when unset/invalid.
        let mode = service::resolve_autonomy_mode(db_ref);
        // Resolve run preset from settings (T5): default coding when
        // unset/invalid. Document runs hide the shell from the schema and
        // deny it structurally on dispatch.
        let preset = service::resolve_preset(db_ref);

        let run_id = service::start_run(
            db_ref,
            registry_arc,
            host,
            executor,
            root,
            AgentRunRequest {
                conversation_id,
                user_request: content,
                provider,
                model,
                credential,
                max_iterations: None,
                spend_limit_micro_usd: service::resolve_spend_limit(db_ref),
            },
            mode,
            preset,
        )?;
        Ok::<StartAgentRunResponse, CommandError>(StartAgentRunResponse { run_id })
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("start_agent_run blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the agent run could not be started",
            ))
        }
    }
}

/// Cancel an active run. Works from *every* state — running, approval-parked,
/// or budget-parked — because `RunControl::cancel` wakes all parked waits
/// (DP-3). A run that already finished simply misses (`NotFound` mapping).
#[tauri::command]
pub(crate) fn cancel_agent_run(
    run_id: i64,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    if registry.cancel(run_id) {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        ))
    }
}

/// Resolve a parked approval (5.1 minimum; 5.2 polishes the UX).
///
/// M1-core `scope`: `None`/`"single"` resolves one call; `"group"` records
/// the verdict as the session-sticky decision for the call's group and, for
/// approved non-shell groups, persists an `allow` rule (`priority=200`).
#[tauri::command]
pub(crate) fn resolve_agent_approval(
    run_id: i64,
    call_id: String,
    approved: bool,
    scope: Option<String>,
    registry: State<'_, ManagedRegistry>,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    let scope_ref = scope.as_deref();
    if let Some(value) = scope_ref {
        if value != "single" && value != "group" {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                "scope must be 'single' or 'group'",
            ));
        }
    }
    let (outcome, pending) = registry.resolve_with_scope(run_id, &call_id, approved, scope_ref);
    match outcome {
        ResolveOutcome::RunNotActive => Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        )),
        ResolveOutcome::NoPendingApproval => Err(CommandError::new(
            ErrorKind::NotFound,
            "the run has no pending approval for that call",
        )),
        ResolveOutcome::Resolved => {
            // M1-core persistent "never ask for this pattern": group-scope
            // approvals for non-shell tools persist an allow rule. Shell
            // groups never persist (they always park except FullAutonomous).
            if scope_ref == Some("group") && approved {
                if let Some((tool_name, group_key)) = pending {
                    if tool_name != "execute_command" {
                        let path_pattern = group_path_pattern(group_key.as_deref());
                        let _ = insert_group_allow_rule(
                            db.inner(),
                            group_preset(group_key.as_deref()),
                            &tool_name,
                            path_pattern.as_deref(),
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

/// Extract the rule `preset` from a `preset:tool:path` group key (T5: group
/// keys are preset-scoped, so a document run's group approval persists a
/// document-scoped rule). Unknown shapes fall back to `"coding"`, the
/// pre-T5 behavior.
fn group_preset(group_key: Option<&str>) -> &str {
    match group_key.and_then(|key| key.split(':').next()) {
        Some("document") => "document",
        _ => "coding",
    }
}

/// Extract the rule `path_pattern` from a `preset:tool:path` group key.
/// `*` path groups persist as `*` (match-any); otherwise the parent dir.
fn group_path_pattern(group_key: Option<&str>) -> Option<String> {
    let key = group_key?;
    let mut parts = key.splitn(3, ':');
    let _preset = parts.next()?;
    let _tool = parts.next()?;
    let path = parts.next()?;
    if path == "*" || path.is_empty() {
        Some("*".to_string())
    } else {
        Some(path.to_string())
    }
}

/// Best-effort insert of the group-allow rule (`effect='allow'`,
/// `priority=200`). Failures (e.g. duplicate) are ignored: the session-sticky
/// verdict already governs this run. Shell tools never persist (defense in
/// depth alongside the caller's guard): shell groups always park except
/// under `FullAutonomous`.
fn insert_group_allow_rule(
    db: &Database,
    preset: &str,
    tool_name: &str,
    path_pattern: Option<&str>,
) -> bool {
    if tool_name == "execute_command" || tool_name == "*" {
        return false;
    }
    let pattern = path_pattern.unwrap_or("*");
    permissions::insert_rule(db, preset, tool_name, Some(pattern), RuleEffect::Allow, 200).is_ok()
}

/// Grant `extra_steps` further iterations to a budget-parked (or running)
/// run — the "Continue" affordance (design §5).
#[tauri::command]
pub(crate) fn extend_agent_run(
    run_id: i64,
    extra_steps: u32,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    if extra_steps == 0 {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "extra steps must be greater than zero",
        ));
    }
    if registry.extend(run_id, extra_steps as usize) {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        ))
    }
}

/// Live-switch the autonomy mode of an active run (Task 5.2, DP-AUTONOMY).
/// A parked approval is never auto-resolved by a mode switch.
#[tauri::command]
pub(crate) fn agent_set_mode(
    run_id: i64,
    mode: String,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    let mode = match mode.as_str() {
        "supervised" => AutonomyMode::Supervised,
        "semi_autonomous" => AutonomyMode::SemiAutonomous,
        "full_autonomous" => AutonomyMode::FullAutonomous,
        _ => {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                format!("value '{mode}' is not a valid 'agent.autonomy' setting"),
            ))
        }
    };
    if registry.set_mode(run_id, mode) {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        ))
    }
}

/// Pause an active run (Task 5.2, DP-PAUSE). Takes effect at the next step
/// boundary; `resume` or `cancel` ends it.
#[tauri::command]
pub(crate) fn pause_agent_run(
    run_id: i64,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    if registry.pause(run_id) {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        ))
    }
}

/// Resume a paused run (Task 5.2, DP-PAUSE).
#[tauri::command]
pub(crate) fn resume_agent_run(
    run_id: i64,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    if registry.resume(run_id) {
        Ok(())
    } else {
        Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        ))
    }
}

/// Rehydration: the runs of one conversation, `started_at` DESC.
#[tauri::command]
pub(crate) fn list_agent_runs(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AgentRun>, CommandError> {
    service::list_runs_for_conversation(db.inner(), conversation_id).map_err(Into::into)
}

/// Rehydration: the steps of one run, `seq` ASC.
#[tauri::command]
pub(crate) fn list_agent_steps(
    run_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<AgentStep>, CommandError> {
    service::list_steps_for_run(db.inner(), run_id).map_err(Into::into)
}

/// Add one persistent permission rule (M1-core).
///
/// `preset` is `coding`/`document`/`*`; `tool_pattern` is a tool name or `*`
/// (1..64 chars); `path_pattern` is `None` (no path dimension) or 1..1024
/// chars (`*` = match-any); `effect` is `allow`/`ask`/`deny`. Persistent
/// `allow` for `execute_command` (or `*`, which includes the shell) is
/// rejected: shell groups always park except under `FullAutonomous`.
#[tauri::command]
pub(crate) fn add_permission_rule(
    preset: String,
    tool_pattern: String,
    path_pattern: Option<String>,
    effect: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    validate_new_rule(&preset, &tool_pattern, path_pattern.as_deref(), &effect)?;
    let rule_effect = match effect.as_str() {
        "allow" => RuleEffect::Allow,
        "ask" => RuleEffect::Ask,
        "deny" => RuleEffect::Deny,
        _ => {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                "effect must be 'allow', 'ask', or 'deny'",
            ));
        }
    };
    match permissions::insert_rule(
        db.inner(),
        &preset,
        &tool_pattern,
        path_pattern.as_deref(),
        rule_effect,
        100,
    ) {
        Ok(id) => Ok(id),
        Err(err) => {
            log::warn!("add_permission_rule insert failed: {err}");
            Err(CommandError::new(
                ErrorKind::InvalidInput,
                "the permission rule could not be added",
            ))
        }
    }
}

/// Validate a new rule against the v7 CHECKs plus the shell-allow ban.
/// Never echoes argument content beyond the fixed vocabularies.
fn validate_new_rule(
    preset: &str,
    tool_pattern: &str,
    path_pattern: Option<&str>,
    effect: &str,
) -> Result<(), CommandError> {
    if !matches!(preset, "coding" | "document" | "*") {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "preset must be 'coding', 'document', or '*'",
        ));
    }
    if tool_pattern.is_empty() || tool_pattern.len() > 64 {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "tool pattern must be 1..64 characters",
        ));
    }
    if let Some(path) = path_pattern {
        if path.is_empty() || path.len() > 1024 {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                "path pattern must be 1..1024 characters",
            ));
        }
    }
    if !matches!(effect, "allow" | "ask" | "deny") {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "effect must be 'allow', 'ask', or 'deny'",
        ));
    }
    if effect == "allow" && (tool_pattern == "execute_command" || tool_pattern == "*") {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "persistent allow is not available for shell commands",
        ));
    }
    Ok(())
}

/// Remove one persistent permission rule by id (M1-core).
#[tauri::command]
pub(crate) fn remove_permission_rule(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    match permissions::delete_rule(db.inner(), id) {
        Ok(true) => Ok(()),
        Ok(false) => Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no permission rule with id {id}"),
        )),
        Err(err) => Err(CommandError::from(err)),
    }
}

/// List persistent permission rules ordered `priority ASC, id ASC` (M1-core).
#[tauri::command]
pub(crate) fn list_permission_rules(
    db: State<'_, Database>,
) -> Result<Vec<PermissionRule>, CommandError> {
    permissions::list_rules(db.inner()).map_err(CommandError::from)
}

impl From<AgentRunError> for CommandError {
    fn from(err: AgentRunError) -> Self {
        match err {
            AgentRunError::ConversationNotFound { id } => Self::new(
                ErrorKind::NotFound,
                format!("conversation {id} does not exist"),
            ),
            AgentRunError::RunAlreadyActive { conversation_id } => Self::new(
                ErrorKind::InvalidInput,
                format!("an agent run is already active for conversation {conversation_id}"),
            ),
            AgentRunError::Request(inner) => Self::from(inner),
            AgentRunError::RunNotPersisted => {
                Self::new(ErrorKind::Database, "the agent run could not be persisted")
            }
            AgentRunError::ThreadSpawn(_) => {
                Self::new(ErrorKind::Request, "the agent run could not be started")
            }
            AgentRunError::Database(inner) => Self::from(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::service::AgentRunError as ServiceError;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    /// Negative-control: the command mapping must never surface a secret, even
    /// when the source error carries raw provider output (design §3.4).
    #[test]
    fn agent_run_error_mapping_is_secret_free() {
        let cases = [
            ServiceError::ConversationNotFound { id: 7 },
            ServiceError::RunAlreadyActive { conversation_id: 7 },
            ServiceError::Request(RequestError::UnknownProvider {
                name: "nope".into(),
            }),
            ServiceError::RunNotPersisted,
            ServiceError::ThreadSpawn("boom: sk-12ab34cd".into()),
            ServiceError::Database(crate::infrastructure::database::DatabaseError::Lock(
                "sk-".into(),
            )),
        ];
        for case in cases {
            let mapped: CommandError = case.into();
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }

    #[test]
    fn new_agent_commands_are_secret_free_and_classified() {
        // agent_set_mode invalid mode -> InvalidInput, secret-free
        let invalid = CommandError::new(
            ErrorKind::InvalidInput,
            format!(
                "value '{}' is not a valid 'agent.autonomy' setting",
                "bad_mode"
            ),
        );
        assert_eq!(invalid.kind, ErrorKind::InvalidInput);
        assert!(safe_message(&invalid));

        // pause/resume/set_mode unknown run -> NotFound, secret-free
        let not_found = CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {}", 9999),
        );
        assert_eq!(not_found.kind, ErrorKind::NotFound);
        assert!(safe_message(&not_found));

        // resolve unknown approval -> NotFound
        let no_approval = CommandError::new(
            ErrorKind::NotFound,
            "the run has no pending approval for that call".to_string(),
        );
        assert_eq!(no_approval.kind, ErrorKind::NotFound);
        assert!(safe_message(&no_approval));

        // extend zero steps -> InvalidInput
        let bad_extend = CommandError::new(
            ErrorKind::InvalidInput,
            "extra steps must be greater than zero".to_string(),
        );
        assert_eq!(bad_extend.kind, ErrorKind::InvalidInput);
        assert!(safe_message(&bad_extend));
    }

    #[test]
    fn autonomy_mode_string_validation_accepts_only_three_values() {
        for mode in ["supervised", "semi_autonomous", "full_autonomous"] {
            let ok = match mode {
                "supervised" => AutonomyMode::Supervised,
                "semi_autonomous" => AutonomyMode::SemiAutonomous,
                "full_autonomous" => AutonomyMode::FullAutonomous,
                _ => panic!("should be valid"),
            };
            // Ensure the parsing in agent_set_mode would succeed (by not returning error)
            assert!(matches!(
                ok,
                AutonomyMode::Supervised
                    | AutonomyMode::SemiAutonomous
                    | AutonomyMode::FullAutonomous
            ));
        }
        // Invalid values would be rejected by the command (InvalidInput)
        for bad in ["", "semi", "SERPER"] {
            assert!(!["supervised", "semi_autonomous", "full_autonomous"].contains(&bad));
        }
    }

    // ---- IPC naming-parity guard (v1.0.1) -------------------------------
    //
    // Tauri v2 deserializes command arguments by their camelCase parameter
    // name, while response payloads are snake_case (serde
    // `rename_all = "snake_case"`). The 1.0.0 release shipped every agent
    // command invoked with snake_case ARG keys and was therefore dead at IPC
    // validation. These tests parse the argument object literals of every
    // `invoke(...)` call in `src/lib/tauri.ts` (never interface/type
    // declarations, which legitimately keep snake_case response fields) and
    // pin them against the Rust signatures above.

    /// Source of the frontend IPC wrapper, resolved relative to
    /// `src-tauri/src/commands/` (3 ups = repository root).
    const TAURI_TS: &str = include_str!("../../../src/lib/tauri.ts");

    /// True when `key` looks like a `snake_case` identifier (e.g. `run_id`):
    /// a lowercase letter, an underscore, and another lowercase letter.
    fn is_snake_case(key: &str) -> bool {
        let bytes = key.as_bytes();
        bytes
            .windows(3)
            .any(|w| w[1] == b'_' && w[0].is_ascii_lowercase() && w[2].is_ascii_lowercase())
    }

    /// Skips a string literal starting at `open` (`"`, `'` or backtick) and
    /// returns the index just past its closing quote.
    fn skip_string(src: &[u8], open: usize) -> usize {
        let quote = src[open];
        let mut i = open + 1;
        while i < src.len() {
            if src[i] == b'\\' {
                i += 2;
                continue;
            }
            if src[i] == quote {
                return i + 1;
            }
            i += 1;
        }
        src.len()
    }
    /// Collects every `invoke("command", { ... })` call site in `src` as
    /// (command name, top-level object-literal keys). Calls without an
    /// argument object yield an empty key list.
    fn extract_invoke_arg_literals(src: &str) -> Vec<(String, Vec<String>)> {
        let bytes = src.as_bytes();
        let mut calls = Vec::new();
        let mut cursor = 0usize;
        while let Some(found) = src[cursor..].find("invoke") {
            let at = cursor + found;
            cursor = at + "invoke".len();
            // Only call syntax counts: `invoke<...>(` or `invoke(` — the
            // `import { invoke }` binding and prose are skipped here.
            let rest = src[cursor..].trim_start();
            if !rest.starts_with('<') && !rest.starts_with('(') {
                continue;
            }
            let Some(open) = bytes[cursor..].iter().position(|&b| b == b'(') else {
                continue;
            };
            let mut i = cursor + open + 1;
            // First argument: the command-name string literal.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'"' {
                continue;
            }
            let cmd_start = i + 1;
            i = skip_string(bytes, i);
            let command = src[cmd_start..i - 1].to_string();
            // Second argument (optional): the argument object literal.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let keys = if i < bytes.len() && bytes[i] == b',' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'{' {
                    let mut depth = 0i32;
                    let mut in_string: Option<u8> = None;
                    let start = i;
                    while i < bytes.len() {
                        let b = bytes[i];
                        if let Some(quote) = in_string {
                            if b == b'\\' {
                                i += 2;
                                continue;
                            }
                            if b == quote {
                                in_string = None;
                            }
                        } else {
                            match b {
                                b'"' | b'\'' | b'`' => in_string = Some(b),
                                b'{' => depth += 1,
                                b'}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        i += 1;
                    }
                    top_level_keys(&src[start + 1..i.min(bytes.len())])
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            calls.push((command, keys));
        }
        calls
    }

    /// Splits the inner text of an object literal into its top-level keys,
    /// handling shorthand properties (`conversationId`) and explicit ones
    /// (`runId: runId`).
    fn top_level_keys(inner: &str) -> Vec<String> {
        fn push_key(keys: &mut Vec<String>, segment: &str) {
            let segment = segment.trim();
            if segment.is_empty() {
                return;
            }
            let key = match segment.find(':') {
                Some(idx) => segment[..idx].trim(),
                None => segment,
            };
            if !key.is_empty() {
                keys.push(key.to_string());
            }
        }

        let mut keys = Vec::new();
        let mut depth = 0i32;
        let mut in_string: Option<char> = None;
        let mut current = String::new();
        for ch in inner.chars() {
            if let Some(quote) = in_string {
                current.push(ch);
                if ch == quote {
                    in_string = None;
                }
                continue;
            }
            match ch {
                '"' | '\'' | '`' => {
                    in_string = Some(ch);
                    current.push(ch);
                }
                '{' | '(' | '[' => {
                    depth += 1;
                    current.push(ch);
                }
                '}' | ')' | ']' => {
                    depth -= 1;
                    current.push(ch);
                }
                ',' if depth == 0 => {
                    push_key(&mut keys, &current);
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
        push_key(&mut keys, &current);
        keys
    }

    /// Test A: no `invoke` call in `src/lib/tauri.ts` may pass a `snake_case`
    /// argument key — command ARGS are camelCase (Tauri v2) repo-wide.
    #[test]
    fn ipc_args_are_camel_case() {
        let calls = extract_invoke_arg_literals(TAURI_TS);
        // Non-vacuous: the parser must see the 27 non-agent call sites plus
        // the 11 agent ones with argument objects (36 + add/remove).
        assert!(
            calls.len() >= 38,
            "naming-parity parser found only {} invoke calls in src/lib/tauri.ts; \
             it must parse every call site to be a real guard",
            calls.len()
        );
        for (command, keys) in calls {
            for key in keys {
                assert!(
                    !is_snake_case(&key),
                    "invoke(\"{command}\") argument key '{key}' is snake_case; \
                     Tauri v2 command ARGS must use camelCase keys"
                );
            }
        }
    }

    /// Test B: the argument key set of each agent command in `tauri.ts` must
    /// exactly equal the Rust parameter names in camelCase — no extra key,
    /// no missing key.
    #[test]
    fn agent_command_arg_keys_match_rust_params() {
        const AGENT_COMMANDS: [(&str, &[&str]); 12] = [
            (
                "start_agent_run",
                &["conversationId", "content", "provider", "model"],
            ),
            ("cancel_agent_run", &["runId"]),
            (
                "resolve_agent_approval",
                &["runId", "callId", "approved", "scope"],
            ),
            ("extend_agent_run", &["runId", "extraSteps"]),
            ("list_agent_runs", &["conversationId"]),
            ("list_agent_steps", &["runId"]),
            ("agent_set_mode", &["runId", "mode"]),
            ("pause_agent_run", &["runId"]),
            ("resume_agent_run", &["runId"]),
            (
                "add_permission_rule",
                &["preset", "toolPattern", "pathPattern", "effect"],
            ),
            ("remove_permission_rule", &["id"]),
            ("list_permission_rules", &[]),
        ];
        let calls = extract_invoke_arg_literals(TAURI_TS);
        for (command, want) in AGENT_COMMANDS {
            let got = &calls
                .iter()
                .find(|(name, _)| name == command)
                .unwrap_or_else(|| panic!("invoke(\"{command}\") not found in src/lib/tauri.ts"))
                .1;
            let mut got_sorted: Vec<&str> = got.iter().map(String::as_str).collect();
            got_sorted.sort_unstable();
            let mut want_sorted: Vec<&str> = want.to_vec();
            want_sorted.sort_unstable();
            assert_eq!(
                got_sorted, want_sorted,
                "invoke(\"{command}\") argument keys must exactly match the \
                 Rust command parameters (camelCase)"
            );
        }
    }

    /// Static wiring check: the production `start_agent_run` command must
    /// route through the application-layer bridge (`service::start_run`) and
    /// must not construct the runner directly. The last mile —
    /// `commands::agent::start_agent_run` down to `service::start_run` —
    /// cannot be driven from a test because it needs `State<'_, _>`, so this
    /// source check pins the delegation instead. The runner needle is built
    /// with `concat!` so this test's own source never matches it verbatim.
    #[test]
    fn start_agent_run_routes_through_service_bridge() {
        const SOURCE: &str = include_str!("agent.rs");
        assert!(
            SOURCE.contains("service::start_run("),
            "start_agent_run must delegate to the service bridge"
        );
        let runner_needle = concat!("AgentRunner", "::new");
        assert!(
            !SOURCE.contains(runner_needle),
            "commands/agent.rs must not construct the runner directly"
        );
    }

    /// Static wiring check mirroring the bridge pattern above: a provider
    /// call cancelled in flight must route to the `cancelled` terminal
    /// outcome end to end. The runner maps the provider's cancellation to
    /// `AgentError::Cancelled`, and the shared terminal mapping turns that
    /// into the `cancelled` run status — pinning both needles keeps the
    /// route from silently breaking into an `error`/`Provider` mapping.
    #[test]
    fn shell_never_gets_persistent_allow() {
        // Direct validation: persistent allow for the shell is InvalidInput.
        let err = validate_new_rule("coding", "execute_command", Some("*"), "allow")
            .expect_err("shell allow must be rejected");
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        let wildcard = validate_new_rule("coding", "*", Some("*"), "allow")
            .expect_err("wildcard allow includes the shell and must be rejected");
        assert_eq!(wildcard.kind, ErrorKind::InvalidInput);
        // Non-shell allow validates.
        validate_new_rule("coding", "write_file", Some("*"), "allow")
            .expect("non-shell allow valid");
        // Group path never persists shell-allow rows (the v7 seed is an
        // ask row for the shell, which must survive untouched).
        let db = crate::infrastructure::database::in_memory_database();
        insert_group_allow_rule(&db, "coding", "execute_command", Some("*"));
        let rules = permissions::list_rules(&db).expect("list rules");
        assert!(
            rules
                .iter()
                .all(|rule| !(rule.tool_pattern == "execute_command"
                    && rule.effect == RuleEffect::Allow)),
            "no shell-allow row may persist via the group path, got {rules:?}"
        );
        insert_group_allow_rule(&db, "coding", "write_file", Some("*"));
        let rules = permissions::list_rules(&db).expect("list rules");
        let persisted = rules
            .iter()
            .find(|rule| rule.tool_pattern == "write_file")
            .expect("write_file rule persisted via group path");
        assert_eq!(persisted.effect, RuleEffect::Allow);
        assert_eq!(persisted.priority, 200);
    }

    #[test]
    fn resolve_agent_approval_scope_routes_through_registry() {
        const SOURCE: &str = include_str!("agent.rs");
        assert!(
            SOURCE.contains("resolve_with_scope("),
            "resolve_agent_approval must route group scope through the registry"
        );
        assert!(
            SOURCE.contains("insert_group_allow_rule("),
            "group-scope approvals must offer the persistent group-allow path"
        );
    }

    #[test]
    fn provider_cancellation_routes_to_cancelled_outcome() {
        const RUNNER: &str = include_str!("../application/agent/runner.rs");
        const PERSISTENCE: &str = include_str!("../application/agent/persistence.rs");
        assert!(
            RUNNER.contains("ExecutorError::Cancelled"),
            "runner.rs must route provider cancellation explicitly"
        );
        assert!(
            RUNNER.contains("AgentRunEvent::Cancelled"),
            "runner.rs must stream the Cancelled governance event on abort"
        );
        assert!(
            PERSISTENCE.contains("\"cancelled\""),
            "persistence.rs must map cancellation onto the cancelled status"
        );
    }

    #[test]
    fn group_preset_routes_rule_scope_from_group_key() {
        // Group keys are preset-scoped (`preset:tool:path`), so a document
        // run's group approval persists a document-scoped rule; unknown
        // shapes keep the pre-T5 coding fallback.
        assert_eq!(group_preset(Some("document:write_file:docs")), "document");
        assert_eq!(group_preset(Some("coding:write_file:docs")), "coding");
        assert_eq!(group_preset(Some("coding:write_file:*")), "coding");
        assert_eq!(group_preset(None), "coding");
        assert_eq!(group_preset(Some("bogus")), "coding");
        assert_eq!(group_preset(Some("")), "coding");

        let db = crate::infrastructure::database::in_memory_database();
        assert!(insert_group_allow_rule(
            &db,
            group_preset(Some("document:write_file:docs")),
            "write_file",
            Some("docs"),
        ));
        let rules = permissions::list_rules(&db).expect("list rules");
        let persisted = rules
            .iter()
            .find(|rule| rule.tool_pattern == "write_file")
            .expect("write_file rule persisted via group path");
        assert_eq!(persisted.preset, "document");
        assert_eq!(persisted.effect, RuleEffect::Allow);
    }
}
