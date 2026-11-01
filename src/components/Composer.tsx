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

   NO ATTACHMENTS IN AGENT MODE (R4-02 / G-20). `start_agent_run`
   cannot carry attachments yet, so while Agent is the mode the
   "+" control, the attached-file chips and the "+ attach files"
   teaching chip are ABSENT — not disabled, not greyed. A disabled
   control promises "later"; only an absent one promises nothing.
   The draft itself survives the switch untouched: switching modes
   is not a send, and a send is the only thing that clears it.

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

import { useEffect, useRef } from "react";

import { formatBytes } from "../lib/format";
import type { Attachment, AutonomyMode } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import ContextRing from "./ContextRing";
import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import M3SegmentedGroup from "./M3SegmentedGroup";
import ModelPicker from "./ModelPicker";
import Tooltip from "./Tooltip";
import WorkspaceContextRow from "./WorkspaceContextRow";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import {
  ArrowUpIcon,
  ChevronDownIcon,
  CloseIcon,
  PaperclipIcon,
  PlusIcon,
  StopIcon,
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
  /** Workspace store for the control row above the card (D3 / R6-03). */
  workspace: WorkspaceStore;
  /** Focus the field when an empty conversation opens (D1). The effect
   *  never steals focus from a modal or from another field. */
  autoFocusField?: boolean;
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
  autonomy,
  onAutonomyChange,
  selectedModel,
  models,
  onSelectModel,
  modelError = null,
  ready,
  onOpenSettings,
  conversationId,
  contextRevision,
  liveRunId,
  onStop,
  workspace,
  autoFocusField = false,
}: ComposerProps) {
  const { t } = useStrings();
  const busy = sending || agentBusy;
  const live = liveRunId !== null;
  const fieldRef = useRef<HTMLTextAreaElement>(null);
  const autoFocusedRef = useRef(false);

  // D1: an empty conversation is the composer, so the caret belongs in the
  // field. The guard is the whole contract: only a body/no-focus state (or
  // this field already) may be claimed, and a modal on top always wins.
  useEffect(() => {
    if (!autoFocusField || autoFocusedRef.current) return;
    if (document.querySelector('[role="dialog"][aria-modal="true"]') !== null) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body && active !== fieldRef.current) return;
    fieldRef.current?.focus();
    autoFocusedRef.current = true;
  }, [autoFocusField]);

  return (
    <div className="nex-composer">
      <div className="nex-composer-inner">
        <WorkspaceContextRow workspace={workspace} />
        <div className="nex-composer-shell">
          {/* Attachments belong to the CHAT path only. An agent run
              cannot carry them (G-20: `start_agent_run` has no
              attachment parameter), so neither the draft chips nor
              the attach control render in Agent mode: showing a file
              the run would silently drop is a promise the backend
              cannot keep. */}
          {!agentMode && attachments.length > 0 && (
            <ul className="nex-composer-attachments" aria-label={t("conv.attachedFiles")}>
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

          <div className="nex-composer-field">
            <textarea
              ref={fieldRef}
              className="nex-composer-input"
              rows={1}
              placeholder={t("conv.messagePlaceholder")}
              aria-label={t("conv.messageAria")}
              value={draft}
              disabled={busy}
              onChange={(event) => onDraftChange(event.target.value)}
              onKeyDown={(event) => {
                // shortcut:composer.send (Shift+Enter = shortcut:composer.newline).
                // D2: Enter never sends while a run is live — the one button is
                // a stop in that state, and typing a second message mid-run is
                // not a send.
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  if (!live) onSubmit();
                }
              }}
            />
          </div>

          <div className="nex-composer-bar">
            {/* Chat only: absent (not disabled) in Agent mode — see the
                attachment note at the top of this file. */}
            {!agentMode && (
              <Tooltip label={t("conv.addFile")}>
                <Button
                  variant="ghost"
                  size="icon-lg"
                  className="nex-composer-attach rounded-full"
                  aria-label={t("conv.addFile")}
                  title={t("conv.addFile")}
                  disabled={busy || attachmentsBusy}
                  onClick={onPickFiles}
                >
                  <PlusIcon />
                </Button>
              </Tooltip>
            )}

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
                    leading: <span className="nex-composer-pill-dot" aria-hidden="true" />,
                  },
                  {
                    value: "agent",
                    label: t("conv.agentToggle"),
                    leading: <span className="nex-composer-pill-dot" aria-hidden="true" />,
                  },
                ]}
              />
            </span>

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
                    aria-label={t("conv.autonomyAria")}
                    title={t("conv.autonomyLabel")}
                  >
                    <span className="nex-composer-autonomy-dot" aria-hidden="true" />
                    <span className="nex-composer-autonomy-name">
                      {autonomy.mode === "supervised"
                        ? t("conv.autonomySupervised")
                        : autonomy.mode === "semi_autonomous"
                          ? t("conv.autonomySemi")
                          : t("conv.autonomyFull")}
                    </span>
                    <ChevronDownIcon />
                  </Button>
                </PopoverTrigger>
                <PopoverContent align="end" side="top" sideOffset={8} className="nex-composer-autonomy-menu">
                  <PopoverHeader>
                    <PopoverTitle>{t("conv.autonomyLabel")}</PopoverTitle>
                  </PopoverHeader>
                  <M3SegmentedGroup
                    className="nex-composer-autonomy"
                    label={t("conv.autonomyAria")}
                    semantics="radio"
                    indicator={false}
                    value={autonomy.mode}
                    onChange={(mode) => onAutonomyChange(mode)}
                    options={[
                      { value: "supervised", label: t("conv.autonomySupervised") },
                      {
                        value: "semi_autonomous",
                        label: t("conv.autonomySemi"),
                        ariaLabel: t("conv.autonomySemiAria"),
                      },
                      { value: "full_autonomous", label: t("conv.autonomyFull") },
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
            <ContextRing conversationId={conversationId} revision={contextRevision} />

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
              modelError={modelError}
              onOpenSettings={onOpenSettings}
            />

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
              onClick={live ? onStop : onSubmit}
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
        {!ready && (
          <p className="nex-composer-hint">
            <span className="nex-composer-shortcut">{t("conv.hintSetup")} </span>
            <M3Button variant="quiet" size="sm" onClick={onOpenSettings}>
              {t("conv.openSettings")}
            </M3Button>
          </p>
        )}
      </div>
    </div>
  );
}