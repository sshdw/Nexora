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
//!
//! #220: the composer moved to `Composer.tsx` — it is now one dominant
//! control on the black theme, and testing it does not need a thread,
//! a backend or an agent run around it. Everything below this line
//! still behaves exactly as before: this component keeps owning the
//! hooks, the send guards, the draft and the error surfaces, and hands
//! the composer a plain set of callbacks.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { formatRelativeTime } from "../lib/format";
import {
  agentSetMode,
  cancelAgentRun,
  extendAgentRun,
  pauseAgentRun,
  resolveAgentApproval,
  resumeAgentRun,
  startAgentRun,
  type AutonomyMode,
} from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import { useAgentRun } from "../lib/useAgentRun";
import { useAttachments } from "../lib/useAttachments";
import { useConversation } from "../lib/useConversation";
import AgentRunSteps from "./AgentRunSteps";
import Composer from "./Composer";
import ContextPanel from "./ContextPanel";
import { useStrings } from "../lib/useLocale";
import M3LoadingIndicator from "./M3LoadingIndicator";
import M3SegmentedGroup from "./M3SegmentedGroup";
import NexoraMark from "./NexoraMark";

export interface ConversationViewProps {
  conversationId: number;
  /** Internal name of the selected provider (FR-004), or null if none. */
  selectedProvider: string | null;
  /** Selected model for the selected provider, or null if none. */
  selectedModel: string | null;
  /** Models the selected provider offers (#220). The composer lists
   * these; it never invents a model, and it never keeps its own
   * copy of the selection. */
  models: readonly string[];
  /** Persist a model choice through the app's provider store (the
   * single source of truth, FR-012). */
  onSelectModel: (model: string) => void;
  /** Last model-selection failure, surfaced in the composer's strip. */
  modelError: string | null;
  /** Agent-autonomy default store lifted in App (single source, NEX-FE-003):
   * the header selector reads and writes the same store as Settings, so a
   * change in either surface reflects in the other immediately. */
  autonomy: AutonomyStore;
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
  models,
  onSelectModel,
  modelError,
  autonomy,
  onOpenSettings,
  onMessageSent,
  draft,
  setDraft,
}: ConversationViewProps) {
  const { locale, t } = useStrings();
  const { messages, loading, error, sending, send } = useConversation(conversationId);
  const {
    attachments,
    error: attachmentError,
    busy: attachmentsBusy,
    pickAndAttach,
    remove,
    refresh: refreshAttachments,
  } = useAttachments(conversationId);
  const { runs, reload: reloadAgent, expandStep, collapseStep } = useAgentRun(conversationId);
  const [agentMode, setAgentMode] = useState<boolean>(false);
  const [viewMode, setViewMode] = useState<"chat" | "context">("chat");
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
  const hasActiveRun = runs.some((r) => r.status === "running" || r.status === "paused" || r.status === "budget_exhausted");
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

  const handleAgentPause = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await pauseAgentRun(runId);
    } catch (e) {
      setAgentError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const handleAgentResume = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await resumeAgentRun(runId);
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

  // Dispatchers handed to `AgentRunSteps` (#148/#153): the run id travels in
  // the callback instead of being captured by a fresh inline arrow per run,
  // so every run keeps the same prop identity across renders and the memoized
  // row re-renders only when its own data changes. `threadItems` is
  // deliberately NOT memoized here — it is rebuilt each render on purpose and
  // #148 owns that decision.
  const dispatchAgentApprove = useCallback(
    (runId: number, callId: string, approved: boolean) => {
      void handleAgentApprove(runId, callId, approved);
    },
    [handleAgentApprove],
  );
  const dispatchAgentCancel = useCallback(
    (runId: number) => {
      void handleAgentCancel(runId);
    },
    [handleAgentCancel],
  );
  const dispatchAgentContinue = useCallback(
    (runId: number) => {
      void handleAgentContinue(runId);
    },
    [handleAgentContinue],
  );
  const dispatchAgentPause = useCallback(
    (runId: number) => {
      void handleAgentPause(runId);
    },
    [handleAgentPause],
  );
  const dispatchAgentResume = useCallback(
    (runId: number) => {
      void handleAgentResume(runId);
    },
    [handleAgentResume],
  );
  const dispatchExpandStep = useCallback(
    (runId: number, seq: number) => {
      void expandStep(runId, seq);
    },
    [expandStep],
  );
  const dispatchCollapseStep = useCallback(
    (runId: number, seq: number) => {
      collapseStep(runId, seq);
    },
    [collapseStep],
  );

  const handleAutonomyChange = useCallback(
    async (mode: AutonomyMode) => {
      // Single-source write (NEX-FE-003): the store optimistically updates
      // and rolls back on failure, so Settings reflects the change (or its
      // rollback) immediately. A rejected persist surfaces here and skips
      // the live-switch, as before.
      const failure = await autonomy.setMode(mode);
      if (failure !== null) {
        setAgentError(failure);
        return;
      }
      // Live-switch any active run's gate (DP-AUTONOMY): parked approval stays.
      for (const run of runs) {
        if (run.status === "running" || run.status === "paused") {
          try {
            await agentSetMode(run.run_id, mode);
          } catch {
            // Best-effort: a run that just finished will miss, ignore.
          }
        }
      }
    },
    [autonomy, runs],
  );

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

  // Stagger order for the spatial slide-in: genuinely inserted messages
  // animate in sequence (contract motion table streaming/insertion row);
  // reduced motion collapses to instant append via the global gate.
  const freshOrder = new Map<number, number>();
  {
    let order = 0;
    for (const item of threadItems) {
      if (item.kind === "message" && freshMessageIds?.has(item.message.id)) {
        freshOrder.set(item.message.id, Math.min(order, 6));
        order += 1;
      }
    }
  }
  // Tone + spacing grouping: a message that directly follows another of
  // the same role reads as one turn cluster (spacing groups, never
  // per-row borders). Runs break the chain.
  let prevThreadRole: string | null = null;

  return (
    <div className="nex-main-conversation">
      <div className="nex-context-tabs">
        <M3SegmentedGroup
          label={t("conv.viewLabel")}
          semantics="tabs"
          value={viewMode}
          onChange={setViewMode}
          options={[
            { value: "chat", label: t("conv.chatTab") },
            { value: "context", label: t("conv.contextTab") },
          ]}
        />
      </div>
      {viewMode === "context" ? (
        <ContextPanel conversationId={conversationId} />
      ) : (
        <>
      <div className="nex-thread" ref={threadRef} role="log" aria-label={t("conv.messagesAria")}>
        {loading ? (
          <M3LoadingIndicator label={t("conv.loadingMessages")} />
        ) : threadItems.length === 0 ? (
          <div className="nex-conversation-placeholder nex-empty-enter">
            <span className="nex-conversation-empty-mark-wrap" aria-hidden="true">
              <NexoraMark className="nex-conversation-empty-mark" width={26} height={26} />
            </span>
            <h2 className="nex-conversation-empty-title">{t("conv.emptyTitle")}</h2>
            <p className="nex-conversation-empty-text">
              {t("conv.emptyText")}
            </p>
          </div>
        ) : (
          threadItems.map((item) => {
            if (item.kind === "run") {
              prevThreadRole = null;
              return (
                <AgentRunSteps
                  key={`run-${item.run.run_id}`}
                  run={item.run}
                  onResolveApproval={dispatchAgentApprove}
                  onCancel={dispatchAgentCancel}
                  onContinue={dispatchAgentContinue}
                  onPause={dispatchAgentPause}
                  onResume={dispatchAgentResume}
                  onExpandStep={dispatchExpandStep}
                  onCollapseStep={dispatchCollapseStep}
                />
              );
            }
            const isFresh = freshMessageIds?.has(item.message.id) ?? false;
            const grouped = prevThreadRole === item.message.role;
            prevThreadRole = item.message.role;
            return (
              <article
                key={`msg-${item.message.id}`}
                className={
                  "nex-message " +
                  (item.message.role === "user"
                    ? "nex-message-user"
                    : "nex-message-assistant") +
                  (grouped ? " nex-message-grouped" : "") +
                  (isFresh ? " nex-message-enter" : "")
                }
                style={
                  isFresh
                    ? ({ "--nex-chat-insert-index": freshOrder.get(item.message.id) ?? 0 } as CSSProperties)
                    : undefined
                }
              >
                <div className="nex-message-meta">
                  <span className="nex-message-author">
                    {item.message.role === "user" ? t("conv.authorYou") : t("conv.authorAssistant")}
                  </span>
                  {item.message.role === "assistant" && item.message.model_name && (
                    <span
                      className="nex-message-origin"
                      title={t("conv.originTitle")}
                    >
                      {item.message.model_name}
                    </span>
                  )}
                  <time
                    className="nex-message-time"
                    dateTime={new Date(item.message.created_at * 1000).toISOString()}
                  >
                    {formatRelativeTime(item.message.created_at, locale)}
                  </time>
                </div>
                <div className="nex-message-body">{item.message.content}</div>
              </article>
            );
          })
        )}
        {sending && !agentMode && (
          <div className="nex-thread-waiting">
            <M3LoadingIndicator label={t("conv.responding")} size="sm" />
          </div>
        )}
        {agentBusy && (
          <div className="nex-thread-waiting">
            <M3LoadingIndicator label={t("conv.agentWorking")} size="sm" />
          </div>
        )}
      </div>

      {/* The COMPOSER's own failure strips (#220). These used to carry
          the shared `.nex-composer-error`, which nine other panels also
          use and which stays on the M3E error tokens in
          conversation.css — repainting it here recoloured all of them.
          The composer's strips get their own class instead. */}
      {attachmentError && (
        <div className="nex-composer-strip-error nex-fade-in" role="alert">
          {attachmentError.message}
        </div>
      )}

      {error && (
        <div className="nex-composer-strip-error nex-fade-in" role="alert">
          {error.message}
        </div>
      )}

      {agentError && (
        <div className="nex-composer-strip-error nex-fade-in" role="alert">
          {agentError}
        </div>
      )}

      <Composer
        draft={draft}
        onDraftChange={setDraft}
        onSubmit={() => void handleSubmit()}
        sending={sending}
        agentBusy={agentBusy}
        attachmentsBusy={attachmentsBusy}
        canSend={canSend}
        attachments={attachments}
        onPickFiles={() => void pickAndAttach()}
        onRemoveAttachment={(id) => void remove(id)}
        agentMode={agentMode}
        onAgentModeChange={setAgentMode}
        autonomy={autonomy}
        onAutonomyChange={(mode) => void handleAutonomyChange(mode)}
        selectedModel={selectedModel}
        models={models}
        onSelectModel={onSelectModel}
        modelError={modelError}
        ready={ready}
        onOpenSettings={onOpenSettings}
      />
        </>
      )}
    </div>
  );
}
