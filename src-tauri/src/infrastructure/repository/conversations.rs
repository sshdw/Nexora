//! Conversation repository: persistence for the `conversations` table
//! (DATABASE.md §7.1).
//!
//! `conversations` stores the AI conversation entities that back FR-002,
//! FR-005, FR-006, and FR-013. This repository is the only data-access path
//! for that table and reuses the [`Repository`] foundation, so it never
//! duplicates connection or transaction handling, nor error conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `conversations` without interpreting them. Validation
//! (for example the `title` length and `status` enumeration CHECKs and the
//! `New Conversation` naming rule) and other business rules intentionally
//! live in higher application layers and are not enforced here.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError, Transaction};
use serde::Serialize;

/// A single `conversations` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.1. It is a plain persistence record and carries no
/// interpretation or business meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Conversation {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Human-readable name (`title`).
    pub title: String,
    /// Archive state (`status`), stored as the value recorded in the column.
    pub status: String,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
    /// Last modification timestamp (`updated_at`).
    pub updated_at: i64,
}

/// Repository for the `conversations` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct ConversationRepository<'a> {
    db: &'a Database,
}

impl<'a> ConversationRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for ConversationRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl ConversationRepository<'_> {
    /// Insert a new conversation (DATABASE.md §7.1).
    ///
    /// Persists the caller-supplied `title` and `status`. The surrogate `id`
    /// and the `created_at` / `updated_at` timestamps are assigned by the
    /// schema (defaults / trigger) and are not supplied here.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a `title`
    /// or `status` value rejected by the table CHECK constraints.
    pub(crate) fn create(&self, title: &str, status: &str) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO conversations (title, status) VALUES (?1, ?2)",
            params![title, status],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Insert a new conversation with explicit timestamps (DATABASE.md §7.1),
    /// used by import (FR-011) to preserve the exported record's
    /// `created_at` / `updated_at`. The surrogate `id` is still assigned by the
    /// schema; the exported id is deliberately not reused.
    ///
    /// Must be called inside the transaction supplied by
    /// [`Repository::transaction`] so it participates in any surrounding atomic
    /// operation.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a `title`,
    /// `status`, or timestamp value rejected by the table CHECK constraints.
    pub(crate) fn create_with_timestamps(
        tx: &Transaction<'_>,
        title: &str,
        status: &str,
        created_at: i64,
        updated_at: i64,
    ) -> Result<i64> {
        tx.execute(
            "INSERT INTO conversations (title, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![title, status, created_at, updated_at],
        )?;
        Ok(tx.last_insert_rowid())
    }

    /// Advance a conversation's `updated_at` to the current Unix time.
    ///
    /// The `conversations_touch_updated_at` trigger only fires on `UPDATE OF
    /// title, status`; a send inserts a new `messages` row and never changes a
    /// mutable `conversations` column, so it cannot touch recency through the
    /// trigger. This explicit write is the send's recency update and is made in
    /// the same transaction as the message insert so the conversation's recent
    /// activity is reflected atomically with the message itself (DATABASE.md
    /// §7.1, §12). Must be called inside the transaction supplied by
    /// [`Repository::transaction`].
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails.
    pub(crate) fn touch_updated_at(tx: &Transaction<'_>, id: i64) -> Result<()> {
        tx.execute(
            "UPDATE conversations SET updated_at = (unixepoch()) WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Delete **every** `conversations` row (DATABASE.md §7.1).
    ///
    /// Linked `messages` and `attachments` — including draft attachments whose
    /// `message_id` is `NULL` — are removed by the schema's `ON DELETE CASCADE`
    /// foreign keys, and their FTS index rows by the synchronization triggers
    /// (DATABASE.md §9, §11). Must be called inside the transaction supplied by
    /// [`Repository::transaction`] so it participates in an atomic "clear all
    /// application data" operation (FR-013; ROADMAP.md Phase 9).
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn clear_in_transaction(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("DELETE FROM conversations", [])?;
        Ok(())
    }

    /// Read a conversation by `id`.
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no conversation
    /// with that `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Conversation>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, title, status, created_at, updated_at \
             FROM conversations WHERE id = ?1",
            [id],
            |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        ) {
            Ok(conversation) => Ok(Some(conversation)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Update the mutable fields of an existing conversation by `id`.
    ///
    /// Only `title` and `status` are written, matching the fields DATABASE.md
    /// §7.1 defines as mutable; `id`, `created_at`, and `updated_at` are never
    /// touched here (`updated_at` is maintained by the schema trigger). If `id`
    /// does not exist, no row is changed.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a value
    /// rejected by the table CHECK constraints.
    pub(crate) fn update(&self, id: i64, title: &str, status: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE conversations SET title = ?2, status = ?3 WHERE id = ?1",
            params![id, title, status],
        )?;
        Ok(())
    }

    /// Delete a conversation by `id`.
    ///
    /// Deleting a non-existent `id` is a no-op. Cascading deletion of linked
    /// `messages` and `attachments` is enforced by the schema's foreign keys
    /// and is not handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Return whether a conversation with `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check fails.
    pub(crate) fn exists(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// List all conversations.
    ///
    /// Rows are ordered by `updated_at` descending, which is the repository's
    /// primary retrieval use case: DATABASE.md §7.1 notes `updated_at` tracks
    /// recency for conversation listing and sorting (FR-006). No filtering,
    /// search, or pagination is applied here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Conversation>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, status, created_at, updated_at \
             FROM conversations ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        let mut conversations = Vec::new();
        for row in rows {
            conversations.push(row?);
        }
        Ok(conversations)
    }
}
