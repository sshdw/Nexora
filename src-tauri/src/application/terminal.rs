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
//! requests the approval; the backend still requires a single-use
//! confirmation id minted by the `request_confirmation` command *after a
//! blocking native OS dialog the user accepted*, and bound to this exact
//! command + working directory (the server-side confirmation gate shared with
//! data management — [`TerminalError::Unconfirmed`]). A caller-supplied
//! boolean could be forged by any IPC caller (NEX-SEC-004); an id the user
//! approved for one command cannot authorize a different one.
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
use crate::application::confirmations::{ConfirmationRegistry, ConfirmationTarget, SCOPE_TERMINAL};
use crate::application::execution::{AiMessage, AiRequest, AiRole, RequestError, ToolCall};
use crate::infrastructure::database::Database;

use super::agent::runner::DEFAULT_REQUEST_TIMEOUT;

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
    /// No valid single-use confirmation id was presented (unknown, expired,
    /// already consumed, or minted for another scope).
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

/// Bracketed marker the tool path appends after non-empty output for
/// non-zero exits (`executor::execute_command_with_limits`): its presence
/// means failure. The leading `[` anchors on the executor's exact render
/// so a passing command that merely echoes the words cannot forge a
/// failure badge.
const EXIT_MARKER_BRACKETED: &str = "[command exited with status ";

/// Prefix of the tool path's empty-output render for non-zero exits with
/// no captured output (`command exited with status {status}` as the whole
/// trailing line): matched as a trailing line only, so a mid-output echo
/// of the words cannot forge it either. Keeps the trailing space — a bare
/// `echo` of the phrase with no status after it stays success.
const EXIT_MARKER: &str = "command exited with status ";

/// True when `output` carries the tool path's non-zero-exit render (either
/// the bracketed form after non-empty output or the bare form as the whole
/// trailing line for empty output).
fn exited_nonzero(output: &str) -> bool {
    if output.contains(EXIT_MARKER_BRACKETED) {
        return true;
    }
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with(EXIT_MARKER))
}

/// Truncation markers the tool path's `truncate_output` renders
/// (`tools::output`): either means the output was capped with a notice.
const TRUNCATE_MARKERS: [&str; 2] = ["[output truncated,", "[truncated: "];

/// Run `command` in `workspace_root` (optionally scoped to the
/// workspace-relative `cwd`) through the existing `execute_command` tool.
///
/// `confirmation_id` must be a live single-use id minted for the terminal
/// scope by the `request_confirmation` command and bound to this exact
/// `command` + `cwd`; it is consumed atomically before execution, so a
/// forged, expired, replayed, cross-scope, or different-command id refuses
/// with [`TerminalError::Unconfirmed`] and runs nothing.
///
/// Inputs are validated *before* the id is consumed, so a rejected command or
/// working directory leaves the confirmation usable (the user is not charged
/// an approval for a typo). `token` is the run's cancellation token (kill
/// path). See the module docs for the inherited limits.
pub(crate) fn execute_terminal(
    workspace_root: &Path,
    command: &str,
    cwd: Option<&str>,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
    token: &CancellationToken,
) -> Result<TerminalExecuted, TerminalError> {
    // Validate inputs BEFORE consuming: a rejected command or working
    // directory must leave the user's confirmation id usable, so a typo in
    // `cwd` never costs them a fresh native-dialog approval. The tool path
    // re-validates everything below on the authoritative pass.
    if command.trim().is_empty() {
        return Err(TerminalError::InvalidInput(
            "the terminal command must not be empty".to_string(),
        ));
    }
    if let Some(dir) = cwd.filter(|dir| !dir.trim().is_empty()) {
        if std::path::Path::new(dir).is_absolute() {
            return Err(TerminalError::InvalidInput(
                "the terminal working directory is outside the workspace".to_string(),
            ));
        }
        if dir.split(['/', '\\']).any(|segment| segment == "..") {
            return Err(TerminalError::InvalidInput(
                "the terminal working directory is outside the workspace".to_string(),
            ));
        }
    }
    let arguments = match cwd {
        Some(dir) => serde_json::json!({ "command": command, "cwd": dir }),
        None => serde_json::json!({ "command": command }),
    };
    // Consume LAST, after every input check has passed.
    let target = ConfirmationTarget::in_cwd(command, cwd);
    if !confirmations.consume(SCOPE_TERMINAL, confirmation_id, &target) {
        return Err(TerminalError::Unconfirmed);
    }
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
            success: !exited_nonzero(&output),
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

// ---------------------------------------------------------------------------
// Error intelligence: AI diagnosis of failed runs
// ---------------------------------------------------------------------------

/// Cap for the failed-output excerpt fed to the explain prompt — the same
/// 64 KiB summary shape as the commit-message path. Longer output is cut
/// with a notice, and the cut is flagged to the model and the caller.
pub(crate) const MAX_EXPLAIN_BYTES: usize = 64 * 1024;

/// Longest exit-context string kept (the caller sends a short display line
/// such as the exit badge text; anything longer is cut, never an error).
const MAX_EXIT_CONTEXT_CHARS: usize = 512;

/// Longest diagnosis / suggested-fix text returned, in characters.
const MAX_EXPLANATION_CHARS: usize = 4000;

/// AI diagnosis of one failed terminal run plus a copy-only fix suggestion.
/// Nothing is persisted; the failed output is sent to the provider once and
/// dropped with the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ErrorExplanation {
    /// What went wrong, in plain language.
    pub explanation: String,
    /// What to try next, as copyable text (never auto-applied).
    pub suggested_fix: String,
    /// Whether the failed output fed to the model was cut at
    /// [`MAX_EXPLAIN_BYTES`].
    pub truncated_input: bool,
}

/// Secret-free failures for error explanation: the failed output may carry
/// user secrets, so no variant echoes caller content — not in `Display`, not
/// in logs, not across IPC.
#[derive(Debug)]
pub(crate) enum ExplainError {
    /// The failed output was empty (the exit context is only the fixed
    /// badge line on this path and can never substitute for output).
    InvalidInput,
    /// The AI request failed (unknown provider, missing credentials,
    /// provider failure). Carries no prompt or output content.
    Request(RequestError),
}

impl std::fmt::Display for ExplainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput => write!(f, "the failed output to explain is invalid"),
            Self::Request(_) => write!(f, "the error explanation could not be generated"),
        }
    }
}

impl std::error::Error for ExplainError {}

impl From<RequestError> for ExplainError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// Narrow prompt turning one failed run's output into a diagnosis plus a
/// fix suggestion. The model must reply with exactly two sections —
/// `Diagnosis:` then `Suggested fix:` — and plain text only (no code fences
/// needed, though fences are stripped by the sanitizer anyway).
pub(crate) fn build_error_prompt(output: &str, exit_context: &str, truncated: bool) -> String {
    let mut prompt = String::from(
        "Explain why the terminal command below failed and suggest a fix.\n\
         Reply with ONLY two sections, each a short plain-text paragraph:\n\
         `Diagnosis:` what went wrong, then `Suggested fix:` one concrete command \
         or edit to try next. Do not run anything; describe only.\n",
    );
    if truncated {
        prompt.push_str(
            "Note: the failed output was truncated to fit; diagnose only what is shown.\n",
        );
    }
    if !exit_context.trim().is_empty() {
        prompt.push_str("Exit context: ");
        prompt.push_str(exit_context.trim());
        prompt.push('\n');
    }
    prompt.push_str("Failed output:\n");
    prompt.push_str(output);
    prompt
}

/// Cut `text` to `max_chars` characters on a char boundary.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_string()
}

/// Coerce raw model output into a `(diagnosis, suggested fix)` pair: strip
/// NULs and code fences, split on the first `Suggested fix` header line,
/// and fall back to fixed-vocabulary text for any missing half. Both halves
/// are capped at [`MAX_EXPLANATION_CHARS`].
pub(crate) fn sanitize_explanation(raw: &str) -> (String, String) {
    // Strip NULs upfront so neither half can carry one through.
    let without_nul = raw.replace('\0', "");
    let all: Vec<&str> = without_nul
        .lines()
        .map(str::trim_end)
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != "```" && !trimmed.starts_with("```")
        })
        .collect();
    // Drop leading prose before an explicit `Diagnosis:` / `Explanation:`
    // header (mirrors the commit-message sanitizer, which takes the first
    // conventional line): a model that chats first still parses.
    let lines: Vec<&str> = match all.iter().position(|line| {
        let lowered = line.trim().to_lowercase();
        lowered.starts_with("diagnosis") || lowered.starts_with("explanation")
    }) {
        Some(index) => all[index..].to_vec(),
        None => all,
    };
    let fix_at = lines
        .iter()
        .position(|line| line.trim().to_lowercase().starts_with("suggested fix"));
    let (diagnosis_lines, fix_lines) = match fix_at {
        Some(index) => (&lines[..index], &lines[index..]),
        None => (lines.as_slice(), &[][..]),
    };
    // Drop a leading `Diagnosis:` / `Explanation:` header line, keeping any
    // inline content after the colon.
    let mut diagnosis: Vec<&str> = diagnosis_lines.to_vec();
    if let Some(first) = diagnosis.first() {
        let lowered = first.trim().to_lowercase();
        if lowered.starts_with("diagnosis") || lowered.starts_with("explanation") {
            match first.split_once(':') {
                Some((_, rest)) if !rest.trim().is_empty() => {
                    diagnosis[0] = rest.trim();
                }
                _ => {
                    diagnosis.remove(0);
                }
            }
        }
    }
    // Drop the `Suggested fix:` header line itself, keeping inline content.
    let mut suggested: Vec<&str> = fix_lines.to_vec();
    if let Some(first) = suggested.first() {
        match first.split_once(':') {
            Some((_, rest)) if !rest.trim().is_empty() => {
                suggested[0] = rest.trim();
            }
            _ => {
                suggested.remove(0);
            }
        }
    }
    let trim_blanks = |lines: &[&str]| -> String {
        let start = lines
            .iter()
            .position(|line| !line.trim().is_empty())
            .unwrap_or(lines.len());
        let end = lines
            .iter()
            .rposition(|line| !line.trim().is_empty())
            .map_or(0, |index| index + 1);
        if start < end {
            lines[start..end].join("\n").trim().to_string()
        } else {
            String::new()
        }
    };
    let mut explanation = trim_blanks(&diagnosis);
    if explanation.is_empty() {
        explanation =
            "The command failed; the cause could not be determined from the output.".to_string();
    }
    let mut fix = trim_blanks(&suggested);
    if fix.is_empty() {
        fix =
            "No suggested fix was produced — review the diagnosis and retry manually.".to_string();
    }
    (
        truncate_chars(&explanation, MAX_EXPLANATION_CHARS),
        truncate_chars(&fix, MAX_EXPLANATION_CHARS),
    )
}

/// Explain one failed terminal run through the existing AI execution path:
/// the failed output (capped at [`MAX_EXPLAIN_BYTES`]) feeds a narrow
/// prompt to a single text-only [`AiRequest`] executed by the shared
/// [`RequestExecutionService`](crate::application::execution::RequestExecutionService)
/// — the same execution boundary `send_message`, the agent-run bridge, and
/// commit-message generation use. Credentials resolve from the OS keyring
/// only; no new key input exists on this path, and nothing is persisted.
///
/// The failed output may contain secrets: it is capped and truncated, never
/// logged, and never echoed by [`ExplainError`].
///
/// # Errors
///
/// Returns [`ExplainError::InvalidInput`] when the output is empty (the
/// exit context is only the fixed badge line on this path and can never
/// substitute for output), [`ExplainError::Request`] when AI execution fails.
pub(crate) fn explain_terminal_error(
    db: &Database,
    output: &str,
    exit_context: &str,
    provider: &str,
    model: &str,
) -> Result<ErrorExplanation, ExplainError> {
    use crate::application::execution::RequestExecutionService;
    if output.trim().is_empty() {
        return Err(ExplainError::InvalidInput);
    }
    let context = truncate_chars(exit_context.trim(), MAX_EXIT_CONTEXT_CHARS);
    let (excerpt, truncated) = if output.len() > MAX_EXPLAIN_BYTES {
        let mut end = MAX_EXPLAIN_BYTES;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        (output[..end].to_string(), true)
    } else {
        (output.to_string(), false)
    };
    let prompt = build_error_prompt(&excerpt, &context, truncated);
    let request = AiRequest {
        provider: provider.to_string(),
        model: model.to_string(),
        messages: vec![AiMessage {
            role: AiRole::User,
            content: prompt,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }],
        tools: Vec::new(),
        model_config: None,
        request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
    };
    let response = RequestExecutionService::new(db).execute(&request)?;
    let (explanation, suggested_fix) = sanitize_explanation(&response.content);
    Ok(ErrorExplanation {
        explanation,
        suggested_fix,
        truncated_input: truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::temp_workspace;
    use crate::application::confirmations::{
        ConfirmationRegistry, ConfirmationTarget, SCOPE_DATA_MANAGEMENT, SCOPE_TERMINAL,
    };

    const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];

    /// Mint an id bound to the exact operation a test is about to run.
    fn mint_for(confirmations: &ConfirmationRegistry, command: &str, cwd: Option<&str>) -> String {
        confirmations
            .request(SCOPE_TERMINAL, &ConfirmationTarget::in_cwd(command, cwd))
            .expect("mint confirmation id")
    }

    /// Mint an id for the canonical `"echo terminal-ok"` run the happy-path
    /// tests share.
    fn mint_id(confirmations: &ConfirmationRegistry) -> String {
        mint_for(confirmations, "echo terminal-ok", None)
    }

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
        let confirmations = ConfirmationRegistry::new();
        for forged in ["", "forged-id", "confirm", "true"] {
            let err = execute_terminal(&ws, "echo hi", None, forged, &confirmations, &token)
                .expect_err("forged confirmation must be refused");
            assert_eq!(err, TerminalError::Unconfirmed);
        }
        // A real id for a *different* command is refused just like a forgery:
        // the id is bound to the operation the user actually approved.
        let other = mint_for(&confirmations, "echo something-else", None);
        let err = execute_terminal(&ws, "echo hi", None, &other, &confirmations, &token)
            .expect_err("an id minted for another command must be refused");
        assert_eq!(err, TerminalError::Unconfirmed);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn cross_scope_id_is_refused_without_execution() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let foreign = confirmations
            .request(
                SCOPE_DATA_MANAGEMENT,
                &ConfirmationTarget::new(crate::application::confirmations::OP_CLEAR_ALL_DATA),
            )
            .expect("mint confirmation id");
        let err = execute_terminal(&ws, "echo hi", None, &foreign, &confirmations, &token)
            .expect_err("cross-scope id must be refused");
        assert_eq!(err, TerminalError::Unconfirmed);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn consumed_id_cannot_be_replayed() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let id = mint_id(&confirmations);
        execute_terminal(&ws, "echo terminal-ok", None, &id, &confirmations, &token)
            .expect("first use runs");
        let err = execute_terminal(&ws, "echo terminal-ok", None, &id, &confirmations, &token)
            .expect_err("replayed id must be refused");
        assert_eq!(err, TerminalError::Unconfirmed);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn empty_command_is_invalid() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        for command in ["", "   "] {
            let id = mint_for(&confirmations, command, None);
            let err = execute_terminal(&ws, command, None, &id, &confirmations, &token)
                .expect_err("empty command must be refused");
            assert!(
                matches!(err, TerminalError::InvalidInput(_)),
                "unexpected: {err:?}"
            );
            assert_secret_free(&err.to_string());
            // Invalid input refuses before consuming: the id stays live.
            assert!(
                confirmations.consume(SCOPE_TERMINAL, &id, &ConfirmationTarget::new(command)),
                "refused input must not burn the confirmation id"
            );
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn real_command_runs_in_workspace_scope() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let executed = execute_terminal(
            &ws,
            "echo terminal-ok",
            None,
            &mint_id(&confirmations),
            &confirmations,
            &token,
        )
        .expect("echo runs");
        assert!(executed.success);
        assert!(!executed.truncated);
        assert!(executed.output.contains("terminal-ok"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn failing_command_reports_no_success_with_marker() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        // Non-zero exits still return output (with the tool path's marker),
        // never an error — the panel renders output + a non-zero badge.
        let executed = execute_terminal(
            &ws,
            "exit 1",
            None,
            &mint_for(&confirmations, "exit 1", None),
            &confirmations,
            &token,
        )
        .expect("exit 1 runs");
        assert!(!executed.success);
        assert!(executed.output.contains(EXIT_MARKER));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn echoing_the_exit_words_with_zero_exit_stays_success() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        // Regression: success was a bare substring test, so a passing
        // command echoing the marker words forged a failure badge.
        let executed = execute_terminal(
            &ws,
            "echo command exited with status",
            None,
            &mint_for(&confirmations, "echo command exited with status", None),
            &confirmations,
            &token,
        )
        .expect("echo runs");
        assert!(
            executed.success,
            "zero-exit echo must not forge failure: {:?}",
            executed.output
        );
        assert!(executed.output.contains("command exited with status"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn absolute_escape_cwd_is_refused_secret_free() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let outside = if cfg!(windows) {
            "C:\\Windows\\System32"
        } else {
            "/etc"
        };
        let id = mint_for(&confirmations, "echo hi", Some(outside));
        let err = execute_terminal(&ws, "echo hi", Some(outside), &id, &confirmations, &token)
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
        // The PathTraversal refusal happens before the consume, so the user's
        // confirmation survives a bad working directory.
        assert!(
            confirmations.consume(
                SCOPE_TERMINAL,
                &id,
                &ConfirmationTarget::in_cwd("echo hi", Some(outside)),
            ),
            "a rejected cwd must not burn the confirmation id"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn dotdot_escape_cwd_is_refused() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let err = execute_terminal(
            &ws,
            "echo hi",
            Some("../.."),
            &mint_for(&confirmations, "echo hi", Some("../..")),
            &confirmations,
            &token,
        )
        .expect_err("dotdot escape must be refused");
        assert!(
            matches!(err, TerminalError::InvalidInput(_)),
            "unexpected: {err:?}"
        );
        assert_secret_free(&err.to_string());
        // The refused id is still spendable on the same operation — the
        // user is never charged an approval for a rejected input.
        let id = mint_for(&confirmations, "echo hi", Some("../.."));
        let _ = execute_terminal(&ws, "echo hi", Some("../.."), &id, &confirmations, &token)
            .expect_err("still refused");
        assert!(
            confirmations.consume(
                SCOPE_TERMINAL,
                &id,
                &ConfirmationTarget::in_cwd("echo hi", Some("../..")),
            ),
            "a rejected cwd must not burn the confirmation id"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// An id minted for one working directory must not authorize a run in
    /// another (the cwd is part of the binding, not just the command).
    #[test]
    fn id_minted_for_another_working_directory_is_refused() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let confirmations = ConfirmationRegistry::new();
        let id = mint_for(&confirmations, "echo hi", Some("sub"));
        let err = execute_terminal(&ws, "echo hi", Some("other"), &id, &confirmations, &token)
            .expect_err("an id minted for another cwd must be refused");
        assert_eq!(err, TerminalError::Unconfirmed);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn pre_cancelled_token_aborts_secret_free() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        token.cancel();
        let confirmations = ConfirmationRegistry::new();
        let err = execute_terminal(
            &ws,
            "echo hi",
            None,
            &mint_for(&confirmations, "echo hi", None),
            &confirmations,
            &token,
        )
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
        let confirmations = ConfirmationRegistry::new();
        let err = execute_terminal(
            &ws,
            secret_command,
            Some("/etc"),
            &mint_for(&confirmations, secret_command, Some("/etc")),
            &confirmations,
            &token,
        )
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

    #[test]
    fn error_prompt_flags_truncation_and_carries_context() {
        let prompt = build_error_prompt("boom output", "non-zero exit", false);
        assert!(prompt.contains("Diagnosis:"));
        assert!(prompt.contains("Suggested fix:"));
        assert!(prompt.contains("boom output"));
        assert!(prompt.contains("non-zero exit"));
        assert!(!prompt.contains("truncated to fit"));
        let prompt = build_error_prompt("boom output", "non-zero exit", true);
        assert!(prompt.contains("truncated to fit"));
    }

    #[test]
    fn sanitize_explanation_splits_sections_with_fallbacks() {
        let (diagnosis, fix) =
            sanitize_explanation("Diagnosis: missing file\n\nSuggested fix: touch it");
        assert_eq!(diagnosis, "missing file");
        assert_eq!(fix, "touch it");
        // Fences and prose around the sections are stripped, not echoed.
        let (diagnosis, fix) = sanitize_explanation(
            "Sure! ```\nDiagnosis: bad flag\n\nSuggested fix:\n```\nuse --help\n```",
        );
        assert_eq!(diagnosis, "bad flag");
        assert_eq!(fix, "use --help");
        // A missing half falls back to fixed vocabulary.
        let (diagnosis, fix) = sanitize_explanation("just some prose");
        assert!(!diagnosis.is_empty());
        assert!(fix.contains("review the diagnosis"));
        let (diagnosis, _) = sanitize_explanation("Suggested fix: only a fix");
        assert!(diagnosis.contains("could not be determined"));
        // NULs never pass through.
        let (diagnosis, fix) = sanitize_explanation("Diagnosis: a\0b\n\nSuggested fix: c\0d");
        assert!(!diagnosis.contains('\0'));
        assert!(!fix.contains('\0'));
        // Overlong halves are capped.
        let long = "x".repeat(MAX_EXPLANATION_CHARS + 100);
        let (diagnosis, _) =
            sanitize_explanation(&format!("Diagnosis: {long}\n\nSuggested fix: ok"));
        assert!(diagnosis.chars().count() <= MAX_EXPLANATION_CHARS);
    }

    #[test]
    fn explain_refuses_empty_input_secret_free() {
        // Empty output refuses with fixed vocabulary, even when the fixed
        // badge-line context is present (context can never substitute for
        // output on this path). No database or provider is touched:
        // `explain_terminal_error` validates before any request is built,
        // so this needs no keyring.
        let db = crate::infrastructure::database::in_memory_database();
        for (output, context) in [
            ("", ""),
            ("   ", "  "),
            ("", "non-zero exit"),
            ("   ", "non-zero exit"),
        ] {
            let err = explain_terminal_error(&db, output, context, "openai", "m")
                .expect_err("empty input must be refused");
            assert!(
                matches!(err, ExplainError::InvalidInput),
                "unexpected: {err:?}"
            );
            assert_eq!(format!("{err}"), "the failed output to explain is invalid");
        }
        assert_eq!(
            format!(
                "{}",
                ExplainError::Request(
                    crate::application::execution::RequestError::UnknownProvider {
                        name: "x".to_string(),
                    }
                )
            ),
            "the error explanation could not be generated"
        );
    }

    #[test]
    fn oversized_output_is_capped_with_notice() {
        // The cap helper shape: output past the budget cuts on a char
        // boundary and reports truncation (mirrors the commit-summary cap).
        let big = "e".repeat(MAX_EXPLAIN_BYTES + 1024);
        let prompt = build_error_prompt(&big, "", false);
        assert!(prompt.contains(&big[..MAX_EXPLAIN_BYTES]));
        let (excerpt, truncated) = if big.len() > MAX_EXPLAIN_BYTES {
            (big[..MAX_EXPLAIN_BYTES].to_string(), true)
        } else {
            (big.clone(), false)
        };
        assert!(truncated);
        assert!(excerpt.len() <= MAX_EXPLAIN_BYTES);
        let prompt = build_error_prompt(&excerpt, "", truncated);
        assert!(prompt.contains("truncated to fit"));
    }
}
