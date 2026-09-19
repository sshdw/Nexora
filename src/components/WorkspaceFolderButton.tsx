import type { WorkspaceStore } from "../lib/useWorkspace";

export interface WorkspaceFolderButtonProps {
  store: WorkspaceStore;
}

/** Sidebar folder-picker button (1.3.0): opens the native directory dialog
 * (`dialog.open`) and persists the chosen root backend-side. */
export default function WorkspaceFolderButton({ store }: WorkspaceFolderButtonProps) {
  return (
    <button
      type="button"
      className="nex-nav-entry"
      aria-label="Choose workspace folder"
      title="Choose workspace folder"
      disabled={store.saving}
      onClick={() => void store.pickFolder()}
    >
      <span className="nex-nav-entry-icon" aria-hidden="true">
        📁
      </span>
      <span>Workspace folder</span>
    </button>
  );
}
