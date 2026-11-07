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
        let conn = self.read_conn()?;
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
        let conn = self.read_conn()?;
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
        let conn = self.read_conn()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    fn repo(db: &Database) -> ProviderRepository<'_> {
        ProviderRepository::new(db)
    }

    #[test]
    fn create_read_round_trip_by_id_and_by_name() {
        let db = in_memory_database();
        let providers = repo(&db);

        let id = providers.create("openai", "OpenAI").expect("create");
        assert!(id > 0, "the surrogate id is assigned by the schema");

        let by_id = providers.read(id).expect("read by id").expect("exists");
        assert_eq!(by_id.name, "openai");
        assert_eq!(by_id.display_name, "OpenAI");

        // read_by_name resolves the same row: the application layer uses it
        // for the keyring lookup by internal name (DATABASE.md §7.5).
        let by_name = providers
            .read_by_name("openai")
            .expect("read by name")
            .expect("exists");
        assert_eq!(by_name, by_id);

        assert!(
            providers.read(42).expect("read unknown id").is_none(),
            "an unknown id reads as None, not an error"
        );
        assert!(
            providers
                .read_by_name("absent")
                .expect("read unknown name")
                .is_none(),
            "an unknown name reads as None, not an error"
        );
    }

    #[test]
    fn create_rejects_a_duplicate_name() {
        let db = in_memory_database();
        let providers = repo(&db);

        providers.create("openai", "OpenAI").expect("first insert");
        assert!(
            providers.create("openai", "Second label").is_err(),
            "providers.name is UNIQUE: a duplicate insert must fail"
        );
        assert_eq!(
            providers.list().expect("list after duplicate").len(),
            1,
            "the failed duplicate must not create a row"
        );
        // The surviving row is the original one, not the duplicate's label.
        let survivor = providers
            .read_by_name("openai")
            .expect("read survivor")
            .expect("exists");
        assert_eq!(survivor.display_name, "OpenAI");
    }

    #[test]
    fn check_constraints_reject_invalid_values() {
        let db = in_memory_database();
        let providers = repo(&db);

        assert!(
            providers.create("", "OpenAI").is_err(),
            "an empty name must be rejected (length(name) > 0)"
        );
        assert!(
            providers
                .create("p".repeat(101).as_str(), "OpenAI")
                .is_err(),
            "a name over 100 chars must be rejected (length(name) <= 100)"
        );
        assert!(
            providers.create("openai", "").is_err(),
            "an empty display_name must be rejected (length(display_name) > 0)"
        );

        // Boundary: a 100-char name is accepted, pinning the CHECK limit.
        providers
            .create("p".repeat(100).as_str(), "Boundary")
            .expect("a 100-char name is accepted");
    }

    #[test]
    fn list_is_in_insertion_order_and_delete_removes() {
        let db = in_memory_database();
        let providers = repo(&db);

        let first = providers.create("openai", "OpenAI").expect("first");
        let second = providers.create("anthropic", "Anthropic").expect("second");

        // No timestamp column: rows are ordered by id ascending (insertion
        // order), per the repository's documented listing behavior.
        let listed = providers.list().expect("list");
        let names: Vec<&str> = listed
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["openai", "anthropic"]);

        providers.delete(first).expect("delete first");
        assert!(providers.read(first).expect("read deleted").is_none());
        assert_eq!(providers.list().expect("list after delete").len(), 1);
        assert_eq!(
            providers
                .read(second)
                .expect("read survivor")
                .expect("survivor exists")
                .name,
            "anthropic",
            "deleting one provider must not touch the others"
        );

        // Deleting a non-existent id is a no-op.
        providers.delete(42).expect("delete of an unknown id");
    }

    #[test]
    fn clear_in_transaction_deletes_every_row() {
        let db = in_memory_database();
        let providers = repo(&db);

        providers.create("openai", "OpenAI").expect("first");
        providers.create("anthropic", "Anthropic").expect("second");

        // clear_in_transaction must run inside Repository::transaction so it
        // participates in the atomic "clear all application data" operation
        // (FR-013). Credentials stay in the OS keyring by construction: the
        // table holds non-sensitive metadata only.
        providers
            .transaction(ProviderRepository::clear_in_transaction)
            .expect("clear inside a transaction");

        assert!(providers.list().expect("list after clear").is_empty());
        assert!(providers
            .read_by_name("openai")
            .expect("read after clear")
            .is_none());
    }
}
