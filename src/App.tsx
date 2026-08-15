import { useState } from "react";

import EmptyState from "./components/EmptyState";
import NexoraMark from "./components/NexoraMark";
import Sidebar from "./components/Sidebar";
import type { Conversation } from "./lib/tauri";
import { useConversations } from "./lib/useConversations";

interface MainContentProps {
  selected: Conversation | undefined;
  hasConversations: boolean;
}

function MainContent({ selected, hasConversations }: MainContentProps) {
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
      <div className="nex-main-body">
        <div className="nex-conversation-placeholder">
          <NexoraMark
            className="nex-conversation-empty-mark"
            width={24}
            height={24}
          />
          <h2 className="nex-conversation-empty-title">No messages yet</h2>
          <p className="nex-conversation-empty-text">
            Messages in this conversation will appear here.
          </p>
        </div>
      </div>
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
  const [selectedId, setSelectedId] = useState<number | null>(null);

  const handleNewConversation = async () => {
    const id = await create();
    if (id !== null) {
      setSelectedId(id);
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
      />
      <div className="nex-main">
        <MainContent
          selected={selected}
          hasConversations={conversations.length > 0}
        />
      </div>
    </div>
  );
}

export default App;

