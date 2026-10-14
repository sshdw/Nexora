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
import { NavigationProvider, type Navigation } from "./NavigationContext";
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
// nav.activityLabel, …), `nav.workspaceFolder` on WorkspaceFolderButton,
// `roots.add` on WorkspaceRootsSwitcher, `nav.importConversation` on the
// import rail row, and `nav.terminal` on the deleted TerminalEntry. The two
// rows that had no component of their own (the import row was inline in
// Sidebar) and the deleted terminal row kept their keys only as long as the
// row existed, so the labels below are pinned here as the contract.
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
  "Add folder…",
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
    const rail = document.querySelector(".nex-sidebar-footer");
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