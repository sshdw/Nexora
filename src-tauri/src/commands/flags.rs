//! Feature-flag IPC command: the read-only 2.0 rollout status view.
//!
//! Thin translation only (ARCHITECTURE.md §5): the command resolves the
//! effective workspace root, delegates to the application-layer
//! [`FlagService`](crate::application::flags::FlagService), and maps
//! failures into secret-free [`CommandError`] values. There is intentionally
//! no setter command: flags are edited where they live (the workspace
//! `.nexora/flags.json` file, or the `flags.*` settings keys through the
//! existing `set_setting` path).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

use crate::application::flags::{FlagService, FlagStatus};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Default workspace location: the pre-picker `agent_workspace` directory
/// under the app-data dir. Used when no valid `agent.workspace_root` setting
/// exists (the pre-picker behavior; mirrors `commands/workspace.rs`).
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

/// Read-only feature-flag status: every registered flag mapped to its
/// effective value plus its fixed-vocabulary source
/// (`workspace` / `global` / `default`).
///
/// Resolution is workspace → global → default with the current behavior as
/// every default, so an unconfigured install reports every flag enabled from
/// `default`. Values and sources are fixed vocabulary and booleans only, so
/// the response is secret-free by construction.
///
/// # Errors
///
/// Returns a classified [`CommandError`] when the app-data dir is
/// unavailable or the settings store cannot be read.
#[tauri::command]
pub(crate) fn flags_status(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<BTreeMap<String, FlagStatus>, CommandError> {
    let fallback = default_root(&app)?;
    let root = crate::application::workspace::resolve_workspace_root(db.inner(), &fallback);
    FlagService::new(db.inner())
        .status(Some(&root))
        .map_err(CommandError::from)
}

impl From<crate::application::flags::FlagError> for CommandError {
    fn from(err: crate::application::flags::FlagError) -> Self {
        match err {
            crate::application::flags::FlagError::InvalidDocument { .. } => Self::new(
                ErrorKind::InvalidData,
                "the workspace flags file is invalid",
            ),
            crate::application::flags::FlagError::Database(inner) => Self::from(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::flags::FLAGS;
    use crate::application::settings::SettingsService;

    fn test_db() -> Database {
        crate::infrastructure::database::in_memory_database()
    }

    /// Drive the exact producer the command delegates to (the command body
    /// needs `State<'_, _>` and cannot be invoked here): the status view over
    /// a workspace root, mirroring the `inspect_run_not_found` pattern.
    fn status_for(db: &Database, ws: Option<&std::path::Path>) -> BTreeMap<String, FlagStatus> {
        FlagService::new(db).status(ws).expect("status succeeds")
    }

    #[test]
    fn status_shape_lists_every_flag_with_fixed_vocab_sources() {
        let db = test_db();
        let status = status_for(&db, None);
        assert_eq!(status.len(), FLAGS.len());
        for flag in FLAGS {
            let entry = status.get(flag.name).expect("every flag listed");
            assert!(entry.enabled, "unconfigured flags report current behavior");
            assert_eq!(entry.source, "default");
            assert!(["workspace", "global", "default"].contains(&entry.source));
        }
        // Response-side snake_case shape.
        let rendered = serde_json::to_string(&status).expect("serialize status");
        for flag in FLAGS {
            assert!(
                rendered.contains(flag.name),
                "flag {:?} in payload",
                flag.name
            );
        }
        assert!(rendered.contains("\"enabled\""));
        assert!(rendered.contains("\"source\""));
        for camel in ["workspaceRoot", "flagStatus", "isEnabled"] {
            assert!(
                !rendered.contains(camel),
                "payload must not use camelCase {camel}"
            );
        }
    }

    #[test]
    fn status_prefers_workspace_over_global() {
        let db = test_db();
        SettingsService::new(&db)
            .write("flags.assembly", Some("false"))
            .expect("seed global");
        let dir = std::env::temp_dir().join(format!("nexora-flags-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp root");
        let ws: PathBuf =
            crate::application::workspace::strip_verbatim(dir.canonicalize().expect("canon"));
        crate::application::project_dir::init_nexora_dir(&ws).expect("init");
        std::fs::write(
            ws.join(".nexora").join("flags.json"),
            r#"{"assembly": true}"#,
        )
        .expect("seed flags");
        let status = status_for(&db, Some(ws.as_path()));
        assert_eq!(
            status["assembly"],
            FlagStatus {
                enabled: true,
                source: "workspace"
            }
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn status_is_secret_free_with_hostile_values() {
        const SENTINEL: &str = "sk-test-sentinel-63cd";
        let db = test_db();
        // A hostile global value degrades to the default without echo.
        SettingsService::new(&db)
            .write("flags.assembly", Some(SENTINEL))
            .expect("seed hostile global");
        // A hostile workspace document falls back without echo.
        let dir =
            std::env::temp_dir().join(format!("nexora-flags-cmd-hostile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp root");
        let ws: PathBuf =
            crate::application::workspace::strip_verbatim(dir.canonicalize().expect("canon"));
        crate::application::project_dir::init_nexora_dir(&ws).expect("init");
        std::fs::write(
            ws.join(".nexora").join("flags.json"),
            format!(r#"{{"ghost-{SENTINEL}": true}}"#),
        )
        .expect("seed hostile flags");
        let status = status_for(&db, Some(ws.as_path()));
        assert_eq!(
            status["assembly"],
            FlagStatus {
                enabled: true,
                source: "default"
            }
        );
        let rendered = serde_json::to_string(&status).expect("serialize status");
        assert!(!rendered.to_lowercase().contains("sk-test-sentinel-63cd"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn flag_error_mapping_is_secret_free_and_classified() {
        let invalid = CommandError::from(crate::application::flags::FlagError::InvalidDocument {
            reason: "feature flags document names an unknown flag",
        });
        assert_eq!(invalid.kind, ErrorKind::InvalidData);
        assert_eq!(invalid.message, "the workspace flags file is invalid");
        let db_err = CommandError::from(crate::application::flags::FlagError::Database(
            crate::infrastructure::database::DatabaseError::Lock("sk-".into()),
        ));
        assert_eq!(db_err.kind, ErrorKind::Database);
        assert!(!db_err.message.to_lowercase().contains("sk-"));
    }
}
