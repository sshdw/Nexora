//! Credentials section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`). The typed key drafts stay in the
//! parent (and are cleared only on a successful connect), so a section
//! round trip preserves them exactly as before the split.

import { useEffect, useState } from "react";

import type { SupportedProvider } from "../../lib/tauri";
import {
  isCliAdapterProvider,
  type ProvidersStore,
} from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";
import Button from "../Button";

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
  const [oauthOptInAck, setOauthOptInAck] = useState(false);

  const refreshCliAdapters = store.refreshCliAdapters;
  useEffect(() => {
    void refreshCliAdapters?.();
  }, [refreshCliAdapters]);

  const apiProviders = store.providers.filter(
    ({ supported }) => !isCliAdapterProvider(supported.name)
  );
  const oauthConnected = Boolean(store.chatgptOAuth?.connected);
  const oauthHasRefresh = Boolean(store.chatgptOAuth?.has_refresh_token);
  const oauthBusy = Boolean(store.chatgptOAuthWorking) || store.working;
  const cliAdapters = store.cliAdapters ?? [];

  return (
    <section
      className="nex-settings-section"
      aria-labelledby="providers-heading"
    >
      <h3 id="providers-heading" className="nex-settings-heading">
        {t("settings.searchProviderCreds")}
      </h3>
      <p className="nex-settings-hint">{t("settings.credentialsHint")}</p>

      <ul className="nex-provider-list">
        {apiProviders.map(({ supported, credentialed, available }) => (
          <li key={supported.name} className="nex-provider-row">
            <div className="nex-provider-meta">
              <span className="nex-provider-name">
                {supported.display_name}
              </span>
              <span className="nex-provider-status">
                <span
                  className={"nex-tag" + (available ? " is-connected" : "")}
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
                placeholder={
                  credentialed
                    ? t("settings.apiKeyUpdatePh")
                    : t("settings.apiKeyPh")
                }
                aria-label={t("settings.apiKeyAria", {
                  name: supported.display_name,
                })}
                aria-describedby={
                  store.error ? "nex-settings-store-error" : undefined
                }
                value={draftKeys[supported.name] ?? ""}
                disabled={store.working}
                onChange={(event) =>
                  onDraftChange(supported.name, event.target.value)
                }
              />
              <Button
                variant="primary"
                size="sm"
                disabled={store.working || !draftKeys[supported.name]?.trim()}
                onClick={() => onConnect(supported)}
              >
                {credentialed ? t("settings.update") : t("settings.connect")}
              </Button>
              {credentialed && (
                <Button
                  variant="quiet"
                  className="nex-provider-remove"
                  disabled={
                    store.working ||
                    (supported.name === "openai" &&
                      Boolean(store.chatgptOAuthWorking))
                  }
                  onClick={() => onDisconnect(supported)}
                >
                  {t("settings.disconnect")}
                </Button>
              )}
            </div>
          </li>
        ))}
      </ul>

      {store.startChatGptOAuth && (
        <section
          className="nex-optin-card"
          aria-labelledby="chatgpt-oauth-heading"
        >
          <div className="nex-optin-header">
            <h4 id="chatgpt-oauth-heading" className="nex-settings-label">
              {t("settings.chatgptOAuthHeading")}
            </h4>
            <span
              className={"nex-tag" + (oauthConnected ? " is-connected" : "")}
            >
              <span
                className={"nex-tag-dot" + (oauthConnected ? " is-ok" : "")}
                aria-hidden="true"
              />
              {oauthConnected
                ? t("settings.chatgptOAuthConnected")
                : t("settings.chatgptOAuthNotConnected")}
            </span>
          </div>
          <p className="nex-optin-notice" role="note">
            {t("settings.chatgptOAuthNotice")}
          </p>
          {oauthConnected ? (
            <p className="nex-settings-hint">
              {t("settings.chatgptOAuthActiveHint")}
            </p>
          ) : (
            <label className="nex-capabilities-check">
              <input
                type="checkbox"
                checked={oauthOptInAck}
                disabled={oauthBusy}
                onChange={(event) => setOauthOptInAck(event.target.checked)}
              />
              <span>{t("settings.chatgptOAuthOptInLabel")}</span>
            </label>
          )}
          <div className="nex-optin-actions">
            {!oauthConnected ? (
              <Button
                variant="primary"
                size="sm"
                disabled={oauthBusy || !oauthOptInAck}
                onClick={() => void store.startChatGptOAuth?.()}
              >
                {store.chatgptOAuthWorking ? (
                  <span role="status">
                    {t("settings.chatgptOAuthSigningIn")}
                  </span>
                ) : (
                  t("settings.chatgptOAuthSignIn")
                )}
              </Button>
            ) : (
              <>
                {oauthHasRefresh && store.refreshChatGptOAuth && (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={oauthBusy}
                    onClick={() => void store.refreshChatGptOAuth?.()}
                  >
                    {t("settings.chatgptOAuthRefresh")}
                  </Button>
                )}
                {store.disconnectChatGptOAuth && (
                  <Button
                    variant="quiet"
                    className="nex-provider-remove"
                    disabled={oauthBusy}
                    onClick={() => void store.disconnectChatGptOAuth?.()}
                  >
                    {t("settings.chatgptOAuthDisconnect")}
                  </Button>
                )}
              </>
            )}
          </div>
        </section>
      )}

      {store.refreshCliAdapters && (
        <section
          className="nex-optin-card"
          aria-labelledby="cli-adapters-heading"
        >
          <div className="nex-optin-header">
            <h4 id="cli-adapters-heading" className="nex-settings-label">
              {t("settings.cliAdaptersHeading")}
            </h4>
            <Button
              variant="quiet"
              size="sm"
              disabled={Boolean(store.cliAdaptersLoading) || store.working}
              onClick={() => void store.refreshCliAdapters?.()}
            >
              {store.cliAdaptersLoading
                ? t("settings.cliAdaptersDetecting")
                : t("settings.cliAdaptersDetect")}
            </Button>
          </div>
          <p className="nex-settings-hint">{t("settings.cliAdaptersHint")}</p>
          <ul className="nex-cli-list">
            {cliAdapters.map((adapter) => (
              <li key={adapter.id} className="nex-cli-row">
                <div className="nex-cli-meta">
                  <span className="nex-provider-name">
                    {adapter.display_name}
                  </span>
                  {adapter.version && (
                    <span className="nex-cli-version">{adapter.version}</span>
                  )}
                </div>
                <div className="nex-optin-actions">
                  <span
                    className={
                      "nex-tag" + (adapter.installed ? " is-connected" : "")
                    }
                  >
                    <span
                      className={
                        "nex-tag-dot" + (adapter.installed ? " is-ok" : "")
                      }
                      aria-hidden="true"
                    />
                    {adapter.installed
                      ? t("settings.cliAdapterInstalled")
                      : t("settings.cliAdapterNotDetected")}
                  </span>
                  {adapter.enabled && (
                    <span className="nex-tag is-connected">
                      {t("settings.cliAdapterEnabled")}
                    </span>
                  )}
                  {store.toggleCliAdapter && (
                    <Button
                      variant={adapter.enabled ? "quiet" : "secondary"}
                      size="sm"
                      disabled={
                        store.working || (!adapter.enabled && !adapter.installed)
                      }
                      onClick={() =>
                        void store.toggleCliAdapter?.(
                          adapter.id,
                          !adapter.enabled
                        )
                      }
                    >
                      {adapter.enabled
                        ? t("settings.cliAdapterDisable")
                        : t("settings.cliAdapterEnable")}
                    </Button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}
    </section>
  );
}
