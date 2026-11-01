#!/usr/bin/env node
// design-lint.mjs — advisory Nexora design detector (non-blocking).
// Usage: node scripts/design-lint.mjs [--json] [paths...]  (default: src)
// Exit: 0 = clean, 2 = findings (advisory, never fail CI yet), 1 = script error.
// Suppress per line: // design-lint-disable <rule>  (applies to that line + next).

import { readFileSync, statSync, readdirSync, existsSync } from "node:fs";
import { join, extname } from "node:path";

const RULES = [
  {
    id: "gradient-hero",
    re: /(linear-gradient|radial-gradient|conic-gradient)\s*\(/g,
    files: /\.(css|tsx|jsx)$/,
    msg: "gradient wash — use surface-tone steps (see .ai/research/design/05-color.md)",
  },
  {
    id: "3-card-grid",
    re: /repeat\(\s*3\b/g,
    files: /\.(css|tsx|jsx)$/,
    msg: "3-column card grid — reject dashboard grids of identical cards (02-anti-patterns)",
  },
  {
    id: "emoji-icon",
    re: /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}\u{2B00}-\u{2BFF}]/gu,
    files: /\.(tsx|jsx)$/,
    msg: "emoji as icon — use real icons (08-icons)",
  },
  {
    id: "eyebrow",
    re: /eyebrow/gi,
    files: /\.(css|tsx|jsx)$/,
    msg: "eyebrow-label pattern — avoid eyebrow+headline+pill stacks (02-anti-patterns)",
    count: true,
  },
  {
    id: "pill-on-image",
    re: /border-radius:\s*(999(px)?|var\(--nex-sys-shape-full\))/g,
    files: /\.css$/,
    msg: "possible pill-on-image CTA — verify against 02-anti-patterns",
    needs: /(hero|image|background-image|banner)/i,
  },
  {
    id: "raw-hex",
    re: /#[0-9a-fA-F]{3,8}\b/g,
    files: /\.(tsx|jsx|ts)$/,
    msg: "raw hex in component — consume --nex-sys-* tokens (05-color)",
  },
  {
    id: "raw-hex-css",
    re: /#[0-9a-fA-F]{3,8}\b/g,
    files: /\.css$/,
    msg: "raw hex in CSS outside tokens.css — consume --nex-sys-* tokens",
  },
  {
    id: "transition-all",
    re: /transition\s*:\s*all\b|transition-all/g,
    files: /\.(css|tsx|jsx)$/,
    msg: "transition:all — transition explicit properties on --motion-* tokens (07-motion)",
  },
  {
    id: "scale-zero",
    re: /scale\(\s*0[\s,)]/g,
    files: /\.(css|tsx|jsx)$/,
    msg: "scale(0) — collapsing scale reads as a glitch; fade/slide instead",
  },
];

const SKIP_DIRS = new Set(["node_modules", "dist", "target", ".git", "gen"]);
const args = process.argv.slice(2);
const wantJson = args.includes("--json");
const roots = args.filter((a) => !a.startsWith("--"));
if (roots.length === 0) roots.push("src");

function collect(p, out) {
  if (!existsSync(p)) throw new Error("path not found: " + p);
  const st = statSync(p);
  if (st.isFile()) {
    out.push(p);
    return;
  }
  for (const e of readdirSync(p)) {
    if (SKIP_DIRS.has(e)) continue;
    collect(join(p, e), out);
  }
}

function main() {
  const files = [];
  for (const r of roots) collect(r, files);
  const findings = [];
  for (const f of files) {
    const ext = extname(f);
    if (![".css", ".tsx", ".jsx", ".ts"].includes(ext)) continue;
    if (f.endsWith("tokens.css") || f.endsWith("motion.css")) continue; // token sources are exempt
    let text;
    try {
      text = readFileSync(f, "utf8");
    } catch {
      continue;
    }
    const lines = text.split("\n");
    RULES.forEach((rule) => {
      if (!rule.files.test(f)) return;
      const isRawHexCss = rule.id === "raw-hex-css";
      let hits = 0;
      lines.forEach((line, i) => {
        if (line.includes("design-lint-disable")) return; // suppressed line
        if (i > 0 && lines[i - 1].includes("design-lint-disable")) return; // disable-next-line
        rule.re.lastIndex = 0;
        if (!rule.re.test(line)) return;
        if (rule.needs) {
          const win = lines.slice(Math.max(0, i - 5), i + 6).join("\n");
          if (!rule.needs.test(win)) return;
        }
        if (isRawHexCss && /--nex-(ref|sys)-/.test(line)) return; // token defs reference hex legitimately
        hits++;
        if (!rule.count)
          findings.push({ rule: rule.id, file: f, line: i + 1, msg: rule.msg });
      });
      if (rule.count && hits > 2)
        findings.push({
          rule: rule.id,
          file: f,
          line: 0,
          msg: rule.msg + " (" + hits + " hits)",
        });
    });
  }
  if (wantJson) console.log(JSON.stringify({ findings }, null, 2));
  else {
    if (findings.length === 0) console.log("design-lint: clean");
    for (const x of findings)
      console.log(
        (x.line ? x.file + ":" + x.line : x.file) + " [" + x.rule + "] " + x.msg
      );
  }
  process.exit(findings.length > 0 ? 2 : 0);
}

try {
  main();
} catch (err) {
  console.error("design-lint error: " + (err && err.message));
  process.exit(1);
}
