//! Version-control IPC commands: the Tauri side of git inspection and guarded
//! writes.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves the
//! effective workspace root (the stored `agent.workspace_root` or the default
//! `agent_workspace` directory, exactly like the workspace commands),
//! delegates to the application-layer version-control service
//! ([`crate::application::version_control`]), and maps failures into
//! secret-free [`CommandError`] values. No business logic lives here beyond
//! that translation.
//!
//! Command-shape decision (one feature area, seven commands): `git_info`
//! batches branch + status + log in a single round trip because the panel
//! always renders them together, while `git_file_diff` loads each unified
//! diff lazily on selection (diffs are size-capped server-side, but there is
//! no reason to fetch all of them up front). The four writes stay separate —
//! `git_stage` / `git_unstage` take a path batch plus an explicit
//! `confirmed` flag, `git_commit` takes the validated message plus
//! `confirmed` and returns the new hash, `git_push` takes the allowlisted
//! remote name plus `confirmed` — because each carries different arguments
//! and the panel invokes them independently. `git_generate_commit_message`
//! takes the provider/model names and runs the blocking AI round trip on the
//! blocking pool (like `send_message`).
//!
//! Approval-gate note: the agent `ApprovalGate` parks live agent tool calls
//! and cannot apply to direct IPC commands (no run, no park, no autonomy
//! mode). Writes therefore reuse the destructive-action confirmation pattern:
//! every write refuses with `ConfirmationRequired` unless its per-call
//! `confirmed` flag is `true` — the same gate shape as data management,
//! not a parallel mechanism.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::version_control::{GeneratedCommitMessage, GitFileDiff, GitInfo};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Aggregate read-only git view for the effective workspace root: current
/// branch, changed files, and the `limit` most recent commits (backend clamps
/// the limit; the frontend passes a small page such as 20).
#[tauri::command]
pub(crate) fn git_info(
    limit: Option<u32>,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<GitInfo, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_info(&root, limit).map_err(CommandError::from)
}

/// Per-file unified diff for `path` (repository-relative), capped server-side
/// with a truncation notice. The path is validated to stay inside the
/// repository workdir; traversal attempts fail with a fixed-vocabulary error.
#[tauri::command]
pub(crate) fn git_file_diff(
    path: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<GitFileDiff, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_file_diff(&root, path.as_str())
        .map_err(CommandError::from)
}

/// Stage `paths` (repository-relative) into the index, returning how many
/// were staged. Refuses with `ConfirmationRequired` unless `confirmed` is
/// `true`; escaping paths fail with a fixed-vocabulary error.
#[tauri::command]
pub(crate) fn git_stage(
    paths: Vec<String>,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<usize, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_stage(&root, &paths, confirmed)
        .map_err(CommandError::from)
}

/// Unstage `paths` (repository-relative) back to `HEAD`, returning how many
/// were unstaged. Same confirmation and path guards as [`git_stage`].
#[tauri::command]
pub(crate) fn git_unstage(
    paths: Vec<String>,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<usize, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_unstage(&root, &paths, confirmed)
        .map_err(CommandError::from)
}

/// Commit the staged index with `message`, returning the new commit hash.
/// The message is validated backend-side (non-empty, bounded, usable subject
/// line); the author comes from the local git config only. Refuses with
/// `ConfirmationRequired` unless `confirmed` is `true`.
#[tauri::command]
pub(crate) fn git_commit(
    message: String,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<String, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_commit(&root, message.as_str(), confirmed)
        .map_err(CommandError::from)
}

/// Push the current branch to `remote` (only `"origin"` is accepted).
/// Never forced: the backend builds a plain fast-forward refspec and no force
/// flag exists on the path. Refuses with `ConfirmationRequired` unless
/// `confirmed` is `true`.
#[tauri::command]
pub(crate) fn git_push(
    remote: String,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::version_control::git_push(&root, remote.as_str(), confirmed)
        .map_err(CommandError::from)
}

/// Generate a conventional-commit message for the staged changes through the
/// existing AI execution path (keyring-only credentials, nothing persisted).
///
/// # Threading
///
/// Like `send_message`, the provider round trip is blocking end to end, so
/// the body runs on the runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn git_generate_commit_message(
    provider: String,
    model: String,
    app: AppHandle,
) -> Result<GeneratedCommitMessage, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::generate_commit_message(
            db.inner(),
            &root,
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
            log::error!("git_generate_commit_message blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the commit message could not be generated",
            ))
        }
    }
}
