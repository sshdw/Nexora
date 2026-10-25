//! M3SegmentedGroup — Glide Select (R4-08) + the `.nex-seg` contract
//! (G-18).
//!
//! jsdom ships no layout engine, so the geometry has to be supplied:
//! `offsetLeft` / `offsetWidth` are answered from two data attributes
//! a test can set per segment, which is enough for the component's
//! measurement path. ResizeObserver is absent as well; the stub below
//! records callbacks so a test can fire one on demand.
//!
//! What is pinned here:
//!   1. MEASUREMENT — the two sanctioned custom properties
//!      (`--seg-indicator-x` / `--seg-indicator-w`) follow the
//!      SELECTED segment's box, on mount, on click, on resize, after
//!      a locale swap and after `document.fonts.ready`.
//!   2. NO FLY-IN — `data-ready="false"` until the first successful
//!      measurement, and `data-no-anim` for one frame when the layout
//!      changes under a still pill. The sheet side of both is
//!      asserted from `components.css`, because jsdom computes no
//!      cascade.
//!   3. SEMANTICS — radiogroup/radio, tablist/tab, aria-checked vs
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

/** Give one element a box without a layout engine. */
function setBox(element: Element, left: number, width: number): void {
  const node = element as HTMLElement;
  node.dataset.testOffsetLeft = String(left);
  node.dataset.testOffsetWidth = String(width);
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
  vi.spyOn(HTMLElement.prototype, "offsetLeft", "get").mockImplementation(
    function (this: HTMLElement) {
      return Number(this.dataset.testOffsetLeft ?? 0);
    },
  );
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(
    function (this: HTMLElement) {
      return Number(this.dataset.testOffsetWidth ?? 0);
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
    expect(marker![1]).toContain("border-radius: var(--radius-sm)");
    expect(marker![1]).toContain(
      "transition: transform var(--motion-duration-short4)",
    );
    expect(marker![1]).toContain("var(--motion-easing-standard)");
    // The gate in motion.css reaches transitions only, never keyframes.
    expect(marker![1]).not.toContain("animation");
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
