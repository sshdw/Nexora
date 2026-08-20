import { BookIcon } from "./icons";

export interface PromptLibraryEntryProps {
  /** Whether the Prompt Library screen is currently open. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the Prompt Library screen (Phase 10.4 — FR-007),
// anchored below the Settings entry, in the same quiet list-row language.
export default function PromptLibraryEntry({ active = false, onClick }: PromptLibraryEntryProps) {
  return (
    <button
      type="button"
      className={"nex-prompt-library-entry" + (active ? " is-active" : "")}
      aria-label="Prompt Library"
      aria-pressed={active}
      title="Prompt Library"
      onClick={onClick}
    >
      <BookIcon className="nex-prompt-library-icon" />
      <span>Prompt Library</span>
    </button>
  );
}