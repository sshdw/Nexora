//! Stage5 permanent strip retains the 40px chrome freeze; compact split
//! identities replace pane headers. Legacy rows retain their own tokens.

import { describe, expect, it } from "vitest";

import { readRepoFile } from "./test/readRepoFile";

/** Comments are prose, not declarations — never let one satisfy a pin. */
function code(path: string): string {
  return readRepoFile(path).replace(/\/\*[\s\S]*?\*\//g, "");
}

/** The body of a top-level rule, matched by its EXACT selector. */
function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = new RegExp(`(?:^|\\n)${escaped}\\s*\\{([\\s\\S]*?)\\n\\}`).exec(
    css
  );
  if (!match) throw new Error(`no top-level rule for ${selector}`);
  return match[1];
}

/** The last declaration of `property` in a rule body. */
function declaration(body: string, property: string): string | undefined {
  const matches = [
    ...body.matchAll(
      new RegExp(`(?:^|[;{\\s])${property}\\s*:\\s*([^;]+);`, "gi")
    ),
  ];
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

/** The remaining row THEME §6 freezes. */
const CHROME_ROWS = [["tab strip", ".nex-tabstrip"]] as const;

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
      const height = declaration(
        ruleBody(code("src/styles/tabs.css"), selector),
        "min-height"
      );
      expect(height).toBe("var(--chrome-height)");
      expect(resolve(height)).toBe("40px");
    });
  }

  it("constrains the permanent strip to 40px while removing the pane header", () => {
    const css = code("src/styles/tabs.css");
    const strip = ruleBody(css, ".nex-tabstrip");
    expect(resolve(declaration(strip, "height"))).toBe("40px");
    expect(css).not.toMatch(/\.nex-pane-header\s*\{/);
    const identity = ruleBody(css, ".nex-pane-identity");
    expect(declaration(identity, "height")).toBeUndefined();
    expect(declaration(identity, "min-height")).toBeUndefined();
  });

  it("keeps the shared legacy token at 52px", () => {
    // The token cannot be deleted or retargeted: five sheets below
    // still resolve it, and it dies with tokens.css in Phase 6.
    expect(declaration(code("src/index.css"), "--layout-header-height")).toBe(
      "52px"
    );
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
