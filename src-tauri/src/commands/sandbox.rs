//! Read-only thin IPC translation. The frontend cannot assert sandbox posture.
#![allow(clippy::needless_pass_by_value)] // Tauri owns deserialized handles.
use super::error::CommandError;
use crate::application::agent::sandbox::{self, SandboxStatus};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;
use tauri::{AppHandle, State};
#[tauri::command]
pub(crate) fn get_sandbox_status(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<SandboxStatus, CommandError> {
    let fallback = super::workspace::default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    Ok(sandbox::status(&root))
}
