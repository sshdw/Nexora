//! Local Search service: application-layer orchestration for offline search of
//! locally stored conversations and prompts (FR-009; ROADMAP.md Phase 7 —
//! Local Search; ARCHITECTURE.md §5).
//!
//! This service composes the existing [`SearchRepository`] and reuses the
//! existing persistence row types ([`Conversation`], [`Message`], [`Prompt`])
//! that back the searchable entities. It adds no schema, no SQL, and no
//! database access of its own: all queries are delegated to the repository's
//! FTS5-backed reads over the `conversations_fts`, `messages_fts`, and
//! `prompts_fts` indexes (DATABASE.md §10–§11).
//!
//! # Scope and result shape (FR-009)
//!
//! One [`LocalSearchService::search`] call covers the Phase 7 scope —
//! **conversation search** and **prompt search** — and returns grouped
//! [`SearchResults`] so the caller can navigate them:
//!
//! - [`SearchResults::conversations`]: conversations whose *title* matched;
//!   the associated item opens by [`Conversation::id`].
//! - [`SearchResults::message_matches`]: messages whose *content* matched
//!   (conversation content search, DATABASE.md §10); each row carries the
//!   [`Message::conversation_id`] the result opens.
//! - [`SearchResults::prompts`]: prompts whose *title* or *content* matched;
//!   the associated item opens by [`Prompt::id`].
//!
//! Search runs entirely against the local `SQLite` FTS indexes and never
//! touches the network, satisfying FR-009's "Search operates without internet
//! access" (FR-015). No credentials, secrets, or tokens are involved here.
//!
//! A blank query (empty or whitespace) yields empty [`SearchResults`] without
//! touching the database: there is nothing to match, and an FTS `MATCH` on an
//! empty expression would be meaningless. Any other query is passed through
//! unchanged to the FTS index; a malformed FTS expression surfaces as a
//! classified [`SearchError::Database`], never a panic.

use serde::Serialize;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::Conversation;
use crate::infrastructure::repository::messages::Message;
use crate::infrastructure::repository::prompts::Prompt;
use crate::infrastructure::repository::search::SearchRepository;

/// Application-layer result shared by local-search operations, unifying
/// persistence and query failures.
pub(crate) type Result<T> = std::result::Result<T, SearchError>;

/// Grouped results of one [`LocalSearchService::search`] call (FR-009).
///
/// Empty groups are empty vectors; a search that matches nothing returns a
/// value with every group empty, not an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SearchResults {
    /// Conversations whose title matched, ordered by relevance.
    pub conversations: Vec<Conversation>,
    /// Messages whose content matched, ordered by relevance. Each row's
    /// `conversation_id` identifies the conversation the result opens.
    pub message_matches: Vec<Message>,
    /// Prompts whose title or content matched, ordered by relevance.
    pub prompts: Vec<Prompt>,
}

/// Application-layer service orchestrating offline local search (FR-009).
///
/// Wraps [`SearchRepository`] and exposes one grouped, application-facing
/// search operation. It is deliberately focused on orchestration: blank-query
/// handling and result grouping only, with all persistence behavior left in
/// the repository and the database.
pub(crate) struct LocalSearchService<'a> {
    search: SearchRepository<'a>,
}

impl<'a> LocalSearchService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            search: SearchRepository::new(db),
        }
    }

    /// Search locally stored conversations and prompts (FR-009).
    ///
    /// A blank `query` (empty or whitespace only) returns empty
    /// [`SearchResults`] without querying the database. Otherwise the trimmed
    /// query is matched against the conversation-title, message-content, and
    /// prompt-title/content FTS indexes (DATABASE.md §10), and the matches are
    /// returned grouped in [`SearchResults`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] if any index query fails, for example
    /// because the query is not a valid FTS expression or because the
    /// `conversations_fts` / `messages_fts` / `prompts_fts` indexes cannot be
    /// queried.
    pub(crate) fn search(&self, query: &str) -> Result<SearchResults> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResults::default());
        }
        Ok(SearchResults {
            conversations: self.search.search_conversations(query)?,
            message_matches: self.search.search_messages(query)?,
            prompts: self.search.search_prompts(query)?,
        })
    }
}

/// Errors raised by the local search service.
///
/// Unifies the persistence and FTS query failures surfaced by
/// [`SearchRepository`]. The variant carries no user content, so formatting a
/// [`SearchError`] never leaks stored data into the logs (ARCHITECTURE.md §9,
/// §11).
#[derive(Debug)]
pub(crate) enum SearchError {
    /// A persistence or FTS query failure from the search repository.
    Database(DatabaseError),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for SearchError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::repository::conversations::ConversationRepository;
    use crate::infrastructure::repository::messages::MessageRepository;
    use crate::infrastructure::repository::prompts::PromptRepository;
    use rusqlite::Connection;

    /// Build a service over an in-memory database whose schema mirrors the
    /// documented `providers` / `conversations` / `messages` / `prompts`
    /// tables plus the three FTS5 virtual indexes and their synchronization
    /// triggers (DATABASE.md §7, §10, §11). The application's migration set is
    /// intentionally empty (Phase 1 migrations are a separate task), so the
    /// test schema is created here to exercise the search flow end to end.
    fn test_db() -> Database {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE providers (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL UNIQUE
                     CHECK(length(name) > 0 AND length(name) <= 100),
                 display_name TEXT NOT NULL CHECK(length(display_name) > 0)
             );
             CREATE TABLE conversations (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT 'Untitled Conversation'
                     CHECK(length(title) > 0 AND length(title) <= 500),
                 status TEXT NOT NULL DEFAULT 'active'
                     CHECK(status IN ('active', 'archived')),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0),
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at)
             );
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY,
                 conversation_id INTEGER NOT NULL CHECK(conversation_id > 0)
                     REFERENCES conversations(id) ON DELETE CASCADE,
                 role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                 content TEXT NOT NULL CHECK(length(content) > 0),
                 provider_id INTEGER
                     CHECK(provider_id IS NULL OR provider_id > 0)
                     REFERENCES providers(id) ON DELETE SET NULL,
                 model_name TEXT CHECK(model_name IS NULL OR length(model_name) <= 200),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0)
             );
             CREATE INDEX messages_conversation_order
                 ON messages (conversation_id, created_at);
             CREATE TABLE prompts (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL CHECK(length(title) > 0 AND length(title) <= 200),
                 content TEXT NOT NULL
                     CHECK(length(content) > 0 AND length(content) <= 10000),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0),
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at)
             );
             CREATE VIRTUAL TABLE conversations_fts USING fts5(title);
             CREATE VIRTUAL TABLE messages_fts USING fts5(content);
             CREATE VIRTUAL TABLE prompts_fts USING fts5(title, content);
             CREATE TRIGGER conversations_after_insert AFTER INSERT ON conversations BEGIN
                 INSERT INTO conversations_fts(rowid, title) VALUES (new.id, new.title);
             END;
             CREATE TRIGGER conversations_after_update AFTER UPDATE OF title ON conversations BEGIN
                 DELETE FROM conversations_fts WHERE rowid = old.id;
                 INSERT INTO conversations_fts(rowid, title) VALUES (new.id, new.title);
             END;
             CREATE TRIGGER conversations_after_delete AFTER DELETE ON conversations BEGIN
                 DELETE FROM conversations_fts WHERE rowid = old.id;
             END;
             CREATE TRIGGER messages_after_insert AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
             END;
             CREATE TRIGGER messages_after_delete AFTER DELETE ON messages BEGIN
                 DELETE FROM messages_fts WHERE rowid = old.id;
             END;
             CREATE TRIGGER prompts_after_insert AFTER INSERT ON prompts BEGIN
                 INSERT INTO prompts_fts(rowid, title, content)
                     VALUES (new.id, new.title, new.content);
             END;
             CREATE TRIGGER prompts_after_update AFTER UPDATE OF title, content ON prompts BEGIN
                 DELETE FROM prompts_fts WHERE rowid = old.id;
                 INSERT INTO prompts_fts(rowid, title, content)
                     VALUES (new.id, new.title, new.content);
             END;
             CREATE TRIGGER prompts_after_delete AFTER DELETE ON prompts BEGIN
                 DELETE FROM prompts_fts WHERE rowid = old.id;
             END;",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    fn create_conversation(db: &Database, title: &str) -> i64 {
        ConversationRepository::new(db)
            .create(title, "active")
            .expect("conversation created")
    }

    fn insert_message(db: &Database, conversation_id: i64, role: &str, content: &str) -> i64 {
        MessageRepository::new(db)
            .create(conversation_id, role, content, None, None)
            .expect("message created")
    }

    fn create_prompt(db: &Database, title: &str, content: &str) -> i64 {
        PromptRepository::new(db)
            .create(title, content)
            .expect("prompt created")
    }

    #[test]
    fn search_finds_conversation_by_title() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let roadmap = create_conversation(&db, "Q3 roadmap planning");
        create_conversation(&db, "Trivial chat");

        let results = service.search("roadmap").expect("search succeeds");

        assert_eq!(results.conversations.len(), 1);
        assert_eq!(results.conversations[0].id, roadmap);
        assert_eq!(results.conversations[0].title, "Q3 roadmap planning");
        assert!(results.message_matches.is_empty());
        assert!(results.prompts.is_empty());
    }

    #[test]
    fn search_is_case_and_punctuation_insensitive() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let plan = create_conversation(&db, "1. Roadmap for release");

        let results = service.search("ROADMAP").expect("search succeeds");

        assert_eq!(
            results.conversations,
            vec![Conversation {
                id: plan,
                title: "1. Roadmap for release".to_string(),
                status: "active".to_string(),
                created_at: 1,
                updated_at: 1,
            }]
        );
    }

    #[test]
    fn search_finds_message_content_within_conversation() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let chat = create_conversation(&db, "General");
        insert_message(&db, chat, "user", "the launch strategy needs review");

        let results = service.search("strategy").expect("search succeeds");

        assert_eq!(results.message_matches.len(), 1);
        assert_eq!(results.message_matches[0].conversation_id, chat);
        assert_eq!(results.message_matches[0].role, "user");
        assert_eq!(
            results.message_matches[0].content,
            "the launch strategy needs review"
        );
        assert!(results.conversations.is_empty());
        assert!(results.prompts.is_empty());
    }

    #[test]
    fn message_match_result_names_the_conversation_to_open() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let first = create_conversation(&db, "First");
        let second = create_conversation(&db, "Second");
        insert_message(&db, first, "user", "review the parking proposal");
        let msg_in_second = insert_message(&db, second, "assistant", "parking is approved");

        let results = service.search("parking").expect("search succeeds");

        assert_eq!(results.message_matches.len(), 2);
        let hit = results
            .message_matches
            .iter()
            .find(|message| message.id == msg_in_second)
            .expect("second message matched");
        // The content hit carries the conversation_id used to open it (FR-009).
        assert_eq!(hit.conversation_id, second);
    }

    #[test]
    fn search_finds_prompt_by_title_and_by_content() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let by_title = create_prompt(&db, "Code review checklist", "Go through the diff.");
        let by_content = create_prompt(&db, "Debugging", "Isolate the failing query plan.");

        let results = service.search("checklist").expect("search succeeds");
        assert_eq!(results.prompts.len(), 1);
        assert_eq!(results.prompts[0].id, by_title);

        let results = service.search("query plan").expect("search succeeds");
        assert_eq!(results.prompts.len(), 1);
        assert_eq!(results.prompts[0].id, by_content);
        assert!(results.conversations.is_empty());
        assert!(results.message_matches.is_empty());
    }

    #[test]
    fn search_returns_empty_results_when_nothing_matches() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        create_conversation(&db, "Roads");
        create_prompt(&db, "Rails", "Track notes");

        let results = service.search("unrelatedterm").expect("search succeeds");

        assert!(results.conversations.is_empty());
        assert!(results.message_matches.is_empty());
        assert!(results.prompts.is_empty());
    }

    #[test]
    fn search_with_blank_query_returns_empty_results_without_error() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        create_conversation(&db, "Roadmap");

        for query in ["", "   ", "\t\n"] {
            let results = service.search(query).expect("blank query is not an error");
            assert!(results.conversations.is_empty());
            assert!(results.message_matches.is_empty());
            assert!(results.prompts.is_empty());
        }
    }

    #[test]
    fn archived_conversations_are_searchable() {
        // FR-009 places no status filter on search, and DATABASE.md §7.1/§8
        // restrict the status-based index to the active list, so archived
        // conversations remain findable through the FTS title index.
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let archived_id = ConversationRepository::new(&db)
            .create("Old planning notes", "archived")
            .expect("archived conversation created");

        let results = service.search("planning").expect("search succeeds");

        assert_eq!(results.conversations.len(), 1);
        assert_eq!(results.conversations[0].id, archived_id);
        assert_eq!(results.conversations[0].status, "archived");
    }

    #[test]
    fn renamed_conversation_is_reindexed() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let id = create_conversation(&db, "old title");
        ConversationRepository::new(&db)
            .update(id, "new title", "active")
            .expect("conversation renamed");

        let results = service.search("new").expect("search succeeds");
        assert_eq!(results.conversations.len(), 1);
        assert_eq!(results.conversations[0].id, id);

        let results = service.search("old").expect("search succeeds");
        assert!(results.conversations.is_empty());
    }

    #[test]
    fn deleted_conversation_is_removed_from_the_index() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        insert_message(
            &db,
            create_conversation(&db, "Planning"),
            "user",
            "launch strategy",
        );
        let deleted = create_conversation(&db, "Roadmap");
        insert_message(&db, deleted, "user", "parking proposal");
        ConversationRepository::new(&db)
            .delete(deleted)
            .expect("conversation deleted");

        let results = service.search("parking").expect("search succeeds");
        assert!(results.message_matches.is_empty());
        let results = service.search("roadmap").expect("search succeeds");
        assert!(results.conversations.is_empty());
    }

    #[test]
    fn deleted_and_updated_prompts_are_reindexed() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        let updated = create_prompt(&db, "Prompt A", "old content");
        let deleted = create_prompt(&db, "Prompt B", "about logistics");
        PromptRepository::new(&db)
            .update(updated, "Prompt A", "fresh content")
            .expect("prompt updated");
        PromptRepository::new(&db)
            .delete(deleted)
            .expect("prompt deleted");

        let results = service.search("fresh").expect("search succeeds");
        assert_eq!(results.prompts.len(), 1);
        assert_eq!(results.prompts[0].id, updated);

        for query in ["old", "logistics"] {
            let results = service.search(query).expect("search succeeds");
            assert!(results.prompts.is_empty());
        }
    }

    #[test]
    fn malformed_fts_query_is_a_classified_error_not_a_panic() {
        let db = test_db();
        let service = LocalSearchService::new(&db);
        create_conversation(&db, "Roadmap");

        let err = service
            .search("\"unterminated")
            .expect_err("malformed query");

        assert!(matches!(err, SearchError::Database(_)));
    }

    #[test]
    fn missing_fts_index_surfaces_as_a_database_error() {
        // A database whose schema has the base tables but lacks the FTS
        // indexes (the documented indexes belong to the schema's migration,
        // not to the search code) surfaces as a classified database error
        // rather than a panic.
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE conversations (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT 'Untitled Conversation'
                     CHECK(length(title) > 0 AND length(title) <= 500),
                 status TEXT NOT NULL DEFAULT 'active'
                     CHECK(status IN ('active', 'archived')),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0),
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at)
             );",
        )
        .expect("create conversations-only schema");
        let db = Database::new(conn);
        create_conversation(&db, "Roadmap");
        let service = LocalSearchService::new(&db);

        let err = service.search("roadmap").expect_err("missing FTS index");

        assert!(matches!(err, SearchError::Database(_)));
    }
}
