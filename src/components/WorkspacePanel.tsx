//! Workspace panel — the shell's folder destination (D-06 / O-07).
//!
//! The header chip already names the active folder, but it is a 12 px label
//! with no way to change it: since #219 the folder controls left the rail and
//! the only remaining entry points are the command palette and onboarding.
//! This panel is the visible one — the current root, its full path, the
//! switch action, and the recent roots the backend already keeps.
//!
//! Everything here is the `useWorkspace` store's own data and actions: the
//! panel reads `root` / `recent` and dispatches `pickFolder` /
//! `selectRecent`. It performs no validation of its own — the backend owns
//! that (nesting refusal, canonicalisation) and its error surfaces below.

import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";

export interface WorkspacePanelProps {
  workspace: WorkspaceStore;
}

/** The folder's NAME — last path segment, either separator style. Roots
 *  arrive as the OS wrote them (`C:\src\app`, `/home/me/app`). */
function folderName(root: string): string {
  const trimmed = root.replace(/[\\/]+$/, "");
  if (trimmed === "") return root;
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut === -1 ? trimmed : trimmed.slice(cut + 1);
}

export default function WorkspacePanel({ workspace }: WorkspacePanelProps) {
  const { t } = useStrings();
  const { root, recent, loading, saving, error, pickFolder, selectRecent } = workspace;
  const others = recent.filter((path) => path !== root);

  return (
    <div className="nex-workspace-panel">
      {root === null ? (
        <div className="nex-workspace-current is-empty">
          <p className="nex-workspace-name">{t("dock.workspaceNone")}</p>
          <p className="nex-dock-panel-note">{t("dock.workspaceNoneText")}</p>
        </div>
      ) : (
        <div className="nex-workspace-current">
          <p className="nex-workspace-name" title={root}>
            {folderName(root)}
          </p>
          <p className="nex-workspace-path" title={root}>
            {root}
          </p>
        </div>
      )}

      <M3Button
        variant={root === null ? "primary" : "secondary"}
        size="sm"
        className="nex-workspace-action"
        disabled={saving || loading}
        onClick={() => void pickFolder()}
      >
        {root === null ? t("dock.workspaceOpen") : t("dock.workspaceChange")}
      </M3Button>

      {error !== null && (
        <p className="nex-dock-panel-note is-error" role="alert">
          {error.message}
        </p>
      )}

      {others.length > 0 && (
        <>
          <p className="nex-dock-panel-group">{t("dock.workspaceRecent")}</p>
          <ul className="nex-sessions-list">
            {others.map((path) => (
              <li key={path}>
                <button
                  type="button"
                  className="nex-sessions-row"
                  title={path}
                  disabled={saving}
                  onClick={() => void selectRecent(path)}
                >
                  <span className="nex-sessions-row-title">{folderName(path)}</span>
                  <span className="nex-sessions-row-meta nex-workspace-row-path">
                    {path}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
