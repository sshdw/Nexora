//! Workspace section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`): read-only view of the current root and
//! the recent-folders list. The loaded list state stays in the parent so the
//! once-per-mount fetch is not re-fired by a section remount.

import { useStrings } from "../../lib/useLocale";

export interface WorkspaceSectionProps {
  workspaceRoot: string | null;
  workspaceLoading: boolean;
  /** `null` until the once-per-mount load answers. */
  recentFolders: string[] | null;
}

export default function WorkspaceSection({
  workspaceRoot,
  workspaceLoading,
  recentFolders,
}: WorkspaceSectionProps) {
  const { t } = useStrings();
  return (
    <section className="nex-settings-section" aria-labelledby="workspace-heading">
      <h3 id="workspace-heading" className="nex-settings-heading">
        {t("settings.sectionWorkspace")}
      </h3>
      <p className="nex-settings-hint">
        {t("settings.workspaceHint")}
      </p>
      <div className="nex-settings-field">
        <span className="nex-settings-label">{t("settings.currentRoot")}</span>
        <p className="nex-settings-value" title={workspaceRoot ?? ""}>
          {workspaceLoading ? t("common.loading") : (workspaceRoot ?? t("settings.unset"))}
        </p>
      </div>
      <div className="nex-settings-field">
        <span className="nex-settings-label">{t("settings.recentLabel")}</span>
        {recentFolders === null ? (
          <p className="nex-settings-hint">{t("common.loading")}</p>
        ) : recentFolders.length === 0 ? (
          <p className="nex-settings-hint">{t("settings.noRecent")}</p>
        ) : (
          recentFolders.map((folder) => (
            <p key={folder} className="nex-settings-value" title={folder}>
              {folder}
            </p>
          ))
        )}
      </div>
    </section>
  );
}
