//! Workspace terminal runs: user-authored shell commands through the existing
//! agent tool path.
//!
//! The terminal panel is a thin interactive skin over the agent
//! `execute_command` tool ([`ToolRegistry`]): every run builds a real
//! `execute_command` [`ToolCall`] and dispatches it through
//! [`ToolRegistry::execute_with_cancellation`], so workspace confinement,
//! the hard timeout, bounded capture, cooperative kill, and output
//! truncation are the agent path's own — this module invents no executor,
//! no shell spawning, and no output shaping.
//!
//! Approval-gate note: the agent [`ApprovalGate`](crate::application::agent::approval::ApprovalGate)
//! parks agent-proposed tool calls inside a live run (no run, no park).
//! A terminal command is authored by the user directly, so the Run gesture
//! itself is the approval; the backend still requires the per-call
//! `confirmed` flag (the destructive-action confirmation pattern shared
//! with the git writes and data management — [`TerminalError::Unconfirmed`])
//! so bare IPC callers cannot execute without it.
//!
//! Limits (inherited, documented for the panel UX):
//! - workspace-scoped: `cwd` resolves inside the workspace root or the run
//!   is refused; absolute escape is rejected by the tool path itself.
//! - time-boxed: the tool path kills the child after its hard timeout
//!   (30 s); [`TerminalRegistry::cancel_active`] kills it earlier via the
//!   run's [`CancellationToken`].
//! - output-capped: the tool path truncates to its context budget with an
//!   inline notice; [`TerminalExecuted::truncated`] surfaces that so the
//!   panel can badge it.
//! - stdin is closed: interactive-TUI programs (`vim`, `ssh`, …) cannot
//!   read input and run until the timeout — documented as unsupported,
//!   never special-cased here.
//! - single session: at most one active run per process; a second
//!   [`TerminalRegistry::begin`] while one is active returns `None`
//!   ([`TerminalError::AlreadyRunning`]). No persistence across restart.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::application::agent::control::CancellationToken;
use crate::application::agent::tools::{ToolError, ToolRegistry};
use crate::application::execution::ToolCall;

// ---------------------------------------------------------------------------
// Result and error
// ---------------------------------------------------------------------------

/// Outcome of one finished terminal run: the tool path's own combined
/// output plus the two display flags the panel needs. `success` is false
/// when the tool path rendered a non-zero-exit marker; the numeric status
/// stays inside `output` text (the tool path returns no structured code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TerminalExecuted {
    pub output: String,
    pub truncated: bool,
    pub success: bool,
}

/// Classified terminal failure. Every message is fixed vocabulary: the
/// user's command text may carry secrets (tokens, private flags), so it is
/// never echoed — not here, not in logs, not across IPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalError {
    /// The per-call `confirmed` flag was `false`.
    Unconfirmed,
    /// Empty command, or the tool path rejected the command/working dir.
    InvalidInput(String),
    /// The tool path failed to run the command (spawn failure and friends).
    Execution(String),
    /// The tool path killed the child on its hard timeout.
    Timeout,
    /// The run was stopped via [`TerminalRegistry::cancel_active`].
    Cancelled,
    /// A run is already active (single session).
    AlreadyRunning,
}

impl std::fmt::Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfirmed => write!(
                f,
                "explicit confirmation is required before a terminal command can run"
            ),
            Self::InvalidInput(message) | Self::Execution(message) => write!(f, "{message}"),
            Self::Timeout => write!(f, "the terminal command timed out before it completed"),
            Self::Cancelled => write!(f, "the terminal command was stopped before it completed"),
            Self::AlreadyRunning => {
                write!(f, "a terminal command is already running — stop it first")
            }
        }
    }
}

impl std::error::Error for TerminalError {}

// ---------------------------------------------------------------------------
// Execution (thin skin over the tool path)
// ---------------------------------------------------------------------------

/// Marker the tool path appends for non-zero exits
/// (`executor::execute_command_with_limits`): its presence means failure.
/// Searched as a substring of the combined output.
const EXIT_MARKER: &str = "command exited with status";

/// Truncation markers the tool path's `truncate_output` renders
/// (`tools::output`): either means the output was capped with a notice.
const TRUNCATE_MARKERS: [&str; 2] = ["[output truncated,", "[truncated: "];

/// Run `command` in `workspace_root` (optionally scoped to the
/// workspace-relative `cwd`) through the existing `execute_command` tool.
///
/// `confirmed` must be `true` (the Run gesture); `token` is the run's
/// cancellation token (kill path). See the module docs for the inherited
/// limits.
pub(crate) fn execute_terminal(
    workspace_root: &Path,
    command: &str,
    cwd: Option<&str>,
    confirmed: bool,
    token: &CancellationToken,
) -> Result<TerminalExecuted, TerminalError> {
    if !confirmed {
        return Err(TerminalError::Unconfirmed);
    }
    if command.trim().is_empty() {
        return Err(TerminalError::InvalidInput(
            "the terminal command must not be empty".to_string(),
        ));
    }
    let arguments = match cwd {
        Some(dir) => serde_json::json!({ "command": command, "cwd": dir }),
        None => serde_json::json!({ "command": command }),
    };
    let call = ToolCall {
        id: "terminal".to_string(),
        name: "execute_command".to_string(),
        arguments: arguments.to_string(),
        thought_signature: None,
    };
    match ToolRegistry::execute_with_cancellation(&call, workspace_root, token) {
        Ok(output) => Ok(TerminalExecuted {
            truncated: TRUNCATE_MARKERS
                .iter()
                .any(|marker| output.contains(marker)),
            success: !output.contains(EXIT_MARKER),
            output,
        }),
        Err(err) => Err(map_tool_error(&err)),
    }
}

/// Map the tool path's classified error onto fixed-vocabulary terminal
/// errors. The source messages may echo caller input (`cwd '…'`), so they
/// are classified — never forwarded — keeping command text (and any
/// secrets inside it) out of logs and IPC.
fn map_tool_error(err: &ToolError) -> TerminalError {
    match err {
        ToolError::InvalidArguments(_) => TerminalError::InvalidInput(
            "the terminal command or working directory is invalid".to_string(),
        ),
        ToolError::PathTraversal(_) => TerminalError::InvalidInput(
            "the terminal working directory is outside the workspace".to_string(),
        ),
        ToolError::Timeout(_) => TerminalError::Timeout,
        ToolError::Cancelled => TerminalError::Cancelled,
        ToolError::UnknownTool(_) | ToolError::Io(_) => {
            log::warn!("terminal run failed on the tool path");
            TerminalError::Execution("the terminal command could not be executed".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Registry (single active run + kill)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ActiveTerminalRun {
    id: u64,
    token: CancellationToken,
}

#[derive(Debug, Default)]
struct TerminalState {
    next_id: u64,
    active: Option<ActiveTerminalRun>,
}

/// Process-wide single-session terminal state: id counter plus the active
/// run's cancellation token. Held as managed Tauri state (`Arc` over this).
#[derive(Debug, Default)]
pub(crate) struct TerminalRegistry {
    state: Mutex<TerminalState>,
}

impl TerminalRegistry {
    /// Create an empty registry (no active run, ids from 1).
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, TerminalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claim the single session for a new run: returns the run id and its
    /// fresh cancellation token, or `None` when a run is already active.
    pub(crate) fn begin(&self) -> Option<(u64, CancellationToken)> {
        let mut state = self.lock_state();
        if state.active.is_some() {
            return None;
        }
        state.next_id = state.next_id.saturating_add(1);
        let token = CancellationToken::new();
        let id = state.next_id;
        state.active = Some(ActiveTerminalRun {
            id,
            token: token.clone(),
        });
        Some((id, token))
    }

    /// Release the session for `id` (no-op for a stale id). Always call
    /// when the run thread returns, on every path.
    pub(crate) fn finish(&self, id: u64) {
        let mut state = self.lock_state();
        if state.active.as_ref().is_some_and(|run| run.id == id) {
            state.active = None;
        }
    }

    /// Cancel the active run, if any. The executor kills the child
    /// promptly; the run thread then returns `Cancelled`. Returns whether
    /// a run was active.
    pub(crate) fn cancel_active(&self) -> bool {
        let state = self.lock_state();
        match state.active.as_ref() {
            Some(run) => {
                run.token.cancel();
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn has_active(&self) -> bool {
        self.lock_state().active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::temp_workspace;

    const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];

    fn assert_secret_free(text: &str) {
        for sentinel in SECRET_SENTINELS {
            assert!(
                !text.to_lowercase().contains(sentinel),
                "terminal error must stay secret-free, found {sentinel:?} in {text:?}"
            );
        }
    }

    #[test]
    fn unconfirmed_run_is_refused_before_execution() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let err = execute_terminal(&ws, "echo hi", None, false, &token)
            .expect_err("unconfirmed must be refused");
        assert_eq!(err, TerminalError::Unconfirmed);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn empty_command_is_invalid() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        for command in ["", "   "] {
            let err = execute_terminal(&ws, command, None, true, &token)
                .expect_err("empty command must be refused");
            assert!(
                matches!(err, TerminalError::InvalidInput(_)),
                "unexpected: {err:?}"
            );
            assert_secret_free(&err.to_string());
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn real_command_runs_in_workspace_scope() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let executed =
            execute_terminal(&ws, "echo terminal-ok", None, true, &token).expect("echo runs");
        assert!(executed.success);
        assert!(!executed.truncated);
        assert!(executed.output.contains("terminal-ok"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn failing_command_reports_no_success_with_marker() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        // Non-zero exits still return output (with the tool path's marker),
        // never an error — the panel renders output + a non-zero badge.
        let executed = execute_terminal(&ws, "exit 1", None, true, &token).expect("exit 1 runs");
        assert!(!executed.success);
        assert!(executed.output.contains(EXIT_MARKER));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn absolute_escape_cwd_is_refused_secret_free() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let outside = if cfg!(windows) {
            "C:\\Windows\\System32"
        } else {
            "/etc"
        };
        let err = execute_terminal(&ws, "echo hi", Some(outside), true, &token)
            .expect_err("absolute escape must be refused");
        assert!(
            matches!(err, TerminalError::InvalidInput(_)),
            "unexpected: {err:?}"
        );
        assert!(
            !err.to_string().contains(outside),
            "rejected cwd must not be echoed: {err:?}"
        );
        assert_secret_free(&err.to_string());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn dotdot_escape_cwd_is_refused() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let err = execute_terminal(&ws, "echo hi", Some("../.."), true, &token)
            .expect_err("dotdot escape must be refused");
        assert!(
            matches!(err, TerminalError::InvalidInput(_)),
            "unexpected: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn pre_cancelled_token_aborts_secret_free() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        token.cancel();
        let err = execute_terminal(&ws, "echo hi", None, true, &token)
            .expect_err("cancelled token must abort");
        assert_eq!(err, TerminalError::Cancelled);
        assert_secret_free(&err.to_string());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn secret_bearing_command_never_echoes_in_errors() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        // A command carrying a secret-shaped token plus an invalid cwd: the
        // refusal must classify, never echo either value.
        let secret_command = "curl -H \"Authorization: Bearer sk-secret-123\" hi";
        let err = execute_terminal(&ws, secret_command, Some("/etc"), true, &token)
            .expect_err("invalid cwd must be refused");
        let text = err.to_string();
        assert!(!text.contains("sk-secret-123"), "secret echoed: {text:?}");
        assert!(!text.contains("curl"), "command echoed: {text:?}");
        assert_secret_free(&text);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn registry_is_single_session_with_kill() {
        let registry = TerminalRegistry::new();
        assert!(!registry.has_active());
        assert!(!registry.cancel_active(), "kill with no run is false");
        let (first, _token) = registry.begin().expect("first run claims");
        assert!(registry.has_active());
        assert!(
            registry.begin().is_none(),
            "second begin while active must refuse"
        );
        assert!(registry.cancel_active(), "kill with active run is true");
        // A stale finish never releases a newer session: finish the first,
        // claim again, then replay the stale finish.
        registry.finish(first);
        assert!(!registry.has_active());
        let (second, _token) = registry.begin().expect("re-claim after finish");
        assert_ne!(first, second);
        registry.finish(first);
        assert!(
            registry.has_active(),
            "stale finish must not release the newer session"
        );
        registry.finish(second);
        assert!(!registry.has_active());
    }
}
