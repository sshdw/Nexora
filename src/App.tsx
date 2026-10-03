import { useCallback, useEffect, useRef, useState } from "react";

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
import WorkspaceChip from "./components/WorkspaceChip";
import type { Conversation } from "./lib/tauri";
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
      className="nex-pane"
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
  useEffect(() => {
    tabs.prune(new Set(conversations.map((c) => c.id)));
    // `tabs.prune` identity follows tab state; running the validation pass on
    // every tab change is harmless (early return when all ids are valid).
  }, [conversations]);

  // After a successful import the conversation list is reloaded from the
  // backend (single source of truth) and the new conversation is opened.
  const handleImported = async (newId: number) => {
    await reload();
    tabs.open(newId);
    setLibraryOpen(false);
    setSettingsOpen(false);
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
      }
    } finally {
      creatingInFlight.current = false;
    }
  };

  const handleSelect = (id: number) => {
    tabs.open(id);
    setLibraryOpen(false);
    // Opening a conversation (including from a search result) leaves Settings.
    setSettingsOpen(false);
  };

  const openSettings = () => {
    setSettingsOpen(true);
    setLibraryOpen(false);
  };
  const openLibrary = () => {
    // A fresh entry to the library opens the list, not a previously staged edit.
    setPromptToEditId(null);
    setLibraryOpen(true);
    setSettingsOpen(false);
  };
  const closeLibrary = () => {
    setLibraryOpen(false);
    setPromptToEditId(null);
  };

  // Open a prompt found by search: show the Prompt Library and open the selected
  // prompt (not merely the first prompt) in its existing Edit modal (FR-009).
  const handleSelectPrompt = (promptId: number) => {
    setSettingsOpen(false);
    setLibraryOpen(true);
    setPromptToEditId(promptId);
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
      // as clicking a tab or sidebar row (Settings/Library never sit above
      // a switched tab).
      const leaveOverlays = () => {
        setLibraryOpen(false);
        setSettingsOpen(false);
      };
      if (event.ctrlKey && !event.altKey && !event.metaKey) {
        if (event.key === "Tab") {
          event.preventDefault();
          leaveOverlays();
          if (event.shiftKey) tabs.prev();
          else tabs.next();
          return;
        }
        if (event.key === "PageDown") {
          event.preventDefault();
          leaveOverlays();
          tabs.next();
          return;
        }
        if (event.key === "PageUp") {
          event.preventDefault();
          leaveOverlays();
          tabs.prev();
          return;
        }
      }
      if (event.altKey && !event.ctrlKey && !event.metaKey) {
        if (isTypingTarget(event.target)) return;
        if (event.key >= "1" && event.key <= "9") {
          event.preventDefault();
          leaveOverlays();
          const position = event.key === "9" ? tabs.openIds.length - 1 : Number(event.key) - 1;
          tabs.jumpTo(position);
          return;
        }
        switch (event.key.toLowerCase()) {
          case "w":
            event.preventDefault();
            leaveOverlays();
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
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [zen, tabs, handleToggleSplit, toggleZen]);

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

  const showOverlays = libraryOpen || settingsOpen;
  const splitVisible =
    !showOverlays && !zen && tabs.splitId !== null && splitConversation !== undefined;

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
        {libraryOpen ? (
          <PromptLibraryView
            onClose={closeLibrary}
            hasActiveConversation={hasActiveConversation}
            onUse={handleUsePrompt}
            initialEditId={promptToEditId}
          />
        ) : settingsOpen ? (
          <SettingsView
            store={providers}
            appearance={appearance}
            workspaceRoot={workspace.root}
            workspaceLoading={workspace.loading}
            spendLimit={spendLimit}
            onClose={() => setSettingsOpen(false)}
            onDataCleared={() => void reload()}
          />
        ) : activeConversation === undefined ? (
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
        )}
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
    </div>
  );
}

export default App;
