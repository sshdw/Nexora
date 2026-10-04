//! Keyboard-shortcut registry (hotkeys): the single source of truth for
//! every keyboard shortcut in Nexora.
//!
//! Before adding a shortcut, read this file first: pick an unused combo
//! from the inventory below instead of re-mapping collisions by hand (the
//! pre-registry norm — see PRs #90/#93 reviews). New shortcuts extend
//! SHORTCUTS; they never re-bind an inventoried combo without a proposal.
//!
//! Conventions:
//!   - `id` is stable (never renamed once shipped); handlers that implement
//!     an effect carry a `// shortcut:<id>` comment pointing here.
//!   - `keys` are display labels (what the help dialog renders in <kbd>).
//!   - `combos` (global shortcuts only) are the machine-readable definition
//!     the App-level effects match with `matchesCombo` — one definition,
//!     read by both the effect and the dialog, so they cannot drift apart.
//!     Scoped/widget-local keys (Enter/Escape/arrows inside a focused
//!     widget) have no `combos`: their context IS the scope, and wiring them
//!     through a global matcher would change behavior. They are inventoried
//!     here with their source handler ref and left inline.
//!   - `scope` names the context that owns the keys. Unmodified keys are
//!     safe exactly because only one context owns focus at a time.
//!   - `source` is the implementing handler as file:line on origin/main
//!     (base 8ef7fe9). Line numbers drift; the handler name does not.
//!
//! Collision analysis (2026-10-03, full grep over src/**/*.tsx):
//!   - Escape is owned by exactly one context at a time (open dialog >
//!     floating toolbar > rename input > settings search > zen mode), and
//!     zen explicitly yields to dialogs (App.tsx) — no conflict.
//!   - Enter is likewise context-scoped (composer / rename / prompt title /
//!     settings fields / palette run / settings search) — no conflict.
//!   - Arrows/Home/End are scoped to the focused widget (segmented group,
//!     tab strip, toolbar, palette list) — no conflict.
//!   - Ctrl+K / Ctrl+P (palette): the Ctrl map owns only Tab/PageUp/PageDown
//!     (useConversationTabs.ts:19-20), so the alias is collision-free.
//!   - Ctrl+/ + F1 (this task, help.show-shortcuts): no existing handler
//!     binds "/" or "F1" with any modifier — collision-free. Ctrl+/ is the
//!     primary (web-app norm for a shortcuts overlay); F1 is the alias
//!     (universal "help" key; preventDefault suppresses browser help).
//!     Chords (e.g. Ctrl+K Ctrl+S) are intentionally unsupported: every
//!     shortcut is a single key press with modifiers.
//!   - Ctrl+Shift+A / Ctrl+Shift+H (activity/health): the Ctrl map owns only
//!     Tab/PageUp/PageDown (+ palette K/P, help //?) and Ctrl+Shift+Tab is
//!     owned by tabs.prev — A/H with Ctrl+Shift are unbound, collision-free.
//!   - Ctrl+` (terminal): the Ctrl map owns only Tab/PageUp/PageDown (+
//!     palette K/P, help //?, activity/health A/H) — backtick with Ctrl is
//!     unbound, collision-free (VS Code norm; inserts no text).
//!   - Ctrl+Shift+T (tasks): the Ctrl map owns only Tab/PageUp/PageDown (+
//!     palette K/P, help //?, activity/health A/H, terminal `) — T with
//!     Ctrl+Shift is unbound, collision-free (inserts no text).
//!   - Ctrl+Shift+U (code audit): same map plus tasks T — U with Ctrl+Shift
//!     is unbound, collision-free (inserts no text).
//!
//! Out of scope (deferred, documented): user-customizable bindings. The
//! registry shape (stable ids + combo definitions) is designed so a future
//! remap UI can override `combos` per id without touching handlers.

/** Modifier-aware combo: the machine-readable half of a global shortcut. */
export interface ShortcutCombo {
  /** event.key value ("Tab", "PageDown", "w", "/", "F1", ...). */
  key: string;
  /** Required ctrlKey state; undefined = don't care (preserves legacy
   * branches that never checked the modifier — see scope notes). */
  ctrl?: boolean;
  /** Required altKey state. */
  alt?: boolean;
  /** Required metaKey state. */
  meta?: boolean;
  /** Required shiftKey state; "any" preserves branches that dispatch on
   * shift AFTER the combo match (Ctrl+Tab/Ctrl+Shift+Tab). */
  shift?: boolean | "any";
  /** Case-insensitive key match (preserves branches that lowercased
   * event.key before comparing — the Alt+letter and Ctrl+letter maps). */
  ci?: boolean;
}

/** Minimal keyboard-event surface (works for both React and DOM events). */
export interface KeyEventLike {
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** True when the event satisfies every specified combo field. Unspecified
 * modifiers are ignored — the caller keeps its own outer guards, so a
 * combo only ever narrows the key comparison it replaces. */
export function matchesCombo(event: KeyEventLike, combo: ShortcutCombo): boolean {
  if (combo.ctrl !== undefined && combo.ctrl !== event.ctrlKey) return false;
  if (combo.alt !== undefined && combo.alt !== event.altKey) return false;
  if (combo.meta !== undefined && combo.meta !== event.metaKey) return false;
  if (combo.shift !== undefined && combo.shift !== "any" && combo.shift !== event.shiftKey) {
    return false;
  }
  if (combo.ci === true) {
    return event.key.toLowerCase() === combo.key.toLowerCase();
  }
  return event.key === combo.key;
}

/** True when any combo in the list matches (multi-key shortcuts). */
export function matchesAnyCombo(event: KeyEventLike, combos: readonly ShortcutCombo[]): boolean {
  return combos.some((combo) => matchesCombo(event, combo));
}

/** One inventoried shortcut. */
export interface ShortcutEntry {
  /** Stable id, referenced by `// shortcut:<id>` handler comments. */
  id: string;
  /** Display labels rendered as <kbd> chips in the help dialog. */
  keys: string[];
  /** Group heading in the help dialog. */
  group: string;
  /** Context that owns the keys (global guard or focused widget). */
  scope: string;
  /** What the shortcut does. */
  description: string;
  /** Implementing handler (file:line on origin/main base 8ef7fe9). */
  source: string;
  /** Machine-readable definition for global shortcuts; omitted for
   * scoped widget-local keys (see module header). */
  combos?: readonly ShortcutCombo[];
}

// --- Combo definitions (single-definition: App effects read these). --------

const NO_META = { alt: false, meta: false } as const;

export const COMBO_TAB_NEXT_TAB: ShortcutCombo = { key: "Tab", ctrl: true, ...NO_META, shift: "any" };
export const COMBO_TAB_NEXT_PAGEDOWN: ShortcutCombo = { key: "PageDown", ctrl: true, ...NO_META, shift: "any" };
export const COMBO_TAB_PREV_PAGEDUP: ShortcutCombo = { key: "PageUp", ctrl: true, ...NO_META, shift: "any" };
export const COMBO_TAB_CLOSE: ShortcutCombo = { key: "w", ctrl: false, alt: true, meta: false, shift: "any", ci: true };
export const COMBO_SPLIT_TOGGLE: ShortcutCombo = { key: "s", ctrl: false, alt: true, meta: false, shift: "any", ci: true };
export const COMBO_ZEN_TOGGLE: ShortcutCombo = { key: "z", ctrl: false, alt: true, meta: false, shift: "any", ci: true };
export const COMBO_PALETTE_K: ShortcutCombo = { key: "k", ctrl: true, alt: false, meta: false, shift: "any", ci: true };
export const COMBO_PALETTE_P: ShortcutCombo = { key: "p", ctrl: true, alt: false, meta: false, shift: "any", ci: true };
/** Alt+1..Alt+9 are matched by range, not by combo (position math lives in
 * the handler): this entry documents the guard exactly as implemented. */
export const COMBO_TAB_JUMP_GUARD: ShortcutCombo = { key: "", ctrl: false, alt: true, meta: false, shift: "any" };
export const COMBO_HELP_SLASH: ShortcutCombo = { key: "/", ctrl: true, alt: false, meta: false, shift: "any" };
export const COMBO_HELP_QUESTION: ShortcutCombo = { key: "?", ctrl: true, alt: false, meta: false, shift: "any" };
export const COMBO_HELP_F1: ShortcutCombo = { key: "F1", ctrl: false, alt: false, meta: false, shift: "any" };
/** Ctrl+Shift+A / Ctrl+Shift+H (activity/health): the Ctrl map owns only
 * Tab/PageUp/PageDown (+ palette K/P, help //?), and Ctrl+Shift+Tab is
 * owned by tabs.prev — A/H with Ctrl+Shift are unbound, collision-free.
 * `ci` matches the "A"/"H" key value Shift produces. */
export const COMBO_ACTIVITY_OPEN: ShortcutCombo = { key: "a", ctrl: true, alt: false, meta: false, shift: true, ci: true };
export const COMBO_HEALTH_OPEN: ShortcutCombo = { key: "h", ctrl: true, alt: false, meta: false, shift: true, ci: true };
/** Ctrl+` (terminal): the Ctrl map owns only Tab/PageUp/PageDown (+
 * palette K/P, help //?, activity/health A/H) — backtick with Ctrl is
 * unbound, collision-free (VS Code terminal norm; inserts no text, so it
 * opens from anywhere including typing targets). */
export const COMBO_TERMINAL_OPEN: ShortcutCombo = { key: "`", ctrl: true, alt: false, meta: false, shift: "any" };
/** Ctrl+Shift+T (tasks): the Ctrl map owns only Tab/PageUp/PageDown (+
 * palette K/P, help //?, activity/health A/H, terminal `) — T with
 * Ctrl+Shift is unbound, collision-free (`ci` matches the "T" key value
 * Shift produces; inserts no text, so it opens from anywhere including
 * typing targets). */
export const COMBO_TASKS_OPEN: ShortcutCombo = { key: "t", ctrl: true, alt: false, meta: false, shift: true, ci: true };
/** Ctrl+Shift+U (code audit): the Ctrl map owns only Tab/PageUp/PageDown (+
 * palette K/P, help //?, activity/health A/H, terminal `, tasks T) — U with
 * Ctrl+Shift is unbound, collision-free (`ci` matches the "U" key value
 * Shift produces; inserts no text, so it opens from anywhere including
 * typing targets). */
export const COMBO_AUDIT_OPEN: ShortcutCombo = { key: "u", ctrl: true, alt: false, meta: false, shift: true, ci: true };

// --- The inventory (100% of the Phase-1 grep — see module header). ----------

export const SHORTCUTS: readonly ShortcutEntry[] = [
  // Launcher.
  {
    id: "palette.toggle",
    keys: ["Ctrl+K", "Ctrl+P"],
    group: "Launcher",
    scope: "Global (any focus, no open dialog)",
    description: "Open / close the command palette",
    source: "App.tsx:467-478 (palette toggle effect)",
    combos: [COMBO_PALETTE_K, COMBO_PALETTE_P],
  },
  {
    id: "help.show-shortcuts",
    keys: ["Ctrl+/", "Ctrl+?", "F1"],
    group: "Launcher",
    scope: "Global (any focus, no open dialog)",
    description: "Open this keyboard-shortcuts reference",
    source: "App.tsx (shortcuts-dialog effect, added with the registry)",
    combos: [COMBO_HELP_SLASH, COMBO_HELP_QUESTION, COMBO_HELP_F1],
  },
  // Go (navigation destinations).
  {
    id: "go.activity",
    keys: ["Ctrl+Shift+A"],
    group: "Go",
    scope: "Global (any focus, no open dialog)",
    description: "Open the Activity feed",
    source: "App.tsx (activity-health effect)",
    combos: [COMBO_ACTIVITY_OPEN],
  },
  {
    id: "go.health",
    keys: ["Ctrl+Shift+H"],
    group: "Go",
    scope: "Global (any focus, no open dialog)",
    description: "Open the Project health view",
    source: "App.tsx (activity-health effect)",
    combos: [COMBO_HEALTH_OPEN],
  },
  {
    id: "go.terminal",
    keys: ["Ctrl+`"],
    group: "Go",
    scope: "Global (any focus, no open dialog)",
    description: "Open the workspace Terminal",
    source: "App.tsx (terminal effect)",
    combos: [COMBO_TERMINAL_OPEN],
  },
  {
    id: "go.tasks",
    keys: ["Ctrl+Shift+T"],
    group: "Go",
    scope: "Global (any focus, no open dialog)",
    description: "Open the Task manager",
    source: "App.tsx (tasks effect)",
    combos: [COMBO_TASKS_OPEN],
  },
  {
    id: "go.audit",
    keys: ["Ctrl+Shift+U"],
    group: "Go",
    scope: "Global (any focus, no open dialog)",
    description: "Open the Code audit",
    source: "App.tsx (audit effect)",
    combos: [COMBO_AUDIT_OPEN],
  },
  // Tabs.
  {
    id: "tabs.next",
    keys: ["Ctrl+Tab", "Ctrl+PgDn"],
    group: "Tabs",
    scope: "Global (typing targets included — Ctrl+Tab inserts no text)",
    description: "Next conversation tab",
    source: "App.tsx:402-415 (tab cycle effect)",
    combos: [COMBO_TAB_NEXT_TAB, COMBO_TAB_NEXT_PAGEDOWN],
  },
  {
    id: "tabs.prev",
    keys: ["Ctrl+Shift+Tab", "Ctrl+PgUp"],
    group: "Tabs",
    scope: "Global (typing targets included)",
    description: "Previous conversation tab",
    source: "App.tsx:402-421 (tab cycle effect)",
    combos: [COMBO_TAB_NEXT_TAB, COMBO_TAB_PREV_PAGEDUP],
  },
  {
    id: "tabs.jump",
    keys: ["Alt+1 … Alt+8", "Alt+9 = last"],
    group: "Tabs",
    scope: "Global except typing targets (Alt+digits can compose characters)",
    description: "Jump to tab by position",
    source: "App.tsx:442-451 (Alt+digit branch)",
    combos: [COMBO_TAB_JUMP_GUARD],
  },
  {
    id: "tabs.close-active",
    keys: ["Alt+W"],
    group: "Tabs",
    scope: "Global, typing targets included (Alt+W produces no text)",
    description: "Close the active tab",
    source: "App.tsx:429-433 (Alt+W branch)",
    combos: [COMBO_TAB_CLOSE],
  },
  // Layout.
  {
    id: "layout.split",
    keys: ["Alt+S"],
    group: "Layout",
    scope: "Global, typing targets included",
    description: "Toggle split pane",
    source: "App.tsx:434-437 (Alt+S branch)",
    combos: [COMBO_SPLIT_TOGGLE],
  },
  {
    id: "layout.zen",
    keys: ["Alt+Z"],
    group: "Layout",
    scope: "Global, typing targets included (yields to open dialogs)",
    description: "Toggle zen reading mode",
    source: "App.tsx:438-441 (Alt+Z branch)",
    combos: [COMBO_ZEN_TOGGLE],
  },
  {
    id: "zen.exit",
    keys: ["Esc"],
    group: "Layout",
    scope: "Zen mode with no dialog open (dialogs own Esc)",
    description: "Exit zen reading mode",
    source: "App.tsx:393-398 (zen Esc branch)",
  },
  // Composer.
  {
    id: "composer.send",
    keys: ["Enter"],
    group: "Composer",
    scope: "Composer textarea",
    description: "Send message",
    source: "ConversationView.tsx:463-468 (composer onKeyDown)",
  },
  {
    id: "composer.newline",
    keys: ["Shift+Enter"],
    group: "Composer",
    scope: "Composer textarea",
    description: "New line without sending",
    source: "ConversationView.tsx:463-468 (Shift+Enter falls through)",
  },
  // Inline editing.
  {
    id: "rename.commit",
    keys: ["Enter"],
    group: "Inline editing",
    scope: "Conversation rename input",
    description: "Commit rename",
    source: "ConversationItem.tsx:79-85 (rename onKeyDown)",
  },
  {
    id: "rename.cancel",
    keys: ["Esc"],
    group: "Inline editing",
    scope: "Conversation rename input",
    description: "Cancel rename",
    source: "ConversationItem.tsx:83-85 (rename onKeyDown)",
  },
  {
    id: "prompt.save",
    keys: ["Enter"],
    group: "Inline editing",
    scope: "Prompt title field",
    description: "Save prompt",
    source: "PromptLibraryView.tsx:339-344 (title onKeyDown)",
  },
  {
    id: "settings.custom-model-commit",
    keys: ["Enter"],
    group: "Inline editing",
    scope: "Custom model id field",
    description: "Commit custom model id",
    source: "SettingsView.tsx:676-677 (custom model onKeyDown)",
  },
  {
    id: "settings.spend-commit",
    keys: ["Enter"],
    group: "Inline editing",
    scope: "Spend-limit field",
    description: "Commit spend limit",
    source: "SettingsView.tsx:737-738 (spend onKeyDown)",
  },
  {
    id: "settings.clear-confirm",
    keys: ["Enter"],
    group: "Inline editing",
    scope: "Clear-data confirm field",
    description: "Confirm clear data",
    source: "SettingsView.tsx:926-927 (clear confirm onKeyDown)",
  },
  // Search.
  {
    id: "settings.search-jump",
    keys: ["Enter"],
    group: "Search",
    scope: "Settings search field (matches present)",
    description: "Jump to best matching group",
    source: "SettingsView.tsx:416-422 (search onKeyDown)",
  },
  {
    id: "settings.search-clear",
    keys: ["Esc"],
    group: "Search",
    scope: "Settings search field",
    description: "Clear search",
    source: "SettingsView.tsx:419-421 (search onKeyDown)",
  },
  // Widget navigation (scoped roving focus — context is the scope).
  {
    id: "segmented.move",
    keys: ["←", "→", "↑", "↓"],
    group: "Option navigation",
    scope: "Focused segmented group (automatic activation)",
    description: "Move and select option",
    source: "M3SegmentedGroup.tsx:83-93 (segment onKeyDown)",
  },
  {
    id: "segmented.edges",
    keys: ["Home", "End"],
    group: "Option navigation",
    scope: "Focused segmented group",
    description: "First / last option",
    source: "M3SegmentedGroup.tsx:94-101 (segment onKeyDown)",
  },
  {
    id: "tabstrip.move",
    keys: ["←", "→", "↑", "↓"],
    group: "Option navigation",
    scope: "Focused conversation tab (automatic activation)",
    description: "Move to neighbouring tab",
    source: "ConversationTabs.tsx:91-101 (tab onKeyDown)",
  },
  {
    id: "tabstrip.edges",
    keys: ["Home", "End"],
    group: "Option navigation",
    scope: "Focused conversation tab",
    description: "First / last tab",
    source: "ConversationTabs.tsx:102-110 (tab onKeyDown)",
  },
  {
    id: "toolbar.move",
    keys: ["←", "→", "↑", "↓"],
    group: "Option navigation",
    scope: "Focused toolbar",
    description: "Move between toolbar items",
    source: "M3Toolbar.tsx:80-113 (toolbar onKeyDown)",
  },
  {
    id: "toolbar.edges",
    keys: ["Home", "End"],
    group: "Option navigation",
    scope: "Focused toolbar",
    description: "First / last toolbar item",
    source: "M3Toolbar.tsx:107-112 (toolbar onKeyDown)",
  },
  {
    id: "toolbar.dismiss",
    keys: ["Esc"],
    group: "Option navigation",
    scope: "Floating toolbar only (docked toolbars stay)",
    description: "Dismiss the selection-context toolbar",
    source: "M3Toolbar.tsx:74-78 (toolbar onKeyDown)",
  },
  {
    id: "palette.navigate",
    keys: ["↑", "↓"],
    group: "Option navigation",
    scope: "Command-palette results",
    description: "Move highlight",
    source: "CommandPalette.tsx:81-94 (palette onKeyDown)",
  },
  {
    id: "palette.edges",
    keys: ["Home", "End"],
    group: "Option navigation",
    scope: "Command-palette results",
    description: "First / last result",
    source: "CommandPalette.tsx:95-102 (palette onKeyDown)",
  },
  {
    id: "palette.run",
    keys: ["Enter"],
    group: "Option navigation",
    scope: "Command-palette results",
    description: "Run highlighted command",
    source: "CommandPalette.tsx:103-106 (palette onKeyDown)",
  },
  // Dialogs.
  {
    id: "dialog.close",
    keys: ["Esc"],
    group: "Dialogs",
    scope: "Any open dialog (except busy/in-flight)",
    description: "Close dialog",
    source: "Modal.tsx:117-123 (ModalShell onKeyDown)",
  },
  {
    id: "dialog.trap",
    keys: ["Tab", "Shift+Tab"],
    group: "Dialogs",
    scope: "Any open dialog",
    description: "Cycle focus inside the dialog",
    source: "Modal.tsx:124-148 (ModalShell focus trap)",
  },
  {
    id: "button.activate",
    keys: ["Enter", "Space"],
    group: "Dialogs",
    scope: "Focused button (native behaviour, no custom handler)",
    description: "Activate button",
    source: "M3Button.tsx:9 (native <button> semantics)",
  },
];

/** Group names in dialog display order (first appearance in SHORTCUTS). */
export const SHORTCUT_GROUPS: readonly string[] = (() => {
  const seen: string[] = [];
  for (const entry of SHORTCUTS) {
    if (!seen.includes(entry.group)) seen.push(entry.group);
  }
  return seen;
})();

/** Entries of one dialog group, in inventory order. */
export function shortcutsInGroup(group: string): ShortcutEntry[] {
  return SHORTCUTS.filter((entry) => entry.group === group);
}
