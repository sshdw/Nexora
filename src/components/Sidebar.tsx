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
//
// #230 Phase 1 — the rail is the first shell surface painted from the mono
// theme. Its composition follows `components/sidebar.md` + blocks/sidebar-07
// (header · actions · search · scrolling content · footer rail) at
// NEXORA-COMPONENT-MAP.md row 1's binding deviations, and every value resolves
// to a token: the rail's own surface is `--card` (one tone step above the
// canvas, THEME §2), the seam a hairline, the width `w-64` = 256px (THEME §4
// 240–256, normalized by THEME §6). Tailwind carries structure only
// (NEXORA-DESIGN-SYSTEM.md §1) — never a colour, radius or duration.
//
// TWO hooks survive the migration on purpose, and neither is a paint class:
//   `data-slot="sidebar"` — src/styles/tabs.css hides the rail in Zen mode
//     through this selector (that sheet is Phase 2's), so the hook travels
//     with the surface instead of leaving a `nex-*` class behind.
//   `nex-logo` on the mark — theme.css inverts the brand asset on the light
//     theme through that class; NexoraMark's prop surface is fixed, so the
//     class is the asset hook, not a style. Both die in Phase 6.
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
    <aside
      data-slot="sidebar"
      aria-label="Nexora"
      className="flex w-64 shrink-0 flex-col overflow-hidden border-r border-border bg-card font-sans text-card-foreground"
    >
      <div className="flex flex-col gap-2 px-2 pt-2">
        <div className="flex items-center gap-2 px-1">
          <NexoraMark className="nex-logo size-5" width={22} height={22} />
          <span
            className="text-sm font-semibold tracking-tight text-foreground"
            aria-hidden="true"
          >
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

      {/* Bottom-anchored rail destinations, Settings last so it reads as
          the rail's floor. The single hairline above the group is the one
          structural seam the rail keeps (THEME §2: borders are structural;
          everything else separates by tone). No-argument wrappers
          (NEX-FE-001): rail `onClick` handlers receive the native click
          event, which must never reach navigation openers (previously
          `openSettings(clickEvent)` set the settings section to the event
          object and Settings opened with a blank body). */}
      <div
        data-slot="sidebar-footer"
        className="mt-auto flex flex-col gap-0.5 border-t border-border px-2 pb-2 pt-1.5"
      >
        <VersionControlEntry active={vcsActive} onClick={() => onOpenVersionControl()} />
        <TaskEntry active={tasksActive} onClick={() => onOpenTasks()} />
        <SettingsEntry onClick={() => onOpenSettings()} />
      </div>
    </aside>
  );
}
