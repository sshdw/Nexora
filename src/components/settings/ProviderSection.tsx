//! Provider & model section (AC-208 split, per the review §3.1 plan — JSX
//! moved verbatim from `SettingsView.tsx`). The custom-model draft state and
//! the handlers stay in the parent so a section round trip preserves the
//! typed draft exactly as before the split.

import { useEffect, useState } from "react";

import type {
  ConnectionModelCapabilities,
  SupportedProvider,
} from "../../lib/tauri";
import type { ProvidersStore } from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";

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
  const activeModelId = customActive ? customValue.trim() : (effectiveModel ?? "");
  const activeCapabilities: ConnectionModelCapabilities | null =
    selectedDefinition && activeModelId
      ? (store.capabilities?.find(
          (c) =>
            c.connection_id === selectedDefinition.name &&
            c.model_id === activeModelId
        ) ?? store.selectedCapabilities ?? null)
      : null;

  const [ctxDraft, setCtxDraft] = useState("");
  const [effortsDraft, setEffortsDraft] = useState<string[]>([]);
  const [thinkingDraft, setThinkingDraft] = useState(false);
  const [toolsDraft, setToolsDraft] = useState(false);
  const [attachmentsDraft, setAttachmentsDraft] = useState(false);

  useEffect(() => {
    if (!activeCapabilities) return;
    setCtxDraft(
      activeCapabilities.context_window !== null
        ? String(activeCapabilities.context_window)
        : ""
    );
    setEffortsDraft(activeCapabilities.supported_efforts);
    setThinkingDraft(activeCapabilities.supports_thinking);
    setToolsDraft(activeCapabilities.supports_tools);
    setAttachmentsDraft(activeCapabilities.supports_attachments);
  }, [activeCapabilities]);

  const toggleEffort = (level: string) => {
    setEffortsDraft((prev) =>
      prev.includes(level) ? prev.filter((x) => x !== level) : [...prev, level]
    );
  };

  const handleSaveOverride = () => {
    if (!selectedDefinition || !activeModelId || !store.setCapabilityOverride) {
      return;
    }
    const parsedCtx = Number.parseInt(ctxDraft.trim(), 10);
    void store.setCapabilityOverride(selectedDefinition.name, activeModelId, {
      context_window:
        Number.isFinite(parsedCtx) && parsedCtx > 0 ? parsedCtx : null,
      supported_efforts: effortsDraft,
      supports_thinking: thinkingDraft,
      supports_tools: toolsDraft,
      supports_attachments: attachmentsDraft,
    });
  };

  const handleClearOverride = () => {
    if (!selectedDefinition || !activeModelId || !store.setCapabilityOverride) {
      return;
    }
    void store.setCapabilityOverride(selectedDefinition.name, activeModelId, null);
  };

  const provenanceLabel = (
    p: ConnectionModelCapabilities["provenance"]
  ): string => {
    switch (p) {
      case "manual_override":
        return t("conv.provenance.manual_override");
      case "api_metadata":
        return t("conv.provenance.api_metadata");
      case "built_in_catalog":
        return t("conv.provenance.built_in_catalog");
      default:
        return t("conv.provenance.unknown");
    }
  };
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

      {selectedDefinition && activeModelId && activeCapabilities && (
        <details className="nex-settings-details nex-capabilities-details">
          <summary>{t("settings.capabilitiesHeading")}</summary>
          <div className="nex-capabilities-panel">
            <div className="nex-capabilities-meta">
              <span className="nex-settings-hint">
                {t("settings.capabilitiesProvenance", {
                  source: provenanceLabel(activeCapabilities.provenance),
                })}
              </span>
              {store.refreshModelMetadata && (
                <M3Button
                  variant="quiet"
                  onClick={() =>
                    void store.refreshModelMetadata(selectedDefinition.name)
                  }
                >
                  {t("settings.capabilitiesRefreshMeta")}
                </M3Button>
              )}
            </div>

            <div className="nex-settings-field">
              <label
                className="nex-settings-label"
                htmlFor="cap-context-window"
              >
                {t("settings.capabilitiesContext")}
              </label>
              <input
                id="cap-context-window"
                className="nex-input"
                type="number"
                min={1}
                value={ctxDraft}
                onChange={(e) => setCtxDraft(e.target.value)}
              />
            </div>

            <div className="nex-settings-field">
              <span className="nex-settings-label">
                {t("settings.capabilitiesEfforts")}
              </span>
              <div className="nex-capabilities-efforts">
                {(["low", "medium", "high", "xhigh"] as const).map((level) => (
                  <label key={level} className="nex-capabilities-check">
                    <input
                      type="checkbox"
                      checked={effortsDraft.includes(level)}
                      onChange={() => toggleEffort(level)}
                    />
                    <span>{level}</span>
                  </label>
                ))}
              </div>
            </div>

            <div className="nex-capabilities-toggles">
              <label className="nex-capabilities-check">
                <input
                  type="checkbox"
                  checked={thinkingDraft}
                  onChange={(e) => setThinkingDraft(e.target.checked)}
                />
                <span>{t("settings.capabilitiesThinking")}</span>
              </label>
              <label className="nex-capabilities-check">
                <input
                  type="checkbox"
                  checked={toolsDraft}
                  onChange={(e) => setToolsDraft(e.target.checked)}
                />
                <span>{t("settings.capabilitiesTools")}</span>
              </label>
              <label className="nex-capabilities-check">
                <input
                  type="checkbox"
                  checked={attachmentsDraft}
                  onChange={(e) => setAttachmentsDraft(e.target.checked)}
                />
                <span>{t("settings.capabilitiesAttachments")}</span>
              </label>
            </div>

            <div className="nex-capabilities-actions">
              <M3Button variant="secondary" onClick={handleSaveOverride}>
                {t("settings.capabilitiesSaveOverride")}
              </M3Button>
              {activeCapabilities.has_manual_override && (
                <M3Button variant="quiet" onClick={handleClearOverride}>
                  {t("settings.capabilitiesClearOverride")}
                </M3Button>
              )}
            </div>
          </div>
        </details>
      )}
    </section>
  );
}
