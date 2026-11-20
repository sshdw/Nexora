//! Tauri IPC wrappers for user-added MCP servers, tool discovery, and OAuth
//! 2.1 + PKCE sign-in (`M18` / `P26`).
//!
//! Casing rule (AGENTS.md): command ARGS are camelCase (`{ name, enabled }`)
//! mapping to snake_case Rust params; returned structs stay snake_case.
//! Never carries or returns secrets/tokens (OS keyring only).

import { invoke } from "./core";

export type McpTransportKind = "stdio" | "streamable_http";

export interface McpOAuthConfig {
  authorization_url: string;
  token_url: string;
  client_id: string;
  scopes: string[];
}

export interface McpServerEntry {
  name: string;
  transport: McpTransportKind;
  enabled: boolean;
  command: string;
  args: string[];
  env: Record<string, string>;
  url?: string | null;
  oauth?: McpOAuthConfig | null;
}

export interface McpServerView {
  name: string;
  transport: McpTransportKind;
  enabled: boolean;
  command: string;
  args: string[];
  env: Record<string, string>;
  url: string | null;
  oauth: McpOAuthConfig | null;
  has_auth: boolean;
  auth_kind: "none" | "bearer" | "oauth";
  expires_at: number | null;
}

export interface McpToolSummary {
  namespaced_name: string;
  server_name: string;
  tool_name: string;
  description: string;
}

export interface McpAuthStatus {
  has_auth: boolean;
  auth_kind: "none" | "bearer" | "oauth";
  expires_at: number | null;
}

export function mcpListServers(): Promise<McpServerView[]> {
  return invoke<McpServerView[]>("mcp_list_servers");
}

export function mcpUpsertServer(
  server: McpServerEntry
): Promise<McpServerView[]> {
  return invoke<McpServerView[]>("mcp_upsert_server", { server });
}

export function mcpRemoveServer(name: string): Promise<McpServerView[]> {
  return invoke<McpServerView[]>("mcp_remove_server", { name });
}

export function mcpSetServerEnabled(
  name: string,
  enabled: boolean
): Promise<McpServerView[]> {
  return invoke<McpServerView[]>("mcp_set_server_enabled", { name, enabled });
}

export function mcpListServerTools(name: string): Promise<McpToolSummary[]> {
  return invoke<McpToolSummary[]>("mcp_list_server_tools", { name });
}

export function mcpSetBearerToken(name: string, token: string): Promise<void> {
  return invoke<void>("mcp_set_bearer_token", { name, token });
}

export function mcpClearAuth(name: string): Promise<void> {
  return invoke<void>("mcp_clear_auth", { name });
}

export function mcpStartOAuth(name: string): Promise<McpAuthStatus> {
  return invoke<McpAuthStatus>("mcp_start_oauth", { name });
}

export function mcpRefreshOAuth(name: string): Promise<McpAuthStatus> {
  return invoke<McpAuthStatus>("mcp_refresh_oauth", { name });
}
