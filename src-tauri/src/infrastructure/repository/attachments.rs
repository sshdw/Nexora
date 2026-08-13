//! Attachment repository: persistence for the `attachments` table
//! (DATABASE.md §7.4).
//!
//! `attachments` tracks local files attached to AI requests (FR-008). A row in
//! the draft state has `message_id IS NULL`; it is linked to a message when
//! the request is sent (`message_id` set). This repository is the only
//! data-access path for that table and reuses the [`Repository`] foundation,
//! so it never duplicates connection or transaction handling, nor error
//! conversion.
//!
//! This repository is responsible **only** for persistence: it stores and
//! retrieves rows in `attachments` without interpreting them. Validation (for
//! example the `file_name` / `mime_type` length CHECKs or file-size limits),
//! any filesystem operations, and all business rules intentionally live in
//! higher application layers and are not enforced here.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::{params, Error as SqliteError};

/// A single `attachments` row as persisted, mirroring the columns defined by
/// DATABASE.md §7.4. It is a plain persistence record carrying the raw stored
/// values only, with no interpretation or business meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attachment {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning conversation foreign key (`conversation_id`).
    pub conversation_id: i64,
    /// Associated message (`message_id`), `None` while the attachment is still
    /// in the draft state (not yet linked to a message).
    pub message_id: Option<i64>,
    /// Display name (`file_name`).
    pub file_name: String,
    /// Absolute filesystem path (`file_path`).
    pub file_path: String,
    /// File size in bytes (`file_size_bytes`), `None` when not recorded.
    pub file_size_bytes: Option<i64>,
    /// Media type (`mime_type`), `None` when not recorded.
    pub mime_type: Option<String>,
}

/// Repository for the `attachments` table.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct AttachmentRepository<'a> {
    db: &'a Database,
}

impl<'a> AttachmentRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for AttachmentRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl AttachmentRepository<'_> {
    /// Insert a new draft attachment (DATABASE.md §7.4).
    ///
    /// Persists the caller-supplied `conversation_id`, `file_name`, `file_path`,
    /// `file_size_bytes`, and `mime_type`. `message_id` is stored as `NULL`,
    /// placing the row in the draft state (not yet linked to a message). The
    /// surrogate `id` is assigned by the schema.
    ///
    /// Returns the `id` of the newly inserted row.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the insert fails, for example a missing
    /// `conversation_id` (foreign-key violation) or a `file_name` / `file_path`
    /// value rejected by the table CHECK constraints.
    pub(crate) fn create(
        &self,
        conversation_id: i64,
        file_name: &str,
        file_path: &str,
        file_size_bytes: Option<i64>,
        mime_type: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO attachments \
             (conversation_id, message_id, file_name, file_path, file_size_bytes, mime_type) \
             VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
            params![
                conversation_id,
                file_name,
                file_path,
                file_size_bytes,
                mime_type
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read an attachment by `id`.
    ///
    /// Returns [`Some`] with the matched row, or [`None`] when no attachment
    /// with that `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] on a failed query or a poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Attachment>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT id, conversation_id, message_id, file_name, file_path, \
             file_size_bytes, mime_type FROM attachments WHERE id = ?1",
            [id],
            |row| {
                Ok(Attachment {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    message_id: row.get(2)?,
                    file_name: row.get(3)?,
                    file_path: row.get(4)?,
                    file_size_bytes: row.get(5)?,
                    mime_type: row.get(6)?,
                })
            },
        ) {
            Ok(attachment) => Ok(Some(attachment)),
            Err(SqliteError::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(DatabaseError::from(err)),
        }
    }

    /// Link an attachment to a message by updating only its `message_id`
    /// (DATABASE.md §7.4: "UPDATE of `message_id` only").
    ///
    /// `message_id` is the only mutable field; `id`, `conversation_id`,
    /// `file_name`, `file_path`, `file_size_bytes`, and `mime_type` are never
    /// touched here. Passing [`None`] returns the row to the draft state; if
    /// `id` does not exist, no row is changed.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the update fails, for example a
    /// `message_id` that violates the foreign key or CHECK constraint.
    pub(crate) fn update_message_id(&self, id: i64, message_id: Option<i64>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE attachments SET message_id = ?2 WHERE id = ?1",
            params![id, message_id],
        )?;
        Ok(())
    }

    /// Delete an attachment by `id`.
    ///
    /// Deleting a non-existent `id` is a no-op. Deleting by primary key only:
    /// cascading deletion from a `conversations` or `messages` deletion is
    /// enforced by the schema's foreign keys and is not handled here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM attachments WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Return whether an attachment with `id` exists.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the existence check fails.
    pub(crate) fn exists(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM attachments WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// List the draft attachments of one conversation.
    ///
    /// Rows are filtered to draft attachments only (`conversation_id = ?` and
    /// `message_id IS NULL`), matching the DATABASE.md §7.4 read behavior for
    /// pre-submission attachments (FR-008). DATABASE.md defines no explicit
    /// ordering for this list, so rows are ordered by `id` ascending (stable
    /// insertion order; the `attachments` table has no timestamp column). No
    /// pagination, search, or archive logic is applied here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_by_conversation(&self, conversation_id: i64) -> Result<Vec<Attachment>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, message_id, file_name, file_path, \
             file_size_bytes, mime_type FROM attachments \
             WHERE conversation_id = ?1 AND message_id IS NULL ORDER BY id",
        )?;
        let rows = stmt.query_map([conversation_id], |row| {
            Ok(Attachment {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                message_id: row.get(2)?,
                file_name: row.get(3)?,
                file_path: row.get(4)?,
                file_size_bytes: row.get(5)?,
                mime_type: row.get(6)?,
            })
        })?;
        let mut attachments = Vec::new();
        for row in rows {
            attachments.push(row?);
        }
        Ok(attachments)
    }

    /// List the historical attachments linked to one message.
    ///
    /// Rows are filtered to attachments whose `message_id` equals the given
    /// value, matching the DATABASE.md §7.4 read behavior for message-linked
    /// attachments. DATABASE.md defines no explicit ordering for this list, so
    /// rows are ordered by `id` ascending (stable insertion order; the
    /// `attachments` table has no timestamp column). No pagination, search, or
    /// archive logic is applied here.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if listing fails.
    pub(crate) fn list_by_message(&self, message_id: i64) -> Result<Vec<Attachment>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, message_id, file_name, file_path, \
             file_size_bytes, mime_type FROM attachments \
             WHERE message_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([message_id], |row| {
            Ok(Attachment {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                message_id: row.get(2)?,
                file_name: row.get(3)?,
                file_path: row.get(4)?,
                file_size_bytes: row.get(5)?,
                mime_type: row.get(6)?,
            })
        })?;
        let mut attachments = Vec::new();
        for row in rows {
            attachments.push(row?);
        }
        Ok(attachments)
    }
}
