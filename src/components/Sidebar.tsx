import ActivityHealthEntry from "./ActivityHealthEntry";
import AuditEntry from "./AuditEntry";
import ConversationList from "./ConversationList";
import DebtEntry from "./DebtEntry";
import DocsEntry from "./DocsEntry";
import IssuesEntry from "./IssuesEntry";
import M3RailItem from "./M3RailItem";
import { useNavigation } from "./NavigationContext";
import NewConversationButton from "./NewConversationButton";
import NexoraMark from "./NexoraMark";
import PromptLibraryEntry from "./PromptLibraryEntry";
import ReleaseEntry from "./ReleaseEntry";
import SearchBox from "./SearchBox";
import SettingsEntry from "./SettingsEntry";
import TaskEntry from "./TaskEntry";
import TerminalEntry from "./TerminalEntry";
import VersionControlEntry from "./VersionControlEntry";
import WorkspaceFolderButton from "./WorkspaceFolderButton";
import WorkspaceRootsSwitcher from "./WorkspaceRootsSwitcher";
import { ImportIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

// Navigation + conversation wiring comes from NavigationContext (NEX-FE-001),
// so the shell renders a prop-less `<Sidebar />`. Field-for-field identical
// rendering — pure prop-to-context move, no visual change.
export default function Sidebar() {
  const {
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
    debtActive,
    onOpenDebt,
    ghActive,
    onOpenGh,
    docsActive,
    onOpenDocs,
    releaseActive,
    onOpenRelease,
    onSelectPrompt,
    onImport,
    onRename,
    onArchive,
    onRestore,
    onDelete,
    workspace,
  } = useNavigation();
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
        <NewConversationButton onClick={() => onNewConversation()} disabled={creating}>
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
          margin-top:auto, separated by a single hairline seam.
          No-argument wrappers (NEX-FE-001): rail `onClick` handlers receive
          the native click event, which must never reach navigation openers
          (previously `openSettings(clickEvent)` set the settings section to
          the event object and Settings opened with a blank body). */}
      <div className="nex-sidebar-footer">
        <SettingsEntry onClick={() => onOpenSettings()} />
        <PromptLibraryEntry active={libraryActive} onClick={() => onOpenPromptLibrary()} />
        <VersionControlEntry active={vcsActive} onClick={() => onOpenVersionControl()} />
        <ActivityHealthEntry active={activityActive} onClick={() => onOpenActivity()} />
        <TerminalEntry active={terminalActive} onClick={() => onOpenTerminal()} />
        <TaskEntry active={tasksActive} onClick={() => onOpenTasks()} />
        <AuditEntry active={auditActive} onClick={() => onOpenAudit()} />
        <DebtEntry active={debtActive} onClick={() => onOpenDebt()} />
        <IssuesEntry active={ghActive} onClick={() => onOpenGh()} />
        <DocsEntry active={docsActive} onClick={() => onOpenDocs()} />
        <ReleaseEntry active={releaseActive} onClick={() => onOpenRelease()} />
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
          onClick={() => onImport()}
        />
      </div>
    </aside>
  );
}
