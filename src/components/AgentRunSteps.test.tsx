//! Agent Activity Spine + run-state visuals (#153).
//!
//! Behavior pins only — no timing assertions, no sleeps (repo policy).
//! Fixtures below are built to the vocabulary the backend actually persists —
//! `kind` ∈ `model_turn` | `tool_call` | `approval` and `agent_steps.status`
//! ∈ `succeeded` | `failed` | `denied` | `cancelled` (the CHECK in
//! `database.rs`), mapped onto the render views this component consumes
//! (`AgentRunView` / `AgentStepView`, the shapes the live hook emits).
//! `useAgentRun.test.tsx:48-103` builds its `AgentStep` rows with
//! `kind: "tool"` and `status: "ok"`, which that persisted vocabulary
//! excludes, so it exercises the hook's own mapping rather than this
//! component's and is deliberately not treated as authoritative here.
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
    // The status pill's own motion follows the same rule: a working run
    // keeps its spinner…
    expect(container.querySelector(".nex-spinner")).not.toBeNull();
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
    // …and the header stops spinning for the same reason: the status string
    // still reads `running`, but nothing here is burning cycles. A spinner
    // on a park is the component's own contradiction (§7: motion means
    // "executing right now").
    expect(container.querySelector(".nex-spinner")).toBeNull();
    expect(container.querySelector(".nex-agent-run")?.className).toContain("nex-agent-run-parked");
    // The blocking approval stays fully readable.
    expect(screen.getByRole("region", { name: "Approval required" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Deny" })).toBeInTheDocument();
  });

  it("changes no markup under prefers-reduced-motion, and stops the ticking clock", () => {
    // Two honest pins, because one alone is not worth much here.
    //
    //  (a) THE TREE. jsdom computes no CSS, so the only reduced-motion claim
    //      available in this file is that the media query changes nothing in
    //      the markup: the pulse element is still there (motion.css + its
    //      CSS fallback freeze it to a static accent segment). Removing it
    //      would drop the live state's only mark, so both arms are identical
    //      DOM and the reduced arm still carries the segment.
    //  (b) THE BEHAVIOUR. The arms must genuinely differ in JS: reduced
    //      schedules ONE timeout at expiry, motion an interval that ticks.
    //      A stub that answered `matches: true` to every query would render
    //      the "motion" baseline as reduced and make (a) vacuous — this is
    //      the self-comparing trap, so the stub below is query-aware and (b)
    //      fails the moment it stops discriminating.
    const matchMedia = window.matchMedia;
    const queries: string[] = [];
    let prefersReduced = true;
    window.matchMedia = ((query: string) => {
      queries.push(query);
      return {
        matches: prefersReduced && query.includes("reduce"),
        media: query,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      };
    }) as unknown as typeof window.matchMedia;
    vi.useFakeTimers();

    try {
      // The stub discriminates, both ways, before anything is asserted.
      expect(window.matchMedia("(prefers-reduced-motion: reduce)").matches).toBe(true);
      prefersReduced = false;
      expect(window.matchMedia("(prefers-reduced-motion: reduce)").matches).toBe(false);
      prefersReduced = true;

      const run = runView({ steps: mixedSteps() });
      const reduced = renderRun(run);
      const markup = reduced.container.innerHTML;
      const spineMarkup = rail(reduced.container)?.outerHTML ?? "";
      // Reduced motion keeps the accent segment: static, not absent.
      expect(pulse(reduced.container)).not.toBeNull();
      reduced.unmount();

      // (a) is only honest if this arm really renders WITH motion: flip the
      // stub immediately before the render, so the DOM equality below compares
      // a reduced tree against a motion tree rather than against itself.
      prefersReduced = false;
      const motion = renderRun(run);
      expect(motion.container.innerHTML).toBe(markup);
      expect(rail(motion.container)?.outerHTML ?? "").toBe(spineMarkup);
      expect(pulse(motion.container)).not.toBeNull();
      motion.unmount();

      // (b) The real difference, on a run that actually has a countdown.
      // Clock frozen by the fake timers, so `remaining` is exactly the
      // 600s deadline: reduced → one 600_000ms timeout, no interval;
      // motion → a 30_000ms interval (the coarser pre-minute cadence).
      // This is the arm that discriminates, and its FIRST render is the
      // reduced one, so the stub goes back before it renders.
      prefersReduced = true;
      const nowSecs = Date.now() / 1000;
      const parked = runView({
        steps: mixedSteps(),
        pending_approval: {
          call_id: "call-r",
          name: "execute_command",
          arguments: "{}",
          timeout_secs: 600,
          requested_at: nowSecs,
        },
      });
      const intervalSpy = vi.spyOn(window, "setInterval");
      const timeoutSpy = vi.spyOn(window, "setTimeout");

      const reducedParked = renderRun(parked);
      expect(queries).toContain("(prefers-reduced-motion: reduce)");
      expect(intervalSpy, "reduced motion must not schedule a ticking clock").not.toHaveBeenCalled();
      expect(timeoutSpy).toHaveBeenCalledWith(expect.any(Function), 600_000);
      reducedParked.unmount();

      intervalSpy.mockClear();
      timeoutSpy.mockClear();
      prefersReduced = false;

      const motionParked = renderRun(parked);
      expect(intervalSpy).toHaveBeenCalledWith(expect.any(Function), 30_000);
      expect(timeoutSpy, "motion arm must not fall back to the one-shot").not.toHaveBeenCalledWith(
        expect.any(Function),
        600_000,
      );
      motionParked.unmount();
      intervalSpy.mockRestore();
      timeoutSpy.mockRestore();
    } finally {
      vi.useRealTimers();
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
      // An approval with NO status is the outstanding shape, and the only
      // one that protects: a granted park persists `status: "succeeded"`
      // (persistence.rs) and is settled, so it fades like any other row.
      stepView({ id: 4, seq: 4, kind: "approval", tool_name: "execute_command", status: null }),
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
    // Protected rows never fade, at any age: the outstanding approval row is
    // age 3 and the failed row age 4 here, so both would otherwise be dimmed.
    expect(bySeq.get(4)?.className).toBe("nex-agent-step");
    expect(bySeq.get(3)?.className).toBe("nex-agent-step");
    // A failed row also keeps its own connector tone.
    expect(connectors(container)[2].className).toContain("nex-agent-step-slot-failed");
  });

  it("never fades a denied or cancelled call, and keeps the failure tone", () => {
    // The vocabulary the backend actually writes: `denied` for the four
    // dispatch denials (document shell ban, WS-B.3 role gate, over-cap
    // batch, injection hold with no gate to park on) — each a tool_call
    // row with an `Error:` observation and NO approval row beside it (the
    // sticky-group and rule denials are approval ROWS, not tool_call
    // paths) — and `cancelled` for a call the cancellation token tripped
    // mid-flight.
    const denial = "Error: tool 'execute_command' is not available to agent role 'reader'";
    const steps = [
      stepView({ id: 1, seq: 1 }),
      stepView({ id: 2, seq: 2, status: "failed", observation: "Error: exit 1" }),
      stepView({ id: 3, seq: 3, status: "denied", observation: denial }),
      stepView({ id: 4, seq: 4 }),
      stepView({ id: 5, seq: 5, status: "cancelled", observation: "Error: interrupted" }),
      stepView({ id: 6, seq: 6 }),
    ];
    const { container } = renderRun(runView({ status: "running", steps }));
    const bySeq = new Map(rows(container).map((el) => [Number(seqOf(el)?.slice(1)), el]));

    // Age 4 and age 2 would otherwise be at the floor tier: the denial is
    // the only record that the model was refused, so it stays at full ink.
    expect(bySeq.get(2)?.className).toBe("nex-agent-step");
    expect(bySeq.get(3)?.className).toBe("nex-agent-step");
    expect(bySeq.get(5)?.className).toBe("nex-agent-step");
    // …and an untouched row at the same age still fades, so the pin above
    // is about the status and not about the position.
    expect(bySeq.get(1)?.className).toBe("nex-agent-step nex-agent-step-age-2");
    expect(bySeq.get(4)?.className).toBe("nex-agent-step nex-agent-step-age-1");
    // One predicate for the tone: denied and cancelled read as failed too.
    for (const idx of [1, 2, 4]) {
      expect(connectors(container)[idx].className).toContain("nex-agent-step-slot-failed");
    }
    expect(connectors(container)[3].className).not.toContain("nex-agent-step-slot-failed");
  });
});

describe("tool group folding (brief 13.33)", () => {
  /**
   * 7 steps: 5 tool calls (3 file ops — 2 reads + 1 write, 1 command,
   * 1 search) and 2 model turns. The shape test below pins every count.
   */
  const longRun = (overrides: Partial<AgentRunView> = {}): AgentRunView =>
    runView({
      status: "completed",
      steps: [
        ...mixedSteps(),
        stepView({ id: 16, seq: 6, tool_name: "write_file", arguments: '{"path":"src/db.rs"}' }),
        stepView({ id: 17, seq: 7, kind: "model_turn", tool_name: null }),
      ],
      ...overrides,
    });

  it("pins the longRun fixture shape its doc comment states", () => {
    const steps = longRun().steps;
    const toolCalls = steps.filter((step) => step.kind === "tool_call");
    expect(steps).toHaveLength(7);
    expect(steps.filter((step) => step.kind === "model_turn")).toHaveLength(2);
    expect(toolCalls).toHaveLength(5);
    expect(
      toolCalls.filter((step) => step.tool_name === "read_file" || step.tool_name === "write_file"),
    ).toHaveLength(3);
    expect(toolCalls.filter((step) => step.tool_name === "execute_command")).toHaveLength(1);
    expect(toolCalls.filter((step) => step.tool_name === "search_files")).toHaveLength(1);
  });

  it("collapses a finished long run into one summary and restores the same rows in order", () => {
    const { container, rerenderRun } = renderRun(longRun());

    const summary = screen.getByRole("button", { name: "Show run steps" });
    expect(summary).toHaveAttribute("aria-expanded", "false");
    // The counts now live on the ONE summary line, exactly once:
    // "1 file changed · 1 command · 5 tool calls".
    expect(
      container.querySelector(".nex-agent-run-summary")?.textContent,
    ).toBe("1 file changed · 1 command · 5 tool calls");
    // Two COMPLETED blocks cannot render: exactly one summary element.
    expect(container.querySelectorAll(".nex-agent-run-summary")).toHaveLength(1);
    // The rows are hidden, never unmounted: the same objects stay in the tree.
    expect(container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    expect(rows(container)).toHaveLength(7);

    const before = rows(container);
    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(summary).toHaveAttribute("aria-controls", "nex-agent-timeline-7");
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
    expect(screen.getByRole("button", { name: "Show run steps" })).toHaveAttribute("aria-expanded", "false");
  });

  it("restores a row's own open state after the fold round trip", () => {
    const { container } = renderRun(longRun());
    const summary = screen.getByRole("button", { name: "Show run steps" });

    fireEvent.click(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    // A read row names itself by its verb now ("Read"), not the tool id.
    const readRows = screen.getAllByRole("button", { name: /Read/ });
    expect(readRows).toHaveLength(2);
    fireEvent.click(readRows[0]);
    expect(readRows[0]).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("group", { name: "Tool arguments" })).toBeInTheDocument();

    fireEvent.click(summary);
    expect(container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    fireEvent.click(summary);
    expect(container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");

    const reopened = screen.getAllByRole("button", { name: /Read/ });
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

  it("never folds a call that failed, was denied, or was cancelled", () => {
    // Each of these is persisted on a tool_call row with an `Error:`
    // observation and no companion approval row, so folding the run would
    // take the only record of the refusal out of sight.
    for (const status of ["failed", "denied", "cancelled"]) {
      const view = renderRun(
        longRun({
          steps: [
            ...mixedSteps(),
            stepView({ id: 16, seq: 6, status, observation: "Error: tool call refused" }),
          ],
        }),
      );
      expect(screen.queryByRole("button", { name: "Show run steps" }), `status ${status}`).toBeNull();
      expect(view.container.querySelector(".nex-agent-timeline"), `status ${status}`).not.toHaveAttribute(
        "hidden",
      );
      view.unmount();
    }

    // A rule denial parks the same status on the approval row itself.
    const ruled = renderRun(
      longRun({
        steps: [
          ...mixedSteps(),
          stepView({
            id: 16,
            seq: 6,
            kind: "approval",
            tool_name: "execute_command",
            status: "denied",
            observation: "denied by rule:r1",
          }),
        ],
      }),
    );
    expect(screen.queryByRole("button", { name: "Show run steps" })).toBeNull();
    expect(ruled.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
    ruled.unmount();
  });

  it("folds a run whose only approval was granted, and protects an outstanding one", () => {
    // persistence.rs: a granted park persists `kind: "approval"` with
    // `status: "succeeded"` — settled, not awaiting an eye — so it must not
    // hold the whole run open. Without the status qualifier every run that
    // ever gated one tool would be permanently unfoldable.
    const granted = renderRun(
      longRun({
        steps: [
          ...mixedSteps(),
          stepView({
            id: 16,
            seq: 6,
            kind: "approval",
            tool_name: "execute_command",
            status: "succeeded",
            observation: "approved",
          }),
        ],
      }),
    );
    expect(screen.getByRole("button", { name: "Show run steps" })).toBeInTheDocument();
    expect(granted.container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
    granted.unmount();

    // No status at all is the outstanding shape (the live gate on a parked
    // run is `run.pending_approval`, which a finished run no longer has) —
    // that one still protects.
    const outstanding = renderRun(
      longRun({
        steps: [
          ...mixedSteps(),
          stepView({ id: 16, seq: 6, kind: "approval", tool_name: "execute_command", status: null }),
        ],
      }),
    );
    expect(screen.queryByRole("button", { name: "Show run steps" })).toBeNull();
    expect(outstanding.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
  });

  it("keeps a budget-exhausted run expanded, Continue and all", () => {
    // Terminal in the database, unfinished in the §13.33 sense: the header
    // still offers Continue, so "where it ran out" is the question the user
    // is about to ask. Folding it would answer with a count.
    const exhausted = renderRun(longRun({ status: "budget_exhausted" }));
    expect(screen.queryByRole("button", { name: "Show run steps" })).toBeNull();
    expect(exhausted.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");
    expect(screen.getByRole("button", { name: "Continue" })).toBeInTheDocument();
    exhausted.unmount();

    // The same long, clean, terminal run without the budget stop does fold —
    // so the exclusion is the status and not the length.
    const completed = renderRun(longRun({ status: "completed" }));
    expect(screen.getByRole("button", { name: "Show run steps" })).toBeInTheDocument();
    expect(completed.container.querySelector(".nex-agent-timeline")).toHaveAttribute("hidden");
  });

  it("names the fold button for what it does and owns the timeline it hides", () => {
    const en = renderRun(longRun());
    const timeline = en.container.querySelector(".nex-agent-timeline");
    const summary = screen.getByRole("button", { name: "Show run steps" });
    // The visible line says what the control DOES (show/hide), not the counts —
    // the counts live on the ONE summary line above the run.
    expect(timeline).toHaveAttribute("id", "nex-agent-timeline-7");
    expect(summary).toHaveAttribute("aria-controls", "nex-agent-timeline-7");
    expect(summary).toHaveTextContent("Show run steps");

    fireEvent.click(summary);
    expect(screen.getByRole("button", { name: "Hide run steps" })).toBe(summary);
    expect(summary).toHaveAttribute("aria-expanded", "true");
    expect(en.container.querySelector(".nex-agent-timeline")).not.toHaveAttribute("hidden");

    const stored = window.localStorage.getItem(LOCALE_KEY);
    window.localStorage.setItem(LOCALE_KEY, "ru");
    try {
      // `longRun()` is always run 7, so the EN tree MUST come down first:
      // two live runs would put two `id="nex-agent-timeline-7"` in one DOM and
      // `aria-controls` would resolve to an ambiguous target — the exact defect
      // the uniqueness above is meant to rule out.
      en.unmount();
      const ru = renderRun(longRun());
      expect(
        screen.getByRole("button", { name: "Показать шаги запуска" }),
      ).toHaveAttribute("aria-controls", "nex-agent-timeline-7");
      expect(ru.container.querySelectorAll("#nex-agent-timeline-7")).toHaveLength(1);
      ru.unmount();
    } finally {
      if (stored === null) window.localStorage.removeItem(LOCALE_KEY);
      else window.localStorage.setItem(LOCALE_KEY, stored);
    }
  });

  it("declines the fold summary in EN and RU", () => {
    const stored = window.localStorage.getItem(LOCALE_KEY);
    window.localStorage.setItem(LOCALE_KEY, "ru");
    try {
      // RU: the fold button carries the action only — never the counts.
      const ru = renderRun(longRun());
      const ruButton = screen.getByRole("button", { name: "Показать шаги запуска" });
      expect(ruButton).toHaveTextContent("Показать шаги запуска");
      // The counts live on the ONE summary line, once.
      expect(
        ru.container.querySelector(".nex-agent-run-summary")?.textContent,
      ).toBe("1 файл изменён · 1 команда · 5 вызовов инструментов");
      ru.unmount();

      // 6 steps, 5 tool calls, 4 of them file tools.
      const many = renderRun(
        runView({
          status: "completed",
          steps: [
            stepView({ id: 21, seq: 1 }),
            stepView({ id: 22, seq: 2 }),
            stepView({ id: 23, seq: 3, tool_name: "edit_file", arguments: '{"path":"src/a.ts"}' }),
            stepView({ id: 24, seq: 4, tool_name: "write_file", arguments: '{"path":"src/b.ts"}' }),
            stepView({ id: 25, seq: 5, kind: "model_turn", tool_name: null }),
            stepView({ id: 26, seq: 6, tool_name: "execute_command" }),
          ],
        }),
      );
      expect(screen.getByRole("button", { name: "Показать шаги запуска" })).toBeInTheDocument();
      expect(
        many.container.querySelector(".nex-agent-run-summary")?.textContent,
      ).toBe("2 файла изменено · 1 команда · 5 вызовов инструментов");
      many.unmount();

      window.localStorage.removeItem(LOCALE_KEY);
      renderRun(longRun());
      expect(screen.getByRole("button", { name: "Show run steps" })).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Show run steps" }).querySelector(
          ".nex-agent-fold-label",
        )?.textContent,
      ).toBe("Show run steps");
    } finally {
      if (stored === null) window.localStorage.removeItem(LOCALE_KEY);
      else window.localStorage.setItem(LOCALE_KEY, stored);
    }
  });

  it("repeated groups never share a React key or a DOM id", () => {
    // Two non-consecutive groups with the same tool + target:
    // read src/a.ts ×2 → write src/b.ts → read src/a.ts ×2.
    // Without a stable per-group id they would collapse to one id and one key.
    const { container } = renderRun(
      runView({
        status: "completed",
        steps: [
          stepView({ id: 41, seq: 1, tool_name: "read_file", arguments: '{"path":"src/a.ts"}' }),
          stepView({ id: 42, seq: 2, tool_name: "read_file", arguments: '{"path":"src/a.ts"}' }),
          stepView({ id: 43, seq: 3, tool_name: "write_file", arguments: '{"path":"src/b.ts"}' }),
          stepView({ id: 44, seq: 4, tool_name: "read_file", arguments: '{"path":"src/a.ts"}' }),
          stepView({ id: 45, seq: 5, tool_name: "read_file", arguments: '{"path":"src/a.ts"}' }),
        ],
      }),
    );
    const groups = container.querySelectorAll(".nex-agent-step-group");
    expect(groups).toHaveLength(2);
    const ids = [...groups].map((g) => g.getAttribute("id"));
    expect(ids).toEqual(
      expect.arrayContaining(["nex-agent-timeline-7-g1", "nex-agent-timeline-7-g4"]),
    );
    expect(new Set(ids).size).toBe(2);
    // aria-controls always points at an element that exists, collapsed or not.
    container.querySelectorAll(".nex-agent-step-header-group").forEach((h) => {
      const controls = h.getAttribute("aria-controls");
      expect(controls).not.toBeNull();
      expect(document.getElementById(controls!)).not.toBeNull();
    });
  });

  it("counts distinct files, not write steps", () => {
    // Writing the same file twice must read as one file changed, not two.
    const { container } = renderRun(
      runView({
        status: "completed",
        steps: [
          stepView({ id: 51, seq: 1, tool_name: "write_file", arguments: '{"path":"src/x.rs"}' }),
          stepView({ id: 52, seq: 2, tool_name: "write_file", arguments: '{"path":"src/x.rs"}' }),
          stepView({ id: 53, seq: 3, tool_name: "execute_command", arguments: '{"command":"ls"}' }),
        ],
      }),
    );
    expect(
      container.querySelector(".nex-agent-run-summary")?.textContent,
    ).toBe("1 file changed · 1 command · 3 tool calls");
  });

  it("error block: what happened never reads as the next action", () => {
    const { container } = renderRun(
      runView({ status: "error", error: null, steps: mixedSteps() }),
    );
    const lines = container.querySelectorAll(".nex-agent-failure-line");
    expect(lines).toHaveLength(3);
    // First line: "What happened" carries a real cause, not the retry sentence.
    expect(lines[0].textContent).toContain("What happened");
    expect(lines[0].textContent).not.toContain("Send the message again to retry.");
    // Third line: the next action stays separate and unchanged.
    expect(lines[2].textContent).toContain("Next");
    expect(lines[2].textContent).toContain("Send the message again to retry.");
  });

  it("path target renders directory + file name, name stays visible (LTR)", () => {
    const { container } = renderRun(
      runView({
        status: "completed",
        steps: [
          stepView({ id: 81, seq: 1, tool_name: "write_file", arguments: '{"path":"src/lib/db.rs"}' }),
        ],
      }),
    );
    const slot = container.querySelector(".nex-agent-step-target");
    expect(slot).not.toBeNull();
    // Full path is on hover (SYSTEM §8 "paths on hover").
    expect(slot!.getAttribute("title")).toBe("Full path");
    const dir = slot!.querySelector(".nex-agent-step-target-dir");
    const name = slot!.querySelector(".nex-agent-step-target-name");
    expect(dir).not.toBeNull();
    expect(name).not.toBeNull();
    // Directory prefix elides with its own ellipsis; the file name stays.
    expect(dir?.textContent).toBe("src/lib/");
    expect(name?.textContent).toBe("db.rs");
    // LTR order: directory part before the file name.
    expect(Array.from(slot!.children).map((c) => c.textContent)).toEqual(["src/lib/", "db.rs"]);
  });

  it("path with no directory (bare file name) renders name only", () => {
    const { container } = renderRun(
      runView({
        status: "completed",
        steps: [
          stepView({ id: 82, seq: 1, tool_name: "write_file", arguments: '{"path":"db.rs"}' }),
        ],
      }),
    );
    const slot = container.querySelector(".nex-agent-step-target")!;
    const dir = slot.querySelector(".nex-agent-step-target-dir");
    const name = slot.querySelector(".nex-agent-step-target-name");
    expect(dir?.textContent).toBe("");
    expect(name?.textContent).toBe("db.rs");
  });

  it("command target renders whole text as the name slot, no dir split", () => {
    const { container } = renderRun(
      runView({
        status: "completed",
        steps: [
          stepView({ id: 83, seq: 1, tool_name: "execute_command", arguments: '{"command":"npm --version"}' }),
        ],
      }),
    );
    const slot = container.querySelector(".nex-agent-step-target")!;
    // A command is not a path: no directory/name split is forced onto it.
    expect(slot.querySelector(".nex-agent-step-target-dir")).toBeNull();
    const name = slot.querySelector(".nex-agent-step-target-name");
    expect(name).not.toBeNull();
    expect(name?.textContent).toBe("npm --version");
    expect(slot.getAttribute("title")).toBe("Full command");
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
