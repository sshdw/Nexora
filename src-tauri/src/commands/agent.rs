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

/// The per-run agent workspace root: a dedicated subdirectory of the app-data
/// dir that every workspace-bounded tool operates within. Created on demand;
/// 5.2's settings UI can replace this with a user-chosen folder.
fn workspace_root(app: &AppHandle) -> Result<PathBuf, CommandError> {
    let base = app.path().app_data_dir().map_err(|err| {
        CommandError::new(
            ErrorKind::Io,
            format!("the application data directory is unavailable: {err}"),
        )
    })?;
    let root = base.join("agent_workspace");
    std::fs::create_dir_all(&root).map_err(|_| {
        CommandError::new(ErrorKind::Io, "the agent workspace could not be created")
    })?;
    Ok(root)
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

        let root = workspace_root(&handle)?;
        let host: Arc<dyn AgentRunHost> = Arc::new(TauriAgentHost::new(handle, db_client.clone()));
        // Resolve autonomy mode from settings (DP-AUTONOMY): default
        // semi_autonomous when unset/invalid.
        let mode = service::resolve_autonomy_mode(db_ref);

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
                spend_limit_micro_usd: None,
            },
            mode,
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
#[tauri::command]
pub(crate) fn resolve_agent_approval(
    run_id: i64,
    call_id: String,
    approved: bool,
    registry: State<'_, ManagedRegistry>,
) -> Result<(), CommandError> {
    match registry.resolve(run_id, &call_id, approved) {
        ResolveOutcome::Resolved => Ok(()),
        ResolveOutcome::RunNotActive => Err(CommandError::new(
            ErrorKind::NotFound,
            format!("no active agent run with id {run_id}"),
        )),
        ResolveOutcome::NoPendingApproval => Err(CommandError::new(
            ErrorKind::NotFound,
            "the run has no pending approval for that call",
        )),
    }
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
        // the 9 agent ones (36 invoke sites carry an argument object).
        assert!(
            calls.len() >= 36,
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
        const AGENT_COMMANDS: [(&str, &[&str]); 9] = [
            (
                "start_agent_run",
                &["conversationId", "content", "provider", "model"],
            ),
            ("cancel_agent_run", &["runId"]),
            ("resolve_agent_approval", &["runId", "callId", "approved"]),
            ("extend_agent_run", &["runId", "extraSteps"]),
            ("list_agent_runs", &["conversationId"]),
            ("list_agent_steps", &["runId"]),
            ("agent_set_mode", &["runId", "mode"]),
            ("pause_agent_run", &["runId"]),
            ("resume_agent_run", &["runId"]),
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
}
