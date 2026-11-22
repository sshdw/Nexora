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
      conflicts: [] as Array<{ path: string; status: string }>,
      merge_in_progress: false,
    },
    branches: [
      {
        name: "main",
        is_current: true,
        upstream: "origin/main",
        last_commit_hash: "1234567890abcdef1234567890abcdef12345678",
      },
      {
        name: "feat/wip",
        is_current: false,
        upstream: null,
        last_commit_hash: "abcdef1234567890abcdef1234567890abcdef12",
      },
    ],
    stashes: [
      {
        index: 0,
        message: "WIP on main",
        hash: "1111111111111111111111111111111111111111",
      },
    ],
    failUnmergedDelete: false,
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
        case "git_fetch":
          return { remote: "origin", updated_refs: 2 };
        case "git_pull":
          return {
            status: "conflicts",
            commit_hash: null,
            conflicts: [{ path: "src/conflict.ts", status: "UU" }],
          };
        case "git_branches":
          return JSON.parse(JSON.stringify(state.branches));
        case "git_branch_create":
          return {
            name: (args as { name: string }).name,
            is_current: true,
            upstream: null,
            last_commit_hash: "1234567890abcdef1234567890abcdef12345678",
          };
        case "git_branch_switch":
          return null;
        case "git_branch_delete":
          if (state.failUnmergedDelete && !(args as { force: boolean }).force) {
            throw {
              kind: "invalidInput",
              message:
                "the branch has unmerged commits; confirm force delete to remove it",
            };
          }
          return null;
        case "git_stash_list":
          return JSON.parse(JSON.stringify(state.stashes));
        case "git_stash_push":
          return {
            index: 0,
            message: String((args as { message?: string | null }).message ?? "WIP"),
            hash: "2222222222222222222222222222222222222222",
          };
        case "git_stash_pop":
          return { status: "applied", conflicts: [] };
        case "git_conflict_diff":
          return {
            path: (args as { path: string }).path,
            status: "UU",
            ours: "const x = 'ours';\n",
            theirs: "const x = 'theirs';\n",
            working:
              "<<<<<<< HEAD\nconst x = 'ours';\n=======\nconst x = 'theirs';\n>>>>>>> origin/main\n",
            truncated: false,
            binary: false,
          };
        case "git_resolve_conflict":
          return 0;
        case "git_abort_merge":
          return null;
        case "git_prepare_review_diff":
          return {
            scope: (args as { scope: string }).scope,
            base_branch: null,
            files: ["src/a.ts"],
            files_overflow: 0,
            diff: "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n+API_KEY=[REDACTED]\n",
            truncated: false,
            redacted: true,
            prompt:
              "/review\n\nReview the current working-tree changes (1 file(s)).\n```diff\n+API_KEY=[REDACTED]\n```\n",
          };
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
  state.info.conflicts = [];
  state.info.merge_in_progress = false;
  state.failUnmergedDelete = false;
});

afterEach(() => {
  cleanup();
});

function renderPanel(onSendToAgent?: (prompt: string) => void) {
  return render(
    <LocaleProvider>
      <VersionControlPanel onClose={() => {}} onSendToAgent={onSendToAgent} />
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
    expect(screen.getByText(/On branch main\./)).toBeInTheDocument();
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

describe("VersionControlPanel — conflicts, workflow & review (Stage 10)", () => {
  it("renders Conflicted Files section with badges, preview, resolve, Ask Agent, and guarded Abort", async () => {
    state.info.conflicts = [
      { path: "src/conflict.ts", status: "UU" },
      { path: "src/added.ts", status: "AA" },
    ];
    state.info.merge_in_progress = true;
    const onSendToAgent = vi.fn();
    renderPanel(onSendToAgent);

    await waitFor(() =>
      expect(screen.getByText("Conflicted files: 2")).toBeInTheDocument()
    );
    expect(screen.getByText("Merge / pull in progress")).toBeInTheDocument();
    expect(screen.getByText("UU")).toBeInTheDocument();
    expect(screen.getByText("AA")).toBeInTheDocument();

    // Selecting a conflicted file loads the conflict preview.
    fireEvent.click(screen.getByRole("button", { name: /src\/conflict\.ts/ }));
    await waitFor(() =>
      expect(screen.getAllByText(/const x = 'ours'/).length).toBeGreaterThan(0)
    );
    expect(screen.getAllByText(/const x = 'theirs'/).length).toBeGreaterThan(0);

    // Accept Ours mints git_resolve_conflict and resolves the file.
    fireEvent.click(screen.getAllByRole("button", { name: "Accept Ours" })[0]);
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("git_resolve_conflict", {
        path: "src/conflict.ts",
        strategy: "ours",
        confirmationId: "confirm-id-1",
      })
    );

    // Ask Agent to Resolve stages a prompt via onSendToAgent.
    fireEvent.click(
      screen.getAllByRole("button", { name: "Ask Agent to Resolve" })[0]
    );
    expect(onSendToAgent).toHaveBeenCalledWith(
      expect.stringContaining("src/conflict.ts")
    );

    // Abort merge / pull requires two-press confirmation before calling git_abort_merge.
    fireEvent.click(screen.getByRole("button", { name: "Abort merge / pull" }));
    expect(
      mockInvoke.mock.calls.some(([cmd]) => cmd === "git_abort_merge")
    ).toBe(false);
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm abort merge / pull" })
    );
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("git_abort_merge", {
        confirmationId: "confirm-id-1",
      })
    );
  });

  it("supports fetch, pull conflict notice, branch create/force-delete, stash, and redacted /review", async () => {
    const onSendToAgent = vi.fn();
    renderPanel(onSendToAgent);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Fetch" })).toBeInTheDocument()
    );

    // Fetch
    fireEvent.click(screen.getByRole("button", { name: "Fetch" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("git_fetch", { remote: "origin" })
    );

    // Pull reports conflicts cleanly in a status notice.
    fireEvent.click(screen.getByRole("button", { name: "Pull" }));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Pull completed with 1 conflict(s)"
      )
    );

    // Branch delete with unmerged branch shows explicit force-delete confirmation.
    state.failUnmergedDelete = true;
    fireEvent.click(screen.getByLabelText("Select branch"));
    fireEvent.click(screen.getByRole("option", { name: "feat/wip" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: 'Force delete "feat/wip"' })
      ).toBeInTheDocument()
    );
    fireEvent.click(
      screen.getByRole("button", { name: 'Force delete "feat/wip"' })
    );
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("git_branch_delete", {
        name: "feat/wip",
        force: true,
        confirmationId: "confirm-id-1",
      })
    );

    // Stash pop
    fireEvent.click(screen.getByRole("button", { name: "Pop" }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("git_stash_pop", {
        index: 0,
        confirmationId: "confirm-id-1",
      })
    );

    // AI /review shows secret redaction notice and sends redacted prompt to Agent.
    fireEvent.click(
      screen.getByRole("button", { name: "Review working tree" })
    );
    await waitFor(() =>
      expect(
        screen.getByText(/Potential secrets were automatically redacted/)
      ).toBeInTheDocument()
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Send /review to Agent" })
    );
    expect(onSendToAgent).toHaveBeenCalledWith(
      expect.stringContaining("[REDACTED]")
    );
  });

  it("clears selected conflict preview on refresh when resolved externally without re-fetching conflict diff", async () => {
    state.info.conflicts = [{ path: "src/conflict.ts", status: "UU" }];
    state.info.merge_in_progress = true;
    renderPanel();

    await waitFor(() =>
      expect(screen.getByText("Conflicted files: 1")).toBeInTheDocument()
    );

    // Select the conflicted file and load its preview.
    fireEvent.click(screen.getByRole("button", { name: /src\/conflict\.ts/ }));
    await waitFor(() =>
      expect(screen.getAllByText(/const x = 'ours'/).length).toBeGreaterThan(0)
    );
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === "git_conflict_diff")
    ).toHaveLength(1);

    // Simulate external resolution before manual Refresh.
    state.info.conflicts = [];
    state.info.merge_in_progress = false;
    mockInvoke.mockClear();

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() =>
      expect(screen.queryByText("Conflicted files: 1")).not.toBeInTheDocument()
    );

    // Stale selectedConflictPath must not trigger git_conflict_diff again.
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === "git_conflict_diff")
    ).toHaveLength(0);
    expect(screen.queryByText(/const x = 'ours'/)).not.toBeInTheDocument();
  });
});
