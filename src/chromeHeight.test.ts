//! THEME §6 chrome freeze (issue #262).
//!
//! `NEXORA-THEME.md` §6 (owner-approved ballot, 2026-10-15) says
//! verbatim: "Header 40px freeze stands (52 to 40 = 12px
//! reclaimed)". The two chrome rows #230 Phase 2 owns — the tab
//! strip and the pane header (MAP rows 4 and 5) — rendered at the
//! legacy 52px instead.
//!
//! The obvious fix is the wrong one: `--nex-sys-layout-header-height`
//! is ALSO consumed by five un-migrated legacy sheets
//! (activityHealth, settings, prompts, versionControl and
//! src/styles.css). Retargeting it to 40px would silently re-skin
//! five out-of-scope surfaces, which is exactly the one-phase-per-
//! surface rule #230 forbids. So the two Phase 2 rows moved to a
//! flat 40px token in src/index.css and the legacy token stayed at
//! 52px for everyone else.
//!
//! This gate pins both halves by resolving the declarations as text,
//! the way src/theme.test.ts does: a token NAME proving nothing is
//! how a wrong value ships behind a right-looking alias. It fails on
//! the pre-fix tree (the tab strip and pane header read the legacy
//! token) and on any later attempt to retarget the shared token.
//!
//! Round 2: resolving a 40px min-height was not enough. `min-height`
//! cannot shrink content, and the pane header's own row (32px icon
//! control + 8px block padding + hairline) still rendered 48px on the
//! real build. So the pane header also declares a DEFINITE height and
//! the gate pins that height plus the box budget that lets the row fit
//! its control without clipping.

import { describe, expect, it } from "vitest";

import { readRepoFile } from "./test/readRepoFile";

/** Comments are prose, not declarations — never let one satisfy a pin. */
function code(path: string): string {
  return readRepoFile(path).replace(/\/\*[\s\S]*?\*\//g, "");
}

/** The body of a top-level rule, matched by its EXACT selector. */
function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(css);
  if (!match) throw new Error(`no top-level rule for ${selector}`);
  return match[1];
}

/** The last declaration of `property` in a rule body. */
function declaration(body: string, property: string): string | undefined {
  const matches = [...body.matchAll(new RegExp(`(?:^|[;{\\s])${property}\\s*:\\s*([^;]+);`, "gi"))];
  const last = matches[matches.length - 1];
  return last?.[1].trim();
}

/**
 * The token sheets the two chrome rows can resolve through. Last
 * declaration wins, which matches the cascade for the only token
 * these assertions read: `--chrome-height` is declared once, in the
 * default `:root` block, and the light block inherits it.
 *
 * `--nex-sys-layout-header-height` died with tokens.css in Phase 6;
 * the shared 52px row now resolves through the flat `--layout-header-
 * height` token declared in src/index.css.
 */
const tokens = new Map<string, string>();
for (const path of ["src/index.css"]) {
  for (const match of code(path).matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/gi)) {
    tokens.set(match[1], match[2].trim());
  }
}

/** Follow `var(--token)` to a literal, reporting a dangling chain. */
function resolve(value: string | undefined): string | undefined {
  let current = value;
  const seen = new Set<string>();
  while (current) {
    const ref = /^var\(\s*(--[a-z0-9-]+)\s*\)$/i.exec(current);
    if (!ref) return current;
    if (seen.has(ref[1])) throw new Error(`circular token: ${ref[1]}`);
    seen.add(ref[1]);
    current = tokens.get(ref[1]);
  }
  return undefined;
}

/** The two rows THEME §6 freezes. */
const CHROME_ROWS = [
  ["tab strip", ".nex-tabstrip"],
  ["pane header", ".nex-pane-header"],
] as const;

/**
 * The five un-migrated sheets that still own the legacy 52px row.
 * Each keeps its own selector so the pin names the surface, not just
 * the file — a file-level grep would pass if the wrong rule moved.
 */
const LEGACY_ROWS = [
  ["src/styles/activityHealth.css", ".nex-activity-header"],
  ["src/styles/settings.css", ".nex-settings-header"],
  ["src/styles/prompts.css", ".nex-prompt-library-header"],
  ["src/styles/versionControl.css", ".nex-vcs-header"],
  ["src/styles.css", ".nex-main-header"],
] as const;

describe("THEME §6 chrome freeze — the 40px header", () => {
  it("declares the flat chrome token at 40px in src/index.css", () => {
    expect(declaration(code("src/index.css"), "--chrome-height")).toBe("40px");
  });

  for (const [label, selector] of CHROME_ROWS) {
    it(`${label} (${selector}) resolves to 40px through the token`, () => {
      const height = declaration(ruleBody(code("src/styles/tabs.css"), selector), "min-height");
      expect(height).toBe("var(--chrome-height)");
      expect(resolve(height)).toBe("40px");
    });
  }

  it("constrains the pane header row itself to 40px, not just its min-height", () => {
    const header = ruleBody(code("src/styles/tabs.css"), ".nex-pane-header");
    // `min-height` cannot shrink content: the header's 32px icon
    // control plus 8px block padding rendered 48px on the real build
    // (Visual QA, #262 round 2). A definite height is what constrains
    // the ROW; min-height alone only sets a floor.
    const height = declaration(header, "height");
    expect(height).toBe("var(--chrome-height)");
    expect(resolve(height)).toBe("40px");
    expect(declaration(header, "min-height")).toBe("var(--chrome-height)");

    // The row must also FIT its tallest child inside those 40px:
    // border-box height minus block padding and the bottom hairline
    // must leave room for the 32px md icon button (`size-8`).
    expect(code("src/components/ui/button.tsx")).toMatch(/\bicon:\s*"size-8"/);
    const padding = (declaration(header, "padding") ?? "").split(/\s+/);
    const blockPad = Number.parseFloat(resolve(padding[0]) ?? "");
    const border = Number.parseFloat(
      /([\d.]+)px/.exec(declaration(header, "border-bottom") ?? "")?.[1] ?? "",
    );
    expect(Number.isFinite(blockPad)).toBe(true);
    expect(Number.isFinite(border)).toBe(true);
    expect(40 - 2 * blockPad - border).toBeGreaterThanOrEqual(32);
  });

  it("keeps the shared legacy token at 52px", () => {
    // The token cannot be deleted or retargeted: five sheets below
    // still resolve it, and it dies with tokens.css in Phase 6.
    expect(declaration(code("src/index.css"), "--layout-header-height")).toBe("52px");
    expect(resolve("var(--layout-header-height)")).toBe("52px");
  });

  for (const [path, selector] of LEGACY_ROWS) {
    it(`${path} ${selector} still resolves to the legacy 52px`, () => {
      const sheet = code(path);
      const height = declaration(ruleBody(sheet, selector), "min-height");
      expect(height).toBe("var(--layout-header-height)");
      expect(resolve(height)).toBe("52px");
      // And the Phase 2 token must not leak into an out-of-scope sheet.
      expect(sheet).not.toMatch(/--chrome-height/);
    });
  }
});
