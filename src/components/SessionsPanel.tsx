//! Sessions panel — the shell's history destination (D-06 / §6.5 / R6-06).
//!
//! Why this exists at all: the shell has no sidebar and tabs are not
//! persisted, so without this panel a conversation that is not already open
//! is unreachable — the first screen could honestly say "no conversations
//! yet" while the database held dozens. The Dock's Sessions button is the
//! destination the register names for exactly this job: "Sessions = вся
//! история/поиск", "Tabs = открытые сессии".
//!
//! Data is the App's single `useConversations` store — this panel filters and
//! groups it, it never fetches or invents rows. Filtering is by title only:
//! the backend's full-text `search` command covers message bodies, and a
//! title filter that silently pretended to search bodies would be the kind of
//! half-truth the brief's data-honesty rule forbids.
//!
//! R6-06 restores the row actions the old sidebar had and the panel lost:
//! rename (inline, Enter/Esc, empty rejected, focus returned), archive /
//! restore, and permanent delete. Delete asks in-app first through
//! `ConfirmDialog` and then calls the store's existing `remove`, whose native
//! OS prompt is the second gate (NEX-SEC-004) — no new backend path.
//!
//! Actions are revealed by `opacity`, never `display`/`visibility`: every
//! button stays in the tab order, and focusing one reveals the group.

import { useMemo, useRef, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import ConfirmDialog from "./ConfirmDialog";
import M3IconButton from "./M3IconButton";
import M3Toolbar from "./M3Toolbar";
import { ArchiveIcon, PencilIcon, SearchIcon, TrashIcon, UnarchiveIcon } from "./icons";

export interface SessionsPanelProps {
  conversations: readonly Conversation[];
  loading: boolean;
  activeId: number | null;
  /** A rename/archive/restore/delete is in flight. */
  busy?: boolean;
  onOpen: (id: number) => void;
  onRename: (id: number, title: string) => Promise<boolean>;
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
  onDelete: (id: number) => Promise<boolean>;
}

export default function SessionsPanel({
  conversations,
  loading,
  activeId,
  busy = false,
  onOpen,
  onRename,
  onArchive,
  onRestore,
  onDelete,
}: SessionsPanelProps) {
  const { t, locale } = useStrings();
  const [query, setQuery] = useState("");
  const [renamingId, setRenamingId] = useState<number | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [pendingDelete, setPendingDelete] = useState<Conversation | null>(null);
  const rowRefs = useRef(new Map<number, HTMLButtonElement>());
  const skipBlur = useRef(false);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (needle === "") return conversations;
    return conversations.filter((c) => c.title.toLowerCase().includes(needle));
  }, [conversations, query]);

  // The store is already ordered by `updated_at DESC` (backend order, kept
  // verbatim by useConversations), so grouping is a stable partition and each
  // group stays newest-first without a second sort.
  const active = filtered.filter((c) => c.status === "active");
  const archived = filtered.filter((c) => c.status !== "active");

  const returnFocus = (id: number) => {
    window.setTimeout(() => rowRefs.current.get(id)?.focus(), 0);
  };

  const beginRename = (conversation: Conversation) => {
    setRenameDraft(conversation.title);
    setRenamingId(conversation.id);
  };

  const cancelRename = (id: number, restoreFocus = true) => {
    skipBlur.current = true;
    setRenamingId(null);
    setRenameDraft("");
    if (restoreFocus) returnFocus(id);
  };

  const commitRename = async (conversation: Conversation, restoreFocus: boolean) => {
    if (skipBlur.current) {
      skipBlur.current = false;
      return;
    }
    const next = renameDraft.trim();
    if (next === "" || next === conversation.title) {
      cancelRename(conversation.id, restoreFocus);
      return;
    }
    setRenamingId(null);
    setRenameDraft("");
    await onRename(conversation.id, next);
    // Enter returns the reader to the row they just edited. Blur does NOT:
    // the blur happened because they clicked somewhere else, and pulling
    // focus back would steal it from the thing they aimed at.
    if (restoreFocus) returnFocus(conversation.id);
  };

  const row = (conversation: Conversation) => {
    const isCurrent = conversation.id === activeId;
    const isArchived = conversation.status !== "active";
    const renaming = renamingId === conversation.id;

    return (
      <li key={conversation.id} className="nex-sessions-item">
        {renaming ? (
          <div className="nex-sessions-row is-renaming">
            <input
              className="nex-sessions-rename-input"
              value={renameDraft}
              aria-label={t("list.renameAria")}
              autoFocus
              onChange={(event) => setRenameDraft(event.target.value)}
              onBlur={() => void commitRename(conversation, false)}
              onKeyDown={(event) => {
                // shortcut:rename.commit / shortcut:rename.cancel.
                if (event.key === "Enter") {
                  event.preventDefault();
                  void commitRename(conversation, true);
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  cancelRename(conversation.id);
                }
              }}
            />
          </div>
        ) : (
          <>
            <button
              type="button"
              ref={(element) => {
                if (element === null) rowRefs.current.delete(conversation.id);
                else rowRefs.current.set(conversation.id, element);
              }}
              className={
                "nex-sessions-row" + (isCurrent ? " is-current" : "")
              }
              aria-current={isCurrent ? "true" : undefined}
              onClick={() => onOpen(conversation.id)}
            >
              <span className="nex-sessions-row-title" title={conversation.title}>
                {conversation.title}
              </span>
              <span className="nex-sessions-row-meta">
                {formatRelativeTime(conversation.updated_at, locale)}
              </span>
            </button>
            <div className="nex-sessions-row-actions">
              <M3Toolbar
                label={t("list.itemActions", { title: conversation.title })}
                className="nex-sessions-actions"
              >
                <M3IconButton
                  size="sm"
                  label={t("list.actionRename")}
                  disabled={busy}
                  onClick={() => beginRename(conversation)}
                >
                  <PencilIcon />
                </M3IconButton>
                {isArchived ? (
                  <M3IconButton
                    size="sm"
                    label={t("list.actionRestore")}
                    disabled={busy}
                    onClick={() => void onRestore(conversation.id)}
                  >
                    <UnarchiveIcon />
                  </M3IconButton>
                ) : (
                  <M3IconButton
                    size="sm"
                    label={t("list.actionArchive")}
                    disabled={busy}
                    onClick={() => void onArchive(conversation.id)}
                  >
                    <ArchiveIcon />
                  </M3IconButton>
                )}
                <M3IconButton
                  size="sm"
                  danger
                  label={t("list.actionDelete")}
                  disabled={busy}
                  onClick={() => setPendingDelete(conversation)}
                >
                  <TrashIcon />
                </M3IconButton>
              </M3Toolbar>
            </div>
          </>
        )}
      </li>
    );
  };

  return (
    <div className="nex-sessions">
      <div className="nex-sessions-search">
        <SearchIcon className="nex-sessions-search-icon" aria-hidden="true" />
        <input
          className="nex-sessions-search-input"
          type="search"
          data-dock-panel-autofocus
          value={query}
          placeholder={t("dock.sessionsFilter")}
          aria-label={t("dock.sessionsFilter")}
          onChange={(event) => setQuery(event.target.value)}
        />
      </div>

      {loading ? (
        <p className="nex-dock-panel-note">{t("common.loading")}</p>
      ) : filtered.length === 0 ? (
        <p className="nex-dock-panel-note">
          {conversations.length === 0 ? t("dock.sessionsEmpty") : t("dock.sessionsNoMatch")}
        </p>
      ) : (
        <div className="nex-sessions-scroll">
          {active.length > 0 && (
            <>
              <p className="nex-dock-panel-group">{t("common.active")}</p>
              <ul className="nex-sessions-list">{active.map(row)}</ul>
            </>
          )}
          {archived.length > 0 && (
            <>
              <p className="nex-dock-panel-group">{t("common.archived")}</p>
              <ul className="nex-sessions-list">{archived.map(row)}</ul>
            </>
          )}
        </div>
      )}

      {pendingDelete !== null && (
        <ConfirmDialog
          title={t("dock.sessionsDeleteTitle")}
          body={t("dock.sessionsDeleteBody", { title: pendingDelete.title })}
          confirmLabel={t("dock.sessionsDeleteConfirm")}
          danger
          busy={busy}
          onConfirm={() => {
            const id = pendingDelete.id;
            setPendingDelete(null);
            void onDelete(id);
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </div>
  );
}
