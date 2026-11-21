//! Access to the single shared `app_settings` store.
//!
//! The path comes from the same app-data resolver the Desktop uses
//! (`nexora_core::storage::app_data`), and the connection goes through the
//! same migration/bootstrap code. There is no `nexora-cli.db` and no second
//! settings file (brief §3.2, §14).

use std::path::{Path, PathBuf};

use nexora_core::storage::repository::settings::SettingsRepository;
use nexora_core::storage::{app_data, Database};

use crate::error::CliError;

/// Resolve the shared database path, or `None` when the platform has no base
/// directory to resolve it from.
#[must_use]
pub(crate) fn db_path() -> Option<PathBuf> {
    app_data::app_data_dir().map(|dir| dir.join(app_data::DB_FILE_NAME))
}

fn open_db(path: &Path) -> Result<Database, CliError> {
    let conn = nexora_core::storage::open(path).map_err(|_| CliError::database())?;
    Ok(Database::new(conn))
}

/// Read one setting without creating a database file.
///
/// A fresh install has no settings yet, so a missing database file is an
/// honest `None`, not an error and not a file creation side effect. If a
/// database file already exists, opening it may migrate it through the shared
/// forward-only path (`nexora_core::storage::open`), exactly as Desktop does.
pub(crate) fn read(key: &str) -> Result<Option<String>, CliError> {
    let Some(path) = db_path() else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let db = open_db(&path)?;
    SettingsRepository::new(&db)
        .get(key)
        .map_err(|_| CliError::database())
}

/// Write one already-validated setting to the shared store.
pub(crate) fn write(key: &str, value: &str) -> Result<(), CliError> {
    let path = db_path().ok_or_else(|| {
        CliError::validation(
            "the application data directory is unavailable; no setting was changed",
        )
    })?;
    let db = open_db(&path)?;
    SettingsRepository::new(&db)
        .set(key, value)
        .map_err(|_| CliError::database())
}
