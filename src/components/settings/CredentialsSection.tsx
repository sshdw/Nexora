//! Credentials section (AC-208 split, per the review §3.1 plan — JSX moved
//! verbatim from `SettingsView.tsx`). The typed key drafts stay in the
//! parent (and are cleared only on a successful connect), so a section
//! round trip preserves them exactly as before the split.

import { useEffect, useState } from "react";

import { openExternalUrl, type SupportedProvider } from "../../lib/tauri";
import {
  CHATGPT_PLAN_PROVIDER,
  type ProvidersStore,
} from "../../lib/useProviders";
import { useStrings } from "../../lib/useLocale";
import Button from "../Button";
import { ChatGptMarkIcon } from "../icons";
import { Checkbox } from "../ui/checkbox";

const CHATGPT_MANAGE_USAGE_URL = "https://chatgpt.com/#settings";

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

  // Clear scoped ChatGPT OAuth errors when leaving the Credentials section so
  // they never linger across tab/modal switches.
  const clearOAuthError = store.clearChatGptOAuthError;
  useEffect(() => {
    return () => {
      clearOAuthError?.();
    };
  }, [clearOAuthError]);

  const apiProviders = store.providers.filter(
    ({ supported }) => supported.name !== CHATGPT_PLAN_PROVIDER
  );
  const oauthConnected = Boolean(store.chatgptOAuth?.connected);
  const oauthCleanupRequired =
    store.chatgptOAuth?.problem === "cleanup_required";
  const oauthReauthRequired =
    store.chatgptOAuth?.problem === "reauthorization_required";
  const oauthOtherInstance = store.chatgptOAuth?.problem === "other_instance";
  const oauthHasRefresh = Boolean(store.chatgptOAuth?.has_refresh_token);
  const oauthBusy = Boolean(store.chatgptOAuthWorking) || store.working;

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
                  disabled={store.working}
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
              {oauthOtherInstance
                ? t("settings.chatgptOAuthOtherInstance")
                : oauthCleanupRequired
                  ? t("settings.chatgptOAuthCleanupRequired")
                  : oauthReauthRequired
                    ? t("settings.chatgptOAuthReauthRequired")
                    : oauthConnected
                      ? t("settings.chatgptOAuthConnected")
                      : t("settings.chatgptOAuthNotConnected")}
            </span>
          </div>
          <p className="nex-optin-notice" role="note">
            {t("settings.chatgptOAuthNotice")}
          </p>
          {oauthConnected ? (
            <p className="nex-settings-hint">
              {t("settings.chatgptOAuthActiveHint")}{" "}
              <a
                href={CHATGPT_MANAGE_USAGE_URL}
                rel="noreferrer noopener"
                className="nex-optin-link"
                onClick={(event) => {
                  event.preventDefault();
                  void openExternalUrl(CHATGPT_MANAGE_USAGE_URL).catch(
                    () => {}
                  );
                }}
              >
                {t("settings.chatgptOAuthManageUsage")}
              </a>
            </p>
          ) : oauthCleanupRequired ? (
            <p className="nex-settings-hint" role="status">
              {t("settings.chatgptOAuthCleanupHint")}
            </p>
          ) : oauthReauthRequired ? (
            <p className="nex-settings-hint" role="status">
              {t("settings.chatgptOAuthReauthHint")}
            </p>
          ) : oauthOtherInstance ? null : (
            <label className="nex-capabilities-check">
              <Checkbox
                checked={oauthOptInAck}
                disabled={oauthBusy}
                onCheckedChange={(checked) => setOauthOptInAck(checked)}
              />
              <span>{t("settings.chatgptOAuthOptInLabel")}</span>
            </label>
          )}
          {store.chatgptOAuthError && (
            <p
              id="nex-chatgpt-oauth-error"
              className="nex-settings-error nex-fade-in"
              role="alert"
            >
              {store.chatgptOAuthError.message}
            </p>
          )}
          {oauthOtherInstance ? (
            <Button
              variant="quiet"
              size="sm"
              disabled={store.loading}
              onClick={() => void store.reload()}
            >
              {t("settings.chatgptOAuthCheckAgain")}
            </Button>
          ) : (
            <div className="nex-optin-actions">
              {(!oauthConnected && !oauthCleanupRequired) ||
              oauthReauthRequired ? (
                <button
                  type="button"
                  className="nex-siwc-btn"
                  disabled={oauthBusy || (!oauthReauthRequired && !oauthOptInAck)}
                  onClick={() => {
                    setOauthOptInAck(false);
                    void store.startChatGptOAuth?.();
                  }}
                >
                  <ChatGptMarkIcon size={16} />
                  {store.chatgptOAuthWorking ? (
                    <span className="nex-siwc-label" role="status">
                      {t("settings.chatgptOAuthSigningIn")}
                    </span>
                  ) : (
                    <span className="nex-siwc-label">
                      {t("settings.chatgptOAuthSignIn")}
                    </span>
                  )}
                </button>
              ) : null}
              {oauthConnected &&
                oauthHasRefresh &&
                store.refreshChatGptOAuth && (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={oauthBusy}
                    onClick={() => void store.refreshChatGptOAuth?.()}
                  >
                    {t("settings.chatgptOAuthRefresh")}
                  </Button>
                )}
              {(oauthConnected ||
                oauthCleanupRequired ||
                oauthReauthRequired) &&
                store.disconnectChatGptOAuth && (
                  <Button
                    variant="quiet"
                    className="nex-provider-remove"
                    disabled={oauthBusy}
                    onClick={() => void store.disconnectChatGptOAuth?.()}
                  >
                    {oauthCleanupRequired
                      ? t("settings.chatgptOAuthRetryCleanup")
                      : t("settings.chatgptOAuthDisconnect")}
                  </Button>
                )}
            </div>
          )}
        </section>
      )}
    </section>
  );
}
