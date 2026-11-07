//! Store tests for `useVcs` (AC-206).
//!
//! The hook owns the VCS panel's whole git machine. The pins cover the
//! contract the panel renders against: mount load, lazy diffs, the guarded
//! writes refreshing the aggregate afterwards, the two-press push arming,
//! the commit-message lifecycle (generate fills, commit clears), and the
//! provider-settings gates on the two AI assists. The writes' mint calls
//! (`request_confirmation` per SEC-03) are visible in the transcript.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useVcs } from "./useVcs";

const { mockInvoke, settings, failNext } = vi.hoisted(() => {
  const settings = new Map<string, string | null>();
  const failNext = { command: "" };
  const fixtures = {
    info: {
      branch: "main",
      files: [{ path: "src/lib/a.ts", status: "modified" }],
      files_overflow: 0,
      commits: [
        {
          hash: "abc1234567890",
          message: "fix: thing",
          author: "d",
          time: 1000,
          files_changed: 1,
          insertions: 2,
          deletions: 1,
          risk_signals: [],
        },
      ],
    },
    diff: { path: "src/lib/a.ts", diff: "@@ -1 +1 @@", truncated: false, binary: false },
    commitDiff: {
      hash: "abc1234567890",
      diff: "@@ -1 +1 @@",
      truncated: false,
      binary: false,
      files: ["src/lib/a.ts"],
      files_overflow: 0,
    },
    generated: { message: "fix: generated", truncated_input: true },
    explained: { explanation: "it broke", truncated_input: false },
  };
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    if (failNext.command === command) {
      failNext.command = "";
      throw { kind: "io", message: "git refused" };
    }
    switch (command) {
      case "git_info":
        return JSON.parse(JSON.stringify(fixtures.info));
      case "git_file_diff":
        return fixtures.diff;
      case "git_commit_diff":
        return fixtures.commitDiff;
      case "request_confirmation":
        return "confirm-id-1";
      case "git_stage":
      case "git_unstage":
        return 1;
      case "git_commit":
        return "def000";
      case "git_push":
        return null;
      case "git_generate_commit_message":
        return fixtures.generated;
      case "git_explain_commit":
        return fixtures.explained;
      case "get_setting":
        return settings.has((args as { key: string }).key)
          ? settings.get((args as { key: string }).key)
          : null;
      default:
        throw new Error(`unexpected command ${command}`);
    }
  });
  return { mockInvoke, settings, failNext };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  settings.clear();
  failNext.command = "";
});

describe("useVcs", () => {
  it("loads the aggregate on mount with the 20-commit page", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockInvoke).toHaveBeenCalledWith("git_info", { limit: 20 });
    expect(result.current.info?.branch).toBe("main");
    expect(result.current.error).toBeNull();
  });

  it("loads a file diff lazily into its slot", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.loadDiff("src/lib/a.ts");
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_file_diff", { path: "src/lib/a.ts" });
    expect(result.current.selectedPath).toBe("src/lib/a.ts");
    expect(result.current.diff?.truncated).toBe(false);
    expect(result.current.diffLoading).toBe(false);
  });

  it("stages and unstages through the mint gate, then refreshes", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.toggleStage("src/lib/a.ts", false);
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "version_control",
      operation: "git_stage",
      cwd: null,
      targetId: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_stage", {
      paths: ["src/lib/a.ts"],
      confirmationId: "confirm-id-1",
    });
    // The write refreshes the aggregate afterwards (same page).
    expect(mockInvoke.mock.calls.filter(([command]) => command === "git_info")).toHaveLength(2);

    await act(async () => {
      await result.current.toggleStage("src/lib/a.ts", true);
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "version_control",
      operation: "git_unstage",
      cwd: null,
      targetId: null,
    });
    expect(result.current.acting).toBeNull();
    expect(result.current.actionError).toBeNull();
  });

  it("commits with a minted id, clears the message and refreshes", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.setCommitMessage("fix: mine");
    });
    await act(async () => {
      await result.current.commit();
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "version_control",
      operation: "git_commit",
      cwd: null,
      targetId: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_commit", {
      message: "fix: mine",
      confirmationId: "confirm-id-1",
    });
    expect(result.current.commitMessage).toBe("");
    expect(result.current.acting).toBeNull();
  });

  it("arms the push on the first press and sends on the second", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.push();
    });
    expect(result.current.pushArmed).toBe(true);
    expect(mockInvoke).not.toHaveBeenCalledWith("git_push", expect.anything());

    await act(async () => {
      await result.current.push();
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "version_control",
      operation: "git_push",
      cwd: null,
      targetId: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_push", {
      remote: "origin",
      confirmationId: "confirm-id-1",
    });
    // A sent push is disarmed again.
    expect(result.current.pushArmed).toBe(false);
  });

  it("routes a failed write into the action error", async () => {
    failNext.command = "git_push";
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    // Separate acts: each press must see the arming state the previous one
    // committed (the second press is the one that sends and fails).
    await act(async () => {
      await result.current.push();
    });
    await act(async () => {
      await result.current.push();
    });
    expect(result.current.actionError).toBe("git refused");
    expect(result.current.acting).toBeNull();
  });

  it("refuses to generate without persisted provider settings", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.generate();
    });
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "git_generate_commit_message",
      expect.anything(),
    );
    expect(result.current.actionError).not.toBeNull();
    expect(result.current.generating).toBe(false);
  });

  it("fills the commit message from the AI generation", async () => {
    settings.set("provider.selected", "openai");
    settings.set("provider.model", "gpt");
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.generate();
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_generate_commit_message", {
      provider: "openai",
      model: "gpt",
    });
    expect(result.current.commitMessage).toBe("fix: generated");
    expect(result.current.generatedTruncated).toBe(true);
  });

  it("explains the selected commit through the same settings gate", async () => {
    settings.set("provider.selected", "openai");
    settings.set("provider.model", "gpt");
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.explainCommit("abc1234567890");
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_explain_commit", {
      commitHash: "abc1234567890",
      provider: "openai",
      model: "gpt",
    });
    expect(result.current.explain.explanation).toBe("it broke");
    expect(result.current.explain.loading).toBe(false);
  });

  it("loads the commit diff and its files", async () => {
    const { result } = renderHook(() => useVcs());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.loadCommitDiff("abc1234567890");
    });
    expect(mockInvoke).toHaveBeenCalledWith("git_commit_diff", { hash: "abc1234567890" });
    expect(result.current.commitDiff?.files).toEqual(["src/lib/a.ts"]);
    expect(result.current.selectedCommit).toBe("abc1234567890");
  });
});
