import { useEffect, useMemo, useState } from "react";

import { scoreCommand } from "../lib/commands";
import type { AutonomyMode, SupportedProvider } from "../lib/tauri";
import { clearApplicationData, getSetting, setSetting } from "../lib/tauri";
import { getLocale, settingsSearchTitle, tp, tr, type Locale, type StringKey } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
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

const SECTION_TITLES: Record<SettingsSectionId, StringKey> = {
  appearance: "settings.sectionAppearance",
  provider: "settings.sectionProvider",
  agent: "settings.sectionAgent",
  workspace: "settings.sectionWorkspace",
  credentials: "settings.sectionCredentials",
  data: "settings.sectionData",
  advanced: "settings.sectionAdvanced",
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
const AUTONOMY_LABELS: Record<AutonomyMode, StringKey> = {
  supervised: "settings.autonomySupervised",
  semi_autonomous: "settings.autonomySemi",
  full_autonomous: "settings.autonomyFull",
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

function summarizeRoutingProfile(raw: string | null, locale: Locale = getLocale()): string {
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
      if (typeof record.provider !== "string" || typeof record.model !== "string") {
        return null;
      }
      return `${record.provider} / ${record.model}`;
    })
    .filter((name): name is string => name !== null);
  const shown = names.slice(0, 4).join(", ");
  const extra = names.length > 4 ? tr(locale, "settings.summaryExtra", { n: names.length - 4 }) : "";
  const details = shown ? `: ${shown}${extra}` : "";
  return tr(locale, "settings.routingSummary", {
    count: tp(locale, "entries", parsed.length),
    details,
  });
}

function summarizeMcpServers(raw: string | null, locale: Locale = getLocale()): string {
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
  const extra = names.length > 5 ? tr(locale, "settings.summaryExtra", { n: names.length - 5 }) : "";
  const details = shown ? `: ${shown}${extra}` : "";
  return tr(locale, "settings.mcpSummary", {
    count: tp(locale, "servers", parsed.length),
    details,
  });
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

/** The phrase the user must type before the Clear-All-Data button arms
 * (FR-013 AC-5). A local UX gate only: the backend no longer takes a phrase —
 * the wrapper mints a single-use server-side confirmation id after this
 * local check passes (NEX-SEC-004). Never translated (see strings.ts). */
const CLEAR_CONFIRMATION_PHRASE = "confirm";

export interface SettingsViewProps {
  onClose: () => void;
  /** Shared provider/model/credential store lifted in App (single source). */
  store: ProvidersStore;
  /** Persisted appearance preference lifted in App so it loads at startup. */
  appearance: AppearanceStore;
  /** Interface-language preference lifted in App (persisted, EN default). */
  locale: Locale;
  /** Persist a language selection. */
  onLocaleChange: (locale: Locale) => void;
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
  /** Reopen the first-run onboarding flow (wired by App; hidden when absent). */
  onReplayOnboarding?: () => void;
}

export default function SettingsView({
  onClose,
  store,
  appearance,
  locale,
  onLocaleChange,
  workspaceRoot,
  workspaceLoading,
  spendLimit,
  onDataCleared,
  initialSection = "appearance",
  onOpenImport,
  onExportActive,
  onReplayOnboarding,
}: SettingsViewProps) {
  const { t } = useStrings();
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
          setAdvancedError(tr(getLocale(), "settings.advancedLoadFail"));
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
  // Titles render in the UI locale (keywords stay English matching aids).
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
        title: tr(locale, "settings.apiKeySearchTitle", { name: supported.display_name }),
        keywords: [supported.name, supported.display_name, "api key", "key"],
      });
    }
    return [
      ...STATIC_SEARCH_ENTRIES.map((entry) => ({
        ...entry,
        title: settingsSearchTitle(locale, entry.title),
      })),
      ...dynamic,
    ];
  }, [store.providers, locale]);

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

  // shortcut:settings.search-jump / shortcut:settings.search-clear.
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
          : tr(getLocale(), "settings.autonomySaveFail"),
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
          : tr(getLocale(), "settings.flagSaveFail", { name }),
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
      setClearError(tr(getLocale(), "settings.clearMismatch", { phrase: CLEAR_CONFIRMATION_PHRASE }));
      return;
    }
    setClearing(true);
    setClearError(null);
    try {
      // The backend refuses to run unless the phrase matches exactly and
      // clears everything atomically — a failure leaves all data intact.
      await clearApplicationData();
      // The cleared settings included the provider/model selection.
      await store.reload();
      onDataCleared();
      cancelClearConfirmation();
    } catch (e) {
      setClearError(
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : tr(getLocale(), "settings.clearFail"),
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
            {t("settings.sectionAppearance")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.appearanceHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label" id="theme-label">
              {t("settings.themeLabel")}
            </span>
            <M3SegmentedGroup
              labelledBy="theme-label"
              value={appearance.theme}
              onChange={(theme) => void appearance.setTheme(theme)}
              options={[
                { value: "dark", label: t("settings.themeDark") },
                { value: "light", label: t("settings.themeLight") },
              ]}
            />
            <p className="nex-settings-hint">
              {t("settings.themeProvisional")}
            </p>
          </div>
          <div className="nex-settings-field">
            <span className="nex-settings-label" id="language-label">
              {t("settings.languageLabel")}
            </span>
            <M3SegmentedGroup
              labelledBy="language-label"
              value={locale}
              onChange={onLocaleChange}
              options={[
                { value: "en", label: t("settings.langEn") },
                { value: "ru", label: t("settings.langRu") },
              ]}
            />
            <p className="nex-settings-hint">
              {t("settings.languageHint")}
            </p>
          </div>
        </section>
      );
    }

    if (id === "provider") {
      return (
        <section key={id} className="nex-settings-section" aria-labelledby="selection-heading">
          <h3 id="selection-heading" className="nex-settings-heading">
            {t("settings.sectionProvider")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.providerHint")}
          </p>
          <p className="nex-settings-hint">
            {t("settings.providerStaleHint")}
          </p>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="provider-select">
              {t("settings.providerLabel")}
            </label>
            <select
              id="provider-select"
              className="nex-select"
              value={selectedDefinition?.name ?? ""}
              onChange={(event) => handleProviderChange(event.target.value)}
            >
              <option value="" disabled>
                {t("settings.selectProvider")}
              </option>
              {store.providers.map(({ supported, available }) => (
                <option key={supported.name} value={supported.name}>
                  {supported.display_name}
                  {available ? t("settings.readySuffix") : t("settings.notConnectedSuffix")}
                </option>
              ))}
            </select>
          </div>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="model-select">
              {t("settings.modelLabel")}
            </label>
            <select
              id="model-select"
              className="nex-select"
              value={customActive ? "__custom__" : (effectiveModel ?? "")}
              disabled={selectedModels.length === 0}
              onChange={(event) => handleModelSelect(event.target.value)}
            >
              {selectedModels.length === 0 && (
                <option value="">{t("settings.selectProviderFirst")}</option>
              )}
              {selectedModels.map((model) => (
                <option key={model} value={model}>
                  {model}
                </option>
              ))}
              {selectedModels.length > 0 && (
                <option value="__custom__">{t("settings.customOpt")}</option>
              )}
            </select>
          </div>

          {customActive && selectedModels.length > 0 && (
            <div className="nex-settings-field">
              <label className="nex-settings-label" htmlFor="model-custom">
                {t("settings.customModelLabel")}
              </label>
              <input
                id="model-custom"
                className="nex-input"
                type="text"
                value={customValue}
                placeholder={t("settings.customModelPh")}
                onChange={(event) => setCustomDraft(event.target.value)}
                onBlur={commitCustom}
                onKeyDown={(event) => {
                  // shortcut:settings.custom-model-commit.
                  if (event.key === "Enter") commitCustom();
                }}
              />
              <p className="nex-settings-hint">
                {t("settings.customModelHint")}
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
            {t("settings.sectionAgent")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.agentHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label" id="autonomy-label">
              {t("settings.autonomyLabel")}
            </span>
            <M3SegmentedGroup
              labelledBy="autonomy-label"
              value={autonomy}
              onChange={(mode) => void handleAutonomyChange(mode)}
              options={AUTONOMY_MODES.map((mode) => ({
                value: mode,
                label: t(AUTONOMY_LABELS[mode]),
              }))}
            />
            <p className="nex-settings-hint">
              {t("settings.autonomyHint")}
            </p>
            {autonomyError && (
              <p className="nex-settings-error nex-fade-in" role="alert">
                {autonomyError}
              </p>
            )}
          </div>

          <div className="nex-settings-field">
            <label className="nex-settings-label" htmlFor="spend-limit">
              {t("settings.spendLabel")}
            </label>
            <input
              id="spend-limit"
              className="nex-input"
              type="text"
              value={spendValue}
              placeholder={t("settings.spendPh")}
              onChange={(event) => setSpendDraft(event.target.value)}
              onBlur={commitSpendLimit}
              onKeyDown={(event) => {
                // shortcut:settings.spend-commit.
                if (event.key === "Enter") commitSpendLimit();
              }}
            />
            <p className="nex-settings-hint">
              {t("settings.spendHint")}
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
            {t("settings.sectionWorkspace")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.workspaceHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.currentRoot")}</span>
            <p className="nex-settings-value" title={workspaceRoot ?? ""}>
              {workspaceLoading ? t("common.loading") : (workspaceRoot ?? t("settings.unset"))}
            </p>
          </div>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.recentLabel")}</span>
            {recentFolders === null ? (
              <p className="nex-settings-hint">{t("common.loading")}</p>
            ) : recentFolders.length === 0 ? (
              <p className="nex-settings-hint">{t("settings.noRecent")}</p>
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
            {t("settings.searchProviderCreds")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.credentialsHint")}
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
                        ? t("settings.connected")
                        : credentialed
                          ? t("settings.credentialSaved")
                          : t("settings.notConnected")}
                    </span>
                  </span>
                </div>

                <div className="nex-provider-actions">
                  <input
                    className="nex-input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={credentialed ? t("settings.apiKeyUpdatePh") : t("settings.apiKeyPh")}
                    aria-label={t("settings.apiKeyAria", { name: supported.display_name })}
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
                    {credentialed ? t("settings.update") : t("settings.connect")}
                  </M3Button>
                  {credentialed && (
                    <M3Button
                      variant="quiet"
                      className="nex-provider-remove"
                      disabled={store.working}
                      onClick={() => handleDisconnect(supported)}
                    >
                      {t("settings.disconnect")}
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
            {t("settings.sectionData")}
          </h3>
          <p className="nex-settings-hint">
            {t("settings.dataHint")}
          </p>

          {(onOpenImport || onExportActive) && (
            <div className="nex-settings-field">
              <span className="nex-settings-label">{t("settings.transfer")}</span>
              <div className="nex-provider-actions">
                {onExportActive && (
                  <M3Button variant="primary" size="sm" onClick={onExportActive}>
                    {t("settings.exportActive")}
                  </M3Button>
                )}
                {onOpenImport && (
                  <M3Button variant="quiet" size="sm" onClick={onOpenImport}>
                    {t("settings.importBtn")}
                  </M3Button>
                )}
              </div>
            </div>
          )}

          <div className="nex-danger-zone">
            <h4 className="nex-danger-title">{t("settings.dangerTitle")}</h4>
            <p className="nex-danger-text">
              {t("settings.dangerText")}
            </p>
            {!confirmingClear ? (
              <M3Button variant="destructive" onClick={openClearConfirmation}>
                {t("settings.clearBtn")}
              </M3Button>
            ) : (
              <div className="nex-danger-confirm">
                <label className="nex-settings-label" htmlFor="clear-confirm-input">
                  {t("settings.clearConfirmLabel", { phrase: CLEAR_CONFIRMATION_PHRASE })}
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
                    // shortcut:settings.clear-confirm.
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
                    {t("common.cancel")}
                  </M3Button>
                  <M3Button
                    variant="destructive"
                    filled
                    loading={clearing}
                    disabled={clearPhrase !== CLEAR_CONFIRMATION_PHRASE}
                    onClick={() => void handleClearData()}
                  >
                    {clearing ? t("settings.clearing") : t("settings.clearAll")}
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
          {t("settings.sectionAdvanced")}
        </h3>
        <p className="nex-settings-hint">
          {t("settings.advancedHint")}
        </p>
        {advancedError && (
          <p className="nex-settings-error nex-fade-in" role="alert">
            {advancedError}
          </p>
        )}

        <div className="nex-settings-field">
          <span className="nex-settings-label" id="flags-label">
            {t("settings.flagsLabel")}
          </span>
          <p className="nex-settings-hint">
            {t("settings.flagsHint")}
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
                <span className="nex-tag">{flag.enforced ? t("settings.enforced") : t("settings.resolvedOnly")}</span>
                <span id={`flag-${flag.name}-hint`} className="nex-settings-hint">
                  {" "}{t(flag.descriptionKey)}
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
          <summary>{t("settings.routingTitle")}</summary>
          <p className="nex-settings-hint">
            {t("settings.routingHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.chatProfile")}</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? t("common.loading")
                : summarizeRoutingProfile(advancedValues[ROUTING_CHAT_KEY] ?? null, locale)}
            </p>
          </div>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.agentProfile")}</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? t("common.loading")
                : summarizeRoutingProfile(advancedValues[ROUTING_AGENT_KEY] ?? null, locale)}
            </p>
          </div>
        </details>

        <details className="nex-settings-details">
          <summary>{t("settings.mcpTitle")}</summary>
          <p className="nex-settings-hint">
            {t("settings.mcpHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.mcpServers")}</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? t("common.loading")
                : summarizeMcpServers(advancedValues[MCP_SERVERS_KEY] ?? null, locale)}
            </p>
          </div>
        </details>

        <details className="nex-settings-details">
          <summary>{t("settings.presetTitle")}</summary>
          <p className="nex-settings-hint">
            {t("settings.presetHint")}
          </p>
          <div className="nex-settings-field">
            <span className="nex-settings-label">{t("settings.presetLabel")}</span>
            <p className="nex-settings-value">
              {advancedValues === null
                ? t("common.loading")
                : (advancedValues[PRESET_KEY] ?? t("settings.presetUnset"))}
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
          <h2 className="nex-settings-title">{t("settings.title")}</h2>
          <p className="nex-settings-subtitle">
            {t("settings.subtitle")}
          </p>
        </div>
        <M3Button variant="quiet" onClick={onClose}>
          {t("common.backToConversations")}
        </M3Button>
      </header>

      <div className="nex-settings-layout">
        <nav className="nex-settings-nav" aria-label={t("settings.sectionsAria")}>
          <div className="nex-settings-search" role="search">
            <input
              className="nex-input"
              type="search"
              value={query}
              placeholder={t("settings.searchPh")}
              aria-label={t("settings.searchAria")}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={handleSearchKeyDown}
            />
            {matches !== null && (
              <p className="nex-settings-result-count" role="status">
                {matches.length === 0
                  ? t("settings.noMatch")
                  : t("settings.matchCount", {
                      n: matches.length,
                      groups: tp(locale, "groups", matches.length),
                    })}
              </p>
            )}
          </div>
          {SECTION_ORDER.map((id) => (
            <M3RailItem
              key={id}
              label={t(SECTION_TITLES[id])}
              active={section === id}
              aria-current={section === id ? "page" : undefined}
              onClick={() => handleNavSelect(id)}
            >
              {t(SECTION_TITLES[id])}
            </M3RailItem>
          ))}
          {onReplayOnboarding && (
            <div className="nex-settings-replay">
              <M3RailItem
                label={t("settings.replayLabel")}
                onClick={onReplayOnboarding}
              >
                {t("settings.replayText")}
              </M3RailItem>
            </div>
          )}
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
                  {t("settings.searchEmptyTitle")}
                </h3>
                <p className="nex-settings-hint">
                  {t("settings.searchEmptyHint", { q: query.trim() })}
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
