import RailItem from "./RailItem";
import { DebtIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface DebtEntryProps {
  /** Whether the Debt backlog screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the technical-debt backlog screen, on the
// shared RailItem row primitive with its .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination.
// Unrendered since #219 (the nav trim) — the panel, this entry and its hooks
// stay, so re-adding the rail row is a one-line change.
export default function DebtEntry({ active = false, onClick }: DebtEntryProps) {
  const { t } = useStrings();
  return (
    <RailItem
      label={t("nav.debtLabel")}
      icon={<DebtIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.debt")}
    </RailItem>
  );
}
