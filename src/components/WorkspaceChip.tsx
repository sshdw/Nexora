import { useStrings } from "../lib/useLocale";

export interface WorkspaceChipProps {
  root: string | null;
  loading: boolean;
}

/** Header chip showing the current agent workspace folder (1.3.0). */
export default function WorkspaceChip({ root, loading }: WorkspaceChipProps) {
  const { t } = useStrings();
  const label = loading ? t("nav.workspaceLoading") : (root ?? t("nav.workspaceUnset"));
  const short = label.length > 48 ? `…${label.slice(-47)}` : label;
  return (
    <span
      className="nex-workspace-chip"
      role="group"
      title={label}
      aria-label={t("nav.workspaceChipAria", { label })}
    >
      <span aria-hidden="true">📁</span>
      <span className="nex-workspace-chip-text">{short}</span>
    </span>
  );
}
