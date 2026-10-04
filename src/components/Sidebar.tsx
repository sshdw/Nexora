import type { CommandError, Conversation } from "../lib/tauri";
import type { WorkspaceStore } from "../lib/useWorkspace";
import ActivityHealthEntry from "./ActivityHealthEntry";
import AuditEntry from "./AuditEntry";
import ConversationList from "./ConversationList";
import IssuesEntry from "./IssuesEntry";
import M3RailItem from "./M3RailItem";
import NewConversationButton from "./NewConversationButton";
import NexoraMark from "./NexoraMark";
import PromptLibraryEntry from "./PromptLibraryEntry";
import SearchBox from "./SearchBox";
import SettingsEntry from "./SettingsEntry";
import TaskEntry from "./TaskEntry";
import TerminalEntry from "./TerminalEntry";
import VersionControlEntry from "./VersionControlEntry";
import WorkspaceFolderButton from "./WorkspaceFolderButton";
import WorkspaceRootsSwitcher from "./WorkspaceRootsSwitcher";
import { ImportIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface SidebarProps {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  creating: boolean;
  busy: boolean;
  selectedId: number | null;
  onSelect: (id: number) => void;
  onExport: (id: number) => void;
  onNewConversation: () => void;
  onRetry: () => void;
  onOpenSettings: () => void;
  /** Whether the Prompt Library screen is currently shown. */
  libraryActive: boolean;
  onOpenPromptLibrary: () => void;
  /** Whether the Version Control screen is currently shown. */
  vcsActive: boolean;
  onOpenVersionControl: () => void;
  /** Whether the Activity & Health screen is currently shown. */
  activityActive: boolean;
  onOpenActivity: () => void;
  /** Whether the Terminal screen is currently shown. */
  terminalActive: boolean;
  onOpenTerminal: () => void;
  /** Whether the Tasks screen is currently shown. */
  tasksActive: boolean;
  onOpenTasks: () => void;
  /** Whether the Code Audit screen is currently shown. */
  auditActive: boolean;
  onOpenAudit: () => void;
  /** Whether the Issues & PRs screen is currently shown. */
  ghActive: boolean;
  onOpenGh: () => void;
  /** Open a prompt found by search in the Prompt Library editor. */
  onSelectPrompt: (promptId: number) => void;
  /** Open the import-conversation flow (FR-011). */
  onImport: () => void;
  onRename: (id: number, title: string) => Promise<void>;
  onArchive: (id: number) => void;
  onRestore: (id: number) => void;
  onDelete: (id: number) => void;
  /** Agent workspace folder store (1.3.0 folder picker + recent list). */
  workspace: WorkspaceStore;
}

export default function Sidebar({
  conversations,
  loading,
  error,
  creating,
  busy,
  selectedId,
  onSelect,
  onExport,
  onNewConversation,
  onRetry,
  onOpenSettings,
  libraryActive,
  onOpenPromptLibrary,
  vcsActive,
  onOpenVersionControl,
  activityActive,
  onOpenActivity,
  terminalActive,
  onOpenTerminal,
  tasksActive,
  onOpenTasks,
  auditActive,
  onOpenAudit,
  ghActive,
  onOpenGh,
  onSelectPrompt,
  onImport,
  onRename,
  onArchive,
  onRestore,
  onDelete,
  workspace,
}: SidebarProps) {
  const { t } = useStrings();
  return (
    <aside className="nex-sidebar" aria-label="Nexora">
      <div className="nex-sidebar-head">
        <div className="nex-brand">
          <NexoraMark className="nex-logo" width={22} height={22} />
          <span className="nex-brand-name" aria-hidden="true">
            Nexora
          </span>
        </div>
        <NewConversationButton onClick={onNewConversation} disabled={creating}>
          {t("nav.newConversation")}
        </NewConversationButton>
        <SearchBox
          conversations={conversations}
          onSelectResult={onSelect}
          onSelectPrompt={onSelectPrompt}
        />
      </div>

      <ConversationList
        conversations={conversations}
        loading={loading}
        error={error}
        selectedId={selectedId}
        busy={busy}
        onSelect={onSelect}
        onExport={onExport}
        onRetry={onRetry}
        onRename={onRename}
        onArchive={onArchive}
        onRestore={onRestore}
        onDelete={onDelete}
      />

      {/* Bottom-anchored rail destinations (settings entry + workspace):
          .nex-sidebar-footer pins this group to the rail foot with
          margin-top:auto, separated by a single hairline seam. */}
      <div className="nex-sidebar-footer">
        <SettingsEntry onClick={onOpenSettings} />
        <PromptLibraryEntry active={libraryActive} onClick={onOpenPromptLibrary} />
        <VersionControlEntry active={vcsActive} onClick={onOpenVersionControl} />
        <ActivityHealthEntry active={activityActive} onClick={onOpenActivity} />
        <TerminalEntry active={terminalActive} onClick={onOpenTerminal} />
        <TaskEntry active={tasksActive} onClick={onOpenTasks} />
        <AuditEntry active={auditActive} onClick={onOpenAudit} />
        <IssuesEntry active={ghActive} onClick={onOpenGh} />
        <WorkspaceFolderButton store={workspace} />
        <WorkspaceRootsSwitcher store={workspace} />
        {workspace.error && (
          <p className="nex-sidebar-error" role="alert">
            {workspace.error.message}
          </p>
        )}
        <M3RailItem
          label={t("nav.importConversation")}
          icon={<ImportIcon />}
          onClick={onImport}
        />
      </div>
    </aside>
  );
}


