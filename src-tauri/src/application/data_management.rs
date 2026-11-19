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
//! # Confirmation (AC-5; NEX-SEC-004)
//!
//! Every operation is destructive and requires a live single-use confirmation
//! id minted for the data-management scope by the `request_confirmation`
//! command before it executes. That command shows a **blocking native OS
//! dialog** and mints only after the user accepts, so the id carries a
//! Rust-side user-presence proof rather than a caller-supplied value. Each
//! method additionally requires the id to be *bound to the exact operation* it
//! performs (see [`ConfirmationTarget`]): an id the user approved for one row
//! delete cannot be spent on another row or on clear-all.
//!
//! Without a valid unconsumed unexpired correctly-bound id the operation
//! returns [`DataManagementError::ConfirmationRequired`] and performs **no**
//! write. A caller-controlled constant (the previous `"confirm"` phrase) is
//! not a user-presence proof — any IPC caller could pass it — so the trust
//! anchor lives server-side: ids can only come from an accepted prompt, are
//! bound to this scope and operation, consumed atomically on first use, and
//! short-lived.
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

use crate::application::agent::checkpoints::CheckpointStore;
use crate::application::confirmations::{
    ConfirmationRegistry, ConfirmationTarget, OP_CLEAR_ALL_DATA, OP_DELETE_CONVERSATION,
    OP_DELETE_PROMPT, SCOPE_DATA_MANAGEMENT,
};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::file_checkpoints::FileCheckpointRepository;
use crate::infrastructure::repository::prompts::PromptRepository;
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::settings::SettingsRepository;
use crate::infrastructure::repository::Repository;

/// Application-layer result shared by data-management operations, unifying
/// confirmation and persistence failures.
pub(crate) type Result<T> = std::result::Result<T, DataManagementError>;

/// Application-layer service exposing the Phase 9 destructive data-management
/// operations (FR-013) over the existing repositories.
///
/// Each method composes the existing repositories for persistence and delegates
/// cascade / FTS synchronization to the database. This service is deliberately
/// focused on orchestration and the explicit-confirmation gate; schema
/// constraints and cascade behavior remain in the repositories and the
/// database.
pub(crate) struct DataManagementService<'a> {
    db: &'a Database,
    conversations: ConversationRepository<'a>,
    prompts: PromptRepository<'a>,
    providers: ProviderRepository<'a>,
    settings: SettingsRepository<'a>,
    checkpoints: Option<CheckpointStore>,
    attachments: Option<super::managed_attachments::AttachmentStore>,
    #[cfg(test)]
    after_attachment_delete: Option<&'a (dyn Fn() + Sync)>,
}

impl<'a> DataManagementService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            db,
            conversations: ConversationRepository::new(db),
            prompts: PromptRepository::new(db),
            providers: ProviderRepository::new(db),
            settings: SettingsRepository::new(db),
            checkpoints: None,
            attachments: None,
            #[cfg(test)]
            after_attachment_delete: None,
        }
    }

    /// Create a service bound to a verified disk checkpoint store so
    /// conversation deletion and full clear also garbage-collect unreferenced
    /// backup blobs.
    pub(crate) fn with_checkpoints(db: &'a Database, store: CheckpointStore) -> Self {
        Self {
            checkpoints: Some(store),
            ..Self::new(db)
        }
    }

    pub(crate) fn with_attachments(
        mut self,
        store: super::managed_attachments::AttachmentStore,
    ) -> Self {
        self.attachments = Some(store);
        self
    }

    /// Require `confirmation_id` to be a live single-use id minted for the
    /// data-management scope **and bound to this exact operation**, consuming
    /// it atomically.
    ///
    /// The binding is what stops an id the user approved for a cheap
    /// row-scoped delete from being spent on a different row or on
    /// clear-all. Returns [`DataManagementError::ConfirmationRequired`]
    /// otherwise (and performs no write).
    fn require_confirmation(
        confirmations: &ConfirmationRegistry,
        confirmation_id: &str,
        target: &ConfirmationTarget<'_>,
    ) -> Result<()> {
        if confirmations.consume(SCOPE_DATA_MANAGEMENT, confirmation_id, target) {
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
    /// Returns [`DataManagementError::ConfirmationRequired`] if
    /// `confirmation_id` is not a live id minted for the data-management
    /// scope and bound to this exact `id`, or
    /// [`DataManagementError::Database`] if the delete fails (AC-8).
    pub(crate) fn delete_conversation(
        &self,
        id: i64,
        confirmation_id: &str,
        confirmations: &ConfirmationRegistry,
    ) -> Result<()> {
        Self::require_confirmation(
            confirmations,
            confirmation_id,
            &ConfirmationTarget::for_target(OP_DELETE_CONVERSATION, id),
        )?;
        let _lifecycle = super::managed_attachments::lifecycle(self.db)?;
        let candidate_blobs = if self.checkpoints.is_some() {
            FileCheckpointRepository::new(self.db).collect_pre_blobs_for_conversation(id)?
        } else {
            Vec::new()
        };
        self.conversations.delete(id)?;
        #[cfg(test)]
        if let Some(after_delete) = self.after_attachment_delete {
            after_delete();
        }
        if let Some(store) = &self.checkpoints {
            if let Err(err) = store.gc_unreferenced_blobs(self.db, Some(&candidate_blobs)) {
                log::warn!("checkpoint blob GC failed: {}", err.summary());
            }
        }
        if let Some(store) = &self.attachments {
            store
                .purge(Some(id))
                .map_err(|_| DataManagementError::AttachmentCleanup)?;
        }
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
    /// Returns [`DataManagementError::ConfirmationRequired`] if
    /// `confirmation_id` is not a live id minted for the data-management
    /// scope and bound to this exact `id`, or
    /// [`DataManagementError::Database`] if the delete fails (AC-8).
    pub(crate) fn delete_prompt(
        &self,
        id: i64,
        confirmation_id: &str,
        confirmations: &ConfirmationRegistry,
    ) -> Result<()> {
        Self::require_confirmation(
            confirmations,
            confirmation_id,
            &ConfirmationTarget::for_target(OP_DELETE_PROMPT, id),
        )?;
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
    /// Returns [`DataManagementError::ConfirmationRequired`] if
    /// `confirmation_id` is not a live id minted for the data-management
    /// scope and bound to [`OP_CLEAR_ALL_DATA`] (an id the user approved for a
    /// single-row delete is refused here), or
    /// [`DataManagementError::Database`] if any step of the clear fails (AC-8).
    pub(crate) fn clear(
        &self,
        confirmation_id: &str,
        confirmations: &ConfirmationRegistry,
    ) -> Result<()> {
        Self::require_confirmation(
            confirmations,
            confirmation_id,
            &ConfirmationTarget::new(OP_CLEAR_ALL_DATA),
        )?;
        let _lifecycle = super::managed_attachments::lifecycle(self.db)?;
        let clock = crate::application::agent::access::policy_clock(self.db);
        let mut epoch = clock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *epoch = epoch.saturating_add(1);
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
        #[cfg(test)]
        if let Some(after_delete) = self.after_attachment_delete {
            after_delete();
        }
        if let Some(store) = &self.checkpoints {
            if let Err(err) = store.gc_unreferenced_blobs(self.db, None) {
                log::warn!("checkpoint clear GC failed: {}", err.summary());
            }
        }
        if let Some(store) = &self.attachments {
            store
                .purge(None)
                .map_err(|_| DataManagementError::AttachmentCleanup)?;
        }
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
    /// Rows were removed, but owned file cleanup needs a retry.
    AttachmentCleanup,
    /// A destructive operation was invoked without a live single-use
    /// confirmation id (AC-5; NEX-SEC-004).
    ConfirmationRequired,
    /// A persistence failure from a repository or the clear transaction.
    Database(DatabaseError),
}

impl std::fmt::Display for DataManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AttachmentCleanup => f.write_str("attachment cleanup failed; retry deletion"),
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
            Self::ConfirmationRequired | Self::AttachmentCleanup => None,
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
    use crate::application::confirmations::SCOPE_DATA_MANAGEMENT;
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
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at),
                  workspace_root TEXT CHECK(workspace_root IS NULL OR length(workspace_root) <= 1024)
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
             CREATE TABLE agent_runs (
                 id INTEGER PRIMARY KEY,
                 conversation_id INTEGER REFERENCES conversations(id) ON DELETE CASCADE
             );
             CREATE TABLE durable_run_events (
                 event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 run_id INTEGER REFERENCES agent_runs(id) ON DELETE CASCADE,
                 conversation_id INTEGER REFERENCES conversations(id) ON DELETE CASCADE,
                 kind TEXT NOT NULL
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

    fn count(db: &Database, table: &'static str) -> i64 {
        let conn = db.lock().expect("lock connection");
        // `table` is a `&'static str` (SEC-07, same typing as
        // `application/system.rs::count`): interpolation only ever sees a
        // fixed literal, never caller text.
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count rows")
    }

    /// Mint one live single-use id for the data-management scope, bound to the
    /// exact operation the caller is about to perform.
    fn mint_for(confirmations: &ConfirmationRegistry, target: &ConfirmationTarget<'_>) -> String {
        confirmations
            .request(SCOPE_DATA_MANAGEMENT, target)
            .expect("mint confirmation id")
    }

    fn mint_delete_conversation(confirmations: &ConfirmationRegistry, id: i64) -> String {
        mint_for(
            confirmations,
            &ConfirmationTarget::for_target(OP_DELETE_CONVERSATION, id),
        )
    }

    fn mint_delete_prompt(confirmations: &ConfirmationRegistry, id: i64) -> String {
        mint_for(
            confirmations,
            &ConfirmationTarget::for_target(OP_DELETE_PROMPT, id),
        )
    }

    fn mint_clear(confirmations: &ConfirmationRegistry) -> String {
        mint_for(confirmations, &ConfirmationTarget::new(OP_CLEAR_ALL_DATA))
    }

    #[test]
    fn delete_conversation_without_confirmation_is_refused() {
        let db = test_db();
        let id = create_conversation(&db, "Chat");
        insert_message(&db, id, "content to keep");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        for forged in ["DELETE", "", "confirm", "true"] {
            let err = service
                .delete_conversation(id, forged, &confirmations)
                .expect_err("forged confirmation");
            assert!(matches!(err, DataManagementError::ConfirmationRequired));
        }

        // Nothing was deleted.
        assert_eq!(count(&db, "conversations"), 1);
        assert_eq!(count(&db, "messages"), 1);
    }

    #[test]
    fn consumed_id_cannot_delete_twice() {
        let db = test_db();
        let first = create_conversation(&db, "Chat");
        let second = create_conversation(&db, "Other");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        let id = mint_delete_conversation(&confirmations, first);
        service
            .delete_conversation(first, &id, &confirmations)
            .expect("first use deletes");
        let err = service
            .delete_conversation(second, &id, &confirmations)
            .expect_err("replayed id must be refused");
        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        // The replay deleted nothing.
        assert_eq!(count(&db, "conversations"), 1);
    }

    #[test]
    fn terminal_scope_id_cannot_drive_data_management() {
        use crate::application::confirmations::SCOPE_TERMINAL;
        let db = test_db();
        let id = create_conversation(&db, "Chat");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();
        let foreign = confirmations
            .request(SCOPE_TERMINAL, &ConfirmationTarget::new("echo hi"))
            .expect("mint confirmation id");

        let err = service
            .delete_conversation(id, &foreign, &confirmations)
            .expect_err("cross-scope id must be refused");
        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 1);
    }

    /// NEX-SEC-004: the id is bound to the exact operation. An id the user
    /// approved for one row must not delete a different row, and an id
    /// approved for a row delete must not clear everything.
    #[test]
    fn id_minted_for_another_row_or_operation_is_refused() {
        let db = test_db();
        let first = create_conversation(&db, "First");
        let second = create_conversation(&db, "Second");
        let prompt = create_prompt(&db, "Plan", "content");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        let wrong_row = mint_delete_conversation(&confirmations, first);
        let err = service
            .delete_conversation(second, &wrong_row, &confirmations)
            .expect_err("an id minted for another row must be refused");
        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 2);

        let row_delete = mint_delete_conversation(&confirmations, second);
        let err = service
            .clear(&row_delete, &confirmations)
            .expect_err("a row-delete id must not authorize clear-all");
        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 2);

        let prompt_id = mint_delete_prompt(&confirmations, prompt);
        let err = service
            .delete_conversation(second, &prompt_id, &confirmations)
            .expect_err("a prompt-delete id must not delete a conversation");
        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 2);
        assert_eq!(count(&db, "prompts"), 1);

        // The correctly-bound id still works afterwards: the refusals above
        // consumed only the ids they were presented with.
        service
            .delete_conversation(
                second,
                &mint_delete_conversation(&confirmations, second),
                &confirmations,
            )
            .expect("the matching id still deletes");
        assert_eq!(count(&db, "conversations"), 1);
    }

    #[test]
    fn delete_prompt_without_confirmation_is_refused() {
        let db = test_db();
        let id = create_prompt(&db, "Plan", "content");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        let err = service
            .delete_prompt(id, "", &confirmations)
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
        let confirmations = ConfirmationRegistry::new();

        let err = service
            .clear("yes please", &confirmations)
            .expect_err("wrong confirmation");

        assert!(matches!(err, DataManagementError::ConfirmationRequired));
        assert_eq!(count(&db, "conversations"), 1);
        assert_eq!(count(&db, "messages"), 1);
        assert_eq!(count(&db, "prompts"), 1);
        assert_eq!(count(&db, "providers"), 1);
        assert_eq!(count(&db, "app_settings"), 1);
        // Search still sees the data.
        let search = LocalSearchService::new(&db);
        assert_eq!(
            search
                .search("content")
                .expect("search")
                .message_matches
                .len(),
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
        let confirmations = ConfirmationRegistry::new();

        service
            .delete_conversation(
                id,
                &mint_delete_conversation(&confirmations, id),
                &confirmations,
            )
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
        assert!(search
            .search("strategy")
            .expect("search")
            .conversations
            .is_empty());
        assert!(search
            .search("strategy")
            .expect("search")
            .message_matches
            .is_empty());
    }

    #[test]
    fn delete_conversation_of_unknown_id_is_a_no_op() {
        let db = test_db();
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        service
            .delete_conversation(
                42,
                &mint_delete_conversation(&confirmations, 42),
                &confirmations,
            )
            .expect("no-op delete succeeds");
    }

    #[test]
    fn delete_prompt_removes_it_from_library_and_search() {
        let db = test_db();
        let id = create_prompt(&db, "Launch plan", "roll out the launch plan");
        create_prompt(&db, "Other", "unrelated topic");
        let service = DataManagementService::new(&db);
        let confirmations = ConfirmationRegistry::new();

        service
            .delete_prompt(id, &mint_delete_prompt(&confirmations, id), &confirmations)
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
        let confirmations = ConfirmationRegistry::new();

        service
            .delete_prompt(99, &mint_delete_prompt(&confirmations, 99), &confirmations)
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
        let confirmations = ConfirmationRegistry::new();

        service
            .clear(&mint_clear(&confirmations), &confirmations)
            .expect("clear succeeds");

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
        let confirmations = ConfirmationRegistry::new();
        service
            .delete_conversation(
                conv,
                &mint_delete_conversation(&confirmations, conv),
                &confirmations,
            )
            .expect("delete conversation");
        service
            .delete_prompt(
                prompt,
                &mint_delete_prompt(&confirmations, prompt),
                &confirmations,
            )
            .expect("delete prompt");

        // A fresh service and search over the same database read an empty
        // library / search.
        let reopened = DataManagementService::new(&db);
        reopened
            .clear(&mint_clear(&confirmations), &confirmations)
            .expect("clear no-op succeeds");
        let search = LocalSearchService::new(&db);
        let results = search.search("old").expect("search");
        assert!(results.conversations.is_empty());
        assert!(results.message_matches.is_empty());
        assert!(results.prompts.is_empty());
        assert_eq!(count(&db, "conversations"), 0);
    }

    #[test]
    fn delete_conversation_and_clear_garbage_collect_checkpoint_blobs() {
        use crate::application::agent::checkpoints::{CheckpointRunContext, CheckpointStore};
        use crate::application::conversations::ConversationService;
        use crate::infrastructure::database::open;
        use crate::infrastructure::repository::agent_runs::AgentRunRepository;
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "nexora-dm-ckpt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db_path = dir.join("dm.db");
        let conn = open(&db_path).expect("open migrated db");
        let db = Arc::new(Database::new(conn));

        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).expect("ws dir");
        let ws = std::fs::canonicalize(&ws).expect("canon ws");
        let ws_file = ws.join("keep.txt");
        std::fs::write(&ws_file, b"workspace file stays").expect("seed workspace");

        let store = CheckpointStore::open(dir.join("checkpoints")).expect("open store");
        let conv_svc = ConversationService::new(&db);
        let conv1 = conv_svc.create("Chat 1").expect("conv1");
        let conv2 = conv_svc.create("Chat 2").expect("conv2");
        let run_repo = AgentRunRepository::new(&db);
        let run1 = run_repo
            .create_run(Some(conv1), "m", "full_autonomous")
            .expect("run1");
        let run2 = run_repo
            .create_run(Some(conv2), "m", "full_autonomous")
            .expect("run2");

        let ctx1 = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv1, run1);
        let ctx2 = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv2, run2);

        let shared_bytes = b"shared-blob-bytes";
        let unique1_bytes = b"unique-conv1-bytes";
        let unique2_bytes = b"unique-conv2-bytes";
        let (shared_sha, _) = store.write_blob(shared_bytes).expect("shared");
        let (unique1_sha, _) = store.write_blob(unique1_bytes).expect("unique1");
        let (unique2_sha, _) = store.write_blob(unique2_bytes).expect("unique2");

        ctx1.capture_pre(0, &ws, "keep.txt", "write_file", true, shared_bytes)
            .expect("cp1 shared");
        ctx1.capture_pre(0, &ws, "keep.txt", "write_file", true, unique1_bytes)
            .expect("cp1 unique");
        ctx2.capture_pre(0, &ws, "keep.txt", "write_file", true, shared_bytes)
            .expect("cp2 shared");
        ctx2.capture_pre(0, &ws, "keep.txt", "write_file", true, unique2_bytes)
            .expect("cp2 unique");

        let confirmations = ConfirmationRegistry::new();
        let dm = DataManagementService::with_checkpoints(&db, store.clone());
        dm.delete_conversation(
            conv1,
            &mint_delete_conversation(&confirmations, conv1),
            &confirmations,
        )
        .expect("delete conv1");

        assert!(!store.root().join(&unique1_sha).exists());
        assert!(store.root().join(&shared_sha).exists());
        assert!(store.root().join(&unique2_sha).exists());
        assert_eq!(count(&db, "file_checkpoints"), 2);
        assert_eq!(
            std::fs::read(&ws_file).expect("workspace file"),
            b"workspace file stays"
        );

        dm.clear(&mint_clear(&confirmations), &confirmations)
            .expect("clear all");
        assert_eq!(count(&db, "file_checkpoints"), 0);
        assert!(!store.root().join(&shared_sha).exists());
        assert!(!store.root().join(&unique2_sha).exists());
        assert_eq!(
            std::fs::read(&ws_file).expect("workspace file still untouched"),
            b"workspace file stays"
        );

        drop(conv_svc);
        drop(dm);
        drop(ctx1);
        drop(ctx2);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn confirmed_conversation_deletion_removes_managed_copies_and_preserves_originals() {
        let db = test_db();
        let root =
            std::env::temp_dir().join(format!("nexora-attachment-delete-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.txt");
        std::fs::write(&source, "original bytes").unwrap();
        let store =
            super::super::managed_attachments::AttachmentStore::open(root.join("attachments"))
                .unwrap();
        let conversation = create_conversation(&db, "managed");
        let other = create_conversation(&db, "other");
        let attachments =
            super::super::attachments::AttachmentService::with_store(&db, store.clone());
        let own = attachments
            .attach(conversation, "source.txt", &source.to_string_lossy(), None)
            .unwrap();
        let kept = attachments
            .attach(other, "source.txt", &source.to_string_lossy(), None)
            .unwrap();
        let message = insert_message(&db, conversation, "attached message");
        AttachmentRepository::new(&db)
            .update_message_id(own.id, Some(message))
            .unwrap();
        let service = DataManagementService::new(&db).with_attachments(store);
        let confirmations = ConfirmationRegistry::new();
        assert!(service
            .delete_conversation(conversation, "unconfirmed", &confirmations)
            .is_err());
        assert!(std::path::Path::new(&own.file_path).exists());
        service
            .delete_conversation(
                conversation,
                &mint_delete_conversation(&confirmations, conversation),
                &confirmations,
            )
            .unwrap();
        assert!(!std::path::Path::new(&own.file_path).exists());
        assert!(std::path::Path::new(&kept.file_path).exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"original bytes");
        service
            .clear(&mint_clear(&confirmations), &confirmations)
            .unwrap();
        assert!(!std::path::Path::new(&kept.file_path).exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"original bytes");
        drop(service);
        drop(attachments);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_attachment_admission_and_deletion_leave_no_managed_orphans() {
        let db = test_db();
        let conversation = create_conversation(&db, "delete race");
        let root =
            std::env::temp_dir().join(format!("nexora-delete-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("note.txt");
        std::fs::write(&source, "original").unwrap();
        let store =
            super::super::managed_attachments::AttachmentStore::open(root.join("attachments"))
                .unwrap();
        let drafts = super::super::attachments::AttachmentService::with_store(&db, store.clone());
        let service = DataManagementService::new(&db).with_attachments(store);
        let confirmations = ConfirmationRegistry::new();
        let confirmation = mint_delete_conversation(&confirmations, conversation);
        std::thread::scope(|scope| {
            let attach = scope
                .spawn(|| drafts.attach(conversation, "note.txt", &source.to_string_lossy(), None));
            let delete = scope
                .spawn(|| service.delete_conversation(conversation, &confirmation, &confirmations));
            let _ = attach.join().unwrap();
            delete.join().unwrap().unwrap();
        });
        assert_eq!(count(&db, "conversations"), 0);
        assert_eq!(count(&db, "attachments"), 0);
        assert!(!root
            .join("attachments")
            .join(conversation.to_string())
            .exists());
        assert_eq!(std::fs::read(source).unwrap(), b"original");
        drop(service);
        drop(drafts);
        std::fs::remove_dir_all(root).unwrap();
    }

    // The child is a fresh test executable with its own mutex and SQLite
    // connections. Only the parent supplies this explicit synthetic root.
    const ATTACHMENT_CHILD_ROOT: &str = "NEXORA_ATTACHMENT_LIFECYCLE_TEST_ROOT";
    const ATTACHMENT_CHILD_TEST: &str =
        "application::data_management::tests::cross_process_delete_serializes_reused_id_admission_and_preserves_original";
    const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

    struct ProcessFixture(std::path::PathBuf);

    impl ProcessFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "nexora-attachment-process-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(super::super::workspace::strip_verbatim(
                root.canonicalize().unwrap(),
            ))
        }

        fn wait_for(&self, marker: &str) {
            let deadline = std::time::Instant::now() + HANDSHAKE_TIMEOUT;
            while !self.0.join(marker).exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "attachment process handshake timed out: {marker}; child log: {}",
                    std::fs::read_to_string(self.0.join("child.log")).unwrap_or_default()
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    impl Drop for ProcessFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct AttachmentChild(std::process::Child);

    impl AttachmentChild {
        fn wait_success(&mut self, root: &ProcessFixture) {
            let deadline = std::time::Instant::now() + HANDSHAKE_TIMEOUT;
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "attachment child failed: {}",
                        std::fs::read_to_string(root.0.join("child.log")).unwrap_or_default()
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "attachment child hung"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    impl Drop for AttachmentChild {
        fn drop(&mut self) {
            // Always reap, including parent assertion failure / timeout.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn cross_process_attachment_worker(root: std::ffi::OsString) {
        let root = ProcessFixture(std::path::PathBuf::from(root));
        // Parent owns cleanup; don't remove its store from the child.
        let root = std::mem::ManuallyDrop::new(root);
        root.wait_for("deleted");
        let db =
            Database::new(crate::infrastructure::database::open(root.0.join("nexora.db")).unwrap());
        let conversation = create_conversation(&db, "new after deletion commit");
        assert_eq!(conversation, 1, "exercise SQLite maximum-id reuse");
        // A's transaction has committed, so B can write the reused id, but
        // admission must still be excluded until A finishes owned-file purge.
        super::super::managed_attachments::assert_lifecycle_contended(&db);
        std::fs::write(root.0.join("contended"), b"OS lease held by parent").unwrap();
        let store =
            super::super::managed_attachments::AttachmentStore::open(root.0.join("attachments"))
                .unwrap();
        let attachment = super::super::attachments::AttachmentService::with_store(&db, store)
            .attach(
                conversation,
                "original.txt",
                &root.0.join("original.txt").to_string_lossy(),
                None,
            )
            .unwrap();
        let (message, payloads) = super::super::conversations::ConversationService::new(&db)
            .persist_user_message_with_attachments(
                conversation,
                "new attachment",
                "openai",
                crate::infrastructure::providers::catalog::openai::SUPPORTED_MODELS[0],
                &[attachment.id],
            )
            .unwrap();
        assert_eq!(payloads.len(), 1);
        let result = (
            std::process::id(),
            conversation,
            attachment.id,
            message,
            attachment.file_path,
        );
        std::fs::write(
            root.0.join("result.json"),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
    }

    fn cross_process_purge_then_admission(clear_all: bool) {
        use sha2::{Digest, Sha256};

        let root = ProcessFixture::new();
        let db =
            Database::new(crate::infrastructure::database::open(root.0.join("nexora.db")).unwrap());
        let original = root.0.join("original.txt");
        std::fs::write(&original, b"synthetic original must survive unchanged").unwrap();
        let original_sha = Sha256::digest(std::fs::read(&original).unwrap());
        let store =
            super::super::managed_attachments::AttachmentStore::open(root.0.join("attachments"))
                .unwrap();
        let conversation = create_conversation(&db, "sole maximum id");
        assert_eq!(conversation, 1);
        let old = super::super::attachments::AttachmentService::with_store(&db, store.clone())
            .attach(
                conversation,
                "original.txt",
                &original.to_string_lossy(),
                None,
            )
            .unwrap();
        let log = std::fs::File::create(root.0.join("child.log")).unwrap();
        let mut child = AttachmentChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", ATTACHMENT_CHILD_TEST, "--nocapture"])
                .env(ATTACHMENT_CHILD_ROOT, &root.0)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let after_commit = || {
            assert_eq!(count(&db, "conversations"), 0);
            assert!(std::path::Path::new(&old.file_path).exists());
            std::fs::write(root.0.join("deleted"), b"commit complete; purge pending").unwrap();
            root.wait_for("contended");
            assert!(
                !root.0.join("result.json").exists(),
                "admission escaped the lease"
            );
        };
        let mut service = DataManagementService::new(&db).with_attachments(store);
        service.after_attachment_delete = Some(&after_commit);
        let confirmations = ConfirmationRegistry::new();
        if clear_all {
            service
                .clear(&mint_clear(&confirmations), &confirmations)
                .unwrap();
        } else {
            service
                .delete_conversation(
                    conversation,
                    &mint_delete_conversation(&confirmations, conversation),
                    &confirmations,
                )
                .unwrap();
        }
        child.wait_success(&root);
        let (child_pid, new_conversation, attachment_id, message_id, path): (
            u32,
            i64,
            i64,
            i64,
            String,
        ) = serde_json::from_slice(&std::fs::read(root.0.join("result.json")).unwrap()).unwrap();
        assert_ne!(child_pid, std::process::id());
        assert_eq!(new_conversation, conversation);
        assert!(!std::path::Path::new(&old.file_path).exists());
        let row = AttachmentRepository::new(&db)
            .read(attachment_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.conversation_id, new_conversation);
        assert_eq!(row.message_id, Some(message_id));
        assert_eq!(row.file_path, path);
        assert_eq!(count(&db, "attachments"), 1);
        assert_eq!(count(&db, "messages"), 1);
        assert_eq!(
            super::super::managed_attachments::read(std::path::Path::new(&path)).unwrap(),
            std::fs::read(&original).unwrap()
        );
        assert_eq!(
            Sha256::digest(std::fs::read(&original).unwrap()),
            original_sha
        );
        println!("two-process {}: committed delete -> reused id -> OS contention -> purge -> admitted/linked copy survives; original SHA unchanged", if clear_all { "clear" } else { "delete" });
    }

    #[test]
    fn cross_process_delete_serializes_reused_id_admission_and_preserves_original() {
        if let Some(root) = std::env::var_os(ATTACHMENT_CHILD_ROOT) {
            cross_process_attachment_worker(root);
        } else {
            cross_process_purge_then_admission(false);
        }
    }

    #[test]
    fn cross_process_clear_serializes_new_admission_and_preserves_original() {
        cross_process_purge_then_admission(true);
    }

    #[test]
    fn lifecycle_lock_open_failure_preserves_rows_and_originals() {
        let root = ProcessFixture::new();
        let db =
            Database::new(crate::infrastructure::database::open(root.0.join("nexora.db")).unwrap());
        let conversation = create_conversation(&db, "lock failure");
        let original = root.0.join("original.txt");
        std::fs::write(&original, b"unchanged original").unwrap();
        let store =
            super::super::managed_attachments::AttachmentStore::open(root.0.join("attachments"))
                .unwrap();
        let attachment = AttachmentRepository::new(&db)
            .create(
                conversation,
                "original.txt",
                &original.to_string_lossy(),
                Some(18),
                Some("text/plain"),
            )
            .unwrap();
        // A directory at the stable lock name cannot be opened as the regular
        // no-follow file required by the coordinator.
        std::fs::create_dir(root.0.join("nexora.db.attachments.lock")).unwrap();
        let attachments =
            super::super::attachments::AttachmentService::with_store(&db, store.clone());
        assert!(attachments
            .attach(
                conversation,
                "original.txt",
                &original.to_string_lossy(),
                None
            )
            .is_err());
        assert!(attachments.remove(attachment).is_err());
        let conversations = super::super::conversations::ConversationService::new(&db);
        assert!(conversations
            .persist_user_message_with_attachments(conversation, "blocked", "openai", "unused", &[])
            .is_err());
        assert!(conversations.agent_history(conversation, "openai").is_err());
        let service = DataManagementService::new(&db).with_attachments(store);
        let confirmations = ConfirmationRegistry::new();
        assert!(service
            .delete_conversation(
                conversation,
                &mint_delete_conversation(&confirmations, conversation),
                &confirmations
            )
            .is_err());
        assert!(service
            .clear(&mint_clear(&confirmations), &confirmations)
            .is_err());
        assert_eq!(count(&db, "conversations"), 1);
        assert_eq!(count(&db, "attachments"), 1);
        assert_eq!(count(&db, "messages"), 0);
        assert_eq!(std::fs::read(&original).unwrap(), b"unchanged original");
    }
}
