import { useState } from "react";

import type { WorkspaceStore } from "../lib/useWorkspace";
import M3RailItem from "./M3RailItem";

export interface WorkspaceRecentListProps {
  store: WorkspaceStore;
}

/** Recent workspace folders dropdown (1.3.0): at most 5 entries,
 * most-recent first. Selecting an entry persists it backend-side. */
export default function WorkspaceRecentList({ store }: WorkspaceRecentListProps) {
  const [open, setOpen] = useState(false);

  if (store.recent.length === 0) return null;

  return (
    <div className="nex-workspace-recent">
      <M3RailItem
        label={`Recent folders (${store.recent.length})`}
        icon={
          <span aria-hidden="true">
            🕘
          </span>
        }
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        Recent folders ({store.recent.length})
      </M3RailItem>
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
