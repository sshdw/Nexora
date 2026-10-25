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
    expect(resolve(declaration(stage, "padding-inline"))).toBe("60px");
    // Symmetry is the invariant: separate sides can drift apart, and a
    // shorthand `padding` after the inline inset would silently win.
    expect(declaration(stage, "padding-left")).toBeUndefined();
    expect(declaration(stage, "padding-right")).toBeUndefined();
    expect(declaration(stage, "padding")).toBeUndefined();
    expect(declaration(stage, "margin-inline")).toBeUndefined();
  });

  it("puts no Dock offset on the thread, the composer or the empty copy", () => {
    // The fix is the container's. If any of these carried the token
    // themselves, the thread and the composer would be two columns that can
    // drift, which is the thing this contract forbids.
    const ownedBlocks: Array<[string, string]> = [
      ["conversation.css .nex-thread", ruleBody(stageSheet, ".nex-thread")],
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
    // Stage inset + thread padding-left + placeholder padding-left, the
    // three declarations that place the empty-state sentence.
    const inset = resolve(declaration(stage, "padding-inline")) ?? "";
    const threadPad = (declaration(ruleBody(stageSheet, ".nex-thread"), "padding") ?? "").split(/\s+/);
    const placeholderPad = (declaration(ruleBody(stageSheet, ".nex-conversation-placeholder"), "padding") ?? "").split(/\s+/);
    // `padding: y x y` and `padding: y` — the SECOND value is the inline
    // side; a one-value shorthand means both.
    const threadLeft = resolveInline(threadPad[1] ?? threadPad[0] ?? "");
    const placeholderLeft = resolveInline(placeholderPad[1] ?? placeholderPad[0] ?? "");
    const dock = resolve("var(--dock-width)") ?? "";

    const contentLeft = Number.parseFloat(inset) + Number.parseFloat(threadLeft) + Number.parseFloat(placeholderLeft);
    expect(contentLeft).toBeGreaterThan(Number.parseFloat(dock));
    // 60 + 16 + 32 = 108 in the shipped tokens: 48px of clearance.
    expect(contentLeft).toBe(108);
  });

  it("keeps the stage's own chrome row full-bleed", () => {
    // `.nex-context-tabs` sits above the rail's vertical band and its
    // surface band must still span the pane, so it cancels the stage inset.
    const tabs = ruleBody(contextSheet, ".nex-context-tabs");
    expect(declaration(tabs, "margin-inline")).toBe("calc(-1 * var(--dock-width))");
    expect(resolveInline(declaration(tabs, "margin-inline") ?? "")).toBe("calc(-1 * 60px)");
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
    expect(resolveInline(declaration(band, "margin-inline") ?? "")).toBe("calc(-1 * 60px)");
  });

  it("spends the borrowed width on transparent border, so content stays reserved", () => {
    expect(declaration(band, "border-inline")).toBe("var(--dock-width) solid transparent");
    const resolved = resolveInline(declaration(band, "border-inline") ?? "");
    expect(resolved).toBe("60px solid transparent");
    // The border width IS the reserve: content box = stage content box, so
    // the composer column cannot drift with the band's paint.
    expect(Number.parseFloat(resolved)).toBe(Number.parseFloat(resolve("var(--dock-width)") ?? ""));
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
   *  it must opt out; `content` = paints no band of its own, so it must not
   *  touch the reserve at all. The completeness test below fails when a new
   *  direct child of `.nex-main-conversation` is added without a decision. */
  const SURFACES: Array<{
    root: string;
    sheet: string;
    selector: string;
    kind: "band" | "content";
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
      selector: ".nex-thread",
      kind: "content",
      note: "scroll viewport: paints nothing, the stage canvas shows through",
    },
    {
      root: "nex-context",
      sheet: contextSheet,
      selector: ".nex-context",
      kind: "content",
      note: "context scroller: quiet, no band",
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
        "calc(-1 * 60px)",
      );
      // The literal the token replaces must not come back as a hand-typed
      // rail width; a 1px hairline elsewhere in the rule is not a rail width.
      expect(body, surface.note).not.toMatch(/\b60px\b/);
    }
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
    // Scrolls: the scroll viewport belongs to the content, so no tone/hairline
    // ends at the inset and none is needed at the window edge either.
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
