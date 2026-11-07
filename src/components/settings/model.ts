//! Settings data model (AC-208 split, per the review §3.1 plan — moved
//! verbatim from `SettingsView.tsx`): the section vocabulary, the backend
//! settings keys, the feature-flag definitions, and the pure parsers/
//! summarizers the Advanced section reads. No React.

import type { AutonomyMode } from "../../lib/tauri";
import {
  getLocale,
  tp,
  tr,
  type Locale,
  type StringKey,
} from "../../lib/strings";

/** Settings groups (slim surface: 7 top-level groups, in-page search, and an
 * Advanced section for power keys).
 *
 * GROUP MAP (from the Phase-1 inventory — every user-visible key stays
 * reachable; related areas are merged, nothing is dropped):
 *
 * | Group         | Keys / controls shown                              | Backend source |
 * |---------------|----------------------------------------------------|----------------|
 * | appearance    | Theme (dark/light)                                 | `appearance.theme` (commands/settings.rs:40 THEME_KEY) |
 * | provider      | Provider select, model select, custom model ID     | `provider.selected` (:43), `provider.model` (:46) |
 * | agent         | Autonomy mode, run cost limit                      | `agent.autonomy` (:49), `agent.spend_limit_micro_usd` (application/agent/service.rs:312) |
 * | workspace     | Current root (read-only), recent folders (read-only) | `agent.workspace_root` (application/workspace.rs:28), `agent.workspace_recent` (workspace.rs:31) |
 * | credentials   | Per-provider API keys (keyring, never in settings) | provider credential commands (FR-014; keyring-only) |
 * | data          | Clear-all-data, import / export entry points       | `clear_application_data` (FR-013), ImportExportModals |
 * | advanced      | Feature flags (editable toggles), routing profiles + MCP servers + agent preset (read-only, collapsed) | `flags.*` (application/flags.rs:60), `routing.profile.chat/agent` (application/routing.rs:34,37), `mcp.servers` (application/import.rs:498), `agent.preset` (application/agent/service.rs:287) |
 *
 * Previously visible outside Settings (now also reachable here without
 * removing the original surface): the autonomy selector
 * (ConversationView.tsx:407-413) and the import/export modals (App.tsx).
 * Backend-only power keys (flags, routing profiles, MCP servers, preset)
 * had no UI at all; they are visible in Advanced. No backend key is
 * renamed, removed, or re-defaulted — this is UI-only regrouping.
 */
export type SettingsSectionId =
  | "appearance"
  | "provider"
  | "agent"
  | "workspace"
  | "credentials"
  | "data"
  | "advanced";

export const SECTION_ORDER: readonly SettingsSectionId[] = [
  "appearance",
  "provider",
  "agent",
  "workspace",
  "credentials",
  "data",
  "advanced",
];

export const SECTION_TITLES: Record<SettingsSectionId, StringKey> = {
  appearance: "settings.sectionAppearance",
  provider: "settings.sectionProvider",
  agent: "settings.sectionAgent",
  workspace: "settings.sectionWorkspace",
  credentials: "settings.sectionCredentials",
  data: "settings.sectionData",
  advanced: "settings.sectionAdvanced",
};

/** The agent-autonomy default persisted for new runs (Task 5.2, DP-AUTONOMY)
 * lives in the `autonomy` store prop (single source shared with the
 * conversation header — NEX-FE-003); this panel only renders the selector. */
export const AUTONOMY_MODES: readonly AutonomyMode[] = [
  "supervised",
  "semi_autonomous",
  "full_autonomous",
];

export const AUTONOMY_LABELS: Record<AutonomyMode, StringKey> = {
  supervised: "settings.autonomySupervised",
  semi_autonomous: "settings.autonomySemi",
  full_autonomous: "settings.autonomyFull",
};

/** Feature flags exposed as toggles (application/flags.rs:89-110 FLAGS).
 * Descriptions and enforcement marks mirror the backend module docs:
 * `injection`/`assembly` are enforced in the run path; `snapshots` /
 * `self_audit` resolve but gate nothing yet. */
export const FLAG_DEFS: readonly {
  name: string;
  descriptionKey: StringKey;
  enforced: boolean;
}[] = [
  {
    name: "snapshots",
    descriptionKey: "settings.flagSnapshots",
    enforced: false,
  },
  {
    name: "self_audit",
    descriptionKey: "settings.flagSelfAudit",
    enforced: false,
  },
  {
    name: "injection",
    descriptionKey: "settings.flagInjection",
    enforced: true,
  },
  {
    name: "assembly",
    descriptionKey: "settings.flagAssembly",
    enforced: true,
  },
];

/** Parse a stored flag value like the backend: absent or unparseable degrades
 * to the hardcoded default (`true` — current behavior). */
export function parseFlagValue(value: string | null): boolean {
  if (value === null) return true;
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "false") return false;
  return true;
}

export const RECENT_KEY = "agent.workspace_recent";
export const ROUTING_CHAT_KEY = "routing.profile.chat";
export const ROUTING_AGENT_KEY = "routing.profile.agent";
export const MCP_SERVERS_KEY = "mcp.servers";
export const PRESET_KEY = "agent.preset";

export function parseRecentList(raw: string | null): string[] {
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (entry): entry is string => typeof entry === "string" && entry.length > 0
    );
  } catch {
    return [];
  }
}

export function summarizeRoutingProfile(
  raw: string | null,
  locale: Locale = getLocale()
): string {
  if (raw === null) return tr(locale, "settings.routingUnset");
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return tr(locale, "settings.routingBadJson");
  }
  if (!Array.isArray(parsed) || parsed.length === 0) {
    return tr(locale, "settings.routingUnset");
  }
  const names = parsed
    .map((entry) => {
      if (typeof entry !== "object" || entry === null) return null;
      const record = entry as Record<string, unknown>;
      if (
        typeof record.provider !== "string" ||
        typeof record.model !== "string"
      ) {
        return null;
      }
      return `${record.provider} / ${record.model}`;
    })
    .filter((name): name is string => name !== null);
  const shown = names.slice(0, 4).join(", ");
  const extra =
    names.length > 4
      ? tr(locale, "settings.summaryExtra", { n: names.length - 4 })
      : "";
  const details = shown ? `: ${shown}${extra}` : "";
  return tr(locale, "settings.routingSummary", {
    count: tp(locale, "entries", parsed.length),
    details,
  });
}

export function summarizeMcpServers(
  raw: string | null,
  locale: Locale = getLocale()
): string {
  if (raw === null) return tr(locale, "settings.mcpUnset");
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return tr(locale, "settings.mcpBadJson");
  }
  if (!Array.isArray(parsed)) return tr(locale, "settings.mcpNotList");
  if (parsed.length === 0) return tr(locale, "settings.mcpEmpty");
  const names = parsed
    .map((entry) => {
      if (typeof entry !== "object" || entry === null) return null;
      const record = entry as Record<string, unknown>;
      return typeof record.name === "string" ? record.name : null;
    })
    .filter((name): name is string => name !== null);
  const shown = names.slice(0, 5).join(", ");
  const extra =
    names.length > 5
      ? tr(locale, "settings.summaryExtra", { n: names.length - 5 })
      : "";
  const details = shown ? `: ${shown}${extra}` : "";
  return tr(locale, "settings.mcpSummary", {
    count: tp(locale, "servers", parsed.length),
    details,
  });
}
