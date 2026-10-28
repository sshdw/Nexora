//! Sidebar navigation-contract tests (NEX-FE-001).
//!
//! `Sidebar` reads everything from `NavigationContext` (zero props). These
//! pins prove every rail entry invokes its wired opener — and, critically,
//! that openers receive NO arguments: rail `onClick` handlers get the native
//! click event, which previously flowed straight into the parameterized
//! `openSettings(section?)` / `openVcs(request?)` and opened Settings with
//! a blank body. No timing assertions (policy).
//!
//! Nav trim (#219): the rail offers exactly three destinations — Version
//! Control, Tasks, Settings — under the Chat block (brand + New Conversation +
//! search + conversation list). The second suite below pins that every
//! trimmed destination renders no control at all, using the SAME accessible
//! names its entry component used before, so a rename cannot make the
//! absence vacuous.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import { readRepoFile } from "../test/readRepoFile";
import type { Conversation } from "../lib/tauri";
import { NavigationProvider, type Navigation } from "./NavigationContext";
import { SnackbarProvider } from "./SnackbarHost";
import Sidebar from "./Sidebar";

const { mockInvoke } = vi.hoisted(() => {
  const mockInvoke = vi.fn(async () => null);
  return { mockInvoke };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

afterEach(() => {
  cleanup();
});

function navigationStub(): Navigation {
  return {
    conversations: [],
    loading: false,
    error: null,
    creating: false,
    busy: false,
    selectedId: null,
    onSelect: vi.fn(),
    onExport: vi.fn(),
    onNewConversation: vi.fn(),
    onRetry: vi.fn(),
    onOpenSettings: vi.fn(),
    vcsActive: false,
    onOpenVersionControl: vi.fn(),
    tasksActive: false,
    onOpenTasks: vi.fn(),
    onRename: vi.fn(async () => true),
    onArchive: vi.fn(),
    onRestore: vi.fn(),
    onDelete: vi.fn(),
  };
}

function renderSidebar(navigation: Navigation) {
  return render(
    <LocaleProvider>
      <NavigationProvider value={navigation}>
        <Sidebar />
      </NavigationProvider>
    </LocaleProvider>,
  );
}

function conversation(id: number, title: string): Conversation {
  return {
    id,
    title,
    status: "active",
    created_at: 0,
    updated_at: 0,
    workspace_root: null,
  };
}

const RAIL_ENTRIES: ReadonlyArray<readonly [string, (navigation: Navigation) => (...args: never[]) => void]> = [
  ["Version Control", (navigation) => navigation.onOpenVersionControl],
  ["Tasks", (navigation) => navigation.onOpenTasks],
  ["Settings", (navigation) => navigation.onOpenSettings],
];

describe("Sidebar rail entries", () => {
  it.each(RAIL_ENTRIES)("%s invokes its opener with no arguments", (name, pick) => {
    const navigation = navigationStub();
    renderSidebar(navigation);
    fireEvent.click(screen.getByRole("button", { name }));
    const opener = pick(navigation) as ReturnType<typeof vi.fn>;
    expect(opener).toHaveBeenCalledTimes(1);
    // The native click event must not leak into the opener's parameters.
    expect(opener.mock.calls[0]).toEqual([]);
  });
});

// The EN catalog value each trimmed control carried, so these assertions
// fail if a row returns under a new label. Sources: `nav.*Label` on the
// entry components (PromptLibraryEntry → nav.library, ActivityHealthEntry →
// nav.activityLabel, …), `nav.workspaceFolder` / `roots.title` on the two
// workspace rail rows, `nav.importConversation` on the import rail row, and
// `nav.terminal` on the deleted TerminalEntry. The two rows that had no
// component of their own (the import row was inline in Sidebar) and the
// deleted terminal row kept their keys only as long as the row existed, so
// the labels below are pinned here as the contract.
//
// Every entry names the row's ACCESSIBLE NAME (the M3RailItem `label`, which
// sets aria-label), not a label only visible after an expand click: the
// workspace-roots panel is gated behind an `open` state initialised to false,
// so "Add folder…" would be null even if the row came back. "Workspace
// roots" is the rail button's own name and renders unconditionally.
const TRIMMED_CONTROLS: readonly string[] = [
  "Prompt Library",
  "Activity and health",
  "Terminal",
  "Code audit",
  "Technical debt backlog",
  "GitHub issues and pull requests",
  "Product documentation",
  "Release readiness",
  "Workspace folder",
  "Workspace roots",
  "Import conversation",
];

describe("Sidebar nav trim (#219)", () => {
  it.each(TRIMMED_CONTROLS)("renders no control named %s", (name) => {
    renderSidebar(navigationStub());
    expect(screen.queryByRole("button", { name })).toBeNull();
    expect(screen.queryByText(name)).toBeNull();
  });

  it("exposes exactly the three kept rail destinations and the Chat block", () => {
    renderSidebar(navigationStub());
    // #230 Phase 1: the rail group is located through its migrated
    // `data-slot` hook — the `nex-sidebar-footer` class this test used is
    // gone with the rest of the sidebar's legacy classes. The ORDER and the
    // accessible names remain the contract; the selector is only how the
    // group is found.
    const rail = document.querySelector('[data-slot="sidebar-footer"]');
    expect(rail).not.toBeNull();
    const railLabels = Array.from(
      rail!.querySelectorAll<HTMLButtonElement>("button"),
    ).map((button) => button.getAttribute("aria-label"));
    expect(railLabels).toEqual(["Version Control", "Tasks", "Settings"]);
  });

  it("keeps New Conversation and the search box wired", () => {
    const navigation = navigationStub();
    renderSidebar(navigation);
    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
    const creator = navigation.onNewConversation as ReturnType<typeof vi.fn>;
    expect(creator).toHaveBeenCalledTimes(1);
    expect(creator.mock.calls[0]).toEqual([]);
    expect(screen.getByRole("searchbox")).not.toBeNull();
  });
});

/**
 * #230 Phase 1 — the migrated rail (MAP rows 1–3).
 *
 * The visual rebuild is only real if the migrated markup carries NO
 * legacy class and the two defects P1 inherited cannot come back, so
 * those are pinned here rather than left to a screenshot.
 */
describe("Sidebar migrated surface (#230 Phase 1)", () => {
  const source = readRepoFile("src/components/Sidebar.tsx");
  const listSource = readRepoFile("src/components/ConversationList.tsx");
  const itemSource = readRepoFile("src/components/ConversationItem.tsx");
  const searchSource = readRepoFile("src/components/SearchBox.tsx");
  const newSource = readRepoFile("src/components/NewConversationButton.tsx");
  const sheet = readRepoFile("src/styles/components.css");
  /** The migrated sidebar's own CSS section, comments included. */
  function sidebarSection(): string {
    const header = sheet.indexOf("SIDEBAR — the migrated rail");
    return sheet.slice(
      sheet.lastIndexOf("/* =", header),
      sheet.indexOf("TEXT FIELDS (input / textarea / select)"),
    );
  }
  // Comments name the retired classes on purpose (the WHY of each
  // migration), so the check runs against the JSX only.
  const markup = [source, listSource, itemSource, searchSource, newSource]
    .map((code) => code.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/^\s*\/\/.*$/gm, " "))
    .join("\n");

  it("carries no legacy nex-* class in the migrated markup", () => {
    const legacy = [...markup.matchAll(/className="([^"]*)"/g)]
      .flatMap((match) => match[1].split(/\s+/))
      .filter((token) => token.startsWith("nex-"));
    // One documented exception, and it is not a paint class: `nex-logo` is
    // the hook theme.css inverts the brand ASSET through on the light
    // theme, and NexoraMark's prop surface is fixed. It dies with the rest
    // in Phase 6.
    expect(legacy).toEqual(["nex-logo"]);
  });

  it("declares the rail's width once, on the rail itself", () => {
    // 256px = `w-64`: THEME §4's 240–256 band, normalized by THEME §6.
    // The component still owns its own width; the shell grid no longer
    // has a rail column at all (App.tsx renders the fixed edge Dock and
    // `.nex-main` is the only in-flow child), so the grid must not name
    // a second copy of the number — neither the old M3E
    // `--nex-sys-layout-sidebar-width` (280px) nor an `auto` track.
    expect(source).toContain("w-64");
    const shell = readRepoFile("src/styles.css");
    const appRule = /^\.nex-app \{[\s\S]*?\n\}/m.exec(shell)?.[0] ?? "";
    expect(appRule).not.toMatch(/--nex-sys-layout-sidebar-width/);
    expect(appRule).toMatch(/grid-template-columns:\s*1fr\s*;/);
  });

  it("keeps the row's title truncating and its date out of that box", () => {
    // The inherited defect: title and relative date ran together with no
    // gap ("Новый разговорТолько что"). The date must sit in the row's
    // `trailing` slot — beside the truncating label, never inside it — and
    // must be `shrink-0`, so a long RU title ellipsizes AROUND the date
    // and the date is never the part that clips.
    //
    // Every claim is read from the JSX with comments stripped: the prose
    // above each of these files NAMES `shrink-0` and `truncate`, so a
    // source-wide regex would keep passing after the classes were gone.
    const itemJsx = itemSource
      .replace(/\/\*[\s\S]*?\*\//g, " ")
      .replace(/^\s*\/\/.*$/gm, " ");
    expect(itemJsx).toContain("trailing={");
    const railJsx = readRepoFile("src/components/M3RailItem.tsx")
      .replace(/\/\/![\s\S]*?^\/\//m, " ")
      .replace(/^\s*\/\/.*$/gm, " ");
    // The label box: the only truncating element, and it can shrink.
    expect(railJsx).toMatch(/className="min-w-0 flex-1 truncate"/);
    // The trailing box: never compressible, so the date is never the part
    // that clips, and it sits OUTSIDE the label span.
    expect(railJsx).toMatch(/className="flex shrink-0 items-center"/);
  });

  it("keeps the row actions reachable from the keyboard", () => {
    const navigation = navigationStub();
    navigation.conversations = [conversation(1, "Trip plan")];
    render(
      <LocaleProvider>
        <SnackbarProvider>
          <NavigationProvider value={navigation}>
            <Sidebar />
          </NavigationProvider>
        </SnackbarProvider>
      </LocaleProvider>,
    );
    const exportButton = screen.getByRole("button", { name: "Export conversation" });
    exportButton.focus();
    expect(document.activeElement).toBe(exportButton);
    // Nothing may hide the actions from the tab order: the reveal is
    // `opacity` + `pointer-events` only. `visibility`/`display` would make
    // them un-focusable, which is what made the old reveal pointer-only.
    const actions = document.querySelector('[data-slot="row-actions"]');
    expect(actions).not.toBeNull();
    expect(actions!.getAttribute("aria-hidden")).toBeNull();
    expect(actions!.querySelector('[hidden]')).toBeNull();
    const reveal = sheet.slice(sheet.indexOf('[data-slot="row-actions"]'));
    const revealRules = reveal.slice(0, reveal.indexOf("Rail scroll container"));
    expect(revealRules).not.toMatch(/visibility\s*:/);
    expect(revealRules).not.toMatch(/display\s*:\s*none/);
    expect(revealRules).toContain("opacity: 1");
  });

  it("paints the overlay actions group instead of letting the title bleed through", () => {
    // The group is absolutely positioned over the row's trailing edge, so it
    // covers the row instead of reserving width for it — which means the
    // four ghost buttons (no fill at rest) would otherwise have the title
    // running between and under them. It has to carry the tone of the
    // surface it sits on: the rail at rest, the row's hover raise, the
    // selected row's own tone.
    const block = sheet.slice(
      sheet.indexOf('[data-slot="row-actions"] {'),
      sheet.indexOf("Rail scroll container"),
    );
    expect(block).toMatch(/background:\s*var\(--card\)/);
    expect(block).toMatch(
      /\[data-slot="conversation-row"\]:hover \[data-slot="row-actions"\] \{\s*background:\s*var\(--muted\)/,
    );
    // The selected tone comes LAST so a selected row that is also hovered
    // keeps `--accent` instead of dropping back to the hover step.
    expect(block).toMatch(
      /\[data-slot="conversation-row"\]\[data-selected="true"\] \[data-slot="row-actions"\] \{\s*background:\s*var\(--accent\)/,
    );
  });

  it("routes every migrated control through a primitive that owns its focus ring", () => {
    // SYSTEM §3 names SearchBox's raw `<button>`s as by-passers to fix in
    // this PR; MAIN-SCREEN §8 requires the achromatic ring on every control.
    // A hand-written `outline-none` outranks the global `:focus-visible`
    // ring (utilities sit in a later cascade layer — index.css CASCADE),
    // so any one of them would leave a focusable control with no focus
    // signal at all. Focus must come from a primitive that ships one.
    expect(markup).not.toMatch(/<button[\s>]/);
    expect(markup).not.toMatch(/outline-none/);
  });

  it("adds no motion of its own — the global gate is the only source", () => {
    // Every transition in the migrated section must go through a
    // `--motion-*` duration, and its animations must reuse the keyframes
    // the sheet already owns, so the ONE reduced-motion block in
    // motion.css still collapses all of it. No keyframes here.
    const section = sidebarSection();
    expect(section).not.toMatch(/@keyframes/);
    const animations = [...section.matchAll(/animation:\s*([^;]+);/g)].map((m) => m[1]);
    expect(animations.length).toBeGreaterThan(0);
    for (const declaration of animations) {
      expect(declaration).toContain("var(--motion-");
    }
    const transitions = [...section.matchAll(/transition:\s*([^;]+);/g)].map((m) => m[1]);
    expect(transitions.length).toBeGreaterThan(0);
    for (const declaration of transitions) {
      expect(declaration).toContain("var(--motion-");
    }
  });

  it("takes no colour, radius or duration from the legacy token set", () => {
    // SYSTEM §2: the shadcn vars are the only theme on a migrated
    // surface. Structure stays in Tailwind utilities; the CSS section
    // resolves to the z / scrollbar / motion tokens. Comments are stripped
    // first — prose names (`#230`, `--nex-sys-color-primary`) are not paint.
    const section = sidebarSection().replace(/\/\*[\s\S]*?\*\//g, " ");
    expect(section).not.toMatch(/#[0-9a-f]{3,8}\b/i);
    expect(section).not.toMatch(/oklch\(/i);
    expect(section).not.toMatch(/rgb\(/i);
    expect(section).not.toMatch(/--nex-sys-color/);
    expect(section).not.toMatch(/--nex-sys-type/);
    expect(section).not.toMatch(/--nex-sys-shape/);
  });
});
