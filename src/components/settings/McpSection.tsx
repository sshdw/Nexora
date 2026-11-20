//! MCP servers section (`M18` / `P26`): user-added `stdio` and
//! `streamable_http` MCP servers, OAuth 2.1 (PKCE S256) sign-in/refresh,
//! keyring-only Bearer token management, and tool discovery.
//!
//! Hard invariants:
//! - Zero bundled MCP servers.
//! - Personal-only (never loaded from untrusted project `.nexora/` config).
//! - Every MCP tool call requires explicit per-call approval (never auto-approved).
//! - Tokens/secrets never appear in SQLite or rendered payloads; Bearer drafts
//!   are cleared from component state immediately on save.

import { useCallback, useEffect, useState } from "react";

import type {
  CommandError,
  McpOAuthConfig,
  McpServerEntry,
  McpServerView,
  McpToolSummary,
  McpTransportKind,
  ResolvedSettingEntry,
} from "../../lib/tauri";
import {
  mcpClearAuth,
  mcpListServerTools,
  mcpListServers,
  mcpRefreshOAuth,
  mcpRemoveServer,
  mcpSetBearerToken,
  mcpSetServerEnabled,
  mcpStartOAuth,
  mcpUpsertServer,
} from "../../lib/tauri";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";
import M3SegmentedGroup from "../M3SegmentedGroup";
import SettingSourceBadge from "./SettingSourceBadge";

export interface McpSectionProps {
  resolvedEntry?: ResolvedSettingEntry;
}

function errorMessage(err: unknown, fallback: string): string {
  if (
    typeof err === "object" &&
    err !== null &&
    "message" in err &&
    typeof (err as CommandError).message === "string"
  ) {
    return (err as CommandError).message;
  }
  if (err instanceof Error && err.message) return err.message;
  return fallback;
}

export default function McpSection({ resolvedEntry }: McpSectionProps) {
  const { t } = useStrings();
  const [servers, setServers] = useState<McpServerView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyServer, setBusyServer] = useState<string | null>(null);
  const [oauthBusy, setOauthBusy] = useState<string | null>(null);
  const [toolsByServer, setToolsByServer] = useState<
    Record<string, McpToolSummary[]>
  >({});
  const [bearerDrafts, setBearerDrafts] = useState<Record<string, string>>({});

  // Add-server form state
  const [transport, setTransport] = useState<McpTransportKind>("stdio");
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [url, setUrl] = useState("");
  const [oauthEnabled, setOAuthEnabled] = useState(false);
  const [oauthAuthUrl, setOAuthAuthUrl] = useState("");
  const [oauthTokenUrl, setOAuthTokenUrl] = useState("");
  const [oauthClientId, setOAuthClientId] = useState("");
  const [oauthScopes, setOAuthScopes] = useState("");
  const [adding, setAdding] = useState(false);

  const loadServers = useCallback(async () => {
    try {
      const list = await mcpListServers();
      setServers(list);
      setError(null);
    } catch (err) {
      setServers([]);
      setError(errorMessage(err, t("common.backendUnreachable")));
    }
  }, [t]);

  useEffect(() => {
    void loadServers();
  }, [loadServers]);

  const handleToggleEnabled = async (serverName: string, next: boolean) => {
    setBusyServer(serverName);
    setError(null);
    try {
      const updated = await mcpSetServerEnabled(serverName, next);
      setServers(updated);
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setBusyServer(null);
    }
  };

  const handleRemove = async (serverName: string) => {
    setBusyServer(serverName);
    setError(null);
    try {
      const updated = await mcpRemoveServer(serverName);
      setServers(updated);
      setToolsByServer((prev) => {
        const copy = { ...prev };
        delete copy[serverName];
        return copy;
      });
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setBusyServer(null);
    }
  };

  const handleListTools = async (serverName: string) => {
    setBusyServer(serverName);
    setError(null);
    try {
      const tools = await mcpListServerTools(serverName);
      setToolsByServer((prev) => ({ ...prev, [serverName]: tools }));
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setBusyServer(null);
    }
  };

  const handleSaveBearer = async (serverName: string) => {
    const token = bearerDrafts[serverName]?.trim() ?? "";
    if (!token) return;
    setBusyServer(serverName);
    setError(null);
    try {
      await mcpSetBearerToken(serverName, token);
      // Clear secret draft from React state immediately after keyring write.
      setBearerDrafts((prev) => ({ ...prev, [serverName]: "" }));
      await loadServers();
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setBusyServer(null);
    }
  };

  const handleClearAuth = async (serverName: string) => {
    setBusyServer(serverName);
    setError(null);
    try {
      await mcpClearAuth(serverName);
      await loadServers();
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setBusyServer(null);
    }
  };

  const handleStartOAuth = async (serverName: string) => {
    setOauthBusy(serverName);
    setError(null);
    try {
      await mcpStartOAuth(serverName);
      await loadServers();
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setOauthBusy(null);
    }
  };

  const handleRefreshOAuth = async (serverName: string) => {
    setOauthBusy(serverName);
    setError(null);
    try {
      await mcpRefreshOAuth(serverName);
      await loadServers();
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setOauthBusy(null);
    }
  };

  const handleAddServer = async () => {
    setError(null);
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError(t("settings.mcpValidationNameRequired"));
      return;
    }

    let entry: McpServerEntry;
    if (transport === "stdio") {
      const trimmedCmd = command.trim();
      if (!trimmedCmd) {
        setError(t("settings.mcpValidationCommandRequired"));
        return;
      }
      const args = argsText
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter((line) => line.length > 0);
      const env: Record<string, string> = {};
      for (const rawLine of envText.split(/\r?\n/)) {
        const line = rawLine.trim();
        if (!line) continue;
        const eq = line.indexOf("=");
        if (eq <= 0) {
          setError(t("settings.mcpValidationEnvFormat"));
          return;
        }
        const key = line.slice(0, eq).trim();
        const val = line.slice(eq + 1);
        if (!key) {
          setError(t("settings.mcpValidationEnvFormat"));
          return;
        }
        env[key] = val;
      }
      entry = {
        name: trimmedName,
        transport: "stdio",
        enabled: true,
        command: trimmedCmd,
        args,
        env,
        url: null,
        oauth: null,
      };
    } else {
      const trimmedUrl = url.trim();
      if (!trimmedUrl) {
        setError(t("settings.mcpValidationUrlRequired"));
        return;
      }
      let oauth: McpOAuthConfig | null = null;
      if (oauthEnabled) {
        const authUrl = oauthAuthUrl.trim();
        const tokenUrl = oauthTokenUrl.trim();
        const clientId = oauthClientId.trim();
        if (!authUrl || !tokenUrl || !clientId) {
          setError(t("settings.mcpValidationOAuthRequired"));
          return;
        }
        const scopes = oauthScopes
          .split(/\s+/)
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
        oauth = {
          authorization_url: authUrl,
          token_url: tokenUrl,
          client_id: clientId,
          scopes,
        };
      }
      entry = {
        name: trimmedName,
        transport: "streamable_http",
        enabled: true,
        command: "",
        args: [],
        env: {},
        url: trimmedUrl,
        oauth,
      };
    }

    setAdding(true);
    try {
      const updated = await mcpUpsertServer(entry);
      setServers(updated);
      setName("");
      setCommand("");
      setArgsText("");
      setEnvText("");
      setUrl("");
      setOAuthEnabled(false);
      setOAuthAuthUrl("");
      setOAuthTokenUrl("");
      setOAuthClientId("");
      setOAuthScopes("");
    } catch (err) {
      setError(errorMessage(err, t("common.backendUnreachable")));
    } finally {
      setAdding(false);
    }
  };

  return (
    <section className="nex-settings-section" aria-labelledby="mcp-heading">
      <div className="nex-settings-label-row">
        <h3 id="mcp-heading" className="nex-settings-heading">
          {t("settings.mcpTitle")}
        </h3>
        <SettingSourceBadge entry={resolvedEntry} />
      </div>
      <p className="nex-settings-hint">{t("settings.mcpHint")}</p>

      {error && (
        <p className="nex-settings-error nex-fade-in" role="alert">
          {error}
        </p>
      )}

      {servers === null ? (
        <p className="nex-settings-hint">{t("common.loading")}</p>
      ) : servers.length === 0 ? (
        <p className="nex-settings-hint">{t("settings.mcpEmptySection")}</p>
      ) : (
        <ul className="nex-provider-list">
          {servers.map((srv) => {
            const isBusy = busyServer === srv.name || oauthBusy === srv.name;
            const discovered = toolsByServer[srv.name];
            return (
              <li key={srv.name} className="nex-provider-row">
                <div className="nex-provider-meta">
                  <div className="nex-mcp-badges">
                    <label className="nex-capabilities-check">
                      <input
                        type="checkbox"
                        checked={srv.enabled}
                        disabled={isBusy}
                        aria-label={t("settings.mcpToggleAria", {
                          name: srv.name,
                        })}
                        onChange={(event) =>
                          void handleToggleEnabled(
                            srv.name,
                            event.target.checked
                          )
                        }
                      />
                      <span className="nex-provider-name">{srv.name}</span>
                    </label>
                    <span className="nex-tag">
                      {srv.transport === "streamable_http"
                        ? t("settings.mcpTransportHttp")
                        : t("settings.mcpTransportStdio")}
                    </span>
                    <span className="nex-tag">
                      {srv.enabled
                        ? t("settings.mcpEnabled")
                        : t("settings.mcpDisabled")}
                    </span>
                    {srv.transport === "streamable_http" && (
                      <span
                        className={
                          "nex-tag" + (srv.has_auth ? " is-connected" : "")
                        }
                      >
                        {srv.auth_kind === "oauth"
                          ? t("settings.mcpAuthOAuth")
                          : srv.auth_kind === "bearer"
                            ? t("settings.mcpAuthBearer")
                            : t("settings.mcpAuthNone")}
                      </span>
                    )}
                  </div>

                  <div className="nex-provider-actions">
                    <M3Button
                      variant="secondary"
                      size="sm"
                      disabled={isBusy}
                      onClick={() => void handleListTools(srv.name)}
                    >
                      {busyServer === srv.name
                        ? t("settings.mcpListingTools")
                        : t("settings.mcpListTools")}
                    </M3Button>
                    <M3Button
                      variant="quiet"
                      size="sm"
                      className="nex-provider-remove"
                      disabled={isBusy}
                      onClick={() => void handleRemove(srv.name)}
                    >
                      {t("settings.mcpRemove")}
                    </M3Button>
                  </div>
                </div>

                <p className="nex-mcp-endpoint">
                  {srv.transport === "streamable_http"
                    ? srv.url
                    : [srv.command, ...srv.args].join(" ")}
                </p>

                {srv.transport === "streamable_http" && (
                  <div className="nex-provider-actions">
                    {srv.oauth && (
                      <>
                        <M3Button
                          variant="primary"
                          size="sm"
                          disabled={isBusy}
                          onClick={() => void handleStartOAuth(srv.name)}
                        >
                          {oauthBusy === srv.name
                            ? t("settings.mcpOAuthSigningIn")
                            : t("settings.mcpOAuthSignIn")}
                        </M3Button>
                        {srv.auth_kind === "oauth" && (
                          <M3Button
                            variant="secondary"
                            size="sm"
                            disabled={isBusy}
                            onClick={() => void handleRefreshOAuth(srv.name)}
                          >
                            {t("settings.mcpOAuthRefresh")}
                          </M3Button>
                        )}
                      </>
                    )}
                    <input
                      className="nex-input"
                      type="password"
                      autoComplete="new-password"
                      placeholder={t("settings.mcpBearerPh")}
                      aria-label={t("settings.mcpBearerAria", {
                        name: srv.name,
                      })}
                      value={bearerDrafts[srv.name] ?? ""}
                      disabled={isBusy}
                      onChange={(event) =>
                        setBearerDrafts((prev) => ({
                          ...prev,
                          [srv.name]: event.target.value,
                        }))
                      }
                    />
                    <M3Button
                      variant="secondary"
                      size="sm"
                      disabled={
                        isBusy || !(bearerDrafts[srv.name]?.trim() ?? "")
                      }
                      onClick={() => void handleSaveBearer(srv.name)}
                    >
                      {t("settings.mcpSaveBearer")}
                    </M3Button>
                    {srv.has_auth && (
                      <M3Button
                        variant="quiet"
                        size="sm"
                        disabled={isBusy}
                        onClick={() => void handleClearAuth(srv.name)}
                      >
                        {t("settings.mcpClearAuth")}
                      </M3Button>
                    )}
                  </div>
                )}

                {discovered !== undefined && (
                  <div className="nex-mcp-tools-list" role="status">
                    <span>
                      {discovered.length === 0
                        ? t("settings.mcpToolsNone")
                        : t("settings.mcpToolsFound", { n: discovered.length })}
                    </span>
                    {discovered.map((tool) => (
                      <code key={tool.namespaced_name}>
                        {tool.namespaced_name}
                      </code>
                    ))}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <div className="nex-mcp-form">
        <h4 className="nex-settings-label">{t("settings.mcpAddHeading")}</h4>

        <div className="nex-settings-field">
          <span className="nex-settings-label" id="mcp-transport-label">
            {t("settings.mcpTransportLabel")}
          </span>
          <M3SegmentedGroup
            labelledBy="mcp-transport-label"
            indicator={false}
            value={transport}
            onChange={(value) => setTransport(value as McpTransportKind)}
            options={[
              { value: "stdio", label: t("settings.mcpTransportStdio") },
              {
                value: "streamable_http",
                label: t("settings.mcpTransportHttp"),
              },
            ]}
          />
        </div>

        <div className="nex-settings-field">
          <label className="nex-settings-label" htmlFor="mcp-server-name">
            {t("settings.mcpNameLabel")}
          </label>
          <input
            id="mcp-server-name"
            className="nex-input"
            type="text"
            placeholder={t("settings.mcpNamePh")}
            value={name}
            disabled={adding}
            onChange={(event) => setName(event.target.value)}
          />
        </div>

        {transport === "stdio" ? (
          <>
            <div className="nex-settings-field">
              <label
                className="nex-settings-label"
                htmlFor="mcp-server-command"
              >
                {t("settings.mcpCommandLabel")}
              </label>
              <input
                id="mcp-server-command"
                className="nex-input"
                type="text"
                placeholder={t("settings.mcpCommandPh")}
                value={command}
                disabled={adding}
                onChange={(event) => setCommand(event.target.value)}
              />
            </div>

            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="mcp-server-args">
                {t("settings.mcpArgsLabel")}
              </label>
              <textarea
                id="mcp-server-args"
                className="nex-input nex-mcp-textarea"
                placeholder={t("settings.mcpArgsPh")}
                value={argsText}
                disabled={adding}
                onChange={(event) => setArgsText(event.target.value)}
              />
            </div>

            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="mcp-server-env">
                {t("settings.mcpEnvLabel")}
              </label>
              <textarea
                id="mcp-server-env"
                className="nex-input nex-mcp-textarea"
                placeholder={t("settings.mcpEnvPh")}
                value={envText}
                disabled={adding}
                onChange={(event) => setEnvText(event.target.value)}
              />
            </div>
          </>
        ) : (
          <>
            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="mcp-server-url">
                {t("settings.mcpUrlLabel")}
              </label>
              <input
                id="mcp-server-url"
                className="nex-input"
                type="url"
                placeholder={t("settings.mcpUrlPh")}
                value={url}
                disabled={adding}
                onChange={(event) => setUrl(event.target.value)}
              />
            </div>

            <label className="nex-capabilities-check">
              <input
                type="checkbox"
                checked={oauthEnabled}
                disabled={adding}
                onChange={(event) => setOAuthEnabled(event.target.checked)}
              />
              <span>{t("settings.mcpOAuthToggle")}</span>
            </label>

            {oauthEnabled && (
              <div className="nex-mcp-oauth-fields">
                <div className="nex-settings-field">
                  <label
                    className="nex-settings-label"
                    htmlFor="mcp-oauth-auth-url"
                  >
                    {t("settings.mcpOAuthAuthUrl")}
                  </label>
                  <input
                    id="mcp-oauth-auth-url"
                    className="nex-input"
                    type="url"
                    placeholder={t("settings.mcpOAuthAuthUrlPh")}
                    value={oauthAuthUrl}
                    disabled={adding}
                    onChange={(event) => setOAuthAuthUrl(event.target.value)}
                  />
                </div>
                <div className="nex-settings-field">
                  <label
                    className="nex-settings-label"
                    htmlFor="mcp-oauth-token-url"
                  >
                    {t("settings.mcpOAuthTokenUrl")}
                  </label>
                  <input
                    id="mcp-oauth-token-url"
                    className="nex-input"
                    type="url"
                    placeholder={t("settings.mcpOAuthTokenUrlPh")}
                    value={oauthTokenUrl}
                    disabled={adding}
                    onChange={(event) => setOAuthTokenUrl(event.target.value)}
                  />
                </div>
                <div className="nex-settings-field">
                  <label
                    className="nex-settings-label"
                    htmlFor="mcp-oauth-client-id"
                  >
                    {t("settings.mcpOAuthClientId")}
                  </label>
                  <input
                    id="mcp-oauth-client-id"
                    className="nex-input"
                    type="text"
                    placeholder={t("settings.mcpOAuthClientIdPh")}
                    value={oauthClientId}
                    disabled={adding}
                    onChange={(event) => setOAuthClientId(event.target.value)}
                  />
                </div>
                <div className="nex-settings-field">
                  <label
                    className="nex-settings-label"
                    htmlFor="mcp-oauth-scopes"
                  >
                    {t("settings.mcpOAuthScopes")}
                  </label>
                  <input
                    id="mcp-oauth-scopes"
                    className="nex-input"
                    type="text"
                    placeholder={t("settings.mcpOAuthScopesPh")}
                    value={oauthScopes}
                    disabled={adding}
                    onChange={(event) => setOAuthScopes(event.target.value)}
                  />
                </div>
              </div>
            )}
          </>
        )}

        <div>
          <M3Button
            variant="primary"
            size="sm"
            disabled={adding}
            onClick={() => void handleAddServer()}
          >
            {adding ? t("settings.mcpAdding") : t("settings.mcpAddButton")}
          </M3Button>
        </div>
      </div>
    </section>
  );
}
