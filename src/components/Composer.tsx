import type { ContextSelector, PromptContextView } from "../lib/tauri";
import type { ContextDraftToken, ComposerContextState } from "../lib/useComposerContext";
import ComposerPromptSheet from "./ComposerPromptSheet";
/* ============================================================
   COMPOSER — the conversation's single dominant control (#220)

   Rebuilt Cursor-style on the mono theme (#220, repainted #235):
   ONE rounded container (`.nex-composer-shell`) holding the field,
   its inline shortcut-teaching line, and a compact bottom control
   row — attach, the two primary modes, the mode/model picker,
   and the one light fill (send). Everything else recedes.

   Anatomy stolen from the owner-supplied reference: the single
   rounded container; a field whose placeholder says where typing
   begins; the read-only workspace line above it (D3); the round "+"
   on the left with the mode/model picker beside it; the context ring
   left of the model (D4); the one arrow/square action on the right
   (D2); hairline separators and no fills where a line will do. See the
   PR body for the two deliberate departures.

   TWO MODES, AND AGENT IS THE ONE THE PANE OPENS ON (R4-02).
   `agentMode` is controlled: the pane owns the value and the
   composer only dispatches `onAgentModeChange`. There is no
   second default here — one owner, one truth.

   MANAGED ATTACHMENTS IN BOTH MODES (Stage 8 / M15). The same draft
   chips and attach action are visible in Chat and Agent. Live Queue and
   Clarify remain text-only, so attachment-bearing live drafts stay visible
   and wait for the current run to finish.

   THE PLACEHOLDER IS THE AFFORDANCE. The textarea keeps its real
   `placeholder` attribute and it is what a user SEES: the field has no
   chrome of its own (the shell is the control), so the placeholder is
   the only thing that says where typing begins. Because it carries that
   whole job it is a sentence that says what to do
   (`conv.messagePlaceholder`), not a bare noun: "Message" read as a form
   label above the caret rather than as an invitation (visual QA, Phase C
   round 4). The accessible name stays the short noun `conv.messageAria`
   — a name names the control; repeating the instruction as the name
   makes AT announce the same sentence twice. The teaching line is a
   quiet row BELOW the editable line, in flow — it used to be an opaque
   `--card` overlay sitting on the placeholder's own line, which left an
   empty composer looking like a stack of chrome with no field at all
   (visual QA, Phase C round 2). Nothing is painted over the
   placeholder.

   THE SHORTCUT CHIPS ARE NOT INVENTED. `palette.toggle` and
   `help.show-shortcuts` are read out of src/lib/shortcuts.ts —
   the same registry the help dialog and the global key effects
   match — so a rebound key changes the chip with it. The "+"
   chip is a capability hint, not a key, and says so by being a
   literal glyph. `src/components/Composer.test.tsx` asserts both ids are
   registered.

   SOURCES OF TRUTH, unchanged from before this PR:
     - agent mode is local composer state (as it always was),
     - autonomy is the lifted `AutonomyStore` (NEX-FE-003),
     - the model is whatever App's provider store says — this
       component only DISPATCHES `onSelectModel`, it never keeps
       a copy, never optimistically rewrites the label, and
       re-renders from the prop when the write lands.
   ============================================================ */

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import { formatBytes } from "../lib/format";
import type {
  Attachment,
  AutonomyMode,
  CompactionResult,
  ConnectionModelCapabilities,
} from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";
import AccessProfileControl from "./AccessProfileControl";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import ContextRing from "./ContextRing";
import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import M3SegmentedGroup from "./M3SegmentedGroup";
import ModelPicker, { type ModelConnectionGroup } from "./ModelPicker";
import Tooltip from "./Tooltip";
import WorkspaceContextRow from "./WorkspaceContextRow";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import {
  ArrowUpIcon,
  ChevronDownIcon,
  CloseIcon,
  FolderIcon,
  PaperclipIcon,
  PlusIcon,
  SettingsIcon,
  StopIcon,
  TaskIcon,
} from "./icons";

export interface ComposerProps {
  /** Current draft (lifted so the Prompt Library can stage text). */
  draft: string;
  onDraftChange: (value: string) => void;
  /** Send the draft. The caller owns readiness and every guard. */
  onSubmit: () => void;
  /** A send is in flight. */
  sending: boolean;
  /** An agent run is starting. */
  agentBusy: boolean;
  /** Attach/remove work is in flight. */
  attachmentsBusy: boolean;
  /** Every send precondition is met (ready + non-empty + idle). */
  canSend: boolean;
  /** Draft attachments (backend order). */
  attachments: readonly Attachment[];
  onPickFiles: () => void;
  onRemoveAttachment: (id: number) => void;
  /** Agent mode is on: messages start an agent run. */
  agentMode: boolean;
  onAgentModeChange: (agentMode: boolean) => void;
  /** Agent-only Plan toggle: visible and enabled only when `agentMode` is true. */
  planMode?: boolean;
  onPlanModeChange?: (planMode: boolean) => void;
  /** Lifted autonomy store — the only source for the mode pills. */
  autonomy: AutonomyStore;
  onAutonomyChange: (mode: AutonomyMode) => void;
  /** Selected model, or null when none is configured. */
  selectedModel: string | null;
  /** Models offered by the selected provider (empty until a
   *  provider is configured). */
  models: readonly string[];
  /** Persist a model choice. The provider store owns the result. */
  onSelectModel: (model: string) => void;
  /** Optional connection-grouped models across configured connections. */
  connectionGroups?: readonly ModelConnectionGroup[];
  selectedConnectionId?: string | null;
  onSelectConnectionModel?: (connectionId: string, model: string) => void;
  selectedCapabilities?: ConnectionModelCapabilities | null;
  capabilitiesByKey?: ReadonlyMap<string, ConnectionModelCapabilities>;
  selectedEffort?: string | null;
  onSelectEffort?: (effort: string | null) => void;
  /** Last model-selection failure, surfaced in the composer's strip. */
  modelError?: string | null;
  /** Provider + model are configured, so the composer can send. */
  ready: boolean;
  onOpenSettings: () => void;
  /** Conversation the context ring reads (`conversationContextStats`). */
  conversationId: number;
  /** Monotonic tick from the pane's run events: the ring refreshes when a
   *  run advances, never on a timer. */
  contextRevision: string;
  /** Active run id, or null when no run is live. Drives the one button's
   *  arrow → square swap (D2). */
  liveRunId: number | null;
  /** Stop the live run. The same button becomes Square while it is set. */
  onStop: () => void;
  /** Send a live `Clarify now` steering message while a run is active. */
  onClarifyNow?: () => void;
  /** Enqueue the draft onto the persistent FIFO queue while a run is active. */
  onQueuePrompt?: () => void;
  /** True while a live clarification or queue enqueue call is in flight. */
  liveActionBusy?: boolean;
  /** Workspace store for the control row above the card (D3 / R6-03). */
  workspace: WorkspaceStore;
  /** Focus the field when an empty conversation opens (D1). The effect
   *  never steals focus from a modal or from another field. */
  autoFocusField?: boolean;
  /** Trigger manual `/compact` context compaction (`M16`). */
  onCompactContext?: () => void;
  /** True while manual `/compact` is in flight. */
  compactBusy?: boolean;
  /** Notified when `ContextRing` completes a manual compaction. */
  onCompacted?: (result: CompactionResult) => void;
  promptContext?: ComposerContextState;
  onReview?: (view: PromptContextView) => Promise<boolean>;
  onUndo?: () => void;
}

const CONTEXT_SELECTORS = ["file", "folder", "diff", "symbol"] as const;

function mentionAt(draft: string, caret: number) {
  const match = /(?:^|\s)(\/?@)(\S*)$/.exec(draft.slice(0, caret));
  if (!match) return null;
  const tail = /^\S*/.exec(draft.slice(caret))?.[0] ?? "";
  return {
    query: match[2].toLowerCase(),
    slash: match[1] === "/@",
    token: { draft, start: caret - match[1].length - match[2].length, end: caret + tail.length },
  };
}

export default function Composer({
  draft,
  onDraftChange,
  onSubmit,
  sending,
  agentBusy,
  attachmentsBusy,
  canSend,
  attachments,
  onPickFiles,
  onRemoveAttachment,
  agentMode,
  onAgentModeChange,
  planMode,
  onPlanModeChange,
  autonomy,
  onAutonomyChange,
  selectedModel,
  models,
  onSelectModel,
  connectionGroups,
  selectedConnectionId = null,
  onSelectConnectionModel,
  selectedCapabilities = null,
  capabilitiesByKey,
  selectedEffort = null,
  onSelectEffort,
  modelError = null,
  ready,
  onOpenSettings,
  conversationId,
  contextRevision,
  liveRunId,
  onStop,
  onClarifyNow,
  onQueuePrompt,
  liveActionBusy = false,
  workspace,
  autoFocusField = false,
  onCompactContext,
  compactBusy = false,
  onCompacted,
  promptContext,
  onReview,
  onUndo,
}: ComposerProps) {
  const { t } = useStrings();
  const busy = sending || agentBusy;
  const live = liveRunId !== null;
  const fieldRef = useRef<HTMLTextAreaElement>(null);
  const autoFocusedRef = useRef(false);
  const [localPlanMode, setLocalPlanMode] = useState(false);
  const effectivePlan = agentMode && (planMode ?? localPlanMode);

  const [plusOpen, setPlusOpen] = useState(false);
  const [slashOpen, setSlashOpen] = useState(false);
  const [mentionOpen, setMentionOpen] = useState(false);
  const [slashHighlight, setSlashHighlight] = useState<number | null>(null);
  const slashListId = useId();
  const [mentionHighlight, setMentionHighlight] = useState(0);
  const [caret, setCaret] = useState<number | null>(null);
  const includedCaret = useRef<number | null>(null);
  const returnFocusToFieldRef = useRef(false);

  const togglePlanMode = useCallback(() => {
    if (!agentMode) return;
    const next = !effectivePlan;
    if (onPlanModeChange) onPlanModeChange(next);
    else setLocalPlanMode(next);
  }, [agentMode, effectivePlan, onPlanModeChange]);

  const setPlanEnabled = useCallback(
    (next: boolean) => {
      if (onPlanModeChange) onPlanModeChange(next);
      else setLocalPlanMode(next);
    },
    [onPlanModeChange]
  );

  // Active slash/mention query extracted from the current draft.
  const slashCommand = /^\/([^\s]*)(?:[ \t]+[\s\S]*)?$/.exec(draft)?.[1];
  const slashHasArguments = /^\/\S+[ \t]+/.test(draft);
  const contextQuery = useMemo(
    () => mentionAt(draft, Math.min(caret ?? draft.length, draft.length)),
    [caret, draft]
  );
  const slashContextQuery = contextQuery?.slash ? contextQuery.query : null;
  const slashQuery = slashContextQuery === null ? slashCommand?.toLowerCase() ?? "" : `@${slashContextQuery}`;
  const mentionQuery = contextQuery && !contextQuery.slash ? contextQuery.query : "";
  const contextToken = contextQuery?.token;
  const mentionToken = contextQuery?.slash ? undefined : contextToken;

  const setPromptSheet = promptContext?.setSheet;
  const openContext = useCallback((selector: ContextSelector["kind"], token?: ContextDraftToken) => {
    fieldRef.current?.focus();
    setPromptSheet?.({ kind: "context", selector, token });
  }, [setPromptSheet]);

  const slashItems = useMemo(() => {
    const items = [
      {
        id: "agent",
        command: "/agent",
        description: t("conv.slashAgentDesc"),
        disabled: agentMode,
        run: () => onAgentModeChange(true),
      },
      {
        id: "chat",
        command: "/chat",
        description: t("conv.slashChatDesc"),
        disabled: !agentMode,
        run: () => onAgentModeChange(false),
      },
      {
        id: "plan",
        command: "/plan",
        description: t("conv.slashPlanDesc"),
        disabled: false,
        run: () => {
          if (!agentMode) {
            onAgentModeChange(true);
            setPlanEnabled(true);
          } else {
            togglePlanMode();
          }
        },
      },
      {
        id: "compact",
        command: "/compact",
        description: t("conv.slashCompactDesc"),
        disabled: compactBusy || !onCompactContext,
        run: () => onCompactContext?.(),
      },
      { id: "attach", command: "/attach", description: t("conv.slashAttachDesc"),
        disabled: busy || attachmentsBusy || live,
        run: () => onPickFiles(),
      },
      {
        id: "clear",
        command: "/clear",
        description: t("conv.slashClearDesc"),
        disabled: draft.length === 0 && !promptContext?.active,
        run: () => {
          promptContext?.clear();
          onDraftChange("");
        },
      },
      {
        id: "settings",
        command: "/settings",
        description: t("conv.slashSettingsDesc"),
        disabled: false,
        run: () => onOpenSettings(),
      },
    ];
    if (promptContext && onReview) {
      items.push({
        id: "review",
        command: "/review",
        description: t("prompt.reviewAction"),
        disabled: !ready || busy || live,
        run: () => promptContext.setSheet({ kind: "review" }),
      });
    }
    if (onUndo) {
      items.push({
        id: "undo",
        command: "/undo",
        description: t("prompt.undoAction"),
        disabled: busy || live,
        run: onUndo,
      });
    }
    if (promptContext) {
      for (const selector of CONTEXT_SELECTORS) {
        items.push({
          id: `context:${selector}`,
          command: `/@${selector}`,
          description: t(`prompt.kind.${selector}`),
          disabled: busy || live,
          run: () => openContext(selector, slashContextQuery !== null ? contextToken :
            slashCommand !== undefined ? { draft, start: 0, end: slashCommand.length + 1 } : undefined),
        });
      }
      items.push({
        id: "skill-catalog",
        command: "/skills",
        description: t("prompt.manageSkills"),
        disabled: busy || live,
        run: () => {
          void promptContext.refresh();
          promptContext.setSheet({ kind: "skills" });
        },
      });
      for (const skill of promptContext.skills) {
        items.push({
          id: `skill:${skill.reference.source_id}`,
          command: `/${skill.name}`,
          description: `${skill.description} · ${t(skill.scope === "personal" ? "prompt.personal" : "prompt.project")} / ${skill.path}`,
          disabled: busy || live,
          run: () => promptContext.setSheet({ kind: "skill", skill }),
        });
      }
    }
    if (slashContextQuery !== null) {
      return items.filter((item) => item.id.startsWith("context:") && item.command.startsWith(`/@${slashContextQuery}`));
    }
    if (slashHasArguments) {
      return items.filter((item) => (item.id.startsWith("skill:") || item.id.startsWith("context:")) &&
        item.command === `/${slashQuery}`);
    }
    if (!slashQuery) return items;
    return items.filter(
      (item) =>
        item.command.slice(1).includes(slashQuery) ||
        item.description.toLowerCase().includes(slashQuery)
    ).sort((a, b) => Number(b.command === `/${slashQuery}`) - Number(a.command === `/${slashQuery}`));
  }, [
    agentMode,
    attachmentsBusy,
    busy,
    live,
    contextToken,
    draft,
    onAgentModeChange,
    onCompactContext,
    onDraftChange,
    onOpenSettings,
    onPickFiles,
    onReview,
    onUndo,
    openContext,
    promptContext,
    ready,
    compactBusy,
    setPlanEnabled,
    slashQuery,
    slashHasArguments,
    slashContextQuery,
    slashCommand,
    t,
    togglePlanMode,
  ]);

  const activeSlashIndex = (slashHighlight ?? 0) % Math.max(1, slashItems.length);
  const keepSlashOptionVisible = useCallback((option: HTMLButtonElement | null) => {
    const list = option?.closest("ul");
    if (!option || !list) return;
    const row = option.getBoundingClientRect();
    const viewport = list.getBoundingClientRect();
    if (row.top < viewport.top) list.scrollTop += row.top - viewport.top;
    else if (row.bottom > viewport.bottom) list.scrollTop += row.bottom - viewport.bottom;
  }, []);

  const mentionItems = useMemo(() => {
    const baseName = (fullPath: string): string => {
      const normalized = fullPath.replace(/\\/g, "/").replace(/\/+$/, "");
      const idx = normalized.lastIndexOf("/");
      return idx >= 0 ? normalized.slice(idx + 1) || normalized : normalized;
    };
    const items: Array<{
      id: string;
      label: string;
      detail: string;
      kind: "workspace" | "attachment" | "action";
      token?: string;
      run?: () => void;
    }> = [];
    if (promptContext && !live) {
      for (const selector of CONTEXT_SELECTORS) {
        items.push({
          id: `context:${selector}`,
          label: `@${selector}`,
          detail: t(`prompt.kind.${selector}`),
          kind: "workspace",
          run: () => openContext(selector, mentionToken),
        });
      }
    }
    if (!promptContext && workspace.root) {
      const name = baseName(workspace.root);
      items.push({
        id: `ws:${workspace.root}`,
        label: `@${name}`,
        detail: `${t("conv.mentionWorkspace")} · ${workspace.root}`,
        kind: "workspace",
        token: `@${name}`,
      });
    }
    for (const recent of promptContext ? [] : workspace.recent) {
      if (recent === workspace.root) continue;
      const name = baseName(recent);
      items.push({
        id: `recent:${recent}`,
        label: `@${name}`,
        detail: `${t("conv.mentionRecentFolder")} · ${recent}`,
        kind: "workspace",
        token: `@${name}`,
      });
    }
    if (!agentMode) {
      for (const att of attachments) {
        items.push({
          id: `att:${att.id}`,
          label: `@${att.file_name}`,
          detail: t("conv.mentionAttachment"),
          kind: "attachment",
          token: `@${att.file_name}`,
        });
      }
    }
    items.push({
      id: "pick-folder",
      label: t("conv.mentionChooseFolder"),
      detail: workspace.root ?? t("workspace.none"),
      kind: "action",
      run: () => {
        void workspace.pickFolder();
      },
    });
    if (!mentionQuery) return items;
    return items.filter(
      (item) =>
        item.label.toLowerCase().includes(mentionQuery) ||
        item.detail.toLowerCase().includes(mentionQuery)
    );
  }, [agentMode, attachments, live, mentionQuery, mentionToken, openContext, promptContext, t, workspace]);

  const runSlashItem = useCallback(
    (item: (typeof slashItems)[number]) => {
      if (item.disabled) return;
      if (item.id !== "clear" && item.id !== "review" && item.id !== "undo"
          && !item.id.startsWith("skill:") && !item.id.startsWith("context:") && /^\/[^\s]*$/.test(draft)) {
        onDraftChange("");
      }
      item.run();
      setSlashOpen(false);
      fieldRef.current?.focus();
    },
    [draft, onDraftChange]
  );

  const runMentionItem = useCallback(
    (item: (typeof mentionItems)[number]) => {
      if (item.run) {
        item.run();
        setMentionOpen(false);
        return;
      }
      if (!item.token) return;
      const replacement = `${item.token} `;
      let next: string;
      if (mentionToken) {
        const tail = draft.slice(mentionToken.end);
        next = draft.slice(0, mentionToken.start) + (tail ? item.token : replacement) + tail;
      } else if (draft.length === 0 || /\s$/.test(draft)) {
        next = draft + replacement;
      } else {
        next = `${draft} ${replacement}`;
      }
      onDraftChange(next);
      setMentionOpen(false);
      fieldRef.current?.focus();
    },
    [draft, mentionToken, onDraftChange]
  );

  const handleDraftInput = useCallback(
    (value: string, position: number) => {
      onDraftChange(value);
      setCaret(position);
      const query = mentionAt(value, position);
      const skillCommand = /^\/([^\s]+)[ \t]+/.exec(value)?.[1].toLowerCase();
      if (query?.slash || /^\/[^\s]*$/.test(value) || promptContext?.skills.some((item) => item.name === skillCommand)) {
        if (!slashOpen && promptContext) void promptContext.refresh();
        setPlusOpen(false);
        setSlashOpen(true);
        setSlashHighlight(null);
        setMentionOpen(false);
      } else {
        if (slashOpen) setSlashOpen(false);
        if (query) {
          setPlusOpen(false);
          setMentionOpen(true);
          setMentionHighlight(0);
        } else if (mentionOpen) {
          setMentionOpen(false);
        }
      }
    },
    [mentionOpen, onDraftChange, promptContext, slashOpen]
  );

  // D1: an empty conversation is the composer, so the caret belongs in the
  // field. The guard is the whole contract: only a body/no-focus state (or
  // this field already) may be claimed, and a modal on top always wins.
  useEffect(() => {
    if (!autoFocusField || autoFocusedRef.current) return;
    if (document.querySelector('[role="dialog"][aria-modal="true"]') !== null)
      return;
    const active = document.activeElement;
    if (
      active !== null &&
      active !== document.body &&
      active !== fieldRef.current
    )
      return;
    fieldRef.current?.focus();
    autoFocusedRef.current = true;
  }, [autoFocusField]);

  const sheet = promptContext?.sheet;
  useEffect(() => {
    if (sheet || includedCaret.current === null) return;
    fieldRef.current?.setSelectionRange(includedCaret.current, includedCaret.current);
    includedCaret.current = null;
  }, [draft, sheet]);

  const runExactSlash = () => {
    if (slashQuery) {
      const matching = slashItems.filter((item) => item.command === `/${slashQuery}`);
      const builtIn = matching.find((item) => !item.id.startsWith("skill:"));
      if (builtIn && !builtIn.disabled) {
        runSlashItem(builtIn);
        return true;
      }
      const matchingSkills = matching.filter((item) => item.id.startsWith("skill:") && !item.disabled);
      if (matchingSkills.length === 1) {
        runSlashItem(matchingSkills[0]);
        return true;
      }
      if (matchingSkills.length > 1) {
        setSlashOpen(true);
        fieldRef.current?.focus();
        return true;
      }
      if (slashCommand && promptContext?.skillsLoading) {
        setSlashOpen(true);
        return true;
      }
    }
    return false;
  };

  const submitDraft = () => {
    if (mentionOpen && contextQuery && !contextQuery.slash) {
      const exact = mentionItems.find((item) => item.id.startsWith("context:") && item.label === `@${mentionQuery}`);
      if (exact) {
        runMentionItem(exact);
        return;
      }
    }
    if (!runExactSlash()) onSubmit();
  };
  const fieldFocus = () => fieldRef.current?.focus();
  const finishContext = () => {
    const token = sheet?.kind === "context" ? sheet.token : undefined;
    if (!token || token.draft !== draft) return;
    includedCaret.current = token.start;
    setCaret(token.start);
    onDraftChange(draft.slice(0, token.start) + draft.slice(token.end));
  };

  return (
    <div className="nex-composer">
      {promptContext && sheet && (
        <ComposerPromptSheet
          key={sheet.kind === "skill" ? sheet.skill.reference.revision : sheet.kind}
          conversationId={conversationId}
          state={promptContext}
          sheet={sheet}
          showNoShell={agentMode && promptContext.active}
          onReview={onReview}
          onUseSkill={(name) => {
            const invocation = /^\/([^\s]+)(?:[ \t]+([\s\S]*))?$/.exec(draft);
            if (invocation?.[1].toLowerCase() === name) onDraftChange(invocation[2] ?? "");
            fieldFocus();
          }}
          onIncludeContext={finishContext}
        />
      )}
      <div className="nex-composer-inner">
        <WorkspaceContextRow workspace={workspace} />
        <div className="nex-composer-shell">
          {attachments.length > 0 && (
            <ul
              className="nex-composer-attachments"
              aria-label={t("conv.attachedFiles")}
            >
              {attachments.map((attachment) => (
                <li key={attachment.id} className="nex-chip">
                  <PaperclipIcon className="nex-chip-icon" />
                  <span className="nex-attachment-text">
                    <span
                      className="nex-chip-name"
                      title={attachment.file_name}
                    >
                      {attachment.file_name}
                    </span>
                    {attachment.availability === "missing" && <span className="nex-attachment-missing">{t("conv.attachmentMissing")}</span>}
                    {formatBytes(attachment.file_size_bytes) !== null && (
                      <span className="nex-chip-size">
                        {formatBytes(attachment.file_size_bytes)}
                      </span>
                    )}
                  </span>
                  <M3IconButton
                    label={t("conv.removeFile", { name: attachment.file_name })}
                    size="sm"
                    className="nex-chip-remove"
                    disabled={attachmentsBusy || sending}
                    onClick={() => void onRemoveAttachment(attachment.id)}
                  >
                    <CloseIcon />
                  </M3IconButton>
                </li>
              ))}
            </ul>
          )}

          {promptContext?.active && (
            <div className="nex-composer-context">
              <ul className="nex-composer-attachments" aria-label={t("prompt.included")}>
                {promptContext.selected.map((skill) => (
                  <li className="nex-chip" key={skill.reference.source_id}>
                    <button type="button" className="nex-prompt-chip-name" disabled={busy}
                      onClick={() => promptContext.setSheet({ kind: "skill", skill })}>
                      <code>/{skill.name}</code>
                    </button>
                    <M3IconButton size="sm" className="nex-chip-remove" disabled={busy}
                      label={t("prompt.remove", { name: skill.name })}
                      onClick={() => promptContext.removeSkill(skill)}><CloseIcon /></M3IconButton>
                  </li>
                ))}
                {promptContext.contexts.map((context) => (
                  <li className="nex-chip" key={context.reference.revision}>
                    <span className="nex-chip-name" title={context.label}>
                      <code>@{context.reference.selector.kind} {context.label}</code>
                    </span>
                    <M3IconButton size="sm" className="nex-chip-remove" disabled={busy}
                      label={t("prompt.remove", { name: context.label })}
                      onClick={() => promptContext.removeContext(context)}><CloseIcon /></M3IconButton>
                  </li>
                ))}
              </ul>
              {agentMode && !sheet && <p className="nex-prompt-inline-note">{t("prompt.noShell")}</p>}
            </div>
          )}

          <div className="nex-composer-field">
            <textarea
              ref={fieldRef}
              className="nex-composer-input"
              rows={1}
              placeholder={
                live ? t("composer.livePlaceholder") : t("conv.messagePlaceholder")
              }
              aria-label={t("conv.messageAria")}
              value={draft}
              aria-controls={slashOpen && slashItems.length > 0 ? slashListId : undefined}
              aria-activedescendant={slashOpen && slashItems.length > 0 ? `${slashListId}-${activeSlashIndex}` : undefined}
              disabled={busy}
              onChange={(event) => handleDraftInput(event.target.value, event.target.selectionStart)}
              onSelect={(event) => {
                const field = event.currentTarget;
                setCaret(field.selectionStart);
                const query = field.selectionStart === field.selectionEnd
                  ? mentionAt(draft, field.selectionStart) : null;
                if (mentionOpen && mentionToken && (!query || query.slash)) setMentionOpen(false);
                if (slashOpen && slashContextQuery !== null && !query?.slash) setSlashOpen(false);
              }}
              onKeyDown={(event) => {
                if (slashOpen) {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    setSlashOpen(false);
                    return;
                  }
                  if (event.key === "ArrowDown" && slashItems.length > 0) {
                    event.preventDefault();
                    setSlashHighlight((i) => ((i ?? 0) + 1) % slashItems.length);
                    return;
                  }
                  if (event.key === "ArrowUp" && slashItems.length > 0) {
                    event.preventDefault();
                    setSlashHighlight((i) => ((i ?? 0) - 1 + slashItems.length) % slashItems.length);
                    return;
                  }
                  if (event.key === "Enter" && !event.shiftKey) {
                    if (slashHighlight === null && runExactSlash()) {
                      event.preventDefault();
                      return;
                    }
                    const target = slashItems[activeSlashIndex];
                    if (target && !target.disabled) {
                      event.preventDefault();
                      runSlashItem(target);
                      return;
                    }
                  }
                }
                if (mentionOpen) {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    setMentionOpen(false);
                    return;
                  }
                  if (event.key === "ArrowDown" && mentionItems.length > 0) {
                    event.preventDefault();
                    setMentionHighlight((i) => (i + 1) % mentionItems.length);
                    return;
                  }
                  if (event.key === "ArrowUp" && mentionItems.length > 0) {
                    event.preventDefault();
                    setMentionHighlight(
                      (i) => (i - 1 + mentionItems.length) % mentionItems.length
                    );
                    return;
                  }
                  if (event.key === "Enter" && !event.shiftKey) {
                    const target = mentionItems[mentionHighlight];
                    if (target) {
                      event.preventDefault();
                      runMentionItem(target);
                      return;
                    }
                  }
                }
                // shortcut:composer.send (Shift+Enter = shortcut:composer.newline).
                // D2: Enter never sends while a run is live — the one button is
                // a stop in that state, and typing a second message mid-run is
                // not a send.
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  if (!live) submitDraft();
                }
              }}
            />
          </div>

          {effectivePlan && (
            <div className="nex-composer-plan-banner" role="status">
              <TaskIcon />
              <span>{t("conv.planActiveHint")}</span>
            </div>
          )}

          <div className="nex-composer-bar">
            {/* "+" consolidated action menu (available in both Chat and Agent
                modes). Attach files is Chat-only (`!agentMode`, R4-02 / G-20);
                typing `/` or `@` in the field also opens the anchored menus
                below. */}
            <div className="nex-composer-plus-anchor">
              <Popover
                open={plusOpen}
                onOpenChange={(next) => {
                  setPlusOpen(next);
                  if (next) {
                    setSlashOpen(false);
                    setMentionOpen(false);
                  }
                }}
              >
                <PopoverTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-lg"
                    className="nex-composer-attach rounded-full"
                    aria-label={t("conv.plusMenuAria")}
                    title={t("conv.plusMenuTitle")}
                    disabled={busy}
                  >
                    <PlusIcon />
                  </Button>
                </PopoverTrigger>
                <PopoverContent
                  align="start"
                  side="top"
                  sideOffset={8}
                  className="nex-composer-plus-menu"
                  onCloseAutoFocus={(event) => {
                    if (returnFocusToFieldRef.current) {
                      event.preventDefault();
                      returnFocusToFieldRef.current = false;
                      fieldRef.current?.focus();
                    }
                  }}
                >
                  <div
                    role="menu"
                    aria-label={t("conv.plusMenuTitle")}
                    className="nex-composer-plus-list"
                  >
                    <button
                        type="button"
                        role="menuitem"
                        className="nex-composer-trigger-option"
                        disabled={busy || attachmentsBusy || live}
                        onClick={() => {
                          setPlusOpen(false);
                          onPickFiles();
                        }}
                      >
                        <span className="nex-composer-trigger-icon">
                          <PaperclipIcon />
                        </span>
                        <span className="nex-composer-plus-label">
                          {t("conv.plusMenuAttach")}
                        </span>
                      </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="nex-composer-trigger-option"
                      disabled={busy}
                      onClick={() => {
                        returnFocusToFieldRef.current = true;
                        setPlusOpen(false);
                        setMentionOpen(false);
                        setSlashHighlight(null);
                        setSlashOpen(true);
                      }}
                    >
                      <code className="nex-composer-trigger-cmd">/</code>
                      <span className="nex-composer-plus-label">
                        {t("conv.plusMenuCommands")}
                      </span>
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      className="nex-composer-trigger-option"
                      disabled={busy}
                      onClick={() => {
                        returnFocusToFieldRef.current = true;
                        setPlusOpen(false);
                        setSlashOpen(false);
                        setMentionHighlight(0);
                        setMentionOpen(true);
                      }}
                    >
                      <code className="nex-composer-trigger-cmd">@</code>
                      <span className="nex-composer-plus-label">
                        {t("conv.plusMenuMentions")}
                      </span>
                    </button>
                  </div>
                </PopoverContent>
              </Popover>

              <Popover
                open={slashOpen}
                onOpenChange={(next) => {
                  setSlashOpen(next);
                  if (next) {
                    setPlusOpen(false);
                    setMentionOpen(false);
                    setSlashHighlight(null);
                  }
                }}
              >
                <PopoverAnchor className="nex-composer-popover-anchor" />
                <PopoverContent
                  align="start"
                  side="top"
                  sideOffset={8}
                  className="nex-composer-trigger-menu"
                  onOpenAutoFocus={(event) => event.preventDefault()}
                >
                  <PopoverHeader>
                    <PopoverTitle>{t("conv.slashMenuTitle")}</PopoverTitle>
                  </PopoverHeader>
                  {slashItems.length === 0 && promptContext?.skillsLoading ? (
                    <p role="status" className="nex-composer-trigger-empty">{t("common.loading")}</p>
                  ) : slashItems.length === 0 ? (
                    <p className="nex-composer-trigger-empty">
                      {t("conv.slashEmpty")}
                    </p>
                  ) : (
                    <ul
                      id={slashListId}
                      role="listbox"
                      aria-label={t("conv.slashMenuAria")}
                      className="nex-composer-trigger-list"
                    >
                      {slashItems.map((item, idx) => (
                        <li key={item.id}>
                          <button
                            id={`${slashListId}-${idx}`}
                            ref={idx === activeSlashIndex ? keepSlashOptionVisible : undefined}
                            type="button"
                            role="option"
                            aria-selected={idx === activeSlashIndex}
                            disabled={item.disabled}
                            className={
                              "nex-composer-trigger-option" +
                              (idx === activeSlashIndex ? " is-highlighted" : "")
                            }
                            onClick={() => runSlashItem(item)}
                          >
                            <code className="nex-composer-trigger-cmd">
                              {item.command}
                            </code>
                            <span className="nex-composer-trigger-desc">
                              {item.description}
                            </span>
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                </PopoverContent>
              </Popover>

              <Popover
                open={mentionOpen}
                onOpenChange={(next) => {
                  setMentionOpen(next);
                  if (next) {
                    setPlusOpen(false);
                    setSlashOpen(false);
                    setMentionHighlight(0);
                  }
                }}
              >
                <PopoverAnchor className="nex-composer-popover-anchor" />
                <PopoverContent
                  align="start"
                  side="top"
                  sideOffset={8}
                  className="nex-composer-trigger-menu"
                  onOpenAutoFocus={(event) => event.preventDefault()}
                >
                  <PopoverHeader>
                    <PopoverTitle>{t("conv.mentionMenuTitle")}</PopoverTitle>
                  </PopoverHeader>
                  {mentionItems.length === 0 ? (
                    <p className="nex-composer-trigger-empty">
                      {t("conv.mentionEmpty")}
                    </p>
                  ) : (
                    <ul
                      role="listbox"
                      aria-label={t("conv.mentionMenuAria")}
                      className="nex-composer-trigger-list"
                    >
                      {mentionItems.map((item, idx) => (
                        <li key={item.id}>
                          <button
                            type="button"
                            role="option"
                            aria-selected={idx === mentionHighlight}
                            className={
                              "nex-composer-trigger-option" +
                              (idx === mentionHighlight ? " is-highlighted" : "")
                            }
                            onClick={() => runMentionItem(item)}
                          >
                            <span className="nex-composer-trigger-icon">
                              {item.kind === "attachment" ? (
                                <PaperclipIcon />
                              ) : item.kind === "action" ? (
                                <SettingsIcon />
                              ) : (
                                <FolderIcon />
                              )}
                            </span>
                            <span className="nex-composer-trigger-cmd">
                              {item.label}
                            </span>
                            <span className="nex-composer-trigger-desc">
                              {item.detail}
                            </span>
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                </PopoverContent>
              </Popover>
            </div>

            {/* The two primary modes, as pills: same segmented
                primitive and the same radio semantics as every
                other choice group in the app — Chat and Agent are
                a preference pair, and the old checkbox (a role
                nobody could arrow through) is gone. */}
            <span title={t("conv.agentToggleTitle")}>
              <M3SegmentedGroup
                className="nex-composer-pills"
                label={t("conv.agentModeAria")}
                semantics="radio"
                value={agentMode ? "agent" : "chat"}
                onChange={(value) => onAgentModeChange(value === "agent")}
                options={[
                  {
                    value: "chat",
                    label: t("conv.modeChat"),
                    ariaLabel: t("conv.modeChatAria"),
                    leading: (
                      <span
                        className="nex-composer-pill-dot"
                        aria-hidden="true"
                      />
                    ),
                  },
                  {
                    value: "agent",
                    label: t("conv.agentToggle"),
                    leading: (
                      <span
                        className="nex-composer-pill-dot"
                        aria-hidden="true"
                      />
                    ),
                  },
                ]}
              />
            </span>

            {agentMode && (
              <Tooltip label={t("conv.planToggleTitle")}>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className={
                    "nex-composer-plan-toggle" +
                    (effectivePlan ? " is-active" : "")
                  }
                  aria-pressed={effectivePlan}
                  aria-label={t("conv.planToggleAria")}
                  title={t("conv.planToggleTitle")}
                  data-state={effectivePlan ? "on" : "off"}
                  disabled={busy}
                  onClick={togglePlanMode}
                >
                  <TaskIcon />
                  <span>{t("conv.planLabel")}</span>
                </Button>
              </Tooltip>
            )}

            <span className="nex-composer-bar-spacer" />

            {/* The trailing group is ONE flex item, not four: in a split
                pane the row wraps, and loose items split across the break
                (the model name on line one, Send alone on line two).
                Wrapped together, the group moves as a unit and keeps its
                internal rhythm. */}
            <span className="nex-composer-bar-trailing">
              {/* Autonomy is a MODE, not a row of pills under the field (D-03
                rejects the always-open autonomy row; O-12 puts it in
                "Settings + compact run meta"). The trigger names the live
                mode in one word and the three choices plus their explainer
                live behind it, so the composer keeps one control row and the
                sentence that explains a mode is only on screen while the
                user is choosing one. */}
              {agentMode && (
                <Popover>
                  <PopoverTrigger asChild>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="nex-composer-autonomy-trigger"
                      aria-label={t("access.label")}
                      title={t("access.label")}
                    >
                      <span
                        className="nex-composer-autonomy-dot"
                        aria-hidden="true"
                      />
                      <span className="nex-composer-autonomy-name">
                        {t(`access.${autonomy.access.profile}`)}
                      </span>
                      <ChevronDownIcon />
                    </Button>
                  </PopoverTrigger>
                  <PopoverContent
                    align="end"
                    side="top"
                    sideOffset={8}
                    className="nex-composer-autonomy-menu"
                  >
                    <PopoverHeader>
                      <PopoverTitle>{t("access.label")}</PopoverTitle>
                    </PopoverHeader>
                    <AccessProfileControl
                      store={autonomy.access}
                      showLabel={false}
                    />
                    <p className="nex-settings-label">
                      {t("access.legacyAutonomy")}
                    </p>
                    <M3SegmentedGroup
                      className="nex-composer-autonomy"
                      label={t("conv.autonomyAria")}
                      semantics="radio"
                      indicator={false}
                      value={autonomy.mode}
                      onChange={(mode) => onAutonomyChange(mode)}
                      options={[
                        {
                          value: "supervised",
                          label: t("conv.autonomySupervised"),
                        },
                        {
                          value: "semi_autonomous",
                          label: t("conv.autonomySemi"),
                          ariaLabel: t("conv.autonomySemiAria"),
                        },
                        {
                          value: "full_autonomous",
                          label: t("conv.autonomyFull"),
                        },
                      ]}
                    />
                    <ComposerAutonomyHint
                      mode={autonomy.mode}
                      text={
                        autonomy.mode === "supervised"
                          ? t("conv.autonomyExplainSupervised")
                          : autonomy.mode === "semi_autonomous"
                            ? t("conv.autonomyExplainSemi")
                            : t("conv.autonomyExplainFull")
                      }
                    />
                  </PopoverContent>
                </Popover>
              )}

              {/* The context ring sits left of the model trigger — the
                reference order is ring → model → Send. It is the composer's
                only reading of the context window, and it reads `n/a` rather
                than inventing 0%. Inside the trailing group, so the bar still
                wraps ring+model+Send as one unit in a split pane. */}
              <ContextRing
                conversationId={conversationId}
                revision={contextRevision}
                selectedConnectionId={selectedConnectionId}
                selectedModel={selectedModel}
                onCompacted={onCompacted}
              />

              {/* Model picker (R6-02). A real searchable listbox now: the
                segmented primitive's inline-grid root resolved its implicit
                column to max-content and grew wider than the popover, which
                is where the UA's horizontal bar came from. The picker owns
                its own search + keyboard contract; this component only
                dispatches the choice. */}
              <ModelPicker
                selectedModel={selectedModel}
                models={models}
                onSelectModel={onSelectModel}
                connectionGroups={connectionGroups}
                selectedConnectionId={selectedConnectionId}
                onSelectConnectionModel={onSelectConnectionModel}
                capabilities={selectedCapabilities}
                capabilitiesByKey={capabilitiesByKey}
                selectedEffort={selectedEffort}
                onSelectEffort={onSelectEffort}
                modelError={modelError}
                onOpenSettings={onOpenSettings}
              />

              {live && agentMode && (onClarifyNow || onQueuePrompt) && (
                <span className="nex-composer-live-actions">
                  {onClarifyNow && (
                    <Button
                      type="button"
                      size="xs"
                      variant="outline"
                      disabled={liveActionBusy || attachments.length > 0 || Boolean(promptContext?.active) || draft.trim() === ""}
                      onClick={onClarifyNow}
                    >
                      {t("composer.clarifyNow")}
                    </Button>
                  )}
                  {onQueuePrompt && (
                    <Button
                      type="button"
                      size="xs"
                      variant="ghost"
                      disabled={!ready || liveActionBusy || attachments.length > 0 || Boolean(promptContext?.active) || draft.trim() === ""}
                      onClick={onQueuePrompt}
                    >
                      {t("composer.queuePrompt")}
                    </Button>
                  )}
                </span>
              )}

              {/* Send. ONE button, ONE geometry, three states (R6-01):
                  idle   muted plate + arrow (no draft, no run)
                  armed  light plate + arrow
                  stop   light plate + FILLED square, click stops the run
                The plate is always painted: an arrow floating on the card
                was the Round 5 finding. `disabled:border-transparent` and
                the disabled fill utilities are the only state tokens the
                composer adds: `ui/button`'s default variant ships the
                theme's disabled recipe as a full hairline and no fill, and
                a hairline is exactly what makes an inert Send read as an
                enabled secondary button. `cn` runs tailwind-merge, so the
                classes below replace the variant's disabled utilities
                instead of stacking with them —
                src/components/Composer.test.tsx asserts the merge. */}
              <Button
                size="icon"
                variant="default"
                className={
                  "nex-composer-send disabled:border-transparent " +
                  (live || canSend
                    ? "bg-primary text-primary-foreground hover:bg-primary-hover active:bg-primary-pressed"
                    : "bg-secondary text-muted-foreground disabled:bg-secondary disabled:text-muted-foreground disabled:opacity-100")
                }
                disabled={!live && !canSend}
                aria-busy={(!live && busy) || undefined}
                aria-label={live ? t("conv.stopAria") : t("conv.sendAria")}
                title={live ? t("conv.stopAria") : t("conv.sendAria")}
                data-state={live ? "stop" : canSend ? "armed" : "idle"}
                onClick={live ? onStop : submitDraft}
              >
                {live ? (
                  <StopIcon size={16} />
                ) : (
                  <ArrowUpIcon size={20} className="size-5" />
                )}
              </Button>
            </span>
          </div>
        </div>

        {/* A hint row is only worth its line when it is ACTIONABLE. "Enter
            to send" restated what the field already does and sat under the
            composer on every screen; the setup hint is the one case where
            the composer cannot work and says what to do about it. */}
        {live && attachments.length > 0 && <p className="nex-composer-hint" role="status">{t("conv.attachmentsWaitForRun")}</p>}
        {!ready && (
          <p className="nex-composer-hint">
            <span className="nex-composer-shortcut">
              {t("conv.hintSetup")}{" "}
            </span>
            <M3Button variant="quiet" size="sm" onClick={onOpenSettings}>
              {t("conv.openSettings")}
            </M3Button>
          </p>
        )}
      </div>
    </div>
  );
}
