//! Panel pins for `VersionControlPanel` (AC-211 minimum set, review 03 §6.3
//! item 1 — "stage/commit/push are irreversible; the confirm-gate and error
//! paths are the highest-risk untested UI").
//!
//! The panel is presentational over `useVcs`, so every pin goes through the
//! real store and the real wrapper layer (invoke mocked at
//! `@tauri-apps/api/core`): staging mints the backend's `version_control`
//! confirmation before `git_stage` fires, commit stays disabled until both a
//! message and staged changes exist, a failed commit surfaces in the alert
//! and keeps the typed message, push is the documented two-press arm/confirm
//! gate whose first press sends nothing, a bare `git_info` failure degrades
//! to the alert, and message generation without a persisted provider/model
//! refuses before any AI call. Display rules: hashes render shortened.

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import VersionControlPanel from "./VersionControlPanel";

const { mockInvoke, failNext, state } = vi.hoisted(() => {
  const failNext = { command: "" };
  const state = {
    info: {
      branch: "main",
      files: [
        { path: "src/a.ts", status: "modified" },
        { path: "src/b.ts", status: "staged" },
      ],
      files_overflow: 0,
      commits: [
        {
          hash: "1234567890abcdef1234567890abcdef12345678",
          message: "feat(10.4): add runner",
          author: "sshdw",
          time: 1700000000,
          files_changed: 3,
          insertions: 120,
          deletions: 4,
          risk_signals: ["large-diff"],
        },
      ],
    },
  };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "version_control", message: "backend refused" };
      }
      switch (command) {
        case "git_info":
          return JSON.parse(JSON.stringify(state.info));
        case "git_file_diff":
          return {
            path: (args as { path: string }).path,
            diff: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-old\n+new",
            truncated: false,
            binary: false,
          };
        case "git_commit_diff":
          return {
            hash: (args as { hash: string }).hash,
            diff: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-old\n+new",
            truncated: false,
            binary: false,
            files: ["src/a.ts"],
            files_overflow: 0,
          };
        case "request_confirmation":
          return "confirm-id-1";
        case "git_stage":
        case "git_unstage":
          return 1;
        case "git_commit":
          return "fedcba9876543210fedcba9876543210fedcba98";
        case "git_push":
          return null;
        case "get_setting":
          return null;
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, state };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
  state.info.files = [
    { path: "src/a.ts", status: "modified" },
    { path: "src/b.ts", status: "staged" },
  ];
});

afterEach(() => {
  cleanup();
});

function renderPanel() {
  return render(
    <LocaleProvider>
      <VersionControlPanel onClose={() => {}} />
    </LocaleProvider>
  );
}

async function renderLoadedPanel() {
  renderPanel();
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: /src\/b\.ts/ })
    ).toBeInTheDocument()
  );
}

describe("VersionControlPanel — working tree", () => {
  it("renders the branch, the changed files, and the per-status action", async () => {
    await renderLoadedPanel();
    expect(screen.getByText("Version Control")).toBeInTheDocument();
    expect(screen.getByText(/main/)).toBeInTheDocument();
    // One row per changed file; a staged file offers Unstage, a modified
    // one offers Stage.
    expect(screen.getByRole("button", { name: "Stage" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Unstage" })).toBeInTheDocument();
  });

  it("mints the version_control confirmation before staging a file", async () => {
    await renderLoadedPanel();

    fireEvent.click(screen.getByRole("button", { name: "Stage" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
        scope: "version_control",
        operation: "git_stage",
        cwd: null,
        targetId: null,
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith("git_stage", {
      paths: ["src/a.ts"],
      confirmationId: "confirm-id-1",
    });
    // Every write refreshes the aggregate afterwards.
    await waitFor(() =>
      expect(
        mockInvoke.mock.calls.filter(([command]) => command === "git_info")
      ).toHaveLength(2)
    );
  });

  it("keeps the commit disabled until a message and staged changes exist", async () => {
    state.info.files = [{ path: "src/a.ts", status: "modified" }];
    renderPanel();
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /src\/a\.ts/ })
      ).toBeInTheDocument()
    );

    const commit = screen.getByRole("button", { name: "Commit" });
    // No staged changes: disabled even before any message is typed.
    expect(commit).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Commit message"), {
      target: { value: "feat: work" },
    });
    expect(commit).toBeDisabled();

    // With a staged file the same message enables the write.
    state.info.files = [{ path: "src/b.ts", status: "staged" }];
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Unstage" })
      ).toBeInTheDocument()
    );
    fireEvent.change(screen.getByLabelText("Commit message"), {
      target: { value: "feat: work" },
    });
    expect(screen.getByRole("button", { name: "Commit" })).toBeEnabled();
  });

  it("commits through the minted confirmation and clears the composer", async () => {
    await renderLoadedPanel();
    fireEvent.change(screen.getByLabelText("Commit message"), {
      target: { value: "feat: work" },
    });

    fireEvent.click(screen.getByRole("button", { name: "Commit" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
        scope: "version_control",
        operation: "git_commit",
        cwd: null,
        targetId: null,
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith("git_commit", {
      message: "feat: work",
      confirmationId: "confirm-id-1",
    });
    await waitFor(() =>
      expect(screen.getByLabelText("Commit message")).toHaveValue("")
    );
  });

  it("surfaces a failed commit and keeps the typed message", async () => {
    await renderLoadedPanel();
    fireEvent.change(screen.getByLabelText("Commit message"), {
      target: { value: "feat: work" },
    });
    failNext.command = "git_commit";

    fireEvent.click(screen.getByRole("button", { name: "Commit" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("backend refused")
    );
    expect(screen.getByLabelText("Commit message")).toHaveValue("feat: work");
  });
});

describe("VersionControlPanel — push gate", () => {
  it("arms on the first press (sending nothing) and pushes on the second", async () => {
    await renderLoadedPanel();
    const pushButton = () =>
      screen.getByRole("button", { name: /push to origin/i });

    fireEvent.click(pushButton());
    expect(
      screen.getByRole("button", { name: "Confirm push to origin" })
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Push sends the current branch to origin — click again to confirm."
      )
    ).toBeInTheDocument();
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "git_push")
    ).toBe(false);

    fireEvent.click(pushButton());
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
        scope: "version_control",
        operation: "git_push",
        cwd: null,
        targetId: null,
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith("git_push", {
      remote: "origin",
      confirmationId: "confirm-id-1",
    });
  });
});

describe("VersionControlPanel — error paths", () => {
  it("degrades a failed git_info load to the alert", async () => {
    failNext.command = "git_info";
    renderPanel();
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("backend refused")
    );
  });

  it("refuses message generation before any AI call when no provider is set", async () => {
    await renderLoadedPanel();
    fireEvent.click(screen.getByRole("button", { name: "Generate message" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Select a provider and model first (Settings), then generate."
      )
    );
    expect(
      mockInvoke.mock.calls.some(
        ([command]) => command === "git_generate_commit_message"
      )
    ).toBe(false);
  });
});

describe("VersionControlPanel — timeline", () => {
  it("renders commits with shortened hashes and risk labels", async () => {
    await renderLoadedPanel();
    fireEvent.click(screen.getByRole("tab", { name: "Timeline" }));

    // Display rule: hashes render shortened to 7 chars.
    expect(screen.getByText("1234567")).toBeInTheDocument();
    expect(screen.getByText("feat(10.4): add runner")).toBeInTheDocument();
    expect(screen.getByText("Large diff")).toBeInTheDocument();
  });
});
