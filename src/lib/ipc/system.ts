//! System/health IPC area (AC-205): the diagnostics bundle, update check and
//! pre-update snapshot (commands/system.rs), the release-readiness + issue
//! automation pair (commands/release.rs), and the privacy-center telemetry
//! controls (commands/privacy.rs).

import { invoke } from "./core";
import {
  OP_CREATE_ISSUE,
  OP_PRIVACY_WIPE,
  requestConfirmation,
} from "./confirmations";
import type { GhRateLimit } from "./github";

// ---- Diagnostics / update check / pre-update snapshot ---------------------
// Three read-mostly commands behind the health panel's System section:
// `diagnostics_bundle` collects the secret-free bundle (versions, platform,
// storage counts, swept crash rows — never content, credentials, or paths),
// `update_check` compares the running build against the workspace origin
// repo's latest GitHub release (one GET — check only, no download or
// install; the panel renders the release URL as text), and
// `snapshot_database` writes one user-initiated `VACUUM INTO` copy under the
// app-data `backups` directory before an update (never silent, never on a
// timer). Payloads stay snake_case like every other backend struct.

/** Storage counters in the diagnostics bundle: row counts only. */
export interface BundleCounts {
  conversations: number;
  messages: number;
  prompts: number;
  agent_runs: number;
  agent_steps: number;
  agent_tasks: number;
}

/** One retained crash row: startup-sweep rows only (id, model, timestamp). */
export interface CrashedRun {
  run_id: number;
  model: string;
  started_at: number;
}

/** Secret-free diagnostics bundle: versions, platform, storage, crashes. */
export interface DiagnosticsBundle {
  app_version: string;
  platform_os: string;
  platform_arch: string;
  schema_version: number;
  schema_target: number;
  db_size_bytes: number | null;
  counts: BundleCounts;
  fts_present: boolean;
  crashed_runs: CrashedRun[];
  error_runs: number;
}

/** Collect the secret-free diagnostics bundle via `diagnostics_bundle`. */
export function diagnosticsBundle(): Promise<DiagnosticsBundle> {
  return invoke<DiagnosticsBundle>("diagnostics_bundle");
}

/** One update check: running build vs the origin repo's latest release. */
export interface UpdateCheck {
  owner: string;
  repo: string;
  current_version: string;
  latest_tag: string | null;
  latest_name: string;
  latest_url: string | null;
  published_at: string | null;
  update_available: boolean;
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** Check the origin repo's latest release via `update_check` (GET only). */
export function updateCheck(): Promise<UpdateCheck> {
  return invoke<UpdateCheck>("update_check");
}

/** A pre-update snapshot copy: file name (never the path) plus size. */
export interface SnapshotInfo {
  file_name: string;
  size_bytes: number;
  created_at: number;
}

/** Write one user-initiated pre-update snapshot via `snapshot_database`. */
export function snapshotDatabase(): Promise<SnapshotInfo> {
  return invoke<SnapshotInfo>("snapshot_database");
}

// ---- Release readiness + issue automation ---------------------------------
// Two commands behind the Release panel (operator close-the-loop surface):
// `release_status` aggregates the honest readiness facts — manifest version
// parity (`package.json` / `src-tauri/Cargo.toml` /
// `src-tauri/tauri.conf.json` below the workspace root; missing manifests
// surface as `null`, never an error), migration facts (known vs applied vs
// pending), the newest pre-update snapshot presence, and the local `gh` CLI
// probe. Read-only and network-free; no new tables. The update signal stays
// on `update_check` (it needs the network) — the panel calls both.
// `create_issue_for_finding` files one user-confirmed issue through the
// user's own `gh` CLI (`gh issue create` with `cwd` = workspace root, so the
// repo resolves itself). Auth stays in the CLI: no token ever crosses IPC —
// only the title/location/detail/source strings travel, and only
// title/body strings reach the subprocess as argv. The destructive-action
// confirmation is the same mint-and-consume gate as the other writes (SEC-03):
// the wrapper mints a single-use `release`-scope id through the blocking
// native OS dialog and the backend consumes it before `gh` runs — bare IPC
// callers cannot file. Payloads stay snake_case like every other backend
// struct; command args are camelCase (`debtId` style) like every other command.

/** Newest pre-update snapshot: file name (never the path) plus size. */
export interface ReleaseSnapshot {
  file_name: string;
  size_bytes: number;
}

/** Release readiness facts, all computed backend-side (never hand-waved).
 * The panel maps these to pass/warn/fail badges. */
export interface ReleaseStatus {
  app_version: string;
  package_json_version: string | null;
  cargo_version: string | null;
  tauri_conf_version: string | null;
  versions_agree: boolean;
  migration_count: number;
  schema_version: number;
  schema_target: number;
  pending_migrations: number;
  snapshot: ReleaseSnapshot | null;
  gh_available: boolean;
  gh_version: string | null;
}

/** Collect the release readiness facts via `release_status`. */
export function releaseStatus(): Promise<ReleaseStatus> {
  return invoke<ReleaseStatus>("release_status");
}

/** Fixed-vocabulary issue source for the link-back line. */
export type IssueSource = "debt" | "audit" | "release-check";

/** One filed issue: the canonical URL `gh` printed plus its issue number. */
export interface CreatedIssue {
  url: string;
  number: number;
}

/** File one user-confirmed issue via `create_issue_for_finding`.
 *
 * SEC-03: the confirmation is the blocking native OS dialog this wrapper
 * raises (minting a single-use `release`-scope id bound to
 * `create_issue_for_finding`) — there is no caller-supplied boolean. */
export async function createIssueForFinding(
  title: string,
  source: IssueSource,
  location?: string | null,
  detail?: string | null
): Promise<CreatedIssue> {
  const confirmationId = await requestConfirmation("release", OP_CREATE_ISSUE);
  return invoke<CreatedIssue>("create_issue_for_finding", {
    title,
    location: location ?? null,
    detail: detail ?? null,
    source,
    confirmationId,
  });
}

// ---- Privacy center (telemetry controls + local usage ledger) -------------
// Three commands behind the health panel's Privacy section:
// `privacy_status` (every egress surface with its live state + the ledger
// aggregates), `privacy_export` (the same aggregates as copy/download JSON),
// and `privacy_wipe` (user-confirmed delete behind the mint-and-consume
// confirmation gate, SEC-03). Payloads
// stay snake_case like every other backend struct. There is no telemetry
// upload anywhere: the ledger never leaves the machine except through the
// user's own copy/download of this export.

/** Live state of one egress surface. */
export type PrivacySurfaceState = "on" | "off" | "manual" | "local";

/** One egress surface: where data can leave, its live state, honest detail. */
export interface PrivacySurface {
  id: string;
  title: string;
  destination: string;
  state: PrivacySurfaceState;
  detail: string;
}

/** One aggregated ledger row: event kind + day bucket + count. Counts only. */
export interface LedgerStat {
  kind: string;
  day: number;
  count: number;
}

/** Privacy status: the surface inventory plus ledger aggregates. */
export interface PrivacyStatus {
  surfaces: PrivacySurface[];
  stats: LedgerStat[];
  total_events: number;
  retention_days: number;
  oldest_day: number | null;
}

/** Ledger export: aggregates plus metadata, shaped for copy/download. */
export interface LedgerExport {
  exported_at: number;
  retention_days: number;
  stats: LedgerStat[];
}

/** Result of a confirmed ledger wipe: how many rows were deleted. */
export interface WipeResult {
  deleted_rows: number;
}

/** Collect the privacy status via `privacy_status` (read-only, local). */
export function privacyStatus(): Promise<PrivacyStatus> {
  return invoke<PrivacyStatus>("privacy_status");
}

/** Export the ledger aggregates via `privacy_export` (counts only). */
export function privacyExport(): Promise<LedgerExport> {
  return invoke<LedgerExport>("privacy_export");
}

/** Wipe every ledger row via `privacy_wipe`.
 *
 * SEC-03: the confirmation is the blocking native OS dialog this wrapper
 * raises (minting a single-use `privacy`-scope id bound to `privacy_wipe`) —
 * there is no caller-supplied boolean. Cancelling at the dialog rejects and
 * deletes nothing. */
export async function privacyWipe(): Promise<WipeResult> {
  const confirmationId = await requestConfirmation("privacy", OP_PRIVACY_WIPE);
  return invoke<WipeResult>("privacy_wipe", { confirmationId });
}
