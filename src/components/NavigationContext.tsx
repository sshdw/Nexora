//! Navigation context for the M3E shell sidebar (NEX-FE-001).
//!
//! `Sidebar` took ~30 props (every rail destination's `active` flag +
//! opener, conversation list + CRUD, workspace store, import/search hooks).
//! It now reads everything from this context, so `AppShell` renders a
//! prop-less `<Sidebar />` and navigation wiring lives in one place.
//!
//! All openers are invoked through Sidebar's no-argument click wrappers:
//! rail `onClick` handlers receive the native click event, which must never
//! reach parameterized openers (previously `openSettings(clickEvent)` set
//! the settings section to the event object and Settings opened with a
//! blank body). App additionally passes no-arg wrappers; palette deep links
//! keep calling the router's parameterized openers directly.

import { createContext, useContext } from "react";

import type { CommandError, Conversation } from "../lib/tauri";
import type { WorkspaceStore } from "../lib/useWorkspace";

export interface Navigation {
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
  /** Whether the Debt backlog screen is currently shown. */
  debtActive: boolean;
  onOpenDebt: () => void;
  /** Whether the Issues & PRs screen is currently shown. */
  ghActive: boolean;
  onOpenGh: () => void;
  /** Whether the Docs screen is currently shown. */
  docsActive: boolean;
  onOpenDocs: () => void;
  /** Whether the Release screen is currently shown. */
  releaseActive: boolean;
  onOpenRelease: () => void;
  /** Open a prompt found by search in the Prompt Library editor. */
  onSelectPrompt: (promptId: number) => void;
  /** Open the import-conversation flow (FR-011). */
  onImport: () => void;
  onRename: (id: number, title: string) => Promise<boolean>;
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
  onDelete: (id: number) => void;
  /** Agent workspace folder store (1.3.0 folder picker + recent list). */
  workspace: WorkspaceStore;
}

const NavigationContext = createContext<Navigation | null>(null);

export const NavigationProvider = NavigationContext.Provider;

export function useNavigation(): Navigation {
  const navigation = useContext(NavigationContext);
  if (navigation === null) {
    throw new Error("useNavigation must be used inside NavigationProvider");
  }
  return navigation;
}
