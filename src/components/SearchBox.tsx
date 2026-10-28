import { useEffect, useState } from "react";

import type {
  CommandError,
  Conversation,
  SearchResults,
} from "../lib/tauri";
import { search } from "../lib/tauri";
import { getLocale, tr } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import { Input } from "./ui/input";
import { Spinner } from "./ui/spinner";
import M3Button from "./M3Button";
import { SearchIcon } from "./icons";

export interface SearchBoxProps {
  /** Loaded conversations, used to label message hits with their title. */
  conversations: Conversation[];
  /** Called with the conversation id when a conversation/message result is chosen. */
  onSelectResult: (conversationId: number) => void;
}

// Prompt results are deliberately not listed (#219): choosing one opened the
// Prompt Library, and that panel has no UI access path any more. The backend
// `search` command still returns prompt hits and `usePrompts` still owns the
// Prompt Library data — only the row that would have opened it is gone.

// Small settle delay so each keystroke does not fire an FTS query.
const DEBOUNCE_MS = 250;

// #230 Phase 1 — the search field and its results panel, migrated onto the
// mono theme. Composition follows `components/input.md` (one field, one
// focus-visible ring), `components/spinner.md` (the wait signal, never alone),
// `components/command.md` (results = grouped item rows: a title line over a
// muted meta/snippet line) and `components/empty.md` (the empty-with-query
// pattern: a quiet mark over one line that quotes the query back).
// NEXORA-DESIGN-SYSTEM.md §6 is the binding contract and is preserved
// unchanged: the spinner is an inline adornment INSIDE the field, and the
// panel opens only for results or an error — never for loading alone.
//
// The two result rows ride the canonical button primitive (SYSTEM §3 lists
// this file's raw `<button>`s as by-passers to fix in this PR), which is
// where their hover tone and focus ring come from.
export default function SearchBox({
  conversations,
  onSelectResult,
}: SearchBoxProps) {
  const { t } = useStrings();
  const [value, setValue] = useState("");
  const [results, setResults] = useState<SearchResults | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<CommandError | null>(null);

  useEffect(() => {
    const query = value.trim();
    if (query === "") {
      setResults(null);
      setError(null);
      setLoading(false);
      return;
    }
    // Loading-only must never open the panel (#228): the spinner lives
    // inline in the input. Any results from the previous query are dropped
    // here too, so the panel cannot linger over the list showing stale hits
    // for a query that no longer matches — it opens only when the CURRENT
    // query's results or error actually arrive.
    setLoading(true);
    setError(null);
    setResults(null);
    // Cancelling the timer is not enough: once it fires, the request keeps
    // running. Without the `active` identity guard a response for a
    // superseded or cleared query would still land and re-open the panel
    // with hits that no longer match the field (#233).
    let active = true;
    const timer = window.setTimeout(() => {
      void search(query)
        .then((r) => {
          if (active) setResults(r);
        })
        .catch((e) => {
          if (active) setError(toCommandError(e));
        })
        .finally(() => {
          if (active) setLoading(false);
        });
    }, DEBOUNCE_MS);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [value]);

  const open = (conversationId: number) => {
    onSelectResult(conversationId);
    // Return the sidebar to the organized list after opening a result.
    setValue("");
    setResults(null);
  };

  const titleFor = (conversationId: number): string => {
    const match = conversations.find((c) => c.id === conversationId);
    return match ? match.title : tr(getLocale(), "common.conversation");
  };

  const showResults = error !== null || results !== null;
  // Prompt hits are part of the backend payload but not of the result list
  // (#219) — see the note above the component.
  const hasResults =
    results !== null &&
    (results.conversations.length > 0 || results.message_matches.length > 0);

  return (
    <div className="relative">
      <label htmlFor="nex-search-input" className="sr-only">
        {t("search.label")}
      </label>
      <SearchIcon
        data-icon="inline-start"
        aria-hidden="true"
        className="pointer-events-none absolute top-1/2 left-2 size-4 -translate-y-1/2 text-muted-foreground"
      />
      <Input
        id="nex-search-input"
        type="search"
        // `pr-8` is unconditional, not a loading-only reservation: the
        // trailing adornment then never shifts the text as it appears, and
        // a long query can never run underneath the spinner.
        className="h-8 bg-muted pl-8 pr-8"
        placeholder={t("search.placeholder")}
        value={value}
        onChange={(event) => setValue(event.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      {loading && (
        // The wait signal is an inline adornment inside the field, not a
        // floating status over the list (#228). role="status" carries the
        // text so reduced motion — where the ring freezes on one frame —
        // loses no information.
        <span
          data-slot="search-adornment"
          role="status"
          className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground"
        >
          <Spinner className="size-4" />
          <span className="sr-only">{t("search.searching")}</span>
        </span>
      )}
      {showResults && (
        // Plain labelled group of actionable buttons. The previous
        // role="listbox" was an ARIA misuse: its children are buttons,
        // not options (keyboard users reach them via normal Tab order).
        <div
          data-slot="search-results"
          role="group"
          aria-label={t("search.resultsAria")}
          className="absolute inset-x-0 top-full mt-1 flex max-h-80 flex-col gap-0.5 overflow-y-auto rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-md"
        >
          {error && (
            <p
              data-slot="search-status"
              role="alert"
              className="rounded-md px-2 py-1.5 text-xs text-destructive"
            >
              {error.message}
            </p>
          )}
          {!error && results && (
            <>
              {!hasResults && (
                <div
                  data-slot="search-empty"
                  className="flex flex-col items-center gap-2 px-2 py-4 text-center text-xs text-muted-foreground"
                >
                  <SearchIcon aria-hidden="true" className="size-5 opacity-60" />
                  <span>{t("search.noResults", { q: value.trim() })}</span>
                </div>
              )}
              {results.conversations.length > 0 && (
                <>
                  <p className="px-2 pt-1.5 pb-0.5 text-xs text-muted-foreground">
                    {t("search.groupConversations")}
                  </p>
                  {results.conversations.map((conversation) => (
                    <M3Button
                      key={`conversation-${conversation.id}`}
                      variant="quiet"
                      // The canonical button primitive, not a raw
                      // <button> (SYSTEM §3 names SearchBox's raw
                      // buttons as by-passers to fix in this PR): it
                      // carries the hover/active tone and the
                      // focus-visible ring, so no rule is hand-rolled
                      // here. `h-auto flex-col` turns the fixed-height
                      // control into the command.md row shape — a title
                      // line over a muted meta line.
                      className="h-auto w-full flex-col items-start gap-0.5 px-2 py-1.5 text-left"
                      onClick={() => open(conversation.id)}
                    >
                      <span className="w-full truncate font-medium">
                        {conversation.title}
                      </span>
                      <span className="w-full truncate text-xs text-muted-foreground font-normal">
                        {conversation.status === "archived" ? t("search.metaArchived") : t("search.metaConversation")}
                      </span>
                    </M3Button>
                  ))}
                </>
              )}
              {results.message_matches.length > 0 && (
                <>
                  <p className="px-2 pt-1.5 pb-0.5 text-xs text-muted-foreground">
                    {t("search.groupMessages")}
                  </p>
                  {results.message_matches.map((message) => (
                    <M3Button
                      key={`message-${message.id}`}
                      variant="quiet"
                      className="h-auto w-full flex-col items-start gap-0.5 px-2 py-1.5 text-left"
                      onClick={() => open(message.conversation_id)}
                    >
                      <span className="w-full truncate font-medium">
                        {titleFor(message.conversation_id)}
                      </span>
                      {/* The snippet line is the muted meta line of the
                          command.md row, not a summary of the result —
                          MAP row 3 bans snippets on the list rows, and
                          this one lives in the panel, where MAP row 2
                          asks for exactly this. */}
                      <span className="w-full truncate text-xs text-muted-foreground font-normal">
                        {message.content}
                      </span>
                    </M3Button>
                  ))}
                </>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function toCommandError(error: unknown): CommandError {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).kind === "string" &&
    typeof (error as CommandError).message === "string"
  ) {
    return { kind: (error as CommandError).kind, message: (error as CommandError).message };
  }
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error) return { kind: "unknown", message: error.message };
  return { kind: "unknown", message: tr(getLocale(), "common.searchUnreachable") };
}
