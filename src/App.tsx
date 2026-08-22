import { useState } from "react";

import ConversationView from "./components/ConversationView";
import EmptyState from "./components/EmptyState";
import NexoraMark from "./components/NexoraMark";
import PromptLibraryView from "./components/PromptLibraryView";
import SettingsView from "./components/SettingsView";
import Sidebar from "./components/Sidebar";
import type { Conversation } from "./lib/tauri";
import { useConversations } from "./lib/useConversations";
import { useProviders } from "./lib/useProviders";

interface MainContentProps {
  selected: Conversation | undefined;
  hasConversations: boolean;
  selectedProvider: string | null;
  selectedModel: string | null;
  draft: string;
  setDraft: (value: string) => void;
  onOpenSettings: () => void;
  onMessageSent: () => void;
}

function MainContent({
  selected,
  hasConversations,
  selectedProvider,
  selectedModel,
  draft,
  setDraft,
  onOpenSettings,
  onMessageSent,
}: MainContentProps) {
  if (!selected) {
    if (!hasConversations) {
      return <EmptyState />;
    }
    // Conversations exist but none is open.
    return (
      <div className="nex-main-placeholder">
        <NexoraMark className="nex-placeholder-mark" width={24} height={24} />
        <p className="nex-placeholder-title">No conversation selected</p>
        <p className="nex-text-tertiary nex-text-sm">
          Choose a conversation from the sidebar to open it.
        </p>
      </div>
    );
  }

  const isArchived = selected.status === "archived";
  return (
    <>
      <header className="nex-main-header">
        <h2 className="nex-main-title">
          {selected.title}
          {isArchived && (
            <span className="nex-main-title-badge" aria-label="Archived">
              Archived
            </span>
                    )}
        </h2>
      </header>
      <ConversationView
        conversationId={selected.id}
        selectedProvider={selectedProvider}
        selectedModel={selectedModel}
        onOpenSettings={onOpenSettings}
        onMessageSent={onMessageSent}
        draft={draft}
        setDraft={setDraft}
      />
    </>
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
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // The composer draft is lifted here so the Prompt Library screen can stage a
  // prompt's content into the active conversation's input field (FR-007), and
  // so it can be reset when the conversation selection changes.
  const [draft, setDraft] = useState("");
  const [libraryOpen, setLibraryOpen] = useState(false);
  // Prompt Library navigation state. When set from a search result (FR-009), the
  // Prompt Library screen opens with that prompt's existing Edit modal.
  const [promptToEditId, setPromptToEditId] = useState<number | null>(null);

  const handleNewConversation = async () => {
    const id = await create();
    if (id !== null) {
      setSelectedId(id);
      setDraft("");
      setLibraryOpen(false);
    }
  };

  const handleSelect = (id: number) => {
    setSelectedId(id);
    setDraft("");
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

  // FR-007 "Use": stage the prompt's content into the active conversation's
  // composer, then return to the conversation so the staged text is visible.
  const handleUsePrompt = (content: string) => {
    setDraft(content);
    setLibraryOpen(false);
  };

  const selected = conversations.find((c) => c.id === selectedId);
  // A prompt can only be staged when a conversation is open.
  const hasActiveConversation = selected != null;

  return (
    <div className="nex-app">
      <Sidebar
        conversations={conversations}
        loading={loading}
        error={error}
        creating={creating}
        busy={working}
        selectedId={selectedId}
        onSelect={handleSelect}
        onNewConversation={handleNewConversation}
        onRetry={reload}
        onOpenSettings={openSettings}
        libraryActive={libraryOpen}
        onOpenPromptLibrary={openLibrary}
        onSelectPrompt={handleSelectPrompt}
        onRename={rename}
        onArchive={(id) => void archive(id)}
        onRestore={(id) => void restore(id)}
        onDelete={(id) => void remove(id)}
      />
      <div className="nex-main">
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
            onClose={() => setSettingsOpen(false)}
          />
        ) : (
          <MainContent
            selected={selected}
            hasConversations={conversations.length > 0}
            selectedProvider={providers.selectedProvider}
            selectedModel={providers.selectedModel}
            draft={draft}
            setDraft={setDraft}
            onOpenSettings={openSettings}
            onMessageSent={() => void reload()}
          />
        )}
      </div>
    </div>
  );
}

export default App;

