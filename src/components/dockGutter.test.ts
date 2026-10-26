//! Stage + Dock gutter contract (owner decision B).
//!
//! The Dock is a fixed rail pinned to the left edge, vertically centred, so
//! it is painted ON TOP of the conversation stage rather than beside it. The
//! stage never reserved its width, and the empty state's sentence ran
//! underneath the rail ("o messages yet") on the real build at 1200x800 and
//! 1024x720.
//!
//! jsdom computes no cascade and no layout, so a render test cannot see this:
//! it can prove which elements exist, never where they land or which box
//! contains them. The three properties that make the fix correct are
//! therefore pinned against the SHEETS and the SOURCE, in the style of
//! `threadSheet.test.ts` / `chromeHeight.test.ts`:
//!
//!  1. ONE SOURCE OF TRUTH. `--dock-width` is declared once, in the default
//!     theme block, and BOTH the rail and the stage reserve read it. A second
//!     copy of `60px` on either side is how the rail and the reserve drift
//!     apart.
//!
//!  2. THE INSET IS THE CONTAINER'S, NOT THE CHILD'S. It is
//!     `padding-inline` on `.nex-main-conversation` — the box that owns both
//!     the thread and the composer — and neither child, nor the empty-state
//!     copy, carries it. A per-element margin is what turns the thread and
//!     the composer into two unrelated columns.
//!
//!  3. SYMMETRY IS THE CENTRE. `padding-inline` (one value, both sides) keeps
//!     the content box centred at 50vw, so a centred reading column is not
//!     shifted by half the rail. Separate left/right declarations would let
//!     a later pass change one side.
//!
//!  4. A SCROLL VIEWPORT'S SCROLLBAR IS PAINT. The rail reserves room for
//!     content, so a scroll container inside the inset must cancel it — but
//!     with PADDING, not the band's transparent border: a scrollbar is
//!     painted in the padding box, so a border leaves it at width − 60. The
//!     two halves therefore net to zero on content (margin −inset, padding
//!     +inset) while the scrollbar reaches the window edge.
//!
//! The empty-state clearance is also computed here, because "it clears the
//! rail" is arithmetic the sheets fully determine: the stage inset plus the
//! thread's and the placeholder's own padding must exceed the rail width.

import { describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

/** Comments are prose, not declarations — a token named in a comment must
 *  never satisfy a pin. */
function code(path: string): string {
  return readRepoFile(path).replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Body of a top-level rule, matched by its EXACT selector. */
function ruleBody(sheet: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(sheet);
  if (!match) throw new Error(`no top-level rule for ${selector}`);
  return match[1];
}

/** Body of the LAST top-level rule for a selector. One class in this file is
 *  legitimately declared twice — `.nex-conversation-placeholder` appears in
 *  the shared empty-state group and again as its own conversation rule — and
 *  the later rule is the one that places it. */
function lastRuleBody(sheet: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matches = [
    ...sheet.matchAll(new RegExp(`(?:^|\\n)${escaped}\\s*\\{([\\s\\S]*?)\\n\\}`, "g")),
  ];
  const last = matches[matches.length - 1];
  if (!last) throw new Error(`no top-level rule for ${selector}`);
  return last[1];
}

/** The LAST declaration of `property` in a rule body — last wins in CSS. */
function declaration(body: string, property: string): string | undefined {
  const matches = [...body.matchAll(new RegExp(`(?:^|[;{\\s])${property}\\s*:\\s*([^;]+);`, "gi"))];
  return matches[matches.length - 1]?.[1]?.trim();
}

/** Every custom property declared in src/index.css, last declaration wins. */
const TOKENS = new Map<string, string>();
for (const match of code("src/index.css").matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
  TOKENS.set(match[1] ?? "", (match[2] ?? "").trim());
}

/** Follow `var()` references to literals, like the browser at computed time. */
function resolve(value: string | undefined): string | undefined {
  let current = value;
  const seen = new Set<string>();
  while (current) {
    const ref = /^var\(\s*(--[a-z0-9-]+)\s*\)$/i.exec(current.trim());
    if (!ref) return current;
    const name = ref[1] ?? "";
    if (seen.has(name)) throw new Error(`circular token: ${name}`);
    seen.add(name);
    current = TOKENS.get(name);
  }
  return undefined;
}

/** Replace every `var(--token)` in a value with its resolved literal. */
function resolveInline(value: string): string {
  return value.replace(/var\(\s*(--[a-z0-9-]+)\s*\)/g, (_match, name: string) => {
    const resolved = resolve(`var(${name})`);
    if (resolved === undefined) throw new Error(`unresolved token: ${name}`);
    return resolved;
  });
}

/** The rail's width, as the sheets resolve it. The ONE concrete number in
 *  this file is the token's own declaration (pinned once in the first
 *  describe); every other expectation compares against this, so a width
 *  change that keeps the contract does not fail a test. */
const RAIL_WIDTH = resolve("var(--dock-width)") ?? "";

/** `60px` as written by hand instead of `var(--dock-width)`. */
function railLiteral(): RegExp {
  return new RegExp(`\\b${RAIL_WIDTH.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
}

/** The reading measure — the ONE column the thread's rows, the empty state
 *  and the composer are all bounded by. Resolved, never hardcoded, so a
 *  measure change that keeps the contract does not fail a test. */
const MEASURE = resolve("var(--layout-measure-max)") ?? "";

/** The narrowest window the app ships at (`tauri.conf.json`), so the axis
 *  can be proved where it is tightest without inventing a width. */
const MIN_WINDOW_WIDTH = Number(
  /"minWidth"\s*:\s*(\d+)/.exec(readRepoFile("src-tauri/tauri.conf.json"))?.[1],
);

/** Where the shared column lands at a given window width: the stage's
 *  symmetric reserve, then the measure centred inside it — clamped to the
 *  stage's content box when the window is narrower than the measure, which
 *  is the same clamp on both children. */
function sharedColumn(windowWidth: number): {
  left: number;
  width: number;
  centre: number;
} {
  const inset = Number.parseFloat(RAIL_WIDTH);
  const measure = Number.parseFloat(MEASURE);
  const stageContent = windowWidth - 2 * inset;
  const width = Math.min(measure, stageContent);
  const left = inset + (stageContent - width) / 2;
  return { left, width, centre: left + width / 2 };
}

const indexSheet = code("src/index.css");
const stageSheet = code("src/styles/conversation.css");
const composerSheet = code("src/styles/composer.css");
const tabsSheet = code("src/styles/tabs.css");
const contextSheet = code("src/styles/context.css");
const shellSheet = code("src/styles.css");
const dockSource = readRepoFile("src/components/Dock.tsx");
const viewSource = readRepoFile("src/components/ConversationView.tsx");
const contextSource = readRepoFile("src/components/ContextPanel.tsx");
const composerSource = readRepoFile("src/components/Composer.tsx");

/** `<div` / `</div>` nesting depth at a source index. The JSX in
 *  `ConversationView` carries no literal `<div` inside a string or a comment,
 *  so a tag scan is enough to tell a direct child of the stage from a nested
 *  one. */
function divDepthAt(index: number): number {
  const before = viewSource.slice(0, index);
  const opens = (before.match(/<div\b/g) ?? []).length;
  const closes = (before.match(/<\/div>/g) ?? []).length;
  return opens - closes;
}

describe("--dock-width — one source of truth for the rail (plan §5.2)", () => {
  it("declares --dock-width at 60px, exactly once, in src/index.css", () => {
    const declarations = [
      ...indexSheet.matchAll(/(?:^|[;{\s])--dock-width\s*:\s*([^;]+);/g),
    ].map((match) => (match[1] ?? "").trim());
    expect(declarations).toEqual(["60px"]);
  });

  it("keeps the token theme-independent (a length, not a paint)", () => {
    // Declared in the default `:root` block, which the light and
    // high-contrast blocks inherit — the same discipline `--chrome-height`
    // follows. Restating it per theme is how the rail width acquires two
    // values that disagree in one theme.
    const lightBlock = ruleBody(indexSheet, ':root[data-theme="light"]');
    expect(declaration(lightBlock, "--dock-width")).toBeUndefined();
    expect(resolve("var(--dock-width)")).toBe("60px");
  });

  it("sizes the Dock from the token and spells no rail width literal", () => {
    expect(dockSource).toContain('width: "var(--dock-width)"');
    // No second copy of the number on the component side.
    expect(dockSource).not.toMatch(/\bwidth:\s*60\b/);
  });
});

describe("conversation stage reserves the rail at the container", () => {
  const stage = ruleBody(stageSheet, ".nex-main-conversation");

  it("declares the inset as one symmetric padding-inline from the token", () => {
    expect(declaration(stage, "padding-inline")).toBe("var(--dock-width)");
    // The inset resolves to the rail's own width — the reserve is exactly the
    // rail, not a second number that happens to be close.
    expect(resolve(declaration(stage, "padding-inline"))).toBe(RAIL_WIDTH);
    // Symmetry is the invariant: separate sides can drift apart, and a
    // shorthand `padding` after the inline inset would silently win.
    expect(declaration(stage, "padding-left")).toBeUndefined();
    expect(declaration(stage, "padding-right")).toBeUndefined();
    expect(declaration(stage, "padding")).toBeUndefined();
    expect(declaration(stage, "margin-inline")).toBeUndefined();
  });

  it("puts no Dock offset on the composer or the empty copy", () => {
    // The fix is the container's. If any of these carried the token
    // themselves, the thread and the composer would be two columns that can
    // drift, which is the thing this contract forbids. The thread's own
    // scroll-viewport opt-out is a margin/padding PAIR that nets to zero on
    // its content box (round 4, below) — not an offset — so it is excluded
    // here and pinned there.
    const ownedBlocks: Array<[string, string]> = [
      ["conversation.css .nex-conversation-placeholder", ruleBody(stageSheet, ".nex-conversation-placeholder")],
      ["conversation.css .nex-conversation-empty-text", ruleBody(stageSheet, ".nex-conversation-empty-text")],
      ["composer.css .nex-composer", ruleBody(composerSheet, ".nex-composer")],
      ["composer.css .nex-composer-inner", ruleBody(composerSheet, ".nex-composer-inner")],
    ];
    for (const [label, body] of ownedBlocks) {
      expect(body, label).not.toMatch(/--dock-width/);
    }
    // The composer's reading column stays centred in whatever box the stage
    // hands it, so it shares the thread's axis instead of its own.
    expect(declaration(ruleBody(composerSheet, ".nex-composer-inner"), "margin-inline")).toBe("auto");
  });

  it("clears the empty state past the rail by arithmetic, not by eye", () => {
    // Round 3 moved the empty state ONTO the shared column, so the
    // sentence's left edge is the column's left edge: the stage inset plus
    // the centring offset, with no per-text padding left to add into the
    // sum. Proved at the window's own minimum width — the tightest case the
    // app ships — rather than at a comfortable one.
    expect(Number.isFinite(MIN_WINDOW_WIDTH), "minWidth is readable").toBe(true);
    const column = sharedColumn(MIN_WINDOW_WIDTH);
    expect(Number.isFinite(column.left), "the column's left edge is a length").toBe(true);
    expect(Number.isFinite(column.width), "the column's width is a length").toBe(true);

    // THE CONTRACT: the sentence's left edge starts strictly right of the
    // rail's right edge, and the centring alone is what puts it there.
    expect(column.left).toBeGreaterThan(Number.parseFloat(RAIL_WIDTH));

    // …and the empty copy is not padded off the column it belongs to, so
    // the sum above is complete: what places the text is the column.
    const placeholder = lastRuleBody(stageSheet, ".nex-conversation-placeholder");
    expect(resolveInline(declaration(placeholder, "padding-inline") ?? "")).toBe("0");
  });

  it("keeps the stage's own chrome row full-bleed", () => {
    // `.nex-context-tabs` sits above the rail's vertical band and its
    // surface band must still span the pane, so it cancels the stage inset.
    const tabs = ruleBody(contextSheet, ".nex-context-tabs");
    expect(declaration(tabs, "margin-inline")).toBe("calc(-1 * var(--dock-width))");
    expect(resolveInline(declaration(tabs, "margin-inline") ?? "")).toBe(`calc(-1 * ${RAIL_WIDTH})`);
  });

  it("does not reach the tab strip or the pane header", () => {
    // Owner scope: the inset is the conversation stage's only. The chrome
    // rows keep their full-width placement.
    expect(tabsSheet).not.toMatch(/--dock-width/);
    expect(shellSheet).not.toMatch(/--dock-width/);
  });
});

describe("thread and composer share one stage box", () => {
  const containerAt = viewSource.indexOf('<div className="nex-main-conversation">');

  it("renders the thread and the composer as children of the stage container", () => {
    expect(containerAt).toBeGreaterThan(-1);
    // One stage per view; split panes each render their own.
    expect([...viewSource.matchAll(/className="nex-main-conversation"/g)]).toHaveLength(1);

    const threadAt = viewSource.indexOf('<div className="nex-thread"');
    const composerAt = viewSource.indexOf("<Composer");
    expect(threadAt).toBeGreaterThan(containerAt);
    expect(composerAt).toBeGreaterThan(threadAt);
    // Depth 1 from the container: neither is nested in its own wrapper, so
    // both are laid out by the one padded box the sheet insets.
    expect(divDepthAt(threadAt)).toBe(1);
    expect(divDepthAt(composerAt)).toBe(1);
  });
});

// ============================================================
// Round 2 — the rail reserves room for CONTENT, never for PAINT.
//
// Round 1 reserved the rail on the stage container, which also shrank every
// band that used to run edge to edge. The composer's footer band (its
// `--background` tone, one step off the stage canvas, plus the hairline that
// opens it) then stopped at x = 60 and at width − 60: a hard vertical edge on
// both sides and a horizontal step where the band began.
//
// The opt-out therefore has two halves, and the SECOND one is what makes it
// safe to apply to a band that carries the reading column:
//
//   1. `margin-inline: calc(-1 * var(--dock-width))` — the band's border box
//      reaches both window edges, so its background and its top hairline
//      paint full-bleed again.
//   2. `border-inline: var(--dock-width) solid transparent` — the width the
//      margin bought back is spent on invisible border, so the padding box
//      (every child's containing block) is EXACTLY the box the stage's
//      padding left it in. No per-band padding arithmetic, and unlike a
//      margin-only cancel it still holds at narrow widths, where the centred
//      760px column is clamped by the stage box and would otherwise cross
//      x = 60.
//
// What is pinned below: the bands opt out, the content rows do not, and the
// sweep is COMPLETE — every direct child of the stage has an entry, so a
// surface added later cannot silently end at the inset.
// ============================================================

describe("the composer band keeps its paint full-bleed (round 2)", () => {
  const band = ruleBody(stageSheet, ".nex-main-conversation > .nex-composer");

  it("cancels the stage reserve on the band, from the one rail token", () => {
    expect(declaration(band, "margin-inline")).toBe("calc(-1 * var(--dock-width))");
    expect(resolveInline(declaration(band, "margin-inline") ?? "")).toBe(`calc(-1 * ${RAIL_WIDTH})`);
  });

  it("spends the borrowed width on transparent border, so content stays reserved", () => {
    expect(declaration(band, "border-inline")).toBe("var(--dock-width) solid transparent");
    const resolved = resolveInline(declaration(band, "border-inline") ?? "");
    expect(resolved).toBe(`${RAIL_WIDTH} solid transparent`);
    // The border width IS the reserve: content box = stage content box, so
    // the composer column cannot drift with the band's paint.
    expect(Number.parseFloat(resolved)).toBe(Number.parseFloat(RAIL_WIDTH));
  });

  it("moves geometry only — the band's tone and hairline stay in composer.css", () => {
    const composer = ruleBody(composerSheet, ".nex-composer");
    expect(declaration(composer, "background")).toBe("var(--background)");
    expect(declaration(composer, "border-top")).toBe("1px solid var(--border)");
    // The band's own rule carries no Dock opt-out of its own, and the
    // stage-side opt-out repaints nothing.
    expect(composer).not.toMatch(/--dock-width/);
    expect(declaration(composer, "margin-inline")).toBeUndefined();
    expect(declaration(composer, "border-inline")).toBeUndefined();
    expect(band).not.toMatch(/(?:^|[;{\s])background\s*:/);
    expect(band).not.toMatch(/(?:^|[;{\s])border-(?:top|bottom)\s*:/);
  });

  it("spells no raw pixel value in the opt-out", () => {
    // Geometry comes from the token, so the band and the reserve can never
    // disagree about how wide the rail is.
    expect(band).not.toMatch(/\d+px/);
  });
});

describe("stage sweep — no surface may be left ending at the inset", () => {
  /** Root class → where its layout rule lives and how it must treat the
   *  reserve. `band` = paints a tone and/or a hairline ACROSS the stage, so
   *  it must opt out; `scroll` = a scroll container whose SCROLLBAR is paint,
   *  so it opts out too, with padding instead of a transparent border;
   *  `content` = paints no band and scrolls not, so it must not touch the
   *  reserve at all. The completeness test below fails when a new direct
   *  child of `.nex-main-conversation` is added without a decision. */
  const SURFACES: Array<{
    root: string;
    sheet: string;
    selector: string;
    kind: "band" | "scroll" | "content";
    note: string;
  }> = [
    {
      root: "nex-context-tabs",
      sheet: contextSheet,
      selector: ".nex-context-tabs",
      kind: "band",
      note: "stage chrome row: tone band + hairline, above the rail's vertical band",
    },
    {
      root: "nex-composer",
      sheet: stageSheet,
      selector: ".nex-main-conversation > .nex-composer",
      kind: "band",
      note: "composer footer band: --background tone + top hairline",
    },
    {
      root: "nex-thread",
      sheet: stageSheet,
      selector: ".nex-main-conversation > .nex-thread",
      kind: "scroll",
      note: "scroll viewport: paints no tone, but its scrollbar is paint",
    },
    {
      root: "nex-context",
      sheet: contextSheet,
      selector: ".nex-main-conversation > .nex-context",
      kind: "scroll",
      note: "context scroller: no band, scrollbar is paint",
    },
    {
      root: "nex-composer-strip-error",
      sheet: composerSheet,
      selector: ".nex-composer-strip-error",
      kind: "content",
      note: "centred card above the composer, not a full-width band",
    },
  ];

  /** Root classes of the stage's DIRECT children, read from the component
   *  sources: `<div className="nex-…">` at depth 1, plus the two components
   *  rendered at depth 1. A component's root class is named here (its file
   *  contains nested `<div>`s whose classes are not the stage's children) and
   *  checked against that component's own source. */
  function stageChildRoots(): string[] {
    const roots = new Set<string>();
    for (const match of viewSource.matchAll(/<div className="(nex-[a-z-]+)(?: [^"]*)?"/g)) {
      if (divDepthAt(match.index ?? 0) === 1) roots.add(match[1] ?? "");
    }
    const components: Array<[string, string, string]> = [
      ["<ContextPanel", contextSource, "nex-context"],
      ["<Composer", composerSource, "nex-composer"],
    ];
    for (const [tag, source, root] of components) {
      const at = viewSource.indexOf(tag);
      if (at > -1 && divDepthAt(at) === 1) {
        // The root element of the component itself, not a nested element.
        expect(source, tag).toMatch(new RegExp(`<div className="${root}(?:[ "]|$)`));
        roots.add(root);
      }
    }
    return [...roots].sort();
  }

  it("accounts for every direct child of the stage", () => {
    expect(stageChildRoots()).toEqual(SURFACES.map((surface) => surface.root).sort());
  });

  it("opts every band out of the inset, with the token and no literal", () => {
    for (const surface of SURFACES.filter((entry) => entry.kind === "band")) {
      const body = ruleBody(surface.sheet, surface.selector);
      expect(declaration(body, "margin-inline"), surface.note).toBe("calc(-1 * var(--dock-width))");
      expect(resolveInline(declaration(body, "margin-inline") ?? ""), surface.note).toBe(
        `calc(-1 * ${RAIL_WIDTH})`,
      );
      // The literal the token replaces must not come back as a hand-typed
      // rail width; a 1px hairline elsewhere in the rule is not a rail width.
      expect(body, surface.note).not.toMatch(railLiteral());
    }
  });

  it("opts every scroll viewport out with a margin/padding pair, never a border", () => {
    const stageInset = declaration(ruleBody(stageSheet, ".nex-main-conversation"), "padding-inline");
    for (const surface of SURFACES.filter((entry) => entry.kind === "scroll")) {
      const body = ruleBody(surface.sheet, surface.selector);
      // Half one, shared with the bands: cancel exactly the reserve the
      // stage applies, so the border box reaches both window edges.
      expect(declaration(body, "margin-inline"), surface.note).toBe(
        `calc(-1 * ${stageInset})`,
      );
      // Half two is PADDING here. A scrollbar is painted in the padding box,
      // so a transparent border would leave it at width − 60 (measured in
      // the running build); padding puts the padding-box edge — the
      // scrollbar — at the window edge.
      expect(declaration(body, "padding-inline"), surface.note).toBeDefined();
      expect(declaration(body, "border-inline"), surface.note).toBeUndefined();
      expect(declaration(body, "border"), surface.note).toBeUndefined();
      expect(body, surface.note).not.toMatch(railLiteral());
    }
    // The thread's inline padding was 0, so its padding is exactly the
    // reserve: content box = stage content box, unchanged.
    expect(declaration(ruleBody(stageSheet, ".nex-main-conversation > .nex-thread"), "padding-inline")).toBe(
      stageInset,
    );
    // The context panel already spent `--space-xl` of inline padding, so it
    // gets the reserve PLUS its own inset — otherwise the panel's content
    // would move by that inset.
    expect(
      declaration(ruleBody(contextSheet, ".nex-main-conversation > .nex-context"), "padding-inline"),
    ).toBe(`calc(${stageInset} + var(--space-xl))`);
  });

  it("keeps the reading column reserved inside a full-bleed band", () => {
    // Chrome that sits above the rail's vertical band returns to the pane
    // edge (round 1, unchanged): margin only, no compensating border.
    const tabs = ruleBody(contextSheet, ".nex-context-tabs");
    expect(declaration(tabs, "border-inline")).toBeUndefined();
    // The band that CARRIES the reading column keeps the content box: the
    // transparent border spends exactly the width the margin bought.
    const band = ruleBody(stageSheet, ".nex-main-conversation > .nex-composer");
    expect(declaration(band, "border-inline")).toBe("var(--dock-width) solid transparent");
  });

  it("leaves every content row on the inset, with no opt-out of its own", () => {
    for (const surface of SURFACES.filter((entry) => entry.kind === "content")) {
      const body = ruleBody(surface.sheet, surface.selector);
      expect(body, surface.note).not.toMatch(/--dock-width/);
      expect(body, surface.note).not.toMatch(/calc\(\s*-1\s*\*/);
    }
    // The composer's reading column and its rows stay ordinary children of
    // the reserved box — no per-text margins, no second axis.
    const inner = ruleBody(composerSheet, ".nex-composer-inner");
    const hint = ruleBody(composerSheet, ".nex-composer-hint");
    expect(declaration(inner, "margin-inline")).toBe("auto");
    for (const body of [inner, hint, ruleBody(stageSheet, ".nex-thread")]) {
      expect(body).not.toMatch(/--dock-width/);
      expect(body).not.toMatch(/margin-inline\s*:\s*calc\(\s*-1\s*\*/);
    }
  });

  it("classifies each content surface as a scroll box or a bounded card, not a band", () => {
    const thread = ruleBody(stageSheet, ".nex-thread");
    // Scrolls: the viewport paints no tone, so nothing ends at the inset and
    // nothing needs repainting at the window edge either — only its
    // scrollbar (paint) is moved there, by the round-4 rule.
    expect(declaration(thread, "background")).toBeUndefined();
    expect(declaration(thread, "border-top")).toBeUndefined();
    expect(declaration(thread, "border-bottom")).toBeUndefined();

    const context = ruleBody(contextSheet, ".nex-context");
    expect(declaration(context, "background")).toBeUndefined();
    expect(declaration(context, "border-top")).toBeUndefined();
    expect(declaration(context, "border-bottom")).toBeUndefined();

    // A card: it does paint, but its edge is bounded and centred, so the
    // inset is where its content belongs — it neither stretches nor bleeds.
    const strip = ruleBody(composerSheet, ".nex-composer-strip-error");
    expect(declaration(strip, "max-width")).toBe("760px");
    expect(declaration(strip, "margin")).toBe("0 auto");
  });
});

// ============================================================
// Round 3 — ONE reading measure.
//
// Rounds 1 and 2 put the thread and the composer in one BOX; this round puts
// them on one COLUMN. The box was not enough. The composer centres a
// 760px-bounded card inside it (`.nex-composer-inner`), while the thread let
// its empty state stretch the full width and left-aligned the copy — so the
// empty conversation read as two unrelated columns: at 1536 the sentence
// started at x = 108 and the composer card at x = 388. The transcript ROWS
// were already on the composer's axis (`.nex-message` carries the same
// centred measure); the empty state was the fourth left edge.
//
// THE CHOICE, stated because it could have gone the other way: the composer
// already owned the measure, so the thread was given it — a centred,
// `--layout-measure-max`-bounded column — rather than the composer being
// widened to the stage. A 1536px-wide input line is a worse reading surface
// than a 760px one, and the owner's constraint was one AXIS, not one width.
//
// What is pinned below is the RELATIONSHIP — same measure, same containing
// box, same centre, clearance at the window minimum — never a pixel snapshot
// of any of them.
// ============================================================

describe("one reading measure — the thread and the composer share one column", () => {
  /** Every thread surface that is a row of the reading column. The run
   *  timeline is one: it is a card in the column, and its own indent stays
   *  INSIDE the measure, which is why its padding is untouched here. */
  const COLUMN_ROWS: Array<[string, string]> = [
    ["conversation.css .nex-message", ruleBody(stageSheet, ".nex-message")],
    ["conversation.css .nex-thread-status", ruleBody(stageSheet, ".nex-thread-status")],
    ["conversation.css .nex-thread-waiting", ruleBody(stageSheet, ".nex-thread-waiting")],
    ["conversation.css .nex-agent-run", ruleBody(stageSheet, ".nex-agent-run")],
    [
      "conversation.css .nex-conversation-placeholder",
      lastRuleBody(stageSheet, ".nex-conversation-placeholder"),
    ],
  ];

  it("bounds the empty state and every thread row by the composer's measure", () => {
    expect(MEASURE, "the measure resolves to a length").toMatch(/^\d+(?:\.\d+)?px$/);
    expect(Number.parseFloat(MEASURE)).toBeGreaterThan(0);

    for (const [label, body] of COLUMN_ROWS) {
      expect(declaration(body, "max-width"), label).toBe("var(--layout-measure-max)");
      expect(declaration(body, "margin-inline"), label).toBe("auto");
    }

    // The composer's own column is the same length, whichever sheet spells
    // it. composer.css writes it as a literal on purpose (that sheet depends
    // on no M3E var), so the pin is the RESOLVED equality, not the spelling.
    const composerMeasure = resolveInline(
      declaration(ruleBody(composerSheet, ".nex-composer-inner"), "max-width") ?? "",
    );
    expect(composerMeasure).toBe(MEASURE);
  });

  it("gives both children the same containing block, so the centring agrees", () => {
    // The thread's base rule carries no inline padding of its own: its
    // content box IS the stage's content box. Round 4 adds inline padding
    // from the stage-side opt-out, but exactly the reserve its negative
    // margin took away, so the content box — the box the column is centred
    // in — is unchanged. Padding that did NOT cancel would shrink it:
    // harmless while the measure fits, a 16px drift the moment it does not.
    const thread = ruleBody(stageSheet, ".nex-thread");
    const padding = (declaration(thread, "padding") ?? "").split(/\s+/);
    expect(resolveInline(padding[1] ?? padding[0] ?? "")).toBe("0");
    expect(declaration(thread, "padding-left")).toBeUndefined();
    expect(declaration(thread, "padding-right")).toBeUndefined();

    // …and the pair that does touch it nets to zero: the margin takes the
    // reserve away, the padding gives back exactly the same width.
    const viewport = ruleBody(stageSheet, ".nex-main-conversation > .nex-thread");
    expect(resolveInline(declaration(viewport, "padding-inline") ?? "")).toBe(RAIL_WIDTH);
    expect(declaration(viewport, "margin-inline")).toBe("calc(-1 * var(--dock-width))");

    // The composer band's opt-out spends the width its negative margin buys
    // on transparent border, so the box its children are centred in is that
    // same stage content box (round 2, unchanged).
    const band = ruleBody(stageSheet, ".nex-main-conversation > .nex-composer");
    expect(declaration(band, "border-inline")).toBe("var(--dock-width) solid transparent");
  });

  it("keeps the shared column's centre exactly on the window centre", () => {
    // Symmetry, not arithmetic luck: the reserve is equal on both sides, so
    // the column's centre is 50vw at every width — including below the
    // measure, where both children clamp to the stage's content box.
    expect(Number.isFinite(MIN_WINDOW_WIDTH), "minWidth is readable").toBe(true);
    for (const width of [MIN_WINDOW_WIDTH, 1200, 1536, 2560]) {
      const column = sharedColumn(width);
      expect(column.centre, `${width}px column centre`).toBeCloseTo(width / 2, 6);
      // Neither side of the column ever crosses the rail.
      expect(column.left, `${width}px left`).toBeGreaterThanOrEqual(Number.parseFloat(RAIL_WIDTH));
      expect(
        column.left + column.width,
        `${width}px right`,
      ).toBeLessThanOrEqual(width - Number.parseFloat(RAIL_WIDTH));
    }
  });

  it("keeps the empty-state copy on the column's edge, not padded off it", () => {
    const placeholder = lastRuleBody(stageSheet, ".nex-conversation-placeholder");
    // The block padding is the 22vh anchor and stays; the inline padding is
    // what used to place the sentence 32px inside the stage's reserved edge.
    expect(declaration(placeholder, "padding-inline")).toBe("0");
    expect(declaration(placeholder, "padding-top")).toBe("22vh");
    // Nothing in the empty state's own text re-centres or re-pads itself: the
    // fix is the column's, not the copy's.
    for (const selector of [".nex-conversation-empty-title", ".nex-conversation-empty-text"]) {
      const body = ruleBody(stageSheet, selector);
      expect(declaration(body, "margin-inline"), selector).toBeUndefined();
      expect(declaration(body, "margin"), selector).toBe("0");
      expect(declaration(body, "padding"), selector).toBeUndefined();
      expect(declaration(body, "padding-inline"), selector).toBeUndefined();
    }
  });
});

// ============================================================
// Round 4 — the SCROLLBAR is paint.
//
// Rounds 1–3 reserved the rail for content and put the thread and the
// composer on one column. The thread's scroll viewport, though, still began
// at x = 60: `overflow-y: auto` paints its scrollbar at the padding box's
// inline end, which was width − 60, 60px inside the window edge and inside
// the gutter the rail is supposed to leave empty.
//
// The fix is the band's two-step opt-out with its second half swapped, and
// that swap is measured, not stylistic: a scrollbar is painted in the
// PADDING box, so `border-inline: var(--dock-width) solid transparent` — the
// band's compensating border — leaves the scrollbar exactly where it was
// (1120…1140 at 1200 wide). `padding-inline: var(--dock-width)` puts it at
// 1180…1200, and the content box is the same 60…1120 either way.
//
// What is pinned below is the RELATIONSHIP between the two declarations and
// the boxes they produce — never a pixel snapshot: margin = −reserve gives a
// window-wide border box, padding = +reserve gives back the stage content
// box, so the shared measure cannot move while the scrollbar reaches the
// edge.
// ============================================================

describe("the thread's scroll viewport spans the window; its content box does not move (round 4)", () => {
  const stage = ruleBody(stageSheet, ".nex-main-conversation");
  const viewport = ruleBody(stageSheet, ".nex-main-conversation > .nex-thread");

  it("spans the window while its content box stays on the shared measure", () => {
    const inset = Number.parseFloat(RAIL_WIDTH);
    expect(Number.isFinite(inset), "the reserve is a length").toBe(true);

    // The two declarations, read off the sheet as the relationship they are:
    // the margin is exactly the stage's reserve, and the padding is exactly
    // the width that margin borrowed.
    expect(declaration(viewport, "margin-inline")).toBe(
      `calc(-1 * ${declaration(stage, "padding-inline")})`,
    );
    expect(declaration(viewport, "padding-inline")).toBe(declaration(stage, "padding-inline"));
    // No inline border: the scrollbar paints at the padding box's inline end,
    // so nothing may sit between that edge and the border box.
    expect(declaration(viewport, "border-inline")).toBeUndefined();
    expect(declaration(viewport, "border")).toBeUndefined();

    for (const width of [MIN_WINDOW_WIDTH, 1200, 1536]) {
      // The box the stage hands both children: its content box.
      const stageContent = { left: inset, width: width - 2 * inset };
      // The viewport's border box: the stage content box plus the margin on
      // each side — the window, at every width.
      const border = {
        left: stageContent.left - inset,
        width: stageContent.width + 2 * inset,
      };
      expect(border.left, `${width}px viewport left`).toBe(0);
      expect(border.left + border.width, `${width}px scrollbar edge`).toBe(width);

      // Its content box: the border box less the padding — the stage content
      // box again, unchanged by the opt-out.
      const content = { left: border.left + inset, width: border.width - 2 * inset };
      expect(content, `${width}px content box`).toEqual(stageContent);

      // …so the 760 column is centred in the same box as the composer card
      // and lands on the same left edge and centre it always did.
      const column = sharedColumn(width);
      expect(content.left + (content.width - column.width) / 2, `${width}px column left`).toBeCloseTo(
        column.left,
        6,
      );
      expect(column.centre, `${width}px column centre`).toBeCloseTo(width / 2, 6);
    }
  });
});
