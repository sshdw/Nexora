//! Web Access IPC wrappers (Stage 12 PR 6): configuration snapshot and
//! OS-keyring presence-only search backend credentials.
//!
//! Search API keys (`tavily`, `brave`, `exa`) live strictly in the OS keyring;
//! these wrappers never retrieve or expose secret values.

import { invoke } from "./core";

export type WebSearchBackend = "ddg" | "tavily" | "brave" | "exa" | "searxng";
export type KeyedWebSearchBackend = "tavily" | "brave" | "exa";

export interface WebAccessConfig {
  enabled: boolean;
  search_backend: WebSearchBackend;
  searxng_url: string | null;
  max_results: number;
  max_chars: number;
  has_tavily_key: boolean;
  has_brave_key: boolean;
  has_exa_key: boolean;
}

export function getWebAccessConfig(): Promise<WebAccessConfig> {
  return invoke<WebAccessConfig>("get_web_access_config");
}

export function setWebSearchCredential(
  backend: KeyedWebSearchBackend,
  apiKey: string
): Promise<void> {
  return invoke<void>("set_web_search_credential", { backend, apiKey });
}

export function removeWebSearchCredential(
  backend: KeyedWebSearchBackend
): Promise<void> {
  return invoke<void>("remove_web_search_credential", { backend });
}

export function hasWebSearchCredential(
  backend: KeyedWebSearchBackend
): Promise<boolean> {
  return invoke<boolean>("has_web_search_credential", { backend });
}
