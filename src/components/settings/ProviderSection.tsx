//! Provider & model section (AC-208 split, per the review §3.1 plan — JSX
//! moved verbatim from `SettingsView.tsx`). The custom-model draft state and
//! the handlers stay in the parent so a section round trip preserves the
//! typed draft exactly as before the split.

import type { SupportedProvider } from "../../lib/tauri";
import type { ProvidersStore } from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";

export interface ProviderSectionProps {
  store: ProvidersStore;
  selectedDefinition: SupportedProvider | null;
  selectedModels: string[];
  /** The persisted selection, or the provider default. */
  effectiveModel: string | null;
  /** Non-null means the Custom… option is active. */
  customActive: boolean;
  customValue: string;
  onProviderChange: (name: string) => void;
  onModelSelect: (value: string) => void;
  onCustomDraftChange: (value: string) => void;
  onCustomCommit: () => void;
}

export default function ProviderSection({
  store,
  selectedDefinition,
  selectedModels,
  effectiveModel,
  customActive,
  customValue,
  onProviderChange,
  onModelSelect,
  onCustomDraftChange,
  onCustomCommit,
}: ProviderSectionProps) {
  const { t } = useStrings();
  return (
    <section
      className="nex-settings-section"
      aria-labelledby="selection-heading"
    >
      <h3 id="selection-heading" className="nex-settings-heading">
        {t("settings.sectionProvider")}
      </h3>
      <p className="nex-settings-hint">{t("settings.providerHint")}</p>
      <p className="nex-settings-hint">{t("settings.providerStaleHint")}</p>

      <div className="nex-settings-field">
        <label className="nex-settings-label" htmlFor="provider-select">
          {t("settings.providerLabel")}
        </label>
        <select
          id="provider-select"
          className="nex-select"
          value={selectedDefinition?.name ?? ""}
          onChange={(event) => onProviderChange(event.target.value)}
        >
          <option value="" disabled>
            {t("settings.selectProvider")}
          </option>
          {store.providers.map(({ supported, available }) => (
            <option key={supported.name} value={supported.name}>
              {supported.display_name}
              {available
                ? t("settings.readySuffix")
                : t("settings.notConnectedSuffix")}
            </option>
          ))}
        </select>
      </div>

      <div className="nex-settings-field">
        <label className="nex-settings-label" htmlFor="model-select">
          {t("settings.modelLabel")}
        </label>
        <select
          id="model-select"
          className="nex-select"
          value={customActive ? "__custom__" : (effectiveModel ?? "")}
          disabled={selectedModels.length === 0}
          onChange={(event) => onModelSelect(event.target.value)}
        >
          {selectedModels.length === 0 && (
            <option value="">{t("settings.selectProviderFirst")}</option>
          )}
          {selectedModels.map((model) => (
            <option key={model} value={model}>
              {model}
            </option>
          ))}
          {selectedModels.length > 0 && (
            <option value="__custom__">{t("settings.customOpt")}</option>
          )}
        </select>
      </div>

      {customActive && selectedModels.length > 0 && (
        <div className="nex-settings-field">
          <label className="nex-settings-label" htmlFor="model-custom">
            {t("settings.customModelLabel")}
          </label>
          <input
            id="model-custom"
            className="nex-input"
            type="text"
            value={customValue}
            placeholder={t("settings.customModelPh")}
            onChange={(event) => onCustomDraftChange(event.target.value)}
            onBlur={onCustomCommit}
            onKeyDown={(event) => {
              // shortcut:settings.custom-model-commit.
              if (event.key === "Enter") onCustomCommit();
            }}
          />
          <p className="nex-settings-hint">{t("settings.customModelHint")}</p>
        </div>
      )}
    </section>
  );
}
