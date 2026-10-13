import { readFileSync } from "node:fs";
import { resolve } from "node:path";

/**
 * Read a repo file as text, anchored to the project root.
 *
 * Test helpers only: these files exist so the theme and
 * reduced-motion gates can assert on what the stylesheet and the
 * vendored components actually DECLARE, rather than on what the
 * bundler made of them. (`?raw` is not usable here — vitest runs
 * the CSS through its own transform first.)
 *
 * vitest runs with cwd at the project root, which is also where
 * `npm test` is invoked from, so `src/...` resolves the same way
 * in a local run and in CI.
 */
export function readRepoFile(relativePath: string): string {
  return readFileSync(resolve(process.cwd(), relativePath), "utf8");
}