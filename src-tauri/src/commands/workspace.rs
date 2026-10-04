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

use crate::application::routing::{profile_file_name, RoutingProfile, TaskKind};
use crate::application::settings::SettingsService;
use crate::application::workspace::{
    list_roots, parse_recent, push_recent, register_root, resolve_workspace_root, unregister_root,
    validate_workspace_root, RootsList, WORKSPACE_RECENT_KEY, WORKSPACE_ROOT_KEY,
};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Default workspace location: the pre-picker `agent_workspace` directory
/// under the app-data dir. Used when no valid `agent.workspace_root` setting
/// exists (the pre-picker behavior). Shared with the version-control
/// commands, which resolve the same effective workspace root.
pub(crate) fn default_root(app: &AppHandle) -> Result<PathBuf, CommandError> {
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

/// List the multi-root registry: the active root plus every registered root.
///
/// Read-only: the active entry is the resolved effective root (the stored
/// `agent.workspace_root` when it still validates, else the default
/// `agent_workspace` directory), followed by the stored registry entries
/// de-duplicated. Every root-aware feature (git panel, audit, terminal,
/// agent runs, GitHub lists, flags, diagnostics) follows the active entry.
#[tauri::command]
pub(crate) fn roots_list(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<RootsList, CommandError> {
    let fallback = default_root(&app)?;
    Ok(list_roots(db.inner(), &fallback))
}

/// Register `path` as a root and make it the active root (idempotent).
///
/// Validates with the workspace guard (must exist, canonicalized, no UNC /
/// system / drive-root) and refuses nesting inside (or around) an existing
/// root with fixed vocabulary. Returns the updated registry view so the
/// switcher refreshes in one round trip.
#[tauri::command]
pub(crate) fn roots_add(
    path: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<RootsList, CommandError> {
    let fallback = default_root(&app)?;
    register_root(db.inner(), path.as_str())
        .map_err(|err| CommandError::new(ErrorKind::InvalidInput, err.to_string()))?;
    Ok(list_roots(db.inner(), &fallback))
}

/// Unregister `path` from the registry and return the updated view.
///
/// Removing the active root clears it, so resolution falls back to the
/// default root; every root-aware feature follows on its next manual refresh.
/// Removal never deletes directories — registry bookkeeping only.
#[tauri::command]
pub(crate) fn roots_remove(
    path: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<RootsList, CommandError> {
    let fallback = default_root(&app)?;
    unregister_root(db.inner(), path.as_str(), &fallback)
        .map_err(|err| CommandError::new(ErrorKind::InvalidInput, err.to_string()))
}

/// Initialize the workspace `.nexora/` project directory (idempotent).
///
/// Resolves the effective workspace root (the stored `agent.workspace_root`
/// or the default `agent_workspace` directory) and creates `.nexora/` there
/// with its manifest, ignore file, and `profiles/` scaffold — only on this
/// explicit call, never implicitly. Existing files are never overwritten, and
/// every write is guarded to stay inside the workspace. Returns the `.nexora/`
/// directory path.
#[tauri::command]
pub(crate) fn nexora_init(app: AppHandle, db: State<'_, Database>) -> Result<String, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    let dir =
        crate::application::project_dir::init_nexora_dir(&root).map_err(CommandError::from)?;
    Ok(dir.to_string_lossy().to_string())
}

/// Persist one workspace-scoped routing profile document (`task` is `"chat"`
/// or `"agent"`) as `.nexora/profiles/<task>.json` under the effective
/// workspace root.
///
/// The document is validated with the routing single source
/// ([`RoutingProfile::from_json`]) before anything is written: invalid
/// documents are refused with a secret-free error and leave any existing file
/// untouched. The task label mirrors the checkpoint-label rule (run-snapshot
/// accessories): it is caller-chosen input, so an unknown label fails with
/// fixed vocabulary that never echoes it. `.nexoraignore` is never consulted
/// (`profiles/` is Nexora-owned config, not user content). Returns the written
/// file path.
#[tauri::command]
pub(crate) fn save_workspace_profile(
    task: String,
    document: String,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<String, CommandError> {
    let kind = TaskKind::parse(task.as_str()).ok_or_else(|| {
        CommandError::new(ErrorKind::InvalidInput, "unknown workspace profile task")
    })?;
    RoutingProfile::from_json(document.as_str()).map_err(|_| {
        CommandError::new(ErrorKind::InvalidData, "the workspace profile is invalid")
    })?;
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    let file_name = profile_file_name(kind);
    crate::application::project_dir::save_profile_file(&root, file_name, document.as_str())
        .map_err(CommandError::from)?;
    Ok(root
        .join(crate::application::project_dir::NEXORA_DIR_NAME)
        .join(crate::application::project_dir::PROFILES_DIR_NAME)
        .join(file_name)
        .to_string_lossy()
        .to_string())
}
