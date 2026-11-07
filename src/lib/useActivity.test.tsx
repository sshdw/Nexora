//! Store tests for `useActivity` (AC-206).
//!
//! The hook owns the Activity & Health machines and the dock's one-shot task
//! queue. The pins cover the panel's documented contract: the feed loads as
//! one batched round trip (capped page + server-max spend page + titles),
//! the health aggregate probes every supported provider (per-probe failures
//! degrade to unknown, never abort the rest), the update check and snapshot
//! run on their buttons only, the confirmed wipe reloads the privacy status,
//! and a `taskQueueOnly` store costs exactly one `list_tasks` call — the
//! dock's fetch-on-open contract (G-23), no health IPC.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SPEND_LIMIT_KEY } from "./useSpendLimit";
import { useActivity } from "./useActivity";

const { mockInvoke, failNext } = vi.hoisted(() => {
  const failNext = { command: "" };
  const fixtures = {
    feed: {
      runs: [
        {
          run_id: 1,
          conversation_id: null,
          model: "gpt",
          mode: "agent",
          status: "completed",
          started_at: 1000,
          finished_at: 1100,
          total_steps: 2,
          spent_micro_usd: null,
          limit_micro_usd: null,
        },
      ],
      totals: {
        runs: 1,
        total_steps: 2,
        total_spent_micro_usd: 0,
        runs_with_spend: 0,
        runs_with_limit: 0,
      },
    },
    conversations: [
      {
        id: 5,
        title: "chat",
        status: "active" as const,
        created_at: 900,
        updated_at: 950,
        workspace_root: null,
      },
    ],
    providers: [
      { name: "openai", display_name: "OpenAI", models: ["gpt"] },
      { name: "anthropic", display_name: "Anthropic", models: ["claude"] },
    ],
    bundle: {
      app_version: "2.0.0",
      platform_os: "linux",
      platform_arch: "x64",
      schema_version: 10,
      schema_target: 10,
      db_size_bytes: null,
      counts: {
        conversations: 1,
        messages: 1,
        prompts: 0,
        agent_runs: 1,
        agent_steps: 2,
        agent_tasks: 0,
      },
      fts_present: true,
      crashed_runs: [],
      error_runs: 0,
    },
    privacy: {
      surfaces: [],
      stats: [],
      total_events: 0,
      retention_days: 30,
      oldest_day: null,
    },
    wiped: { deleted_rows: 3 },
    tasks: [
      {
        id: 1,
        title: "queued",
        description: null,
        status: "pending" as const,
        conversation_id: null,
        provider: null,
        model: null,
        max_steps: 3,
        current_step: 0,
        total_steps: 3,
        report: null,
        run_id: null,
        created_at: 1,
        updated_at: 1,
      },
    ],
  };
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    void args;
    if (failNext.command === command) {
      failNext.command = "";
      throw { kind: "io", message: "backend refused" };
    }
    switch (command) {
      case "activity_feed":
        return JSON.parse(JSON.stringify(fixtures.feed));
      case "list_conversations":
        return JSON.parse(JSON.stringify(fixtures.conversations));
      case "supported_providers":
        return JSON.parse(JSON.stringify(fixtures.providers));
      case "provider_health":
        return {
          provider: (args as { name: string }).name,
          status: "healthy",
          has_configuration: true,
          has_credential: true,
          last_checked: 1000,
        };
      case "flags_status":
        return { flags: {}, notice: null };
      case "git_info":
        return { branch: "main", files: [], files_overflow: 0, commits: [] };
      case "get_setting":
        return null;
      case "diagnostics_bundle":
        return JSON.parse(JSON.stringify(fixtures.bundle));
      case "privacy_status":
        return JSON.parse(JSON.stringify(fixtures.privacy));
      case "conversation_context_stats":
        return {
          conversation_id: (args as { conversationId: number }).conversationId,
          title: "chat",
          provider: "openai",
          provider_display: "OpenAI",
          model: "gpt",
          context_limit: 128000,
          total_tokens: 1,
          input_tokens: 1,
          output_tokens: 0,
          reasoning_tokens: 0,
          cache_read_tokens: 0,
          cache_write_tokens: 0,
          has_token_data: true,
          total_cost_micro_usd: 0,
          usage_percent: 1,
          message_count: 1,
          user_message_count: 1,
          assistant_message_count: 0,
          tool_call_count: 0,
          other_step_count: 0,
          created_at: 1,
          updated_at: 2,
        };
      case "privacy_export":
        return { exported_at: 1, retention_days: 30, stats: [] };
      case "request_confirmation":
        return "confirm-id-1";
      case "privacy_wipe":
        return fixtures.wiped;
      case "update_check":
        return {
          owner: "shdw",
          repo: "nx",
          current_version: "2.0.0",
          latest_tag: null,
          latest_name: "",
          latest_url: null,
          published_at: null,
          update_available: false,
          authenticated: false,
          rate_limited: false,
          rate_limit: { limit: null, remaining: null, reset: null },
        };
      case "snapshot_database":
        return { file_name: "nx.db", size_bytes: 1, created_at: 1 };
      case "list_tasks":
        return JSON.parse(JSON.stringify(fixtures.tasks));
      default:
        throw new Error(`unexpected command ${command}`);
    }
  });
  return { mockInvoke, failNext };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
});

describe("useActivity — health panel store", () => {
  it("loads the feed batch and the whole health aggregate on mount", async () => {
    const { result } = renderHook(() =>
      useActivity({ activeConversationId: 5 }),
    );
    await waitFor(() => expect(result.current.healthLoading).toBe(false));
    await waitFor(() => expect(result.current.activityLoading).toBe(false));
    // Feed: capped page + server-max spend page + conversation titles.
    expect(mockInvoke).toHaveBeenCalledWith("activity_feed", { limit: 50 });
    expect(mockInvoke).toHaveBeenCalledWith("activity_feed", { limit: 100 });
    expect(mockInvoke).toHaveBeenCalledWith("list_conversations");
    expect(result.current.feed?.runs).toHaveLength(1);
    expect(result.current.conversations[0].id).toBe(5);
    // Health: every supported provider probed, plus flags/git/limit/bundle/privacy.
    expect(mockInvoke).toHaveBeenCalledWith("supported_providers");
    expect(mockInvoke).toHaveBeenCalledWith("provider_health", { name: "openai" });
    expect(mockInvoke).toHaveBeenCalledWith("provider_health", { name: "anthropic" });
    expect(mockInvoke).toHaveBeenCalledWith("flags_status");
    expect(mockInvoke).toHaveBeenCalledWith("git_info", { limit: 5 });
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", { key: SPEND_LIMIT_KEY });
    expect(mockInvoke).toHaveBeenCalledWith("diagnostics_bundle");
    expect(mockInvoke).toHaveBeenCalledWith("privacy_status");
    expect(mockInvoke).toHaveBeenCalledWith("conversation_context_stats", {
      conversationId: 5,
    });
    expect(result.current.health["openai"]?.status).toBe("healthy");
    expect(result.current.context?.usage_percent).toBe(1);
    expect(result.current.contextError).toBeNull();
    expect(result.current.privacy).not.toBeNull();
  });

  it("runs the update check and the snapshot on their buttons only", async () => {
    const { result } = renderHook(() => useActivity({ activeConversationId: null }));
    await waitFor(() => expect(result.current.healthLoading).toBe(false));
    const calls = mockInvoke.mock.calls.filter(
      ([command]) => command === "update_check" || command === "snapshot_database",
    ).length;
    expect(calls).toBe(0);

    await act(async () => {
      await result.current.checkUpdate();
    });
    expect(mockInvoke).toHaveBeenCalledWith("update_check");
    expect(result.current.update?.update_available).toBe(false);
    expect(result.current.updateBusy).toBe(false);

    await act(async () => {
      await result.current.makeSnapshot();
    });
    expect(mockInvoke).toHaveBeenCalledWith("snapshot_database");
    expect(result.current.snapshot?.file_name).toBe("nx.db");
  });

  it("wipes the ledger through the mint gate and reloads the status", async () => {
    const { result } = renderHook(() => useActivity({ activeConversationId: null }));
    await waitFor(() => expect(result.current.healthLoading).toBe(false));
    const privacyCalls = mockInvoke.mock.calls.filter(
      ([command]) => command === "privacy_status",
    ).length;
    await act(async () => {
      const result2 = await result.current.wipe();
      expect(result2.deleted_rows).toBe(3);
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "privacy",
      operation: "privacy_wipe",
      cwd: null,
      targetId: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("privacy_wipe", { confirmationId: "confirm-id-1" });
    expect(
      mockInvoke.mock.calls.filter(([command]) => command === "privacy_status").length,
    ).toBeGreaterThan(privacyCalls);
  });

  it("degrades a failed context probe to the section error", async () => {
    failNext.command = "conversation_context_stats";
    const { result } = renderHook(() =>
      useActivity({ activeConversationId: 5 }),
    );
    await waitFor(() => expect(result.current.healthLoading).toBe(false));
    expect(result.current.contextError).toBe("backend refused");
    expect(result.current.context).toBeNull();
  });
});

describe("useActivity — dock queue store", () => {
  it("fires exactly one list_tasks and no health IPC", async () => {
    const { result } = renderHook(() => useActivity({ taskQueueOnly: true }));
    await waitFor(() => expect(result.current.queue).not.toBeNull());
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("list_tasks");
    expect(result.current.queue?.[0].title).toBe("queued");
    expect(result.current.queueError).toBeNull();
    // The health slices never ran.
    expect(mockInvoke.mock.calls.some(([command]) => command === "supported_providers")).toBe(
      false,
    );
    expect(mockInvoke.mock.calls.some(([command]) => command === "activity_feed")).toBe(false);
  });

  it("surfaces a failed queue fetch with the honest fallback", async () => {
    failNext.command = "list_tasks";
    const { result } = renderHook(() => useActivity({ taskQueueOnly: true }));
    await waitFor(() => expect(result.current.queue).toEqual([]));
    expect(result.current.queueError).not.toBeNull();
  });
});
