import ConversationList from "./ConversationList";
import NewConversationButton from "./NewConversationButton";
import NexoraMark from "./NexoraMark";
import SearchBox from "./SearchBox";
import SettingsEntry from "./SettingsEntry";
import TaskEntry from "./TaskEntry";
import VersionControlEntry from "./VersionControlEntry";
import { useNavigation } from "./NavigationContext";
import { useStrings } from "../lib/useLocale";

// Navigation + conversation wiring comes from NavigationContext (NEX-FE-001),
// so the shell renders a prop-less `<Sidebar />`. Field-for-field identical
// rendering — pure prop-to-context move, no visual change.
//
// Nav trim (#219): the rail keeps exactly three destinations — Version
// Control, Tasks, Settings — under the Chat block (brand + New Conversation +
// Search + conversations). Prompt Library, Activity, Audit, Debt, Issues &
// PRs, Docs, Release, the Workspace folder button + roots switcher and the
// import row have NO rail entry and no other UI access path; their components,
// hooks, i18n keys and backend commands are untouched, so re-adding one is a
// single-row change here. The terminal UI is gone outright (TerminalPanel +
// TerminalEntry deleted); `terminal_run` / `terminal_kill` stay registered
// backend-side.
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
    vcsActive,
    onOpenVersionControl,
    tasksActive,
    onOpenTasks,
    onRename,
    onArchive,
    onRestore,
    onDelete,
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
        <SearchBox conversations={conversations} onSelectResult={onSelect} />
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

      {/* Bottom-anchored rail destinations. `.nex-sidebar-footer` pins this
          group to the rail foot with margin-top:auto, separated by a single
          hairline seam — Version Control and Tasks keep their icon rows, and
          Settings stays last so it reads as the rail's floor (the Cursor
          pattern the owner's reference shows). No-argument wrappers
          (NEX-FE-001): rail `onClick` handlers receive the native click
          event, which must never reach navigation openers (previously
          `openSettings(clickEvent)` set the settings section to the event
          object and Settings opened with a blank body). */}
      <div className="nex-sidebar-footer">
        <VersionControlEntry active={vcsActive} onClick={() => onOpenVersionControl()} />
        <TaskEntry active={tasksActive} onClick={() => onOpenTasks()} />
        <SettingsEntry onClick={() => onOpenSettings()} />
      </div>
    </aside>
  );
}