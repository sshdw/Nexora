import M3RailItem from "./M3RailItem";
import { DebtIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface DebtEntryProps {
  /** Whether the Debt backlog screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the technical-debt backlog screen, anchored
// below the Code Audit entry, on the shared M3RailItem row primitive with
// its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal /
// Tasks / Audit semantics.
export default function DebtEntry({ active = false, onClick }: DebtEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.debtLabel")}
      icon={<DebtIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.debt")}
    </M3RailItem>
  );
}
