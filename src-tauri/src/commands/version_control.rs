//! Read-only version-control IPC commands: the Tauri side of git inspection.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves the
//! effective workspace root (the stored `agent.workspace_root` or the default
//! `agent_workspace` directory, exactly like the workspace commands),
//! delegates to the application-layer version-control service
//! ([`crate::application::version_control`]), and maps failures into
//! secret-free [`CommandError`] values. No business logic lives here beyond
//! that translation.
//!
//! Command-shape decision (one feature area, two commands): `git_info`
//! batches branch + status + log in a single round trip because the panel
//! always renders them together, while `git_file_diff` loads each unified
//! diff lazily on selection (diffs are size-capped server-side, but there is
//! no reason to fetch all of them up front).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, State};

use crate::application::version_control::{GitFileDiff, GitInfo};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::CommandError;
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
