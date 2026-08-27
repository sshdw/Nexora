//! Conversation import service: application-layer orchestration for FR-011
//! (ROADMAP.md Phase 8.2; ARCHITECTURE.md §5; DATABASE.md §16).
//!
//! Imports a single conversation from the JSON document produced by the
//! Phase 8.1 export ([`crate::application::export`]) as a **new** conversation.
//! The service accepts exactly the Phase 8.1 format
//! (`format: "nexora-conversation"`, `version: 1`) and no other format.
//!
//! # Atomicity
//!
//! The whole import runs inside one transaction via the shared
//! [`Repository::transaction`] foundation (DATABASE.md §5, §12): all `INSERT`s
//! commit together or roll back together on any failure, so an import can
//! never leave a partially populated database. The document is fully decoded
//! and validated before any write, so an invalid document performs no writes
//! at all.
//!
//! # New identifiers
//!
//! Imported conversations and messages always receive **new** surrogate ids
//! assigned by the schema (DATABASE.md §16). Exported primary-key ids are
//! never reused, and nothing is merged or modified: imported items are
//! inserted as new rows.
//!
//! # Provider references
//!
//! Messages reference providers by `provider_id` (an integer foreign key into
//! `providers`). Because an exported `provider_id` is local to the exporting
//! machine and may not exist on this one, the reference is preserved only when
//! it matches an existing local provider and otherwise imported as `NULL`,
//! keeping the database's "provider reference is valid or `NULL`" invariant
//! (DATABASE.md §13) and honouring the schema foreign key. `model_name` is
//! always preserved. No provider records are ever created by an import.
//!
//! # Error handling
//!
//! Failures are classified by [`ImportError`]: malformed JSON is
//! [`ImportError::InvalidJson`], a wrong `format` is
//! [`ImportError::UnsupportedFormat`], an unsupported `version` is
//! [`ImportError::UnsupportedVersion`], invalid document data is
//! [`ImportError::InvalidData`], and persistence/transaction failures are
//! [`ImportError::Database`]. No error variant carries a credential or other
//! secret value (ARCHITECTURE.md §9, §11).

use std::collections::HashSet;

use serde::Deserialize;

use crate::application::export::{EXPORT_FORMAT, EXPORT_VERSION};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::MessageRepository;
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::Repository;

/// Application-layer result shared by import operations, unifying
/// validation and persistence failures.
pub(crate) type Result<T> = std::result::Result<T, ImportError>;

/// Return [`Ok`] when `condition` is true, otherwise an
/// [`ImportError::InvalidData`] carrying `reason`.
fn ensure_valid(condition: bool, reason: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(ImportError::InvalidData(reason.into()))
    }
}

/// A Phase 8.1 conversation export document decoded from JSON.
///
/// Field presence matches the document written by the Phase 8.1 export
/// ([`crate::application::export`]); every structurally required field is a
/// required field here. `messages[].id` is intentionally not declared: the
/// exported primary-key ids are ignored and never reused.
#[derive(Debug, Deserialize)]
pub(crate) struct ImportDocument {
    /// Document kind marker; must equal [`EXPORT_FORMAT`].
    pub format: String,
    /// Document layout version; must equal [`EXPORT_VERSION`].
    pub version: i64,
    /// The conversation record to import.
    pub conversation: ImportConversation,
    /// The conversation's messages in persisted order.
    pub messages: Vec<ImportMessage>,
}

/// A `conversation` record inside an [`ImportDocument`].
#[derive(Debug, Deserialize)]
pub(crate) struct ImportConversation {
    /// Exported conversation primary key, used only to verify each imported
    /// message's `conversation_id` refers to its own conversation.
    pub id: i64,
    /// Human-readable name (`title`).
    pub title: String,
    /// Archive state (`status`).
    pub status: String,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
    /// Last modification timestamp (`updated_at`).
    pub updated_at: i64,
}

/// A single `messages[]` entry inside an [`ImportDocument`].
#[derive(Debug, Deserialize)]
pub(crate) struct ImportMessage {
    /// Conversation the message belonged to in the exported document; must
    /// equal the document's conversation `id`.
    pub conversation_id: i64,
    /// Message author type (`role`): `user` or `assistant`.
    pub role: String,
    /// Message text (`content`).
    pub content: String,
    /// Exported provider reference (`provider_id`), resolved against local
    /// providers at import time.
    pub provider_id: Option<i64>,
    /// Specific model used (`model_name`).
    pub model_name: Option<String>,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
}

impl ImportDocument {
    /// Validate the document against the Phase 8.1 format and the schema
    /// constraints of `conversations` / `messages` (DATABASE.md §7.1, §7.2)
    /// without touching the database.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::UnsupportedFormat`],
    /// [`ImportError::UnsupportedVersion`], or [`ImportError::InvalidData`] on
    /// the first problem encountered.
    fn validate(&self) -> Result<()> {
        if self.format != EXPORT_FORMAT {
            return Err(ImportError::UnsupportedFormat {
                format: self.format.clone(),
            });
        }
        if self.version != EXPORT_VERSION {
            return Err(ImportError::UnsupportedVersion {
                version: self.version,
            });
        }

        let conversation = &self.conversation;
        ensure_valid(
            !conversation.title.is_empty() && conversation.title.len() <= 500,
            "conversation 'title' must be non-empty and at most 500 characters",
        )?;
        ensure_valid(
            conversation.status == "active" || conversation.status == "archived",
            format!(
                "conversation 'status' must be 'active' or 'archived', found '{}'",
                conversation.status
            ),
        )?;
        ensure_valid(
            conversation.created_at > 0,
            "conversation 'created_at' must be a positive integer",
        )?;
        ensure_valid(
            conversation.updated_at >= conversation.created_at,
            "conversation 'updated_at' must not be earlier than 'created_at'",
        )?;

        for (index, message) in self.messages.iter().enumerate() {
            validate_message(message, index, conversation.id)?;
        }
        Ok(())
    }
}

/// Validate a single imported message against the schema constraints
/// (DATABASE.md §7.2) and require it to belong to its own conversation.
fn validate_message(message: &ImportMessage, index: usize, conversation_id: i64) -> Result<()> {
    let field = |name: &str| format!("messages[{index}].{name}");

    ensure_valid(
        message.conversation_id == conversation_id,
        format!(
            "{} must equal the conversation id {conversation_id}, found {}",
            field("conversation_id"),
            message.conversation_id
        ),
    )?;
    ensure_valid(
        message.role == "user" || message.role == "assistant",
        format!(
            "{} role must be 'user' or 'assistant', found '{}'",
            field("role"),
            message.role
        ),
    )?;
    ensure_valid(
        !message.content.is_empty(),
        format!("{} content must be non-empty", field("content")),
    )?;
    if let Some(provider_id) = message.provider_id {
        ensure_valid(
            provider_id > 0,
            format!(
                "{} provider_id must be positive or null, found {provider_id}",
                field("provider_id")
            ),
        )?;
    }
    if let Some(model) = &message.model_name {
        ensure_valid(
            model.len() <= 200,
            format!(
                "{} model_name must be at most 200 characters",
                field("model_name")
            ),
        )?;
    }
    ensure_valid(
        message.created_at > 0,
        format!(
            "{} created_at must be a positive integer",
            field("created_at")
        ),
    )
}

/// Decode `json` into an [`ImportDocument`] and validate it, performing no
/// database writes.
fn parse_document(json: &str) -> Result<ImportDocument> {
    let document: ImportDocument = match serde_json::from_str(json) {
        Ok(document) => document,
        Err(err) => return Err(classify_error(err)),
    };
    document.validate()?;
    Ok(document)
}

/// Distinguish malformed JSON from well-formed JSON with invalid structure.
/// JSON syntax/EOF errors are [`ImportError::InvalidJson`]; missing or
/// wrongly-typed fields are [`ImportError::InvalidData`].
fn classify_error(err: serde_json::Error) -> ImportError {
    match err.classify() {
        serde_json::error::Category::Data => ImportError::InvalidData(err.to_string()),
        _ => ImportError::InvalidJson(err),
    }
}

/// Application-layer service that imports conversations from Phase 8.1 JSON
/// documents (FR-011).
///
/// The service reads provider metadata to decide which exported `provider_id`
/// references are valid locally, then delegates all persistence to the
/// existing repositories inside the shared transaction foundation. It contains
/// no schema and performs no raw SQL of its own: inserts go through
/// [`ConversationRepository::create_with_timestamps`] and
/// [`MessageRepository::create_with_timestamps`].
pub(crate) struct ImportService<'a> {
    conversations: ConversationRepository<'a>,
    messages: MessageRepository<'a>,
    providers: ProviderRepository<'a>,
}

impl<'a> ImportService<'a> {
    /// Create an import service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            conversations: ConversationRepository::new(db),
            messages: MessageRepository::new(db),
            providers: ProviderRepository::new(db),
        }
    }

    /// Import a conversation from the Phase 8.1 JSON document `json` (FR-011).
    ///
    /// The document is fully decoded and validated before any write, then the
    /// inserts run atomically in one transaction that also assigns new
    /// surrogate ids. Message `provider_id` references that do not match an
    /// existing local provider are imported as `NULL`.
    ///
    /// Returns the new conversation's id.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::UnsupportedFormat`] or
    /// [`ImportError::UnsupportedVersion`] for a non-Phase 8.1 document,
    /// [`ImportError::InvalidData`] when the document violates the schema
    /// constraints, or [`ImportError::Database`] when a read or the
    /// transactional insert fails.
    pub(crate) fn import(&self, json: &str) -> Result<i64> {
        let document = parse_document(json)?;
        let valid_providers = self.resolve_provider_ids(&document)?;
        let conversation_id = self.conversations.transaction(|tx| {
            let conversation_id = ConversationRepository::create_with_timestamps(
                tx,
                &document.conversation.title,
                &document.conversation.status,
                document.conversation.created_at,
                document.conversation.updated_at,
            )?;
            for message in &document.messages {
                // Keep the reference only when it resolves to a local provider;
                // otherwise store NULL so the FK check is satisfied and the
                // "valid or NULL" invariant (DATABASE.md §13) holds.
                let provider_id = message
                    .provider_id
                    .filter(|id| valid_providers.contains(id));
                MessageRepository::create_with_timestamps(
                    tx,
                    conversation_id,
                    &message.role,
                    &message.content,
                    provider_id,
                    message.model_name.as_deref(),
                    message.created_at,
                )?;
            }
            Ok(conversation_id)
        })?;
        Ok(conversation_id)
    }

    /// Resolve which exported `provider_id` values reference an existing local
    /// `providers` row. Read-only; providers are never created here. Read
    /// happens before the insert transaction so it never contends with its
    /// connection lock.
    fn resolve_provider_ids(&self, document: &ImportDocument) -> Result<HashSet<i64>> {
        let mut referenced = HashSet::new();
        for message in &document.messages {
            if let Some(id) = message.provider_id {
                referenced.insert(id);
            }
        }
        let mut valid = HashSet::new();
        for id in referenced {
            if self.providers.read(id)?.is_some() {
                valid.insert(id);
            }
        }
        Ok(valid)
    }
}

/// Classified errors raised by conversation import (FR-011).
///
/// No variant carries a credential or other secret value, so formatting an
/// [`ImportError`] never writes a secret to the logs (ARCHITECTURE.md §9,
/// §11).
#[derive(Debug)]
pub(crate) enum ImportError {
    /// The input is not valid JSON.
    InvalidJson(serde_json::Error),
    /// The document's `format` value is not the Phase 8.1 format
    /// ([`EXPORT_FORMAT`]).
    UnsupportedFormat {
        /// The `format` value found in the document.
        format: String,
    },
    /// The document's `version` is not supported ([`EXPORT_VERSION`]).
    UnsupportedVersion {
        /// The `version` value found in the document.
        version: i64,
    },
    /// The document violates the Phase 8.1 format or the schema constraints.
    InvalidData(String),
    /// A persistence or transaction failure from a repository.
    Database(DatabaseError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(err) => write!(f, "invalid JSON: {err}"),
            Self::UnsupportedFormat { format } => {
                write!(f, "unsupported import format '{format}'")
            }
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported import version {version}")
            }
            Self::InvalidData(reason) => write!(f, "invalid import document: {reason}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(err) => Some(err),
            Self::Database(err) => Some(err),
            Self::UnsupportedFormat { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidData(_) => None,
        }
    }
}

impl From<DatabaseError> for ImportError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::repository::conversations::Conversation;
    use rusqlite::Connection;

    /// Build an in-memory database whose `providers` / `conversations` /
    /// `messages` tables mirror the production schema (DATABASE.md §7.1, §7.2,
    /// §7.5), with `messages.content` constrained to `content_check` so tests
    /// can inject a stricter rule to exercise rollback.
    fn test_db_with_content_check(content_check: &str) -> Database {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(&format!(
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
                 content TEXT NOT NULL CHECK({content_check}),
                 provider_id INTEGER
                     CHECK(provider_id IS NULL OR provider_id > 0)
                     REFERENCES providers(id) ON DELETE SET NULL,
                 model_name TEXT CHECK(length(model_name) <= 200),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0)
             );"
        ))
        .expect("create test schema");
        Database::new(conn)
    }

    /// The default test database mirrors the production `content` CHECK
    /// (non-empty only).
    fn test_db() -> Database {
        test_db_with_content_check("length(content) > 0")
    }

    /// Wrap a conversation and ordered messages into a Phase 8.1 document.
    ///
    /// Takes references (the `json!` macro borrows them), so this helper is
    /// intentionally not pass-by-value.
    #[allow(clippy::needless_pass_by_value)]
    fn doc(conversation: serde_json::Value, messages: Vec<serde_json::Value>) -> String {
        serde_json::json!({
            "format": "nexora-conversation",
            "version": 1,
            "conversation": conversation,
            "messages": messages,
        })
        .to_string()
    }

    /// A valid conversation record for the given exported id.
    fn conversation(id: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": "Planning",
            "status": "active",
            "created_at": 1000,
            "updated_at": 1000,
        })
    }

    /// A message record for the given conversation id. The exported message
    /// `id` is included (as an export would) but is ignored by import.
    #[allow(clippy::too_many_arguments)]
    fn message(
        conversation_id: i64,
        id: i64,
        role: &str,
        content: &str,
        provider_id: Option<i64>,
        model_name: Option<&str>,
        created_at: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "conversation_id": conversation_id,
            "role": role,
            "content": content,
            "provider_id": provider_id,
            "model_name": model_name,
            "created_at": created_at,
        })
    }

    /// Read a conversation by id, panicking if absent.
    fn read_conversation(db: &Database, id: i64) -> Conversation {
        ConversationRepository::new(db)
            .read(id)
            .expect("read conversation")
            .expect("conversation exists")
    }

    #[test]
    fn import_creates_new_conversation_and_messages_with_new_ids() {
        let db = test_db();
        // An existing local provider that one imported message references.
        let provider_id = {
            let conn = db.lock().expect("lock connection");
            conn.execute(
                "INSERT INTO providers (name, display_name) VALUES ('openai', 'OpenAI')",
                [],
            )
            .expect("insert provider");
            conn.last_insert_rowid()
        };
        let exported_conversation_id = 999;
        let json = doc(
            conversation(exported_conversation_id),
            vec![
                message(exported_conversation_id, 11, "user", "hello", None, None, 1),
                message(
                    exported_conversation_id,
                    12,
                    "assistant",
                    "hi there",
                    Some(provider_id),
                    Some("gpt-4o-mini"),
                    2,
                ),
            ],
        );
        let service = ImportService::new(&db);

        let new_id = service.import(&json).expect("import succeeds");

        // A brand-new conversation id, never the exported one.
        assert!(new_id > 0);
        assert_ne!(new_id, exported_conversation_id);

        // Conversation metadata and timestamps are preserved.
        let imported = read_conversation(&db, new_id);
        assert_eq!(imported.title, "Planning");
        assert_eq!(imported.status, "active");
        assert_eq!(imported.created_at, 1000);
        assert_eq!(imported.updated_at, 1000);

        // Messages get new ids under the new conversation, with role/content
        // preserved and (for the assistant) provider/model preserved.
        let messages = MessageRepository::new(&db)
            .list_by_conversation(new_id)
            .expect("list messages");
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|m| m.id > 0));
        assert_ne!(messages[0].id, 11);
        assert_ne!(messages[1].id, 12);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert!(messages[0].provider_id.is_none());
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "hi there");
        assert_eq!(messages[1].provider_id, Some(provider_id));
        assert_eq!(messages[1].model_name.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn import_preserves_message_order() {
        let db = test_db();
        let id = 7;
        let json = doc(
            conversation(id),
            vec![
                message(id, 1, "user", "first", None, None, 10),
                message(id, 2, "assistant", "second", None, None, 20),
                message(id, 3, "user", "third", None, None, 30),
            ],
        );
        let service = ImportService::new(&db);

        let new_id = service.import(&json).expect("import succeeds");
        let contents: Vec<String> = MessageRepository::new(&db)
            .list_by_conversation(new_id)
            .expect("list messages")
            .into_iter()
            .map(|m| m.content)
            .collect();
        // Order matches the JSON `messages` array exactly.
        assert_eq!(contents, vec!["first", "second", "third"]);
    }

    #[test]
    fn import_nulls_provider_reference_without_a_local_provider() {
        let db = test_db();
        // Referenced provider does not exist locally.
        let id = 3;
        let json = doc(
            conversation(id),
            vec![
                message(id, 1, "user", "hello", None, None, 1),
                message(id, 2, "assistant", "hi", Some(424_242), Some("gpt-x"), 2),
            ],
        );
        let service = ImportService::new(&db);

        let new_id = service.import(&json).expect("import succeeds");
        let messages = MessageRepository::new(&db)
            .list_by_conversation(new_id)
            .expect("list messages");
        // Unavailable reference becomes NULL; model name is preserved.
        assert_eq!(messages[0].provider_id, None);
        assert_eq!(messages[1].provider_id, None);
        assert_eq!(messages[1].model_name.as_deref(), Some("gpt-x"));
    }

    #[test]
    fn import_of_empty_conversation_creates_a_conversation_without_messages() {
        let db = test_db();
        let json = doc(conversation(1), vec![]);
        let service = ImportService::new(&db);

        let new_id = service.import(&json).expect("import succeeds");
        assert_eq!(
            MessageRepository::new(&db)
                .list_by_conversation(new_id)
                .expect("list messages")
                .len(),
            0
        );
        assert_eq!(read_conversation(&db, new_id).title, "Planning");
    }

    #[test]
    fn import_leaves_existing_conversations_and_messages_unchanged() {
        let db = test_db();
        let conversations = ConversationRepository::new(&db);
        let messages = MessageRepository::new(&db);
        let existing_id = conversations
            .create("Existing", "active")
            .expect("create existing conversation");
        messages
            .create(existing_id, "user", "keep me", None, None)
            .expect("create existing message");

        // Snapshot the existing rows.
        let before_conversation = conversations
            .read(existing_id)
            .expect("read")
            .expect("exists");
        let before_messages = messages.list_by_conversation(existing_id).expect("list");

        let id = 5;
        let json = doc(
            conversation(id),
            vec![message(id, 1, "user", "imported", None, None, 1)],
        );
        let service = ImportService::new(&db);
        let new_id = service.import(&json).expect("import succeeds");
        assert_ne!(new_id, existing_id);

        // The existing conversation and its message are unchanged.
        let after_conversation = conversations
            .read(existing_id)
            .expect("read")
            .expect("exists");
        let after_messages = messages.list_by_conversation(existing_id).expect("list");
        assert_eq!(before_conversation, after_conversation);
        assert_eq!(before_messages, after_messages);
    }
    /// Count rows in `conversations`, used to assert an import wrote nothing.
    fn conversation_count(db: &Database) -> i64 {
        let conn = db.lock().expect("lock connection");
        conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
            .expect("count conversations")
    }

    #[test]
    fn invalid_json_is_invalid_json_and_writes_nothing() {
        let db = test_db();
        let service = ImportService::new(&db);
        let err = service.import("{ not json").expect_err("malformed JSON");
        assert!(matches!(err, ImportError::InvalidJson(_)));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn wrong_format_is_unsupported_format_and_writes_nothing() {
        let db = test_db();
        let service = ImportService::new(&db);
        let json = serde_json::json!({
            "format": "some-other-format",
            "version": 1,
            "conversation": conversation(1),
            "messages": [],
        })
        .to_string();
        let err = service.import(&json).expect_err("unsupported format");
        assert!(matches!(
            err,
            ImportError::UnsupportedFormat { ref format } if format == "some-other-format"
        ));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn unsupported_version_is_unsupported_version_and_writes_nothing() {
        let db = test_db();
        let service = ImportService::new(&db);
        let json = serde_json::json!({
            "format": "nexora-conversation",
            "version": 2,
            "conversation": conversation(1),
            "messages": [],
        })
        .to_string();
        let err = service.import(&json).expect_err("unsupported version");
        assert!(matches!(
            err,
            ImportError::UnsupportedVersion { version: 2 }
        ));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn invalid_message_role_is_invalid_data_and_writes_nothing() {
        let db = test_db();
        let service = ImportService::new(&db);
        let id = 1;
        let json = doc(
            conversation(id),
            vec![message(id, 1, "system", "nope", None, None, 1)],
        );
        let err = service.import(&json).expect_err("invalid role");
        assert!(matches!(err, ImportError::InvalidData(_)));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn message_conversation_id_mismatch_is_invalid_data() {
        let db = test_db();
        let service = ImportService::new(&db);
        let id = 1;
        // The message belongs to a different exported conversation.
        let json = doc(
            conversation(id),
            vec![message(999, 1, "user", "hi", None, None, 1)],
        );
        let err = service.import(&json).expect_err("relationship mismatch");
        assert!(matches!(
            err,
            ImportError::InvalidData(reason) if reason.contains("conversation_id")
        ));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn missing_required_field_is_invalid_data_and_writes_nothing() {
        let db = test_db();
        let service = ImportService::new(&db);
        // A well-formed document missing the required conversation `title`.
        let json = serde_json::json!({
            "format": "nexora-conversation",
            "version": 1,
            "conversation": {
                "id": 1,
                "status": "active",
                "created_at": 1000,
                "updated_at": 1000,
            },
            "messages": [],
        })
        .to_string();
        let err = service.import(&json).expect_err("missing field");
        assert!(matches!(err, ImportError::InvalidData(_)));
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn database_failure_rolls_back_the_entire_import() {
        // A stricter `content` CHECK lets a valid-looking document trip the
        // database mid-way, after the conversation INSERT already happened.
        let db = test_db_with_content_check("length(content) > 0 AND length(content) <= 16");
        let service = ImportService::new(&db);
        let id = 1;
        let json = doc(
            conversation(id),
            vec![
                message(id, 1, "user", "ok", None, None, 1),
                message(
                    id,
                    2,
                    "assistant",
                    "this content is far too long to fit",
                    None,
                    None,
                    2,
                ),
            ],
        );

        let err = service.import(&json).expect_err("second insert fails");
        assert!(matches!(err, ImportError::Database(_)));

        // No conversation (or orphaned first message) survived the rollback.
        assert_eq!(conversation_count(&db), 0);
        let conn = db.lock().expect("lock connection");
        let message_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .expect("count messages");
        assert_eq!(message_count, 0);
    }
}
