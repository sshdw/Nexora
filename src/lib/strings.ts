//! EN+RU UI-chrome string catalog (hand-rolled i18n, no deps).
//!
//! Scope: user-visible frontend chrome ONLY (nav, buttons, panel titles,
//! empty states, placeholders, frontend-authored notices, onboarding,
//! palette titles, shortcut descriptions). Explicitly OUT of scope:
//!   - AI message bodies / explanations (untouched data, never translated).
//!   - Backend `CommandError.message` pass-throughs: fixed-vocabulary English
//!     by security design (secret-free error surface) — rendered as-is and
//!     never passed through this catalog.
//!   - `mockBackend.ts` (DEV-ONLY visual-QA mock, dead-code-eliminated in
//!     production) — left in English on purpose.
//!   - The clear-data typed phrase `"confirm"`: a local UX gate only (the
//!     backend mints single-use confirmation ids server-side — NEX-SEC-004) —
//!     never translated, only the surrounding label template is cataloged.
//!   - Number/date formatting beyond fixed words: `Intl` keeps using
//!     `navigator.language` (format.ts) and the existing compact helpers;
//!     only fixed words ("Just now", "Yesterday", "n/a") are cataloged.
//!   - Palette/settings-search keywords stay English matching aids (matching,
//!     not display) — titles and group labels are translated.
//!
//! Fallback: a missing RU key resolves to EN + a DEV-only console warning —
//! the UI never renders blank. `tr`/`tp` are pure (locale passed in) so
//! non-React call sites (format.ts, hook fallbacks, label helpers) stay
//! testable; React components should prefer `useStrings()` from useLocale.

export type Locale = "en" | "ru";

export function isLocale(value: string | null): value is Locale {
  return value === "en" || value === "ru";
}

/** Module-level current locale for non-React call sites. The
 * LocaleProvider keeps this in sync with React state; the default is EN
 * (zero behavior change when untouched). */
let currentLocale: Locale = "en";

export function getLocale(): Locale {
  return currentLocale;
}

export function setCurrentLocale(locale: Locale): void {
  currentLocale = locale;
}

// AC-301 split: the en/ru dictionaries and the plural tables are pure data
// and live in strings/en.ts, strings/ru.ts and strings/plural.ts. This file
// stays the tr/tp facade — every importer keeps importing from "./strings",
// and the exported surface below is identical to the pre-split one.
import { en, type StringKey } from "./strings/en";
import { ru } from "./strings/ru";
import {
  enPlural,
  pluralForm,
  ruPlural,
  type PluralForm,
  type PluralKey,
  type PluralTable,
} from "./strings/plural";

export { pluralForm };
export type { PluralForm, PluralKey, StringKey };
type Vars = Record<string, string | number>;

function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in vars ? String(vars[name]) : match
  );
}

function lookup(locale: Locale, key: StringKey): string {
  const table = locale === "ru" ? ru : en;
  const hit = (table as Record<string, string>)[key];
  if (hit !== undefined) return hit;
  if (locale !== "en") {
    if (typeof import.meta !== "undefined" && import.meta.env?.DEV) {
      console.warn(
        `[i18n] missing RU string for key "${key}" — falling back to EN`
      );
    }
    return en[key];
  }
  return en[key];
}

/** Translate a catalog key with optional `{var}` interpolation. Missing RU
 * keys fall back to EN (never blank). */
export function tr(locale: Locale, key: StringKey, vars?: Vars): string {
  return interpolate(lookup(locale, key), vars);
}

/** Whether a runtime-built key (e.g. a versioned `docs.migN` note) exists
 * in the catalog. Lets version-driven views fall back to a bare label when
 * prose for a brand-new version has not landed yet — never blank, never a
 * crash on `undefined`. */
export function hasString(key: string): key is StringKey {
  return key in en;
}

/** Plural-aware translate: picks one/few/many by locale rules, then
 * interpolates `{n}` plus extra vars. */
export function tp(
  locale: Locale,
  key: PluralKey,
  count: number,
  vars?: Vars
): string {
  const table: PluralTable = locale === "ru" ? ruPlural : enPlural;
  const forms = table[key] ?? enPlural[key];
  return interpolate(forms[pluralForm(locale, count)], { ...vars, n: count });
}

// ---- registry maps (shortcuts dialog, settings search) ----------------------
// Shortcut ids are stable (never renamed); map id → catalog key so the dialog
// localizes while the registry keeps defining. Unknown ids fall back to the
// registry's own English text.

const SHORTCUT_DESC_KEY: Record<string, StringKey> = {
  "palette.toggle": "shortcuts.desc.palette.toggle",
  "help.show-shortcuts": "shortcuts.desc.help.show-shortcuts",
  "go.tasks": "shortcuts.desc.go.tasks",
  "tabs.next": "shortcuts.desc.tabs.next",
  "tabs.prev": "shortcuts.desc.tabs.prev",
  "tabs.jump": "shortcuts.desc.tabs.jump",
  "tabs.close-active": "shortcuts.desc.tabs.close-active",
  "layout.split": "shortcuts.desc.layout.split",
  "composer.send": "shortcuts.desc.composer.send",
  "composer.newline": "shortcuts.desc.composer.newline",
  "rename.commit": "shortcuts.desc.rename.commit",
  "rename.cancel": "shortcuts.desc.rename.cancel",
  "prompt.save": "shortcuts.desc.prompt.save",
  "settings.custom-model-commit": "shortcuts.desc.settings.custom-model-commit",
  "settings.spend-commit": "shortcuts.desc.settings.spend-commit",
  "settings.search-jump": "shortcuts.desc.settings.search-jump",
  "settings.search-clear": "shortcuts.desc.settings.search-clear",
  "segmented.move": "shortcuts.desc.segmented.move",
  "segmented.edges": "shortcuts.desc.segmented.edges",
  "tabstrip.move": "shortcuts.desc.tabstrip.move",
  "tabstrip.edges": "shortcuts.desc.tabstrip.edges",
  "toolbar.move": "shortcuts.desc.toolbar.move",
  "toolbar.edges": "shortcuts.desc.toolbar.edges",
  "toolbar.dismiss": "shortcuts.desc.toolbar.dismiss",
  "palette.navigate": "shortcuts.desc.palette.navigate",
  "palette.edges": "shortcuts.desc.palette.edges",
  "palette.run": "shortcuts.desc.palette.run",
  "dialog.close": "shortcuts.desc.dialog.close",
  "dialog.trap": "shortcuts.desc.dialog.trap",
  "button.activate": "shortcuts.desc.button.activate",
};

const SHORTCUT_GROUP_KEY: Record<string, StringKey> = {
  Launcher: "shortcuts.groupLauncher",
  Go: "shortcuts.groupGo",
  Tabs: "shortcuts.groupTabs",
  Layout: "shortcuts.groupLayout",
  Composer: "shortcuts.groupComposer",
  "Inline editing": "shortcuts.groupInline",
  Search: "shortcuts.groupSearch",
  "Option navigation": "shortcuts.groupOptions",
  Dialogs: "shortcuts.groupDialogs",
};

const SHORTCUT_SCOPE_KEY: Record<string, StringKey> = {
  "Global (any focus, no open dialog)": "shortcuts.scopePalette",
  "Global (typing targets included — Ctrl+Tab inserts no text)":
    "shortcuts.scopeTyping",
  "Global (typing targets included)": "shortcuts.scopeTypingShort",
  "Global except typing targets (Alt+digits can compose characters)":
    "shortcuts.scopeAltDigits",
  "Global, typing targets included (Alt+W produces no text)":
    "shortcuts.scopeAltNoText",
  "Global, typing targets included": "shortcuts.scopeAltTyping",
  "Composer textarea": "shortcuts.scopeComposer",
  "Conversation rename input": "shortcuts.scopeRename",
  "Prompt title field": "shortcuts.scopePromptTitle",
  "Custom model id field": "shortcuts.scopeCustomModel",
  "Spend-limit field": "shortcuts.scopeSpend",

  "Settings search field (matches present)": "shortcuts.scopeSettingsSearch",
  "Settings search field": "shortcuts.scopeSettingsSearchEsc",
  "Focused segmented group (automatic activation)": "shortcuts.scopeSegmented",
  "Focused segmented group": "shortcuts.scopeSegmentedEdges",
  "Focused conversation tab (automatic activation)": "shortcuts.scopeTabstrip",
  "Focused conversation tab": "shortcuts.scopeTabstripEdges",
  "Focused toolbar": "shortcuts.scopeToolbar",
  "Floating toolbar only (docked toolbars stay)": "shortcuts.scopeToolbarFloat",
  "Command-palette results": "shortcuts.scopePaletteNav",
  "Any open dialog (except busy/in-flight)": "shortcuts.scopeDialog",
  "Any open dialog": "shortcuts.scopeDialogTrap",
  "Focused button (native behaviour, no custom handler)":
    "shortcuts.scopeButton",
};

export function shortcutDesc(
  locale: Locale,
  id: string,
  fallback: string
): string {
  const key = SHORTCUT_DESC_KEY[id];
  return key ? tr(locale, key) : fallback;
}

export function shortcutGroup(locale: Locale, group: string): string {
  const key = SHORTCUT_GROUP_KEY[group];
  return key ? tr(locale, key) : group;
}

export function shortcutScope(locale: Locale, scope: string): string {
  const key = SHORTCUT_SCOPE_KEY[scope];
  return key ? tr(locale, key) : scope;
}

/** Map a static settings-search entry title to its catalog key (titles are
 * the stable part; keywords stay English matching aids). Returns null for
 * dynamic provider rows, which compose `settings.apiKeySearchTitle`. */
const SETTINGS_SEARCH_TITLE_KEY: Record<string, StringKey> = {
  Appearance: "settings.sectionAppearance",
  Theme: "settings.searchTheme",
  "Provider & model": "settings.sectionProvider",
  Provider: "settings.searchProvider",
  Model: "settings.searchModel",
  "Custom model ID": "settings.searchCustomModel",
  "Agent & budgets": "settings.sectionAgent",
  "Autonomy mode": "settings.searchAutonomy",
  "Access profile": "access.label",
  "Run cost limit": "settings.searchSpend",
  "Workspace folder": "settings.sectionWorkspace",
  "Current root": "settings.searchCurrentRoot",
  "Recent folders": "settings.searchRecent",
  Credentials: "settings.sectionCredentials",
  "Provider credentials": "settings.searchProviderCreds",
  "Data management": "settings.sectionData",
  "Clear all application data": "settings.searchClearAll",
  "Import conversation": "settings.searchImport",
  "Export conversation": "settings.searchExport",
  Advanced: "settings.sectionAdvanced",
  "Feature flags": "settings.searchFlags",
  "Routing profiles": "settings.searchRouting",
  "MCP servers": "settings.searchMcp",
  "Agent preset": "settings.searchPreset",
};

export function settingsSearchTitle(locale: Locale, titleEn: string): string {
  const key = SETTINGS_SEARCH_TITLE_KEY[titleEn];
  return key ? tr(locale, key) : titleEn;
}
