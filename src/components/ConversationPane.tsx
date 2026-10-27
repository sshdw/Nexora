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
  /** The tab strip above owns the session's identity and its actions
   *  (D-04), so this pane renders no header at all. False in split view,
   *  where two panes need to be told apart and each needs its own actions. */
  hideHeader?: boolean;
  /** Chromeless reading mode (its strip home is gone at one session). */
  zen: boolean;
  onToggleZen: () => void;
  /** Draw the Zen control here. False while the tab strip is on screen: the
   *  strip carries it, and split view would otherwise show one per pane plus
   *  one in the strip. */
  showZen?: boolean;
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
  hideHeader = false,
  zen,
  onToggleZen,
  showZen = true,
}: ConversationPaneProps) {
  const { t } = useStrings();
  const isArchived = conversation.status === "archived";
  return (
    <section
      className={"nex-pane" + (secondary ? " nex-pane--secondary" : "")}
      aria-label={secondary ? t("app.splitPaneAria", { title: conversation.title }) : conversation.title}
    >
      {!hideHeader && (
        <header className="nex-pane-header">
          <h2 className="nex-main-title">
            <span className="nex-main-title-text">{conversation.title}</span>
            {isArchived && (
              <span className="nex-main-title-badge">
                {t("app.archivedBadge")}
              </span>
            )}
          </h2>
          {/* separator.md, vertical: the hairline between the title group
              and the trailing controls (MAP row 5). Decorative — it
              carries no text, so it is hidden from assistive tech rather
              than announced. */}
          <span className="nex-pane-header-separator" aria-hidden="true" />
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
            {/* Zen lives here only while the tab strip is not on screen
                (one session): at two or more the strip owns it. */}
            {showZen && (
              <M3Button
                variant="quiet"
                size="sm"
                aria-pressed={zen}
                title={t("tabs.zenTitle")}
                onClick={onToggleZen}
              >
                {t("tabs.zen")}
              </M3Button>
            )}
          </M3Toolbar>
        </header>
      )}
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
          workspaceRoot={workspaceRoot}
        />
      </div>
    </section>
  );
}
