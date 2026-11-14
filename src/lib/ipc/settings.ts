//! Settings IPC area (AC-205): the key/value settings store (FR-012).

import { invoke } from "./core";

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

export type SettingScope = "personal" | "project";
export type SettingSource = "session" | "project" | "personal" | "default";

export interface ResolvedSettingEntry {
  key: string;
  value: string | null;
  source: SettingSource;
  blocked_project_value: boolean;
  blocked_reason: string | null;
}

export interface BlockedProjectOverride {
  key: string;
  reason: string;
  detail: string;
}

export interface ResolvedSettingsPayload {
  workspace_root: string | null;
  has_project_config: boolean;
  entries: Record<string, ResolvedSettingEntry>;
  blocked: BlockedProjectOverride[];
  config_notice: string | null;
}

/** Resolve all layered settings with provenance and untrusted-repo diagnostics. */
export function getResolvedSettings(
  sessionOverrides?: Record<string, string>
): Promise<ResolvedSettingsPayload> {
  return invoke<ResolvedSettingsPayload>("get_resolved_settings", {
    sessionOverrides: sessionOverrides ?? null,
  });
}

/** Persist or clear one setting in `personal` (SQLite) or `project` (`.nexora/`). */
export function saveScopedSetting(
  scope: SettingScope,
  key: string,
  value: string | null,
  sessionOverrides?: Record<string, string>
): Promise<ResolvedSettingsPayload> {
  return invoke<ResolvedSettingsPayload>("save_scoped_setting", {
    scope,
    key,
    value,
    sessionOverrides: sessionOverrides ?? null,
  });
}
