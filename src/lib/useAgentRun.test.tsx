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
} from "./tauri";
import { useAgentRun } from "./useAgentRun";

type TauriEvent = { payload: AgentRunEventPayload };

const { mockInvoke, mockListen, listeners, emitAgentEvent } = vi.hoisted(
  () => {
    const listeners: Array<(event: TauriEvent) => void> = [];
    const mockInvoke = vi.fn();
    const mockListen = vi.fn(
      async (
        _eventName: string,
        handler: (event: TauriEvent) => void,
      ): Promise<() => void> => {
        listeners.push(handler);
        return () => {
          const i = listeners.indexOf(handler);
          if (i >= 0) listeners.splice(i, 1);
        };
      },
    );
    const emitAgentEvent = (payload: AgentRunEventPayload): void => {
      for (const handler of [...listeners]) handler({ payload });
    };
    return { mockInvoke, mockListen, listeners, emitAgentEvent };
  },
);

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

function runRow(overrides: Partial<AgentRun> = {}): AgentRun {
  return {
    id: 7,
    conversation_id: 1,
    model: "test-model",
    mode: "supervised",
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

/** Default invoke router mirroring the `tauri.ts` wrapper contract. */
function routeInvoke(
  runsByConversation: Record<number, AgentRun[]>,
  stepsByRun: Record<number, AgentStep[]>,
  fullObservation = "FULL OBSERVATION TEXT",
): void {
  mockInvoke.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_agent_runs") {
      const id = args?.["conversationId"];
      if (typeof id !== "number" || !(id in runsByConversation)) {
        return Promise.reject(new Error(`no fixture for conversation ${String(id)}`));
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
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockClear();
  listeners.length = 0;
});

describe("useAgentRun", () => {
  it("rehydrates runs and steps on mount via the typed wrappers", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [stepRow()] });

    const { result } = renderHook(() => useAgentRun(1));

    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    expect(result.current.runs[0]).toMatchObject({
      run_id: 7,
      conversation_id: 1,
      status: "running",
    });
    expect(result.current.runs[0].steps).toHaveLength(1);
    expect(result.current.runs[0].steps[0]).toMatchObject({
      seq: 1,
      tool_name: "read_file",
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
    // Wrapper contract: snake_case command, camelCase args.
    expect(mockInvoke).toHaveBeenCalledWith("list_agent_runs", {
      conversationId: 1,
    });
    expect(mockInvoke).toHaveBeenCalledWith("list_agent_steps", { runId: 7 });
  });

  it("merges live step frames sorted by seq and ignores duplicate seq", async () => {
    routeInvoke({ 1: [runRow()] }, { 7: [] });

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs).toHaveLength(1));

    act(() => {
      emitAgentEvent({ type: "step", run_id: 7, event: stepFrame({ seq: 2 }) });
    });
    expect(result.current.runs[0].steps.map((s) => s.seq)).toEqual([2]);

    act(() => {
      emitAgentEvent({ type: "step", run_id: 7, event: stepFrame({ seq: 1 }) });
    });
    expect(result.current.runs[0].steps.map((s) => s.seq)).toEqual([1, 2]);

    act(() => {
      emitAgentEvent({ type: "step", run_id: 7, event: stepFrame({ seq: 1 }) });
    });
    expect(result.current.runs[0].steps).toHaveLength(2);
  });

  it("ignores finished frames from other conversations (no cross-conversation bleed)", async () => {
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

    // Own conversation's terminal frame: honored — triggers a reload that
    // reconciles with persisted truth (fixture status is "running" again).
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
    await waitFor(() => expect(reloadCalls()).toBeGreaterThan(baseline));
    expect(result.current.runs).toHaveLength(1);
    expect(result.current.runs[0].run_id).toBe(7);
  });

  it("drops a stale reload that resolves after a conversation switch", async () => {
    let resolveConv1: ((runs: AgentRun[]) => void) | null = null;
    mockInvoke.mockImplementation((cmd: string, args?: Record<string, unknown>) => {
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
    });

    const { result, rerender } = renderHook(
      ({ id }: { id: number | null }) => useAgentRun(id),
      { initialProps: { id: 1 as number | null } },
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
      "FULL OBSERVATION TEXT",
    );

    const { result } = renderHook(() => useAgentRun(1));
    await waitFor(() => expect(result.current.runs[0]?.steps).toHaveLength(1));
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();

    await act(async () => {
      await result.current.expandStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_full).toBe(
      "FULL OBSERVATION TEXT",
    );
    expect(mockInvoke).toHaveBeenCalledWith("get_agent_step_observation", {
      stepId: 11,
    });

    act(() => {
      result.current.collapseStep(7, 1);
    });
    expect(result.current.runs[0].steps[0].observation_full).toBeNull();
  });
});
