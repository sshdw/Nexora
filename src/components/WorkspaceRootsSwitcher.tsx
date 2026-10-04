import { useState } from "react";

import type { WorkspaceStore } from "../lib/useWorkspace";
import { useStrings } from "../lib/useLocale";
import M3RailItem from "./M3RailItem";

export interface WorkspaceRootsSwitcherProps {
  store: WorkspaceStore;
}

/** Multi-root registry switcher: registered roots with set-active + remove,
 * an add-folder entry, and the honest per-feature coverage note.
 * Rendered on the shared M3RailItem row primitive; tokens only, no raw
 * values (paths are user data and render raw). Removing the active root
 * falls back to the default — every root-aware feature follows on its next
 * manual refresh. */
export default function WorkspaceRootsSwitcher({ store }: WorkspaceRootsSwitcherProps) {
  const { t } = useStrings();
  const [open, setOpen] = useState(false);

  const count = store.roots.length;
  const label = count === 0 ? t("roots.title") : `${t("roots.title")} (${count})`;

  return (
    <div className="nex-roots">
      <M3RailItem
        label={label}
        icon={
          <span aria-hidden="true">
            🗂
          </span>
        }
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {label}
      </M3RailItem>
      {open && (
        <div className="nex-roots-panel">
          {count === 0 ? (
            <p className="nex-roots-note">{t("roots.empty")}</p>
          ) : (
            <ul className="nex-roots-list" aria-label={t("roots.listAria")}>
              {store.roots.map((path) => {
                const isActive = path === store.root;
                return (
                  <li key={path} className="nex-roots-row">
                    <span className="nex-roots-path" title={path}>
                      {isActive ? `● ${path}` : path}
                    </span>
                    <span className="nex-roots-actions">
                      {isActive ? (
                        <span className="nex-roots-badge">{t("roots.active")}</span>
                      ) : (
                        <button
                          type="button"
                          className="nex-roots-btn"
                          title={t("roots.useTitle")}
                          disabled={store.saving}
                          onClick={() => void store.addRoot(path)}
                        >
                          {t("roots.use")}
                        </button>
                      )}
                      <button
                        type="button"
                        className="nex-roots-btn is-danger"
                        title={t("roots.removeTitle")}
                        disabled={store.saving}
                        onClick={() => void store.removeRoot(path)}
                      >
                        {t("roots.remove")}
                      </button>
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
          <button
            type="button"
            className="nex-roots-btn is-add"
            disabled={store.saving}
            onClick={() => void store.addRootFolder()}
          >
            {t("roots.add")}
          </button>
          <p className="nex-roots-note">{t("roots.coverageNote")}</p>
        </div>
      )}
    </div>
  );
}
