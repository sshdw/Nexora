//! M3SegmentedGroup — Glide Select (R4-08) + the `.nex-seg` contract
//! (G-18).
//!
//! jsdom ships no layout engine, so the geometry has to be supplied:
//! `getBoundingClientRect` is answered from per-element box attributes
//! (a segment's box from two data attributes, the container's from
//! `setTrack`), which is enough for the component's measurement path.
//! ResizeObserver is absent as well; the stub below records callbacks
//! so a test can fire one on demand.
//!
//! What is pinned here:
//!   1. MEASUREMENT — the two sanctioned custom properties
//!      (`--seg-indicator-x` / `--seg-indicator-w`) follow the
//!      SELECTED segment's seat, on mount, on click, on resize, after
//!      a locale swap and after `document.fonts.ready`. The seat is
//!      exact (fractional `1fr` tracks keep their fraction): the pill
//!      is neither rounded off its segment nor flush against the
//!      track's end padding.
//!   2. NO FLY-IN — `data-ready="false"` until the first successful
//!      measurement, and `data-no-anim` for one frame when the layout
//!      changes under a still pill. The sheet side of both is
//!      asserted from `components.css`, because jsdom computes no
//!      cascade.
//!   3. SEAT SYMMETRY — the same inset token on every side of the
//!      pill, at the first, a middle and the last seat, and one
//!      shared radius for the pill and the segment it sits on.
//!   4. SEMANTICS — radiogroup/radio, tablist/tab, aria-checked vs
//!      aria-selected, roving tabindex, arrow keys, disabled skip:
//!      unchanged by the pill. The pill itself is aria-hidden and
//!      unfocusable.
//!
//! No timing assertions anywhere (repo policy): the frame tests wait
//! for the attribute to go away, they never assert a duration.

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import M3SegmentedGroup from "./M3SegmentedGroup";
import type { M3SegmentOption } from "./M3SegmentedGroup";
import { readRepoFile } from "../test/readRepoFile";

type Mode = "chat" | "agent" | "context";

const TWO_OPTIONS: readonly M3SegmentOption<Mode>[] = [
  { value: "chat", label: "Chat" },
  { value: "agent", label: "Agent" },
];

const THREE_OPTIONS: readonly M3SegmentOption<Mode>[] = [
  { value: "chat", label: "Chat" },
  { value: "agent", label: "Agent" },
  { value: "context", label: "Context" },
];

const THREE_WITH_DISABLED: readonly M3SegmentOption<Mode>[] = [
  { value: "chat", label: "Chat" },
  { value: "agent", label: "Agent", disabled: true },
  { value: "context", label: "Context" },
];

/** ResizeObserver callbacks, so a test can fire a resize on demand. */
const resizeCallbacks = new Set<ResizeObserverCallback>();

class ControlledResizeObserver {
  private readonly callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeCallbacks.add(callback);
  }

  observe(): void {}

  unobserve(): void {}

  disconnect(): void {
    resizeCallbacks.delete(this.callback);
  }
}

function fireResize(): void {
  act(() => {
    for (const callback of [...resizeCallbacks]) {
      callback([], {} as ResizeObserver);
    }
  });
}

/** A DOMRect with the four fields the measurement reads. */
function makeRect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    toJSON: () => ({}),
  } as DOMRect;
}

/** Give one segment a box without a layout engine. `left` is its
 * offset inside the container, i.e. what a real `offsetLeft` reported. */
function setBox(element: Element, left: number, width: number): void {
  const node = element as HTMLElement;
  node.dataset.testOffsetLeft = String(left);
  node.dataset.testOffsetWidth = String(width);
}

/** Give the container a border box: 32px tall (28px segment + the
 * --space-xxxs padding above and below) and as wide as the tracks. */
function setTrack(element: Element, width: number, height = 32): void {
  const node = element as HTMLElement;
  node.dataset.testRectWidth = String(width);
  node.dataset.testRectHeight = String(height);
}

/** The layout of a standard group: `1fr` tracks are fractional, so the
 * seat edges land on fractions — the case integer offsets rounded away. */
function threeSeatLayout(trackWidth = 283): {
  track: number;
  padding: number;
  seats: Array<{ left: number; width: number }>;
} {
  const padding = 2;
  const gap = 2;
  const width = (trackWidth - padding * 2 - gap * 2) / 3;
  return {
    track: trackWidth,
    padding,
    seats: [0, 1, 2].map((index) => ({
      left: padding + index * (width + gap),
      width,
    })),
  };
}

function pill(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".nex-seg-indicator");
}

function group(): HTMLElement {
  return screen.getByRole("radiogroup", { name: "Mode" });
}

function segments(): HTMLElement[] {
  return screen.getAllByRole("radio");
}

/** Stateful wrapper: arrow keys and clicks actually move the value. */
function GroupHarness({
  initial = "chat",
  options = TWO_OPTIONS,
  indicator,
  variant,
  semantics,
  disabled,
}: {
  initial?: Mode | null;
  options?: readonly M3SegmentOption<Mode>[];
  indicator?: boolean;
  variant?: "standard" | "connected";
  semantics?: "radio" | "tabs";
  disabled?: boolean;
}) {
  const [value, setValue] = useState<Mode | null>(initial);
  return (
    <M3SegmentedGroup<Mode>
      label="Mode"
      options={options}
      value={value}
      onChange={setValue}
      indicator={indicator}
      variant={variant}
      semantics={semantics}
      disabled={disabled}
    />
  );
}

beforeAll(() => {
  vi.stubGlobal("ResizeObserver", ControlledResizeObserver);
  // jsdom answers 0×0 for every element. The measurement reads layout
  // rects, so the harness supplies them: a container from `setTrack`, a
  // segment from `setBox` (+ the segment height the sheet gives it).
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.classList.contains("nex-seg")) {
        return makeRect(
          0,
          0,
          Number(this.dataset.testRectWidth ?? 0),
          Number(this.dataset.testRectHeight ?? 0),
        );
      }
      const group = this.closest(".nex-seg");
      if (group === null) return makeRect(0, 0, 0, 0);
      const track = group.getBoundingClientRect();
      return makeRect(
        track.left + Number(this.dataset.testOffsetLeft ?? 0),
        track.top,
        Number(this.dataset.testOffsetWidth ?? 0),
        28,
      );
    },
  );
});

afterAll(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  resizeCallbacks.clear();
});

afterEach(() => {
  Reflect.deleteProperty(document, "fonts");
  cleanup();
});

describe("Glide Select measurement", () => {
  it("renders data-ready=false until the first successful measurement, then arms the transition", () => {
    render(<GroupHarness />);
    const root = group();

    // jsdom reports 0×0: that is "not laid out yet", not a measurement.
    expect(root).toHaveAttribute("data-ready", "false");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("");

    const [chat, agent] = segments();
    setBox(chat!, 2, 68);
    setBox(agent!, 72, 68);
    fireResize();

    expect(root).toHaveAttribute("data-ready", "true");
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("2px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("68px");
    // The ready flip itself is never an animated one.
    expect(root).not.toHaveAttribute("data-no-anim");
  });

  it("publishes the selected segment's geometry and follows a click", () => {
    render(<GroupHarness />);
    const root = group();
    const [chat, agent] = segments();
    setBox(chat!, 2, 64);
    setBox(agent!, 70, 80);
    fireResize();
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("2px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("64px");

    fireEvent.click(agent!);

    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("70px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("80px");
    // A selection change glides: the transition stays live.
    expect(root).not.toHaveAttribute("data-no-anim");
    expect(agent).toHaveAttribute("aria-checked", "true");
  });

  it("re-measures on resize and jumps for exactly one frame", async () => {
    render(<GroupHarness />);
    const root = group();
    const [chat, agent] = segments();
    setBox(chat!, 2, 64);
    setBox(agent!, 70, 64);
    fireResize();
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("64px");

    // A narrower container: the tracks shrink, the pill must jump.
    setBox(chat!, 4, 40);
    setBox(agent!, 48, 40);
    fireResize();

    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("4px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("40px");
    expect(root).toHaveAttribute("data-no-anim", "true");
    await waitFor(() => {
      expect(root).not.toHaveAttribute("data-no-anim");
    });
  });

  it("re-measures after a label change (EN → RU) without sliding", async () => {
    const ru: readonly M3SegmentOption<Mode>[] = [
      { value: "chat", label: "Чат" },
      { value: "agent", label: "Агент" },
    ];
    const { rerender } = render(
      <M3SegmentedGroup<Mode>
        label="Mode"
        options={TWO_OPTIONS}
        value="chat"
        onChange={() => {}}
      />,
    );
    const root = group();
    let [chat, agent] = segments();
    setBox(chat!, 2, 64);
    setBox(agent!, 70, 64);
    fireResize();

    rerender(
      <M3SegmentedGroup<Mode>
        label="Mode"
        options={ru}
        value="chat"
        onChange={() => {}}
      />,
    );

    // Equal-width tracks: the box is the layout's, not the label's.
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("2px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("64px");
    expect(root).toHaveAttribute("data-no-anim", "true");
    await waitFor(() => {
      expect(root).not.toHaveAttribute("data-no-anim");
    });
    [chat, agent] = segments();
    expect(chat).toHaveTextContent("Чат");
    expect(agent).toHaveTextContent("Агент");
  });

  it("re-measures when document.fonts.ready resolves", async () => {
    let resolveFonts: () => void = () => {};
    const ready = new Promise<void>((resolve) => {
      resolveFonts = resolve;
    });
    Object.defineProperty(document, "fonts", {
      configurable: true,
      value: { ready },
    });

    render(<GroupHarness />);
    const root = group();
    const [chat] = segments();
    setBox(chat!, 3, 56);

    await act(async () => {
      resolveFonts();
      await ready;
    });

    expect(root).toHaveAttribute("data-ready", "true");
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("3px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("56px");
  });
});

describe("Glide Select seat (geometry contract)", () => {
  /** Publish the fractional three-column layout the bug was measured on
   * (283px track, 2px padding, 2px gaps → 91.667px seats). */
  function layOutThreeSeats(): ReturnType<typeof threeSeatLayout> {
    const layout = threeSeatLayout();
    const root = group();
    setTrack(root, layout.track);
    segments().forEach((segment, index) => {
      const seat = layout.seats[index];
      if (seat !== undefined) setBox(segment, seat.left, seat.width);
    });
    fireResize();
    return layout;
  }

  function published(root: HTMLElement): { x: number; w: number } {
    return {
      x: Number.parseFloat(root.style.getPropertyValue("--seg-indicator-x")),
      w: Number.parseFloat(root.style.getPropertyValue("--seg-indicator-w")),
    };
  }

  it("seats the pill exactly on the first, a middle and the last segment", () => {
    render(<GroupHarness options={THREE_OPTIONS} />);
    const root = group();
    const layout = layOutThreeSeats();
    const [chat, agent, context] = segments();

    // First seat: the pill's left edge is the container's padding — not 0,
    // and not a rounded 0.333 off the track.
    const first = published(root);
    expect(first.x).toBeCloseTo(layout.seats[0]!.left, 2);
    expect(first.w).toBeCloseTo(layout.seats[0]!.width, 2);
    expect(first.x).toBeCloseTo(layout.padding, 2);

    fireEvent.click(agent!);
    // The click really moved the selection, not just the pill.
    expect(chat).toHaveAttribute("aria-checked", "false");
    const middle = published(root);
    expect(middle.x).toBeCloseTo(layout.seats[1]!.left, 2);
    expect(middle.w).toBeCloseTo(layout.seats[1]!.width, 2);

    // Last seat: the same inset on the outer side as the first seat has
    // on its own — track width minus padding, exactly.
    fireEvent.click(context!);
    const last = published(root);
    expect(last.x).toBeCloseTo(layout.seats[2]!.left, 2);
    expect(last.w).toBeCloseTo(layout.seats[2]!.width, 2);
    expect(layout.track - (last.x + last.w)).toBeCloseTo(layout.padding, 2);
  });

  it("keeps the fractional seat instead of rounding it to whole pixels", () => {
    render(<GroupHarness options={THREE_OPTIONS} />);
    const root = group();
    layOutThreeSeats();
    const [, agent, context] = segments();

    // The rounded pair (`offsetLeft` / `offsetWidth`) would have been
    // "96px" / "92px" and "189px" / "92px" on this layout.
    fireEvent.click(agent!);
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("95.667px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("91.667px");

    fireEvent.click(context!);
    expect(root.style.getPropertyValue("--seg-indicator-x")).toBe("189.333px");
    expect(root.style.getPropertyValue("--seg-indicator-w")).toBe("91.667px");
  });

  it("never lets the pill sit flush against the track's edge", () => {
    render(<GroupHarness options={THREE_OPTIONS} />);
    const root = group();
    const layout = layOutThreeSeats();
    const [, , context] = segments();

    // First seat: a flush pill would publish x = 0.
    const first = published(root);
    expect(first.x).toBeGreaterThan(0);
    expect(first.x).toBeCloseTo(layout.padding, 2);

    fireEvent.click(context!);
    const last = published(root);
    const right = last.x + last.w;
    // A flush last seat would end exactly on the track's edge.
    expect(right).toBeLessThan(layout.track);
    expect(right).not.toBeCloseTo(layout.track, 2);
    expect(right + layout.padding).toBeCloseTo(layout.track, 2);
  });

  it("leaves the segment's corners to the sheet, not to a utility class", () => {
    render(<GroupHarness />);
    // A `rounded-*` utility sits in a later cascade layer than the sheet,
    // so it wins over a caller's rule: the segment would stay at the
    // primitive's radius while the pill took the caller's (6px over
    // 4.8px on the composer). The shared `--seg-radius` is only real if
    // the segment has no utility of its own.
    for (const segment of segments()) {
      expect(segment.className).not.toMatch(/\brounded\b/);
      expect(segment.className).not.toMatch(/\brounded-/);
    }
  });
});

describe("Glide Select presence", () => {
  it("draws no pill when indicator is false, keeping the segment fill as the cue", () => {
    render(<GroupHarness indicator={false} />);
    expect(pill()).toBeNull();

    const chat = screen.getByRole("radio", { name: "Chat" });
    expect(chat).toHaveAttribute("aria-checked", "true");
    // Without the pill the selected segment carries its own fill.
    expect(chat).toHaveClass("bg-background");
  });

  it("never draws the pill on the connected variant", () => {
    render(<GroupHarness variant="connected" />);
    const root = group();
    expect(root).toHaveClass("nex-seg");
    expect(root).toHaveClass("nex-seg-connected");
    expect(pill()).toBeNull();
  });

  it("draws no pill when nothing is selected", () => {
    render(<GroupHarness initial={null} />);
    expect(group()).toHaveAttribute("data-ready", "false");
    expect(pill()).toBeNull();
    // Nothing selected: the first enabled segment anchors the tab order.
    expect(segments()[0]).not.toHaveAttribute("tabindex");
    expect(segments()[1]).toHaveAttribute("tabindex", "-1");
  });

  it("keeps the pill aria-hidden, unfocusable and outside the accessibility tree", () => {
    render(<GroupHarness />);
    const marker = pill();
    expect(marker).not.toBeNull();
    expect(marker).toHaveAttribute("aria-hidden", "true");
    expect(marker).not.toHaveAttribute("tabindex");
    expect(marker?.tagName).toBe("SPAN");
    expect(marker).toHaveTextContent("");
    // Only the real segments are widgets.
    expect(segments()).toHaveLength(2);
  });
});

describe("semantics are unchanged by the pill", () => {
  it("keeps radiogroup semantics, roving tabindex and arrow-key activation", () => {
    render(<GroupHarness />);
    const root = group();
    expect(root).toHaveClass("nex-seg");
    const [chat, agent] = segments();
    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(agent).toHaveAttribute("aria-checked", "false");
    expect(chat).not.toHaveAttribute("tabindex");
    expect(agent).toHaveAttribute("tabindex", "-1");
    expect(chat).toHaveClass("is-active");
    expect(agent).not.toHaveClass("is-active");

    fireEvent.keyDown(chat!, { key: "ArrowRight" });

    expect(agent).toHaveFocus();
    expect(agent).toHaveAttribute("aria-checked", "true");
    expect(chat).toHaveAttribute("tabindex", "-1");

    // Wraps at the end.
    fireEvent.keyDown(agent!, { key: "ArrowRight" });
    expect(chat).toHaveFocus();
    expect(chat).toHaveAttribute("aria-checked", "true");
  });

  it("keeps tablist semantics with tabs and aria-selected", () => {
    render(<GroupHarness semantics="tabs" />);
    expect(screen.getByRole("tablist", { name: "Mode" })).toBeInTheDocument();
    const tabs = screen.getAllByRole("tab");
    expect(tabs).toHaveLength(2);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    expect(tabs[0]).not.toHaveAttribute("aria-checked");
    expect(tabs[1]).toHaveAttribute("aria-selected", "false");
    expect(pill()).not.toBeNull();
  });

  it("skips a disabled segment with the arrow keys", () => {
    render(
      <GroupHarness options={THREE_WITH_DISABLED} initial="context" />,
    );
    const [, disabled, context] = segments();
    expect(disabled).toBeDisabled();

    // Backwards from Context must land on Chat, not the disabled Agent.
    fireEvent.keyDown(context!, { key: "ArrowLeft" });

    const chat = screen.getByRole("radio", { name: "Chat" });
    expect(chat).toHaveFocus();
    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(disabled).not.toHaveFocus();
  });

  it("disables every segment when the group is disabled", () => {
    render(<GroupHarness disabled />);
    for (const segment of segments()) {
      expect(segment).toBeDisabled();
    }
    expect(group()).toHaveClass("nex-seg");
  });
});

describe("components.css contract (G-18)", () => {
  const sheet = readRepoFile("src/styles/components.css").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );

  it("declares the container and pill contract the component emits", () => {
    const container = /\.nex-seg\s*\{([\s\S]*?)\n\}/.exec(sheet);
    expect(container).not.toBeNull();
    expect(container![1]).toContain("position: relative");
    expect(container![1]).toContain("display: inline-grid");
    expect(container![1]).toContain("grid-auto-flow: column");
    expect(container![1]).toContain("grid-auto-columns: minmax(0, 1fr)");

    const marker = /\.nex-seg-indicator\s*\{([\s\S]*?)\n\}/.exec(sheet);
    expect(marker).not.toBeNull();
    expect(marker![1]).toContain("position: absolute");
    expect(marker![1]).toContain("z-index: 0");
    expect(marker![1]).toContain("pointer-events: none");
    expect(marker![1]).toContain("width: var(--seg-indicator-w");
    expect(marker![1]).toContain("transform: translateX(var(--seg-indicator-x");
    expect(marker![1]).toContain("background: var(--background)");
    expect(marker![1]).toContain("border-radius: var(--seg-radius)");
    expect(marker![1]).toContain(
      "transition: transform var(--motion-duration-short4)",
    );
    expect(marker![1]).toContain("var(--motion-easing-standard)");
    // The gate in motion.css reaches transitions only, never keyframes.
    expect(marker![1]).not.toContain("animation");
  });

  it("declares ONE seat radius for the pill and the segment it covers", () => {
    // Declared once on the container, consumed by both. A second,
    // pill-only radius is how the pill resolved to 6px over a 4.8px
    // segment in Phase C, so the sheet must not grow one back.
    const container = /\.nex-seg\s*\{([\s\S]*?)\n\}/.exec(sheet);
    expect(container![1]).toContain("--seg-radius: var(--radius-sm)");
    // One declaration point in the whole sheet: no third radius to drift.
    expect(sheet.match(/--seg-radius\s*:/g)).toHaveLength(1);

    const marker = /\.nex-seg-indicator\s*\{([\s\S]*?)\n\}/.exec(sheet);
    expect(marker![1]).toContain("border-radius: var(--seg-radius)");

    const segment =
      /\.nex-seg:not\(\.nex-seg-connected\) button\s*\{([\s\S]*?)\n\}/.exec(
        sheet,
      );
    expect(segment).not.toBeNull();
    expect(segment![1]).toContain("border-radius: var(--seg-radius)");
    // No calc() on either side: identical corners, not two expressions
    // that happen to agree today.
    expect(marker![1]).not.toMatch(/border-radius:[^;]*calc\(/);
    expect(segment![1]).not.toMatch(/border-radius:[^;]*calc\(/);
    // The connected variant keeps its flush hairline, so the segment
    // radius must not reach it.
    expect(sheet).toMatch(/\.nex-seg:not\(\.nex-seg-connected\) button/);
  });

  it("keeps the pill's ring on the token and out of the component", () => {
    const marker = /\.nex-seg-indicator\s*\{([\s\S]*?)\n\}/.exec(sheet);
    // The vertical ring is the spacing token…
    expect(marker![1]).toContain("top: var(--space-xxxs)");
    expect(marker![1]).toContain("bottom: var(--space-xxxs)");
    // …and the horizontal one is the container's padding (the seat's own
    // left edge), so the pill declares no inset of its own: no `right`,
    // no `inset`, no margin, no padding that could apply it twice or pin
    // the pill flush to the track's edge.
    expect(marker![1]).not.toMatch(/\bright:/);
    expect(marker![1]).not.toMatch(/\binset:/);
    expect(marker![1]).not.toMatch(/\bmargin/);
    expect(marker![1]).not.toMatch(/\bpadding/);
  });

  it("suppresses the transition before ready and during the no-anim frame", () => {
    expect(sheet).toMatch(
      /\.nex-seg\[data-ready="false"\]\s+\.nex-seg-indicator[\s\S]*?transition:\s*none/,
    );
    expect(sheet).toMatch(
      /\.nex-seg\[data-no-anim\]\s+\.nex-seg-indicator[\s\S]*?transition:\s*none/,
    );
  });

  it("uses tokens only — no raw colour, duration or radius", () => {
    const start = sheet.indexOf(".nex-seg {");
    const lastOwnRule = sheet.indexOf('.nex-seg[data-no-anim]');
    expect(start).toBeGreaterThan(-1);
    expect(lastOwnRule).toBeGreaterThan(start);
    const end = sheet.indexOf("\n}", lastOwnRule) + 2;
    const block = sheet.slice(start, end);
    expect(block).not.toMatch(/#[0-9a-f]{3,8}\b/i);
    expect(block).not.toMatch(/\b\d+(?:\.\d+)?m?s\b/);
    expect(block).not.toMatch(/border-radius:\s*\d/);
  });
});
