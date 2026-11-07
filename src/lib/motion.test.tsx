import { render } from "@testing-library/react";
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
 *   - CSS:  the ONE global block, in src/styles/motion.css,
 *           collapses duration, delay and iteration-count, and
 *           holds the end frame.
 *   - JS:   useReducedMotion() is the fail-closed clock every
 *           non-CSS animator branches on.
 *
 * The JS half used to be asserted against the vendored per-character
 * typing component and its timeline library with a tween spy; P7
 * deleted both from the tree (review 06 D-15) — no product component
 * imports a JS timeline any more, so the hook tests below are the
 * surviving JS half, alongside the P7 motion-contract gates for the
 * token sheet.
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
  //
  // src/styles/motion.css, NOT src/index.css. There used to be a
  // second copy of this block in the Tailwind entry — with the same
  // properties, the same values and no coverage the other file did
  // not already have. Two copies drift, and the tests below would
  // have kept reading whichever one they were written against, so
  // #218 folded both into motion.css and the entry points here.
  const css = readRepoFile("src/styles/motion.css");
  const reducedBlock = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(css);
  // Comments are stripped before matching: these assertions are
  // about declarations, and a prose line that happens to contain
  // "animation-fill-mode: forwards !important" must not satisfy
  // one.
  const declarations = reducedBlock?.[1].replace(/\/\*[\s\S]*?\*\//g, "") ?? "";

  it("ships a prefers-reduced-motion block", () => {
    expect(reducedBlock).not.toBeNull();
    expect(declarations).not.toBe("");
  });

  it("keeps the block in ONE file", () => {
    // The entry stylesheet must not grow a second reduced-motion
    // block. motion.css is the one that ships. (Comments stripped:
    // the entry POINTS at the gate in prose, and prose is not a
    // rule.)
    const entry = readRepoFile("src/index.css").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(entry).not.toMatch(/@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{[\s\S]*?\n\}/);
  });

  it("collapses durations and delays to a single frame", () => {
    expect(declarations).toMatch(/animation-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    expect(declarations).toMatch(/animation-delay:\s*0ms\s*!important/);
    expect(declarations).toMatch(/transition-delay:\s*0ms\s*!important/);
  });

  it("freezes looping keyframes at one iteration", () => {
    // The app's skeleton pulse and progress slide are both
    // `infinite`. Without this line they keep looping while the
    // durations are collapsed.
    expect(declarations).toMatch(/animation-iteration-count:\s*1\s*!important/);
  });

  it("holds the end frame so a collapsed entrance cannot flash back to its start", () => {
    expect(declarations).toMatch(/animation-fill-mode:\s*forwards\s*!important/);
  });

  it("reaches pseudo-elements and wins over utilities", () => {
    expect(declarations).toMatch(/\*,\s*\*::before,\s*\*::after/);
    expect(declarations).toMatch(/!important/);
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

  // P7 (review 06 D-15): the three component tests that lived here —
  // the reduced-motion static frame, the motion-allowed tween, and the
  // empty-list guard — tested the vendored per-character typing
  // component. That component and its timeline library left the tree
  // with zero product importers, so those tests left with it (tests of
  // deleted code, not of live code). The JS half of the gate is now the
  // three useReducedMotion tests above; the CSS half below is unchanged.
});

/**
 * P7 motion contract — OD-D9·motion / OD-D10·motion
 * (NEXORA-DESIGN-SYSTEM.md §7.1–§7.3, review 06 D-03/D-10).
 *
 * The spring tokens are DERIVED, not invented: T = −ln(0.001)/(ζωₙ)
 * (0.1% settle time), peak = e^(−πζ/√(1−ζ²)), t_p = π/(ωₙ·√(1−ζ²)).
 * These tests re-derive all three from (ζ, ωₙ) and hold the
 * declarations in src/styles/motion.css against them, so a
 * hand-tuned "spring" that drifts from the contract fails here.
 * The t_p crest must be its own linear() stop — interpolation
 * between neighbours would clip it (§7.1).
 */
describe("motion contract — spring tokens (OD-D9 §7.1)", () => {
  const css = readRepoFile("src/styles/motion.css").replace(/\/\*[\s\S]*?\*\//g, "");

  /** Re-derivation of the §7.1 table from the contract's (ζ, ωₙ). */
  function derive(zeta: number, wn: number): { T: number; peak: number; tp: number } {
    return {
      T: -Math.log(0.001) / (zeta * wn),
      peak: Math.exp((-Math.PI * zeta) / Math.sqrt(1 - zeta * zeta)),
      tp: Math.PI / (wn * Math.sqrt(1 - zeta * zeta)),
    };
  }

  /** Parse a token's linear() stop list as (progress, percent) pairs. */
  function stops(token: string): Array<[number, number]> {
    const match = new RegExp(`--nex-sys-motion-spring-${token}:\\s*linear\\(([^)]*)\\)`).exec(
      css,
    );
    expect(match, `--nex-sys-motion-spring-${token} must carry a linear() value`).not.toBeNull();
    return match![1].split(",").map((entry) => {
      const parts = entry.trim().split(/\s+/);
      return [Number(parts[0]), parts.length > 1 ? Number(parts[1].replace("%", "")) : 0];
    });
  }

  for (const [token, zeta, wn] of [
    ["settle", 0.84, 24],
    ["snap", 0.7, 24],
  ] as const) {
    it(`re-derives ${token} (ζ=${zeta}, ωₙ=${wn}) from the declaration`, () => {
      const { T, peak, tp } = derive(zeta, wn);
      // The paired duration is the settle time, rounded to the ms the
      // contract table prints (343 / 411).
      expect(css).toMatch(
        new RegExp(`--nex-sys-motion-spring-${token}-duration:\\s*${Math.round(T * 1000)}ms`),
      );
      const list = stops(token);
      // The trajectory starts at 0 and lands exactly at rest.
      expect(list[0][0]).toBe(0);
      expect(list[list.length - 1][1]).toBe(100);
      expect(list[list.length - 1][0]).toBe(1);
      // The crest is its own stop, at the re-derived t_p (as a percent
      // of T, 2dp) with the re-derived overshoot (1 + peak, 4dp).
      const crestPercent = Number(((tp * 1000 / Math.round(T * 1000)) * 100).toFixed(2));
      const crest = list.find(([, percent]) => percent === crestPercent);
      expect(crest, `crest stop missing at ${crestPercent}%`).toBeDefined();
      expect(crest![0]).toBeCloseTo(1 + peak, 4);
      // ...and it IS the crest: nothing in the list overshoots further.
      const max = Math.max(...list.map(([value]) => value));
      expect(max).toBeCloseTo(1 + peak, 4);
    });
  }

  it("pairs snap's fallback with emphasized-decelerate at 410ms, 0% overshoot", () => {
    // §7.1 fallback row, verbatim.
    expect(css).toMatch(
      /--nex-sys-motion-spring-snap-duration:\s*410ms/,
    );
    expect(css).toMatch(
      /--nex-sys-motion-spring-snap:\s*var\(--motion-easing-emphasized-decelerate\)/,
    );
    // settle's fallback is the expressive-default-spatial pair (1.39%).
    expect(css).toMatch(
      /--nex-sys-motion-spring-settle:\s*var\(--motion-spring-expressive-default-spatial-curve\)/,
    );
    expect(css).toMatch(
      /--nex-sys-motion-spring-settle-duration:\s*var\(--motion-spring-expressive-default-spatial-duration\)/,
    );
  });

  it("adds the 120ms hover token as the off-ladder freeze exception", () => {
    // §7.2 hover row + NEXORA-THEME.md §6 2026-10-20 amendment:
    // hover/focus is 120 ms, not the 150ms short3 stop.
    expect(css).toMatch(/--nex-sys-motion-duration-hover:\s*120ms/);
  });
});

describe("motion contract — adaptive levels (OD-D10 §7.3)", () => {
  const css = readRepoFile("src/styles/motion.css").replace(/\/\*[\s\S]*?\*\//g, "");

  it("declares exactly three tier blocks on the html attribute", () => {
    // Exactly THREE override blocks (§7.3). Tier blocks sit at column 0;
    // the reduced-motion guard's selector list also names the attribute
    // but indented, so the line-start anchor counts blocks only.
    expect(
      css.match(/^:root\[data-motion-level="l[123]"\]\s*\{/gm)?.length,
    ).toBe(3);
    for (const level of ["l3", "l2", "l1"]) {
      expect(css).toMatch(new RegExp(`:root\\[data-motion-level="${level}"\\]\\s*\\{`));
    }
  });

  it("keeps L2 «плавный» as the no-attribute default", () => {
    // Before src/lib/motionLevel.ts writes the attribute, :root IS l2.
    expect(css).toMatch(
      /--nex-sys-motion-entrance-easing:\s*var\(--nex-sys-motion-spring-settle\)/,
    );
    expect(css).toMatch(/--nex-sys-motion-entrance-travel:\s*8px/);
    expect(css).toMatch(/--motion-scale-press:\s*0\.97/);
  });

  it("maps character values per level — snap/settle/220ms, 12/8/4px, 0.955/0.97/0.985", () => {
    // Only CHARACTER adapts (§7.3): press, entrances, travel.
    const block = (level: string): string =>
      new RegExp(`:root\\[data-motion-level="${level}"\\]\\s*\\{([^}]*)\\}`).exec(css)?.[1] ??
      "";
    const l3 = block("l3");
    expect(l3).toMatch(/--motion-scale-press:\s*0\.955/);
    expect(l3).toMatch(/--nex-sys-motion-entrance-easing:\s*var\(--nex-sys-motion-spring-snap\)/);
    expect(l3).toMatch(/--nex-sys-motion-entrance-travel:\s*12px/);
    const l2 = block("l2");
    expect(l2).toMatch(/--motion-scale-press:\s*0\.97/);
    expect(l2).toMatch(
      /--nex-sys-motion-entrance-easing:\s*var\(--nex-sys-motion-spring-settle\)/,
    );
    expect(l2).toMatch(/--nex-sys-motion-entrance-travel:\s*8px/);
    const l1 = block("l1");
    expect(l1).toMatch(/--motion-scale-press:\s*0\.985/);
    expect(l1).toMatch(
      /--nex-sys-motion-entrance-easing:\s*var\(--nex-sys-motion-easing-emphasized-decelerate\)/,
    );
    expect(l1).toMatch(/--nex-sys-motion-entrance-duration:\s*220ms/);
    expect(l1).toMatch(/--nex-sys-motion-entrance-travel:\s*4px/);
    // L1 has no spring, so the press RELEASE is also 220ms
    // emphasized-decelerate (§7.3 derivation).
    expect(l1).toMatch(/--nex-sys-motion-press-release-duration:\s*220ms/);
    expect(l1).toMatch(
      /--nex-sys-motion-press-release-easing:\s*var\(--nex-sys-motion-easing-emphasized-decelerate\)/,
    );
    // L2/L3 keep the snap release (no release override in their blocks).
    expect(block("l2")).not.toMatch(/press-release/);
    expect(block("l3")).not.toMatch(/press-release/);
  });

  it("always yields L1 under prefers-reduced-motion, manual choice included", () => {
    // The SECOND reduced-motion block in motion.css (the first is the
    // global gate, pinned by the tests above) pins the L1 character on
    // :root AND on every tier attribute, so a manual l3 attribute
    // cannot out-rank the OS preference and the values hold pre-JS.
    const blocks = [
      ...css.matchAll(/@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/g),
    ];
    expect(blocks.length).toBe(2);
    const guard = blocks[1][1];
    expect(guard).toMatch(/:root,\s*:root\[data-motion-level="l3"\]/);
    expect(guard).toMatch(/--motion-scale-press:\s*0\.985/);
    expect(guard).toMatch(/--nex-sys-motion-entrance-travel:\s*4px/);
    expect(guard).toMatch(
      /--nex-sys-motion-entrance-easing:\s*var\(--nex-sys-motion-easing-emphasized-decelerate\)/,
    );
    expect(guard).toMatch(/--nex-sys-motion-press-release-duration:\s*220ms/);
  });
});
