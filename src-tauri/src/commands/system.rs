//! Diagnostics / update-check / snapshot IPC commands: the Tauri side of the
//! health-panel System section.
//!
//! Thin translation only (ARCHITECTURE.md §5): the three commands resolve
//! paths from the [`AppHandle`] (the database file and the workspace root,
//! exactly like the version-control and GitHub commands), delegate to the
//! application-layer services ([`crate::application::system`] for the bundle
//! and the snapshot, [`crate::application::github::check_update`] for the
//! release check), and map failures into secret-free [`CommandError`] values.
//! No business logic lives here beyond that translation.
//!
//! Read-only except the snapshot: `diagnostics_bundle` and `update_check`
//! write nothing; `snapshot_database` writes exactly one new
//! `nexora-backup-<secs>.db` file under the app-data `backups` directory on
//! the user's explicit button press (never silently, never on a timer).
//! There is no download and no install anywhere on this path — the update
//! panel renders the release URL as text for the user to open themselves.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

use crate::application::github::UpdateCheck;
use crate::application::system::{DiagnosticsBundle, SnapshotInfo};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Database file name inside the app-data directory (mirrors the startup
/// path in [`crate::run`]).
const DB_FILE_NAME: &str = "nexora.db";

/// Snapshot backups directory name inside the app-data directory.
const BACKUP_DIR_NAME: &str = "backups";

/// Resolve the live database file for size reporting. A missing or
/// unreadable file is NOT an error: the bundle reports `db_size_bytes` as
/// unknown instead of failing the whole call.
fn database_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    Some(dir.join(DB_FILE_NAME))
}

/// Collect the secret-free diagnostics bundle (versions, platform, storage
/// counts, crash evidence). Read-only: counts and versions only, never
/// content, credentials, SQL, or paths.
#[tauri::command]
pub(crate) fn diagnostics_bundle(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<DiagnosticsBundle, CommandError> {
    let db_size_bytes = database_file(&app)
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len());
    crate::application::system::collect_bundle(db.inner(), db_size_bytes)
        .map_err(CommandError::from)
}

/// Check the workspace `origin` repo's latest GitHub release against the
/// running build. Read-only check only: one GET, no download, no install.
///
/// Like the GitHub list commands, the blocking HTTP round trip runs on the
/// runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn update_check(app: AppHandle) -> Result<UpdateCheck, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let result = crate::application::github::check_update(&root).map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful check — a ledger failure never fails the check.
            crate::application::privacy::record(db.inner(), "update_check");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("update_check blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the update check could not be completed",
            ))
        }
    }
}

/// Write a consistent pre-update snapshot copy of the database (`VACUUM
/// INTO`) under the app-data `backups` directory. The user presses the
/// button explicitly — there is no silent or scheduled write; the response
/// carries the file name (never the full path) plus size.
#[tauri::command]
pub(crate) fn snapshot_database(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<SnapshotInfo, CommandError> {
    let backup_dir = app
        .path()
        .app_data_dir()
        .map(|dir| dir.join(BACKUP_DIR_NAME))
        .map_err(|err| {
            log::error!("snapshot_database app-data dir failed: {err}");
            CommandError::new(
                ErrorKind::Io,
                "the pre-update snapshot could not be written",
            )
        })?;
    crate::application::system::snapshot_to(db.inner(), &backup_dir).map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::system::SystemError;
    use crate::infrastructure::database::DatabaseError;

    const SOURCE: &str = include_str!("system.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn system_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(SystemError::Io);
        assert_eq!(mapped.kind, ErrorKind::Io);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(SystemError::Database(DatabaseError::Lock(
            "poisoned sk-secret-PLANTED".to_string(),
        )));
        assert_eq!(mapped.kind, ErrorKind::Database);
        assert_eq!(
            mapped.message,
            "a database operation failed; no data was changed"
        );
        assert!(safe_message(&mapped));
        assert!(
            !mapped.message.contains("PLANTED"),
            "lock detail must never leak: {}",
            mapped.message
        );
    }

    /// Static wiring check: the system commands stay thin translation —
    /// path resolution plus service delegation. No process spawning, no
    /// network client of their own (the release GET lives in the github
    /// service), no silent installs. Needles are built with `concat!` so
    /// this test's own source never matches them.
    #[test]
    fn system_commands_stay_thin_translation() {
        for needle in [
            "diagnostics_bundle",
            "update_check",
            "snapshot_database",
            "collect_bundle",
            "check_update",
            "snapshot_to",
        ] {
            assert!(
                SOURCE.contains(needle),
                "the system commands must exist, missing {needle:?}"
            );
        }
        for needle in [
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
            concat!(".", "post("),
            concat!(".", "put("),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/system.rs must not write processes or mutate over HTTP, found {needle:?}"
            );
        }
    }
}
