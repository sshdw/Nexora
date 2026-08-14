//! Prompt repository: persistence for the `prompts` table (DATABASE.md §7.3).
//!
//! `prompts` stores reusable prompt templates that back FR-007. This
//! repository is the only data-access path for that table and reuses the
//! [`Repository`] foundation, so it never duplicates connection or transaction
//! handling, nor error conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `prompts` without interpreting them. Validation (for
//! example the `title` and `content` length CHECKs) intentionally lives in
//! higher application layers, and business rules belong to higher layers; none
//! of that is enforced here.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError, Transaction};

/// A single `prompts` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.3. It is a plain persistence record carrying the raw stored
/// values only, with no interpretation or business meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Prompt {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Prompt name (`title`).
    pub title: String,
    /// Prompt text (`content`).
    pub content: String,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
    /// Last edit timestamp (`updated_at`).
    pub updated_at: i64,
}

/// Repository for the `prompts` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct PromptRepository<'a> {
    db: &'a Database,
}

impl<'a> PromptRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl PromptRepository<'_> {
    /// Insert a new prompt (DATABASE.md §7.3).
    ///
    /// Persists the caller-supplied `title` and `content`. The surrogate `id`
    /// and the `created_at` / `updated_at` timestamps are assigned by the
    /// schema (defaults / trigger) and are not supplied here.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a `title`
    /// or `content` value rejected by the table CHECK constraints.
    pub(crate) fn create(&self, title: &str, content: &str) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO prompts (title, content) VALUES (?1, ?2)",
            params![title, content],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read a prompt by `id`.
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no prompt with
    /// that `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Prompt>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, title, content, created_at, updated_at \
             FROM prompts WHERE id = ?1",
            [id],
            |row| {
                Ok(Prompt {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        ) {
            Ok(prompt) => Ok(Some(prompt)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Update the mutable fields of an existing prompt by `id`.
    ///
    /// Only `title` and `content` are written, matching the fields DATABASE.md
    /// §7.3 defines as mutable; `id`, `created_at`, and `updated_at` are never
    /// touched here (`updated_at` is maintained by the schema trigger). If
    /// `id` does not exist, no row is changed.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a value
    /// rejected by the table CHECK constraints.
    pub(crate) fn update(&self, id: i64, title: &str, content: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE prompts SET title = ?2, content = ?3 WHERE id = ?1",
            params![id, title, content],
        )?;
        Ok(())
    }

    /// Delete a prompt by `id`.
    ///
    /// Deleting a non-existent `id` is a no-op. Prompts are standalone entities
    /// (DATABASE.md §7.3: no cascade), so deleting by primary key alone fully
    /// removes the row; any cascading behavior would be enforced by the
    /// schema's foreign keys and is not handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM prompts WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Delete **every** `prompts` row (DATABASE.md §7.3) and, through the
    /// FTS synchronization triggers, their `prompts_fts` index rows (DATABASE.md
    /// §11). Must be called inside the transaction supplied by
    /// [`Repository::transaction`] so it participates in an atomic "clear all
    /// application data" operation (FR-013; ROADMAP.md Phase 9).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn clear_in_transaction(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("DELETE FROM prompts", [])?;
        Ok(())
    }

    /// Return whether a prompt with `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check fails.
    pub(crate) fn exists(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM prompts WHERE id = ?1", [id], |row| {
                row.get(0)
            })?;
        Ok(count > 0)
    }

    /// List all prompts.
    ///
    /// Rows are ordered by `created_at` ascending. DATABASE.md §7.3 ties
    /// `created_at` to FR-007 library organization and does not otherwise
    /// define a list ordering beyond returning all rows, so creation order is
    /// used. No filtering, search, pagination, FTS, or archive logic is applied
    /// here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Prompt>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, content, created_at, updated_at \
             FROM prompts ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Prompt {
                id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        let mut prompts = Vec::new();
        for row in rows {
            prompts.push(row?);
        }
        Ok(prompts)
    }
}

impl Repository for PromptRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}
