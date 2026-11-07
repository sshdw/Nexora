//! Dependency + refactor IPC area (AC-205): the read-only lockfile inventory
//! and the confirmed dead-code removal write — both live in
//! commands/dep_refactor.rs.

import { invoke } from "./core";
import { OP_REFACTOR_APPLY, requestConfirmation } from "./confirmations";

// ---- Dependency inventory (read-only lockfile tables) -------------------
// One batch command over the workspace lockfiles: `dep_inventory` reads
// `src-tauri/Cargo.lock` (cargo table) and `package-lock.json` (npm table)
// and returns locked name/version/source rows, capped server-side with
// overflow counts. Tables report locked (resolved) versions only — declared
// ranges from the manifests never appear. No install/update/remove path
// exists anywhere in this slice: the inventory never writes.

/** One locked dependency: resolved name + version plus a fixed-vocabulary
 * source label (cargo: `"crates.io"` | `"git"` | `"local"` | `"unknown"`;
 * npm: `"registry"` | `"git"` | `"local"` | `"unknown"`). */
export interface DepEntry {
  name: string;
  version: string;
  source: string;
}

/** One read-only inventory run: capped cargo + npm tables with totals.
 * Totals always cover the whole lockfile; lists stay capped with overflow
 * counts. A missing or unparsable lockfile yields an empty table. */
export interface DepInventory {
  cargo: DepEntry[];
  cargo_total: number;
  cargo_overflow: number;
  npm: DepEntry[];
  npm_total: number;
  npm_overflow: number;
}

/** Load the dependency inventory via `dep_inventory`. Manual loads only —
 * the panel never watches the lockfiles. */
export function depInventory(): Promise<DepInventory> {
  return invoke<DepInventory>("dep_inventory");
}

// ---- Safe refactor apply (confirmed dead-code removals only) ------------
// One write command over a single workspace file range: `refactor_apply`
// deletes workspace-relative `.rs` lines `[startLine, endLine]` (1-based,
// inclusive) for a finding of an allowlisted kind (`dead-code-candidate`
// only — every other audit kind refuses). The backend re-verifies every
// guard before writing: explicit confirmation, kind allowlist, git-repo
// presence, tracked-and-clean file, usable range, and a `pub `-declaration
// first line. The file was clean beforehand, so `git checkout -- <path>`
// reverts. NOTE: command ARGS are camelCase (Tauri v2); payloads stay
// snake_case — never align one to the other.

/** One applied refactor: what was removed, the re-read file shape, and the
 * post-apply verification flag. */
export interface RefactorApplyResult {
  path: string;
  start_line: number;
  end_line: number;
  removed_lines: number;
  file_lines: number;
  removed_preview: string;
  verified: boolean;
}

/** Apply one confirmed dead-code removal via `refactor_apply`.
 *
 * SEC-03: the backend consumes a live single-use confirmation id minted for
 * `refactor_apply` in the `refactor` scope — this wrapper mints it through
 * the blocking native OS dialog before the write. The panel's in-app
 * confirmation tick stays as the pre-gate UX, but the approval the backend
 * verifies is the native dialog, never a caller-supplied boolean. */
export async function refactorApply(
  path: string,
  startLine: number,
  endLine: number,
  kind: string,
): Promise<RefactorApplyResult> {
  const confirmationId = await requestConfirmation("refactor", OP_REFACTOR_APPLY);
  return invoke<RefactorApplyResult>("refactor_apply", {
    path,
    startLine,
    endLine,
    kind,
    confirmationId,
  });
}
