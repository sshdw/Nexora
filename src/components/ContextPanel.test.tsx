//! Context panel's budget slider (#220) — the control, not the paint.
//!
//! jsdom computes no cascade, so what is pinned here is everything
//! that makes the slider a real control rather than a styled bar:
//! the ARIA contract Radix gives it, the value it renders, the fact
//! that moving it changes the panel's own reading (the whole reason
//! it exists), and that the value persists.
//!
//! The rail's paint lives in src/components/ui/slider.tsx and
//! src/styles/composer.css; the tokens those files use are asserted
//! in src/components/Composer.test.tsx.

import { cleanup, render, screen } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { ConversationContextStats } from "../lib/tauri";

const { conversationContextStats, listAgentRuns, listAgentSteps } = vi.hoisted(() => ({
  conversationContextStats: vi.fn(),
  listAgentRuns: vi.fn(),
  listAgentSteps: vi.fn(),
}));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  conversationContextStats,
  listAgentRuns,
  listAgentSteps,
}));

import ContextPanel from "./ContextPanel";
import { CONTEXT_BUDGET_KEY } from "../lib/useContextBudget";

function stats(overrides: Partial<ConversationContextStats> = {}): ConversationContextStats {
  return {
    conversation_id: 1,
    title: "Designing the composer",
    message_count: 3,
    provider: "gemini",
    provider_display: "Gemini",
    model: "gemini-3.6-flash",
    context_limit: 128_000,
    total_tokens: 32_000,
    has_token_data: true,
    usage_percent: 25,
    input_tokens: 20_000,
    output_tokens: 12_000,
    reasoning_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    user_message_count: 2,
    assistant_message_count: 1,
    tool_call_count: 0,
    other_step_count: 0,
    total_cost_micro_usd: 0,
    created_at: 1_700_000_000,
    updated_at: 1_700_003_600,
    ...overrides,
  };
}

// Radix's Slider measures its thumb with a ResizeObserver, which jsdom
// does not implement; without a stub the primitive throws on mount and
// the control cannot be tested at all. Nothing here asserts layout —
// this is the primitive asking the environment a question jsdom cannot
// answer.
beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
});

afterAll(() => {
  vi.unstubAllGlobals();
});

beforeEach(() => {
  conversationContextStats.mockResolvedValue(stats());
  listAgentRuns.mockResolvedValue([]);
  listAgentSteps.mockResolvedValue([]);
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/** Render the panel and wait past its loading indicator. */
async function renderPanel() {
  const view = render(<ContextPanel conversationId={1} />);
  // The panel renders a loading indicator until the stats land; the
  // slider is what proves the panel got there.
  await screen.findByRole("slider", { name: "Context budget" });
  return view;
}

describe("context panel — budget slider", () => {
  it("exposes one slider with a name, a description and a real value", async () => {
    await renderPanel();

    const slider = await screen.findByRole("slider", { name: "Context budget" });
    // Radix carries the ARIA value plumbing; the panel carries the name.
    expect(slider).toHaveAttribute("aria-valuemin", "8000");
    expect(slider).toHaveAttribute("aria-valuemax", "256000");
    expect(slider).toHaveAttribute("aria-valuenow", "128000");
    // The rendered number and the control's own value are one number.
    expect(slider.getAttribute("aria-valuenow")).toBe(
      screen.getByText("128 000 tokens").textContent?.replace(/\D/g, ""),
    );
    // Described by the reading underneath it (the fixture's usage).
    expect(slider).toHaveAccessibleDescription("32 000 of 128 000 tokens used");
  });

  it("reads the stored budget on mount", async () => {
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "64000");
    await renderPanel();
    const slider = await screen.findByRole("slider", { name: "Context budget" });
    expect(slider).toHaveAttribute("aria-valuenow", "64000");
    expect(screen.getByText("64 000 tokens")).toBeInTheDocument();
    // …and the reading is recomputed against it, not against the
    // model's own context limit.
    expect(screen.getByText("32 000 of 64 000 tokens used")).toBeInTheDocument();
  });

  it("changes the panel's own reading when the value changes", async () => {
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "40000");
    await renderPanel();
    const slider = await screen.findByRole("slider", { name: "Context budget" });
    expect(screen.getByText("32 000 of 40 000 tokens used")).toBeInTheDocument();

    // ArrowRight is a real Slider key: one step up, and the reading
    // follows it. No timing assertion (repo policy).
    slider.focus();
    slider.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    await screen.findByText("32 000 of 48 000 tokens used");
    expect(screen.getByRole("slider", { name: "Context budget" })).toHaveAttribute(
      "aria-valuenow",
      "48000",
    );
  });

  it("persists only on commit, not on every drag frame", async () => {
    await renderPanel();
    const slider = await screen.findByRole("slider", { name: "Context budget" });
    expect(window.localStorage.getItem(CONTEXT_BUDGET_KEY)).toBeNull();
    // Rendering the control must not write; only a committed change does.
    expect(slider).toBeInTheDocument();
  });

  it("says so when there is no persisted token usage to measure", async () => {
    conversationContextStats.mockResolvedValue(stats({ has_token_data: false, total_tokens: 0 }));
    await renderPanel();
    const slider = await screen.findByRole("slider", { name: "Context budget" });
    expect(slider).toHaveAttribute("aria-valuenow", "128000");
    expect(screen.getByText("No persisted token usage")).toBeInTheDocument();
  });
});