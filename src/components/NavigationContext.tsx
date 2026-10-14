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

// Nav trim (#219): the interface below is the SHELL's nav contract — it
// names only what the sidebar actually renders. Destinations the rail no
// longer offers (Prompt Library, Activity, Audit, Debt, Issues & PRs, Docs,
// Release, Terminal, Workspace roots, import) are not listed here, because a
// context member the sidebar never reads is a dead nav entry. Their
// components, hooks, i18n keys, router state and backend commands are all
// intact — re-adding one restores the member, the rail row and App's wiring
// without touching the panel.

import { createContext, useContext } from "react";

import type { CommandError, Conversation } from "../lib/tauri";

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
  /** Whether the Version Control screen is currently shown. */
  vcsActive: boolean;
  onOpenVersionControl: () => void;
  /** Whether the Tasks screen is currently shown. */
  tasksActive: boolean;
  onOpenTasks: () => void;
  onRename: (id: number, title: string) => Promise<boolean>;
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
  onDelete: (id: number) => void;
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
