//! Settings -> Agent -> Web Access section (Stage 12 PR 6).
//!
//! Controls opt-in agent web tools (`web_search`, `web_fetch`), default-off,
//! search backend selection (`ddg` keyless default, keyed `tavily`/`brave`/`exa`
//! stored strictly in the OS keyring with presence-only UI, and validated
//! HTTPS-or-loopback `searxng`), plus result/character caps.
//! Styling uses shared Nexora CSS tokens/classes only (no raw hex or radii).

import { useCallback, useEffect, useState } from "react";

import {
  formatIpcError,
  getWebAccessConfig,
  removeWebSearchCredential,
  setSetting,
  setWebSearchCredential,
  type KeyedWebSearchBackend,
  type ResolvedSettingEntry,
  type WebAccessConfig,
  type WebSearchBackend,
} from "../../lib/tauri";
import { useStrings } from "../../lib/useLocale";
import Button from "../Button";
import { Switch } from "../ui/switch";
import SettingSourceBadge from "./SettingSourceBadge";

const WEB_ENABLED_KEY = "agent.web.enabled";
const WEB_BACKEND_KEY = "agent.web.search_backend";
const WEB_SEARXNG_URL_KEY = "agent.web.searxng_url";
const WEB_MAX_RESULTS_KEY = "agent.web.max_results";
const WEB_MAX_CHARS_KEY = "agent.web.max_chars";

const BACKEND_OPTIONS: readonly WebSearchBackend[] = [
  "ddg",
  "tavily",
  "brave",
  "exa",
  "searxng",
];

function isKeyedBackend(
  backend: WebSearchBackend
): backend is KeyedWebSearchBackend {
  return backend === "tavily" || backend === "brave" || backend === "exa";
}

function hasKeyForBackend(
  config: WebAccessConfig,
  backend: KeyedWebSearchBackend
): boolean {
  switch (backend) {
    case "tavily":
      return config.has_tavily_key;
    case "brave":
      return config.has_brave_key;
    case "exa":
      return config.has_exa_key;
  }
}

export interface WebAccessSectionProps {
  resolvedEntries?: Record<string, ResolvedSettingEntry> | null;
  onSettingsChanged?: () => void;
}

export default function WebAccessSection({
  resolvedEntries,
  onSettingsChanged,
}: WebAccessSectionProps) {
  const { t } = useStrings();
  const [config, setConfig] = useState<WebAccessConfig | null>(null);
  const [searxngDraft, setSearxngDraft] = useState("");
  const [maxResultsDraft, setMaxResultsDraft] = useState("5");
  const [maxCharsDraft, setMaxCharsDraft] = useState("20000");
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusNote, setStatusNote] = useState<string | null>(null);

  const loadConfig = useCallback(async () => {
    try {
      const next = await getWebAccessConfig();
      setConfig(next);
      setSearxngDraft(next.searxng_url ?? "");
      setMaxResultsDraft(String(next.max_results));
      setMaxCharsDraft(String(next.max_chars));
      setError(null);
    } catch (err) {
      setError(formatIpcError(err));
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  const persistSetting = async (key: string, value: string | null) => {
    setSaving(true);
    setError(null);
    setStatusNote(null);
    try {
      await setSetting(key, value);
      await loadConfig();
      onSettingsChanged?.();
    } catch (err) {
      setError(formatIpcError(err));
    } finally {
      setSaving(false);
    }
  };

  const handleToggleEnabled = (checked: boolean) => {
    void persistSetting(WEB_ENABLED_KEY, checked ? "true" : "false");
  };

  const handleBackendChange = (next: WebSearchBackend) => {
    setApiKeyDraft("");
    setStatusNote(null);
    void persistSetting(WEB_BACKEND_KEY, next);
  };

  const handleSaveSearxng = () => {
    const trimmed = searxngDraft.trim();
    void persistSetting(WEB_SEARXNG_URL_KEY, trimmed.length > 0 ? trimmed : null);
  };

  const handleSaveMaxResults = () => {
    const trimmed = maxResultsDraft.trim();
    const parsed = Number.parseInt(trimmed, 10);
    if (!Number.isFinite(parsed) || parsed < 1 || parsed > 8) {
      setError(t("settings.webMaxResultsInvalid"));
      return;
    }
    void persistSetting(WEB_MAX_RESULTS_KEY, String(parsed));
  };

  const handleSaveMaxChars = () => {
    const trimmed = maxCharsDraft.trim();
    const parsed = Number.parseInt(trimmed, 10);
    if (!Number.isFinite(parsed) || parsed < 1000 || parsed > 48000) {
      setError(t("settings.webMaxCharsInvalid"));
      return;
    }
    void persistSetting(WEB_MAX_CHARS_KEY, String(parsed));
  };

  const handleSaveApiKey = async (backend: KeyedWebSearchBackend) => {
    const trimmed = apiKeyDraft.trim();
    if (!trimmed) return;
    setSaving(true);
    setError(null);
    setStatusNote(null);
    try {
      await setWebSearchCredential(backend, trimmed);
      // Never retain secret material in component state after saving.
      setApiKeyDraft("");
      setStatusNote(t("settings.webKeySaved"));
      await loadConfig();
    } catch (err) {
      setError(formatIpcError(err));
    } finally {
      setSaving(false);
    }
  };

  const handleRemoveApiKey = async (backend: KeyedWebSearchBackend) => {
    setSaving(true);
    setError(null);
    setStatusNote(null);
    try {
      await removeWebSearchCredential(backend);
      setApiKeyDraft("");
      setStatusNote(t("settings.webKeyRemoved"));
      await loadConfig();
    } catch (err) {
      setError(formatIpcError(err));
    } finally {
      setSaving(false);
    }
  };

  const currentBackend: WebSearchBackend = config?.search_backend ?? "ddg";

  return (
    <section
      className="nex-settings-section"
      aria-labelledby="web-access-heading"
    >
      <h3 id="web-access-heading" className="nex-settings-heading">
        {t("settings.webAccessTitle")}
      </h3>
      <p className="nex-settings-hint">{t("settings.webAccessHint")}</p>

      <div className="nex-settings-field">
        <div className="nex-flag-row">
          <div className="nex-flag-info">
            <div className="nex-settings-label-row">
              <label className="nex-flag-title" htmlFor="web-access-enabled">
                {t("settings.webEnabledLabel")}
              </label>
              <SettingSourceBadge entry={resolvedEntries?.[WEB_ENABLED_KEY]} />
            </div>
            <p id="web-access-enabled-hint" className="nex-flag-desc">
              {t("settings.webEnabledHint")}
            </p>
          </div>
          <div className="nex-flag-control">
            <Switch
              id="web-access-enabled"
              checked={config?.enabled ?? false}
              disabled={saving || config === null}
              onCheckedChange={handleToggleEnabled}
              aria-describedby="web-access-enabled-hint"
            />
          </div>
        </div>
      </div>

      <div className="nex-settings-field">
        <div className="nex-settings-label-row">
          <label className="nex-settings-label" htmlFor="web-search-backend">
            {t("settings.webBackendLabel")}
          </label>
          <SettingSourceBadge entry={resolvedEntries?.[WEB_BACKEND_KEY]} />
        </div>
        <select
          id="web-search-backend"
          className="nex-input"
          value={currentBackend}
          disabled={saving || config === null}
          onChange={(event) =>
            handleBackendChange(event.target.value as WebSearchBackend)
          }
        >
          {BACKEND_OPTIONS.map((backend) => (
            <option key={backend} value={backend}>
              {t(`settings.webBackend.${backend}`)}
            </option>
          ))}
        </select>
        <p className="nex-settings-hint">{t("settings.webBackendHint")}</p>
      </div>

      {config && isKeyedBackend(currentBackend) && (
        <div className="nex-settings-field">
          <div className="nex-settings-label-row">
            <label className="nex-settings-label" htmlFor="web-search-api-key">
              {t("settings.webApiKeyLabel")}
            </label>
            <span className="nex-tag">
              {hasKeyForBackend(config, currentBackend)
                ? t("settings.credentialSaved")
                : t("settings.notConnected")}
            </span>
          </div>
          <div className="nex-settings-inline-row">
            <input
              id="web-search-api-key"
              className="nex-input"
              type="password"
              autoComplete="off"
              value={apiKeyDraft}
              placeholder={
                hasKeyForBackend(config, currentBackend)
                  ? t("settings.apiKeyUpdatePh")
                  : t("settings.apiKeyPh")
              }
              disabled={saving}
              onChange={(event) => setApiKeyDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  void handleSaveApiKey(currentBackend);
                }
              }}
            />
            <Button
              size="sm"
              variant="primary"
              disabled={saving || apiKeyDraft.trim().length === 0}
              onClick={() => void handleSaveApiKey(currentBackend)}
            >
              {hasKeyForBackend(config, currentBackend)
                ? t("settings.update")
                : t("settings.connect")}
            </Button>
            {hasKeyForBackend(config, currentBackend) && (
              <Button
                size="sm"
                variant="quiet"
                disabled={saving}
                onClick={() => void handleRemoveApiKey(currentBackend)}
              >
                {t("settings.disconnect")}
              </Button>
            )}
          </div>
          <p className="nex-settings-hint">{t("settings.webApiKeyHint")}</p>
        </div>
      )}

      {currentBackend === "searxng" && (
        <div className="nex-settings-field">
          <div className="nex-settings-label-row">
            <label className="nex-settings-label" htmlFor="web-searxng-url">
              {t("settings.webSearxngLabel")}
            </label>
            <SettingSourceBadge
              entry={resolvedEntries?.[WEB_SEARXNG_URL_KEY]}
            />
          </div>
          <div className="nex-settings-inline-row">
            <input
              id="web-searxng-url"
              className="nex-input"
              type="url"
              value={searxngDraft}
              placeholder={t("settings.webSearxngPh")}
              disabled={saving}
              onChange={(event) => setSearxngDraft(event.target.value)}
              onBlur={handleSaveSearxng}
              onKeyDown={(event) => {
                if (event.key === "Enter") handleSaveSearxng();
              }}
            />
            <Button
              size="sm"
              variant="quiet"
              disabled={saving}
              onClick={handleSaveSearxng}
            >
              {t("settings.webSave")}
            </Button>
          </div>
          <p className="nex-settings-hint">{t("settings.webSearxngHint")}</p>
        </div>
      )}

      <div className="nex-settings-field">
        <div className="nex-settings-label-row">
          <label className="nex-settings-label" htmlFor="web-max-results">
            {t("settings.webMaxResultsLabel")}
          </label>
          <SettingSourceBadge entry={resolvedEntries?.[WEB_MAX_RESULTS_KEY]} />
        </div>
        <input
          id="web-max-results"
          className="nex-input"
          type="number"
          min={1}
          max={8}
          value={maxResultsDraft}
          disabled={saving || config === null}
          onChange={(event) => setMaxResultsDraft(event.target.value)}
          onBlur={handleSaveMaxResults}
          onKeyDown={(event) => {
            if (event.key === "Enter") handleSaveMaxResults();
          }}
        />
        <p className="nex-settings-hint">{t("settings.webMaxResultsHint")}</p>
      </div>

      <div className="nex-settings-field">
        <div className="nex-settings-label-row">
          <label className="nex-settings-label" htmlFor="web-max-chars">
            {t("settings.webMaxCharsLabel")}
          </label>
          <SettingSourceBadge entry={resolvedEntries?.[WEB_MAX_CHARS_KEY]} />
        </div>
        <input
          id="web-max-chars"
          className="nex-input"
          type="number"
          min={1000}
          max={48000}
          step={1000}
          value={maxCharsDraft}
          disabled={saving || config === null}
          onChange={(event) => setMaxCharsDraft(event.target.value)}
          onBlur={handleSaveMaxChars}
          onKeyDown={(event) => {
            if (event.key === "Enter") handleSaveMaxChars();
          }}
        />
        <p className="nex-settings-hint">{t("settings.webMaxCharsHint")}</p>
      </div>

      {statusNote && (
        <p className="nex-settings-hint" role="status">
          {statusNote}
        </p>
      )}
      {error && (
        <p className="nex-settings-error nex-fade-in" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
