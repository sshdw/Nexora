//! Tauri commands over the existing [`ExportService`] / [`ImportService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer export / import services (FR-010, FR-011)
//! and converts their classified errors into safe [`CommandError`] values.
//!
//! # Confirmation coverage (incomplete — NEX-SEC-005)
//!
//! The write-to-disk exports here (`export_conversation_to_file`,
//! `export_setup_to_file`, `export_setup`) write to a caller-chosen path with
//! **no Rust-side confirmation gate at all**: unlike the terminal and
//! data-management paths, which require a single-use id minted only after the
//! user accepts a blocking native OS dialog, these take a raw path and run.
//! Migrating them is the open audit item #137 (SEC-005); see
//! `application/confirmations.rs` for the full coverage split. Do not describe
//! the export paths as server-verified until then.
//!
//! NEX-SEC-005 backstop (same item): an existing destination is never
//! overwritten — the export services refuse it (`destination already exists`)
//! and the write itself is atomic `create_new`, so a caller-chosen path can
//! only create a fresh `.json` document, never clobber one.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};

use crate::application::export::{ExportService, SetupExportService};
use crate::application::import::{ImportService, SetupImportReport, SetupImportService};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Export one conversation to its JSON document (returns the document text).
///
/// Runs on the runtime blocking pool like `send_message`: the export is
/// blocking end to end (`SQLite` reads plus serialization), so a synchronous
/// command would run on the main thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn export_conversation(
    conversation_id: i64,
    app: AppHandle,
) -> Result<String, CommandError> {
    // Owned handle so the managed state can be reached from the blocking
    // thread (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<String, CommandError> = ExportService::new(db.inner())
            .serialize(conversation_id)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("export_conversation blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Serialization,
                "the conversation could not be serialized for export",
            ))
        }
    }
}

/// Export one conversation and write the document to the given `path`.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_conversation_to_file(
    conversation_id: i64,
    path: String,
    app: AppHandle,
) -> Result<(), CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<(), CommandError> = ExportService::new(db.inner())
            .export_to_file(conversation_id, std::path::Path::new(&path))
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("export_conversation_to_file blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the exported conversation could not be written to disk",
            ))
        }
    }
}

/// Import a conversation from an existing export document; returns the new
/// conversation's `id`.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn import_conversation(json: String, app: AppHandle) -> Result<i64, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<i64, CommandError> = ImportService::new(db.inner())
            .import(&json)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("import_conversation blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::InvalidData,
                "the import document is invalid: the document could not be processed",
            ))
        }
    }
}

/// Import a VS Code `settings.json` document (WS-E.2); returns the per-key
/// report (`imported` / `skipped` / `denied`). Only the mappable subset
/// translates — everything else is reported as skipped, never guessed.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn import_vscode_settings(
    json: String,
    app: AppHandle,
) -> Result<SetupImportReport, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<SetupImportReport, CommandError> = SetupImportService::new(db.inner())
            .import_vscode(&json)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("import_vscode_settings blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::InvalidData,
                "the import document is invalid: the document could not be processed",
            ))
        }
    }
}

/// Import an MCP servers document (`{ "mcpServers": { ... } }`, WS-E.2);
/// returns the per-server report. Validated servers are stored under the
/// `mcp.servers` setting; secret-like entries are denied per server with a
/// secret-free reason while valid servers still import.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn import_mcp_servers(
    json: String,
    app: AppHandle,
) -> Result<SetupImportReport, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<SetupImportReport, CommandError> = SetupImportService::new(db.inner())
            .import_mcp(&json)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("import_mcp_servers blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::InvalidData,
                "the import document is invalid: the document could not be processed",
            ))
        }
    }
}

/// Export the current Nexora setup (settings plus routing profiles and
/// feature flags) to its portable JSON document (WS-E.2); returns the
/// document text.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_setup(app: AppHandle) -> Result<String, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<String, CommandError> = SetupExportService::new(db.inner())
            .serialize_setup()
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("export_setup blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Serialization,
                "the setup could not be serialized for export",
            ))
        }
    }
}

/// Export the current Nexora setup and write the document to the given
/// `path` (WS-E.2). The destination goes through the same JSON-only
/// allowlist and path validation as conversation exports.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_setup_to_file(path: String, app: AppHandle) -> Result<(), CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<(), CommandError> = SetupExportService::new(db.inner())
            .export_setup_to_file(std::path::Path::new(&path))
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("export_setup_to_file blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the exported setup could not be written to disk",
            ))
        }
    }
}

/// Import a portable Nexora setup document (WS-E.2); returns the per-key
/// report. Allowlisted keys with in-domain values are written (`null`
/// clears a key); unknown keys are skipped; out-of-domain values are denied
/// with fixed-vocabulary reasons.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn import_setup(
    json: String,
    app: AppHandle,
) -> Result<SetupImportReport, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<SetupImportReport, CommandError> = SetupImportService::new(db.inner())
            .import_setup(&json)
            .map_err(CommandError::from);
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("import_setup blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::InvalidData,
                "the import document is invalid: the document could not be processed",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;

    const SOURCE: &str = include_str!("import_export.rs");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Static wiring check (NEX-IPC-001): every import/export command is
    /// declared `async` and moves its `SQLite`/document/file work onto the
    /// runtime blocking pool, matching the `send_message` precedent.
    #[test]
    fn import_export_commands_run_on_the_blocking_pool() {
        for name in [
            "export_conversation",
            "export_conversation_to_file",
            "import_conversation",
            "import_vscode_settings",
            "import_mcp_servers",
            "export_setup",
            "export_setup_to_file",
            "import_setup",
        ] {
            assert!(
                SOURCE.contains(&format!("async fn {name}(")),
                "{name} must be an async command so it never runs on the main thread"
            );
        }
        assert!(
            SOURCE.contains("spawn_blocking"),
            "the import/export commands must move blocking work onto the blocking pool"
        );
    }

    /// Drive one `'static` closure through the production scheduling shape
    /// (`spawn_blocking` awaited from the async runtime) and join it,
    /// proving the moved command body completes off-thread. The in-memory
    /// database handle is cheaply cloneable (`Arc` behind `Mutex`), so the
    /// closure owns its database exactly like the command owns its `AppHandle`.
    fn through_boundary<T, F>(work: F) -> T
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(work)
                .await
                .expect("blocking task joins without panicking")
        })
    }

    fn seeded_conversation(db: &Database) -> i64 {
        let id = ConversationRepository::new(db)
            .create("boundary smoke", "active")
            .expect("conversation seeds");
        MessageRepository::new(db)
            .create(id, "user", "hello from the boundary", None, None)
            .expect("message seeds");
        id
    }

    fn scratch_path(name: &str) -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("nexora-impex-cmd-test-{name}-{id}.json"))
    }

    /// Boundary smoke: `export_conversation` + `import_conversation` round
    /// trip through the async boundary — the re-imported conversation gets a
    /// new id with the same history.
    #[test]
    fn conversation_export_import_round_trip_through_the_boundary() {
        let db = in_memory_database();
        let id = seeded_conversation(&db);

        let document = through_boundary({
            let db = db.clone();
            move || ExportService::new(&db).serialize(id)
        })
        .expect("export succeeds through the boundary");
        assert!(
            document.contains("hello from the boundary"),
            "the exported document carries the message"
        );

        let imported = through_boundary({
            let db = db.clone();
            move || ImportService::new(&db).import(&document)
        })
        .expect("import succeeds through the boundary");
        assert_ne!(imported, id, "the import creates a new conversation");

        let history = crate::application::conversations::ConversationService::new(&db)
            .history(imported)
            .expect("re-imported history reads");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "hello from the boundary");
    }

    /// Arg-contract pin (#185): the `tauri.ts`
    /// `exportConversationToFile` wrapper must invoke
    /// `export_conversation_to_file` with exactly the Rust command's
    /// parameters in camelCase (`conversation_id` -> `conversationId`,
    /// plus `path`). Sending `{ id, path }` compiles fine but fails at
    /// runtime with a missing-`conversationId` error, so this source
    /// check keeps the wrapper and the signature in sync.
    #[test]
    fn export_conversation_to_file_invoke_args_match_rust_params() {
        const TAURI_TS: &str = include_str!("../../../src/lib/tauri.ts");
        let needle = "\"export_conversation_to_file\"";
        let at = TAURI_TS
            .find(needle)
            .expect("invoke(\"export_conversation_to_file\") must exist in src/lib/tauri.ts");
        let after = &TAURI_TS[at + needle.len()..];
        let open = after
            .find('{')
            .expect("the export_conversation_to_file invoke must pass an argument object");
        let inner = &after[open + 1..];
        let close = inner
            .find('}')
            .expect("the export_conversation_to_file argument object must close");
        let mut got: Vec<String> = inner[..close]
            .split(',')
            .map(|segment| {
                let segment = segment.trim();
                match segment.find(':') {
                    Some(idx) => segment[..idx].trim().to_string(),
                    None => segment.to_string(),
                }
            })
            .filter(|key| !key.is_empty())
            .collect();
        got.sort_unstable();
        let mut want = vec!["conversationId".to_string(), "path".to_string()];
        want.sort_unstable();
        assert_eq!(
            got, want,
            "invoke(\"export_conversation_to_file\") argument keys must exactly match \
             the Rust command parameters (camelCase)"
        );
    }

    /// Boundary smoke: `export_conversation_to_file` writes a re-importable
    /// document through the async boundary.
    #[test]
    fn conversation_export_to_file_through_the_boundary() {
        let db = in_memory_database();
        let id = seeded_conversation(&db);
        let path = scratch_path("conversation");

        through_boundary({
            let db = db.clone();
            let path = path.clone();
            move || ExportService::new(&db).export_to_file(id, &path)
        })
        .expect("file export succeeds through the boundary");

        let document = std::fs::read_to_string(&path).expect("export file reads");
        assert!(
            document.contains("hello from the boundary"),
            "the exported file carries the message"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Boundary smoke: `import_vscode_settings` completes through the async
    /// boundary (unmapped keys land in the report, nothing is stored).
    #[test]
    fn vscode_import_through_the_boundary() {
        let db = in_memory_database();
        let json = serde_json::json!({
            "editor.fontSize": 14,
            "workbench.startupEditor": "newUntitledFile",
        })
        .to_string();

        let report = through_boundary({
            let db = db.clone();
            move || SetupImportService::new(&db).import_vscode(&json)
        })
        .expect("vscode import succeeds through the boundary");
        assert!(report.imported.is_empty());
        assert_eq!(report.skipped.len(), 2);
    }

    /// Boundary smoke: `import_mcp_servers` stores validated servers through
    /// the async boundary.
    #[test]
    fn mcp_import_through_the_boundary() {
        let db = in_memory_database();
        let json = serde_json::json!({
            "mcpServers": {
                "notes": { "command": "/usr/local/bin/notes-mcp" },
            },
        })
        .to_string();

        let report = through_boundary({
            let db = db.clone();
            move || SetupImportService::new(&db).import_mcp(&json)
        })
        .expect("mcp import succeeds through the boundary");
        assert_eq!(report.imported.len(), 1);
        assert!(report.denied.is_empty());
    }

    /// Boundary smoke: `export_setup` + `import_setup` round trip through
    /// the async boundary.
    #[test]
    fn setup_export_import_round_trip_through_the_boundary() {
        let db = in_memory_database();

        let seed = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": { "flags.assembly": "false" },
        })
        .to_string();
        let seed_report = through_boundary({
            let db = db.clone();
            move || SetupImportService::new(&db).import_setup(&seed)
        })
        .expect("setup seed import succeeds through the boundary");
        assert_eq!(seed_report.imported.len(), 1);

        let document = through_boundary({
            let db = db.clone();
            move || SetupExportService::new(&db).serialize_setup()
        })
        .expect("setup export succeeds through the boundary");
        assert!(
            document.contains("flags.assembly"),
            "the setup document carries the seeded key"
        );

        let report = through_boundary({
            let db = db.clone();
            move || SetupImportService::new(&db).import_setup(&document)
        })
        .expect("setup re-import succeeds through the boundary");
        assert!(report.denied.is_empty());
    }

    /// Boundary smoke: `export_setup_to_file` writes a readable document
    /// through the async boundary.
    #[test]
    fn setup_export_to_file_through_the_boundary() {
        let db = in_memory_database();
        let path = scratch_path("setup");

        through_boundary({
            let db = db.clone();
            let path = path.clone();
            move || SetupExportService::new(&db).export_setup_to_file(&path)
        })
        .expect("setup file export succeeds through the boundary");

        let document = std::fs::read_to_string(&path).expect("export file reads");
        assert!(
            document.contains("nexora-setup"),
            "the exported file is a setup document"
        );
        let _ = std::fs::remove_file(&path);
    }
}
