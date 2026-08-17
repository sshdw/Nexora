import { useState } from "react";

import type { SupportedProvider } from "../lib/tauri";
import { useProviders } from "../lib/useProviders";

export interface SettingsViewProps {
  onClose: () => void;
}

export default function SettingsView({ onClose }: SettingsViewProps) {
  const store = useProviders();
  const [draftKeys, setDraftKeys] = useState<Record<string, string>>({});

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

  return (
    <div className="nex-settings">
      <header className="nex-settings-header">
        <h2 className="nex-settings-title">Settings</h2>
        <button type="button" className="nex-btn nex-btn-ghost" onClick={onClose}>
          Back to conversations
        </button>
      </header>

      <div className="nex-settings-body">
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
              {selectedModels.length === 0 && <option value="">Select a provider first</option>}
              {selectedModels.map((model) => (
                <option key={model} value={model}>
                  {model}
                </option>
              ))}
            </select>
          </div>
        </section>

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
      </div>
    </div>
  );
}