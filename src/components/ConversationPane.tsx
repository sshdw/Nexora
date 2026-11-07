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
//! export only). R6-03 moved the workspace identity to the composer's
//! control row, and R6-04 deleted Zen, so the header is now title + export.
//!
//! The workspace store is threaded to `ConversationView` for that control;
//! the pane header no longer repeats the folder as a read-only chip.

import ConversationView from "./ConversationView";
import ErrorBoundary from "./ErrorBoundary";
import M3IconButton from "./M3IconButton";
import M3Toolbar from "./M3Toolbar";
import { ExportIcon } from "./icons";
import type { Conversation } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
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
  /** Workspace store for the composer's control row (R6-03). */
  workspace: WorkspaceStore;
  /** Secondary (split) pane: marked apart from the primary one. */
  secondary: boolean;
  /** The tab strip above owns the session's identity and its actions
   *  (D-04), so this pane renders no header at all. False in split view,
   *  where two panes need to be told apart and each needs its own actions. */
  hideHeader?: boolean;
}

/** The props every pane receives identically (AC-305, review 03 §2): App
 * builds ONE `PaneSlot` object and each `ConversationPane` call site spreads
 * it, contributing only what differs per pane — the conversation, its draft
 * pair, and the placement flags. Keeps the three call sites from repeating
 * the same ten props. */
export type PaneSlot = Pick<
  ConversationPaneProps,
  | "selectedProvider"
  | "selectedModel"
  | "models"
  | "onSelectModel"
  | "modelError"
  | "autonomy"
  | "onOpenSettings"
  | "onMessageSent"
  | "onExport"
  | "workspace"
>;

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
  workspace,
  secondary,
  hideHeader = false,
}: ConversationPaneProps) {
  const { t } = useStrings();
  const isArchived = conversation.status === "archived";
  return (
    <section
      className="nex-pane"
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
            <M3IconButton
              label={t("app.exportConversation")}
              onClick={() => onExport(conversation.id)}
            >
              <ExportIcon />
            </M3IconButton>
          </M3Toolbar>
        </header>
      )}
      <div className="nex-pane-body">
        {/* Keyed by conversation id: each pane owns independent hook state
            (messages, attachments, agent runs, scroll position) so the two
            panes can never clobber each other. Drafts are likewise keyed per
            conversation in App (see `drafts`), never shared. */}
        {/* ADD-05 thread boundary: this is the most dynamic subtree (per-frame
            agent-run renders, markdown bodies, attachments). A frame-shaped
            runtime error degrades to the boundary strip inside the pane —
            header and shell stay up — never a blank window (review 05-ADD
            §3.5a). */}
        <ErrorBoundary>
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
            workspace={workspace}
          />
        </ErrorBoundary>
      </div>
    </section>
  );
}
