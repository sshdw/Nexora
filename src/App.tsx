import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import CommandPalette from "./components/CommandPalette";
import ConversationTabs, { type TabEntry } from "./components/ConversationTabs";
import ConversationView from "./components/ConversationView";
import EmptyState from "./components/EmptyState";
import { ExportIcon } from "./components/icons";
import { ExportModal, ImportModal } from "./components/ImportExportModals";
import M3Button from "./components/M3Button";
import M3IconButton from "./components/M3IconButton";
import M3Toolbar from "./components/M3Toolbar";
import NexoraMark from "./components/NexoraMark";
import PromptLibraryView from "./components/PromptLibraryView";
import SettingsView from "./components/SettingsView";
import Sidebar from "./components/Sidebar";
import VersionControlPanel from "./components/VersionControlPanel";
import WorkspaceChip from "./components/WorkspaceChip";
import type { Conversation } from "./lib/tauri";
import {
  buildCommands,
  recordUse,
  type PaletteCommand,
  type PaletteSettingsSection,
  type PaletteVcsRequest,
} from "./lib/commands";
import { useAppearance } from "./lib/useAppearance";
import { useConversations } from "./lib/useConversations";
import { useConversationTabs } from "./lib/useConversationTabs";
import { useImportExport } from "./lib/useImportExport";
import { useProviders } from "./lib/useProviders";
import { useSpendLimit } from "./lib/useSpendLimit";
import { useWorkspace } from "./lib/useWorkspace";

interface ConversationPaneProps {
  conversation: Conversation;
  selectedProvider: string | null;
  selectedModel: string | null;
  draft: string;
  setDraft: (value: string) => void;
  onOpenSettings: () => void;
  onMessageSent: () => void;
  onExport: (id: number) => void;
  workspaceRoot: string | null;
  workspaceLoading: boolean;
  /** Secondary (split) pane: the header offers "Close split". */
  secondary: boolean;
  /** Primary pane: whether the "Split" opener is available. */
  splitAvailable: boolean;
  splitOpen: boolean;
  onToggleSplit: () => void;
}

function ConversationPane({
  conversation,
  selectedProvider,
  selectedModel,
  draft,
  setDraft,
  onOpenSettings,
  onMessageSent,
  onExport,
  workspaceRoot,
  workspaceLoading,
  secondary,
  splitAvailable,
  splitOpen,
  onToggleSplit,
}: ConversationPaneProps) {
  const isArchived = conversation.status === "archived";
  return (
    <section
      className={"nex-pane" + (secondary ? " nex-pane--secondary" : "")}
      aria-label={secondary ? `Split: ${conversation.title}` : conversation.title}
    >
      <header className="nex-pane-header">
        <h2 className="nex-main-title">
          <span className="nex-main-title-text">{conversation.title}</span>
          {isArchived && (
            <span className="nex-main-title-badge" aria-label="Archived">
              Archived
            </span>
          )}
        </h2>
        <M3Toolbar
          label={`Actions for ${conversation.title}`}
          className="nex-main-header-actions"
        >
          <WorkspaceChip root={workspaceRoot} loading={workspaceLoading} />
          <M3Button
            variant="quiet"
            size="sm"
            onClick={onToggleSplit}
            aria-pressed={splitOpen}
            disabled={secondary ? false : !splitAvailable && !splitOpen}
            title={
              secondary
                ? "Close the split pane (Alt+S)"
                : "Show a second conversation beside this one (Alt+S)"
            }
          >
            {secondary ? "Close split" : "Split"}
          </M3Button>
          <M3IconButton
            label="Export conversation"
            onClick={() => onExport(conversation.id)}
          >
            <ExportIcon />
          </M3IconButton>
        </M3Toolbar>
      </header>
      <div className="nex-pane-body">
        {/* Keyed by conversation id: each pane owns independent hook state
            (messages, attachments, agent runs, scroll position) so the two
            panes can never clobber each other. Drafts are likewise keyed per
            conversation in App (see `drafts`), never shared. */}
        <ConversationView
          key={conversation.id}
          conversationId={conversation.id}
          selectedProvider={selectedProvider}
          selectedModel={selectedModel}
          onOpenSettings={onOpenSettings}
          onMessageSent={onMessageSent}
          draft={draft}
          setDraft={setDraft}
        />
      </div>
    </section>
  );
}

function App() {
  const {
    conversations,
    loading,
    error,
    creating,
    working,
    reload,
    create,
    rename,
    archive,
    restore,
    remove,
  } = useConversations();
  // Single provider/model/credential store shared by the Settings view and the
  // conversation composer, so the selection made in Settings is the selection
  // used when sending (FR-004).
  const providers = useProviders();
  // Appearance preference is loaded once here so the persisted theme applies
  // at startup, not only while Settings is open (FR-012 persistence).
  const appearance = useAppearance();
  // Multi-conversation tabs + split: extension of the former single
  // `selectedId` — open tab ids, the primary pane id, and the optional
  // secondary split-pane id (see useConversationTabs.ts).
  const tabs = useConversationTabs();
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Per-conversation composer drafts, keyed by conversation id: each tab (and
  // each split pane) owns its draft, so typing in one pane never clobbers the
  // other and switching tabs preserves in-progress input (tab state survives
  // conversation switch). The Prompt Library stages into the active pane's
  // draft (FR-007).
  const [drafts, setDrafts] = useState<Record<number, string>>({});
  const [libraryOpen, setLibraryOpen] = useState(false);
  // Version Control screen (read-only git base): a workspace-scoped
  // navigation destination like Settings/Library — a sidebar rail entry
  // (not a header action: the header is per-conversation, VCS is per
  // workspace) opening an overlay over the still-mounted panes.
  const [vcsOpen, setVcsOpen] = useState(false);
  // Prompt Library navigation state. When set from a search result (FR-009), the
  // Prompt Library screen opens with that prompt's existing Edit modal.
  const [promptToEditId, setPromptToEditId] = useState<number | null>(null);
  // Import/Export (FR-010, FR-011): the conversation being exported (opens the
  // export modal when set) and whether the import modal is shown.
  const io = useImportExport();
  const [exportTargetId, setExportTargetId] = useState<number | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  // Agent workspace folder (1.3.0): sidebar picker + header indicator share
  // this store; Settings reads it read-only.
  const workspace = useWorkspace();
  // Per-run spend guard (micro-USD budget): Settings edits it, new agent runs
  // read it backend-side.
  const spendLimit = useSpendLimit();
  // Zen reading mode: chromeless (sidebar + tab strip + pane headers hidden
  // via .nex-zen), Esc exits. Session-only, like tab state.
  const [zen, setZen] = useState(false);
  // Command palette (Ctrl+K): launcher over the registry in lib/commands.ts.
  const [paletteOpen, setPaletteOpen] = useState(false);
  // A palette-chosen command runs after the palette unmounts (see the
  // deferred-run effect below), so focus moves land on live targets.
  const [pendingPaletteRun, setPendingPaletteRun] = useState<(() => void) | null>(null);
  // Palette deep-link targets: the settings section to show and the VCS
  // panel request to raise when those overlays open from the palette.
  const [settingsSection, setSettingsSection] =
    useState<PaletteSettingsSection>("appearance");
  const [vcsRequest, setVcsRequest] = useState<{
    token: number;
    action: PaletteVcsRequest;
  } | null>(null);
  const mainRef = useRef<HTMLDivElement>(null);

  const draftFor = useCallback(
    (id: number) => drafts[id] ?? "",
    [drafts],
  );
  const setDraftFor = useCallback(
    (id: number) => (value: string) =>
      setDrafts((prev) => (prev[id] === value ? prev : { ...prev, [id]: value })),
    [],
  );

  // Drop tabs for conversations that no longer exist (deleted). Archived
  // conversations stay listed (ConversationList.tsx:69-70 groups them), so
  // archiving never closes a tab.
  //
  // Retain-vs-drop for drafts: drafts of closed/pruned tabs are dropped,
  // not retained. Retaining would grow the map without bound across a
  // session and resurrect stale input if the id is ever reused; dropping
  // is safe because a closed tab has no visible composer to preserve.
  // Syncing to openIds (rather than hooking every close call site) covers
  // tab close, Alt+W, prune, and backend delete uniformly.
  useEffect(() => {
    const open = new Set(tabs.openIds);
    setDrafts((prev) => {
      const keys = Object.keys(prev);
      if (keys.every((key) => open.has(Number(key)))) return prev;
      const next: Record<number, string> = {};
      for (const [key, value] of Object.entries(prev)) {
        if (open.has(Number(key))) next[Number(key)] = value;
      }
      return next;
    });
  }, [tabs.openIds]);

  useEffect(() => {
    tabs.prune(new Set(conversations.map((c) => c.id)));
    // `tabs.prune` identity follows tab state; running the validation pass on
    // every tab change is harmless (early return when all ids are valid).
  }, [conversations, tabs]);

  // After a successful import the conversation list is reloaded from the
  // backend (single source of truth) and the new conversation is opened.
  const handleImported = async (newId: number) => {
    await reload();
    tabs.open(newId);
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
  };

  // Leading-edge guard for conversation creation: synchronous rapid clicks on
  // "New Conversation" all land before the button's disabled state re-renders,
  // so re-entrant calls are dropped here until the in-flight create resolves.
  const creatingInFlight = useRef(false);
  const handleNewConversation = async () => {
    if (creatingInFlight.current) return;
    creatingInFlight.current = true;
    try {
      const id = await create();
      if (id !== null) {
        tabs.open(id);
        setLibraryOpen(false);
        setVcsOpen(false);
      }    } finally {
      creatingInFlight.current = false;
    }
  };

  const handleSelect = (id: number) => {
    tabs.open(id);
    setLibraryOpen(false);
    // Opening a conversation (including from a search result) leaves Settings.
    setSettingsOpen(false);
    setVcsOpen(false);
  };

  const openSettings = (section?: PaletteSettingsSection) => {
    setSettingsSection(section ?? "appearance");
    setSettingsOpen(true);
    setLibraryOpen(false);
    setVcsOpen(false);
  };
  const openLibrary = () => {
    // A fresh entry to the library opens the list, not a previously staged edit.
    setPromptToEditId(null);
    setLibraryOpen(true);
    setSettingsOpen(false);
    setVcsOpen(false);
  };
  const closeLibrary = () => {
    setLibraryOpen(false);
    setPromptToEditId(null);
  };
  const openVcs = (request?: PaletteVcsRequest) => {
    setVcsRequest(request ? { token: Date.now(), action: request } : null);
    setVcsOpen(true);
    setLibraryOpen(false);
    setSettingsOpen(false);
  };
  const closeVcs = () => {
    setVcsOpen(false);
  };

  // Open a prompt found by search: show the Prompt Library and open the selected
  // prompt (not merely the first prompt) in its existing Edit modal (FR-009).
  const handleSelectPrompt = (promptId: number) => {
    setSettingsOpen(false);
    setLibraryOpen(true);
    setPromptToEditId(promptId);
    setVcsOpen(false);
  };

  // FR-007 "Use": stage the prompt's content into the active pane's composer,
  // then return to the conversation so the staged text is visible.
  const handleUsePrompt = (content: string) => {
    if (tabs.activeId !== null) {
      setDrafts((prev) => ({ ...prev, [tabs.activeId as number]: content }));
    }
    setLibraryOpen(false);
  };

  // Split toggle (Alt+S, tab strip + pane headers): close the secondary pane
  // when open; otherwise split the most-recently opened other tab beside the
  // active one. Single-tab state cannot split (button disables).
  const handleToggleSplit = useCallback(() => {
    if (tabs.splitId !== null) {
      tabs.closeSplit();
      return;
    }
    if (tabs.activeId === null) return;
    const other = [...tabs.openIds].reverse().find((id) => id !== tabs.activeId);
    if (other !== undefined) tabs.openInSplit(other);
  }, [tabs]);

  const toggleZen = useCallback(() => {
    setZen((prev) => !prev);
  }, []);

  // Overlay-exit contract shared by tab mutations and palette commands:
  // switching surface closes Settings/Library/VCS (same as clicking a
  // tab or sidebar row — overlays never sit above a switched tab).
  const closeOverlays = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
  }, []);

  // Focus the active pane's composer (palette "Focus message input"):
  // overlays close first, then focus lands after the re-render commits.
  const focusComposer = useCallback(() => {
    setLibraryOpen(false);
    setSettingsOpen(false);
    setVcsOpen(false);
    window.setTimeout(() => {
      document
        .querySelector<HTMLTextAreaElement>(".nex-composer-input")
        ?.focus();
    }, 0);
  }, []);

  const exportActiveConversation = useCallback(() => {
    if (tabs.activeId !== null) setExportTargetId(tabs.activeId);
  }, [tabs.activeId]);

  // Entering zen keeps focus in content (the main landmark); leaving zen
  // returns focus to the active tab so keyboard users resume where they
  // were. Reduced-motion users get the same instant chrome swap — zen never
  // animates, only toggles visibility.
  useEffect(() => {
    if (zen) {
      mainRef.current?.focus();
    } else {
      document
        .querySelector<HTMLElement>(".nex-tab.is-active .nex-tab-label")
        ?.focus();
    }
  }, [zen]);

  // Global tab/split/zen shortcuts (full map documented in
  // useConversationTabs.ts). Guarded: typing targets (inputs, textareas,
  // selects, contentEditable, rename fields) keep their keys — only the
  // window-level combos below are intercepted, and none collides with the
  // existing unmodified keys (composer Enter, rename/modal/toolbar Escape,
  // segmented arrows). An open dialog owns Escape, so zen yields to modals.
  useEffect(() => {
    const isTypingTarget = (target: EventTarget | null) => {
      if (!(target instanceof HTMLElement)) return false;
      if (target.isContentEditable) return true;
      const tag = target.tagName;
      return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
    };
    const dialogOpen = () => document.querySelector('[role="dialog"]') !== null;

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && zen && !dialogOpen()) {
        event.preventDefault();
        setZen(false);
        return;
      }
      // Tab mutations surface the conversation: same overlay-exit contract
      // as clicking a tab or sidebar row (Settings/Library/VCS never sit
      // above a switched tab) — see closeOverlays above.
      if (event.ctrlKey && !event.altKey && !event.metaKey) {
        if (event.key === "Tab") {
          event.preventDefault();
          closeOverlays();
          if (event.shiftKey) tabs.prev();
          else tabs.next();
          return;
        }
        if (event.key === "PageDown") {
          event.preventDefault();
          closeOverlays();
          tabs.next();
          return;
        }
        if (event.key === "PageUp") {
          event.preventDefault();
          closeOverlays();
          tabs.prev();
          return;
        }
      }
      if (event.altKey && !event.ctrlKey && !event.metaKey) {
        // Non-character shortcuts run even from typing targets: Alt+W/S/Z
        // produce no text in inputs, so close/split/zen never steal composer
        // input. Alt+digits stay guarded — they can compose characters on
        // some layouts, so the typing-target check still owns that branch.
        switch (event.key.toLowerCase()) {
          case "w":
            event.preventDefault();
            closeOverlays();
            if (tabs.activeId !== null) tabs.close(tabs.activeId);
            break;
          case "s":
            event.preventDefault();
            handleToggleSplit();
            break;
          case "z":
            event.preventDefault();
            if (!dialogOpen()) toggleZen();
            break;
          default: {
            if (isTypingTarget(event.target)) return;
            if (event.key >= "1" && event.key <= "9") {
              event.preventDefault();
              closeOverlays();
              const position =
                event.key === "9" ? tabs.openIds.length - 1 : Number(event.key) - 1;
              tabs.jumpTo(position);
            }
          }
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [zen, tabs, handleToggleSplit, toggleZen, closeOverlays]);

  // Command-palette toggle (Ctrl+K, with Ctrl+P as a collision-free alias:
  // the existing Ctrl map owns only Tab/PageUp/PageDown — see
  // useConversationTabs.ts:12-28 — and no other feature binds Ctrl+P).
  // Guarded like the Alt+digit branch above: typing targets (inputs,
  // textareas, selects, contentEditable, rename fields) keep their keys,
  // and an open dialog owns the keyboard (no stacked dialogs). The
  // palette input autofocuses on open; ModalShell restores focus to the
  // invoker on close.
  useEffect(() => {
    const isTypingTarget = (target: EventTarget | null) => {
      if (!(target instanceof HTMLElement)) return false;
      if (target.isContentEditable) return true;
      const tag = target.tagName;
      return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.ctrlKey || event.altKey || event.metaKey) return;
      const key = event.key.toLowerCase();
      if (key !== "k" && key !== "p") return;
      if (isTypingTarget(event.target)) return;
      if (document.querySelector('[role="dialog"]') !== null) return;
      event.preventDefault();
      setPaletteOpen((prev) => !prev);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // The palette registry: every entry wraps the existing UI handler, so
  // invoking from the palette === clicking its UI equivalent.
  const paletteCommands = useMemo(
    () =>
      buildCommands({
        goConversations: closeOverlays,
        openSettings,
        openLibrary,
        openVcs,
        newConversation: () => void handleNewConversation(),
        openImport: () => setImportOpen(true),
        exportActive: exportActiveConversation,
        focusComposer,
        tabNext: () => {
          closeOverlays();
          tabs.next();
        },
        tabPrev: () => {
          closeOverlays();
          tabs.prev();
        },
        tabCloseActive: () => {
          closeOverlays();
          if (tabs.activeId !== null) tabs.close(tabs.activeId);
        },
        toggleSplit: handleToggleSplit,
        toggleZen,
        jumpToTab: (index: number) => {
          closeOverlays();
          tabs.jumpTo(index);
        },
        tabCount: () => tabs.openIds.length,
      }),
    [
      closeOverlays,
      openSettings,
      openLibrary,
      openVcs,
      handleNewConversation,
      exportActiveConversation,
      focusComposer,
      tabs,
      handleToggleSplit,
      toggleZen,
    ],
  );

  // Choosing a command closes the palette first; the command itself runs
  // after the palette unmounts (post-commit, inert lifted), so focus moves
  // and overlay switches land on live targets.
  const handlePaletteRun = useCallback((command: PaletteCommand) => {
    recordUse(command.id);
    setPendingPaletteRun(() => command.run);
    setPaletteOpen(false);
  }, []);

  useEffect(() => {
    if (!paletteOpen && pendingPaletteRun !== null) {
      const run = pendingPaletteRun;
      setPendingPaletteRun(null);
      run();
    }
  });

  const activeConversation =
    tabs.activeId !== null
      ? conversations.find((c) => c.id === tabs.activeId)
      : undefined;
  const splitConversation =
    tabs.splitId !== null
      ? conversations.find((c) => c.id === tabs.splitId)
      : undefined;
  // A tab can outlive its list entry briefly (delete reload in flight); fall
  // back to the id so the pane keeps its keyed state until prune lands.
  const resolveTab = (id: number): TabEntry => {
    const found = conversations.find((c) => c.id === id);
    return {
      id,
      title: found?.title ?? "Conversation",
      archived: found?.status === "archived",
    };
  };
  // A prompt can only be staged when a conversation is open.
  const hasActiveConversation = activeConversation != null;
  const showOverlays = libraryOpen || settingsOpen || vcsOpen;
  // The split grid stays mounted in zen (the secondary pane hides via
  // .nex-zen CSS, same technique as the zen chrome rules) so both
  // ConversationView instances survive entering/exiting zen. Split
  // open/close itself still remounts panes (tree position changes between
  // single-pane and grid) — accepted: toggling split is an explicit layout
  // change, not a transient overlay.
  const splitVisible =
    !showOverlays && tabs.splitId !== null && splitConversation !== undefined;

  // Live pane content, extracted so Settings/Library overlays render above
  // still-mounted panes (hidden via CSS, never unmounted) instead of
  // replacing them — see the overlay branch below.
  const conversationContent =
    activeConversation === undefined ? (
      tabs.openIds.length === 0 ? (
        // First-run empty state: logo + heading + supporting line only. The
        // sidebar's New Conversation row is the single creation CTA (one
        // primary per region) — no duplicate CTA here (contract §Shell).
        <EmptyState />
      ) : (
        // Tabs exist but the active id has no list entry yet (reload in
        // flight after delete/import) — hold the calm placeholder.
        <div className="nex-main-placeholder nex-empty-enter">
          <span className="nex-placeholder-mark-wrap" aria-hidden="true">
            <NexoraMark className="nex-placeholder-mark" width={30} height={30} />
          </span>
          <p className="nex-placeholder-title">No conversation selected</p>
          <p className="nex-placeholder-text">
            Choose a conversation from the sidebar, or create one with New Conversation.
          </p>
        </div>
      )
    ) : splitVisible && splitConversation ? (
      <div className="nex-split">
        <ConversationPane
          conversation={activeConversation}
          selectedProvider={providers.selectedProvider}
          selectedModel={providers.selectedModel}
          draft={draftFor(activeConversation.id)}
          setDraft={setDraftFor(activeConversation.id)}
          onOpenSettings={openSettings}
          onMessageSent={() => void reload()}
          onExport={setExportTargetId}
          workspaceRoot={workspace.root}
          workspaceLoading={workspace.loading}
          secondary={false}
          splitAvailable={tabs.openIds.length > 1}
          splitOpen
          onToggleSplit={handleToggleSplit}
        />
        <ConversationPane
          conversation={splitConversation}
          selectedProvider={providers.selectedProvider}
          selectedModel={providers.selectedModel}
          draft={draftFor(splitConversation.id)}
          setDraft={setDraftFor(splitConversation.id)}
          onOpenSettings={openSettings}
          onMessageSent={() => void reload()}
          onExport={setExportTargetId}
          workspaceRoot={workspace.root}
          workspaceLoading={workspace.loading}
          secondary
          splitAvailable={false}
          splitOpen
          onToggleSplit={handleToggleSplit}
        />
      </div>
    ) : (
      <ConversationPane
        conversation={activeConversation}
        selectedProvider={providers.selectedProvider}
        selectedModel={providers.selectedModel}
        draft={draftFor(activeConversation.id)}
        setDraft={setDraftFor(activeConversation.id)}
        onOpenSettings={openSettings}
        onMessageSent={() => void reload()}
        onExport={setExportTargetId}
        workspaceRoot={workspace.root}
        workspaceLoading={workspace.loading}
        secondary={false}
        splitAvailable={tabs.openIds.length > 1}
        splitOpen={false}
        onToggleSplit={handleToggleSplit}
      />
    );

  return (
    <div className={"nex-app" + (zen ? " nex-zen" : "")}>
      <a className="nex-skip-link" href="#nex-main-content">
        Skip to main content
      </a>
      <Sidebar
        conversations={conversations}
        loading={loading}
        error={error}
        creating={creating}
        busy={working}
        selectedId={tabs.activeId}
        onSelect={handleSelect}
        onExport={setExportTargetId}
        onNewConversation={() => void handleNewConversation()}
        onRetry={reload}
        onOpenSettings={openSettings}
        libraryActive={libraryOpen}
        onOpenPromptLibrary={openLibrary}
        vcsActive={vcsOpen}
        onOpenVersionControl={openVcs}
        onSelectPrompt={handleSelectPrompt}
        onImport={() => setImportOpen(true)}
        onRename={rename}
        onArchive={(id) => void archive(id)}
        onRestore={(id) => void restore(id)}
        onDelete={(id) => void remove(id)}
        workspace={workspace}
      />
      <div className="nex-main" id="nex-main-content" tabIndex={-1} ref={mainRef}>
        {!zen && !showOverlays && (
          <ConversationTabs
            tabs={tabs.openIds.map(resolveTab)}
            activeId={tabs.activeId}
            splitId={tabs.splitId}
            onActivate={(id) => {
              tabs.activate(id);
              setLibraryOpen(false);
              setSettingsOpen(false);
              setVcsOpen(false);
            }}
            onClose={tabs.close}
            onNewConversation={() => void handleNewConversation()}
            creating={creating}
            zen={zen}
            onToggleZen={toggleZen}
            splitOpen={tabs.splitId !== null}
            onToggleSplit={handleToggleSplit}
          />
        )}
        {showOverlays ? (
          <>
            {/* Live panes stay mounted under Settings/Library: hidden via
                CSS (display:none drops them from layout, tab order, and the
                a11y tree while the overlay owns the view) but never
                unmounted, so ConversationView instances keep scroll position
                and in-flight agent-run state. The tab strip is chrome-only
                (no live state) and stays conditionally rendered. */}
            <div className="nex-main-hidden" aria-hidden="true">
              {conversationContent}
            </div>
            {libraryOpen ? (
              <PromptLibraryView
                onClose={closeLibrary}
                hasActiveConversation={hasActiveConversation}
                onUse={handleUsePrompt}
                initialEditId={promptToEditId}
              />
            ) : vcsOpen ? (
              <VersionControlPanel onClose={closeVcs} request={vcsRequest} />
            ) : (
              <SettingsView
                store={providers}
                appearance={appearance}
                workspaceRoot={workspace.root}
                workspaceLoading={workspace.loading}
                spendLimit={spendLimit}
                onClose={() => setSettingsOpen(false)}
                onDataCleared={() => void reload()}
                initialSection={settingsSection}
              />
            )}
          </>
        ) : (
          conversationContent
        )}
      </div>
      {/* Polite announcement of zen enter/exit: the chrome swap is instant
          with no visual transition, so screen-reader users get the mode
          change (plus how to leave) here; focus moves alongside (see the
          zen effect above). */}
      <div className="nex-sr-only" aria-live="polite">
        {zen
          ? "Zen reading mode on. Press Escape to exit."
          : "Zen reading mode off."}
      </div>
      {zen && (
        <div className="nex-zen-exit">
          <M3Button
            variant="quiet"
            size="sm"
            onClick={toggleZen}
            title="Exit chromeless reading mode (Esc)"
          >
            Exit zen · Esc
          </M3Button>
        </div>
      )}
      {exportTargetId !== null &&
        (() => {
          const target = conversations.find((c) => c.id === exportTargetId);
          if (!target) return null;
          return (
            <ExportModal
              conversationId={target.id}
              conversationTitle={target.title}
              store={io}
              onClose={() => {
                setExportTargetId(null);
                io.clearStatus();
              }}
            />
          );
        })()}
      {importOpen && (
        <ImportModal
          store={io}
          onImported={(newId) => void handleImported(newId)}
          onClose={() => {
            setImportOpen(false);
            io.clearStatus();
          }}
        />
      )}
      {paletteOpen && (
        <CommandPalette
          commands={paletteCommands}
          onClose={() => setPaletteOpen(false)}
          onRun={handlePaletteRun}
        />
      )}
    </div>
  );
}

export default App;
