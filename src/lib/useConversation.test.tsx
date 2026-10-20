//! Hook tests for `useConversation` (issue #267).
//!
//! Mocks mirror the real wrapper shapes in `src/lib/tauri.ts` exactly:
//! `conversation_history` takes camelCase args (`conversationId`), and
//! `listen` delivers Tauri `Event<T>` objects whose payload is read from
//! `.payload`. A run's terminal `finished` frame must re-read the active
//! conversation's persisted history without a conversation switch.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentRunEventPayload, Message } from "./tauri";
import { useConversation } from "./useConversation";

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

function messageRow(overrides: Partial<Message> = {}): Message {
  return {
    id: 1,
    conversation_id: 1,
    role: "user",
    content: "hello",
    provider_id: null,
    model_name: null,
    created_at: 1000,
    ...overrides,
  };
}

/** Serve one history fixture per `conversation_history` call, repeating the
 * last one for any extra call. */
function routeHistory(histories: Message[][]): void {
  let call = 0;
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === "conversation_history") {
      const fixture = histories[Math.min(call, histories.length - 1)];
      call += 1;
      return Promise.resolve(fixture);
    }
    return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
  });
}

function historyCalls(): number {
  return mockInvoke.mock.calls.filter((c) => c[0] === "conversation_history")
    .length;
}

function finishedFrame(conversationId: number): AgentRunEventPayload {
  return {
    type: "finished",
    run_id: 7,
    event: {
      conversation_id: conversationId,
      status: "completed",
      final_content: "done",
      error: null,
    },
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockClear();
  listeners.length = 0;
});

describe("useConversation", () => {
  it("subscribes to agent-run-event and re-reads history on the active conversation's finished frame", async () => {
    const user = messageRow();
    const assistant = messageRow({
      id: 2,
      role: "assistant",
      content: "final answer",
      created_at: 1001,
    });
    // The backend persists the final assistant message before the terminal
    // frame, so the second read carries it.
    routeHistory([[user], [user, assistant]]);

    const { result } = renderHook(() => useConversation(1));
    await waitFor(() => expect(result.current.messages).toHaveLength(1));
    expect(historyCalls()).toBe(1);

    act(() => {
      emitAgentEvent(finishedFrame(1));
    });

    await waitFor(() => expect(result.current.messages).toHaveLength(2));
    expect(result.current.messages.map((m) => m.id)).toEqual([1, 2]);
    expect(result.current.messages[1].content).toBe("final answer");
    expect(historyCalls()).toBe(2);
    // Channel contract: production subscribes to "agent-run-event".
    expect(mockListen).toHaveBeenCalledWith(
      "agent-run-event",
      expect.any(Function),
    );
  });

  it("replaces the live list with persisted history instead of appending", async () => {
    const user = messageRow();
    const assistant = messageRow({
      id: 2,
      role: "assistant",
      content: "final answer",
      created_at: 1001,
    });
    // The prior read already holds the user message; the reload returns the
    // same user row plus the assistant row. Appending would yield 3 rows.
    routeHistory([[user], [user, assistant]]);

    const { result } = renderHook(() => useConversation(1));
    await waitFor(() => expect(result.current.messages).toHaveLength(1));

    act(() => {
      emitAgentEvent(finishedFrame(1));
    });

    await waitFor(() => expect(result.current.messages).toHaveLength(2));
    expect(result.current.messages).toEqual([user, assistant]);
  });

  it("refreshes in place on the terminal frame: no loading flash, messages stay rendered", async () => {
    const user = messageRow();
    const assistant = messageRow({
      id: 2,
      role: "assistant",
      content: "final answer",
      created_at: 1001,
    });
    let resolveRefresh: ((value: Message[]) => void) | null = null;
    let call = 0;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd !== "conversation_history") {
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
      }
      call += 1;
      if (call === 1) return Promise.resolve([user]);
      // Deterministic deferred refresh — no timers, no sleeps.
      return new Promise<Message[]>((resolve) => {
        resolveRefresh = resolve;
      });
    });

    const { result } = renderHook(() => useConversation(1));
    await waitFor(() => expect(result.current.messages).toHaveLength(1));

    await act(async () => {
      emitAgentEvent(finishedFrame(1));
      await Promise.resolve();
    });

    // In flight: the populated thread is untouched and no spinner is shown.
    expect(result.current.loading).toBe(false);
    expect(result.current.messages.map((m) => m.id)).toEqual([1]);

    await act(async () => {
      resolveRefresh?.([user, assistant]);
    });
    expect(result.current.messages.map((m) => m.id)).toEqual([1, 2]);
    expect(result.current.loading).toBe(false);
  });

  it("ignores finished frames for a different conversation", async () => {
    routeHistory([[messageRow()]]);

    const { result } = renderHook(() => useConversation(1));
    await waitFor(() => expect(result.current.messages).toHaveLength(1));
    const baseline = historyCalls();

    await act(async () => {
      emitAgentEvent(finishedFrame(2));
      await Promise.resolve();
    });

    expect(historyCalls()).toBe(baseline);
    expect(result.current.messages.map((m) => m.id)).toEqual([1]);
  });
});
