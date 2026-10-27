//! Sessions panel — the shell's history destination (D-06 / §6.5).
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

import { useMemo, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import { SearchIcon } from "./icons";

export interface SessionsPanelProps {
  conversations: readonly Conversation[];
  loading: boolean;
  activeId: number | null;
  onOpen: (id: number) => void;
}

export default function SessionsPanel({
  conversations,
  loading,
  activeId,
  onOpen,
}: SessionsPanelProps) {
  const { t, locale } = useStrings();
  const [query, setQuery] = useState("");

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

  const row = (conversation: Conversation) => (
    <li key={conversation.id}>
      <button
        type="button"
        className={
          "nex-sessions-row" + (conversation.id === activeId ? " is-current" : "")
        }
        aria-current={conversation.id === activeId ? "true" : undefined}
        onClick={() => onOpen(conversation.id)}
      >
        <span className="nex-sessions-row-title">{conversation.title}</span>
        <span className="nex-sessions-row-meta">
          {formatRelativeTime(conversation.updated_at, locale)}
        </span>
      </button>
    </li>
  );

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
    </div>
  );
}
