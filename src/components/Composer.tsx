/* ============================================================
   COMPOSER — the conversation's single dominant control (#220)

   Rebuilt Cursor-style on the mono theme (#220, repainted #235):
   ONE rounded container (`.nex-composer-shell`) holding the field,
   its inline shortcut-teaching line, and a compact bottom control
   row — attach, the two primary modes, the mode/model picker,
   and the one light fill (send). Everything else recedes.

   Anatomy stolen from the owner-supplied reference: the single
   rounded container; a field whose placeholder says where typing
   begins, with a teaching line just under it; the round "+" on the
   left with the mode/model picker beside it; the send action on the
   right; a hint row under the box; hairline separators and no fills
   where a line will do. See the PR body for the two deliberate
   departures.

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

import { formatBytes } from "../lib/format";
import type { Attachment, AutonomyMode } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import { useStrings } from "../lib/useLocale";
import ComposerAutonomyHint from "./ComposerAutonomyHint";
import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import M3SegmentedGroup from "./M3SegmentedGroup";
import Tooltip from "./Tooltip";
import { Button } from "./ui/button";
import {
  Popover,
  PopoverContent,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import { ArrowUpIcon, ChevronDownIcon, CloseIcon, PaperclipIcon, PlusIcon } from "./icons";

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
}: ComposerProps) {
  const { t } = useStrings();
  const busy = sending || agentBusy;

  return (
    <div className="nex-composer">
      <div className="nex-composer-inner">
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
              className="nex-composer-input"
              rows={1}
              placeholder={t("conv.messagePlaceholder")}
              aria-label={t("conv.messageAria")}
              value={draft}
              disabled={busy}
              onChange={(event) => onDraftChange(event.target.value)}
              onKeyDown={(event) => {
                // shortcut:composer.send (Shift+Enter = shortcut:composer.newline).
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  onSubmit();
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

            {/* The trailing group is ONE flex item, not three: in a split
                pane the row wraps, and three loose items split across the
                break (the model name on line one, Send alone on line two).
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

            {/* Model picker. The trigger shows what the provider
                store holds; picking a row DISPATCHES and waits —
                the label changes when the store says so. No
                tooltip on it: the trigger already carries the
                model in text, and a bubble repeating it is noise
                (the icon-only attach beside it does get one).

                THE FULL ID IS THE TITLE (Phase C round 4). The
                visible label renders in full for every id in the
                catalog (`.nex-composer-model-name` is capped at
                50ch, the longest one), but the trigger can still
                shrink on a card narrower than the bar's budget —
                and a mid-token ellipsis on `…-super-…` hides the
                variant, which is the part that names the model.
                `title` is therefore the full id whenever one is
                selected: it is the hover read AND the accessible
                description, so AT never gets the cut string. With
                no model selected the title stays the explanation
                (there is no id to carry). The accessible NAME is
                left alone — it says what the control is
                ("Agent mode and model"), not which model is in it. */}
            <Popover>
              <PopoverTrigger asChild>
                <Button
                  variant="ghost"
                  size="sm"
                  className="nex-composer-model-trigger"
                  aria-label={t("conv.modePickerAria")}
                  title={selectedModel ?? t("conv.modelTagTitle")}
                >
                  <span
                    className={
                      selectedModel === null
                        ? "nex-composer-model-name is-placeholder"
                        : "nex-composer-model-name"
                    }
                  >
                    {selectedModel ?? t("conv.modelNone")}
                  </span>
                  <ChevronDownIcon />
                </Button>
              </PopoverTrigger>
              <PopoverContent
                align="end"
                side="top"
                sideOffset={8}
                className="nex-composer-model-menu"
              >
                <PopoverHeader>
                  <PopoverTitle>{t("conv.modelSection")}</PopoverTitle>
                </PopoverHeader>
                {/* A vertical list of options: the primitive is laid
                    out as rows here (composer.css re-flows its grid
                    from columns to rows) and `indicator={false}`
                    keeps a pill from sliding across rows, where a
                    slide means nothing. The selected row keeps its
                    own wash and leading tick. */}
                {models.length === 0 ? (
                  <p className="nex-composer-model-empty">{t("conv.modelNone")}</p>
                ) : (
                  <M3SegmentedGroup
                    className="nex-composer-model-list"
                    label={t("conv.modelSection")}
                    semantics="radio"
                    indicator={false}
                    value={selectedModel}
                    onChange={(model) => onSelectModel(model)}
                    options={models.map((model) => ({ value: model, label: model }))}
                  />
                )}
                {modelError && (
                  <p className="nex-composer-model-empty" role="alert">
                    {modelError || t("conv.modelSwitchFailed")}
                  </p>
                )}
                <div className="nex-composer-model-footer">
                  <Button variant="ghost" size="xs" onClick={onOpenSettings}>
                    {t("conv.modelOpenSettings")}
                  </Button>
                </div>
              </PopoverContent>
            </Popover>

            {/* Send. `disabled:border-transparent` is the ONE state token
                the composer adds: `ui/button`'s default variant ships the
                theme's disabled recipe as a full 1px hairline
                (`disabled:border-border`), and a hairline is exactly what
                makes an inert Send read as an enabled secondary button.
                THEME §1's disabled hairline is therefore dropped for this
                control only — muted label and no fill stay. It has to be a
                utility here rather than a rule in composer.css: that sheet
                is `layer(base)` and a Tailwind utility outranks it at any
                specificity, so only `!important` (reserved for the motion
                gate) could win from the sheet. `cn` runs tailwind-merge, so
                this class replaces the variant's `disabled:border-border`
                instead of stacking with it —
                src/components/Composer.test.tsx asserts the merge. */}
            <Button
              className="nex-composer-send disabled:border-transparent"
              disabled={!canSend}
              aria-busy={busy || undefined}
              onClick={onSubmit}
            >
              {busy ? (
                agentMode ? t("conv.running") : t("conv.sending")
              ) : (
                <>
                  {t("conv.send")}
                  <ArrowUpIcon aria-hidden="true" />
                </>
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