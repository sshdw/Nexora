//! The icon layer's invariants (D5).
//!
//! `icons.tsx` is the ONE place a glyph may be drawn. Everything else imports
//! an exported component, so the library, the weight and the size grid can
//! change in a single file. These pins are deliberately structural — a scan of
//! the source for a stray `<svg>` or a second Phosphor import, plus a render
//! check of the shared wrapper — because jsdom cannot see "this icon came from
//! the library" any other way.

import { readdirSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { cleanup, render } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";
import { ContextRingIcon, PlusIcon, type UiIconSize } from "./icons";

afterEach(cleanup);

const SRC = resolve(process.cwd(), "src");
const ICON_LAYER = "src/components/icons.tsx";

function tsxFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) return tsxFiles(full);
    return entry.name.endsWith(".tsx") ? [full] : [];
  });
}

/** Comments are prose, not code: a doc comment that names `<svg>` must not
 *  satisfy or fail a pin. */
function code(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^\s*\/\/.*$/gm, "");
}

const SOURCES = tsxFiles(SRC).map((full) => ({
  path: relative(process.cwd(), full).replace(/\\/g, "/"),
  body: code(readRepoFile(relative(process.cwd(), full).replace(/\\/g, "/"))),
}));

describe("icon layer — one source of truth", () => {
  it("draws no raw <svg> outside the icon layer", () => {
    const offenders = SOURCES.filter(
      (file) => file.path !== ICON_LAYER && /<svg[\s>]/.test(file.body),
    ).map((file) => file.path);
    expect(offenders).toEqual([]);
  });

  it("is the only file that imports the icon library", () => {
    const importers = SOURCES.filter((file) => file.body.includes("@phosphor-icons/react")).map(
      (file) => file.path,
    );
    expect(importers).toEqual([ICON_LAYER]);
  });

  it("fixes the weight, the default size and the size grid in one place", () => {
    const source = readRepoFile(ICON_LAYER);
    // Weight is never a call-site prop.
    expect(source).toMatch(/weight="regular"/);
    expect(source).toMatch(/Omit<IconProps, "weight" \| "size" \| "ref">/);
    expect(source).toMatch(/size = 16/);
    expect(source).toMatch(/type UiIconSize = 16 \| 20/);
  });

  it("marks every glyph decorative so its button carries the name", () => {
    const source = readRepoFile(ICON_LAYER);
    expect(source).toMatch(/aria-hidden="true"/);
    expect(source).toMatch(/focusable="false"/);
    expect(source).toMatch(/color="currentColor"/);
  });

  it("draws the custom ring on tokens, never a literal colour", () => {
    const source = readRepoFile(ICON_LAYER);
    const start = source.indexOf("export function ContextRingIcon");
    expect(start).toBeGreaterThan(-1);
    const body = source.slice(start, source.indexOf("\n}", start));
    // The track is the control's boundary, so it must be a token that the
    // theme can retarget — a hex here is how the ring went invisible on dark.
    expect(body).toContain('stroke="var(--muted-foreground)"');
    expect(body).toContain('stroke="currentColor"');
    expect(body).not.toMatch(/#[0-9a-f]{3,8}\b/i);
    expect(body).not.toMatch(/\brgba?\(/);
    expect(body).not.toMatch(/\boklch\(/);
  });
});

describe("icon layer — rendered contract", () => {
  it("renders a library glyph on the 16 grid", () => {
    const { container } = render(createElement(PlusIcon));
    const svg = container.querySelector("svg");
    expect(svg).not.toBeNull();
    expect(svg).toHaveAttribute("width", "16");
    expect(svg).toHaveAttribute("height", "16");
    expect(svg).toHaveAttribute("aria-hidden", "true");
    expect(svg).toHaveAttribute("focusable", "false");
    // Weight is consumed by Phosphor to choose the path set; it is not a DOM
    // attribute, so the source pin above is what keeps it "regular". What the
    // DOM can prove is that the glyph is ink, not a hard-coded colour.
    expect(svg).toHaveAttribute("fill", "currentColor");
  });

  it("accepts the 20 step and nothing between", () => {
    const size: UiIconSize = 20;
    const { container } = render(createElement(PlusIcon, { size }));
    const svg = container.querySelector("svg");
    expect(svg).toHaveAttribute("width", "20");
    expect(svg).toHaveAttribute("height", "20");
  });

  it("draws the context ring as data: dashed with no value, an arc with one", () => {
    const empty = render(createElement(ContextRingIcon, { percent: null }));
    const emptySvg = empty.container.querySelector("svg");
    expect(emptySvg).toHaveAttribute("data-state", "na");
    expect(emptySvg?.querySelectorAll("circle")).toHaveLength(1);
    expect(emptySvg?.querySelector("circle")).toHaveAttribute("stroke-dasharray", "2 2");
    empty.unmount();

    const half = render(createElement(ContextRingIcon, { percent: 50 }));
    const halfSvg = half.container.querySelector("svg");
    expect(halfSvg).toHaveAttribute("data-state", "value");
    expect(halfSvg?.querySelectorAll("circle")).toHaveLength(2);
  });

  it("clamps a percentage that arrives out of range", () => {
    const over = render(createElement(ContextRingIcon, { percent: 140 }));
    const overArc = over.container.querySelectorAll("circle")[1];
    const fullArc = render(createElement(ContextRingIcon, { percent: 100 })).container.querySelectorAll(
      "circle",
    )[1];
    expect(overArc?.getAttribute("stroke-dasharray")).toBe(fullArc?.getAttribute("stroke-dasharray"));
  });
});
