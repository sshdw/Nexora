import M3RailItem from "./M3RailItem";
import { BookIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface DocsEntryProps {
  /** Whether the Docs screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the in-app product documentation screen,
// anchored below the Issues & PRs entry, on the shared M3RailItem row
// primitive with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal /
// Tasks / Audit / Debt semantics.
export default function DocsEntry({ active = false, onClick }: DocsEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.docsLabel")}
      icon={<BookIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.docs")}
    </M3RailItem>
  );
}
