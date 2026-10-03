import M3RailItem from "./M3RailItem";
import { BookIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface PromptLibraryEntryProps {
  /** Whether the Prompt Library screen is currently open. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the Prompt Library screen (Phase 10.4 — FR-007),
// anchored below the Settings entry, on the shared M3RailItem row primitive
// with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings navigation semantics (0.2.5 QA pass).
export default function PromptLibraryEntry({ active = false, onClick }: PromptLibraryEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.library")}
      icon={<BookIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.library")}
    </M3RailItem>
  );
}
