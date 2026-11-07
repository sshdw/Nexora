//! Credentials section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`). The typed key drafts stay in the
//! parent (and are cleared only on a successful connect), so a section
//! round trip preserves them exactly as before the split.

import type { SupportedProvider } from "../../lib/tauri";
import type { ProvidersStore } from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";

export interface CredentialsSectionProps {
  store: ProvidersStore;
  draftKeys: Record<string, string>;
  onDraftChange: (name: string, value: string) => void;
  onConnect: (definition: SupportedProvider) => void;
  onDisconnect: (definition: SupportedProvider) => void;
}

export default function CredentialsSection({
  store,
  draftKeys,
  onDraftChange,
  onConnect,
  onDisconnect,
}: CredentialsSectionProps) {
  const { t } = useStrings();
  return (
    <section className="nex-settings-section" aria-labelledby="providers-heading">
      <h3 id="providers-heading" className="nex-settings-heading">
        {t("settings.searchProviderCreds")}
      </h3>
      <p className="nex-settings-hint">
        {t("settings.credentialsHint")}
      </p>

      <ul className="nex-provider-list">
        {store.providers.map(({ supported, credentialed, available }) => (
          <li key={supported.name} className="nex-provider-row">
            <div className="nex-provider-meta">
              <span className="nex-provider-name">{supported.display_name}</span>
              <span className="nex-provider-status">
                <span
                  className={
                    "nex-tag" +
                    (available ? " is-connected" : "")
                  }
                >
                  <span
                    className={
                      "nex-tag-dot" +
                      (available ? " is-ok" : credentialed ? "" : "")
                    }
                    aria-hidden="true"
                  />
                  {available
                    ? t("settings.connected")
                    : credentialed
                      ? t("settings.credentialSaved")
                      : t("settings.notConnected")}
                </span>
              </span>
            </div>

            <div className="nex-provider-actions">
              <input
                className="nex-input"
                type="password"
                autoComplete="new-password"
                placeholder={credentialed ? t("settings.apiKeyUpdatePh") : t("settings.apiKeyPh")}
                aria-label={t("settings.apiKeyAria", { name: supported.display_name })}
                aria-describedby={
                  store.error ? "nex-settings-store-error" : undefined
                }
                value={draftKeys[supported.name] ?? ""}
                disabled={store.working}
                onChange={(event) => onDraftChange(supported.name, event.target.value)}
              />
              <M3Button
                variant="primary"
                size="sm"
                disabled={store.working || !(draftKeys[supported.name]?.trim())}
                onClick={() => onConnect(supported)}
              >
                {credentialed ? t("settings.update") : t("settings.connect")}
              </M3Button>
              {credentialed && (
                <M3Button
                  variant="quiet"
                  className="nex-provider-remove"
                  disabled={store.working}
                  onClick={() => onDisconnect(supported)}
                >
                  {t("settings.disconnect")}
                </M3Button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
