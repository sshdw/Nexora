import { useStrings } from "../lib/useLocale";

export interface WorkspaceChipProps {
  root: string | null;
  loading: boolean;
}

/** Visible width cap for the chip's label: past this the chip has
 *  stopped naming a folder and started transcribing a path, which
 *  the tooltip already carries in full. */
const MAX_LABEL = 48;

/** The workspace folder's NAME — the last path segment, either
 *  separator style (Tauri roots arrive as the OS wrote them:
 *  `C:\src\app`, `/home/me/app`, `\\share\app`). Trailing
 *  separators are dropped first so `/srv/app/` and `/srv/app`
 *  agree, and a bare volume or root (`C:\`, `/`) keeps its whole
 *  self: there is no segment left to name it with. */
function folderName(root: string): string {
  const trimmed = root.replace(/[\\/]+$/, "");
  if (trimmed === "") return "";
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut === -1 ? trimmed : trimmed.slice(cut + 1);
}

/** Header chip showing the current agent workspace folder (1.3.0).
 *
 *  MAP row 5 / SYSTEM §4: the header shows the workspace NAME, and
 *  the full path lives in `title=` (and the accessible name) only.
 *  It used to print the path itself, truncated from the LEFT so the
 *  tail survived — a chip that read `…ts\nx\packages\web`.
 *
 *  No leading glyph: this chip has no icon of its own, and the 📁
 *  emoji it used to carry was neither an icon nor from
 *  `icons.tsx` (the sole icon source, SYSTEM §1) — a colour glyph
 *  on a monochrome chrome row, on a 12px slot. */
export default function WorkspaceChip({ root, loading }: WorkspaceChipProps) {
  const { t } = useStrings();
  const label = loading ? t("nav.workspaceLoading") : (root ?? t("nav.workspaceUnset"));
  // Loading/unset labels are not paths, so they are shown whole.
  const name = loading || root === null ? label : folderName(root);
  const visible = name === "" ? label : name;
  const short =
    visible.length > MAX_LABEL ? `${visible.slice(0, MAX_LABEL - 1)}…` : visible;
  return (
    <span
      className="nex-workspace-chip"
      role="group"
      title={label}
      aria-label={t("nav.workspaceChipAria", { label })}
    >
      <span className="nex-workspace-chip-text">{short}</span>
    </span>
  );
}
