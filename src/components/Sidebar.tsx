import type { CommandError, Conversation } from "../lib/tauri";
import ConversationList from "./ConversationList";
import NewConversationButton from "./NewConversationButton";
import NexoraMark from "./NexoraMark";
import SearchBox from "./SearchBox";
import SettingsEntry from "./SettingsEntry";

export interface SidebarProps {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  creating: boolean;
  selectedId: number | null;
  onSelect: (id: number) => void;
  onNewConversation: () => void;
  onRetry: () => void;
  onOpenSettings: () => void;
}

export default function Sidebar({
  conversations,
  loading,
  error,
  creating,
  selectedId,
  onSelect,
  onNewConversation,
  onRetry,
  onOpenSettings,
}: SidebarProps) {
  return (
    <aside className="nex-sidebar" aria-label="Nexora">
      <div className="nex-sidebar-head">
        <NexoraMark className="nex-logo" />
        <NewConversationButton onClick={onNewConversation} disabled={creating}>
          New Conversation
        </NewConversationButton>
        <SearchBox />
      </div>

      <ConversationList
        conversations={conversations}
        loading={loading}
        error={error}
        selectedId={selectedId}
        onSelect={onSelect}
        onRetry={onRetry}
      />

      <SettingsEntry onClick={onOpenSettings} />
    </aside>
  );
}


