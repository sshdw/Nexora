//! Provider IPC area (AC-205): provider metadata and supported-model
//! definitions, the OpenAI-compatible endpoint configuration
//! (commands/compat.rs), and keyring credential presence probes
//! (commands/credentials.rs).

import { invoke } from "./core";
import type { SettingScope } from "./settings";

// ---- Providers -------------------------------------------------------
// Provider metadata (non-sensitive) and supported-provider/model definitions.

/** A configured provider row (`providers` table, DATABASE.md §7.5). */
export interface ProviderDef {
  id: number;
  name: string;
  display_name: string;
}

/** A build-supported provider with its hardcoded supported models
 * (DATABASE.md §7.5). Exposed by the backend so the UI never invents
 * providers or models. */
export interface SupportedProvider {
  name: string;
  display_name: string;
  models: string[];
}

/** List all configured providers (metadata only) via `list_providers`. */
export function listProviders(): Promise<ProviderDef[]> {
  return invoke<ProviderDef[]>("list_providers");
}

/** List the providers supported by this build, with their models. */
export function supportedProviders(): Promise<SupportedProvider[]> {
  return invoke<SupportedProvider[]>("supported_providers");
}

/** Register a new provider definition (FR-004). Backend rejects duplicates. */
export function createProvider(
  name: string,
  displayName: string
): Promise<number> {
  return invoke<number>("create_provider", { name, displayName });
}

/** Remove a provider definition by id (FR-004 / provider configuration). */
export function removeProvider(id: number): Promise<void> {
  return invoke<void>("remove_provider", { id });
}

/** Report whether a provider is configured and has stored credentials. */
export function isProviderAvailable(name: string): Promise<boolean> {
  return invoke<boolean>("is_provider_available", { name });
}

/** Local health verdict for one provider (`provider_health`, WS-A.5).
 * Local-only: no network probe — `healthy` means locally ready to serve. */
export type ProviderHealthStatus =
  "healthy" | "degraded" | "unreachable" | "unknown";

/** Local health snapshot for one provider. Metadata only — presence booleans
 * plus a `last_checked` Unix-seconds timestamp; never a credential value.
 * `last_checked` is `0` only when the backend clock was unavailable: render
 * it as "unknown", never as the epoch. */
export interface ProviderHealth {
  provider: string;
  status: ProviderHealthStatus;
  has_configuration: boolean;
  has_credential: boolean;
  last_checked: number; // seconds since unix epoch
}

/** Probe the local health of one provider via `provider_health`. */
export function providerHealth(name: string): Promise<ProviderHealth> {
  return invoke<ProviderHealth>("provider_health", { name });
}

// ---- OpenAI-compatible endpoint (custom base URL) ----------------------
// The endpoint metadata (base URL, model, organization, headers) is plain
// settings; the API key stays in the OS keyring under `openai_compat` and
// only its presence is ever exposed.

/** User configuration for the generic OpenAI-compatible endpoint
 * (`openai_compat`). Field names mirror the Rust struct (snake_case).
 * Carries no secret — the key is keyring-only. */
export interface CompatConfig {
  base_url: string;
  model: string;
  organization: string | null;
  headers: [string, string][];
  supports_tools: boolean;
}

/** UI-facing status of the OpenAI-compatible endpoint: presence booleans
 * plus readiness. Carries metadata only — never a secret value. */
export interface CompatStatus {
  has_base_url: boolean;
  base_url_valid: boolean;
  has_model: boolean;
  has_organization: boolean;
  header_count: number;
  has_credential: boolean;
  ready: boolean;
}

/** Read the stored OpenAI-compatible endpoint configuration. */
export function getCompatConfig(): Promise<CompatConfig> {
  return invoke<CompatConfig>("get_compat_config");
}

/** Persist the OpenAI-compatible endpoint configuration. The backend
 * validates it and rejects it unchanged on failure. */
export function setCompatConfig(config: CompatConfig): Promise<void> {
  return invoke<void>("set_compat_config", { config });
}

/** Report the UI-facing status (presence booleans + readiness) of the
 * OpenAI-compatible endpoint. */
export function compatStatus(): Promise<CompatStatus> {
  return invoke<CompatStatus>("compat_status");
}

// ---- Credentials -----------------------------------------------------
// Values stay in the OS secure keyring; only presence is ever exposed.

/** Whether `provider` has a stored keyring credential (never the value). */
export function hasProviderCredential(provider: string): Promise<boolean> {
  return invoke<boolean>("has_provider_credential", { provider });
}

/** Store a new keyring credential for `provider` (FR-014). */
export function addProviderCredential(
  provider: string,
  credential: string
): Promise<void> {
  return invoke<void>("add_provider_credential", { provider, credential });
}

/** Update the stored keyring credential for `provider` (FR-014). */
export function updateProviderCredential(
  provider: string,
  credential: string
): Promise<void> {
  return invoke<void>("update_provider_credential", { provider, credential });
}

/** Remove the stored keyring credential for `provider` (no-op if absent). */
export function removeProviderCredential(provider: string): Promise<void> {
  return invoke<void>("remove_provider_credential", { provider });
}

// ---- Opt-in ChatGPT Plus/Pro OAuth (`S11-11a`) -------------------------
// Tokens live strictly in the OS secure keyring; IPC only carries status.

/** Secret-free connection status for opt-in ChatGPT Plus/Pro OAuth. */
export interface ChatGptOAuthStatus {
  connected: boolean;
  has_refresh_token: boolean;
  expires_at: number | null;
  connected_at: number | null;
  auth_mode: "oauth" | "none";
  problem: "cleanup_required" | "other_instance" | null;
  models: { slug: string; display_name: string }[];
}

/** Read secret-free ChatGPT OAuth status from the OS keyring. */
export function chatgptOAuthStatus(): Promise<ChatGptOAuthStatus> {
  return invoke<ChatGptOAuthStatus>("chatgpt_oauth_status");
}

/** Start interactive opt-in ChatGPT Plus/Pro OAuth 2.0 + PKCE sign-in. */
export function chatgptOAuthStart(): Promise<ChatGptOAuthStatus> {
  return invoke<ChatGptOAuthStatus>("chatgpt_oauth_start");
}

/** Refresh stored ChatGPT OAuth tokens in the OS keyring. */
export function chatgptOAuthRefresh(): Promise<ChatGptOAuthStatus> {
  return invoke<ChatGptOAuthStatus>("chatgpt_oauth_refresh");
}

/** Disconnect opt-in ChatGPT OAuth (removes keyring tokens; keeps API keys). */
export function chatgptOAuthDisconnect(): Promise<void> {
  return invoke<void>("chatgpt_oauth_disconnect");
}

// ---- (connection_id, model_id) Capability Catalog & Provenance --------

export type CapabilityProvenance =
  "manual_override" | "api_metadata" | "built_in_catalog" | "unknown";

export type EffortLevel = "low" | "medium" | "high" | "xhigh";

export interface ConnectionModelCapabilities {
  connection_id: string;
  model_id: string;
  context_window: number | null;
  supported_efforts: string[];
  supports_thinking: boolean;
  supports_attachments: boolean;
  supports_tools: boolean;
  provenance: CapabilityProvenance;
  has_manual_override: boolean;
}

export interface ModelCapabilityOverride {
  context_window?: number | null;
  supported_efforts?: string[] | null;
  supports_thinking?: boolean | null;
  supports_attachments?: boolean | null;
  supports_tools?: boolean | null;
}

/** Resolve capabilities for `(connectionId, modelId)` using strict precedence:
 * `ManualOverride > ApiMetadata > BuiltInCatalog > Unknown`. */
export function getModelCapabilities(
  connectionId: string,
  modelId: string
): Promise<ConnectionModelCapabilities> {
  return invoke<ConnectionModelCapabilities>("get_model_capabilities", {
    connectionId,
    modelId,
  });
}

/** List resolved capabilities across all supported connections and models. */
export function listModelCapabilities(): Promise<
  ConnectionModelCapabilities[]
> {
  return invoke<ConnectionModelCapabilities[]>("list_model_capabilities");
}

/** Persist or clear a user manual capability override for `(connectionId, modelId)`. */
export function setModelCapabilityOverride(
  connectionId: string,
  modelId: string,
  overrideSpec: ModelCapabilityOverride | null,
  scope?: SettingScope
): Promise<ConnectionModelCapabilities> {
  return invoke<ConnectionModelCapabilities>("set_model_capability_override", {
    connectionId,
    modelId,
    overrideSpec,
    scope: scope ?? null,
  });
}

/** Clear a manual capability override for `(connectionId, modelId)` in `scope`. */
export function clearModelCapabilityOverride(
  connectionId: string,
  modelId: string,
  scope?: SettingScope
): Promise<ConnectionModelCapabilities> {
  return invoke<ConnectionModelCapabilities>(
    "clear_model_capability_override",
    {
      connectionId,
      modelId,
      scope: scope ?? null,
    }
  );
}

/** Refresh zero-token `/models` metadata from the provider API for `connectionId`. */
export function refreshProviderModelMetadata(
  connectionId: string
): Promise<ConnectionModelCapabilities[]> {
  return invoke<ConnectionModelCapabilities[]>(
    "refresh_provider_model_metadata",
    { connectionId }
  );
}
