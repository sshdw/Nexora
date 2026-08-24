//! Conversation workspace: persisted history, send/receive flow, and the
//! composer (FR-003, FR-005).
//!
//! This is a thin presentation layer over the existing hooks and backend: the
//! conversation's message list is loaded whole from `conversation_history` and
//! `send` delegates to the existing `send_message` command with the currently
//! selected provider/model (FR-004). User and assistant messages are kept
//! visually distinguishable by role, chronological order comes from the
//! backend, and request failures surface as a visible error without inventing
//! or corrupting persisted state. A failed AI request additionally restores
//! the sent text into the composer so it can be resent without retyping —
//! frontend draft state only; persisted history is never modified.

import { useEffect, useRef } from "react";

import { formatBytes, formatRelativeTime } from "../lib/format";
import { useAttachments } from "../lib/useAttachments";
import { useConversation } from "../lib/useConversation";
import Tooltip from "./Tooltip";
import { CloseIcon, PaperclipIcon } from "./icons";

export interface ConversationViewProps {
  conversationId: number;
  /** Internal name of the selected provider (FR-004), or null if none. */
  selectedProvider: string | null;
  /** Selected model for the selected provider, or null if none. */
  selectedModel: string | null;
  onOpenSettings: () => void;
  /** Called after a send resolves, so the sidebar can refresh ordering by
   * the backend's recency update (conversation becomes recently active). */
  onMessageSent?: () => void;
  /** Current composer draft value (lifted so the Prompt Library can stage a
   * prompt's content into the active conversation's input field — FR-007). */
  draft: string;
  /** Replace the composer draft (FR-007 "Use" insertion). */
  setDraft: (value: string) => void;
}

export default function ConversationView({
  conversationId,
  selectedProvider,
  selectedModel,
  onOpenSettings,
  onMessageSent,
  draft,
  setDraft,
}: ConversationViewProps) {
  const { messages, loading, error, sending, send } = useConversation(conversationId);
  const {
    attachments,
    error: attachmentError,
    busy: attachmentsBusy,
    pickAndAttach,
    remove,
    refresh: refreshAttachments,
  } = useAttachments(conversationId);
  const threadRef = useRef<HTMLDivElement>(null);

  const ready = selectedProvider !== null && selectedModel !== null;
  const canSend =
    ready && draft.trim() !== "" && !sending && !attachmentsBusy;

  // Keep the newest message in view as history loads and responses arrive.
  useEffect(() => {
    const el = threadRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, loading, sending]);

  const handleSubmit = async () => {
    const content = draft.trim();
    if (!canSend || !content || !selectedProvider || !selectedModel) return;
    const attachmentIds = attachments.map((attachment) => attachment.id);
    setDraft("");
    const failure = await send(content, selectedProvider, selectedModel, attachmentIds);
    // A failed AI request keeps the persisted user message and creates no
    // assistant message (DATABASE.md §7.2). Offline/network failures arrive
    // classified under the backend's "request" kind (commands/error.rs maps
    // every provider execution failure there), so the exact sent text is
    // restored into the composer: the user can edit or resend it once
    // connectivity returns without retyping. Restoration is frontend state
    // only — nothing is written to or removed from history, so resending
    // goes through the normal single-turn send flow.
    if (failure !== null && failure.kind === "request") {
      setDraft(content);
    }
    // The backend is the source of truth: on success the drafts are now
    // message-linked and disappear; on failure they remain drafts and
    // reappear unchanged (FR-008 draft lifecycle).
    await refreshAttachments();
    onMessageSent?.();
  };

  return (
    <div className="nex-main-conversation">
      <div className="nex-thread" ref={threadRef} aria-label="Messages">
        {loading ? (
          <p className="nex-thread-status">Loading messages…</p>
        ) : messages.length === 0 ? (
          <div className="nex-conversation-placeholder">
            <h2 className="nex-conversation-empty-title">No messages yet</h2>
            <p className="nex-conversation-empty-text">
              Send a message to start the conversation.
            </p>
          </div>
        ) : (
          messages.map((message) => (
            <article
              key={message.id}
              className={
                "nex-message " +
                (message.role === "user"
                  ? "nex-message-user"
                  : "nex-message-assistant")
              }
            >
              <div className="nex-message-meta">
                <span className="nex-message-author">
                  {message.role === "user"
                    ? "You"
                    : "Assistant"}
                </span>
                {message.role === "assistant" && message.model_name && (
                  <span
                    className="nex-message-origin"
                    title="Provider and model used for this message"
                  >
                    {message.model_name}
                  </span>
                )}
                <time
                  className="nex-message-time"
                  dateTime={new Date(message.created_at * 1000).toISOString()}
                >
                  {formatRelativeTime(message.created_at)}
                </time>
              </div>
              <div className="nex-message-body">{message.content}</div>
            </article>
          ))
        )}
        {sending && (
          <div className="nex-typing" aria-label="Assistant is responding">
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
          </div>
        )}
      </div>

      {attachmentError && (
        <div className="nex-composer-error" role="alert">
          {attachmentError.message}
        </div>
      )}

      {error && (
        <div className="nex-composer-error" role="alert">
          {error.message}
        </div>
      )}

      <div className="nex-composer">
        <div className="nex-composer-inner">
          {attachments.length > 0 && (
            <ul className="nex-composer-attachments" aria-label="Attached files">
              {attachments.map((attachment) => (
                <li key={attachment.id} className="nex-chip">
                  <PaperclipIcon className="nex-chip-icon" />
                  <span className="nex-attachment-text">
                    <span className="nex-chip-name" title={attachment.file_name}>
                      {attachment.file_name}
                    </span>
                    {formatBytes(attachment.file_size_bytes) !== null && (
                      <span className="nex-chip-size">
                        {formatBytes(attachment.file_size_bytes)}
                      </span>
                    )}
                  </span>
                  <button
                    type="button"
                    className="nex-chip-remove"
                    aria-label={`Remove ${attachment.file_name}`}
                    disabled={attachmentsBusy || sending}
                    onClick={() => void remove(attachment.id)}
                  >
                    <CloseIcon />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="nex-composer-row">
            <Tooltip label="Add file">
              <button
                type="button"
                className="nex-composer-attach"
                aria-label="Add file"
                disabled={sending || attachmentsBusy}
                onClick={() => void pickAndAttach()}
              >
                <PaperclipIcon />
              </button>
            </Tooltip>
            <div className="nex-composer-input-wrap">
              <textarea
                className="nex-composer-input"
                rows={1}
                placeholder="Message"
                aria-label="Message"
                value={draft}
                disabled={sending}
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && !event.shiftKey) {
                    event.preventDefault();
                    handleSubmit();
                  }
                }}
              />
            </div>
            <button
              type="button"
              className="nex-composer-send"
              onClick={handleSubmit}
              disabled={!canSend}
            >
              {sending ? "Sending…" : "Send"}
            </button>
          </div>
          {ready ? (
            <p className="nex-composer-hint">
              <span className="nex-composer-model">{selectedModel}</span>
              <span className="nex-composer-shortcut">
                Enter to send · Shift+Enter for a new line
              </span>
            </p>
          ) : (
            <p className="nex-composer-hint">
              Choose a provider and model in Settings to send messages.{" "}
              <button
                type="button"
                className="nex-btn nex-btn-accent nex-btn-sm"
                onClick={onOpenSettings}
              >
                Open Settings
              </button>
            </p>
          )}
        </div>
      </div>
    </div>
  );
}