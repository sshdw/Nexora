//! Style coverage for the P2 chrome (#230 Phase 2).
//!
//! Two classes shipped for months with NO rule anywhere in the tree —
//! `.nex-workspace-chip` (used by WorkspaceChip.tsx) and, on the
//! reverse side, `.nex-main-header` / `.nex-main-body`, whose rules
//! exist in src/styles.css while no component references them. A
//! className with no rule is not a styling bug the eye catches: the
//! element renders, it just renders as whatever the cascade happens
//! to leave behind. So the gate is structural, and it is bidirectional
//! — this is the test that would have caught both.
//!
//! The second half holds the sheet to the token contract: the tabs +
//! pane-header sections declare no raw hex and no raw millisecond
//! duration. Token durations are not a style preference here — they
//! are what lets the ONE global prefers-reduced-motion gate in
//! motion.css collapse a transition to a single frame (motion.test.tsx
//! owns that block and pins that there is exactly one).

import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

/** Every stylesheet the components layer loads, as one text blob. */
function allCss(): string {
  const dir = resolve(process.cwd(), "src/styles");
  return [
    readRepoFile("src/styles.css"),
    ...readdirSync(dir)
      .filter((name) => name.endsWith(".css"))
      .map((name) => readRepoFile(`src/styles/${name}`)),
  ].join("\n");
}

/** Every class token the three P2 components actually put on the DOM. */
function emittedClasses(source: string): string[] {
  const tokens = new Set<string>();
  for (const match of source.matchAll(/className\s*=\s*(?:"([^"]*)"|\{([^}]*)\})/g)) {
    const literal = match[1];
    // A plain literal is the class list itself; a braced one is an
    // expression, so its quoted arms ("a", cond ? "b" : "c") are the
    // lists. Reading the arm's own quotes as if they were the outer
    // pair is what returns nothing for every plain className.
    const lists = literal === undefined
      ? [...(match[2] ?? "").matchAll(/"([^"]*)"/g)].map((arm) => arm[1] ?? "")
      : [literal];
    for (const list of lists) {
      for (const token of list.split(/\s+/)) {
        if (token) tokens.add(token);
      }
    }
  }
  return [...tokens].sort();
}

/** Every class SELECTOR the components layer can see: comments
 *  stripped, names whole. A plain `css.includes("." + token)` lookup
 *  is not a gate — a longer class CONTAINS a shorter one
 *  (`.nex-tabstrip` contains `.nex-tab`, `.nex-tabstrip-list`
 *  contains `.nex-tabstrip`), and a comment may NAME a class that has
 *  no rule at all. Both satisfy the substring. Deleting the `.nex-tab`
 *  rule — the exact regression this file exists for — went green on
 *  it. So the names come from the selectors themselves, and each one
 *  is taken whole: a class name ends where its name ends. */
function classNames(css: string): string[] {
  const uncommented = css
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^[ \t]*\/\/.*$/gm, "");
  return [
    ...new Set(
      [...uncommented.matchAll(/\.([_a-zA-Z][\w-]*)/g)].map((match) => match[1] ?? ""),
    ),
  ].sort();
}

/** One declaration's VALUE out of a rule body. The LAST occurrence
 *  wins in CSS, so the last match is the one a browser would apply —
 *  a first-match read would report a declaration that is overridden
 *  two lines down. */
function declaration(rule: string, property: string): string {
  const found = [...rule.matchAll(new RegExp(`(?:^|;)\\s*${property}:\\s*([^;]+)`, "g"))];
  return found[found.length - 1]?.[1]?.trim() ?? "";
}

const CSS = allCss();

/** The selector names `CSS` actually declares. */
const CLASS_NAMES = new Set(classNames(CSS));

/** The P2 components, with the classNames they own. */
const SOURCES: Record<string, string> = {
  ConversationTabs: readRepoFile("src/components/ConversationTabs.tsx"),
  ConversationPane: readRepoFile("src/components/ConversationPane.tsx"),
  WorkspaceChip: readRepoFile("src/components/WorkspaceChip.tsx"),
};

describe("P2 chrome — no unstyled className", () => {
  it("extracts the class tokens it claims to", () => {
    // The anti-vacuity pin, same shape as theme.test.ts's: an extractor
    // that quietly returns nothing would make every assertion below
    // pass on an empty set, forever.
    const classes = emittedClasses(SOURCES.ConversationTabs);
    expect(classes).toContain("nex-tabstrip");
    expect(classes).toContain("nex-tab");
    expect(classes).toContain("nex-tab-enter");
    expect(classes).toContain("is-active");
    expect(emittedClasses(SOURCES.WorkspaceChip)).toEqual([
      "nex-workspace-chip",
      "nex-workspace-chip-text",
    ]);
    expect(emittedClasses(SOURCES.ConversationPane)).toContain(
      "nex-pane-header-separator",
    );
  });

  it("scans whole class names, and only names a selector declares", () => {
    // Anti-vacuity pin for the scan the check below leans on.
    // `.nex-tabstrip-list` is ONE name — a scan that reported prefixes
    // would satisfy `.nex-tab` out of `.nex-tabstrip` again — and a
    // name that lives only inside a comment is not a rule.
    expect(classNames(".nex-tabstrip-list")).toEqual(["nex-tabstrip-list"]);
    expect(
      classNames("/* .nex-comment-only */\n.a,\n.b.is-active { color: red }"),
    ).toEqual(["a", "b", "is-active"]);
  });

  for (const [name, source] of Object.entries(SOURCES)) {
    it(`${name}: every class it emits has a rule`, () => {
      // A modifier lives inside a compound selector
      // (`.nex-tab.is-active`), so both halves land in CLASS_NAMES in
      // their own right and each is checked on its own.
      const missing = emittedClasses(source).filter((token) => !CLASS_NAMES.has(token));
      expect(missing).toEqual([]);
    });
  }
});

describe("P2 chrome — token discipline in the tabs + pane-header sheet", () => {
  const css = readRepoFile("src/styles/tabs.css").replace(/\/\*[\s\S]*?\*\//g, "");

  it("declares no raw colour value", () => {
    expect(css).not.toMatch(/#[0-9a-f]{3,8}\b/i);
    expect(css).not.toMatch(/\brgba?\(/i);
  });

  it("declares no raw duration — every one is a token", () => {
    // The gate that collapses reduced motion cannot collapse a literal.
    expect(css).not.toMatch(/\d+ms\b/);
    const durations = [...css.matchAll(/var\((--[\w-]*(?:duration|travel)[\w-]*)\)/g)];
    expect(durations.length).toBeGreaterThan(0);
  });

  it("adds no second reduced-motion block", () => {
    // motion.css owns the one global gate; a local copy here would be a
    // second source of truth (and motion.test.tsx fails on that shape).
    expect(css).not.toMatch(/@media\s*\(prefers-reduced-motion/);
  });

  it("keeps the active tab to a tick plus a tone step", () => {
    const tab = /\.nex-tab \{([\s\S]*?)\}/.exec(css)?.[1] ?? "";
    // No pill and no wash: the row is text, not a container.
    expect(tab).not.toMatch(/border-radius/);
    expect(tab).not.toMatch(/background/);
    const active = /\.nex-tab\.is-active \{([\s\S]*?)\}/.exec(css)?.[1] ?? "";
    expect(active).not.toMatch(/background/);
    // The tick: bottom-anchored and 2px (--nex-sys-space-xxxs).
    const tick = /\.nex-tab\.is-active::after \{([\s\S]*?)\}/.exec(css)?.[1] ?? "";
    expect(tick).toMatch(/bottom:\s*0/);
    expect(tick).toMatch(/height:\s*var\(--space-xxxs\)/);
    // …AND its indicator colour, which is what this test was blind
    // to for months. Nothing watched the hue, so a role-shaped alias
    // that resolves to a hue shipped green: `--nex-sys-color-
    // accent-indicator` READS like a role and IS a role — it just
    // aliases the legacy amber primary. A bare "has a background"
    // check would sail straight past it, so the value is pinned whole.
    expect(declaration(tick, "background")).toBe("var(--color-tick)");
    // Named explicitly, because this is the specific way back: any
    // other amber-bearing alias (`--nex-sys-color-primary` direct)
    // fails the pin above, but the alias that started it should not
    // be able to come back through a door this file can see. `css`
    // has comments stripped, so the sheet's prose ABOUT this alias
    // cannot satisfy or trip it — only a declaration can.
    expect(tick).not.toMatch(/--nex-sys-color-accent-indicator/);
  });

  it("lets both split tracks shrink", () => {
    expect(css).toMatch(/grid-template-columns:\s*minmax\(0,\s*1fr\)\s*minmax\(0,\s*1fr\)/);
  });

  it("holds the header's trailing group at its intrinsic width", () => {
    // `.nex-main-header-actions` sits on the M3Toolbar in the pane
    // header. It must live in THIS sheet, next to the header rules
    // it belongs to: it is what stops a long workspace name from
    // flexing the trailing group and squeezing the export control
    // off the row, and the title beside it is the part allowed to
    // truncate.
    const rule = /\.nex-main-header-actions \{([\s\S]*?)\}/.exec(css)?.[1] ?? "";
    expect(rule).toMatch(/flex-shrink:\s*0/);
    // …and nothing else: display/alignment/gap belong to
    // `.nex-m3-toolbar-docked`, which this sheet out-ranks, so a
    // duplicate would silently re-space the row.
    expect(rule.split(";").filter((line) => line.trim() !== "")).toHaveLength(1);
    // The legacy sheet is not the second home for it.
    const legacy = readRepoFile("src/styles.css");
    expect(legacy).not.toMatch(/\.nex-main-header-actions\s*\{/);
  });
});