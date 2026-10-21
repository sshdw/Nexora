//! The global press token, where it is consumed (#230 Phase 2/7).
//!
//! SYSTEM §7.2 (OD-D9·motion, owner-approved 2026-10-20) approves ONE
//! press for the whole app: scale 0.97, 90ms in, 411ms spring-snap out,
//! on ALL controls. The tokens live in src/styles/motion.css and the
//! transition itself in `.nex-press` (src/styles/components.css) — the
//! vendored button carries the class and the scale utility, because a
//! browser without `linear()` must keep the 410ms
//! emphasized-decelerate release, and only CSS can spell that
//! fallback-first declaration pair.
//!
//! A class string is the only place this can be asserted: jsdom has no
//! cascade, no Tailwind build and no `prefers-reduced-motion`, so a
//! rendered button reports the class NAME, never the pixels. So the
//! assertions here are about what the vendored file and the sheets
//! DECLARE — and they run against the rendered element too, so a
//! variant that dropped the base string on the floor would still fail.
//!
//! The one thing NOT asserted here is that Tailwind compiles the
//! arbitrary values; `npx vite build` is where that is checked, because
//! the utilities are generated from the literal class string in the file.

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { readRepoFile } from "../../test/readRepoFile";

import { Button } from "./button";

/** The vendored source, comments stripped: these assertions are
 *  about declarations, and prose that quotes a class name must
 *  not be able to satisfy one. */
const source = readRepoFile("src/components/ui/button.tsx").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  "",
);

/** The base class string as it is written, whitespace collapsed. */
const BASE = /"([^"]*group\/button[^"]*)"/.exec(source)?.[1] ?? "";

/** The token sheet, comments stripped, same reason. */
const motion = readRepoFile("src/styles/motion.css").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  "",
);

/** The sheet that owns the press transition. */
const components = readRepoFile("src/styles/components.css").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  "",
);

/** Body of a flat rule in components.css, by literal selector. */
function ruleBody(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`${escaped}\\s*\\{([^{}]*)\\}`).exec(components);
  if (!match) throw new Error(`src/styles/components.css: no rule matching ${selector}`);
  return match[1];
}

describe("press token — declared", () => {
  it("extracts the base class string it claims to", () => {
    // Anti-vacuity pin: a regex that quietly stopped matching would
    // leave every assertion below passing on an empty string.
    expect(BASE).toContain("group/button");
    expect(BASE).toContain("rounded-lg");
    expect(BASE).toMatch(/^\S+ \S+/);
  });

  it("declares both press tokens, in both name families", () => {
    // The off-ladder 90ms is part of the SYSTEM §7.2 exception; the
    // ladder itself (motion.css header) has no such stop. 0.97 is the
    // canonical press, superseding the 2026-10-15 0.985.
    expect(motion).toMatch(/--motion-duration-press:\s*90ms/);
    expect(motion).toMatch(/--motion-scale-press:\s*0\.97/);
    // Canonical aliases — new code consumes the --nex-sys-motion-*
    // names, which is what the class string and .nex-press reference.
    expect(motion).toMatch(
      /--nex-sys-motion-duration-press:\s*var\(--motion-duration-press\)/,
    );
    expect(motion).toMatch(
      /--nex-sys-motion-scale-press:\s*var\(--motion-scale-press\)/,
    );
  });

  it("declares the 411ms snap and the 120ms hover paint clock", () => {
    expect(motion).toMatch(/--nex-sys-motion-spring-snap:\s*linear\([^)]*\)\s*411ms/);
    expect(motion).toMatch(/--nex-sys-motion-duration-hover:\s*120ms/);
  });
});

describe("press token — consumed by the button", () => {
  it("presses with the scale token instead of a 1px nudge", () => {
    expect(BASE).toContain("active:scale-[var(--nex-sys-motion-scale-press)]");
    // The nudge is gone, not merely out-voted.
    expect(BASE).not.toContain("translate-y-px");
    expect(BASE).not.toContain("scale-95");
  });

  it("delegates its clock to .nex-press and keeps no transition utility", () => {
    // One clock, one owner: a Tailwind property/duration list would
    // duplicate (and out-rank) the sheet, and the fallback-first
    // declaration pair cannot be expressed in a utility at all.
    expect(BASE).toContain("nex-press");
    expect(BASE).not.toContain("transition-[");
    expect(BASE).not.toContain("[transition-duration:");
    expect(BASE).not.toContain("transition-all");
  });

  it("presses every variant, not only the primary one", () => {
    // The registry guarded the nudge with `not-aria-[haspopup]`; the
    // contract says ALL controls and no element in src/ sets
    // aria-haspopup, so the guard is gone.
    expect(BASE).not.toContain("aria-[haspopup]");
    expect(BASE).not.toContain("not-aria");
  });

  it("carries no raw duration or raw scale of its own", () => {
    // Tokens only — the values live in motion.css.
    expect(BASE).not.toMatch(/\d+ms\b/);
    expect(BASE).not.toMatch(/scale-\[0\./);
    expect(BASE).not.toMatch(/scale-9\d/);
  });
});

describe("press token — the .nex-press sheet", () => {
  it("releases on the 411ms snap, fallback declaration first", () => {
    const body = ruleBody(".nex-press");
    const fallback = body.indexOf("410ms var(--motion-easing-emphasized-decelerate)");
    const snap = body.indexOf("var(--nex-sys-motion-spring-snap)");
    expect(fallback).toBeGreaterThanOrEqual(0);
    expect(snap).toBeGreaterThanOrEqual(0);
    // Declaration order IS the fallback contract: an engine that does
    // not understand linear() drops only the second declaration.
    expect(fallback).toBeLessThan(snap);
  });

  it("presses in on the 90ms token and keeps paint on 120ms", () => {
    const active = ruleBody(".nex-press:active:not(:disabled)");
    expect(active).toContain("var(--nex-sys-motion-duration-press)");
    expect(active).toContain("var(--nex-sys-motion-duration-hover)");
    // The press never borrows the hover clock for `scale`, and never
    // borrows the press clock for paint.
    expect(ruleBody(".nex-press")).toContain("var(--nex-sys-motion-duration-hover)");
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
    const gate = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(
      motion,
    )?.[1] ?? "";
    expect(gate).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
    // And it must be the ONLY such block in the tree.
    expect(readRepoFile("src/index.css")).not.toMatch(
      /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{/,
    );
  });
});

describe("press token — on the rendered control", () => {
  it("reaches every adapter variant, so M3Button and M3IconButton inherit it", () => {
    for (const variant of ["default", "secondary", "ghost", "destructive"] as const) {
      const { container, unmount } = render(
        <Button variant={variant} size={variant === "ghost" ? "icon-sm" : "sm"}>
          press
        </Button>,
      );
      const className = container.querySelector("button")?.className ?? "";
      expect(className).toContain("active:scale-[var(--nex-sys-motion-scale-press)]");
      expect(className).toContain("motion-reduce:active:scale-none");
      expect(className).toContain("nex-press");
      unmount();
    }
  });
});
