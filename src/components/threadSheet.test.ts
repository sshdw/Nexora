//! Thread + Context sheet contract (#230 Phase 3).
//!
//! jsdom computes no cascade, so a component test cannot see what a rule
//! PAINTS — only which elements exist and what they say. Three of this
//! phase's binding rules are therefore invisible to every behavioural test
//! in the repo, and each one has been violated at least once in this very
//! phase's diff. They are pinned here, against the SHEETS:
//!
//!  1. NO HUE. THEME §1 is a monochrome system, and this file found six
//!     carriers of the legacy amber `--nex-sys-color-primary` and its
//!     aliases inside the two sheets P3 owns — the Context legend, the
//!     run-state pills, the run spine's pulse, the current-row connector,
//!     the diff hunk header and the terminal's `$`. Plus green/red on diff
//!     lines, which are state hues spent on something that is not a state.
//!     The rule below is the generalisation of the P2 tab-tick fix, and
//!     of issue #261 behind it.
//!
//!  2. NO RAW HEX, NO RAW DURATION. Tokens only (SYSTEM §2). A raw
//!     millisecond is not a style preference here: it is precisely what
//!     stops the ONE global prefers-reduced-motion gate from being able to
//!     collapse a transition (motion.test.tsx owns that block).
//!
//!  3. REDUCED MOTION. Every animated thing on these two sheets needs a
//!     hand-written `prefers-reduced-motion: reduce` branch collapsing to
//!     the static end state (SYSTEM §7). The global gate covers a colour
//!     or opacity transition because its end state IS the information; it
//!     does NOT cover an animation that carries its meaning in POSITION —
//!     the spine pulse ends at opacity 0, so a collapsed gate would leave
//!     a live run looking dead. That branch has to be hand-written, and it
//!     has to be asserted here, because no component test can reach it.

import { describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

/** The two sheets P3 owns: the thread section of conversation.css and
 *  MAP row 20's context.css. Comments stripped — a token named in a
 *  comment is documentation of what USED to be there, and reading it as
 *  a live rule is how the amber survived a comment that described it.
 *
 *  THE SCOPE IS DELIBERATE. conversation.css is a MERGED sheet, not P3's
 *  to repaint whole: `.nex-composer-input` and `.nex-composer-error` are
 *  blocks that live here precisely because they are SHARED — nine panels
 *  outside this phase use `.nex-composer-error`, and P4 owns the composer.
 *  They stay on the legacy tokens until their own phase moves them, and a
 *  gate that flagged them would push this phase into nine surfaces it was
 *  told not to touch. So the hue scan reads from the MESSAGE section —
 *  everything the thread owns, `.nex-message-*` and `.nex-agent-*` — to the
 *  end of the sheet, plus context.css whole. The marker is the thread's
 *  own scroll container, the first thread rule on the sheet, matched on the
 *  LIVE selector (comments are stripped before the slice, so a heading
 *  comment would not survive to anchor it). */
const conversationAll = readRepoFile("src/styles/conversation.css").replace(
  /\/\*[\s\S]*?\*\//g,
  "",
);
const threadStart = conversationAll.indexOf(".nex-thread {");
const conversation = conversationAll.slice(threadStart);
/** `.nex-composer-error` is the ONE block after the thread marker that this
 *  phase does not own — nine panels outside P3 share it, and it stays on
 *  the legacy tokens until its own phase moves it. Slicing it out by
 *  SELECTOR rather than by position keeps the gate honest about that
 *  exclusion instead of quietly widening or narrowing the scope. */
const conversationThread = conversation.replace(
  /\.nex-composer-error\s*\{[^}]*\}/,
  "",
);
const context = readRepoFile("src/styles/context.css").replace(/\/\*[\s\S]*?\*\//g, "");
const sheets = `${conversationThread}\n${context}`;

/** Every `@media (prefers-reduced-motion: reduce) { … }` block in a
 *  sheet, as one string. A naive indexOf for the opening brace would stop
 *  at the FIRST inner brace, so the brace depth is tracked. */
function reducedMotionBlocks(css: string): string[] {
  const out: string[] = [];
  const open = "@media (prefers-reduced-motion: reduce)";
  let from = 0;
  for (;;) {
    const at = css.indexOf(open, from);
    if (at < 0) return out;
    let depth = 0;
    let i = css.indexOf("{", at);
    const start = i;
    for (; i < css.length; i += 1) {
      if (css[i] === "{") depth += 1;
      else if (css[i] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    out.push(css.slice(start, i + 1));
    from = i;
  }
}

/** The SELECTORS inside reduced-motion blocks — one entry per comma-
 *  separated name, pseudo-parts stripped. */
function reducedMotionSelectors(css: string): string[] {
  return reducedMotionBlocks(css).flatMap((block) =>
    [...block.matchAll(/([^{}]+)\{/g)]
      .map((m) => (m[1] ?? "").trim())
      .flatMap((chunk) => chunk.split(",").map((s) => s.trim()))
      .filter(Boolean),
  );
}

describe("thread + context sheets — no hue (#230 P3, THEME §1)", () => {
  /* The generalised P2 tab-tick defect: a token whose NAME reads like a
   * role — `primary`, `tertiary`, `accent-indicator` — resolving to a hue.
   *
   * `--nex-sys-color-accent-indicator` is an ALIAS of
   * `--nex-sys-color-primary` (tokens.css:213), and `--nex-sys-color-primary`
   * is still the legacy amber: #a9824a dark, #e3c07c light (tokens.css:504).
   * `--nex-sys-color-tertiary` is the SAME hex (tokens.css:517).
   * `--nex-sys-color-success` and `--nex-sys-color-warning` are the legacy
   * sage and ochre. All of them are listed below, so a future pass that
   * reaches for one gets a named failure rather than a silent amber.
   *
   * `--nex-sys-color-error` is deliberately NOT in the list: it is a STATE
   * role and the thread uses it only where the state is real (a failed
   * step's connector, the failure block's rule and text). */
  const CHROMATIC = [
    "--nex-sys-color-primary",
    "--nex-sys-color-primary-container",
    "--nex-sys-color-primary-hover",
    "--nex-sys-color-tertiary",
    "--nex-sys-color-tertiary-container",
    "--nex-sys-color-accent-indicator",
    "--nex-sys-color-accent-indicator-surface",
    "--nex-sys-color-success",
    "--nex-sys-color-success-container",
    "--nex-sys-color-secondary",
    "--nex-sys-color-warning",
    "--nex-sys-color-warning-container",
  ];

  for (const token of CHROMATIC) {
    it(`declares no ${token} — every carrier resolves to a hue`, () => {
      // `var(--x` catches the reference form; the bare declaration form is
      // caught too, because an alias re-declared locally is the same defect
      // one indirection deeper.
      const uses = sheets.match(new RegExp(`var\\(\\s*${token}\\s*[,\\)]`, "g")) ?? [];
      const declares = sheets.match(new RegExp(`${token}\\s*:`, "g")) ?? [];
      expect(
        [...uses, ...declares],
        `${token} still resolves to a hue on a #230 P3 surface. Use --color-tick ` +
          `for an indicator, --state-ok/warn/err for a real state, or an ` +
          `--nex-sys-color-on-surface-* step.`,
      ).toHaveLength(0);
    });
  }

  it("spends green/red/yellow only on the mono theme's sanctioned state roles", () => {
    // `--state-ok` / `--state-warn` / `--state-err` are the THREE roles the
    // mono theme reserves hue for (src/index.css). The old sheet reached for
    // the legacy sage and brick INSTEAD, which is the same meaning painted
    // from a system this phase is deleting.
    for (const banned of [
      "--nex-sys-color-success",
      "--nex-sys-color-error-container",
      "--nex-sys-color-warning",
    ]) {
      expect(sheets, `${banned} is a legacy state hue`).not.toMatch(
        new RegExp(`var\\(\\s*${banned}\\s*[,\\)]`),
      );
    }
    // …and the sanctioned ones ARE used, on the states that are real.
    expect(sheets).toMatch(/--state-ok/);
    expect(sheets).toMatch(/--state-warn/);
    expect(sheets).toMatch(/--state-err/);
  });

  it("keeps the Context legend achromatic and re-spreads it for high contrast", () => {
    // The verified defect this whole phase was pointed at. The fix is
    // documented at length in context.css; what is pinned here is that
    // all four rungs are tone steps off the ink token, and that the
    // high-contrast theme gets its OWN re-spread rather than inheriting
    // the dark ladder's arithmetic.
    for (const seg of ["user", "assistant", "tools", "other"]) {
      const rule = context.match(
        new RegExp(`\\.nex-context-seg-${seg}\\s*\\{[^}]*\\}`),
      )?.[0];
      expect(rule, `.nex-context-seg-${seg} has no rule`).toBeDefined();
      expect(
        rule,
        `.nex-context-seg-${seg} must be an ink tone step, not a hue`,
      ).toMatch(/var\(--nex-sys-color-on-surface\)\s+\d+%/);
      expect(rule, `.nex-context-seg-${seg} must not name a hue`).not.toMatch(
        /--(nex-sys-color-(primary|success|tertiary|secondary|warning))|--(state-|destructive)/,
      );
    }
    // HC re-spreads, keyed off the theme ATTRIBUTE (the mechanism
    // tokens.css uses), never a media query.
    expect(context).toMatch(
      /\[data-theme="high-contrast"\]\s+\.nex-context-seg-user/,
    );
    expect(context).toMatch(/\.nex-theme-high-contrast\s+\.nex-context-seg-other/);
  });

  it("gives the legend's adjacency a structural edge, not a tone alone", () => {
    // Four achromatic steps cannot each clear 3:1 against their NEIGHBOUR
    // on this panel's surface — the measured best spread is 1.74 / 1.81 /
    // 1.89, which is below the non-text floor. So the edge, not the tone,
    // is what separates one bucket from the next, and the edge is asserted
    // here so a future pass cannot quietly delete it and leave the ladder
    // to carry adjacency it cannot carry.
    expect(context).toMatch(
      /\.nex-context-seg\s*\{[^}]*box-shadow:\s*inset\s+1px\s+0\s+0\s+0\s+var\(--nex-sys-color-surface-container-high\)/,
    );
  });
});

describe("thread + context sheets — tokens only (SYSTEM §2)", () => {
  it("declares no raw hex", () => {
    // `#` inside a comment is stripped above; a selector like `#foo` would
    // be a false positive, so require the hex to sit in a declaration.
    const hits = sheets.match(/:\s*[^;{}]*#[0-9a-fA-F]{3,8}\b/g) ?? [];
    expect(hits, `raw hex in a declaration: ${hits.join(" | ")}`).toHaveLength(0);
  });

  it("declares no raw millisecond duration or delay", () => {
    const hits = sheets.match(/(?:animation|transition)[^;{}]*\b\d+(?:\.\d+)?ms\b/g) ?? [];
    expect(hits, `raw duration: ${hits.join(" | ")}`).toHaveLength(0);
  });

  it("declares no raw duration at all outside a comment", () => {
    const hits = sheets.match(/:\s*[^;{}]*\b\d+(?:\.\d+)?m?s\s*(?:;|$)/g) ?? [];
    expect(hits, `raw time value: ${hits.join(" | ")}`).toHaveLength(0);
  });

  it("uses only radii on the frozen 4 / 6 / 8 / 12 scale", () => {
    // THEME §4 freezes the scale and says an increase needs a visual
    // reason proven on a real screen. Every radius here is a token, so this
    // asserts the absence of a literal rather than a list of values.
    const hits = sheets.match(/border-radius:\s*\d/g) ?? [];
    expect(hits, "raw border-radius — use a --nex-sys-shape-* token").toHaveLength(0);
  });

  it("points the budget hairline at the mono fill, not the legacy amber", () => {
    // Same defect class as the legend, one sheet over: `--color-primary` is
    // the light fill (src/index.css), `--nex-sys-color-primary` is the amber.
    expect(context).toMatch(/--color-primary/);
    expect(context).not.toMatch(/var\(\s*--nex-sys-color-primary\s*[,)]/);
  });
});

describe("thread + context sheets — reduced motion (SYSTEM §7)", () => {
  it("keeps the spine pulse's static arm, which the global gate cannot do", () => {
    // The gate collapses durations and holds the END frame. The pulse's end
    // frame is opacity 0, so a live run would read as dead. This branch is
    // the one thing on these sheets that MUST be hand-written, and it must
    // hold the segment at full opacity — the reduced arm has to be
    // INFORMATIVE, not merely still.
    const arm = conversation.match(
      /@media \(prefers-reduced-motion: reduce\)\s*\{[^}]*nex-agent-spine-pulse[^}]*\}/,
    )?.[0];
    expect(arm, "the pulse has no hand-written reduced-motion branch").toBeDefined();
    expect(arm).toMatch(/animation:\s*none/);
    expect(arm).toMatch(/opacity:\s*1/);
  });

  it("collapses every colour-only transition on these sheets to instant", () => {
    // `.nex-agent-step-header`, `.nex-agent-step-target` and
    // `.nex-agent-fold` all carry a colour transition. The gate would
    // already resolve those correctly — but the branch drops the
    // declaration outright, so nothing is even scheduled. Asserted because
    // a `transition` added without a matching branch is invisible in jsdom.
    // The BODY is what carries `transition: none`, not the selector list,
    // so the assertion reads the block, not the names.
    const branches = reducedMotionBlocks(conversation).join("\n");
    for (const sel of [
      ".nex-agent-step-header",
      ".nex-agent-step-target",
      ".nex-agent-fold",
    ]) {
      expect(
        reducedMotionSelectors(conversation),
        `${sel} animates with no reduced-motion branch`,
      ).toContain(sel);
    }
    expect(branches, "the colour transitions are not dropped").toMatch(
      /transition:\s*none/,
    );
  });

  it("collapses the budget hairline's fill to instant", () => {
    expect(reducedMotionSelectors(context)).toContain(".nex-context-meter-fill");
    expect(reducedMotionBlocks(context).join("\n")).toMatch(/transition:\s*none/);
  });

  it("animates nothing on these sheets that has no reduced-motion branch at all", () => {
    // The inverse check, which is the one that catches a NEW animation:
    // every selector that declares a transition or an animation must appear
    // in a reduced-motion block OR be one the global gate provably resolves
    // to its information (the message-insert entrance, whose end frame is
    // the finished state). Anything else is an unhandled loop.
    const animated = [
      ...conversation.matchAll(/([^{}]+)\{([^}]*)\}/g),
    ]
      .filter(([, , body]) => /\b(animation|transition)\s*:/.test(body ?? ""))
      .map(([, selector]) => (selector ?? "").trim())
      .flatMap((chunk) => chunk.split(",").map((s) => s.trim()))
      .filter((s) => s.startsWith(".nex-agent") || s.startsWith(".nex-message") || s.startsWith(".nex-context"));
    const branches = reducedMotionSelectors(conversationThread).join(" ");
    // The two exemptions are NOT "left alone" — they are the two animations
    // whose END FRAME IS THE INFORMATION, which is the gate's own contract
    // (motion.test.tsx pins that the gate holds the end frame). Both are
    // named here so the exemption list is visible in the diff rather than
    // hidden in a regex:
    //
    //   .nex-message-enter  — a one-shot entrance whose `to` is
    //     opacity 1 / translateY(0). Collapsed, it resolves to "the message
    //     has arrived", which is the entire content of the animation. The
    //     global gate's `animation-fill-mode: forwards` is what makes that
    //     true, and motion.test.tsx:105 pins that declaration exists.
    //   .nex-conversation-placeholder — the same `.nex-empty-enter`
    //     one-shot entrance the shared empty-state rule already owns, and
    //     which P0 restyled; it inherits the identical end frame.
    //
    // Everything else must have its own branch, or the list must say why.
    const GATE_RESOLVES = [".nex-message-enter", ".nex-conversation-placeholder"];
    for (const selector of new Set(animated)) {
      const base = selector.replace(/::?[a-z-]+(\([^)]*\))?$/, "");
      expect(
        branches.includes(base) || GATE_RESOLVES.some((g) => base.includes(g)),
        `${selector} animates and has no prefers-reduced-motion branch`,
      ).toBe(true);
    }
  });
});

describe("thread + context sheets — the classes P3 removed", () => {
  it("no longer rules on classes #230 P3 deleted from the markup", () => {
    // `.nex-agent-step-kind` and `.nex-agent-step-tool` were the tool
    // identifier printed twice (once as the kind label, once as itself).
    // Both are gone from the component. A rule left behind is not inert:
    // it reads as the truth about a class nothing emits, and it is what
    // the next reader trusts.
    for (const dead of [
      ".nex-agent-step-kind",
      ".nex-agent-step-tool",
      ".nex-agent-step-status",
      ".nex-conversation-empty-mark",
      ".nex-conversation-empty-mark-wrap",
    ]) {
      expect(conversation, `${dead} has no class emitting it any more`).not.toMatch(
        new RegExp(`\\${dead}\\b`),
      );
    }
  });

  it("rules on every class the step grammar emits", () => {
    // The other half of paneChrome's gate: a className with no rule is not
    // a styling bug the eye catches — the element renders as whatever the
    // cascade happens to leave behind. Selector names matched WHOLE,
    // against comments stripped.
    for (const emitted of [
      ".nex-agent-step-action",
      ".nex-agent-step-target",
      ".nex-agent-step-change",
      ".nex-agent-step-verify",
      ".nex-agent-step-count",
      ".nex-agent-step-group",
      ".nex-agent-run-status",
      ".nex-agent-run-summary",
      ".nex-agent-failure",
      ".nex-agent-failure-key",
      ".nex-agent-failure-value",
      ".nex-agent-diff-header-line",
      ".nex-agent-diff-count",
    ]) {
      expect(sheets, `${emitted} is emitted with no rule anywhere`).toMatch(
        new RegExp(`\\${emitted}\\s*[,\\{]`),
      );
    }
  });

  it("names the completion summary in exactly one place, once", () => {
    // The CSS half of the "no duplicated COMPLETED blocks" rule: one class,
    // one rule. A second rule block for the same class is how two
    // summaries get out of sync.
    expect(conversation.match(/\.nex-agent-run-summary\s*\{/g) ?? []).toHaveLength(1);
  });
});