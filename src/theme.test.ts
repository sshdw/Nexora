import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { createElement } from "react";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { contrastOnWash, contrastOver, contrastRatio, parseOklch, type Oklch } from "./test/color";
import { readRepoFile } from "./test/readRepoFile";
import { PARITY_CASES } from "./test/parityMap";
import {
  ZEN_EXIT_LAYER_MS,
  ZEN_EXIT_STEP_MS,
  ZEN_EXIT_TOTAL_MS,
  zenExitDelayMs,
} from "./lib/zenExit";

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
  input: readRepoFile("src/components/ui/input.tsx"),
  textarea: readRepoFile("src/components/ui/textarea.tsx"),
  nativeSelect: readRepoFile("src/components/ui/native-select.tsx"),
  spinner: readRepoFile("src/components/ui/spinner.tsx"),
  // #230 Phase 1: the sidebar conversation list's loading placeholder.
  skeleton: readRepoFile("src/components/ui/skeleton.tsx"),
};

/**
 * Theme contract for the mono theme in src/index.css (#218, #235).
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
 *   3. DIRECTION  the canvas is near-black, NO role carries hue
 *                 except `--state-ok/warn/err` (+`--destructive`),
 *                 the primary action is a light fill with dark text
 *                 at 16.08, and the text pairs clear WCAG AA.
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
 * Vars this file references that the theme does not own.
 *
 * Phase 0 aliased `--font-sans` at the merged token stack
 * (`--nex-sys-type-family`, declared by src/styles/tokens.css)
 * because the app adopted no typeface of its own. #235 wires
 * Inter/Cascadia Code instead (NEXORA-THEME.md §3), so both font
 * keys now resolve to `--font-sans` / `--font-mono` declared in
 * the theme blocks below — the map is empty, kept so the next
 * external reference has a named place to be justified rather
 * than a deny-list to hide in.
 *
 * #230 Phase 2 adds the three motion tokens ui/button.tsx reaches
 * for the global press (NEXORA-THEME.md §6): the state-fade it
 * already inherited from Tailwind's `transition-all`, plus the
 * press duration and scale. They resolve in src/styles/motion.css,
 * which owns motion the way tokens.css owns space/type — `:root`,
 * not a theme block, and not `@theme inline`. Each entry names its
 * real home so the next reader can go and check it.
 */
const externalVars = new Map<string, string>([
  ["--nex-sys-motion-duration-fast", "src/styles/motion.css :root"],
  ["--nex-sys-motion-duration-press", "src/styles/motion.css :root"],
  ["--nex-sys-motion-scale-press", "src/styles/motion.css :root"],
  // #230 Phase 7 (SYSTEM §7.1/§7.2, OD-D9·motion): the two springs and
  // the one named off-ladder hover duration. Same home as the press
  // tokens — motion.css owns motion, not the theme blocks.
  ["--nex-sys-motion-spring-settle", "src/styles/motion.css :root"],
  ["--nex-sys-motion-spring-snap", "src/styles/motion.css :root"],
  ["--nex-sys-motion-duration-hover", "src/styles/motion.css :root"],
]);

/**
 * Theme vars that are NOT colours: the shape step, the two
 * font stacks, and the chrome row height. The achromatic sweep
 * below parses every other defined var as OKLCH, so these are
 * named here rather than skipped silently — an unparseable colour
 * would otherwise pass by looking like a font.
 *
 * `--chrome-height` is layout, not paint: THEME §6 freezes the tab
 * strip and pane header at 40px (issue #262), and it lives in the
 * default theme block because a length is theme-independent — the
 * light block inherits it from `:root`.
 */
const NON_COLOUR_VARS = new Set([
  "--radius",
  "--font-sans",
  "--font-mono",
  "--chrome-height",
  /* #230 Phase 6: the legacy-format tokens relocated from tokens.css into
     src/index.css. They keep their legacy hex/rgba/px literals (not oklch())
     so the per-(theme, token) parity test can compare resolved literals as
     strings. They are therefore outside the okLCH achromatic sweep. */
  "--color-accent-indicator",
  "--color-error",
  "--color-error-container",
  "--color-error-filled",
  "--color-error-outline",
  "--color-inverse-on-surface",
  "--color-inverse-surface",
  "--color-on-error",
  "--color-on-error-container",
  "--color-on-primary",
  "--color-on-primary-container",
  "--color-on-secondary-container",
  "--color-on-success-container",
  "--color-on-surface",
  "--color-on-surface-faint",
  "--color-on-surface-tertiary",
  "--color-on-surface-variant",
  "--color-outline",
  "--color-outline-variant",
  "--color-primary",
  "--color-primary-container",
  "--color-primary-hover",
  "--color-scrim",
  "--color-secondary-container",
  "--color-shell-seam",
  "--color-success",
  "--color-success-container",
  "--color-surface-canvas",
  "--color-surface-container",
  "--color-surface-container-high",
  "--color-surface-container-highest",
  "--color-surface-container-low",
  "--color-surface-container-lowest",
  "--color-surface-elevated",
  "--color-surface-floating",
  "--color-surface-nested",
  "--focus-ring-offset-inset",
  "--layout-content-max",
  "--layout-header-height",
  "--layout-measure-max",
  "--layout-sidebar-width",
  "--nav-indicator-height",
  "--nav-indicator-width",
  "--nex-sys-elevation-level1",
  "--nex-sys-elevation-level2",
  "--nex-sys-z-dialog",
  "--nex-sys-z-popover",
  "--nex-sys-z-snackbar",
  "--nex-sys-z-tooltip",
  "--scrollbar-thumb",
  "--scrollbar-thumb-hover",
  "--shape-extra-small",
  "--shape-full",
  "--shape-large",
  "--shape-large-increased",
  "--shape-medium",
  "--shape-none",
  "--shape-pill",
  "--shape-small",
  "--space-2xl",
  "--space-lg",
  "--space-md",
  "--space-sm",
  "--space-xl",
  "--space-xs",
  "--space-xxs",
  "--space-xxxs",
  "--state-disabled-opacity",
  "--state-dragged-opacity",
  "--state-focus-opacity",
  "--state-hover",
  "--state-hover-opacity",
  "--state-inverse-hover",
  "--state-inverse-pressed",
  "--state-pressed",
  "--state-pressed-opacity",
  "--state-selected",
  "--state-selected-opacity",
  "--type-family",
  "--type-family-mono",
  "--type-line-height-normal",
  "--type-line-height-relaxed",
  "--type-line-height-tight",
  "--type-size-body-large",
  "--type-size-body-medium",
  "--type-size-body-small",
  "--type-size-headline-small",
  "--type-size-label-large",
  "--type-size-label-medium",
  "--type-size-label-small",
  "--type-size-title-large",
  "--type-size-title-medium",
  "--type-size-title-small",
  "--type-tracking-body-small",
  "--type-weight-emphasized",
  "--type-weight-medium",
  "--type-weight-regular",
  "--type-weight-semibold",
]);

/** `ring-offset-<colour>` resolves like `bg-<colour>`: Tailwind
 *  paints the offset from `--color-<colour>`, while
 *  `ring-offset-<n>` is a WIDTH (2px gap), not a colour at all. */
function themeKeyForUtility(utility: string): string | null {
  if (utility.startsWith("ring-offset-")) {
    const rest = utility.slice("ring-offset-".length);
    if (/^\d+$/.test(rest)) return null;
    return `--color-${rest}`;
  }
  return `--color-${utility.slice(utility.indexOf("-") + 1)}`;
}

/** `--name` from the dark/default block — the mono theme proper. */
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
    // `ring-offset-2` is the focus gap WIDTH, not a colour —
    // `ring-offset-background` (the gap's paint) is, and is
    // resolved through themeKeyForUtility below.
    if (/^ring-offset-\d+$/.test(bare)) continue;
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
    //
    // `externalVars` counts here for the same reason it counts in the
    // index.css test above: a var declared in one of the merged
    // sheets (#230 Phase 2 — the press tokens, which live in
    // motion.css because that is where motion is declared) resolves
    // exactly as well as one the theme blocks happen to own.
    const missing: string[] = [];
    for (const [name, source] of Object.entries(componentSources)) {
      for (const match of source.matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
        const target = match[1];
        if (darkDefined.has(target) || themeKeys.has(target) || externalVars.has(target)) {
          continue;
        }
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
      "skeleton",
      "spinner",
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
        "bg-primary-hover",
        "bg-primary-pressed",
        "ring-offset-background",
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
    // (#235: +`bg-primary-hover`/`bg-primary-pressed` — the
    // light-fill hover/pressed steps — +`ring-offset-background`
    // — the 2px focus gap's paint — and +`text-muted-foreground`
    // — the disabled default's muted text, no fill.)
    expect(button).toEqual([
      "bg-background",
      "bg-destructive",
      "bg-input",
      "bg-muted",
      "bg-primary",
      "bg-primary-hover",
      "bg-primary-pressed",
      "bg-secondary",
      "border-border",
      "border-destructive",
      "border-input",
      "border-ring",
      "ring-destructive",
      "ring-offset-background",
      "ring-ring",
      "text-destructive",
      "text-foreground",
      "text-muted-foreground",
      "text-primary",
      "text-primary-foreground",
      "text-secondary-foreground",
    ]);

    // The #230 Phase 1 skeleton: its one colour utility has to
    // route through a defined var like any other copied component, and
    // the pulse it declares must be the Tailwind utility the global
    // reduced-motion gate already collapses.
    expect(colourUtilities(componentSources.skeleton)).toEqual(["bg-accent"]);
    expect(classTokens(componentSources.skeleton)).toContain("animate-pulse");
    // SYSTEM §7.2 skeleton row: the 300ms delay + `both` fill live on
    // the component too, so a Skeleton that is not a `.nex-skeleton-row`
    // still starts late and holds its first frame.
    expect(classTokens(componentSources.skeleton)).toEqual(
      expect.arrayContaining(["[animation-delay:300ms]", "[animation-fill-mode:both]"]),
    );
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
        const key = themeKeyForUtility(utility);
        if (key === null) continue;
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

  it("paints the primary action as a light fill with dark text", () => {
    // No hue: the action is `#EDEAE7` with the near-black canvas
    // colour as text on it. Pinned as the 16.08 pair (THEME §2) —
    // a band, not a point, so rounding the hex→oklch transcription
    // cannot fail the gate.
    const primary = blackThemeVar("--primary");
    const onPrimary = blackThemeVar("--primary-foreground");
    expect(primary.l).toBeGreaterThan(0.9);
    expect(primary.c).toBeLessThan(0.02);
    expect(onPrimary.l).toBeLessThan(0.2);
    const pair = contrastRatio(onPrimary, primary);
    expect(pair).toBeGreaterThanOrEqual(15.5);
    expect(pair).toBeLessThanOrEqual(16.6);
  });

  it("steps hover lighter and pressed darker, both achromatic and AA", () => {
    // State reads through LIGHTNESS, never hue: hover one step up
    // (`#F5F3EF`), pressed one step down (`#D8D4CE`), dark text on
    // both still clearing AA.
    const primary = blackThemeVar("--primary");
    const onPrimary = blackThemeVar("--primary-foreground");
    const hover = blackThemeVar("--primary-hover");
    const pressed = blackThemeVar("--primary-pressed");
    expect(hover.l).toBeGreaterThan(primary.l);
    expect(pressed.l).toBeLessThan(primary.l);
    expect(hover.c).toBeLessThan(0.02);
    expect(pressed.c).toBeLessThan(0.02);
    expect(contrastRatio(onPrimary, hover)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(onPrimary, pressed)).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps every role achromatic except the state roles", () => {
    // THE mono pin (#235): no permanent blue, amber or violet
    // anywhere. The ONLY chromatic roles are the state signals —
    // `--state-ok/warn/err`, plus the pre-existing `--destructive`
    // — and the pin asserts they ARE chromatic, so a greyed-out
    // error state fails too. There is deliberately no hue assertion
    // anywhere in this file anymore: the cold-accent band
    // (h 200–270) died with the accent.
    const chromatic = new Set(["--state-ok", "--state-warn", "--state-err", "--destructive"]);
    const themes = [
      ["dark", darkDefined],
      ["light", lightDefined],
    ] as const;
    for (const [label, vars] of themes) {
      for (const [name, value] of vars) {
        if (NON_COLOUR_VARS.has(name)) continue;
        const parsed = parseOklch(value);
        if (!parsed) throw new Error(`src/index.css: ${name} is not an oklch() value (${label})`);
        if (chromatic.has(name)) {
          expect(parsed.c).toBeGreaterThan(0.02);
        } else {
          expect(parsed.c).toBeLessThan(0.02);
        }
      }
    }
  });

  it("rings focus in white at 80%, not in colour", () => {
    // THEME §1: 2px solid white 80% + 2px offset. The token is the
    // colour; the width/offset live on the components (utilities:
    // `ring-2` + `ring-offset-2`; plain CSS: the two-shadow spell
    // in composer.css).
    const ring = blackThemeVar("--ring");
    expect(ring.l).toBeGreaterThan(0.99);
    expect(ring.c).toBeLessThan(0.02);
    expect(ring.a).toBeGreaterThanOrEqual(0.75);
    expect(ring.a).toBeLessThanOrEqual(0.85);
  });

  it("keeps the state roles legible on the canvas, both themes", () => {
    for (const name of ["--state-ok", "--state-warn", "--state-err"] as const) {
      expect(contrastRatio(blackThemeVar(name), blackThemeVar("--background"))).toBeGreaterThanOrEqual(
        4.5,
      );
      expect(contrastRatio(lightThemeVar(name), lightThemeVar("--background"))).toBeGreaterThanOrEqual(
        4.5,
      );
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

  it("keeps text on the light fill above the AA floor", () => {
    // Subsumed by the 16.08 pair pin above; kept as the AA-floor
    // phrasing so the ladder reads complete.
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

  it("gives the light counterpart the same mono discipline", () => {
    const light = lightThemeVar;
    expect(light("--background").l).toBeGreaterThan(0.95);
    // Same pair, swapped: dark fill, light text — still 16.08, so
    // identity does not change with the theme. No hue band: see
    // the achromatic sweep above, which covers both themes.
    expect(light("--primary").l).toBeLessThan(0.25);
    expect(light("--primary-foreground").l).toBeGreaterThan(0.9);
    const pair = contrastRatio(light("--primary-foreground"), light("--primary"));
    expect(pair).toBeGreaterThanOrEqual(15.5);
    expect(pair).toBeLessThanOrEqual(16.6);
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

/* ----------------------------------------------------------
   Motion contract — SYSTEM §7 (#230 Phase 7).

   Everything below is read as SOURCE: jsdom paints no cascade, so
   what is pinned is what the sheets and components DECLARE. The
   spring table is re-derived from (ζ, ωₙ) rather than transcribed
   from the comment, and the stop count is deliberately NOT pinned —
   only the facts the contract names (duration, peak, first/last
   sample, the t_p crest stop).
   ---------------------------------------------------------- */
describe("motion contract — SYSTEM §7", () => {
  const motionSheet = readRepoFile("src/styles/motion.css");
  const motionCode = motionSheet.replace(/\/\*[\s\S]*?\*\//g, "");
  const componentsCode = readRepoFile("src/styles/components.css").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );
  const tabsCode = readRepoFile("src/styles/tabs.css").replace(/\/\*[\s\S]*?\*\//g, "");
  const conversationCode = readRepoFile("src/styles/conversation.css").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );

  /** A `--name: value;` declaration from motion.css, comments stripped. */
  function tokenValue(name: string): string {
    const match = new RegExp(`${name}\\s*:\\s*([^;]+);`).exec(motionCode);
    if (!match) throw new Error(`src/styles/motion.css: ${name} is not declared`);
    return match[1].trim();
  }

  /** The body of a flat rule, by literal selector. */
  function ruleBody(selector: string, source: string): string {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = new RegExp(`${escaped}\\s*\\{([^{}]*)\\}`).exec(source);
    if (!match) throw new Error(`no rule matching ${selector}`);
    return match[1];
  }

  /** The linear() stops of a spring token, plus its declared duration. */
  function spring(name: "settle" | "snap"): {
    durationMs: number;
    stops: Array<{ y: number; percent: number }>;
  } {
    const value = tokenValue(`--nex-sys-motion-spring-${name}`);
    const match = /^linear\(([^)]*)\)\s*(\d+)ms$/.exec(value);
    if (!match) throw new Error(`--nex-sys-motion-spring-${name} is not a linear() with a duration`);
    const stops = match[1].split(",").map((stop) => {
      const parsed = /([\d.]+)\s+([\d.]+)%/.exec(stop.trim());
      if (!parsed) throw new Error(`unparseable spring stop: ${stop}`);
      return { y: Number(parsed[1]), percent: Number(parsed[2]) };
    });
    return { durationMs: Number(match[2]), stops };
  }

  /** The comment immediately above a spring token. */
  function springComment(name: "settle" | "snap"): string {
    const at = motionSheet.indexOf(`--nex-sys-motion-spring-${name}:`);
    const start = motionSheet.lastIndexOf("/*", at);
    const end = motionSheet.lastIndexOf("*/", at);
    if (at < 0 || start < 0 || end < start) throw new Error(`no comment above the ${name} spring`);
    return motionSheet.slice(start, end + 2);
  }

  const SPRINGS = {
    // The source of truth: SYSTEM §7.1. Everything else is derived.
    settle: { zeta: 0.84, omega: 24, duration: 343, tpStop: 70.41 },
    snap: { zeta: 0.70, omega: 24, duration: 411, tpStop: 44.58 },
  } as const;

  it("declares the three approved tokens and registers them in externalVars", () => {
    for (const [name, home] of [
      ["--nex-sys-motion-spring-settle", "src/styles/motion.css :root"],
      ["--nex-sys-motion-spring-snap", "src/styles/motion.css :root"],
      ["--nex-sys-motion-duration-hover", "src/styles/motion.css :root"],
    ] as const) {
      expect(tokenValue(name)).not.toBe("");
      expect(externalVars.get(name)).toBe(home);
    }
    expect(tokenValue("--nex-sys-motion-duration-hover")).toBe("120ms");
  });

  it("re-derives both springs from (ζ, ωₙ) and pins the linear() contract", () => {
    for (const name of ["settle", "snap"] as const) {
      const spec = SPRINGS[name];
      const comment = springComment(name);
      // The pair is the contract. The comment has to name ζ and ωₙ,
      // plus the three derived facts, so a reader can check the maths.
      expect(comment).toMatch(new RegExp(`ζ\\s*${spec.zeta}`));
      expect(comment).toMatch(new RegExp(`ωₙ\\s*${spec.omega}`));
      for (const derived of ["T", "peak", "t_p"]) {
        expect(comment).toContain(derived);
      }

      const { durationMs, stops } = spring(name);
      const T = (-Math.log(0.001) / (spec.zeta * spec.omega)) * 1000;
      const peak = Math.exp((-Math.PI * spec.zeta) / Math.sqrt(1 - spec.zeta ** 2));
      const tp = (Math.PI / (spec.omega * Math.sqrt(1 - spec.zeta ** 2))) * 1000;

      expect(Math.round(T)).toBe(spec.duration);
      expect(durationMs).toBe(spec.duration);
      expect(Math.round(tp)).toBe(name === "settle" ? 241 : 183);
      // Peak: the curve's maximum must be the derived overshoot. The
      // ±0.0005 band is the four-decimal precision of the token.
      expect(Math.max(...stops.map((stop) => stop.y)) - 1).toBeCloseTo(peak, 3);
      expect(Math.abs(Math.max(...stops.map((stop) => stop.y)) - (1 + peak))).toBeLessThan(0.0005);
      // First sample is 0, last is 1 — the spring starts at rest and
      // settles exactly.
      expect(stops[0]).toEqual({ y: 0, percent: 0 });
      expect(stops[stops.length - 1]).toEqual({ y: 1, percent: 100 });
      // The crest is its OWN stop (SYSTEM §7.1). Interpolation between
      // 4% samples would clip it, so a stop must sit at t_p/T.
      const tpPercent = (tp / T) * 100;
      expect(Math.abs(tpPercent - spec.tpStop)).toBeLessThan(0.05);
      const crest = stops.find(
        (stop) => Math.abs(stop.percent - tpPercent) <= 0.05,
      );
      expect(crest, `no t_p stop at ${tpPercent.toFixed(2)}%`).toBeDefined();
      expect(Math.abs(crest!.y - (1 + peak))).toBeLessThan(0.0005);
      // Do NOT pin the stop count: the contract pins the facts, not the
      // sampling density.
    }
  });

  it("leaves --motion-duration-fast at 150ms", () => {
    // The hover token is an addition; it never redefines the state-fade.
    expect(tokenValue("--motion-duration-fast")).toBe("var(--motion-duration-short3)");
    expect(tokenValue("--motion-duration-short3")).toBe("150ms");
    expect(tokenValue("--motion-duration-press")).toBe("90ms");
    expect(tokenValue("--motion-scale-press")).toBe("0.97");
  });

  it("uses settle for every entrance, fallback declaration FIRST", () => {
    const entrances: Array<[label: string, selector: string, source: string]> = [
      ["dialog backdrop", ".nex-dialog-backdrop", componentsCode],
      ["dialog card", ".nex-dialog-card", componentsCode],
      ["popover", ".nex-pop-enter", componentsCode],
      ["search popover", '[data-slot="search-results"]', componentsCode],
      ["view", ".nex-view-enter", componentsCode],
      ["empty", ".nex-empty-enter", componentsCode],
      ["stagger item", ".nex-stagger > *", componentsCode],
      ["snackbar", ".nex-snackbar", componentsCode],
    ];
    for (const [label, selector, source] of entrances) {
      const body = ruleBody(selector, source);
      const fallback = body.indexOf("var(--motion-spring-expressive-default-spatial-duration)");
      const settle = body.indexOf("var(--nex-sys-motion-spring-settle)");
      expect(fallback, `${label}: no non-linear() fallback`).toBeGreaterThanOrEqual(0);
      expect(settle, `${label}: does not use the settle spring`).toBeGreaterThanOrEqual(0);
      // Declaration order is the fallback: the browser drops the
      // unsupported SECOND declaration and keeps the FIRST.
      expect(fallback, `${label}: fallback must come first`).toBeLessThan(settle);
    }
  });

  it("gives exits only to the surfaces that stay mounted", () => {
    // SYSTEM §7.2: dialog, popover, snackbar. Everything else unmounts.
    for (const keyframes of ["nex-pop-exit", "nex-snackbar-exit", "nex-dialog-exit"]) {
      expect(componentsCode).toContain(`@keyframes ${keyframes}`);
    }
    for (const absent of ["nex-view-exit", "nex-empty-exit", "nex-stagger-exit", "nex-message-exit"]) {
      expect(componentsCode).not.toContain(absent);
    }
    // Exit timing: 0.55 × 343 = 189ms, standard-accelerate, no delay.
    for (const selector of ['.nex-pop-enter[data-state="closed"]', '.nex-snackbar[data-state="closed"]']) {
      const body = ruleBody(selector, componentsCode);
      expect(body).toContain("189ms var(--motion-easing-standard-accelerate) forwards");
      expect(body).not.toMatch(/\d+ms\s+(?:var\([^)]*\)\s+)?\d+ms/);
    }
    // Dialog exit is unchanged: 200ms short4.
    expect(ruleBody(".nex-dialog-card.is-closing", componentsCode)).toContain(
      "var(--motion-duration-short4)",
    );
  });

  it("turns tab create into a scale-only snap and drops expressive-fast-spatial", () => {
    const body = ruleBody(".nex-tab-enter", tabsCode);
    const fallback = body.indexOf("410ms var(--motion-easing-emphasized-decelerate)");
    const snap = body.indexOf("var(--nex-sys-motion-spring-snap)");
    expect(fallback).toBeGreaterThanOrEqual(0);
    expect(snap).toBeGreaterThanOrEqual(0);
    expect(fallback).toBeLessThan(snap);
    const keyframes = /@keyframes nex-tab-in \{([\s\S]*?)\n\}/.exec(tabsCode)?.[1] ?? "";
    expect(keyframes).toContain("scale(0.97)");
    expect(keyframes).toContain("scale(1)");
    expect(keyframes).not.toMatch(/opacity|translate/);
    expect(tabsCode).not.toContain("expressive-fast-spatial");
  });

  it("inserts messages at 200ms / emphasized-decelerate / 8px, with no spring or delay", () => {
    const body = ruleBody(".nex-message-enter", conversationCode);
    expect(body).toContain("var(--motion-duration-short4)");
    expect(body).toContain("var(--motion-easing-emphasized-decelerate)");
    expect(body).not.toMatch(/spring/);
    expect(body).not.toContain("animation-delay");
    const keyframes = /@keyframes nex-message-insert \{([\s\S]*?)\n\}/.exec(
      conversationCode,
    )?.[1] ?? "";
    expect(keyframes).toContain("translateY(8px)");
    // The stagger indexed the thread, which the contract bans.
    expect(conversationCode).not.toContain("--nex-chat-insert-index");
    const view = readRepoFile("src/components/ConversationView.tsx");
    expect(view).not.toContain("--nex-chat-insert-index");
    expect(view).not.toContain("freshOrder");
  });

  it("never animates streaming text", () => {
    // Streaming is information, not an event: nothing may select a
    // stream node with an animation. There is no such selector today,
    // and this fails the day one is written.
    expect(conversationCode).not.toMatch(/nex-(?:message-)?stream/i);
    expect(conversationCode).not.toMatch(/@keyframes[^{]*stream/i);
  });

  it("stagger: exactly five children, 50ms steps, last delay 200ms", () => {
    const rules = [
      ...componentsCode.matchAll(
        /\.nex-stagger > \*:nth-child\((\d+)\)\s*\{\s*animation-delay:\s*calc\(var\(--motion-stagger-step\) \* (\d+)\);\s*\}/g,
      ),
    ];
    expect(rules.map((match) => Number(match[1]))).toEqual([1, 2, 3, 4, 5]);
    expect(rules.map((match) => Number(match[2]))).toEqual([0, 1, 2, 3, 4]);
    // Σ = 4 × 50 = 200ms, under the 250ms contract limit.
    const STEP_MS = Number(
      /--motion-stagger-step:\s*(\d+)ms/.exec(motionCode)?.[1],
    );
    expect(STEP_MS).toBe(50);
    expect(STEP_MS * Number(rules[rules.length - 1][2])).toBe(200);
    expect(ruleBody(".nex-stagger > *", componentsCode)).toContain(
      "var(--nex-sys-motion-spring-settle)",
    );
    // No sixth (or later) staggered child.
    expect([...componentsCode.matchAll(/\.nex-stagger > \*:nth-child\(\d+\)/g)]).toHaveLength(5);
  });

  it("skeleton: 300ms delay + both, and the gate's zeroing is expected", () => {
    const body = ruleBody(".nex-skeleton-row", componentsCode);
    expect(body).toContain("animation-delay: 300ms");
    expect(body).toContain("animation-fill-mode: both");
    const skeleton = classTokens(componentSources.skeleton);
    expect(skeleton).toEqual(
      expect.arrayContaining(["[animation-delay:300ms]", "[animation-fill-mode:both]"]),
    );
    // EXPECTED: the global reduced-motion gate zeroes animation-delay
    // for every animation. A reduced-motion user is supposed to skip
    // the 300ms wait and see the static frame immediately — the delay
    // is decoration, not information.
    const gate = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(
      motionCode,
    )?.[1] ?? "";
    expect(gate).toMatch(/animation-delay:\s*0ms\s*!important/);
  });

  it("press: 0.97 / 90ms in / 411ms snap out, fallback first", () => {
    const body = ruleBody(".nex-press", componentsCode);
    const fallback = body.indexOf("410ms var(--motion-easing-emphasized-decelerate)");
    const snap = body.indexOf("var(--nex-sys-motion-spring-snap)");
    expect(fallback).toBeGreaterThanOrEqual(0);
    expect(snap).toBeGreaterThanOrEqual(0);
    expect(fallback).toBeLessThan(snap);
    expect(ruleBody(".nex-press:active:not(:disabled)", componentsCode)).toContain(
      "var(--nex-sys-motion-duration-press)",
    );
    // Paint rides the 120ms hover token, never the press clock.
    expect(body).toContain("var(--nex-sys-motion-duration-hover)");

    const button = componentSources.button.replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, "");
    expect(button).toContain("nex-press");
    expect(button).toContain("active:scale-[var(--nex-sys-motion-scale-press)]");
    expect(button).toContain("motion-reduce:active:scale-none");
    // No Tailwind transition utilities: the fallback-first declaration
    // pair cannot be expressed, and duplicate clocks would fight.
    expect(button).not.toContain("transition-[");
    expect(button).not.toContain("[transition-duration:");
    expect(button).not.toMatch(/\d+ms\b/);
    expect(button).not.toMatch(/scale-\[0\./);
  });

  it("hover: controls move to the 120ms token while fast stays 150ms", () => {
    expect(tokenValue("--nex-sys-motion-duration-hover")).toBe("120ms");
    const hoverRules: Array<[label: string, selector: string, source: string]> = [
      ["legacy button", ".nex-btn", componentsCode],
      ["tab label", ".nex-tab-label", tabsCode],
      ["conversation date", '[data-slot="conversation-date"]', componentsCode],
      ["segmented control", ".nex-seg button", componentsCode],
      ["nav entry", ".nex-nav-entry", componentsCode],
      ["snackbar action", ".nex-snackbar-action", componentsCode],
    ];
    for (const [label, selector, source] of hoverRules) {
      expect(ruleBody(selector, source), `${label} is not on the hover token`).toContain(
        "var(--nex-sys-motion-duration-hover)",
      );
    }
    // Non-hover motion keeps the 150ms clock.
    expect(ruleBody(".nex-m3-progress-linear-fill", componentsCode)).toContain(
      "var(--nex-sys-motion-duration-fast)",
    );
    expect(ruleBody(".nex-context-meter-fill", readRepoFile("src/styles/context.css").replace(/\/\*[\s\S]*?\*\//g, ""))).toContain(
      "var(--motion-duration-fast)",
    );
  });

  it("audits every transition: hover/paint token or the non-hover fast token", () => {
    // The complete sweep. A transition on neither clock is an unaudited
    // duration — the failure mode this test exists to stop.
    const dir = resolve(process.cwd(), "src/styles");
    const offenders: string[] = [];
    for (const file of readdirSync(dir).filter((name) => name.endsWith(".css"))) {
      const code = readRepoFile(`src/styles/${file}`).replace(/\/\*[\s\S]*?\*\//g, "");
      for (const match of code.matchAll(/transition:\s*([^;]+);/g)) {
        const value = match[1].replace(/\s+/g, " ").trim();
        if (value === "none") continue;
        if (
          !/var\(--nex-sys-motion-duration-hover\)|var\(--motion-duration-fast\)|var\(--nex-sys-motion-duration-fast\)/.test(
            value,
          )
        ) {
          offenders.push(`${file}: ${value}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it("zen: three chrome layers, 40ms step, 189ms, content not moved", () => {
    const exiting = [...tabsCode.matchAll(/\.nex-app\.nex-zen-exiting ([^{]+)\{([^}]*)\}/g)];
    expect(exiting).toHaveLength(3);
    const names = exiting.map((match) => match[1].trim());
    expect(names).toEqual([
      '[data-slot="sidebar"]',
      ".nex-tabstrip",
      ".nex-pane-header",
    ]);
    const delays = exiting.map((match) => {
      const value = match[2];
      const delay = /nex-zen-[a-z]+-exit 189ms var\(--motion-easing-emphasized-decelerate\) (\d+)ms both/.exec(
        value,
      );
      expect(delay, `layer has no 189ms emphasized-decelerate exit: ${value}`).not.toBeNull();
      return Number(delay![1]);
    });
    expect(delays).toEqual([0, 40, 80]);
    // 189 = 0.55 × 343. No delay token exists; the step is raw by
    // contract and the helper below is its JS half.
    expect(Math.round(0.55 * 343)).toBe(189);
    const zenKeyframes = (name: string): string =>
      new RegExp(`@keyframes ${name} \\{([\\s\\S]*?)\\n\\}`).exec(tabsCode)?.[1] ?? "";
    expect(zenKeyframes("nex-zen-sidebar-exit")).toContain("translateX(-8px)");
    expect(zenKeyframes("nex-zen-strip-exit")).toContain("translateY(-4px)");
    expect(zenKeyframes("nex-zen-header-exit")).toContain("translateY(-4px)");

    // The helper table App.tsx schedules on.
    expect(ZEN_EXIT_STEP_MS).toBe(40);
    expect(ZEN_EXIT_LAYER_MS).toBe(189);
    expect(ZEN_EXIT_TOTAL_MS).toBe(269);
    expect([zenExitDelayMs(false), zenExitDelayMs(true)]).toEqual([269, 0]);
  });
});

/* ----------------------------------------------------------
   Parity — the tokens.css relocation (#230 Phase 6).

   Every `--nex-sys-*` name that still has a var() caller was
   relocated to src/index.css as a flat twin (prefix stripped) or,
   for elevation / z, kept canonical and moved here. VALUES do not
   change: resolve(newName, theme) must equal the base literal the
   old name resolved to in the BASE commit's tokens.css, for every
   theme (dark, light, high-contrast).

   PARITY_CASES is committed test data (src/test/parityMap.ts),
   generated by resolving the BASE tokens.css per theme. Names are
   not compared — only resolved literals.
   ---------------------------------------------------------- */
describe("parity — tokens.css relocation (#230 Phase 6)", () => {
  const css = readRepoFile("src/index.css").replace(/\/\*[\s\S]*?\*\//g, "");

  function ruleBody(selector: RegExp, label: string): string {
    // The selector may be followed by comma-separated co-selectors across
    // lines before the opening brace (e.g. the high-contrast block), so
    // allow anything between the selector name and `{`.
    const match = new RegExp(selector.source + '[\\s\\S]*?\\{([\\s\\S]*?)\\n\\}').exec(css);
    if (!match) throw new Error(`src/index.css: no block matching ${label}`);
    return match[1];
  }

  function defsOf(body: string): Map<string, string> {
    const out = new Map<string, string>();
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([\s\S]*?);/gi)) out.set(m[1], m[2].trim());
    return out;
  }

  const DARK_BLOCK = ruleBody(/:root,\s*:root\[data-theme="dark"\]/, "dark/default theme block");
  const LIGHT_BLOCK = ruleBody(/:root\[data-theme="light"\]/, "light theme block");
  const HC_BLOCK = ruleBody(/:root\[data-theme="high-contrast"\]/, "high-contrast theme block");

  const darkDefs = defsOf(DARK_BLOCK);
  const lightDefs = new Map(darkDefs);
  for (const [k, v] of defsOf(LIGHT_BLOCK)) lightDefs.set(k, v);
  const hcDefs = new Map(darkDefs);
  for (const [k, v] of defsOf(HC_BLOCK)) hcDefs.set(k, v);

  const THEMES: Array<{ label: string; key: "dark" | "light" | "hc"; defs: Map<string, string> }> = [
    { label: "dark/default", key: "dark", defs: darkDefs },
    { label: "light", key: "light", defs: lightDefs },
    { label: "high-contrast", key: "hc", defs: hcDefs },
  ];

  /** Follow `var()` chains recursively, like the browser does. */
  function resolve(value: string, defs: Map<string, string>, seen = new Set<string>()): string {
    return value.replace(
      /var\(\s*(--[a-z0-9-]+)(?:\s*,\s*([\s\S]*?))?\s*\)/g,
      (match, name, fallback) => {
        if (seen.has(name)) return fallback || "";
        const def = defs.get(name);
        if (def === undefined) return fallback || match;
        seen.add(name);
        const resolved = resolve(def, defs, seen);
        seen.delete(name);
        return resolved;
      },
    );
  }

  it("covers every old name that still has a var() caller, and every target exists", () => {
    // Sanity on the committed data: no duplicate new names, every case
    // has a base literal for all three themes.
    const news = PARITY_CASES.map((c) => c.new);
    expect([...new Set(news)].length).toBe(news.length);
    for (const c of PARITY_CASES) {
      expect(c.dark).toBeDefined();
      expect(c.light).toBeDefined();
      expect(c.hc).toBeDefined();
    }
  });

  const norm = (s: string) => s.replace(/\s+/g, " ").trim();
  for (const c of PARITY_CASES) {
    for (const theme of THEMES) {
      it(`${c.old} -> ${c.new} resolves to the base literal in the ${theme.label} theme`, () => {
        const value = theme.defs.get(c.new);
        expect(value, `${c.new} is not defined in the ${theme.label} theme block`).toBeDefined();
        const resolved = resolve(value!, theme.defs);
        expect(norm(resolved)).toBe(norm(c[theme.key]));
      });
    }
  }
});
