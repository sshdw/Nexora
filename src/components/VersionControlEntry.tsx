import M3RailItem from "./M3RailItem";
import { BranchIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface VersionControlEntryProps {
  /** Whether the Version Control screen is currently open. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the read-only Version Control screen, on the
// shared M3RailItem row primitive with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination.
// One of the three rows the #219 nav trim kept in the rail, above Settings.
export default function VersionControlEntry({ active = false, onClick }: VersionControlEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.vcs")}
      icon={<BranchIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.vcs")}
    </M3RailItem>
  );
}
