//! Data-management section (AC-208 split, per the review §3.1 plan — JSX
//! moved verbatim from `SettingsView.tsx`): import/export entry points plus
//! the clear-all danger zone. There is deliberately NO typed-phrase gate and
//! NO in-app confirm dialog: the confirmation is the blocking NATIVE OS
//! prompt the `clearApplicationData` wrapper raises (NEX-SEC-004).

import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";

export interface DataSectionProps {
  /** Whether the clear-all call is in flight. */
  clearing: boolean;
  clearError: string | null;
  onClearData: () => void;
  onOpenImport?: () => void;
  onExportActive?: () => void;
}

export default function DataSection({
  clearing,
  clearError,
  onClearData,
  onOpenImport,
  onExportActive,
}: DataSectionProps) {
  const { t } = useStrings();
  return (
    <section className="nex-settings-section" aria-labelledby="data-heading">
      <h3 id="data-heading" className="nex-settings-heading">
        {t("settings.sectionData")}
      </h3>
      <p className="nex-settings-hint">{t("settings.dataHint")}</p>

      {(onOpenImport || onExportActive) && (
        <div className="nex-settings-field">
          <span className="nex-settings-label">{t("settings.transfer")}</span>
          <div className="nex-provider-actions">
            {onExportActive && (
              <M3Button variant="primary" size="sm" onClick={onExportActive}>
                {t("settings.exportActive")}
              </M3Button>
            )}
            {onOpenImport && (
              <M3Button variant="quiet" size="sm" onClick={onOpenImport}>
                {t("settings.importBtn")}
              </M3Button>
            )}
          </div>
        </div>
      )}

      <div className="nex-danger-zone">
        <h4 className="nex-danger-title">{t("settings.dangerTitle")}</h4>
        <p className="nex-danger-text">{t("settings.dangerText")}</p>
        <M3Button
          variant="destructive"
          loading={clearing}
          onClick={onClearData}
        >
          {clearing ? t("settings.clearing") : t("settings.clearBtn")}
        </M3Button>
        {clearError && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {clearError}
          </p>
        )}
      </div>
    </section>
  );
}
