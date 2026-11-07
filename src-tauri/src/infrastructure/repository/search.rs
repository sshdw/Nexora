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
        let conn = self.read_conn()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.status, c.created_at, c.updated_at, c.workspace_root \
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
                workspace_root: row.get(5)?,
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
        let conn = self.read_conn()?;
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
        let conn = self.read_conn()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;
    use crate::infrastructure::repository::prompts::PromptRepository;

    fn repo(db: &Database) -> SearchRepository<'_> {
        SearchRepository::new(db)
    }

    #[test]
    fn conversation_index_follows_inserts_updates_and_deletes() {
        let db = in_memory_database();
        let search = repo(&db);
        let conversations = ConversationRepository::new(&db);

        // Insert: the conversations_fts_insert trigger indexes the title.
        let id = conversations
            .create("Rust async patterns", "active")
            .expect("create conversation");
        let hits = search
            .search_conversations("rust")
            .expect("search after insert");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert_eq!(hits[0].title, "Rust async patterns");
        assert_eq!(hits[0].status, "active");
        assert!(hits[0].created_at > 0);
        assert!(
            hits[0].workspace_root.is_none(),
            "rows created without a workspace root read as None"
        );

        // Update: the conversations_fts_update trigger re-indexes, so the old
        // term goes stale and the new one matches.
        conversations
            .update(id, "Kanban board design", "active")
            .expect("update title");
        assert!(
            search
                .search_conversations("rust")
                .expect("search for the stale title")
                .is_empty(),
            "the pre-update title must no longer match"
        );
        let hits = search
            .search_conversations("kanban")
            .expect("search for the fresh title");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);

        // Delete: the conversations_fts_delete trigger de-indexes the row.
        conversations.delete(id).expect("delete conversation");
        assert!(
            search
                .search_conversations("kanban")
                .expect("search after delete")
                .is_empty(),
            "a deleted conversation must not be searchable"
        );
    }

    #[test]
    fn message_index_follows_inserts_and_deletes() {
        let db = in_memory_database();
        let search = repo(&db);
        let conversations = ConversationRepository::new(&db);
        let messages = MessageRepository::new(&db);

        let conversation_id = conversations
            .create("chat", "active")
            .expect("create conversation");
        let id = messages
            .create(
                conversation_id,
                "user",
                "The spend guard blocks oversized requests",
                None,
                None,
            )
            .expect("create message");

        // Insert: the messages_fts_insert trigger indexes the content.
        let hits = search
            .search_messages("spend")
            .expect("search after insert");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert_eq!(
            hits[0].conversation_id, conversation_id,
            "each hit carries the conversation so the caller can open it"
        );
        assert_eq!(hits[0].role, "user");
        assert_eq!(hits[0].content, "The spend guard blocks oversized requests");

        // Delete: the messages_fts_delete trigger de-indexes the row.
        messages.delete(id).expect("delete message");
        assert!(
            search
                .search_messages("spend")
                .expect("search after delete")
                .is_empty(),
            "a deleted message must not be searchable"
        );
    }

    #[test]
    fn prompt_index_covers_title_and_content_and_follows_updates() {
        let db = in_memory_database();
        let search = repo(&db);
        let prompts = PromptRepository::new(&db);

        let id = prompts
            .create(
                "Refactor helper",
                "extract pure functions from the executor",
            )
            .expect("create prompt");

        // Both indexed columns match through the one prompts_fts index.
        let by_title = search.search_prompts("refactor").expect("title search");
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].id, id);
        assert_eq!(by_title[0].title, "Refactor helper");
        let by_content = search.search_prompts("extract").expect("content search");
        assert_eq!(by_content.len(), 1);
        assert_eq!(by_content[0].id, id);

        // Update: the prompts_fts_update trigger re-indexes title and content.
        prompts
            .update(id, "Refactor helper", "split the god module now")
            .expect("update prompt");
        assert!(
            search
                .search_prompts("extract")
                .expect("stale content search")
                .is_empty(),
            "the pre-update content must no longer match"
        );
        assert_eq!(
            search
                .search_prompts("split")
                .expect("fresh content search")
                .len(),
            1
        );

        // Delete: the prompts_fts_delete trigger de-indexes the row.
        prompts.delete(id).expect("delete prompt");
        assert!(
            search
                .search_prompts("refactor")
                .expect("search after delete")
                .is_empty(),
            "a deleted prompt must not be searchable"
        );
    }

    #[test]
    fn results_are_ordered_by_rank_with_id_as_tiebreaker() {
        let db = in_memory_database();
        let search = repo(&db);
        let conversations = ConversationRepository::new(&db);
        let messages = MessageRepository::new(&db);

        // Identical titles produce identical FTS ranks, so the documented
        // deterministic tiebreaker (id ascending) decides the order.
        let first = conversations
            .create("duplicate needle", "active")
            .expect("create first");
        let second = conversations
            .create("duplicate needle", "active")
            .expect("create second");
        let hits = search.search_conversations("needle").expect("tie search");
        assert_eq!(hits.len(), 2);
        let ids: Vec<i64> = hits.iter().map(|conversation| conversation.id).collect();
        assert_eq!(
            ids,
            vec![first, second],
            "equal ranks fall back to id order"
        );

        // Equal term frequency (one hit each) with different document lengths:
        // the shorter document scores better under bm25 and must come first.
        let short_id = messages
            .create(first, "user", "needle", None, None)
            .expect("short message");
        let long_id = messages
            .create(
                first,
                "assistant",
                "a longer message that mentions needle once among many other words",
                None,
                None,
            )
            .expect("long message");
        let hits = search.search_messages("needle").expect("rank search");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].id, short_id,
            "the shorter document with the same term frequency ranks first"
        );
        assert_eq!(hits[1].id, long_id);
    }

    #[test]
    fn unmatched_queries_return_empty_and_malformed_queries_are_errors() {
        let db = in_memory_database();
        let search = repo(&db);
        let conversations = ConversationRepository::new(&db);
        conversations
            .create("Rust async patterns", "active")
            .expect("create conversation");

        // A query that matches nothing yields an empty result, not an error.
        assert!(search
            .search_conversations("zenithterm")
            .expect("no-match search")
            .is_empty());
        assert!(search
            .search_messages("zenithterm")
            .expect("no-match search")
            .is_empty());
        assert!(search
            .search_prompts("zenithterm")
            .expect("no-match search")
            .is_empty());

        // `query` is passed to MATCH as supplied: a malformed FTS expression
        // surfaces as a DatabaseError (documented error contract).
        assert!(
            search.search_conversations("\"unterminated").is_err(),
            "an unterminated FTS phrase must be an error, not a silent miss"
        );
    }
}
