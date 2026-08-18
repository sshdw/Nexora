//! Conversation workspace: persisted history, send/receive flow, and the
//! composer (FR-003, FR-005).
//!
//! This is a thin presentation layer over the existing hooks and backend: the
//! conversation's message list is loaded whole from `conversation_history` and
//! `send` delegates to the existing `send_message` command with the currently
//! selected provider/model (FR-004). User and assistant messages are kept
//! visually distinguishable by role, chronological order comes from the
//! backend, and request failures surface as a visible error without inventing
//! or corrupting persisted state.

import { useEffect, useRef, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import { useConversation } from "../lib/useConversation";

export interface ConversationViewProps {
  conversationId: number;
  /** Internal name of the selected provider (FR-004), or null if none. */
  selectedProvider: string | null;
  /** Selected model for the selected provider, or null if none. */
  selectedModel: string | null;
  onOpenSettings: () => void;
}

export default function ConversationView({
  conversationId,
  selectedProvider,
  selectedModel,
  onOpenSettings,
}: ConversationViewProps) {
  const { messages, loading, error, sending, send } = useConversation(conversationId);
  const [draft, setDraft] = useState("");
  const threadRef = useRef<HTMLDivElement>(null);

  const ready = selectedProvider !== null && selectedModel !== null;
  const canSend = ready && draft.trim() !== "" && !sending;

  // Keep the newest message in view as history loads and responses arrive.
  useEffect(() => {
    const el = threadRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, loading, sending]);

  const handleSubmit = () => {
    const content = draft.trim();
    if (!canSend || !content || !selectedProvider || !selectedModel) return;
    setDraft("");
    void send(content, selectedProvider, selectedModel);
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
                    : (message.model_name ?? "Assistant")}
                </span>
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
      </div>

      {error && (
        <div className="nex-composer-error" role="alert">
          {error.message}
        </div>
      )}

      <div className="nex-composer">
        <div className="nex-composer-row">
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
          <button
            type="button"
            className="nex-btn nex-btn-accent"
            onClick={handleSubmit}
            disabled={!canSend}
          >
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
        {ready ? (
          <p className="nex-composer-hint">{selectedModel}</p>
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
  );
}