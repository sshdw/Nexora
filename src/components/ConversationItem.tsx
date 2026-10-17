import { useState } from "react";

import { formatRelativeTime } from "../lib/format";
import type { Conversation } from "../lib/tauri";
import M3IconButton from "./M3IconButton";
import M3RailItem from "./M3RailItem";
import M3Toolbar from "./M3Toolbar";
import { Input } from "./ui/input";
import { useStrings } from "../lib/useLocale";
import { useSnackbar } from "./SnackbarHost";
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
  onRename: (id: number, title: string) => Promise<boolean>;
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
  onDelete: (id: number) => void;
}

// One conversation row, NEXORA-COMPONENT-MAP.md row 3: a title that
// ellipsizes, a muted relative date that never yields to it, hover-revealed
// icon actions, and an inline rename that replaces the title in place. No
// badges, counters, snippets or pins.
//
// The row rides the shared M3RailItem primitive (thin light tick + tone step
// when selected — never a colour fill), and the two facts that were
// colliding in the old markup are now structural rather than stylistic:
//   - the title is the ONLY truncating box (`min-w-0 flex-1 truncate`), and
//     the date lives in the row's `trailing` slot, outside that box;
//   - the date is `shrink-0`, so a long RU title ellipsizes around it and
//     the date is never the part that clips.
//
// Height comes from the primitive (32px, THEME §4 compact) rather than a
// per-row override, so the conversation rows, the footer rail rows and the
// loading placeholders that stand in for them all share one rhythm — the
// four 32px icon actions then fill exactly one row.
//
// An ARCHIVED row keeps the same tone as an unselected active one. The old
// sheet dimmed its title one more step down the legacy ladder; the new
// theme's only dimmer step is an opacity multiplier, which would push the
// title under the 4.5:1 text floor (THEME §2), and the "Archived" group
// label above the group already carries that fact.
//
// The actions are absolutely positioned and revealed by the migrated CSS in
// src/styles/components.css (`[data-slot="row-actions"]`). That reveal is CSS
// and not Tailwind because it is a cross-element state rule — it has to read
// the row's hover/selected state AND the actions' own focus-within — and
// because it must never hide the buttons from the tab order: `opacity` +
// `pointer-events` only, so Tab reaches the actions and focusing one reveals
// the group.
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
  // NEX-SEC-004: there is NO in-app confirm dialog for deletion. The trash
  // action calls straight through to the gated permanent-delete path, whose
  // blocking NATIVE OS prompt (bound to this exact row) is the single,
  // honest confirmation — a cancel there deletes nothing. Same shape as the
  // Prompt Library's delete flow; the old in-app ConfirmDialog step is gone
  // on purpose so approval cannot be shown twice and drift from what runs.
  const { locale, t } = useStrings();
  const { showSnack } = useSnackbar();

  // Archive is the reversible soft-delete (typed `archive_conversation` /
  // `restore_conversation` round-trip): it runs immediately and the snackbar
  // offers a bounded Undo window that restores. Permanent deletion stays on
  // the native OS prompt (NEX-SEC-004) and is intentionally not undoable.

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
    // The field replaces the row IN PLACE (MAP row 3): same box, same
    // height as the row it stands in for, so the list does not jump when
    // the title turns editable.
    return (
      <li className="px-2">
        <Input
          className="h-8"
          value={draftTitle}
          aria-label={t("list.renameAria")}
          autoFocus
          onChange={(event) => setDraftTitle(event.target.value)}
          onBlur={() => void commitRename()}
          onKeyDown={(event) => {
            // shortcut:rename.commit / shortcut:rename.cancel.
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
    <li
      data-slot="conversation-row"
      data-selected={selected ? "true" : undefined}
      className="relative"
    >
      <M3RailItem
        label={conversation.title}
        active={selected}
        onClick={() => onSelect(conversation.id)}
        trailing={
          <time
            data-slot="conversation-date"
            className="text-xs text-muted-foreground tabular-nums"
            dateTime={new Date(conversation.updated_at * 1000).toISOString()}
          >
            {formatRelativeTime(conversation.updated_at, locale)}
          </time>
        }
      >
        <span className="truncate" title={conversation.title} aria-hidden="true">
          {conversation.title}
        </span>
      </M3RailItem>
      {/* Compact icon actions, docked at the row's trailing edge. The group
          overlays the row rather than reserving width for it, so the title
          never reflows on hover; it carries its own surface and the date
          yields under it while it shows (the migrated CSS in
          components.css explains both). It is revealed by `opacity`, never
          `visibility`, so the four buttons stay in the tab order and
          focusing one reveals the group. */}
      <div data-slot="row-actions">
        <M3Toolbar
          label={t("list.itemActions", { title: conversation.title })}
          className="gap-0.5"
        >
          <M3IconButton
            size="md"
            label={t("list.actionExport")}
            onClick={() => onExport(conversation.id)}
            disabled={busy}
          >
            <ExportIcon />
          </M3IconButton>
          <M3IconButton
            size="md"
            label={t("list.actionRename")}
            onClick={beginRename}
            disabled={busy}
          >
            <PencilIcon />
          </M3IconButton>
          {archived ? (
            <M3IconButton
              size="md"
              label={t("list.actionRestore")}
              onClick={() => {
                void onRestore(conversation.id);
              }}
              disabled={busy}
            >
              <UnarchiveIcon />
            </M3IconButton>
          ) : (
            <M3IconButton
              size="md"
              label={t("list.actionArchive")}
              onClick={() => {
                void (async () => {
                  // Gate the success snackbar on the threaded result: a
                  // failed archive lands in the store error (honest error
                  // panel) and must never claim success with an Undo for a
                  // row that was never archived.
                  const ok = await onArchive(conversation.id);
                  if (!ok) return;
                  showSnack(t("list.archived", { title: conversation.title }), {
                    actionLabel: t("common.undo"),
                    onAction: () => {
                      void (async () => {
                        // Same gate for Undo: a failed restore is surfaced
                        // honestly (failure notice) instead of silently
                        // dismissing as if the row had been restored.
                        const restored = await onRestore(conversation.id);
                        if (!restored) {
                          showSnack(
                            t("list.restoreFailed", {
                              title: conversation.title,
                            }),
                          );
                        }
                      })();
                    },
                  });
                })();
              }}
              disabled={busy}
            >
              <ArchiveIcon />
            </M3IconButton>
          )}
          <M3IconButton
            size="md"
            danger
            label={t("list.actionDelete")}
            onClick={() => onDelete(conversation.id)}
            disabled={busy}
          >
            <TrashIcon />
          </M3IconButton>
        </M3Toolbar>
      </div>
    </li>
  );
}
