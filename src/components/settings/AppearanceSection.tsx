//! Appearance section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`; state stays in the parent).

import type { Locale } from "../../lib/strings";
import type { ResolvedSettingEntry } from "../../lib/tauri";
import type { AppearanceStore, Theme } from "../../lib/useAppearance";
import { useStrings } from "../../lib/useLocale";
import SegmentedGroup from "../SegmentedGroup";
import SettingSourceBadge from "./SettingSourceBadge";

export interface AppearanceSectionProps {
  appearance: AppearanceStore;
  locale: Locale;
  onLocaleChange: (locale: Locale) => void;
  effectiveTheme?: Theme;
  onThemeChange?: (theme: Theme) => void;
  resolvedEntries?: Record<string, ResolvedSettingEntry> | null;
}

export default function AppearanceSection({
  appearance,
  locale,
  onLocaleChange,
  effectiveTheme,
  onThemeChange,
  resolvedEntries,
}: AppearanceSectionProps) {
  const { t } = useStrings();
  const displayedTheme = effectiveTheme ?? appearance.theme;
  const handleThemeChange = onThemeChange ?? ((theme: Theme) => void appearance.setTheme(theme));
  return (
    <section
      className="nex-settings-section"
      aria-labelledby="appearance-heading"
    >
      <h3 id="appearance-heading" className="nex-settings-heading">
        {t("settings.sectionAppearance")}
      </h3>
      <p className="nex-settings-hint">{t("settings.appearanceHint")}</p>
      <div className="nex-settings-field">
        <div className="nex-settings-label-row">
          <span className="nex-settings-label" id="theme-label">
            {t("settings.themeLabel")}
          </span>
          <SettingSourceBadge entry={resolvedEntries?.["appearance.theme"]} />
        </div>
        <SegmentedGroup
          labelledBy="theme-label"
          value={displayedTheme}
          onChange={handleThemeChange}
          options={[
            { value: "dark", label: t("settings.themeDark") },
            { value: "light", label: t("settings.themeLight") },
          ]}
        />
        <p className="nex-settings-hint">{t("settings.themeProvisional")}</p>
      </div>
      <div className="nex-settings-field">
        <span className="nex-settings-label" id="language-label">
          {t("settings.languageLabel")}
        </span>
        <SegmentedGroup
          labelledBy="language-label"
          value={locale}
          onChange={onLocaleChange}
          options={[
            { value: "en", label: t("settings.langEn") },
            { value: "ru", label: t("settings.langRu") },
          ]}
        />
        <p className="nex-settings-hint">{t("settings.languageHint")}</p>
      </div>
    </section>
  );
}
