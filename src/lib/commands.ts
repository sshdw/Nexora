//! Command-palette registry (NL-palette): the single inventory of invokable
//! app commands the Ctrl+K palette launches.
//!
//! Built from the Phase-1 inventory — every entry wraps the SAME handler
//! the equivalent button/sidebar row/shortcut calls (App.tsx), so invoking
//! from the palette === clicking the UI (no parallel command system, no
//! duplicated logic). Sources:
//!   sidebar destinations ... Sidebar.tsx:107-121 (Settings/Library/VCS/
//!                             Import rail entries) + App.tsx:255-278 openers
//!   header actions ......... App.tsx:76-101 (Split toggle, Export)
//!   tab commands ........... useConversationTabs.ts:12-28 shortcut map +
//!                             ConversationTabs.tsx:174-203 strip actions
//!   VCS actions ............ VersionControlPanel.tsx (tabs: Working tree +
//!                             Timeline; Refresh, commit composer, push,
//!                             commit explain)
//!   activity & health ....... ActivityHealthPanel.tsx (read-only feed +
//!                             project snapshot; go.activity/go.health deep
//!                             links raise the panel's tab request)
//!   terminal ................ TerminalPanel.tsx (workspace command runs;
//!                             go.terminal opens, focus-input/clear raise the
//!                             panel's request)
//!   tasks ................... TaskPanel.tsx (user task lists + the autonomous
//!                             plan → act → verify → report loop; go.tasks
//!                             opens)
//!   chat commands .......... ConversationView.tsx:463-469 composer Enter,
//!                             497-511 Send button; App.tsx:233-245 create
//!   settings sections ...... SettingsView GROUP MAP comment (7 groups)
//!   help ................... ShortcutsDialog.tsx (show-shortcuts command),
//!                             OnboardingFlow.tsx (replay-onboarding command)
//!
//! NL-tolerance WITHOUT an LLM (explicit scope decision): each command
//! carries hand-written keyword aliases (including common verbs like
//! "commmit"/"setings"), and matching is a hand-rolled case-insensitive
//! subsequence scorer (matchCommand below) — typos survive because extra or
//! missing letters only lower the score, never veto. No network, no deps.
//!
//! Ranking characterization (empty MRU; MRU boosts add +8 per recency rank,
//! so a recently used runner-up can outrank these):
//!   query .............. expected top hit
//!   "commmit" .......... vcs.commit-focus (typo alias)
//!   "api key" .......... settings.credentials
//!   "dark" ............. settings.appearance
//!   "clear" ............ settings.data
//!   "close tab" ........ tabs.close-active
//!   "prompt library" ... go.library
//!   "new conv" ......... chat.new
//!
//! MRU decision: in-memory only (module-level Map, session lifetime).
//! Persisting via the settings store was evaluated and rejected — tab ids
//! (useConversationTabs.ts:30-36) document why id-backed state needs a
//! validation UX the shell does not have yet; command ids are stable, but
//! the existing settings store holds device preferences, not usage stats,
//! and a new persistence shape is scope creep for this task.
//!
//! Localization: titles + section labels render through the string catalog
//! (`buildCommands(deps, locale)` — App rebuilds on language switch).
//! Keywords stay English matching aids (matching, not display).

import { tr, type Locale } from "./strings";

/** A settings section the palette can deep-link (SettingsView group ids). */
export type PaletteSettingsSection =
  | "appearance"
  | "provider"
  | "agent"
  | "workspace"
  | "credentials"
  | "data"
  | "advanced";

/** VCS panel requests the palette can raise (handled inside the panel). */
export type PaletteVcsRequest = "refresh" | "focus-commit";

/** Terminal panel requests the palette can raise (handled inside the panel). */
export type PaletteTerminalRequest = "clear" | "focus-input";

/** One invokable command: stable id, display title, NL keyword aliases, run. */
export interface PaletteCommand {
  /** Stable id (also the MRU key). Never renamed once shipped. */
  id: string;
  /** Display title (matched + shown). */
  title: string;
  /** Group label for section headers ("Go", "Tabs", "Chat", ...). */
  section: string;
  /** NL aliases: verbs, synonyms, common misspellings. Matched + not shown. */
  keywords: string[];
  /** Wraps the existing UI handler — never reimplements it. */
  run: () => void;
}

/** Callbacks the registry wraps — all supplied by App from existing handlers. */
export interface PaletteDeps {
  goConversations: () => void;
  openSettings: (section?: PaletteSettingsSection) => void;
  openLibrary: () => void;
  openVcs: (request?: PaletteVcsRequest) => void;
  /** Open the workspace Terminal overlay (optional panel request). */
  openTerminal: (request?: PaletteTerminalRequest) => void;
  /** Open the Tasks overlay (task manager + autonomous mode). */
  openTasks: () => void;
  /** Open the Code Audit overlay (read-only repo findings). */
  openAudit: () => void;
  /** Open the Issues & PRs overlay (read-only GitHub lists). */
  openGh: () => void;
  /** Open the Activity & Health overlay on the requested tab. */
  openActivity: (tab: "activity" | "health") => void;
  newConversation: () => void;
  openImport: () => void;
  exportActive: () => void;
  focusComposer: () => void;
  tabNext: () => void;
  tabPrev: () => void;
  tabCloseActive: () => void;
  toggleSplit: () => void;
  toggleZen: () => void;
  showShortcuts: () => void;
  /** Flip the interface language EN <-> RU (Settings appearance owns it). */
  toggleLanguage: () => void;
  /** Reopen the first-run onboarding flow (Help re-entry point). */
  replayOnboarding: () => void;
  jumpToTab: (index: number) => void;
  tabCount: () => number;
}

export function buildCommands(deps: PaletteDeps, locale: Locale = "en"): PaletteCommand[] {
  const tabCount = deps.tabCount();
  const go = tr(locale, "palette.sectionGo");
  const chat = tr(locale, "palette.sectionChat");
  const tabs = tr(locale, "palette.sectionTabs");
  const help = tr(locale, "palette.sectionHelp");
  const vcs = tr(locale, "palette.sectionVcs");
  const terminal = tr(locale, "palette.sectionTerminal");
  const commands: PaletteCommand[] = [
    {
      id: "go.conversations",
      title: tr(locale, "palette.cmd.go_conversations"),
      section: go,
      keywords: ["back", "home", "chat list", "conversations", "close panel"],
      run: deps.goConversations,
    },
    {
      id: "go.library",
      title: tr(locale, "palette.cmd.go_library"),
      section: go,
      keywords: ["prompts", "prompt library", "templates", "saved prompts"],
      run: deps.openLibrary,
    },
    {
      id: "go.vcs",
      title: tr(locale, "palette.cmd.go_vcs"),
      section: go,
      keywords: ["git", "vcs", "version control", "source control", "commits", "status"],
      run: () => deps.openVcs(),
    },
    {
      id: "go.terminal",
      title: tr(locale, "palette.cmd.go_terminal"),
      section: go,
      keywords: ["terminal", "console", "shell", "command line", "cli", "run command", "prompt"],
      run: () => deps.openTerminal(),
    },
    {
      id: "go.tasks",
      title: tr(locale, "palette.cmd.go_tasks"),
      section: go,
      keywords: ["tasks", "todo", "autonomous", "plan", "checklist", "agent tasks", "run tasks"],
      run: () => deps.openTasks(),
    },
    {
      id: "go.audit",
      title: tr(locale, "palette.cmd.go_audit"),
      section: go,
      keywords: ["audit", "code audit", "lint", "static analysis", "dead code", "review", "bugs", "quality"],
      run: () => deps.openAudit(),
    },
    {
      id: "go.gh",
      title: tr(locale, "palette.cmd.go_gh"),
      section: go,
      keywords: ["github", "issues", "pull requests", "prs", "bugs", "tickets"],
      run: () => deps.openGh(),
    },
    {
      id: "go.activity",
      title: tr(locale, "palette.cmd.go_activity"),
      section: go,
      keywords: ["activity", "feed", "runs", "history", "recent", "agent runs", "timeline"],
      run: () => deps.openActivity("activity"),
    },
    {
      id: "go.health",
      title: tr(locale, "palette.cmd.go_health"),
      section: go,
      keywords: ["health", "status", "providers", "budget", "spend", "cost", "context", "git", "flags"],
      run: () => deps.openActivity("health"),
    },
    {
      id: "go.settings",
      title: tr(locale, "palette.cmd.go_settings"),
      section: go,
      keywords: ["settings", "setings", "preferences", "options", "setup", "config"],
      run: () => deps.openSettings(),
    },
    {
      id: "settings.appearance",
      title: tr(locale, "palette.cmd.settings_appearance"),
      section: go,
      keywords: ["theme", "dark", "light", "appearance", "look"],
      run: () => deps.openSettings("appearance"),
    },
    {
      id: "settings.provider",
      title: tr(locale, "palette.cmd.settings_provider"),
      section: go,
      keywords: ["provider", "model", "ai model", "llm", "openai", "anthropic", "gemini"],
      run: () => deps.openSettings("provider"),
    },
    {
      id: "settings.agent",
      title: tr(locale, "palette.cmd.settings_agent"),
      section: go,
      keywords: ["agent", "autonomy", "budget", "spend", "cost limit", "supervised"],
      run: () => deps.openSettings("agent"),
    },
    {
      id: "settings.workspace",
      title: tr(locale, "palette.cmd.settings_workspace"),
      section: go,
      keywords: ["workspace", "folder", "directory", "root", "project path"],
      run: () => deps.openSettings("workspace"),
    },
    {
      id: "settings.credentials",
      title: tr(locale, "palette.cmd.settings_credentials"),
      section: go,
      keywords: ["api key", "credentials", "connect", "key", "token", "auth"],
      run: () => deps.openSettings("credentials"),
    },
    {
      id: "settings.data",
      title: tr(locale, "palette.cmd.settings_data"),
      section: go,
      keywords: ["data", "clear", "delete all", "reset", "storage", "database"],
      run: () => deps.openSettings("data"),
    },
    {
      id: "settings.advanced",
      title: tr(locale, "palette.cmd.settings_advanced"),
      section: go,
      keywords: ["advanced", "power", "flags", "routing", "mcp", "preset", "internals"],
      run: () => deps.openSettings("advanced"),
    },
    {
      id: "settings.language",
      title: tr(locale, "palette.cmd.settings_language"),
      section: go,
      keywords: ["language", "язык", "locale", "russian", "english", "русский", "английский", "переключить язык"],
      run: deps.toggleLanguage,
    },
    {
      id: "chat.new",
      title: tr(locale, "palette.cmd.chat_new"),
      section: chat,
      keywords: ["new", "create", "start", "conversation", "chat", "compose"],
      run: deps.newConversation,
    },
    {
      id: "chat.focus-composer",
      title: tr(locale, "palette.cmd.chat_focus-composer"),
      section: chat,
      keywords: ["type", "message", "input", "composer", "write", "send", "focus"],
      run: deps.focusComposer,
    },
    {
      id: "chat.export-active",
      title: tr(locale, "palette.cmd.chat_export-active"),
      section: chat,
      keywords: ["export", "download", "save", "share", "backup"],
      run: deps.exportActive,
    },
    {
      id: "chat.import",
      title: tr(locale, "palette.cmd.chat_import"),
      section: chat,
      keywords: ["import", "upload", "restore", "open file"],
      run: deps.openImport,
    },
    {
      id: "tabs.next",
      title: tr(locale, "palette.cmd.tabs_next"),
      section: tabs,
      keywords: ["next", "tab", "forward", "switch", "cycle"],
      run: deps.tabNext,
    },
    {
      id: "tabs.prev",
      title: tr(locale, "palette.cmd.tabs_prev"),
      section: tabs,
      keywords: ["previous", "prev", "back", "tab", "switch"],
      run: deps.tabPrev,
    },
    {
      id: "tabs.close-active",
      title: tr(locale, "palette.cmd.tabs_close-active"),
      section: tabs,
      keywords: ["close", "tab", "dismiss", "shut"],
      run: deps.tabCloseActive,
    },
    {
      id: "tabs.split",
      title: tr(locale, "palette.cmd.tabs_split"),
      section: tabs,
      keywords: ["split", "side by side", "pane", "two", "compare", "divide"],
      run: deps.toggleSplit,
    },
    {
      id: "tabs.zen",
      title: tr(locale, "palette.cmd.tabs_zen"),
      section: tabs,
      keywords: ["zen", "focus", "chromeless", "reading", "distraction", "fullscreen"],
      run: deps.toggleZen,
    },
    {
      id: "help.show-shortcuts",
      title: tr(locale, "palette.cmd.help_show-shortcuts"),
      section: help,
      keywords: [
        "shortcuts",
        "hotkeys",
        "keys",
        "keybindings",
        "bindings",
        "help",
        "cheatsheet",
        "f1",
      ],
      run: deps.showShortcuts,
    },
    {
      id: "help.replay-onboarding",
      title: tr(locale, "palette.cmd.help_replay-onboarding"),
      section: help,
      keywords: [
        "onboarding",
        "walkthrough",
        "tour",
        "getting started",
        "setup",
        "first run",
        "replay",
        "welcome",
      ],
      run: deps.replayOnboarding,
    },
    {
      id: "vcs.refresh",
      title: tr(locale, "palette.cmd.vcs_refresh"),
      section: vcs,
      keywords: ["refresh", "reload", "git status", "sync", "update", "vcs"],
      run: () => deps.openVcs("refresh"),
    },
    {
      id: "vcs.commit-focus",
      title: tr(locale, "palette.cmd.vcs_commit-focus"),
      section: vcs,
      keywords: [
        "commit",
        "commmit",
        "comit",
        "stage",
        "push",
        "message",
        "save changes",
        "check in",
      ],
      run: () => deps.openVcs("focus-commit"),
    },
    {
      id: "terminal.focus-input",
      title: tr(locale, "palette.cmd.terminal_focus-input"),
      section: terminal,
      keywords: ["terminal", "focus", "input", "type", "command line", "shell prompt"],
      run: () => deps.openTerminal("focus-input"),
    },
    {
      id: "terminal.clear",
      title: tr(locale, "palette.cmd.terminal_clear"),
      section: terminal,
      keywords: ["terminal", "clear", "clean", "wipe", "scrollback", "reset output"],
      run: () => deps.openTerminal("clear"),
    },
  ];
  // Per-tab jumps mirror Alt+1..Alt+9 (useConversationTabs.ts:21) for the
  // currently open tabs only — built from live tab state, not persisted.
  for (let index = 0; index < Math.min(tabCount, 9); index += 1) {
    const position = index + 1;
    commands.push({
      id: `tabs.jump-${position}`,
      title:
        position === 9
          ? tr(locale, "palette.cmd.tabJumpLast")
          : tr(locale, "palette.cmd.tabJump", { n: position }),
      section: tabs,
      keywords: [`tab ${position}`, `go ${position}`, "jump", "switch tab"],
      run: () => deps.jumpToTab(index),
    });
  }
  return commands;
}

// --- Fuzzy matching -------------------------------------------------------
// Hand-rolled case-insensitive subsequence scorer over title + keywords.
// Empty query matches everything (score 0 — MRU ordering owns that view).
// Non-matching (not a subsequence) returns null. Typo tolerance falls out
// of subsequence semantics: "commmit" still subsequences "commit"-bearing
// haystacks, and "stngs" still reaches "settings".

export interface ScoredCommand {
  command: PaletteCommand;
  score: number;
}

function subsequenceScore(needle: string, haystack: string): number | null {
  let score = 0;
  let hi = 0;
  let consecutive = 0;
  for (let ni = 0; ni < needle.length; ni += 1) {
    const ch = needle[ni];
    let found = -1;
    for (let i = hi; i < haystack.length; i += 1) {
      if (haystack[i] === ch) {
        found = i;
        break;
      }
    }
    if (found === -1) return null;
    // Word-start and consecutive bonuses: exact/prefix hits rank first.
    if (found === 0 || haystack[found - 1] === " " || haystack[found - 1] === ":") {
      score += 12;
    } else if (found === hi) {
      consecutive += 1;
      score += 4 + consecutive;
    } else {
      consecutive = 0;
      score += 1;
    }
    // Early-position bonus: matches near the title start win ties.
    score += Math.max(0, 6 - found);
    hi = found + 1;
  }
  return score;
}

/** Score one command against the query (title weighted above keywords). */
export function scoreCommand(query: string, command: PaletteCommand): number | null {
  const needle = query.trim().toLowerCase();
  if (needle === "") return 0;
  const title = command.title.toLowerCase();
  const titleScore = subsequenceScore(needle, title);
  // Exact-substring in the title outranks any fuzzy spread.
  if (title.includes(needle)) return (titleScore ?? 0) + 60;
  if (titleScore !== null) return titleScore + 20;
  let best: number | null = null;
  for (const keyword of command.keywords) {
    const haystack = keyword.toLowerCase();
    if (haystack.includes(needle)) {
      const score = (subsequenceScore(needle, haystack) ?? 0) + 30;
      if (best === null || score > best) best = score;
      continue;
    }
    const fuzzy = subsequenceScore(needle, haystack);
    if (fuzzy !== null && (best === null || fuzzy > best)) best = fuzzy;
  }
  return best;
}

/** Filter + rank commands; MRU ids (most recent first) boost repeats. */
export function filterCommands(
  query: string,
  commands: PaletteCommand[],
  mru: string[],
): ScoredCommand[] {
  const rank = new Map(mru.map((id, index) => [id, mru.length - index]));
  const scored: ScoredCommand[] = [];
  for (const command of commands) {
    const score = scoreCommand(query, command);
    if (score === null) continue;
    scored.push({ command, score: score + (rank.get(command.id) ?? 0) * 8 });
  }
  scored.sort((a, b) => b.score - a.score || a.command.title.localeCompare(b.command.title));
  return scored;
}

// --- In-memory MRU (session only — see header for the persistence decision). */
const MRU_LIMIT = 8;
const mruIds: string[] = [];

export function getMru(): string[] {
  return [...mruIds];
}

export function recordUse(id: string): void {
  const at = mruIds.indexOf(id);
  if (at !== -1) mruIds.splice(at, 1);
  mruIds.unshift(id);
  if (mruIds.length > MRU_LIMIT) mruIds.length = MRU_LIMIT;
}
