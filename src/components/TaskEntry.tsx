import M3RailItem from "./M3RailItem";
import { TaskIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface TaskEntryProps {
  /** Whether the Tasks screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the task manager screen, anchored below the
// Terminal entry, on the shared M3RailItem row primitive with its
// .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal
// semantics.
export default function TaskEntry({ active = false, onClick }: TaskEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.tasksLabel")}
      icon={<TaskIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.tasks")}
    </M3RailItem>
  );
}
