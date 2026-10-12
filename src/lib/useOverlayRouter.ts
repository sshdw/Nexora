//! Overlay router for the M3E shell (NEX-FE-001).
//!
//! Extracted verbatim from `src/App.tsx` (`AppShell`): the eleven
//! workspace-scoped navigation destinations (Settings, Prompt Library,
//! Version Control, Activity & Health, Terminal, Tasks, Code Audit, Debt,
//! Issues & PRs, Docs, Release) each open as an overlay over the
//! still-mounted conversation panes. Opening any destination closes the
//! others (overlay-exit contract); the request-token pattern (`vcsRequest`,
//! `activityRequest`, `terminalRequest`) lets palette deep links retarget an
//! already-open panel without replays on re-render.
//!
//! `active` is the single discriminated view of which overlay is on top,
//! derived in the same priority as App's render chain (library, VCS,
//! activity, terminal, tasks, audit, debt, GH, docs, release, settings), so
//! rail `active` flags and `hasOverlay` can never disagree with what
//! renders. No visual or behavioral change — pure move out of `AppShell`.
//!
//! Drive-by fix (found while extracting): rail/composer `onClick` handlers
//! were wired DIRECTLY to the parameterized openers (`openSettings`,
//! `openVcs`), so the native click event flowed into the optional parameter
//! (`setSettingsSection(clickEvent)`, `setVcsRequest({action: clickEvent})`)
//! and Settings opened with a blank body. `NavigationContext` now exposes
//! only no-argument wrappers; the parameterized openers stay available here
//! for palette deep links.

import { useCallback, useMemo, useState } from "react";

import type { ActivityHealthTab } from "../components/ActivityHealthPanel";
import type {
  PaletteSettingsSection,
  PaletteTerminalRequest,
  PaletteVcsRequest,
} from "./commands";

/** Workspace-scoped overlay destinations (one rail entry each). */
export type OverlayKind =
  | "library"
  | "settings"
  | "vcs"
  | "activity"
  | "terminal"
  | "tasks"
  | "audit"
  | "debt"
  | "gh"
  | "docs"
  | "release";

export interface OverlayRouter {
  libraryOpen: boolean;
  settingsOpen: boolean;
  vcsOpen: boolean;
  activityOpen: boolean;
  terminalOpen: boolean;
  tasksOpen: boolean;
  auditOpen: boolean;
  debtOpen: boolean;
  ghOpen: boolean;
  docsOpen: boolean;
  releaseOpen: boolean;
  /** Which overlay is on top (render-chain priority), or null. */
  active: OverlayKind | null;
  /** Whether any overlay covers the conversation panes. */
  hasOverlay: boolean;
  /** Prompt Library navigation state: the prompt to edit on open. */
  promptToEditId: number | null;
  /** Palette deep-link section for Settings. */
  settingsSection: PaletteSettingsSection;
  /** Palette deep-link request for the VCS panel. */
  vcsRequest: { token: number; action: PaletteVcsRequest } | null;
  /** Activity & Health tab + deep-link request token. */
  activityTab: ActivityHealthTab;
  activityRequest: { token: number; tab: ActivityHealthTab } | null;
  /** Palette deep-link request for the Terminal panel. */
  terminalRequest: { token: number; action: PaletteTerminalRequest } | null;
  openSettings: (section?: PaletteSettingsSection) => void;
  closeSettings: () => void;
  openLibrary: () => void;
  closeLibrary: () => void;
  openVcs: (request?: PaletteVcsRequest) => void;
  closeVcs: () => void;
  openTerminal: (request?: PaletteTerminalRequest) => void;
  closeTerminal: () => void;
  openActivity: (tab?: ActivityHealthTab) => void;
  closeActivity: () => void;
  openTasks: () => void;
  closeTasks: () => void;
  openAudit: () => void;
  closeAudit: () => void;
  openDebt: () => void;
  closeDebt: () => void;
  openGh: () => void;
  closeGh: () => void;
  openDocs: () => void;
  closeDocs: () => void;
  openRelease: () => void;
  closeRelease: () => void;
  /** Open the Prompt Library on one prompt's Edit modal (search result). */
  selectPrompt: (promptId: number) => void;
  /** Close every overlay (overlay-exit contract). */
  closeAll: () => void;
}

export function useOverlayRouter(): OverlayRouter {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [libraryOpen, setLibraryOpen] = useState(false);
  // Version Control screen (read-only git base): a workspace-scoped
  // navigation destination like Settings/Library — a sidebar rail entry
  // (not a header action: the header is per-conversation, VCS is per
  // workspace) opening an overlay over the still-mounted panes.
  const [vcsOpen, setVcsOpen] = useState(false);
  // Prompt Library navigation state. When set from a search result (FR-009), the
  // Prompt Library screen opens with that prompt's existing Edit modal.
  const [promptToEditId, setPromptToEditId] = useState<number | null>(null);
  // Palette deep-link targets: the settings section to show and the VCS
  // panel request to raise when those overlays open from the palette.
  const [settingsSection, setSettingsSection] =
    useState<PaletteSettingsSection>("appearance");
  const [vcsRequest, setVcsRequest] = useState<{
    token: number;
    action: PaletteVcsRequest;
  } | null>(null);
  // Activity & Health screen (read-only feed + project snapshot): ONE overlay
  // area with two tabs — a single rail entry opens it; palette/shortcuts
  // deep-link the tab via the request token (same pattern as vcsRequest).
  const [activityOpen, setActivityOpen] = useState(false);
  const [activityTab, setActivityTab] = useState<ActivityHealthTab>("activity");
  const [activityRequest, setActivityRequest] = useState<{
    token: number;
    tab: ActivityHealthTab;
  } | null>(null);
  // Workspace Terminal screen: a workspace-scoped navigation destination
  // like Settings/Library/VCS/Activity — a sidebar rail entry (not a
  // header action: the header is per-conversation, the terminal is per
  // workspace) opening an overlay over the still-mounted panes. Palette
  // deep-links (clear / focus-input) ride the same request-token pattern
  // as vcsRequest.
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [terminalRequest, setTerminalRequest] = useState<{
    token: number;
    action: PaletteTerminalRequest;
  } | null>(null);
  // Tasks screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal — a sidebar rail entry opening
  // an overlay over the still-mounted panes (palette + Ctrl+Shift+T open it
  // too; no panel request token — the panel reloads from the backend).
  const [tasksOpen, setTasksOpen] = useState(false);
  // Code Audit screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal/Tasks — a sidebar rail entry
  // opening an overlay over the still-mounted panes (palette + Ctrl+Shift+U
  // open it too; the panel scans manually on Run only, never on mount).
  const [auditOpen, setAuditOpen] = useState(false);
  // Debt backlog screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal/Tasks/Audit — a sidebar rail
  // entry opening an overlay over the still-mounted panes (palette opens it
  // too; the panel lists from the backend on mount and imports manually).
  const [debtOpen, setDebtOpen] = useState(false);
  // Issues & PRs screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal/Tasks/Audit — a sidebar rail
  // entry opening an overlay over the still-mounted panes (palette +
  // Ctrl+Shift+G open it too; the panel loads from the backend on mount
  // and on every kind/state change, plus manual Refresh).
  const [ghOpen, setGhOpen] = useState(false);
  // Docs screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal/Tasks/Audit/Debt/GH — a sidebar
  // rail entry opening an overlay over the still-mounted panes (palette opens
  // it too; the migration section loads its version spine from the backend
  // on mount, everything else renders from code-backed registries).
  const [docsOpen, setDocsOpen] = useState(false);
  // Release screen: a workspace-scoped navigation destination like
  // Settings/Library/VCS/Activity/Terminal/Tasks/Audit/Debt/GH/Docs — a
  // sidebar rail entry opening an overlay over the still-mounted panes
  // (palette opens it too; the panel loads from the backend on mount and on
  // manual Refresh).
  const [releaseOpen, setReleaseOpen] = useState(false);

  const openSettings = useCallback((section?: PaletteSettingsSection) => {
    setSettingsSection(section ?? "appearance");
    setSettingsOpen(true);
    setLibraryOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const openLibrary = useCallback(() => {
    // A fresh entry to the library opens the list, not a previously staged edit.
    setPromptToEditId(null);
    setLibraryOpen(true);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const closeLibrary = useCallback(() => {
    setLibraryOpen(false);
    setPromptToEditId(null);
  }, []);
  const closeSettings = useCallback(() => {
    setSettingsOpen(false);
  }, []);
  const openVcs = useCallback((request?: PaletteVcsRequest) => {
    setVcsRequest(request ? { token: Date.now(), action: request } : null);
    setVcsOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const closeVcs = useCallback(() => {
    setVcsOpen(false);
  }, []);
  const openTerminal = useCallback((request?: PaletteTerminalRequest) => {
    setTerminalRequest(request ? { token: Date.now(), action: request } : null);
    setTerminalOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTasksOpen(false);
    // NOTE: verbatim from App.tsx — the terminal opener never touched
    // audit/debt/docs/gh/release flags (pre-existing asymmetry, preserved).
  }, []);
  const closeTerminal = useCallback(() => {
    setTerminalOpen(false);
  }, []);
  const openActivity = useCallback((tab: ActivityHealthTab = "activity") => {
    // Deep links always retarget the tab (even when already open): the
    // token identifies the request so re-renders never replay it.
    setActivityTab(tab);
    setActivityRequest({ token: Date.now(), tab });
    setActivityOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const closeActivity = useCallback(() => {
    setActivityOpen(false);
  }, []);
  const openTasks = useCallback(() => {
    setTasksOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const closeTasks = useCallback(() => {
    setTasksOpen(false);
  }, []);
  const openAudit = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setAuditOpen(true);
  }, []);
  const closeAudit = useCallback(() => {
    setAuditOpen(false);
    setGhOpen(false);
  }, []);
  const openDebt = useCallback(() => {
    setDebtOpen(true);
    setDocsOpen(false);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const closeDebt = useCallback(() => {
    setDebtOpen(false);
  }, []);
  const openGh = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setReleaseOpen(false);
    setGhOpen(true);
  }, []);
  const closeGh = useCallback(() => {
    setGhOpen(false);
  }, []);
  const openDocs = useCallback(() => {
    setDocsOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const openRelease = useCallback(() => {
    setReleaseOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setGhOpen(false);
    setDocsOpen(false);
  }, []);
  const closeDocs = useCallback(() => {
    setDocsOpen(false);
  }, []);
  const closeRelease = useCallback(() => {
    setReleaseOpen(false);
  }, []);

  // Open a prompt found by search: show the Prompt Library and open the selected
  // prompt (not merely the first prompt) in its existing Edit modal (FR-009).
  const selectPrompt = useCallback((promptId: number) => {
    setSettingsOpen(false);
    setLibraryOpen(true);
    setPromptToEditId(promptId);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);

  // Overlay-exit contract shared by tab mutations and palette commands:
  // switching surface closes Settings/Library/VCS/Activity/Terminal/Tasks (same
  // as clicking a tab or sidebar row — overlays never sit above a switched
  // tab).
  const closeAll = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTerminalOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);

  // Discriminated view of the top overlay, in render-chain priority
  // (library first, settings last — must match App's overlay branch).
  const active: OverlayKind | null = useMemo(() => {
    if (libraryOpen) return "library";
    if (vcsOpen) return "vcs";
    if (activityOpen) return "activity";
    if (terminalOpen) return "terminal";
    if (tasksOpen) return "tasks";
    if (auditOpen) return "audit";
    if (debtOpen) return "debt";
    if (ghOpen) return "gh";
    if (docsOpen) return "docs";
    if (releaseOpen) return "release";
    if (settingsOpen) return "settings";
    return null;
  }, [
    libraryOpen,
    vcsOpen,
    activityOpen,
    terminalOpen,
    tasksOpen,
    auditOpen,
    debtOpen,
    ghOpen,
    docsOpen,
    releaseOpen,
    settingsOpen,
  ]);
  const hasOverlay = active !== null;

  // Memoized router identity (NEX-FE-002): every member is state or a
  // useCallback-stable function, so consumers ([router] deps, context value)
  // only see a new reference when overlay state actually changes.
  return useMemo(
    () => ({
      libraryOpen,
      settingsOpen,
      vcsOpen,
      activityOpen,
      terminalOpen,
      tasksOpen,
      auditOpen,
      debtOpen,
      ghOpen,
      docsOpen,
      releaseOpen,
      active,
      hasOverlay,
      promptToEditId,
      settingsSection,
      vcsRequest,
      activityTab,
      activityRequest,
      terminalRequest,
      openSettings,
      closeSettings,
      openLibrary,
      closeLibrary,
      openVcs,
      closeVcs,
      openTerminal,
      closeTerminal,
      openActivity,
      closeActivity,
      openTasks,
      closeTasks,
      openAudit,
      closeAudit,
      openDebt,
      closeDebt,
      openGh,
      closeGh,
      openDocs,
      closeDocs,
      openRelease,
      closeRelease,
      selectPrompt,
      closeAll,
    }),
    [
      libraryOpen,
      settingsOpen,
      vcsOpen,
      activityOpen,
      terminalOpen,
      tasksOpen,
      auditOpen,
      debtOpen,
      ghOpen,
      docsOpen,
      releaseOpen,
      active,
      hasOverlay,
      promptToEditId,
      settingsSection,
      vcsRequest,
      activityTab,
      activityRequest,
      terminalRequest,
      openSettings,
      closeSettings,
      openLibrary,
      closeLibrary,
      openVcs,
      closeVcs,
      openTerminal,
      closeTerminal,
      openActivity,
      closeActivity,
      openTasks,
      closeTasks,
      openAudit,
      closeAudit,
      openDebt,
      closeDebt,
      openGh,
      closeGh,
      openDocs,
      closeDocs,
      openRelease,
      closeRelease,
      selectPrompt,
      closeAll,
    ],
  );
}
