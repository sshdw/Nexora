import M3RailItem from "./M3RailItem";
import { AuditIcon } from "./icons";
import { useStrings } from "../lib/useLocale";

export interface AuditEntryProps {
  /** Whether the Code Audit screen is currently shown. */
  active?: boolean;
  onClick?: () => void;
}

// Navigation entry point for the read-only Code Audit screen, anchored below
// the Tasks entry, on the shared M3RailItem row primitive with its
// .is-active pill/tone emphasis.
// `aria-current` (not `aria-pressed`): this is a navigation destination,
// matching the Settings / Prompt Library / VCS / Activity / Terminal /
// Tasks semantics.
export default function AuditEntry({ active = false, onClick }: AuditEntryProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.auditLabel")}
      icon={<AuditIcon />}
      active={active}
      onClick={onClick}
    >
      {t("nav.audit")}
    </M3RailItem>
  );
}
