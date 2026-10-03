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

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::export::{EXPORT_FORMAT, EXPORT_VERSION, SETUP_FORMAT, SETUP_VERSION};
use super::flags::{is_known_flag_setting, parse_global_bool};
use super::routing::{
    is_valid_custom_model_id, RoutingProfile, AGENT_PROFILE_KEY, CHAT_PROFILE_KEY,
};
use super::settings::SettingsService;
use super::workspace::{
    parse_recent, WORKSPACE_RECENT_KEY, WORKSPACE_RECENT_MAX, WORKSPACE_ROOT_KEY,
    WORKSPACE_ROOT_MAX_LEN,
};
use crate::application::agent::injection::contains_secret;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::providers::supported_providers;
use crate::infrastructure::repository::conversations::ConversationRepository;
use crate::infrastructure::repository::messages::MessageRepository;
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::Repository;

/// Hard cap on an import document (bytes). Pinned from the bounded-read
/// family: the 64 KiB [`MAX_ERROR_BODY_BYTES`](crate::infrastructure::providers::transport::MAX_ERROR_BODY_BYTES)
/// sniff precedent for hostile input, scaled to a document budget — the full
/// bounded input is secrets-scanned, so the scan cost stays bounded too.
pub(crate) const MAX_IMPORT_JSON_BYTES: usize = 16 * 1024 * 1024;

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

// ---------------------------------------------------------------------------
// Setup import: VS Code settings + MCP servers + setup documents (WS-E.2)
// ---------------------------------------------------------------------------
//
// Reads a VS Code `settings.json` and an MCP servers JSON file, translates
// the mappable subset into Nexora settings/routing keys, and re-imports the
// portable setup documents written by
// [`SetupExportService`](crate::application::export::SetupExportService) —
// all through the hardened import/export paths (the [`MAX_IMPORT_JSON_BYTES`]
// cap, the [`contains_secret`] denial, secret-free [`ImportError`]s, and the
// settings store; no new tables).
//
// # Mapping table (hardcoded, documented)
//
// Every writable Nexora settings key (the settings-command gate in
// `commands/settings.rs`) was inspected for a genuine VS Code counterpart.
// Exactly one survives; everything else is reported as skipped, never
// guessed:
//
// | VS Code key | Nexora key | Translation |
// |---|---|---|
// | `workbench.colorTheme` | `appearance.theme` | Unambiguous dark/light
// mention → `dark` / `light` ([`translate_vscode_theme`]); anything else is
// denied as an unsupported value. |
// | `editor.*`, `files.*`, all other keys | — | Skipped: no Nexora
// counterpart exists (no editor font/tab settings, no autosave concept, no
// VS Code provider/model/autonomy/workspace/flag equivalents). |
//
// Considered but rejected: `editor.fontSize` / `editor.tabSize` (Nexora has no
// editor settings at all — inventing keys would be speculative), and any
// theme-name guessing beyond the unambiguous dark/light mention (a theme like
// `Monokai` names no implemented Nexora theme, so mapping it would be a lie).
//
// # MCP storage map
//
// Validated `mcpServers` entries are stored as one JSON array under
// [`MCP_SERVERS_KEY`] in the existing generic settings store (FR-012) —
// `command` + `args` + environment names and values verbatim, except
// secret-like environment material (see below), which denies that server
// entry. The stored shape is validated by [`McpServerList::from_json`], the
// single source shared with the settings-command gate, so both paths agree
// exactly. Later imports replace the stored list (same semantics as
// `set_setting`, never a merge).
//
// # Report secrecy rule
//
// [`SetupImportReport`] echoes caller-supplied *key names* only — the
// checkpoint-label rule from the run snapshots
// (`application/agent/snapshots.rs`): caller-chosen labels may appear in
// read-only views but never in errors. *Values* are never echoed anywhere:
// per-entry denials carry fixed-vocabulary reasons
// ([`DENY_UNSUPPORTED_VALUE`], [`DENY_SECRET_VALUE`], [`DENY_INVALID_RECORD`])
// and every [`ImportError`] display path is fixed vocabulary or a bare
// structural label, exactly like the conversation import errors.
//
// # Hostile input handling
//
// Beyond the shared gates, caller-supplied keys longer than
// [`MAX_SOURCE_KEY_LEN`] deny the whole document with fixed vocabulary (the
// report must not parrot arbitrarily large caller content), and deeply nested
// documents trip `serde_json`'s recursion limit into [`ImportError::InvalidJson`].

/// Settings key holding the validated MCP server list (a JSON array of
/// [`McpServerEntry`]) in the existing generic settings store (FR-012).
/// Accepted by the settings-command gate exactly when
/// [`McpServerList::from_json`] accepts the value, so both paths agree.
pub(crate) const MCP_SERVERS_KEY: &str = "mcp.servers";

/// Value bound mirrored from the `app_settings` CHECK (DATABASE.md §7.6):
/// `length(value) <= 10000`. Any storable settings value — including the
/// serialized [`McpServerList`] — must fit inside it.
pub(crate) const SETTINGS_VALUE_MAX_LEN: usize = 10_000;

/// Cap (bytes) on a caller-supplied source key (VS Code dotted key, MCP
/// server name, setup key) echoed in a [`SetupImportReport`]. Longer keys
/// deny the whole document with fixed vocabulary instead of being echoed.
const MAX_SOURCE_KEY_LEN: usize = 512;

/// Maximum MCP servers accepted in one import document or one stored list.
pub(crate) const MAX_MCP_SERVERS: usize = 32;

/// Maximum characters in one MCP server name.
const MAX_MCP_NAME_LEN: usize = 128;

/// Maximum characters in one MCP server command.
const MAX_MCP_COMMAND_LEN: usize = 1024;

/// Maximum arguments accepted on one MCP server entry.
const MAX_MCP_ARGS: usize = 64;

/// Maximum characters in one MCP server argument.
const MAX_MCP_ARG_LEN: usize = 1024;

/// Maximum environment variables accepted on one MCP server entry.
const MAX_MCP_ENV_VARS: usize = 64;

/// Maximum characters in one MCP environment variable name.
const MAX_MCP_ENV_NAME_LEN: usize = 128;

/// Maximum characters in one MCP environment variable value.
const MAX_MCP_ENV_VALUE_LEN: usize = 4096;

// The four key strings below mirror the gate-owned constants in
// `commands/settings.rs` (`THEME_KEY`, `SELECTED_PROVIDER_KEY`,
// `SELECTED_MODEL_KEY`, `AUTONOMY_KEY`). The routing, flags, and workspace
// keys are reused from their application owners instead of being duplicated.
// The `gate_agrees_with_setup_import_allowlist` test in
// `commands/settings.rs` pins both paths to identical verdicts — update it
// with any change here.
const APPEARANCE_THEME_KEY: &str = "appearance.theme";
const SELECTED_PROVIDER_KEY: &str = "provider.selected";
const SELECTED_MODEL_KEY: &str = "provider.model";
const AUTONOMY_KEY: &str = "agent.autonomy";

/// VS Code `settings.json` keys with a genuine Nexora counterpart:
/// `(source key, Nexora settings key)`. See the mapping table in the
/// section docs above: today exactly one entry survives inspection.
const VSCODE_SETTING_MAP: &[(&str, &str)] = &[("workbench.colorTheme", APPEARANCE_THEME_KEY)];

/// Fixed-vocabulary denial reason: the source key maps to Nexora, but its
/// value is outside the Nexora domain (never the offending value).
pub(crate) const DENY_UNSUPPORTED_VALUE: &str = "unsupported value";

/// Fixed-vocabulary denial reason: the entry carries secret-like material
/// (never the offending value).
pub(crate) const DENY_SECRET_VALUE: &str = "secret-like value denied";

/// Fixed-vocabulary denial reason: the entry is structurally invalid
/// (never the offending content).
pub(crate) const DENY_INVALID_RECORD: &str = "invalid record";

/// Translate a VS Code color-theme name to the implemented Nexora appearance
/// theme: an unambiguous dark/light mention (case-insensitive) maps to
/// `dark` / `light`; anything else (a theme naming neither, or confusingly
/// both) yields [`None`] so the caller denies the key instead of guessing.
fn translate_vscode_theme(value: &str) -> Option<&'static str> {
    let lowered = value.to_lowercase();
    match (lowered.contains("dark"), lowered.contains("light")) {
        (true, false) => Some("dark"),
        (false, true) => Some("light"),
        _ => None,
    }
}

/// One successfully translated key: `(source key, Nexora key)`. Key names
/// are caller-file content echoed in a read-only view (checkpoint-label
/// rule); values are never carried here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ImportedEntry {
    /// The caller-supplied source key (VS Code dotted key,
    /// `mcpServers.<name>`, or setup key).
    pub source_key: String,
    /// The Nexora settings key written.
    pub nexora_key: String,
}

/// One rejected key: the source key plus a fixed-vocabulary reason
/// ([`DENY_UNSUPPORTED_VALUE`], [`DENY_SECRET_VALUE`],
/// [`DENY_INVALID_RECORD`]). Values are never carried here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DeniedEntry {
    /// The caller-supplied source key.
    pub source_key: String,
    /// Fixed, secret-free reason category.
    pub reason: &'static str,
}

/// The per-key outcome of a setup import (WS-E.2): translated keys land in
/// `imported`, keys with no Nexora counterpart land in `skipped`, and mapped
/// keys with unusable values land in `denied`. Secret-free by construction:
/// key names only, fixed-vocabulary reasons, never values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct SetupImportReport {
    /// Successfully translated and stored keys.
    pub imported: Vec<ImportedEntry>,
    /// Source keys with no Nexora counterpart (never guessed).
    pub skipped: Vec<String>,
    /// Mapped keys whose values could not be stored, with fixed reasons.
    pub denied: Vec<DeniedEntry>,
}

/// One validated MCP server: a command to launch plus its arguments and
/// environment. Identifiers and plain configuration only — secret-like
/// environment material is denied at validation, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct McpServerEntry {
    /// Server name (the `mcpServers` object key).
    pub name: String,
    /// Executable command for the server.
    pub command: String,
    /// Command arguments, verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables, names to values (non-secret only).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl McpServerEntry {
    /// Validate one server record, returning a fixed-vocabulary denial
    /// reason on the first problem. Structural bounds keep the record
    /// storable; the [`contains_secret`] scan over every field keeps
    /// secret-like material out of the settings store (the predicate returns
    /// only `bool`, so a detected secret can never echo).
    fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.name.is_empty()
            || self.name.len() > MAX_MCP_NAME_LEN
            || self.name.contains("..")
            || self.name.chars().any(char::is_control)
        {
            return Err(DENY_INVALID_RECORD);
        }
        if contains_secret(&self.name) || contains_secret(&self.command) {
            return Err(DENY_SECRET_VALUE);
        }
        if self.command.is_empty() || self.command.len() > MAX_MCP_COMMAND_LEN {
            return Err(DENY_INVALID_RECORD);
        }
        if self.args.len() > MAX_MCP_ARGS {
            return Err(DENY_INVALID_RECORD);
        }
        for arg in &self.args {
            if arg.len() > MAX_MCP_ARG_LEN {
                return Err(DENY_INVALID_RECORD);
            }
            if contains_secret(arg) {
                return Err(DENY_SECRET_VALUE);
            }
        }
        if self.env.len() > MAX_MCP_ENV_VARS {
            return Err(DENY_INVALID_RECORD);
        }
        for (name, value) in &self.env {
            if name.is_empty()
                || name.len() > MAX_MCP_ENV_NAME_LEN
                || name.chars().any(char::is_control)
            {
                return Err(DENY_INVALID_RECORD);
            }
            if value.len() > MAX_MCP_ENV_VALUE_LEN {
                return Err(DENY_INVALID_RECORD);
            }
            if contains_secret(name) || contains_secret(value) {
                return Err(DENY_SECRET_VALUE);
            }
        }
        Ok(())
    }
}

/// The validated MCP server list stored under [`MCP_SERVERS_KEY`]: a JSON
/// array of [`McpServerEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) struct McpServerList {
    /// Validated servers in import order.
    pub servers: Vec<McpServerEntry>,
}

impl McpServerList {
    /// Parse a stored MCP server list, rejecting malformed or out-of-domain
    /// payloads with secret-free [`ImportError`]s.
    ///
    /// Single source shared by the settings-command gate and the setup
    /// import, so both paths classify stored values identically (structural
    /// bounds, duplicate names, the [`contains_secret`] scan, and the
    /// [`SETTINGS_VALUE_MAX_LEN`] fit are all enforced here).
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// or [`ImportError::InvalidData`] with fixed vocabulary when the payload
    /// violates the structural bounds or carries secret-like material.
    pub(crate) fn from_json(raw: &str) -> Result<Self> {
        ensure_valid(
            raw.len() <= SETTINGS_VALUE_MAX_LEN,
            "stored MCP server list exceeds the settings value limit",
        )?;
        let servers: Vec<McpServerEntry> = serde_json::from_str(raw).map_err(classify_error)?;
        let list = Self { servers };
        list.validate()?;
        Ok(list)
    }

    /// Serialize a validated list for the settings store, enforcing the
    /// [`SETTINGS_VALUE_MAX_LEN`] fit so the value always satisfies the
    /// `app_settings` CHECK (DATABASE.md §7.6).
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidData`] with fixed vocabulary when the
    /// list violates the structural bounds or the serialized form does not
    /// fit the settings value limit.
    pub(crate) fn to_json(&self) -> Result<String> {
        self.validate()?;
        let raw = serde_json::to_string(&self.servers).map_err(classify_error)?;
        ensure_valid(
            raw.len() <= SETTINGS_VALUE_MAX_LEN,
            "MCP server list exceeds the settings value limit",
        )?;
        Ok(raw)
    }

    /// Check structural bounds: at most [`MAX_MCP_SERVERS`] entries, unique
    /// names, and every entry passing [`McpServerEntry::validate`].
    fn validate(&self) -> Result<()> {
        ensure_valid(
            self.servers.len() <= MAX_MCP_SERVERS,
            "MCP server list carries too many entries",
        )?;
        let mut names = HashSet::new();
        for server in &self.servers {
            ensure_valid(
                names.insert(server.name.as_str()),
                "MCP server list repeats a server name",
            )?;
            if let Err(reason) = server.validate() {
                return Err(ImportError::InvalidData(reason.to_string()));
            }
        }
        Ok(())
    }
}

/// A setup document decoded from JSON: the portable format written by
/// [`SetupExportService`](crate::application::export::SetupExportService).
/// `settings` maps each key to its value ([`None`] for `NULL`, which clears
/// the key back to its default on import).
#[derive(Debug, Deserialize)]
struct SetupDocument {
    /// Document kind marker; must equal
    /// [`SETUP_FORMAT`](crate::application::export::SETUP_FORMAT).
    format: String,
    /// Document layout version; must equal
    /// [`SETUP_VERSION`](crate::application::export::SETUP_VERSION).
    version: i64,
    /// The exported settings rows.
    settings: BTreeMap<String, Option<String>>,
}

/// Whether `key` may be written by a setup import at all: the eight
/// gate-owned settings keys, the two routing profile keys, the registered
/// `flags.*` keys, and [`MCP_SERVERS_KEY`]. Anything else is skipped, never
/// stored.
fn is_allowlisted_key(key: &str) -> bool {
    matches!(
        key,
        APPEARANCE_THEME_KEY
            | SELECTED_PROVIDER_KEY
            | SELECTED_MODEL_KEY
            | AUTONOMY_KEY
            | WORKSPACE_ROOT_KEY
            | WORKSPACE_RECENT_KEY
            | CHAT_PROFILE_KEY
            | AGENT_PROFILE_KEY
            | MCP_SERVERS_KEY
    ) || is_known_flag_setting(key)
}

/// Strict `agent.workspace_recent` domain check, mirroring the
/// settings-command gate exactly: the value must be a JSON array of at most
/// [`WORKSPACE_RECENT_MAX`] non-empty strings within
/// [`WORKSPACE_ROOT_MAX_LEN`] that round-trips through [`parse_recent`].
fn is_valid_recent_value(value: &str) -> bool {
    let items = parse_recent(Some(value));
    match serde_json::from_str::<Vec<String>>(value) {
        Ok(list) => {
            list.len() <= WORKSPACE_RECENT_MAX
                && list
                    .iter()
                    .all(|s| !s.trim().is_empty() && s.len() <= WORKSPACE_ROOT_MAX_LEN)
                && items.len() == list.len()
        }
        Err(_) => false,
    }
}

/// Classify an allowlisted `(key, value)` pair: [`None`] when the value may
/// be stored, or the fixed-vocabulary denial reason otherwise. Domain logic
/// mirrors the settings-command gate key for key (theme/provider/model/
/// autonomy values, workspace syntax, [`RoutingProfile::from_json`], the
/// [`parse_global_bool`] flag domain, [`McpServerList::from_json`]);
/// [`is_importable_setting`] is the boolean projection both the setup import
/// and the gate-agreement test use.
fn setup_denial_reason(key: &str, value: &str) -> Option<&'static str> {
    if value.len() > SETTINGS_VALUE_MAX_LEN {
        return Some(DENY_INVALID_RECORD);
    }
    match key {
        APPEARANCE_THEME_KEY => {
            if matches!(value, "dark" | "light") {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        SELECTED_PROVIDER_KEY => {
            if supported_providers().iter().any(|p| p.name == value) {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        SELECTED_MODEL_KEY => {
            if supported_providers()
                .iter()
                .any(|p| p.models.iter().any(|m| m == value))
                || is_valid_custom_model_id(value)
            {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        AUTONOMY_KEY => {
            if matches!(value, "supervised" | "semi_autonomous" | "full_autonomous") {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        WORKSPACE_ROOT_KEY => {
            if !value.trim().is_empty()
                && !value.contains('\0')
                && value.len() <= WORKSPACE_ROOT_MAX_LEN
            {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        WORKSPACE_RECENT_KEY => {
            if is_valid_recent_value(value) {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        CHAT_PROFILE_KEY | AGENT_PROFILE_KEY => {
            if RoutingProfile::from_json(value).is_ok() {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        MCP_SERVERS_KEY => {
            if McpServerList::from_json(value).is_ok() {
                None
            } else {
                Some(DENY_INVALID_RECORD)
            }
        }
        _ if is_known_flag_setting(key) => {
            if parse_global_bool(value).is_some() {
                None
            } else {
                Some(DENY_UNSUPPORTED_VALUE)
            }
        }
        _ => None,
    }
}

/// Whether `(key, value)` may be stored by a setup import: allowlisted key
/// ([`is_allowlisted_key`]) with an in-domain value
/// ([`setup_denial_reason`] reporting no reason).
///
/// Single boolean projection shared by the setup import and the
/// `gate_agrees_with_setup_import_allowlist` test in `commands/settings.rs`,
/// which pins it to the settings-command gate verdict for every sampled
/// input.
///
/// # Panics
///
/// Never panics: all checks are bounded string predicates.
#[must_use]
pub(crate) fn is_importable_setting(key: &str, value: &str) -> bool {
    is_allowlisted_key(key) && setup_denial_reason(key, value).is_none()
}

/// Deny an oversized caller-supplied source key with fixed vocabulary before
/// it can be echoed into a [`SetupImportReport`].
fn check_source_key(key: &str) -> Result<()> {
    ensure_valid(
        key.len() <= MAX_SOURCE_KEY_LEN,
        "import document carries an oversized key",
    )
}

/// Decode one `mcpServers` entry into a validated [`McpServerEntry`],
/// returning the fixed-vocabulary denial reason for the report when the
/// entry is unusable. Unknown fields are ignored (forward compatibility);
/// `args` and `env` default to empty when absent.
fn parse_mcp_entry(
    name: &str,
    def: &serde_json::Value,
) -> std::result::Result<McpServerEntry, &'static str> {
    let object = def.as_object().ok_or(DENY_INVALID_RECORD)?;
    let command = object
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or(DENY_INVALID_RECORD)?;
    let mut args = Vec::new();
    if let Some(raw_args) = object.get("args") {
        let list = raw_args.as_array().ok_or(DENY_INVALID_RECORD)?;
        for arg in list {
            args.push(arg.as_str().ok_or(DENY_INVALID_RECORD)?.to_string());
        }
    }
    let mut env = BTreeMap::new();
    if let Some(raw_env) = object.get("env") {
        let map = raw_env.as_object().ok_or(DENY_INVALID_RECORD)?;
        for (key, val) in map {
            env.insert(
                key.clone(),
                val.as_str().ok_or(DENY_INVALID_RECORD)?.to_string(),
            );
        }
    }
    let entry = McpServerEntry {
        name: name.to_string(),
        command: command.to_string(),
        args,
        env,
    };
    entry.validate()?;
    Ok(entry)
}

/// Application-layer service that imports VS Code settings, MCP servers,
/// and Nexora setup documents into the existing settings store (WS-E.2).
///
/// Every document is fully decoded and validated before any write, so an
/// invalid document performs no writes at all. Settings keys are
/// independent rows with no cross-key invariants, so multi-key imports apply
/// sequentially once validated (unlike the conversation import, no single
/// transaction is needed for coherence). All persistence is delegated to
/// [`SettingsService`]: no SQL, no new tables.
pub(crate) struct SetupImportService<'a> {
    settings: SettingsService<'a>,
}

impl<'a> SetupImportService<'a> {
    /// Create a setup import service over the shared application [`Database`].
    pub(crate) fn new(db: &'a Database) -> Self {
        Self {
            settings: SettingsService::new(db),
        }
    }

    /// Import a VS Code `settings.json` document (WS-E.2).
    ///
    /// The size cap and the whole-document [`contains_secret`] scan fire
    /// before any parse or write (VS Code settings never legitimately carry
    /// key-like material). Mapped keys are translated per
    /// [`VSCODE_SETTING_MAP`] and stored; unmapped keys are skipped; mapped
    /// keys with unusable values are denied with fixed vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::InvalidData`] when the document is oversized, carries
    /// key-like material or an oversized key, or is not a JSON object, or
    /// [`ImportError::Database`] when a settings write fails.
    pub(crate) fn import_vscode(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        ensure_valid(
            !contains_secret(json),
            "import document contains key-like material",
        )?;
        let value: serde_json::Value = serde_json::from_str(json).map_err(classify_error)?;
        let object = value.as_object().ok_or_else(|| {
            ImportError::InvalidData("VS Code settings document is not a JSON object".to_string())
        })?;
        let mut report = SetupImportReport::default();
        for (source_key, raw) in object {
            check_source_key(source_key)?;
            let Some(nexora_key) = VSCODE_SETTING_MAP
                .iter()
                .find(|(from, _)| *from == source_key)
                .map(|(_, to)| *to)
            else {
                report.skipped.push(source_key.clone());
                continue;
            };
            // Fail closed on future table rows: only wired targets translate.
            let translated = match nexora_key {
                APPEARANCE_THEME_KEY => raw.as_str().and_then(translate_vscode_theme),
                _ => None,
            };
            let Some(translated) = translated else {
                report.denied.push(DeniedEntry {
                    source_key: source_key.clone(),
                    reason: DENY_UNSUPPORTED_VALUE,
                });
                continue;
            };
            self.settings.write(nexora_key, Some(translated))?;
            report.imported.push(ImportedEntry {
                source_key: source_key.clone(),
                nexora_key: nexora_key.to_string(),
            });
        }
        Ok(report)
    }

    /// Import an MCP servers document (`{ "mcpServers": { name: {
    /// `command`, `args`, `env` } } }`, WS-E.2).
    ///
    /// The size cap fires before any parse or write. Unlike
    /// [`Self::import_vscode`], no whole-document [`contains_secret`] scan
    /// applies: `env` blocks legitimately carry key names such as `API_KEY`,
    /// so a whole-document scan would deny the primary use case. Each entry
    /// is scanned with the same predicate instead
    /// ([`McpServerEntry::validate`]): secret-like entries are denied per
    /// server with a secret-free reason while valid servers still import. The
    /// accepted list replaces any previously stored list.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::InvalidData`] when the document is oversized, carries
    /// an oversized key, is not an `mcpServers` object, names too many
    /// servers, or the accepted list does not fit the settings value limit,
    /// or [`ImportError::Database`] when the settings write fails.
    pub(crate) fn import_mcp(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        let value: serde_json::Value = serde_json::from_str(json).map_err(classify_error)?;
        let servers = value
            .get("mcpServers")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ImportError::InvalidData("MCP document is not an mcpServers object".to_string())
            })?;
        ensure_valid(
            servers.len() <= MAX_MCP_SERVERS,
            "MCP document carries too many server entries",
        )?;
        let mut accepted = Vec::new();
        let mut report = SetupImportReport::default();
        for (name, def) in servers {
            check_source_key(name)?;
            let source_key = format!("mcpServers.{name}");
            match parse_mcp_entry(name, def) {
                Ok(entry) => accepted.push(entry),
                Err(reason) => report.denied.push(DeniedEntry { source_key, reason }),
            }
        }
        let list = McpServerList { servers: accepted };
        let raw = list.to_json()?;
        if !list.servers.is_empty() {
            self.settings.write(MCP_SERVERS_KEY, Some(&raw))?;
        }
        for server in &list.servers {
            report.imported.push(ImportedEntry {
                source_key: format!("mcpServers.{}", server.name),
                nexora_key: MCP_SERVERS_KEY.to_string(),
            });
        }
        Ok(report)
    }

    /// Import a portable Nexora setup document written by
    /// [`SetupExportService`](crate::application::export::SetupExportService)
    /// (WS-E.2).
    ///
    /// The size cap and the whole-document [`contains_secret`] scan fire
    /// before any parse or write (setup documents never legitimately carry
    /// key-like material: credentials live in the OS keyring, never in
    /// settings). Allowlisted keys with in-domain values are written ([`None`]
    /// values delete the key, restoring its default); unknown keys are
    /// skipped; allowlisted keys with out-of-domain values are denied with
    /// fixed vocabulary.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::InvalidJson`] when the input is not valid JSON,
    /// [`ImportError::UnsupportedFormat`] or [`ImportError::UnsupportedVersion`]
    /// for a non-setup document, [`ImportError::InvalidData`] when the
    /// document is oversized, carries key-like material or an oversized key,
    /// or violates the setup shape, or [`ImportError::Database`] when a
    /// settings write fails.
    pub(crate) fn import_setup(&self, json: &str) -> Result<SetupImportReport> {
        ensure_valid(
            json.len() <= MAX_IMPORT_JSON_BYTES,
            "import document exceeds the 16 MiB size limit",
        )?;
        ensure_valid(
            !contains_secret(json),
            "import document contains key-like material",
        )?;
        let document: SetupDocument = serde_json::from_str(json).map_err(classify_error)?;
        if document.format != SETUP_FORMAT {
            return Err(ImportError::UnsupportedFormat {
                format: document.format.clone(),
            });
        }
        if document.version != SETUP_VERSION {
            return Err(ImportError::UnsupportedVersion {
                version: document.version,
            });
        }
        let mut report = SetupImportReport::default();
        for (key, value) in &document.settings {
            check_source_key(key)?;
            if !is_allowlisted_key(key) {
                report.skipped.push(key.clone());
                continue;
            }
            let Some(stored) = value.as_deref() else {
                self.settings.delete(key)?;
                report.imported.push(ImportedEntry {
                    source_key: key.clone(),
                    nexora_key: key.clone(),
                });
                continue;
            };
            if let Some(reason) = setup_denial_reason(key, stored) {
                report.denied.push(DeniedEntry {
                    source_key: key.clone(),
                    reason,
                });
                continue;
            }
            self.settings.write(key, Some(stored))?;
            report.imported.push(ImportedEntry {
                source_key: key.clone(),
                nexora_key: key.clone(),
            });
        }
        Ok(report)
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
}

#[cfg(test)]
mod setup_import_tests {
    use super::*;
    use crate::application::export::SetupExportService;
    use crate::infrastructure::database::in_memory_database;

    const SECRET_SENTINELS: [&str; 3] = ["sk-live-sentinel-42", "sk-", "api_key"];

    fn setup_db() -> Database {
        in_memory_database()
    }

    /// Assert `rendered` echoes none of the hostile sentinels (case-insensitive).
    fn assert_secret_free(rendered: &str) {
        for sentinel in SECRET_SENTINELS {
            assert!(
                !rendered.to_lowercase().contains(sentinel),
                "output must stay secret-free, found {sentinel:?} in {rendered:?}"
            );
        }
    }

    #[test]
    fn vscode_theme_mapping_hits_dark_and_light() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "workbench.colorTheme": "Default Dark Modern",
        })
        .to_string();

        let report = service.import_vscode(&json).expect("theme import succeeds");

        assert_eq!(
            report.imported,
            vec![ImportedEntry {
                source_key: "workbench.colorTheme".to_string(),
                nexora_key: "appearance.theme".to_string(),
            }]
        );
        assert!(report.skipped.is_empty());
        assert!(report.denied.is_empty());
        assert_eq!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme"),
            Some("dark".to_string())
        );

        // A light theme overwrites through the same mapped key.
        let json = serde_json::json!({ "workbench.colorTheme": "Default Light+" }).to_string();
        let report = service.import_vscode(&json).expect("light import succeeds");
        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme"),
            Some("light".to_string())
        );
    }

    #[test]
    fn vscode_unmapped_keys_are_skipped_and_store_nothing() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "editor.fontSize": 14,
            "editor.tabSize": 4,
            "files.autoSave": "afterDelay",
            "workbench.startupEditor": "newUntitledFile",
        })
        .to_string();

        let report = service
            .import_vscode(&json)
            .expect("unmapped import succeeds");

        assert!(report.imported.is_empty());
        assert!(report.denied.is_empty());
        let mut skipped = report.skipped.clone();
        skipped.sort();
        assert_eq!(
            skipped,
            vec![
                "editor.fontSize".to_string(),
                "editor.tabSize".to_string(),
                "files.autoSave".to_string(),
                "workbench.startupEditor".to_string(),
            ]
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "skipped keys store nothing"
        );
    }

    #[test]
    fn vscode_unsupported_theme_value_is_denied_secret_free() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        // Names neither implemented theme: mappable key, unusable value.
        let json = serde_json::json!({ "workbench.colorTheme": "Monokai Dimmed" }).to_string();

        let report = service
            .import_vscode(&json)
            .expect("denial lands in the report");

        assert!(report.imported.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "workbench.colorTheme".to_string(),
                reason: DENY_UNSUPPORTED_VALUE,
            }]
        );
        // The report echoes the key name (caller content, checkpoint-label
        // rule) but never the value.
        let rendered = format!("{report:?}");
        assert!(rendered.contains("workbench.colorTheme"));
        assert!(!rendered.contains("Monokai Dimmed"));
        assert!(
            SettingsService::new(&db)
                .read("appearance.theme")
                .expect("read theme")
                .is_none(),
            "denied values store nothing"
        );
    }

    #[test]
    fn vscode_secret_bearing_document_is_denied_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "workbench.colorTheme": "Default Dark",
            "http.proxyPassword": "sk-live-sentinel-42",
        })
        .to_string();

        let err = service
            .import_vscode(&json)
            .expect_err("secret must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("key-like")),
            "secret denial must use fixed vocabulary, got {err:?}"
        );
        assert_secret_free(&format!("{err}"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied documents write nothing"
        );
    }

    #[test]
    fn vscode_oversize_document_is_denied_before_any_write() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let padding = "x".repeat(MAX_IMPORT_JSON_BYTES);
        let json = serde_json::json!({ "workbench.colorTheme": padding }).to_string();
        assert!(json.len() > MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_vscode(&json)
            .expect_err("oversize must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("16 MiB")),
            "oversize denial must name the pinned cap, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "oversize documents write nothing"
        );
    }

    #[test]
    fn vscode_hostile_shapes_are_handled_by_the_gates() {
        let db = setup_db();
        let service = SetupImportService::new(&db);

        // A non-object document is invalid data.
        let err = service.import_vscode("[]").expect_err("array must fail");
        assert!(matches!(err, ImportError::InvalidData(_)));

        // Deep nesting trips the JSON recursion limit into invalid JSON.
        let mut nested = "null".to_string();
        for _ in 0..300 {
            nested = format!("{{\"k\":{nested}}}");
        }
        let err = service
            .import_vscode(&nested)
            .expect_err("deep nesting must fail");
        assert!(matches!(err, ImportError::InvalidJson(_)));

        // A huge key denies the whole document with fixed vocabulary rather
        // than being echoed into the report.
        let huge_key = "k".repeat(MAX_SOURCE_KEY_LEN + 1);
        let json = format!("{{\"{huge_key}\": 1}}");
        let err = service
            .import_vscode(&json)
            .expect_err("huge key must fail");
        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("oversized key")),
            "huge keys deny the document, got {err:?}"
        );
        assert!(!format!("{err}").contains(&huge_key));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "hostile documents write nothing"
        );
    }

    #[test]
    fn mcp_import_stores_validated_servers() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "github-mcp"],
                    "env": { "PORT": "8080" },
                },
                "notes": { "command": "/usr/local/bin/notes-mcp" },
            },
        })
        .to_string();

        let report = service.import_mcp(&json).expect("mcp import succeeds");

        assert_eq!(report.skipped.len(), 0);
        assert_eq!(report.denied.len(), 0);
        assert_eq!(report.imported.len(), 2);
        for entry in &report.imported {
            assert_eq!(entry.nexora_key, MCP_SERVERS_KEY);
            assert!(entry.source_key.starts_with("mcpServers."));
        }
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("servers stored");
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 2);
        let github = list
            .servers
            .iter()
            .find(|server| server.name == "github")
            .expect("github stored");
        assert_eq!(github.command, "npx");
        assert_eq!(github.args, vec!["-y", "github-mcp"]);
        assert_eq!(github.env.get("PORT").map(String::as_str), Some("8080"));
    }

    #[test]
    fn mcp_secret_env_denies_only_that_server_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "clean": { "command": "clean-mcp" },
                "leaky": {
                    "command": "leaky-mcp",
                    "env": { "GITHUB_TOKEN": "sk-live-sentinel-42" },
                },
            },
        })
        .to_string();

        let report = service
            .import_mcp(&json)
            .expect("partial mcp import succeeds");

        assert_eq!(report.imported.len(), 1);
        assert_eq!(report.imported[0].source_key, "mcpServers.clean");
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "mcpServers.leaky".to_string(),
                reason: DENY_SECRET_VALUE,
            }]
        );
        assert_secret_free(&format!("{report:?}"));
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("clean server stored");
        assert_secret_free(&raw);
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].name, "clean");
    }

    #[test]
    fn mcp_traversal_name_is_denied_and_stores_nothing_for_it() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "mcpServers": {
                "../evil": { "command": "evil-mcp" },
                "fine": { "command": "fine-mcp" },
            },
        })
        .to_string();

        let report = service
            .import_mcp(&json)
            .expect("partial mcp import succeeds");

        assert_eq!(report.imported.len(), 1);
        assert_eq!(
            report.denied,
            vec![DeniedEntry {
                source_key: "mcpServers.../evil".to_string(),
                reason: DENY_INVALID_RECORD,
            }]
        );
        let raw = SettingsService::new(&db)
            .read(MCP_SERVERS_KEY)
            .expect("read servers")
            .expect("fine server stored");
        let list = McpServerList::from_json(&raw).expect("stored list validates");
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].name, "fine");
    }

    #[test]
    fn mcp_oversize_document_is_denied_before_any_write() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let padding = "x".repeat(MAX_IMPORT_JSON_BYTES);
        let json = serde_json::json!({
            "mcpServers": { "big": { "command": padding } },
        })
        .to_string();
        assert!(json.len() > MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_mcp(&json)
            .expect_err("oversize must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("16 MiB")),
            "oversize denial must name the pinned cap, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "oversize documents write nothing"
        );
    }

    #[test]
    fn mcp_aggregate_overflow_denies_the_document() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        // Enough medium servers to pass every per-server bound while the
        // serialized list no longer fits the settings value limit.
        let mut servers = serde_json::Map::new();
        for index in 0..MAX_MCP_SERVERS {
            servers.insert(
                format!("srv-{index:02}"),
                serde_json::json!({ "command": "c".repeat(300) }),
            );
        }
        let json = serde_json::json!({ "mcpServers": servers }).to_string();
        assert!(json.len() <= MAX_IMPORT_JSON_BYTES);

        let err = service
            .import_mcp(&json)
            .expect_err("aggregate overflow must fail");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("value limit")),
            "aggregate overflow must name the value limit, got {err:?}"
        );
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "overflowed documents write nothing"
        );
    }

    #[test]
    fn setup_round_trip_export_import_is_stable() {
        let db = setup_db();
        let settings = SettingsService::new(&db);
        settings
            .write("appearance.theme", Some("dark"))
            .expect("seed theme");
        settings
            .write("flags.assembly", Some("false"))
            .expect("seed flag");
        let mcp = McpServerList {
            servers: vec![McpServerEntry {
                name: "github".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string()],
                env: BTreeMap::from([("PORT".to_string(), "8080".to_string())]),
            }],
        };
        settings
            .write(MCP_SERVERS_KEY, Some(&mcp.to_json().expect("encode mcp")))
            .expect("seed mcp");
        let first = SetupExportService::new(&db)
            .serialize_setup()
            .expect("export succeeds");

        // Import into a fresh database, then export again: stable.
        let fresh = setup_db();
        let report = SetupImportService::new(&fresh)
            .import_setup(&first)
            .expect("setup re-import succeeds");
        assert!(report.skipped.is_empty());
        assert!(report.denied.is_empty());
        assert!(!report.imported.is_empty());
        let second = SetupExportService::new(&fresh)
            .serialize_setup()
            .expect("re-export succeeds");
        assert_eq!(first, second, "export → import must round-trip cleanly");
    }

    #[test]
    fn setup_import_skips_unknown_keys_and_deletes_nulls() {
        let db = setup_db();
        let settings = SettingsService::new(&db);
        settings
            .write("appearance.theme", Some("dark"))
            .expect("seed theme");
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": {
                "appearance.theme": null,
                "flags.assembly": "false",
                "editor.fontSize": "14",
            },
        })
        .to_string();

        let report = SetupImportService::new(&db)
            .import_setup(&json)
            .expect("setup import succeeds");

        // The null clears the seeded theme back to its default (absent).
        assert!(
            settings
                .read("appearance.theme")
                .expect("read theme")
                .is_none(),
            "null setup values delete the key"
        );
        assert_eq!(
            settings.read("flags.assembly").expect("read flag"),
            Some("false".to_string())
        );
        assert_eq!(report.skipped, vec!["editor.fontSize".to_string()]);
        assert!(report.denied.is_empty());
        assert_eq!(report.imported.len(), 2);
    }

    #[test]
    fn setup_import_rejects_wrong_format_and_version() {
        let db = setup_db();
        let service = SetupImportService::new(&db);

        let json = serde_json::json!({
            "format": "some-other-format",
            "version": 1,
            "settings": {},
        })
        .to_string();
        let err = service
            .import_setup(&json)
            .expect_err("wrong format must fail");
        assert!(matches!(err, ImportError::UnsupportedFormat { .. }));

        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 2,
            "settings": {},
        })
        .to_string();
        let err = service
            .import_setup(&json)
            .expect_err("wrong version must fail");
        assert!(matches!(
            err,
            ImportError::UnsupportedVersion { version: 2 }
        ));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "rejected documents write nothing"
        );
    }

    #[test]
    fn setup_import_denies_out_of_domain_values_secret_free() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": {
                "appearance.theme": "ultraviolet",
                "flags.assembly": "maybe",
            },
        })
        .to_string();

        let report = service
            .import_setup(&json)
            .expect("denials land in the report");

        assert!(report.imported.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(report.denied.len(), 2);
        for denial in &report.denied {
            assert_eq!(denial.reason, DENY_UNSUPPORTED_VALUE);
        }
        let rendered = format!("{report:?}");
        assert!(!rendered.contains("ultraviolet"));
        assert!(!rendered.contains("maybe"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied values store nothing"
        );
    }

    #[test]
    fn setup_secret_bearing_document_is_denied_without_echo() {
        let db = setup_db();
        let service = SetupImportService::new(&db);
        let json = serde_json::json!({
            "format": "nexora-setup",
            "version": 1,
            "settings": { "appearance.theme": "sk-live-sentinel-42" },
        })
        .to_string();

        let err = service
            .import_setup(&json)
            .expect_err("secret must be denied");

        assert!(
            matches!(err, ImportError::InvalidData(ref reason) if reason.contains("key-like")),
            "secret denial must use fixed vocabulary, got {err:?}"
        );
        assert_secret_free(&format!("{err}"));
        assert!(
            SettingsService::new(&db)
                .list()
                .expect("list settings")
                .is_empty(),
            "denied documents write nothing"
        );
    }
}
