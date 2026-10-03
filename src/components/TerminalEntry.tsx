import M3RailItem from "./M3RailItem";
import { TerminalIcon } from "./icons";

export interface TerminalEntryProps {
  /** Whether the Terminal screen is currently open. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the workspace Terminal screen, anchored
// below the Activity entry, on the shared M3RailItem row primitive with
// its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity semantics.
export default function TerminalEntry({ active = false, onClick }: TerminalEntryProps) {
  return (
    <M3RailItem
      label="Terminal"
      icon={<TerminalIcon />}
      active={active}
      onClick={onClick}
    >
      Terminal
    </M3RailItem>
  );
}
