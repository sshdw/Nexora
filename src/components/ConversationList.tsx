import type { CommandError, Conversation } from "../lib/tauri";
import ConversationItem from "./ConversationItem";

export interface ConversationListProps {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  selectedId: number | null;
  onSelect: (id: number) => void;
  onRetry: () => void;
}

export default function ConversationList({
  conversations,
  loading,
  error,
  selectedId,
  onSelect,
  onRetry,
}: ConversationListProps) {
  if (loading) {
    return (
      <nav className="nex-conversation-nav" aria-label="Conversations">
        <ul className="nex-conversation-list" role="status" aria-label="Loading">
          {Array.from({ length: 5 }).map((_, index) => (
            <li key={index} className="nex-conversation-skeleton-row" />
          ))}
        </ul>
      </nav>
    );
  }

  if (error) {
    return (
      <nav className="nex-conversation-nav" aria-label="Conversations">
        <div className="nex-conversation-error" role="alert">
          <span className="nex-conversation-error-text">{error.message}</span>
          <button
            type="button"
            className="nex-btn nex-btn-ghost nex-btn-sm"
            onClick={onRetry}
          >
            Try again
          </button>
        </div>
      </nav>
    );
  }

  if (conversations.length === 0) {
    return (
      <nav className="nex-conversation-nav" aria-label="Conversations">
        <p className="nex-conversation-empty-hint">No conversations yet.</p>
      </nav>
    );
  }

  // Visual grouping only: the backend already sorts by updated_at DESC, so the
  // relative order within each group is preserved without re-sorting here.
  const active = conversations.filter((c) => c.status === "active");
  const archived = conversations.filter((c) => c.status === "archived");

  return (
    <nav className="nex-conversation-nav" aria-label="Conversations">
      <ul className="nex-conversation-list">
        {active.length > 0 && (
          <li className="nex-conversation-group-label">Active</li>
        )}
        {active.map((conversation) => (
          <ConversationItem
            key={conversation.id}
            conversation={conversation}
            selected={conversation.id === selectedId}
            onSelect={onSelect}
            archived={false}
          />
        ))}

        {archived.length > 0 && (
          <>
            <li className="nex-conversation-group-label">Archived</li>
            {archived.map((conversation) => (
              <ConversationItem
                key={conversation.id}
                conversation={conversation}
                selected={conversation.id === selectedId}
                onSelect={onSelect}
                archived
              />
            ))}
          </>
        )}
      </ul>
    </nav>
  );
}
