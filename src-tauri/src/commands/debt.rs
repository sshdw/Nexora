//! Debt-backlog IPC commands: the Tauri side of the technical-debt backlog
//! (manual entries plus idempotent imports of `repo_audit` findings).
//!
//! Thin translation only (ARCHITECTURE.md §5): each command delegates to the
//! application-layer debt service ([`crate::application::debt`]) and maps
//! classified errors into secret-free [`CommandError`] values (no
//! credentials, raw SQL, or item content in error text — validation messages
//! are fixed vocabulary that never echo the rejected value).
//!
//! Command-shape decision (one feature area, five commands): `create_debt_item`
//! adds one manual entry; `list_debt_items` returns every row (most recently
//! touched first); `update_debt_item_status` moves one row through the triage
//! lifecycle; `delete_debt_item` removes one row; `import_debt_from_audit`
//! runs the read-only audit over the effective workspace root (exactly like
//! `repo_audit`) and inserts fresh findings as rows, skipping keys it already
//! imported and reporting unimportable rows in the returned summary. There is
//! no watching or live re-import: the panel runs the import manually, and
//! re-runs insert nothing new.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. agent.rs.)
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::debt::{DebtError, DebtService, ImportSummary};
use crate::application::repo_audit::RepoAuditError;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::debt_items::DebtItem;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Add one manually tracked debt entry (`source='manual'`). The title is
/// required (1..=200 chars); severity is `info` / `warning`; location and
/// note are optional. Returns the schema-assigned item id.
#[tauri::command]
pub(crate) fn create_debt_item(
    title: String,
    severity: String,
    location: Option<String>,
    note: Option<String>,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    DebtService::new(db.inner())
        .add(&title, &severity, location.as_deref(), note.as_deref())
        .map_err(CommandError::from)
}

/// List every debt item, most recently touched first.
#[tauri::command]
pub(crate) fn list_debt_items(db: State<'_, Database>) -> Result<Vec<DebtItem>, CommandError> {
    DebtService::new(db.inner())
        .list()
        .map_err(CommandError::from)
}

/// Move one debt item to a new triage status (`open` / `accepted` / `fixed` /
/// `wontfix`). Any transition between the four statuses is accepted; unknown
/// ids fail with a secret-free not-found error carrying only the id.
#[tauri::command]
pub(crate) fn update_debt_item_status(
    debt_id: i64,
    status: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    DebtService::new(db.inner())
        .update_status(debt_id, &status)
        .map_err(CommandError::from)
}

/// Delete one debt item by id (a no-op when the id is unknown).
#[tauri::command]
pub(crate) fn delete_debt_item(debt_id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    DebtService::new(db.inner())
        .delete(debt_id)
        .map_err(CommandError::from)
}

/// Import the current `repo_audit` scanner findings over the effective
/// workspace root as debt rows. Findings already imported are skipped, so
/// re-running inserts nothing new and never touches triaged rows. Returns
/// the [`ImportSummary`] for the run: newly inserted rows plus rows skipped
/// as unimportable (`inserted == 0 && skipped == 0` means already tracked).
///
/// Runs on the runtime blocking pool like `repo_audit`: the import rescans
/// the same workspace sources, so a synchronous command would run on the
/// main thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn import_debt_from_audit(app: AppHandle) -> Result<ImportSummary, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        DebtService::new(db.inner())
            .import_from_audit(&root)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("import_debt_from_audit blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the repository audit could not read the workspace",
            ))
        }
    }
}

impl From<DebtError> for CommandError {
    fn from(err: DebtError) -> Self {
        match err {
            DebtError::ItemNotFound { id } => {
                Self::new(ErrorKind::NotFound, format!("no debt item with id {id}"))
            }
            DebtError::InvalidInput { message } => Self::new(ErrorKind::InvalidInput, message),
            // The workspace path may contain a user name, so even the
            // fixed text names no path (same mapping as `repo_audit`).
            DebtError::Audit(RepoAuditError::InvalidRoot) => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for audit",
            ),
            DebtError::Audit(RepoAuditError::Io) => Self::new(
                ErrorKind::Io,
                "the repository audit could not read the workspace",
            ),
            DebtError::Database(inner) => Self::from(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn debt_error_mapping_is_classified_and_secret_free() {
        let cases = [
            (DebtError::ItemNotFound { id: 7 }, ErrorKind::NotFound),
            (
                DebtError::InvalidInput {
                    message: "the debt title must be 1..200 characters".to_string(),
                },
                ErrorKind::InvalidInput,
            ),
            (
                DebtError::Audit(RepoAuditError::InvalidRoot),
                ErrorKind::InvalidInput,
            ),
            (DebtError::Audit(RepoAuditError::Io), ErrorKind::Io),
            (
                DebtError::Database(crate::infrastructure::database::DatabaseError::Lock(
                    "sk-".into(),
                )),
                ErrorKind::Database,
            ),
        ];
        for (case, kind) in cases {
            let mapped: CommandError = case.into();
            assert_eq!(mapped.kind, kind);
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }

    /// Static wiring check (NEX-IPC-001): the audit-backed debt import is
    /// declared `async` and moves the workspace scan onto the runtime
    /// blocking pool, like `repo_audit` whose engine it reuses. The four
    /// single-row CRUD commands stay synchronous: one indexed row each.
    #[test]
    fn debt_audit_import_runs_on_the_blocking_pool() {
        const SOURCE: &str = include_str!("debt.rs");
        assert!(
            SOURCE.contains("async fn import_debt_from_audit("),
            "import_debt_from_audit must be async so it never runs on the main thread"
        );
        assert!(
            SOURCE.contains("spawn_blocking"),
            "import_debt_from_audit must move the workspace scan onto the blocking pool"
        );
    }

    /// Boundary smoke: the exact body the command moves onto the blocking
    /// pool completes there against a scratch workspace plus an in-memory
    /// database, returning a summary. Fixture bytes go through `OpenOptions`
    /// (never the one-call file-writing shorthand) so the read-only static
    /// check above keeps holding for this file.
    #[test]
    fn debt_audit_import_body_completes_through_the_async_boundary() {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::infrastructure::database::in_memory_database;

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("nexora-debt-cmd-test-{id}"));
        std::fs::create_dir_all(&root).expect("scratch workspace creates");
        let full = root.join("lib.rs");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&full)
            .expect("fixture file opens");
        file.write_all(b"pub fn noted() {}\n// TODO: boundary smoke\n")
            .expect("fixture file writes");
        drop(file);

        let db = in_memory_database();
        let summary = tauri::async_runtime::block_on(async {
            let db = db.clone();
            let root = root.clone();
            tauri::async_runtime::spawn_blocking(move || {
                DebtService::new(&db).import_from_audit(&root)
            })
            .await
            .expect("blocking task joins without panicking")
        })
        .expect("debt import succeeds through the boundary");
        assert!(
            summary.inserted + summary.skipped > 0,
            "the seeded marker surfaces in the summary"
        );

        // Re-running over the same tree tracks nothing new.
        let rerun = DebtService::new(&db)
            .import_from_audit(&root)
            .expect("re-import succeeds");
        assert_eq!(rerun.inserted, 0, "re-runs track nothing new");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Static wiring check: the debt commands stay a thin translation over
    /// the application-layer service — no SQL, no scanner logic, and no
    /// workspace writes of their own. Needles are built with `concat!` so
    /// this test's own source never matches them verbatim.
    #[test]
    fn debt_commands_stay_thin_over_the_service() {
        const SOURCE: &str = include_str!("debt.rs");
        for needle in ["DebtService::new(", "import_from_audit("] {
            assert!(
                SOURCE.contains(needle),
                "commands/debt.rs must delegate to the debt service, missing {needle:?}"
            );
        }
        for needle in [
            concat!("SELECT", " "),
            concat!("INSERT", " "),
            concat!("UPDATE", " "),
            concat!("DELETE", " "),
            concat!("fs", "::write"),
            concat!("audit", "_workspace("),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/debt.rs must hold no SQL/scanner/write logic, found {needle:?}"
            );
        }
    }
}
