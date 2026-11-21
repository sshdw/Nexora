//! Provider & model section (AC-208 split, per the review §3.1 plan — JSX
//! moved verbatim from `SettingsView.tsx`). The custom-model draft state and
//! the handlers stay in the parent so a section round trip preserves the
//! typed draft exactly as before the split.

import { useEffect, useState } from "react";

import type {
  ConnectionModelCapabilities,
  ResolvedSettingEntry,
  SettingScope,
  SupportedProvider,
} from "../../lib/tauri";
import {
  CHATGPT_PLAN_PROVIDER,
  validateCompatBaseUrl,
  type ProvidersStore,
} from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";
import Button from "../Button";
import SettingSourceBadge from "./SettingSourceBadge";

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
  saveScope?: SettingScope;
  hasProjectWorkspace?: boolean;
  resolvedEntries?: Record<string, ResolvedSettingEntry> | null;
  onCapabilitySaved?: () => void;
  onScopeBlockedError?: (message: string) => void;
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
  saveScope = "personal",
  hasProjectWorkspace = true,
  resolvedEntries,
  onCapabilitySaved,
  onScopeBlockedError,
}: ProviderSectionProps) {
  const { t } = useStrings();
  const activeModelId = customActive
    ? customValue.trim()
    : (effectiveModel ?? "");
  const activeCapabilities: ConnectionModelCapabilities | null =
    selectedDefinition && activeModelId
      ? (store.capabilities?.find(
          (c) =>
            c.connection_id === selectedDefinition.name &&
            c.model_id === activeModelId
        ) ??
        store.selectedCapabilities ??
        null)
      : null;

  const [ctxDraft, setCtxDraft] = useState("");
  const [effortsDraft, setEffortsDraft] = useState<string[]>([]);
  const [thinkingDraft, setThinkingDraft] = useState(false);
  const [toolsDraft, setToolsDraft] = useState(false);
  const [attachmentsDraft, setAttachmentsDraft] = useState(false);

  const isCompat = selectedDefinition?.name === "openai_compat";
  const isPlan = selectedDefinition?.name === CHATGPT_PLAN_PROVIDER;
  const [compatBaseUrl, setCompatBaseUrl] = useState("");
  const [compatModel, setCompatModel] = useState("");
  const [compatOrg, setCompatOrg] = useState("");
  const [compatSupportsTools, setCompatSupportsTools] = useState(true);
  const [compatSubmitted, setCompatSubmitted] = useState(false);
  const [compatDirty, setCompatDirty] = useState(false);

  useEffect(() => {
    setCompatDirty(false);
    setCompatSubmitted(false);
  }, [selectedDefinition?.name]);

  useEffect(() => {
    if (!store.compatConfig || compatDirty) return;
    setCompatBaseUrl(store.compatConfig.base_url);
    setCompatModel(store.compatConfig.model);
    setCompatOrg(store.compatConfig.organization ?? "");
    setCompatSupportsTools(store.compatConfig.supports_tools);
    setCompatSubmitted(false);
  }, [store.compatConfig, compatDirty]);

  const compatUrlError = isCompat ? validateCompatBaseUrl(compatBaseUrl) : null;
  const compatModelError =
    isCompat && compatModel.trim() === ""
      ? ("settings.compatErrMissingModel" as const)
      : null;
  const showCompatUrlError =
    compatUrlError !== null && (compatSubmitted || compatBaseUrl.trim() !== "");

  const handleSaveCompat = () => {
    setCompatSubmitted(true);
    if (compatUrlError || compatModelError || !store.saveCompatConfig) {
      return;
    }
    setCompatDirty(false);
    void store.saveCompatConfig({
      base_url: compatBaseUrl.trim(),
      model: compatModel.trim(),
      organization: compatOrg.trim() || null,
      headers: store.compatConfig?.headers ?? [],
      supports_tools: compatSupportsTools,
    });
  };

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

  const handleSaveOverride = async () => {
    if (!selectedDefinition || !activeModelId || !store.setCapabilityOverride) {
      return;
    }
    if (saveScope === "project" && !hasProjectWorkspace) {
      onScopeBlockedError?.(t("settings.capabilitiesScopeMissingWorkspace"));
      return;
    }
    const parsedCtx = Number.parseInt(ctxDraft.trim(), 10);
    const updated = await store.setCapabilityOverride(
      selectedDefinition.name,
      activeModelId,
      {
        context_window:
          Number.isFinite(parsedCtx) && parsedCtx > 0 ? parsedCtx : null,
        supported_efforts: effortsDraft,
        supports_thinking: thinkingDraft,
        supports_tools: toolsDraft,
        supports_attachments: attachmentsDraft,
      },
      saveScope
    );
    if (updated) onCapabilitySaved?.();
  };

  const handleClearOverride = async () => {
    if (!selectedDefinition || !activeModelId) {
      return;
    }
    if (saveScope === "project" && !hasProjectWorkspace) {
      onScopeBlockedError?.(t("settings.capabilitiesScopeMissingWorkspace"));
      return;
    }
    const clearFn =
      store.clearCapabilityOverride ??
      (store.setCapabilityOverride
        ? (conn: string, mod: string, scope?: SettingScope) =>
            store.setCapabilityOverride(conn, mod, null, scope)
        : null);
    if (!clearFn) return;
    const updated = await clearFn(
      selectedDefinition.name,
      activeModelId,
      saveScope
    );
    if (updated) onCapabilitySaved?.();
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
        <div className="nex-settings-label-row">
          <label className="nex-settings-label" htmlFor="provider-select">
            {t("settings.providerLabel")}
          </label>
          <SettingSourceBadge entry={resolvedEntries?.["provider.selected"]} />
        </div>
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

      {isPlan && store.chatgptOAuth?.connected && (
        <p className="nex-optin-notice" role="status">
          {t("settings.chatgptOAuthProviderNote")}
        </p>
      )}

      {isCompat && (
        <div className="nex-settings-details">
          <div className="nex-capabilities-panel">
            <h4 className="nex-settings-label">
              {t("settings.compatHeading")}
            </h4>
            {store.compatStatus && (
              <p className="nex-settings-hint">
                {store.compatStatus.ready
                  ? t("settings.compatReady")
                  : t("settings.compatNotReady")}
              </p>
            )}
            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="compat-base-url">
                {t("settings.compatBaseUrl")}
              </label>
              <input
                id="compat-base-url"
                className="nex-input"
                type="url"
                value={compatBaseUrl}
                placeholder={t("settings.compatBaseUrlPh")}
                onChange={(event) => {
                  setCompatDirty(true);
                  setCompatBaseUrl(event.target.value);
                }}
              />
              {showCompatUrlError ? (
                <p className="nex-settings-error" role="alert">
                  {t(compatUrlError)}
                </p>
              ) : (
                <p className="nex-settings-hint">
                  {t("settings.compatBaseUrlHint")}
                </p>
              )}
            </div>

            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="compat-model">
                {t("settings.compatModel")}
              </label>
              <input
                id="compat-model"
                className="nex-input"
                type="text"
                value={compatModel}
                placeholder={t("settings.compatModelPh")}
                onChange={(event) => {
                  setCompatDirty(true);
                  setCompatModel(event.target.value);
                }}
              />
              {compatSubmitted && compatModelError && (
                <p className="nex-settings-error" role="alert">
                  {t(compatModelError)}
                </p>
              )}
            </div>

            <div className="nex-settings-field">
              <label
                className="nex-settings-label"
                htmlFor="compat-organization"
              >
                {t("settings.compatOrganization")}
              </label>
              <input
                id="compat-organization"
                className="nex-input"
                type="text"
                value={compatOrg}
                placeholder={t("settings.compatOrganizationPh")}
                onChange={(event) => {
                  setCompatDirty(true);
                  setCompatOrg(event.target.value);
                }}
              />
            </div>

            <div className="nex-settings-field">
              <label className="nex-capabilities-check">
                <input
                  id="compat-supports-tools"
                  type="checkbox"
                  checked={compatSupportsTools}
                  onChange={(event) => {
                    setCompatDirty(true);
                    setCompatSupportsTools(event.target.checked);
                  }}
                />
                <span>{t("settings.compatSupportsTools")}</span>
              </label>
            </div>

            <div className="nex-capabilities-actions">
              <Button
                variant="secondary"
                disabled={store.compatSaving}
                onClick={handleSaveCompat}
              >
                {store.compatSaving
                  ? t("settings.compatSaving")
                  : t("settings.compatSave")}
              </Button>
            </div>
          </div>
        </div>
      )}

      <div className="nex-settings-field">
        <div className="nex-settings-label-row">
          <label className="nex-settings-label" htmlFor="model-select">
            {t("settings.modelLabel")}
          </label>
          <SettingSourceBadge entry={resolvedEntries?.["provider.model"]} />
        </div>
        <select
          id="model-select"
          className="nex-select"
          value={
            !isPlan && customActive
              ? "__custom__"
              : (effectiveModel ?? selectedModels[0] ?? "")
          }
          disabled={selectedModels.length === 0}
          onChange={(event) => onModelSelect(event.target.value)}
        >
          {selectedModels.length === 0 && (
            <option value="">
              {isCompat
                ? t("settings.compatNoModelsYet")
                : t("settings.selectProviderFirst")}
            </option>
          )}
          {selectedModels.map((model) => (
            <option key={model} value={model}>
              {(isPlan &&
                store.chatgptOAuth?.models?.find((item) => item.slug === model)
                  ?.display_name) ||
                model}
            </option>
          ))}
          {!isPlan && selectedModels.length > 0 && (
            <option value="__custom__">{t("settings.customOpt")}</option>
          )}
        </select>
        {!isPlan &&
          selectedDefinition &&
          store.refreshModelMetadata &&
          !(activeModelId && activeCapabilities) && (
            <div className="nex-capabilities-actions">
              <Button
                variant="quiet"
                onClick={() =>
                  void store.refreshModelMetadata(selectedDefinition.name)
                }
              >
                {t("settings.capabilitiesRefreshMeta")}
              </Button>
            </div>
          )}
      </div>

      {!isPlan && customActive && selectedModels.length > 0 && (
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
              <SettingSourceBadge
                entry={resolvedEntries?.["provider.capability_overrides"]}
              />
              {!isPlan && store.refreshModelMetadata && (
                <Button
                  variant="quiet"
                  onClick={() =>
                    void store.refreshModelMetadata(selectedDefinition.name)
                  }
                >
                  {t("settings.capabilitiesRefreshMeta")}
                </Button>
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
              <Button
                variant="secondary"
                onClick={() => void handleSaveOverride()}
              >
                {t("settings.capabilitiesSaveOverride")}
              </Button>
              {activeCapabilities.has_manual_override && (
                <Button
                  variant="quiet"
                  onClick={() => void handleClearOverride()}
                >
                  {t("settings.capabilitiesClearOverride")}
                </Button>
              )}
            </div>
          </div>
        </details>
      )}
    </section>
  );
}
