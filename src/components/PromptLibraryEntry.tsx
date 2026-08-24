import { BookIcon } from "./icons";

export interface PromptLibraryEntryProps {
  /** Whether the Prompt Library screen is currently open. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the Prompt Library screen (Phase 10.4 — FR-007),
// anchored below the Settings entry, on the shared .nex-nav-entry row
// primitive (0.2.2 component layer) with its .is-active emphasis.
export default function PromptLibraryEntry({ active = false, onClick }: PromptLibraryEntryProps) {
  return (
    <button
      type="button"
      className={"nex-nav-entry" + (active ? " is-active" : "")}
      aria-label="Prompt Library"
      aria-pressed={active}
      title="Prompt Library"
      onClick={onClick}
    >
      <BookIcon className="nex-nav-entry-icon" />
      <span>Prompt Library</span>
    </button>
  );
}