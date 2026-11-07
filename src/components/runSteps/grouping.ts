//! Fold gate + consecutive-repeat grouping for the agent-run steps tree
//! (AC-208 split, per the review §3.1 plan — moved verbatim from
//! `AgentRunSteps.tsx`). No React: pure predicates over the run's steps.

import type { AgentRunView, AgentStepView } from "../../lib/useAgentRun";
import { isProtectedRow, isTerminalStatus, stepTarget } from "./labels";

/** Smallest sequence worth folding. Under this a summary row would cost
 * more than the two or three steps it replaces — folding has to be a density
 * lever, not a tax on every finished run. */
export const FOLD_MIN_STEPS = 6;

/** `tool_name` buckets for the fold summary (§13.33 `12 tool calls · 4 files ·
 * 2 commands`). Files bucket the four filesystem tools, commands bucket
 * `execute_command`, searches bucket `search_files` — the three buckets the
 * brief names, kept separate because "searched" and "touched" are different
 * facts about a run. The leading count is EVERY tool call, so the buckets are
 * a breakdown of that total: a tool outside all three (`apply_patch`,
 * `list_agents`, …) is still counted honestly in the total instead of
 * vanishing. `model_turn` and `approval` rows are not tool calls and are
 * counted in neither. */
export const FILE_TOOLS = new Set([
  "read_file",
  "write_file",
  "edit_file",
  "list_directory",
]);

export interface FoldCounts {
  calls: number;
  files: number;
  commands: number;
  searches: number;
}

export function foldCounts(steps: AgentStepView[]): FoldCounts {
  const counts: FoldCounts = { calls: 0, files: 0, commands: 0, searches: 0 };
  for (const step of steps) {
    if (step.kind !== "tool_call") continue;
    counts.calls += 1;
    if (step.tool_name === "execute_command") counts.commands += 1;
    else if (step.tool_name === "search_files") counts.searches += 1;
    else if (step.tool_name !== null && FILE_TOOLS.has(step.tool_name))
      counts.files += 1;
  }
  return counts;
}

/** §13.33 fold gate. Deliberately never a status string alone: the run must
 * be finished AND nothing in it may still need the user's eye — no pending
 * approval (the gate keeps its own Approve/Deny live), no failed, denied or
 * cancelled row, no failed full-output fetch — and the sequence must be long
 * enough for the summary to pay for itself and contain at least one tool call
 * to count.
 *
 * `budget_exhausted` is terminal in the database sense but NOT finished in
 * the one §13.33 means ("after a run finishes"): the run stopped at the cap
 * and is still carrying an unacknowledged decision — the header offers
 * Continue, and "where exactly did it run out" is the question the user is
 * about to ask before pressing it. Folding it would answer that question with
 * a count, so it stays expanded. */
export function foldableRun(
  run: AgentRunView,
  sorted: AgentStepView[]
): boolean {
  if (!isTerminalStatus(run.status)) return false;
  if (run.status === "budget_exhausted") return false;
  if (run.pending_approval !== null) return false;
  // AC-102/ADD-08: an UNLOADED run decides its default fold from the
  // persisted `total_steps` column, so a long finished run starts folded
  // and the lazy step fetch does not fire for every rehydrated run on
  // mount — the fetch happens when the user expands the run. The per-step
  // gates below (tool calls, protected rows) need the list on screen and
  // cannot pre-approve a fold they cannot evaluate; expanding is the user's
  // explicit ask, the steps load into the open run, and a failed or denied
  // row inside is therefore shown, never hidden behind the fold
  // (§13.29 "failure stays findable").
  if (!run.steps_loaded) {
    return run.total_steps !== null && run.total_steps >= FOLD_MIN_STEPS;
  }
  if (sorted.length < FOLD_MIN_STEPS) return false;
  if (!sorted.some((step) => step.kind === "tool_call")) return false;
  return !sorted.some(isProtectedRow);
}

/* ---- Repeated calls grouped behind one expandable line ------------------
   MAP row 8: "tool calls fold to one line (action → file → change →
   verification)", and P3's acceptance names the grouping explicitly:
   "repeated calls grouped with an expandable list".

   The unit is a CONSECUTIVE run of steps that share a tool AND a target —
   "Read ×6 src/a.ts, src/b.ts…" is noise; the same read of the same file
   five times in a row is one fact the reader wants once. Consecutive only,
   never across the whole run: interleaved work keeps its real order, so
   the timeline never lies about what happened when.

   A group of ONE is not a group — it renders as the ordinary row, so the
   collapse never adds a disclosure around a single item. */
export interface StepGroup {
  key: string; // grouping identity (tool|target) — consecutive comparison only
  id: string; // stable unique DOM/React identity within the run
  steps: AgentStepView[];
}

function groupKey(step: AgentStepView): string | null {
  if (step.kind !== "tool_call" || step.tool_name === null) return null;
  const target = stepTarget(step);
  // No single target means no identity to repeat: two `execute_command`
  // calls with different commands must not fold into one line.
  return target === null ? null : `${step.tool_name}|${target.text}`;
}

export function groupSteps(
  steps: AgentStepView[]
): Array<AgentStepView | StepGroup> {
  const out: Array<AgentStepView | StepGroup> = [];
  let current: StepGroup | null = null;
  for (const step of steps) {
    const key = groupKey(step);
    if (key === null) {
      current = null;
      out.push(step);
      continue;
    }
    if (current !== null && current.key === key) {
      current.steps.push(step);
      continue;
    }
    current = { key, id: `g${step.seq}`, steps: [step] };
    out.push(current);
  }
  return out;
}

export function isGroup(item: AgentStepView | StepGroup): item is StepGroup {
  return (item as StepGroup).steps !== undefined;
}
