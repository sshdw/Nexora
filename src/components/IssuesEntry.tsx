import M3RailItem from "./M3RailItem";
import { IssuesIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface IssuesEntryProps {
  /** Whether the Issues & PRs screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the read-only GitHub Issues & PRs screen,
// anchored below the Code Audit entry, on the shared M3RailItem row
// primitive with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal /
// Tasks / Audit semantics.
export default function IssuesEntry({ active = false, onClick }: IssuesEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.ghLabel")}
      icon={<IssuesIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.gh")}
    </M3RailItem>
  );
}
