import { describe, expect, it } from "vitest";

import { contrastOnWash, contrastOver, contrastRatio, parseOklch, type Oklch } from "./test/color";
import { readRepoFile } from "./test/readRepoFile";

// Read as text, not as a style injection: these assertions are
// about what the files DECLARE.
const css = readRepoFile("src/index.css");
const componentSources: Record<string, string> = {
  button: readRepoFile("src/components/ui/button.tsx"),
  slider: readRepoFile("src/components/ui/slider.tsx"),
  tooltip: readRepoFile("src/components/ui/tooltip.tsx"),
  textType: readRepoFile("src/components/TextType.tsx"),
};

/**
 * Theme contract for the black theme in src/index.css (#218).
 *
 * Tailwind resolves theme vars LAZILY: a `bg-primary` whose
 * `--primary` was never defined compiles cleanly and renders as
 * transparent. Nothing in the build or in `tsc` catches that, so
 * this file is the gate that does. It asserts three things:
 *
 *   1. COVERAGE   every var the `@theme inline` mapping aliases,
 *                 and every var a vendored component references,
 *                 is actually defined in the stylesheet.
 *   2. TYPED      every colour-bearing role parses as an OKLCH
 *                 value, so the contrast assertions below are
 *                 real measurements rather than string matches.
 *   3. DIRECTION  the canvas is near-black, one cold accent
 *                 carries the identity, the role ladder holds, and
 *                 the text pairs clear WCAG AA.
 *
 * The numbers asserted here are computed from the stylesheet by
 * src/test/color.ts — nothing here is transcribed from the
 * comment block in the CSS, which can drift.
 */

/** All custom properties DEFINED in the file, as `--name -> value`. */
function definedVars(): Map<string, string> {
  const out = new Map<string, string>();
  // A declaration is `--name: value;`. Negative values, `calc()`
  // and `var()` fall back are fine — only `--*: ` at the start of
  // a declaration counts.
  for (const match of css.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    out.set(match[1], match[2].trim());
  }
  return out;
}

/** Every custom property REFERENCE anywhere in the file. */
function referencedVars(): Set<string> {
  const out = new Set<string>();
  for (const match of css.matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
    out.add(match[1]);
  }
  return out;
}

const defined = definedVars();

/** The @theme inline mapping block, as `--alias -> var(--target)`. */
function themeMapping(): Map<string, string> {
  const block = /@theme inline \{([\s\S]*?)\n\}/.exec(css);
  if (!block) throw new Error("src/index.css: no @theme inline block found");
  const out = new Map<string, string>();
  for (const match of block[1].matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    const target = /var\((--[a-z0-9-]+)\)/.exec(match[2]);
    if (target) out.set(match[1], target[1]);
  }
  return out;
}

/** `--name` from the `:root, .dark` block — the black theme proper. */
function blackThemeVar(name: string): Oklch {
  return themeBlockVar(":root,\\s*\\.dark", name, "black");
}

/** `--name` from the `.light` block. */
function lightThemeVar(name: string): Oklch {
  return themeBlockVar("\\.light", name, "light");
}

function themeBlockVar(selectorPattern: string, name: string, label: string): Oklch {
  const block = new RegExp(`${selectorPattern} \\{([\\s\\S]*?)\\n\\}`).exec(css);
  if (!block) throw new Error(`src/index.css: no block matching ${selectorPattern}`);
  const declaration = new RegExp(`${name}\\s*:\\s*([^;]+);`).exec(block[1]);
  if (!declaration) throw new Error(`src/index.css: ${name} is not defined in the ${label} theme`);
  const parsed = parseOklch(declaration[1]);
  if (!parsed) throw new Error(`src/index.css: ${name} is "${declaration[1]}", not an oklch() value`);
  return parsed;
}

const mapping = themeMapping();

describe("black theme — variable coverage", () => {
  it("defines every var the @theme inline mapping aliases", () => {
    const missing = [...mapping.values()]
      .filter((target) => !defined.has(target))
      .map((target) => `${target} (aliased by ${[...mapping].find(([, v]) => v === target)?.[0]})`);
    expect(missing).toEqual([]);
  });

  it("defines every var referenced with var() inside src/index.css", () => {
    // `--font-sans` is forwarded from the runtime font stack and is
    // intentionally not a colour var; the font is not adopted in
    // this PR (see the header of src/index.css).
    const tolerated = new Set(["--font-sans"]);
    const missing = [...referencedVars()]
      .filter((name) => !defined.has(name) && !tolerated.has(name))
      .sort();
    expect(missing).toEqual([]);
  });

  it("defines every theme var referenced by a vendored component", () => {
    // Radix/Origin UI/React Bits sources reference shadcn's
    // semantic tokens directly (and `--border` reaches the app
    // through arbitrary-value utilities). A typo there renders
    // as an invisible border, not an error.
    const missing: string[] = [];
    for (const [name, source] of Object.entries(componentSources)) {
      for (const match of source.matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
        if (!defined.has(match[1])) missing.push(`${name}: ${match[1]}`);
      }
    }
    expect([...new Set(missing)].sort()).toEqual([]);
  });

  it("maps --radius to a rem length so rounded-* utilities resolve", () => {
    const radius = defined.get("--radius");
    expect(radius).toMatch(/^[\d.]+rem$/);
  });
});

describe("black theme — direction", () => {
  it("paints a near-black canvas", () => {
    const background = blackThemeVar("--background");
    expect(background.l).toBeLessThan(0.2);
    // Near-black means near-black, not #000: the canvas keeps a
    // hair of chroma so it reads as a surface.
    expect(background.c).toBeLessThan(0.02);
    expect(background.c).toBeGreaterThan(0);
  });

  it("keeps every surface barely lighter than the canvas", () => {
    const canvas = blackThemeVar("--background").l;
    const steps = ["--card", "--popover", "--sidebar", "--muted", "--accent"].map((name) => ({
      name,
      l: blackThemeVar(name).l,
    }));
    for (const step of steps) {
      expect(step.l).toBeGreaterThan(canvas);
      expect(step.l - canvas).toBeLessThanOrEqual(0.13);
    }
  });

  it("uses exactly one accent, and it is cold", () => {
    const primary = blackThemeVar("--primary");
    // Hue 232 = cold blue. Warm accents are the direction the
    // pivot moved away from.
    expect(primary.h).toBeGreaterThanOrEqual(200);
    expect(primary.h).toBeLessThanOrEqual(270);
    expect(primary.c).toBeGreaterThan(0.05);
  });

  it("keeps the secondary/accent roles achromatic so a stray fill cannot paint colour", () => {
    for (const name of ["--secondary", "--muted", "--accent"]) {
      expect(blackThemeVar(name).c).toBeLessThan(0.02);
    }
  });

  it("separates surfaces with hairlines, not fills", () => {
    const canvas = blackThemeVar("--background");
    const border = blackThemeVar("--border");
    // Hairline borders are alpha-composited white at ~12%: they
    // read as a line without becoming a surface of their own.
    // WCAG 1.4.11's 3:1 floor governs boundaries a user needs to
    // identify a CONTROL; a decorative seam is exempt. What is
    // pinned here is the band: visible enough to read as a line,
    // faint enough to stay a line. A "hairline" that drifts to
    // invisible, or that inflates into a fill, both fail.
    const seam = contrastOver(border, canvas);
    expect(seam).toBeGreaterThan(1.2);
    expect(seam).toBeLessThan(3);
    // Alpha-composited, so it inherits whatever it sits on.
    expect(border.a).toBeGreaterThan(0);
    expect(border.a).toBeLessThanOrEqual(0.15);
    // Controls keep a slightly stronger edge than seams.
    expect(blackThemeVar("--input").a).toBeGreaterThan(border.a);
  });

  it("mutes secondary text and keeps it above the AA floor", () => {
    const foreground = blackThemeVar("--foreground");
    const mutedForeground = blackThemeVar("--muted-foreground");
    expect(mutedForeground.l).toBeLessThan(foreground.l);
    // Muted text is the first thing this theme dims, but never
    // past AA on any surface it lands on.
    for (const surface of ["--background", "--card", "--popover", "--muted", "--sidebar"]) {
      expect(contrastRatio(mutedForeground, blackThemeVar(surface))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("carries a legible primary text ladder", () => {
    const foreground = blackThemeVar("--foreground");
    for (const surface of ["--background", "--card", "--popover", "--muted", "--sidebar"]) {
      expect(contrastRatio(foreground, blackThemeVar(surface))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("keeps text on the accent fill above the AA floor", () => {
    expect(
      contrastRatio(blackThemeVar("--primary-foreground"), blackThemeVar("--primary")),
    ).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps text on the destructive fill above the AA floor", () => {
    expect(
      contrastRatio(
        blackThemeVar("--destructive-foreground"),
        blackThemeVar("--destructive"),
      ),
    ).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps the destructive role legible in both pairings it is used in", () => {
    const destructive = blackThemeVar("--destructive");
    const canvas = blackThemeVar("--background");
    // As text straight on the canvas…
    expect(contrastRatio(destructive, canvas)).toBeGreaterThanOrEqual(4.5);
    // …and as text on the `destructive/10` wash that shadcn's
    // destructive button variant paints behind it. The wash is
    // the generated component's, not the theme's, so this is the
    // assertion that keeps the two from drifting apart.
    expect(
      contrastOnWash(destructive, { ...destructive, a: 0.1 }, canvas),
    ).toBeGreaterThanOrEqual(4.5);
  });

  it("gives the light counterpart the same discipline", () => {
    const light = lightThemeVar;
    expect(light("--background").l).toBeGreaterThan(0.95);
    // Same single cold accent, so identity does not change with
    // the theme.
    expect(light("--primary").h).toBeGreaterThanOrEqual(200);
    expect(light("--primary").h).toBeLessThanOrEqual(270);
    expect(contrastRatio(light("--foreground"), light("--background"))).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(light("--muted-foreground"), light("--background"))).toBeGreaterThanOrEqual(4.5);
  });
});