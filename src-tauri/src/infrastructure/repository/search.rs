//! Search repository: FTS5-backed persistence reads for offline local search
//! (FR-009; ROADMAP.md Phase 7 — Local Search; DATABASE.md §10–§11).
//!
//! The database keeps the search index in three FTS5 virtual tables
//! (DATABASE.md §10): `conversations_fts` (indexes `conversations.title`),
//! `messages_fts` (indexes `messages.content`), and `prompts_fts` (indexes
//! `prompts.title` and `prompts.content`), maintained by database triggers on
//! `INSERT`, `UPDATE`, and `DELETE` (DATABASE.md §11) so the index can never
//! drift from the persisted rows. This repository is the only data-access path
//! for those indexes and reuses the [`Repository`] foundation, so it never
//! duplicates connection or error handling.
//!
//! This repository is responsible **only** for the FTS reads: it matches a
//! query against an index, joins each matched `rowid` back to the full
//! persisted row (reusing the existing [`Conversation`], [`Message`], and
//! [`Prompt`] row types), and orders results by the index's relevance `rank`.
//! It performs no interpretation or business logic; validation and result
//! shaping intentionally live in higher application layers.
//!
//! The FTS indexes live entirely inside `SQLite`, so every search works fully
//! offline (FR-009, FR-015). The tokenizer is the `unicode61` default chosen
//! by the schema; tokenizer selection is explicitly an implementation decision
//! (DATABASE.md §10).

use crate::infrastructure::database::Database;
use crate::infrastructure::repository::conversations::Conversation;
use crate::infrastructure::repository::messages::Message;
use crate::infrastructure::repository::prompts::Prompt;
use crate::infrastructure::repository::{Repository, Result};
use rusqlite::params;

/// Repository over the FTS5 search indexes.
///
/// Implements [`Repository`], supplying the shared [`Database`] handle, and
/// inherits connection and transaction handling from the foundation. It is
/// deliberately focused purely on persistence.
pub(crate) struct SearchRepository<'a> {
    db: &'a Database,
}

impl<'a> SearchRepository<'a> {
    /// Create a repository over the shared application [`Database`].
    pub(crate) const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl Repository for SearchRepository<'_> {
    fn db(&self) -> &Database {
        self.db
    }
}

impl SearchRepository<'_> {
    /// Search conversation titles through the `conversations_fts` index
    /// (FR-009, DATABASE.md §10).
    ///
    /// `query` is passed to the FTS `MATCH` expression as supplied. Rows are
    /// ordered by the index's relevance `rank` (best match first), with `id`
    /// as a deterministic tiebreaker. A query that matches nothing yields an
    /// empty result, not an error.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the query fails, including a malformed
    /// FTS `MATCH` expression or a missing `conversations_fts` index.
    pub(crate) fn search_conversations(&self, query: &str) -> Result<Vec<Conversation>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.status, c.created_at, c.updated_at \
             FROM conversations_fts \
             JOIN conversations c ON c.id = conversations_fts.rowid \
             WHERE conversations_fts MATCH ?1 \
             ORDER BY rank, c.id",
        )?;
        let rows = stmt.query_map(params![query], |row| {
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

    /// Search message contents through the `messages_fts` index (FR-009
    /// conversation content search, DATABASE.md §10).
    ///
    /// Each hit is a full persisted [`Message`] row and therefore carries the
    /// `conversation_id` an application layer needs to open the conversation
    /// it belongs to. `query` is passed to the FTS `MATCH` expression as
    /// supplied. Rows are ordered by relevance `rank`, with `id` as a
    /// deterministic tiebreaker.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the query fails, including a malformed
    /// FTS `MATCH` expression or a missing `messages_fts` index.
    pub(crate) fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.conversation_id, m.role, m.content, m.provider_id, \
             m.model_name, m.created_at \
             FROM messages_fts \
             JOIN messages m ON m.id = messages_fts.rowid \
             WHERE messages_fts MATCH ?1 \
             ORDER BY rank, m.id",
        )?;
        let rows = stmt.query_map(params![query], |row| {
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
    /// Search prompt titles and contents through the `prompts_fts` index
    /// (FR-009, DATABASE.md §10).
    ///
    /// `query` is passed to the FTS `MATCH` expression as supplied. Rows are
    /// ordered by relevance `rank`, with `id` as a deterministic tiebreaker.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if the query fails, including a malformed
    /// FTS `MATCH` expression or a missing `prompts_fts` index.
    pub(crate) fn search_prompts(&self, query: &str) -> Result<Vec<Prompt>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT p.id, p.title, p.content, p.created_at, p.updated_at \
             FROM prompts_fts \
             JOIN prompts p ON p.id = prompts_fts.rowid \
             WHERE prompts_fts MATCH ?1 \
             ORDER BY rank, p.id",
        )?;
        let rows = stmt.query_map(params![query], |row| {
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
