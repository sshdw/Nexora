//! Tauri commands over the existing [`ExportService`] / [`ImportService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer export / import services (FR-010, FR-011)
//! and converts their classified errors into safe [`CommandError`] values.
//!
//! # Confirmation coverage (complete — SEC-03)
//!
//! All four export commands here (`export_conversation`,
//! `export_conversation_to_file`, `export_setup`, `export_setup_to_file`)
//! require a live single-use `confirmation_id` minted in the `export` scope
//! by `request_confirmation` — which only mints after the user accepts a
//! blocking native OS dialog — and bound to the exact export operation (the
//! two conversation operations additionally bind to the conversation row).
//! The application-layer export services consume the id atomically before
//! any read or write; forged, expired, reused, cross-scope, and
//! different-operation ids refuse with `ConfirmationRequired`. This closes
//! the former renderer-attested gap (open audit item #137 / SEC-005).
//!
//! NEX-SEC-005 backstop (same item): an existing destination is never
//! overwritten — the export services refuse it (`destination already exists`)
//! and the write itself is atomic `create_new`, so a caller-chosen path can
//! only create a fresh `.json` document, never clobber one.
//!
//! # Import reads are backend-side (SEC-01)
//!
//! `import_conversation` takes the dialog-picked absolute `path` and reads
//! the document backend-side (`application::import::read_import_document`):
//! the `.json`-only allowlist, path containment guards, and the 16 MiB cap
//! fire before any byte is read. The renderer no longer reads picked files
//! through the `fs` plugin.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::confirmations::ManagedConfirmations;
use crate::application::export::{ExportService, SetupExportService};
use crate::application::import::{ImportService, SetupImportReport, SetupImportService};
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};

/// Export one conversation to its JSON document (returns the document text).
///
/// Requires `confirmation_id` — a live single-use id minted for
/// `export_conversation` in the `export` scope by `request_confirmation`,
/// bound to this conversation row (SEC-03).
///
/// Runs on the runtime blocking pool like `send_message`: the export is
/// blocking end to end (`SQLite` reads plus serialization), so a synchronous
/// command would run on the main thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn export_conversation(
    conversation_id: i64,
    confirmation_id: String,
    app: AppHandle,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<String, CommandError> {
    // Owned handle so the managed state can be reached from the blocking
    // thread (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let confirmations = confirmations.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<String, CommandError> = ExportService::new(db.inner())
            .serialize(conversation_id, confirmation_id.as_str(), &confirmations)
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
/// Requires `confirmation_id` minted for `export_conversation_to_file`,
/// bound to this conversation row (SEC-03).
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_conversation_to_file(
    conversation_id: i64,
    path: String,
    confirmation_id: String,
    app: AppHandle,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<(), CommandError> {
    let handle = app.clone();
    let confirmations = confirmations.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<(), CommandError> = ExportService::new(db.inner())
            .export_to_file(
                conversation_id,
                std::path::Path::new(&path),
                confirmation_id.as_str(),
                &confirmations,
            )
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

/// Import a conversation from the `.json` document at the dialog-picked
/// absolute `path` (SEC-01: the document is read backend-side — the `.json`
/// allowlist, path containment guards, and the 16 MiB cap fire before any
/// read); returns the new conversation's `id`.
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn import_conversation(path: String, app: AppHandle) -> Result<i64, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<i64, CommandError> = ImportService::new(db.inner())
            .import_from_file(path.as_str())
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
/// document text. Requires `confirmation_id` minted for `export_setup`
/// (SEC-03).
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_setup(
    confirmation_id: String,
    app: AppHandle,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<String, CommandError> {
    let handle = app.clone();
    let confirmations = confirmations.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<String, CommandError> = SetupExportService::new(db.inner())
            .serialize_setup(confirmation_id.as_str(), &confirmations)
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
/// allowlist and path validation as conversation exports; requires
/// `confirmation_id` minted for `export_setup_to_file` (SEC-03).
///
/// Runs on the blocking pool like [`export_conversation`].
#[tauri::command]
pub(crate) async fn export_setup_to_file(
    path: String,
    confirmation_id: String,
    app: AppHandle,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<(), CommandError> {
    let handle = app.clone();
    let confirmations = confirmations.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        let result: Result<(), CommandError> = SetupExportService::new(db.inner())
            .export_setup_to_file(
                std::path::Path::new(&path),
                confirmation_id.as_str(),
                &confirmations,
            )
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

    use crate::application::confirmations::{
        ConfirmationRegistry, ConfirmationTarget, OP_EXPORT_CONVERSATION,
        OP_EXPORT_CONVERSATION_TO_FILE, OP_EXPORT_SETUP, OP_EXPORT_SETUP_TO_FILE, SCOPE_EXPORT,
    };
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;

    const SOURCE: &str = include_str!("import_export.rs");

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Mint one fresh export-scope id for `target` (each export consumes a
    /// single-use id, so every confirmed call mints its own). Returns the id
    /// with the registry that must consume it.
    fn minted(target: &ConfirmationTarget<'_>) -> (String, ConfirmationRegistry) {
        let registry = ConfirmationRegistry::new();
        let id = registry
            .request(SCOPE_EXPORT, target)
            .expect("mint export confirmation");
        (id, registry)
    }

    /// [`ExportService::serialize`] with a freshly minted
    /// [`OP_EXPORT_CONVERSATION`] id bound to the conversation row.
    fn serialize_confirmed(
        service: &ExportService<'_>,
        conversation_id: i64,
    ) -> Result<String, CommandError> {
        let (id, confirmations) = minted(&ConfirmationTarget::for_target(
            OP_EXPORT_CONVERSATION,
            conversation_id,
        ));
        service
            .serialize(conversation_id, &id, &confirmations)
            .map_err(CommandError::from)
    }

    /// [`ExportService::export_to_file`] with a freshly minted
    /// [`OP_EXPORT_CONVERSATION_TO_FILE`] id bound to the conversation row.
    fn export_to_file_confirmed(
        service: &ExportService<'_>,
        conversation_id: i64,
        path: &std::path::Path,
    ) -> Result<(), CommandError> {
        let (id, confirmations) = minted(&ConfirmationTarget::for_target(
            OP_EXPORT_CONVERSATION_TO_FILE,
            conversation_id,
        ));
        service
            .export_to_file(conversation_id, path, &id, &confirmations)
            .map_err(CommandError::from)
    }

    /// [`SetupExportService::serialize_setup`] with a freshly minted
    /// [`OP_EXPORT_SETUP`] id.
    fn serialize_setup_confirmed(service: &SetupExportService<'_>) -> Result<String, CommandError> {
        let (id, confirmations) = minted(&ConfirmationTarget::new(OP_EXPORT_SETUP));
        service
            .serialize_setup(&id, &confirmations)
            .map_err(CommandError::from)
    }

    /// [`SetupExportService::export_setup_to_file`] with a freshly minted
    /// [`OP_EXPORT_SETUP_TO_FILE`] id.
    fn export_setup_to_file_confirmed(
        service: &SetupExportService<'_>,
        path: &std::path::Path,
    ) -> Result<(), CommandError> {
        let (id, confirmations) = minted(&ConfirmationTarget::new(OP_EXPORT_SETUP_TO_FILE));
        service
            .export_setup_to_file(path, &id, &confirmations)
            .map_err(CommandError::from)
    }

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
            move || serialize_confirmed(&ExportService::new(&db), id)
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

    /// Arg-contract pin (#185, SEC-03 revision): the Rust
    /// `export_conversation_to_file` command takes exactly
    /// `conversation_id` + `path` + `confirmation_id`, which project to the
    /// camelCase invoke keys `{ conversationId, path, confirmationId }`.
    /// Sending `{ id, path }` compiles fine but fails at runtime with a
    /// missing-`conversationId` error, so this source check pins the Rust
    /// side of the contract. The cross-language half (the `tauri.ts`
    /// `exportConversationToFile` wrapper passing those exact keys, minting
    /// through `requestConfirmation` first) is re-established by work
    /// package 2 when the wrapper is updated.
    #[test]
    fn export_conversation_to_file_rust_params_project_to_camel_case() {
        const SIGNATURE: &str = "pub(crate) async fn export_conversation_to_file(";
        let at = SOURCE
            .find(SIGNATURE)
            .expect("the export_conversation_to_file command signature must exist");
        // The signature itself ends at the parameter-list open paren, so the
        // list spans from there to the `) ->` that closes it.
        let params = &SOURCE[at + SIGNATURE.len()..];
        let close = params
            .find(") ->")
            .expect("the command parameter list closes before the return type");
        let inner = &params[..close];
        let mut got: Vec<String> = inner
            .split(',')
            .filter(|segment| segment.contains(':'))
            .map(|segment| {
                let segment = segment.trim();
                segment.split(':').next().unwrap_or("").trim().to_string()
            })
            .filter(|name| !name.is_empty())
            .collect();
        got.sort_unstable();
        // `app` and `confirmations` are Tauri-injected (`AppHandle` and the
        // managed registry) and are never caller invoke args; every other
        // parameter is caller-supplied and projects to a camelCase key.
        let mut want = vec![
            "app".to_string(),
            "confirmation_id".to_string(),
            "confirmations".to_string(),
            "conversation_id".to_string(),
            "path".to_string(),
        ];
        want.sort_unstable();
        assert_eq!(
            got, want,
            "export_conversation_to_file must take exactly the conversation row, \
             the destination path, and the minted confirmation_id from the caller \
             (camelCase {{ conversationId, path, confirmationId }}), plus the \
             Tauri-injected app + confirmations state"
        );
        // The caller-facing keys are exactly the three above: no `confirmed`
        // flag, no other caller parameter.
        for injected in [
            "app: AppHandle",
            "confirmations: State<'_, ManagedConfirmations>",
        ] {
            assert!(
                inner.contains(injected),
                "the framework-injected {injected:?} parameter must stay explicit"
            );
        }
        // The confirmation id must come from the mint flow, not a boolean.
        assert!(
            !inner.contains("confirmed"),
            "the export command must take a minted confirmation id, not a caller flag"
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
            move || export_to_file_confirmed(&ExportService::new(&db), id, &path)
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
            move || serialize_setup_confirmed(&SetupExportService::new(&db))
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
            move || export_setup_to_file_confirmed(&SetupExportService::new(&db), &path)
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
