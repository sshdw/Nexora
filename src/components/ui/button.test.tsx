//! The global press token, where it is consumed (#230 Phase 2).
//!
//! NEXORA-THEME.md §6 approved ONE press for the whole app:
//! scale(0.985), 90ms in, instant out, on ALL controls. P7 UPDATE
//! (OD-D9·motion, owner-approved 2026-10-20 — review 06 D-03 marks the
//! 2026-10-15 values SUPERSEDED): 90ms in to scale(0.97) (the l2
//! default; the data-motion-level tiers in src/styles/motion.css
//! override the press scale per level), and a 411ms spring-snap release
//! instead of instant-out. Hover/focus paint moves onto the 120ms
//! `--nex-sys-motion-duration-hover`. This file pins the shipped
//! contract — the OD-D9 press, not the historical one.
//!
//! A class string is the only place this can be asserted: jsdom
//! has no cascade, no Tailwind build and no `prefers-reduced-
//! motion`, so a rendered button reports the class NAME, never
//! the pixels. So the assertions here are about what the vendored
//! file and the token sheet DECLARE — and they run against the
//! rendered element too, so a variant that dropped the base
//! string on the floor would still fail.
//!
//! The one thing NOT asserted here is that Tailwind compiles the
//! arbitrary values; `npx vite build` is where that is checked,
//! because the utilities are generated from the literal class
//! string in the file.

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { readRepoFile } from "../../test/readRepoFile";

import { Button } from "./button";

/** The vendored source, comments stripped: these assertions are
 *  about declarations, and prose that quotes a class name must
 *  not be able to satisfy one. */
const source = readRepoFile("src/components/ui/button.tsx").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  ""
);

/** The base class string as it is written, whitespace collapsed. */
const BASE = /"([^"]*group\/button[^"]*)"/.exec(source)?.[1] ?? "";

/** The token sheet, comments stripped, same reason. */
const motion = readRepoFile("src/styles/motion.css").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  ""
);

describe("press token — declared", () => {
  it("extracts the base class string it claims to", () => {
    // Anti-vacuity pin: a regex that quietly stopped matching would
    // leave every assertion below passing on an empty string.
    expect(BASE).toContain("group/button");
    expect(BASE).toContain("rounded-lg");
    expect(BASE).toMatch(/^\S+ \S+/);
  });

  it("declares both press tokens, in both name families", () => {
    // The off-ladder 90ms is approved in NEXORA-THEME.md §6; the
    // ladder itself (motion.css header) has no such stop.
    expect(motion).toMatch(/--motion-duration-press:\s*90ms/);
    // The scale default moved 0.985 → 0.97: OD-D9·motion (2026-10-20)
    // supersedes the 2026-10-15 press, and 0.97 is the l2 «плавный»
    // value of the adaptive levels (the tier blocks override it).
    expect(motion).toMatch(/--motion-scale-press:\s*0\.97/);
    // Canonical aliases — new code consumes the --nex-sys-motion-*
    // names, which is what the class string below references.
    expect(motion).toMatch(
      /--nex-sys-motion-duration-press:\s*var\(\s*--motion-duration-press\s*\)/
    );
    expect(motion).toMatch(
      /--nex-sys-motion-scale-press:\s*var\(\s*--motion-scale-press\s*\)/
    );
  });

  it("declares the OD-D9 spring, hover and release tokens", () => {
    // §7.1 freeze exceptions — settle, snap, 120ms hover — plus the
    // release pair the press row of §7.2 prescribes.
    expect(motion).toMatch(/--nex-sys-motion-spring-settle:\s*linear\(/);
    expect(motion).toMatch(/--nex-sys-motion-spring-snap:\s*linear\(/);
    expect(motion).toMatch(/--nex-sys-motion-spring-settle-duration:\s*343ms/);
    expect(motion).toMatch(/--nex-sys-motion-spring-snap-duration:\s*411ms/);
    expect(motion).toMatch(/--nex-sys-motion-duration-hover:\s*120ms/);
    expect(motion).toMatch(
      /--nex-sys-motion-press-release-duration:\s*var\(\s*--nex-sys-motion-spring-snap-duration\s*\)/
    );
    expect(motion).toMatch(
      /--nex-sys-motion-press-release-easing:\s*var\(\s*--nex-sys-motion-spring-snap\s*\)/
    );
  });

  it("keeps the §7.1 fallback declarations ahead of the linear() ones", () => {
    // Fallback FIRST, linear() SECOND, no @supports (§7.1 verbatim).
    // settle's fallback is the expressive-default-spatial pair; snap's
    // is emphasized-decelerate at 410ms, 0% overshoot. Patterns, not
    // substrings: the printer may wrap a var() call across lines, and
    // only whitespace may sit between the tokens the contract names.
    const settleFallback = motion.search(
      /--nex-sys-motion-spring-settle:\s*var\(\s*--motion-spring-expressive-default-spatial-curve\s*\)/
    );
    const settleLinear = motion.search(
      /--nex-sys-motion-spring-settle:\s*linear\(/
    );
    const snapFallback = motion.search(
      /--nex-sys-motion-spring-snap:\s*var\(\s*--motion-easing-emphasized-decelerate\s*\)/
    );
    const snapLinear = motion.search(
      /--nex-sys-motion-spring-snap:\s*linear\(/
    );
    expect(settleFallback).toBeGreaterThanOrEqual(0);
    expect(settleFallback).toBeLessThan(settleLinear);
    expect(snapFallback).toBeGreaterThanOrEqual(0);
    expect(snapFallback).toBeLessThan(snapLinear);
  });
});

describe("press token — consumed by the button", () => {
  it("presses with the scale token instead of a 1px nudge", () => {
    expect(BASE).toContain("active:scale-[var(--nex-sys-motion-scale-press)]");
    // The nudge is gone, not merely out-voted.
    expect(BASE).not.toContain("translate-y-px");
    expect(BASE).not.toContain("scale-95");
  });

  it("gives the press its own clock: 90ms in, spring-snap out, hover paint at 120ms", () => {
    // `scale` is the FIRST entry of the property list, so the first
    // duration is its own — that alignment is the whole mechanic
    // (and the reason the lists are spelled out per state).
    const properties =
      /transition-\[([^\]]+)\]/.exec(BASE)?.[1].split(",") ?? [];
    expect(properties[0]).toBe("scale");
    // The rest of the list is paint only: fill, text, border, shadow,
    // opacity. Nothing that moves, so nothing a press can re-clock —
    // and where the snap release must never land (§7.1: snap is for
    // scale moments only, never opacity).
    for (const property of properties.slice(1)) {
      expect(property).toMatch(
        /^(background-color|color|border-color|box-shadow|opacity)$/
      );
    }
    // Duration and timing lists line up with the property list
    // positionally, so the contract is read per entry — not by
    // counting how many times a token appears, which is a number that
    // silently rots the next time a property is added or dropped.
    const HOVER = "var(--nex-sys-motion-duration-hover)";
    const PRESS = "var(--nex-sys-motion-duration-press)";
    const RELEASE_DURATION = "var(--nex-sys-motion-press-release-duration)";
    const RELEASE_EASING = "var(--nex-sys-motion-press-release-easing)";
    const STANDARD = "var(--nex-sys-motion-easing-standard)";
    /** The list `utility` spells out, one entry per transitioned property. */
    const list = (utility: string, kind: string): string[] =>
      new RegExp(`(?:^|\\s)${utility}\\[transition-${kind}:([^\\]]+)\\]`)
        .exec(BASE)?.[1]
        .split(",") ?? [];
    // At rest: the release owns scale (411ms snap — OD-D9 replaced the
    // 2026-10-15 instant 0s release); paint rides the 120ms hover
    // clock with the standard easing.
    expect(list("", "duration")).toEqual([
      RELEASE_DURATION,
      ...properties.slice(1).map(() => HOVER),
    ]);
    expect(list("", "timing-function")).toEqual([
      RELEASE_EASING,
      ...properties.slice(1).map(() => STANDARD),
    ]);
    // Held: the 90ms press token takes the scale clock with the
    // standard easing, and ONLY there — the paint entries keep the
    // same token they had at rest, so a press cannot speed up the
    // hover/focus fade it sits on top of.
    expect(list("active:", "duration")).toEqual([
      PRESS,
      ...properties.slice(1).map(() => HOVER),
    ]);
    expect(list("active:", "timing-function")).toEqual(
      properties.map(() => STANDARD)
    );
  });

  it("presses every variant, not only the primary one", () => {
    // The registry guarded the nudge with `not-aria-[haspopup]`; the
    // contract says ALL controls and no element in src/ sets
    // aria-haspopup, so the guard is gone.
    expect(BASE).not.toContain("aria-[haspopup]");
    expect(BASE).not.toContain("not-aria");
  });

  it("carries no raw duration or raw scale of its own", () => {
    // Tokens only: every clock and scale on the base string resolves
    // through a --nex-sys-motion-* token (the tier blocks in
    // motion.css decide the actual values per adaptive level).
    expect(BASE).not.toMatch(/\d+ms\b/);
    expect(BASE).not.toMatch(/\d+s[,\]]/);
    expect(BASE).not.toMatch(/scale-\[0\./);
    expect(BASE).not.toMatch(/scale-9\d/);
  });
});

describe("press token — reduced motion", () => {
  it("removes the press scale under prefers-reduced-motion", () => {
    // The hand-written half. motion.css's global gate collapses the
    // DURATION (so the press cannot animate); this removes the
    // scale outright, so a held control does not shrink at all.
    expect(BASE).toContain("motion-reduce:active:scale-none");
  });

  it("leans on the one global gate for the timing half", () => {
    // `scale` is transitioned, so the gate's
    // `transition-duration: 0.01ms !important` is what flattens the
    // 90ms. Both halves are needed and neither is optional.
    const gate =
      /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(
        motion
      )?.[1] ?? "";
    expect(gate).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    // And it must be the ONLY such block in the tree.
    expect(readRepoFile("src/index.css")).not.toMatch(
      /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{/
    );
  });
});

describe("press token — on the rendered control", () => {
  it("reaches every adapter variant, so M3Button and M3IconButton inherit it", () => {
    for (const variant of [
      "default",
      "secondary",
      "ghost",
      "destructive",
    ] as const) {
      const { container, unmount } = render(
        <Button variant={variant} size={variant === "ghost" ? "icon-sm" : "sm"}>
          press
        </Button>
      );
      const className = container.querySelector("button")?.className ?? "";
      expect(className).toContain(
        "active:scale-[var(--nex-sys-motion-scale-press)]"
      );
      expect(className).toContain("motion-reduce:active:scale-none");
      unmount();
    }
  });
});
