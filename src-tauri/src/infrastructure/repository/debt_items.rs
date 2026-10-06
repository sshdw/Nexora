//! Debt-backlog repository: persistence for the `debt_items` table (v10
//! migration; debt backlog: manual entries plus idempotent imports of
//! `repo_audit` findings).
//!
//! `debt_items` stores one row per tracked technical-debt item: a title, the
//! origin (`source`: `'audit'` for scanner imports, `'manual'` for hand-added
//! entries), a severity reusing the audit `info` / `warning` vocabulary, a
//! triage lifecycle status (`open` / `accepted` / `fixed` / `wontfix`), an
//! optional code location (`location`, workspace-relative `path:line` for
//! imports), an optional longer note, and the import identity (`audit_key`,
//! `NULL` for manual rows).
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows without interpreting them. Triage policy (which statuses an
//! item may move to, how audit findings map onto rows) lives in the
//! application layer ([`crate::application::debt`]).
//!
//! - `updated_at` is refreshed explicitly on every status write (no trigger:
//!   only the status path touches it), keeping the `updated_at >= created_at`
//!   monotonic discipline of the sibling tables.
//! - Import idempotency is schema-enforced: `audit_key` is `UNIQUE`, so
//!   [`DebtItemRepository::insert_import`] uses `INSERT OR IGNORE` and
//!   re-running an import over the same findings creates no duplicates.
//!   `NULL` manual rows never collide (`SQLite` treats `NULL` `UNIQUE` members
//!   as distinct).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError};
use serde::Serialize;

/// A single `debt_items` row as persisted. It is a plain persistence record
/// and carries no interpretation; `source`, `severity`, and `status` hold the
/// column values (`'audit'` / `'manual'`, `'info'` / `'warning'`,
/// `'open'` / `'accepted'` / `'fixed'` / `'wontfix'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DebtItem {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Item title (`title`, 1..=200 chars).
    pub title: String,
    /// Item origin (`source`): `'audit'` for scanner imports, `'manual'` for
    /// hand-added entries.
    pub source: String,
    /// Severity (`severity`): `'info'` or `'warning'`.
    pub severity: String,
    /// Triage lifecycle status (`status`): `'open'` / `'accepted'` /
    /// `'fixed'` / `'wontfix'`.
    pub status: String,
    /// Optional code location (`location`, `None` when absent; imports store
    /// workspace-relative `path:line`).
    pub location: Option<String>,
    /// Optional longer note (`note`, `None` when absent; imports store the
    /// capped scanner excerpt).
    pub note: Option<String>,
    /// Import identity (`audit_key`, `None` for manual rows): `UNIQUE`, so
    /// re-imports skip rows they already created.
    pub audit_key: Option<String>,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
    /// Last mutation timestamp (`updated_at`).
    pub updated_at: i64,
}

/// Repository for the `debt_items` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct DebtItemRepository<'a> {
    db: &'a Database,
}

impl<'a> DebtItemRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for DebtItemRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl DebtItemRepository<'_> {
    /// Insert a new debt item row.
    ///
    /// `audit_key` is `Some` for scanner imports and `None` for manual rows.
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a value
    /// rejected by the table CHECK constraints or a duplicate `audit_key`
    /// (unique-violation).
    pub(crate) fn create(
        &self,
        title: &str,
        source: &str,
        severity: &str,
        location: Option<&str>,
        note: Option<&str>,
        audit_key: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO debt_items (title, source, severity, location, note, audit_key) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![title, source, severity, location, note, audit_key],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Insert an imported scanner row, ignoring it when its `audit_key` was
    /// already imported. Returns `true` when a row was inserted, `false` when
    /// the key already existed (the idempotent re-run path).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails for any reason other
    /// than the duplicate key, for example a value rejected by the table
    /// CHECK constraints.
    pub(crate) fn insert_import(
        &self,
        title: &str,
        severity: &str,
        location: Option<&str>,
        note: Option<&str>,
        audit_key: &str,
    ) -> Result<bool> {
        let conn = self.conn()?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO debt_items (title, source, severity, location, note, audit_key) \
             VALUES (?1, 'audit', ?2, ?3, ?4, ?5)",
            params![title, severity, location, note, audit_key],
        )?;
        Ok(inserted == 1)
    }

    /// Read one debt item by `id`. Returns `Ok(None)` when no item exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the read fails.
    pub(crate) fn read(&self, id: i64) -> Result<Option<DebtItem>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, title, source, severity, status, location, note, audit_key, created_at, updated_at \
             FROM debt_items WHERE id = ?1",
            [id],
            row_to_debt_item,
        ) {
            Ok(item) => Ok(Some(item)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// List all debt items ordered by `updated_at` descending (most recently
    /// touched first), ties broken by `id` descending.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<DebtItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, source, severity, status, location, note, audit_key, created_at, updated_at \
             FROM debt_items ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_debt_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Move an item to a new triage `status`, refreshing `updated_at`.
    ///
    /// The caller owns the transition policy (the repository enforces no
    /// status precondition beyond the schema CHECK); writing an unknown id
    /// changes nothing.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a
    /// `status` value rejected by the table CHECK constraint.
    pub(crate) fn update_status(&self, id: i64, status: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE debt_items SET status = ?2, updated_at = (unixepoch()) WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// Delete a debt item by `id`. Deleting a non-existent `id` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM debt_items WHERE id = ?1", [id])?;
        Ok(())
    }
}

/// Map one `debt_items` row onto a [`DebtItem`] record.
fn row_to_debt_item(row: &rusqlite::Row<'_>) -> std::result::Result<DebtItem, SqliteError> {
    Ok(DebtItem {
        id: row.get(0)?,
        title: row.get(1)?,
        source: row.get(2)?,
        severity: row.get(3)?,
        status: row.get(4)?,
        location: row.get(5)?,
        note: row.get(6)?,
        audit_key: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn repo(db: &Database) -> DebtItemRepository<'_> {
        DebtItemRepository::new(db)
    }

    #[test]
    fn create_read_update_delete_round_trip() {
        let db = in_memory_database();
        let items = repo(&db);

        let id = items
            .create(
                "stale clone in chat view",
                "manual",
                "warning",
                Some("src/view.rs:12"),
                None,
                None,
            )
            .expect("create");
        let created = items.read(id).expect("read").expect("exists");
        assert_eq!(created.title, "stale clone in chat view");
        assert_eq!(created.source, "manual");
        assert_eq!(created.severity, "warning");
        assert_eq!(created.status, "open");
        assert_eq!(created.location.as_deref(), Some("src/view.rs:12"));
        assert_eq!(created.audit_key, None);
        assert!(created.created_at > 0);
        assert!(created.updated_at >= created.created_at);

        items.update_status(id, "accepted").expect("accept");
        let accepted = items.read(id).expect("read").expect("exists");
        assert_eq!(accepted.status, "accepted");
        assert!(accepted.updated_at >= accepted.created_at);

        items.update_status(id, "fixed").expect("fix");
        let fixed = items.read(id).expect("read").expect("exists");
        assert_eq!(fixed.status, "fixed");

        items.delete(id).expect("delete");
        assert!(items.read(id).expect("read").is_none());
    }

    #[test]
    fn list_returns_all_items() {
        let db = in_memory_database();
        let items = repo(&db);
        items
            .create("first", "manual", "info", None, None, None)
            .expect("first");
        items
            .create("second", "manual", "warning", None, None, None)
            .expect("second");

        let listed = items.list().expect("list");
        assert_eq!(listed.len(), 2);
        let titles: Vec<&str> = listed.iter().map(|item| item.title.as_str()).collect();
        assert!(titles.contains(&"first"));
        assert!(titles.contains(&"second"));
    }

    #[test]
    fn read_returns_none_for_unknown_id() {
        let db = in_memory_database();
        let items = repo(&db);
        assert!(items.read(42).expect("read unknown").is_none());
    }

    #[test]
    fn delete_of_unknown_id_is_a_no_op() {
        let db = in_memory_database();
        let items = repo(&db);
        items.delete(42).expect("delete unknown succeeds");
    }

    #[test]
    fn check_constraints_reject_invalid_values() {
        let db = in_memory_database();
        let items = repo(&db);

        assert!(
            items
                .create("", "manual", "info", None, None, None)
                .is_err(),
            "empty title must be rejected"
        );
        assert!(
            items
                .create("t", "scanned", "info", None, None, None)
                .is_err(),
            "unknown source must be rejected"
        );
        assert!(
            items
                .create("t", "manual", "critical", None, None, None)
                .is_err(),
            "unknown severity must be rejected"
        );

        let id = items
            .create("t", "manual", "info", None, None, None)
            .expect("item");
        assert!(
            items.update_status(id, "archived").is_err(),
            "unknown status must be rejected"
        );
    }

    #[test]
    fn import_insert_is_idempotent_per_audit_key() {
        let db = in_memory_database();
        let items = repo(&db);

        let inserted = items
            .insert_import(
                "todo-debt at src/main.rs:7",
                "info",
                Some("src/main.rs:7"),
                Some("TODO"),
                "todo-debt\x1fsrc/main.rs\x1f7",
            )
            .expect("first import inserts");
        assert!(inserted);

        let row = items.read(1).expect("read").expect("exists");
        assert_eq!(row.source, "audit");
        assert_eq!(row.status, "open");

        let inserted_again = items
            .insert_import(
                "todo-debt at src/main.rs:7",
                "info",
                Some("src/main.rs:7"),
                Some("TODO"),
                "todo-debt\x1fsrc/main.rs\x1f7",
            )
            .expect("second import is ignored, not an error");
        assert!(!inserted_again);

        assert_eq!(items.list().expect("list").len(), 1);

        // A distinct finding (different key) still inserts.
        let other = items
            .insert_import(
                "unwrap-hotspot at src/main.rs:9",
                "warning",
                Some("src/main.rs:9"),
                None,
                "unwrap-hotspot\x1fsrc/main.rs\x1f9",
            )
            .expect("distinct key inserts");
        assert!(other);
        assert_eq!(items.list().expect("list").len(), 2);
    }

    #[test]
    fn manual_rows_never_collide_on_null_audit_key() {
        let db = in_memory_database();
        let items = repo(&db);

        items
            .create("one", "manual", "info", None, None, None)
            .expect("first manual");
        items
            .create("two", "manual", "info", None, None, None)
            .expect("second manual with NULL key");
        assert_eq!(items.list().expect("list").len(), 2);
    }
}
