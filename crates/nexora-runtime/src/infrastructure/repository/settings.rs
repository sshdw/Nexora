//! Settings repository: persistence for the `app_settings` table (DATABASE.md
//! §7.6).
//!
//! `app_settings` stores application configuration as key-value pairs keyed by
//! `key` (TEXT PRIMARY KEY) with an optional `value`. It backs FR-012. This
//! repository is the only data-access path for that table and reuses the
//! [`Repository`] foundation, so it never duplicates connection or transaction
//! handling.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `app_settings` without interpreting them. Validation,
//! default-value application, and other business rules (including those
//! implied by FR-012) intentionally live in higher application layers and are
//! not enforced here.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError, Transaction};

/// Repository for the `app_settings` key-value table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct SettingsRepository<'a> {
    db: &'a Database,
}

impl<'a> SettingsRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for SettingsRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

/// Full state of a single setting as stored in the database, preserving the
/// distinction between an absent row and a `NULL` value without extra queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingValue {
    /// The setting `key` does not exist in `app_settings`.
    Missing,
    /// The setting exists with a `NULL` value.
    Null,
    /// The setting exists with a non-`NULL` value.
    Value(String),
}

impl SettingsRepository<'_> {
    /// Insert a new setting (DATABASE.md §7.6).
    ///
    /// `key` must not already exist; inserting a duplicate `key` violates the
    /// primary key constraint. `value` may be `NULL`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a
    /// duplicate `key`.
    pub(crate) fn create(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Read a setting by `key`, preserving the full database state.
    ///
    /// Returns [`SettingValue::Missing`] when no row exists,
    /// [`SettingValue::Null`] when the row exists with a `NULL` value, or
    /// [`SettingValue::Value`] with the stored value otherwise.
    ///
    /// This is a pure persistence read: the three states are distinguished
    /// from the single `SELECT` result, and no validation, defaulting, or
    /// other business rule is applied here (those belong to higher layers).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, key: &str) -> Result<SettingValue> {
        let conn = self.read_conn()?;
        match conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(Some(value)) => Ok(SettingValue::Value(value)),
            Ok(None) => Ok(SettingValue::Null),
            Err(SqliteError::QueryReturnedNoRows) => Ok(SettingValue::Missing),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Update the value of an existing setting by `key`.
    ///
    /// If `key` does not exist, no row is changed. `value` may be `NULL`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn update(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE app_settings SET value = ?2 WHERE key = ?1",
            params![key, value],
        )?;
        Ok(())
    }

    /// Delete a setting by `key`.
    ///
    /// Deleting a non-existent `key` is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, key: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM app_settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Return whether a setting with `key` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check fails.
    pub(crate) fn exists(&self, key: &str) -> Result<bool> {
        let conn = self.read_conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Delete **every** `app_settings` row (DATABASE.md §7.6; user preferences
    /// cleared by FR-013). Must be called inside the transaction supplied by
    /// [`Repository::transaction`] so it participates in an atomic "clear all
    /// application data" operation (FR-013; ROADMAP.md Phase 9).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn clear_in_transaction(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("DELETE FROM app_settings", [])?;
        Ok(())
    }

    /// List all settings as `(key, value)` pairs.
    ///
    /// Pairs are ordered by `key`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.read_conn()?;
        let mut stmt = conn.prepare("SELECT key, value FROM app_settings ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut settings = Vec::new();
        for row in rows {
            settings.push(row?);
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn repo(db: &Database) -> SettingsRepository<'_> {
        SettingsRepository::new(db)
    }

    #[test]
    fn create_read_round_trip_preserves_the_three_states() {
        let db = in_memory_database();
        let settings = repo(&db);

        // An absent key reads as Missing and does not exist...
        assert_eq!(
            settings.read("appearance").expect("read missing"),
            SettingValue::Missing
        );
        assert!(!settings.exists("appearance").expect("exists missing"));

        // ...a key with a value reads as Value...
        settings
            .create("appearance", Some("mono"))
            .expect("create with a value");
        assert_eq!(
            settings.read("appearance").expect("read with value"),
            SettingValue::Value("mono".to_owned())
        );
        assert!(settings.exists("appearance").expect("exists with value"));

        // ...and a key with an explicit NULL reads as Null, not Missing.
        settings
            .create("onboarding_done", None)
            .expect("create with NULL value");
        assert_eq!(
            settings.read("onboarding_done").expect("read NULL value"),
            SettingValue::Null
        );
        assert!(settings
            .exists("onboarding_done")
            .expect("exists NULL value"));
    }

    #[test]
    fn create_rejects_a_duplicate_key_instead_of_upserting() {
        let db = in_memory_database();
        let settings = repo(&db);

        settings
            .create("appearance", Some("mono"))
            .expect("first insert");
        assert!(
            settings.create("appearance", Some("warm")).is_err(),
            "app_settings.key is the PRIMARY KEY: a duplicate insert must fail, not upsert"
        );
        // The failed duplicate left the original value in place.
        assert_eq!(
            settings.read("appearance").expect("read after duplicate"),
            SettingValue::Value("mono".to_owned())
        );
    }

    #[test]
    fn update_changes_existing_keys_and_ignores_missing_ones() {
        let db = in_memory_database();
        let settings = repo(&db);

        settings.create("appearance", Some("mono")).expect("create");
        settings
            .update("appearance", Some("high-contrast"))
            .expect("update to a new value");
        assert_eq!(
            settings.read("appearance").expect("read updated value"),
            SettingValue::Value("high-contrast".to_owned())
        );

        // Updating to NULL preserves the row (Null, not Missing).
        settings.update("appearance", None).expect("update to NULL");
        assert_eq!(
            settings.read("appearance").expect("read updated NULL"),
            SettingValue::Null
        );

        // Updating an unknown key changes nothing and is not an error.
        settings
            .update("absent", Some("value"))
            .expect("update of a missing key");
        assert_eq!(
            settings.read("absent").expect("read absent after update"),
            SettingValue::Missing
        );
    }

    #[test]
    fn delete_removes_the_row_and_missing_delete_is_a_no_op() {
        let db = in_memory_database();
        let settings = repo(&db);

        settings.create("appearance", Some("mono")).expect("create");
        settings.delete("appearance").expect("delete");
        assert_eq!(
            settings.read("appearance").expect("read deleted"),
            SettingValue::Missing
        );
        assert!(!settings.exists("appearance").expect("exists deleted"));

        // Deleting a non-existent key is a no-op.
        settings.delete("absent").expect("delete of a missing key");
    }

    #[test]
    fn list_returns_value_pairs_ordered_by_key() {
        let db = in_memory_database();
        let settings = repo(&db);

        settings
            .create("spend_limit", Some("10"))
            .expect("create spend_limit");
        settings.create("autonomy", None).expect("create autonomy");
        settings
            .create("appearance", Some("mono"))
            .expect("create appearance");

        let listed = settings.list().expect("list");
        assert_eq!(
            listed,
            vec![
                ("appearance".to_owned(), Some("mono".to_owned())),
                ("autonomy".to_owned(), None),
                ("spend_limit".to_owned(), Some("10".to_owned())),
            ]
        );
    }

    #[test]
    fn check_constraints_reject_invalid_values() {
        let db = in_memory_database();
        let settings = repo(&db);

        assert!(
            settings.create("", Some("v")).is_err(),
            "an empty key must be rejected (length(key) > 0)"
        );
        assert!(
            settings
                .create("k".repeat(201).as_str(), Some("v"))
                .is_err(),
            "a key over 200 chars must be rejected (length(key) <= 200)"
        );
        assert!(
            settings
                .create("k", Some("v".repeat(10001).as_str()))
                .is_err(),
            "a value over 10000 chars must be rejected (length(value) <= 10000)"
        );

        // Boundary values are accepted, pinning the CHECK limits.
        settings
            .create("k".repeat(200).as_str(), Some("v".repeat(10000).as_str()))
            .expect("boundary key and value are accepted");
    }

    #[test]
    fn clear_in_transaction_deletes_every_row() {
        let db = in_memory_database();
        let settings = repo(&db);

        settings.create("appearance", Some("mono")).expect("create");
        settings.create("spend_limit", Some("10")).expect("create");

        // clear_in_transaction must run inside Repository::transaction so it
        // participates in the atomic "clear all application data" operation
        // (FR-013).
        settings
            .transaction(SettingsRepository::clear_in_transaction)
            .expect("clear inside a transaction");

        assert!(settings.list().expect("list after clear").is_empty());
        assert_eq!(
            settings.read("appearance").expect("read after clear"),
            SettingValue::Missing
        );
    }
}
