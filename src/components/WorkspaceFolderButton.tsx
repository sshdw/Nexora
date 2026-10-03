import M3RailItem from "./M3RailItem";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";

export interface WorkspaceFolderButtonProps {
  store: WorkspaceStore;
}

/** Sidebar folder-picker button (1.3.0): opens the native directory dialog
 * (`dialog.open`) and persists the chosen root backend-side. Rendered on the
 * shared M3RailItem row primitive. */
export default function WorkspaceFolderButton({ store }: WorkspaceFolderButtonProps) {
  const { t } = useStrings();
  return (
    <M3RailItem
      label={t("nav.workspaceFolder")}
      icon={
        <span aria-hidden="true">
          📁
        </span>
      }
      disabled={store.saving}
      onClick={() => void store.pickFolder()}
    >
      {t("nav.workspaceFolder")}
    </M3RailItem>
  );
}
