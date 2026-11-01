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
//! FR-008 requires a *local-file reference*: the `file_name`, `file_path`, and
//! `mime_type` metadata are persisted in a draft row (`message_id` is `NULL`)
//! so attachments are visible before submission and removable before sending
//! (DATABASE.md §7.4). `file_size_bytes` is derived server-side from the
//! file's own metadata (SEC-01) — the renderer never supplies it. No file is
//! copied, parsed, or uploaded at attach time, no content is extracted, and
//! no cloud path, remote URL, upload mechanism, or provider-specific
//! attachment representation is introduced here.
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

use std::path::{Path, PathBuf};

use super::conversations::MAX_ATTACHMENT_BYTES;
use super::workspace::{
    is_drive_root, is_system_file_path, is_unc_path, strip_verbatim, WORKSPACE_ROOT_MAX_LEN,
};
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
    ///   1. Validate `file_name`, `file_path`, and `mime_type` against the
    ///      documented `attachments` constraints.
    ///   2. Require the conversation to exist.
    ///   3. Resolve and validate the file path (canonicalization, system /
    ///      sensitive-location guards) and derive `file_size_bytes` from the
    ///      file's own metadata — the size is **server-derived**, never a
    ///      caller-supplied value (SEC-01/SEC-02).
    ///   4. Persist a draft attachment row (`message_id` is `NULL`) through the
    ///      existing repository.
    ///   5. Return the persisted attachment, including the schema-assigned
    ///      `id`.
    ///
    /// No file content is opened, copied, parsed, or uploaded here: this is a
    /// local-file reference only.
    ///
    /// The MVP applies no supported-file-type allowlist (Product Owner
    /// confirmation): any local file type may be attached, subject to the
    /// documented schema constraints, the conversation ownership rule, and
    /// the sensitive-location blocklist in
    /// [`resolve_attachment_file_path`].
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::InvalidInput`] when a value violates a
    /// documented `attachments` constraint or the file is oversized;
    /// [`AttachmentError::ConversationNotFound`] when no conversation with
    /// `conversation_id` exists; or [`AttachmentError::Database`] when a
    /// persistence step fails.
    pub(crate) fn attach(
        &self,
        conversation_id: i64,
        file_name: &str,
        file_path: &str,
        mime_type: Option<&str>,
    ) -> Result<Attachment> {
        validate_attachment_input(file_name, file_path, mime_type)?;

        if !self.conversations.exists(conversation_id)? {
            return Err(AttachmentError::ConversationNotFound {
                id: conversation_id,
            });
        }

        let canonical = resolve_attachment_file_path(file_path)?;
        // Server-derived size (SEC-01): the renderer no longer supplies a
        // declared size, so the WS-C.2 gate checks the only size that exists —
        // the file's own metadata. A metadata failure after a successful
        // canonicalization means the file vanished mid-check: fail closed.
        let actual_size = std::fs::metadata(&canonical)
            .map(|meta| meta.len())
            .ok()
            .and_then(|len| i64::try_from(len).ok())
            .unwrap_or(i64::MAX);
        if actual_size > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::InvalidInput {
                field: "file_size_bytes",
                reason: "exceeds the 20 MiB limit",
            });
        }
        let canonical_text = canonical.to_string_lossy().into_owned();

        let id = self.attachments.create(
            conversation_id,
            file_name,
            &canonical_text,
            Some(actual_size),
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
/// * `mime_type`: `length(mime_type) <= 127` (only when present).
///
/// `file_size_bytes` is intentionally absent: the size is derived from the
/// file's own metadata in [`AttachmentService::attach`] (SEC-01), never from
/// caller-supplied text. No supported-file-type allowlist is applied
/// (Product Owner confirmation): any local file type is accepted, and
/// `length` counts characters, matching `SQLite`'s `length()` semantics.
fn validate_attachment_input(
    file_name: &str,
    file_path: &str,
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
    if matches!(mime_type, Some(value) if value.chars().count() > 127) {
        return Err(AttachmentError::InvalidInput {
            field: "mime_type",
            reason: "must be at most 127 characters",
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sensitive user-profile blocklist (SEC-02)
// ---------------------------------------------------------------------------

/// Sensitive dot-directory components: a canonical attachment path carrying
/// any of these components — on any drive, for any user — names a well-known
/// credential store (SSH keys, cloud credentials, the `GnuPG` keyring,
/// Kubernetes cluster configs) and is refused. The system blocklist
/// (`is_system_file_path`) deliberately admits the `Users` tree, so this
/// subpath rule is what keeps profile-level credential files unattachable.
const SENSITIVE_DOT_COMPONENTS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube"];

/// User-profile-relative browser user-data directories (SEC-02), matched
/// case-insensitively against the home-relative remainder of a canonical
/// path with both separators normalized to `/`. Covers the Chrome /
/// Chromium / Edge / Firefox families on Windows and Unix, including the
/// snap-packaged Firefox layout.
const BROWSER_PROFILE_PATHS: &[&str] = &[
    "appdata/local/google/chrome/user data",
    "appdata/local/chromium/user data",
    "appdata/local/microsoft/edge/user data",
    "appdata/roaming/mozilla/firefox",
    "snap/firefox/common/.mozilla/firefox",
    ".config/google-chrome",
    ".config/chromium",
    ".config/microsoft-edge",
    ".mozilla/firefox",
];

/// The current user's home directory from the environment (no new
/// dependency): `USERPROFILE` on Windows, `HOME` elsewhere, with the other
/// variable as a fallback. `None` when neither is set — the dot-component
/// rule still applies without it.
fn current_user_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Whether `canonical` names a sensitive user-profile location (SEC-02):
/// any path component equals one of [`SENSITIVE_DOT_COMPONENTS`]
/// (case-insensitive), or the path sits inside the current user's browser
/// user-data directory ([`BROWSER_PROFILE_PATHS`], home-relative). A pure
/// predicate over text — no filesystem access — so tests can drive it with
/// fabricated homes.
fn is_sensitive_profile_path(home: Option<&Path>, canonical: &Path) -> bool {
    for component in canonical.components() {
        if let std::path::Component::Normal(part) = component {
            let text = part.to_string_lossy();
            if SENSITIVE_DOT_COMPONENTS
                .iter()
                .any(|sensitive| text.eq_ignore_ascii_case(sensitive))
            {
                return true;
            }
        }
    }
    let Some(home) = home else {
        return false;
    };
    // Separator- and case-agnostic comparison: canonical paths use `\` on
    // Windows and `/` elsewhere, and the environment's casing is not
    // guaranteed to match the filesystem's, so both sides normalize to
    // lowercase `/` text before the prefix test.
    let mut home_text = home.to_string_lossy().replace('\\', "/");
    while home_text.ends_with('/') {
        home_text.pop();
    }
    let home_text = home_text.to_lowercase();
    let canonical_text = canonical
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let Some(relative) = canonical_text.strip_prefix(&home_text) else {
        return false;
    };
    let relative = relative.strip_prefix('/').unwrap_or(relative);
    BROWSER_PROFILE_PATHS
        .iter()
        .any(|profile| relative == *profile || relative.starts_with(&format!("{profile}/")))
}

/// Resolve `raw` as an attachment file path and return its canonical form.
///
/// Checks run in exactly this order, mirroring [`super::workspace::validate_workspace_root`]:
/// trim, reject empty / null-byte / overlong / relative / UNC / protected-system
/// paths, canonicalize via the filesystem (non-existent paths fail here), then
/// reject UNC / non-file / protected-system / drive-root canonical paths,
/// sensitive user-profile locations ([`is_sensitive_profile_path`], SEC-02),
/// and overlong canonical text. The returned path is the canonicalized
/// absolute path with any Windows verbatim prefix stripped, ready to store.
///
/// Every failure is the existing [`AttachmentError::InvalidInput`] with
/// `field: "file_path"` and a static reason that never contains the path or
/// file content. No extension/MIME allowlist is applied (the Product Owner
/// no-allowlist rule); the user-profile sub-path blocklist is the one
/// deliberate addition (SEC-02).
pub(crate) fn resolve_attachment_file_path(raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must not be empty",
        });
    }
    if trimmed.contains('\0') {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must not contain a null byte",
        });
    }
    if trimmed.len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must be at most 1024 characters",
        });
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must be an absolute path",
        });
    }
    if is_unc_path(Path::new(trimmed)) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "UNC network paths are not allowed",
        });
    }
    if is_system_file_path(Path::new(trimmed)) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "a protected system location is not allowed",
        });
    }
    let canonical = std::fs::canonicalize(trimmed).map_err(|_| AttachmentError::InvalidInput {
        field: "file_path",
        reason: "path does not exist",
    })?;
    let canonical = strip_verbatim(canonical);
    if is_unc_path(&canonical) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "UNC network paths are not allowed",
        });
    }
    if !canonical.is_file() {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "must be an existing regular file",
        });
    }
    if is_system_file_path(&canonical) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "a protected system location is not allowed",
        });
    }
    if is_drive_root(&canonical) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "a drive root is not allowed",
        });
    }
    // SEC-02: credential stores and browser profile databases are never
    // valid attachments — the send path would ship their bytes to the
    // configured AI provider.
    if is_sensitive_profile_path(current_user_home().as_deref(), &canonical) {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "a sensitive user-profile location is not allowed",
        });
    }
    if canonical.to_string_lossy().len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(AttachmentError::InvalidInput {
            field: "file_path",
            reason: "canonical path exceeds 1024 characters",
        });
    }
    Ok(canonical)
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Create a real temporary file with `contents` and return its path.
    /// The service canonicalizes on attach, so callers must compare the
    /// stored path against the canonical form, not this raw path.
    fn temp_file(contents: &[u8]) -> PathBuf {
        let id = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nexora-attach-test-{}-{}-{id}.txt",
            std::process::id(),
            nanos
        ));
        std::fs::write(&path, contents).expect("write temp attachment file");
        path
    }

    /// Canonical form of `path` as the service stores it.
    fn canonical_str(path: &Path) -> String {
        resolve_attachment_file_path(path.to_string_lossy().as_ref())
            .expect("temp file resolves")
            .to_string_lossy()
            .into_owned()
    }

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
        let path = temp_file(b"notes body");

        let attachment = service
            .attach(
                conversation_id,
                "notes.txt",
                path.to_string_lossy().as_ref(),
                Some("text/plain"),
            )
            .expect("attachment created");

        // `id` is schema-assigned and the row is in the draft state.
        assert!(attachment.id > 0);
        assert_eq!(attachment.conversation_id, conversation_id);
        assert_eq!(attachment.message_id, None);
        assert_eq!(attachment.file_name, "notes.txt");
        // The stored path is the canonical form, not the raw input.
        assert_eq!(attachment.file_path, canonical_str(&path));
        // The size is derived server-side from the file's own metadata
        // (SEC-01): `notes body` is 10 bytes, whatever the caller claims.
        assert_eq!(attachment.file_size_bytes, Some(10));
        assert_eq!(attachment.mime_type.as_deref(), Some("text/plain"));

        // The row is independently persisted through the repository.
        assert_eq!(read_attachment(&db, attachment.id), attachment);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn attached_files_belong_only_to_their_conversation() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let first = create_conversation(&db);
        let second = create_conversation(&db);
        let first_path = temp_file(b"a");
        let second_path = temp_file(b"b");

        let first_file = service
            .attach(first, "a.txt", first_path.to_string_lossy().as_ref(), None)
            .expect("attach to first");
        let second_file = service
            .attach(
                second,
                "b.txt",
                second_path.to_string_lossy().as_ref(),
                None,
            )
            .expect("attach to second");

        let first_drafts = service.list(first).expect("list first");
        assert_eq!(first_drafts.len(), 1);
        assert_eq!(first_drafts[0].id, first_file.id);
        assert_eq!(first_drafts[0].conversation_id, first);

        let second_drafts = service.list(second).expect("list second");
        assert_eq!(second_drafts.len(), 1);
        assert_eq!(second_drafts[0].id, second_file.id);
        assert_eq!(second_drafts[0].conversation_id, second);
        let _ = std::fs::remove_file(&first_path);
        let _ = std::fs::remove_file(&second_path);
    }

    #[test]
    fn list_returns_draft_attachments_in_insertion_order() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let first_path = temp_file(b"a");
        let second_path = temp_file(b"b");

        let first = service
            .attach(
                conversation_id,
                "a.txt",
                first_path.to_string_lossy().as_ref(),
                Some("text/plain"),
            )
            .expect("attach first");
        let second = service
            .attach(
                conversation_id,
                "b.txt",
                second_path.to_string_lossy().as_ref(),
                None,
            )
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
        let path = temp_file(b"note");
        let id = service
            .attach(
                conversation_id,
                "note.txt",
                path.to_string_lossy().as_ref(),
                None,
            )
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
        let path = temp_file(b"note");
        let id = service
            .attach(
                conversation_id,
                "note.txt",
                path.to_string_lossy().as_ref(),
                None,
            )
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
        let keep_path = temp_file(b"keep");
        let gone_path = temp_file(b"gone");
        let keep = service
            .attach(
                conversation_id,
                "keep.txt",
                keep_path.to_string_lossy().as_ref(),
                None,
            )
            .expect("attach kept file")
            .id;
        let gone = service
            .attach(
                conversation_id,
                "gone.txt",
                gone_path.to_string_lossy().as_ref(),
                None,
            )
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
            .attach(42, "note.txt", "/tmp/note.txt", None)
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
            .attach(conversation_id, "", "/tmp/x.txt", None)
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
            .attach(conversation_id, &long_name, "/tmp/x.txt", None)
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
            .attach(conversation_id, "x.txt", "", None)
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
    fn attach_with_overlong_mime_type_is_invalid_input() {
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);

        // 128 characters violate length(mime_type) <= 127.
        let long_mime = "a".repeat(128);
        let err = service
            .attach(conversation_id, "x.txt", "/tmp/x.txt", Some(&long_mime))
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

        // 255-char `file_name`, 127-char `mime_type`, and a zero-byte file
        // (server-derived size 0) are the bounds the schema permits.
        let name = "x".repeat(255);
        let mime = "m".repeat(127);
        let path = temp_file(b"");
        let attachment = service
            .attach(
                conversation_id,
                &name,
                path.to_string_lossy().as_ref(),
                Some(&mime),
            )
            .expect("boundary values accepted");

        assert_eq!(attachment.file_name, name);
        assert_eq!(attachment.file_size_bytes, Some(0));
        assert_eq!(attachment.mime_type.as_deref(), Some(mime.as_str()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn attach_accepts_any_file_type() {
        // The confirmed MVP rule imposes no supported-file-type allowlist: an
        // arbitrary extension and MIME type are accepted as long as the
        // documented schema constraints hold.
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let path = temp_file(b"xyz");

        let attachment = service
            .attach(
                conversation_id,
                "archive.xyz",
                path.to_string_lossy().as_ref(),
                Some("application/x-unknown"),
            )
            .expect("arbitrary file type accepted");

        assert_eq!(attachment.file_name, "archive.xyz");
        assert_eq!(attachment.file_path, canonical_str(&path));
        assert_eq!(
            attachment.mime_type.as_deref(),
            Some("application/x-unknown")
        );
        assert!(attachment.id > 0);
        let _ = std::fs::remove_file(&path);
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
                 updated_at INTEGER NOT NULL DEFAULT 1 CHECK(updated_at >= created_at),
                  workspace_root TEXT CHECK(workspace_root IS NULL OR length(workspace_root) <= 1024)
             );",
        )
        .expect("create conversations-only schema");
        let db = Database::new(conn);
        let conversation_id = ConversationRepository::new(&db)
            .create("Chat", "active")
            .expect("conversation created");
        let service = AttachmentService::new(&db);
        let path = temp_file(b"note");

        let err = service
            .attach(
                conversation_id,
                "note.txt",
                path.to_string_lossy().as_ref(),
                None,
            )
            .expect_err("missing attachments table");

        assert!(matches!(err, AttachmentError::Database(_)));
        let _ = std::fs::remove_file(&path);
    }

    fn invalid_file_path_reason(raw: &str) -> (&'static str, &'static str) {
        match resolve_attachment_file_path(raw) {
            Err(AttachmentError::InvalidInput { field, reason }) => (field, reason),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn attachment_path_rejects_relative_and_missing_and_directory() {
        assert_eq!(
            invalid_file_path_reason("relative/notes.txt"),
            ("file_path", "must be an absolute path")
        );
        let missing = std::env::temp_dir().join("nexora-attach-missing-9f3c2a1e.txt");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(
            invalid_file_path_reason(missing.to_string_lossy().as_ref()),
            ("file_path", "path does not exist")
        );
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        assert_eq!(
            invalid_file_path_reason(dir.to_string_lossy().as_ref()),
            ("file_path", "must be an existing regular file")
        );
    }

    #[cfg(windows)]
    #[test]
    fn attachment_path_rejects_unc_network_path() {
        // The backslash form is what a real Windows caller sends: the UNC
        // prefix makes it absolute, so the UNC guard (check 5) fires.
        assert_eq!(
            invalid_file_path_reason(r"\\server\share\file.txt"),
            ("file_path", "UNC network paths are not allowed")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn attachment_path_rejects_unc_network_path() {
        // The backslash form is relative on Unix (no leading `/`), so the
        // absolute-path guard would fire before the UNC guard there. The
        // forward-slash form is absolute on Unix, and `is_unc_text`
        // normalizes `/` to `\`, so the UNC guard fires on both platforms.
        assert_eq!(
            invalid_file_path_reason("//server/share/file.txt"),
            ("file_path", "UNC network paths are not allowed")
        );
    }

    #[cfg(windows)]
    #[test]
    fn attachment_path_rejects_protected_location_and_drive_root() {
        assert_eq!(
            invalid_file_path_reason(r"C:\Windows\System32\definitely-missing.dll"),
            ("file_path", "a protected system location is not allowed")
        );
        // A drive root is a directory, so it fails the regular-file check
        // before the drive-root check; either way it must be rejected.
        let (field, _) = invalid_file_path_reason(r"C:\");
        assert_eq!(field, "file_path");
        assert_eq!(
            invalid_file_path_reason(""),
            ("file_path", "must not be empty")
        );
        assert_eq!(
            invalid_file_path_reason("C:\\tmp\0x.txt"),
            ("file_path", "must not contain a null byte")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn attachment_path_rejects_protected_location_and_drive_root() {
        assert_eq!(
            invalid_file_path_reason("/etc/definitely-missing-9f3c2a1e.conf"),
            ("file_path", "a protected system location is not allowed")
        );
        let (field, _) = invalid_file_path_reason("/");
        assert_eq!(field, "file_path");
        assert_eq!(
            invalid_file_path_reason(""),
            ("file_path", "must not be empty")
        );
    }

    #[test]
    fn attachment_path_accepts_regular_file_as_canonical_and_user_profile() {
        // A regular temp file is accepted and the stored path is canonical.
        // Temp dirs live under C:\Users\<user>\AppData\Local\Temp on Windows
        // (elsewhere, e.g. /tmp, on Unix — likewise outside the file guard),
        // so this also proves the C:\Users tree is NOT blocked for
        // attachments (unlike the workspace-root guard).
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let path = temp_file(b"hello");
        let attachment = service
            .attach(
                conversation_id,
                "hello.txt",
                path.to_string_lossy().as_ref(),
                None,
            )
            .expect("temp file accepted");
        assert_eq!(attachment.file_path, canonical_str(&path));
        assert!(
            attachment.file_path.contains("nexora-attach-test-"),
            "stored canonical path keeps the temp file name"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn attach_rejects_actual_size_above_the_limit() {
        // Sparse file: the logical size trips the gate without 20 MiB of I/O.
        // With the size now derived server-side (SEC-01), this is the only
        // size gate — there is no separate caller-declared size to reject.
        let db = test_db();
        let service = AttachmentService::new(&db);
        let conversation_id = create_conversation(&db);
        let id = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "nexora-attach-sparse-{}-{id}.bin",
            std::process::id()
        ));
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(u64::try_from(MAX_ATTACHMENT_BYTES + 1).expect("limit fits"))
            .expect("sparse size");

        let err = service
            .attach(
                conversation_id,
                "sparse.bin",
                path.to_string_lossy().as_ref(),
                None,
            )
            .expect_err("actual oversize must be denied");
        assert!(matches!(
            err,
            AttachmentError::InvalidInput {
                field: "file_size_bytes",
                reason: "exceeds the 20 MiB limit",
            }
        ));
        // The denial is fixed vocabulary: no path or size echoes.
        let rendered = format!("{err}");
        assert_eq!(
            rendered,
            "invalid file_size_bytes: exceeds the 20 MiB limit"
        );
        assert!(service.list(conversation_id).expect("list").is_empty());
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // Sensitive user-profile blocklist (SEC-02)
    // -----------------------------------------------------------------------

    /// Fabricated home used by the blocklist predicate tests: no filesystem
    /// access happens behind the predicate, so a non-existent user tree is
    /// sufficient. Both separators and mixed casing are exercised on purpose.
    fn fabricated_home() -> &'static str {
        if cfg!(windows) {
            r"C:\Users\Tester"
        } else {
            "/home/tester"
        }
    }

    /// The blocklist predicate must fire for a sensitive path under the
    /// fabricated home (or anywhere, for the dot-directory rule). Paths are
    /// passed as `Path`/`PathBuf` values, not raw string literals: on Unix a
    /// Windows-shaped literal like `r"D:\repo\.ssh"` parses as ONE relative
    /// component, so only component-built paths behave identically on every
    /// OS. Windows-only literals (drive letters, `\` separators) are gated
    /// behind `#[cfg(windows)]` at their call sites.
    fn sensitive(home: Option<&Path>, path: impl AsRef<Path>) -> bool {
        is_sensitive_profile_path(home, path.as_ref())
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_ssh_store() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(Some(home), home.join(".ssh").join("id_rsa")));
        // The dot-directory rule is home-independent: any root, any casing.
        // Built from components so it parses identically on every OS.
        assert!(sensitive(
            Some(home),
            PathBuf::from("repo").join(".SSH").join("known_hosts"),
        ));
        assert!(sensitive(Some(home), "/etc/dropbear/.SSH/authorized_keys"));
        // Drive-letter literal with `\` separators: only tokenizes into
        // components on Windows, so it is meaningless on Unix.
        #[cfg(windows)]
        assert!(sensitive(Some(home), r"D:\repo\.ssh\known_hosts"));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_aws_store() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(Some(home), home.join(".aws").join("credentials"),));
        // Case-insensitive dot-component match, root-independent, on every OS.
        assert!(sensitive(
            Some(home),
            PathBuf::from("work").join(".AWS").join("config"),
        ));
        // Drive-letter literal with `\` separators: only tokenizes into
        // components on Windows, so it is meaningless on Unix.
        #[cfg(windows)]
        assert!(sensitive(Some(home), r"E:\work\.AWS\config"));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_gnupg_keyring() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(
            Some(home),
            home.join(".gnupg").join("pubring.kbx"),
        ));
        // Casing of the sensitive component does not matter under the home
        // root (on Windows the fabricated home itself differs in case).
        assert!(sensitive(
            Some(home),
            home.join(".GNUPG").join("secring.gpg")
        ));
        // Drive-letter literal with `\` separators: only tokenizes into
        // components on Windows, so it is meaningless on Unix.
        #[cfg(windows)]
        assert!(sensitive(Some(home), r"C:\Users\tester\.GNUPG\secring.gpg"));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_kube_config() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(Some(home), home.join(".kube").join("config")));
        // Case-insensitive dot-component match, root-independent, on every OS.
        assert!(sensitive(
            Some(home),
            PathBuf::from("ops").join(".KUBE").join("admin.conf"),
        ));
        // Drive-letter literal with `\` separators: only tokenizes into
        // components on Windows, so it is meaningless on Unix.
        #[cfg(windows)]
        assert!(sensitive(Some(home), r"D:\ops\.KUBE\admin.conf"));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_chrome_user_data_dir() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(
            Some(home),
            format!(
                "{}/AppData/Local/Google/Chrome/User Data/Default/Cookies",
                fabricated_home()
            ),
        ));
        assert!(sensitive(
            Some(home),
            format!(
                "{}/.config/google-chrome/Default/Cookies",
                fabricated_home()
            ),
        ));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_chromium_user_data_dir() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(
            Some(home),
            format!(
                "{}/AppData/Local/Chromium/User Data/Default/Cookies",
                fabricated_home()
            ),
        ));
        assert!(sensitive(
            Some(home),
            format!("{}/.config/chromium/Default/Cookies", fabricated_home()),
        ));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_edge_user_data_dir() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(
            Some(home),
            format!(
                "{}/AppData/Local/Microsoft/Edge/User Data/Default/Login Data",
                fabricated_home()
            ),
        ));
        assert!(sensitive(
            Some(home),
            format!(
                "{}/.config/microsoft-edge/Default/Cookies",
                fabricated_home()
            ),
        ));
    }

    #[test]
    fn sensitive_path_blocklist_rejects_the_firefox_profile_dir() {
        let home = Path::new(fabricated_home());
        assert!(sensitive(
            Some(home),
            format!(
                "{}/AppData/Roaming/Mozilla/Firefox/Profiles/abc.default/logins.json",
                fabricated_home()
            ),
        ));
        assert!(sensitive(
            Some(home),
            format!(
                "{}/.mozilla/firefox/abc.default/logins.json",
                fabricated_home()
            ),
        ));
        // The snap-packaged layout is part of the Firefox family.
        assert!(sensitive(
            Some(home),
            format!(
                "{}/snap/firefox/common/.mozilla/firefox/abc.default/logins.json",
                fabricated_home()
            ),
        ));
    }

    #[test]
    fn sensitive_path_blocklist_admits_ordinary_user_files_and_near_misses() {
        let home = Path::new(fabricated_home());
        // Ordinary profile files stay attachable (the whole point of keeping
        // the `Users` tree out of the system blocklist).
        assert!(!sensitive(
            Some(home),
            format!("{}/Documents/notes.txt", fabricated_home())
        ));
        assert!(!sensitive(
            Some(home),
            format!("{}/AppData/Local/Temp/scratch.txt", fabricated_home())
        ));
        // Near-misses of the dot rule are not sensitive components.
        assert!(!sensitive(
            Some(home),
            format!("{}/.sshx/id_backup", fabricated_home())
        ));
        assert!(!sensitive(
            Some(home),
            format!("{}/src/aws_client.rs", fabricated_home())
        ));
        // A browser-profile shape outside the current user's home does not
        // match the home-relative rule (the dot-directory rule still applies
        // wherever a `.ssh`-style component appears). Built from components
        // so it parses identically on every OS.
        assert!(!sensitive(
            Some(home),
            PathBuf::from("OtherUsers")
                .join("Elsewhere")
                .join("AppData")
                .join("Local")
                .join("Google")
                .join("Chrome")
                .join("User Data")
                .join("Cookies"),
        ));
        // Drive-letter literal with `\` separators: only tokenizes into
        // components on Windows, so it is meaningless on Unix.
        #[cfg(windows)]
        assert!(!sensitive(
            Some(home),
            r"D:\OtherUsers\Elsewhere\AppData\Local\Google\Chrome\User Data\Cookies",
        ));
        // Without a resolvable home, only the dot rule fires.
        assert!(sensitive(
            None,
            PathBuf::from("any").join(".ssh").join("id_rsa"),
        ));
        assert!(!sensitive(
            None,
            PathBuf::from("any").join("Documents").join("notes.txt"),
        ));
        // Drive-letter literals with `\` separators: only tokenizes into
        // components on Windows, so they are meaningless on Unix.
        #[cfg(windows)]
        assert!(sensitive(None, r"C:\any\.ssh\id_rsa"));
        #[cfg(windows)]
        assert!(!sensitive(None, r"C:\any\Documents\notes.txt"));
    }

    #[test]
    fn attachment_path_rejects_a_real_sensitive_dot_directory_file() {
        // End-to-end through the resolver: a real file under a `.ssh`
        // component is refused with the fixed reason, never the path.
        let id = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let outer = std::env::temp_dir().join(format!(
            "nexora-attach-blocklist-{}-{id}.d",
            std::process::id()
        ));
        let dir = outer.join(".ssh");
        std::fs::create_dir_all(&dir).expect("create .ssh test dir");
        let file = dir.join("id_blocklist_test");
        std::fs::write(&file, b"not a real key").expect("write test file");

        let (field, reason) = match resolve_attachment_file_path(file.to_string_lossy().as_ref()) {
            Err(AttachmentError::InvalidInput { field, reason }) => (field, reason),
            other => panic!("expected InvalidInput, got {other:?}"),
        };
        assert_eq!(field, "file_path");
        assert_eq!(reason, "a sensitive user-profile location is not allowed");
        // The refusal is secret-free: no path fragment, no file content.
        assert!(!reason.contains(".ssh"));
        assert!(!reason.contains("id_blocklist_test"));

        let _ = std::fs::remove_dir_all(&outer);
    }
}
