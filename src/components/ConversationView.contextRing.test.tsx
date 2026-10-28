//! The context ring's refresh signature (D4) — the Chat-mode half.
//!
//! Round 5 derived the ring's `revision` from the run store only. In Agent
//! mode a run frame moved it, so the ring refreshed; in Chat mode nothing
//! ever did, so after a reply the ring stayed `n/a`/stale even though
//! `conversation_context_stats` now had token data. These pins are about the
//! signature, not the ring's paint: a persisted message must move it, a
//! re-render with the same history must not, and the original run-step
//! trigger must still work.
//!
//! The hooks are replaced wholesale so the test can hand the pane a new
//! message array without a backend. `conversationContextStats` is the one
//! Tauri command that is counted — it is the effect under test.

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AgentRunView,
  AgentStepView,
} from "../lib/useAgentRun";
import type { ConversationContextStats, Message } from "../lib/tauri";
import type { AutonomyStore } from "../lib/useAutonomy";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import ConversationView from "./ConversationView";

const { store, mockStats } = vi.hoisted(() => ({
  store: { messages: [] as Message[], runs: [] as AgentRunView[] },
  mockStats: vi.fn(),
}));

vi.mock("../lib/tauri", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/tauri")>();
  return { ...actual, conversationContextStats: mockStats };
});

vi.mock("../lib/useConversation", () => ({
  useConversation: () => ({
    messages: store.messages,
    loading: false,
    error: null,
    sending: false,
    reload: vi.fn(async () => {}),
    send: vi.fn(async () => null),
  }),
}));

vi.mock("../lib/useAttachments", () => ({
  useAttachments: () => ({
    attachments: [],
    error: null,
    busy: false,
    pickAndAttach: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
  }),
}));

vi.mock("../lib/useAgentRun", () => ({
  useAgentRun: () => ({
    runs: store.runs,
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    expandStep: vi.fn(),
    collapseStep: vi.fn(),
  }),
}));

function message(id: number, role: Message["role"]): Message {
  return {
    id,
    conversation_id: 1,
    role,
    content: `${role} ${id}`,
    provider_id: null,
    model_name: null,
    created_at: 1000 + id,
  };
}

function stats(known: boolean): ConversationContextStats {
  return {
    conversation_id: 1,
    title: "Alpha",
    provider: "openai",
    provider_display: "OpenAI",
    model: "gpt-5",
    context_limit: 200_000,
    total_tokens: known ? 12_000 : 0,
    input_tokens: known ? 9_000 : 0,
    output_tokens: known ? 3_000 : 0,
    reasoning_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    has_token_data: known,
    total_cost_micro_usd: 0,
    usage_percent: known ? 6 : 0,
    message_count: 2,
    user_message_count: 1,
    assistant_message_count: 1,
    tool_call_count: 0,
    other_step_count: 0,
    created_at: 1000,
    updated_at: 1000,
  };
}

const run: AgentRunView = {
  run_id: 7,
  conversation_id: 1,
  status: "completed",
  model: "test-model",
  started_at: 1000,
  finished_at: 1002,
  error: null,
  steps: [],
  pending_approval: null,
};

const step: AgentStepView = {
  id: 11,
  step_id: 11,
  run_id: 7,
  seq: 1,
  kind: "tool_call",
  tool_name: "read_file",
  arguments: "{}",
  observation: "out",
  observation_len: 3,
  observation_truncated: false,
  observation_full: null,
  observation_loading: false,
  observation_error: null,
  status: "succeeded",
  started_at: 1000,
  duration_ms: 5,
};

function autonomyStub(): AutonomyStore {
  return {
    mode: "semi_autonomous",
    loading: false,
    error: null,
    reload: vi.fn(async () => {}),
    setMode: vi.fn(async () => null),
  };
}

function pane() {
  return (
    <LocaleProvider>
      <ConversationView
        conversationId={1}
        selectedProvider="gemini"
        selectedModel="gemini-3.6-flash"
        models={["gemini-3.6-flash"]}
        onSelectModel={() => {}}
        modelError={null}
        autonomy={autonomyStub()}
        onOpenSettings={() => {}}
        draft=""
        setDraft={() => {}}
      />
    </LocaleProvider>
  );
}

beforeEach(() => {
  window.localStorage.setItem(LOCALE_KEY, "en");
  store.messages = [];
  store.runs = [];
  mockStats.mockReset();
  mockStats.mockImplementation(async () => stats(store.messages.length > 0));
});

afterEach(() => {
  cleanup();
  window.localStorage.removeItem(LOCALE_KEY);
  vi.restoreAllMocks();
});

describe("context ring — Chat-mode revision", () => {
  it("refetches when a Chat message is persisted", async () => {
    const view = render(pane());
    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(1));
    expect(
      await screen.findByRole("button", { name: "Context window: n/a" }),
    ).toBeInTheDocument();

    // The finding's scenario: Chat mode, no run ever starts, so only the
    // message store can move the revision.
    await act(async () => {
      fireEvent.click(screen.getByRole("radio", { name: /Chat mode/ }));
    });
    expect(mockStats).toHaveBeenCalledTimes(1);

    store.messages = [message(1, "user"), message(2, "assistant")];
    await act(async () => {
      view.rerender(pane());
    });

    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(2));
    expect(
      await screen.findByRole("button", { name: "Context window: 6% used" }),
    ).toBeInTheDocument();
  });

  it("does not refetch when a re-render carries the same history", async () => {
    const view = render(pane());
    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(1));

    // A NEW array with the same content: the signature is a string, so a
    // fresh reference alone must not schedule another fetch.
    store.messages = [];
    await act(async () => {
      view.rerender(pane());
      view.rerender(pane());
    });
    expect(mockStats).toHaveBeenCalledTimes(1);
  });

  it("still refetches when a run step arrives", async () => {
    // The original trigger must survive the extension to messages.
    const view = render(pane());
    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(1));

    store.runs = [run];
    await act(async () => {
      view.rerender(pane());
    });
    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(2));

    store.runs = [{ ...run, steps: [step] }];
    await act(async () => {
      view.rerender(pane());
    });
    await waitFor(() => expect(mockStats).toHaveBeenCalledTimes(3));
  });
});
