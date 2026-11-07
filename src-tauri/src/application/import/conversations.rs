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
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::application::agent::injection::contains_secret;
use crate::application::export::{ALLOWED_EXPORT_EXTENSIONS, EXPORT_FORMAT, EXPORT_VERSION};
use crate::application::workspace::{
    is_drive_root, is_system_file_path, is_unc_path, strip_verbatim, WORKSPACE_ROOT_MAX_LEN,
};
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::MessageRepository;
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::Repository;

use super::{classify_error, ensure_valid, ImportError, Result, MAX_IMPORT_JSON_BYTES};

/// Allowed clock skew for imported timestamps (seconds). Imported
/// `created_at` / `updated_at` values beyond `now + skew` are rejected:
/// without an upper bound a future-dated import permanently bricks the
/// conversation — every later write stamps `updated_at` with the (smaller)
/// current time and violates the `updated_at >= created_at` CHECK
/// (NEX-DB-001). The 300 s budget tolerates honest clock drift between the
/// exporting and importing machines.
pub(crate) const IMPORT_FUTURE_SKEW_SECS: i64 = 300;

/// Current time as Unix seconds, matching the `unixepoch()` the schema
/// defaults and touch triggers use.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Upper bound for an honestly-produced imported timestamp: now plus the
/// future-skew budget (`IMPORT_FUTURE_SKEW_SECS`), saturating on overflow.
fn import_timestamp_upper_bound() -> i64 {
    now_secs().saturating_add(IMPORT_FUTURE_SKEW_SECS)
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
            "conversation 'status' must be 'active' or 'archived'",
        )?;
        ensure_valid(
            conversation.created_at > 0,
            "conversation 'created_at' must be a positive integer",
        )?;
        ensure_valid(
            conversation.updated_at >= conversation.created_at,
            "conversation 'updated_at' must not be earlier than 'created_at'",
        )?;
        // NEX-DB-001: no unbounded future timestamps. A future-dated import
        // would brick the conversation (every later `updated_at` write stamps
        // the smaller current time and violates the schema CHECK), so reject
        // honestly-impossible timestamps with a secret-free error.
        let upper_bound = import_timestamp_upper_bound();
        ensure_valid(
            conversation.created_at <= upper_bound,
            "conversation 'created_at' must not be in the future",
        )?;
        ensure_valid(
            conversation.updated_at <= upper_bound,
            "conversation 'updated_at' must not be in the future",
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
            "{} must equal the conversation id",
            field("conversation_id")
        ),
    )?;
    ensure_valid(
        message.role == "user" || message.role == "assistant",
        format!("{} role must be 'user' or 'assistant'", field("role")),
    )?;
    ensure_valid(
        !message.content.is_empty(),
        format!("{} content must be non-empty", field("content")),
    )?;
    if let Some(provider_id) = message.provider_id {
        ensure_valid(
            provider_id > 0,
            format!(
                "{} provider_id must be positive or null",
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
    )?;
    // Same future bound as the conversation timestamps (NEX-DB-001):
    // unbounded message timestamps carry the same bricking potential.
    ensure_valid(
        message.created_at <= import_timestamp_upper_bound(),
        format!(
            "{} created_at must not be in the future",
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

// ---------------------------------------------------------------------------
// Backend-side document read (SEC-01)
// ---------------------------------------------------------------------------

/// Read the conversation import document at `raw_path` backend-side (SEC-01).
///
/// The renderer no longer reads picked files through the `fs` plugin — it
/// hands the dialog-picked absolute path to `import_conversation` and this
/// module reads the bytes with the same containment discipline as the export
/// write side
/// (`crate::application::export::validate_export_path`): fixed-vocabulary
/// refusal before any read for empty / null-byte / overlong / non-`.json` /
/// UNC / protected-system / drive-root paths, then canonicalization (which
/// doubles as the existence check), a regular-file check, and a metadata
/// size cap ([`MAX_IMPORT_JSON_BYTES`]) enforced **before** the read, so an
/// oversized document is never pulled into memory. The canonical form is
/// what is read, so a symlinked path resolves to its target first. The
/// post-write swap backstop of the export side guards a write; a read's
/// content is fully re-validated by [`ImportService::import`] regardless, so
/// no extra post-read check is needed.
///
/// Errors stay fixed vocabulary and secret-free: the picked path is never
/// echoed.
///
/// # Errors
///
/// Returns [`ImportError::InvalidData`] for every rejected path or failed
/// read.
pub(crate) fn read_import_document(raw_path: &str) -> Result<String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(ImportError::InvalidData(
            "import path must not be empty".to_string(),
        ));
    }
    if trimmed.contains('\0') {
        return Err(ImportError::InvalidData(
            "import path contains a null byte".to_string(),
        ));
    }
    if trimmed.len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(ImportError::InvalidData(
            "import path exceeds the length limit".to_string(),
        ));
    }
    let path = Path::new(trimmed);
    // The import document is the read twin of the `.json`-only export
    // artifact: the same allowlist applies.
    let allowed = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ALLOWED_EXPORT_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        });
    if !allowed {
        return Err(ImportError::InvalidData(
            "only .json import documents are allowed".to_string(),
        ));
    }
    if is_unc_path(path) {
        return Err(ImportError::InvalidData(
            "UNC network paths are not allowed".to_string(),
        ));
    }
    if is_system_file_path(path) {
        return Err(ImportError::InvalidData(
            "a protected system location is not allowed".to_string(),
        ));
    }
    if is_drive_root(path) {
        return Err(ImportError::InvalidData(
            "a drive root is not allowed".to_string(),
        ));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| ImportError::InvalidData("the import document does not exist".to_string()))?;
    let canonical = strip_verbatim(canonical);
    if is_unc_path(&canonical) {
        return Err(ImportError::InvalidData(
            "UNC network paths are not allowed".to_string(),
        ));
    }
    if !canonical.is_file() {
        return Err(ImportError::InvalidData(
            "the import document is not a regular file".to_string(),
        ));
    }
    let size = std::fs::metadata(&canonical)
        .map_err(|_| ImportError::InvalidData("the import document could not be read".to_string()))?
        .len();
    if usize::try_from(size).unwrap_or(usize::MAX) > MAX_IMPORT_JSON_BYTES {
        return Err(ImportError::InvalidData(
            "import document exceeds the 16 MiB size limit".to_string(),
        ));
    }
    std::fs::read_to_string(&canonical)
        .map_err(|_| ImportError::InvalidData("the import document could not be read".to_string()))
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
    /// constraints, is oversized, or carries key-like material, or
    /// [`ImportError::Database`] when a read or the
    /// transactional insert fails.
    pub(crate) fn import(&self, json: &str) -> Result<i64> {
        // WS-C.2 import gates, before any parse or write: the byte cap bounds
        // memory against a hostile document, and the secrets scan denies
        // key-like material with a secret-free error (the predicate returns
        // only `bool`, so a detected secret can never echo).
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        ensure_valid(
            !contains_secret(json),
            "import document contains key-like material",
        )?;
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

    /// Import a conversation from the JSON document at `raw_path`
    /// (FR-011; SEC-01): the document is read backend-side by
    /// [`read_import_document`] (extension allowlist, path containment, size
    /// cap) and then runs through the same decode / validate / transactional
    /// insert flow as [`Self::import`].
    ///
    /// Returns the new conversation's id.
    ///
    /// # Errors
    ///
    /// Returns every [`ImportError`] of [`read_import_document`] and
    /// [`Self::import`].
    pub(crate) fn import_from_file(&self, raw_path: &str) -> Result<i64> {
        let json = read_import_document(raw_path)?;
        self.import(&json)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;
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
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at),
                  workspace_root TEXT CHECK(workspace_root IS NULL OR length(workspace_root) <= 1024)
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
    fn validation_reasons_never_echo_document_values() {
        // NEX-SEC-011: validation reasons carry structural labels only. A
        // hostile (but not key-like, so past the secrets gate) status, role,
        // or relationship value must not survive in the reason text, and the
        // serde-typed failures must render fixed vocabulary too.
        let db = test_db();
        let service = ImportService::new(&db);
        let hostile = "archived'; DROP TABLE conversations; --";
        let mut bad_status = conversation(1);
        bad_status["status"] = serde_json::json!(hostile);
        let err = service
            .import(&doc(
                bad_status,
                vec![message(1, 1, "user", "hi", None, None, 1)],
            ))
            .expect_err("bad status");
        match err {
            ImportError::InvalidData(reason) => {
                assert!(
                    !reason.contains(hostile),
                    "status value must not echo, got {reason:?}"
                );
                assert_eq!(
                    reason,
                    "conversation 'status' must be 'active' or 'archived'"
                );
            }
            other => panic!("expected InvalidData, got {other:?}"),
        }

        let hostile_role = "admin'; DROP TABLE messages; --";
        let err = service
            .import(&doc(
                conversation(1),
                vec![message(1, 1, hostile_role, "hi", None, None, 1)],
            ))
            .expect_err("bad role");
        match err {
            ImportError::InvalidData(reason) => {
                assert!(
                    !reason.contains(hostile_role),
                    "role value must not echo, got {reason:?}"
                );
                assert!(
                    reason.contains("messages[0].role"),
                    "the structural label stays, got {reason:?}"
                );
            }
            other => panic!("expected InvalidData, got {other:?}"),
        }

        // Wrongly-typed fields (serde `Data` errors, which quote content)
        // map to fixed vocabulary.
        let mistyped = serde_json::json!({
            "format": "nexora-conversation",
            "version": 1,
            "conversation": conversation(1),
            "messages": [],
        })
        .to_string()
        .replace("\"version\":1", "\"version\":\"one\"");
        let err = service.import(&mistyped).expect_err("mistyped version");
        match err {
            ImportError::InvalidData(reason) => {
                assert!(!reason.contains("one"), "got {reason:?}");
                assert_eq!(
                    reason,
                    "import document has an invalid field type or structure"
                );
            }
            other => panic!("expected InvalidData, got {other:?}"),
        }

        // Display paths stay fixed-vocabulary too: neither the serde detail
        // nor the document's format value renders.
        let rendered = format!(
            "{}",
            ImportError::UnsupportedFormat {
                format: hostile.to_string(),
            }
        );
        assert!(!rendered.contains(hostile), "got {rendered:?}");
        let rendered = format!(
            "{}",
            ImportError::InvalidJson(
                serde_json::from_str::<serde_json::Value>("{oops").expect_err("malformed")
            )
        );
        assert!(!rendered.contains("oops"), "got {rendered:?}");
        assert_eq!(conversation_count(&db), 0, "denied documents write nothing");
    }

    #[test]
    fn frontend_capability_has_no_home_read_scope_nor_opener() {
        // SEC-01 (NEX-SEC-012 closed): the `fs` plugin carries **no read
        // scope at all**. The former `$HOME` / `$HOME/**/*` pair
        // (`fs:allow-read-text-file` + `fs:scope`) is gone — the import
        // document is read backend-side (`read_import_document`) and the
        // attachment size is derived server-side, so no renderer JS reads
        // the home tree through the `fs` plugin. Only `fs:allow-stat`
        // remains (a stat needs no path scope beyond the picked file).
        // SEC-08: the `opener` grant is gone too, so renderer JS cannot ask
        // the OS to open URLs or handlers.
        const CAPABILITY: &str = include_str!("../../../capabilities/default.json");
        let document: serde_json::Value =
            serde_json::from_str(CAPABILITY).expect("capability is valid JSON");
        let permissions = document
            .get("permissions")
            .and_then(serde_json::Value::as_array)
            .expect("permissions array present");
        for forbidden in [
            "fs:allow-read-text-file",
            "fs:scope",
            "fs:allow-read",
            "opener:default",
        ] {
            assert!(
                !permissions.iter().any(|entry| entry == forbidden
                    || entry.get("identifier") == Some(&serde_json::json!(forbidden))),
                "the {forbidden} grant must be gone"
            );
        }
        // No `$HOME` scope survives anywhere in the capability file.
        let rendered = serde_json::to_string(&document).expect("serialize capability");
        assert!(
            !rendered.contains("$HOME"),
            "no home-tree scope may remain in the capability file: {rendered:?}"
        );
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

    /// A `conversation` record carrying explicit timestamps (unlike the
    /// fixed-timestamp [`conversation`] helper), for the future-bound tests.
    fn conversation_with_timestamps(
        id: i64,
        created_at: i64,
        updated_at: i64,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": "Planning",
            "status": "active",
            "created_at": created_at,
            "updated_at": updated_at,
        })
    }

    #[test]
    fn far_future_conversation_timestamps_are_rejected_without_writes() {
        // NEX-DB-001: an import carrying timestamps beyond `now + skew`
        // would brick the conversation (every later `updated_at` write stamps
        // the smaller current time and violates the schema CHECK), so it is
        // rejected up front with a secret-free error and no writes.
        let db = test_db();
        let service = ImportService::new(&db);
        let id = 1;
        let future = now_secs() + IMPORT_FUTURE_SKEW_SECS + 3600;
        let json = doc(
            conversation_with_timestamps(id, future, future),
            vec![message(id, 1, "user", "hi", None, None, 1)],
        );
        let err = service
            .import(&json)
            .expect_err("future timestamps rejected");
        assert!(
            matches!(
                err,
                ImportError::InvalidData(ref reason) if reason.contains("future")
            ),
            "unexpected error: {err:?}"
        );
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn far_future_message_timestamp_is_rejected_without_writes() {
        let db = test_db();
        let service = ImportService::new(&db);
        let id = 1;
        let future = now_secs() + IMPORT_FUTURE_SKEW_SECS + 3600;
        let json = doc(
            conversation(id),
            vec![message(id, 1, "user", "hi", None, None, future)],
        );
        let err = service.import(&json).expect_err("future message rejected");
        assert!(
            matches!(
                err,
                ImportError::InvalidData(ref reason) if reason.contains("future")
            ),
            "unexpected error: {err:?}"
        );
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn within_skew_future_import_stays_renamable_and_touchable() {
        // NEX-DB-001 acceptance on the production schema: an import carrying
        // honest near-future timestamps (inside the skew budget) must remain
        // renamable (trigger path) and send-touchable (explicit-write path) —
        // the monotonic `updated_at` writes keep the CHECK satisfiable.
        let db = crate::infrastructure::database::in_memory_database();
        let service = ImportService::new(&db);
        let id = 1;
        let near_future = now_secs() + 100;
        let json = doc(
            conversation_with_timestamps(id, near_future, near_future),
            vec![message(id, 1, "user", "hi", None, None, near_future)],
        );
        let new_id = service.import(&json).expect("within-skew import succeeds");

        ConversationRepository::new(&db)
            .update(new_id, "Renamed", "active")
            .expect("rename after near-future import succeeds");
        ConversationRepository::new(&db)
            .transaction(|tx| ConversationRepository::touch_updated_at(tx, new_id))
            .expect("send touch after near-future import succeeds");

        let imported = read_conversation(&db, new_id);
        assert_eq!(imported.title, "Renamed");
        assert!(
            imported.updated_at >= imported.created_at,
            "updated_at must stay monotonic: {imported:?}"
        );
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

    #[test]
    fn oversize_document_is_denied_before_any_write() {
        let db = test_db();
        let service = ImportService::new(&db);
        // A valid-shaped document padded past the pinned byte cap.
        let id = 1;
        let padding = "x".repeat(MAX_IMPORT_JSON_BYTES);
        let json = doc(
            conversation(id),
            vec![message(id, 1, "user", &padding, None, None, 1)],
        );
        assert!(
            json.len() > MAX_IMPORT_JSON_BYTES,
            "padded document must exceed the cap"
        );
        let err = service.import(&json).expect_err("oversize must be denied");
        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("16 MiB")),
            "oversize denial must name the pinned cap, got {err:?}"
        );
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn secret_bearing_document_is_denied_without_echo() {
        let db = test_db();
        let service = ImportService::new(&db);
        let id = 1;
        let json = doc(
            conversation(id),
            vec![message(
                id,
                1,
                "user",
                "my token is sk-live-sentinel-42 keep it safe",
                None,
                None,
                1,
            )],
        );
        let err = service.import(&json).expect_err("secret must be denied");
        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("key-like")),
            "secret denial must use fixed vocabulary, got {err:?}"
        );
        // Secret-free by construction: the fixed reason never echoes input.
        let rendered = format!("{err}");
        for sentinel in [
            "sk-live-sentinel-42",
            "sk-",
            "secret",
            "credential",
            "api_key",
        ] {
            assert!(
                !rendered.to_lowercase().contains(sentinel),
                "denial must not echo hostile material {sentinel:?}: {rendered:?}"
            );
        }
        assert_eq!(conversation_count(&db), 0);
    }

    #[test]
    fn import_size_cap_matches_the_pinned_precedent() {
        assert_eq!(MAX_IMPORT_JSON_BYTES, 16 * 1024 * 1024);
    }

    // -----------------------------------------------------------------------
    // Backend-side document read (SEC-01)
    // -----------------------------------------------------------------------
    //
    // Moved from `setup_import_tests` in the AC-207 split: these exercise
    // `read_import_document` / `ImportService::import_from_file`, which live
    // in the conversation half. The one `setup_db()` call became
    // `in_memory_database()` (its exact body) since that helper stayed with
    // the setup half.

    /// Write a unique temporary `.json` import document carrying `contents`
    /// and return its path. The read path canonicalizes, so callers compare
    /// outcomes, not raw paths.
    fn temp_json_document(contents: &str) -> std::path::PathBuf {
        static DOCS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = DOCS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nexora-import-doc-{}-{nanos}-{id}.json",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("write temp import document");
        path
    }

    #[test]
    fn import_from_file_imports_a_real_backend_read_document() {
        let db = in_memory_database();
        let id = 11;
        let json = serde_json::json!({
            "format": "nexora-conversation",
            "version": 1,
            "conversation": {
                "id": id,
                "title": "Planning",
                "status": "active",
                "created_at": 1000,
                "updated_at": 1000,
            },
            "messages": [{
                "id": 1,
                "conversation_id": id,
                "role": "user",
                "content": "read backend-side",
                "provider_id": null,
                "model_name": null,
                "created_at": 1,
            }],
        })
        .to_string();
        let path = temp_json_document(&json);

        let new_id = ImportService::new(&db)
            .import_from_file(path.to_string_lossy().as_ref())
            .expect("backend-side import succeeds");
        let contents: Vec<String> = MessageRepository::new(&db)
            .list_by_conversation(new_id)
            .expect("list messages")
            .into_iter()
            .map(|m| m.content)
            .collect();
        assert_eq!(contents, vec!["read backend-side"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn import_read_rejects_non_json_paths_without_echoing_them() {
        // Extension allowlist fires before any read.
        let text_path = {
            let path = temp_json_document("{}");
            let renamed = path.with_extension("txt");
            std::fs::rename(&path, &renamed).expect("rename to .txt");
            renamed
        };
        let err = read_import_document(text_path.to_string_lossy().as_ref())
            .expect_err("non-json must be refused");
        match err {
            ImportError::InvalidData(reason) => {
                assert_eq!(reason, "only .json import documents are allowed");
                assert!(
                    !reason.contains("nexora-import-doc"),
                    "the picked path must never echo: {reason:?}"
                );
            }
            other => panic!("expected InvalidData, got {other:?}"),
        }
        let _ = std::fs::remove_file(&text_path);

        // Empty and missing paths refuse with fixed vocabulary too.
        let err = read_import_document("").expect_err("empty path");
        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason == "import path must not be empty")
        );
        let missing = std::env::temp_dir().join("nexora-import-missing-9f3c2a1e.json");
        let _ = std::fs::remove_file(&missing);
        let err =
            read_import_document(missing.to_string_lossy().as_ref()).expect_err("missing document");
        assert!(matches!(
            err,
            ImportError::InvalidData(ref reason) if reason == "the import document does not exist"
        ));
        assert!(
            !format!("{err}").contains("nexora-import-missing"),
            "the missing path must never echo: {err:?}"
        );
    }

    #[test]
    fn import_read_enforces_the_size_cap_before_reading() {
        // Sparse file: the logical size trips the metadata gate without
        // writing 16 MiB — and without ever reading the bytes.
        let path = temp_json_document("{}");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open sparse document");
        file.set_len(u64::try_from(MAX_IMPORT_JSON_BYTES + 1).expect("cap fits"))
            .expect("sparse size");
        drop(file);

        let err = read_import_document(path.to_string_lossy().as_ref())
            .expect_err("oversized document must be refused");
        assert!(matches!(
            err,
            ImportError::InvalidData(ref reason) if reason == "import document exceeds the 16 MiB size limit"
        ));
        let _ = std::fs::remove_file(&path);
    }
}
