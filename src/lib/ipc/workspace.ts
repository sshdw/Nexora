//! Workspace IPC area (AC-205): the agent workspace root (1.3.0), the
//! multi-root registry, and the per-workspace `.nexora/` project directory.

import { invoke } from "./core";

// ---- Agent workspace folder (1.3.0) ------------------------------------
// The agent's filesystem tools are scoped to one workspace root
// (`agent.workspace_root` setting, canonicalized backend-side). The recent
// list (`agent.workspace_recent`) holds at most 5 canonical paths,
// most-recent first.

/** Effective workspace root for tool scoping (`get_workspace_root`). */
export function getWorkspaceRoot(): Promise<string> {
  return invoke<string>("get_workspace_root");
}

/** Validate, canonicalize, persist `path` as the active root and register it
 * (`set_workspace_root` — runs the identical validation as `roots_add`,
 * including the nesting refusal). Joins the registry and the 5-entry picker
 * history. Returns the canonical path. */
export function setWorkspaceRoot(path: string): Promise<string> {
  return invoke<string>("set_workspace_root", { path });
}

/** Recent workspace roots, most-recent first (at most 5). */
export function listWorkspaceRecent(): Promise<string[]> {
  return invoke<string[]>("list_workspace_recent");
}

// ---- Multi-root registry (workspace roots switcher) --------------------
// The registry spans multiple repo roots: `active` is the effective root
// every root-aware feature follows (git panel, audit, terminal, agent runs,
// GitHub lists, flags, diagnostics); `roots` lists every registered root,
// active first. Registration validates backend-side (must exist,
// canonicalized, disjoint — nesting refused) into an unbounded registry
// (registering never evicts); removal never deletes
// directories, and removing the active root falls back to the default.

/** Multi-root registry view: the active root plus every registered root. */
export interface RootsList {
  active: string;
  roots: string[];
}

/** List the registered roots with the active root first (`roots_list`). */
export function rootsList(): Promise<RootsList> {
  return invoke<RootsList>("roots_list");
}

/** Register `path` as a root and make it active (`roots_add`).
 * Returns the updated registry view. */
export function rootsAdd(path: string): Promise<RootsList> {
  return invoke<RootsList>("roots_add", { path });
}

/** Unregister `path` from the registry (`roots_remove`). Removing the
 * active root falls back to the default. Returns the updated view. */
export function rootsRemove(path: string): Promise<RootsList> {
  return invoke<RootsList>("roots_remove", { path });
}

// ---- Workspace project directory (`.nexora/`) --------------------------
// Per-workspace Nexora home: a `nexora.json` manifest (forward-only
// version), workspace-relative ignore rules, and a `profiles/` scaffold.
// Created on explicit init only — never implicitly — and workspace-scoped
// files take precedence over app-global settings where both exist.

/** Initialize the workspace `.nexora/` project directory (`nexora_init`).
 * Idempotent: existing files are never overwritten. Returns the `.nexora/`
 * directory path. */
export function nexoraInit(): Promise<string> {
  return invoke<string>("nexora_init");
}

/** Persist one workspace-scoped routing profile (`save_workspace_profile`).
 * `task` selects `.nexora/profiles/chat.json` vs `agent.json`; `document`
 * is the JSON array of `{provider, model}` entries. The backend validates
 * before writing and refuses invalid documents secret-free. Returns the
 * written file path. */
export function saveWorkspaceProfile(task: "chat" | "agent", document: string): Promise<string> {
  return invoke<string>("save_workspace_profile", { task, document });
}
