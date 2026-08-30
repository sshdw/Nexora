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

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { formatBytes, formatRelativeTime } from "../lib/format";
import {
  cancelAgentRun,
  extendAgentRun,
  resolveAgentApproval,
  startAgentRun,
} from "../lib/tauri";
import { useAgentRun } from "../lib/useAgentRun";
import { useAttachments } from "../lib/useAttachments";
import { useConversation } from "../lib/useConversation";
import Tooltip from "./Tooltip";
import AgentRunSteps from "./AgentRunSteps";
import {
  ArrowUpIcon,
  CloseIcon,
  PaperclipIcon,
} from "./icons";
import NexoraMark from "./NexoraMark";

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
  const { runs, reload: reloadAgent } = useAgentRun(conversationId);
  const [agentMode, setAgentMode] = useState<boolean>(false);
  const [agentBusy, setAgentBusy] = useState<boolean>(false);
  const [agentError, setAgentError] = useState<string | null>(null);
  const threadRef = useRef<HTMLDivElement>(null);

  // Insertion-motion bookkeeping (presentation only): the message ids shown
  // in the previous committed render, so genuinely inserted messages can be
  // distinguished from the initial history fill — only true insertions get
  // the entrance animation; unchanged content never animates in. Reset
  // wholesale when the selected conversation changes.
  const prevMessagesRef = useRef<{ conversationId: number; ids: Set<number> }>({
    conversationId,
    ids: new Set(),
  });
  const prevMessages = prevMessagesRef.current;
  const freshMessageIds =
    prevMessages.conversationId === conversationId &&
    prevMessages.ids.size > 0 &&
    messages.length > 0
      ? new Set(
          messages
            .filter((message) => !prevMessages.ids.has(message.id))
            .map((message) => message.id),
        )
      : null;
  useLayoutEffect(() => {
    prevMessagesRef.current = {
      conversationId,
      ids: new Set(messages.map((message) => message.id)),
    };
  }, [conversationId, messages]);

  const ready = selectedProvider !== null && selectedModel !== null;
  const hasActiveRun = runs.some((r) => r.status === "running" || r.status === "budget_exhausted");
  const canSend =
    ready && draft.trim() !== "" && !sending && !attachmentsBusy && !agentBusy && !hasActiveRun;

  // Keep the newest message in view as history loads and responses arrive.
  useEffect(() => {
    const el = threadRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, loading, sending, runs, agentBusy]);

  const handleAgentCancel = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await cancelAgentRun(runId);
    } catch (e) {
      setAgentError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const handleAgentApprove = useCallback(async (runId: number, callId: string, approved: boolean) => {
    setAgentError(null);
    try {
      await resolveAgentApproval(runId, callId, approved);
    } catch (e) {
      setAgentError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const handleAgentContinue = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await extendAgentRun(runId, 10);
    } catch (e) {
      setAgentError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const handleSubmit = async () => {
    const content = draft.trim();
    if (!canSend || !content || !selectedProvider || !selectedModel) return;
    const attachmentIds = attachments.map((attachment) => attachment.id);
    // Agent path: opt-in streaming via start_agent_run (DP-3, DP-4).
    if (agentMode) {
      setAgentError(null);
      setAgentBusy(true);
      setDraft("");
      try {
        await startAgentRun(conversationId, content, selectedProvider, selectedModel);
        // User message persisted before spawn; refresh both histories.
        await Promise.all([refreshAttachments(), reloadAgent()]);
        onMessageSent?.();
      } catch (e) {
        // Start failed: surface error, restore draft, and reload to show persisted state.
        const msg = e instanceof Error ? e.message : typeof e === "object" && e !== null && "message" in e ? String((e as { message: unknown }).message) : String(e);
        setAgentError(msg);
        setDraft(content);
        await Promise.all([refreshAttachments(), reloadAgent()]);
      } finally {
        setAgentBusy(false);
      }
      return;
    }
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

  // Chronological thread: messages and agent runs interleaved by timestamp (Task 5.1 DP-6).
  const threadItems: Array<
    | { kind: "message"; ts: number; message: (typeof messages)[number] }
    | { kind: "run"; ts: number; run: (typeof runs)[number] }
  > = [
    ...messages.map((m) => ({ kind: "message" as const, ts: m.created_at, message: m })),
    ...runs.map((r) => ({ kind: "run" as const, ts: r.started_at, run: r })),
  ].sort((a, b) => a.ts - b.ts);

  return (
    <div className="nex-main-conversation">
      <div className="nex-thread" ref={threadRef} aria-label="Messages">
        {loading ? (
          <p className="nex-thread-status nex-fade-in" role="status">
            Loading messages…
          </p>
        ) : threadItems.length === 0 ? (
          <div className="nex-conversation-placeholder nex-empty-enter">
            <span className="nex-conversation-empty-mark-wrap" aria-hidden="true">
              <NexoraMark className="nex-conversation-empty-mark" width={26} height={26} />
            </span>
            <h2 className="nex-conversation-empty-title">No messages yet</h2>
            <p className="nex-conversation-empty-text">
              Write your first message below to start the conversation.
            </p>
          </div>
        ) : (
          threadItems.map((item) =>
            item.kind === "message" ? (
              <article
                key={`msg-${item.message.id}`}
                className={
                  "nex-message " +
                  (item.message.role === "user"
                    ? "nex-message-user"
                    : "nex-message-assistant") +
                  (freshMessageIds?.has(item.message.id) ? " nex-message-enter" : "")
                }
              >
                <div className="nex-message-meta">
                  <span className="nex-message-author">
                    {item.message.role === "user" ? "You" : "Assistant"}
                  </span>
                  {item.message.role === "assistant" && item.message.model_name && (
                    <span
                      className="nex-message-origin"
                      title="Provider and model used for this message"
                    >
                      {item.message.model_name}
                    </span>
                  )}
                  <time
                    className="nex-message-time"
                    dateTime={new Date(item.message.created_at * 1000).toISOString()}
                  >
                    {formatRelativeTime(item.message.created_at)}
                  </time>
                </div>
                <div className="nex-message-body">{item.message.content}</div>
              </article>
            ) : (
              <AgentRunSteps
                key={`run-${item.run.run_id}`}
                run={item.run}
                onResolveApproval={(callId, approved) => void handleAgentApprove(item.run.run_id, callId, approved)}
                onCancel={() => void handleAgentCancel(item.run.run_id)}
                onContinue={() => void handleAgentContinue(item.run.run_id)}
              />
            ),
          )
        )}
        {sending && !agentMode && (
          <div
            className="nex-typing"
            role="status"
            aria-label="Assistant is responding"
          >
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
          </div>
        )}
        {agentBusy && (
          <div className="nex-typing" role="status" aria-label="Agent is responding">
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
            <span className="nex-typing-dot" />
          </div>
        )}
      </div>

      {attachmentError && (
        <div className="nex-composer-error nex-fade-in" role="alert">
          {attachmentError.message}
        </div>
      )}

      {error && (
        <div className="nex-composer-error nex-fade-in" role="alert">
          {error.message}
        </div>
      )}

      {agentError && (
        <div className="nex-composer-error nex-fade-in" role="alert">
          {agentError}
        </div>
      )}

      <div className="nex-composer">
        <div className="nex-composer-inner">
          <div className="nex-composer-shell">
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
            <textarea
              className="nex-composer-input"
              rows={1}
              placeholder="Message"
              aria-label="Message"
              value={draft}
              disabled={sending || agentBusy}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  void handleSubmit();
                }
              }}
            />
            <div className="nex-composer-bar">
              <Tooltip label="Add file">
                <button
                  type="button"
                  className="nex-composer-attach"
                  aria-label="Add file"
                  disabled={sending || attachmentsBusy || agentBusy}
                  onClick={() => void pickAndAttach()}
                >
                  <PaperclipIcon />
                </button>
              </Tooltip>
              <label className="nex-composer-agent-toggle" title="Stream steps via the agent (opt-in)">
                <input
                  type="checkbox"
                  checked={agentMode}
                  onChange={(e) => setAgentMode(e.target.checked)}
                  aria-label="Agent mode"
                />
                <span>Agent</span>
              </label>
              <span className="nex-composer-bar-spacer" />
              {selectedModel && (
                <span
                  className="nex-tag nex-tag-mono nex-composer-model-tag"
                  title="Model used for new messages"
                >
                  {selectedModel}
                </span>
              )}
              <button
                type="button"
                className="nex-composer-send nex-morph-pill"
                onClick={() => void handleSubmit()}
                disabled={!canSend}
                aria-busy={sending || agentBusy}
              >
                {sending || agentBusy ? (
                  <>
                    <span className="nex-spinner" aria-hidden="true" />
                    {agentMode ? "Running…" : "Sending…"}
                  </>
                ) : (
                  <>
                    Send
                    <ArrowUpIcon aria-hidden="true" />
                  </>
                )}
              </button>
            </div>
          </div>
          {ready ? (
            <p className="nex-composer-hint">
              <span className="nex-composer-shortcut">
                Enter to send · Shift+Enter for a new line
              </span>
            </p>
          ) : (
            <p className="nex-composer-hint">
              <span className="nex-composer-shortcut">
                Choose a provider and model in Settings to send messages.{" "}
              </span>
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