//! Terminal IPC commands: the Tauri side of the workspace terminal panel.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves the
//! effective workspace root (the stored `agent.workspace_root` or the
//! default `agent_workspace` directory, exactly like the workspace and
//! version-control commands), claims the single-session
//! [`TerminalRegistry`](crate::application::terminal::TerminalRegistry),
//! and delegates to the application-layer terminal service
//! ([`crate::application::terminal`]), which itself is a thin skin over the
//! agent `execute_command` tool path. No business logic lives here beyond
//! that translation.
//!
//! Command-shape decision (one feature area, two commands): `terminal_run`
//! runs one user-authored command synchronously on the blocking pool (like
//! `send_message`) and returns the tool path's combined output with its
//! `truncated`/`success` display flags; `terminal_kill` cancels the active
//! run's token so the executor kills the child promptly. No streaming: the
//! reused tool path delivers output on completion only.
//!
//! Approval-gate note: the agent `ApprovalGate` parks live agent tool calls
//! and cannot apply to direct IPC commands (no run, no park, no autonomy
//! mode). The panel's Run press is the approval (user-authored commands),
//! and the backend still requires the per-call `confirmed` flag — the same
//! gate shape as the git writes, not a parallel mechanism.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::application::terminal::{TerminalExecuted, TerminalRegistry};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Managed terminal registry state is an [`Arc`] so commands can clone an
/// owned handle into `spawn_blocking` without borrowing the managed value.
pub(crate) type ManagedTerminal = Arc<TerminalRegistry>;

/// Combined output of one finished terminal run, plus the display flags the
/// panel needs. Payloads stay `snake_case` (serde `rename_all`); command args
/// are camelCase (Tauri v2) — never align one to the other.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TerminalRunResponse {
    pub run_id: u64,
    pub output: String,
    pub truncated: bool,
    pub success: bool,
}

/// Run one user-authored workspace command through the existing agent
/// `execute_command` tool path and return its combined output.
///
/// `command` must be non-empty; `cwd` (when set) is workspace-relative and
/// workspace-confined by the tool path itself. `confirmed` must be `true`
/// (the panel's Run press); at most one run is active at a time. The call
/// blocks on the runtime's blocking pool until the tool path returns
/// (completion, timeout kill, or stop kill).
///
/// # Errors
///
/// Classified [`CommandError`]s for unconfirmed runs
/// (`ConfirmationRequired`), invalid input (empty command, bad/escaping
/// working dir), an already-active run, execution/timeout failures, or a
/// stop. Secret-free by construction: the command text is never echoed.
#[tauri::command]
pub(crate) async fn terminal_run(
    command: String,
    cwd: Option<String>,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
    registry: State<'_, ManagedTerminal>,
) -> Result<TerminalRunResponse, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    // Cheap pre-claim validation: a bare-IPC unconfirmed/empty call must
    // be refused before it can briefly hold the single session. The
    // authoritative checks stay inside `execute_terminal`.
    if !confirmed {
        return Err(CommandError::new(
            ErrorKind::ConfirmationRequired,
            "explicit confirmation is required before a terminal command can run",
        ));
    }
    if command.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "the terminal command must not be empty",
        ));
    }
    let (run_id, token) = registry.begin().ok_or_else(|| {
        CommandError::new(
            ErrorKind::InvalidInput,
            "a terminal command is already running — stop it first",
        )
    })?;
    let registry_arc = Arc::clone(registry.inner());
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::application::terminal::execute_terminal(
            &root,
            command.as_str(),
            cwd.as_deref(),
            confirmed,
            &token,
        )
    })
    .await;
    registry_arc.finish(run_id);
    let outcome = outcome.map_err(|err| {
        // Only reachable if the blocking task panicked: report a safe,
        // classified failure instead of leaving the promise dangling.
        log::error!("terminal_run blocking task failed: {err}");
        CommandError::new(ErrorKind::Request, "the terminal command could not be run")
    })?;
    let executed: TerminalExecuted = outcome.map_err(CommandError::from)?;
    Ok(TerminalRunResponse {
        run_id,
        output: executed.output,
        truncated: executed.truncated,
        success: executed.success,
    })
}

/// Stop the active terminal run, if any: cancels its token so the executor
/// kills the child promptly. Returns `true` when a run was active.
#[tauri::command]
pub(crate) fn terminal_kill(registry: State<'_, ManagedTerminal>) -> bool {
    registry.cancel_active()
}

impl From<crate::application::terminal::TerminalError> for CommandError {
    fn from(err: crate::application::terminal::TerminalError) -> Self {
        use crate::application::terminal::TerminalError as Source;
        match err {
            Source::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before a terminal command can run",
            ),
            Source::InvalidInput(message) => Self::new(ErrorKind::InvalidInput, message),
            // The service message is already fixed vocabulary (the tool
            // path's detail is classified away, never echoed).
            Source::Execution(message) => Self::new(ErrorKind::Io, message),
            Source::Timeout => Self::new(
                ErrorKind::Io,
                "the terminal command timed out before it completed",
            ),
            Source::Cancelled => Self::new(ErrorKind::Io, "the terminal command was stopped"),
            Source::AlreadyRunning => Self::new(
                ErrorKind::InvalidInput,
                "a terminal command is already running — stop it first",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn terminal_error_mapping_is_classified_and_secret_free() {
        use crate::application::terminal::TerminalError as Source;
        let cases = [
            (
                Source::Unconfirmed,
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before a terminal command can run",
            ),
            (
                Source::InvalidInput("the terminal command must not be empty".into()),
                ErrorKind::InvalidInput,
                "the terminal command must not be empty",
            ),
            (
                Source::Execution("the terminal command could not be executed".into()),
                ErrorKind::Io,
                "the terminal command could not be executed",
            ),
            (
                Source::Timeout,
                ErrorKind::Io,
                "the terminal command timed out before it completed",
            ),
            (
                Source::Cancelled,
                ErrorKind::Io,
                "the terminal command was stopped",
            ),
            (
                Source::AlreadyRunning,
                ErrorKind::InvalidInput,
                "a terminal command is already running — stop it first",
            ),
        ];
        for (source, kind, message) in cases {
            let mapped = CommandError::from(source);
            assert_eq!(mapped.kind, kind);
            assert_eq!(mapped.message, message);
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }

    /// Static wiring check: the terminal commands must route through the
    /// existing agent tool path (`execute_terminal`, which dispatches a real
    /// `execute_command` tool call) and must never spawn a process of
    /// their own — no parallel executor. The command bodies need
    /// `State<'_, _>` and cannot be invoked here, so this source check pins
    /// the delegation instead. Needles are built with `concat!` so this
    /// test's own source never matches them verbatim.
    #[test]
    fn terminal_run_routes_through_the_tool_path_without_its_own_executor() {
        const SOURCE: &str = include_str!("terminal.rs");
        const SERVICE: &str = include_str!("../application/terminal.rs");
        assert!(
            SOURCE.contains("execute_terminal("),
            "terminal_run must delegate to the application-layer terminal service"
        );
        for needle in [
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/terminal.rs must not spawn processes itself, found {needle:?}"
            );
            assert!(
                !SERVICE.contains(needle),
                "application/terminal.rs must not spawn processes itself, found {needle:?}"
            );
        }
        assert!(
            SERVICE.contains("execute_with_cancellation("),
            "the terminal service must dispatch through the tool registry"
        );
        assert!(
            SERVICE.contains("\"execute_command\""),
            "the terminal service must reuse the execute_command tool"
        );
    }
}
