//! Debt-backlog service: application-layer orchestration for the
//! technical-debt backlog (manual entries plus idempotent imports of the
//! read-only `repo_audit` scanner findings).
//!
//! This service composes the [`DebtItemRepository`] for persistence and the
//! existing [`audit_workspace`] engine for imports. It adds no schema, no
//! SQL, and no database access of its own: all persistence is delegated to
//! the repository.
//!
//! # Import identity (idempotency without re-scanning architecture)
//!
//! The slice deliberately reuses the existing scanners where cheap: each
//! [`AuditFinding`] maps onto one debt row whose `audit_key` is
//! `kind<US>path<US>line` (`<US>` = U+001F, unusable in file names on the
//! supported platforms, so distinct findings never share a key). The key
//! column is `UNIQUE`, and the repository inserts with `INSERT OR IGNORE` —
//! re-running the import over unchanged sources inserts nothing new. Status
//! changes on already-imported rows survive re-imports (the ignore path
//! never touches existing rows). There is no watching or live re-audit: the
//! frontend runs the import manually, exactly like the audit panel's Run.

use std::path::Path;

use crate::application::repo_audit::{audit_workspace, AuditFinding, RepoAuditError};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::debt_items::{DebtItem, DebtItemRepository};

/// Application-layer result shared by debt-backlog operations, unifying
/// persistence, validation, and scan failures.
pub(crate) type Result<T> = std::result::Result<T, DebtError>;

/// Longest accepted debt-item title in characters (mirrors the schema CHECK).
const MAX_TITLE_CHARS: usize = 200;

/// Longest accepted location in characters (mirrors the schema CHECK).
const MAX_LOCATION_CHARS: usize = 1024;

/// Longest accepted note in characters (mirrors the schema CHECK).
const MAX_NOTE_CHARS: usize = 4000;

/// Longest stored import key in characters (mirrors the schema CHECK). The
/// key is `kind<US>path<US>line`; workspace-relative paths beyond this make
/// the row unimportable rather than silently colliding after truncation.
const MAX_AUDIT_KEY_CHARS: usize = 2048;

/// Field separator inside `audit_key` (U+001F UNIT SEPARATOR): unusable in
/// file names on the supported platforms, so two distinct findings can never
/// produce the same key through the separator.
const KEY_SEPARATOR: char = '\u{1f}';

/// Fixed source vocabulary (mirrors the schema CHECK).
pub(crate) const SOURCE_AUDIT: &str = "audit";
/// Fixed source vocabulary (mirrors the schema CHECK).
pub(crate) const SOURCE_MANUAL: &str = "manual";

/// Fixed severity vocabulary (mirrors the schema CHECK and the audit engine).
pub(crate) const SEVERITY_INFO: &str = "info";
/// Fixed severity vocabulary (mirrors the schema CHECK and the audit engine).
pub(crate) const SEVERITY_WARNING: &str = "warning";

/// Every accepted severity, in panel filter order.
pub(crate) const SEVERITIES: [&str; 2] = [SEVERITY_INFO, SEVERITY_WARNING];

/// Fixed status vocabulary (mirrors the schema CHECK).
pub(crate) const STATUS_OPEN: &str = "open";
/// Fixed status vocabulary (mirrors the schema CHECK).
pub(crate) const STATUS_ACCEPTED: &str = "accepted";
/// Fixed status vocabulary (mirrors the schema CHECK).
pub(crate) const STATUS_FIXED: &str = "fixed";
/// Fixed status vocabulary (mirrors the schema CHECK).
pub(crate) const STATUS_WONT_FIX: &str = "wontfix";

/// Every accepted status, in lifecycle order.
pub(crate) const STATUSES: [&str; 4] =
    [STATUS_OPEN, STATUS_ACCEPTED, STATUS_FIXED, STATUS_WONT_FIX];

/// Application-layer service managing the technical-debt backlog.
///
/// Wraps [`DebtItemRepository`] for persistence and the existing audit engine
/// for imports. It is deliberately focused on orchestration and validation;
/// persistence behavior and schema constraints remain in the repository and
/// the database.
pub(crate) struct DebtService<'a> {
    items: DebtItemRepository<'a>,
}

impl<'a> DebtService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            items: DebtItemRepository::new(db),
        }
    }

    /// Add a manually tracked debt entry (`source='manual'`).
    ///
    /// Returns the `id` of the newly inserted item.
    ///
    /// # Errors
    ///
    /// Returns [`DebtError::InvalidInput`] when the title is empty or over
    /// 200 characters, the severity is outside the `info` / `warning`
    /// vocabulary, or the location / note exceed their length caps; or
    /// [`DebtError::Database`] when the insert fails.
    pub(crate) fn add(
        &self,
        title: &str,
        severity: &str,
        location: Option<&str>,
        note: Option<&str>,
    ) -> Result<i64> {
        validate_title(title)?;
        validate_severity(severity)?;
        validate_location(location)?;
        validate_note(note)?;
        Ok(self
            .items
            .create(title, SOURCE_MANUAL, severity, location, note, None)?)
    }

    /// List every debt item, most recently touched first (the repository's
    /// persisted order).
    ///
    /// # Errors
    ///
    /// Returns [`DebtError::Database`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<DebtItem>> {
        Ok(self.items.list()?)
    }

    /// Move an item to a new triage `status`. Any transition between the four
    /// lifecycle statuses is accepted (including re-opening a `fixed` item);
    /// only the vocabulary is enforced.
    ///
    /// # Errors
    ///
    /// Returns [`DebtError::InvalidInput`] for a status outside the fixed
    /// vocabulary, [`DebtError::ItemNotFound`] when no item with `id`
    /// exists, or [`DebtError::Database`] when the update fails.
    pub(crate) fn update_status(&self, id: i64, status: &str) -> Result<()> {
        validate_status(status)?;
        self.items.read(id)?.ok_or(DebtError::ItemNotFound { id })?;
        self.items.update_status(id, status)?;
        Ok(())
    }

    /// Delete a debt item by `id`. Deleting an unknown id is a no-op,
    /// matching the repository's delete semantics.
    ///
    /// # Errors
    ///
    /// Returns [`DebtError::Database`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        Ok(self.items.delete(id)?)
    }

    /// Import the current `repo_audit` scanner findings over `root` as debt
    /// rows. Findings whose `audit_key` was already imported are skipped
    /// (the ignore path never touches existing rows, so triage statuses
    /// survive re-imports); findings whose key would exceed the length cap
    /// are skipped rather than truncated into a possible collision.
    ///
    /// Returns the number of newly inserted rows (0 when everything was
    /// already tracked).
    ///
    /// # Errors
    ///
    /// Returns [`DebtError::Audit`] when the workspace scan fails, or
    /// [`DebtError::Database`] when an insert fails.
    pub(crate) fn import_from_audit(&self, root: &Path) -> Result<usize> {
        let report = audit_workspace(root).map_err(DebtError::Audit)?;
        let mut inserted = 0;
        for finding in &report.findings {
            let Some((title, location, note, key)) = import_row(finding) else {
                continue;
            };
            if self.items.insert_import(
                &title,
                &finding.severity,
                Some(&location),
                Some(&note),
                &key,
            )? {
                inserted += 1;
            }
        }
        Ok(inserted)
    }
}

/// Classified errors raised by debt-backlog orchestration.
///
/// Unifies validation, scan, and persistence failures. No variant carries a
/// secret value, so formatting a [`DebtError`] never writes a secret to the
/// logs (ARCHITECTURE.md §9, §11). Finding titles, locations, and notes are
/// workspace-relative scanner output, never credentials.
#[derive(Debug)]
pub(crate) enum DebtError {
    /// No debt item with the referenced `id` exists.
    ItemNotFound {
        /// The requested debt-item id.
        id: i64,
    },
    /// A caller-supplied value failed validation (fixed-vocabulary message,
    /// never echoing the value).
    InvalidInput {
        /// What was wrong, in fixed vocabulary.
        message: String,
    },
    /// The workspace scan backing an import failed (no payload, so no path
    /// can leak — paths may contain user names).
    Audit(RepoAuditError),
    /// A persistence failure from the repository.
    Database(DatabaseError),
}

impl std::fmt::Display for DebtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ItemNotFound { id } => write!(f, "debt item {id} does not exist"),
            Self::InvalidInput { message } => write!(f, "{message}"),
            // Fixed vocabulary with no path (paths may contain user names),
            // mirroring the repo-audit command mapping.
            Self::Audit(RepoAuditError::InvalidRoot) => {
                write!(f, "the workspace folder is not available for audit")
            }
            Self::Audit(RepoAuditError::Io) => {
                write!(f, "the repository audit could not read the workspace")
            }
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DebtError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ItemNotFound { .. } | Self::InvalidInput { .. } | Self::Audit(_) => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for DebtError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

/// Reject an empty or overlong title without echoing it (values stay out of
/// fixed-vocabulary errors by the secret-free doctrine).
fn validate_title(title: &str) -> Result<()> {
    let len = title.chars().count();
    if len == 0 || len > MAX_TITLE_CHARS {
        return Err(DebtError::InvalidInput {
            message: "the debt title must be 1..200 characters".to_string(),
        });
    }
    Ok(())
}

/// Reject a severity outside the fixed vocabulary without echoing the value.
fn validate_severity(severity: &str) -> Result<()> {
    if !SEVERITIES.contains(&severity) {
        return Err(DebtError::InvalidInput {
            message: "the debt severity must be 'info' or 'warning'".to_string(),
        });
    }
    Ok(())
}

/// Reject a status outside the fixed lifecycle vocabulary without echoing it.
fn validate_status(status: &str) -> Result<()> {
    if !STATUSES.contains(&status) {
        return Err(DebtError::InvalidInput {
            message: "the debt status must be 'open', 'accepted', 'fixed', or 'wontfix'"
                .to_string(),
        });
    }
    Ok(())
}

/// Reject an empty or overlong location without echoing it.
fn validate_location(location: Option<&str>) -> Result<()> {
    if let Some(value) = location {
        let len = value.chars().count();
        if len == 0 || len > MAX_LOCATION_CHARS {
            return Err(DebtError::InvalidInput {
                message: "the debt location must be 1..1024 characters".to_string(),
            });
        }
    }
    Ok(())
}

/// Reject an overlong note without echoing it.
fn validate_note(note: Option<&str>) -> Result<()> {
    if let Some(value) = note {
        if value.chars().count() > MAX_NOTE_CHARS {
            return Err(DebtError::InvalidInput {
                message: "the debt note must be at most 4000 characters".to_string(),
            });
        }
    }
    Ok(())
}

/// Truncate text to `max` characters (char-boundary safe — byte slicing
/// could split multi-byte text).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Map one audit finding onto an importable debt row
/// `(title, location, note, audit_key)`, or `None` when the finding's key
/// would exceed the length cap (skipped rather than truncated into a
/// possible collision — the import reports only inserted rows, so a skipped
/// overlong key simply never appears).
fn import_row(finding: &AuditFinding) -> Option<(String, String, String, String)> {
    let location = format!("{}:{}", finding.path, finding.line);
    if location.chars().count() > MAX_LOCATION_CHARS {
        return None;
    }
    let key = format!(
        "{}{KEY_SEPARATOR}{}{KEY_SEPARATOR}{}",
        finding.kind, finding.path, finding.line
    );
    if key.chars().count() > MAX_AUDIT_KEY_CHARS {
        return None;
    }
    let title = truncate_chars(&format!("{} at {location}", finding.kind), MAX_TITLE_CHARS);
    let note = truncate_chars(&finding.excerpt, MAX_NOTE_CHARS);
    Some((title, location, note, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn service(db: &Database) -> DebtService<'_> {
        DebtService::new(db)
    }

    /// Write one minimal Rust source file with a TODO marker into a fresh
    /// temporary workspace root (mirrors the audit engine's own test
    /// scaffolding shape: files under a temp dir, scanned as text).
    fn workspace_with_todo(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nexora-debt-import-test-{tag}-{}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root).expect("clear stale workspace");
        }
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(
            root.join("sample.rs"),
            "pub fn sample() {\n    // TODO: pay this down\n}\n",
        )
        .expect("write sample source");
        root
    }

    #[test]
    fn seed_list_status_transitions_round_trip() {
        let db = in_memory_database();
        let debt = service(&db);

        let id = debt
            .add(
                "stale clone in chat view",
                SEVERITY_WARNING,
                Some("src/view.rs:12"),
                None,
            )
            .expect("add");
        let listed = debt.list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].source, SOURCE_MANUAL);
        assert_eq!(listed[0].status, STATUS_OPEN);

        for status in [STATUS_ACCEPTED, STATUS_FIXED, STATUS_OPEN, STATUS_WONT_FIX] {
            debt.update_status(id, status).expect("transition");
            let row = debt.list().expect("list").into_iter().next().expect("row");
            assert_eq!(row.status, status, "status must transition to {status}");
        }

        debt.delete(id).expect("delete");
        assert!(debt.list().expect("list").is_empty());
    }

    #[test]
    fn update_status_of_unknown_item_is_not_found() {
        let db = in_memory_database();
        let debt = service(&db);

        let err = debt
            .update_status(42, STATUS_FIXED)
            .expect_err("unknown item");
        assert!(matches!(err, DebtError::ItemNotFound { id: 42 }));
    }

    #[test]
    fn delete_of_unknown_item_is_a_no_op() {
        let db = in_memory_database();
        let debt = service(&db);
        debt.delete(42).expect("delete unknown succeeds");
    }

    #[test]
    fn add_validates_title_severity_location_and_note() {
        let db = in_memory_database();
        let debt = service(&db);

        assert!(matches!(
            debt.add("", SEVERITY_INFO, None, None)
                .expect_err("empty title"),
            DebtError::InvalidInput { .. }
        ));
        assert!(matches!(
            debt.add(&"t".repeat(201), SEVERITY_INFO, None, None)
                .expect_err("overlong title"),
            DebtError::InvalidInput { .. }
        ));
        assert!(matches!(
            debt.add("t", "critical", None, None)
                .expect_err("bad severity"),
            DebtError::InvalidInput { .. }
        ));
        assert!(matches!(
            debt.update_status(1, "archived").expect_err("bad status"),
            DebtError::InvalidInput { .. }
        ));
        assert!(matches!(
            debt.add("t", SEVERITY_INFO, Some(""), None)
                .expect_err("empty location"),
            DebtError::InvalidInput { .. }
        ));
        assert!(matches!(
            debt.add("t", SEVERITY_INFO, None, Some(&"n".repeat(4001)))
                .expect_err("overlong note"),
            DebtError::InvalidInput { .. }
        ));
        assert!(debt.list().expect("list").is_empty(), "nothing persisted");
    }

    #[test]
    fn import_creates_rows_without_duplicates_on_re_run() {
        let db = in_memory_database();
        let debt = service(&db);
        let root = workspace_with_todo("rerun");

        let first = debt.import_from_audit(&root).expect("first import");
        assert!(
            first > 0,
            "the TODO fixture must surface at least one finding"
        );
        let rows = debt.list().expect("list");
        assert_eq!(rows.len(), first);
        assert!(
            rows.iter()
                .all(|row| row.source == SOURCE_AUDIT && row.status == STATUS_OPEN),
            "imports start as open audit rows"
        );
        assert!(
            rows.iter().all(|row| row.audit_key.is_some()),
            "imports carry an identity key"
        );

        // Triage one row, then re-import: the count is zero and the triage
        // status survives (the ignore path never touches existing rows).
        let triaged = rows[0].id;
        debt.update_status(triaged, STATUS_ACCEPTED)
            .expect("triage");
        let second = debt.import_from_audit(&root).expect("second import");
        assert_eq!(second, 0, "re-run over unchanged sources inserts nothing");
        let rows = debt.list().expect("list");
        assert_eq!(rows.len(), first, "no duplicates on re-run");
        let kept = rows.iter().find(|row| row.id == triaged).expect("row kept");
        assert_eq!(kept.status, STATUS_ACCEPTED, "triage survives re-import");

        std::fs::remove_dir_all(&root).expect("clear workspace");
    }

    #[test]
    fn import_on_invalid_root_is_an_audit_error() {
        let db = in_memory_database();
        let debt = service(&db);

        let missing = std::env::temp_dir().join(format!(
            "nexora-debt-missing-{}-{}",
            std::process::id(),
            "no-such-dir"
        ));
        if missing.exists() {
            std::fs::remove_dir_all(&missing).expect("clear stale dir");
        }
        let err = debt.import_from_audit(&missing).expect_err("invalid root");
        assert!(
            matches!(err, DebtError::Audit(RepoAuditError::InvalidRoot)),
            "invalid root must surface as an audit error"
        );
    }
}
