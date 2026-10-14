//! The #219 split: hidden ≠ deleted.
//!
//! Eight panels lost every UI access path (no rail row, no palette command,
//! no shortcut, no search deep link) but their code, hooks, router state,
//! i18n keys and backend commands are all still here, ready to be re-exposed
//! by adding one row back. This file is the gate for the second half of that
//! promise: it fails if a "hidden" panel, its entry component or its data
//! hook disappears, and it fails if a UI access path sneaks back in.
//!
//! The terminal is the deliberate exception — its UI is deleted outright, so
//! it is asserted absent rather than present.

import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { buildCommands, type PaletteDeps } from "./commands";
import { COMBO_TASKS_OPEN, SHORTCUTS } from "./shortcuts";

const srcDir = resolve(__dirname);
const componentsDir = resolve(srcDir, "..", "components");

/** Read a file from src/lib or src/components by bare name. */
function source(relative: string): string {
  const inComponents = readdirSync(componentsDir).includes(relative);
  const path = inComponents
    ? resolve(componentsDir, relative)
    : resolve(srcDir, relative);
  return readFileSync(path, "utf8");
}

const HIDDEN_PANELS = [
  "PromptLibraryView",
  "ActivityHealthPanel",
  "AuditPanel",
  "DebtPanel",
  "IssuesPanel",
  "DocsPanel",
  "ReleasePanel",
] as const;

const HIDDEN_ENTRIES = [
  "PromptLibraryEntry",
  "ActivityHealthEntry",
  "AuditEntry",
  "DebtEntry",
  "IssuesEntry",
  "DocsEntry",
  "ReleaseEntry",
  "WorkspaceFolderButton",
  "WorkspaceRootsSwitcher",
] as const;

const HIDDEN_HOOKS = ["usePrompts", "useWorkspace", "useTasks"] as const;

describe("hidden panels stay on disk (#219)", () => {
  it.each(HIDDEN_PANELS)("%s.tsx still exists", (name) => {
    expect(readdirSync(componentsDir)).toContain(`${name}.tsx`);
  });

  it.each(HIDDEN_ENTRIES)("%s.tsx still exists", (name) => {
    expect(readdirSync(componentsDir)).toContain(`${name}.tsx`);
  });

  it.each(HIDDEN_HOOKS)("%s still exists", (name) => {
    expect(source(`${name}.ts`)).toContain(`export function ${name}`);
  });

  it("keeps every hidden panel's router flag and opener", () => {
    const router = source("useOverlayRouter.ts");
    for (const flag of [
      "libraryOpen",
      "activityOpen",
      "auditOpen",
      "debtOpen",
      "ghOpen",
      "docsOpen",
      "releaseOpen",
    ]) {
      expect(router).toContain(`${flag}: boolean;`);
      expect(router).toContain(`const [${flag}, set${flag[0].toUpperCase()}${flag.slice(1)}]`);
    }
    for (const opener of [
      "openLibrary",
      "openActivity",
      "openAudit",
      "openDebt",
      "openGh",
      "openDocs",
      "openRelease",
      "selectPrompt",
    ]) {
      expect(router).toContain(`${opener}:`);
    }
    // The prompt deep-link state the search box used to stage survives too.
    expect(router).toContain("promptToEditId");
  });

  it("keeps the terminal backend wrappers while the terminal UI is gone", () => {
    expect(readdirSync(componentsDir)).not.toContain("TerminalPanel.tsx");
    expect(readdirSync(componentsDir)).not.toContain("TerminalEntry.tsx");
    const tauri = source("tauri.ts");
    expect(tauri).toContain('invoke<TerminalRunResult>("terminal_run"');
    expect(tauri).toContain('invoke<boolean>("terminal_kill")');
    expect(tauri).toContain('invoke<ErrorExplanation>("terminal_explain"');
  });

  it("registers no sidebar rail row for a hidden destination", () => {
    const sidebar = source("Sidebar.tsx");
    // Imports and JSX tags — not prose: the Sidebar's header comment names the
    // trimmed surfaces on purpose, so a bare substring check would be vacuous.
    for (const component of [...HIDDEN_ENTRIES, "TerminalEntry"]) {
      expect(sidebar).not.toContain(`from "./${component}"`);
      expect(sidebar).not.toContain(`<${component}`);
    }
    // The kept rail rows, in order, with Settings last.
    const rail = sidebar.slice(sidebar.indexOf('className="nex-sidebar-footer"'));
    const order = ["VersionControlEntry", "TaskEntry", "SettingsEntry"].map((name) =>
      rail.indexOf(`<${name}`),
    );
    expect(order.every((index) => index > -1)).toBe(true);
    expect(order).toEqual([...order].sort((a, b) => a - b));
    expect(rail).not.toContain("ImportIcon");
  });

  it("exposes no PaletteDeps field that could open a hidden panel", () => {
    const commands = source("commands.ts");
    const depsBlock = commands.slice(
      commands.indexOf("export interface PaletteDeps"),
      commands.indexOf("export function buildCommands"),
    );
    for (const opener of [
      "openLibrary",
      "openTerminal",
      "openActivity",
      "openAudit",
      "openDebt",
      "openGh",
      "openDocs",
      "openRelease",
    ]) {
      expect(depsBlock).not.toContain(opener);
    }
    expect(depsBlock).toContain("openVcs");
    expect(depsBlock).toContain("openTasks");
    expect(depsBlock).toContain("openSettings");
  });

  it("registers no shortcut that opens a hidden destination", () => {
    const goEntries = SHORTCUTS.filter((entry) => entry.group === "Go");
    expect(goEntries.map((entry) => entry.id)).toEqual(["go.tasks"]);
    expect(COMBO_TASKS_OPEN.key).toBe("t");
    for (const id of ["go.activity", "go.health", "go.audit", "go.gh", "go.terminal"]) {
      expect(SHORTCUTS.some((entry) => entry.id === id)).toBe(false);
    }
  });

  it("keeps the hidden panels' i18n keys in both locales", () => {
    const strings = source("strings.ts");
    for (const key of [
      "palette.cmd.go_library",
      "palette.cmd.go_activity",
      "palette.cmd.go_audit",
      "palette.cmd.go_debt",
      "palette.cmd.go_gh",
      "palette.cmd.go_docs",
      "palette.cmd.go_release",
      "nav.library",
      "nav.activityLabel",
      "nav.auditLabel",
      "nav.debtLabel",
      "nav.ghLabel",
      "nav.docsLabel",
      "nav.releaseLabel",
      "nav.workspaceFolder",
      "docs.featTerminal",
    ]) {
      expect(strings).toContain(`"${key}":`);
      // Each key appears in the EN table and the RU table.
      expect(strings.split(`"${key}":`).length - 1).toBe(2);
    }
  });

  it("keeps the palette reachable for every kept destination", () => {
    const deps = {
      goConversations: () => {},
      openSettings: () => {},
      openVcs: () => {},
      openTasks: () => {},
      newConversation: () => {},
      openImport: () => {},
      exportActive: () => {},
      focusComposer: () => {},
      tabNext: () => {},
      tabPrev: () => {},
      tabCloseActive: () => {},
      toggleSplit: () => {},
      toggleZen: () => {},
      showShortcuts: () => {},
      toggleLanguage: () => {},
      addWorkspaceRoot: () => {},
      replayOnboarding: () => {},
      jumpToTab: () => {},
      tabCount: () => 0,
    } satisfies PaletteDeps;
    const ids = buildCommands(deps, "en").map((command) => command.id);
    expect(ids).toContain("go.conversations");
    expect(ids).toContain("go.vcs");
    expect(ids).toContain("go.tasks");
    expect(ids).toContain("go.settings");
  });
});