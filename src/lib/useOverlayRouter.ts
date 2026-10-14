//! Overlay router for the M3E shell (NEX-FE-001).
//!
//! Extracted verbatim from `src/App.tsx` (`AppShell`): the workspace-scoped
//! navigation destinations each open as an overlay over the still-mounted
//! conversation panes. Opening any destination closes the others (overlay-exit
//! contract); the request-token pattern (`vcsRequest`, `activityRequest`) lets
//! palette deep links retarget an already-open panel without replays on
//! re-render.
//!
//! Nav trim (#219): of the ten overlays below only three are reachable
//! from the shell today (settings, vcs, tasks) — the rail offers no entry for
//! the other seven and the palette registers no command that opens them, so
//! their flags stay `false` and their branches never render. They are NOT
//! dead code: each panel, its hook, its i18n keys and its backend commands
//! stay, so re-adding a rail row or a palette command re-opens the overlay
//! with no other change. The terminal flag/opener/priority branch are gone
//! outright (the terminal UI is deleted); `terminal_run`/`terminal_kill`
//! stay registered backend-side.
//!
//! `active` is the single discriminated view of which overlay is on top,
//! derived in the same priority as App's render chain (library, VCS,
//! activity, tasks, audit, debt, GH, docs, release, settings), so
//! rail `active` flags and `hasOverlay` can never disagree with what
//! renders. Extracted from `AppShell` as a pure move; #219 only removed the
//! terminal rows and unreachable entries.
//!
//! Drive-by fix (found while extracting): rail/composer `onClick` handlers
//! were wired DIRECTLY to the parameterized openers (`openSettings`,
//! `openVcs`), so the native click event flowed into the optional parameter
//! (`setSettingsSection(clickEvent)`, `setVcsRequest({action: clickEvent})`)
//! and Settings opened with a blank body. `NavigationContext` now exposes
//! only no-argument wrappers; the parameterized openers stay available here
//! for palette deep links.
//!
//! Follow-up fix (NEX-UX-001 remainder): the terminal opener never closed
//! the audit/debt/docs/gh/release flags, so those flags could stay true
//! under an open Terminal. The render chain still showed exactly one overlay
//! (priority branch), but the flags disagreed with it — now every opener
//! closes every other overlay, so flags and render can never disagree. (The
//! terminal opener itself was removed with the terminal UI in #219; the
//! per-opener resets it proved still govern the remaining overlays.)

import { useCallback, useMemo, useState } from "react";

import type { ActivityHealthTab } from "../components/ActivityHealthPanel";
import type {
  PaletteSettingsSection,
  PaletteVcsRequest,
} from "./commands";

/** Workspace-scoped overlay destinations (one rail entry each). */
export type OverlayKind =
  | "library"
  | "settings"
  | "vcs"
  | "activity"
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
  openSettings: (section?: PaletteSettingsSection) => void;
  closeSettings: () => void;
  openLibrary: () => void;
  closeLibrary: () => void;
  openVcs: (request?: PaletteVcsRequest) => void;
  closeVcs: () => void;
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
  /** Open the Prompt Library on one prompt's Edit modal. Unreachable since
   * #219 (the sidebar search box no longer routes prompt hits), kept with
   * the library so the deep link returns with the panel. */
  selectPrompt: (promptId: number) => void;
  /** Close every overlay (overlay-exit contract). */
  closeAll: () => void;
}

export function useOverlayRouter(): OverlayRouter {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [libraryOpen, setLibraryOpen] = useState(false);
  // Version Control screen (read-only git base): a workspace-scoped
  // navigation destination like Settings — a sidebar rail entry (not a header
  // action: the header is per-conversation, VCS is per workspace) opening an
  // overlay over the still-mounted panes.
  const [vcsOpen, setVcsOpen] = useState(false);
  // Prompt Library navigation state: the prompt to edit on open (FR-009).
  // Unreachable since #219 — the library has no rail entry, and the sidebar
  // search box no longer routes prompt hits here — but the flag and the
  // panel's `initialEditId` stay so re-adding either restores the deep link.
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
  // area with two tabs — deep-linked to a tab via the request token (same
  // pattern as vcsRequest). No rail entry, palette command or shortcut opens
  // it since #219; flag, openers, tab state and panel stay.
  const [activityOpen, setActivityOpen] = useState(false);
  const [activityTab, setActivityTab] = useState<ActivityHealthTab>("activity");
  const [activityRequest, setActivityRequest] = useState<{
    token: number;
    tab: ActivityHealthTab;
  } | null>(null);
  // Tasks screen: a workspace-scoped navigation destination — a sidebar rail
  // entry opening an overlay over the still-mounted panes (palette +
  // Ctrl+Shift+T open it too; no panel request token — the panel reloads
  // from the backend).
  const [tasksOpen, setTasksOpen] = useState(false);
  // Code Audit screen: a workspace-scoped destination like
  // Settings/Library/VCS/Activity — an overlay over the still-mounted panes
  // (the panel scans manually on Run only, never on mount). No rail entry,
  // palette command or shortcut opens it since #219; the flag, opener and
  // panel stay so re-adding one is a one-line change.
  const [auditOpen, setAuditOpen] = useState(false);
  // Debt backlog screen: a workspace-scoped destination opening an overlay
  // over the still-mounted panes (the panel lists from the backend on mount
  // and imports manually). No rail entry or palette command opens it since
  // #219; flag, opener and panel stay.
  const [debtOpen, setDebtOpen] = useState(false);
  // Issues & PRs screen: a workspace-scoped destination opening an overlay
  // over the still-mounted panes (the panel loads from the backend on mount
  // and on every kind/state change, plus manual Refresh). No rail entry,
  // palette command or shortcut opens it since #219; flag, opener and panel
  // stay.
  const [ghOpen, setGhOpen] = useState(false);
  // Docs screen: a workspace-scoped destination opening an overlay over the
  // still-mounted panes (the migration section loads its version spine from
  // the backend on mount, everything else renders from code-backed
  // registries). No rail entry or palette command opens it since #219;
  // flag, opener and panel stay.
  const [docsOpen, setDocsOpen] = useState(false);
  // Release screen: a workspace-scoped destination opening an overlay over
  // the still-mounted panes (the panel loads from the backend on mount and on
  // manual Refresh). No rail entry or palette command opens it since #219;
  // flag, opener and panel stay.
  const [releaseOpen, setReleaseOpen] = useState(false);

  const openSettings = useCallback((section?: PaletteSettingsSection) => {
    setSettingsSection(section ?? "appearance");
    setSettingsOpen(true);
    setLibraryOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);
  const openLibrary = useCallback(() => {
    // No rail entry or palette command calls this since #219; the opener and
    // the panel stay, so re-adding either restores the destination.
    // A fresh entry to the library opens the list, not a previously staged edit.
    setPromptToEditId(null);
    setLibraryOpen(true);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
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
  const openActivity = useCallback((tab: ActivityHealthTab = "activity") => {
    // Deep links always retarget the tab (even when already open): the
    // token identifies the request so re-renders never replay it.
    setActivityTab(tab);
    setActivityRequest({ token: Date.now(), tab });
    setActivityOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
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
    setTasksOpen(false);
    setAuditOpen(false);
    setDebtOpen(false);
    setDocsOpen(false);
    setGhOpen(false);
    setReleaseOpen(false);
  }, []);

  // Overlay-exit contract shared by tab mutations and palette commands:
  // switching surface closes every overlay (same as clicking a tab or sidebar
  // row — overlays never sit above a switched tab).
  const closeAll = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    setActivityOpen(false);
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
      openSettings,
      closeSettings,
      openLibrary,
      closeLibrary,
      openVcs,
      closeVcs,
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
      openSettings,
      closeSettings,
      openLibrary,
      closeLibrary,
      openVcs,
      closeVcs,
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
