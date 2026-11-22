//! Provider / model / credential data hook (Phase 10.3.2).
//!
//! Orchestrates loading the build-supported providers and their models, the
//! configured `providers` table rows, credential presence, and the persisted
//! provider/model selection (FR-004, FR-012, FR-014). The backend is the source
//! of truth for supported providers and models; this hook never invents them.
//!
//! Credential values are handled transiently by the UI and passed straight to
//! the OS keyring via the Tauri layer — never stored or returned here.

import { useCallback, useEffect, useMemo, useState } from "react";

import {
  type ChatGptOAuthStatus,
  type CommandError,
  type CompatConfig,
  type CompatStatus,
  type ConnectionModelCapabilities,
  type ModelCapabilityOverride,
  type ProviderDef,
  type SettingScope,
  type SupportedProvider,
  addProviderCredential,
  chatgptOAuthDisconnect,
  chatgptOAuthRefresh,
  chatgptOAuthStart,
  chatgptOAuthStatus,
  clearModelCapabilityOverride,
  compatStatus as fetchCompatStatus,
  createProvider,
  getCompatConfig,
  getModelCapabilities,
  getSetting,
  hasProviderCredential,
  listModelCapabilities,
  listProviders,
  refreshProviderModelMetadata,
  removeProvider,
  removeProviderCredential,
  setCompatConfig as persistCompatConfig,
  setModelCapabilityOverride,
  setSetting,
  supportedProviders,
  updateProviderCredential,
} from "./tauri";
import { getLocale, tr } from "./strings";

/** Setting keys backing the persisted selection (FR-012). */
const SELECTED_PROVIDER_KEY = "provider.selected";
const SELECTED_MODEL_KEY = "provider.model";
const SELECTED_EFFORT_KEY = "provider.effort";
const VALID_EFFORTS = ["low", "medium", "high", "xhigh"] as const;
const OPENAI_COMPAT_PROVIDER = "openai_compat";
export const CHATGPT_PLAN_PROVIDER = "chatgpt_subscription";

export type CompatUrlErrorKey =
  | "settings.compatErrMissingUrl"
  | "settings.compatErrCredentials"
  | "settings.compatErrInsecureHttp"
  | "settings.compatErrInvalidUrl";

function hasControlOrWhitespace(value: string): boolean {
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f || /\s/.test(value[i]!)) {
      return true;
    }
  }
  return false;
}

/** Mirror backend strict HTTPS endpoint validation for instant UI feedback. */
export function validateCompatBaseUrl(url: string): CompatUrlErrorKey | null {
  const trimmed = url.trim();
  if (!trimmed) return "settings.compatErrMissingUrl";
  if (hasControlOrWhitespace(trimmed)) return "settings.compatErrInvalidUrl";
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\//.exec(trimmed);
  if (!match) return "settings.compatErrInvalidUrl";
  const scheme = match[1].toLowerCase();
  if (scheme !== "https" && scheme !== "http") {
    return "settings.compatErrInvalidUrl";
  }
  const afterScheme = trimmed.slice(match[0].length);
  const authority = afterScheme.split(/[/?#]/, 1)[0] ?? "";
  if (!authority) return "settings.compatErrInvalidUrl";
  if (authority.includes("@")) return "settings.compatErrCredentials";

  let host = "";
  if (authority.startsWith("[")) {
    const close = authority.indexOf("]");
    if (close <= 1) return "settings.compatErrInvalidUrl";
    host = authority.slice(1, close);
    const rest = authority.slice(close + 1);
    if (rest) {
      if (!rest.startsWith(":") || !isValidPort(rest.slice(1))) {
        return "settings.compatErrInvalidUrl";
      }
    }
  } else {
    const colon = authority.lastIndexOf(":");
    if (colon !== -1) {
      const port = authority.slice(colon + 1);
      host = authority.slice(0, colon);
      if (!host || !isValidPort(port)) return "settings.compatErrInvalidUrl";
    } else {
      host = authority;
    }
  }
  if (!host) return "settings.compatErrInvalidUrl";
  const isLocalhost =
    host.toLowerCase() === "localhost" ||
    host === "127.0.0.1" ||
    host === "::1";
  if (scheme === "http" && !isLocalhost) {
    return "settings.compatErrInsecureHttp";
  }
  return null;
}

function isValidPort(port: string): boolean {
  if (!/^\d{1,5}$/.test(port)) return false;
  const parsed = Number.parseInt(port, 10);
  return parsed >= 1 && parsed <= 65535;
}

/** Merge static provider models with metadata-discovered capability models.
 *  For `chatgpt_subscription`, the visible account catalog returned by the
 *  backend is the single source of truth — stale or capability-only slugs are
 *  never merged because the executor rejects them. */
export function mergeConnectionModels(
  staticModels: readonly string[],
  connectionId: string,
  capabilities: readonly ConnectionModelCapabilities[],
  extraModel?: string | null
): string[] {
  const merged: string[] = [];
  const seen = new Set<string>();
  const pushUnique = (model: string | null | undefined) => {
    if (!model) return;
    const trimmed = model.trim();
    if (trimmed !== "" && !seen.has(trimmed)) {
      seen.add(trimmed);
      merged.push(trimmed);
    }
  };
  for (const model of staticModels) pushUnique(model);
  if (connectionId === CHATGPT_PLAN_PROVIDER) {
    return merged;
  }
  pushUnique(extraModel);
  for (const cap of capabilities) {
    if (cap.connection_id === connectionId) pushUnique(cap.model_id);
  }
  return merged;
}

function capabilityKey(connectionId: string, modelId: string): string {
  return `${connectionId}::${modelId}`;
}

/** Mirror of the backend custom model ID rule (commands/settings.rs):
 * listed shortlist ID or 1..=200 chars of A-Za-z0-9._/:-+ without `..`. */
export function isCustomModelId(value: string): boolean {
  if (value.length === 0 || value.length > 200) return false;
  if (value.includes("..")) return false;
  return /^[A-Za-z0-9._/:+-]+$/.test(value);
}

/** Runtime status for one build-supported provider. */
export interface ProviderStatus {
  /** Build-supported provider definition. */
  supported: SupportedProvider;
  /** Optional display labels keyed by model slug (e.g. ChatGPT account catalog). */
  modelLabels?: Readonly<Record<string, string>>;
  /** Whether a `providers` row exists for this provider. */
  configured: boolean;
  /** Whether the OS keyring holds a credential for this provider. */
  credentialed: boolean;
  /** Shorthand: configured AND credentialed (usable for requests). */
  available: boolean;
}

export interface ProvidersStore {
  /** Every build-supported provider with runtime status. */
  providers: ProviderStatus[];
  /** Internal name of the selected provider, if any. */
  selectedProvider: string | null;
  /** Selected model for the selected provider, if any. */
  selectedModel: string | null;
  /** Selected reasoning effort for the active model, if any. */
  selectedEffort: string | null;
  /** Resolved capabilities across `(connection_id, model_id)` pairs. */
  capabilities: ConnectionModelCapabilities[];
  /** Resolved capability record for `(selectedProvider, selectedModel)`, if known. */
  selectedCapabilities: ConnectionModelCapabilities | null;
  loading: boolean;
  error: CommandError | null;
  working: boolean;
  reload: () => Promise<void>;
  /** Persist the selected provider (FR-004). */
  selectProvider: (name: string) => Promise<void>;
  /** Persist the selected model for the current (or explicit) provider (FR-004). */
  selectModel: (model: string, providerId?: string) => Promise<void>;
  /** Persist both provider and model selection without stale closure reads. */
  selectProviderAndModel: (providerId: string, model: string) => Promise<void>;
  /** Persist or clear the selected reasoning effort. */
  selectEffort: (effort: string | null) => Promise<void>;
  /** Save or clear a manual capability override for `(connectionId, modelId)`. */
  setCapabilityOverride: (
    connectionId: string,
    modelId: string,
    overrideSpec: ModelCapabilityOverride | null,
    scope?: SettingScope
  ) => Promise<ConnectionModelCapabilities | null>;
  /** Clear a manual capability override for `(connectionId, modelId)` in `scope`. */
  clearCapabilityOverride?: (
    connectionId: string,
    modelId: string,
    scope?: SettingScope
  ) => Promise<ConnectionModelCapabilities | null>;
  /** Refresh zero-token `/models` metadata from the provider API. */
  refreshModelMetadata: (connectionId: string) => Promise<void>;
  /** Configured `openai_compat` endpoint settings, when loaded. */
  compatConfig?: CompatConfig | null;
  /** Readiness & presence flags for `openai_compat`, when loaded. */
  compatStatus?: CompatStatus | null;
  /** True while saving `openai_compat` endpoint settings. */
  compatSaving?: boolean;
  /** Persist and validate `openai_compat` endpoint settings. */
  saveCompatConfig?: (config: CompatConfig) => Promise<boolean>;
  /** Register the provider definition (if needed) and store its credential.
   * Resolves `true` only when the operation succeeded, so the caller can
   * safely discard transient input; failures surface through `error`. */
  connect: (
    name: string,
    displayName: string,
    credential: string
  ) => Promise<boolean>;
  /** Remove the provider definition and its keyring credential. */
  disconnect: (name: string) => Promise<void>;
  /** Secret-free status of opt-in ChatGPT Plus/Pro OAuth (`S11-11a`). */
  chatgptOAuth?: ChatGptOAuthStatus | null;
  /** True while an opt-in ChatGPT OAuth flow/refresh/disconnect is running. */
  chatgptOAuthWorking?: boolean;
  /** Start interactive opt-in ChatGPT Plus/Pro OAuth 2.0 + PKCE sign-in. */
  startChatGptOAuth?: () => Promise<boolean>;
  /** Refresh stored opt-in ChatGPT OAuth tokens in the OS keyring. */
  refreshChatGptOAuth?: () => Promise<boolean>;
  /** Disconnect opt-in ChatGPT OAuth (removes OS keyring tokens). */
  disconnectChatGptOAuth?: () => Promise<void>;
  /** Detected external CLI adapters (`codex` / `claude`) & opt-in state (`S11-11b`). */
}

export function useProviders(): ProvidersStore {
  const [supported, setSupported] = useState<SupportedProvider[]>([]);
  const [configuredProviders, setConfiguredProviders] = useState<ProviderDef[]>(
    []
  );
  const [credentialedNames, setCredentialedNames] = useState<Set<string>>(
    new Set()
  );
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [selectedEffort, setSelectedEffort] = useState<string | null>(null);
  const [capabilities, setCapabilities] = useState<
    ConnectionModelCapabilities[]
  >([]);
  const [compatConfig, setCompatConfig] = useState<CompatConfig | null>(null);
  const [compatStatus, setCompatStatus] = useState<CompatStatus | null>(null);
  const [compatSaving, setCompatSaving] = useState<boolean>(false);
  const [chatgptOAuth, setChatGptOAuth] = useState<ChatGptOAuthStatus | null>(
    null
  );
  const [chatgptOAuthWorking, setChatGptOAuthWorking] =
    useState<boolean>(false);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [working, setWorking] = useState<boolean>(false);

  const loadCompatState = useCallback(async (): Promise<void> => {
    const [cfg, status] = await Promise.all([
      getCompatConfig().catch(() => null),
      fetchCompatStatus().catch(() => null),
    ]);
    setCompatConfig(cfg);
    setCompatStatus(status);
  }, []);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const [
        defs,
        configured,
        providerSetting,
        modelSetting,
        effortSetting,
        capList,
        oauthStatus,
      ] = await Promise.all([
        supportedProviders(),
        listProviders(),
        getSetting(SELECTED_PROVIDER_KEY),
        getSetting(SELECTED_MODEL_KEY),
        getSetting(SELECTED_EFFORT_KEY).catch(() => null),
        listModelCapabilities().catch(
          () => [] as ConnectionModelCapabilities[]
        ),
        chatgptOAuthStatus().catch(() => ({
          connected: false,
          has_refresh_token: false,
          expires_at: null,
          connected_at: null,
          auth_mode: "none" as const,
          problem: "cleanup_required" as const,
          models: [],
        })),
      ]);
      setSupported(defs);
      setConfiguredProviders(configured);
      setChatGptOAuth(
        oauthStatus && typeof oauthStatus.connected === "boolean"
          ? oauthStatus
          : null
      );

      // Credential presence is checked per API provider (values never leave the
      // keyring; only presence is reported). ChatGPT subscription credentials
      // are managed through OAuth status above.
      const credentialed = new Set<string>();
      await Promise.all(
        defs.map(async (provider) => {
          if (provider.name === CHATGPT_PLAN_PROVIDER) return;
          if (await hasProviderCredential(provider.name))
            credentialed.add(provider.name);
        })
      );
      setCredentialedNames(credentialed);

      // FR-012: a persisted selection only becomes active state if it still
      // belongs to the supported provider/model domains; anything else (a
      // stale or hand-edited value) falls back to the default (no selection).
      // For `chatgpt_subscription`, ONLY models present in the live visible
      // account catalog are valid; custom/stale slugs are rejected.
      const validProvider = defs.some(
        (provider) => provider.name === providerSetting
      )
        ? providerSetting
        : null;
      const planCatalogSlugs =
        oauthStatus && oauthStatus.connected && Array.isArray(oauthStatus.models)
          ? oauthStatus.models.map((model) => model.slug)
          : [];
      const validModel =
        modelSetting === null
          ? null
          : validProvider === CHATGPT_PLAN_PROVIDER
            ? planCatalogSlugs.includes(modelSetting)
              ? modelSetting
              : null
            : defs.some((provider) => provider.models.includes(modelSetting)) ||
                isCustomModelId(modelSetting)
              ? modelSetting
              : null;
      const validEffort =
        effortSetting !== null &&
        VALID_EFFORTS.includes(effortSetting as (typeof VALID_EFFORTS)[number])
          ? effortSetting
          : null;
      setSelectedProvider(validProvider);
      setSelectedModel(validModel);
      setSelectedEffort(validEffort);

      let nextCaps = capList;
      if (
        validProvider &&
        validModel &&
        !capList.some(
          (c) => c.connection_id === validProvider && c.model_id === validModel
        )
      ) {
        const single = await getModelCapabilities(
          validProvider,
          validModel
        ).catch(() => null);
        if (single) nextCaps = [...capList, single];
      }
      setCapabilities(nextCaps);
    } catch (e) {
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    if (selectedProvider === OPENAI_COMPAT_PROVIDER) {
      void loadCompatState();
    }
  }, [selectedProvider, loadCompatState]);

  const planCatalogSlugs = useMemo(
    () =>
      chatgptOAuth?.connected
        ? (chatgptOAuth.models ?? []).map((model) => model.slug)
        : [],
    [chatgptOAuth]
  );

  const selectProvider = useCallback(
    async (name: string): Promise<void> => {
      setError(null);
      try {
        // Keep the persisted model valid: clear it when the newly selected
        // provider does not support the previously selected model — except a
        // valid custom model ID on non-plan providers. For ChatGPT
        // subscription, only the visible account catalog is authoritative.
        const def = supported.find((provider) => provider.name === name);
        const allowedModels =
          name === CHATGPT_PLAN_PROVIDER
            ? planCatalogSlugs
            : (def?.models ?? []);
        const nextModel =
          def &&
          selectedModel &&
          (allowedModels.includes(selectedModel) ||
            (name !== CHATGPT_PLAN_PROVIDER && isCustomModelId(selectedModel)))
            ? selectedModel
            : null;
        if (nextModel !== selectedModel) {
          await setSetting(SELECTED_MODEL_KEY, nextModel);
          setSelectedModel(nextModel);
        }
        await setSetting(SELECTED_PROVIDER_KEY, name);
        setSelectedProvider(name);
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    [supported, selectedModel, planCatalogSlugs]
  );

  const loadModelCapabilitiesIfMissing = useCallback(
    async (connectionId: string, model: string): Promise<void> => {
      if (
        !connectionId ||
        capabilities.some(
          (c) => c.connection_id === connectionId && c.model_id === model
        )
      ) {
        return;
      }
      const single = await getModelCapabilities(connectionId, model).catch(
        () => null
      );
      if (single) {
        setCapabilities((prev) => {
          const filtered = prev.filter(
            (c) =>
              capabilityKey(c.connection_id, c.model_id) !==
              capabilityKey(single.connection_id, single.model_id)
          );
          return [...filtered, single];
        });
      }
    },
    [capabilities]
  );

  const selectModel = useCallback(
    async (model: string, providerId?: string): Promise<void> => {
      setError(null);
      try {
        const targetProvider = providerId ?? selectedProvider;
        if (
          targetProvider === CHATGPT_PLAN_PROVIDER &&
          !planCatalogSlugs.includes(model)
        ) {
          return;
        }
        await setSetting(SELECTED_MODEL_KEY, model);
        setSelectedModel(model);
        if (targetProvider) {
          await loadModelCapabilitiesIfMissing(targetProvider, model);
        }
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    [selectedProvider, planCatalogSlugs, loadModelCapabilitiesIfMissing]
  );

  const selectProviderAndModel = useCallback(
    async (providerId: string, model: string): Promise<void> => {
      setError(null);
      try {
        if (
          providerId === CHATGPT_PLAN_PROVIDER &&
          !planCatalogSlugs.includes(model)
        ) {
          return;
        }
        if (providerId !== selectedProvider) {
          await setSetting(SELECTED_PROVIDER_KEY, providerId);
          setSelectedProvider(providerId);
        }
        await setSetting(SELECTED_MODEL_KEY, model);
        setSelectedModel(model);
        await loadModelCapabilitiesIfMissing(providerId, model);
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    [selectedProvider, planCatalogSlugs, loadModelCapabilitiesIfMissing]
  );

  const selectEffort = useCallback(
    async (effort: string | null): Promise<void> => {
      setError(null);
      try {
        await setSetting(SELECTED_EFFORT_KEY, effort);
        setSelectedEffort(effort);
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    []
  );

  const setCapabilityOverride = useCallback(
    async (
      connectionId: string,
      modelId: string,
      overrideSpec: ModelCapabilityOverride | null,
      scope?: SettingScope
    ): Promise<ConnectionModelCapabilities | null> => {
      setError(null);
      try {
        const updated =
          overrideSpec === null
            ? await clearModelCapabilityOverride(connectionId, modelId, scope)
            : await setModelCapabilityOverride(
                connectionId,
                modelId,
                overrideSpec,
                scope
              );
        setCapabilities((prev) => {
          const key = capabilityKey(updated.connection_id, updated.model_id);
          const filtered = prev.filter(
            (c) => capabilityKey(c.connection_id, c.model_id) !== key
          );
          return [...filtered, updated];
        });
        return updated;
      } catch (e) {
        setError(toCommandError(e));
        return null;
      }
    },
    []
  );

  const clearCapabilityOverride = useCallback(
    async (
      connectionId: string,
      modelId: string,
      scope?: SettingScope
    ): Promise<ConnectionModelCapabilities | null> =>
      setCapabilityOverride(connectionId, modelId, null, scope),
    [setCapabilityOverride]
  );

  const refreshModelMetadata = useCallback(
    async (connectionId: string): Promise<void> => {
      setError(null);
      try {
        const updated = await refreshProviderModelMetadata(connectionId);
        setCapabilities((prev) => {
          const byKey = new Map(
            prev.map((c) => [capabilityKey(c.connection_id, c.model_id), c])
          );
          for (const item of updated) {
            byKey.set(capabilityKey(item.connection_id, item.model_id), item);
          }
          return Array.from(byKey.values());
        });
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    []
  );

  const saveCompatConfig = useCallback(
    async (config: CompatConfig): Promise<boolean> => {
      setCompatSaving(true);
      setError(null);
      try {
        await persistCompatConfig(config);
        const [savedCfg, nextStatus] = await Promise.all([
          getCompatConfig().catch(() => config),
          fetchCompatStatus().catch(() => null),
        ]);
        setCompatConfig(savedCfg);
        setCompatStatus(nextStatus);
        const nextModel = config.model.trim();
        if (nextModel && selectedProvider === OPENAI_COMPAT_PROVIDER) {
          await setSetting(SELECTED_MODEL_KEY, nextModel);
          setSelectedModel(nextModel);
          const single = await getModelCapabilities(
            OPENAI_COMPAT_PROVIDER,
            nextModel
          ).catch(() => null);
          if (single) {
            setCapabilities((prev) => {
              const byKey = new Map(
                prev.map((c) => [capabilityKey(c.connection_id, c.model_id), c])
              );
              byKey.set(
                capabilityKey(single.connection_id, single.model_id),
                single
              );
              return Array.from(byKey.values());
            });
          }
        }
        return true;
      } catch (e) {
        setError(toCommandError(e));
        return false;
      } finally {
        setCompatSaving(false);
      }
    },
    [selectedProvider]
  );
  const connect = useCallback(
    async (
      name: string,
      displayName: string,
      credential: string
    ): Promise<boolean> => {
      setWorking(true);
      setError(null);
      try {
        const configured = configuredProviders.some(
          (provider) => provider.name === name
        );
        if (!configured) {
          await createProvider(name, displayName);
        }
        if (credentialedNames.has(name)) {
          await updateProviderCredential(name, credential);
        } else {
          await addProviderCredential(name, credential);
        }
        await reload();
        return true;
      } catch (e) {
        setError(toCommandError(e));
        return false;
      } finally {
        setWorking(false);
      }
    },
    [configuredProviders, credentialedNames, reload]
  );

  const disconnect = useCallback(
    async (name: string): Promise<void> => {
      setWorking(true);
      setError(null);
      try {
        // Clear the persisted selection first so it never points at a removed
        // provider (a selection is only valid while the provider is usable).
        if (selectedProvider === name) {
          await setSetting(SELECTED_PROVIDER_KEY, null);
          await setSetting(SELECTED_MODEL_KEY, null);
        }
        await removeProviderCredential(name);
        const def = configuredProviders.find(
          (provider) => provider.name === name
        );
        if (def) {
          await removeProvider(def.id);
        }
        await reload();
      } catch (e) {
        setError(toCommandError(e));
      } finally {
        setWorking(false);
      }
    },
    [configuredProviders, selectedProvider, reload]
  );

  const startChatGptOAuth = useCallback(async (): Promise<boolean> => {
    setChatGptOAuthWorking(true);
    setError(null);
    try {
      const next = await chatgptOAuthStart();
      setChatGptOAuth(next);
      await reload();
      return true;
    } catch (e) {
      setError(toCommandError(e));
      return false;
    } finally {
      setChatGptOAuthWorking(false);
    }
  }, [reload]);

  const refreshChatGptOAuth = useCallback(async (): Promise<boolean> => {
    setChatGptOAuthWorking(true);
    setError(null);
    try {
      const next = await chatgptOAuthRefresh();
      setChatGptOAuth(next);
      return true;
    } catch (e) {
      await reload();
      setError(toCommandError(e));
      return false;
    } finally {
      setChatGptOAuthWorking(false);
    }
  }, [reload]);

  const disconnectChatGptOAuth = useCallback(async (): Promise<void> => {
    setChatGptOAuthWorking(true);
    setError(null);
    try {
      await chatgptOAuthDisconnect();
      if (selectedProvider === CHATGPT_PLAN_PROVIDER) {
        await setSetting(SELECTED_PROVIDER_KEY, null);
        await setSetting(SELECTED_MODEL_KEY, null);
      }
      await reload();
    } catch (e) {
      await reload();
      setError(toCommandError(e));
    } finally {
      setChatGptOAuthWorking(false);
    }
  }, [selectedProvider, reload]);

  // Memoized derivation (NEX-FE-002 audit): without this the `providers`
  // array — and therefore the whole store — is a fresh reference every
  // render, defeating the store memo below.
  const providers: ProviderStatus[] = useMemo(
    () =>
      supported.map((definition) => {
        const isConfigured = configuredProviders.some(
          (provider) => provider.name === definition.name
        );
        const credentialed =
          definition.name === CHATGPT_PLAN_PROVIDER
            ? Boolean(chatgptOAuth?.connected)
            : credentialedNames.has(definition.name);
        const visiblePlanModels =
          definition.name === CHATGPT_PLAN_PROVIDER && chatgptOAuth?.connected
            ? (chatgptOAuth.models ?? [])
            : [];
        const models =
          definition.name === CHATGPT_PLAN_PROVIDER
            ? visiblePlanModels.map((model) => model.slug)
            : definition.models;
        const modelLabels =
          definition.name === CHATGPT_PLAN_PROVIDER &&
          visiblePlanModels.length > 0
            ? Object.fromEntries(
                visiblePlanModels.map((model) => [
                  model.slug,
                  model.display_name,
                ])
              )
            : undefined;
        return {
          supported:
            definition.name === CHATGPT_PLAN_PROVIDER
              ? { ...definition, models }
              : definition,
          modelLabels,
          configured: isConfigured,
          credentialed,
          available:
            isConfigured &&
            credentialed &&
            (definition.name !== CHATGPT_PLAN_PROVIDER || models.length > 0),
        };
      }),
    [supported, configuredProviders, credentialedNames, chatgptOAuth]
  );

  const selectedCapabilities = useMemo(() => {
    if (!selectedProvider || !selectedModel) return null;
    return (
      capabilities.find(
        (c) =>
          c.connection_id === selectedProvider && c.model_id === selectedModel
      ) ?? null
    );
  }, [capabilities, selectedProvider, selectedModel]);

  // Memoized store identity (NEX-FE-002 audit).
  return useMemo(
    () => ({
      providers,
      selectedProvider,
      selectedModel,
      selectedEffort,
      capabilities,
      selectedCapabilities,
      compatConfig,
      compatStatus,
      compatSaving,
      chatgptOAuth,
      chatgptOAuthWorking,
      loading,
      error,
      working,
      reload,
      selectProvider,
      selectModel,
      selectProviderAndModel,
      selectEffort,
      setCapabilityOverride,
      clearCapabilityOverride,
      refreshModelMetadata,
      saveCompatConfig,
      connect,
      disconnect,
      startChatGptOAuth,
      refreshChatGptOAuth,
      disconnectChatGptOAuth,
    }),
    [
      providers,
      selectedProvider,
      selectedModel,
      selectedEffort,
      capabilities,
      selectedCapabilities,
      compatConfig,
      compatStatus,
      compatSaving,
      chatgptOAuth,
      chatgptOAuthWorking,
      loading,
      error,
      working,
      reload,
      selectProvider,
      selectModel,
      selectProviderAndModel,
      selectEffort,
      setCapabilityOverride,
      clearCapabilityOverride,
      refreshModelMetadata,
      saveCompatConfig,
      connect,
      disconnect,
      startChatGptOAuth,
      refreshChatGptOAuth,
      disconnectChatGptOAuth,
    ]
  );
}

function toCommandError(error: unknown): CommandError {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).kind === "string" &&
    typeof (error as CommandError).message === "string"
  ) {
    return {
      kind: (error as CommandError).kind,
      message: (error as CommandError).message,
    };
  }
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error)
    return { kind: "unknown", message: error.message };
  return { kind: "unknown", message: tr(getLocale(), "common.dbUnreachable") };
}
