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

  it("cannot open a hidden destination from a fuzzy query either", () => {
    const commands = buildCommands(depsStub(), "en");
    // Queries a user would plausibly type for a hidden surface. Each must
    // score nothing at all, or at best land on a kept command — never on a
    // command whose run() reaches a hidden panel.
    const queries = [
      "terminal",
      "prompt library",
      "code audit",
      "tech debt",
      "issues",
      "pull requests",
      "docs",
      "release",
      "activity feed",
      "project health",
    ];
    for (const query of queries) {
      const hits = filterCommands(query, commands, []);
      for (const hit of hits) {
        expect(HIDDEN_DESTINATION_IDS).not.toContain(hit.command.id);
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