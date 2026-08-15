import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";

export interface ConversationItemProps {
  conversation: Conversation;
  selected: boolean;
  archived: boolean;
  onSelect: (id: number) => void;
}

export default function ConversationItem({
  conversation,
  selected,
  archived,
  onSelect,
}: ConversationItemProps) {
  return (
    <li>
      <button
        type="button"
        className={
          "nex-conversation-item" +
          (selected ? " is-selected" : "") +
          (archived ? " is-archived" : "")
        }
        aria-selected={selected}
        aria-label={conversation.title}
        title={conversation.title}
        onClick={() => onSelect(conversation.id)}
      >
        <span
          className="nex-conversation-title"
          title={conversation.title}
          aria-hidden="true"
        >
          {conversation.title}
        </span>
        <time
          className="nex-conversation-time"
          dateTime={new Date(conversation.updated_at * 1000).toISOString()}
        >
          {formatRelativeTime(conversation.updated_at)}
        </time>
      </button>
    </li>
  );
}
