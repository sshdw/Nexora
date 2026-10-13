//! Agent Activity Spine + run-state visuals (#153).
//!
//! Behavior pins only — no timing assertions, no sleeps (repo policy).
//! Fixtures reuse the row shapes built in `useAgentRun.test.tsx:48-103`
//! (`AgentRun` / `AgentStep` wire rows) mapped onto the render views this
//! component consumes (`AgentRunView` / `AgentStepView`, the shapes the live
//! hook emits), so a drift in either vocabulary shows up here.
//!
//! jsdom ships no layout and computes no cascade, so these tests pin the two
//! things jsdom CAN prove, and leave the rest to the stylesheet:
//!   1. WHICH elements exist — a rail for every run, a connector per row, and
//!      a pulse element present only for an executing run. Absence (not
//!      `opacity: 0`) is the structural guarantee.
//!   2. WHICH state each run renders — the run-status class, the per-row
//!      fade bucket, the fold gate, and the fold summary in both locales.
//! The pulse's animation, its CSS gating and its reduced-motion fallback
//! live in conversation.css + the global motion.css gate.

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";
import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import AgentRunSteps, { type AgentRunStepsProps } from "./AgentRunSteps";

/** `AgentStep` row shape → `AgentStepView` (mirrors the hook's `toStepView`). */
function stepView(overrides: Partial<AgentStepView> = {}): AgentStepView {
  return {
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
    ...overrides,
  };
}

/** `AgentRun` row shape → `AgentRunView`. */
function runView(overrides: Partial<AgentRunView> = {}): AgentRunView {
  return {
    run_id: 7,
    conversation_id: 1,
    status: "running",
    model: "test-model",
    started_at: 1000,
    finished_at: null,
    error: null,
    steps: [],
    pending_approval: null,
    ...overrides,
  };
}

/** Five steps out of `seq` order: 2 file reads, 1 command, 1 search and a
 * model turn (4 tool calls total). */
function mixedSteps(): AgentStepView[] {
  return [
    stepView({ id: 11, seq: 3, tool_name: "execute_command", arguments: '{"command":"ls"}' }),
    stepView({ id: 12, seq: 1, kind: "model_turn", tool_name: null, observation: "thinking" }),
    stepView({ id: 13, seq: 5, tool_name: "search_files", arguments: '{"query":"refreshToken"}' }),
    stepView({ id: 14, seq: 2 }),
    stepView({ id: 15, seq: 4 }),
  ];
}

function noopProps(): Omit<AgentRunStepsProps, "run"> {
  return {
    onResolveApproval: vi.fn(),
    onCancel: vi.fn(),
    onContinue: vi.fn(),
    onPause: vi.fn(),
    onResume: vi.fn(),
    onExpandStep: vi.fn(),
    onCollapseStep: vi.fn(),
  };
}

function renderRun(run: AgentRunView, props: Partial<Omit<AgentRunStepsProps, "run">> = {}) {
  const base: Omit<AgentRunStepsProps, "run"> = { ...noopProps(), ...props };
  const view = render(
    <LocaleProvider>
      <AgentRunSteps {...base} run={run} />
    </LocaleProvider>,
  );
  return {
    ...view,
    rerenderRun: (next: AgentRunView, nextProps: Partial<Omit<AgentRunStepsProps, "run">> = {}) =>
      view.rerender(
        <LocaleProvider>
          <AgentRunSteps {...base} {...nextProps} run={next} />
        </LocaleProvider>,
      ),
  };
}

const rail = (c: HTMLElement): Element | null => c.querySelector(".nex-agent-spine");
const pulse = (c: HTMLElement): Element | null => c.querySelector(".nex-agent-spine-pulse");
const connectors = (c: HTMLElement): Element[] => [...c.querySelectorAll(".nex-agent-step-slot")];
const rows = (c: HTMLElement): Element[] => [...c.querySelectorAll(".nex-agent-step")];
const seqOf = (row: Element): string | null | undefined => row.querySelector(".nex-agent-step-seq")?.textContent;

afterEach(cleanup);

describe("Agent Activity Spine", () => {
  it("mounts one rail and one connector per row, with the pulse only while executing", () => {
    const run = runView({ steps: mixedSteps() });
    const { container, rerenderRun } = renderRun(run);

    expect(rail(container)).not.toBeNull();
    expect(pulse(container)).not.toBeNull();
    // The class the pulse animation is keyed on.
    expect(container.querySelector(".nex-agent-run")?.className).toContain("nex-agent-run-state-running");
    // One connector segment per row — never a per-row border rebuilding a rail.
    expect(connectors(container)).toHaveLength(5);
    // The current row is the last in seq order and carries the accent segment.
    expect(connectors(container)[connectors(container).length - 1].className).toContain(
      "nex-agent-step-slot-current",
    );
    expect(connectors(container)[0].className).not.toContain("nex-agent-step-slot-current");

    // Nothing else may animate: not idle/queued, not paused, not a terminal.
    // The class is the CSS key, so pinning it pins "no pulse" downstream.
    const stateClass: Record<string, string> = {
      queued: "nex-agent-run-state-quiet",
      paused: "nex-agent-run-state-paused",
      completed: "nex-agent-run-state-completed",
      cancelled: "nex-agent-run-state-cancelled",
      budget_exhausted: "nex-agent-run-state-budget",
      spend_limit_exceeded: "nex-agent-run-state-spend",
      error: "nex-agent-run-state-error",
    };
    for (const [status, expectedClass] of Object.entries(stateClass)) {
      rerenderRun(runView({ status, steps: mixedSteps() }));
      expect(pulse(container), `status ${status} must not pulse`).toBeNull();
      expect(container.querySelector(".nex-agent-run")?.className, `class for ${status}`).toContain(
        expectedClass,
      );
      // Same information without motion: the rail frame and its segments stay.
      expect(rail(container), `status ${status} must keep the rail`).not.toBeNull();
      expect(connectors(container), `status ${status} must keep its connectors`).toHaveLength(5);
    }

    // …and running brings the pulse back.
    rerenderRun(run);
    expect(pulse(container)).not.toBeNull();
  });

  it("keeps the rail and the read on a run parked on an approval, but no pulse", () => {
    const { container } = renderRun(
      runView({
        // A parked run persists as `running` (lifecycle.rs: `AwaitingApproval`
        // → `running`), so the pending approval — not the status string — is
        // what says the run is waiting on a human instead of working.
        status: "running",
        steps: mixedSteps(),
        pending_approval: {
          call_id: "call-1",
          name: "execute_command",
          arguments: '{"command":"rm -rf build"}',
          timeout_secs: 0,
          requested_at: 1000,
        },
      }),
    );

    expect(rail(container)).not.toBeNull();
    expect(connectors(container)).toHaveLength(5);
    expect(pulse(container)).toBeNull();
    // The blocking approval stays fully readable.
    expect(screen.getByRole("region", { name: "Approval required" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Deny" })).toBeInTheDocument();
  });

  it("renders an identical tree for a running run under prefers-reduced-motion", () => {
    // jsdom computes no CSS, so the honest reduced-motion pin available here
    // is that the media query changes nothing in the tree: the pulse element
    // is still there (motion.css + the CSS fallback freeze it to a static
    // accent segment). Removing it would drop the live state's only mark.
    const matchMedia = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: true,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;

    try {
      const run = runView({ steps: mixedSteps() });
      const reduced = renderRun(run);
      const markup = reduced.container.innerHTML;
      const spineMarkup = rail(reduced.container)?.outerHTML ?? "";
      reduced.unmount();

      const motion = renderRun(run);
      expect(motion.container.innerHTML).toBe(markup);
      expect(rail(motion.container)?.outerHTML ?? "").toBe(spineMarkup);
      expect(pulse(motion.container)).not.toBeNull();
    } finally {
      window.matchMedia = matchMedia;
    }
  });
});

describe("temporal fade (brief 13.9)", () => {
  it("steps old rows down the ladder and never touches the current or protected rows", () => {
    const steps = [
      stepView({ id: 1, seq: 1 }),
      stepView({ id: 2, seq: 2 }),
      stepView({ id: 3, seq: 3, status: "failed" }),
      stepView({ id: 4, seq: 4, kind: "approval", tool_name: "execute_command" }),
      stepView({ id: 5, seq: 5, tool_name: "edit_file" }),
      stepView({ id: 6, seq: 6, observation_error: "boom" }),
      stepView({ id: 7, seq: 7 }),
    ];
    const { container } = renderRun(runView({ status: "running", steps }));
    const bySeq = new Map(rows(container).map((el) => [Number(seqOf(el)?.slice(1)), el]));

    // Age 0 (current) and age 1 stay at full ink: two dimmed rows would read
    // as a rendering fault rather than as age.
    expect(bySeq.get(7)?.className).toBe("nex-agent-step");
    expect(bySeq.get(6)?.className).toBe("nex-agent-step");
    // Age 2 → first ladder step; age 3 and deeper → the floor tier.
    expect(bySeq.get(5)?.className).toBe("nex-agent-step nex-agent-step-age-1");
    expect(bySeq.get(2)?.className).toBe("nex-agent-step nex-agent-step-age-2");
    expect(bySeq.get(1)?.className).toBe("nex-agent-step nex-agent-step-age-2");
    // Protected rows never fade, at any age: the approval row is age 3 and
    // the failed row age 4 here, so both would otherwise be dimmed.
    expect(bySeq.get(4)?.className).toBe("nex-agent-step");
    expect(bySeq.get(3)?.className).toBe("nex-agent-step");
    // A failed row also keeps its own connector tone.
    expect(connectors(container)[2].className).toContain("nex-agent-step-slot-failed");
  });
});

describe("tool group folding (brief 13.33)", () => {
  /** 7 steps: 5 tool calls (3 file, 1 command, 1 search) and 2 model turns. */
  const longRun = (overrides: Partial<AgentRunView> = {}): AgentRunView =>
    runView({
      status: "completed",
      steps: [
        ...mixedSteps(),
        stepView({ id: 16, seq: 6, tool_name: "write_file" }),
        stepView({ id: 17, seq: 7, kind: "model_turn", tool_name: null }),
      ],
      ...overrides,
    });

  it("collapses a finished long run into one summary and restores the same rows in order", () => {
    const { container, rerenderRun } = renderRun(longRun());

    const summary = screen.getByRole("button", { name: /5 tool calls/ });
    expect(summary).toHaveAttribute("aria-expanded", "false");
    // Buckets: 3 file tools, 1 command, 1 search. Zero buckets are dropped.
    expect(summary).toHaveTextContent("5 tool calls · 3 files · 1 command · 1 search");
    // The rows are hidden, never unmounted: the same objects stay in the tree.
    expect(container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    expect(rows(container)).toHaveLength(7);

    const before = rows(container);
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
    const after = rows(container);
    // Same seq order, and the very same element instances — which is what
    // preserves each row's own open/closed state across the round trip.
    expect(after.map(seqOf)).toEqual(["#1", "#2", "#3", "#4", "#5", "#6", "#7"]);
    expect(after[2]).toBe(before[2]);

    fireEvent.click(summary);
    const collapsed = rows(container);
    expect(collapsed.map(seqOf)).toEqual(after.map(seqOf));
    expect(collapsed[0]).toBe(after[0]);

    // A re-render mid-fold (every step frame does exactly this in the app)
    // keeps the folded state and the summary.
    rerenderRun(longRun());
    expect(container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    expect(screen.getByRole("button", { name: /5 tool calls/ })).toHaveAttribute("aria-expanded", "false");
  });

  it("restores a row's own open state after the fold round trip", () => {
    const { container } = renderRun(longRun());
    const summary = screen.getByRole("button", { name: /5 tool calls/ });

    fireEvent.click(summary);
    const readRows = screen.getAllByRole("button", { name: /read_file/ });
    fireEvent.click(readRows[0]);
    expect(readRows[0]).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("group", { name: "Tool arguments" })).toBeInTheDocument();

    fireEvent.click(summary);
    expect(container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    fireEvent.click(summary);

    const reopened = screen.getAllByRole("button", { name: /read_file/ });
    expect(reopened[0]).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("group", { name: "Tool arguments" })).toBeInTheDocument();
  });

  it("never folds a run that still needs the user's eye, nor a short one", () => {
    const parked = renderRun(
      longRun({
        pending_approval: {
          call_id: "call-9",
          name: "execute_command",
          arguments: "{}",
          timeout_secs: 0,
          requested_at: 1000,
        },
      }),
    );
    expect(screen.queryByRole("button", { name: /tool calls/ })).toBeNull();
    expect(parked.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
    parked.unmount();

    const failed = renderRun(
      longRun({ steps: [...mixedSteps(), stepView({ id: 16, seq: 6, status: "failed" })] }),
    );
    expect(screen.queryByRole("button", { name: /tool calls/ })).toBeNull();
    expect(failed.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
    failed.unmount();

    const broken = renderRun(
      longRun({ steps: [...mixedSteps(), stepView({ id: 16, seq: 6, observation_error: "gone" })] }),
    );
    expect(screen.queryByRole("button", { name: /tool calls/ })).toBeNull();
    broken.unmount();

    // Below the fold threshold a summary row would cost more than the rows it
    // replaces, so a short finished run stays expanded.
    const short = renderRun(runView({ status: "completed", steps: mixedSteps() }));
    expect(screen.queryByRole("button", { name: /tool calls/ })).toBeNull();
    expect(rows(short.container)).toHaveLength(5);
    expect(pulse(short.container)).toBeNull();
    short.unmount();

    // A live run is never folded, however long it gets.
    const live = renderRun(runView({ status: "running", steps: longRun().steps }));
    expect(screen.queryByRole("button", { name: /tool calls/ })).toBeNull();
    expect(pulse(live.container)).not.toBeNull();
  });

  it("declines the fold summary in EN and RU", () => {
    const stored = window.localStorage.getItem(LOCALE_KEY);
    window.localStorage.setItem(LOCALE_KEY, "ru");
    try {
      // RU few/many declension across every bucket.
      const ru = renderRun(longRun());
      expect(screen.getByRole("button", { name: /5 вызовов/ })).toHaveTextContent(
        "5 вызовов · 3 файла · 1 команда · 1 поиск",
      );
      ru.unmount();

      // 6 steps, 5 tool calls, 4 of them file tools.
      const many = renderRun(
        runView({
          status: "completed",
          steps: [
            stepView({ id: 21, seq: 1 }),
            stepView({ id: 22, seq: 2 }),
            stepView({ id: 23, seq: 3, tool_name: "edit_file" }),
            stepView({ id: 24, seq: 4, tool_name: "write_file" }),
            stepView({ id: 25, seq: 5, kind: "model_turn", tool_name: null }),
            stepView({ id: 26, seq: 6, tool_name: "execute_command" }),
          ],
        }),
      );
      expect(screen.getByRole("button", { name: /5 вызовов/ })).toHaveTextContent(
        "5 вызовов · 4 файла · 1 команда",
      );
      many.unmount();

      window.localStorage.removeItem(LOCALE_KEY);
      renderRun(longRun());
      expect(screen.getByRole("button", { name: /5 tool calls/ })).toHaveTextContent(
        "5 tool calls · 3 files · 1 command · 1 search",
      );
    } finally {
      if (stored === null) window.localStorage.removeItem(LOCALE_KEY);
      else window.localStorage.setItem(LOCALE_KEY, stored);
    }
  });
});

describe("run-scoped callbacks", () => {  it("passes the run id to every control the run renders", () => {
    const onResolveApproval = vi.fn();
    const onCancel = vi.fn();
    const onPause = vi.fn();
    const { container } = renderRun(
      runView({
        steps: mixedSteps(),
        pending_approval: {
          call_id: "call-3",
          name: "execute_command",
          arguments: "{}",
          timeout_secs: 0,
          requested_at: 1000,
        },
      }),
      { onResolveApproval, onCancel, onPause },
    );

    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(onResolveApproval).toHaveBeenCalledWith(7, "call-3", false);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalledWith(7);
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(onPause).toHaveBeenCalledWith(7);
    expect(within(container).getByText("run 7")).toBeInTheDocument();
  });
});
