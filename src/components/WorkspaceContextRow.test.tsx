//! Workspace context row (D3 / R5-09) — one read-only line above the card.
//!
//! Data honesty is the whole contract: no root renders NOTHING (not a dash),
//! a failed or detached git read drops the branch and keeps the folder, and
//! the visible text is the basename with the full path on hover. The long
//! name splits into a truncatable head and a fixed tail so the END of a
//! folder name is never the part CSS eats.

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GitInfo } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import WorkspaceContextRow, {
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

function renderRow(root: string | null) {
  return render(
    <LocaleProvider>
      <WorkspaceContextRow root={root} />
    </LocaleProvider>,
  );
}

describe("workspace context row — data honesty", () => {
  it("renders nothing when there is no root", () => {
    const { container } = renderRow(null);
    expect(container.innerHTML).toBe("");
    expect(screen.queryByRole("group", { name: "Workspace context" })).toBeNull();
    // No git read without a workspace to read.
    expect(mockGitInfo).not.toHaveBeenCalled();
  });

  it("shows the folder's basename with the full path on hover", async () => {
    mockGitInfo.mockResolvedValue(git("main"));
    const { container } = renderRow("C:\\src\\nexora");
    const group = screen.getByRole("group", { name: "Workspace context" });
    expect(group).toHaveClass("nex-composer-context");

    const name = container.querySelector(".nex-composer-context-name");
    expect(name).toHaveTextContent("nexora");
    expect(name).toHaveAttribute("title", "C:\\src\\nexora");
    await screen.findByText("main");
  });

  it("adds the branch only after git answers", async () => {
    mockGitInfo.mockResolvedValue(git("feat/composer-round5"));
    const { container } = renderRow("/home/me/app");
    expect(container.querySelector(".nex-composer-context-branch")).toBeNull();

    const branch = await screen.findByText("feat/composer-round5");
    expect(branch).toHaveClass("nex-composer-context-branch");
    expect(branch).toHaveAttribute("title", "feat/composer-round5");
    expect(container.querySelector(".nex-composer-context-sep")).toHaveTextContent("·");
  });

  it("drops the branch when git fails or reports none", async () => {
    mockGitInfo.mockRejectedValue(new Error("not a repository"));
    const failed = renderRow("C:\\src\\notes");
    expect(screen.getByText("notes")).toBeInTheDocument();
    await waitFor(() => expect(mockGitInfo).toHaveBeenCalledTimes(1));
    expect(failed.container.querySelector(".nex-composer-context-branch")).toBeNull();
    failed.unmount();

    mockGitInfo.mockResolvedValue(git(null));
    const detached = renderRow("C:\\src\\notes");
    await screen.findByText("notes");
    expect(detached.container.querySelector(".nex-composer-context-branch")).toBeNull();
    expect(detached.container.querySelector(".nex-composer-context-sep")).toBeNull();
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
    const { container } = renderRow("C:\\src\\nexora");
    expect(container.querySelector(".nex-composer-context-tail")).toBeNull();
    expect(container.querySelector(".nex-composer-context-head")).toHaveTextContent("nexora");
  });

  it("renders both halves so the visible line still reads the whole name", () => {
    const name = "a-very-long-workspace-folder-name";
    mockGitInfo.mockResolvedValue(git(null));
    const { container } = renderRow(`C:\\src\\${name}`);
    const wrapper = container.querySelector(".nex-composer-context-name");
    expect(wrapper?.querySelector(".nex-composer-context-tail")).toHaveTextContent(name.slice(-10));
    expect(wrapper?.textContent).toBe(name);
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
