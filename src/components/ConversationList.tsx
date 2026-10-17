import type { CommandError, Conversation } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import ConversationItem from "./ConversationItem";
import M3Button from "./M3Button";
import { Skeleton } from "./ui/skeleton";

export interface ConversationListProps {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  selectedId: number | null;
  busy?: boolean;
  onSelect: (id: number) => void;
  onExport: (id: number) => void;
  onRetry: () => void;
  onRename: (id: number, title: string) => Promise<boolean>;
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
  onDelete: (id: number) => void;
}

// #230 Phase 1 — the scrolling half of the rail (`components/sidebar.md`:
// SidebarContent between the search field and the footer). Loading mirrors
// the rows it replaces (MAP row 19: skeletons stand in for the coming layout,
// not generic blocks), the group labels are sentence-case quiet text
// (NEXORA-MAIN-SCREEN.md §3), and the failure notice keeps what happened
// plus one action to try (SYSTEM §8). Structure only in Tailwind; every
// colour is a theme token.
export default function ConversationList({
  conversations,
  loading,
  error,
  selectedId,
  busy = false,
  onSelect,
  onExport,
  onRetry,
  onRename,
  onArchive,
  onRestore,
  onDelete,
}: ConversationListProps) {
  const { t } = useStrings();
  // NEX-SEC-004: a delete that ends at the native system prompt's Cancel
  // rejects with `confirmationRequired` — nothing was deleted, so it renders
  // as a calm status line over the untouched list (same shape as the Prompt
  // Library's `deleteCancelled`), never as the error panel. Derived from the
  // store error so it clears on the next operation.
  const deleteCancelled = error?.kind === "confirmationRequired";
  if (loading) {
    return (
      <nav
        data-slot="conversation-list"
        aria-label={t("list.navAria")}
        className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden px-2 pb-2"
      >
        <div
          role="status"
          aria-label={t("list.loadingAria")}
          className="flex flex-col gap-1 pt-3"
        >
          {Array.from({ length: 5 }).map((_, index) => (
            // One row-shaped placeholder per row: same height as the row it
            // stands in for, so nothing shifts when the data lands.
            <Skeleton key={index} className="h-8 w-full" />
          ))}
        </div>
      </nav>
    );
  }

  if (error && !deleteCancelled) {
    return (
      <nav
        data-slot="conversation-list"
        aria-label={t("list.navAria")}
        className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden px-2 pb-2"
      >
        <div
          role="alert"
          className="mt-3 flex flex-col items-start gap-2 rounded-md border border-border p-2"
        >
          <span className="text-xs text-destructive">{error.message}</span>
          <M3Button variant="quiet" size="sm" onClick={onRetry}>
            {t("common.retry")}
          </M3Button>
        </div>
      </nav>
    );
  }

  if (conversations.length === 0) {
    return (
      <nav
        data-slot="conversation-list"
        aria-label={t("list.navAria")}
        className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden px-2 pb-2"
      >
        <p className="px-2 py-3 text-xs text-muted-foreground">
          {t("list.emptyHint")}
        </p>
      </nav>
    );
  }

  // Visual grouping only: the backend already sorts by updated_at DESC, so the
  // relative order within each group is preserved without re-sorting here.
  const active = conversations.filter((c) => c.status === "active");
  const archived = conversations.filter((c) => c.status === "archived");

  return (
    <nav
      data-slot="conversation-list"
      aria-label={t("list.navAria")}
      className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden px-2 pb-2"
    >
      {deleteCancelled && (
        <p role="status" className="px-2 py-1.5 text-xs text-muted-foreground">
          {t("list.deleteCancelled")}
        </p>
      )}
      <ul className="w-full">
        {active.length > 0 && (
          // 12/400 muted, sentence case (NEXORA-MAIN-SCREEN.md §3,
          // THEME §6 ballot): a section label, never an emphasis run.
          <li className="px-2 pt-3 pb-1 text-xs text-muted-foreground">
            {t("list.groupActive")}
          </li>
        )}
        {active.map((conversation) => (
          <ConversationItem
            key={conversation.id}
            conversation={conversation}
            selected={conversation.id === selectedId}
            onSelect={onSelect}
            onExport={onExport}
            archived={false}
            busy={busy}
            onRename={onRename}
            onArchive={onArchive}
            onRestore={onRestore}
            onDelete={onDelete}
          />
        ))}

        {archived.length > 0 && (
          <>
            <li className="px-2 pt-3 pb-1 text-xs text-muted-foreground">
              {t("list.groupArchived")}
            </li>
            {archived.map((conversation) => (
              <ConversationItem
                key={conversation.id}
                conversation={conversation}
                selected={conversation.id === selectedId}
                onSelect={onSelect}
                onExport={onExport}
                archived
                busy={busy}
                onRename={onRename}
                onArchive={onArchive}
                onRestore={onRestore}
                onDelete={onDelete}
              />
            ))}
          </>
        )}
      </ul>
    </nav>
  );
}
