//! Tauri IPC layer.
//!
//! Thin, typed wrappers around the existing Phase 10.2 `#[tauri::command]`
//! functions. No business logic lives here — commands are delegated to the
//! backend verbatim. Field names mirror the Rust structs serialized via serde
//! (snake_case). Timestamps are Unix seconds (SQLite `unixepoch()`), per
//! DATABASE.md §7.1.

import { invoke } from "@tauri-apps/api/core";

/** A conversation row as persisted by the backend (DATABASE.md §7.1). */
export interface Conversation {
  id: number;
  title: string;
  status: "active" | "archived";
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Safe, secret-free command error returned to the frontend (commands/error.rs). */
export interface CommandError {
  kind: string;
  message: string;
}

/** List all conversations via the `list_conversations` command.
 * The backend owns ordering (`updated_at DESC`); the frontend does not re-sort. */
export function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

/** Create a conversation via the `create_conversation` command.
 * The backend requires a non-empty title (<=500 chars per the schema CHECK). */
export function createConversation(title: string): Promise<number> {
  return invoke<number>("create_conversation", { title });
}

/** One persisted `messages` row (DATABASE.md §7.2). */
export interface Message {
  id: number;
  conversation_id: number;
  role: "user" | "assistant";
  content: string;
  provider_id: number | null;
  model_name: string | null;
  created_at: number; // seconds since unix epoch
}

/** Load a conversation's persisted messages via `conversation_history`, in the
 * backend's chronological order (`created_at` ascending). The backend is the
 * only source of history; the UI never invents or reorders messages. */
export function conversationHistory(conversationId: number): Promise<Message[]> {
  return invoke<Message[]>("conversation_history", { conversationId });
}

/** Rename a conversation via `rename_conversation` (FR-002, FR-006). */
export function renameConversation(id: number, title: string): Promise<void> {
  return invoke<void>("rename_conversation", { id, title });
}

/** Archive an active conversation via `archive_conversation` (FR-006). */
export function archiveConversation(id: number): Promise<void> {
  return invoke<void>("archive_conversation", { id });
}

/** Restore an archived conversation to active via `restore_conversation` (FR-006). */
export function restoreConversation(id: number): Promise<void> {
  return invoke<void>("restore_conversation", { id });
}

/** Delete a conversation and cascade its messages/attachments (FR-002). */
export function deleteConversation(id: number): Promise<void> {
  return invoke<void>("delete_conversation", { id });
}

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
export function createProvider(name: string, displayName: string): Promise<number> {
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

// ---- Credentials -----------------------------------------------------
// Values stay in the OS secure keyring; only presence is ever exposed.

/** Whether `provider` has a stored keyring credential (never the value). */
export function hasProviderCredential(provider: string): Promise<boolean> {
  return invoke<boolean>("has_provider_credential", { provider });
}

/** Store a new keyring credential for `provider` (FR-014). */
export function addProviderCredential(provider: string, credential: string): Promise<void> {
  return invoke<void>("add_provider_credential", { provider, credential });
}

/** Update the stored keyring credential for `provider` (FR-014). */
export function updateProviderCredential(provider: string, credential: string): Promise<void> {
  return invoke<void>("update_provider_credential", { provider, credential });
}

/** Remove the stored keyring credential for `provider` (no-op if absent). */
export function removeProviderCredential(provider: string): Promise<void> {
  return invoke<void>("remove_provider_credential", { provider });
}

// ---- Settings (FR-012) ----------------------------------------------

/** Read one setting by key (`null` when absent), via `get_setting`. */
export function getSetting(key: string): Promise<string | null> {
  return invoke<string | null>("get_setting", { key });
}

/** Write one setting by key (value may be `null`), via `set_setting`. */
export function setSetting(key: string, value: string | null): Promise<void> {
  return invoke<void>("set_setting", { key, value });
}

/** Delete one setting by key, via `delete_setting`. */
export function deleteSetting(key: string): Promise<void> {
  return invoke<void>("delete_setting", { key });
}

// ---- Search (FR-006, FR-009) -------------------------------------------

/** One `prompts` row as persisted (DATABASE.md §7.3). */
export interface Prompt {
  id: number;
  title: string;
  content: string;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Grouped results of one `search` call (BACKEND application/search.rs). */
export interface SearchResults {
  /** Conversations whose title matched, ordered by relevance. */
  conversations: Conversation[];
  /** Messages whose content matched; each opens its `conversation_id`. */
  message_matches: Message[];
  /** Prompts whose title/content matched. */
  prompts: Prompt[];
}

/** Run the existing local `search` command over conversations, messages, and
 * prompts. A blank query yields empty results (backend contract). */
export function search(query: string): Promise<SearchResults> {
  return invoke<SearchResults>("search", { query });
}

// ---- AI execution ----------------------------------------------------

/** Normalized AI response returned by `send_message`. */
export interface AiResponse {
  content: string;
  model: string;
}

/** Send a user message and return/ persist the AI response.
 * The backend enforces the 40-per-minute outbound rate cap. */
export function sendMessage(
  conversationId: number,
  content: string,
  provider: string,
  model: string,
): Promise<AiResponse> {
  return invoke<AiResponse>("send_message", { conversationId, content, provider, model });
}
