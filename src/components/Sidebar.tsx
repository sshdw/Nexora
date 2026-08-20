import type { CommandError, Conversation } from "../lib/tauri";
import ConversationList from "./ConversationList";
import NewConversationButton from "./NewConversationButton";
import NexoraMark from "./NexoraMark";
import PromptLibraryEntry from "./PromptLibraryEntry";
import SearchBox from "./SearchBox";
import SettingsEntry from "./SettingsEntry";

export interface SidebarProps {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  creating: boolean;
  busy: boolean;
  selectedId: number | null;
  onSelect: (id: number) => void;
  onNewConversation: () => void;
  onRetry: () => void;
  onOpenSettings: () => void;
  /** Whether the Prompt Library screen is currently shown. */
  libraryActive: boolean;
  onOpenPromptLibrary: () => void;
  /** Open a prompt found by search in the Prompt Library editor. */
  onSelectPrompt: (promptId: number) => void;
  onRename: (id: number, title: string) => Promise<void>;
  onArchive: (id: number) => void;
  onRestore: (id: number) => void;
  onDelete: (id: number) => void;
}

export default function Sidebar({
  conversations,
  loading,
  error,
  creating,
  busy,
  selectedId,
  onSelect,
  onNewConversation,
  onRetry,
  onOpenSettings,
  libraryActive,
  onOpenPromptLibrary,
  onSelectPrompt,
  onRename,
  onArchive,
  onRestore,
  onDelete,
}: SidebarProps) {
  return (
    <aside className="nex-sidebar" aria-label="Nexora">
      <div className="nex-sidebar-head">
        <NexoraMark className="nex-logo" />
        <NewConversationButton onClick={onNewConversation} disabled={creating}>
          New Conversation
        </NewConversationButton>
        <SearchBox
          conversations={conversations}
          onSelectResult={onSelect}
          onSelectPrompt={onSelectPrompt}
        />
      </div>

      <ConversationList
        conversations={conversations}
        loading={loading}
        error={error}
        selectedId={selectedId}
        busy={busy}
        onSelect={onSelect}
        onRetry={onRetry}
        onRename={onRename}
        onArchive={onArchive}
        onRestore={onRestore}
        onDelete={onDelete}
      />

      <SettingsEntry onClick={onOpenSettings} />
      <PromptLibraryEntry active={libraryActive} onClick={onOpenPromptLibrary} />
    </aside>
  );
}


