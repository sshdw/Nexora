import { useState } from "react";

import ConversationView from "./components/ConversationView";
import EmptyState from "./components/EmptyState";
import NexoraMark from "./components/NexoraMark";
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
  onOpenSettings: () => void;
}

function MainContent({
  selected,
  hasConversations,
  selectedProvider,
  selectedModel,
  onOpenSettings,
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
    reload,
    create,
  } = useConversations();
  // Single provider/model/credential store shared by the Settings view and the
  // conversation composer, so the selection made in Settings is the selection
  // used when sending (FR-004).
  const providers = useProviders();
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);

  const handleNewConversation = async () => {
    const id = await create();
    if (id !== null) {
      setSelectedId(id);
      setSettingsOpen(false);
    }
  };

  const selected = conversations.find((c) => c.id === selectedId);

  return (
    <div className="nex-app">
      <Sidebar
        conversations={conversations}
        loading={loading}
        error={error}
        creating={creating}
        selectedId={selectedId}
        onSelect={setSelectedId}
        onNewConversation={handleNewConversation}
        onRetry={reload}
        onOpenSettings={() => setSettingsOpen(true)}
      />
      <div className="nex-main">
        {settingsOpen ? (
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
            onOpenSettings={() => setSettingsOpen(true)}
          />
        )}
      </div>
    </div>
  );
}

export default App;

