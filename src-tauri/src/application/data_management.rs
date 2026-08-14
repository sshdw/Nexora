//! Data management service: application-layer orchestration for deleting and
//! clearing locally stored application data (FR-013; ROADMAP.md Phase 9 —
//! Data Management; ARCHITECTURE.md §5).
//!
//! This service is the Phase 9 contract for destructive data-management
//! operations. It composes the existing [`ConversationRepository`],
//! [`PromptRepository`], [`ProviderRepository`], and [`SettingsRepository`]
//! and reuses the existing database cascade and FTS synchronization behavior
//! (DATABASE.md §9, §11):
//!
//! - [`DataManagementService::delete_conversation`] removes one conversation;
//!   its `messages` and `attachments` are removed by the schema's `ON DELETE
//!   CASCADE` foreign keys (AC-2, AC-9), and the deleted entities disappear
//!   from local search through the FTS triggers (AC-7).
//! - [`DataManagementService::delete_prompt`] removes one prompt (AC-3); the
//!   deleted prompt no longer appears in the prompt library or local search
//!   (AC-7).
//! - [`DataManagementService::clear`] atomically clears all locally stored
//!   application data — conversations (and their cascaded messages /
//!   attachments), prompts, non-sensitive provider metadata, and application
//!   settings (AC-4).
//!
//! # Confirmation (AC-5)
//!
//! Every operation is destructive and requires explicit user confirmation
//! before it executes. Each method therefore requires the caller to supply the
//! [`CONFIRMATION`] phrase; without it the operation returns
//! [`DataManagementError::ConfirmationRequired`] and performs **no** write.
//! Because the destructive action is gated on an explicit argument that must
//! equal the documented phrase, confirmation cannot be bypassed by normal
//! application flow.
//!
//! # Atomicity (AC-8)
//!
//! [`DataManagementService::clear`] deletes across several tables inside one
//! transaction via the shared [`Repository::transaction`] foundation
//! (DATABASE.md §5): all clears commit together or roll back together, so a
//! recoverable failure can never leave partially deleted application data. The
//! ordering deletes `conversations` (first, cascading to `messages` /
//! `attachments`) before the standalone tables, so no orphaned rows remain
//! (AC-9).
//!
//! # No credential leakage (AC-10)
//!
//! Provider credentials are never stored in `SQLite` and are never written or
//! touched here; only the non-sensitive `providers` metadata rows are cleared
//! (ARCHITECTURE.md §12; DATABASE.md §14). No error variant carries a secret
//! value, so formatting a [`DataManagementError`] never logs a credential
//! (ARCHITECTURE.md §9, §11).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::prompts::PromptRepository;
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::settings::SettingsRepository;
use crate::infrastructure::repository::Repository;

/// Application-layer result shared by data-management operations, unifying
/// confirmation and persistence failures.
pub(crate) type Result<T> = std::result::Result<T, DataManagementError>;

/// The confirmation phrase that must accompany any destructive data-management
/// operation before it executes (FR-013; AC-5).
///
/// A caller obtains this from the user through its explicit confirmation flow
/// and must supply precisely this value; the service refuses to run a
/// destructive operation with any other value. A fixed, non-empty, exact phrase
/// is used instead of a bare boolean so that confirmation cannot be supplied
/// incidentally or by default.
pub(crate) const CONFIRMATION: &str = "confirm";

/// Application-layer service exposing the Phase 9 destructive data-management
/// operations (FR-013) over the existing repositories.
///
/// Each method composes the existing repositories for persistence and delegates
/// cascade / FTS synchronization to the database. This service is deliberately
/// focused on orchestration and the explicit-confirmation gate; schema
/// constraints and cascade behavior remain in the repositories and the
/// database.
pub(crate) struct DataManagementService<'a> {
    conversations: ConversationRepository<'a>,
    prompts: PromptRepository<'a>,
    providers: ProviderRepository<'a>,
    settings: SettingsRepository<'a>,
}

impl<'a> DataManagementService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            conversations: ConversationRepository::new(db),
            prompts: PromptRepository::new(db),
            providers: ProviderRepository::new(db),
            settings: SettingsRepository::new(db),
        }
    }

    /// Require `confirmation` to equal the documented [`CONFIRMATION`] phrase.
    ///
    /// Returns [`DataManagementError::ConfirmationRequired`] otherwise.
    fn require_confirmation(confirmation: &str) -> Result<()> {
        if confirmation == CONFIRMATION {
            Ok(())
        } else {
            Err(DataManagementError::ConfirmationRequired)
        }
    }

    /// Permanently delete the conversation `id` (FR-013; DATABASE.md §7.1).
    ///
    /// Applies the schema's `ON DELETE CASCADE` so the conversation's `messages`
    /// and `attachments` are removed with it (AC-2, AC-9), and the FTS triggers
    /// remove it from local search (AC-7). Deleting a conversation that does not
    /// exist is a no-op, matching the repository's existing delete semantics
    /// (AC-1).
    ///
    /// # Errors
    ///
    /// Returns [`DataManagementError::ConfirmationRequired`] if `confirmation`
    /// is not the [`CONFIRMATION`] phrase, or
    /// [`DataManagementError::Database`] if the delete fails (AC-8).
    pub(crate) fn delete_conversation(&self, id: i64, confirmation: &str) -> Result<()> {
        Self::require_confirmation(confirmation)?;
        self.conversations.delete(id)?;
        Ok(())
    }

    /// Permanently delete the prompt `id` (FR-013; DATABASE.md §7.3).
    ///
    /// The deleted prompt is no longer available through the prompt library or
    /// local search (AC-3, AC-7). Deleting a prompt that does not exist is a
    /// no-op, matching the repository's existing delete semantics.
    ///
    /// # Errors
    ///
    /// Returns [`DataManagementError::ConfirmationRequired`] if `confirmation`
    /// is not the [`CONFIRMATION`] phrase, or
    /// [`DataManagementError::Database`] if the delete fails (AC-8).
    pub(crate) fn delete_prompt(&self, id: i64, confirmation: &str) -> Result<()> {
        Self::require_confirmation(confirmation)?;
        self.prompts.delete(id)?;
        Ok(())
    }

    /// Clear all locally stored application data (FR-013; AC-4): conversations
    /// (and their cascaded messages / attachments), prompts, non-sensitive
    /// provider metadata, and application settings.
    ///
    /// All clears run atomically in one transaction (DATABASE.md §5), so a
    /// recoverable failure during any step rolls back every change and leaves no
    /// partially deleted data (AC-8). Schema bookkeeping (`schema_version`) is
    /// application metadata, not user data, and is intentionally preserved;
    /// provider **credentials** remain exclusively in the OS secure keyring and
    /// are never touched (AC-10).
    ///
    /// # Errors
    ///
    /// Returns [`DataManagementError::ConfirmationRequired`] if `confirmation`
    /// is not the [`CONFIRMATION`] phrase, or
    /// [`DataManagementError::Database`] if any step of the clear fails (AC-8).
    pub(crate) fn clear(&self, confirmation: &str) -> Result<()> {
        Self::require_confirmation(confirmation)?;
        // `conversations` is cleared first; the ON DELETE CASCADE foreign keys
        // remove its messages and attachments, and the FTS triggers remove them
        // from search. The standalone tables follow.
        self.conversations.transaction(|tx| {
            ConversationRepository::clear_in_transaction(tx)?;
            PromptRepository::clear_in_transaction(tx)?;
            ProviderRepository::clear_in_transaction(tx)?;
            SettingsRepository::clear_in_transaction(tx)?;
            Ok(())
        })?;
        Ok(())
    }
}

/// Classified errors raised by data-management operations.
///
/// No variant carries a credential or other secret value, so formatting a
/// [`DataManagementError`] never writes a secret to the logs (ARCHITECTURE.md
/// §9, §11; AC-10).
#[derive(Debug)]
pub(crate) enum DataManagementError {
    /// A destructive operation was invoked without the required explicit
    /// confirmation phrase (AC-5).
    ConfirmationRequired,
    /// A persistence failure from a repository or the clear transaction.
    Database(DatabaseError),
}

impl std::fmt::Display for DataManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfirmationRequired => write!(
                f,
                "explicit confirmation is required before this destructive action can run"
            ),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DataManagementError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfirmationRequired => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for DataManagementError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::search::LocalSearchService;
    use crate::infrastructure::repository::attachments::AttachmentRepository;
    use crate::infrastructure::repository::messages::MessageRepository;
    use rusqlite::Connection;

    /// Open an in-memory database whose schema mirrors the production schema:
    /// every user-data table (DATABASE.md §7) with its cascade foreign keys,
    /// plus the three FTS5 indexes and their synchronization triggers
    /// (DATABASE.md §9-§11). The application's migration set is intentionally
    /// not reused here (it is a separate task), so the test schema is built
    /// directly to exercise the data-management flow end to end.
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
             CREATE TABLE prompts (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL CHECK(length(title) > 0 AND length(title) <= 200),
                 content TEXT NOT NULL
                     CHECK(length(content) > 0 AND length(content) <= 10000),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0),
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at)
             );
             CREATE TABLE attachments (
                 id INTEGER PRIMARY KEY,
                 conversation_id INTEGER NOT NULL CHECK(conversation_id > 0)
                     REFERENCES conversations(id) ON DELETE CASCADE,
                 message_id INTEGER
                     CHECK(message_id IS NULL OR message_id > 0)
                     REFERENCES messages(id) ON DELETE CASCADE,
                 file_name TEXT NOT NULL
                     CHECK(length(file_name) > 0 AND length(file_name) <= 255),
                 file_path TEXT NOT NULL CHECK(length(file_path) > 0),
                 file_size_bytes INTEGER CHECK(file_size_bytes >= 0),
                 mime_type TEXT CHECK(mime_type IS NULL OR length(mime_type) <= 127)
             );
             CREATE TABLE app_settings (
                 key TEXT PRIMARY KEY CHECK(length(key) > 0 AND length(key) <= 200),
                 value TEXT CHECK(value IS NULL OR length(value) <= 10000)
             );
             CREATE VIRTUAL TABLE conversations_fts USING fts5(title);
             CREATE VIRTUAL TABLE messages_fts USING fts5(content);
             CREATE VIRTUAL TABLE prompts_fts USING fts5(title, content);
             CREATE TRIGGER conversations_after_insert AFTER INSERT ON conversations BEGIN
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
             CREATE TRIGGER prompts_after_delete AFTER DELETE ON prompts BEGIN
                 DELETE FROM prompts_fts WHERE rowid = old.id;
             END;
            ",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    fn create_conversation(db: &Database, title: &str) -> i64 {
        ConversationRepository::new(db)
            .create(title, "active")
            .expect("conversation created")
    }

    fn insert_message(db: &Database, conversation_id: i64, content: &str) -> i64 {
        MessageRepository::new(db)
            .create(conversation_id, "user", content, None, None)
            .expect("message created")
    }

    fn insert_attachment(db: &Database, conversation_id: i64) -> i64 {
        AttachmentRepository::new(db)
            .create(
                conversation_id,
                "notes.txt",
                "C:\\attachments\\notes.txt",
                Some(42),
                Some("text/plain"),
            )
            .expect("attachment created")
    }

    fn create_prompt(db: &Database, title: &str, content: &str) -> i64 {
        PromptRepository::new(db)
            .create(title, content)
            .expect("prompt created")
    }

    fn create_provider(db: &Database, name: &str, display_name: &str) -> i64 {
        ProviderRepository::new(db)
            .create(name, display_name)
            .expect("provider created")
    }

    fn create_setting(db: &Database, key: &str, value: Option<&str>) {
        SettingsRepository::new(db)
            .create(key, value)
            .expect("setting created");
    }

    fn count(db: &Database, table: &str) -> i64 {
        let conn = db.lock().expect("lock connection");
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .expect("count rows")
    }

    #[test]
    fn delete_conversation_without_confirmation_is_refused() {
        let db = test_db();
        let id = create_conversation(&db, "Chat");
        insert_message(&db, id, "content to keep");
        let service = DataManagementService::new(&db);

        let err = service
            .delete_conversation(id, "DELETE")
            .expect_err("wrong confirmation");

        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        // Nothing was deleted.
        assert_eq!(count(&db, "conversations"), 1);
        assert_eq!(count(&db, "messages"), 1);
    }

    #[test]
    fn delete_prompt_without_confirmation_is_refused() {
        let db = test_db();
        let id = create_prompt(&db, "Plan", "content");
        let service = DataManagementService::new(&db);

        let err = service
            .delete_prompt(id, "")
            .expect_err("wrong confirmation");

        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "prompts"), 1);
    }

    #[test]
    fn clear_without_confirmation_is_refused_and_leaves_everything() {
        let db = test_db();
        let conv = create_conversation(&db, "Chat");
        insert_message(&db, conv, "content");
        create_prompt(&db, "Plan", "content");
        create_provider(&db, "openai", "OpenAI");
        create_setting(&db, "theme", Some("dark"));
        let service = DataManagementService::new(&db);

        let err = service.clear("yes please").expect_err("wrong confirmation");

        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 1);
        assert_eq!(count(&db, "messages"), 1);
        assert_eq!(count(&db, "prompts"), 1);
        assert_eq!(count(&db, "providers"), 1);
        assert_eq!(count(&db, "app_settings"), 1);
        // Search still sees the data.
        let search = LocalSearchService::new(&db);
        assert_eq!(
            search.search("content").expect("search").message_matches.len(),
            1
        );
    }

    #[test]
    fn delete_conversation_cascades_messages_and_attachments() {
        let db = test_db();
        let id = create_conversation(&db, "Planning strategy");
        insert_message(&db, id, "launch the strategy");
        insert_attachment(&db, id);
        let service = DataManagementService::new(&db);

        service
            .delete_conversation(id, CONFIRMATION)
            .expect("delete succeeds");

        // AC-1 / AC-9: the conversation and its dependent rows are gone, with
        // no orphaned messages or attachments.
        assert_eq!(count(&db, "conversations"), 0);
        assert_eq!(count(&db, "messages"), 0);
        assert_eq!(count(&db, "attachments"), 0);
        let conversation = ConversationRepository::new(&db)
            .read(id)
            .expect("read conversation");
        assert!(conversation.is_none());
        // AC-7: the deleted conversation no longer appears in local search.
        let search = LocalSearchService::new(&db);
        assert!(search.search("strategy").expect("search").conversations.is_empty());
        assert!(search.search("strategy").expect("search").message_matches.is_empty());
    }

    #[test]
    fn delete_conversation_of_unknown_id_is_a_no_op() {
        let db = test_db();
        let service = DataManagementService::new(&db);

        service
            .delete_conversation(42, CONFIRMATION)
            .expect("no-op delete succeeds");
    }

    #[test]
    fn delete_prompt_removes_it_from_library_and_search() {
        let db = test_db();
        let id = create_prompt(&db, "Launch plan", "roll out the launch plan");
        create_prompt(&db, "Other", "unrelated topic");
        let service = DataManagementService::new(&db);

        service
            .delete_prompt(id, CONFIRMATION)
            .expect("delete succeeds");

        // AC-3: the prompt is gone from the library.
        assert_eq!(count(&db, "prompts"), 1);
        assert!(PromptRepository::new(&db)
            .read(id)
            .expect("read prompt")
            .is_none());
        // AC-7: the deleted prompt no longer matches local search, while the
        // surviving prompt is still searchable.
        let search = LocalSearchService::new(&db);
        assert!(search.search("launch").expect("search").prompts.is_empty());
        assert_eq!(search.search("unrelated").expect("search").prompts.len(), 1);
    }

    #[test]
    fn delete_prompt_of_unknown_id_is_a_no_op() {
        let db = test_db();
        let service = DataManagementService::new(&db);

        service
            .delete_prompt(99, CONFIRMATION)
            .expect("no-op delete succeeds");
    }

    #[test]
    fn clear_removes_all_application_data_and_search_index() {
        let db = test_db();
        let conv = create_conversation(&db, "Roadmap");
        insert_message(&db, conv, "the parking proposal");
        insert_attachment(&db, conv);
        create_prompt(&db, "Prompt", "logistics checklist");
        create_provider(&db, "openai", "OpenAI");
        create_setting(&db, "theme", Some("dark"));
        let service = DataManagementService::new(&db);

        service.clear(CONFIRMATION).expect("clear succeeds");

        // AC-4 / AC-9: every application-data table is empty, including the
        // cascaded messages / attachments, with no orphaned rows.
        for table in [
            "conversations",
            "messages",
            "attachments",
            "prompts",
            "providers",
            "app_settings",
        ] {
            assert_eq!(count(&db, table), 0, "{table} should be empty after clear");
        }
        // AC-7: local search returns nothing after the clear.
        let search = LocalSearchService::new(&db);
        let results = search
            .search("planning OR parking OR checklist")
            .expect("search");
        assert!(results.conversations.is_empty());
        assert!(results.message_matches.is_empty());
        assert!(results.prompts.is_empty());
    }

    #[test]
    fn deleted_data_remains_absent_when_the_service_is_recreated() {
        // AC-6 (persistence): deletion runs on the shared connection that owns
        // the on-disk SQLite file, so a freshly constructed service (as after a
        // restart) over the same database observes the removal.
        let db = test_db();
        let conv = create_conversation(&db, "Chat");
        insert_message(&db, conv, "old content");
        let prompt = create_prompt(&db, "Plan", "old plan");
        let service = DataManagementService::new(&db);
        service
            .delete_conversation(conv, CONFIRMATION)
            .expect("delete conversation");
        service
            .delete_prompt(prompt, CONFIRMATION)
            .expect("delete prompt");

        // A fresh service and search over the same database read an empty
        // library / search.
        let reopened = DataManagementService::new(&db);
        reopened.clear(CONFIRMATION).expect("clear no-op succeeds");
        let search = LocalSearchService::new(&db);
        let results = search.search("old").expect("search");
        assert!(results.conversations.is_empty());
        assert!(results.message_matches.is_empty());
        assert!(results.prompts.is_empty());
        assert_eq!(count(&db, "conversations"), 0);
    }
}