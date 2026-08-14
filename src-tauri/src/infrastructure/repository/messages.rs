//! Message repository: persistence for the `messages` table (DATABASE.md §7.2).
//!
//! `messages` stores the individual messages within conversations (FR-002,
//! FR-003, FR-004, FR-005). This repository is the only data-access path for
//! that table and reuses the [`Repository`] foundation, so it never duplicates
//! connection or transaction handling, nor error conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `messages` without interpreting them. Validation (for
//! example the `role` enumeration and `content` non-empty CHECKs) and other
//! business rules intentionally live in higher application layers and are not
//! enforced here.
//!
//! Per DATABASE.md §7.2, messages are immutable after creation ("Update:
//! None"). The table has no mutable fields and no `update_at` column, so this
//! repository exposes no update method.

use serde::Serialize;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError, Transaction};

/// A single `messages` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.2. It is a plain persistence record and carries no
/// interpretation or business meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Message {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning conversation foreign key (`conversation_id`).
    pub conversation_id: i64,
    /// Message author type (`role`), stored as the value recorded in the
    /// column (`'user'` or `'assistant'`).
    pub role: String,
    /// Message text (`content`).
    pub content: String,
    /// AI provider used (`provider_id`), `None` when not set.
    pub provider_id: Option<i64>,
    /// Specific model used (`model_name`), `None` when not set.
    pub model_name: Option<String>,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
}

/// Repository for the `messages` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct MessageRepository<'a> {
    db: &'a Database,
}

impl<'a> MessageRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for MessageRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl MessageRepository<'_> {
    /// Insert a new message (DATABASE.md §7.2).
    ///
    /// Persists the caller-supplied `conversation_id`, `role`, `content`,
    /// `provider_id`, and `model_name`. The surrogate `id` and the `created_at`
    /// timestamp are assigned by the schema (defaults) and are not supplied
    /// here.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a missing
    /// `conversation_id` (foreign-key violation) or a `role` / `content` value
    /// rejected by the table CHECK constraints.
    pub(crate) fn create(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        provider_id: Option<i64>,
        model_name: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO messages \
             (conversation_id, role, content, provider_id, model_name) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![conversation_id, role, content, provider_id, model_name],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read a message by `id`.
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no message with
    /// that `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Message>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, conversation_id, role, content, provider_id, \
             model_name, created_at FROM messages WHERE id = ?1",
            [id],
            |row| {
                Ok(Message {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    provider_id: row.get(4)?,
                    model_name: row.get(5)?,
                    created_at: row.get(6)?,
                })
            },
        ) {
            Ok(message) => Ok(Some(message)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Insert a message with an explicit timestamp (DATABASE.md §7.2), used by
    /// import (FR-011) to preserve the exported message's `created_at` and
    /// therefore its chronological position within the conversation. The
    /// surrogate `id` is still assigned by the schema; the exported id is
    /// deliberately not reused. `provider_id` must be `NULL` or reference an
    /// existing `providers` row (enforced by the schema foreign key).
    ///
    /// Must be called inside the transaction supplied by
    /// [`Repository::transaction`] so it participates in any surrounding atomic
    /// operation.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a `role`,
    /// `content`, `provider_id`, or `model_name` value rejected by the table
    /// CHECK / foreign-key constraints.
    pub(crate) fn create_with_timestamps(
        tx: &Transaction<'_>,
        conversation_id: i64,
        role: &str,
        content: &str,
        provider_id: Option<i64>,
        model_name: Option<&str>,
        created_at: i64,
    ) -> Result<i64> {
        tx.execute(
            "INSERT INTO messages
                 (conversation_id, role, content, provider_id, model_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![conversation_id, role, content, provider_id, model_name, created_at],
        )?;
        Ok(tx.last_insert_rowid())
    }

    /// Delete a message by `id`.
    ///
    /// Deleting a non-existent `id` is a no-op. Deleting by primary key only:
    /// cascading deletion of linked `attachments` and `CASCADE` from
    /// `conversations` deletion is enforced by the schema's foreign keys and is
    /// not handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM messages WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Return whether a message with `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check fails.
    pub(crate) fn exists(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM messages WHERE id = ?1", [id], |row| {
                row.get(0)
            })?;
        Ok(count > 0)
    }

    /// List all messages belonging to one conversation.
    ///
    /// Rows are filtered only by `conversation_id` and ordered by `created_at`
    /// ascending, the ordering defined by DATABASE.md §7.2 ("SELECT by
    /// `conversation_id` ordered by `created_at`"), which preserves strict
    /// chronological order within a conversation per FR-005. No pagination,
    /// filtering, search, or archive logic is applied here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_by_conversation(&self, conversation_id: i64) -> Result<Vec<Message>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, provider_id, \
             model_name, created_at FROM messages \
             WHERE conversation_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([conversation_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                provider_id: row.get(4)?,
                model_name: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }
}
