//! Hook tests for `useAgentRun` (NEX-TEST-001).
//!
//! Mocks mirror the real wrapper shapes in `src/lib/tauri.ts` exactly:
//! commands are snake_case (`list_agent_runs`, `list_agent_steps`,
//! `get_agent_step_observation`), args are camelCase (`conversationId`,
//! `runId`, `stepId`), and `listen` delivers Tauri `Event<T>` objects whose
//! payload is read from `.payload`.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AgentRun,
  AgentRunEventPayload,
  AgentStep,
  StepEventFrame,
  GovernanceEventPayload,
  DurableRunEvent,
  DurableRunEventPage,
} from "./tauri";
import { listAgentRuns, listAgentSteps, startAgentRun } from "./tauri";
import { useAgentRun } from "./useAgentRun";

type TauriEvent = { payload: AgentRunEventPayload };

const { mockInvoke, mockListen, listeners, emitAgentEvent } = vi.hoisted(() => {
  const listeners: Array<(event: TauriEvent) => void> = [];
  const mockInvoke = vi.fn();
  const mockListen = vi.fn(
    async (
      _eventName: string,
      handler: (event: TauriEvent) => void
    ): Promise<() => void> => {
      listeners.push(handler);
      return () => {
        const i = listeners.indexOf(handler);
        if (i >= 0) listeners.splice(i, 1);
      };
    }
  );
  const emitAgentEvent = (payload: AgentRunEventPayload): void => {
    for (const handler of [...listeners]) handler({ payload });
  };
  return { mockInvoke, mockListen, listeners, emitAgentEvent };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

function runRow(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    id: 7,
    conversation_id: 1,
    model: "test-model",
    mode: "supervised",
    interrupted: false,
    status: "running",
    started_at: 1000,
    finished_at: null,
    total_steps: 0,
    final_content: null,
    error: null,
    spent_micro_usd: null,
    limit_micro_usd: null,
    ...overrides,
  };
}

function stepRow(overrides: Partial<AgentStep> = {}): AgentStep {
  return {
    id: 11,
    run_id: 7,
    seq: 1,
    kind: "tool",
    tool_name: "read_file",
    arguments: "{}",
    observation: "out",
    observation_len: 3,
    observation_truncated: false,
    status: "ok",
    started_at: 1000,
    duration_ms: 5,
    rule_id: null,
    group_key: null,
    decided_by: null,
    input_tokens: null,
    output_tokens: null,
    ...overrides,
  };
}

function stepFrame(overrides: Partial<StepEventFrame> = {}): StepEventFrame {
  return {
    step_id: 11,
    seq: 1,
    kind: "tool",
    tool_name: "read_file",
    arguments: "{}",
    observation: "out",
    observation_len: 3,
    observation_truncated: false,
    status: "ok",
    duration_ms: 5,
    ...overrides,
  };
}

function approvalRequest(): GovernanceEventPayload {
  return {
    type: "approval_requested",
    call_id: "call",
    name: "write_file",
    arguments: "{}",
    group_key: null,
    group_size: 1,
    timeout_secs: 30,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function eventPage(
  events: DurableRunEvent[] = [],
  overrides: Partial<DurableRunEventPage> = {}
): DurableRunEventPage {
  return {
    events,
    next_cursor: events[events.length - 1]?.event_id ?? 0,
    high_water_event_id: events[events.length - 1]?.event_id ?? 0,
    has_more: false,
    ...overrides,
  };
}
function durable(
  event_id: number,
  overrides: Partial<DurableRunEvent> = {}
): DurableRunEvent {
  return {
    event_id,
    conversation_id: 1,
    run_id: 7,
    kind: "step_recorded",
    step_id: 11,
    schema_version: 1,
    created_at: 1001,
    step: stepRow(),
    terminal: null,
    ...overrides,
  };
}

/** Default invoke router mirroring the `tauri.ts` wrapper contract. */
function routeInvoke(
  runsByConversation: Record<number, AgentRun[]>,
  stepsByRun: Record<number, AgentStep[]>,
  fullObservation = "FULL OBSERVATION TEXT"
): void {
  mockInvoke.mockImplementation(
    (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "list_conversation_run_events")
        return Promise.resolve(
          eventPage([], { next_cursor: Number(args?.afterEventId ?? 0) })
        );
      if (cmd === "list_agent_runs") {
        const id = args?.["conversationId"];
        if (typeof id !== "number" || !(id in runsByConversation)) {
          return Promise.reject(
            new Error(`no fixture for conversation ${String(id)}`)
          );
        }
        return Promise.resolve(runsByConversation[id]);
      }
      if (cmd === "list_agent_steps") {
        const id = args?.["runId"];
        if (typeof id !== "number" || !(id in stepsByRun)) {
          return Promise.reject(new Error(`no fixture for run ${String(id)}`));
        }
        return Promise.resolve(stepsByRun[id]);
      }
      if (cmd === "get_agent_step_observation") {
        return Promise.resolve(fullObservation);
      }
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  );
}

function historyRouter(
  read: (after: number) => Promise<DurableRunEventPage>,
  runs = [runRow()]
) {
  routeInvoke(
    { 1: runs, 2: [runRow({ id: 8, conversation_id: 2 })] },
    { 7: [stepRow()] }
  );
  const fallback = mockInvoke.getMockImplementation()!;
  mockInvoke.mockImplementation(
    (command: string, args?: Record<string, unknown>) =>
      command === "list_conversation_run_events"
        ? read(Number(args?.afterEventId ?? 0))
        : fallback(command, args)
  );
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockClear();
  listeners.length = 0;
});

describe("useAgentRun", () => {
  it("rehydrates only explicitly flagged interruptions and reads retained steps without execution", async () => {
    const message = "run interrupted by application shutdown";
    routeInvoke(
      {
        1: [
          runRow({
            status: "error",
            interrupted: true,
            finished_at: 1001,
            total_steps: 1,
            error: message,
          }),
          runRow({ id: 8, status: "error", finished_at: 1001, error: message }),
        ],
      },
      { 7: [stepRow({ observation: "retained output" })] }
    );
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs).toHaveLength(2));
    expect(result.current.runs.map((run) => run.status)).toEqual([
      "interrupted",
      "error",
    ]);
    expect(result.current.runs[0].pending_approval).toBeNull();
    await act(async () => result.current.loadRunSteps(7));
    expect(result.current.runs[0].steps[0].observation).toBe("retained output");
    await act(async () => result.current.reload());
    expect(result.current.runs[0].status).toBe("interrupted");
    expect(
      mockInvoke.mock.calls.every(
        ([command]) =>
          command === "list_agent_runs" ||
          command === "list_agent_steps" ||
          command === "list_conversation_run_events"
      )
    ).toBe(true);
  });

  it("rehydrates a bounded run page on mount and lazy-loads steps only when asked", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [stepRow()] });

    const { result } = renderHook(() => useAgentRun(1));

    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    expect(result.current.runs[0]).toMatchObject({
      run_id: 7,
      conversation_id: 1,
      status: "running",
      // AC-102/ADD-08: runs rehydrate WITHOUT their step lists.
      steps_loaded: false,
      total_steps: 0,
    });
    expect(result.current.runs[0].steps).toHaveLength(0);
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
    // Wrapper contract: snake_case command, camelCase args, bounded page
    // (REHYDRATION_RUN_PAGE = 50), no cursor on the initial load.
    expect(mockInvoke).toHaveBeenCalledWith("list_agent_runs", {
      conversationId: 1,
      limit: 50,
      beforeRunId: null,
    });
    // The N+1 must stay dead: NO per-run steps fan-out on load.
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "list_agent_steps",
      expect.anything()
    );
    // Channel contract: production subscribes to "agent-run-event"
    // (useAgentRun.ts); guards a silent channel rename.
    expect(mockListen).toHaveBeenCalledWith(
      "agent-run-event",
      expect.any(Function)
    );

    // Lazy load: the expanded run asks once, steps arrive mapped, and a
    // second ask is a no-op (already loaded — no second IPC).
    await act(async () => {
      await result.current.loadRunSteps(7);
    });
    expect(result.current.runs[0].steps).toHaveLength(1);
    expect(result.current.runs[0].steps[0]).toMatchObject({
      seq: 1,
      tool_name: "read_file",
    });
    expect(result.current.runs[0].steps_loaded).toBe(true);
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd !== "list_conversation_run_events"
      )
    ).toHaveLength(2);
    expect(mockInvoke).toHaveBeenLastCalledWith("list_agent_steps", {
      runId: 7,
      limit: null,
      beforeSeq: null,
    });
    await act(async () => {
      await result.current.loadRunSteps(7);
    });
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd !== "list_conversation_run_events"
      )
    ).toHaveLength(2);
  });

  it("wrapper forwards optional pagination args camelCase over the snake_case commands", async () => {
    // The wrapper contract pinned directly: optional camelCase keys flow
    // through as null when absent and verbatim when present (the Rust side
    // takes `limit` / `before_run_id` / `before_seq` as Option<i64>).
    mockInvoke.mockResolvedValue([]);
    await listAgentRuns(1, { limit: 10, beforeRunId: 42 });
    expect(mockInvoke).toHaveBeenLastCalledWith("list_agent_runs", {
      conversationId: 1,
      limit: 10,
      beforeRunId: 42,
    });
    await listAgentSteps(7, { limit: 5, beforeSeq: 12 });
    expect(mockInvoke).toHaveBeenLastCalledWith("list_agent_steps", {
      runId: 7,
      limit: 5,
      beforeSeq: 12,
    });
  });

  it("merges live step frames sorted by seq and ignores duplicate seq", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [] });

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ seq: 2, step_id: 12 }),
      });
    });
    expect(result.current.runs[0].steps.map((s) => s.seq)).toEqual([2]);

    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ seq: 1 }),
      });
    });
    expect(result.current.runs[0].steps.map((s) => s.seq)).toEqual([1, 2]);

    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ seq: 1 }),
      });
    });
    expect(result.current.runs[0].steps).toHaveLength(2);
  });

  it("ignores finished frames from other conversations (finished-frame scope only)", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [] });

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    const reloadCalls = (): number =>
      mockInvoke.mock.calls.filter((c) => c[0] === "list_agent_runs").length;
    const baseline = reloadCalls();

    // Foreign conversation's terminal frame: must not add a run, must not reload.
    act(() => {
      emitAgentEvent({
        type: "finished",
        run_id: 999,
        event: {
          conversation_id: 2,
          status: "completed",
          final_content: "foreign",
          error: null,
        },
      });
    });
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.runs[0].run_id).toBe(7);
    expect(reloadCalls()).toBe(baseline);

    // Own conversation's terminal frame: honored — AC-102/ADD-08 PATCHES the
    // affected run from the payload (status/finished_at/error) and lazy-
    // fetches its now-final step list ONCE, instead of re-firing the full
    // rehydration fan-out.
    const stepsCalls = (): number =>
      mockInvoke.mock.calls.filter((c) => c[0] === "list_agent_steps").length;
    act(() => {
      emitAgentEvent({
        type: "finished",
        run_id: 7,
        event: {
          conversation_id: 1,
          status: "completed",
          final_content: "done",
          error: null,
        },
      });
    });
    // No runs re-query: the patch came from the frame payload alone.
    expect(reloadCalls()).toBe(baseline);
    await waitFor(() =>
      expect(result.current.runs[0].status).toBe("completed")
    );
    expect(result.current.runs[0].error).toBeNull();
    expect(result.current.runs[0].finished_at).not.toBeNull();
    // The run had no steps on screen (rehydrated unloaded), so its final
    // step list loads exactly once.
    await waitFor(() => expect(stepsCalls()).toBe(1));
    expect(mockInvoke).toHaveBeenCalledWith("list_agent_steps", {
      runId: 7,
      limit: null,
      beforeSeq: null,
    });
    expect(result.current.runs[0].steps_loaded).toBe(true);
  });

  it("finished frame for a run with live steps patches without a steps fetch", async () => {
    // A run the user watched live carries its steps from the frames — the
    // terminal frame must not re-fetch what is already on screen.
    routeInvoke({ 1: [] }, { 7: [] });

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ seq: 1 }),
      });
    });
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.runs[0].steps_loaded).toBe(true);

    act(() => {
      emitAgentEvent({
        type: "finished",
        run_id: 7,
        event: {
          conversation_id: 1,
          status: "completed",
          final_content: "done",
          error: null,
        },
      });
    });
    expect(result.current.runs[0].status).toBe("completed");
    expect(result.current.runs[0].steps.map((s) => s.seq)).toEqual([1]);
    expect(
      mockInvoke.mock.calls.filter((c) => c[0] === "list_agent_steps")
    ).toHaveLength(0);
  });

  it("rejects foreign and unscoped live frames for known and unknown runs without fetching", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [] });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() =>
      emitAgentEvent({
        type: "governance",
        conversation_id: 1,
        run_id: 7,
        event: approvalRequest(),
      })
    );
    const before = result.current.runs;
    const calls = mockInvoke.mock.calls.length;

    act(() => {
      for (const run_id of [7, 999]) {
        for (const conversation_id of [2, undefined]) {
          // Exercise malformed transport data as well as valid foreign envelopes.
          emitAgentEvent({
            type: "step",
            conversation_id,
            run_id,
            event: stepFrame(),
          } as AgentRunEventPayload);
          for (const event of [
            approvalRequest(),
            { type: "approval_resolved", call_id: "call", approved: true },
            { type: "cancelled" },
          ] as GovernanceEventPayload[]) {
            emitAgentEvent({
              type: "governance",
              conversation_id,
              run_id,
              event,
            } as AgentRunEventPayload);
          }
        }
      }
    });
    expect(result.current.runs).toBe(before);
    expect(result.current.runs[0].pending_approval?.call_id).toBe("call");
    expect(mockInvoke).toHaveBeenCalledTimes(calls);
  });

  it.each(["step", "approval"])(
    "accepts an unknown owned %s before the start response",
    async (first) => {
      routeInvoke({ 1: [] }, {});
      const { result } = renderHook(() => useAgentRun(1));
      await waitFor(() => expect(result.current.loading).toBe(false));
      let resolveStart!: (value: { run_id: number }) => void;
      mockInvoke.mockImplementationOnce(
        () =>
          new Promise<{ run_id: number }>((resolve) => {
            resolveStart = resolve;
          })
      );
      const starting = startAgentRun(1, "do work", "openai", "test-model");
      const frames: AgentRunEventPayload[] = [
        { type: "step", conversation_id: 1, run_id: 8, event: stepFrame() },
        {
          type: "governance",
          conversation_id: 1,
          run_id: 8,
          event: approvalRequest(),
        },
      ];
      if (first === "approval") frames.reverse();
      act(() => emitAgentEvent(frames[0]));
      expect(result.current.runs).toHaveLength(1);
      expect(result.current.runs[0]).toMatchObject({
        run_id: 8,
        conversation_id: 1,
        status: "running",
      });
      if (first === "step")
        expect(result.current.runs[0].steps).toHaveLength(1);
      else
        expect(result.current.runs[0].pending_approval?.call_id).toBe("call");
      act(() => emitAgentEvent(frames[1]));
      expect(result.current.runs[0].steps).toHaveLength(1);
      expect(result.current.runs[0].pending_approval?.call_id).toBe("call");
      await act(async () => {
        resolveStart({ run_id: 8 });
        await starting;
      });
      expect(
        mockInvoke.mock.calls
          .map(([command]) => command)
          .filter((command) => command !== "list_conversation_run_events")
      ).toEqual(["list_agent_runs", "start_agent_run"]);
    }
  );

  it("rejects delayed frames after switching conversation and keeps owned governor events live", async () => {
    routeInvoke(
      { 1: [runRow()], 2: [runRow({ id: 8, conversation_id: 2 })] },
      {}
    );
    const { result, rerender } = renderHook(({ id }) => useAgentRun(id), {
      initialProps: { id: 1 },
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    const delayed = () => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 8,
        event: stepFrame(),
      });
      emitAgentEvent({
        type: "governance",
        conversation_id: 1,
        run_id: 8,
        event: approvalRequest(),
      });
    };
    rerender({ id: 2 });
    await waitFor(() =>
      expect(result.current.runs[0]?.conversation_id).toBe(2)
    );
    const before = result.current.runs;
    const calls = mockInvoke.mock.calls.length;
    act(delayed);
    expect(result.current.runs).toBe(before);
    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 2,
        run_id: 8,
        event: stepFrame(),
      });
      emitAgentEvent({
        type: "governance",
        conversation_id: 2,
        run_id: 8,
        event: approvalRequest(),
      });
    });
    expect(result.current.runs[0].steps).toHaveLength(1);
    expect(result.current.runs[0].pending_approval?.call_id).toBe("call");
    for (const [event, status] of [
      [{ type: "paused" }, "paused"],
      [{ type: "resumed" }, "running"],
      [{ type: "budget_exhausted", max_steps: 5 }, "budget_exhausted"],
      [
        { type: "spend_limit_exceeded", spent_micro: 2, limit_micro: 1 },
        "spend_limit_exceeded",
      ],
      [{ type: "cancelled" }, "cancelled"],
    ] as [GovernanceEventPayload, string][]) {
      act(() =>
        emitAgentEvent({
          type: "governance",
          conversation_id: 2,
          run_id: 8,
          event,
        })
      );
      expect(result.current.runs[0].status).toBe(status);
    }
    expect(result.current.runs[0].pending_approval).toBeNull();
    expect(mockInvoke).toHaveBeenCalledTimes(calls);
  });

  it("drops a stale reload that resolves after a conversation switch", async () => {
    let resolveConv1: ((runs: AgentRun[]) => void) | null = null;
    mockInvoke.mockImplementation(
      (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "list_agent_runs") {
          const id = args?.["conversationId"];
          if (id === 1) {
            return new Promise<AgentRun[]>((resolve) => {
              resolveConv1 = resolve;
            });
          }
          return Promise.resolve([runRow({ id: 8, conversation_id: 2 })]);
        }
        if (cmd === "list_agent_steps") return Promise.resolve([]);
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
    );

    const { result, rerender } = renderHook(
      ({ id }: { id: number | null }) => useAgentRun(id),
      { initialProps: { id: 1 as number | null } }
    );

    // Switch before conversation 1's reload resolves.
    rerender({ id: 2 });
    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    expect(result.current.runs[0].run_id).toBe(8);

    // The late conversation-1 response must not clobber conversation-2 state.
    await act(async () => {
      resolveConv1?.([runRow()]);
    });
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.runs[0].run_id).toBe(8);
  });

  it("expands a truncated step on demand and drops the text on collapse", async () => {
    routeInvoke(
      { 1: [runRow()] },
      { 7: [stepRow({ observation: "preview", observation_truncated: true })] },
      "FULL OBSERVATION TEXT"
    );

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    // AC-102/ADD-08: steps lazy-load first — the observation expand works on
    // the fetched list, exactly as it did on the rehydrated one.
    await act(async () => {
      await result.current.loadRunSteps(7);
    });
    await waitFor(() => expect(result.current.runs[0]?.steps).toHaveLength(1));
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();

    await act(async () => {
      await result.current.expandStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_full).toBe(
      "FULL OBSERVATION TEXT"
    );
    expect(mockInvoke).toHaveBeenCalledWith("get_agent_step_observation", {
      stepId: 11,
    });

    act(() => {
      result.current.collapseStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();
  });

  it("collapse before a late expand resolve does not resurrect the full text", async () => {
    let resolveObs: ((value: string) => void) | null = null;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "list_agent_runs") return Promise.resolve([runRow()]);
      if (cmd === "list_agent_steps") {
        return Promise.resolve([
          stepRow({ observation: "preview", observation_truncated: true }),
        ]);
      }
      if (cmd === "get_agent_step_observation") {
        return new Promise<string>((resolve) => {
          resolveObs = resolve;
        });
      }
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    });

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.loadRunSteps(7);
    });
    await waitFor(() => expect(result.current.runs[0]?.steps).toHaveLength(1));

    // Start the expand (sets the loading flag), then collapse before the
    // fetch resolves. Deterministic deferred — no timers, no sleeps.
    let expandPromise: Promise<void> | undefined;
    act(() => {
      expandPromise = result.current.expandStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_loading).toBe(true);
    act(() => {
      result.current.collapseStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_loading).toBe(false);
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();

    // Late resolve must not resurrect the dropped text: the write is guarded
    // on the loading flag cleared by collapseStep (useAgentRun.ts:203-207,
    // 264-282).
    await act(async () => {
      resolveObs?.("FULL OBSERVATION TEXT");
      await expandPromise;
    });
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();
    expect(result.current.runs[0].steps[0].observation_loading).toBe(false);
  });

  it("drains multiple committed pages using next_cursor and deduplicates event, step identity and seq", async () => {
    const reads: number[] = [];
    historyRouter(async (after) => {
      reads.push(after);
      if (after === 0)
        return eventPage([durable(1)], {
          next_cursor: 1,
          high_water_event_id: 20,
          has_more: true,
        });
      if (after === 1)
        return eventPage(
          [
            durable(2, { step: stepRow({ id: 11, seq: 9 }) }),
            durable(2, { step: stepRow({ id: 12, seq: 3 }), step_id: 12 }),
            durable(3, { step: stepRow({ id: 13, seq: 2 }), step_id: 13 }),
          ],
          { next_cursor: 3, high_water_event_id: 20, has_more: true }
        );
      if (after === 3)
        return eventPage(
          [
            durable(4, {
              kind: "run_finished",
              step_id: null,
              step: null,
              terminal: {
                conversation_id: 1,
                status: "completed",
                error: null,
                final_content: null,
              },
            }),
          ],
          { next_cursor: 20 }
        );
      return eventPage([], { next_cursor: after });
    });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(reads.slice(0, 3)).toEqual([0, 1, 3]);
    expect(result.current.runs[0].steps.map((step) => step.seq)).toEqual([
      1, 2,
    ]);
    expect(result.current.runs[0].status).toBe("completed");
    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame(),
      });
      for (const event_id of [4, 2, 4, 1])
        emitAgentEvent({
          type: "durable",
          conversation_id: 1,
          run_id: 7,
          event_id,
        });
    });
    await act(async () => {});
    expect(reads.slice(3).every((cursor) => cursor === 20)).toBe(true);
    expect(result.current.runs[0].steps).toHaveLength(2);
    expect(
      mockInvoke.mock.calls.every(([command]) =>
        ["list_agent_runs", "list_conversation_run_events"].includes(command)
      )
    ).toBe(true);
  });

  it("manual reconnect catches missed steps and finish without any notification", async () => {
    let journal: DurableRunEvent[] = [];
    historyRouter(async (after) =>
      eventPage(
        journal.filter((event) => event.event_id > after),
        { next_cursor: journal[journal.length - 1]?.event_id ?? after }
      )
    );
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    journal = [
      durable(1),
      durable(2, {
        kind: "run_interrupted",
        step_id: null,
        step: null,
        terminal: {
          conversation_id: 1,
          status: "interrupted",
          error: "shutdown",
          final_content: null,
        },
      }),
    ];
    await act(async () => result.current.reload());
    expect(result.current.runs[0]).toMatchObject({
      status: "interrupted",
      pending_approval: null,
    });
    expect(result.current.runs[0].steps.map((step) => step.step_id)).toEqual([
      11,
    ]);
  });

  it("reconciles the asynchronous subscription setup gap without a durable hint", async () => {
    const subscribed = deferred<() => void>();
    mockListen.mockImplementationOnce(() => subscribed.promise);
    let journal: DurableRunEvent[] = [];
    historyRouter(async (after) =>
      eventPage(
        journal.filter((event) => event.event_id > after),
        { next_cursor: journal[journal.length - 1]?.event_id ?? after }
      )
    );
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    journal = [durable(1)];
    await act(async () => subscribed.resolve(vi.fn()));
    await waitFor(() => expect(result.current.runs[0].steps).toHaveLength(1));
  });

  it("queues a second cursor pass for a hint arriving during an outstanding read", async () => {
    const first = deferred<DurableRunEventPage>();
    const reads: number[] = [];
    historyRouter((after) => {
      reads.push(after);
      return reads.length === 1
        ? first.promise
        : Promise.resolve(eventPage([durable(2)], { next_cursor: 2 }));
    });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(reads).toHaveLength(1));
    act(() =>
      emitAgentEvent({
        type: "durable",
        conversation_id: 1,
        run_id: 7,
        event_id: 999,
      })
    );
    await act(async () =>
      first.resolve(eventPage([], { next_cursor: 1, high_water_event_id: 1 }))
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(reads).toEqual([0, 1]);
    expect(result.current.runs[0].steps).toHaveLength(1);
  });

  it.each(["resolve", "reject"] as const)(
    "fences old A history %s across A -> B -> A",
    async (outcome) => {
      const old = deferred<AgentRun[]>();
      routeInvoke(
        {
          1: [runRow({ model: "new A" })],
          2: [runRow({ id: 8, conversation_id: 2 })],
        },
        {}
      );
      const fallback = mockInvoke.getMockImplementation()!;
      let first = true;
      mockInvoke.mockImplementation(
        (command: string, args?: Record<string, unknown>) => {
          if (
            command === "list_agent_runs" &&
            args?.conversationId === 1 &&
            first
          ) {
            first = false;
            return old.promise;
          }
          return fallback(command, args);
        }
      );
      const { result, rerender } = renderHook(({ id }) => useAgentRun(id), {
        initialProps: { id: 1 },
      });
      rerender({ id: 2 });
      rerender({ id: 1 });
      await waitFor(() => expect(result.current.loading).toBe(false));
      await act(async () =>
        outcome === "resolve"
          ? old.resolve([runRow({ model: "stale A" })])
          : old.reject(new Error("old error"))
      );
      expect(result.current.runs[0].model).toBe("new A");
      expect(result.current.error).toBeNull();
      expect(result.current.loading).toBe(false);
    }
  );

  it("latest reload wins and stale errors/finally cannot end a newer request", async () => {
    routeInvoke({ 1: [runRow()] }, {});
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    const first = deferred<AgentRun[]>(),
      second = deferred<AgentRun[]>();
    const fallback = mockInvoke.getMockImplementation()!;
    let requests = 0;
    mockInvoke.mockImplementation(
      (command: string, args?: Record<string, unknown>) =>
        command === "list_agent_runs"
          ? ++requests === 1
            ? first.promise
            : second.promise
          : fallback(command, args)
    );
    let a!: Promise<void>, b!: Promise<void>;
    act(() => {
      a = result.current.reload();
      b = result.current.reload();
    });
    await act(async () => {
      first.reject(new Error("stale"));
      await a;
    });
    expect(result.current.loading).toBe(true);
    expect(result.current.error).toBeNull();
    await act(async () => {
      second.resolve([runRow({ model: "latest" })]);
      await b;
    });
    expect(result.current.runs[0].model).toBe("latest");
    expect(result.current.loading).toBe(false);
  });

  it("stale reload metadata preserves live steps, live approval and terminal outcome", async () => {
    routeInvoke({ 1: [runRow()] }, {});
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    const snapshot = deferred<AgentRun[]>();
    mockInvoke.mockImplementationOnce(() => snapshot.promise);
    let request!: Promise<void>;
    act(() => {
      request = result.current.reload();
    });
    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame(),
      });
      emitAgentEvent({
        type: "governance",
        conversation_id: 1,
        run_id: 7,
        event: approvalRequest(),
      });
    });
    await act(async () => {
      snapshot.resolve([runRow({ status: "completed" })]);
      await request;
    });
    expect(result.current.runs[0].status).toBe("running");
    expect(result.current.runs[0].pending_approval?.call_id).toBe("call");
    const next = deferred<AgentRun[]>();
    mockInvoke.mockImplementationOnce(() => next.promise);
    act(() => {
      request = result.current.reload();
    });
    act(() =>
      emitAgentEvent({
        type: "finished",
        run_id: 7,
        event: {
          conversation_id: 1,
          status: "completed",
          error: null,
          final_content: null,
        },
      })
    );
    await act(async () => {
      next.resolve([runRow()]);
      await request;
    });
    expect(result.current.runs[0]).toMatchObject({
      status: "completed",
      pending_approval: null,
    });
    expect(result.current.runs[0].steps).toHaveLength(1);
  });

  it("merges a lazy snapshot with live steps and preserves loaded history on reload", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [stepRow()] });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    const lazy = deferred<AgentStep[]>();
    mockInvoke.mockImplementationOnce(() => lazy.promise);
    let request!: Promise<void>;
    act(() => {
      request = result.current.loadRunSteps(7);
    });
    act(() =>
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ seq: 2, step_id: 12 }),
      })
    );
    await act(async () => {
      lazy.resolve([stepRow()]);
      await request;
    });
    await act(async () => result.current.expandStep(7, 1));
    await act(async () => result.current.reload());
    expect(result.current.runs[0].steps.map((step) => step.step_id)).toEqual([
      11, 12,
    ]);
    expect(result.current.runs[0].steps[0].observation_full).toBe(
      "FULL OBSERVATION TEXT"
    );
    expect(result.current.runs[0].steps_loaded).toBe(true);
  });

  it("old lazy completion/finally cannot fill or unlock a new A generation", async () => {
    routeInvoke({ 1: [runRow()], 2: [] }, { 7: [stepRow()] });
    const { result, rerender } = renderHook(({ id }) => useAgentRun(id), {
      initialProps: { id: 1 },
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    const old = deferred<AgentStep[]>(),
      fresh = deferred<AgentStep[]>();
    mockInvoke.mockImplementationOnce(() => old.promise);
    let a!: Promise<void>, b!: Promise<void>;
    act(() => {
      a = result.current.loadRunSteps(7);
    });
    rerender({ id: 2 });
    rerender({ id: 1 });
    await waitFor(() => expect(result.current.loading).toBe(false));
    mockInvoke.mockImplementationOnce(() => fresh.promise);
    act(() => {
      b = result.current.loadRunSteps(7);
    });
    await act(async () => {
      old.resolve([stepRow({ observation: "old" })]);
      await a;
    });
    expect(result.current.runs[0].steps).toHaveLength(0);
    const count = mockInvoke.mock.calls.length;
    await act(async () => result.current.loadRunSteps(7));
    expect(mockInvoke.mock.calls).toHaveLength(count);
    await act(async () => {
      fresh.resolve([stepRow({ observation: "fresh" })]);
      await b;
    });
    expect(result.current.runs[0].steps[0].observation).toBe("fresh");
  });

  it.each(["resolve", "reject"] as const)(
    "fences an old observation %s after A -> B -> A",
    async (outcome) => {
      routeInvoke({ 1: [runRow()], 2: [] }, { 7: [stepRow()] });
      const { result, rerender } = renderHook(({ id }) => useAgentRun(id), {
        initialProps: { id: 1 },
      });
      await waitFor(() => expect(result.current.loading).toBe(false));
      await act(async () => result.current.loadRunSteps(7));
      const old = deferred<string>(),
        fresh = deferred<string>();
      mockInvoke.mockImplementationOnce(() => old.promise);
      let a!: Promise<void>, b!: Promise<void>;
      act(() => {
        a = result.current.expandStep(7, 1);
      });
      rerender({ id: 2 });
      rerender({ id: 1 });
      await waitFor(() => expect(result.current.loading).toBe(false));
      await act(async () => result.current.loadRunSteps(7));
      mockInvoke.mockImplementationOnce(() => fresh.promise);
      act(() => {
        b = result.current.expandStep(7, 1);
      });
      await act(async () => {
        if (outcome === "resolve") old.resolve("old");
        else old.reject(new Error("old error"));
        await a;
      });
      expect(result.current.runs[0].steps[0]).toMatchObject({
        observation_full: null,
        observation_loading: true,
        observation_error: null,
      });
      await act(async () => {
        fresh.resolve("fresh");
        await b;
      });
      expect(result.current.runs[0].steps[0].observation_full).toBe("fresh");
    }
  );

  it("collapse and re-expand fences stale observation errors within one generation", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [stepRow()] });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => result.current.loadRunSteps(7));
    const old = deferred<string>(),
      fresh = deferred<string>();
    mockInvoke
      .mockImplementationOnce(() => old.promise)
      .mockImplementationOnce(() => fresh.promise);
    let a!: Promise<void>, b!: Promise<void>;
    act(() => {
      a = result.current.expandStep(7, 1);
    });
    act(() => {
      result.current.collapseStep(7, 1);
      b = result.current.expandStep(7, 1);
    });
    await act(async () => {
      old.reject(new Error("old"));
      await a;
    });
    expect(result.current.runs[0].steps[0]).toMatchObject({
      observation_loading: true,
      observation_error: null,
    });
    await act(async () => {
      fresh.resolve("new");
      await b;
    });
    expect(result.current.runs[0].steps[0].observation_full).toBe("new");
  });

  it("keeps old terminal history bounded and lazy without restoring approvals", async () => {
    historyRouter(
      async () =>
        eventPage([
          durable(1, { run_id: 1, step: stepRow({ run_id: 1 }) }),
          durable(2),
        ]),
      [runRow({ status: "completed", total_steps: 1 })]
    );
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.runs[0]).toMatchObject({
      steps: [],
      steps_loaded: false,
      pending_approval: null,
    });
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "list_agent_steps")
    ).toBe(false);
    await act(async () => result.current.loadRunSteps(7));
    expect(result.current.runs[0].steps[0].step_id).toBe(11);
  });

  it("rejects foreign durable identities before mutation and does not acknowledge their page", async () => {
    let foreign = false;
    const reads: number[] = [];
    historyRouter(async (after) => {
      reads.push(after);
      return foreign
        ? eventPage([durable(2, { conversation_id: 2 })], { next_cursor: 8 })
        : eventPage([], { next_cursor: after });
    });
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    foreign = true;
    const count = reads.length;
    act(() =>
      emitAgentEvent({
        type: "durable",
        conversation_id: 2,
        run_id: 7,
        event_id: 2,
      })
    );
    expect(reads).toHaveLength(count);
    await act(async () => result.current.reload());
    expect(result.current.error).toBe("Run history identity mismatch");
    expect(result.current.runs[0].steps).toHaveLength(0);
    foreign = false;
    await act(async () => result.current.reload());
    expect(reads[reads.length - 1]).toBe(0);
  });

  it("cleans a delayed subscription once and fences callbacks and reads after unmount", async () => {
    const subscribed = deferred<() => void>(),
      page = deferred<DurableRunEventPage>();
    const dispose = vi.fn();
    let callback!: (event: TauriEvent) => void;
    mockListen.mockImplementationOnce((_name, handler) => {
      callback = handler;
      return subscribed.promise;
    });
    historyRouter(() => page.promise);
    const { unmount } = renderHook(() => useAgentRun(1));
    await waitFor(() =>
      expect(
        mockInvoke.mock.calls.some(
          ([command]) => command === "list_conversation_run_events"
        )
      ).toBe(true)
    );
    unmount();
    await act(async () => {
      subscribed.resolve(dispose);
      page.resolve(eventPage([durable(1)]));
    });
    const count = mockInvoke.mock.calls.length;
    act(() =>
      callback({
        payload: {
          type: "durable",
          conversation_id: 1,
          run_id: 7,
          event_id: 100,
        },
      })
    );
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(mockInvoke.mock.calls).toHaveLength(count);
  });

  it("fills partial live steps when Finished precedes the initial durable page", async () => {
    const initial = deferred<DurableRunEventPage>();
    historyRouter((after) =>
      after === 0
        ? initial.promise
        : Promise.resolve(eventPage([], { next_cursor: after }))
    );
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("list_conversation_run_events", {
        conversationId: 1,
        afterEventId: 0,
        limit: 100,
      })
    );
    expect(result.current.runs[0]).toMatchObject({
      status: "running",
      steps_loaded: false,
    });
    act(() => {
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame({ step_id: 12, seq: 2 }),
      });
      emitAgentEvent({
        type: "finished",
        run_id: 7,
        event: {
          conversation_id: 1,
          status: "completed",
          error: null,
          final_content: null,
        },
      });
    });
    expect(result.current.runs[0].steps.map((step) => step.seq)).toEqual([2]);
    expect(result.current.runs[0]).toMatchObject({
      status: "completed",
      steps_loaded: false,
    });
    await act(async () =>
      initial.resolve(
        eventPage([
          durable(1),
          durable(2, { step_id: 12, step: stepRow({ id: 12, seq: 2 }) }),
          durable(3, {
            kind: "run_finished",
            step_id: null,
            step: null,
            terminal: {
              conversation_id: 1,
              status: "completed",
              error: null,
              final_content: null,
            },
          }),
        ])
      )
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.runs[0].steps.map((step) => step.seq)).toEqual([
      1, 2,
    ]);
    expect(result.current.runs[0].steps.map((step) => step.step_id)).toEqual([
      11, 12,
    ]);
    expect(result.current.runs[0]).toMatchObject({
      status: "completed",
      pending_approval: null,
    });
    await act(async () => result.current.reload());
    expect(mockInvoke).toHaveBeenCalledWith("list_conversation_run_events", {
      conversationId: 1,
      afterEventId: 3,
      limit: 100,
    });
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "list_agent_steps")
    ).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("an older cursor page cannot replace a newer terminal snapshot", async () => {
    const stale = deferred<DurableRunEventPage>();
    historyRouter(() => stale.promise);
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    const fallback = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(
      (command: string, args?: Record<string, unknown>) =>
        command === "list_agent_runs"
          ? Promise.resolve([
              runRow({
                status: "error",
                interrupted: true,
                error: "shutdown",
                finished_at: 1002,
              }),
            ])
          : fallback(command, args)
    );
    let reload!: Promise<void>;
    act(() => {
      reload = result.current.reload();
    });
    await waitFor(() =>
      expect(result.current.runs[0].status).toBe("interrupted")
    );
    await act(async () => {
      stale.resolve(
        eventPage([
          durable(1, {
            step_id: null,
            step: null,
            kind: "run_finished",
            terminal: {
              conversation_id: 1,
              status: "completed",
              error: null,
              final_content: null,
            },
          }),
        ])
      );
      await reload;
    });
    expect(result.current.runs[0]).toMatchObject({
      status: "interrupted",
      error: "shutdown",
      finished_at: 1002,
    });
  });

  it("drops an old cursor completion across A -> B -> A", async () => {
    const stale = deferred<DurableRunEventPage>();
    let first = true;
    historyRouter(async (after) => {
      if (first) {
        first = false;
        return stale.promise;
      }
      return eventPage([], { next_cursor: after });
    });
    const { result, rerender } = renderHook(({ id }) => useAgentRun(id), {
      initialProps: { id: 1 },
    });
    await waitFor(() => expect(first).toBe(false));
    rerender({ id: 2 });
    rerender({ id: 1 });
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => stale.resolve(eventPage([durable(1)])));
    expect(result.current.runs[0].steps).toHaveLength(0);
    expect(result.current.error).toBeNull();
  });
  it("a newer legacy count reopens lazy loading for missed pre-journal steps", async () => {
    historyRouter(async (after) => eventPage([], { next_cursor: after }), []);
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() =>
      emitAgentEvent({
        type: "step",
        conversation_id: 1,
        run_id: 7,
        event: stepFrame(),
      })
    );
    expect(result.current.runs[0].steps_loaded).toBe(true);
    const fallback = mockInvoke.getMockImplementation()!;
    mockInvoke.mockImplementation(
      (command: string, args?: Record<string, unknown>) => {
        if (command === "list_agent_runs")
          return Promise.resolve([
            runRow({ status: "completed", total_steps: 2 }),
          ]);
        if (command === "list_agent_steps")
          return Promise.resolve([stepRow(), stepRow({ id: 12, seq: 2 })]);
        return fallback(command, args);
      }
    );
    await act(async () => result.current.reload());
    expect(result.current.runs[0].steps).toHaveLength(1);
    expect(result.current.runs[0].steps_loaded).toBe(false);
    await act(async () => result.current.loadRunSteps(7));
    expect(result.current.runs[0].steps.map((step) => step.step_id)).toEqual([
      11, 12,
    ]);
    expect(result.current.runs[0].steps[0].id).toBe(11);
  });

  it("a newer successful reload remains visible when an older snapshot resolves last", async () => {
    routeInvoke({ 1: [runRow()] }, {});
    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.loading).toBe(false));
    const old = deferred<AgentRun[]>(),
      fresh = deferred<AgentRun[]>();
    mockInvoke
      .mockImplementationOnce(() => old.promise)
      .mockImplementationOnce(() => fresh.promise);
    let a!: Promise<void>, b!: Promise<void>;
    act(() => {
      a = result.current.reload();
      b = result.current.reload();
    });
    await act(async () => {
      fresh.resolve([runRow({ model: "fresh" })]);
      await b;
    });
    await act(async () => {
      old.resolve([runRow({ model: "old" })]);
      await a;
    });
    expect(result.current.runs[0].model).toBe("fresh");
    expect(result.current.error).toBeNull();
    expect(result.current.loading).toBe(false);
  });
});
