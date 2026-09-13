//! Workspace-folder IPC commands: the Tauri side of the folder picker.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command delegates to the
//! application-layer workspace guard
//! ([`crate::application::workspace`]) and the existing settings service, and
//! maps failures into secret-free [`CommandError`] values. No business logic
//! lives here beyond that translation.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

use crate::application::settings::SettingsService;
use crate::application::workspace::{
    parse_recent, push_recent, resolve_workspace_root, validate_workspace_root,
    WORKSPACE_RECENT_KEY, WORKSPACE_ROOT_KEY,
};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Default workspace location: the pre-picker `agent_workspace` directory
/// under the app-data dir. Used when no valid `agent.workspace_root` setting
/// exists (the pre-picker behavior).
fn default_root(app: &AppHandle) -> Result<PathBuf, CommandError> {
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

/// Return the effective workspace root for tool scoping: the stored
/// `agent.workspace_root` when it names an existing directory, else the
/// default `agent_workspace` directory.
#[tauri::command]
pub(crate) fn get_workspace_root(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<String, CommandError> {
    let fallback = default_root(&app)?;
    let resolved = resolve_workspace_root(db.inner(), &fallback);
    Ok(resolved.to_string_lossy().to_string())
}

/// Validate `path` with the workspace guard, canonicalize it, persist it as
/// `agent.workspace_root`, prepend it to the 5-entry
/// `agent.workspace_recent` ring, and return the canonical path.
#[tauri::command]
pub(crate) fn set_workspace_root(
    path: String,
    db: State<'_, Database>,
) -> Result<String, CommandError> {
    let canonical = validate_workspace_root(&path)
        .map_err(|err| CommandError::new(ErrorKind::InvalidInput, err.to_string()))?;
    let text = canonical.to_string_lossy().to_string();
    let service = SettingsService::new(db.inner());
    service
        .write(WORKSPACE_ROOT_KEY, Some(text.as_str()))
        .map_err(CommandError::from)?;
    let existing = service
        .read(WORKSPACE_RECENT_KEY)
        .map_err(CommandError::from)?;
    let next = push_recent(existing.as_deref(), text.as_str());
    service
        .write(WORKSPACE_RECENT_KEY, Some(next.as_str()))
        .map_err(CommandError::from)?;
    Ok(text)
}

/// List the recent workspace roots (most-recent first, at most 5).
#[tauri::command]
pub(crate) fn list_workspace_recent(db: State<'_, Database>) -> Result<Vec<String>, CommandError> {
    let service = SettingsService::new(db.inner());
    let raw = service
        .read(WORKSPACE_RECENT_KEY)
        .map_err(CommandError::from)?;
    Ok(parse_recent(raw.as_deref()))
}
