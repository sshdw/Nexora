//! Sidebar navigation-contract tests (NEX-FE-001).
//!
//! `Sidebar` reads everything from `NavigationContext` (zero props). These
//! pins prove every rail entry invokes its wired opener — and, critically,
//! that openers receive NO arguments: rail `onClick` handlers get the native
//! click event, which previously flowed straight into the parameterized
//! `openSettings(section?)` / `openVcs(request?)` and opened Settings with
//! a blank body. No timing assertions (policy).

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import type { WorkspaceStore } from "../lib/useWorkspace";
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

function workspaceStub(): WorkspaceStore {
  return {
    root: null,
    recent: [],
    roots: [],
    loading: false,
    saving: false,
    error: null,
    reload: vi.fn(async () => {}),
    pickFolder: vi.fn(async () => null),
    selectRecent: vi.fn(async () => null),
    addRoot: vi.fn(async () => []),
    removeRoot: vi.fn(async () => []),
    addRootFolder: vi.fn(async () => null),
  };
}

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
    libraryActive: false,
    onOpenPromptLibrary: vi.fn(),
    vcsActive: false,
    onOpenVersionControl: vi.fn(),
    activityActive: false,
    onOpenActivity: vi.fn(),
    terminalActive: false,
    onOpenTerminal: vi.fn(),
    tasksActive: false,
    onOpenTasks: vi.fn(),
    auditActive: false,
    onOpenAudit: vi.fn(),
    debtActive: false,
    onOpenDebt: vi.fn(),
    ghActive: false,
    onOpenGh: vi.fn(),
    docsActive: false,
    onOpenDocs: vi.fn(),
    releaseActive: false,
    onOpenRelease: vi.fn(),
    onSelectPrompt: vi.fn(),
    onImport: vi.fn(),
    onRename: vi.fn(async () => true),
    onArchive: vi.fn(),
    onRestore: vi.fn(),
    onDelete: vi.fn(),
    workspace: workspaceStub(),
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
  ["Settings", (navigation) => navigation.onOpenSettings],
  ["Prompt Library", (navigation) => navigation.onOpenPromptLibrary],
  ["Version Control", (navigation) => navigation.onOpenVersionControl],
  ["Activity and health", (navigation) => navigation.onOpenActivity],
  ["Terminal", (navigation) => navigation.onOpenTerminal],
  ["Tasks", (navigation) => navigation.onOpenTasks],
  ["Code audit", (navigation) => navigation.onOpenAudit],
  ["Technical debt backlog", (navigation) => navigation.onOpenDebt],
  ["GitHub issues and pull requests", (navigation) => navigation.onOpenGh],
  ["Product documentation", (navigation) => navigation.onOpenDocs],
  ["Release readiness", (navigation) => navigation.onOpenRelease],
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
