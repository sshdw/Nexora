//! Repository-audit IPC area (AC-205): the read-only static-analysis scan
//! (commands/repo_audit.rs) and the template-generated test scaffolds derived
//! from it (commands/testgen.rs).

import { invoke } from "./core";

// ---- Repository audit (read-only static analysis, findings only) ---------
// One batch command over the workspace sources: `repo_audit` scans `.rs` /
// `.ts` / `.tsx` files under the effective workspace root (capped server-side
// with skip notices) and returns fixed-vocabulary findings with `file:line`
// evidence plus capped excerpts. Findings never modify code — there is no
// auto-fix path; results are copy/read-only. Payloads stay snake_case like
// every other backend struct; the command takes no arguments.

/** One audit finding: fixed-vocabulary kind + severity with `file:line`
 * evidence (workspace-relative `path`, 1-based `line`) and a capped code
 * excerpt (at most 3 lines). */
export interface AuditFinding {
  kind: string;
  severity: string;
  path: string;
  line: number;
  excerpt: string;
}

/** One file the scan did not read: path plus a fixed-vocabulary reason
 * (`"too-large"`, `"unreadable"`, `"file-cap"`). */
export interface SkippedFile {
  path: string;
  reason: string;
}

/** One read-only audit run: capped findings plus scan accounting. Totals
 * always cover the whole walk; lists stay capped with overflow counts. */
export interface RepoAuditReport {
  findings: AuditFinding[];
  findings_overflow: number;
  files_scanned: number;
  files_skipped: number;
  skipped: SkippedFile[];
  skipped_overflow: number;
}

/** Run the read-only repository audit via `repo_audit`. Manual runs only —
 * the panel never watches or re-audits live. */
export function repoAudit(): Promise<RepoAuditReport> {
  return invoke<RepoAuditReport>("repo_audit");
}

// ---- Test drafts (template-generated Rust #[test] scaffolds, review buffer)
// One batch command over the workspace sources: `testgen_drafts` runs the
// read-only audit, then derives template-based drafts for undocumented or
// unused public Rust functions (capped server-side with an overflow count).
// Drafts are response data only — nothing is written; the user copies them
// manually. Payloads stay snake_case like every other backend struct; the
// command takes no arguments.

/** One draft test scaffold: arrange/act/assert skeleton with honest TODO
 * markers (`fn_name` tested as `test_<fn_name>`; `signature` is the
 * declaration line the scaffold was derived from). */
export interface TestDraft {
  path: string;
  line: number;
  fn_name: string;
  source_kind: string;
  signature: string;
  code: string;
}

/** One read-only testgen run: capped drafts plus generation accounting. */
export interface TestgenReport {
  drafts: TestDraft[];
  drafts_overflow: number;
  targets_considered: number;
  files_scanned: number;
  audit_overflow: number;
}

/** Generate draft test scaffolds via `testgen_drafts`. Manual runs only —
 * the panel never generates live. */
export function testgenDrafts(): Promise<TestgenReport> {
  return invoke<TestgenReport>("testgen_drafts");
}
