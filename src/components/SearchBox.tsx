import { useEffect, useState } from "react";

import type {
  CommandError,
  Conversation,
  Prompt,
  SearchResults,
} from "../lib/tauri";
import { search } from "../lib/tauri";
import { SearchIcon } from "./icons";

export interface SearchBoxProps {
  /** Loaded conversations, used to label message hits with their title. */
  conversations: Conversation[];
  /** Called with the conversation id when a conversation/message result is chosen. */
  onSelectResult: (conversationId: number) => void;
  /** Called with the prompt id when a prompt search result is chosen. */
  onSelectPrompt: (promptId: number) => void;
}

// Small settle delay so each keystroke does not fire an FTS query.
const DEBOUNCE_MS = 250;

export default function SearchBox({
  conversations,
  onSelectResult,
  onSelectPrompt,
}: SearchBoxProps) {
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
    setLoading(true);
    setError(null);
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

  const openPrompt = (promptId: number) => {
    onSelectPrompt(promptId);
    // Return the sidebar to the organized list after opening a result.
    setValue("");
    setResults(null);
  };

  const titleFor = (conversationId: number): string => {
    const match = conversations.find((c) => c.id === conversationId);
    return match ? match.title : "Conversation";
  };

  const showResults = value.trim() !== "";
  const hasResults =
    results !== null &&
    (results.conversations.length > 0 ||
      results.message_matches.length > 0 ||
      results.prompts.length > 0);

  return (
    <div className="nex-search">
      <label htmlFor="nex-search-input" className="nex-sr-only">
        Search conversations and prompts
      </label>
      <SearchIcon className="nex-search-icon" />
      <input
        id="nex-search-input"
        type="search"
        className="nex-search-input"
        placeholder="Search conversations and prompts"
        value={value}
        onChange={(event) => setValue(event.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      {showResults && (
        <div className="nex-search-results" role="listbox" aria-label="Search results">
          {loading && <p className="nex-search-status">Searching…</p>}
          {error && (
            <p className="nex-search-status" role="alert">
              {error.message}
            </p>
          )}
          {!loading && !error && results && (
            <>
              {!hasResults && (
                <p className="nex-search-status">
                  No results for “{value.trim()}”.
                </p>
              )}
              {results.conversations.length > 0 && (
                <>
                  <p className="nex-search-group">Conversations</p>
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
                        {conversation.status === "archived" ? "Archived" : "Conversation"}
                      </span>
                    </button>
                  ))}
                </>
              )}
              {results.message_matches.length > 0 && (
                <>
                  <p className="nex-search-group">Messages</p>
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
              {results.prompts.length > 0 && (
                <>
                  <p className="nex-search-group">Prompts</p>
                  {results.prompts.map((prompt: Prompt) => (
                    <button
                      key={`prompt-${prompt.id}`}
                      type="button"
                      className="nex-search-result"
                      onClick={() => openPrompt(prompt.id)}
                    >
                      <span className="nex-search-result-title">
                        {prompt.title}
                      </span>
                      <span className="nex-search-result-snippet">
                        {prompt.content}
                      </span>
                      <span className="nex-search-result-meta">Prompt</span>
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
  return { kind: "unknown", message: "Unable to run the search." };
}
