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
//! 2. Persist the user message **and** update the conversation's recency
//!    (`updated_at`) in one atomic transaction (DATABASE.md §12).
//! 3. Load the conversation's persisted history in its existing chronological
//!    order.
//! 4. Build the provider-independent [`AiRequest`].
//! 5. Execute exclusively through the [`AiRequestExecutor`] boundary (the
//!    existing [`RequestExecutionService`] in production).
//! 6. Only after a successful execution, persist the normalized [`AiResponse`]
//!    as an assistant message — again with the conversation's recency touch in
//!    the same atomic transaction — and return it.
//!
//! A failed execution propagates as a classified error: the user message stays
//! persisted and no fake assistant message is created, so conversation history
//! is never corrupted (DATABASE.md §7.2; ARCHITECTURE.md §10). The
//! [`AiRequestExecutor`] seam exists only so this flow — including its failure
//! behavior — is testable without provider execution, keyring access, or the
//! network (ROADMAP.md Phase 10).

use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::attachments::{Attachment, AttachmentRepository};
use crate::infrastructure::repository::conversations::{Conversation, ConversationRepository};
use crate::infrastructure::repository::messages::{Message, MessageRepository};
use crate::infrastructure::repository::providers::ProviderRepository;
use crate::infrastructure::repository::Repository;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use std::fs;

use super::execution::{
    self, AiAttachment, AiAttachmentPayload, AiMessage, AiRequest, AiResponse, AiRole, RequestError,
    RequestExecutionService,
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
    attachments: AttachmentRepository<'a>,
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
            attachments: AttachmentRepository::new(db),
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
            attachments: AttachmentRepository::new(db),
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
    /// `conversation_id`, link the supplied draft attachments to it, and
    /// execute the AI request through the execution boundary (FR-003, FR-004,
    /// FR-008).
    ///
    /// The flow is:
    ///   1. Require the conversation to exist.
    ///   2. Require every referenced attachment to be a draft (`message_id`
    ///      is `NULL`) of *this* conversation — before anything is persisted,
    ///      so a bad id can never leave a half-sent message behind.
    ///   3. Persist the user message.
    ///   4. Link each draft attachment to the created user message
    ///      (`attachments.message_id` update only, DATABASE.md §7.4), so the
    ///      association survives as history and cascade deletion follows the
    ///      existing schema rules.
    ///   5. Load the conversation's persisted history (including the new
    ///      user turn) with its linked attachment references.
    ///   6. Build the [`AiRequest`] from that history.
    ///   7. Execute exclusively through the [`AiRequestExecutor`] (the
    ///      existing [`RequestExecutionService`] in production); no provider
    ///      is called from this layer.
    ///   8. On success, persist the normalized [`AiResponse`] as an assistant
    ///      message and return it.
    ///
    /// `provider` and `model` are passed through unchanged to the execution
    /// boundary (FR-004) and are recorded on the assistant message; this layer
    /// performs no provider-specific branching.
    ///
    /// A failed execution propagates as an error: the persisted user message
    /// (with its now-linked attachments) is kept and no assistant message is
    /// created (FR-003 error handling; DATABASE.md §7.2).
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::NotFound`] when no conversation with
    /// `conversation_id` exists; [`ConversationError::UnknownAttachment`] or
    /// [`ConversationError::ForeignAttachment`] when a referenced attachment
    /// does not exist or is not a draft of this conversation;
    /// [`ConversationError::UnexpectedMessageRole`] when the persisted history
    /// contains a `role` outside `user` / `assistant`;
    /// [`ConversationError::Request`] when AI execution fails; or
    /// [`ConversationError::Database`] when any persistence step fails.
    pub(crate) fn send_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
        attachment_ids: &[i64],
    ) -> Result<AiResponse> {
        if !self.conversations.exists(conversation_id)? {
            return Err(ConversationError::NotFound {
                id: conversation_id,
            });
        }

        // Validate every draft reference and read each attached file up front,
        // so a stale id, foreign draft, unreadable file, oversized file, or
        // provider-unsupported type aborts the send BEFORE anything is
        // persisted (FR-008 error handling).
        let mut prepared: Vec<(i64, AiAttachment)> = Vec::with_capacity(attachment_ids.len());
        for &attachment_id in attachment_ids {
            let attachment = self.attachments.read(attachment_id)?.ok_or(
                ConversationError::UnknownAttachment { id: attachment_id },
            )?;
            if attachment.conversation_id != conversation_id || attachment.message_id.is_some() {
                return Err(ConversationError::ForeignAttachment { id: attachment_id });
            }
            let payload = build_ai_attachment(&attachment, provider)?;
            prepared.push((attachment_id, payload));
        }

        let user_message_id =
            self.persist_message_and_touch(conversation_id, ROLE_USER, content, None, None)?;

        // Associate the drafts with the created message (DATABASE.md §7.4:
        // "UPDATE of `message_id` only"). After this step the attachments are
        // historical: they cascade with their message/conversation per the
        // existing schema foreign keys.
        for &(attachment_id, _) in &prepared {
            self.attachments
                .update_message_id(attachment_id, Some(user_message_id))?;
        }

        let current_attachments = prepared.into_iter().map(|(_, payload)| payload).collect();
        let request = AiRequest {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: self.ai_history(
                conversation_id,
                provider,
                user_message_id,
                current_attachments,
            )?,
            tools: Vec::new(),
        };

        let response = self.execution.execute(&request)?;

        // Execution succeeded, so the provider metadata row is resolvable
        // (RequestExecutionService rejects an unknown provider before any
        // request is sent). The provider's id is recorded on the assistant
        // message (FR-004; DATABASE.md §7.2 `provider_id`).
        let provider_id = self.providers.read_by_name(provider)?.map(|p| p.id);
        self.persist_message_and_touch(
            conversation_id,
            ROLE_ASSISTANT,
            &response.content,
            provider_id,
            Some(&response.model),
        )?;

        Ok(response)
    }

    /// Build the provider-independent history for `conversation_id`, mapping
    /// each persisted message to an [`AiMessage`] and attaching each user
    /// turn's linked attachments (FR-008).
    ///
    /// The *current* user turn reuses `current_attachments` (already read and
    /// validated before the message was persisted); older user turns have
    /// their attachments re-read from disk through their stored `file_path`.
    /// The filesystem path never crosses into [`AiAttachment`], so it can
    /// never enter a provider payload.
    ///
    /// # Errors
    ///
    /// Returns [`ConversationError::UnexpectedMessageRole`] for a persisted
    /// role outside `user` / `assistant`; [`ConversationError::
    /// AttachmentUnreadable`], [`ConversationError::AttachmentTooLarge`],
    /// [`ConversationError::AttachmentNotText`], or
    /// [`ConversationError::AttachmentUnsupported`] when a historical attached
    /// file can no longer be turned into a provider-safe payload; or
    /// [`ConversationError::Database`] when any query fails.
    fn ai_history(
        &self,
        conversation_id: i64,
        provider: &str,
        current_message_id: i64,
        current_attachments: Vec<AiAttachment>,
    ) -> Result<Vec<AiMessage>> {
        let history = self.messages.list_by_conversation(conversation_id)?;
        let mut messages = Vec::with_capacity(history.len());
        for message in &history {
            let mut ai_message = ai_message_from(message)?;
            if ai_message.role == AiRole::User {
                if message.id == current_message_id {
                    ai_message.attachments = current_attachments.clone();
                } else {
                    for attachment in self.attachments.list_by_message(message.id)? {
                        ai_message
                            .attachments
                            .push(build_ai_attachment(&attachment, provider)?);
                    }
                }
            }
            messages.push(ai_message);
        }
        Ok(messages)
    }

    /// Persist one message and update the conversation's recency (`updated_at`)
    /// in a single atomic transaction (DATABASE.md §7.2, §12).
    ///
    /// A send never changes a mutable `conversations` column, so recency cannot
    /// advance through the `conversations_touch_updated_at` trigger; this
    /// explicit write is sent in the same transaction as the message insert so
    /// the message and the conversation's `updated_at` either both land or both
    /// roll back together. The user and assistant messages are each persisted
    /// in their own atomic step so a failed execution leaves the persisted user
    /// message with its recency touch and never manufactures an assistant row.
    ///
    /// Returns the schema-assigned `id` of the created message so the caller
    /// can link draft attachments to the user turn (FR-008).
    ///
    /// # Errors
    ///
    /// Returns a [`ConversationError::Database`] when either the message insert
    /// or the recency update fails (the whole transaction rolls back).
    fn persist_message_and_touch(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        provider_id: Option<i64>,
        model_name: Option<&str>,
    ) -> Result<i64> {
        let message_id = self.conversations.transaction(|tx| {
            let id = MessageRepository::create_in_transaction(
                tx,
                conversation_id,
                role,
                content,
                provider_id,
                model_name,
            )?;
            ConversationRepository::touch_updated_at(tx, conversation_id)?;
            Ok(id)
        })?;
        Ok(message_id)
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
        // Historical attachments are joined by `ai_history`; the plain
        // per-row mapping starts from no attachment references.
        attachments: Vec::new(),
    })
}

/// Hard cap for reading one attachment into memory (FR-008). Chosen below the
/// smallest provider inline limit (Gemini's 20 MB total inline payload) so an
/// oversized file is rejected before a request is built.
const MAX_ATTACHMENT_BYTES: i64 = 20 * 1024 * 1024;

/// The media families the registered providers can consume (FR-008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentFamily {
    /// Decodable UTF-8 text, inlined into the turn content.
    Text,
    /// `image/png`, `image/jpeg`, `image/webp`, `image/gif` — supported as
    /// base64 image parts by all three providers.
    Image,
    /// `application/pdf` — supported as document/inline parts by Anthropic and
    /// Gemini; OpenAI Chat Completions has no inline PDF input.
    Pdf,
}

/// Classify a declared MIME type into a provider-consumable family.
/// `None` means "no declared family" — the caller falls back to a UTF-8 text
/// decode attempt.
fn attachment_family(mime: Option<&str>) -> Option<AttachmentFamily> {
    let mime = mime?;
    let lowered = mime.to_ascii_lowercase();
    if lowered.starts_with("text/") {
        return Some(AttachmentFamily::Text);
    }
    match lowered.as_str() {
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" => {
            Some(AttachmentFamily::Image)
        }
        "application/pdf" => Some(AttachmentFamily::Pdf),
        // Common textual application types are decoded and inlined as text.
        "application/json"
        | "application/xml"
        | "application/javascript"
        | "application/x-yaml"
        | "application/yaml"
        | "application/sql" => Some(AttachmentFamily::Text),
        _ => None,
    }
}

/// Whether `provider`'s existing HTTP contract can carry `family` inline.
fn provider_supports(provider: &str, family: AttachmentFamily) -> bool {
    match provider {
        // Chat Completions carries text parts and base64 `image_url` data
        // URIs; it defines no inline PDF/document input.
        "openai" => !matches!(family, AttachmentFamily::Pdf),
        // Messages API carries base64 `image` and PDF `document` blocks.
        "anthropic" => true,
        // generateContent carries `inlineData` images and PDFs.
        "gemini" => true,
        // Unknown registered provider: conservative, text-only fallback.
        _ => matches!(family, AttachmentFamily::Text),
    }
}

/// Read one persisted attachment from disk through its stored `file_path` and
/// convert it into a provider-safe [`AiAttachment`] (FR-008).
///
/// - A size guard rejects files larger than [`MAX_ATTACHMENT_BYTES`] before
///   they are read into memory (using the recorded size first, then the real
///   filesystem metadata).
/// - Images and PDFs are base64-encoded for provider-native inline parts.
/// - Text-declared files — and files with no recognized MIME type — must be
///   valid UTF-8 and travel decoded as text.
/// - The filesystem path is used **only** to open the file; it is never copied
///   onto the returned value, so no executor can transmit it.
fn build_ai_attachment(attachment: &Attachment, provider: &str) -> Result<AiAttachment> {
    let name = attachment.file_name.clone();

    if let Some(size) = attachment.file_size_bytes {
        if size > MAX_ATTACHMENT_BYTES {
            return Err(ConversationError::AttachmentTooLarge {
                name,
                size_bytes: size,
                max_bytes: MAX_ATTACHMENT_BYTES,
            });
        }
    }

    let actual_size = fs::metadata(&attachment.file_path)
        .map_err(|_| ConversationError::AttachmentUnreadable { name: name.clone() })?
        .len() as i64;
    if actual_size > MAX_ATTACHMENT_BYTES {
        return Err(ConversationError::AttachmentTooLarge {
            name,
            size_bytes: actual_size,
            max_bytes: MAX_ATTACHMENT_BYTES,
        });
    }

    let bytes = fs::read(&attachment.file_path)
        .map_err(|_| ConversationError::AttachmentUnreadable { name: name.clone() })?;

    let declared = attachment_family(attachment.mime_type.as_deref());
    let is_inline = matches!(
        declared,
        Some(AttachmentFamily::Image) | Some(AttachmentFamily::Pdf)
    );

    if let Some(family) = declared {
        if !provider_supports(provider, family) {
            return Err(ConversationError::AttachmentUnsupported {
                name,
                provider: provider.to_string(),
            });
        }
    }

    let payload = if is_inline {
        AiAttachmentPayload::Base64(BASE64_STANDARD.encode(&bytes))
    } else {
        // Text-declared or unrecognized media fall back to UTF-8 decoding;
        // binary data without a supported representation is refused rather
        // than silently corrupted or ignored.
        match String::from_utf8(bytes) {
            Ok(text) => AiAttachmentPayload::Text(text),
            Err(_) => {
                return Err(ConversationError::AttachmentNotText { name });
            }
        }
    };

    Ok(AiAttachment {
        file_name: name,
        file_size_bytes: attachment.file_size_bytes,
        mime_type: attachment.mime_type.clone(),
        payload,
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
    /// A referenced attachment id does not exist (FR-008 draft validation).
    UnknownAttachment {
        /// The referenced attachment id.
        id: i64,
    },
    /// A referenced attachment exists but is not an unsent draft of this
    /// conversation — it belongs to another conversation or is already linked
    /// to a sent message (FR-008 draft validation).
    ForeignAttachment {
        /// The referenced attachment id.
        id: i64,
    },
    /// The attached file could not be read from disk through its stored path
    /// (missing, moved, or permission denied).
    AttachmentUnreadable {
        /// The attachment's display name.
        name: String,
    },
    /// The attached file exceeds the in-memory size guard for request
    /// construction.
    AttachmentTooLarge {
        /// The attachment's display name.
        name: String,
        /// The file's size in bytes.
        size_bytes: i64,
        /// The maximum accepted size in bytes.
        max_bytes: i64,
    },
    /// The attached file has no supported provider transmission representation
    /// (declared text media that is not valid UTF-8).
    AttachmentNotText {
        /// The attachment's display name.
        name: String,
    },
    /// The selected provider cannot carry the attached file type inline.
    AttachmentUnsupported {
        /// The attachment's display name.
        name: String,
        /// The internal provider name.
        provider: String,
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
            Self::UnknownAttachment { id } => {
                write!(f, "attachment {id} does not exist")
            }
            Self::ForeignAttachment { id } => write!(
                f,
                "attachment {id} is not a pending draft of this conversation"
            ),
            Self::AttachmentUnreadable { name } => write!(
                f,
                "could not read attached file '{name}' from disk"
            ),
            Self::AttachmentTooLarge { name, size_bytes, max_bytes } => write!(
                f,
                "attached file '{name}' is too large ({size_bytes} bytes; the limit is {max_bytes} bytes)"
            ),
            Self::AttachmentNotText { name } => write!(
                f,
                "attached file '{name}' is not decodable text and has no supported representation"
            ),
            Self::AttachmentUnsupported { name, provider } => write!(
                f,
                "the AI provider '{provider}' cannot accept attached file '{name}' of this type"
            ),
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
            Self::NotFound { .. }
            | Self::UnknownAttachment { .. }
            | Self::ForeignAttachment { .. }
            | Self::AttachmentUnreadable { .. }
            | Self::AttachmentTooLarge { .. }
            | Self::AttachmentNotText { .. }
            | Self::AttachmentUnsupported { .. }
            | Self::UnexpectedMessageRole { .. } => None,
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
                 ON messages (conversation_id, created_at);
             CREATE TABLE attachments (
                 id INTEGER PRIMARY KEY,
                 conversation_id INTEGER NOT NULL CHECK(conversation_id > 0)
                     REFERENCES conversations(id) ON DELETE CASCADE,
                 message_id INTEGER
                     REFERENCES messages(id) ON DELETE CASCADE,
                 file_name TEXT NOT NULL
                     CHECK(length(file_name) > 0 AND length(file_name) <= 255),
                 file_path TEXT NOT NULL CHECK(length(file_path) > 0),
                 file_size_bytes INTEGER
                     CHECK(file_size_bytes IS NULL OR file_size_bytes >= 0),
                 mime_type TEXT
                     CHECK(mime_type IS NULL OR length(mime_type) <= 127)
             );",
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
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        let response = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
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
                tool_calls: Vec::new(),
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
            .send_message(conversation_id, "question two", "openai", "gpt-4o-mini", &[])
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
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
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
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
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
    fn send_advances_conversation_recency_atomically() {
        let db = test_db();
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "answer".to_string(),
                model: "gpt-4o-mini".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        let repo = ConversationRepository::new(&db);
        let before = repo.read(conversation_id).expect("read conversation").expect("exists");

        service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
            .expect("send succeeds");

        // Sending a message must advance the conversation's recency (DATABASE.md
        // §12) so the sidebar can order it as recently active.
        let after = repo.read(conversation_id).expect("read conversation").expect("exists");
        assert!(after.updated_at > before.updated_at);
        // The message and the recency touch landed together: no extra message row.
        let history = service.history(conversation_id).expect("history loads");
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn failed_send_persists_user_message_and_still_advances_recency() {
        let db = test_db();
        let (service, _captured) = failing_service(&db, "openai");
        let conversation_id = service.create("Chat").expect("conversation created");
        let repo = ConversationRepository::new(&db);
        let before = repo.read(conversation_id).expect("read exists").expect("insert");

        service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
            .expect_err("execution fails");

        // The user message is persisted, so the conversation is modified even on
        // a failed execution; the recency touch reflects that and the failed
        // request leaves no assistant message (DATABASE.md §7.2).
        let after = repo.read(conversation_id).expect("read conversation").expect("exists");
        assert!(after.updated_at > before.updated_at);
        let history = service.history(conversation_id).expect("history loads");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, ROLE_USER);
    }

    #[test]
    fn retry_after_failure_persists_assistant_for_the_follow_up() {
        let db = test_db();
        let id = {
            let (service, _captured) = failing_service(&db, "openai");
            let id = service.create("Chat").expect("conversation created");
            service
                .send_message(id, "question", "openai", "gpt-4o-mini", &[])
                .expect_err("first attempt fails");
            id
        };
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "final answer".to_string(),
                model: "gpt-4o-mini".to_string(),
                tool_calls: Vec::new(),
            },
        );

        service
            .send_message(id, "retry", "openai", "gpt-4o-mini", &[])
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
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        // An arbitrary provider/model flows through the request unchanged; the
        // conversation layer never branches on a specific provider.
        service
            .send_message(conversation_id, "hi", "custom-provider", "custom-model", &[])
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
                tool_calls: Vec::new(),
            },
        );

        let err = service
            .send_message(42, "hello", "openai", "gpt-4o-mini", &[])
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
                tool_calls: Vec::new(),
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
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[])
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

    /// Create a real temporary file with `contents`, insert a draft attachment
    /// row pointing at it, and return its schema-assigned `id`.
    ///
    /// `size_override` simulates rows whose recorded size differs from the
    /// real file (for exercising the size guard without writing large files).
    fn draft_attachment_with(
        db: &Database,
        conversation_id: i64,
        name: &str,
        mime: Option<&str>,
        size_override: Option<i64>,
        contents: &[u8],
    ) -> i64 {
        let path = std::env::temp_dir().join(format!("nexora-test-{}-{name}", std::process::id()));
        fs::write(&path, contents).expect("write temp attachment file");
        AttachmentRepository::new(db)
            .create(
                conversation_id,
                name,
                &path.to_string_lossy(),
                size_override.or(Some(contents.len() as i64)),
                mime,
            )
            .expect("draft attachment created")
    }

    /// Draft attachment backed by a real UTF-8 text file.
    fn draft_text_attachment(
        db: &Database,
        conversation_id: i64,
        name: &str,
        contents: &str,
    ) -> i64 {
        draft_attachment_with(
            db,
            conversation_id,
            name,
            Some("text/plain"),
            None,
            contents.as_bytes(),
        )
    }

    #[test]
    fn send_message_links_draft_attachments_and_carries_them_into_the_request() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        let first = draft_text_attachment(
            &db,
            conversation_id,
            "report.txt",
            "quarterly revenue rose 12 percent",
        );
        let second = draft_text_attachment(&db, conversation_id, "notes.md", "roadmap notes");

        service
            .send_message(
                conversation_id,
                "summarize",
                "openai",
                "gpt-4o-mini",
                &[first, second],
            )
            .expect("send succeeds");

        // Both drafts are now linked to the persisted user message.
        let history = MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages");
        let user_message = history
            .iter()
            .find(|m| m.role == ROLE_USER)
            .expect("user message persisted");
        let linked = AttachmentRepository::new(&db)
            .list_by_message(user_message.id)
            .expect("list by message");
        assert_eq!(linked.len(), 2);
        assert!(linked.iter().all(|a| a.message_id == Some(user_message.id)));
        // No drafts remain for the conversation.
        assert!(AttachmentRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list drafts")
            .is_empty());

        // The request's newest user turn carries both attachments, read from
        // disk and inlined as decoded text — with no filesystem path anywhere.
        let request = captured.borrow().clone().expect("request executed");
        let turn = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == AiRole::User)
            .expect("newest user turn");
        assert_eq!(turn.attachments.len(), 2);
        assert!(turn.attachments.iter().any(|a| a.file_name == "report.txt"));
        assert_eq!(
            turn.attachments[0].payload,
            AiAttachmentPayload::Text("quarterly revenue rose 12 percent".to_string())
        );
        assert_eq!(
            turn.attachments[0].mime_type.as_deref(),
            Some("text/plain")
        );
        // The boundary carries the decoded payload; fencing into turn text
        // happens at wire time in the executors, and no filesystem path is
        // ever present.
        assert!(!format!("{turn:?}").contains("nexora-test"));
    }

    #[test]
    fn send_message_rejects_an_unknown_attachment_before_persisting() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[42])
            .expect_err("unknown attachment id");

        assert!(matches!(
            err,
            ConversationError::UnknownAttachment { id: 42 }
        ));
        // Nothing was persisted and execution was never reached.
        assert!(captured.borrow().is_none());
        assert!(MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages")
            .is_empty());
    }

    #[test]
    fn send_message_rejects_foreign_and_already_linked_attachments() {
        let db = test_db();
        let (service, _captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        let other_conversation = service.create("Other").expect("conversation created");
        let foreign = draft_text_attachment(&db, other_conversation, "other.txt", "foreign");

        // An attachment belonging to another conversation is rejected.
        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[foreign])
            .expect_err("foreign attachment");
        assert!(matches!(
            err,
            ConversationError::ForeignAttachment { .. }
        ));

        // A draft already linked to a sent message can never be re-linked.
        let own = draft_text_attachment(&db, conversation_id, "own.txt", "own content");
        service
            .send_message(conversation_id, "first", "openai", "gpt-4o-mini", &[own])
            .expect("first send succeeds");
        let err = service
            .send_message(conversation_id, "again", "openai", "gpt-4o-mini", &[own])
            .expect_err("already linked attachment");
        assert!(matches!(
            err,
            ConversationError::ForeignAttachment { id } if id == own
        ));
    }

    #[test]
    fn historical_attachments_reappear_in_later_requests() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        let attachment =
            draft_text_attachment(&db, conversation_id, "report.pdf.txt", "historical body");

        service
            .send_message(
                conversation_id,
                "first",
                "openai",
                "gpt-4o-mini",
                &[attachment],
            )
            .expect("first send succeeds");

        // A later, attachment-less send still carries the earlier user turn's
        // attachment (re-read from disk) as part of the conversation context.
        service
            .send_message(conversation_id, "second", "openai", "gpt-4o-mini", &[])
            .expect("second send succeeds");

        let request = captured.borrow().clone().expect("request executed");
        // History at second-execution time: user, assistant, new user (the
        // second assistant turn is only persisted after execution).
        assert_eq!(request.messages.len(), 3);
        let first_turn = &request.messages[0];
        assert_eq!(first_turn.attachments.len(), 1);
        assert_eq!(first_turn.attachments[0].file_name, "report.pdf.txt");
        assert_eq!(
            first_turn.attachments[0].payload,
            AiAttachmentPayload::Text("historical body".to_string())
        );
        // The newest user turn has no attachments of its own.
        let newest_user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == AiRole::User)
            .expect("newest user turn");
        assert!(newest_user.attachments.is_empty());
    }

    #[test]
    fn send_without_attachments_keeps_the_request_unchanged() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        // Drafts exist but are not referenced by this send.
        let _unrelated = draft_text_attachment(&db, conversation_id, "ignored.txt", "unused");

        service
            .send_message(conversation_id, "plain", "openai", "gpt-4o-mini", &[])
            .expect("send succeeds");

        let request = captured.borrow().clone().expect("request executed");
        assert!(request.messages.iter().all(|m| m.attachments.is_empty()));
        // The unreferenced draft stays in the draft state untouched.
        assert_eq!(
            AttachmentRepository::new(&db)
                .list_by_conversation(conversation_id)
                .expect("list drafts")
                .len(),
            1
        );
    }

    #[test]
    fn pdf_attachment_to_openai_is_rejected_before_execution() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        // OpenAI Chat Completions has no inline PDF input; the send must fail
        // with a classified error before anything is persisted or executed.
        let pdf = draft_attachment_with(
            &db,
            conversation_id,
            "paper.pdf",
            Some("application/pdf"),
            None,
            b"%PDF-1.7 fake bytes",
        );

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[pdf])
            .expect_err("openai cannot carry a PDF");

        assert!(matches!(
            err,
            ConversationError::AttachmentUnsupported { ref name, ref provider }
                if name == "paper.pdf" && provider == "openai"
        ));
        assert!(captured.borrow().is_none());
        assert!(MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages")
            .is_empty());
    }

    #[test]
    fn pdf_attachment_to_anthropic_travels_as_base64_document_payload() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "ok".to_string(),
                model: "m".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        let pdf = draft_attachment_with(
            &db,
            conversation_id,
            "paper.pdf",
            Some("application/pdf"),
            None,
            b"%PDF-1.7 fake bytes",
        );

        service
            .send_message(conversation_id, "read this", "anthropic", "claude", &[pdf])
            .expect("send succeeds");

        let request = captured.borrow().clone().expect("request executed");
        let turn = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == AiRole::User)
            .expect("user turn");
        assert_eq!(
            turn.attachments[0].payload,
            AiAttachmentPayload::Base64(BASE64_STANDARD.encode(b"%PDF-1.7 fake bytes"))
        );
    }

    #[test]
    fn unreadable_attachment_aborts_the_send() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        // A row whose stored path does not exist on disk (file moved/deleted).
        let ghost = AttachmentRepository::new(&db)
            .create(
                conversation_id,
                "ghost.txt",
                std::env::temp_dir()
                    .join(format!("nexora-missing-{}.txt", std::process::id()))
                    .to_str()
                    .expect("temp path"),
                Some(3),
                Some("text/plain"),
            )
            .expect("draft attachment row created");

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[ghost])
            .expect_err("unreadable attachment");

        assert!(matches!(
            err,
            ConversationError::AttachmentUnreadable { ref name } if name == "ghost.txt"
        ));
        assert!(captured.borrow().is_none());
        assert!(MessageRepository::new(&db)
            .list_by_conversation(conversation_id)
            .expect("list messages")
            .is_empty());
    }

    #[test]
    fn oversized_attachment_is_rejected_before_reading_the_file() {
        let db = test_db();
        let (service, captured) = succeeding_service(
            &db,
            AiResponse {
                content: "unused".to_string(),
                model: "unused".to_string(),
                tool_calls: Vec::new(),
            },
        );
        let conversation_id = service.create("Chat").expect("conversation created");
        // The recorded size exceeds the guard, so the file is rejected without
        // being read into memory (the real temp file is tiny).
        let huge = draft_attachment_with(
            &db,
            conversation_id,
            "huge.bin",
            None,
            Some(MAX_ATTACHMENT_BYTES + 1),
            b"tiny",
        );

        let err = service
            .send_message(conversation_id, "hello", "openai", "gpt-4o-mini", &[huge])
            .expect_err("oversized attachment");

        assert!(matches!(
            err,
            ConversationError::AttachmentTooLarge { max_bytes, .. }
                if max_bytes == MAX_ATTACHMENT_BYTES
        ));
        assert!(captured.borrow().is_none());
    }
}
