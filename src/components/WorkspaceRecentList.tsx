import { useState } from "react";

import type { WorkspaceStore } from "../lib/useWorkspace";

export interface WorkspaceRecentListProps {
  store: WorkspaceStore;
}

/** Recent workspace folders dropdown (1.2.4): at most 5 entries,
 * most-recent first. Selecting an entry persists it backend-side. */
export default function WorkspaceRecentList({ store }: WorkspaceRecentListProps) {
  const [open, setOpen] = useState(false);

  if (store.recent.length === 0) return null;

  return (
    <div className="nex-workspace-recent">
      <button
        type="button"
        className="nex-nav-entry"
        aria-label="Recent workspace folders"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span aria-hidden="true">🕘</span>
        <span>Recent folders ({store.recent.length})</span>
      </button>
      {open && (
        <ul className="nex-workspace-recent-list" aria-label="Recent workspace folders">
          {store.recent.map((path) => (
            <li key={path}>
              <button
                type="button"
                className="nex-workspace-recent-item"
                title={path}
                disabled={store.saving || path === store.root}
                onClick={() => void store.selectRecent(path).then(() => setOpen(false))}
              >
                {path === store.root ? `● ${path}` : path}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
