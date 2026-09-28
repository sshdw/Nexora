//! Conversation export service: application-layer orchestration for FR-010
//! (ROADMAP.md Phase 8.1 — Export; ARCHITECTURE.md §5, §15; DATABASE.md §16).
//!
//! Exports a single conversation and its messages into a self-describing JSON
//! document, using the existing [`ConversationRepository`] and
//! [`MessageRepository`]. The service adds no schema, no SQL, and no database
//! write access of its own: all data access is delegated to the existing
//! repositories' read methods.
//!
//! # Export format
//!
//! The export document is JSON ([`ConversationExport`]) with a fixed
//! `format` marker and `version` so a consumer (for example the Phase 8.2
//! import task) can recognize it without guessing:
//!
//! ```json
//! {
//!   "format": "nexora-conversation",
//!   "version": 1,
//!   "conversation": { ... },
//!   "messages": [ { ... }, ... ]
//! }
//! ```
//!
//! # Read-only guarantee
//!
//! Export performs read-only access to stored data (FR-010; DATABASE.md §16):
//! only the repositories' `SELECT`-based reads are used
//! ([`ConversationRepository::read`], [`MessageRepository::list_by_conversation`]);
//! no repository write method is ever called, so the persisted conversation
//! history is never modified. Message order is preserved exactly as
//! persisted: messages are read via
//! [`MessageRepository::list_by_conversation`], which orders by `created_at`
//! ascending (DATABASE.md §7.2), and are emitted in that order without any
//! re-sorting.
//!
//! # Error handling
//!
//! All failure modes are classified by [`ExportError`]: a missing conversation
//! is [`ExportError::NotFound`], persistence failures are
//! [`ExportError::Database`], serialization failures are
//! [`ExportError::Serialization`], rejected destinations are
//! [`ExportError::InvalidPath`], and file-write failures are
//! [`ExportError::Io`]. No error variant carries a credential or other secret
//! value (ARCHITECTURE.md §9, §11).

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::workspace::{
    is_drive_root, is_system_file_path, is_unc_path, strip_verbatim, WORKSPACE_ROOT_MAX_LEN,
};
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::conversations::{Conversation, ConversationRepository};
use crate::infrastructure::repository::messages::{Message, MessageRepository};

/// Application-layer result shared by export operations, unifying
/// retrieval, serialization, and file-write failures.
pub(crate) type Result<T> = std::result::Result<T, ExportError>;

/// Value of the `format` field written to every exported document, marking it
/// as a Nexora conversation export (recognizable by Phase 8.2 import).
pub(crate) const EXPORT_FORMAT: &str = "nexora-conversation";

/// Version of the export document layout written by this build.
pub(crate) const EXPORT_VERSION: i64 = 1;

/// File extensions accepted for export artifacts (WS-C.2 import/export
/// allowlist): conversation documents are JSON only. Compared
/// case-insensitively against [`Path::extension`].
pub(crate) const ALLOWED_EXPORT_EXTENSIONS: &[&str] = &["json"];

/// A `conversations` row in exported form.
///
/// Mirrors the persisted columns defined by DATABASE.md §7.1 so the exported
/// document carries the same record the application holds; every value is
/// copied unchanged from the repository record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExportedConversation {
    /// Surrogate primary key (`id`).
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

/// A `messages` row in exported form.
///
/// Mirrors the persisted columns defined by DATABASE.md §7.2. `provider_id`
/// is the provider reference recorded on the message (the foreign key into
/// `providers`), and `model_name` is the model recorded at send time; both are
/// preserved without modification, including their `None` / `Some` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExportedMessage {
    /// Surrogate primary key (`id`).
    pub id: i64,
    /// Owning conversation foreign key (`conversation_id`).
    pub conversation_id: i64,
    /// Message author type (`role`).
    pub role: String,
    /// Message text (`content`).
    pub content: String,
    /// AI provider used (`provider_id`), `None` when not set.
    pub provider_id: Option<i64>,
    /// Specific model used (`model_name`), `None` when not set.
    pub model_name: Option<String>,
    /// Creation timestamp (`created_at`).
    pub created_at: i64,
}

impl ExportedMessage {
    fn from_record(message: &Message) -> Self {
        Self {
            id: message.id,
            conversation_id: message.conversation_id,
            role: message.role.clone(),
            content: message.content.clone(),
            provider_id: message.provider_id,
            model_name: message.model_name.clone(),
            created_at: message.created_at,
        }
    }
}

/// The complete JSON document produced by a conversation export (FR-010).
///
/// The `messages` array preserves the persisted order exactly: it is emitted
/// in the order returned by [`MessageRepository::list_by_conversation`]
/// (`created_at` ascending, DATABASE.md §7.2) and is never re-sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConversationExport {
    /// Fixed marker identifying the document kind ([`EXPORT_FORMAT`]).
    pub format: String,
    /// Layout version of this document ([`EXPORT_VERSION`]).
    pub version: i64,
    /// The exported conversation record.
    pub conversation: ExportedConversation,
    /// The conversation's messages in persisted order.
    pub messages: Vec<ExportedMessage>,
}

impl ConversationExport {
    fn from_records(conversation: &Conversation, messages: &[Message]) -> Self {
        Self {
            format: EXPORT_FORMAT.to_string(),
            version: EXPORT_VERSION,
            conversation: ExportedConversation {
                id: conversation.id,
                title: conversation.title.clone(),
                status: conversation.status.clone(),
                created_at: conversation.created_at,
                updated_at: conversation.updated_at,
            },
            messages: messages.iter().map(ExportedMessage::from_record).collect(),
        }
    }
}

/// Application-layer service that exports a single conversation to a JSON
/// document (FR-010).
///
/// Composes [`ConversationRepository`] and [`MessageRepository`] for
/// read-only retrieval, mirroring how the conversation service composes the
/// same repositories. It is deliberately focused on orchestration and
/// serialization; persistence behavior and schema constraints remain in the
/// repositories and the database.
pub(crate) struct ExportService<'a> {
    conversations: ConversationRepository<'a>,
    messages: MessageRepository<'a>,
}

impl<'a> ExportService<'a> {
    /// Create an export service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            conversations: ConversationRepository::new(db),
            messages: MessageRepository::new(db),
        }
    }

    /// Build the JSON document for `conversation_id` (FR-010).
    ///
    /// Returns the pretty-printed serialized document as a string without
    /// touching the filesystem, so the payload can be inspected or forwarded.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::NotFound`] when no conversation with
    /// `conversation_id` exists, [`ExportError::Database`] when a read fails,
    /// or [`ExportError::Serialization`] when the document cannot be
    /// serialized.
    pub(crate) fn serialize(&self, conversation_id: i64) -> Result<String> {
        let export = self.build(conversation_id)?;
        serde_json::to_string_pretty(&export).map_err(ExportError::Serialization)
    }

    /// Export `conversation_id` to the JSON file at `path` (FR-010).
    ///
    /// The document is fully materialized in memory before any file access, so
    /// a failed write is reported cleanly as [`ExportError::Io`]; the database
    /// is only read. The destination is validated through
    /// [`validate_export_path`] (WS-C.2: the #64 prefix-walk + post-create
    /// backstop pattern, with the chosen directory as its own root) before
    /// the write.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::NotFound`] when no conversation with
    /// `conversation_id` exists, [`ExportError::Database`] when a read fails,
    /// [`ExportError::Serialization`] when the document cannot be serialized,
    /// [`ExportError::InvalidPath`] when the destination is rejected, or
    /// [`ExportError::Io`] when the file cannot be written.
    pub(crate) fn export_to_file(&self, conversation_id: i64, path: &Path) -> Result<()> {
        let json = self.serialize(conversation_id)?;
        let target = validate_export_path(path)?;
        std::fs::write(&target, json.as_bytes()).map_err(ExportError::Io)?;
        // Post-create backstop (#64 pattern): a link swapped in between the
        // pre-write checks and the write must not redirect the artifact. The
        // written file must canonicalize under its own parent; otherwise it
        // is removed best-effort and the write fails instead of succeeding
        // outside the chosen directory.
        if let Some(parent) = target.parent() {
            if let (Ok(canon_file), Ok(canon_parent)) =
                (target.canonicalize(), parent.canonicalize())
            {
                if !canon_file.starts_with(&canon_parent) {
                    let _ = std::fs::remove_file(&target);
                    return Err(ExportError::Io(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "export path escaped its parent directory",
                    )));
                }
            }
        }
        Ok(())
    }

    /// Read `conversation_id` and assemble its export record in persisted
    /// message order, without touching the filesystem.
    fn build(&self, conversation_id: i64) -> Result<ConversationExport> {
        let conversation =
            self.conversations
                .read(conversation_id)?
                .ok_or_else(|| ExportError::NotFound {
                    id: conversation_id,
                })?;
        let messages = self.messages.list_by_conversation(conversation_id)?;
        Ok(ConversationExport::from_records(&conversation, &messages))
    }
}

/// Classified errors raised by conversation export (FR-010).
///
/// No variant carries a credential or other secret value, so formatting an
/// [`ExportError`] never writes a secret to the logs (ARCHITECTURE.md §9,
/// §11).
#[derive(Debug)]
pub(crate) enum ExportError {
    /// No conversation with the referenced `id` exists.
    NotFound {
        /// The requested conversation id.
        id: i64,
    },
    /// A persistence failure from a repository.
    Database(DatabaseError),
    /// The export document could not be serialized to JSON.
    Serialization(serde_json::Error),
    /// The export destination was rejected before any write (WS-C.2).
    /// `reason` is a fixed category, never the offending path.
    InvalidPath {
        /// Secret-free failure category.
        reason: &'static str,
    },
    /// The exported file could not be written.
    Io(io::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { id } => write!(f, "conversation {id} does not exist"),
            Self::Database(err) => write!(f, "{err}"),
            Self::Serialization(err) => write!(f, "export serialization failed: {err}"),
            Self::InvalidPath { reason } => write!(f, "export path invalid: {reason}"),
            Self::Io(err) => write!(f, "export file write failed: {err}"),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound { .. } | Self::InvalidPath { .. } => None,
            Self::Database(err) => Some(err),
            Self::Serialization(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for ExportError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

/// Validate an export destination before any write (WS-C.2).
///
/// Composes with the #64 guards (reuse, not duplication): the extension
/// allowlist ([`ALLOWED_EXPORT_EXTENSIONS`]) fires first, then the lexical
/// UNC / system-directory / drive-root guards shared with the workspace
/// module, then the #64 prefix-walk over every existing ancestor
/// ([`check_export_prefixes`]) with the chosen directory as its own
/// containment root, and finally the final-component symlink rejection. The
/// post-create backstop lives in [`ExportService::export_to_file`].
/// A missing parent is *not* rejected here: it falls through to the write,
/// which fails as [`ExportError::Io`] exactly as before.
///
/// Every failure is [`ExportError::InvalidPath`] with a static reason that
/// never contains the path or file content.
fn validate_export_path(path: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    if text.trim().is_empty() {
        return Err(ExportError::InvalidPath {
            reason: "path must not be empty",
        });
    }
    if text.contains('\0') {
        return Err(ExportError::InvalidPath {
            reason: "path contains a null byte",
        });
    }
    if text.len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(ExportError::InvalidPath {
            reason: "path exceeds the length limit",
        });
    }
    let allowed = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ALLOWED_EXPORT_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        });
    if !allowed {
        return Err(ExportError::InvalidPath {
            reason: "only .json export files are allowed",
        });
    }
    if is_unc_path(path) {
        return Err(ExportError::InvalidPath {
            reason: "UNC network paths are not allowed",
        });
    }
    // Like attachments (and unlike the workspace root, which must never sit
    // under the user profile), a single artifact may live anywhere the user
    // picks — including Temp/Downloads — so the file guard (which excludes
    // the `C:\Users` tree) applies, not the workspace-root guard.
    if is_system_file_path(path) {
        return Err(ExportError::InvalidPath {
            reason: "a system directory is not allowed",
        });
    }
    if is_drive_root(path) {
        return Err(ExportError::InvalidPath {
            reason: "a drive root is not allowed",
        });
    }
    check_export_prefixes(path)?;
    // The final component itself may be a symlink pointing anywhere
    // (single-level escape): reject it before the write that would follow
    // the link. `exists` follows links and misses dangling ones, so the
    // metadata itself is inspected.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(ExportError::InvalidPath {
            reason: "path is a symbolic link",
        });
    }
    Ok(path.to_path_buf())
}

/// Prefix-walk over every existing ancestor of `target`, outermost-first
/// (#64 pattern: existing prefixes via `symlink_metadata`, so dangling links
/// count).
///
/// Each ancestor must canonicalize inside the previous one (chain
/// containment with the chosen directory as its own root) and must not
/// resolve to a UNC / system location — catching the depth>=2 escape where
/// an intermediate component is a link to the outside while deeper parents
/// do not exist yet. Relative paths with no on-disk ancestor skip the walk
/// (nothing to resolve through); the write itself still decides.
fn check_export_prefixes(target: &Path) -> Result<()> {
    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut cursor = target.parent();
    while let Some(dir) = cursor {
        if dir.as_os_str().is_empty() {
            break;
        }
        if std::fs::symlink_metadata(dir).is_ok() {
            ancestors.push(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    ancestors.reverse();
    let mut prev: Option<PathBuf> = None;
    for ancestor in &ancestors {
        let canon =
            strip_verbatim(
                ancestor
                    .canonicalize()
                    .map_err(|_| ExportError::InvalidPath {
                        reason: "a path component is not resolvable",
                    })?,
            );
        if is_unc_path(&canon) {
            return Err(ExportError::InvalidPath {
                reason: "UNC network paths are not allowed",
            });
        }
        if is_system_file_path(&canon) {
            return Err(ExportError::InvalidPath {
                reason: "a system directory is not allowed",
            });
        }
        if let Some(prev_canon) = &prev {
            if !canon.starts_with(prev_canon) {
                return Err(ExportError::InvalidPath {
                    reason: "a path component escapes its parent directory",
                });
            }
        }
        prev = Some(canon);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Build an in-memory database whose schema mirrors the documented
    /// `providers` / `conversations` / `messages` tables (DATABASE.md §7.1,
    /// §7.2, §7.5), the same tables the export reads from.
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
                 role TEXT NOT NULL,
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

    /// Insert a message directly into the test database with an explicit
    /// timestamp, returning its id.
    fn insert_message(
        conn: &Connection,
        conversation_id: i64,
        role: &str,
        content: &str,
        provider_id: Option<i64>,
        model_name: Option<&str>,
        created_at: i64,
    ) -> i64 {
        conn.execute(
            "INSERT INTO messages
                 (conversation_id, role, content, provider_id, model_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                conversation_id,
                role,
                content,
                provider_id,
                model_name,
                created_at
            ],
        )
        .expect("insert message");
        conn.last_insert_rowid()
    }

    /// Seed one provider and one conversation with a user turn and an
    /// assistant reply (which carries the provider reference and model name),
    /// returning `(conversation_id, provider_id, user_message_id, assistant_message_id)`.
    fn seeded_conversation(db: &Database) -> (i64, i64, i64, i64) {
        let conn = db.lock().expect("lock connection");
        conn.execute(
            "INSERT INTO providers (name, display_name) VALUES ('openai', 'OpenAI')",
            [],
        )
        .expect("insert provider");
        let provider_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO conversations (title) VALUES ('Planning')", [])
            .expect("insert conversation");
        let conversation_id = conn.last_insert_rowid();
        let user_message_id =
            insert_message(&conn, conversation_id, "user", "hello", None, None, 1);
        let assistant_message_id = insert_message(
            &conn,
            conversation_id,
            "assistant",
            "hi there",
            Some(provider_id),
            Some("gpt-4o-mini"),
            2,
        );
        (
            conversation_id,
            provider_id,
            user_message_id,
            assistant_message_id,
        )
    }
    #[test]
    fn export_preserves_conversation_and_messages_with_metadata() {
        let db = test_db();
        let (conversation_id, provider_id, user_message_id, assistant_message_id) =
            seeded_conversation(&db);
        let service = ExportService::new(&db);

        let json = service.serialize(conversation_id).expect("export succeeds");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        // The document is self-describing.
        assert_eq!(value["format"], EXPORT_FORMAT);
        assert_eq!(value["version"], EXPORT_VERSION);

        // The conversation record is preserved.
        assert_eq!(value["conversation"]["id"], conversation_id);
        assert_eq!(value["conversation"]["title"], "Planning");
        assert_eq!(value["conversation"]["status"], "active");

        // Both messages are present in persisted order.
        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);

        // User message: role and content preserved; no provider metadata.
        assert_eq!(messages[0]["id"], user_message_id);
        assert_eq!(messages[0]["conversation_id"], conversation_id);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hello");
        assert_eq!(messages[0]["provider_id"], serde_json::Value::Null);
        assert_eq!(messages[0]["model_name"], serde_json::Value::Null);

        // Assistant message: provider reference and model name preserved.
        assert_eq!(messages[1]["id"], assistant_message_id);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "hi there");
        assert_eq!(messages[1]["provider_id"].as_i64(), Some(provider_id));
        assert_eq!(messages[1]["model_name"], "gpt-4o-mini");
    }

    #[test]
    fn exported_message_order_matches_persisted_history_order() {
        let db = test_db();
        let conn = db.lock().expect("lock connection");
        conn.execute("INSERT INTO conversations (title) VALUES ('Ordered')", [])
            .expect("insert conversation");
        let conversation_id = conn.last_insert_rowid();
        // Inserted in an order that is NOT the persisted order, so the export
        // must follow the repository's `created_at` ordering, not insertion
        // order (DATABASE.md §7.2).
        let later = insert_message(&conn, conversation_id, "assistant", "later", None, None, 20);
        let earlier = insert_message(&conn, conversation_id, "user", "earlier", None, None, 10);
        drop(conn);

        let service = ExportService::new(&db);
        let json = service.serialize(conversation_id).expect("export succeeds");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        let messages = value["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["id"], earlier);
        assert_eq!(messages[0]["content"], "earlier");
        assert_eq!(messages[1]["id"], later);
        assert_eq!(messages[1]["content"], "later");
    }

    #[test]
    fn empty_conversation_exports_an_empty_messages_array() {
        let db = test_db();
        let conn = db.lock().expect("lock connection");
        conn.execute("INSERT INTO conversations (title) VALUES ('Empty')", [])
            .expect("insert conversation");
        let conversation_id = conn.last_insert_rowid();
        drop(conn);

        let service = ExportService::new(&db);
        let json = service.serialize(conversation_id).expect("export succeeds");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(value["conversation"]["title"], "Empty");
        assert_eq!(
            value["messages"].as_array().expect("messages array").len(),
            0
        );
    }

    #[test]
    fn export_to_file_writes_the_document_and_leaves_storage_untouched() {
        let db = test_db();
        let (conversation_id, ..) = seeded_conversation(&db);
        let service = ExportService::new(&db);

        // Snapshot the persisted records before exporting.
        let before_conversation = ConversationRepository::new(&db)
            .read(conversation_id)
            .expect("read conversation")
            .expect("conversation exists");
        let before_messages = MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages");

        let path =
            std::env::temp_dir().join(format!("nexora_export_test_{}.json", std::process::id()));
        service
            .export_to_file(conversation_id, &path)
            .expect("export to file succeeds");

        // The written file matches the serialized document exactly.
        let written = std::fs::read_to_string(&path).expect("read exported file");
        assert_eq!(
            written,
            service.serialize(conversation_id).expect("serialize")
        );

        // Export performed read-only access: the stored records are unchanged.
        let after_conversation = ConversationRepository::new(&db)
            .read(conversation_id)
            .expect("read conversation")
            .expect("conversation exists");
        let after_messages = MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages");
        assert_eq!(before_conversation, after_conversation);
        assert_eq!(before_messages, after_messages);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exporting_unknown_conversation_is_not_found() {
        let db = test_db();
        let service = ExportService::new(&db);

        let err = service.serialize(42).expect_err("unknown conversation");
        assert!(matches!(err, ExportError::NotFound { id: 42 }));

        // The file variant also fails cleanly and writes nothing.
        let path = std::env::temp_dir().join("nexora_export_unknown.json");
        let err = service
            .export_to_file(42, &path)
            .expect_err("unknown conversation");
        assert!(matches!(err, ExportError::NotFound { id: 42 }));
        assert!(!path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_failure_is_reported_as_io_error() {
        let db = test_db();
        let (conversation_id, ..) = seeded_conversation(&db);
        let service = ExportService::new(&db);

        // A path whose parent directory does not exist cannot be written.
        let path = std::env::temp_dir()
            .join("nexora_missing_parent_dir")
            .join("export.json");
        let err = service
            .export_to_file(conversation_id, &path)
            .expect_err("write must fail");

        assert!(matches!(err, ExportError::Io(_)));
        assert!(!path.exists());
    }

    fn invalid_export_reason(path: &Path) -> &'static str {
        match validate_export_path(path) {
            Err(ExportError::InvalidPath { reason }) => reason,
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    #[test]
    fn export_path_allowlist_rejects_non_json_extensions() {
        assert_eq!(ALLOWED_EXPORT_EXTENSIONS, &["json"]);
        let dir = std::env::temp_dir();
        assert_eq!(
            invalid_export_reason(&dir.join("out.exe")),
            "only .json export files are allowed"
        );
        assert_eq!(
            invalid_export_reason(&dir.join("out")),
            "only .json export files are allowed"
        );
        assert_eq!(
            invalid_export_reason(&dir.join("out.txt")),
            "only .json export files are allowed"
        );
        // The allowlist itself is case-insensitive: `.JSON` validates.
        assert!(
            validate_export_path(&dir.join("out.JSON")).is_ok(),
            ".JSON must pass the allowlist in a scratch directory"
        );
        // A hostile payload in the path never echoes in the fixed reason.
        let hostile = dir.join("sk-live-sentinel-1.exe");
        let rendered = format!(
            "{}",
            ExportError::InvalidPath {
                reason: invalid_export_reason(&hostile)
            }
        );
        assert!(!rendered.contains("sk-live-sentinel-1"));
    }

    #[test]
    fn export_path_rejects_unc_and_empty_and_null_byte() {
        assert_eq!(
            invalid_export_reason(Path::new("//server/share/out.json")),
            "UNC network paths are not allowed"
        );
        assert_eq!(
            invalid_export_reason(Path::new("")),
            "path must not be empty"
        );
        assert_eq!(
            invalid_export_reason(Path::new("out\0.json")),
            "path contains a null byte"
        );
    }

    #[test]
    fn export_end_to_end_rejects_non_json_artifact() {
        let db = test_db();
        let (conversation_id, ..) = seeded_conversation(&db);
        let service = ExportService::new(&db);
        let path =
            std::env::temp_dir().join(format!("nexora_export_reject_{}.exe", std::process::id()));
        let err = service
            .export_to_file(conversation_id, &path)
            .expect_err("non-json artifact must be denied");
        assert!(matches!(err, ExportError::InvalidPath { .. }));
        assert!(!path.exists(), "denied export writes nothing");
    }

    #[cfg(unix)]
    #[test]
    fn export_blocks_nested_symlink_escape_at_depth_two() {
        use std::os::unix::fs::symlink;
        // H-1 pattern reused for the artifact path: `base/link` points
        // outside while `newsub` does not exist yet — the walk must catch
        // the escape at the `link` level, not the missing leaf.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("nexora-export-base-{stamp}"));
        let outside = std::env::temp_dir().join(format!("nexora-export-outside-{stamp}"));
        std::fs::create_dir_all(&base).expect("create base");
        std::fs::create_dir_all(&outside).expect("create outside");
        symlink(&outside, base.join("link")).expect("symlink base/link -> outside");

        let target = base.join("link").join("newsub").join("out.json");
        let err = invalid_export_reason(&target);
        assert_eq!(err, "a path component escapes its parent directory");

        // The end-to-end export denies the same way and writes nothing
        // outside the chosen directory.
        let db = test_db();
        let (conversation_id, ..) = seeded_conversation(&db);
        let service = ExportService::new(&db);
        let err = service
            .export_to_file(conversation_id, &target)
            .expect_err("depth-2 escape must be denied");
        assert!(matches!(err, ExportError::InvalidPath { .. }));
        assert!(!outside.join("newsub").join("out.json").exists());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
