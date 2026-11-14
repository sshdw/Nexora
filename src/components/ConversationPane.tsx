//! Independently keyed conversation state inside each pane. Stage5 PR5A
//! removes the standalone header and its actions. Split alone adds a quiet
//! title badge; workspace selection remains in Composer.

import ConversationView from "./ConversationView";
import ErrorBoundary from "./ErrorBoundary";
import type { ConnectionModelCapabilities, Conversation } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";
import type { ModelConnectionGroup } from "./ModelPicker";

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
  connectionGroups?: readonly ModelConnectionGroup[];
  onSelectConnectionModel?: (connectionId: string, model: string) => void;
  selectedCapabilities?: ConnectionModelCapabilities | null;
  capabilitiesByKey?: ReadonlyMap<string, ConnectionModelCapabilities>;
  selectedEffort?: string | null;
  onSelectEffort?: (effort: string | null) => void;
  /** Last model-selection failure, surfaced in the composer. */
  modelError: string | null;
  /** Agent-autonomy default store lifted in App (single source, NEX-FE-003). */
  autonomy: AutonomyStore;
  draft: string;
  setDraft: (value: string) => void;
  onOpenSettings: () => void;
  onMessageSent: () => void;
  /** Workspace store for the composer's control row (R6-03). */
  workspace: WorkspaceStore;
  /** Secondary (split) pane: marked apart from the primary one. */
  secondary: boolean;
  /** Compact per-pane identity is visible only while split is active. */
  split?: boolean;
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
  | "connectionGroups"
  | "onSelectConnectionModel"
  | "selectedCapabilities"
  | "capabilitiesByKey"
  | "selectedEffort"
  | "onSelectEffort"
  | "modelError"
  | "autonomy"
  | "onOpenSettings"
  | "onMessageSent"
  | "workspace"
>;

export default function ConversationPane({
  conversation,
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
  draft,
  setDraft,
  onOpenSettings,
  onMessageSent,
  workspace,
  secondary,
  split = false,
}: ConversationPaneProps) {
  const { t } = useStrings();
  const isArchived = conversation.status === "archived";
  return (
    <section
      className="nex-pane"
      aria-label={
        secondary
          ? t("app.splitPaneAria", { title: conversation.title }) +
            (isArchived ? t("tabs.archivedSuffix") : "")
          : conversation.title + (isArchived ? t("tabs.archivedSuffix") : "")
      }
    >
      {split && (
        <div className="nex-pane-identity">
          <span className="nex-pane-title-badge" title={conversation.title}>
            <span className="nex-pane-title-text">{conversation.title}</span>
            {isArchived && <span>{t("app.archivedBadge")}</span>}
          </span>
        </div>
      )}
      <div className="nex-pane-body">
        {/* Keyed by conversation id: each pane owns independent hook state
            (messages, attachments, agent runs, scroll position) so the two
            panes can never clobber each other. Drafts are likewise keyed per
            conversation in App (see `drafts`), never shared. */}
        {/* ADD-05 thread boundary: this is the most dynamic subtree (per-frame
            agent-run renders, markdown bodies, attachments). A frame-shaped
            runtime error degrades to the boundary strip inside the pane —
            pane identity and shell stay up — never a blank window (review 05-ADD
            §3.5a). */}
        <ErrorBoundary>
          <ConversationView
            key={conversation.id}
            conversationId={conversation.id}
            selectedProvider={selectedProvider}
            selectedModel={selectedModel}
            models={models}
            onSelectModel={onSelectModel}
            connectionGroups={connectionGroups}
            onSelectConnectionModel={onSelectConnectionModel}
            selectedCapabilities={selectedCapabilities}
            capabilitiesByKey={capabilitiesByKey}
            selectedEffort={selectedEffort}
            onSelectEffort={onSelectEffort}
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
