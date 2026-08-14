//! Tauri commands over the existing [`SettingsService`]
//! (Phase 10.2 — Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer settings service (FR-012) and converts its
//! classified errors into safe [`CommandError`] values. Settings are key/value
//! pairs; provider credentials are never stored here (ARCHITECTURE.md §12).

use tauri::State;

use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;

use super::error::CommandError;

/// Read one setting by `key` (`None` when absent).
#[tauri::command]
pub(crate) fn get_setting(
    key: String,
    db: State<'_, Database>,
) -> Result<Option<String>, CommandError> {
    SettingsService::new(db.inner()).read(&key).map_err(Into::into)
}

/// Write one setting by `key` (`value` may be `None` to store a `NULL`).
#[tauri::command]
pub(crate) fn set_setting(
    key: String,
    value: Option<String>,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    SettingsService::new(db.inner())
        .write(&key, value.as_deref())
        .map_err(Into::into)
}

/// Delete one setting by `key` (a no-op when it does not exist).
#[tauri::command]
pub(crate) fn delete_setting(
    key: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    SettingsService::new(db.inner()).delete(&key).map_err(Into::into)
}

/// List every setting as `(key, value)` pairs, ordered by `key`.
#[tauri::command]
pub(crate) fn list_settings(
    db: State<'_, Database>,
) -> Result<Vec<(String, Option<String>)>, CommandError> {
    SettingsService::new(db.inner()).list().map_err(Into::into)
}