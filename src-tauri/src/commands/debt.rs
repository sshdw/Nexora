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
//! imported. There is no watching or live re-import: the panel runs the
//! import manually, and re-runs insert nothing new.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. agent.rs.)
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, State};

use crate::application::debt::{DebtError, DebtService};
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
/// the number of newly inserted rows.
#[tauri::command]
pub(crate) fn import_debt_from_audit(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<usize, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    DebtService::new(db.inner())
        .import_from_audit(&root)
        .map_err(CommandError::from)
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
