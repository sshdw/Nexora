//! Appearance section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`; state stays in the parent).

import type { Locale } from "../../lib/strings";
import type { AppearanceStore } from "../../lib/useAppearance";
import { useStrings } from "../../lib/useLocale";
import M3SegmentedGroup from "../M3SegmentedGroup";

export interface AppearanceSectionProps {
  appearance: AppearanceStore;
  locale: Locale;
  onLocaleChange: (locale: Locale) => void;
}

export default function AppearanceSection({ appearance, locale, onLocaleChange }: AppearanceSectionProps) {
  const { t } = useStrings();
  return (
    <section className="nex-settings-section" aria-labelledby="appearance-heading">
      <h3 id="appearance-heading" className="nex-settings-heading">
        {t("settings.sectionAppearance")}
      </h3>
      <p className="nex-settings-hint">
        {t("settings.appearanceHint")}
      </p>
      <div className="nex-settings-field">
        <span className="nex-settings-label" id="theme-label">
          {t("settings.themeLabel")}
        </span>
        <M3SegmentedGroup
          labelledBy="theme-label"
          value={appearance.theme}
          onChange={(theme) => void appearance.setTheme(theme)}
          options={[
            { value: "dark", label: t("settings.themeDark") },
            { value: "light", label: t("settings.themeLight") },
          ]}
        />
        <p className="nex-settings-hint">
          {t("settings.themeProvisional")}
        </p>
      </div>
      <div className="nex-settings-field">
        <span className="nex-settings-label" id="language-label">
          {t("settings.languageLabel")}
        </span>
        <M3SegmentedGroup
          labelledBy="language-label"
          value={locale}
          onChange={onLocaleChange}
          options={[
            { value: "en", label: t("settings.langEn") },
            { value: "ru", label: t("settings.langRu") },
          ]}
        />
        <p className="nex-settings-hint">
          {t("settings.languageHint")}
        </p>
      </div>
    </section>
  );
}
