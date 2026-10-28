//! Workspace panel (D-06 / O-07) — the visible folder entry point.
//!
//! Since #219 the folder controls left the rail, so this panel is the only
//! pointer route to the root besides the command palette and onboarding. It
//! reads `useWorkspace` and dispatches its actions; it validates nothing
//! itself, so the pins here are the display contract (name vs full path), the
//! empty state, the recents list, and the store's own disabled/error flags.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceStore } from "../lib/useWorkspace";
import { LocaleProvider } from "../lib/useLocale";
import WorkspacePanel from "./WorkspacePanel";

afterEach(cleanup);

function store(overrides: Partial<WorkspaceStore> = {}): WorkspaceStore {
  return {
    root: "C:\\src\\app",
    recent: ["C:\\src\\app", "C:\\src\\other"],
    roots: [],
    loading: false,
    saving: false,
    error: null,
    reload: vi.fn(async () => {}),
    pickFolder: vi.fn(async () => null),
    selectRecent: vi.fn(async () => null),
    addRootFolder: vi.fn(async () => null),
    addRoot: vi.fn(async () => null),
    removeRoot: vi.fn(async () => null),
    ...overrides,
  };
}

function renderPanel(overrides: Partial<WorkspaceStore> = {}) {
  const workspace = store(overrides);
  const view = render(
    <LocaleProvider>
      <WorkspacePanel workspace={workspace} />
    </LocaleProvider>,
  );
  return { ...workspace, ...view };
}

describe("workspace panel — current root", () => {
  it("shows the folder's name and keeps the full path on hover", () => {
    const { container } = renderPanel();
    const name = container.querySelector(".nex-workspace-name");
    expect(name).toHaveTextContent("app");
    expect(name).toHaveAttribute("title", "C:\\src\\app");

    const path = container.querySelector(".nex-workspace-path");
    expect(path).toHaveTextContent("C:\\src\\app");
    expect(path).toHaveAttribute("title", "C:\\src\\app");
  });

  it("splits a POSIX root the same way", () => {
    const { container } = renderPanel({ root: "/home/me/nexora" });
    expect(container.querySelector(".nex-workspace-name")).toHaveTextContent("nexora");
  });

  it("offers Change folder when a root is open", () => {
    renderPanel();
    expect(screen.getByRole("button", { name: /Change folder/ })).toBeInTheDocument();
  });
});

describe("workspace panel — no root", () => {
  it("says no folder is open and offers to open one", () => {
    const { container } = renderPanel({ root: null, recent: [] });
    expect(screen.getByText("No folder open")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Open folder/ })).toBeInTheDocument();
    expect(container.querySelector(".nex-workspace-current")).toHaveClass("is-empty");
    expect(container.querySelector(".nex-workspace-path")).toBeNull();
  });
});

describe("workspace panel — recents", () => {
  it("lists the other roots, not the active one twice", () => {
    const { container } = renderPanel();
    const titles = [...container.querySelectorAll(".nex-sessions-row-title")].map(
      (node) => node.textContent,
    );
    expect(titles).toEqual(["other"]);
    expect(screen.getByText("Recent")).toBeInTheDocument();
  });

  it("opens a recent root through the store", () => {
    const { selectRecent } = renderPanel();
    fireEvent.click(screen.getByRole("button", { name: /other/ }));
    expect(selectRecent).toHaveBeenCalledWith("C:\\src\\other");
  });

  it("hides the section when the active root is the only one", () => {
    renderPanel({ recent: ["C:\\src\\app"] });
    expect(screen.queryByText("Recent")).toBeNull();
  });
});

describe("workspace panel — store state", () => {
  it("surfaces the backend's error in words", () => {
    renderPanel({ error: { kind: "invalid_root", message: "That folder is inside the current one" } });
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("That folder is inside the current one");
    expect(alert).toHaveClass("is-error");
  });

  it("disables the actions while the store is saving or loading", () => {
    const saving = renderPanel({ saving: true });
    expect(screen.getByRole("button", { name: /Change folder/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /other/ })).toBeDisabled();
    expect(saving.selectRecent).not.toHaveBeenCalled();
  });

  it("disables the picker while the initial load is in flight", () => {
    renderPanel({ loading: true });
    expect(screen.getByRole("button", { name: /Change folder/ })).toBeDisabled();
  });

  it("dispatches pickFolder from the one folder action", () => {
    const { pickFolder } = renderPanel();
    fireEvent.click(screen.getByRole("button", { name: /Change folder/ }));
    expect(pickFolder).toHaveBeenCalledTimes(1);
  });
});
