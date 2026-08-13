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
use rusqlite::{params, Error as SqliteError};

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
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// List all settings as `(key, value)` pairs.
    ///
    /// Pairs are ordered by `key`.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.conn()?;
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
