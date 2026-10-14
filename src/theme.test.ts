import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { createElement } from "react";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { contrastOnWash, contrastOver, contrastRatio, parseOklch, type Oklch } from "./test/color";
import { readRepoFile } from "./test/readRepoFile";

import { Button } from "./components/ui/button";

// Read as text, not as a style injection: these assertions are
// about what the files DECLARE.
const css = readRepoFile("src/index.css");
const mainTsx = readRepoFile("src/main.tsx");

/**
 * The same stylesheet with its comments removed.
 *
 * The comments here are load-bearing prose — they carry the
 * reasoning behind most of what follows — but they are not
 * declarations. Parsing them would let an `@import "tailwindcss"`
 * inside an explanation, or a `--token:` in a note, satisfy an
 * assertion about what actually ships, which is how a gate starts
 * passing on its own comments.
 */
const cssCode = css.replace(/\/\*[\s\S]*?\*\//g, "");

/**
 * The stack's copied components — the files whose classes route
 * through the black theme's vars. Every one of them has a lineage
 * header in-file; there is no copied source without one.
 *
 * #230 Phase 0 adds the four text-field/spinner primitives the
 * rebuild was missing (ui/input, ui/textarea, ui/native-select,
 * ui/spinner). They are registered here so their colour utilities
 * are gated exactly like button.tsx's: a utility whose target var
 * is undefined renders transparent and nothing else warns.
 */
const componentSources: Record<string, string> = {
  button: readRepoFile("src/components/ui/button.tsx"),
  textType: readRepoFile("src/components/TextType.tsx"),
  input: readRepoFile("src/components/ui/input.tsx"),
  textarea: readRepoFile("src/components/ui/textarea.tsx"),
  nativeSelect: readRepoFile("src/components/ui/native-select.tsx"),
  spinner: readRepoFile("src/components/ui/spinner.tsx"),
};

/**
 * Theme contract for the black theme in src/index.css (#218).
 *
 * Tailwind resolves theme vars LAZILY: a `bg-primary` whose
 * `--primary` was never defined compiles cleanly and renders as
 * transparent. Nothing in the build or in `tsc` catches that, so
 * this file is the gate that does. It asserts four things:
 *
 *   1. COVERAGE   every var the `@theme inline` mapping aliases,
 *                 every var a copied component references, and
 *                 every colour utility it paints with, resolves in
 *                 BOTH theme blocks.
 *   2. TYPED      every colour-bearing role parses as an OKLCH
 *                 value, so the contrast assertions below are
 *                 real measurements rather than string matches.
 *   3. DIRECTION  the canvas is near-black, one cold accent
 *                 carries the identity, the role ladder holds, and
 *                 the text pairs clear WCAG AA.
 *   4. CASCADE    the merged M3E sheets load INSIDE a cascade
 *                 layer, from the one stylesheet entry, so
 *                 Tailwind utilities can outrank the legacy
 *                 element resets instead of losing to them.
 *
 * The numbers asserted here are computed from the stylesheet by
 * src/test/color.ts — nothing here is transcribed from the
 * comment block in the CSS, which can drift.
 *
 * A gate that cannot fail is worse than no gate, so the extractor the
 * coverage half leans on is pinned by its own test: the colour
 * utilities it finds in button.tsx are asserted EXACTLY. That is what
 * caught the first version of it, which returned an empty set and let
 * every assertion below pass on nothing.
 */

/* ----------------------------------------------------------
   Block-scoped parsing.

   Deliberately NOT file-global: a var defined only in the light
   block satisfies nothing here. A `--primary` that existed solely
   under `:root[data-theme="light"]` used to pass this file while
   the default (dark) root painted nothing at all — which is the
   exact failure the file exists to catch, hiding in the file that
   catches it.
   ---------------------------------------------------------- */

/** Body of a top-level rule in src/index.css, found by its selector. */
function ruleBody(selector: RegExp, label: string): string {
  const match = new RegExp(`${selector.source}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(cssCode);
  if (!match) throw new Error(`src/index.css: no block matching ${label}`);
  return match[1];
}

/** The default theme — dark, painted by an un-marked `:root`. */
const DARK_BLOCK = ruleBody(/:root,\s*:root\[data-theme="dark"\]/, "the dark/default theme block");

/** The light counterpart, selected by `data-theme="light"`. */
const LIGHT_BLOCK = ruleBody(/:root\[data-theme="light"\]/, "the light theme block");

/** Custom properties DEFINED in a block body, as `--name -> value`. */
function definedVars(body: string): Map<string, string> {
  const out = new Map<string, string>();
  // A declaration is `--name: value;`. Negative values, `calc()`
  // and `var()` fall back are fine — only `--*: ` at the start of
  // a declaration counts.
  for (const match of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    out.set(match[1], match[2].trim());
  }
  return out;
}

const darkDefined = definedVars(DARK_BLOCK);
const lightDefined = definedVars(LIGHT_BLOCK);

/** Every custom property REFERENCE anywhere in the file. */
function referencedVars(): Set<string> {
  const out = new Set<string>();
  for (const match of cssCode.matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
    out.add(match[1]);
  }
  return out;
}

/** The @theme inline mapping block, as `--alias -> var(--target)`. */
function themeMapping(): Map<string, string> {
  const block = /@theme inline \{([\s\S]*?)\n\}/.exec(cssCode);
  if (!block) throw new Error("src/index.css: no @theme inline block found");
  const out = new Map<string, string>();
  for (const match of block[1].matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    const target = /var\((--[a-z0-9-]+)\)/.exec(match[2]);
    if (target) out.set(match[1], target[1]);
  }
  return out;
}

const mapping = themeMapping();
/** Keys `@theme inline` emits into the theme layer (`--color-*`, `--radius-*`). */
const themeKeys = new Set(mapping.keys());

/**
 * Vars this file references that the theme does not own, named and
 * justified rather than swept into a deny-list:
 *   --nex-sys-type-family  declared by src/styles/tokens.css, which
 *                          src/index.css imports above. The stack's
 *                          `--font-sans` aliases it on purpose: the
 *                          app does not adopt a new typeface.
 */
const externalVars = new Map<string, string>([
  ["--nex-sys-type-family", "declared by src/styles/tokens.css (imported, layer(base))"],
]);

/** `--name` from the dark/default block — the black theme proper. */
function blackThemeVar(name: string): Oklch {
  return blockVar(DARK_BLOCK, name, "dark");
}

/** `--name` from the light block. */
function lightThemeVar(name: string): Oklch {
  return blockVar(LIGHT_BLOCK, name, "light");
}

function blockVar(body: string, name: string, label: string): Oklch {
  const declaration = new RegExp(`${name}\\s*:\\s*([^;]+);`).exec(body);
  if (!declaration) throw new Error(`src/index.css: ${name} is not defined in the ${label} theme`);
  const parsed = parseOklch(declaration[1]);
  if (!parsed) throw new Error(`src/index.css: ${name} is "${declaration[1]}", not an oklch() value`);
  return parsed;
}

/* ----------------------------------------------------------
   Tailwind class tokens.

   A copied component does not reference a theme var the way CSS
   does — it references it the way a design system does, through
   utility classes: `bg-primary`, `text-primary-foreground`,
   `ring-ring/50`, `bg-destructive/10`. A gate that only scanned
   `var(--x)` would have missed every one of them, which is why
   this walks class strings and resolves them through the same
   `@theme inline` table Tailwind uses.
   ---------------------------------------------------------- */

/** Utility prefixes that take a colour, i.e. that resolve to `--color-*`. */
const COLOUR_PREFIXES = [
  "bg-",
  "text-",
  "border-",
  "ring-",
  "outline-",
  "fill-",
  "stroke-",
  "divide-",
  "accent-",
  "caret-",
];

/** `text-*` that are type sizes, not colours. */
const FONT_SIZES = new Set([
  "xs",
  "sm",
  "base",
  "lg",
  "xl",
  "2xl",
  "3xl",
  "4xl",
  "5xl",
  "6xl",
  "7xl",
  "8xl",
  "9xl",
]);

/**
 * `bg-clip-*` is background-CLIP — where the fill is painted out to,
 * not what colour it is. `bg-clip-padding` (shadcn's Button carries
 * it) resolves to a keyword, so routing it at `--color-clip-padding`
 * would report a theme gap that does not exist. These four are the
 * values Tailwind ships; the family, not this list, is the rule.
 */
const BACKGROUND_CLIP = new Set([
  "clip-border",
  "clip-content",
  "clip-padding",
  "clip-text",
]);

/**
 * Tokens Tailwind resolves WITHOUT a `--color-*` entry: the CSS-wide
 * keywords, `transparent`/`current`, the static black/white, and the
 * structural sides of `border-*`. Anything else under a colour
 * prefix has to come from the theme or it paints nothing.
 */
const NOT_A_THEME_COLOUR = new Set([
  "transparent",
  "current",
  "currentcolor",
  "inherit",
  "initial",
  "unset",
  "revert",
  "none",
  "black",
  "white",
  "b",
  "t",
  "l",
  "r",
  "x",
  "y",
  "s",
  "e",
  "w",
  "n",
  "start",
  "end",
  "block",
]);

/**
 * A string literal, matched PER DELIMITER: a `"`-delimited string may
 * contain `'`, and a backtick one may contain both.
 *
 * The obvious one-liner, `/(["'`])([^"'`]*)\1/`, does NOT say that. It
 * pairs whatever quote opened a string with the NEXT quote of ANY
 * kind, so a class string containing an apostrophe is abandoned whole
 * and two quotes of different types are read as one string. shadcn's
 * Button carries `[&_svg:not([class*='size-'])]:size-4`, so that
 * version saw an apostrophe, mispaired every class string in the
 * file, returned 51 tokens of pure structure (`"{\n  variants: {…"`,
 * `default:`, `"icon-xs":`) and ZERO colour utilities — which made
 * the gate below pass on an empty set, forever, silently. Anchoring
 * each delimiter to itself is the entire fix.
 */
const STRING_LITERAL = /"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`/g;

/** Every class token in a component source, comments and interpolations aside. */
function classTokens(source: string): string[] {
  const code = source.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/^\s*\/\/.*$/gm, " ");
  const tokens = new Set<string>();
  for (const match of code.matchAll(STRING_LITERAL)) {
    // The body is the MATCH with its delimiters trimmed off, never a
    // capture group: a capture group delimited by the quote class
    // cannot hold a quote of the other kind, which is the bug above.
    for (const raw of match[0].slice(1, -1).replace(/\$\{[^}]*\}/g, " ").split(/\s+/)) {
      if (raw) tokens.add(raw);
    }
  }
  return [...tokens].sort();
}

/** The colour-bearing utilities among a component's class tokens. */
function colourUtilities(source: string): string[] {
  const out = new Set<string>();
  for (const token of classTokens(source)) {
    // Strip variant prefixes one at a time: `dark:`,
    // `focus-visible:aria-invalid:`, `has-data-[icon=inline-end]:`.
    let bare = token;
    while (/^[a-z0-9[\]_.-]+:/i.test(bare)) bare = bare.replace(/^[a-z0-9[\]_.-]+:/i, "");
    // Strip the opacity modifier: `bg-destructive/10` -> `bg-destructive`.
    bare = bare.replace(/\/\s*\d+$/, "");
    for (const prefix of COLOUR_PREFIXES) {
      if (!bare.startsWith(prefix)) continue;
      const name = bare.slice(prefix.length);
      // `size-4`, `px-2.5`, `[color-mix(...)]` — not a colour name.
      if (name === "" || /^[[\d.]/.test(name)) continue;
      if (prefix === "text-" && FONT_SIZES.has(name)) continue;
      if (prefix === "bg-" && BACKGROUND_CLIP.has(name)) continue;
      if (NOT_A_THEME_COLOUR.has(name)) continue;
      out.add(`${prefix}${name}`);
    }
  }
  return [...out].sort();
}

describe("black theme — variable coverage", () => {
  it("defines every colour role the @theme inline mapping aliases, in BOTH themes", () => {
    // Scoped to the two theme blocks on purpose: a var that exists
    // under one theme but not the other paints nothing on the other.
    // Only the `--color-*` keys are per-theme; the shape scale and
    // the font alias are derived, and the next test holds those.
    const missing: string[] = [];
    for (const [alias, target] of mapping) {
      if (!alias.startsWith("--color-")) continue;
      if (!darkDefined.has(target)) missing.push(`dark: ${target} (aliased by ${alias})`);
      if (!lightDefined.has(target)) missing.push(`light: ${target} (aliased by ${alias})`);
    }
    expect(missing).toEqual([]);
  });

  it("derives the shape and font keys from vars that resolve", () => {
    // `--radius-*` step off the single `--radius` declared on the
    // default root (the light block deliberately does not redeclare
    // it: the shape scale is theme-independent), and `--font-*`
    // points at the merged token stack. Each has to land on
    // something that is actually declared somewhere.
    const unresolved: string[] = [];
    for (const [alias, target] of mapping) {
      if (alias.startsWith("--color-")) continue;
      const resolves = darkDefined.has(target) || themeKeys.has(target) || externalVars.has(target);
      if (!resolves) unresolved.push(`${alias} -> ${target}`);
    }
    expect(unresolved).toEqual([]);
  });

  it("defines every var referenced with var() inside src/index.css", () => {
    // A reference is resolvable if the theme defines it, if
    // `@theme inline` emits it into the theme layer, or if it is one
    // of the named external tokens above.
    const missing = [...referencedVars()]
      .filter((name) => !darkDefined.has(name) && !themeKeys.has(name) && !externalVars.has(name))
      .sort();
    expect(missing).toEqual([]);
  });

  it("defines every theme var a copied component references with var()", () => {
    // shadcn's generated components reach for shadcn's semantic
    // tokens directly, including `--radius-md` from `@theme inline`
    // and, in arbitrary-value utilities, the raw paint vars. A typo
    // there renders as an invisible border, not an error.
    const missing: string[] = [];
    for (const [name, source] of Object.entries(componentSources)) {
      for (const match of source.matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
        const target = match[1];
        if (darkDefined.has(target) || themeKeys.has(target)) continue;
        missing.push(`${name}: ${target}`);
      }
    }
    expect([...new Set(missing)].sort()).toEqual([]);
  });

  it("extracts the class tokens it claims to, from every copied component", () => {
    // The anti-vacuity pin, and the reason the gate below is a gate at
    // all. `unresolved` can only ever be non-empty if this extractor
    // returns something — and the version it replaced returned NOTHING
    // for button.tsx while looking like it worked, so every assertion
    // downstream passed on an empty set. Mutation-checked: restoring
    // `/(["'`])([^"'`]*)\1/` fails HERE and leaves the gate below green.
    expect(Object.keys(componentSources).sort()).toEqual([
      "button",
      "input",
      "nativeSelect",
      "spinner",
      "textType",
      "textarea",
    ]);

    const button = colourUtilities(componentSources.button);
    // The headline case: `bg-primary` is the class the whole black
    // theme exists to paint, and it is the first thing that vanishes
    // when the extractor starts mispairing quotes.
    expect(button).toContain("bg-primary");
    // The rest of what the PR body advertises, named so a regression
    // says WHAT it lost rather than only how many.
    expect(button).toEqual(
      expect.arrayContaining([
        "bg-primary",
        "text-primary-foreground",
        "ring-ring",
        "bg-destructive",
        "border-border",
        "border-destructive",
      ]),
    );
    // EXACT, not a subset. A containment check would still pass if the
    // extractor lost 16 of 17; a set check makes a registry update
    // that introduces a colour utility a diff somebody has to read.
    expect(button).toEqual([
      "bg-background",
      "bg-destructive",
      "bg-input",
      "bg-muted",
      "bg-primary",
      "bg-secondary",
      "border-border",
      "border-destructive",
      "border-input",
      "border-ring",
      "ring-destructive",
      "ring-ring",
      "text-destructive",
      "text-foreground",
      "text-primary",
      "text-primary-foreground",
      "text-secondary-foreground",
    ]);

    // TextType is scanned too. It paints no colour of its own — its
    // classes are layout, and the colour arrives as a `style={{ color }}`
    // prop — so what is pinned there is that the scan SEES the file,
    // not what it happens to find in it today.
    expect(classTokens(componentSources.textType)).toContain("inline-block");
  });

  it("routes every colour utility a copied component uses through a defined var", () => {
    // The real reference from a component into the theme is a class,
    // not a `var()`. `bg-primary` compiles to `var(--color-primary)`
    // to `var(--primary)`; if `--primary` is missing it renders
    // transparent and nothing warns. Each utility is resolved
    // through the same mapping Tailwind uses and must land on a var
    // both theme blocks define.
    const unresolved: string[] = [];
    for (const [name, source] of Object.entries(componentSources)) {
      for (const utility of colourUtilities(source)) {
        const key = `--color-${utility.slice(utility.indexOf("-") + 1)}`;
        const target = mapping.get(key);
        if (target === undefined) {
          unresolved.push(`${name}: ${utility} (no ${key} in @theme inline)`);
          continue;
        }
        if (!darkDefined.has(target)) {
          unresolved.push(`${name}: ${utility} -> ${target} undefined in the dark theme`);
        }
        if (!lightDefined.has(target)) {
          unresolved.push(`${name}: ${utility} -> ${target} undefined in the light theme`);
        }
      }
    }
    expect(unresolved).toEqual([]);
  });

  it("renders the default Button with bg-primary, against the real dark block", () => {
    // What this actually proves, narrowly: the generated Button really
    // does carry `bg-primary`, and `--primary` as the browser sees it
    // on the document root is the value the dark block declares —
    // read out of src/index.css, not typed in.
    //
    // What it deliberately does NOT claim: which rule wins. jsdom
    // compiles no CSS, implements no cascade layers, and does not run
    // Tailwind — an earlier version of this test injected its own
    // `.bg-primary{background-color:var(--primary)}` and then asserted
    // getComputedStyle echoed `var(--primary)` back, which was the
    // test agreeing with itself about a rule it had just written. That
    // echo is gone; the cascade half is asserted from the stylesheet
    // in the cascade describe below, and against the built bundle.
    const style = document.createElement("style");
    style.textContent = `:root{${DARK_BLOCK}}`;
    document.head.appendChild(style);
    try {
      const { container } = render(createElement(Button));
      const button = container.querySelector("button");
      expect(button).not.toBeNull();
      expect(button!.className.split(/\s+/)).toContain("bg-primary");
      expect(getComputedStyle(document.documentElement).getPropertyValue("--primary")).toBe(
        darkDefined.get("--primary"),
      );
      container.remove();
    } finally {
      style.remove();
    }
  });

  it("maps --radius to a rem length so rounded-* utilities resolve", () => {
    const radius = darkDefined.get("--radius");
    expect(radius).toMatch(/^[\d.]+rem$/);
  });
});

describe("cascade — the merged sheets load inside a layer", () => {
  /**
   * The contract, and the bug it prevents.
   *
   * The M3E sheets carry element resets (`button { background:
   * transparent; padding: 0; border: none }`) and they used to be
   * imported UNLAYERED from src/main.tsx. Per CSS Cascade L5 an
   * unlayered normal declaration outranks every layered one, so
   * those resets silently beat `bg-primary`, `px-2.5`, `border`,
   * `text-sm` and every other Tailwind utility in the shipped
   * bundle: components on the stack rendered unstyled while every
   * test stayed green, because the utilities resolve fine and lose
   * at paint time.
   *
   * So: one stylesheet entry, every sheet in a declared layer, and
   * utilities declared last.
   *
   * SCOPE, stated rather than implied: everything in this describe
   * reads src/index.css and src/main.tsx as SOURCE. It asserts the
   * contract that was written, not the bundle that came out. The
   * bundle-level layer order is checked separately and manually —
   * `npx vite build`, then read the layer offsets out of
   * `dist/assets/*.css` — and the result is recorded in the PR body.
   * A source assertion cannot see a bundler normalising a
   * declaration away, which is exactly what it does with
   * `@layer theme, base, components, utilities;`.
   */
  const LEGACY_DIR = resolve(process.cwd(), "src/styles");
  /** Every merged stylesheet on disk, as import specifiers. */
  const legacySheets = [
    "./styles.css",
    ...readdirSync(LEGACY_DIR)
      .filter((name) => name.endsWith(".css"))
      .map((name) => `./styles/${name}`),
  ].sort();

  /** `@import "<specifier>"[ layer(<name>)]` → `specifier -> layer`. */
  function importedLayers(): Map<string, string | null> {
    const out = new Map<string, string | null>();
    for (const match of cssCode.matchAll(/@import\s+["']([^"']+)["']([^;]*);/g)) {
      const qualifier = /\blayer\(\s*([a-z-]+)\s*\)/.exec(match[2]);
      out.set(match[1], qualifier ? qualifier[1] : null);
    }
    return out;
  }

  it("declares the layer order exactly once, ahead of every @import", () => {
    const statements = cssCode.match(/@layer\s+[a-z][a-z-]*(?:\s*,\s*[a-z][a-z-]*)*\s*;/gi) ?? [];
    expect(statements).toHaveLength(1);
    // utilities LAST: that is what lets a utility outrank a reset.
    const order = (statements[0] ?? "")
      .replace(/@layer|;/g, "")
      .split(",")
      .map((s) => s.trim());
    expect(order).toEqual(["theme", "base", "components", "utilities"]);
    expect(cssCode.indexOf("@layer theme,")).toBeLessThan(cssCode.search(/@import\s+["']/));
  });

  it("imports every merged sheet with layer(base)", () => {
    const layers = importedLayers();
    expect([...layers.keys()].filter((spec) => legacySheets.includes(spec)).sort()).toEqual(
      legacySheets,
    );
    for (const specifier of legacySheets) {
      expect(`${specifier}: ${layers.get(specifier)}`).toBe(`${specifier}: base`);
    }
  });

  it("leaves no merged sheet unlayered", () => {
    // The whole point: an unlayered sheet outranks every utility
    // again, which is how the resets got in front of the theme.
    //
    // One exception, named: tw-animate-css ships `@utility` at-rules
    // and Tailwind refuses to parse one inside a layer
    // ("`@utility` cannot be nested"), so its import cannot carry
    // `layer(...)`. Its `@utility` classes WOULD land in the
    // utilities layer, but nothing in src/ uses an `animate-*` class,
    // so today it contributes none — 0 `@utility` and 0 `.animate-*`
    // in the built CSS. What it does emit is 56 `@property --tw-*`
    // registrations, unlayered at top level after the utilities
    // layer closes, and that is inert: an `@property` registers a
    // custom property and declares no rule, so there is nothing there
    // for the unlayered position to out-rank, and the names it
    // registers (`--tw-*`) are ones no legacy sheet defines. So this
    // assertion fails the day a SECOND sheet is imported unlayered —
    // which is the only failure it is here to catch.
    const unlayered = [...importedLayers()]
      .filter(([, layer]) => layer === null)
      .map(([specifier]) => specifier);
    expect(unlayered).toEqual(["tw-animate-css"]);
  });

  it("keeps exactly one stylesheet entry, in src/main.tsx", () => {
    // A second `import "./x.css"` in main.tsx is unlayered by
    // definition and would reinstate the bug behind this file's back.
    const entries = [...mainTsx.matchAll(/import\s+(?:[^"']*from\s+)?["']([^"']+\.css)["']/g)].map(
      (match) => match[1],
    );
    expect(entries).toEqual(["./index.css"]);
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
    const steps = ["--card", "--popover", "--muted", "--accent"].map((name) => ({
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
    for (const surface of ["--background", "--card", "--popover", "--muted"]) {
      expect(contrastRatio(mutedForeground, blackThemeVar(surface))).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("carries a legible primary text ladder", () => {
    const foreground = blackThemeVar("--foreground");
    for (const surface of ["--background", "--card", "--popover", "--muted"]) {
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
    // Same single cold accent, so identity does not change with the
    // theme.
    expect(light("--primary").h).toBeGreaterThanOrEqual(200);
    expect(light("--primary").h).toBeLessThanOrEqual(270);
    expect(contrastRatio(light("--foreground"), light("--background"))).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(light("--muted-foreground"), light("--background"))).toBeGreaterThanOrEqual(4.5);
  });

  it("selects both themes with the attribute the app actually writes", () => {
    // src/lib/useAppearance.ts writes `documentElement.dataset.theme`;
    // tokens.css treats a bare `:root` as dark. A `.dark`/`.light`
    // class pair matched nothing at runtime, and a `dark:` variant
    // keyed off it was dead on every screen.
    expect(css).toMatch(/@custom-variant\s+dark\s*\(\s*&:is\(\[data-theme="dark"\]/);
    expect(css).toMatch(/^:root\[data-theme="light"\] \{/m);
    expect(css).not.toMatch(/^\.dark \{/m);
    expect(css).not.toMatch(/^\.light \{/m);
  });
});
