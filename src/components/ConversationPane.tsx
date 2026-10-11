//! One conversation pane of the M3E shell's main area (NEX-FE-001).
//!
//! Extracted verbatim from `src/App.tsx` (`ConversationPane`, formerly
//! App.tsx:70-166): pane header (title + archived badge, workspace chip,
//! split toggle, export) over a `ConversationView` keyed by conversation id,
//! so each pane owns independent hook state (messages, attachments, agent
//! runs, scroll position) and the two split panes can never clobber each
//! other. No visual or behavioral change — pure move plus the lifted
//! `autonomy` store prop (NEX-FE-003).

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
  /** Agent-autonomy default store lifted in App (single source, NEX-FE-003). */
  autonomy: AutonomyStore;
  draft: string;
  setDraft: (value: string) => void;
  onOpenSettings: () => void;
  onMessageSent: () => void;
  onExport: (id: number) => void;
  workspaceRoot: string | null;
  workspaceLoading: boolean;
  /** Secondary (split) pane: the header offers "Close split". */
  secondary: boolean;
  /** Primary pane: whether the "Split" opener is available. */
  splitAvailable: boolean;
  splitOpen: boolean;
  onToggleSplit: () => void;
}

export default function ConversationPane({
  conversation,
  selectedProvider,
  selectedModel,
  autonomy,
  draft,
  setDraft,
  onOpenSettings,
  onMessageSent,
  onExport,
  workspaceRoot,
  workspaceLoading,
  secondary,
  splitAvailable,
  splitOpen,
  onToggleSplit,
}: ConversationPaneProps) {
  const { t } = useStrings();
  const isArchived = conversation.status === "archived";
  return (
    <section
      className={"nex-pane" + (secondary ? " nex-pane--secondary" : "")}
      aria-label={secondary ? t("app.splitPaneAria", { title: conversation.title }) : conversation.title}
    >
      <header className="nex-pane-header">
        <h2 className="nex-main-title">
          <span className="nex-main-title-text">{conversation.title}</span>
          {isArchived && (
            <span className="nex-main-title-badge">
              {t("app.archivedBadge")}
            </span>
          )}
        </h2>
        <M3Toolbar
          label={t("app.paneActions", { title: conversation.title })}
          className="nex-main-header-actions"
        >
          <WorkspaceChip root={workspaceRoot} loading={workspaceLoading} />
          <M3Button
            variant="quiet"
            size="sm"
            onClick={onToggleSplit}
            aria-pressed={splitOpen}
            disabled={secondary ? false : !splitAvailable && !splitOpen}
            title={
              secondary
                ? t("app.splitCloseTitle")
                : t("app.splitOpenTitle")
            }
          >
            {secondary ? t("app.splitClose") : t("app.split")}
          </M3Button>
          <M3IconButton
            label={t("app.exportConversation")}
            onClick={() => onExport(conversation.id)}
          >
            <ExportIcon />
          </M3IconButton>
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
