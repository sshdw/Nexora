import M3RailItem from "./M3RailItem";
import { ActivityIcon } from "./icons";

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
  return (
    <M3RailItem
      label="Activity and health"
      icon={<ActivityIcon />}
      active={active}
      onClick={onClick}
    >
      Activity
    </M3RailItem>
  );
}
