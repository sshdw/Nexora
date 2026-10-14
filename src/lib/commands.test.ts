//! Command-registry contract for the nav trim (#219).
//!
//! The palette is one of the four surfaces a hidden destination could have
//! kept alive (rail row, palette command, keyboard shortcut, deep link), and
//! it is the only one with a registry that is trivially greppable. These pins
//! assert the trimmed state honestly: the commands that open a hidden panel
//! or the terminal are GONE from the registry (not merely greyed out), the
//! five destinations the shell still offers are all present, and the catalog
//! has a RU translation for every string the registry asks for.
//!
//! The panels themselves are deliberately untouched — a re-added command is
//! the whole re-entry, so this file must not be read as "these features are
//! deleted".

import { describe, expect, it, vi } from "vitest";

import {
  buildCommands,
  filterCommands,
  scoreCommand,
  type PaletteCommand,
  type PaletteDeps,
} from "./commands";

function depsStub(overrides: Partial<PaletteDeps> = {}): PaletteDeps {
  return {
    goConversations: vi.fn(),
    openSettings: vi.fn(),
    openVcs: vi.fn(),
    openTasks: vi.fn(),
    newConversation: vi.fn(),
    openImport: vi.fn(),
    exportActive: vi.fn(),
    focusComposer: vi.fn(),
    tabNext: vi.fn(),
    tabPrev: vi.fn(),
    tabCloseActive: vi.fn(),
    toggleSplit: vi.fn(),
    toggleZen: vi.fn(),
    showShortcuts: vi.fn(),
    toggleLanguage: vi.fn(),
    addWorkspaceRoot: vi.fn(),
    replayOnboarding: vi.fn(),
    jumpToTab: vi.fn(),
    tabCount: () => 0,
    ...overrides,
  };
}

// Every id that used to open a destination the shell no longer offers. The
// palette may not contain any of them, in either locale.
const HIDDEN_DESTINATION_IDS: readonly string[] = [
  "go.library",
  "go.activity",
  "go.health",
  "go.diagnostics",
  "go.privacy",
  "go.audit",
  "go.debt",
  "go.gh",
  "go.docs",
  "go.release",
  "go.terminal",
  "terminal.focus-input",
  "terminal.clear",
];

// The five destinations #219 keeps reachable: Chat (the conversation list +
// its palette entry), Version Control, Tasks, Settings, and Search (which
// lives in the sidebar, not the palette).
const KEPT_GO_IDS: readonly string[] = ["go.conversations", "go.vcs", "go.tasks", "go.settings"];

// Words a user types for a trimmed destination that no kept destination
// answers to. Empirically these return zero results against the trimmed
// registry — verified, not assumed, so a re-registered command that would
// answer one of them fails here.
const UNAMBIGUOUS_HIDDEN_QUERIES: readonly string[] = [
  "terminal",
  "console",
  "shell",
  "command line",
  "prompt library",
  "templates",
  "tech debt",
  "backlog",
  "code audit",
  "static analysis",
  "dead code",
  "activity feed",
  "timeline",
  "project health",
  "diagnostics",
  "telemetry",
  "documentation",
  "migrate",
  "issues",
  "tickets",
  "github",
  "readiness",
  "release",
];

// Words a trimmed panel owned that a KEPT destination also honestly answers,
// so "no results" would be the wrong contract. Verified to still route
// somewhere, which is what keeps the assertion honest rather than vacuous.
const OVERLAPPING_QUERIES: readonly string[] = [
  "docs",
  "status",
  "bugs",
  "checklist",
  "version",
  "update",
  "backup",
  "prs",
  "report",
  "lint",
];

// Topic words no kept command's title or keywords may contain, because the
// fuzzy matcher reads those lists to answer a query.
const FORBIDDEN_TOPIC_WORDS: readonly string[] = [
  "terminal",
  "console",
  "shell",
  "prompt",
  "library",
  "audit",
  "debt",
  "backlog",
  "github",
  "pull request",
  "activity",
  "diagnostics",
  "privacy",
  "documentation",
  "readiness",
];

function ids(locale: "en" | "ru" = "en"): string[] {
  return buildCommands(depsStub(), locale).map((command) => command.id);
}

describe("command palette nav trim (#219)", () => {
  it.each(HIDDEN_DESTINATION_IDS)("registers no %s command", (id) => {
    expect(ids()).not.toContain(id);
    expect(ids("ru")).not.toContain(id);
  });

  it("keeps every command for the five retained destinations", () => {
    const registry = ids();
    for (const id of KEPT_GO_IDS) {
      expect(registry).toContain(id);
    }
    // Version Control keeps its two panel-level deep links; Settings keeps
    // every section deep link.
    expect(registry).toContain("vcs.refresh");
    expect(registry).toContain("vcs.commit-focus");
    for (const section of [
      "appearance",
      "provider",
      "agent",
      "workspace",
      "credentials",
      "data",
      "advanced",
    ]) {
      expect(registry).toContain(`settings.${section}`);
    }
  });

  // The matcher is a fuzzy cross-word subsequence scorer, so "this id is not
  // in the registry" alone does NOT prove the palette is quiet — a kept
  // command whose title/keywords happen to spell the topic could still
  // surface. These tests run the real scorer and pin the user-visible
  // outcome for the words a person would actually type.
  it("surfaces nothing at all when a query names only a trimmed destination", () => {
    const commands = buildCommands(depsStub(), "en");
    for (const query of UNAMBIGUOUS_HIDDEN_QUERIES) {
      const hits = filterCommands(query, commands, []).map((h) => h.command.id);
      expect({ query, hits }).toEqual({ query, hits: [] });
    }
  });

  it("never routes an overlapping query to a trimmed destination", () => {
    // These words belonged to trimmed panels but honestly describe kept ones
    // too, so the contract is not "no results" — it is "whatever surfaces is a
    // kept command". (`docs` reaches help.show-shortcuts because the scorer
    // spans word boundaries inside "keyboard shortcuts"; that is a property of
    // the scorer and predates the trim.)
    const commands = buildCommands(depsStub(), "en");
    let routed = 0;
    for (const query of OVERLAPPING_QUERIES) {
      for (const hit of filterCommands(query, commands, [])) {
        routed += 1;
        expect(HIDDEN_DESTINATION_IDS).not.toContain(hit.command.id);
      }
    }
    // Non-vacuity: these queries really do route somewhere.
    expect(routed).toBeGreaterThan(0);
  });

  it("never advertises a trimmed destination's topic in a kept command", () => {
    // Keyword lists are what a fuzzy match reads, so no kept command may name
    // a topic the user would only expect a hidden panel to answer.
    const commands = buildCommands(depsStub(), "en");
    for (const command of commands) {
      const haystack = [
        command.title.toLowerCase(),
        ...command.keywords.map((k) => k.toLowerCase()),
      ];
      for (const word of FORBIDDEN_TOPIC_WORDS) {
        expect({ id: command.id, word, hit: haystack.some((t) => t.includes(word)) }).toEqual({
          id: command.id,
          word,
          hit: false,
        });
      }
    }
  });

  it("routes the kept commands only to the kept openers", () => {
    const deps = depsStub();
    const commands: PaletteCommand[] = buildCommands(deps, "en");
    const byId = new Map(commands.map((command) => [command.id, command]));
    byId.get("go.vcs")!.run();
    expect(deps.openVcs).toHaveBeenCalledWith();
    byId.get("go.tasks")!.run();
    expect(deps.openTasks).toHaveBeenCalledTimes(1);
    byId.get("go.settings")!.run();
    expect(deps.openSettings).toHaveBeenCalledWith();
    byId.get("go.conversations")!.run();
    expect(deps.goConversations).toHaveBeenCalledTimes(1);
    // Nothing above reached a hidden opener: there is no such field left on
    // PaletteDeps, which is what makes "no command can open them" structural.
    expect(Object.keys(deps)).not.toContain("openLibrary");
    expect(Object.keys(deps)).not.toContain("openTerminal");
  });

  it("gives every registered command a non-empty title in both locales", () => {
    for (const locale of ["en", "ru"] as const) {
      for (const command of buildCommands(depsStub(), locale)) {
        expect(command.title.trim().length).toBeGreaterThan(0);
        expect(command.section.trim().length).toBeGreaterThan(0);
      }
    }
  });

  it("has a RU string for every key the registry reads (no silent EN fallback)", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      buildCommands(depsStub(), "ru");
      const fallbacks = warn.mock.calls.filter((call) =>
        String(call[0]).includes("[i18n] missing RU string"),
      );
      expect(fallbacks).toEqual([]);
    } finally {
      warn.mockRestore();
    }
  });

  it("still scores typo-tolerant queries for the kept commands", () => {
    const commands = buildCommands(depsStub(), "en");
    const byId = new Map(commands.map((command) => [command.id, command]));
    expect(scoreCommand("commmit", byId.get("vcs.commit-focus")!)).not.toBeNull();
    expect(scoreCommand("setings", byId.get("go.settings")!)).not.toBeNull();
    expect(scoreCommand("tas", byId.get("go.tasks")!)).not.toBeNull();
    expect(scoreCommand("verson control", byId.get("go.vcs")!)).not.toBeNull();
  });
});
