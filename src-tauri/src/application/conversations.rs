//! Conversation service: application-layer orchestration for AI conversations
//! (FR-002, FR-003, FR-005, FR-006; ROADMAP.md Phase 4 — Conversations;
//! ARCHITECTURE.md §5, §7).
//!
//! This service composes the existing [`ConversationRepository`] and
//! [`MessageRepository`] for persistence and the existing
//! [`RequestExecutionService`] for AI request execution, completing the Phase 4
//! backend flow: conversation → user message → AI execution → assistant message
//! → persisted history. It adds no schema, no SQL, and no database access of
//! its own: all persistence is delegated to the existing repositories.
//!
//! # Provider independence (ARCHITECTURE.md §7)
//!
//! This module contains no `OpenAI`, `Anthropic`, or `Gemini`-specific behavior.
//! Provider names and models are passed through unchanged to the execution
//! boundary, and all provider-specific execution stays behind
//! [`RequestExecutionService`] / `[ProviderExecutor]`. Credentials are never
//! accessed here: they belong exclusively to the existing
//! [`CredentialStore`](crate::infrastructure::providers::credentials::CredentialStore),
//! which only the execution layer touches.
//!
//! # Send-flow contract
//!
//! [`ConversationService::send_message`] executes the sequence required by
//! FR-003 / DATABASE.md §7.2:
//!
//! 1. Require the conversation to exist.
//! 2. Persist the user message.
//! 3. Load the conversation's persisted history in its existing chronological
//!    order.
//! 4. Build the provider-independent [`AiRequest`].
//! 5. Execute exclusively through the [`AiRequestExecutor`] boundary (the
//!    existing [`RequestExecutionService`] in production).
//! 6. Only after a successful execution, persist the normalized [`AiResponse`]
//!    as an assistant message and return it.
//!
//! A failed execution propagates as a classified error: the user message stays
//! persisted and no fake assistant message is created, so conversation history
//! is never corrupted (DATABASE.md §7.2; ARCHITECTURE.md §10). The
//! [`AiRequestExecutor`] seam exists only so this flow — including its failure
//! behavior — is testable without provider execution, keyring access, or the
//! network (ROADMAP.md Phase 10).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::{Conversation, ConversationRepository};
use crate::infrastructure::repository::messages::{Message, MessageRepository};
use crate::infrastructure::repository::providers::ProviderRepository;

use super::execution::{
    self, AiMessage, AiRequest, AiResponse, AiRole, RequestError, RequestExecutionService,
};

/// Application-layer result shared by conversation operations, unifying
/// persistence, validation, and AI-execution failures.
pub(crate) type Result<T> = std::result::Result<T, ConversationError>;

/// `messages.role` value for user-authored messages (DATABASE.md §7.2).
const ROLE_USER: &str = "user";

/// `messages.role` value for AI-authored messages (DATABASE.md §7.2).
const ROLE_ASSISTANT: &str = "assistant";

/// `conversations.status` value for a new or restored conversation
/// (DATABASE.md §7.1).
const STATUS_ACTIVE: &str = "active";

/// `conversations.status` value for an archived conversation (DATABASE.md §7.1).
const STATUS_ARCHIVED: &str = "archived";

/// Execution boundary consumed by the conversation send flow.
///
/// Accepts a provider-independent [`AiRequest`] and returns the normalized
/// [`AiResponse`] or a classified [`RequestError`]. The sole production
/// implementation is the existing [`RequestExecutionService`] (see the impl
/// below), so AI execution always passes through it; the seam exists so the
/// send flow is testable without provider execution and so this layer never
/// depends on a provider-specific type.
pub(crate) trait AiRequestExecutor {
    /// Execute `request`.
    fn execute(&self, request: &AiRequest) -> execution::Result<AiResponse>;
}

impl AiRequestExecutor for RequestExecutionService<'_> {
    fn execute(&self, request: &AiRequest) -> execution::Result<AiResponse> {
        RequestExecutionService::execute(self, request)
    }
}

/// Application-layer service orchestrating conversations, message exchange,
/// and AI execution.
///
/// Wraps [`ConversationRepository`] and [`MessageRepository`] for persistence,
/// [`ProviderRepository`] to resolve the persisted provider reference recorded
/// on an assistant message, and the [`AiRequestExecutor`] boundary for
/// execution. It is deliberately focused on orchestration and validation;
/// persistence behavior and schema constraints remain in the repositories and
/// the database.
pub(crate) struct ConversationService<'a> {
    conversations: ConversationRepository<'a>,
    messages: MessageRepository<'a>,
    providers: ProviderRepository<'a>,
    execution: Box<dyn AiRequestExecutor + 'a>,
}

impl<'a> ConversationService<'a> {
    /// Create a service over the shared application [`Database`] with the
    /// existing [`RequestExecutionService`] as the execution boundary.
    pub(crate) fn new(db: &'a Database) -> Self {
        let execution: Box<dyn AiRequestExecutor + 'a> = Box::new(RequestExecutionService::new(db));
        Self {
            conversations: ConversationRepository::new(db),
            messages: MessageRepository::new(db),
            providers: ProviderRepository::new(db),
            execution,
        }
    }

    /// Create a service over `db` with an explicit [`AiRequestExecutor`]
    /// (used by tests to drive the send flow without provider execution).
    #[cfg(test)]
    pub(crate) fn with_executor(
        db: &'a Database,
        execution: Box<dyn AiRequestExecutor + 'a>,
    ) -> Self {
        Self {
            conversations: ConversationRepository::new(db),
            messages: MessageRepository::new(db),
            providers: ProviderRepository::new(db),
            execution,
        }
    }

    /// Create and persist a new, active conversation (FR-002).
    ///
    /// The conversation is created with the schema's default active status
    /// (DATABASE.md §7.1); the schema assigns the surrogate `id` and the
    /// timestamps.
    ///
    /// Returns the `id` of the newly inserted conversation.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::Database`] if the insert fails, for
    /// example a `title` rejected by the table CHECK constraint.
    pub(crate) fn create(&self, title: &str) -> Result<i64> {
        Ok(self.conversations.create(title, STATUS_ACTIVE)?)
    }

    /// Persist `content` as a user message in the conversation
    /// `conversation_id` and execute the AI request through the execution
    /// boundary (FR-003, FR-004).
    ///
    /// The flow is:
    ///   1. Require the conversation to exist.
    ///   2. Persist the user message.
    ///   3. Load the conversation's persisted history in its existing
    ///      chronological order (DATABASE.md §7.2), including the message just
    ///      persisted.
    ///   4. Build the [`AiRequest`] from that history.
    ///   5. Execute exclusively through the [`AiRequestExecutor`] (the
    ///      existing [`RequestExecutionService`] in production); no provider
    ///      is called from this layer.
    ///   6. On success, persist the normalized [`AiResponse`] as an assistant
    ///      message and return it.
    ///
    /// `provider` and `model` are passed through unchanged to the execution
    /// boundary (FR-004) and are recorded on the assistant message; this layer
    /// performs no provider-specific branching.
    ///
    /// A failed execution propagates as an error: the persisted user message
    /// is kept and no assistant message is created (FR-003 error handling;
    /// DATABASE.md §7.2).
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with
    /// `conversation_id` exists; [`ConversationError::UnexpectedMessageRole`]
    /// when the persisted history contains a `role` outside `user` /
    /// `assistant`; [`ConversationError::Request`] when AI execution fails
    /// (unknown provider, missing credentials, provider failure, ...); or
    /// [`ConversationError::Database`] when any persistence step fails.
    pub(crate) fn send_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    ) -> Result<AiResponse> {
        if !self.conversations.exists(conversation_id)? {
            return Err(ConversationError::NotFound {
                id: conversation_id,
            });
        }

        self.messages
            .create(conversation_id, ROLE_USER, content, None, None)?;

        let history = self.messages.list_by_conversation(conversation_id)?;
        let request = AiRequest {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: history
                .iter()
                .map(ai_message_from)
                .collect::<Result<Vec<_>>>()?,
        };

        let response = self.execution.execute(&request)?;

        // Execution succeeded, so the provider metadata row is resolvable
        // (RequestExecutionService rejects an unknown provider before any
        // request is sent). The provider's id is recorded on the assistant
        // message (FR-004; DATABASE.md §7.2 `provider_id`).
        let provider_id = self.providers.read_by_name(provider)?.map(|p| p.id);
        self.messages.create(
            conversation_id,
            ROLE_ASSISTANT,
            &response.content,
            provider_id,
            Some(&response.model),
        )?;

        Ok(response)
    }

    /// Retrieve the messages belonging to `conversation_id` (FR-005).
    ///
    /// Returns the persisted [`Message`] records in their existing persisted
    /// order (`created_at` ascending, DATABASE.md §7.2) and their persisted
    /// roles; no second history representation is introduced.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with
    /// `conversation_id` exists, or [`ConversationError::Database`] when the
    /// query fails.
    pub(crate) fn history(&self, conversation_id: i64) -> Result<Vec<Message>> {
        if !self.conversations.exists(conversation_id)? {
            return Err(ConversationError::NotFound {
                id: conversation_id,
            });
        }
        Ok(self.messages.list_by_conversation(conversation_id)?)
    }

    /// Rename `conversation_id` to `title` (FR-002, FR-006).
    ///
    /// Only the `title` column is changed; the conversation's `status` is
    /// preserved (DATABASE.md §7.1). `updated_at` is maintained by the schema
    /// trigger.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with
    /// `conversation_id` exists, or [`ConversationError::Database`] when the
    /// update fails.
    pub(crate) fn rename(&self, id: i64, title: &str) -> Result<()> {
        let conversation = self
            .conversations
            .read(id)?
            .ok_or_else(|| ConversationError::NotFound { id })?;
        self.conversations.update(id, title, &conversation.status)?;
        Ok(())
    }

    /// Archive `conversation_id` (FR-006): set its `status` to `archived`
    /// while preserving its `title` (DATABASE.md §7.1).
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with `id`
    /// exists, or [`ConversationError::Database`] when the update fails.
    pub(crate) fn archive(&self, id: i64) -> Result<()> {
        let conversation = self
            .conversations
            .read(id)?
            .ok_or_else(|| ConversationError::NotFound { id })?;
        self.conversations
            .update(id, &conversation.title, STATUS_ARCHIVED)?;
        Ok(())
    }

    /// Restore `id` (FR-006): set an archived conversation's `status` back to
    /// `active` while preserving its `title` (DATABASE.md §7.1).
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with `id`
    /// exists, or [`ConversationError::Database`] when the update fails.
    pub(crate) fn restore(&self, id: i64) -> Result<()> {
        let conversation = self
            .conversations
            .read(id)?
            .ok_or_else(|| ConversationError::NotFound { id })?;
        self.conversations
            .update(id, &conversation.title, STATUS_ACTIVE)?;
        Ok(())
    }

    /// Delete `conversation_id` (FR-002, FR-013).
    ///
    /// Hard delete through the repository: dependent `messages` (and, in the
    /// full schema, `attachments`) are removed by the database's foreign keys
    /// (DATABASE.md §9). Deleting a conversation that does not exist is a
    /// no-op, matching the repository's existing delete semantics.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::Database`] if the delete fails.
    pub(crate) fn delete(&self, id: i64) -> Result<()> {
        self.conversations.delete(id)?;
        Ok(())
    }

    /// List every conversation (FR-002, FR-005).
    ///
    /// Rows are returned in the repository's persisted order. This is a thin
    /// pass-through to the existing [`ConversationRepository::list`]; no
    /// filtering, search, or pagination is applied here.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::Database`] if listing fails.
    pub(crate) fn list(&self) -> Result<Vec<Conversation>> {
        Ok(self.conversations.list()?)
    }
}

/// Map a persisted [`Message`] to the provider-independent [`AiMessage`] used
/// by an [`AiRequest`] (DATABASE.md §7.2: roles `user` and `assistant`).
///
/// # Errors
///
/// Returns [`ConversationError::UnexpectedMessageRole`] for a persisted role
/// outside `user` / `assistant`, which the table's CHECK constraint forbids.
fn ai_message_from(message: &Message) -> Result<AiMessage> {
    let role = match message.role.as_str() {
        ROLE_USER => AiRole::User,
        ROLE_ASSISTANT => AiRole::Assistant,
        other => {
            return Err(ConversationError::UnexpectedMessageRole {
                role: other.to_string(),
            })
        }
    };
    Ok(AiMessage {
        role,
        content: message.content.clone(),
    })
}

/// Classified errors raised by conversation orchestration.
///
/// Unifies validation, persistence, and AI-execution failures. No variant
/// carries a credential or other secret value, so formatting a
/// [`ConversationError`] never writes a secret to the logs (ARCHITECTURE.md §9,
/// §11). A failed AI request is propagated as [`ConversationError::Request`]
/// exactly as [`RequestError`] classifies it; no provider-specific detail is
/// introduced here.
#[derive(Debug)]
pub(crate) enum ConversationError {
    /// No conversation with the referenced `id` exists.
    NotFound {
        /// The requested conversation id.
        id: i64,
    },
    /// A persisted `messages.role` value outside `user` / `assistant`, which
    /// the schema's CHECK constraint should prevent.
    UnexpectedMessageRole {
        /// The persisted role value.
        role: String,
    },
    /// AI request execution failed (unknown provider, missing credentials,
    /// provider failure, ...).
    Request(RequestError),
    /// A persistence failure from a repository.
    Database(DatabaseError),
}

impl std::fmt::Display for ConversationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { id } => write!(f, "conversation {id} does not exist"),
            Self::UnexpectedMessageRole { role } => {
                write!(
                    f,
                    "persisted message role '{role}' is not a valid conversation role"
                )
            }
            Self::Request(err) => write!(f, "{err}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ConversationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound { .. } | Self::UnexpectedMessageRole { .. } => None,
            Self::Request(err) => Some(err),
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for ConversationError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<RequestError> for ConversationError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Shared cell through which a [`StubExecutor`] exposes the request it
    /// received, so tests can inspect the history passed to execution.
    type Captured = std::rc::Rc<std::cell::RefCell<Option<AiRequest>>>;

    fn captured_cell() -> Captured {
        std::rc::Rc::new(std::cell::RefCell::new(None))
    }

    /// Test-only [`AiRequestExecutor`] that records the request it receives
    /// and returns a preconfigured outcome, without touching the network or
    /// the OS keyring.
    struct StubExecutor {
        success: Option<AiResponse>,
        failure: Option<String>,
        captured: Captured,
    }

    impl StubExecutor {
        fn succeeding(response: AiResponse, captured: Captured) -> Self {
            Self {
                success: Some(response),
                failure: None,
                captured,
            }
        }

        fn failing(provider: String, captured: Captured) -> Self {
            Self {
                success: None,
                failure: Some(provider),
                captured,
            }
        }
    }

    impl AiRequestExecutor for StubExecutor {
        fn execute(&self, request: &AiRequest) -> execution::Result<AiResponse> {
            *self.captured.borrow_mut() = Some(request.clone());
            match (&self.success, &self.failure) {
                (Some(response), _) => Ok(response.clone()),
                (None, Some(provider)) => Err(RequestError::Execution {
                    name: provider.clone(),
                }),
                (None, None) => panic!("stub executor has no configured outcome"),
            }
        }
    }

    /// Build a service over an in-memory database whose schema mirrors the
    /// documented `conversations` / `messages` / `providers` tables
    /// (DATABASE.md §7.1, §7.2, §7.5). The application's migration set is
    /// intentionally empty (Phase 1 migrations are a separate task), so the
    /// test schema is created here to exercise the persisted flow end to end.
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
                 ON messages (conversation_id, created_at);",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    /// Build a conversation service whose execution boundary always succeeds
    /// with `response`; the [`AiRequest`] passed to execution is recorded in
    /// `captured`.
    fn succeeding_service(
        db: &Database,
        response: AiResponse,
    ) -> (ConversationService<'_>, Captured) {
        let captured = captured_cell();
        let service = ConversationService::with_executor(
            db,
            Box::new(StubExecutor::succeeding(
                response,
                std::rc::Rc::clone(&captured),
            )),
        );
        (service, captured)
    }

    /// Build a conversation service whose execution boundary always fails with
    /// an execution error for the provider named `provider`; the request is
    /// recorded in `captured`.
    fn failing_service<'a>(
        db: &'a Database,
        provider: &str,
    ) -> (ConversationService<'a>, Captured) {
        let captured = captured_cell();
        let service = ConversationService::with_executor(
            db,
            Box::new(StubExecutor::failing(
                provider.to_string(),
                std::rc::Rc::clone(&captured),
            )),
        );
        (service, captured)
    }

    fn read_conversation(db: &Database, id: i64) -> Conversation {
        ConversationRepository::new(db)
            .read(id)
            .expect("read conversation")
            .expect("conversation exists")
    }

    #[test]
    fn create_persists_an_active_conversation() {
        let db = test_db();
        let service = ConversationService::new(&db);

        let id = service.create("Planning").expect("conversation created");

        let conversation = read_conversation(&db, id);
        assert_eq!(conversation.title, "Planning");
        assert_eq!(conversation.status, STATUS_ACTIVE);
        // `id` and the timestamps are schema-assigned.
        assert!(conversation.id > 0);
        assert!(conversation.created_at > 0);
        assert!(conversation.updated_at >= conversation.created_at);
    }

    #[test]
    fn send_message_returns_the_normalized_ai_response() {
        let db = test_db();
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "response text".to_string(),
                model: "gpt-4o-mini".to_string(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        let response = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini")
            .expect("send succeeds");

        assert_eq!(response.content, "response text");
        assert_eq!(response.model, "gpt-4o-mini");
    }

    #[test]
    fn history_passed_to_execution_is_chronological_and_complete() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "answer two".to_string(),
                model: "gpt-4o-mini".to_string(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        // A prior user/assistant exchange seeded directly into the repository
        // (prior persisted history), followed by a new message sent through the
        // application flow.
        let messages = MessageRepository::new(&db);
        messages
            .create(conversation_id, ROLE_USER, "question one", None, None)
            .expect("prior user message persisted");
        messages
            .create(conversation_id, ROLE_ASSISTANT, "answer one", None, None)
            .expect("prior assistant message persisted");

        service
            .send_message(conversation_id, "question two", "openai", "gpt-4o-mini")
            .expect("send succeeds");

        let request = captured
            .borrow()
            .as_ref()
            .expect("an AiRequest was passed to execution")
            .clone();
        assert_eq!(request.provider, "openai");
        assert_eq!(request.model, "gpt-4o-mini");
        let turns: Vec<(AiRole, &str)> = request
            .messages
            .iter()
            .map(|m| (m.role, m.content.as_str()))
            .collect();
        assert_eq!(turns.len(), 3);
        // The persisted chronological order and roles are preserved.
        assert_eq!(turns[0], (AiRole::User, "question one"));
        assert_eq!(turns[1], (AiRole::Assistant, "answer one"));
        assert_eq!(turns[2], (AiRole::User, "question two"));
    }

    #[test]
    fn successful_send_persists_user_then_assistant_message() {
        let db = test_db();
        let provider_id = ProviderRepository::new(&db)
            .create("openai", "OpenAI")
            .expect("provider created");
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "persisted answer".to_string(),
                model: "gpt-4o-mini".to_string(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini")
            .expect("send succeeds");

        let history = service.history(conversation_id).expect("history loads");
        assert_eq!(history.len(), 2);
        // The user message is persisted first, without provider attribution.
        assert_eq!(history[0].role, ROLE_USER);
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[0].provider_id, None);
        assert_eq!(history[0].model_name, None);
        // The assistant message carries exactly the normalized response and its
        // provider/model attribution (FR-004; DATABASE.md §7.2).
        assert_eq!(history[1].role, ROLE_ASSISTANT);
        assert_eq!(history[1].content, "persisted answer");
        assert_eq!(history[1].provider_id, Some(provider_id));
        assert_eq!(history[1].model_name.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn failed_execution_persists_user_message_without_assistant_message() {
        let db = test_db();
        let (service, captured) = failing_service(&db, "openai");
        let conversation_id = service.create("Chat").expect("conversation created");

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini")
            .expect_err("execution fails");

        // The execution error is propagated through the application layer,
        // classified exactly as RequestExecutionService classified it.
        assert!(matches!(
            err,
            ConversationError::Request(RequestError::Execution { name }) if name == "openai"
        ));
        // The request was actually handed to the execution boundary.
        assert!(captured.borrow().is_some());

        // The user message remains persisted; no fake assistant message exists.
        let history = service.history(conversation_id).expect("history loads");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, ROLE_USER);
        assert_eq!(history[0].content, "hello");
    }

    #[test]
    fn retry_after_failure_persists_assistant_for_the_follow_up() {
        let db = test_db();
        let id = {
            let (service, _captured) = failing_service(&db, "openai");
            let id = service.create("Chat").expect("conversation created");
            service
                .send_message(id, "question", "openai", "gpt-4o-mini")
                .expect_err("first attempt fails");
            id
        };
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "final answer".to_string(),
                model: "gpt-4o-mini".to_string(),
            },
        );

        service
            .send_message(id, "retry", "openai", "gpt-4o-mini")
            .expect("retry succeeds");

        let history = service.history(id).expect("history loads");
        let turns: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
        // [failing question] stays persisted; the retry appends its own
        // user prompt and the successful assistant answer.
        assert_eq!(turns, ["question", "retry", "final answer"]);
        assert!(history
            .iter()
            .all(|m| m.role == ROLE_USER || m.role == ROLE_ASSISTANT));
    }

    #[test]
    fn rename_changes_title_and_preserves_status() {
        let db = test_db();
        let service = ConversationService::new(&db);
        let id = service.create("Old Title").expect("conversation created");

        service.rename(id, "New Title").expect("rename succeeds");

        let conversation = read_conversation(&db, id);
        assert_eq!(conversation.title, "New Title");
        assert_eq!(conversation.status, STATUS_ACTIVE);
    }

    #[test]
    fn archive_sets_status_to_archived_preserving_title() {
        let db = test_db();
        let service = ConversationService::new(&db);
        let id = service.create("Archive Me").expect("conversation created");

        service.archive(id).expect("archive succeeds");

        let conversation = read_conversation(&db, id);
        assert_eq!(conversation.status, STATUS_ARCHIVED);
        assert_eq!(conversation.title, "Archive Me");
    }

    #[test]
    fn restore_returns_archived_conversation_to_active() {
        let db = test_db();
        let service = ConversationService::new(&db);
        let id = service.create("Restore Me").expect("conversation created");

        service.archive(id).expect("archive succeeds");
        service.restore(id).expect("restore succeeds");

        let conversation = read_conversation(&db, id);
        assert_eq!(conversation.status, STATUS_ACTIVE);
        assert_eq!(conversation.title, "Restore Me");
    }

    #[test]
    fn delete_removes_conversation_and_cascades_its_messages() {
        let db = test_db();
        let service = ConversationService::new(&db);
        let id = service.create("Doomed").expect("conversation created");
        MessageRepository::new(&db)
            .create(id, ROLE_USER, "hello", None, None)
            .expect("user message persisted");

        service.delete(id).expect("delete succeeds");

        // Hard delete: the conversation and its messages are gone.
        assert!(ConversationRepository::new(&db)
            .read(id)
            .expect("read")
            .is_none());
        assert!(
            MessageRepository::new(&db)
                .list_by_conversation(id)
                .expect("list messages")
                .is_empty(),
            "messages cascade-delete with the conversation"
        );
    }

    #[test]
    fn provider_and_model_pass_through_without_provider_specific_branching() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "model-v2".to_string(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        // An arbitrary provider/model flows through the request unchanged; the
        // conversation layer never branches on a specific provider.
        service
            .send_message(conversation_id, "hi", "custom-provider", "custom-model")
            .expect("send succeeds");

        let request = captured
            .borrow()
            .as_ref()
            .expect("an AiRequest was passed to execution")
            .clone();
        assert_eq!(request.provider, "custom-provider");
        assert_eq!(request.model, "custom-model");
    }

    #[test]
    fn send_message_to_unknown_conversation_is_not_found() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
            },
        );

        let err = service
            .send_message(42, "hello", "openai", "gpt-4o-mini")
            .expect_err("unknown conversation");

        assert!(matches!(err, ConversationError::NotFound { id: 42 }));
        // The flow aborts before persisting anything or reaching execution.
        assert!(captured.borrow().is_none());
        assert!(MessageRepository::new(&db)
            .list_by_conversation(42)
            .expect("list messages")
            .is_empty());
    }

    #[test]
    fn rename_archive_and_restore_of_unknown_conversation_are_not_found() {
        let db = test_db();
        let service = ConversationService::new(&db);

        for result in [
            service.rename(99, "X"),
            service.archive(99),
            service.restore(99),
        ] {
            assert!(matches!(
                result,
                Err(ConversationError::NotFound { id: 99 })
            ));
        }
    }

    #[test]
    fn unexpected_persisted_role_aborts_before_execution() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        // Seed a role the schema CHECK forbids, simulating a corrupted row.
        {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO messages (conversation_id, role, content) \
                 VALUES (?1, 'system', 'corrupted')",
                [conversation_id],
            )
            .expect("seed corrupted message");
        }

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini")
            .expect_err("unexpected role");

        assert!(matches!(
            err,
            ConversationError::UnexpectedMessageRole { role } if role == "system"
        ));
        // The user message was persisted before the role check, but execution
        // was never reached.
        assert!(captured.borrow().is_none());
        let history = MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages");
        assert_eq!(history.len(), 2);
    }
}
