import { useState } from "react";

import type { SupportedProvider } from "../lib/tauri";
import { clearApplicationData } from "../lib/tauri";
import type { AppearanceStore } from "../lib/useAppearance";
import type { ProvidersStore } from "../lib/useProviders";

/** Settings sections (Phase 10.8). Only approved areas with defined behavior
 * are offered: Appearance (theme), Provider & model (FR-004), Data management
 * (FR-013 clear-all), and Provider credentials (FR-014). Conversation and
 * export preferences have no defined implementation behavior and are therefore
 * intentionally absent (no invented MVP settings). */
type SettingsSectionId = "appearance" | "provider" | "data" | "credentials";

const NAV: { label: string; items: { id: SettingsSectionId; t: string }[] }[] = [
  { label: "General", items: [{ id: "appearance", t: "Appearance" }] },
  { label: "AI", items: [{ id: "provider", t: "Provider & model" }] },
  { label: "Data", items: [{ id: "data", t: "Data management" }] },
  { label: "Providers", items: [{ id: "credentials", t: "Credentials" }] },
];

/** The exact phrase the backend's `clear_application_data` command requires
 * before it performs any destructive write (application::data_management::
 * CONFIRMATION — FR-013 AC-5). The user must type it explicitly. */
const CLEAR_CONFIRMATION_PHRASE = "confirm";

export interface SettingsViewProps {
  onClose: () => void;
  /** Shared provider/model/credential store lifted in App (single source). */
  store: ProvidersStore;
  /** Persisted appearance preference lifted in App so it loads at startup. */
  appearance: AppearanceStore;
  /** Refresh conversation-dependent UI after all local data is cleared. */
  onDataCleared: () => void;
}

export default function SettingsView({
  onClose,
  store,
  appearance,
  onDataCleared,
}: SettingsViewProps) {
  const [section, setSection] = useState<SettingsSectionId>("appearance");
  const [draftKeys, setDraftKeys] = useState<Record<string, string>>({});
  // Clear-all-data confirmation state (typed phrase; no accidental runs).
  const [confirmingClear, setConfirmingClear] = useState(false);
  const [clearPhrase, setClearPhrase] = useState("");
  const [clearError, setClearError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);

  const selected = store.providers.find((p) => p.supported.name === store.selectedProvider) ?? null;
  const selectedDefinition: SupportedProvider | null = selected ? selected.supported : null;
  const selectedModels = selectedDefinition ? selectedDefinition.models : [];

  /** The model to display: the persisted selection, or the provider default. */
  const effectiveModel =
    store.selectedModel && selectedModels.includes(store.selectedModel)
      ? store.selectedModel
      : selectedModels[0] ?? null;

  const handleProviderChange = async (name: string) => {
    await store.selectProvider(name);
    // Persist the default model for the newly selected provider so the
    // selection is never left without a model.
    const def = store.providers.find((p) => p.supported.name === name)?.supported;
    if (def && def.models.length > 0) {
      await store.selectModel(def.models[0]);
    }
  };

  const handleConnect = async (definition: SupportedProvider) => {
    const credential = draftKeys[definition.name]?.trim() ?? "";
    if (!credential) return;
    await store.connect(definition.name, definition.display_name, credential);
    // Never keep the secret in component state after it is stored.
    setDraftKeys((prev) => ({ ...prev, [definition.name]: "" }));
  };

  const handleDisconnect = async (definition: SupportedProvider) => {
    await store.disconnect(definition.name);
  };

  const openClearConfirmation = () => {
    setClearPhrase("");
    setClearError(null);
    setConfirmingClear(true);
  };

  const cancelClearConfirmation = () => {
    setConfirmingClear(false);
    setClearPhrase("");
    setClearError(null);
  };

  const handleClearData = async () => {
    if (clearing) return;
    if (clearPhrase !== CLEAR_CONFIRMATION_PHRASE) {
      setClearError(`Type "${CLEAR_CONFIRMATION_PHRASE}" to confirm.`);
      return;
    }
    setClearing(true);
    setClearError(null);
    try {
      // The backend refuses to run unless the phrase matches exactly and
      // clears everything atomically — a failure leaves all data intact.
      await clearApplicationData(CLEAR_CONFIRMATION_PHRASE);
      // The cleared settings included the provider/model selection.
      await store.reload();
      onDataCleared();
      cancelClearConfirmation();
    } catch (e) {
      setClearError(
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : "Unable to clear application data.",
      );
    } finally {
      setClearing(false);
    }
  };

  return (
    <div className="nex-settings">
      <header className="nex-settings-header">
        <h2 className="nex-settings-title">Settings</h2>
        <button type="button" className="nex-btn nex-btn-ghost" onClick={onClose}>
          Back to conversations
        </button>
      </header>

      <div className="nex-settings-layout">
        <nav className="nex-settings-nav" aria-label="Settings sections">
          {NAV.map((group) => (
            <div key={group.label} className="nex-settings-nav-group">
              <p className="nex-settings-nav-label">{group.label}</p>
              {group.items.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className={`nex-settings-nav-item${section === item.id ? " is-active" : ""}`}
                  aria-current={section === item.id ? "true" : undefined}
                  onClick={() => setSection(item.id)}
                >
                  {item.t}
                </button>
              ))}
            </div>
          ))}
        </nav>

        <div className="nex-settings-body">
          <div className="nex-settings-inner">
            {section === "appearance" && (
              <section className="nex-settings-section" aria-labelledby="appearance-heading">
                <h3 id="appearance-heading" className="nex-settings-heading">
                  Appearance
                </h3>
                <p className="nex-settings-hint">
                  Visual theme for this device. Applied immediately and persisted between
                  sessions.
                </p>
                <div className="nex-settings-field">
                  <span className="nex-settings-label" id="theme-label">
                    Theme
                  </span>
                  <div className="nex-seg" role="group" aria-labelledby="theme-label">
                    <button
                      type="button"
                      className={appearance.theme === "dark" ? "is-active" : ""}
                      aria-pressed={appearance.theme === "dark"}
                      onClick={() => void appearance.setTheme("dark")}
                    >
                      Dark
                    </button>
                    <button
                      type="button"
                      className={appearance.theme === "light" ? "is-active" : ""}
                      aria-pressed={appearance.theme === "light"}
                      onClick={() => void appearance.setTheme("light")}
                    >
                      Light
                    </button>
                  </div>
                  <p className="nex-settings-hint">
                    The light theme is provisional — the final palette is still open.
                  </p>
                </div>
              </section>
            )}

            {section === "provider" && (
              <section className="nex-settings-section" aria-labelledby="selection-heading">
                <h3 id="selection-heading" className="nex-settings-heading">
                  Provider &amp; model
                </h3>
                <p className="nex-settings-hint">
                  Choose which provider and model new requests use. The backend honors a
                  40-request-per-minute limit on outbound requests.
                </p>

                <div className="nex-settings-field">
                  <label className="nex-settings-label" htmlFor="provider-select">
                    Provider
                  </label>
                  <select
                    id="provider-select"
                    className="nex-select"
                    value={selectedDefinition?.name ?? ""}
                    onChange={(event) => handleProviderChange(event.target.value)}
                  >
                    <option value="" disabled>
                      Select a provider
                    </option>
                    {store.providers.map(({ supported, available }) => (
                      <option key={supported.name} value={supported.name}>
                        {supported.display_name}
                        {available ? " · Ready" : " · Not connected"}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="nex-settings-field">
                  <label className="nex-settings-label" htmlFor="model-select">
                    Model
                  </label>
                  <select
                    id="model-select"
                    className="nex-select"
                    value={effectiveModel ?? ""}
                    disabled={selectedModels.length === 0}
                    onChange={(event) => store.selectModel(event.target.value)}
                  >
                    {selectedModels.length === 0 && (
                      <option value="">Select a provider first</option>
                    )}
                    {selectedModels.map((model) => (
                      <option key={model} value={model}>
                        {model}
                      </option>
                    ))}
                  </select>
                </div>
              </section>
            )}

            {section === "data" && (
              <section className="nex-settings-section" aria-labelledby="data-heading">
                <h3 id="data-heading" className="nex-settings-heading">
                  Data management
                </h3>
                <p className="nex-settings-hint">
                  All application data lives in a single local SQLite database on this device.
                  Nothing is synchronized anywhere. Individual conversations and prompts are
                  managed from the sidebar and Prompt Library.
                </p>

                <div className="nex-danger-zone">
                  <h4 className="nex-danger-title">Clear all application data</h4>
                  <p className="nex-danger-text">
                    Permanently deletes every conversation, message, attachment and prompt stored
                    on this device, along with provider metadata and application settings.
                    Provider credentials in the operating system keyring are not affected. This
                    cannot be undone.
                  </p>
                  {!confirmingClear ? (
                    <button
                      type="button"
                      className="nex-btn nex-btn-ghost nex-danger-button"
                      onClick={openClearConfirmation}
                    >
                      Clear all data…
                    </button>
                  ) : (
                    <div className="nex-danger-confirm">
                      <label className="nex-settings-label" htmlFor="clear-confirm-input">
                        Type &ldquo;{CLEAR_CONFIRMATION_PHRASE}&rdquo; to confirm
                      </label>
                      <input
                        id="clear-confirm-input"
                        className="nex-input"
                        type="text"
                        value={clearPhrase}
                        autoFocus
                        disabled={clearing}
                        onChange={(event) => setClearPhrase(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") void handleClearData();
                        }}
                      />
                      {clearError && (
                        <p className="nex-settings-error" role="alert">
                          {clearError}
                        </p>
                      )}
                      <div className="nex-provider-actions">
                        <button
                          type="button"
                          className="nex-btn nex-btn-ghost"
                          disabled={clearing}
                          onClick={cancelClearConfirmation}
                        >
                          Cancel
                        </button>
                        <button
                          type="button"
                          className="nex-btn nex-btn-ghost nex-danger-button"
                          disabled={clearing || clearPhrase !== CLEAR_CONFIRMATION_PHRASE}
                          onClick={() => void handleClearData()}
                        >
                          {clearing ? "Clearing…" : "Clear all data"}
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              </section>
            )}

            {section === "credentials" && (
              <section className="nex-settings-section" aria-labelledby="providers-heading">
          <h3 id="providers-heading" className="nex-settings-heading">
            Provider credentials
          </h3>
          <p className="nex-settings-hint">
            API keys are stored in your operating system&rsquo;s secure keyring, never in the
            database, and are never shown again after saving.
          </p>

          <ul className="nex-provider-list">
            {store.providers.map(({ supported, credentialed, available }) => (
              <li key={supported.name} className="nex-provider-row">
                <div className="nex-provider-meta">
                  <span className="nex-provider-name">{supported.display_name}</span>
                  <span className="nex-provider-status">
                    {available ? "Connected" : credentialed ? "Credential saved" : "Not connected"}
                  </span>
                </div>

                <div className="nex-provider-actions">
                  <input
                    className="nex-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={credentialed ? "Update API key" : "API key"}
                    aria-label={`${supported.display_name} API key`}
                    value={draftKeys[supported.name] ?? ""}
                    disabled={store.working}
                    onChange={(event) =>
                      setDraftKeys((prev) => ({ ...prev, [supported.name]: event.target.value }))
                    }
                  />
                  <button
                    type="button"
                    className="nex-btn nex-btn-ghost"
                    disabled={store.working || !(draftKeys[supported.name]?.trim())}
                    onClick={() => handleConnect(supported)}
                  >
                    {credentialed ? "Update" : "Connect"}
                  </button>
                  {credentialed && (
                    <button
                      type="button"
                      className="nex-btn nex-btn-ghost nex-provider-remove"
                      disabled={store.working}
                      onClick={() => handleDisconnect(supported)}
                    >
                      Disconnect
                    </button>
                  )}
                </div>
              </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}