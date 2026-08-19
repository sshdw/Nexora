import { useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";

export interface ConversationItemProps {
  conversation: Conversation;
  selected: boolean;
  archived: boolean;
  busy?: boolean;
  onSelect: (id: number) => void;
  onRename: (id: number, title: string) => Promise<void>;
  onArchive: (id: number) => void;
  onRestore: (id: number) => void;
  onDelete: (id: number) => void;
}

export default function ConversationItem({
  conversation,
  selected,
  archived,
  busy = false,
  onSelect,
  onRename,
  onArchive,
  onRestore,
  onDelete,
}: ConversationItemProps) {
  const [renaming, setRenaming] = useState(false);
  const [draftTitle, setDraftTitle] = useState(conversation.title);

  const beginRename = () => {
    setDraftTitle(conversation.title);
    setRenaming(true);
  };

  const commitRename = async () => {
    const next = draftTitle.trim();
    setRenaming(false);
    if (next === "" || next === conversation.title) {
      setDraftTitle(conversation.title);
      return;
    }
    setDraftTitle(next);
    await onRename(conversation.id, next);
  };

  const cancelRename = () => {
    setRenaming(false);
    setDraftTitle(conversation.title);
  };

  const handleDelete = () => {
    const confirmed = window.confirm(
      `Delete "${conversation.title}"? This cannot be undone.`,
    );
    if (confirmed) onDelete(conversation.id);
  };

  if (renaming) {
    return (
      <li className="nex-conversation-item-wrap">
        <input
          className="nex-conversation-rename-input"
          value={draftTitle}
          aria-label="Rename conversation"
          autoFocus
          onChange={(event) => setDraftTitle(event.target.value)}
          onBlur={() => void commitRename()}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void commitRename();
            } else if (event.key === "Escape") {
              cancelRename();
            }
          }}
        />
      </li>
    );
  }

  return (
    <li className="nex-conversation-item-wrap">
      <div className="nex-conversation-row">
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
        <div className="nex-conversation-actions">
          <button
            type="button"
            className="nex-btn nex-btn-ghost nex-btn-sm"
            onClick={beginRename}
            disabled={busy}
            aria-label="Rename conversation"
          >
            Rename
          </button>
          {archived ? (
            <button
              type="button"
              className="nex-btn nex-btn-ghost nex-btn-sm"
              onClick={() => onRestore(conversation.id)}
              disabled={busy}
              aria-label="Restore conversation"
            >
              Restore
            </button>
          ) : (
            <button
              type="button"
              className="nex-btn nex-btn-ghost nex-btn-sm"
              onClick={() => onArchive(conversation.id)}
              disabled={busy}
              aria-label="Archive conversation"
            >
              Archive
            </button>
          )}
          <button
            type="button"
            className="nex-btn nex-btn-danger nex-btn-sm"
            onClick={handleDelete}
            disabled={busy}
            aria-label="Delete conversation"
          >
            Delete
          </button>
        </div>
      </div>
    </li>
  );
}
