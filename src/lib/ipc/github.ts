//! GitHub IPC area (AC-205): read-only issues/PR lists, the Actions
//! fail-loop surface, and the runners detect surface over the workspace
//! `origin` repo (commands/github.rs).

import { invoke } from "./core";

// ---- GitHub issues & PRs (read-only panel, github.com only) --------------
// Two batch commands over the workspace `origin` repo: `gh_issues` lists
// issues (pull-request rows excluded backend-side) and `gh_pulls` lists
// pull requests, each one capped page (at most 50 items) for the `state`
// filter (`open` / `closed` / `all`, default `open`). Read-only end to end:
// GET only, no commenting/labeling/merging anywhere on the path. The token
// resolves backend-side from the OS keyring entry `github` and never
// crosses IPC — a missing token is not an error (`authenticated: false`,
// panel shows its connect-hint; the shared unauthenticated quota applies).
// Payloads stay snake_case like every other backend struct; the command arg
// is camelCase (`state`), like every other command.

/** Rate-limit snapshot echoed from the response headers (`null` when the
 * header was absent — render as "unknown", never as zero). */
export interface GhRateLimit {
  limit: number | null;
  remaining: number | null;
  reset: number | null;
}

/** One issue row: fixed-vocabulary metadata plus the capped body. */
export interface GhIssue {
  number: number;
  title: string;
  state: string;
  author: string;
  labels: string[];
  comments: number;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  body: string;
  body_truncated: boolean;
}

/** One pull-request row: fixed-vocabulary metadata plus the capped body. */
export interface GhPull {
  number: number;
  title: string;
  state: string;
  author: string;
  draft: boolean;
  head_ref: string | null;
  base_ref: string | null;
  comments: number;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  body: string;
  body_truncated: boolean;
}

/** State filter vocabulary for both list commands. */
export type GhState = "open" | "closed" | "all";

/** One `gh_issues` response: resolved repo, filter echo, capped items,
 * token presence, and the rate-limit snapshot. A quota-exhausted API
 * answers with empty `items` and `rate_limited: true` (never a silent
 * empty, never an error dump). */
export interface GhIssuesResult {
  owner: string;
  repo: string;
  state: string;
  items: GhIssue[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** One `gh_pulls` response: same envelope as `GhIssuesResult`. */
export interface GhPullsResult {
  owner: string;
  repo: string;
  state: string;
  items: GhPull[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's issues via `gh_issues`. `state`
 * selects `open` / `closed` / `all` (default `open`). */
export function ghIssues(state?: GhState): Promise<GhIssuesResult> {
  return invoke<GhIssuesResult>("gh_issues", { state: state ?? null });
}

/** List the workspace origin repo's pull requests via `gh_pulls`. */
export function ghPulls(state?: GhState): Promise<GhPullsResult> {
  return invoke<GhPullsResult>("gh_pulls", { state: state ?? null });
}

// ---- GitHub Actions fail-loop (read-only CI surface, same panel) ---------
// Two commands over the workspace `origin` repo. `gh_actions` lists the
// recent workflow runs (at most 20, newest first) with failing runs expanded
// (failed jobs metadata only — failed step names, no log bodies, so one
// batch call never fans out into dozens of log downloads). `gh_action_log`
// fetches one job's capped, secret-scrubbed log tail on job-expand (the last
// 200 lines under 24000 chars; oversized downloads keep their tail). An
// expired or missing log resolves to `log_unavailable`; an auth refusal
// (401/403) to `log_needs_auth`. Read-only end to end: GET only, no
// re-running anywhere on the path (the user re-runs on GitHub; the panel
// points at the run URL). The fix loop creates a task-manager task prefilled
// with the failure context through the existing `createTask` wrapper —
// creating the task starts nothing; the user reviews and runs it from Tasks
// with approvals and budgets intact.
// Payloads stay snake_case like every other backend struct.

/** One failed CI job inside a failing workflow run: fixed-vocabulary
 * metadata and failed step names. Log bodies are lazy: `gh_actions` ships
 * jobs with an empty `log_excerpt` and all log flags false, and the panel
 * fetches each log on job-expand via `gh_action_log`. */
export interface GhActionJob {
  id: number;
  run_id: number;
  name: string;
  status: string;
  conclusion: string;
  started_at: string | null;
  completed_at: string | null;
  html_url: string | null;
  failed_steps: string[];
  log_excerpt: string;
  log_truncated: boolean;
  log_unavailable: boolean;
  log_needs_auth: boolean;
  log_redacted: boolean;
}

/** One workflow run: fixed-vocabulary metadata plus the failed jobs (only
 * failing runs expand; other runs carry an empty `failed_jobs` list).
 * `jobs_truncated` marks omitted failed jobs beyond the kept few. */
export interface GhActionRun {
  id: number;
  name: string;
  head_branch: string | null;
  head_sha: string | null;
  event: string;
  status: string;
  conclusion: string;
  created_at: string | null;
  updated_at: string | null;
  html_url: string | null;
  failed_jobs: GhActionJob[];
  jobs_truncated: boolean;
}

/** One `gh_actions` response: resolved repo, recent runs, the API's total
 * run count, token presence, and the rate-limit snapshot. A quota-exhausted
 * API answers with empty `runs` and `rate_limited: true` (never a silent
 * empty, never an error dump). */
export interface GhActionsResult {
  owner: string;
  repo: string;
  runs: GhActionRun[];
  total_runs: number;
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's recent workflow runs via `gh_actions`. */
export function ghActions(): Promise<GhActionsResult> {
  return invoke<GhActionsResult>("gh_actions");
}

/** One lazily fetched job log via `gh_action_log`: the capped,
 * secret-scrubbed tail of one failed job. `log_unavailable` means the log
 * was expired, never uploaded, or its fetch failed (see the run on GitHub
 * instead); `log_needs_auth` means the endpoint refused for auth reasons
 * (sign in first); `log_redacted` flags a scrubbed excerpt. */
export interface GhActionLog {
  job_id: number;
  run_id: number;
  log_excerpt: string;
  log_truncated: boolean;
  log_unavailable: boolean;
  log_needs_auth: boolean;
  log_redacted: boolean;
}

/** Fetch one failed job's log tail via `gh_action_log`. */
export function ghActionLog(runId: number, jobId: number): Promise<GhActionLog> {
  return invoke<GhActionLog>("gh_action_log", { runId, jobId });
}

// ---- GitHub runners detect (read-only detect surface, same panel) --------
// One batch command over the workspace `origin` repo: `gh_runners` lists the
// repo's self-hosted runners (at most 30, with label names) plus the local
// readiness checklist (`git` + `gh` + `docker` presence via `PATH` lookup —
// never executed). Listing self-hosted runners needs an admin-scoped token,
// so an anonymous or under-scoped read resolves to empty `runners` with
// `runners_needs_auth: true` (the panel shows its needs-token state, never
// an error dump). Detection only: no registration, removal, or dispatch
// exists anywhere on this path. Payloads stay snake_case like every other
// backend struct; the command takes no arguments.

/** One self-hosted runner: fixed-vocabulary metadata plus label names. */
export interface GhRunner {
  id: number;
  name: string;
  os: string;
  status: string;
  busy: boolean;
  labels: string[];
}

/** One local readiness entry: whether the tool resolves on `PATH`. */
export interface GhToolReadiness {
  name: string;
  found: boolean;
}

/** One `gh_runners` response: resolved repo, self-hosted runners, the API's
 * total runner count, the needs-token flag, the local readiness checklist,
 * token presence, and the rate-limit snapshot. */
export interface GhRunnersResult {
  owner: string;
  repo: string;
  runners: GhRunner[];
  runners_total: number;
  runners_needs_auth: boolean;
  local: GhToolReadiness[];
  authenticated: boolean;
  rate_limited: boolean;
  rate_limit: GhRateLimit;
}

/** List the workspace origin repo's self-hosted runners plus the local
 * readiness probe via `gh_runners` (detection only). */
export function ghRunners(): Promise<GhRunnersResult> {
  return invoke<GhRunnersResult>("gh_runners");
}
