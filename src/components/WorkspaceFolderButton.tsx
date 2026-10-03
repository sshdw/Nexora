import M3RailItem from "./M3RailItem";
import type { WorkspaceStore } from "../lib/useWorkspace";

export interface WorkspaceFolderButtonProps {
  store: WorkspaceStore;
}

/** Sidebar folder-picker button (1.3.0): opens the native directory dialog
 * (`dialog.open`) and persists the chosen root backend-side. Rendered on the
 * shared M3RailItem row primitive. */
export default function WorkspaceFolderButton({ store }: WorkspaceFolderButtonProps) {
  return (
    <M3RailItem
      label="Workspace folder"
      icon={
        <span aria-hidden="true">
          📁
        </span>
      }
      disabled={store.saving}
      onClick={() => void store.pickFolder()}
    >
      Workspace folder
    </M3RailItem>
  );
}
