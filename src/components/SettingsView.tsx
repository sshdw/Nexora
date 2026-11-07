//! Settings groups (slim surface: 7 top-level groups, in-page search, and an
//! Advanced section for power keys).
//!
//! AC-208 (review §3.1): the seven section bodies live in `settings/` (one
//! file per section id) and the shared data model (section vocabulary,
//! backend keys, flag definitions, parsers/summarizers) in
//! `settings/model.ts`. This file keeps the shell, the in-page search index,
//! and every piece of state — the once-per-mount power-key load and the
//! cross-section persistence of drafts (custom model, spend limit, typed
//! credentials, clear-all error) behave exactly as before the split, which
//! is why the state did not move with the sections.

import { useEffect, useMemo, useState } from "react";

import { scoreCommand } from "../lib/commands";
import type { AutonomyMode, SupportedProvider } from "../lib/tauri";
import { clearApplicationData, getSetting, setSetting } from "../lib/tauri";
import { getLocale, settingsSearchTitle, tp, tr, type Locale } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import type { AppearanceStore } from "../lib/useAppearance";
import type { AutonomyStore } from "../lib/useAutonomy";
import { isCustomModelId, type ProvidersStore } from "../lib/useProviders";
import type { SpendLimitStore } from "../lib/useSpendLimit";
import M3Button from "./M3Button";
import M3RailItem from "./M3RailItem";
import AppearanceSection from "./settings/AppearanceSection";
import ProviderSection from "./settings/ProviderSection";
import AgentSection from "./settings/AgentSection";
import WorkspaceSection from "./settings/WorkspaceSection";
import CredentialsSection from "./settings/CredentialsSection";
import DataSection from "./settings/DataSection";
import AdvancedSection from "./settings/AdvancedSection";
import {
  FLAG_DEFS,
  MCP_SERVERS_KEY,
  PRESET_KEY,
  RECENT_KEY,
  ROUTING_AGENT_KEY,
  ROUTING_CHAT_KEY,
  SECTION_ORDER,
  SECTION_TITLES,
  parseFlagValue,
  parseRecentList,
  type SettingsSectionId,
} from "./settings/model";

/** Re-exported for the command-palette registry (deep links). */
export type { SettingsSectionId };

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

// NEX-SEC-004: there is deliberately NO typed-phrase gate and NO in-app
// confirm dialog here. `clearApplicationData` raises a blocking NATIVE OS
// confirmation prompt, and the backend mints a single-use id bound to
// clear-all only if the user accepts. That native prompt is the single,
// honest confirmation: the removed local gate was UX-only and would have
// meant two prompts for one action, with the weaker of the two (a renderer-
// skippable local check) implying it was the real one.

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
  /** Agent-autonomy default store lifted in App (single source shared with
   * the conversation header — NEX-FE-003). */
  autonomy: AutonomyStore;
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
  autonomy,
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
  // Clear-all-data state. The confirmation is the native OS prompt raised by
  // `clearApplicationData` (see CLEAR-CONFIRMATION note above), so there is no
  // local arming step here — `clearError` reports a cancel or a real failure
  // and clears on the next attempt.
  const [clearError, setClearError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);
  // Workspace-group state: recent folders (read-only here).
  const [recentFolders, setRecentFolders] = useState<string[] | null>(null);
  // Advanced-group state: power-key values loaded once on mount.
  const [advancedValues, setAdvancedValues] = useState<Record<string, string | null> | null>(null);
  const [advancedError, setAdvancedError] = useState<string | null>(null);
  const [flagValues, setFlagValues] = useState<Record<string, boolean>>({});
  const [flagsSaving, setFlagsSaving] = useState<string | null>(null);
  const [flagsError, setFlagsError] = useState<string | null>(null);

  // Power-key values (recent folders, flags, routing profiles, MCP
  // servers, agent preset) load once on mount. The old focus/visibility
  // re-sync existed only because ConversationView could persist autonomy
  // while Settings was open — autonomy is now a shared store (NEX-FE-003),
  // and Settings is the sole writer of every other key loaded here, so
  // this copy can never go stale.
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const results = await Promise.all([
          getSetting(RECENT_KEY),
          getSetting(ROUTING_CHAT_KEY),
          getSetting(ROUTING_AGENT_KEY),
          getSetting(MCP_SERVERS_KEY),
          getSetting(PRESET_KEY),
          ...FLAG_DEFS.map((flag) => getSetting(`flags.${flag.name}`)),
        ]);
        if (cancelled) return;
        const [recentRaw, chatRaw, agentRaw, mcpRaw, presetRaw, ...flagRaws] = results;
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
    return () => {
      cancelled = true;
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

  // Single-source write (NEX-FE-003): the store optimistically updates and
  // rolls back on failure, so the conversation header reflects the change
  // (or its rollback) immediately. Live runs keep their gate here — the
  // conversation header owns the live-switch (ConversationView).
  const handleAutonomyChange = async (mode: AutonomyMode) => {
    await autonomy.setMode(mode);
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

  const handleClearData = async () => {
    if (clearing) return;
    setClearing(true);
    setClearError(null);
    try {
      // `clearApplicationData` raises the native OS confirmation prompt and
      // only mints a clear-all-bound id if the user accepts; the backend then
      // clears everything atomically — a failure leaves all data intact.
      await clearApplicationData();
      // The cleared settings included the provider/model selection.
      await store.reload();
      onDataCleared();
    } catch (e) {
      // A cancel at the native prompt is reported honestly and distinctly:
      // nothing was deleted, and the panel stays open for another attempt.
      const kind =
        typeof e === "object" && e !== null && "kind" in e
          ? String((e as { kind: unknown }).kind)
          : "";
      setClearError(
        kind === "confirmationRequired"
          ? tr(getLocale(), "settings.clearCancelled")
          : typeof e === "object" && e !== null && "message" in e
            ? String((e as { message: unknown }).message)
            : tr(getLocale(), "settings.clearFail"),
      );
    } finally {
      setClearing(false);
    }
  };

  const renderSection = (id: SettingsSectionId) => {
    switch (id) {
      case "appearance":
        return (
          <AppearanceSection key={id} appearance={appearance} locale={locale} onLocaleChange={onLocaleChange} />
        );
      case "provider":
        return (
          <ProviderSection
            key={id}
            store={store}
            selectedDefinition={selectedDefinition}
            selectedModels={selectedModels}
            effectiveModel={effectiveModel}
            customActive={customActive}
            customValue={customValue}
            onProviderChange={(name) => void handleProviderChange(name)}
            onModelSelect={handleModelSelect}
            onCustomDraftChange={setCustomDraft}
            onCustomCommit={commitCustom}
          />
        );
      case "agent":
        return (
          <AgentSection
            key={id}
            autonomy={autonomy}
            spendLimit={spendLimit}
            spendValue={spendValue}
            onSpendDraftChange={setSpendDraft}
            onSpendCommit={commitSpendLimit}
            onAutonomyChange={(mode) => void handleAutonomyChange(mode)}
          />
        );
      case "workspace":
        return (
          <WorkspaceSection
            key={id}
            workspaceRoot={workspaceRoot}
            workspaceLoading={workspaceLoading}
            recentFolders={recentFolders}
          />
        );
      case "credentials":
        return (
          <CredentialsSection
            key={id}
            store={store}
            draftKeys={draftKeys}
            onDraftChange={(name, value) =>
              setDraftKeys((prev) => ({ ...prev, [name]: value }))
            }
            onConnect={(definition) => void handleConnect(definition)}
            onDisconnect={(definition) => void handleDisconnect(definition)}
          />
        );
      case "data":
        return (
          <DataSection
            key={id}
            clearing={clearing}
            clearError={clearError}
            onClearData={() => void handleClearData()}
            onOpenImport={onOpenImport}
            onExportActive={onExportActive}
          />
        );
      case "advanced":
        return (
          <AdvancedSection
            key={id}
            locale={locale}
            advancedValues={advancedValues}
            advancedError={advancedError}
            flagValues={flagValues}
            flagsSaving={flagsSaving}
            flagsError={flagsError}
            onFlagToggle={(name, next) => void handleFlagToggle(name, next)}
          />
        );
    }
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
