//! Whole-tree class coverage: a class a component puts on the DOM must have
//! a rule in a stylesheet — or be a documented structural hook.
//!
//! Why this file exists: Round 5 deleted `.nex-agent-empty` from
//! `conversation.css` together with the run card it was assumed to belong to.
//! It did not belong to the run card — 22 call sites across eight panels use
//! it for their "nothing here" line — so every one of those empty states
//! silently lost its 13px tertiary tone while every existing test stayed
//! green. `paneChrome.test.ts` runs the same scan for three components and
//! `threadSheet.test.ts` lists the classes the thread no longer emits; this
//! is the missing direction over the WHOLE components tree: used → defined.
//!
//! The scan is deliberately literal. It reads `className` attributes only
//! (never `id`, `htmlFor` or prose), walks a braced expression with a small
//! balanced-brace scanner so `cond ? "a" : "b"` and `` `a ${x}` `` are both
//! seen, and checks each `nex-*` / `is-*` token whole against every sheet the
//! app loads. Tailwind utilities are not in that namespace and are ignored.
//!
//! Hooks: a handful of classes are markers with no paint of their own (the
//! Dock's outside-press hook, role modifiers whose base class carries the
//! rule, the diff badge whose sign carries the meaning). They are listed
//! below WITH the reason, and the list is bidirectional: adding a rule for
//! one of them fails the "drop it from the allowlist" pin.

import { readdirSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { readRepoFile } from "../test/readRepoFile";

/** Every stylesheet the components layer can see. */
function allCss(): string {
  const dir = resolve(process.cwd(), "src/styles");
  return [
    readRepoFile("src/index.css"),
    readRepoFile("src/styles.css"),
    ...readdirSync(dir)
      .filter((name) => name.endsWith(".css"))
      .map((name) => readRepoFile(`src/styles/${name}`)),
  ].join("\n");
}

/** Every class SELECTOR a sheet declares, comments stripped, names whole. */
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

/** The source text inside every `className=` attribute. A braced value is
 *  walked with a balanced-brace scanner that understands quotes and template
 *  literals, so `{cn("a", ok && "b")}` is captured whole rather than cut at
 *  the first `}`. */
function classNameBodies(source: string): string[] {
  const bodies: string[] = [];
  const attribute = /className\s*=\s*/g;
  let match: RegExpExecArray | null;
  while ((match = attribute.exec(source)) !== null) {
    const index = match.index + match[0].length;
    const first = source[index];
    if (first === '"' || first === "'") {
      let end = index + 1;
      while (end < source.length && source[end] !== first) {
        end += source[end] === "\\" ? 2 : 1;
      }
      bodies.push(source.slice(index + 1, end));
      attribute.lastIndex = end + 1;
      continue;
    }
    if (first !== "{") continue;
    let depth = 0;
    let end = index;
    let state: '"' | "'" | "`" | null = null;
    for (; end < source.length; end += 1) {
      const char = source[end];
      if (state === null) {
        if (char === '"' || char === "'" || char === "`") state = char;
        else if (char === "{") depth += 1;
        else if (char === "}") {
          depth -= 1;
          if (depth === 0) break;
        }
      } else if (char === "\\") {
        end += 1;
      } else if (char === state) {
        state = null;
      }
    }
    bodies.push(source.slice(index + 1, end));
    attribute.lastIndex = end + 1;
  }
  return bodies;
}

/** The `nex-*` / `is-*` class tokens a component emits. A quoted className is
 *  the list itself; a braced one contributes its string and template arms. */
function emittedClasses(source: string): string[] {
  const tokens = new Set<string>();
  for (const body of classNameBodies(source)) {
    const lists = [
      body,
      ...[...body.matchAll(/(["'`])((?:\\.|(?!\1)[\s\S])*?)\1/g)].map(
        (literal) => literal[2] ?? "",
      ),
    ];
    for (const list of lists) {
      for (const token of list.split(/\s+/)) {
        if (/^(?:nex|is)-[a-z0-9]+(?:-[a-z0-9]+)*$/.test(token)) tokens.add(token);
      }
    }
  }
  return [...tokens].sort();
}

/** Comments are prose, not markup: a class NAMED in a doc comment must not
 *  count as emitted. Block comments cover JSX comments too. */
function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^[ \t]*\/\/.*$/gm, "");
}

function componentFiles(dir: string): string[] {
  return readdirSync(resolve(process.cwd(), dir), { withFileTypes: true }).flatMap(
    (entry) => {
      const path = `${dir}/${entry.name}`;
      if (entry.isDirectory()) return componentFiles(path);
      return entry.name.endsWith(".tsx") && !entry.name.endsWith(".test.tsx")
        ? [path]
        : [];
    },
  );
}

/** Classes that are markers, not paint. Each one must say why it has no rule
 *  of its own; the allowlist is checked in both directions below. */
const UNSTYLED_HOOKS = new Map<string, string>([
  [
    "nex-dock",
    "structural hook: Tailwind positions the rail, and DockPanel uses the class to exempt the Dock from its outside-press close",
  ],
  [
    "nex-message-user",
    "role modifier: the base `.nex-message` carries the row; `.nex-message-assistant` carries the assistant-only rules",
  ],
  [
    "nex-activity-section",
    "semantic <section> hook (each one has its own aria-label); `.nex-activity-section-title` carries the type",
  ],
  [
    "nex-agent-step-header-group",
    "modifier on the repeated-calls disclosure; `.nex-agent-step-header` owns the row's paint",
  ],
  [
    "nex-agent-diff-count-added",
    "diff-badge modifier: both rungs share `.nex-agent-diff-count`; the +/− sign inside the badge carries the meaning",
  ],
  [
    "nex-agent-diff-count-removed",
    "diff-badge modifier: both rungs share `.nex-agent-diff-count`; the +/− sign inside the badge carries the meaning",
  ],
]);

const FILES = componentFiles("src/components");
const CSS = allCss();
const DEFINED = new Set(classNames(CSS));
const EMITTED = new Map<string, string[]>();
for (const file of FILES) {
  for (const token of emittedClasses(stripComments(readRepoFile(file)))) {
    EMITTED.set(token, [...(EMITTED.get(token) ?? []), file]);
  }
}

describe("class coverage — used → defined", () => {
  it("extracts class tokens from literals and braced expressions", () => {
    // Anti-vacuity: an extractor that quietly returns nothing would make the
    // whole-tree pin pass on an empty set forever.
    expect(emittedClasses('className="nex-a nex-b"')).toEqual(["nex-a", "nex-b"]);
    expect(
      emittedClasses('className={cn("nex-a", ok && "nex-b")}'),
    ).toEqual(["nex-a", "nex-b"]);
    expect(emittedClasses("className={`nex-a ${fade}`}")).toEqual(["nex-a"]);
    expect(emittedClasses('className={ok ? "nex-a" : "nex-b"}')).toEqual([
      "nex-a",
      "nex-b",
    ]);
    // A braced expression's inner brace must not cut the scan short.
    expect(
      emittedClasses('className={ok ? cn("nex-a", { "nex-b": on }) : "nex-c"}'),
    ).toEqual(["nex-a", "nex-b", "nex-c"]);
    // `id`, `htmlFor` and prose are not class emissions.
    expect(emittedClasses('<input id="nex-task-title" />')).toEqual([]);
  });

  it("scans the whole components tree, not one file", () => {
    expect(FILES.length).toBeGreaterThan(60);
    expect(EMITTED.size).toBeGreaterThan(300);
    for (const sample of [
      "nex-agent-empty",
      "nex-composer-shell",
      "nex-dock-panel",
      "nex-sessions-row",
      "nex-agent-run",
    ]) {
      expect(EMITTED.has(sample), `${sample} was not scanned`).toBe(true);
    }
  });

  it("finds a rule for every emitted class, or a documented hook", () => {
    const missing = [...EMITTED.keys()]
      .filter((token) => !DEFINED.has(token) && !UNSTYLED_HOOKS.has(token))
      .sort();
    expect(missing).toEqual([]);
  });

  it("keeps the hook allowlist honest", () => {
    for (const [hook, reason] of UNSTYLED_HOOKS) {
      expect(reason.length, `${hook} needs a reason`).toBeGreaterThan(20);
      // A hook that gains a rule must leave the allowlist, so the exemption
      // can never quietly cover a class that is styled after all.
      expect(DEFINED.has(hook), `${hook} now has a rule — drop it from the allowlist`).toBe(
        false,
      );
    }
  });

  it("keeps the shared panel empty-state line styled (the regression pin)", () => {
    // The exact class Round 5 deleted. Eight panels render it; it is not part
    // of the run card and must keep its own rule.
    expect(EMITTED.get("nex-agent-empty")?.length ?? 0).toBeGreaterThanOrEqual(8);
    const rule = /\.nex-agent-empty\s*\{([\s\S]*?)\}/.exec(
      CSS.replace(/\/\*[\s\S]*?\*\//g, ""),
    )?.[1];
    expect(rule, ".nex-agent-empty has no rule").toBeDefined();
    expect(rule).toMatch(/font-size:\s*var\(--type-size-body-small\)/);
    expect(rule).toMatch(/color:\s*var\(--color-on-surface-tertiary\)/);
  });
});
