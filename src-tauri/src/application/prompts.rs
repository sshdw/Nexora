//! Prompt Library service: application-layer orchestration for reusable
//! prompts (FR-007; ROADMAP.md Phase 5 — Prompt Library; ARCHITECTURE.md §5).
//!
//! This service composes the existing [`PromptRepository`] for prompt
//! persistence and the existing [`ConversationRepository`] /
//! [`MessageRepository`] when a prompt is inserted into a conversation. It
//! adds no schema, no SQL, and no database access of its own: all persistence
//! is delegated to the existing repositories.
//!
//! # Provider independence (ARCHITECTURE.md §7)
//!
//! This module contains no `OpenAI`, `Anthropic`, or `Gemini`-specific
//! behavior. Prompts are plain reusable text templates: inserting a prompt
//! into a conversation persists its content as a user message and never
//! executes an AI request, so the AI execution flow stays exclusively behind
//! the existing [`ConversationService`] and execution layer.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::{Message, MessageRepository};
use crate::infrastructure::repository::prompts::{Prompt, PromptRepository};
use rusqlite::Error as SqliteError;

/// Application-layer result shared by Prompt Library operations, unifying
/// persistence and validation failures.
pub(crate) type Result<T> = std::result::Result<T, PromptLibraryError>;

/// `messages.role` value for user-authored messages (DATABASE.md §7.2).
const ROLE_USER: &str = "user";

/// Application-layer service managing the reusable prompt library (FR-007).
///
/// Wraps [`PromptRepository`] for prompt persistence and the existing
/// [`ConversationRepository`] / [`MessageRepository`] for the
/// insert-into-conversation operation. It is deliberately focused on
/// orchestration and validation; persistence behavior and schema constraints
/// remain in the repositories and the database.
pub(crate) struct PromptLibraryService<'a> {
    prompts: PromptRepository<'a>,
    conversations: ConversationRepository<'a>,
    messages: MessageRepository<'a>,
}

impl<'a> PromptLibraryService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            prompts: PromptRepository::new(db),
            conversations: ConversationRepository::new(db),
            messages: MessageRepository::new(db),
        }
    }

    /// Create and persist a new prompt (FR-007; DATABASE.md §7.3).
    ///
    /// Persists the caller-supplied `title` and `content`. The surrogate `id`
    /// and the `created_at` / `updated_at` timestamps are assigned by the
    /// schema.
    ///
    /// Returns the `id` of the newly inserted prompt.
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::Database`] if the insert fails, for
    /// example a `title` or `content` value rejected by the table CHECK
    /// constraints.
    pub(crate) fn create(&self, title: &str, content: &str) -> Result<i64> {
        Ok(self.prompts.create(title, content)?)
    }

    /// Read a prompt by `id`.
    ///
    /// Returns [`Some`] when a prompt with that `id` exists, or [`None`]
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::Database`] on a failed query or a
    /// poisoned connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Prompt>> {
        Ok(self.prompts.read(id)?)
    }

    /// List all prompts.
    ///
    /// Rows are returned in the repository's persisted order (`created_at`
    /// ascending, DATABASE.md §7.3). No filtering, search, or pagination is
    /// applied here (FR-009 search belongs to a later phase).
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::Database`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Prompt>> {
        Ok(self.prompts.list()?)
    }

    /// Edit an existing prompt (FR-007; DATABASE.md §7.3).
    ///
    /// Only `title` and `content` are written, matching the mutable fields
    /// the repository defines; `id`, `created_at`, and `updated_at` are
    /// preserved (`updated_at` is maintained by the schema trigger).
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::PromptNotFound`] when no prompt with
    /// `id` exists, or [`PromptLibraryError::Database`] when the update
    /// fails.
    pub(crate) fn update(&self, id: i64, title: &str, content: &str) -> Result<()> {
        self.prompts
            .read(id)?
            .ok_or_else(|| PromptLibraryError::PromptNotFound { id })?;
        self.prompts.update(id, title, content)?;
        Ok(())
    }

    /// Delete a prompt by `id` (FR-007).
    ///
    /// Hard delete through the repository. Deleting a prompt that does not
    /// exist is a no-op, matching the repository's existing delete semantics
    /// (DATABASE.md §7.3).
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::Database`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        self.prompts.delete(id)?;
        Ok(())
    }

    /// Insert `prompt_id` into `conversation_id` (FR-007; ROADMAP.md Phase 5).
    ///
    /// A prompt acts as a reusable message text: its `content` is persisted
    /// as a `user` message in the target conversation through the existing
    /// [`MessageRepository`] (DATABASE.md §7.2), and the created [`Message`]
    /// is returned. No AI request is executed here and no provider-specific
    /// behavior is involved.
    ///
    /// The flow is:
    ///   1. Load the prompt.
    ///   2. Require the conversation to exist.
    ///   3. Persist the prompt's `content` as a user message.
    ///
    /// # Errors
    ///
    /// Returns [`PromptLibraryError::PromptNotFound`] when no prompt with
    /// `prompt_id` exists; [`PromptLibraryError::ConversationNotFound`] when
    /// no conversation with `conversation_id` exists; or
    /// [`PromptLibraryError::Database`] when any persistence step fails.
    pub(crate) fn insert_into_conversation(
        &self,
        prompt_id: i64,
        conversation_id: i64,
    ) -> Result<Message> {
        let prompt = self
            .prompts
            .read(prompt_id)?
            .ok_or_else(|| PromptLibraryError::PromptNotFound { id: prompt_id })?;

        if !self.conversations.exists(conversation_id)? {
            return Err(PromptLibraryError::ConversationNotFound {
                id: conversation_id,
            });
        }

        let message_id =
            self.messages
                .create(conversation_id, ROLE_USER, &prompt.content, None, None)?;

        // The message was just inserted on the same shared connection, so it
        // is guaranteed readable; `None` here is an internal invariant
        // violation and is mapped to a classified database error.
        self.messages.read(message_id)?.ok_or_else(|| {
            PromptLibraryError::Database(DatabaseError::Sqlite(SqliteError::QueryReturnedNoRows))
        })
    }
}

/// Classified errors raised by Prompt Library orchestration.
///
/// Unifies validation and persistence failures. No variant carries a secret
/// value, so formatting a [`PromptLibraryError`] never writes a secret to the
/// logs (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub(crate) enum PromptLibraryError {
    /// No prompt with the referenced `id` exists.
    PromptNotFound {
        /// The requested prompt id.
        id: i64,
    },
    /// No conversation with the referenced `id` exists.
    ConversationNotFound {
        /// The requested conversation id.
        id: i64,
    },
    /// A persistence failure from a repository.
    Database(DatabaseError),
}

impl std::fmt::Display for PromptLibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PromptNotFound { id } => write!(f, "prompt {id} does not exist"),
            Self::ConversationNotFound { id } => {
                write!(f, "conversation {id} does not exist")
            }
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PromptLibraryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PromptNotFound { .. } | Self::ConversationNotFound { .. } => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for PromptLibraryError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Build a service over an in-memory database whose schema mirrors the
    /// documented `prompts` / `conversations` / `messages` tables (DATABASE.md
    /// §7.3, §7.1, §7.2), shaped like the sibling conversation-service tests.
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
                 role TEXT NOT NULL, -- CHECK omitted in tests so the
                     -- defensive UnexpectedMessageRole path
                     -- can be exercised (production enforces it).
                 content TEXT NOT NULL CHECK(length(content) > 0),
                 provider_id INTEGER
                     CHECK(provider_id IS NULL OR provider_id > 0)
                     REFERENCES providers(id) ON DELETE SET NULL,
                 model_name TEXT CHECK(length(model_name) <= 200),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0)
             );
             CREATE INDEX messages_conversation_order
                 ON messages (conversation_id, created_at);
             CREATE TABLE prompts (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL CHECK(length(title) > 0 AND length(title) <= 200),
                 content TEXT NOT NULL CHECK(length(content) > 0 AND length(content) <= 10000),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0),
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at)
             );",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    /// Read a prompt back from the repository, panicking if it is absent.
    fn read_prompt(db: &Database, id: i64) -> Prompt {
        PromptRepository::new(db)
            .read(id)
            .expect("read prompt")
            .expect("prompt exists")
    }

    /// Create a conversation directly through its repository.
    fn create_conversation(db: &Database) -> i64 {
        ConversationRepository::new(db)
            .create("Chat", "active")
            .expect("conversation created")
    }

    #[test]
    fn create_persists_a_prompt() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);

        let id = service
            .create("Plan", "Plan a project")
            .expect("prompt created");

        let prompt = read_prompt(&db, id);
        assert_eq!(prompt.title, "Plan");
        assert_eq!(prompt.content, "Plan a project");
        // `id` and the timestamps are schema-assigned.
        assert!(prompt.id > 0);
        assert!(prompt.created_at > 0);
        assert!(prompt.updated_at > 0);
    }

    #[test]
    fn list_returns_all_prompts() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        service.create("A", "first").expect("prompt created");
        service.create("B", "second").expect("prompt created");

        let prompts = service.list().expect("list prompts");
        assert_eq!(prompts.len(), 2);
        let titles: Vec<&str> = prompts.iter().map(|p| p.title.as_str()).collect();
        assert!(titles.contains(&"A"));
        assert!(titles.contains(&"B"));
    }

    #[test]
    fn read_returns_prompt_by_id_and_none_for_unknown() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let id = service.create("Plan", "content").expect("prompt created");

        let prompt = service
            .read(id)
            .expect("read prompt")
            .expect("prompt exists");
        assert_eq!(prompt.title, "Plan");
        assert_eq!(prompt.content, "content");

        assert!(service.read(42).expect("read unknown").is_none());
    }

    #[test]
    fn update_edits_only_title_and_content() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let id = service.create("Plan", "original").expect("prompt created");
        let before = read_prompt(&db, id);

        service
            .update(id, "Review", "revised")
            .expect("prompt updated");

        let after = read_prompt(&db, id);
        assert_eq!(after.id, before.id);
        assert_eq!(after.title, "Review");
        assert_eq!(after.content, "revised");
        // `created_at` and `updated_at` are preserved (updated_at is
        // maintained by the schema trigger in production).
        assert_eq!(after.created_at, before.created_at);
        assert_eq!(after.updated_at, before.updated_at);
    }

    #[test]
    fn update_of_unknown_prompt_is_not_found() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);

        let err = service.update(42, "X", "Y").expect_err("unknown prompt");

        assert!(matches!(err, PromptLibraryError::PromptNotFound { id: 42 }));
    }

    #[test]
    fn delete_removes_a_prompt() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let id = service.create("Plan", "content").expect("prompt created");

        service.delete(id).expect("prompt deleted");

        assert!(service.read(id).expect("read deleted").is_none());
    }

    #[test]
    fn delete_of_unknown_prompt_is_a_no_op() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);

        service.delete(42).expect("delete unknown prompt succeeds");
    }

    #[test]
    fn insert_into_conversation_persists_prompt_content_as_user_message() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let prompt_id = service
            .create("Plan", "Let's plan the launch.")
            .expect("prompt created");
        let conversation_id = create_conversation(&db);

        let message = service
            .insert_into_conversation(prompt_id, conversation_id)
            .expect("prompt inserted");

        assert_eq!(message.conversation_id, conversation_id);
        assert_eq!(message.role, ROLE_USER);
        assert_eq!(message.content, "Let's plan the launch.");
        assert_eq!(message.provider_id, None);
        assert_eq!(message.model_name, None);

        // The prompt row is untouched, and exactly one user message was added.
        assert_eq!(
            read_prompt(&db, prompt_id).content,
            "Let's plan the launch."
        );
        let history = MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "Let's plan the launch.");
    }

    #[test]
    fn insert_with_unknown_prompt_is_not_found() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let conversation_id = create_conversation(&db);

        let err = service
            .insert_into_conversation(42, conversation_id)
            .expect_err("unknown prompt");

        assert!(matches!(err, PromptLibraryError::PromptNotFound { id: 42 }));
        // Nothing is persisted when the prompt is missing.
        assert!(MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages")
            .is_empty());
    }

    #[test]
    fn insert_with_unknown_conversation_is_not_found() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let prompt_id = service.create("Plan", "content").expect("prompt created");

        let err = service
            .insert_into_conversation(prompt_id, 42)
            .expect_err("unknown conversation");

        assert!(matches!(
            err,
            PromptLibraryError::ConversationNotFound { id: 42 }
        ));
    }

    #[test]
    fn create_with_invalid_title_is_a_database_error() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);

        // Empty `title` violates the prompts CHECK constraint.
        let err = service.create("", "content").expect_err("empty title");

        assert!(matches!(err, PromptLibraryError::Database(_)));
    }

    #[test]
    fn update_with_invalid_content_is_a_database_error() {
        let db = test_db();
        let service = PromptLibraryService::new(&db);
        let id = service.create("Plan", "content").expect("prompt created");

        // Empty `content` violates the prompts CHECK constraint.
        let err = service.update(id, "Plan", "").expect_err("empty content");

        assert!(matches!(err, PromptLibraryError::Database(_)));
    }
}
