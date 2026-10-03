import { useEffect, useMemo, useState } from "react";

import { scoreCommand } from "../lib/commands";
import type { AutonomyMode, SupportedProvider } from "../lib/tauri";
import { clearApplicationData, getSetting, setSetting } from "../lib/tauri";
import type { AppearanceStore } from "../lib/useAppearance";
import { isCustomModelId, type ProvidersStore } from "../lib/useProviders";
import type { SpendLimitStore } from "../lib/useSpendLimit";
import M3Button from "./M3Button";
import M3RailItem from "./M3RailItem";
import M3SegmentedGroup from "./M3SegmentedGroup";

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
type SettingsSectionId =
  | "appearance"
  | "provider"
  | "agent"
  | "workspace"
  | "credentials"
  | "data"
  | "advanced";

/** Re-exported for the command-palette registry (deep links). */
export type { SettingsSectionId };

const SECTION_ORDER: readonly SettingsSectionId[] = [
  "appearance",
  "provider",
  "agent",
  "workspace",
  "credentials",
  "data",
  "advanced",
];

const SECTION_TITLES: Record<SettingsSectionId, string> = {
  appearance: "Appearance",
  provider: "Provider & model",
  agent: "Agent & budgets",
  workspace: "Workspace folder",
  credentials: "Credentials",
  data: "Data management",
  advanced: "Advanced",
};

/** The agent-autonomy default persisted for new runs (Task 5.2, DP-AUTONOMY).
 * The same key the conversation header persists (ConversationView owns the
 * live-switch of running agents; this control persists the default). */
const AUTONOMY_KEY = "agent.autonomy";
const AUTONOMY_MODES: readonly AutonomyMode[] = [
  "supervised",
  "semi_autonomous",
  "full_autonomous",
];
const AUTONOMY_LABELS: Record<AutonomyMode, string> = {
  supervised: "Supervised",
  semi_autonomous: "Semi-autonomous",
  full_autonomous: "Fully autonomous",
};

function isAutonomyMode(value: string | null): value is AutonomyMode {
  return (
    value === "supervised" ||
    value === "semi_autonomous" ||
    value === "full_autonomous"
  );
}

/** Feature flags exposed as toggles (application/flags.rs:89-110 FLAGS).
 * Descriptions and enforcement marks mirror the backend module docs:
 * `injection`/`assembly` are enforced in the run path; `snapshots` /
 * `self_audit` resolve but gate nothing yet. */
const FLAG_DEFS: readonly {
  name: string;
  description: string;
  enforced: boolean;
}[] = [
  {
    name: "snapshots",
    description: "Run snapshot capture, checkpoints, and rollback.",
    enforced: false,
  },
  {
    name: "self_audit",
    description: "Self-audit outcome recording on the audit trail.",
    enforced: false,
  },
  {
    name: "injection",
    description: "Untrusted-output envelopes and the marker-scan approval hold.",
    enforced: true,
  },
  {
    name: "assembly",
    description: "Budgeted smart context assembly of the run opening.",
    enforced: true,
  },
];

/** Parse a stored flag value like the backend: absent or unparseable degrades
 * to the hardcoded default (`true` — current behavior). */
function parseFlagValue(value: string | null): boolean {
  if (value === null) return true;
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "false") return false;
  return true;
}

const RECENT_KEY = "agent.workspace_recent";
const ROUTING_CHAT_KEY = "routing.profile.chat";
const ROUTING_AGENT_KEY = "routing.profile.agent";
const MCP_SERVERS_KEY = "mcp.servers";
const PRESET_KEY = "agent.preset";

function parseRecentList(raw: string | null): string[] {
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (entry): entry is string => typeof entry === "string" && entry.length > 0,
    );
  } catch {
    return [];
  }
}

function summarizeRoutingProfile(raw: string | null): string {
  if (raw === null) return "Not set — the default provider order applies.";
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return "The stored value is not valid JSON — the default order applies.";
  }
  if (!Array.isArray(parsed) || parsed.length === 0) {
    return "Not set — the default provider order applies.";
  }
  const names = parsed
    .map((entry) => {
      if (typeof entry !== "object" || entry === null) return null;
      const record = entry as Record<string, unknown>;
      if (typeof record.provider !== "string" || typeof record.model !== "string") {
        return null;
      }
      return `${record.provider} / ${record.model}`;
    })
    .filter((name): name is string => name !== null);
  const shown = names.slice(0, 4).join(", ");
  const extra = names.length > 4 ? ` (+${names.length - 4} more)` : "";
  return `${parsed.length} ${parsed.length === 1 ? "entry" : "entries"}${shown ? `: ${shown}${extra}` : ""}.`;
}

function summarizeMcpServers(raw: string | null): string {
  if (raw === null) return "Not set — no external tool servers configured.";
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return "The stored value is not valid JSON — it is ignored.";
  }
  if (!Array.isArray(parsed)) return "The stored value is not a server list — it is ignored.";
  if (parsed.length === 0) return "Empty list — no external tool servers configured.";
  const names = parsed
    .map((entry) => {
      if (typeof entry !== "object" || entry === null) return null;
      const record = entry as Record<string, unknown>;
      return typeof record.name === "string" ? record.name : null;
    })
    .filter((name): name is string => name !== null);
  const shown = names.slice(0, 5).join(", ");
  const extra = names.length > 5 ? ` (+${names.length - 5} more)` : "";
  return `${parsed.length} ${parsed.length === 1 ? "server" : "servers"}${shown ? `: ${shown}${extra}` : ""}.`;
}

/** One searchable settings entry: a group title or an individual key label
 * plus hand-written keyword aliases. Matched with the palette's
 * case-insensitive subsequence scorer (scoreCommand — reused, not
 * duplicated). */
interface SettingsSearchEntry {
  group: SettingsSectionId;
  title: string;
  keywords: string[];
}

const STATIC_SEARCH_ENTRIES: readonly SettingsSearchEntry[] = [
  { group: "appearance", title: "Appearance", keywords: ["theme", "dark", "light", "appearance", "look"] },
  { group: "appearance", title: "Theme", keywords: ["theme", "dark", "light", "color scheme"] },
  { group: "provider", title: "Provider & model", keywords: ["provider", "model", "ai model", "llm"] },
  { group: "provider", title: "Provider", keywords: ["provider", "vendor"] },
  { group: "provider", title: "Model", keywords: ["model", "llm", "gpt", "claude", "gemini"] },
  { group: "provider", title: "Custom model ID", keywords: ["custom", "model id", "model variant", "exact id"] },
  { group: "agent", title: "Agent & budgets", keywords: ["agent", "budget", "autonomy", "spend", "cost"] },
  { group: "agent", title: "Autonomy mode", keywords: ["autonomy", "supervised", "semi-autonomous", "autonomous", "approval", "agent mode"] },
  { group: "agent", title: "Run cost limit", keywords: ["spend", "budget", "cost", "limit", "micro-usd", "micro usd", "run cost"] },
  { group: "workspace", title: "Workspace folder", keywords: ["workspace", "folder", "directory", "root", "project path"] },
  { group: "workspace", title: "Current root", keywords: ["root", "current", "folder", "scoped"] },
  { group: "workspace", title: "Recent folders", keywords: ["recent", "history", "folders", "previous"] },
  { group: "credentials", title: "Credentials", keywords: ["api key", "credentials", "connect", "key", "token", "auth"] },
  { group: "credentials", title: "Provider credentials", keywords: ["api key", "secret", "keyring", "connect provider"] },
  { group: "data", title: "Data management", keywords: ["data", "clear", "delete all", "reset", "storage", "database"] },
  { group: "data", title: "Clear all application data", keywords: ["clear", "delete", "reset", "wipe", "danger"] },
  { group: "data", title: "Import conversation", keywords: ["import", "upload", "restore", "open file"] },
  { group: "data", title: "Export conversation", keywords: ["export", "download", "save", "share", "backup"] },
  { group: "advanced", title: "Advanced", keywords: ["advanced", "power", "flags", "routing", "mcp", "internals"] },
  { group: "advanced", title: "Feature flags", keywords: ["flags", "feature flags", "snapshots", "self audit", "injection", "assembly", "rollout"] },
  { group: "advanced", title: "Routing profiles", keywords: ["routing", "profile", "order", "chat profile", "agent profile", "fallback"] },
  { group: "advanced", title: "MCP servers", keywords: ["mcp", "servers", "tools", "model context protocol"] },
  { group: "advanced", title: "Agent preset", keywords: ["preset", "agent preset", "defaults"] },
];

/** The exact phrase the backend's `clear_application_data` command requires
 * before it performs any destructive write (application::data_management::
 * CONFIRMATION — FR-013 AC-5). The user must type it explicitly. */
const CLEAR_CONFIRMATION_PHRASE = "confirm";

export interface SettingsViewProps {
  onClose: () => void;
  /** Shared provider/model/credential store lifted in App (single source). */
  store: ProvidersStore;
  /** Persisted appearance preference lifted in App so it loads at startup. */
  appearance: AppearanceStore;
  /** Current agent workspace root (1.3.0, read-only here; change via sidebar). */
  workspaceRoot: string | null;
  workspaceLoading: boolean;
  /** Per-run spend guard store lifted in App (single source). */
  spendLimit: SpendLimitStore;
  /** Refresh conversation-dependent UI after all local data is cleared. */
  onDataCleared: () => void;
  /** Section to show on open (palette deep links); defaults to appearance. */
  initialSection?: SettingsSectionId;
  /** Open the conversation-import modal (wired by App; hidden when absent). */
  onOpenImport?: () => void;
  /** Export the active conversation (wired by App; hidden when absent). */
  onExportActive?: () => void;
}

export default function SettingsView({
  onClose,
  store,
  appearance,
  workspaceRoot,
  workspaceLoading,
  spendLimit,
  onDataCleared,
  initialSection = "appearance",
  onOpenImport,
  onExportActive,
}: SettingsViewProps) {
  const [section, setSection] = useState<SettingsSectionId>(initialSection);
  // Palette deep links retarget an already-mounted panel: follow
  // initialSection instead of pinning the first mount value.
  useEffect(() => {
    setSection(initialSection);
  }, [initialSection]);
  const [query, setQuery] = useState("");
  const [draftKeys, setDraftKeys] = useState<Record<string, string>>({});
  // Clear-all-data confirmation state (typed phrase; no accidental runs).
  const [confirmingClear, setConfirmingClear] = useState(false);
  const [clearPhrase, setClearPhrase] = useState("");
  const [clearError, setClearError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);
  // Agent-group state: persisted autonomy default for new runs.
  const [autonomy, setAutonomy] = useState<AutonomyMode>("semi_autonomous");
  const [autonomyError, setAutonomyError] = useState<string | null>(null);
  // Workspace-group state: recent folders (read-only here).
  const [recentFolders, setRecentFolders] = useState<string[] | null>(null);
  // Advanced-group state: power-key values loaded once on mount.
  const [advancedValues, setAdvancedValues] = useState<Record<string, string | null> | null>(null);
  const [advancedError, setAdvancedError] = useState<string | null>(null);
  const [flagValues, setFlagValues] = useState<Record<string, boolean>>({});
  const [flagsSaving, setFlagsSaving] = useState<string | null>(null);
  const [flagsError, setFlagsError] = useState<string | null>(null);

  // Power-key values (autonomy default, recent folders, flags, routing
  // profiles, MCP servers, agent preset) load on mount and re-sync whenever
  // the panel regains visibility: ConversationView can persist autonomy
  // while Settings is open, which would otherwise leave this copy stale.
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const results = await Promise.all([
          getSetting(AUTONOMY_KEY),
          getSetting(RECENT_KEY),
          getSetting(ROUTING_CHAT_KEY),
          getSetting(ROUTING_AGENT_KEY),
          getSetting(MCP_SERVERS_KEY),
          getSetting(PRESET_KEY),
          ...FLAG_DEFS.map((flag) => getSetting(`flags.${flag.name}`)),
        ]);
        if (cancelled) return;
        const [autonomyRaw, recentRaw, chatRaw, agentRaw, mcpRaw, presetRaw, ...flagRaws] = results;
        if (isAutonomyMode(autonomyRaw)) setAutonomy(autonomyRaw);
        setRecentFolders(parseRecentList(recentRaw));
        setAdvancedValues({
          [ROUTING_CHAT_KEY]: chatRaw,
          [ROUTING_AGENT_KEY]: agentRaw,
          [MCP_SERVERS_KEY]: mcpRaw,
          [PRESET_KEY]: presetRaw,
        });
        const nextFlags: Record<string, boolean> = {};
        FLAG_DEFS.forEach((flag, index) => {
          nextFlags[flag.name] = parseFlagValue(flagRaws[index] ?? null);
        });
        setFlagValues(nextFlags);
      } catch {
        if (!cancelled) {
          setAdvancedError("Unable to load advanced settings.");
          setRecentFolders([]);
        }
      }
    };
    void load();
    const resync = () => {
      if (document.visibilityState === "visible") void load();
    };
    window.addEventListener("focus", resync);
    document.addEventListener("visibilitychange", resync);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", resync);
      document.removeEventListener("visibilitychange", resync);
    };
  }, []);

  const selected = store.providers.find((p) => p.supported.name === store.selectedProvider) ?? null;
  const selectedDefinition: SupportedProvider | null = selected ? selected.supported : null;
  const selectedModels = selectedDefinition ? selectedDefinition.models : [];

  /** The model to display: the persisted selection, or the provider default. */
  const effectiveModel =
    store.selectedModel && selectedModels.includes(store.selectedModel)
      ? store.selectedModel
      : selectedModels[0] ?? null;

  /** A persisted custom model ID (valid but outside the shortlist). */
  const persistedCustom =
    store.selectedModel && !selectedModels.includes(store.selectedModel)
      ? store.selectedModel
      : null;
  /** Local custom draft; non-null means the Custom… option is active. */
  const [customDraft, setCustomDraft] = useState<string | null>(null);
  const customActive = customDraft !== null || persistedCustom !== null;
  const customValue = customDraft ?? persistedCustom ?? "";

  // Search index: static group/key entries plus one entry per registered
  // provider (by name and display name) for the provider + credentials groups.
  const searchEntries = useMemo<SettingsSearchEntry[]>(() => {
    const dynamic: SettingsSearchEntry[] = [];
    for (const { supported } of store.providers) {
      dynamic.push({
        group: "provider",
        title: supported.display_name,
        keywords: [supported.name, supported.display_name, "provider"],
      });
      dynamic.push({
        group: "credentials",
        title: `${supported.display_name} API key`,
        keywords: [supported.name, supported.display_name, "api key", "key"],
      });
    }
    return [...STATIC_SEARCH_ENTRIES, ...dynamic];
  }, [store.providers]);

  // Search matches whole groups (by group title, key label, or keyword) using
  // the palette subsequence scorer. Null = empty query: show all structure.
  const matches = useMemo<SettingsSectionId[] | null>(() => {
    if (query.trim() === "") return null;
    const scored: { group: SettingsSectionId; score: number }[] = [];
    for (const group of SECTION_ORDER) {
      let best: number | null = null;
      for (const entry of searchEntries) {
        if (entry.group !== group) continue;
        const score = scoreCommand(query, {
          id: `settings-search:${group}:${entry.title}`,
          title: entry.title,
          section: "Settings",
          keywords: entry.keywords,
          run: () => {},
        });
        if (score !== null && (best === null || score > best)) best = score;
      }
      if (best !== null) scored.push({ group, score: best });
    }
    scored.sort((a, b) => b.score - a.score);
    return scored.map((item) => item.group);
  }, [query, searchEntries]);

  // Empty query shows structure (single-section view); a query stacks every
  // matching group so matched keys are visible without navigating.
  const visibleSections: SettingsSectionId[] = matches ?? [section];

  const handleSearchKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" && matches && matches.length > 0) {
      setSection(matches[0]);
    } else if (event.key === "Escape") {
      setQuery("");
    }
  };

  const handleNavSelect = (id: SettingsSectionId) => {
    setSection(id);
    // Selecting a group exits the filtered view back to the structure.
    setQuery("");
  };

  const handleProviderChange = async (name: string) => {
    // A custom model ID is provider-independent: keep it across the switch.
    const keepCustom = store.selectedModel ? isCustomModelId(store.selectedModel) : false;
    setCustomDraft(null);
    await store.selectProvider(name);
    if (keepCustom) return;
    // Persist the default model for the newly selected provider so the
    // selection is never left without a model.
    const def = store.providers.find((p) => p.supported.name === name)?.supported;
    if (def && def.models.length > 0) {
      await store.selectModel(def.models[0]);
    }
  };

  const handleModelSelect = (value: string) => {
    // The `__custom__` option is UI-only and is never sent to the backend.
    if (value === "__custom__") {
      setCustomDraft(persistedCustom ?? "");
      return;
    }
    setCustomDraft(null);
    void store.selectModel(value);
  };

  const commitCustom = () => {
    const value = customDraft ?? persistedCustom ?? "";
    if (!value || value === "__custom__") return;
    void store.selectModel(value);
  };

  /** Local spend-limit draft; null means the persisted value is shown. */
  const [spendDraft, setSpendDraft] = useState<string | null>(null);
  const spendValue =
    spendDraft ?? (spendLimit.limitMicroUsd !== null ? String(spendLimit.limitMicroUsd) : "");
  const commitSpendLimit = () => {
    if (spendDraft === null) return;
    const trimmed = spendDraft.trim();
    if (trimmed === "") {
      void spendLimit.setLimit(null).then((ok) => {
        if (ok) setSpendDraft(null);
      });
      return;
    }
    const parsed = Number(trimmed);
    void spendLimit.setLimit(parsed).then((ok) => {
      if (ok) setSpendDraft(null);
    });
  };

  const handleAutonomyChange = async (mode: AutonomyMode) => {
    const previous = autonomy;
    setAutonomy(mode);
    setAutonomyError(null);
    try {
      // Persist the default for new runs. Live runs keep their gate here —
      // the conversation header owns the live-switch (ConversationView).
      await setSetting(AUTONOMY_KEY, mode);
    } catch (e) {
      // Roll back the optimistic control: a rejected write must not leave
      // the selector showing a mode that was never persisted.
      setAutonomy(previous);
      setAutonomyError(
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : "Unable to save the autonomy mode.",
      );
    }
  };

  const handleFlagToggle = async (name: string, next: boolean) => {
    setFlagsSaving(name);
    setFlagsError(null);
    try {
      // The command gate accepts only true/false (parse_global_bool domain).
      await setSetting(`flags.${name}`, next ? "true" : "false");
      setFlagValues((prev) => ({ ...prev, [name]: next }));
    } catch (e) {
      setFlagsError(
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : `Unable to save the ${name} flag.`,
      );
    } finally {
      setFlagsSaving(null);
    }
  };

  const handleConnect = async (definition: SupportedProvider) => {
    const credential = draftKeys[definition.name]?.trim() ?? "";
    if (!credential) return;
    const succeeded = await store.connect(definition.name, definition.display_name, credential);
    if (!succeeded) return; // Keep the typed key for correction; error is shown.
    // Never keep the secret in component state after it is stored.
    setDraftKeys((prev) => ({ ...prev, [definition.name]: "" }));
  };

  const handleDisconnect = async (definition: SupportedProvider) => {
    await store.disconnect(definition.name);
  };

  const openClearConfirmation = () => {
    setClearPhrase("");
    setClearError(null);
    setConfirmingClear(true);
  };

  const cancelClearConfirmation = () => {
    setConfirmingClear(false);
    setClearPhrase("");
    setClearError(null);
  };

  const handleClearData = async () => {
    if (clearing) return;
    if (clearPhrase !== CLEAR_CONFIRMATION_PHRASE) {
      setClearError(`Type "${CLEAR_CONFIRMATION_PHRASE}" to confirm.`);
      return;
    }
    setClearing(true);
    setClearError(null);
    try {
      // The backend refuses to run unless the phrase matches exactly and
      // clears everything atomically — a failure leaves all data intact.
      await clearApplicationData(CLEAR_CONFIRMATION_PHRASE);
      // The cleared settings included the provider/model selection.
      await store.reload();
      onDataCleared();
      cancelClearConfirmation();
    } catch (e) {
      setClearError(
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : "Unable to clear application data.",
      );
    } finally {
      setClearing(false);
    }
  };

  const renderSection = (id: SettingsSectionId) => {
    if (id === "appearance") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="appearance-heading">
          <h3 id="appearance-heading" className="nex-settings-heading">
            Appearance
          </h3>
          <p className="nex-settings-hint">
            Visual theme for this device. Applied immediately and persisted between
            sessions.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label" id="theme-label">
              Theme
            </span>
            <M3SegmentedGroup
              labelledBy="theme-label"
              value={appearance.theme}
              onChange={(theme) => void appearance.setTheme(theme)}
              options={[
                { value: "dark", label: "Dark" },
                { value: "light", label: "Light" },
              ]}
            />
            <p className="nex-settings-hint">
              The light theme is provisional — the final palette is still open.
            </p>
          </div>
        </section>
      );
    }

    if (id === "provider") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="selection-heading">
          <h3 id="selection-heading" className="nex-settings-heading">
            Provider &amp; model
          </h3>
          <p className="nex-settings-hint">
            Choose which provider and model new requests use. Providers must be
            connected with a credential before they can serve requests.
          </p>
          <p className="nex-settings-hint">
            Model options come from the backend&apos;s supported list; the static
            August 2026 catalog doc may be stale.
          </p>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="provider-select">
              Provider
            </label>
            <select
              id="provider-select"
              className="nex-select"
              value={selectedDefinition?.name ?? ""}
              onChange={(event) => handleProviderChange(event.target.value)}
            >
              <option value="" disabled>
                Select a provider
              </option>
              {store.providers.map(({ supported, available }) => (
                <option key={supported.name} value={supported.name}>
                  {supported.display_name}
                  {available ? " · Ready" : " · Not connected"}
                </option>
              ))}
            </select>
          </div>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="model-select">
              Model
            </label>
            <select
              id="model-select"
              className="nex-select"
              value={customActive ? "__custom__" : (effectiveModel ?? "")}
              disabled={selectedModels.length === 0}
              onChange={(event) => handleModelSelect(event.target.value)}
            >
              {selectedModels.length === 0 && (
                <option value="">Select a provider first</option>
              )}
              {selectedModels.map((model) => (
                <option key={model} value={model}>
                  {model}
                </option>
              ))}
              {selectedModels.length > 0 && (
                <option value="__custom__">Custom…</option>
              )}
            </select>
          </div>

          {customActive && selectedModels.length > 0 && (
            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="model-custom">
                Custom model ID
              </label>
              <input
                id="model-custom"
                className="nex-input"
                type="text"
                value={customValue}
                placeholder="e.g. vendor/model-variant"
                onChange={(event) => setCustomDraft(event.target.value)}
                onBlur={commitCustom}
                onKeyDown={(event) => {
                  if (event.key === "Enter") commitCustom();
                }}
              />
              <p className="nex-settings-hint">
                Listed ID or custom: 1–200 chars of A–Z a–z 0–9 . _ / : - +. Use
                the exact model ID from your provider&apos;s model list or
                dashboard.
              </p>
            </div>
          )}
        </section>
      );
    }

    if (id === "agent") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="agent-heading">
          <h3 id="agent-heading" className="nex-settings-heading">
            Agent &amp; budgets
          </h3>
          <p className="nex-settings-hint">
            The default autonomy for new agent runs, and the per-run spend
            guard. The conversation header can still switch autonomy per run.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label" id="autonomy-label">
              Autonomy mode
            </span>
            <M3SegmentedGroup
              labelledBy="autonomy-label"
              value={autonomy}
              onChange={(mode) => void handleAutonomyChange(mode)}
              options={AUTONOMY_MODES.map((mode) => ({
                value: mode,
                label: AUTONOMY_LABELS[mode],
              }))}
            />
            <p className="nex-settings-hint">
              Supervised pauses for approval; semi-autonomous asks on risky
              steps; fully autonomous runs to completion.
            </p>
            {autonomyError && (
              <p className="nex-settings-error nex-fade-in" role="alert">
                {autonomyError}
              </p>
            )}
          </div>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="spend-limit">
              Run cost limit (micro-USD)
            </label>
            <input
              id="spend-limit"
              className="nex-input"
              type="text"
              value={spendValue}
              placeholder="Empty = no limit"
              onChange={(event) => setSpendDraft(event.target.value)}
              onBlur={commitSpendLimit}
              onKeyDown={(event) => {
                if (event.key === "Enter") commitSpendLimit();
              }}
            />
            <p className="nex-settings-hint">
              Empty = no limit. 1 USD = 1000000 micro-USD. Applies to new runs.
            </p>
            {spendLimit.error && (
              <p className="nex-settings-error nex-fade-in" role="alert">
                {spendLimit.error.message}
              </p>
            )}
          </div>
        </section>
      );
    }

    if (id === "workspace") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="workspace-heading">
          <h3 id="workspace-heading" className="nex-settings-heading">
            Workspace folder
          </h3>
          <p className="nex-settings-hint">
            The folder the agent&apos;s tools are scoped to. Change it from the
            sidebar folder picker; the 5 most recent folders are kept there.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Current root</span>
            <p className="nex-settings-value" title={workspaceRoot ?? ""}>
              {workspaceLoading ? "Loading…" : (workspaceRoot ?? "Unset")}
            </p>
          </div>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Recent folders</span>
            {recentFolders === null ? (
              <p className="nex-settings-hint">Loading…</p>
            ) : recentFolders.length === 0 ? (
              <p className="nex-settings-hint">No recent folders yet.</p>
            ) : (
              recentFolders.map((folder) => (
                <p key={folder} className="nex-settings-value" title={folder}>
                  {folder}
                </p>
              ))
            )}
          </div>
        </section>
      );
    }

    if (id === "credentials") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="providers-heading">
          <h3 id="providers-heading" className="nex-settings-heading">
            Provider credentials
          </h3>
          <p className="nex-settings-hint">
            API keys are stored in your operating system&rsquo;s secure keyring, never in the
            database, and are never shown again after saving.
          </p>

          <ul className="nex-provider-list">
            {store.providers.map(({ supported, credentialed, available }) => (
              <li key={supported.name} className="nex-provider-row">
                <div className="nex-provider-meta">
                  <span className="nex-provider-name">{supported.display_name}</span>
                  <span className="nex-provider-status">
                    <span
                      className={
                        "nex-tag" +
                        (available ? " is-connected" : "")
                      }
                    >
                      <span
                        className={
                          "nex-tag-dot" +
                          (available ? " is-ok" : credentialed ? "" : "")
                        }
                        aria-hidden="true"
                      />
                      {available
                        ? "Connected"
                        : credentialed
                          ? "Credential saved"
                          : "Not connected"}
                    </span>
                  </span>
                </div>

                <div className="nex-provider-actions">
                  <input
                    className="nex-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={credentialed ? "Update API key" : "API key"}
                    aria-label={`${supported.display_name} API key`}
                    aria-describedby={
                      store.error ? "nex-settings-store-error" : undefined
                    }
                    value={draftKeys[supported.name] ?? ""}
                    disabled={store.working}
                    onChange={(event) =>
                      setDraftKeys((prev) => ({ ...prev, [supported.name]: event.target.value }))
                    }
                  />
                  <M3Button
                    variant="primary"
                    size="sm"
                    disabled={store.working || !(draftKeys[supported.name]?.trim())}
                    onClick={() => handleConnect(supported)}
                  >
                    {credentialed ? "Update" : "Connect"}
                  </M3Button>
                  {credentialed && (
                    <M3Button
                      variant="quiet"
                      className="nex-provider-remove"
                      disabled={store.working}
                      onClick={() => handleDisconnect(supported)}
                    >
                      Disconnect
                    </M3Button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </section>
      );
    }

    if (id === "data") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="data-heading">
          <h3 id="data-heading" className="nex-settings-heading">
            Data management
          </h3>
          <p className="nex-settings-hint">
            All application data lives in a single local SQLite database on this device.
            Nothing is synchronized anywhere. Individual conversations and prompts are
            managed from the sidebar and Prompt Library.
          </p>

          {(onOpenImport || onExportActive) && (
            <div className="nex-settings-field">
              <span className="nex-settings-label">Conversation transfer</span>
              <div className="nex-provider-actions">
                {onExportActive && (
                  <M3Button variant="primary" size="sm" onClick={onExportActive}>
                    Export active conversation…
                  </M3Button>
                )}
                {onOpenImport && (
                  <M3Button variant="quiet" size="sm" onClick={onOpenImport}>
                    Import conversation…
                  </M3Button>
                )}
              </div>
            </div>
          )}

          <div className="nex-danger-zone">
            <h4 className="nex-danger-title">Clear all application data</h4>
            <p className="nex-danger-text">
              Permanently deletes every conversation, message, attachment and prompt stored
              on this device, along with provider metadata and application settings.
              Provider credentials in the operating system keyring are not affected. This
              cannot be undone.
            </p>
            {!confirmingClear ? (
              <M3Button variant="destructive" onClick={openClearConfirmation}>
                Clear all data…
              </M3Button>
            ) : (
              <div className="nex-danger-confirm">
                <label className="nex-settings-label" htmlFor="clear-confirm-input">
                  Type &ldquo;{CLEAR_CONFIRMATION_PHRASE}&rdquo; to confirm
                </label>
                <input
                  id="clear-confirm-input"
                  className="nex-input"
                  type="text"
                  value={clearPhrase}
                  autoFocus
                  disabled={clearing}
                  aria-invalid={clearError ? true : undefined}
                  aria-describedby={clearError ? "clear-confirm-error" : undefined}
                  onChange={(event) => setClearPhrase(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") void handleClearData();
                  }}
                />
                {clearError && (
                  <p
                    id="clear-confirm-error"
                    className="nex-settings-error nex-fade-in"
                    role="alert"
                  >
                    {clearError}
                  </p>
                )}
                <div className="nex-provider-actions">
                  <M3Button
                    variant="quiet"
                    disabled={clearing}
                    onClick={cancelClearConfirmation}
                  >
                    Cancel
                  </M3Button>
                  <M3Button
                    variant="destructive"
                    filled
                    loading={clearing}
                    disabled={clearPhrase !== CLEAR_CONFIRMATION_PHRASE}
                    onClick={() => void handleClearData()}
                  >
                    {clearing ? "Clearing…" : "Clear all data"}
                  </M3Button>
                </div>
              </div>
            )}
          </div>
        </section>
      );
    }

    // Advanced: power keys, demarked and collapsed. Flags are simple booleans
    // (same true/false domain the backend gate accepts) so they get toggles;
    // routing profiles, MCP servers, and the agent preset are complex JSON
    // the UI does not edit — they stay visible but read-only.
    return (
      <section key={id} className="nex-settings-section" aria-labelledby="advanced-heading">
        <h3 id="advanced-heading" className="nex-settings-heading">
          Advanced
        </h3>
        <p className="nex-settings-hint">
          Power keys for the 2.0 rollout and routing internals. Defaults apply
          when a key is unset; clearing a key restores its default. Nothing
          here is required for everyday use.
        </p>
        {advancedError && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {advancedError}
          </p>
        )}

        <div className="nex-settings-field">
          <span className="nex-settings-label" id="flags-label">
            Feature flags
          </span>
          <p className="nex-settings-hint">
            App-global 2.0 rollout gates. A workspace flags file wins when
            present; these keys decide otherwise.
          </p>
          {FLAG_DEFS.map((flag) => (
            <label key={flag.name} className="nex-flag-row" htmlFor={`flag-${flag.name}`}>
              <input
                id={`flag-${flag.name}`}
                type="checkbox"
                checked={flagValues[flag.name] ?? true}
                disabled={flagsSaving !== null}
                onChange={(event) => void handleFlagToggle(flag.name, event.target.checked)}
                aria-describedby={`flag-${flag.name}-hint`}
              />
              <span>
                <span className="nex-provider-name">{flag.name}</span>{" "}
                <span className="nex-tag">{flag.enforced ? "Enforced in runs" : "Resolved only"}</span>
                <span id={`flag-${flag.name}-hint`} className="nex-settings-hint">
                  {" "}{flag.description}
                </span>
              </span>
            </label>
          ))}
          {flagsError && (
            <p className="nex-settings-error nex-fade-in" role="alert">
              {flagsError}
            </p>
          )}
        </div>

        <details className="nex-settings-details">
          <summary>Routing profiles</summary>
          <p className="nex-settings-hint">
            Explicit provider/model order for chat and agent tasks. Managed via
            setup import; shown here read-only.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Chat profile (routing.profile.chat)</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? "Loading…"
                : summarizeRoutingProfile(advancedValues[ROUTING_CHAT_KEY] ?? null)}
            </p>
          </div>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Agent profile (routing.profile.agent)</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? "Loading…"
                : summarizeRoutingProfile(advancedValues[ROUTING_AGENT_KEY] ?? null)}
            </p>
          </div>
        </details>

        <details className="nex-settings-details">
          <summary>MCP servers</summary>
          <p className="nex-settings-hint">
            External tool servers for agent runs. Managed via setup import;
            shown here read-only.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Servers (mcp.servers)</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? "Loading…"
                : summarizeMcpServers(advancedValues[MCP_SERVERS_KEY] ?? null)}
            </p>
          </div>
        </details>

        <details className="nex-settings-details">
          <summary>Agent preset</summary>
          <p className="nex-settings-hint">
            Stored agent preset override. Unset means the built-in default
            applies.
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">Preset (agent.preset)</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? "Loading…"
                : (advancedValues[PRESET_KEY] ?? "Not set — the built-in default applies.")}
            </p>
          </div>
        </details>
      </section>
    );
  };

  return (
    <div className="nex-settings nex-view-enter">
      <header className="nex-settings-header">
        <div className="nex-settings-heading-block">
          <h2 className="nex-settings-title">Settings</h2>
          <p className="nex-settings-subtitle">
            Preferences for this device. Everything stays local.
          </p>
        </div>
        <M3Button variant="quiet" onClick={onClose}>
          Back to conversations
        </M3Button>
      </header>

      <div className="nex-settings-layout">
        <nav className="nex-settings-nav" aria-label="Settings sections">
          <div className="nex-settings-search" role="search">
            <input
              className="nex-input"
              type="search"
              value={query}
              placeholder="Search settings"
              aria-label="Search settings"
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={handleSearchKeyDown}
            />
            {matches !== null && (
              <p className="nex-settings-result-count" role="status">
                {matches.length === 0
                  ? "No settings match."
                  : `${matches.length} ${matches.length === 1 ? "group" : "groups"} match — Enter jumps to the best.`}
              </p>
            )}
          </div>
          {SECTION_ORDER.map((id) => (
            <M3RailItem
              key={id}
              label={SECTION_TITLES[id]}
              active={section === id}
              aria-current={section === id ? "page" : undefined}
              onClick={() => handleNavSelect(id)}
            >
              {SECTION_TITLES[id]}
            </M3RailItem>
          ))}
        </nav>

        <div className="nex-settings-body">
          <div className="nex-settings-inner">
            {store.error && (
              <p id="nex-settings-store-error" className="nex-settings-error nex-fade-in" role="alert">
                {store.error.message}
              </p>
            )}
            {visibleSections.length === 0 ? (
              <section className="nex-settings-section" aria-labelledby="search-empty-heading">
                <h3 id="search-empty-heading" className="nex-settings-heading">
                  No matching settings
                </h3>
                <p className="nex-settings-hint">
                  Nothing matches &ldquo;{query.trim()}&rdquo;. Try a group name
                  (appearance, provider, agent, workspace, credentials, data,
                  advanced) or a key word (theme, model, budget, flag, routing).
                </p>
              </section>
            ) : (
              visibleSections.map((id) => renderSection(id))
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
