//! The global press token, where it is consumed (#230 Phase 2).
//!
//! NEXORA-THEME.md §6 approves ONE press for the whole app:
//! scale(0.985), 90ms in, instant out, on ALL controls. The two
//! `--motion-*` tokens it needs were declared in
//! src/styles/motion.css by an earlier pass of this phase, and
//! nothing read them — the button the whole app presses with
//! (ui/button, via M3Button / M3IconButton) still carried the
//! registry's 1px `active:translate-y-px`.
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
  "",
);

/** The base class string as it is written, whitespace collapsed. */
const BASE = /"([^"]*group\/button[^"]*)"/.exec(source)?.[1] ?? "";

/** The token sheet, comments stripped, same reason. */
const motion = readRepoFile("src/styles/motion.css").replace(
  /(?:\/\*[\s\S]*?\*\/|\/\/[^\n]*)/g,
  "",
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
    expect(motion).toMatch(/--motion-scale-press:\s*0\.985/);
    // Canonical aliases — new code consumes the --nex-sys-motion-*
    // names, which is what the class string below references.
    expect(motion).toMatch(
      /--nex-sys-motion-duration-press:\s*var\(--motion-duration-press\)/,
    );
    expect(motion).toMatch(
      /--nex-sys-motion-scale-press:\s*var\(--motion-scale-press\)/,
    );
  });
});

describe("press token — consumed by the button", () => {
  it("presses with the scale token instead of a 1px nudge", () => {
    expect(BASE).toContain("active:scale-[var(--nex-sys-motion-scale-press)]");
    // The nudge is gone, not merely out-voted.
    expect(BASE).not.toContain("translate-y-px");
    expect(BASE).not.toContain("scale-95");
  });

  it("gives the press its own clock: 90ms in, 0s out", () => {
    // `scale` is the FIRST entry of the property list, so the first
    // duration is its own — that alignment is the whole mechanic
    // (and the reason the list is spelled out twice).
    expect(BASE).toMatch(/transition-\[scale,[^\]]*opacity\]/);
    // At rest: instant release, no travel back.
    expect(BASE).toMatch(/\[transition-duration:0s,/);
    // Held: the press token, with the paint transitions untouched.
    expect(BASE).toMatch(
      /active:\[transition-duration:var\(--nex-sys-motion-duration-press\),/,
    );
    // Every paint entry stays on the state-fade token.
    const fast = (BASE.match(/var\(--nex-sys-motion-duration-fast\)/g) ?? []).length;
    expect(fast).toBe(10);
  });

  it("presses every variant, not only the primary one", () => {
    // The registry guarded the nudge with `not-aria-[haspopup]`; the
    // contract says ALL controls and no element in src/ sets
    // aria-haspopup, so the guard is gone.
    expect(BASE).not.toContain("aria-[haspopup]");
    expect(BASE).not.toContain("not-aria");
  });

  it("carries no raw duration or raw scale of its own", () => {
    // Tokens only. 0s is the approved INSTANT release, not a value.
    expect(BASE).not.toMatch(/\d+ms\b/);
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
      unmount();
    }
  });
});
