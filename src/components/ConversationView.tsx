//! Conversation workspace: persisted history, send/receive flow, and the
//! composer (FR-003, FR-005).
//!
//! This is a thin presentation layer over the existing hooks and backend: the
//! conversation's message list is loaded whole from `conversation_history` and
//! `send` delegates to the existing `send_message` command with the currently
//! selected provider/model (FR-004). User and assistant messages are kept
//! visually distinguishable by role, chronological order comes from the
//! backend, and request failures surface as a visible error without inventing
//! or corrupting persisted state. Failed Chat sends retain the text in the
//! composer so it can be resent without retyping —
//! frontend draft state only; persisted history is never modified.
//!
//! #220: the composer moved to `Composer.tsx` — it is now one dominant
//! control on the black theme, and testing it does not need a thread,
//! a backend or an agent run around it. Everything below this line
//! still behaves exactly as before: this component keeps owning the
//! hooks, the send guards, the draft and the error surfaces, and hands
//! the composer a plain set of callbacks.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { CSSProperties } from "react";

import { formatRelativeTime } from "../lib/format";
import {
  agentSetMode,
  cancelAgentRun,
  compactConversationContext,
  continueAgentRun,
  decideRunPlan,
  extendAgentRun,
  formatIpcError,
  listRunCheckpoints,
  pauseAgentRun,
  resolveAgentApproval,
  resumeAgentRun,
  startAgentRun,
  type AutonomyMode,
  type CompactionResult,
  type ConnectionModelCapabilities,
  type PlanDecisionKind,
  type PromptContextView,
} from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import { useAgentRun } from "../lib/useAgentRun";
import { useAttachments } from "../lib/useAttachments";
import { useComposerContext, type ComposerPromptDraftStore } from "../lib/useComposerContext";
import { useConversation } from "../lib/useConversation";
import { useWorkQueue } from "../lib/useWorkQueue";
import type { WorkspaceStore } from "../lib/useWorkspace";
import AgentRunSteps from "./AgentRunSteps";
import ApprovalPanel from "./runSteps/ApprovalPanel";
import Composer from "./Composer";
import ConfirmDialog from "./ConfirmDialog";
import PlanDecisionPanel from "./PlanDecisionPanel";
import QuestionPanel from "./QuestionPanel";
import WorkQueueStrip from "./WorkQueueStrip";
import MessageMarkdown from "./MessageMarkdown";
import ThinkingBlock from "./ThinkingBlock";
import type { ModelConnectionGroup } from "./ModelPicker";
import { useStrings } from "../lib/useLocale";
import { ArrowDownIcon } from "./icons";
import LoadingIndicator from "./LoadingIndicator";

const AT_BOTTOM_THRESHOLD_PX = 48;

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
  connectionGroups?: readonly ModelConnectionGroup[];
  onSelectConnectionModel?: (connectionId: string, model: string) => void;
  selectedCapabilities?: ConnectionModelCapabilities | null;
  capabilitiesByKey?: ReadonlyMap<string, ConnectionModelCapabilities>;
  selectedEffort?: string | null;
  onSelectEffort?: (effort: string | null) => void;
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
  onDraftAccepted?: () => void;
  promptDraft?: ComposerPromptDraftStore;
  reviewSend?: { pending: boolean; begin: () => (() => void) | null };
  /** Effective workspace store, for the composer's workspace control
   *  (D3 / R6-03). */
  workspace: WorkspaceStore;
  /** Optional first message staged from a draft thread when the DB row is created. */
  initialPrompt?: {
    content: string;
    agentMode: boolean;
    planMode: boolean;
  } | null;
  onInitialPromptConsumed?: () => void;
}

export default function ConversationView({
  conversationId,
  selectedProvider,
  selectedModel,
  models,
  onSelectModel,
  connectionGroups,
  onSelectConnectionModel,
  selectedCapabilities = null,
  capabilitiesByKey,
  selectedEffort = null,
  onSelectEffort,
  modelError,
  autonomy,
  onOpenSettings,
  onMessageSent,
  draft,
  setDraft,
  onDraftAccepted,
  promptDraft,
  reviewSend,
  workspace,
  initialPrompt,
  onInitialPromptConsumed,
}: ConversationViewProps) {
  const { locale, t } = useStrings();
  const [undo, setUndo] = useState<{ runId: number; turn: number; count: number } | null>(null);
  const [undoBusy, setUndoBusy] = useState(false);
  const {
    messages,
    loading,
    error,
    sending: chatSending,
    streamingText,
    streamingThinking,
    thinkingByMessageId = {},
    send,
    reload: reloadChat,
  } = useConversation(conversationId);
  const sending = chatSending || reviewSend?.pending === true;
  const waitingForUnmountedReview = useRef(false);
  useEffect(() => {
    if (reviewSend?.pending && !chatSending) waitingForUnmountedReview.current = true;
    if (!reviewSend?.pending && waitingForUnmountedReview.current) {
      waitingForUnmountedReview.current = false;
      void reloadChat({ silent: true });
    }
  }, [chatSending, reloadChat, reviewSend?.pending]);
  const {
    attachments,
    error: attachmentError,
    busy: attachmentsBusy,
    pickAndAttach,
    remove,
    refresh: refreshAttachments,
  } = useAttachments(conversationId);
  const {
    runs,
    compactionVersion = 0,
    reload: reloadAgent,
    loadRunSteps,
    expandStep,
    collapseStep,
    loadRunCheckpoints,
    revertCheckpoint,
    revertTurn,
    answerQuestion,
    clarifyRun,
    applyPlanDecision,
    killSubagent,
  } = useAgentRun(conversationId);
  const {
    items: queueItems,
    enqueue: enqueueQueueItem,
    updateItem: updateQueueItem,
    moveItem: moveQueueItem,
    cancelItem: cancelQueueItem,
    runNext: runNextQueueItem,
  } = useWorkQueue(conversationId);
  // R4-02: the pane opens in Agent mode. Chat is still one click away on
  // the composer's pills, and the choice is pane state for the session.
  const [agentMode, setAgentMode] = useState<boolean>(true);
  const [planMode, setPlanMode] = useState<boolean>(false);
  const [agentBusy, setAgentBusy] = useState<boolean>(false);
  const [liveActionBusy, setLiveActionBusy] = useState<boolean>(false);
  const [agentError, setAgentError] = useState<string | null>(null);
  const [compactBusy, setCompactBusy] = useState<boolean>(false);
  const [manualCompactVersion, setManualCompactVersion] = useState<number>(0);
  const [compactNotice, setCompactNotice] = useState<string | null>(null);
  const threadRef = useRef<HTMLDivElement>(null);
  const chatDraftRef = useRef<{ conversationId: number; draft: string } | null>(
    null
  );
  useLayoutEffect(() => {
    // A new identity on each draft/conversation change also detects edits
    // followed by a return to the original text, and unmount invalidates it.
    chatDraftRef.current = { conversationId, draft };
    return () => {
      chatDraftRef.current = null;
    };
  }, [conversationId, draft]);

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
            .map((message) => message.id)
        )
      : null;
  useLayoutEffect(() => {
    prevMessagesRef.current = {
      conversationId,
      ids: new Set(messages.map((message) => message.id)),
    };
  }, [conversationId, messages]);

  const ready = selectedProvider !== null && selectedModel !== null;
  const activeRun =
    runs.find(
      (r) =>
        r.status === "running" ||
        r.status === "paused" ||
        r.status === "budget_exhausted"
    ) ?? null;
  const liveRunId = activeRun?.run_id ?? null;
  const hasActiveRun = activeRun !== null;
  const promptContext = useComposerContext(conversationId, workspace.root, draft, hasActiveRun, promptDraft);
  const pendingQuestion = activeRun?.pending_question ?? null;
  const pendingApprovalRun = useMemo(
    () =>
      runs.find(
        (r) => r.status !== "interrupted" && r.pending_approval !== null
      ) ?? null,
    [runs]
  );
  const pendingPlanRun = useMemo(() => {
    const candidates = runs.filter(
      (r) =>
        r.is_plan === true &&
        r.status === "completed" &&
        r.plan_decision_status === "pending" &&
        Boolean((r.plan_content ?? r.final_content ?? "").trim())
    );
    if (candidates.length === 0) return null;
    return candidates.reduce((best, run) =>
      run.started_at > best.started_at ||
      (run.started_at === best.started_at && run.run_id > best.run_id)
        ? run
        : best
    );
  }, [runs]);
  const latestRun = useMemo(
    () =>
      runs.reduce<(typeof runs)[number] | null>(
        (best, run) =>
          !best ||
          run.started_at > best.started_at ||
          (run.started_at === best.started_at && run.run_id > best.run_id)
            ? run
            : best,
        null
      ),
    [runs]
  );
  const queuePaused =
    !hasActiveRun &&
    queueItems.length > 0 &&
    latestRun !== null &&
    latestRun.status !== "completed";
  const canSend =
    ready &&
    draft.trim() !== "" &&
    !promptContext.pending &&
    !sending &&
    !attachmentsBusy &&
    !agentBusy &&
    !hasActiveRun;

  // D4: the context ring refreshes on conversation change (the pane is keyed
  // by conversation id) and whenever the run store OR the message store
  // advances. A content signature, not a timer and not an effect: a step
  // frame, a status flip or a newly persisted message changes the string, an
  // unrelated render does not. Messages contribute their count and newest id,
  // not their text: a re-render with the same history keeps the string (no
  // refetch), while a persisted user/assistant row changes it. Counting the
  // text would refetch on every streamed chunk; counting nothing at all is
  // the Chat-mode bug this replaces.
  const contextRevision = useMemo(() => {
    const runSignature = runs
      .map(
        (run) =>
          `${run.run_id}:${run.status}:${run.finished_at ?? ""}:${run.compaction_status ?? ""}:${run.steps
            .map((step) => `${step.seq}:${step.status ?? ""}`)
            .join(",")}`
      )
      .join("|");
    const newest = messages[messages.length - 1];
    return `${messages.length}:${newest?.id ?? ""}::${compactionVersion}:${manualCompactVersion}::${runSignature}`;
  }, [messages, runs, compactionVersion, manualCompactVersion]);

  useEffect(() => {
    setCompactNotice(null);
  }, [conversationId]);

  const handleCompacted = useCallback(
    (result: CompactionResult) => {
      setManualCompactVersion((v) => v + 1);
      if (result.status === "scheduled") {
        setCompactNotice(t("ctx.compactedScheduled"));
      } else if (result.status === "nothing_to_compact") {
        setCompactNotice(t("ctx.compactedNothing"));
      } else {
        setCompactNotice(
          t("ctx.compactedNotice", {
            summarized: result.summarized_messages,
            retained: result.retained_messages,
          })
        );
      }
    },
    [t]
  );

  const handleCompactContext = useCallback(async () => {
    if (compactBusy) return;
    setAgentError(null);
    setCompactNotice(null);
    setCompactBusy(true);
    try {
      const hasSelection = Boolean(selectedProvider && selectedModel);
      const result = hasSelection
        ? await compactConversationContext(conversationId, {
            connectionId: selectedProvider,
            modelId: selectedModel,
          })
        : await compactConversationContext(conversationId);
      handleCompacted(result);
    } catch (e) {
      setAgentError(formatIpcError(e, t("ctx.compactFailed")));
    } finally {
      setCompactBusy(false);
    }
  }, [
    compactBusy,
    conversationId,
    handleCompacted,
    selectedModel,
    selectedProvider,
    t,
  ]);

  const shouldAutoScrollRef = useRef(true);
  const [showScrollDown, setShowScrollDown] = useState(false);

  useEffect(() => {
    shouldAutoScrollRef.current = true;
    setShowScrollDown(false);
  }, [conversationId]);

  const handleThreadScroll = useCallback(() => {
    const el = threadRef.current;
    if (!el) return;
    const atBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight <=
      AT_BOTTOM_THRESHOLD_PX;
    shouldAutoScrollRef.current = atBottom;
    setShowScrollDown(!atBottom);
  }, []);

  const scrollToBottom = useCallback(() => {
    const el = threadRef.current;
    shouldAutoScrollRef.current = true;
    setShowScrollDown(false);
    if (!el) return;
    const reduced =
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (typeof el.scrollTo === "function") {
      el.scrollTo({
        top: el.scrollHeight,
        behavior: reduced ? "auto" : "smooth",
      });
    } else {
      el.scrollTop = el.scrollHeight;
    }
  }, []);

  // Keep the newest message in view only when the user is already at the bottom.
  useLayoutEffect(() => {
    const el = threadRef.current;
    if (el && shouldAutoScrollRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, loading, sending, runs, agentBusy]);

  const handleAgentCancel = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await cancelAgentRun(runId);
    } catch (e) {
      setAgentError(formatIpcError(e));
    }
  }, []);

  // D2: the composer's one button becomes a stop while a run is live.
  const handleStop = useCallback(() => {
    if (activeRun !== null) void handleAgentCancel(activeRun.run_id);
  }, [activeRun, handleAgentCancel]);

  const handleAgentPause = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await pauseAgentRun(runId);
    } catch (e) {
      setAgentError(formatIpcError(e));
    }
  }, []);

  const handleAgentResume = useCallback(async (runId: number) => {
    setAgentError(null);
    try {
      await resumeAgentRun(runId);
    } catch (e) {
      setAgentError(formatIpcError(e));
    }
  }, []);

  const handleAgentApprove = useCallback(
    async (
      runId: number,
      callId: string,
      approved: boolean,
      scope?: "single" | "group"
    ) => {
      setAgentError(null);
      try {
        await resolveAgentApproval(runId, callId, approved, scope);
      } catch (e) {
        setAgentError(formatIpcError(e));
      }
    },
    []
  );

  const effectiveEffort = useMemo(() => {
    if (!selectedEffort) return null;
    if (selectedCapabilities) {
      return selectedCapabilities.supported_efforts.includes(selectedEffort)
        ? selectedEffort
        : null;
    }
    return selectedEffort;
  }, [selectedEffort, selectedCapabilities]);

  const handleAgentContinue = useCallback(
    async (runId: number) => {
      setAgentError(null);
      try {
        const target = runs.find((r) => r.run_id === runId);
        const isInterrupted =
          target?.interrupted === true || target?.status === "interrupted";
        if (isInterrupted) {
          if (!selectedProvider || !selectedModel) return;
          await continueAgentRun(
            runId,
            selectedProvider,
            selectedModel,
            effectiveEffort
          );
          await reloadAgent();
        } else {
          await extendAgentRun(runId, 10);
        }
      } catch (e) {
        setAgentError(formatIpcError(e));
      }
    },
    [runs, selectedProvider, selectedModel, effectiveEffort, reloadAgent]
  );

  const handlePlanDecision = useCallback(
    async (runId: number, decision: PlanDecisionKind, reply?: string) => {
      setAgentError(null);
      const wantsImplementation =
        decision === "accepted" || decision === "custom_reply";
      const res = await decideRunPlan(
        runId,
        decision,
        reply ?? null,
        wantsImplementation ? selectedProvider : null,
        wantsImplementation ? selectedModel : null,
        wantsImplementation ? effectiveEffort : null
      );
      applyPlanDecision?.(runId, res.plan);
      if (wantsImplementation) {
        setPlanMode(false);
      }
      await reloadAgent();
    },
    [
      selectedProvider,
      selectedModel,
      effectiveEffort,
      applyPlanDecision,
      reloadAgent,
    ]
  );

  // Dispatchers handed to `AgentRunSteps` (#148/#153): the run id travels in
  // the callback instead of being captured by a fresh inline arrow per run,
  // so every run keeps the same prop identity across renders and the memoized
  // row re-renders only when its own data changes. (`threadItems` below is
  // memoized per AC-203, so a re-render no longer rebuilds the whole thread
  // array either.)
  const dispatchAgentApprove = useCallback(
    (
      runId: number,
      callId: string,
      approved: boolean,
      scope?: "single" | "group"
    ) => {
      void handleAgentApprove(runId, callId, approved, scope);
    },
    [handleAgentApprove]
  );
  const dispatchAgentCancel = useCallback(
    (runId: number) => {
      void handleAgentCancel(runId);
    },
    [handleAgentCancel]
  );
  const dispatchKillSubagent = useCallback(
    (runId: number, subagentId: string) => {
      void killSubagent(runId, subagentId);
    },
    [killSubagent]
  );
  const dispatchAgentContinue = useCallback(
    (runId: number) => {
      void handleAgentContinue(runId);
    },
    [handleAgentContinue]
  );
  const dispatchAgentPause = useCallback(
    (runId: number) => {
      void handleAgentPause(runId);
    },
    [handleAgentPause]
  );
  const dispatchAgentResume = useCallback(
    (runId: number) => {
      void handleAgentResume(runId);
    },
    [handleAgentResume]
  );
  const dispatchExpandStep = useCallback(
    (runId: number, seq: number) => {
      void expandStep(runId, seq);
    },
    [expandStep]
  );
  // AC-102/ADD-08 lazy steps: AgentRunSteps asks when a run's step list is
  // visible but unloaded; the store owns the dedupe. Stable identity like
  // every dispatcher above so the memoized run row keeps its props.
  const dispatchLoadRunSteps = useCallback(
    (runId: number) => {
      void loadRunSteps(runId);
    },
    [loadRunSteps]
  );
  const dispatchCollapseStep = useCallback(
    (runId: number, seq: number) => {
      collapseStep(runId, seq);
    },
    [collapseStep]
  );
  const dispatchLoadCheckpoints = useCallback(
    (runId: number) => {
      void loadRunCheckpoints(runId);
    },
    [loadRunCheckpoints]
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
    [autonomy, runs]
  );

  const clearAcceptedDraft = (submittedDraft: typeof chatDraftRef.current) => {
    if (onDraftAccepted) onDraftAccepted();
    else if (chatDraftRef.current === submittedDraft) setDraft("");
  };

  const handleReview = async (view: PromptContextView) => {
    if (!ready || !selectedProvider || !selectedModel || sending || agentBusy || hasActiveRun) return false;
    const release = reviewSend?.begin();
    if (release === null) return false;
    const submittedDraft = chatDraftRef.current;
    try {
      const failure = await send("/review", selectedProvider, selectedModel, [], effectiveEffort,
        { contexts: [view.reference], review: true });
      if (failure !== null) return false;
      if (draft.trim() === "/review") clearAcceptedDraft(submittedDraft);
      onMessageSent?.();
      return true;
    } finally {
      release?.();
    }
  };

  const handleUndo = async () => {
    if (undoBusy || hasActiveRun || sending || agentBusy) return;
    setUndoBusy(true);
    setAgentError(null);
    try {
      const candidates = [...runs].sort((left, right) => right.run_id - left.run_id);
      for (const run of candidates) {
        const checkpoints = await listRunCheckpoints(run.run_id);
        const eligible = checkpoints.filter((item) => item.reverted_at === null && item.post_blob_sha256 !== null);
        if (eligible.length === 0) continue;
        const latest = Math.max(...eligible.map((item) => item.turn_index));
        setUndo({ runId: run.run_id, turn: latest, count: eligible.filter((item) => item.turn_index === latest).length });
        return;
      }
      setAgentError(t("prompt.undoEmpty"));
    } catch {
      setAgentError(t("prompt.undoFailed"));
    } finally {
      setUndoBusy(false);
    }
  };

  const confirmUndo = async () => {
    if (!undo || undoBusy || hasActiveRun) return;
    const submittedDraft = chatDraftRef.current;
    setUndoBusy(true);
    try {
      const outcome = await revertTurn(undo.runId, undo.turn, false);
      if (outcome.status === "reverted") {
        if (draft.trim() === "/undo") clearAcceptedDraft(submittedDraft);
      } else {
        setAgentError(outcome.message);
      }
      setUndo(null);
    } finally {
      setUndoBusy(false);
    }
  };

  const submitContent = useCallback(
    async (content: string, useAgentMode: boolean, usePlanMode: boolean) => {
      if (!content || !selectedProvider || !selectedModel) return;
      shouldAutoScrollRef.current = true;
      setShowScrollDown(false);
      const attachmentIds = attachments.map((attachment) => attachment.id);
      const hasAutomaticSkills = Boolean(
        promptContext.references?.automatic_skills?.length
      );
      // Agent path: opt-in streaming via start_agent_run (DP-3, DP-4).
      if (useAgentMode) {
        setAgentError(null);
        setAgentBusy(true);
        const submittedDraft = chatDraftRef.current;
        try {
          await startAgentRun(
            conversationId,
            content,
            selectedProvider,
            selectedModel,
            effectiveEffort,
            useAgentMode && usePlanMode,
            attachmentIds,
            ...(promptContext.references ? [promptContext.references] : [])
          );
          clearAcceptedDraft(submittedDraft);
          promptContext.consumeSelection();
          // User message persisted before spawn; refresh both histories.
          await Promise.all([refreshAttachments(), reloadAgent()]);
          onMessageSent?.();
        } catch (e) {
          // Start failed: surface error, restore draft, and reload to show persisted state.
          setAgentError(formatIpcError(e));
          await Promise.all([
            refreshAttachments(),
            reloadAgent(),
            ...(hasAutomaticSkills ? [promptContext.refresh()] : []),
          ]);
        } finally {
          setAgentBusy(false);
        }
        return;
      }
      const submittedDraft = chatDraftRef.current;
      const failure = await send(
        content,
        selectedProvider,
        selectedModel,
        attachmentIds,
        effectiveEffort,
        ...(promptContext.references ? [promptContext.references] : [])
      );
      // Admission can reject before the user message is persisted. Keep the
      // draft for every failure, including provider requests. On success the
      // tab owns draft reconciliation even if this pane has unmounted.
      // History remains exclusively backend-owned (DATABASE.md §7.2).
      if (failure === null) {
        clearAcceptedDraft(submittedDraft);
        promptContext.consumeSelection();
      }
      if (failure !== null && hasAutomaticSkills) await promptContext.refresh();
      // The backend is the source of truth: on success the drafts are now
      // message-linked and disappear; on failure they remain drafts and
      // reappear unchanged (FR-008 draft lifecycle).
      await refreshAttachments();
      onMessageSent?.();
    },
    [
      attachments,
      clearAcceptedDraft,
      conversationId,
      effectiveEffort,
      onMessageSent,
      promptContext,
      refreshAttachments,
      reloadAgent,
      selectedModel,
      selectedProvider,
      send,
      startAgentRun,
    ]
  );

  const consumedInitialPromptRef = useRef<number | null>(null);
  useEffect(() => {
    if (!initialPrompt) return;
    if (consumedInitialPromptRef.current === conversationId) return;
    consumedInitialPromptRef.current = conversationId;
    setAgentMode(initialPrompt.agentMode);
    setPlanMode(initialPrompt.planMode);
    onInitialPromptConsumed?.();
    void submitContent(
      initialPrompt.content,
      initialPrompt.agentMode,
      initialPrompt.planMode
    );
  }, [conversationId, initialPrompt, onInitialPromptConsumed, submitContent]);

  const handleSubmit = async () => {
    const content = draft.trim();
    if (content === "/compact") {
      setDraft("");
      await handleCompactContext();
      return;
    }
    if (content === "/undo") {
      await handleUndo();
      return;
    }
    if (content === "/review") {
      if (ready && !sending && !agentBusy && !hasActiveRun) promptContext.setSheet({ kind: "review" });
      return;
    }
    if (!canSend || !content || !selectedProvider || !selectedModel) return;
    await submitContent(content, agentMode, planMode);
  };

  const handleClarifyNow = useCallback(async () => {
    const content = draft.trim();
    if (!content || liveRunId === null || liveActionBusy || attachments.length > 0 || promptContext.active) return;
    setAgentError(null);
    setLiveActionBusy(true);
    try {
      await clarifyRun(liveRunId, content);
      setDraft("");
    } catch (e) {
      setAgentError(formatIpcError(e));
    } finally {
      setLiveActionBusy(false);
    }
  }, [draft, liveRunId, liveActionBusy, clarifyRun, setDraft, attachments.length, promptContext.active]);

  const handleQueuePrompt = useCallback(async () => {
    const content = draft.trim();
    if (!content || !selectedProvider || !selectedModel || liveActionBusy || attachments.length > 0 || promptContext.active) return;
    setAgentError(null);
    setLiveActionBusy(true);
    try {
      await enqueueQueueItem(
        content,
        selectedProvider,
        selectedModel,
        effectiveEffort
      );
      setDraft("");
    } catch (e) {
      setAgentError(formatIpcError(e));
    } finally {
      setLiveActionBusy(false);
    }
  }, [
    draft,
    selectedProvider,
    selectedModel,
    effectiveEffort,
    liveActionBusy,
    attachments.length,
    promptContext.active,
    enqueueQueueItem,
    setDraft,
  ]);

  const handleAnswerQuestion = useCallback(
    async (selectedOptions: string[], freeText?: string) => {
      if (!activeRun || !pendingQuestion) return;
      setAgentError(null);
      await answerQuestion(
        activeRun.run_id,
        pendingQuestion.question_id,
        selectedOptions,
        freeText
      );
      await reloadAgent();
    },
    [activeRun, pendingQuestion, answerQuestion, reloadAgent]
  );

  const handleRunNextQueued = useCallback(async () => {
    setAgentError(null);
    try {
      await runNextQueueItem();
      await reloadAgent();
    } catch (e) {
      setAgentError(formatIpcError(e));
    }
  }, [runNextQueueItem, reloadAgent]);

  // Chronological thread: messages and agent runs interleaved by timestamp
  // (Task 5.1 DP-6). Memoized per AC-203 (review 03 §2): the array identity
  // is stable across renders that touch neither `messages` nor `runs`, so a
  // keystroke in the composer no longer rebuilds the whole thread.
  const threadItems: Array<
    | { kind: "message"; ts: number; message: (typeof messages)[number] }
    | { kind: "run"; ts: number; run: (typeof runs)[number] }
  > = useMemo(
    () =>
      [
        ...messages.map((m) => ({
          kind: "message" as const,
          ts: m.created_at,
          message: m,
        })),
        ...runs.map((r) => ({
          kind: "run" as const,
          ts: r.started_at,
          run: r,
        })),
      ].sort((a, b) => a.ts - b.ts),
    [messages, runs]
  );

  // Stagger order for the spatial slide-in: genuinely inserted messages
  // animate in sequence (contract motion table streaming/insertion row);
  // reduced motion collapses to instant append via the global gate.
  // Memoized per AC-203 alongside `threadItems` (same input set plus the
  // fresh-message window).
  const freshOrder = useMemo(() => {
    const order = new Map<number, number>();
    let index = 0;
    for (const item of threadItems) {
      if (item.kind === "message" && freshMessageIds?.has(item.message.id)) {
        order.set(item.message.id, Math.min(index, 6));
        index += 1;
      }
    }
    return order;
  }, [threadItems, freshMessageIds]);
  // Tone + spacing grouping: a message that directly follows another of
  // the same role reads as one turn cluster (spacing groups, never
  // per-row borders). Runs break the chain.
  let prevThreadRole: string | null = null;

  // An empty conversation is not a scrolled thread with a footer: it IS the
  // composer, centred, with nothing above it (D1). The class carries that
  // state so the stage can centre the card and drop the composer's footer
  // tone. The composer also takes focus in this state, guarded so a modal or
  // another field is never robbed.
  const emptyStage = !loading && threadItems.length === 0;

  return (
    <div className={"nex-main-conversation" + (emptyStage ? " is-empty" : "")}>
      <div
        className="nex-thread"
        ref={threadRef}
        onScroll={handleThreadScroll}
        role="log"
        aria-label={t("conv.messagesAria")}
      >
        {loading ? (
          <LoadingIndicator label={t("conv.loadingMessages")} />
        ) : (
          threadItems.map((item) => {
            if (item.kind === "run") {
              prevThreadRole = null;
              const runFinal = item.run.final_content?.trim() ?? "";
              const hasPersistedAssistant =
                runFinal.length > 0 &&
                messages.some(
                  (m) =>
                    m.role === "assistant" &&
                    m.created_at >= item.run.started_at &&
                    m.content.trim() === runFinal
                );
              return (
                <AgentRunSteps
                  key={`run-${item.run.run_id}`}
                  run={item.run}
                  supportsThinking={
                    selectedCapabilities?.supports_thinking ?? null
                  }
                  showFinalResult={!hasPersistedAssistant}
                  hasActiveRun={hasActiveRun}
                  renderApprovalInline={false}
                  onResolveApproval={dispatchAgentApprove}
                  onCancel={dispatchAgentCancel}
                  onKillSubagent={dispatchKillSubagent}
                  onContinue={dispatchAgentContinue}
                  onPause={dispatchAgentPause}
                  onResume={dispatchAgentResume}
                  onExpandStep={dispatchExpandStep}
                  onCollapseStep={dispatchCollapseStep}
                  onLoadRunSteps={dispatchLoadRunSteps}
                  onLoadCheckpoints={dispatchLoadCheckpoints}
                  onRevertCheckpoint={revertCheckpoint}
                  onRevertTurn={revertTurn}
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
                    ? ({
                        "--nex-chat-insert-index":
                          freshOrder.get(item.message.id) ?? 0,
                      } as CSSProperties)
                    : undefined
                }
              >
                <div className="nex-message-meta">
                  <span className="nex-message-author">
                    {item.message.role === "user"
                      ? t("conv.authorYou")
                      : t("conv.authorAssistant")}
                  </span>
                  {item.message.role === "assistant" &&
                    item.message.model_name && (
                      <span
                        className="nex-message-origin"
                        title={t("conv.originTitle")}
                      >
                        {item.message.model_name}
                      </span>
                    )}
                  <time
                    className="nex-message-time"
                    dateTime={new Date(
                      item.message.created_at * 1000
                    ).toISOString()}
                  >
                    {formatRelativeTime(item.message.created_at, locale)}
                  </time>
                </div>
                <div className="nex-message-body">
                  {item.message.role === "assistant" &&
                    thinkingByMessageId[item.message.id] && (
                      <ThinkingBlock
                        thinking={thinkingByMessageId[item.message.id]}
                        supportsThinking={
                          selectedCapabilities?.supports_thinking ?? null
                        }
                      />
                    )}
                  {/* ADD-01: only model-authored bodies are markdown-
                      interpreted (05-ADD §3.1); user text is data, not
                      formatting, and stays plain escaped text. */}
                  {(item.message.attachments?.length ?? 0) > 0 && (
                    <ul className="nex-message-attachments" aria-label={t("conv.attachedFiles")}>
                      {item.message.attachments?.map((attachment) => (
                        <li key={attachment.id} className="nex-chip">
                          <span className="nex-chip-name">{attachment.file_name}</span>
                          {attachment.availability === "missing" && <span className="nex-attachment-missing">{t("conv.attachmentMissing")}</span>}
                        </li>
                      ))}
                    </ul>
                  )}
                  {item.message.role === "assistant" ? (
                    <MessageMarkdown
                      messageId={item.message.id}
                      content={item.message.content}
                    />
                  ) : (
                    item.message.content
                  )}
                </div>
              </article>
            );
          })
        )}
        {sending && !agentMode && (streamingText || streamingThinking) && (
          <article
            className="nex-message nex-message-assistant nex-message-streaming"
            aria-live="polite"
          >
            <div className="nex-message-meta">
              <span className="nex-message-author">
                {t("conv.authorAssistant")}
              </span>
              {selectedModel && (
                <span className="nex-message-origin">{selectedModel}</span>
              )}
            </div>
            {streamingThinking && (
              <ThinkingBlock
                thinking={streamingThinking}
                streaming
                supportsThinking={
                  selectedCapabilities?.supports_thinking ?? null
                }
              />
            )}
            {streamingText && (
              <div className="nex-message-body">
                <MessageMarkdown messageId={-1} content={streamingText} />
              </div>
            )}
          </article>
        )}
        {sending && !agentMode && !streamingText && (
          <div className="nex-thread-waiting">
            <LoadingIndicator label={t("conv.responding")} size="sm" />
          </div>
        )}
        {activeRun && activeRun.streaming_text && (
          <article
            className="nex-message nex-message-assistant nex-message-streaming"
            aria-live="polite"
          >
            <div className="nex-message-body">
              <MessageMarkdown
                messageId={-activeRun.run_id}
                content={activeRun.streaming_text}
              />
            </div>
          </article>
        )}
        {agentBusy && (
          <div className="nex-thread-waiting">
            <LoadingIndicator label={t("conv.agentWorking")} size="sm" />
          </div>
        )}
      </div>

      {showScrollDown && (
        <button
          type="button"
          className="nex-scroll-to-bottom"
          aria-label={t("conv.scrollToBottom")}
          onClick={scrollToBottom}
        >
          <ArrowDownIcon size={16} />
        </button>
      )}

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

      {compactNotice && (
        <div className="nex-composer-strip-status nex-fade-in" role="status">
          {compactNotice}
        </div>
      )}

      {(pendingApprovalRun ||
        pendingQuestion ||
        pendingPlanRun ||
        queueItems.length > 0) && (
        <div className="nex-composer-adjacent">
          {pendingApprovalRun && (
            <ApprovalPanel
              run={pendingApprovalRun}
              onResolveApproval={dispatchAgentApprove}
            />
          )}
          {!pendingApprovalRun && pendingQuestion && (
            <QuestionPanel
              question={pendingQuestion}
              onSubmit={handleAnswerQuestion}
            />
          )}
          {!pendingApprovalRun && !pendingQuestion && pendingPlanRun && (
            <PlanDecisionPanel
              runId={pendingPlanRun.run_id}
              disabled={hasActiveRun || agentBusy}
              onDecide={handlePlanDecision}
            />
          )}
          {queueItems.length > 0 && (
            <WorkQueueStrip
              items={queueItems}
              paused={queuePaused}
              hasActiveRun={hasActiveRun}
              onMove={moveQueueItem}
              onEdit={async (id, content) => {
                await updateQueueItem(id, content);
              }}
              onCancel={cancelQueueItem}
              onRunNext={handleRunNextQueued}
            />
          )}
        </div>
      )}

      {undo && (
        <ConfirmDialog
          title={t("prompt.undoTitle")}
          body={t("prompt.undoBody", { count: undo.count })}
          confirmLabel={t("agent.checkpointRevertTurn")}
          danger
          busy={undoBusy}
          onConfirm={() => void confirmUndo()}
          onCancel={() => setUndo(null)}
        />
      )}
      <Composer
        draft={draft}
        promptContext={promptContext}
        onReview={handleReview}
        onUndo={() => void handleUndo()}
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
        planMode={agentMode ? planMode : false}
        onPlanModeChange={setPlanMode}
        autonomy={autonomy}
        onAutonomyChange={(mode) => void handleAutonomyChange(mode)}
        selectedModel={selectedModel}
        models={models}
        onSelectModel={onSelectModel}
        connectionGroups={connectionGroups}
        selectedConnectionId={selectedProvider}
        onSelectConnectionModel={onSelectConnectionModel}
        selectedCapabilities={selectedCapabilities}
        capabilitiesByKey={capabilitiesByKey}
        selectedEffort={selectedEffort}
        onSelectEffort={onSelectEffort}
        modelError={modelError}
        ready={ready}
        onOpenSettings={onOpenSettings}
        conversationId={conversationId}
        contextRevision={contextRevision}
        liveRunId={liveRunId}
        onStop={handleStop}
        onClarifyNow={() => void handleClarifyNow()}
        onQueuePrompt={() => void handleQueuePrompt()}
        liveActionBusy={liveActionBusy}
        workspace={workspace}
        autoFocusField={emptyStage}
        onCompactContext={() => void handleCompactContext()}
        compactBusy={compactBusy}
        onCompacted={handleCompacted}
      />
    </div>
  );
}
