//! Workspace control row (D3 / R5-06 / R6-03) — the one place a workspace is
//! chosen now that the Dock's Workspace destination and its panel are gone.
//!
//! Two contracts meet here. The TRIGGER is still the honest line above the
//! composer: no root says so, a failed or detached git read drops the branch
//! and keeps the folder, and the visible text is the basename with the full
//! path on hover. The MENU is the old panel's job, moved: current root, the
//! backend's MRU, the registered roots (with unregister), and ONE
//! `Choose folder…` row — the three reference rows that all call the same
//! native picker collapse into one, and `Start from scratch` / the GitHub
//! list are absent rather than disabled promises (R6-10).
//!
//! The unique behaviour the deleted `WorkspacePanel` owned — error text,
//! saving/loading disablement, removing a registered root — is ported here
//! rather than dropped with the panel.

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GitInfo } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import type { WorkspaceStore } from "../lib/useWorkspace";
import { readRepoFile } from "../test/readRepoFile";
import WorkspaceContextRow, {
  recentWithoutRegistered,
  splitForMiddleEllipsis,
  workspaceFolderName,
} from "./WorkspaceContextRow";

const { mockGitInfo } = vi.hoisted(() => ({ mockGitInfo: vi.fn() }));

vi.mock("../lib/tauri", () => ({ gitInfo: mockGitInfo }));

afterEach(cleanup);

beforeEach(() => {
  mockGitInfo.mockReset();
  mockGitInfo.mockResolvedValue(git(null));
});

function git(branch: string | null): GitInfo {
  return { branch, files: [], files_overflow: 0, commits: [] };
}

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

function renderRow(overrides: Partial<WorkspaceStore> = {}) {
  const workspace = store(overrides);
  const view = render(
    <LocaleProvider>
      <WorkspaceContextRow workspace={workspace} />
    </LocaleProvider>,
  );
  return { workspace, ...view };
}

/** Open the menu from its trigger and hand back the trigger element. */
function openMenu(): HTMLElement {
  const trigger = screen.getByRole("button", { name: "Workspace context" });
  fireEvent.click(trigger);
  return trigger;
}

describe("workspace context row — the trigger", () => {
  it("says no folder is open instead of rendering nothing", () => {
    const { container } = renderRow({ root: null, recent: [] });
    const trigger = screen.getByRole("button", { name: "Workspace context" });
    expect(trigger).toHaveTextContent("No folder open");
    expect(trigger).toHaveAttribute("title", "No folder open");
    expect(container.querySelector(".nex-composer-context-name")).toBeNull();
    // No git read without a workspace to read.
    expect(mockGitInfo).not.toHaveBeenCalled();
  });

  it("shows the folder's basename with the full path on hover", async () => {
    mockGitInfo.mockResolvedValue(git("main"));
    const { container } = renderRow();
    const trigger = screen.getByRole("button", { name: "Workspace context" });
    expect(trigger).toHaveAttribute("title", "C:\\src\\app");

    const name = container.querySelector(".nex-composer-context-name");
    expect(name).toHaveTextContent("app");
    expect(name).toHaveAttribute("title", "C:\\src\\app");
    await screen.findByText("main");
  });

  it("adds the branch only after git answers", async () => {
    mockGitInfo.mockResolvedValue(git("feat/shell-round6"));
    const { container } = renderRow({ root: "/home/me/app" });
    expect(container.querySelector(".nex-composer-context-branch")).toBeNull();

    const branch = await screen.findByText("feat/shell-round6");
    expect(branch).toHaveClass("nex-composer-context-branch");
    expect(branch).toHaveAttribute("title", "feat/shell-round6");
    expect(container.querySelector(".nex-composer-context-sep")).toHaveTextContent("·");
  });

  it("drops the branch when git fails or reports none", async () => {
    mockGitInfo.mockRejectedValue(new Error("not a repository"));
    const failed = renderRow({ root: "C:\\src\\notes" });
    expect(screen.getByText("notes")).toBeInTheDocument();
    await waitFor(() => expect(mockGitInfo).toHaveBeenCalledTimes(1));
    expect(failed.container.querySelector(".nex-composer-context-branch")).toBeNull();
    failed.unmount();

    mockGitInfo.mockResolvedValue(git(null));
    const detached = renderRow({ root: "C:\\src\\notes" });
    await screen.findByText("notes");
    expect(detached.container.querySelector(".nex-composer-context-branch")).toBeNull();
    expect(detached.container.querySelector(".nex-composer-context-sep")).toBeNull();
  });

  it("sizes the trigger to its label so the chevron cannot drift to the card edge", () => {
    const sheet = readRepoFile("src/styles/composer.css");
    const start = sheet.indexOf(".nex-composer-context {");
    expect(start, ".nex-composer-context has no rule").toBeGreaterThan(-1);
    const body = sheet.slice(start, sheet.indexOf("}", start));
    // The parent is a stretching flex column; without `align-self` the
    // button filled the 760 px card and the chevron sat at its far edge.
    expect(body).toMatch(/align-self:\s*flex-start/);
    expect(body).toMatch(/max-width:\s*100%/);

    const chevronStart = sheet.indexOf(".nex-composer-context-chevron {");
    expect(chevronStart).toBeGreaterThan(-1);
    const chevron = sheet.slice(chevronStart, sheet.indexOf("}", chevronStart));
    // The flex `gap` owns the few pixels after the branch text; an auto
    // margin would push the chevron away from the label again.
    expect(chevron).not.toMatch(/margin-inline-start:\s*auto/);
    expect(chevron).toMatch(/margin-inline-start:\s*0/);
  });
});

describe("workspace context row — recent/registry dedupe", () => {
  it("does not repeat a registered path in Recent", () => {
    expect(
      recentWithoutRegistered(["D:\\2", "D:\\3", "D:\\4"], ["D:\\2"], null),
    ).toEqual(["D:\\3", "D:\\4"]);
  });

  it("drops the current root and caps the recent list at five", () => {
    expect(
      recentWithoutRegistered(
        ["D:\\1", "D:\\2", "D:\\3", "D:\\4", "D:\\5", "D:\\6", "D:\\7"],
        ["D:\\3"],
        "D:\\1",
      ),
    ).toEqual(["D:\\2", "D:\\4", "D:\\5", "D:\\6", "D:\\7"]);
  });

  it("returns an empty list when every recent path is registered", () => {
    expect(recentWithoutRegistered(["D:\\2"], ["D:\\2"], null)).toEqual([]);
  });
});

describe("workspace context row — long names", () => {
  it("splits a long name into a truncatable head and a fixed tail", () => {
    const name = "a-very-long-workspace-folder-name";
    const [head, tail] = splitForMiddleEllipsis(name);
    expect(head + tail).toBe(name);
    expect(tail).toBe(name.slice(-10));
    expect(head).not.toBe("");
  });

  it("keeps a short name whole, with no empty tail element", () => {
    expect(splitForMiddleEllipsis("nexora")).toEqual(["nexora", ""]);
    const { container } = renderRow({ root: "C:\\src\\nexora" });
    expect(container.querySelector(".nex-composer-context-tail")).toBeNull();
    expect(container.querySelector(".nex-composer-context-head")).toHaveTextContent("nexora");
  });

  it("renders both halves so the visible line still reads the whole name", () => {
    const name = "a-very-long-workspace-folder-name";
    const { container } = renderRow({ root: `C:\\src\\${name}` });
    const wrapper = container.querySelector(".nex-composer-context-name");
    expect(wrapper?.querySelector(".nex-composer-context-tail")).toHaveTextContent(
      name.slice(-10),
    );
    expect(wrapper?.textContent).toBe(name);
  });
});

describe("workspace menu — current root", () => {
  it("marks the active root with a check, its name and its full path", () => {
    renderRow();
    openMenu();

    const current = document.querySelector(".nex-workspace-menu-current");
    expect(current).not.toBeNull();
    expect(current?.querySelector(".nex-workspace-menu-check")).not.toBeNull();
    expect(current).toHaveTextContent("app");
    expect(current).toHaveTextContent("C:\\src\\app");
    expect(current?.querySelector(".nex-workspace-menu-path")).toHaveAttribute(
      "title",
      "C:\\src\\app",
    );
  });

  it("keeps the check off the recents and says no folder is open when there is none", () => {
    const { unmount } = renderRow({ root: null, recent: [] });
    openMenu();
    expect(document.querySelector(".nex-workspace-menu-current")).toBeNull();
    expect(screen.getByText("Open a folder to give the agent a workspace.")).toBeInTheDocument();
    unmount();
  });
});

describe("workspace menu — recents", () => {
  it("lists the other roots, not the active one twice", () => {
    renderRow();
    openMenu();

    const rows = [...document.querySelectorAll(".nex-workspace-menu-row")].map(
      (node) => node.textContent,
    );
    expect(rows).toEqual(["otherC:\\src\\other"]);
    expect(screen.getByText("Recent")).toBeInTheDocument();
  });

  it("opens a recent root through the store and closes the menu", async () => {
    const selectRecent = vi.fn(async (path: string) => path);
    renderRow({ selectRecent });
    openMenu();

    fireEvent.click(screen.getByRole("button", { name: /other/ }));
    await waitFor(() => expect(selectRecent).toHaveBeenCalledWith("C:\\src\\other"));
    await waitFor(() => expect(document.querySelector(".nex-workspace-menu-row")).toBeNull());
  });

  it("hides the section when the active root is the only one", () => {
    renderRow({ recent: ["C:\\src\\app"] });
    openMenu();
    expect(screen.queryByText("Recent")).toBeNull();
  });

  it("shows at most the backend's five most recent roots", () => {
    renderRow({
      recent: [
        "C:\\src\\app",
        "C:\\src\\r1",
        "C:\\src\\r2",
        "C:\\src\\r3",
        "C:\\src\\r4",
        "C:\\src\\r5",
        "C:\\src\\r6",
      ],
    });
    openMenu();
    const rows = document.querySelectorAll(".nex-workspace-menu-row");
    expect(rows).toHaveLength(5);
  });
});

describe("workspace menu — registered roots", () => {
  it("lists the registry separately from the MRU", () => {
    renderRow({ roots: ["C:\\src\\app", "C:\\src\\registered"] });
    openMenu();

    const section = screen.getByRole("region", { name: "Workspaces" });
    expect(section).toHaveTextContent("registered");
    expect(section).toHaveTextContent("C:\\src\\registered");
    // The active root is not offered as a second, identical target.
    expect(section).not.toHaveTextContent("C:\\src\\app");
  });

  it("unregisters a root without touching the active one", async () => {
    const { workspace } = renderRow({
      roots: ["C:\\src\\app", "C:\\src\\registered"],
    });
    openMenu();

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(workspace.removeRoot).toHaveBeenCalledWith("C:\\src\\registered");
    expect(workspace.selectRecent).not.toHaveBeenCalled();
  });

  it("hides the registry when the active root is the only registered one", () => {
    renderRow({ roots: ["C:\\src\\app"] });
    openMenu();
    expect(screen.queryByRole("region", { name: "Workspaces" })).toBeNull();
  });
});

describe("workspace menu — choose folder", () => {
  it("offers exactly one folder action and dispatches the native picker", async () => {
    const pickFolder = vi.fn(async () => "C:\\src\\picked");
    const { workspace } = renderRow({ pickFolder });
    openMenu();

    fireEvent.click(screen.getByRole("button", { name: /Choose folder/ }));
    await waitFor(() => expect(pickFolder).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(document.querySelector(".nex-workspace-menu-choose")).toBeNull());
    expect(workspace.pickFolder).toBe(pickFolder);
  });

  it("stays open when the picker is dismissed", async () => {
    renderRow();
    openMenu();

    fireEvent.click(screen.getByRole("button", { name: /Choose folder/ }));
    await waitFor(() =>
      expect(document.querySelector(".nex-workspace-menu-choose")).not.toBeNull(),
    );
  });

  it("promises nothing the product cannot do", () => {
    renderRow();
    openMenu();
    for (const promise of [
      "Start from scratch",
      "On This PC",
      "Use Existing",
      "New Folder",
      "Repos",
      "GitHub",
    ]) {
      expect(screen.queryByText(promise)).toBeNull();
    }
  });
});

describe("workspace menu — store state", () => {
  it("surfaces the backend's error in words", () => {
    renderRow({
      error: { kind: "invalid_root", message: "That folder is inside the current one" },
    });
    openMenu();
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("That folder is inside the current one");
  });

  it("disables every action while the store is saving", () => {
    renderRow({ saving: true, roots: ["C:\\src\\app", "C:\\src\\registered"] });
    openMenu();
    expect(screen.getByRole("button", { name: /Choose folder/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /other/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Remove" })).toBeDisabled();
  });

  it("disables every action while the initial load is in flight", () => {
    renderRow({ loading: true });
    openMenu();
    expect(screen.getByRole("button", { name: /Choose folder/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: /other/ })).toBeDisabled();
    expect(screen.getByText("Loading…")).toBeInTheDocument();
  });
});

describe("workspaceFolderName", () => {
  it("takes the last segment for either separator style", () => {
    expect(workspaceFolderName("C:\\src\\app")).toBe("app");
    expect(workspaceFolderName("/home/me/app")).toBe("app");
    expect(workspaceFolderName("C:\\src\\app\\")).toBe("app");
    expect(workspaceFolderName("/home/me/app/")).toBe("app");
  });

  it("keeps a root with no separator whole", () => {
    expect(workspaceFolderName("app")).toBe("app");
  });

  it("returns a separator-only root as written rather than an empty label", () => {
    expect(workspaceFolderName("/")).toBe("/");
    expect(workspaceFolderName("C:\\")).toBe("C:");
  });
});
