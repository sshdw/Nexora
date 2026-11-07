//! Store tests for `useGh` (AC-206).
//!
//! The hook owns the Issues/PRs + Actions machines. The pins cover: the
//! kind/state loaders hitting the right commands with the right filter, a
//! successful load clearing the transient selection, the lazy job log with
//! its degrade-to-unavailable failure shape, the fix-task create (guarded,
//! success/failure flags), and the runners batch with its needs-auth flag.
//! Wrapper shapes: commands snake_case, args camelCase.

import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { GhActionJob, GhActionRun, GhIssue } from "./tauri";
import { useGh } from "./useGh";

const { mockInvoke, failNext, fixtures } = vi.hoisted(() => {
  const fixtures = {
    issues: {
      owner: "shdw",
      repo: "nx",
      state: "open",
      items: [
        {
          number: 7,
          title: "first issue",
          state: "open",
          author: "a",
          labels: [],
          comments: 0,
          created_at: null,
          updated_at: null,
          html_url: null,
          body: "",
          body_truncated: false,
        },
      ],
      authenticated: true,
      rate_limited: false,
      rate_limit: { limit: 60, remaining: 59, reset: 1 },
    },
    runs: {
      owner: "shdw",
      repo: "nx",
      runs: [
        {
          id: 309,
          name: "ci",
          head_branch: "main",
          head_sha: "abc",
          event: "push",
          status: "completed",
          conclusion: "failure",
          created_at: null,
          updated_at: null,
          html_url: null,
          failed_jobs: [
            {
              id: 900,
              run_id: 309,
              name: "test",
              status: "completed",
              conclusion: "failure",
              started_at: null,
              completed_at: null,
              html_url: null,
              failed_steps: ["vitest"],
              log_excerpt: "",
              log_truncated: false,
              log_unavailable: false,
              log_needs_auth: false,
              log_redacted: false,
            },
          ],
          jobs_truncated: false,
        },
      ] as GhActionRun[],
      total_runs: 1,
      authenticated: true,
      rate_limited: false,
      rate_limit: { limit: 60, remaining: 59, reset: 1 },
    },
    log: {
      job_id: 900,
      run_id: 309,
      log_excerpt: "tail",
      log_truncated: false,
      log_unavailable: false,
      log_needs_auth: false,
      log_redacted: false,
    },
    runners: {
      owner: "shdw",
      repo: "nx",
      runners: [],
      runners_total: 0,
      runners_needs_auth: true,
      local: [{ name: "git", found: true }],
      authenticated: true,
      rate_limited: false,
      rate_limit: { limit: 60, remaining: 59, reset: 1 },
    },
  };
  const failNext = { command: "" };
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    void args;
    if (failNext.command === command) {
      failNext.command = "";
      throw { kind: "network", message: "no route to origin" };
    }
    switch (command) {
      case "gh_issues":
        return fixtures.issues;
      case "gh_pulls":
        return { ...fixtures.issues, items: [] };
      case "gh_actions":
        return fixtures.runs;
      case "gh_action_log":
        return fixtures.log;
      case "gh_runners":
        return fixtures.runners;
      case "create_task":
        return 77;
      default:
        throw new Error(`unexpected command ${command}`);
    }
  });
  return { mockInvoke, failNext, fixtures };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
});

describe("useGh", () => {
  it("loads issues for a state filter and exposes the envelope", async () => {
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.loadIssues("closed");
    });
    expect(mockInvoke).toHaveBeenCalledWith("gh_issues", { state: "closed" });
    expect(result.current.issues[0].number).toBe(7);
    expect(result.current.owner).toBe("shdw");
    expect(result.current.repo).toBe("nx");
    expect(result.current.authenticated).toBe(true);
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("loads pulls with the same envelope and clears the selection on success", async () => {
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.loadIssues("open");
    });
    await act(async () => {
      result.current.selectItem(7);
    });
    expect(result.current.selected).toBe(7);
    await act(async () => {
      await result.current.loadPulls("all");
    });
    expect(mockInvoke).toHaveBeenCalledWith("gh_pulls", { state: "all" });
    expect(result.current.pulls).toEqual([]);
    // A successful load clears the list selection (the old detail must not
    // leak across kinds).
    expect(result.current.selected).toBeNull();
  });

  it("routes a failed list load into the list error with the fixed fallback", async () => {
    // A rejection with no message payload (not a CommandError, not an
    // Error) hits the hook's fixed fallback — the panel's honest
    // unavailable wording. Anything carrying a `.message` renders verbatim.
    mockInvoke.mockImplementationOnce(async () => {
      throw 42;
    });
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.loadIssues("open");
    });
    expect(result.current.error).toBe("Issues & PRs are unavailable.");
  });

  it("loads the actions batch and the runners detect together", async () => {
    const { result } = renderHook(() => useGh());
    let ok = false;
    await act(async () => {
      ok = await result.current.loadActions();
      void result.current.loadRunners();
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("gh_actions");
    expect(mockInvoke).toHaveBeenCalledWith("gh_runners");
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.totalRuns).toBe(1);
    expect(result.current.runnersNeedsAuth).toBe(true);
    expect(result.current.localTools).toEqual([{ name: "git", found: true }]);
  });

  it("fetches one job log lazily and degrades a failure to the unavailable row", async () => {
    const { result } = renderHook(() => useGh());
    const job: GhActionJob = fixtures.runs.runs[0].failed_jobs[0];
    await act(async () => {
      await result.current.showJobLog(job);
    });
    expect(mockInvoke).toHaveBeenCalledWith("gh_action_log", { runId: 309, jobId: 900 });
    expect(result.current.jobLogs[900]?.log_excerpt).toBe("tail");

    // Unknown job whose fetch fails: the store inserts the honest
    // unavailable row instead of an error dump.
    failNext.command = "gh_action_log";
    await act(async () => {
      await result.current.showJobLog({ ...job, id: 901 });
    });
    expect(result.current.jobLogs[901]?.log_unavailable).toBe(true);
    expect(result.current.jobLogs[901]?.log_excerpt).toBe("");
    expect(result.current.logsLoading[901]).toBeUndefined();
  });

  it("creates the fix task with the caller's strings and reports the id", async () => {
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.createFixTask(309, {
        title: "Fix CI: ci #309",
        description: "Failing GitHub Actions run shdw/nx — ci #309.",
        steps: ["one", "two"],
      });
    });
    expect(mockInvoke).toHaveBeenCalledWith("create_task", {
      title: "Fix CI: ci #309",
      description: "Failing GitHub Actions run shdw/nx — ci #309.",
      conversationId: null,
      provider: null,
      model: null,
      steps: ["one", "two"],
      maxSteps: 2,
    });
    expect(result.current.fixCreated).toBe(77);
    expect(result.current.fixFailed).toBe(false);
    expect(result.current.fixBusy).toBeNull();
  });

  it("flags a failed fix-task create", async () => {
    failNext.command = "create_task";
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.createFixTask(309, {
        title: "t",
        description: "d",
        steps: ["s"],
      });
    });
    expect(result.current.fixCreated).toBeNull();
    expect(result.current.fixFailed).toBe(true);
  });

  it("resets selections and fix state on the kind switch", async () => {
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.createFixTask(309, {
        title: "t",
        description: "d",
        steps: ["s"],
      });
    });
    await act(async () => {
      result.current.selectRun(309);
      result.current.selectItem(7);
    });
    act(() => {
      result.current.clearSelections();
    });
    expect(result.current.selected).toBeNull();
    expect(result.current.selectedRun).toBeNull();
    expect(result.current.fixCreated).toBeNull();
    expect(result.current.fixFailed).toBe(false);
  });

  it("types stay honest: the first issue matches the GhIssue shape", async () => {
    const { result } = renderHook(() => useGh());
    await act(async () => {
      await result.current.loadIssues("open");
    });
    const issue: GhIssue = result.current.issues[0];
    expect(issue.title).toBe("first issue");
  });
});
