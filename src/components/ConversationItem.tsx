import { useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";
import ConfirmDialog from "./ConfirmDialog";
import M3IconButton from "./M3IconButton";
import M3RailItem from "./M3RailItem";
import M3Toolbar from "./M3Toolbar";
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
        {/* Row selection rides the shared M3RailItem primitive (contract
            §Shell expanded-rail grammar): active = pill + tone step +
            emphasized label on the default-speed spring, with the
            title/time composed inside the rail label. Selection is conveyed
            by aria-current only. */}
        <M3RailItem
          label={conversation.title}
          active={selected}
          className={
            "nex-conversation-item" + (archived ? " is-archived" : "")
          }
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
        </M3RailItem>
        {/* Compact icon actions replace the timestamp while hovered /
            focused / selected — they no longer consume row width, so the
            title can never collide with them (0.3.0 defect fix). Grouped
            in a docked toolbar (same placement, token-driven look). */}
        <M3Toolbar
          label={`Actions for ${conversation.title}`}
          className="nex-conversation-actions"
        >
          <M3IconButton
            size="sm"
            label="Export conversation"
            onClick={() => onExport(conversation.id)}
            disabled={busy}
          >
            <ExportIcon />
          </M3IconButton>
          <M3IconButton
            size="sm"
            label="Rename conversation"
            onClick={beginRename}
            disabled={busy}
          >
            <PencilIcon />
          </M3IconButton>
          {archived ? (
            <M3IconButton
              size="sm"
              label="Restore conversation"
              onClick={() => onRestore(conversation.id)}
              disabled={busy}
            >
              <UnarchiveIcon />
            </M3IconButton>
          ) : (
            <M3IconButton
              size="sm"
              label="Archive conversation"
              onClick={() => onArchive(conversation.id)}
              disabled={busy}
            >
              <ArchiveIcon />
            </M3IconButton>
          )}
          <M3IconButton
            size="sm"
            danger
            label="Delete conversation"
            onClick={() => setConfirmingDelete(true)}
            disabled={busy}
          >
            <TrashIcon />
          </M3IconButton>
        </M3Toolbar>
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
