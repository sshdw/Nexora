import { useEffect, useState } from "react";

import type {
  CommandError,
  Conversation,
  SearchResults,
} from "../lib/tauri";
import { search } from "../lib/tauri";
import { getLocale, tr } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
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
    const timer = window.setTimeout(() => {
      void search(query)
        .then(setResults)
        .catch((e) => setError(toCommandError(e)))
        .finally(() => setLoading(false));
    }, DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
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
    <div className="nex-search">
      <label htmlFor="nex-search-input" className="nex-sr-only">
        {t("search.label")}
      </label>
      <SearchIcon className="nex-search-icon" />
      <input
        id="nex-search-input"
        type="search"
        className={
          "nex-search-input" + (loading ? " nex-search-input-loading" : "")
        }
        placeholder={t("search.placeholder")}
        value={value}
        onChange={(event) => setValue(event.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      {loading && (
        // The wait signal is an inline adornment inside the field, not a
        // floating status over the list (#228). role="status" carries the
        // text so reduced motion loses no information.
        <span className="nex-spinner nex-search-spinner" role="status">
          <span className="nex-sr-only">{t("search.searching")}</span>
        </span>
      )}
      {showResults && (
        // Plain labelled group of actionable buttons. The previous
        // role="listbox" was an ARIA misuse: its children are buttons,
        // not options (keyboard users reach them via normal Tab order).
        <div
          className="nex-search-results nex-pop-enter"
          role="group"
          aria-label={t("search.resultsAria")}
        >
          {error && (
            <p className="nex-search-status nex-fade-in" role="alert">
              {error.message}
            </p>
          )}
          {!error && results && (
            <>
              {!hasResults && (
                <div className="nex-search-empty nex-fade-in">
                  <SearchIcon className="nex-search-empty-icon" />
                  <span>{t("search.noResults", { q: value.trim() })}</span>
                </div>
              )}
              {results.conversations.length > 0 && (
                <>
                  <p className="nex-search-group">{t("search.groupConversations")}</p>
                  {results.conversations.map((conversation) => (
                    <button
                      key={`conversation-${conversation.id}`}
                      type="button"
                      className="nex-search-result"
                      onClick={() => open(conversation.id)}
                    >
                      <span className="nex-search-result-title">
                        {conversation.title}
                      </span>
                      <span className="nex-search-result-meta">
                        {conversation.status === "archived" ? t("search.metaArchived") : t("search.metaConversation")}
                      </span>
                    </button>
                  ))}
                </>
              )}
              {results.message_matches.length > 0 && (
                <>
                  <p className="nex-search-group">{t("search.groupMessages")}</p>
                  {results.message_matches.map((message) => (
                    <button
                      key={`message-${message.id}`}
                      type="button"
                      className="nex-search-result"
                      onClick={() => open(message.conversation_id)}
                    >
                      <span className="nex-search-result-title">
                        {titleFor(message.conversation_id)}
                      </span>
                      <span className="nex-search-result-snippet">
                        {message.content}
                      </span>
                    </button>
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
