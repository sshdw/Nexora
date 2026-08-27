//! Provider repository: persistence for the `providers` table
//! (DATABASE.md §7.5).
//!
//! `providers` stores non-sensitive metadata for configured AI providers
//! (FR-004, FR-014). This repository is the only data-access path for that
//! table and reuses the [`Repository`] foundation, so it never duplicates
//! connection or transaction handling, nor error conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `providers` without interpreting them. Validation (for
//! example the `name` / `display_name` length CHECKs) and other business rules
//! intentionally live in higher application layers and are not enforced here.
//! In particular, it never touches credentials: API keys, secrets, and tokens
//! belong exclusively to the OS keyring and are never stored in `SQLite`.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError, Transaction};
use serde::Serialize;

/// A single `providers` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.5. It is a plain persistence record carrying the raw stored
/// values only, with no interpretation or business meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Provider {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Internal identifier (`name`), unique and used as the keyring entry
    /// namespace key by the application layer.
    pub name: String,
    /// User-facing label (`display_name`).
    pub display_name: String,
}

/// Repository for the `providers` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct ProviderRepository<'a> {
    db: &'a Database,
}
impl<'a> ProviderRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for ProviderRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl ProviderRepository<'_> {
    /// Insert a new provider (DATABASE.md §7.5).
    ///
    /// Persists the caller-supplied `name` and `display_name`. The surrogate
    /// `id` is assigned by the schema. Provider metadata is immutable in the
    /// MVP, so there is deliberately no update operation here.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a `name`
    /// or `display_name` value rejected by the table CHECK constraints, or a
    /// duplicate `name` (UNIQUE constraint).
    pub(crate) fn create(&self, name: &str, display_name: &str) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO providers (name, display_name) VALUES (?1, ?2)",
            params![name, display_name],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read a provider by `id`.
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no provider
    /// with that `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Provider>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, name, display_name FROM providers WHERE id = ?1",
            [id],
            |row| {
                Ok(Provider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    display_name: row.get(2)?,
                })
            },
        ) {
            Ok(provider) => Ok(Some(provider)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Read a provider by its unique internal `name` (DATABASE.md §7.5 index
    /// `name`, used to resolve the provider by internal name for keyring
    /// lookup).
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no provider
    /// with that `name` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read_by_name(&self, name: &str) -> Result<Option<Provider>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, name, display_name FROM providers WHERE name = ?1",
            [name],
            |row| {
                Ok(Provider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    display_name: row.get(2)?,
                })
            },
        ) {
            Ok(provider) => Ok(Some(provider)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// List every provider.
    ///
    /// DATABASE.md §7.5 defines no explicit ordering for this list, so rows
    /// are ordered by `id` ascending (stable insertion order; the `providers`
    /// table has no timestamp column). No filtering or pagination is applied
    /// here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Provider>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id, name, display_name FROM providers ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Provider {
                id: row.get(0)?,
                name: row.get(1)?,
                display_name: row.get(2)?,
            })
        })?;
        let mut providers = Vec::new();
        for row in rows {
            providers.push(row?);
        }
        Ok(providers)
    }

    /// Delete a provider by `id`.
    ///
    /// Deleting a non-existent `id` is a no-op. The `SET NULL` on linked
    /// `messages.provider_id` is enforced by the schema's foreign keys
    /// (`ON DELETE SET NULL`) and is not handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM providers WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Delete **every** non-sensitive provider-metadata row (DATABASE.md §7.5).
    ///
    /// Provider **credentials** are never stored in `SQLite` (ARCHITECTURE.md
    /// §12; DATABASE.md §14): they belong exclusively to the OS secure keyring
    /// and are deliberately untouched here. Must be called inside the
    /// transaction supplied by [`Repository::transaction`] so it participates in
    /// an atomic "clear all application data" operation (FR-013; ROADMAP.md
    /// Phase 9).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn clear_in_transaction(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("DELETE FROM providers", [])?;
        Ok(())
    }
}
