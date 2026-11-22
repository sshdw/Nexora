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
//! Command-shape decision (one feature area, three commands): `terminal_run`
//! runs one user-authored command synchronously on the blocking pool (like
//! `send_message`) and returns the tool path's combined output with its
//! `truncated`/`success` display flags; `terminal_kill` cancels the active
//! run's token so the executor kills the child promptly; `terminal_explain`
//! sends one failed run's capped output through the existing AI execution
//! path (like `git_generate_commit_message`) and returns a diagnosis plus a
//! copy-only fix suggestion. No streaming: the reused tool path delivers
//! output on completion only.
//!
//! Approval-gate note: the agent `ApprovalGate` parks live agent tool calls
//! and cannot apply to direct IPC commands (no run, no park, no autonomy
//! mode). The panel's Run press *requests* the approval; the backend then
//! requires a single-use confirmation id minted by `request_confirmation`
//! **after a blocking native OS dialog the user accepted**, bound to that
//! exact command and working directory, consumed once per run — the
//! server-side gate shape shared with data management, not a parallel
//! mechanism. A caller-supplied boolean could be forged by any IPC caller
//! (NEX-SEC-004); an id the user approved for one command cannot authorize a
//! different one.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::application::agent::tools::CommandStreamUpdate;
use crate::application::confirmations::ManagedConfirmations;
use crate::application::pty_terminal::{PtyError, PtySessionInfo, PtyTerminalManager};
use crate::application::terminal::{ErrorExplanation, TerminalRegistry};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Managed terminal registry state is an [`Arc`] so commands can clone an
/// owned handle into `spawn_blocking` without borrowing the managed value.
pub(crate) type ManagedTerminal = Arc<TerminalRegistry>;

/// Managed interactive PTY terminal state shared across IPC commands and
/// orderly window shutdown.
pub(crate) type ManagedPtyTerminal = Arc<PtyTerminalManager>;

/// Live event channel emitted while a workspace terminal command executes.
pub(crate) const TERMINAL_OUTPUT_EVENT: &str = "terminal-output";

/// Incremental live event payload emitted on [`TERMINAL_OUTPUT_EVENT`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TerminalOutputEvent {
    pub run_id: u64,
    pub stream: String,
    pub chunk: String,
    pub done: bool,
    pub truncated: bool,
    pub killed: bool,
    pub success: Option<bool>,
}

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

fn translate_terminal_update(
    run_id: u64,
    update: CommandStreamUpdate,
    stream_done: &mut Option<(bool, bool)>,
) -> Option<TerminalOutputEvent> {
    match update {
        CommandStreamUpdate::Started { .. } => Some(TerminalOutputEvent {
            run_id,
            stream: "status".to_string(),
            chunk: String::new(),
            done: false,
            truncated: false,
            killed: false,
            success: None,
        }),
        CommandStreamUpdate::Chunk { stream, text } => Some(TerminalOutputEvent {
            run_id,
            stream: stream.to_string(),
            chunk: text,
            done: false,
            truncated: false,
            killed: false,
            success: None,
        }),
        // Final completion metadata is captured here and emitted once on return
        // with the authoritative `success` flag.
        CommandStreamUpdate::Done { truncated, killed } => {
            *stream_done = Some((truncated, killed));
            None
        }
    }
}

fn emit_terminal_update(
    app: &AppHandle,
    run_id: u64,
    update: CommandStreamUpdate,
    stream_done: &mut Option<(bool, bool)>,
) {
    if let Some(payload) = translate_terminal_update(run_id, update, stream_done) {
        let _ = app.emit(TERMINAL_OUTPUT_EVENT, payload);
    }
}

fn resolve_terminal_done_flags(
    outcome: &Result<
        crate::application::terminal::TerminalExecuted,
        crate::application::terminal::TerminalError,
    >,
    stream_done: Option<(bool, bool)>,
) -> (bool, bool, bool) {
    let (stream_truncated, stream_killed) = stream_done.unwrap_or((false, false));
    match outcome {
        Ok(executed) => (
            executed.truncated || stream_truncated,
            stream_killed,
            executed.success,
        ),
        Err(err) => {
            let killed = stream_killed
                || matches!(
                    err,
                    crate::application::terminal::TerminalError::Cancelled
                        | crate::application::terminal::TerminalError::Timeout
                );
            (stream_truncated, killed, false)
        }
    }
}

fn emit_terminal_done(app: &AppHandle, run_id: u64, truncated: bool, killed: bool, success: bool) {
    let _ = app.emit(
        TERMINAL_OUTPUT_EVENT,
        TerminalOutputEvent {
            run_id,
            stream: "status".to_string(),
            chunk: String::new(),
            done: true,
            truncated,
            killed,
            success: Some(success),
        },
    );
}

/// Run one user-authored workspace command through the existing agent
/// `execute_command` tool path and return its combined output.
///
/// `command` must be non-empty; `cwd` (when set) is workspace-relative and
/// workspace-confined by the tool path itself. `confirmation_id` must be a
/// live single-use id minted for the terminal scope by the
/// `request_confirmation` command — which only mints once the user accepts
/// the native OS confirmation prompt — and bound to this exact command and
/// working directory; at most one run is active at a time. The call blocks on
/// the runtime's blocking pool until the tool path returns (completion,
/// timeout kill, or stop kill).
///
/// # Errors
///
/// Classified [`CommandError`]s for refused confirmations
/// (`ConfirmationRequired`: unknown, expired, already-consumed, cross-scope,
/// or different-operation ids — nothing executes), invalid input (empty
/// command, bad/escaping working dir; the id is *not* consumed for these, so
/// the user keeps their approval), an already-active run,
/// execution/timeout failures, or a stop. Secret-free by construction: the
/// command text is never echoed.
#[tauri::command]
pub(crate) async fn terminal_run(
    command: String,
    cwd: Option<String>,
    confirmation_id: String,
    app: AppHandle,
    db: State<'_, Database>,
    registry: State<'_, ManagedTerminal>,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<TerminalRunResponse, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    // Cheap pre-claim validation: a bare-IPC empty call must be refused
    // before it can briefly hold the single session or burn a confirmation
    // id. The authoritative checks stay inside `execute_terminal`.
    if command.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "the terminal command must not be empty",
        ));
    }
    if confirmation_id.trim().is_empty() {
        return Err(CommandError::new(
            ErrorKind::ConfirmationRequired,
            "explicit confirmation is required before a terminal command can run",
        ));
    }
    let (run_id, token) = registry.begin().ok_or_else(|| {
        CommandError::new(
            ErrorKind::InvalidInput,
            "a terminal command is already running — stop it first",
        )
    })?;
    let registry_arc = Arc::clone(registry.inner());
    let confirmations_arc = Arc::clone(confirmations.inner());
    let app_for_stream = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let mut stream_done = None;
        let mut emit_stream =
            |update| emit_terminal_update(&app_for_stream, run_id, update, &mut stream_done);
        let result = crate::application::terminal::execute_terminal(
            &root,
            command.as_str(),
            cwd.as_deref(),
            confirmation_id.as_str(),
            &confirmations_arc,
            &token,
            Some(&mut emit_stream),
        );
        (result, stream_done)
    })
    .await;
    registry_arc.finish(run_id);
    let (outcome, stream_done) = outcome.map_err(|err| {
        // Only reachable if the blocking task panicked: report a safe,
        // classified failure instead of leaving the promise dangling.
        log::error!("terminal_run blocking task failed: {err}");
        emit_terminal_done(&app, run_id, false, false, false);
        CommandError::new(ErrorKind::Request, "the terminal command could not be run")
    })?;
    let (truncated, killed, success) = resolve_terminal_done_flags(&outcome, stream_done);
    emit_terminal_done(&app, run_id, truncated, killed, success);
    match outcome {
        Ok(executed) => Ok(TerminalRunResponse {
            run_id,
            output: executed.output,
            truncated,
            success: executed.success,
        }),
        Err(err) => Err(CommandError::from(err)),
    }
}

/// Stop the active terminal run, if any: cancels its token so the executor
/// kills the child promptly. Returns `true` when a run was active.
#[tauri::command]
pub(crate) fn terminal_kill(registry: State<'_, ManagedTerminal>) -> bool {
    registry.cancel_active()
}

/// Explain one failed terminal run: diagnose the capped failed `output`
/// (plus the short `exit_context` display line, e.g. the exit badge text)
/// through the existing AI execution path (keyring-only credentials,
/// nothing persisted) and return the diagnosis plus a copy-only fix
/// suggestion — never auto-applied.
///
/// Like `send_message` and `git_generate_commit_message`, the provider round
/// trip is blocking end to end, so the body runs on the runtime's dedicated
/// blocking pool. The failed output may contain secrets: it is capped and
/// truncated, never logged, and never echoed by the classified error.
///
/// # Errors
///
/// Classified [`CommandError`]s for empty input (`InvalidInput`) or AI
/// execution failures (`Request`: unknown provider, missing credentials,
/// provider failure).
#[tauri::command]
pub(crate) async fn terminal_explain(
    output: String,
    exit_context: Option<String>,
    provider: String,
    model: String,
    app: AppHandle,
) -> Result<ErrorExplanation, CommandError> {
    // Owned handle so the managed state can be reached from the blocking
    // thread (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        crate::application::terminal::explain_terminal_error(
            db.inner(),
            output.as_str(),
            exit_context.as_deref().unwrap_or_default(),
            provider.as_str(),
            model.as_str(),
        )
        .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("terminal_explain blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the error explanation could not be generated",
            ))
        }
    }
}

/// Spawn an interactive workspace shell in a real OS PTY rooted at the
/// effective workspace directory.
///
/// # Errors
///
/// Classified [`CommandError`]s when the workspace root cannot be resolved or
/// the OS PTY / shell fails to start.
#[tauri::command]
pub(crate) async fn terminal_pty_spawn(
    cols: u16,
    rows: u16,
    app: AppHandle,
    db: State<'_, Database>,
    pty: State<'_, ManagedPtyTerminal>,
) -> Result<PtySessionInfo, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    let manager = Arc::clone(pty.inner());
    let app_for_spawn = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        manager
            .spawn_session(app_for_spawn, &root, cols, rows)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("terminal_pty_spawn blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the interactive terminal session could not be started",
            ))
        }
    }
}

/// Write raw UTF-8 input / keystrokes into an active PTY session.
///
/// # Errors
///
/// Classified [`CommandError`]s when the session is missing or the write fails.
#[tauri::command]
pub(crate) fn terminal_pty_write(
    session_id: String,
    data: String,
    pty: State<'_, ManagedPtyTerminal>,
) -> Result<(), CommandError> {
    pty.write_session(&session_id, &data)
        .map_err(CommandError::from)
}

/// Resize an active PTY session's terminal grid.
///
/// # Errors
///
/// Classified [`CommandError`]s when the session is missing or the resize fails.
#[tauri::command]
pub(crate) fn terminal_pty_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    pty: State<'_, ManagedPtyTerminal>,
) -> Result<(), CommandError> {
    pty.resize_session(&session_id, cols, rows)
        .map_err(CommandError::from)
}

/// Terminate an active PTY session and clean up its OS handles. Returns `true`
/// if a live session existed and was terminated.
///
/// # Errors
///
/// Classified [`CommandError`] if the session table lock is unavailable.
#[tauri::command]
pub(crate) fn terminal_pty_kill(
    session_id: String,
    pty: State<'_, ManagedPtyTerminal>,
) -> Result<bool, CommandError> {
    pty.kill_session(&session_id).map_err(CommandError::from)
}

impl From<PtyError> for CommandError {
    fn from(err: PtyError) -> Self {
        match err {
            PtyError::InvalidWorkingDirectory => Self::new(
                ErrorKind::InvalidInput,
                "the workspace directory is not available for the terminal",
            ),
            PtyError::SpawnFailed => Self::new(
                ErrorKind::Io,
                "the interactive terminal session could not be started",
            ),
            PtyError::SessionNotFound => Self::new(
                ErrorKind::NotFound,
                "the terminal session is no longer active",
            ),
            PtyError::WriteFailed => Self::new(
                ErrorKind::Io,
                "could not write input to the terminal session",
            ),
            PtyError::ResizeFailed => {
                Self::new(ErrorKind::Io, "could not resize the terminal session")
            }
            PtyError::InternalLock => {
                Self::new(ErrorKind::Io, "the terminal session state is unavailable")
            }
        }
    }
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

impl From<crate::application::terminal::ExplainError> for CommandError {
    fn from(err: crate::application::terminal::ExplainError) -> Self {
        use crate::application::terminal::ExplainError as Source;
        match err {
            // Both sides stay secret-free: the failed output is never echoed.
            Source::InvalidInput => Self::new(
                ErrorKind::InvalidInput,
                "the failed output to explain is invalid",
            ),
            Source::Request(inner) => Self::from(inner),
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
            SERVICE.contains("execute_terminal("),
            "the terminal service must dispatch through the tool registry"
        );
        assert!(
            SERVICE.contains("\"execute_command\""),
            "the terminal service must reuse the execute_command tool"
        );
    }

    /// Regression pin for NEX-SEC-004: the old caller-supplied gate shape
    /// must be gone — `terminal_run` takes a minted `confirmation_id`, never
    /// a boolean, on both the command and the service side. A bare-IPC call
    /// with the old shape then fails deserialization before anything can
    /// execute. Needles are built with `concat!` so this test's own source
    /// never matches them verbatim.
    #[test]
    fn terminal_run_no_longer_accepts_a_caller_supplied_boolean() {
        const SOURCE: &str = include_str!("terminal.rs");
        const SERVICE: &str = include_str!("../application/terminal.rs");
        let needle = concat!("confirmed", ": bool");
        assert!(
            !SOURCE.contains(needle),
            "commands/terminal.rs must not take a caller-supplied boolean, found {needle:?}"
        );
        assert!(
            !SERVICE.contains(needle),
            "application/terminal.rs must not take a caller-supplied boolean, found {needle:?}"
        );
        assert!(
            SOURCE.contains("confirmation_id"),
            "terminal_run must take a minted confirmation id"
        );
    }

    #[test]
    fn explain_error_mapping_is_classified_and_secret_free() {
        use crate::application::terminal::ExplainError as Source;
        let mapped = CommandError::from(Source::InvalidInput);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert_eq!(mapped.message, "the failed output to explain is invalid");
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(Source::Request(
            crate::application::execution::RequestError::UnknownProvider {
                name: "openai".to_string(),
            },
        ));
        assert_eq!(mapped.kind, ErrorKind::Request);
        assert!(safe_message(&mapped));
    }

    /// Static wiring check: the explain path must reuse the shared AI
    /// execution boundary (`RequestExecutionService`) with a tool-free
    /// request — no new provider plumbing — and the suggestion must never
    /// reach an execute path (no auto-apply of AI fixes).
    #[test]
    fn terminal_explain_reuses_the_shared_ai_boundary_without_auto_run() {
        const SOURCE: &str = include_str!("terminal.rs");
        const SERVICE: &str = include_str!("../application/terminal.rs");
        assert!(
            SERVICE.contains("RequestExecutionService::new("),
            "the explain service must execute through the shared boundary"
        );
        for needle in ["terminal_explain", "explain_terminal_error"] {
            assert!(
                SOURCE.contains(needle) || SERVICE.contains(needle),
                "the explain path must exist, missing {needle:?}"
            );
        }
        // The explain result (explanation / suggested fix) must never be
        // fed back into a run: no `terminalRun(` / `terminal_run` call may
        // originate from the explain result on either side. Needles are
        // built with `concat!` so this test's own source never matches
        // them verbatim.
        for needle in [
            concat!("suggested_fix", "_for_run"),
            concat!("auto", "_apply"),
            concat!("apply", "_fix"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "no auto-apply path may exist, found {needle:?}"
            );
            assert!(
                !SERVICE.contains(needle),
                "no auto-apply path may exist, found {needle:?}"
            );
        }
    }

    #[test]
    fn terminal_done_flags_preserve_truncated_on_cancel_and_timeout() {
        use crate::application::terminal::{TerminalError, TerminalExecuted};

        let mut stream_done = None;
        let payload = translate_terminal_update(
            7,
            CommandStreamUpdate::Done {
                truncated: true,
                killed: true,
            },
            &mut stream_done,
        );
        assert!(payload.is_none());
        assert_eq!(stream_done, Some((true, true)));

        let (truncated, killed, success) =
            resolve_terminal_done_flags(&Err(TerminalError::Cancelled), stream_done);
        assert!(truncated, "cancelled run must preserve truncated=true");
        assert!(killed, "cancelled run must report killed=true");
        assert!(!success, "cancelled run must report success=false");

        let (truncated, killed, success) =
            resolve_terminal_done_flags(&Err(TerminalError::Timeout), stream_done);
        assert!(truncated, "timed-out run must preserve truncated=true");
        assert!(killed, "timed-out run must report killed=true");
        assert!(!success, "timed-out run must report success=false");

        let (truncated, killed, success) = resolve_terminal_done_flags(
            &Ok(TerminalExecuted {
                output: "ok".to_string(),
                truncated: false,
                success: true,
            }),
            Some((true, false)),
        );
        assert!(truncated);
        assert!(!killed);
        assert!(success);
    }

    #[test]
    fn pty_error_mapping_is_classified_and_secret_free() {
        let cases = [
            (PtyError::InvalidWorkingDirectory, ErrorKind::InvalidInput),
            (PtyError::SpawnFailed, ErrorKind::Io),
            (PtyError::SessionNotFound, ErrorKind::NotFound),
            (PtyError::WriteFailed, ErrorKind::Io),
            (PtyError::ResizeFailed, ErrorKind::Io),
            (PtyError::InternalLock, ErrorKind::Io),
        ];
        for (source, kind) in cases {
            let mapped = CommandError::from(source);
            assert_eq!(mapped.kind, kind);
            assert!(!mapped.message.is_empty());
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }
}
