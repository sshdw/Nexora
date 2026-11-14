//! Agent-run IPC area (AC-205): the streaming agent runs (Task 5.1), the
//! read-only run inspector (WS-D.1), the spend dashboard, the activity feed,
//! persistent permission rules (M1-core), and run mode/pause control
//! (Task 5.2).

import { invoke } from "./core";
import { getSetting, setSetting } from "./settings";

export type AccessProfile = "read_only" | "standard" | "supervised";
export interface ApprovalMetadata {
  access_profile: AccessProfile;
  sandbox_posture: "PerCommandConsent";
  checkpoints_available: boolean;
  allow_for_run: boolean;
  sensitive_write: boolean;
  shell: {
    category:
      | "GeneralShell"
      | "DependencyInstall"
      | "PublishOrRemote"
      | "DestructiveGit"
      | "MassDelete";
    packages: string[];
    packages_exact: boolean;
    uncertain: boolean;
    sandbox_posture: "PerCommandConsent";
  } | null;
}
export async function getAccessProfile(): Promise<AccessProfile> {
  const raw = await getSetting("agent.access_profile");
  return raw === null || raw === "standard"
    ? "standard"
    : raw === "supervised"
      ? "supervised"
      : "read_only";
}
export function setAccessProfile(profile: AccessProfile): Promise<void> {
  return setSetting("agent.access_profile", profile);
}

// ---- Agent runs (Task 5.1) ------------------------------------------

/** One `agent_runs` row as persisted by the backend (DATABASE.md §7.8). */
export interface AgentRun {
  id: number;
  conversation_id: number | null;
  model: string;
  mode: string;
  status: string;
  /** Explicit startup interruption provenance; raw status remains unchanged. */
  interrupted: boolean;
  started_at: number;
  finished_at: number | null;
  total_steps: number;
  final_content: string | null;
  error: string | null;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
}

/** One `agent_steps` row as served for rehydration (DATABASE.md §7.9).
 * NEX-PERF-003: `observation` is a *bounded preview* of the stored text
 * (never the full output) so rehydration stays bounded; `observation_len`
 * is the byte-exact total of the full sanitized text and
 * `observation_truncated` reports whether the preview was cut. The full
 * text of any step is one `getAgentStepObservation(stepId)` call away. */
export interface AgentStep {
  id: number;
  run_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  observation_len: number | null;
  observation_truncated: boolean;
  status: string | null;
  started_at: number;
  duration_ms: number | null;
  rule_id: number | null;
  group_key: string | null;
  decided_by: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
}

/** Step payload of a `RunFrame::Step` frame (Task 5.1; NEX-PERF-003 — the
 * frame carries the same bounded preview shape as rehydration). */
export interface StepEventFrame {
  step_id: number;
  seq: number;
  kind: string;
  tool_name: string | null;
  arguments: string | null;
  observation: string | null;
  observation_len: number | null;
  observation_truncated: boolean;
  status: string | null;
  duration_ms: number | null;
}

/** Terminal payload of a `RunFrame::Finished` frame (Task 5.1). */
export interface RunFinishedPayload {
  conversation_id: number;
  status: string;
  final_content: string | null;
  error: string | null;
}

/** Governance event payload streamed inside a `RunFrame::Governance`. */
export type GovernanceEventPayload =
  | { type: "paused" }
  | { type: "resumed" }
  | { type: "budget_exhausted"; max_steps: number }
  | { type: "spend_limit_exceeded"; spent_micro: number; limit_micro: number }
  | {
      type: "approval_requested";
      metadata?: ApprovalMetadata;
      call_id: string;
      name: string;
      arguments: string;
      group_key: string | null;
      group_size: number;
      timeout_secs: number;
    }
  | { type: "approval_resolved"; call_id: string; approved: boolean }
  | { type: "cancelled" }
  | { type: "completed"; steps: number }
  | { type: "compaction_started"; reason: string; messages: number }
  | {
      type: "compaction_finished";
      reason: string;
      summarized: number;
      retained: number;
    }
  | { type: "compaction_failed"; reason: string };

/** One `agent-run-event` frame (Task 5.1 design §2.4). */
export type AgentRunEventPayload =
  | {
      type: "durable";
      run_id: number;
      conversation_id: number;
      event_id: number;
    }
  | {
      type: "step";
      run_id: number;
      conversation_id: number;
      event: StepEventFrame;
    }
  | {
      type: "governance";
      run_id: number;
      conversation_id: number;
      event: GovernanceEventPayload;
    }
  | { type: "finished"; run_id: number; event: RunFinishedPayload };

// NOTE: command ARGS are camelCase (Tauri v2); payloads/events stay snake_case
// (serde `rename_all = "snake_case"`) — never align one to the other.

/** Start one opt-in agent run for `conversationId` (Task 5.1).
 * Returns `{ run_id }` immediately; the run streams via `agent-run-event`. */
export function startAgentRun(
  conversationId: number,
  content: string,
  provider: string,
  model: string
): Promise<{ run_id: number }> {
  return invoke<{ run_id: number }>("start_agent_run", {
    conversationId,
    content,
    provider,
    model,
  });
}

/** Cancel an active run (Task 5.1). */
export function cancelAgentRun(runId: number): Promise<void> {
  return invoke<void>("cancel_agent_run", { runId });
}

/** Echo the opaque call_id from the current approval event/live snapshot.
 * Provider tool IDs are not approval response handles. */
export function resolveAgentApproval(
  runId: number,
  callId: string,
  approved: boolean,
  scope?: "single" | "group"
): Promise<void> {
  return invoke<void>("resolve_agent_approval", {
    runId,
    callId,
    approved,
    scope: scope ?? null,
  });
}

/** Grant additional iterations to a budget-parked run (Task 5.1). */
export function extendAgentRun(
  runId: number,
  extraSteps: number
): Promise<void> {
  return invoke<void>("extend_agent_run", {
    runId,
    extraSteps,
  });
}

/** List the runs of one conversation (rehydration, Task 5.1).
 * AC-102/ADD-08 pagination: `page.limit` bounds the read and `page.beforeRunId`
 * is the exclusive keyset cursor (the previous page's last run id) — omitted
 * or null means "unpaged", matching the command's `Option<i64>` params.
 * Args stay camelCase per the casing contract above; payloads stay snake_case. */
export function listAgentRuns(
  conversationId: number,
  page?: { limit?: number; beforeRunId?: number }
): Promise<AgentRun[]> {
  return invoke<AgentRun[]>("list_agent_runs", {
    conversationId,
    limit: page?.limit ?? null,
    beforeRunId: page?.beforeRunId ?? null,
  });
}

/** List the steps of one run (rehydration, Task 5.1). Each item carries a
 * bounded observation preview plus its byte-exact total (NEX-PERF-003);
 * expand via `getAgentStepObservation`. AC-102/ADD-08 pagination: `page.limit`
 * bounds the read and `page.beforeSeq` is an exclusive upper-bound cursor on
 * the run's `seq` ordering — omitted or null means "unpaged". */
export function listAgentSteps(
  runId: number,
  page?: { limit?: number; beforeSeq?: number }
): Promise<AgentStep[]> {
  return invoke<AgentStep[]>("list_agent_steps", {
    runId,
    limit: page?.limit ?? null,
    beforeSeq: page?.beforeSeq ?? null,
  });
}

/** Fetch the full sanitized observation text of one step by its
 * `agent_steps.id` (NEX-PERF-003 on-demand expand). Unknown ids reject with
 * a secret-free not-found error. */
export function getAgentStepObservation(stepId: number): Promise<string> {
  return invoke<string>("get_agent_step_observation", { stepId });
}

// ---- Run inspector (WS-D.1, read-only) ----------------------------------
// One aggregate view over a run's WS-B/WS-C accessories. Payloads stay
// snake_case (serde `rename_all = "snake_case"`); the command arg is
// camelCase (`runId`), like every other agent command. Secret-free by
// construction: fixed-vocabulary strings, counters, ids, and Unix-seconds
// timestamps only — plus caller-chosen checkpoint labels, never message
// content, tool arguments, credentials, or SQL. Opt-in gaps read `null`
// (empty lists for snapshots/checkpoints/self-audit-free runs).

/** One captured position marker (WS-C.1): stage index, fixed-vocabulary role
 * and state, counters, and the Unix-seconds capture time. No content. */
export interface SnapshotView {
  stage_index: number;
  role: string;
  state: string;
  steps_taken: number;
  spent_micro_usd: number;
  audit_len: number;
  captured_at: number;
}

/** One named checkpoint (WS-C.1): caller-chosen label plus snapshot index. */
export interface CheckpointView {
  name: string;
  snapshot_index: number;
}

/** One self-audit finding (WS-C.3): fixed-vocabulary code plus subject marker. */
export interface ViolationView {
  code: string;
  subject: number | null;
}

/** Latest self-audit verdict (WS-C.3): pass flag plus violation codes only. */
export interface SelfAuditView {
  passed: boolean;
  violations: ViolationView[];
}

/** Read-only aggregate view of one run (WS-D.1 `inspect_run`). */
export interface RunInspection {
  run_id: number;
  state: string;
  stage: string | null;
  role: string | null;
  stages_entered: number;
  stages_total: number;
  steps_taken: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
  max_steps: number | null;
  gate_decisions: string[];
  audit_len: number | null;
  snapshots: SnapshotView[];
  checkpoints: CheckpointView[];
  self_audit: SelfAuditView | null;
  started_at: number;
  finished_at: number | null;
}

/** Inspect one run via `inspect_run`: state, stage + role, budget counters,
 * gate decisions, snapshot/checkpoint markers, and the latest self-audit
 * verdict. Unknown runs fail with a secret-free not-found error. */
export function inspectRun(runId: number): Promise<RunInspection> {
  return invoke<RunInspection>("inspect_run", { runId });
}

/**
 * One parked approval (G-15 live snapshot). `arguments` is the exact payload
 * already delivered on `approval_requested`, so re-rendering the approval card
 * after a tab switch or WebView reload discloses nothing new.
 */
export interface PendingApprovalView {
  metadata?: ApprovalMetadata;
  call_id: string;
  name: string;
  arguments: string;
  timeout_secs: number;
  requested_at: number;
  expires_at: number;
}

/**
 * Live state of a conversation's run (G-15). `parked_reason` is fixed
 * vocabulary; `pending_approval` is non-null only when the backend gate really
 * holds an unresolved park — it is never inferred from `status === "running"`.
 */
export interface LiveRunState {
  run_id: number;
  status: string;
  parked_reason: "none" | "paused" | "awaiting_approval" | "awaiting_budget";
  pending_approval: PendingApprovalView | null;
  started_at: number | null;
  finished_at: number | null;
  current_step_seq: number | null;
  current_step_tool: string | null;
}

/**
 * Read the live run snapshot for one conversation via `inspect_run_live`.
 * `null` means "no run to report" (no active run and no persisted row), so the
 * caller renders no approval affordance instead of inventing one.
 */
export function inspectRunLive(
  conversationId: number
): Promise<LiveRunState | null> {
  return invoke<LiveRunState | null>("inspect_run_live", { conversationId });
}

// ---- Spend dashboard (read-only) --------------------------------------
// Per-run spend (steps, micro-USD, budget caps, % used) plus aggregate
// totals across every persisted run, from the existing `agent_runs`
// counters only. Payloads stay snake_case (serde
// `rename_all = "snake_case"`); the command arg is camelCase (`runId`),
// like every other agent command. Secret-free by construction: integer ids,
// counters, and float ratios only — never model names, content, credentials,
// or SQL. Missing spend data reads `null` (never an error); unknown runs
// fail with a secret-free not-found error.

/** Per-run spend view: persisted counters plus the derivable budget ratios. */
export interface SpendRunView {
  run_id: number;
  steps_taken: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
  /** Step cap: in-memory only, never persisted — always `null` here. */
  max_steps: number | null;
  steps_pct: number | null;
  spend_pct: number | null;
}

/** Aggregate spend totals across every persisted run (saturating sums). */
export interface SpendTotals {
  runs: number;
  total_steps: number;
  total_spent_micro_usd: number;
  runs_with_spend: number;
  runs_with_limit: number;
}

/** One dashboard response: the requested run plus the cross-run totals. */
export interface SpendDashboard {
  run: SpendRunView;
  totals: SpendTotals;
}

/** Load the spend dashboard for one run via `spend_dashboard`. */
export function spendDashboard(runId: number): Promise<SpendDashboard> {
  return invoke<SpendDashboard>("spend_dashboard", { runId });
}

// ---- Activity feed (read-only) ----------------------------------------
// One batched aggregate over the persisted run history: capped recent-run
// metadata rows (newest first) plus cross-run spend totals. Rows carry ids,
// fixed-vocabulary labels, counters, and Unix-seconds timestamps only —
// never `final_content` / `error` text (backend omits those columns by
// construction), so feed rows render metadata + labels, never content
// snippets. Payloads stay snake_case; the command arg is camelCase
// (`limit`), like every other agent command.

/** One activity-feed row: secret-free metadata of one `agent_runs` row. */
export interface ActivityRun {
  run_id: number;
  conversation_id: number | null;
  model: string;
  mode: string;
  status: string;
  started_at: number; // seconds since unix epoch
  finished_at: number | null; // seconds since unix epoch
  total_steps: number;
  spent_micro_usd: number | null;
  limit_micro_usd: number | null;
}

/** One activity-feed response: capped rows plus cross-run totals. */
export interface ActivityFeed {
  runs: ActivityRun[];
  totals: SpendTotals;
}

/** Load the activity feed via `activity_feed`. `limit` selects how many
 * recent runs to include (backend clamps to 1..100, default 50); the totals
 * always cover every persisted run. */
export function activityFeed(limit?: number): Promise<ActivityFeed> {
  return invoke<ActivityFeed>("activity_feed", { limit: limit ?? null });
}

// ---- Agent permission rules (M1-core) ----------------------------------

/** One `permission_rules` row (M1-core). */
export interface PermissionRule {
  id: number;
  preset: string;
  tool_pattern: string;
  path_pattern: string | null;
  effect: "allow" | "ask" | "deny";
  priority: number;
}

/** Add one persistent permission rule (M1-core). */
export function addPermissionRule(
  preset: string,
  toolPattern: string,
  pathPattern: string | null,
  effect: "allow" | "ask" | "deny"
): Promise<number> {
  return invoke<number>("add_permission_rule", {
    preset,
    toolPattern,
    pathPattern,
    effect,
  });
}

/** Remove one persistent permission rule by id (M1-core). */
export function removePermissionRule(id: number): Promise<void> {
  return invoke<void>("remove_permission_rule", { id });
}

/** List persistent permission rules (M1-core). */
export function listPermissionRules(): Promise<PermissionRule[]> {
  return invoke<PermissionRule[]>("list_permission_rules");
}

// ---- File checkpoints & turn revert (Stage 2 PR 2B) --------------------

/** Disk-backed pre/post-mutation file checkpoint metadata (v17, PR 2B). */
export interface FileCheckpoint {
  id: number;
  conversation_id: number;
  run_id: number;
  step_id: number | null;
  turn_index: number;
  workspace_root: string;
  relative_path: string;
  tool_name: "write_file" | "edit_file" | string;
  pre_existed: boolean;
  pre_blob_sha256: string | null;
  pre_size_bytes: number | null;
  post_blob_sha256: string | null;
  post_size_bytes: number | null;
  reverted_at: number | null;
  created_at: number;
}

/** List recorded pre/post-mutation file checkpoints for `runId`. */
export function listRunCheckpoints(runId: number): Promise<FileCheckpoint[]> {
  return invoke<FileCheckpoint[]>("list_run_checkpoints", { runId });
}

/**
 * Revert one file checkpoint back to its pre-mutation bytes (or delete newly
 * created files). Pass `force = true` only after user confirmation when the
 * target file was modified externally (`kind === "conflict"`).
 */
export function revertFileCheckpoint(
  checkpointId: number,
  force = false,
): Promise<FileCheckpoint> {
  return invoke<FileCheckpoint>("revert_file_checkpoint", {
    checkpointId,
    force,
  });
}

/**
 * Revert all un-reverted checkpoints recorded during `(runId, turnIndex)`.
 * Fails atomically with `"conflict"` before modifying disk if any target file
 * changed externally unless `force` is explicitly true.
 */
export function revertTurnCheckpoints(
  runId: number,
  turnIndex: number,
  force = false,
): Promise<FileCheckpoint[]> {
  return invoke<FileCheckpoint[]>("revert_turn_checkpoints", {
    runId,
    turnIndex,
    force,
  });
}

// ---- Agent mode & pause (Task 5.2) ------------------------------------

/** Autonomy modes for the agent (Task 5.2). */
export type AutonomyMode = "supervised" | "semi_autonomous" | "full_autonomous";

/** Live-switch the autonomy mode of an active run (Task 5.2). */
export function agentSetMode(runId: number, mode: AutonomyMode): Promise<void> {
  return invoke<void>("agent_set_mode", {
    runId,
    mode,
  });
}

/** Pause an active run at the next step boundary (Task 5.2). */
export function pauseAgentRun(runId: number): Promise<void> {
  return invoke<void>("pause_agent_run", { runId });
}

/** Resume a paused run (Task 5.2). */
export function resumeAgentRun(runId: number): Promise<void> {
  return invoke<void>("resume_agent_run", { runId });
}

/** Committed identity notification; cursor reads reconcile delivery gaps. */
export interface DurableRunEvent {
  event_id: number;
  conversation_id: number;
  run_id: number;
  kind: "run_started" | "step_recorded" | "run_finished" | "run_interrupted";
  step_id: number | null;
  schema_version: number;
  created_at: number;
  step: AgentStep | null;
  terminal: RunFinishedPayload | null;
}
export interface DurableRunEventPage {
  events: DurableRunEvent[];
  next_cursor: number;
  high_water_event_id: number;
  has_more: boolean;
}
/** Process every item before advancing to next_cursor. Repeated event/step
 * identities are deduplicated; high_water_event_id alone is not a page cursor. */
export function listConversationRunEvents(
  conversationId: number,
  afterEventId = 0,
  limit = 100
): Promise<DurableRunEventPage> {
  return invoke("list_conversation_run_events", {
    conversationId,
    afterEventId,
    limit,
  });
}
