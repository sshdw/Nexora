//! Provider / model / credential data hook (Phase 10.3.2).
//!
//! Orchestrates loading the build-supported providers and their models, the
//! configured `providers` table rows, credential presence, and the persisted
//! provider/model selection (FR-004, FR-012, FR-014). The backend is the source
//! of truth for supported providers and models; this hook never invents them.
//!
//! Credential values are handled transiently by the UI and passed straight to
//! the OS keyring via the Tauri layer — never stored or returned here.

import { useCallback, useEffect, useState } from "react";

import {
  type CommandError,
  type ProviderDef,
  type SupportedProvider,
  addProviderCredential,
  createProvider,
  getSetting,
  hasProviderCredential,
  listProviders,
  removeProvider,
  removeProviderCredential,
  setSetting,
  supportedProviders,
  updateProviderCredential,
} from "./tauri";

/** Setting keys backing the persisted selection (FR-012). */
const SELECTED_PROVIDER_KEY = "provider.selected";
const SELECTED_MODEL_KEY = "provider.model";

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
  loading: boolean;
  error: CommandError | null;
  working: boolean;
  reload: () => Promise<void>;
  /** Persist the selected provider (FR-004). */
  selectProvider: (name: string) => Promise<void>;
  /** Persist the selected model for the current provider (FR-004). */
  selectModel: (model: string) => Promise<void>;
  /** Register the provider definition (if needed) and store its credential. */
  connect: (name: string, displayName: string, credential: string) => Promise<void>;
  /** Remove the provider definition and its keyring credential. */
  disconnect: (name: string) => Promise<void>;
}

export function useProviders(): ProvidersStore {
  const [supported, setSupported] = useState<SupportedProvider[]>([]);
  const [configuredProviders, setConfiguredProviders] = useState<ProviderDef[]>([]);
  const [credentialedNames, setCredentialedNames] = useState<Set<string>>(new Set());
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [working, setWorking] = useState<boolean>(false);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const [defs, configured, providerSetting, modelSetting] = await Promise.all([
        supportedProviders(),
        listProviders(),
        getSetting(SELECTED_PROVIDER_KEY),
        getSetting(SELECTED_MODEL_KEY),
      ]);
      setSupported(defs);
      setConfiguredProviders(configured);

      // Credential presence is checked per provider (values never leave the
      // keyring; only presence is reported).
      const credentialed = new Set<string>();
      await Promise.all(
        defs.map(async (provider) => {
          if (await hasProviderCredential(provider.name)) credentialed.add(provider.name);
        }),
      );
      setCredentialedNames(credentialed);

      // FR-012: a persisted selection only becomes active state if it still
      // belongs to the supported provider/model domains; anything else (a
      // stale or hand-edited value) falls back to the default (no selection).
      const validProvider =
        defs.some((provider) => provider.name === providerSetting) ? providerSetting : null;
      const validModel = defs.some((provider) =>
        modelSetting === null ? false : provider.models.includes(modelSetting),
      )
        ? modelSetting
        : null;
      setSelectedProvider(validProvider);
      setSelectedModel(validModel);
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
        // provider does not support the previously selected model.
        const def = supported.find((provider) => provider.name === name);
        const nextModel =
          def && selectedModel && def.models.includes(selectedModel) ? selectedModel : null;
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
    [supported, selectedModel],
  );

  const selectModel = useCallback(async (model: string): Promise<void> => {
    setError(null);
    try {
      await setSetting(SELECTED_MODEL_KEY, model);
      setSelectedModel(model);
    } catch (e) {
      setError(toCommandError(e));
    }
  }, []);
const connect = useCallback(
    async (name: string, displayName: string, credential: string): Promise<void> => {
      setWorking(true);
      setError(null);
      try {
        const configured = configuredProviders.some((provider) => provider.name === name);
        if (!configured) {
          await createProvider(name, displayName);
        }
        if (credentialedNames.has(name)) {
          await updateProviderCredential(name, credential);
        } else {
          await addProviderCredential(name, credential);
        }
        await reload();
      } catch (e) {
        setError(toCommandError(e));
      } finally {
        setWorking(false);
      }
    },
    [configuredProviders, credentialedNames, reload],
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
        const def = configuredProviders.find((provider) => provider.name === name);
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
    [configuredProviders, selectedProvider, reload],
  );

  const providers: ProviderStatus[] = supported.map((definition) => ({
    supported: definition,
    configured: configuredProviders.some((provider) => provider.name === definition.name),
    credentialed: credentialedNames.has(definition.name),
    available:
      credentialedNames.has(definition.name) &&
      configuredProviders.some((provider) => provider.name === definition.name),
  }));

  return {
    providers,
    selectedProvider,
    selectedModel,
    loading,
    error,
    working,
    reload,
    selectProvider,
    selectModel,
    connect,
    disconnect,
  };
}

function toCommandError(error: unknown): CommandError {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).kind === "string" &&
    typeof (error as CommandError).message === "string"
  ) {
    return { kind: (error as CommandError).kind, message: (error as CommandError).message };
  }
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error) return { kind: "unknown", message: error.message };
  return { kind: "unknown", message: "Unable to reach the local database." };
}