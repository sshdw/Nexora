import M3RailItem from "./M3RailItem";
import { ReleaseIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface ReleaseEntryProps {
  /** Whether the Release screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the release-readiness screen, anchored below
// the Issues & PRs entry, on the shared M3RailItem row primitive with its
// .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal /
// Tasks / Audit / Debt / Issues semantics.
export default function ReleaseEntry({ active = false, onClick }: ReleaseEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.releaseLabel")}
      icon={<ReleaseIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.release")}
    </M3RailItem>
  );
}
