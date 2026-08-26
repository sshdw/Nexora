import { useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";
import ConfirmDialog from "./ConfirmDialog";
import {
  ArchiveIcon,
  ExportIcon,
  PencilIcon,
  TrashIcon,
  UnarchiveIcon,
} from "./icons";

export interface ConversationItemProps {
  conversation: Conversation;
  selected: boolean;
  archived: boolean;
  busy?: boolean;
  onSelect: (id: number) => void;
  onExport: (id: number) => void;
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
  onExport,
  onRename,
  onArchive,
  onRestore,
  onDelete,
}: ConversationItemProps) {
  const [renaming, setRenaming] = useState(false);
  const [draftTitle, setDraftTitle] = useState(conversation.title);
  // 0.3.0: deletion confirms in the Nexora dialog system (was
  // window.confirm) — same explicit-confirm behavior, in-app chrome.
  const [confirmingDelete, setConfirmingDelete] = useState(false);

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
        {/* Compact icon actions replace the timestamp while hovered /
            focused / selected — they no longer consume row width, so the
            title can never collide with them (0.3.0 defect fix). */}
        <div className="nex-conversation-actions">
          <button
            type="button"
            className="nex-icon-btn nex-icon-btn-sm"
            onClick={() => onExport(conversation.id)}
            disabled={busy}
            aria-label="Export conversation"
            title="Export"
          >
            <ExportIcon />
          </button>
          <button
            type="button"
            className="nex-icon-btn nex-icon-btn-sm"
            onClick={beginRename}
            disabled={busy}
            aria-label="Rename conversation"
            title="Rename"
          >
            <PencilIcon />
          </button>
          {archived ? (
            <button
              type="button"
              className="nex-icon-btn nex-icon-btn-sm"
              onClick={() => onRestore(conversation.id)}
              disabled={busy}
              aria-label="Restore conversation"
              title="Restore"
            >
              <UnarchiveIcon />
            </button>
          ) : (
            <button
              type="button"
              className="nex-icon-btn nex-icon-btn-sm"
              onClick={() => onArchive(conversation.id)}
              disabled={busy}
              aria-label="Archive conversation"
              title="Archive"
            >
              <ArchiveIcon />
            </button>
          )}
          <button
            type="button"
            className="nex-icon-btn nex-icon-btn-sm nex-icon-btn-danger"
            onClick={() => setConfirmingDelete(true)}
            disabled={busy}
            aria-label="Delete conversation"
            title="Delete"
          >
            <TrashIcon />
          </button>
        </div>
      </div>
      {confirmingDelete && (
        <ConfirmDialog
          title="Delete conversation?"
          body={`“${conversation.title}” and all of its messages will be permanently deleted. This cannot be undone.`}
          confirmLabel="Delete"
          danger
          onConfirm={() => {
            setConfirmingDelete(false);
            onDelete(conversation.id);
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
      )}
    </li>
  );
}
