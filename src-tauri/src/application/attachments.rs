//! Attachment service: application-layer orchestration for local document
//! attachments (FR-008; ROADMAP.md Phase 6 — Documents; ARCHITECTURE.md §5).
//!
//! This service composes the existing [`AttachmentRepository`] for attachment
//! persistence and the existing [`ConversationRepository`] to verify that an
//! attachment belongs to an existing conversation. It adds no schema, no SQL,
//! and no database access of its own: all persistence is delegated to the
//! existing repositories.
//!
//! # Local-file semantics
//!
//! FR-008 requires a *local-file reference*: the `file_name`, `file_path`,
//! `file_size_bytes`, and `mime_type` metadata are persisted in a draft row
//! (`message_id` is `NULL`) so attachments are visible before submission and
//! removable before sending (DATABASE.md §7.4). No file is copied, read,
//! parsed, or uploaded, no content is extracted, and no cloud path, remote URL,
//! upload mechanism, or provider-specific attachment representation is
//! introduced here.
//!
//! # Provider independence (ARCHITECTURE.md §7)
//!
//! This module contains no `OpenAI`, `Anthropic`, or `Gemini`-specific
//! behavior. Attachments are recorded as conversation-owned draft rows and are
//! never sent to a provider from this layer.
//!
//! # Scope
//!
//! Document processing — `OCR`, parsing, indexing, embeddings, retrieval,
//! `RAG`, `AST` analysis, and full-text search — is out of Phase 6 scope and is
//! not implemented here.

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::attachments::{Attachment, AttachmentRepository};
use crate::infrastructure::repository::conversations::ConversationRepository;
use rusqlite::Error as SqliteError;

/// Application-layer result shared by attachment operations, unifying
/// validation and persistence failures.
pub(crate) type Result<T> = std::result::Result<T, AttachmentError>;

/// Application-layer service orchestrating local document attachments.
///
/// Wraps [`AttachmentRepository`] for persistence and [`ConversationRepository`]
/// for conversation ownership checks. It is deliberately focused on
/// orchestration and validation; persistence behavior and schema constraints
/// remain in the repositories and the database.
pub(crate) struct AttachmentService<'a> {
    attachments: AttachmentRepository<'a>,
    conversations: ConversationRepository<'a>,
}

impl<'a> AttachmentService<'a> {
    /// Create a service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            attachments: AttachmentRepository::new(db),
            conversations: ConversationRepository::new(db),
        }
    }

    /// Attach a local file to a conversation as a draft attachment (FR-008;
    /// DATABASE.md §7.4).
    ///
    /// The flow is:
    ///   1. Validate `file_name`, `file_path`, `file_size_bytes`, and
    ///      `mime_type` against the documented `attachments` constraints.
    ///   2. Require the conversation to exist.
    ///   3. Persist a draft attachment row (`message_id` is `NULL`) through the
    ///      existing repository.
    ///   4. Return the persisted attachment, including the schema-assigned
    ///      `id`.
    ///
    /// No file is opened, copied, or parsed here: this is a local-file
    /// reference only, and no document content is ever read or stored.
    ///
    /// The MVP applies no supported-file-type allowlist (Product Owner
    /// confirmation): any local file type may be attached, and only the
    /// documented `attachments` schema constraints and the conversation
    /// ownership rule are enforced.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::InvalidInput`] when a value violates a
    /// documented `attachments` constraint;
    /// [`AttachmentError::ConversationNotFound`] when no conversation with
    /// `conversation_id` exists; or [`AttachmentError::Database`] when a
    /// persistence step fails.
    pub(crate) fn attach(
        &self,
        conversation_id: i64,
        file_name: &str,
        file_path: &str,
        file_size_bytes: Option<i64>,
        mime_type: Option<&str>,
    ) -> Result<Attachment> {
        validate_attachment_input(file_name, file_path, file_size_bytes, mime_type)?;

        if !self.conversations.exists(conversation_id)? {
            return Err(AttachmentError::ConversationNotFound {
                id: conversation_id,
            });
        }

        let id = self.attachments.create(
            conversation_id,
            file_name,
            file_path,
            file_size_bytes,
            mime_type,
        )?;

        // The row was just inserted on the same shared connection, so it is
        // guaranteed readable; `None` here is an internal inconsistency.
        self.attachments.read(id)?.ok_or_else(|| {
            AttachmentError::Database(DatabaseError::Sqlite(SqliteError::QueryReturnedNoRows))
        })
    }

    /// Read an attachment by `id`.
    ///
    /// Returns [`Some`] when an attachment with that `id` exists, or [`None`]
    /// otherwise (including attachments removed by a conversation or message
    /// cascade, DATABASE.md §7.4).
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::Database`] on a failed query or a poisoned
    /// connection.
    pub(crate) fn read(&self, id: i64) -> Result<Option<Attachment>> {
        Ok(self.attachments.read(id)?)
    }

    /// List the draft attachments of one conversation (FR-008; DATABASE.md
    /// §7.4).
    ///
    /// Returns the pre-submission attachments (`message_id` is `NULL`),
    /// ordered by `id` ascending. Historical, message-linked attachments are
    /// not included.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::Database`] if listing fails.
    pub(crate) fn list(&self, conversation_id: i64) -> Result<Vec<Attachment>> {
        Ok(self.attachments.list_by_conversation(conversation_id)?)
    }

    /// Remove an attachment by `id` (FR-008; DATABASE.md §7.4).
    ///
    /// Hard delete through the repository, before the attachment is linked to
    /// a message. A cascade is applied only where the existing schema already
    /// defines one; nothing else is deleted here. The repository fields the
    /// actual delete; this layer only classifies a missing attachment.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::AttachmentNotFound`] when no attachment with
    /// `id` exists, or [`AttachmentError::Database`] if the delete fails.
    pub(crate) fn remove(&self, id: i64) -> Result<()> {
        if !self.attachments.exists(id)? {
            return Err(AttachmentError::AttachmentNotFound { id });
        }
        self.attachments.delete(id)?;
        Ok(())
    }
}

/// Classified errors raised by attachment orchestration.
///
/// Unifies validation and persistence failures. No variant carries a secret
/// value or file content, so formatting an [`AttachmentError`] never writes
/// either to the logs (ARCHITECTURE.md §9, §11).
#[derive(Debug)]
pub(crate) enum AttachmentError {
    /// No conversation with the referenced `id` exists.
    ConversationNotFound {
        /// The requested conversation id.
        id: i64,
    },
    /// No attachment with the referenced `id` exists.
    AttachmentNotFound {
        /// The requested attachment id.
        id: i64,
    },
    /// A value rejected by the documented `attachments` constraints
    /// (DATABASE.md §7.4).
    InvalidInput {
        /// The attachment field that failed validation, mirroring the column
        /// whose constraint was violated.
        field: &'static str,
        /// Why the value is invalid, expressed through the violated
        /// constraint. Carries no file path or file content.
        reason: &'static str,
    },
    /// A persistence failure from a repository.
    Database(DatabaseError),
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConversationNotFound { id } => write!(f, "conversation {id} does not exist"),
            Self::AttachmentNotFound { id } => write!(f, "attachment {id} does not exist"),
            Self::InvalidInput { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::Database(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AttachmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConversationNotFound { .. }
            | Self::AttachmentNotFound { .. }
            | Self::InvalidInput { .. } => None,
            Self::Database(err) => Some(err),
        }
    }
}

impl From<DatabaseError> for AttachmentError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

/// Validate caller-supplied local-file metadata against the documented
/// `attachments` schema constraints (DATABASE.md §7.4) before persistence.
///
/// Each check mirrors the corresponding table CHECK constraint exactly:
///
/// * `file_name`: `length(file_name) > 0` and `length(file_name) <= 255`.
/// * `file_path`: `length(file_path) > 0`.
/// * `file_size_bytes`: `file_size_bytes >= 0` (only when present).
/// * `mime_type`: `length(mime_type) <= 127` (only when present).
///
/// No additional restrictions are invented — in particular, no supported-
/// file-type allowlist is applied (Product Owner confirmation): any local
/// file type is accepted. `file_size_bytes` and `mime_type` remain optional
/// exactly as the schema defines them, and `length` counts characters,
/// matching `SQLite`'s `length()` semantics.
fn validate_attachment_input(
    file_name: &str,
    file_path: &str,
    file_size_bytes: Option<i64>,
    mime_type: Option<&str>,
) -> Result<()> {
    if file_name.chars().count() == 0 {
        return Err(AttachmentError::InvalidInput {
            field: "file_name",
            reason: "must not be empty",
        });
    }
    if file_name.chars().count() > 255 {
        return Err(AttachmentError::InvalidInput {
            field: "file_name",
            reason: "must be at most 255 characters",
        });
    }
    if file_path.is_empty() {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must not be empty",
        });
    }
    if matches!(file_size_bytes, Some(size) if size < 0) {
        return Err(AttachmentError::InvalidInput {
            field: "file_size_bytes",
            reason: "must not be negative",
        });
    }
    if matches!(mime_type, Some(value) if value.chars().count() > 127) {
        return Err(AttachmentError::InvalidInput {
            field: "mime_type",
            reason: "must be at most 127 characters",
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Build a service over an in-memory database whose schema mirrors the
    /// documented `conversations` / `attachments` tables (DATABASE.md §7.1,
    /// §7.4) plus the `messages` / `providers` tables referenced by their
    /// foreign keys, shaped like the sibling conversation-service tests.
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
                 role TEXT NOT NULL,
                 content TEXT NOT NULL CHECK(length(content) > 0),
                 provider_id INTEGER
                     CHECK(provider_id IS NULL OR provider_id > 0)
                     REFERENCES providers(id) ON DELETE SET NULL,
                 model_name TEXT CHECK(length(model_name) <= 200),
                 created_at INTEGER NOT NULL DEFAULT 1 CHECK(created_at > 0)
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
                 mime_type TEXT CHECK(length(mime_type) <= 127)
             );",
        )
        .expect("create test schema");
        Database::new(conn)
    }

    /// Create a conversation directly through its repository.
    fn create_conversation(db: &Database) -> i64 {
        ConversationRepository::new(db)
            .create("Chat", "active")
            .expect("conversation created")
    }

    /// Read an attachment back through the repository, panicking if absent.
    fn read_attachment(db: &Database, id: i64) -> Attachment {
        AttachmentRepository::new(db)
            .read(id)
            .expect("read attachment")
            .expect("attachment exists")
    }

    #[test]
    fn attach_creates_a_draft_attachment_for_the_conversation() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        let attachment = service
            .attach(
                conversation_id,
                "notes.txt",
                "C:\\docs\\notes.txt",
                Some(12),
                Some("text/plain"),
            )
            .expect("attachment created");

        // `id` is schema-assigned and the row is in the draft state.
        assert!(attachment.id > 0);
        assert_eq!(attachment.conversation_id, conversation_id);
        assert_eq!(attachment.message_id, None);
        assert_eq!(attachment.file_name, "notes.txt");
        assert_eq!(attachment.file_path, "C:\\docs\\notes.txt");
        assert_eq!(attachment.file_size_bytes, Some(12));
        assert_eq!(attachment.mime_type.as_deref(), Some("text/plain"));

        // The row is independently persisted through the repository.
        assert_eq!(read_attachment(&db, attachment.id), attachment);
    }

    #[test]
    fn attached_files_belong_only_to_their_conversation() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let first = create_conversation(&db);
        let second = create_conversation(&db);

        let first_file = service
            .attach(first, "a.txt", "/tmp/a.txt", None, None)
            .expect("attach to first");
        let second_file = service
            .attach(second, "b.txt", "/tmp/b.txt", None, None)
            .expect("attach to second");

        let first_drafts = service.list(first).expect("list first");
        assert_eq!(first_drafts.len(), 1);
        assert_eq!(first_drafts[0].id, first_file.id);
        assert_eq!(first_drafts[0].conversation_id, first);

        let second_drafts = service.list(second).expect("list second");
        assert_eq!(second_drafts.len(), 1);
        assert_eq!(second_drafts[0].id, second_file.id);
        assert_eq!(second_drafts[0].conversation_id, second);
    }

    #[test]
    fn list_returns_draft_attachments_in_insertion_order() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        let first = service
            .attach(
                conversation_id,
                "a.txt",
                "/tmp/a.txt",
                Some(1),
                Some("text/plain"),
            )
            .expect("attach first");
        let second = service
            .attach(conversation_id, "b.txt", "/tmp/b.txt", Some(2), None)
            .expect("attach second");

        let drafts = service.list(conversation_id).expect("list attachments");
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].id, first.id);
        assert_eq!(drafts[1].id, second.id);
        assert!(drafts
            .iter()
            .all(|attachment| attachment.message_id.is_none()));
    }

    #[test]
    fn read_returns_attachment_by_id_and_none_for_unknown() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let id = service
            .attach(conversation_id, "note.txt", "/tmp/note.txt", None, None)
            .expect("attachment created")
            .id;

        let attachment = service
            .read(id)
            .expect("read attachment")
            .expect("attachment exists");
        assert_eq!(attachment.conversation_id, conversation_id);
        assert_eq!(attachment.file_name, "note.txt");

        assert!(service.read(42).expect("read unknown").is_none());
    }

    #[test]
    fn remove_deletes_the_attachment() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let id = service
            .attach(conversation_id, "note.txt", "/tmp/note.txt", None, None)
            .expect("attachment created")
            .id;

        service.remove(id).expect("remove succeeds");

        assert!(service.read(id).expect("read removed").is_none());
        assert!(service.list(conversation_id).expect("list").is_empty());
    }

    #[test]
    fn remove_deletes_only_the_targeted_draft() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let keep = service
            .attach(conversation_id, "keep.txt", "/tmp/keep.txt", None, None)
            .expect("attach kept file")
            .id;
        let gone = service
            .attach(conversation_id, "gone.txt", "/tmp/gone.txt", None, None)
            .expect("attach removed file")
            .id;

        service.remove(gone).expect("remove succeeds");

        // No cascading behavior: the sibling draft is untouched.
        let drafts = service.list(conversation_id).expect("list");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].id, keep);
        assert!(service.read(gone).expect("read removed").is_none());
    }

    #[test]
    fn remove_of_unknown_attachment_is_attachment_not_found() {
        let db = test_db();
        let service = AttachmentService::new(&db);

        let err = service.remove(42).expect_err("unknown attachment");

        assert!(matches!(
            err,
            AttachmentError::AttachmentNotFound { id: 42 }
        ));
    }

    #[test]
    fn attach_with_unknown_conversation_is_conversation_not_found() {
        let db = test_db();
        let service = AttachmentService::new(&db);

        let err = service
            .attach(42, "note.txt", "/tmp/note.txt", None, None)
            .expect_err("unknown conversation");

        assert!(matches!(
            err,
            AttachmentError::ConversationNotFound { id: 42 }
        ));
        // Nothing is persisted when the conversation is missing.
        assert!(service
            .list(42)
            .expect("list unknown conversation")
            .is_empty());
    }

    #[test]
    fn attach_with_invalid_file_name_is_invalid_input() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // Empty `file_name` violates length(file_name) > 0.
        let err = service
            .attach(conversation_id, "", "/tmp/x.txt", None, None)
            .expect_err("empty file name");
        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "file_name",
                ..
            }
        ));

        // 256 characters violate length(file_name) <= 255.
        let long_name = "x".repeat(256);
        let err = service
            .attach(conversation_id, &long_name, "/tmp/x.txt", None, None)
            .expect_err("overlong file name");
        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "file_name",
                ..
            }
        ));
    }

    #[test]
    fn attach_with_invalid_file_path_is_invalid_input() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // Empty `file_path` violates length(file_path) > 0.
        let err = service
            .attach(conversation_id, "x.txt", "", None, None)
            .expect_err("empty file path");

        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "file_path",
                ..
            }
        ));
    }
    #[test]
    fn attach_with_negative_file_size_is_invalid_input() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // A negative size violates file_size_bytes >= 0.
        let err = service
            .attach(conversation_id, "x.txt", "/tmp/x.txt", Some(-1), None)
            .expect_err("negative file size");

        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "file_size_bytes",
                ..
            }
        ));
    }

    #[test]
    fn attach_with_overlong_mime_type_is_invalid_input() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // 128 characters violate length(mime_type) <= 127.
        let long_mime = "a".repeat(128);
        let err = service
            .attach(
                conversation_id,
                "x.txt",
                "/tmp/x.txt",
                None,
                Some(&long_mime),
            )
            .expect_err("overlong mime type");

        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "mime_type",
                ..
            }
        ));
    }

    #[test]
    fn attach_accepts_schema_boundary_lengths() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // 255-char `file_name`, 127-char `mime_type`, and a zero size are the
        // upper/lower bounds the schema permits.
        let name = "x".repeat(255);
        let mime = "m".repeat(127);
        let attachment = service
            .attach(
                conversation_id,
                &name,
                "/tmp/boundary.txt",
                Some(0),
                Some(&mime),
            )
            .expect("boundary values accepted");

        assert_eq!(attachment.file_name, name);
        assert_eq!(attachment.file_size_bytes, Some(0));
        assert_eq!(attachment.mime_type.as_deref(), Some(mime.as_str()));
    }

    #[test]
    fn attach_accepts_any_file_type() {
        // The confirmed MVP rule imposes no supported-file-type allowlist: an
        // arbitrary extension and MIME type are accepted as long as the
        // documented schema constraints hold.
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        let attachment = service
            .attach(
                conversation_id,
                "archive.xyz",
                "/tmp/archive.xyz",
                Some(3),
                Some("application/x-unknown"),
            )
            .expect("arbitrary file type accepted");

        assert_eq!(attachment.file_name, "archive.xyz");
        assert_eq!(attachment.file_path, "/tmp/archive.xyz");
        assert_eq!(
            attachment.mime_type.as_deref(),
            Some("application/x-unknown")
        );
        assert!(attachment.id > 0);
    }

    #[test]
    fn repository_failures_map_to_database_error() {
        // A database whose schema has `conversations` but lacks the
        // `attachments` table forces the repository's create to fail; the
        // service classifies that persistence failure as a database error
        // rather than panicking.
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
        let conversation_id = ConversationRepository::new(&db)
            .create("Chat", "active")
            .expect("conversation created");
        let service = AttachmentService::new(&db);

        let err = service
            .attach(conversation_id, "note.txt", "/tmp/note.txt", None, None)
            .expect_err("missing attachments table");

        assert!(matches!(err, AttachmentError::Database(_)));
    }
}
