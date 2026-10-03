import M3RailItem from "./M3RailItem";
import { ActivityIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface ActivityHealthEntryProps {
  /** Whether the Activity & Health screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the read-only Activity & Health screen,
// anchored below the Version Control entry, on the shared M3RailItem row
// primitive with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS navigation semantics.
export default function ActivityHealthEntry({ active = false, onClick }: ActivityHealthEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.activityLabel")}
      icon={<ActivityIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.activity")}
    </M3RailItem>
  );
}
