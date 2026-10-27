//! One conversation pane of the M3E shell's main area (NEX-FE-001).
//!
//! Extracted verbatim from `src/App.tsx` (`ConversationPane`, formerly
//! App.tsx:70-166): pane header (title + archived badge, workspace chip,
//! export) over a `ConversationView` keyed by conversation id, so each
//! pane owns independent hook state (messages, attachments, agent runs,
//! scroll position) and the two split panes can never clobber each
//! other.
//!
//! #230 Phase 2 dropped the header's own Split control (SYSTEM §4: ONE
//! Split entry point — the tab strip owns it; the header keeps title +
//! workspace chip + export only). The props it needed went with it,
//! and App.tsx passes none of them here.

import ConversationView from "./ConversationView";
import M3Button from "./M3Button";
import M3IconButton from "./M3IconButton";
import M3Toolbar from "./M3Toolbar";
import WorkspaceChip from "./WorkspaceChip";
import { ExportIcon } from "./icons";
import type { Conversation } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import { useStrings } from "../lib/useLocale";

export interface ConversationPaneProps {
  conversation: Conversation;
  selectedProvider: string | null;
  selectedModel: string | null;
  /** Models the selected provider offers (#220) — the composer's
   * model list. Passed down, never re-fetched here: the provider
   * store in App stays the only source. */
  models: readonly string[];
  /** Persist a model choice (the app's provider store). */
  onSelectModel: (model: string) => void;
  /** Last model-selection failure, surfaced in the composer. */
  modelError: string | null;
  /** Agent-autonomy default store lifted in App (single source, NEX-FE-003). */
  autonomy: AutonomyStore;
  draft: string;
  setDraft: (value: string) => void;
  onOpenSettings: () => void;
  onMessageSent: () => void;
  onExport: (id: number) => void;
  workspaceRoot: string | null;
  workspaceLoading: boolean;
  /** Secondary (split) pane: marked apart from the primary one. */
  secondary: boolean;
  /** The tab strip above already names the open session (D-04), so this
   *  pane does not repeat the title. False in split view, where two panes
   *  need to be told apart. */
  hideTitle?: boolean;
  /** Chromeless reading mode (its strip home is gone at one session). */
  zen: boolean;
  onToggleZen: () => void;
}

export default function ConversationPane({
  conversation,
  selectedProvider,
  selectedModel,
  models,
  onSelectModel,
  modelError,
  autonomy,
  draft,
  setDraft,
  onOpenSettings,
  onMessageSent,
  onExport,
  workspaceRoot,
  workspaceLoading,
  secondary,
  hideTitle = false,
  zen,
  onToggleZen,
}: ConversationPaneProps) {
  const { t } = useStrings();
  const isArchived = conversation.status === "archived";
  return (
    <section
      className={"nex-pane" + (secondary ? " nex-pane--secondary" : "")}
      aria-label={secondary ? t("app.splitPaneAria", { title: conversation.title }) : conversation.title}
    >
      <header className="nex-pane-header">
        {!hideTitle && (
          <h2 className="nex-main-title">
            <span className="nex-main-title-text">{conversation.title}</span>
            {isArchived && (
              <span className="nex-main-title-badge">
                {t("app.archivedBadge")}
              </span>
            )}
          </h2>
        )}
        {/* separator.md, vertical: the hairline between the title group
            and the trailing controls (MAP row 5). Decorative — it
            carries no text, so it is hidden from assistive tech rather
            than announced. It earns its place only when there IS a title
            group to separate. */}
        {!hideTitle && (
          <span className="nex-pane-header-separator" aria-hidden="true" />
        )}
        <M3Toolbar
          label={t("app.paneActions", { title: conversation.title })}
          className="nex-main-header-actions"
        >
          <WorkspaceChip root={workspaceRoot} loading={workspaceLoading} />
          <M3IconButton
            label={t("app.exportConversation")}
            onClick={() => onExport(conversation.id)}
          >
            <ExportIcon />
          </M3IconButton>
          {/* Zen's strip home is only rendered at two or more sessions
              (D-04), so the pane header carries it for the common case. */}
          <M3Button
            variant="quiet"
            size="sm"
            aria-pressed={zen}
            title={t("tabs.zenTitle")}
            onClick={onToggleZen}
          >
            {t("tabs.zen")}
          </M3Button>
        </M3Toolbar>
      </header>
      <div className="nex-pane-body">
        {/* Keyed by conversation id: each pane owns independent hook state
            (messages, attachments, agent runs, scroll position) so the two
            panes can never clobber each other. Drafts are likewise keyed per
            conversation in App (see `drafts`), never shared. */}
        <ConversationView
          key={conversation.id}
          conversationId={conversation.id}
          selectedProvider={selectedProvider}
          selectedModel={selectedModel}
          models={models}
          onSelectModel={onSelectModel}
          modelError={modelError}
          autonomy={autonomy}
          onOpenSettings={onOpenSettings}
          onMessageSent={onMessageSent}
          draft={draft}
          setDraft={setDraft}
        />
      </div>
    </section>
  );
}
