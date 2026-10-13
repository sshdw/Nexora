import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

import {
  prefersReducedMotion,
  REDUCED_MOTION_QUERY,
  useReducedMotion,
} from "./useReducedMotion";

/**
 * Reduced-motion gate (#218).
 *
 * The invariant: under `prefers-reduced-motion: reduce` NOTHING
 * moves — not a CSS transition, not a CSS keyframe loop, and not
 * a JS timeline. This file pins both halves:
 *
 *   - CSS:  the global block in src/index.css collapses
 *           duration, delay, iteration-count and fill-mode.
 *   - JS:   a vendored React Bits component (TextType, copied in
 *           #218) branches on useReducedMotion() and renders its
 *           final static frame WITHOUT starting GSAP or the
 *           typing timer chain.
 *
 * The JS half is asserted with a matchMedia mock and a GSAP
 * spy: "no timeline was created" is the assertion, not "the
 * text happened to look finished".
 */

/** Install a matchMedia that answers `matches` for the reduced-motion
 *  query and records every listener registration. */
function mockReducedMotion(matches: boolean): { queries: string[]; added: string[] } {
  const queries: string[] = [];
  const added: string[] = [];
  vi.stubGlobal("matchMedia", (query: string) => {
    queries.push(query);
    return {
      matches: query === REDUCED_MOTION_QUERY ? matches : false,
      media: query,
      onchange: null,
      addEventListener: (type: string) => added.push(`${type}:${query}`),
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    };
  });
  return { queries, added };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("reduced-motion gate — CSS half", () => {
  // Read as text: this half of the gate is a CSS block, and the
  // assertion is about what that block declares, not about what
  // the bundler made of it.
  const css = readRepoFile("src/index.css");
  const reducedBlock = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(css);

  it("ships a prefers-reduced-motion block", () => {
    expect(reducedBlock).not.toBeNull();
  });

  it("collapses durations and delays to a single frame", () => {
    const block = reducedBlock![1];
    expect(block).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
    expect(block).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    expect(block).toMatch(/animation-delay:\s*0ms\s*!important/);
    expect(block).toMatch(/transition-delay:\s*0ms\s*!important/);
  });

  it("freezes looping keyframes at one iteration", () => {
    // The app's skeleton pulse, progress slide and the
    // tw-animate-css blink loops are all `infinite`. Without this
    // line they keep looping while the durations are collapsed.
    expect(reducedBlock![1]).toMatch(/animation-iteration-count:\s*1\s*!important/);
  });

  it("holds the end frame so a collapsed entrance cannot flash back to its start", () => {
    expect(reducedBlock![1]).toMatch(/animation-fill-mode:\s*forwards\s*!important/);
  });

  it("reaches pseudo-elements and wins over utilities", () => {
    expect(reducedBlock![1]).toMatch(/\*,\s*\*::before,\s*\*::after/);
    expect(reducedBlock![1]).toMatch(/!important/);
  });
});

describe("reduced-motion gate — JS half", () => {
  it("reports the OS preference synchronously", () => {
    const { queries } = mockReducedMotion(true);
    expect(prefersReducedMotion()).toBe(true);
    expect(queries).toContain(REDUCED_MOTION_QUERY);
  });

  it("fails closed when matchMedia is unavailable", () => {
    vi.stubGlobal("matchMedia", undefined);
    expect(prefersReducedMotion()).toBe(true);
  });

  it("re-renders when the preference changes mid-session", () => {
    const { added } = mockReducedMotion(false);
    let value: boolean | null = null;
    function Probe() {
      value = useReducedMotion();
      return null;
    }
    render(<Probe />);
    expect(value).toBe(false);
    expect(added).toContain(`change:${REDUCED_MOTION_QUERY}`);
  });

  it("renders TextType's final static frame and starts no GSAP tween", async () => {
    // GSAP is mocked rather than driven: the assertion is that no
    // timeline is ever created, so a spy is the honest check.
    const gsapMock = { set: vi.fn(), to: vi.fn() };
    vi.doMock("gsap", () => ({ gsap: gsapMock }));
    mockReducedMotion(true);
    vi.resetModules();

    const { default: ReducedTextType } = await import("../components/TextType");

    render(<ReducedTextType text={["first", "final sentence"]} loop />);

    await act(async () => {
      // No timing assertion: a real reduced-motion render is
      // synchronous, and the timers below would never fire.
      await Promise.resolve();
    });

    // The information the animation would have revealed is all
    // present in the static frame.
    expect(screen.getByText("final sentence")).toBeInTheDocument();
    // No GSAP blink loop, and no typing cursor left behind.
    expect(gsapMock.to).not.toHaveBeenCalled();
    expect(gsapMock.set).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("|");

    vi.doUnmock("gsap");
  });

  it("runs the TextType timeline when motion is allowed", async () => {
    const gsapMock = { set: vi.fn(), to: vi.fn() };
    vi.doMock("gsap", () => ({ gsap: gsapMock }));
    mockReducedMotion(false);
    vi.resetModules();

    const { default: AnimatedTextType } = await import("../components/TextType");

    render(<AnimatedTextType text="typing" />);

    // The cursor blink tween is created on mount when motion is
    // allowed — the counterpart to the assertion above.
    expect(gsapMock.set).toHaveBeenCalledTimes(1);
    expect(gsapMock.to).toHaveBeenCalledTimes(1);

    vi.doUnmock("gsap");
  });
});