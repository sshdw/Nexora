export interface WorkspaceChipProps {
  root: string | null;
  loading: boolean;
}

/** Header chip showing the current agent workspace folder (1.3.0). */
export default function WorkspaceChip({ root, loading }: WorkspaceChipProps) {
  const label = loading ? "Workspace…" : (root ?? "Workspace unset");
  const short = label.length > 48 ? `…${label.slice(-47)}` : label;
  return (
    <span
      className="nex-workspace-chip"
      title={label}
      aria-label={`Current workspace folder: ${label}`}
    >
      <span aria-hidden="true">📁</span>
      <span className="nex-workspace-chip-text">{short}</span>
    </span>
  );
}
