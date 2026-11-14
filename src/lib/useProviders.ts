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
  type CommandError,
  type ConnectionModelCapabilities,
  type ModelCapabilityOverride,
  type ProviderDef,
  type SupportedProvider,
  addProviderCredential,
  createProvider,
  getModelCapabilities,
  getSetting,
  hasProviderCredential,
  listModelCapabilities,
  listProviders,
  refreshProviderModelMetadata,
  removeProvider,
  removeProviderCredential,
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
    overrideSpec: ModelCapabilityOverride | null
  ) => Promise<ConnectionModelCapabilities | null>;
  /** Refresh zero-token `/models` metadata from the provider API. */
  refreshModelMetadata: (connectionId: string) => Promise<void>;
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
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [working, setWorking] = useState<boolean>(false);

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
      ] = await Promise.all([
        supportedProviders(),
        listProviders(),
        getSetting(SELECTED_PROVIDER_KEY),
        getSetting(SELECTED_MODEL_KEY),
        getSetting(SELECTED_EFFORT_KEY).catch(() => null),
        listModelCapabilities().catch(() => [] as ConnectionModelCapabilities[]),
      ]);
      setSupported(defs);
      setConfiguredProviders(configured);

      // Credential presence is checked per provider (values never leave the
      // keyring; only presence is reported).
      const credentialed = new Set<string>();
      await Promise.all(
        defs.map(async (provider) => {
          if (await hasProviderCredential(provider.name))
            credentialed.add(provider.name);
        })
      );
      setCredentialedNames(credentialed);

      // FR-012: a persisted selection only becomes active state if it still
      // belongs to the supported provider/model domains; anything else (a
      // stale or hand-edited value) falls back to the default (no selection).
      // A valid custom model ID persists like a listed one.
      const validProvider = defs.some(
        (provider) => provider.name === providerSetting
      )
        ? providerSetting
        : null;
      const validModel =
        modelSetting === null
          ? null
          : defs.some((provider) => provider.models.includes(modelSetting)) ||
              isCustomModelId(modelSetting)
            ? modelSetting
            : null;
      const validEffort =
        effortSetting !== null &&
        VALID_EFFORTS.includes(
          effortSetting as (typeof VALID_EFFORTS)[number]
        )
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
          (c) =>
            c.connection_id === validProvider && c.model_id === validModel
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

  const selectProvider = useCallback(
    async (name: string): Promise<void> => {
      setError(null);
      try {
        // Keep the persisted model valid: clear it when the newly selected
        // provider does not support the previously selected model — except a
        // valid custom model ID, which is provider-independent and kept.
        const def = supported.find((provider) => provider.name === name);
        const nextModel =
          def &&
          selectedModel &&
          (def.models.includes(selectedModel) || isCustomModelId(selectedModel))
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
    [supported, selectedModel]
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
        await setSetting(SELECTED_MODEL_KEY, model);
        setSelectedModel(model);
        const targetProvider = providerId ?? selectedProvider;
        if (targetProvider) {
          await loadModelCapabilitiesIfMissing(targetProvider, model);
        }
      } catch (e) {
        setError(toCommandError(e));
      }
    },
    [selectedProvider, loadModelCapabilitiesIfMissing]
  );

  const selectProviderAndModel = useCallback(
    async (providerId: string, model: string): Promise<void> => {
      setError(null);
      try {
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
    [selectedProvider, loadModelCapabilitiesIfMissing]
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
      overrideSpec: ModelCapabilityOverride | null
    ): Promise<ConnectionModelCapabilities | null> => {
      setError(null);
      try {
        const updated = await setModelCapabilityOverride(
          connectionId,
          modelId,
          overrideSpec
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

  // Memoized derivation (NEX-FE-002 audit): without this the `providers`
  // array — and therefore the whole store — is a fresh reference every
  // render, defeating the store memo below.
  const providers: ProviderStatus[] = useMemo(
    () =>
      supported.map((definition) => ({
        supported: definition,
        configured: configuredProviders.some(
          (provider) => provider.name === definition.name
        ),
        credentialed: credentialedNames.has(definition.name),
        available:
          credentialedNames.has(definition.name) &&
          configuredProviders.some(
            (provider) => provider.name === definition.name
          ),
      })),
    [supported, configuredProviders, credentialedNames]
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
      loading,
      error,
      working,
      reload,
      selectProvider,
      selectModel,
      selectProviderAndModel,
      selectEffort,
      setCapabilityOverride,
      refreshModelMetadata,
      connect,
      disconnect,
    }),
    [
      providers,
      selectedProvider,
      selectedModel,
      selectedEffort,
      capabilities,
      selectedCapabilities,
      loading,
      error,
      working,
      reload,
      selectProvider,
      selectModel,
      selectProviderAndModel,
      selectEffort,
      setCapabilityOverride,
      refreshModelMetadata,
      connect,
      disconnect,
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
