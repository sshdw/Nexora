//! Context-window ring + popover (D4 / R6-05).
//!
//! The one hard rule: `has_token_data: false` is a REAL state, not zero. The
//! ring shows a solid boundary track and every number reads `n/a`; it never
//! estimates tokens, cache hit rate or quota. These pins cover that state
//! (including a failed fetch, which degrades to the same honest n/a), the
//! value state's numbers, the panel the popover now carries, and the compact
//! formatter the popover depends on.

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import type { ConversationContextStats } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import ContextRing, { formatCompactTokens } from "./ContextRing";

const { mockStats, listAgentRuns, listAgentSteps, getAgentStepObservation } =
  vi.hoisted(() => ({
    mockStats: vi.fn(),
    listAgentRuns: vi.fn(),
    listAgentSteps: vi.fn(),
    getAgentStepObservation: vi.fn(),
  }));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  conversationContextStats: mockStats,
  listAgentRuns,
  listAgentSteps,
  getAgentStepObservation,
}));

afterEach(cleanup);

// Radix's Slider (inside the popover's context panel) measures its thumb
// with a ResizeObserver, which jsdom does not implement.
beforeAll(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    }
  );
});

afterAll(() => {
  vi.unstubAllGlobals();
});

beforeEach(() => {
  mockStats.mockReset();
  listAgentRuns.mockReset();
  listAgentSteps.mockReset();
  getAgentStepObservation.mockReset();
  listAgentRuns.mockResolvedValue([]);
  listAgentSteps.mockResolvedValue([]);
  getAgentStepObservation.mockResolvedValue("");
  window.localStorage.clear();
});

function stats(
  overrides: Partial<ConversationContextStats> = {}
): ConversationContextStats {
  return {
    conversation_id: 7,
    title: "Alpha",
    provider: "openai",
    provider_display: "OpenAI",
    model: "gpt-5",
    context_limit: 200_000,
    total_tokens: 12_000,
    input_tokens: 9_000,
    output_tokens: 3_000,
    reasoning_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    has_token_data: true,
    total_cost_micro_usd: 0,
    usage_percent: 6,
    message_count: 4,
    user_message_count: 2,
    assistant_message_count: 2,
    tool_call_count: 0,
    other_step_count: 0,
    created_at: 1000,
    updated_at: 1000,
    ...overrides,
  };
}

function renderRing() {
  return render(
    <LocaleProvider>
      <ContextRing conversationId={7} revision="0" />
    </LocaleProvider>
  );
}

describe("context ring — no token data", () => {
  it("reads n/a and draws a solid empty track, never a zero percent", async () => {
    mockStats.mockResolvedValue(
      stats({ has_token_data: false, usage_percent: 0, total_tokens: 0 })
    );
    const { container } = renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: n/a",
    });
    expect(trigger).toHaveAttribute("data-context", "na");

    const svg = container.querySelector("svg[data-state='na']");
    expect(svg).not.toBeNull();
    const track = svg?.querySelector("circle");
    // The track is the control's boundary: a TOKEN, never a literal, and a
    // token that clears 3:1 on the card in every theme.
    expect(track).toHaveAttribute("stroke", "var(--muted-foreground)");
    expect(track?.getAttribute("stroke")).toMatch(/^var\(--[a-z-]+\)$/);
    // R6-09: a dashed track read as a loading spinner. The empty state is a
    // present-but-empty ring, and the absent value arc is what says so.
    expect(track).not.toHaveAttribute("stroke-dasharray");
    expect(svg?.querySelectorAll("circle")).toHaveLength(1);
  });

  it("degrades to the same n/a when the fetch fails", async () => {
    mockStats.mockRejectedValue(new Error("backend offline"));
    renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: n/a",
    });
    expect(trigger).toHaveAttribute("data-context", "na");
  });

  it("keeps the numbers honest in the popover", async () => {
    mockStats.mockResolvedValue(
      stats({ has_token_data: false, total_tokens: 0 })
    );
    renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: n/a",
    });
    fireEvent.click(trigger);
    // The usage line and the panel's own reading both say n/a; neither
    // invents a zero.
    expect(screen.getAllByText("n/a").length).toBeGreaterThan(0);
    expect(screen.queryByText("0%")).toBeNull();
    expect(screen.queryByText("0.0%")).toBeNull();
    expect(
      screen.queryByRole("region", { name: "Context breakdown" })
    ).toBeNull();
    expect(
      screen.getByRole("slider", { name: "Context budget" })
    ).toBeInTheDocument();
  });
});

describe("context ring — value", () => {
  it("names the button with the real percentage and paints the arc", async () => {
    mockStats.mockResolvedValue(stats());
    const { container } = renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: 6% used",
    });
    expect(trigger).toHaveAttribute("data-context", "value");

    const svg = container.querySelector("svg[data-state='value']");
    expect(svg?.querySelectorAll("circle")).toHaveLength(2);
    const arc = svg?.querySelectorAll("circle")[1];
    expect(arc).toHaveAttribute("stroke", "currentColor");
    expect(arc?.getAttribute("stroke-dasharray")).toBeTruthy();
  });

  it("shows compact totals, the provider and the model in the popover", async () => {
    mockStats.mockResolvedValue(stats());
    renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: 6% used",
    });
    fireEvent.click(trigger);

    expect(screen.getByText("12K / 200K (6%)")).toBeInTheDocument();
    expect(screen.getByText("Provider")).toBeInTheDocument();
    expect(screen.getByText("OpenAI")).toBeInTheDocument();
    expect(screen.getByText("Model")).toBeInTheDocument();
    expect(screen.getByText("gpt-5")).toBeInTheDocument();
  });

  it("falls back to the provider id when there is no display name", async () => {
    mockStats.mockResolvedValue(
      stats({ provider_display: null, provider: "openai" })
    );
    renderRing();
    fireEvent.click(
      await screen.findByRole("button", { name: "Context window: 6% used" })
    );
    expect(screen.getByText("openai")).toBeInTheDocument();
  });
});

describe("context ring — the panel it carries (R6-05)", () => {
  it("fetches the stats once, however often the popover opens", async () => {
    mockStats.mockResolvedValue(stats());
    renderRing();
    const trigger = await screen.findByRole("button", {
      name: "Context window: 6% used",
    });
    expect(mockStats).toHaveBeenCalledTimes(1);
    fireEvent.click(trigger);
    // The body receives the stats as a prop; it never asks again.
    expect(mockStats).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("slider", { name: "Context budget" })
    ).toBeInTheDocument();
  });

  it("reads runs only while the popover is open", async () => {
    mockStats.mockResolvedValue(stats());
    renderRing();
    await screen.findByRole("button", { name: "Context window: 6% used" });
    // Closed content is unmounted by Radix, so a reader who never opens the
    // ring never pays for runs → steps → diffs.
    expect(listAgentRuns).not.toHaveBeenCalled();

    fireEvent.click(
      screen.getByRole("button", { name: "Context window: 6% used" })
    );
    await waitFor(() => expect(listAgentRuns).toHaveBeenCalledWith(7));
  });

  it("carries the context budget control into the popover", async () => {
    mockStats.mockResolvedValue(stats());
    renderRing();
    fireEvent.click(
      await screen.findByRole("button", { name: "Context window: 6% used" })
    );
    expect(
      screen.getByRole("slider", { name: "Context budget" })
    ).toBeInTheDocument();
    expect(screen.getByText("Context budget")).toBeInTheDocument();
  });
});

describe("formatCompactTokens", () => {
  it("keeps sub-thousand values whole", () => {
    expect(formatCompactTokens(0)).toBe("0");
    expect(formatCompactTokens(999)).toBe("999");
    expect(formatCompactTokens(999.4)).toBe("999");
  });

  it("compacts thousands and millions, trimming a trailing .0", () => {
    expect(formatCompactTokens(1_000)).toBe("1K");
    expect(formatCompactTokens(1_500)).toBe("1.5K");
    expect(formatCompactTokens(12_000)).toBe("12K");
    expect(formatCompactTokens(1_000_000)).toBe("1M");
    expect(formatCompactTokens(1_250_000)).toBe("1.3M");
  });

  it("never invents a number for a non-finite value", () => {
    expect(formatCompactTokens(Number.NaN)).toBe("n/a");
    expect(formatCompactTokens(Number.POSITIVE_INFINITY)).toBe("n/a");
  });
});
