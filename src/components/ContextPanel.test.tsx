//! Context popover body (R6-05) — the control, not the paint.
//!
//! The body used to be the stage's second view; it is the context ring's
//! popover now, and it receives the stats the ring already fetched. These
//! pins cover the budget slider's ARIA contract and reading, the hairline,
//! the changed-files section, and the one-fetch rule: the panel must never
//! ask `conversation_context_stats` a second time.
//!
//! jsdom computes no cascade, so the paint lives in src/styles/composer.css
//! and src/components/ui/slider.tsx; the tokens those files use are asserted
//! in src/components/Composer.test.tsx.

import { cleanup, render, screen } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { ConversationContextStats } from "../lib/tauri";

const { conversationContextStats, listAgentRuns, listAgentSteps, getAgentStepObservation } =
  vi.hoisted(() => ({
    conversationContextStats: vi.fn(),
    listAgentRuns: vi.fn(),
    listAgentSteps: vi.fn(),
    getAgentStepObservation: vi.fn(),
  }));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  conversationContextStats,
  listAgentRuns,
  listAgentSteps,
  getAgentStepObservation,
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
  listAgentRuns.mockResolvedValue([]);
  listAgentSteps.mockResolvedValue([]);
  getAgentStepObservation.mockResolvedValue("");
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/** Render the body with the ring's already-fetched stats. */
async function renderPanel(overrides: Partial<ConversationContextStats> = {}) {
  const view = render(<ContextPanel conversationId={1} stats={stats(overrides)} />);
  // The changed-files section settles asynchronously; the slider is what
  // proves the body rendered.
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
    // Described by the hint: it is the control's DESCRIPTION (what
    // the number is for), while the live reading is the value the
    // control already announces through aria-valuenow. Both were
    // declared in round 1 and the hint was the one never rendered.
    expect(slider).toHaveAccessibleDescription(
      "Tokens this conversation is measured against. Above it the budget reads as over.",
    );
    expect(
      screen.getByText("32 000 of 128 000 tokens used"),
    ).toBeInTheDocument();
  });

  // #230 P3 (MAP row 20): the budget HAIRLINE + cost-on-hover.
  it("draws the budget as a hairline whose fill IS the reading, and never a guess", async () => {
    await renderPanel();
    // `role="img"` with both numbers in its accessible name: the
    // proportion is never visual-only, and the numbers are the SAME
    // numbers the readout below prints.
    const meter = screen.getByRole("img", {
      name: "32 000 of 128 000 budgeted tokens used. Total cost 0.00 $",
    });
    const fill = meter.querySelector(".nex-context-meter-fill");
    expect(fill).not.toBeNull();
    // 32 000 of 128 000 = 25%, computed from the persisted figures — no
    // animation, no timer, no estimate.
    expect((fill as HTMLElement).style.width).toBe("25%");
  });

  it("pins the hairline at full and says over, rather than drawing past its track", async () => {
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "16000");
    await renderPanel();
    // 32 000 used against a 16 000 budget is 200% — the fill clamps to
    // the track, and the READING is what carries the over-budget verdict.
    const meter = screen.getByRole("img", { name: /budgeted tokens used/ });
    const fill = meter.querySelector(".nex-context-meter-fill") as HTMLElement;
    expect(fill.style.width).toBe("100%");
    expect(meter.className).toContain("is-over");
    expect(screen.getByText("Over budget — past 16 000 tokens.")).toBeInTheDocument();
  });

  it("leaves the hairline empty when there is no usage to measure", async () => {
    await renderPanel({ has_token_data: false, total_tokens: 0 });
    // An empty track, not a zero-width fill: the two say different
    // things ("nothing measured" vs "measured, and it is nothing"). The
    // fill stays mounted at zero width — the component's computed width is
    // the single source of its size, and the state is on the track.
    // The name still carries the cost sentence — the detail does not
    // disappear just because there is nothing to draw.
    const meter = screen.getByRole("img", { name: /No persisted token usage/ });
    expect(meter.className).toContain("is-empty");
    const fill = meter.querySelector(".nex-context-meter-fill") as HTMLElement;
    expect(fill).not.toBeNull();
    expect(fill.style.width).toBe("0%");
  });

  it("puts cost on the reading and in the meter, never hover-only", async () => {
    await renderPanel({ total_cost_micro_usd: 1_234_567 });
    // The mouse affordance…
    expect(screen.getByText("32 000 of 128 000 tokens used")).toHaveAttribute(
      "title",
      "Total cost 1.23 $",
    );
    // …and the same sentence where a keyboard or screen-reader user gets
    // it, so the detail is never hover-ONLY (SYSTEM §8).
    expect(
      screen.getByRole("img", { name: /Total cost 1\.23 \$/ }),
    ).toBeInTheDocument();
    // And it is NOT a sixteenth permanent row among the stats.
    expect(screen.queryByText("Total cost")).toBeNull();
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

  it("says the budget is exceeded, and says it as a status", async () => {
    // `ctx.budgetHint` promises the reader that going over is
    // visible. That promise is now kept: the reading swaps to the
    // exceeded copy, takes the destructive tone, and is announced.
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "16000");
    await renderPanel();

    const reading = screen.getByText("Over budget — past 16 000 tokens.");
    expect(reading).toBeInTheDocument();
    expect(reading).toHaveAttribute("role", "status");
    // #230 P3: the ONE line this phase rewrites. `is-over` is the
    // destructive tone both the reading and the new hairline take, and
    // both are asserted here — the reading by class, the hairline by the
    // same token below. The promise `ctx.budgetHint` makes ("Above it the
    // budget reads as over") now has two surfaces keeping it, not one.
    expect(reading).toHaveClass("is-over");
    // The ordinary reading is gone, not appended to.
    expect(screen.queryByText(/of 16 000 tokens used/)).not.toBeInTheDocument();
  });

  it("treats landing exactly on the budget as using it all, not over", async () => {
    // `>`, not `>=`: 32 000 used against a 32 000 budget is exactly
    // on the line, and calling that "over" would make the warning
    // fire a whole step early.
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "32000");
    await renderPanel();
    expect(screen.getByText("32 000 of 32 000 tokens used")).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("gives no verdict when there is no usage to measure", async () => {
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, "8000");
    await renderPanel({ has_token_data: false, total_tokens: 0 });
    expect(screen.getByText("No persisted token usage")).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("says so when there is no persisted token usage to measure", async () => {
    await renderPanel({ has_token_data: false, total_tokens: 0 });
    const slider = await screen.findByRole("slider", { name: "Context budget" });
    expect(slider).toHaveAttribute("aria-valuenow", "128000");
    expect(screen.getByText("No persisted token usage")).toBeInTheDocument();
  });
});

describe("context popover — the ring's stats, not a second fetch", () => {
  it("says the stats are unavailable when the ring's fetch failed", () => {
    render(<ContextPanel conversationId={1} stats={null} />);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Context stats are unavailable.",
    );
    // No control without data: the budget slider is not a placeholder.
    expect(screen.queryByRole("slider", { name: "Context budget" })).toBeNull();
  });

  it("never asks the backend for the stats the ring already fetched", async () => {
    await renderPanel();
    expect(conversationContextStats).not.toHaveBeenCalled();
  });

  it("carries every reading the old Context view showed", async () => {
    await renderPanel();
    for (const value of [
      "Designing the composer",
      "Gemini",
      "gemini-3.6-flash",
      "128 000",
      "25%",
      "32 000",
      "20 000",
      "12 000",
      "3",
    ]) {
      expect(screen.getAllByText(value).length).toBeGreaterThan(0);
    }
  });
});

describe("context popover — changed files", () => {
  function run(id: number): import("../lib/tauri").AgentRun {
    return {
      id,
      conversation_id: 1,
      model: "gemini-3.6-flash",
      mode: "agent",
      status: "completed",
      started_at: 1_700_000_000,
      finished_at: 1_700_000_100,
      total_steps: 2,
      final_content: null,
      error: null,
      spent_micro_usd: null,
      limit_micro_usd: null,
    };
  }

  function step(overrides: Partial<import("../lib/tauri").AgentStep> = {}) {
    return {
      id: 1,
      run_id: 1,
      seq: 1,
      kind: "tool_call",
      tool_name: "write_file",
      arguments: JSON.stringify({ path: "src/App.tsx" }),
      observation: "@@ -1 +1 @@\n-old\n+new",
      observation_len: 24,
      observation_truncated: false,
      status: "completed",
      started_at: 1_700_000_010,
      duration_ms: 5,
      rule_id: null,
      group_key: null,
      decided_by: null,
      input_tokens: null,
      output_tokens: null,
      ...overrides,
    };
  }

  it("lists the winning write per path with its diff", async () => {
    listAgentRuns.mockResolvedValue([run(1)]);
    listAgentSteps.mockResolvedValue([
      step({ id: 1, seq: 1 }),
      // A later write to the same path wins — one row per file, not per step.
      step({ id: 2, seq: 4, observation: "@@ -1 +1 @@\n-old\n+newer" }),
    ]);
    getAgentStepObservation.mockResolvedValue("@@ -1 +1 @@\n-old\n+newer");

    await renderPanel();
    await screen.findByText("src/App.tsx");
    expect(screen.getAllByText("src/App.tsx")).toHaveLength(1);
    expect(getAgentStepObservation).toHaveBeenCalledWith(2);
    expect(getAgentStepObservation).toHaveBeenCalledTimes(1);
  });

  it("says there are no recorded changes instead of drawing an empty list", async () => {
    listAgentRuns.mockResolvedValue([]);
    await renderPanel();
    expect(await screen.findByText("No file changes recorded.")).toBeInTheDocument();
  });

  it("keeps the preview when the full diff cannot be fetched", async () => {
    listAgentRuns.mockResolvedValue([run(1)]);
    listAgentSteps.mockResolvedValue([step()]);
    getAgentStepObservation.mockRejectedValue(new Error("gone"));

    await renderPanel();
    expect(await screen.findByText("src/App.tsx")).toBeInTheDocument();
  });

  it("surfaces a failed run read in words", async () => {
    listAgentRuns.mockRejectedValue(new Error("database is locked"));
    await renderPanel();
    expect(await screen.findByRole("alert")).toHaveTextContent("database is locked");
  });
});