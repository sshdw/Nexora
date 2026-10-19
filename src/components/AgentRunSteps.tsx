//! Steps accordion for one agent run (Task 5.1 + 5.2).
//!
//! Presentational: kind-grouped collapsible sections (`model_turn` |
//! `tool_call` | `approval`), run status pill, live append, bare inline
//! Approve/Deny on `ApprovalRequested`, Continue on `BudgetExhausted`, pause/
//! resume controls, and per-tool body renderers — terminal for
//! `execute_command` (command line + stdout/stderr with distinct separator) and
//! diff viewer for `write_file` (headers, hunks, +/- gutters, mono,
//! collapsible). Chronological placement is owned by the thread view.
//!
//! #153 — Agent Activity Spine + run-state visuals (brief §7, §13.9, §13.33,
//! §15, §16, §18, §19):
//!   - ONE always-mounted hairline rail per run, with a per-row connector
//!     tick. The rail is the structural boundary; the tick is a segment, not
//!     a per-row border — nothing here rebuilds a spine out of row borders.
//!   - The travelling accent pulse is a CSS animation keyed on the run-status
//!     class on the section. Never a timer: every step frame replaces the
//!     runs array and re-renders this whole subtree, so a JS pulse keyed on
//!     step arrival would restart or retime on every frame.
//!   - The pulse exists ONLY while the run is actually executing. Idle,
//!     `paused`, every terminal, and a run parked on an approval carry the
//!     same information with no motion at all, and the reduced-motion frame
//!     is a static accent segment, not an empty rail (brief §15:885-887).
//!   - Temporal fade (§13.9): inactive rows step their ink down the
//!     on-surface ladder and stop at `on-surface-tertiary`. No opacity
//!     multiplier — see the CSS note; the floor tier has no headroom for one.
//!   - Tool group folding (§13.33): a finished, approval-free, error-free,
//!     long sequence collapses behind one disclosure. "Error-free" is
//!     the persisted per-step vocabulary — `failed`, `denied` or `cancelled`
//!     (`isFailedStatus`) — and "approval-free" means no approval still
//!     OUTSTANDING: a granted park is settled history. Expanding
//!     restores the SAME step objects in the SAME order with each row's own
//!     open/closed state intact (rows are hidden, never unmounted).
//!
//! #230 Phase 3 (MAP rows 8 + 9, SYSTEM §7 + §8):
//!   - ONE SUMMARY LINE. The header carries the status word and the run's
//!     real results, counted from its own steps, exactly once. The old
//!     counted fold summary was a SECOND block claiming the same outcome,
//!     which P3's acceptance forbids outright.
//!   - THE STEP GRAMMAR: action → file → change → verification, one line.
//!     Past-tense verbs instead of tool identifiers, the acted-on path with
//!     its full value on hover, `+N −M` counted from the diff text itself,
//!     and the persisted status as a word rather than a CHECK-constraint
//!     string. Never colour-only.
//!   - REPEATED CALLS GROUP. A consecutive run of the same tool against the
//!     same target is one fact ("Read ×6 src/db.rs"), behind a disclosure
//!     that restores the same rows in the same order. Interleaved work keeps
//!     its real order — the timeline never lies about sequence.
//!   - THE ERROR BLOCK. What happened / what survived / next action, in
//!     words, outside the timeline so the fold and the temporal fade can
//!     never take it out of reach. The old bare `nowrap` error string in the
//!     toolbar clipped long provider errors and answered nothing else.
//!   - NO MODEL MONOLOGUE. `model_turn` rows read "Thought" and stay folded
//!     until asked; the raw narration opens on request rather than streaming
//!     into the thread.
//!
//! Callbacks arrive pre-bound to their run id and stable, so the memoized
//! root re-renders only when this run's own data changes (#148).

import { memo, useCallback, useEffect, useMemo, useState } from "react";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";
import { getLocale, tp, tr, type Locale, type StringKey } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3Toolbar from "./M3Toolbar";

export interface AgentRunStepsProps {
  run: AgentRunView;
  /** Every callback carries its own `runId`: the parent owns the binding so
   * the props stay referentially stable across renders. An inline arrow per
   * row (one per run, rebuilt on every message frame) would defeat the memo
   * below and re-render every run in the thread each time any run advances. */
  onResolveApproval: (runId: number, callId: string, approved: boolean) => void;
  onCancel: (runId: number) => void;
  onContinue: (runId: number) => void;
  onPause?: (runId: number) => void;
  onResume?: (runId: number) => void;
  onExpandStep?: (runId: number, seq: number) => void;
  onCollapseStep?: (runId: number, seq: number) => void;
}

function statusClass(status: string): string {
  switch (status) {
    case "running":
      return "nex-tag nex-agent-status-running";
    case "paused":
      return "nex-tag nex-agent-status-paused";
    case "completed":
      return "nex-tag nex-agent-status-completed";
    case "cancelled":
      return "nex-tag nex-agent-status-cancelled";
    case "budget_exhausted":
      return "nex-tag nex-agent-status-budget";
    case "spend_limit_exceeded":
      return "nex-tag nex-agent-status-spend";
    case "error":
      return "nex-tag nex-agent-status-error";
    default:
      return "nex-tag";
  }
}

function statusLabel(status: string, locale: Locale = getLocale()): string {
  switch (status) {
    case "running":
      return tr(locale, "agent.statusRunning");
    case "paused":
      return tr(locale, "agent.statusPaused");
    case "completed":
      return tr(locale, "agent.statusCompleted");
    case "cancelled":
      return tr(locale, "agent.statusCancelled");
    case "budget_exhausted":
      return tr(locale, "agent.statusBudget");
    case "spend_limit_exceeded":
      return tr(locale, "agent.statusSpend");
    case "error":
      return tr(locale, "agent.statusError");
    default:
      return status;
  }
}

/** Run status → the CSS class every run-state visual reads. Rendering
 * vocabulary only, never a new state: each class below is keyed on a status
 * string the backend already emits (`RunState::persisted_status` — the five
 * terminals plus `running` — and the pre-spawn `queued`, which persists no
 * row and so lands in the quiet bucket). Only `…-state-running` may animate
 * (brief §7: a pulse means "executing right now", nothing else). */
function runStateClass(status: string): string {
  switch (status) {
    case "running":
      return "nex-agent-run-state-running";
    case "paused":
      return "nex-agent-run-state-paused";
    case "completed":
      return "nex-agent-run-state-completed";
    case "cancelled":
      return "nex-agent-run-state-cancelled";
    case "budget_exhausted":
      return "nex-agent-run-state-budget";
    case "spend_limit_exceeded":
      return "nex-agent-run-state-spend";
    case "error":
      return "nex-agent-run-state-error";
    default:
      return "nex-agent-run-state-quiet";
  }
}

/** Mirrors `RunState::is_terminal` over the persisted vocabulary the UI
 * receives. Never the only fold condition — see `foldableRun`. */
function isTerminalStatus(status: string): boolean {
  return (
    status === "completed" ||
    status === "cancelled" ||
    status === "budget_exhausted" ||
    status === "spend_limit_exceeded" ||
    status === "error"
  );
}

/** The step statuses that mean "this row did not do what it was asked to",
 * straight off the `agent_steps.status` CHECK (`succeeded` | `failed` |
 * `denied` | `cancelled`) — the persisted vocabulary, not a UI invention.
 *
 * `denied` is not cosmetic: four dispatch paths record a `tool_call` row
 * with `status: "denied"` and an `Error:` observation and NO companion
 * approval row — the per-turn over-cap batch (dispatch.rs:171), the
 * document shell ban (:200), the WS-B.3 role gate (:226), and the injection
 * hold when there is no gate to park on (:279) — so that row is the only
 * trace the model was refused at all. The sticky-group and rule denials are
 * NOT part of this count: those write an `approval` ROW with status `denied`
 * (persistence.rs `approval_denied_by_group` / `approval_with_provenance`),
 * and the execute paths persist only what `classify_outcome` returns —
 * `succeeded` | `cancelled` | `failed`, never `denied`.
 *
 * `cancelled` is protected for the same reason: `execute_tool_call` writes it
 * for a tool row when the cancellation token trips, including the error arm
 * where the observation IS the tool error. A half-executed call is not a
 * settled result, so it never settles away either. */
const FAILED_STEP_STATUSES = new Set(["failed", "denied", "cancelled"]);

/** The one predicate for "this step failed", shared by the fold gate, the
 * fade ladder and the connector tone below — three call sites that must
 * never drift apart on what counts as a failure. */
function isFailedStatus(step: AgentStepView): boolean {
  return step.status !== null && FAILED_STEP_STATUSES.has(step.status);
}

/** A row the user must always be able to find: an approval that has not been
 * granted, a failed/denied/cancelled step, or a failed full-output fetch.
 * These never fade (§13.9: "do not fade important final results, errors,
 * approvals") and they block folding (§13.29: failure stays findable).
 *
 * The approval branch qualifies on `status` rather than on the kind alone: a
 * GRANTED park persists `kind: "approval", status: "succeeded"` and a rule
 * denial `denied`, so protecting every approval row would make any run that
 * ever gated one tool permanently unfoldable — and §13.33 is precisely about
 * the long, gated-heavy runs. A denial still protects (its status is `denied`,
 * caught by `isFailedStatus` above), an outstanding one has no `succeeded`
 * status and protects here, and the live park is covered twice over by the
 * `run.pending_approval` gate in `foldableRun`. */
function isProtectedRow(step: AgentStepView): boolean {
  return (
    (step.kind === "approval" && step.status !== "succeeded") ||
    isFailedStatus(step) ||
    step.observation_error !== null
  );
}

/** Temporal-fade bucket (§13.9). `age` counts rows back from the run's
 * current row, so the newest row is always age 0 — a live run keeps its
 * active row at full ink, and a finished run keeps its final result at full
 * ink (it is "the important final result", not history). One row behind it
 * is left alone too: two dimmed rows would read as a rendering fault rather
 * than as age. */
function fadeClass(age: number): string {
  if (age <= 1) return "";
  return age === 2 ? "nex-agent-step-age-1" : "nex-agent-step-age-2";
}

/** Smallest sequence worth folding. Under this a summary row would cost
 * more than the two or three steps it replaces — folding has to be a density
 * lever, not a tax on every finished run. */
const FOLD_MIN_STEPS = 6;

/** `tool_name` buckets for the fold summary (§13.33 `12 tool calls · 4 files ·
 * 2 commands`). Files bucket the four filesystem tools, commands bucket
 * `execute_command`, searches bucket `search_files` — the three buckets the
 * brief names, kept separate because "searched" and "touched" are different
 * facts about a run. The leading count is EVERY tool call, so the buckets are
 * a breakdown of that total: a tool outside all three (`apply_patch`,
 * `list_agents`, …) is still counted honestly in the total instead of
 * vanishing. `model_turn` and `approval` rows are not tool calls and are
 * counted in neither. */
const FILE_TOOLS = new Set(["read_file", "write_file", "edit_file", "list_directory"]);

interface FoldCounts {
  calls: number;
  files: number;
  commands: number;
  searches: number;
}

function foldCounts(steps: AgentStepView[]): FoldCounts {
  const counts: FoldCounts = { calls: 0, files: 0, commands: 0, searches: 0 };
  for (const step of steps) {
    if (step.kind !== "tool_call") continue;
    counts.calls += 1;
    if (step.tool_name === "execute_command") counts.commands += 1;
    else if (step.tool_name === "search_files") counts.searches += 1;
    else if (step.tool_name !== null && FILE_TOOLS.has(step.tool_name)) counts.files += 1;
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
function foldableRun(run: AgentRunView, sorted: AgentStepView[]): boolean {
  if (!isTerminalStatus(run.status)) return false;
  if (run.status === "budget_exhausted") return false;
  if (run.pending_approval !== null) return false;
  if (sorted.length < FOLD_MIN_STEPS) return false;
  if (!sorted.some((step) => step.kind === "tool_call")) return false;
  return !sorted.some(isProtectedRow);
}

/* =====================================================================
   STEP GRAMMAR (MAP rows 8 + 9, SYSTEM §8) — #230 P3
   =====================================================================
   One line per step, reading:

       ACTION  →  FILE  →  CHANGE  →  VERIFICATION
       Ran       db.rs     +12 −3    ok

   Before this pass a row read `#3 write_file src/db.rs succeeded 84ms`:
   the persisted STATUS VERBATIM (`succeeded`), a second copy of the tool
   name beside its own kind label, and no idea what the tool actually DID
   or what it changed. Three fixes, all binding:

   · ACTION is a verb, not an identifier. `read_file` → "Read",
     `write_file` → "Wrote". An unrecognised tool falls back to its own
     name and a generic verb, because inventing a verb for a tool this
     file has never heard of would be a guess dressed as copy.
   · FILE is the argument the tool acted on, and it carries its FULL
     value in `title` (SYSTEM §8 "paths on hover"), so the visible line can
     truncate without losing the path.
   · VERIFICATION is the word a person uses — "ok", "failed", "denied",
     "cancelled" — not the database's `succeeded`/`failed`/`denied`/
     `cancelled` CHECK constraint. Same four words, translated, and always
     PRESENT: a row never shows a status only as a colour.
   ===================================================================== */

/** tool_name → the past-tense verb for it. The key IS the tool name the
 *  backend persists, so an unknown tool falls through to `agent.actTool`
 *  rather than being dropped. */
const TOOL_VERBS: Record<string, StringKey> = {
  read_file: "agent.actRead",
  write_file: "agent.actWrite",
  edit_file: "agent.actEdit",
  search_files: "agent.actSearch",
  list_directory: "agent.actList",
  execute_command: "agent.actRun",
};

/** Whether this tool has a verb of its own. An UNKNOWN tool has to keep
 *  its identifier in the FILE slot: `apply_patch src/db.rs` says what ran,
 *  where a bare "Called src/db.rs" says only that something did — which is
 *  the generic, contentless row this grammar exists to replace. */
function hasToolVerb(step: AgentStepView): boolean {
  return step.tool_name !== null && TOOL_VERBS[step.tool_name] !== undefined;
}

function toolVerb(step: AgentStepView, locale: Locale = getLocale()): string {
  const key = step.tool_name ? TOOL_VERBS[step.tool_name] : undefined;
  return tr(locale, key ?? "agent.actTool");
}

/** The one argument this tool acted on — the file it touched, the query it
 *  searched, the command it ran. Returns null when the tool has no
 *  single obvious target, and the row then shows no file slot at all
 *  rather than a placeholder. */
function stepTarget(step: AgentStepView): { text: string; title: string } | null {
  const args = parseToolArgs(step.arguments);
  if (args === null) return null;
  if (step.tool_name === "execute_command") {
    const command = typeof args.command === "string" ? args.command : null;
    return command === null ? null : { text: command, title: tr(getLocale(), "agent.commandTitle") };
  }
  const path = typeof args.path === "string" ? args.path : null;
  if (path !== null) return { text: path, title: tr(getLocale(), "agent.pathTitle") };
  const query = typeof args.query === "string" ? args.query : null;
  if (query !== null) return { text: query, title: tr(getLocale(), "agent.queryTitle") };
  return null;
}

/** The persisted step status → the word shown. `null` is the LIVE shape
 *  (a step row with no status yet), which reads as running while its run
 *  is live and as nothing at all once it is not. */
function verifyWord(status: string | null, live: boolean, locale: Locale = getLocale()): string | null {
  if (status === null) return live ? tr(locale, "agent.verifyRunning") : null;
  switch (status) {
    case "succeeded":
      return tr(locale, "agent.verifyOk");
    case "failed":
      return tr(locale, "agent.verifyFailed");
    case "denied":
      return tr(locale, "agent.verifyDenied");
    case "cancelled":
      return tr(locale, "agent.verifyCancelled");
    default:
      return status;
  }
}

/** `+N −M` over a unified diff, counted from the lines themselves rather
 *  than from any stored number. Returns null when there is no diff text —
 *  the CHANGE slot then simply does not render. */
export function diffCounts(observation: string | null): { added: number; removed: number } | null {
  if (!observation) return null;
  let added = 0;
  let removed = 0;
  for (const line of observation.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) continue;
    if (line.startsWith("+")) added += 1;
    else if (line.startsWith("-")) removed += 1;
  }
  if (added === 0 && removed === 0) return null;
  return { added, removed };
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
interface StepGroup {
  key: string; // grouping identity (tool|target) — consecutive comparison only
  id: string;  // stable unique DOM/React identity within the run
  steps: AgentStepView[];
}

function groupKey(step: AgentStepView): string | null {
  if (step.kind !== "tool_call" || step.tool_name === null) return null;
  const target = stepTarget(step);
  // No single target means no identity to repeat: two `execute_command`
  // calls with different commands must not fold into one line.
  return target === null ? null : `${step.tool_name}|${target.text}`;
}

function groupSteps(steps: AgentStepView[]): Array<AgentStepView | StepGroup> {
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

function isGroup(item: AgentStepView | StepGroup): item is StepGroup {
  return (item as StepGroup).steps !== undefined;
}

/** RU noun declension for a count (байт/байта/байтов): same one/few/many
 * rule as `pluralForm` in strings.ts, as a tiny call-site triple since the
 * `agent.showFull` template carries the count inline with `{unit}`. */
function ruPlural(n: number, forms: [string, string, string]): string {
  const mod10 = Math.abs(n) % 10;
  const mod100 = Math.abs(n) % 100;
  if (mod10 === 1 && mod100 !== 11) return forms[0];
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return forms[1];
  return forms[2];
}

/** Nouns the run summary counts. EN needs singular/plural, RU the full
 * one/few/many triple - the same shape `ruPlural` already applies to the
 * `agent.showFull` byte unit, kept at the call site so the catalog holds one
 * `{n} {unit}` template per bucket instead of nine near-identical strings.
 *
 * #230 P3: three of these four died with the counted fold summary, which
 * was the duplicated second "COMPLETED" block. Only commands survive,
 * because "3 commands" is a fact the run's own summary line still reports
 * and no `tp` plural key carries. Tool calls, files and searches go
 * through `tp(locale, "toolCalls" | "changedFiles", n)` instead, which is
 * the catalog's own plural machinery rather than a second copy of it. */
interface CountedNoun {
  en: [string, string];
  ru: [string, string, string];
}

const NOUN_COMMANDS: CountedNoun = {
  en: ["command", "commands"],
  ru: ["команда", "команды", "команд"],
};

/** One counted noun phrase for the run summary line: `3 команды`.
 * The template (`{n} {unit}`) is localized in the catalog and interpolated by
 * `tr`; the noun declines here - EN takes the singular at 1 and the plural
 * otherwise, RU walks the one/few/many triple through `ruPlural` above. */
function countedPhrase(locale: Locale, key: StringKey, n: number, noun: CountedNoun): string {
  const unit = locale === "ru" ? ruPlural(n, noun.ru) : n === 1 ? noun.en[0] : noun.en[1];
  return tr(locale, key, { n, unit });
}

function parseToolArgs(json: string | null): Record<string, unknown> | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function TerminalView({ step }: { step: AgentStepView }) {
  const { t } = useStrings();
  const args = parseToolArgs(step.arguments);
  const command = typeof args?.command === "string" ? (args.command as string) : "";
  const cwd = typeof args?.cwd === "string" ? (args.cwd as string) : null;
  const observation = step.observation ?? "";
  const sep = "--- stderr ---\n";
  const sepIdx = observation.indexOf(sep);
  const stdout = sepIdx >= 0 ? observation.slice(0, sepIdx) : observation;
  const stderr = sepIdx >= 0 ? observation.slice(sepIdx + sep.length) : null;
  const hasStderr = stderr !== null && stderr.length > 0;
  return (
    <div className="nex-agent-terminal" role="group" aria-label={t("agent.terminalOut")}>
      {command && (
        <div className="nex-agent-terminal-header">
          <span className="nex-agent-terminal-prompt" aria-hidden="true">
            $
          </span>
          <span className="nex-agent-terminal-command nex-tag-mono">{command}</span>
          {cwd && <span className="nex-agent-terminal-cwd nex-tag-mono">({cwd})</span>}
        </div>
      )}
      <div className="nex-agent-terminal-body">
        {stdout && <pre className="nex-agent-terminal-stdout">{stdout}</pre>}
        {hasStderr && (
          <>
            <div className="nex-agent-terminal-separator" aria-hidden="true">
              --- stderr ---
            </div>
            <pre className="nex-agent-terminal-stderr">{stderr}</pre>
          </>
        )}
        {!stdout && !hasStderr && <span className="nex-agent-step-empty">{t("agent.noOutput")}</span>}
      </div>
    </div>
  );
}

/** One unified diff (MAP row 9).
 *
 *  NEVER RENDERS EMPTY (MAP row 9, "Diffs never appear empty"). Three
 *  shapes can arrive here — a real diff, a write that changed nothing, and
 *  a write with no observation text at all — and the old code collapsed the
 *  last two into the same one-word span while a "diff" whose lines are all
 *  headers rendered as a bordered box containing nothing but `--- a/x` and
 *  `+++ b/x`. Now the empty cases SAY what they are, in words, and the
 *  frame only appears when there is something in it.
 *
 *  The counts in the header are counted from the lines themselves
 *  (`diffCounts` above) rather than from any stored number, so a header
 *  can never claim "+12 −3" over a diff that shows none. */
export function DiffView({ observation }: { observation: string | null }) {
  const { locale, t } = useStrings();
  if (!observation) return <span className="nex-agent-step-empty">{t("agent.noChanges")}</span>;
  const lines = observation.split("\n");
  const counts = diffCounts(observation);
  if (counts === null) {
    // Headers or context only: there is no change to show, and saying so
    // is more honest than framing an empty box as a diff.
    return (
      <span className="nex-agent-step-empty" aria-label={t("agent.noChangesAria")}>
        {t("agent.noChanges")}
      </span>
    );
  }
  return (
    <div className="nex-agent-diff" role="group" aria-label={t("agent.fileDiff")}>
      {/* The compact action line MAP row 9 asks for, carrying the counts as
          OUTLINE badges rather than filled pills. The label says what the
          numbers are, so the badges are never the only carrier. */}
      <div className="nex-agent-diff-header-line">
        <span className="nex-agent-diff-counts" title={t("agent.diffCountsTitle", counts)}>
          <span className="nex-agent-diff-count nex-agent-diff-count-added">
            +{counts.added}
          </span>
          <span className="nex-agent-diff-count nex-agent-diff-count-removed">
            −{counts.removed}
          </span>
        </span>
        <span className="nex-sr-only">
          {tr(locale, "agent.diffCountsTitle", counts)}
        </span>
      </div>
      <div className="nex-agent-diff-body nex-tag-mono">
        {lines.map((line, idx) => {
          // Classify line for styling
          let cls = "nex-agent-diff-line";
          let gutter: string = " ";
          let content: string = line;
          let ariaLabel: string | undefined;
          if (line.startsWith("--- ")) {
            cls += " nex-agent-diff-header";
            gutter = "-";
            ariaLabel = t("agent.ariaRemovedHeader");
          } else if (line.startsWith("+++ ")) {
            cls += " nex-agent-diff-header";
            gutter = "+";
            ariaLabel = t("agent.ariaAddedHeader");
          } else if (line.startsWith("@@")) {
            cls += " nex-agent-diff-hunk";
            gutter = "@";
            ariaLabel = t("agent.ariaHunk");
          } else if (line.startsWith("+") && !line.startsWith("+++")) {
            cls += " nex-agent-diff-added";
            gutter = "+";
            content = line.slice(1);
            ariaLabel = t("agent.ariaAdded");
          } else if (line.startsWith("-") && !line.startsWith("---")) {
            cls += " nex-agent-diff-removed";
            gutter = "-";
            content = line.slice(1);
            ariaLabel = t("agent.ariaRemoved");
          } else if (line.startsWith(" ")) {
            cls += " nex-agent-diff-context";
            gutter = " ";
            content = line.slice(1);
          } else if (line.startsWith("...")) {
            cls += " nex-agent-diff-meta";
            gutter = " ";
          } else if (line === "") {
            // Empty line from trailing newline split: render as empty context
            cls += " nex-agent-diff-context";
            gutter = " ";
            content = "";
          }
          return (
            <div key={idx} className={cls}>
              <span className="nex-agent-diff-gutter" aria-hidden="true">
                {gutter}
              </span>
              {ariaLabel !== undefined && (
                <span className="nex-sr-only">{ariaLabel}: </span>
              )}
              <span className="nex-agent-diff-content">{content}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** The one line, in the MAP row 8 grammar. Split out so a grouped
 *  repeat's disclosure row can render the SAME line with a count, without
 *  two copies of the grammar drifting apart. */
function StepLine({
  step,
  live,
  locale,
}: {
  step: AgentStepView;
  /** The step's run is executing: a statusless row reads "running" rather
   *  than claiming nothing at all. */
  live: boolean;
  locale: Locale;
}) {
  const target = stepTarget(step);
  const verify = verifyWord(step.status, live, locale);
  const counts =
    step.tool_name === "write_file" || step.tool_name === "edit_file"
      ? diffCounts(step.observation)
      : null;
  // Three kinds, three subjects. A `model_turn` is the model's thinking
  // ("Thought"); an `approval` is a persisted decision about a named tool
  // ("Approval · execute_command"); a `tool_call` gets the verb grammar.
  // #230 P3: the old `kindLabel` covered all three, and folding it into the
  // action slot without the approval branch made every approval row read
  // "Thought" — a settled decision reported as the model musing.
  const isCall = step.kind === "tool_call";
  const known = hasToolVerb(step);
  const action = (() => {
    if (isCall) return toolVerb(step, locale);
    if (step.kind === "approval") {
      return tr(locale, "agent.kindApproval", { tool: step.tool_name ?? "" }).trim();
    }
    return tr(locale, "agent.kindModelTurn");
  })();
  // A known tool spends the FILE slot on the path it acted on. An unknown
  // one has to spend it on its OWN NAME, because the verb alone ("Called")
  // names nothing — the reader must be able to see which tool ran. Its
  // arguments, including any path, are on the expanded row either way.
  const identity =
    !isCall
      ? null
      : known
        ? target
        : step.tool_name !== null
          ? { text: step.tool_name, title: tr(locale, "agent.toolArgs") }
          : null;
  return (
    <>
      <span className="nex-agent-step-action">{action}</span>
      {identity !== null && (
        <span className="nex-agent-step-target nex-tag-mono" title={identity.title}>
          {identity.text}
        </span>
      )}
      {counts !== null && (
        <span
          className="nex-agent-step-change nex-tag-mono"
          title={tr(locale, "agent.diffCountsTitle", counts)}
        >
          {tr(locale, "agent.diffCounts", counts)}
        </span>
      )}
      {verify !== null && (
        <span className="nex-agent-step-verify">{verify}</span>
      )}
    </>
  );
}

const StepSection = memo(function StepSection({
  step,
  defaultOpen,
  fade,
  live,
  runId,
  onExpandStep,
  onCollapseStep,
}: {
  step: AgentStepView;
  defaultOpen: boolean;
  /** Temporal-fade bucket class ("" = current row), applied by the parent
   * decision in `fadeClass` and read by CSS only. */
  fade: string;
  /** The owning run is executing (not parked, not terminal). Drives the
   *  statusless row's "running" word — never a colour, never motion. */
  live: boolean;
  runId: number;
  onExpandStep?: (runId: number, seq: number) => void;
  onCollapseStep?: (runId: number, seq: number) => void;
}) {
  const { locale, t } = useStrings();
  const [open, setOpen] = useState<boolean>(defaultOpen);
  // NEX-PERF-003: `observation` is the bounded preview; the full text loads
  // explicitly into `observation_full` (dropped again on collapse), so the
  // render tree stays bounded no matter how long the run is.
  const expanded = step.observation_full !== null;
  const displayObservation = step.observation_full ?? step.observation ?? "";
  const isTerminal = step.kind === "tool_call" && step.tool_name === "execute_command";
  // Both file WRITERS produce a diff, not just `write_file`: an edit that
  // changed nothing still shows "No changes needed", which is the honest
  // read for a tool whose whole job is to change a file.
  const isDiff =
    step.kind === "tool_call" &&
    (step.tool_name === "write_file" || step.tool_name === "edit_file");
  // Stable identities (not inline arrows) so the memo above survives a
  // re-render driven by any other row's frame.
  const seq = step.seq;
  const handleExpand = useCallback(() => {
    onExpandStep?.(runId, seq);
  }, [onExpandStep, runId, seq]);
  const handleCollapse = useCallback(() => {
    onCollapseStep?.(runId, seq);
  }, [onCollapseStep, runId, seq]);

  return (
    <div className={fade ? `nex-agent-step ${fade}` : "nex-agent-step"}>
      <button
        type="button"
        className="nex-agent-step-header"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="nex-agent-step-seq">#{step.seq}</span>
        <StepLine step={step} live={live} locale={locale} />
        {step.duration_ms !== null && step.duration_ms !== undefined && (
          <span className="nex-agent-step-duration">{step.duration_ms}ms</span>
        )}
        <span className="nex-agent-step-chevron" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className="nex-agent-step-body">
          {isTerminal ? (
            <TerminalView step={{ ...step, observation: displayObservation }} />
          ) : isDiff ? (
            <DiffView observation={displayObservation} />
          ) : (
            <>
              {step.arguments && (
                <pre className="nex-agent-step-args nex-tag-mono" role="group" aria-label={t("agent.toolArgs")}>
                  {step.arguments}
                </pre>
              )}
              {displayObservation && <pre className="nex-agent-step-observation">{displayObservation}</pre>}
              {step.kind === "model_turn" && !displayObservation && (
                <span className="nex-agent-step-empty">{t("agent.noContent")}</span>
              )}
            </>
          )}
          {step.observation_truncated && !expanded && (
            <M3Button
              variant="quiet"
              size="sm"
              onClick={handleExpand}
              disabled={step.observation_loading}
            >
              {step.observation_loading
                ? t("agent.loadingFull")
                : t("agent.showFull", {
                    bytes: step.observation_len ?? 0,
                    unit:
                      locale === "ru"
                        ? ruPlural(step.observation_len ?? 0, ["байт", "байта", "байтов"])
                        : (step.observation_len ?? 0) === 1
                          ? "byte"
                          : "bytes",
                  })}
            </M3Button>
          )}
          {step.observation_error && !expanded && !step.observation_loading && (
            <div className="nex-agent-approval-actions">
              <span className="nex-agent-run-error" role="alert">
                {t("agent.fullLoadError")}
              </span>
              <M3Button variant="quiet" size="sm" onClick={handleExpand}>
                {t("common.retry")}
              </M3Button>
            </div>
          )}
          {expanded && (
            <M3Button variant="quiet" size="sm" onClick={handleCollapse}>
              {t("agent.showLess")}
            </M3Button>
          )}
        </div>
      )}
    </div>
  );
});

/** Repeated calls, one line, expandable list (MAP row 8, P3 acceptance).
 *
 *  The group header carries the SAME action/target line as a single row
 *  plus a count, so a repeat reads as the fact it is ("Read ×6 src/a.ts")
 *  rather than six near-identical lines. Expanding restores the SAME
 *  `StepSection` elements in the SAME order with their own open state
 *  intact — the `hidden` attribute, never a conditional mount, for the
 *  same reason the run-level fold uses it.
 *
 *  A group with a failed member is never collapsed by default: the failure
 *  has to stay findable (SYSTEM §8 "failure stays findable"), so it opens
 *  itself once and stays open until the reader closes it. */
const StepGroupSection = memo(function StepGroupSection({
  group,
  defaultOpen,
  fade,
  live,
  runId,
  timelineId,
  onExpandStep,
  onCollapseStep,
}: {
  group: StepGroup;
  defaultOpen: boolean;
  fade: string;
  live: boolean;
  runId: number;
  timelineId: string;
  onExpandStep?: (runId: number, seq: number) => void;
  onCollapseStep?: (runId: number, seq: number) => void;
}) {
  const { locale, t } = useStrings();
  const head = group.steps[0];
  const hasFailure = group.steps.some(isProtectedRow);
  const [open, setOpen] = useState<boolean>(defaultOpen || hasFailure);
  const n = group.steps.length;
  const target = stepTarget(head);
  return (
    <div className={fade ? `nex-agent-step ${fade}` : "nex-agent-step"}>
      <button
        type="button"
        className="nex-agent-step-header nex-agent-step-header-group"
        aria-expanded={open}
        aria-controls={timelineId}
        aria-label={t(open ? "agent.repeatedHide" : "agent.repeatedShow", {
          n: tp(locale, "toolCalls", n),
          target: target?.text ?? "",
        })}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="nex-agent-step-seq">
          #{head.seq}–#{group.steps[group.steps.length - 1].seq}
        </span>
        <StepLine step={head} live={live} locale={locale} />
        <span className="nex-agent-step-count nex-tag-mono">
          {t("agent.repeatedCalls", { n })}
        </span>
        <span className="nex-agent-step-chevron" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {/* `hidden`, NEVER a conditional mount — the same discipline as the
          run-level fold above, and for the same reasons: the group's rows
          stay in the tree so each keeps its own open state across the round
          trip, and so `aria-controls` points at a region that EXISTS in
          both states. A disclosure whose `aria-controls` target is absent
          while collapsed resolves to nothing for a screen reader, which is
          the relationship the attribute exists to declare. */}
      <div className="nex-agent-step-group" id={timelineId} hidden={!open}>
        {group.steps.map((step) => (
          <StepSection
            key={`${runId}-${step.seq}`}
            step={step}
            // A repeat's members are never auto-opened: the group line
            // already said what happened, and opening all six buries the
            // rest of the timeline.
            defaultOpen={false}
            fade=""
            live={live}
            runId={runId}
            onExpandStep={onExpandStep}
            onCollapseStep={onCollapseStep}
          />
        ))}
      </div>
    </div>
  );
});

/** The Activity Spine (brief §7). Mounted once per run and never
 * conditionally on data: the rail itself is the run's timeline frame and
 * stays put whether the run is live, parked or finished. `live` decides only
 * whether the travelling accent segment is present, so "no pulse when not
 * running" is structural as well as a CSS rule — and because the segment
 * keeps the same element identity across step frames, the animation runs
 * continuously instead of restarting on every arriving step. */
const AgentSpine = memo(function AgentSpine({ live }: { live: boolean }) {
  return (
    <div className="nex-agent-spine" aria-hidden="true">
      {live && <span className="nex-agent-spine-pulse" />}
    </div>
  );
});

/** The folded-run disclosure (§13.33, P3 acceptance "folded tool calls —
 *  one line each, repeated calls grouped with an expandable list").
 *
 *  #230 P3 changed what this line SAYS. It used to be a counted summary
 *  ("12 tool calls · 4 files · 2 commands") — which, next to the run's own
 *  status pill, was the duplicated COMPLETED block P3's acceptance forbids:
 *  two blocks claiming the same result in two different registers. The run
 *  header now carries the ONE summary line with the real counts
 *  (`nex-agent-run-summary`), so this control says only what it DOES —
 *  show or hide the run's steps — and the numbers live in exactly one place.
 *
 *  `aria-controls` still names the region the button owns, so the
 *  relationship survives with the name. */
const StepFoldSummary = memo(function StepFoldSummary({
  expanded,
  timelineId,
  onToggle,
}: {
  expanded: boolean;
  timelineId: string;
  onToggle: () => void;
}) {
  const { t } = useStrings();
  return (
    <button
      type="button"
      className="nex-agent-fold"
      aria-expanded={expanded}
      aria-controls={timelineId}
      onClick={onToggle}
    >
      <span className="nex-agent-fold-label">
        {t(expanded ? "agent.foldHide" : "agent.foldShow")}
      </span>
      <span className="nex-agent-fold-chevron" aria-hidden="true">
        {expanded ? "▾" : "▸"}
      </span>
    </button>
  );
});

function formatCountdown(totalSecs: number, locale: Locale): string {
  const s = Math.max(0, Math.floor(totalSecs));
  if (s >= 3600) {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const r = s % 60;
    return `${h}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
  }
  const m = Math.floor(s / 60);
  const r = s % 60;
  if (m > 0) return `${m}:${String(r).padStart(2, "0")}`;
  // Sub-minute unit is localized (Latin `s` reads as a typo in Russian).
  return locale === "ru" ? `${r} с` : `${r}s`;
}

/** Seconds left on a pending approval, clamped to `[0, timeoutSecs]`
 * (NEX-AGENT-004): `requested_at` is client receipt time, so clock skew can
 * otherwise push the countdown above the deadline it claims to track. */
function remainingSecs(timeoutSecs: number, requestedAt: number, now: number): number {
  return Math.min(timeoutSecs, Math.max(0, requestedAt + timeoutSecs - now));
}

/** Deadline notice + live countdown for a pending approval (NEX-AGENT-004):
 * an unanswered approval expires and cancels the run, so the park is never
 * a silent hang. Hidden when the event carries no deadline (`timeout_secs`
 * 0 from older emitters). Purely presentational: the clock lives in the
 * parent (which also gates the Approve/Deny buttons on it), so the buttons
 * and the notice can never disagree. */
function ApprovalTimeoutNotice({
  remaining,
  expired,
}: {
  remaining: number;
  expired: boolean;
}) {
  const { locale, t } = useStrings();
  if (expired) {
    return (
      <p className="nex-agent-approval-timeout nex-agent-approval-timeout-expired" role="status">
        {t("agent.approvalElapsed")}
      </p>
    );
  }
  // Above 60s the clock ticks every 30s, so M:SS seconds would imply a
  // precision the countdown lacks — show minute-ceiled copy instead and
  // reserve M:SS for the under-60s 1s-tick branch.
  const expiry =
    remaining > 60
      ? t("agent.approvalExpiresInMin", { n: Math.ceil(remaining / 60) })
      : t("agent.approvalExpiresIn", { time: formatCountdown(remaining, locale) });
  return (
    <p className="nex-agent-approval-timeout">
      {t("agent.approvalTimeoutNotice")}{" "}
      <span className="nex-tag nex-tag-mono">{expiry}</span>
    </p>
  );
}

function AgentRunSteps({
  run,
  onResolveApproval,
  onCancel,
  onContinue,
  onPause,
  onResume,
  onExpandStep,
  onCollapseStep,
}: AgentRunStepsProps) {
  const { locale, t } = useStrings();
  const sorted = useMemo(() => [...run.steps].sort((a, b) => a.seq - b.seq), [run.steps]);
  const isRunning = run.status === "running";
  const isPaused = run.status === "paused";
  const isActive = isRunning || isPaused;
  const pending = run.pending_approval;
  const pendingDeadline = pending && pending.timeout_secs > 0 ? pending : null;
  const pendingId = pending?.call_id ?? null;

  // Live clock for the approval countdown. A 1s tick runs only under the
  // final 60s; a minutes-scale park (default 600s) ticks every 30s, and
  // prefers-reduced-motion schedules a single one-shot timeout to expiry
  // (state flip only, zero motion — no interval).
  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const [now, setNow] = useState<number>(() => Date.now() / 1000);
  useEffect(() => {
    setNow(Date.now() / 1000);
  }, [pendingId]);
  const remaining = pendingDeadline
    ? remainingSecs(pendingDeadline.timeout_secs, pendingDeadline.requested_at, now)
    : null;
  // 1s tick only under the final 60s; coarser above. The boolean bucket flips
  // the interval exactly once as the countdown crosses the minute mark.
  const underMinute = remaining !== null && remaining <= 60;
  useEffect(() => {
    if (!pendingDeadline || reduceMotion) return;
    const intervalMs = underMinute ? 1000 : 30000;
    const id = window.setInterval(() => setNow(Date.now() / 1000), intervalMs);
    return () => window.clearInterval(id);
  }, [pendingDeadline, pendingId, reduceMotion, underMinute]);
  // Reduced-motion one-shot: no interval, no motion — a single state flip
  // scheduled exactly at expiry so approvalElapsed can still turn over on
  // its own. Cleanup on unmount/run-change mirrors the interval above; if
  // time still remains after the flip fires, the effect re-arms itself.
  useEffect(() => {
    if (!pendingDeadline || !reduceMotion) return;
    if (remaining === null || remaining <= 0) return;
    const id = window.setTimeout(() => setNow(Date.now() / 1000), remaining * 1000);
    return () => window.clearTimeout(id);
  }, [pendingDeadline, pendingId, reduceMotion, remaining]);
  // Client-math expiry: the window elapsed with no server confirmation. The
  // buttons go away (never live past the self-declared expiry) and the
  // notice says so honestly — "was cancelled" appears only below, once the
  // server actually confirms `cancelled`.
  const approvalElapsed = remaining !== null && remaining <= 0;
  // Latch: once the window has observably elapsed, remember it so the
  // server-confirmed note below can show after `cancelled` clears pending.
  // Resets only on a newly parked approval, never on the cancel itself.
  const [elapsedSeen, setElapsedSeen] = useState(false);
  useEffect(() => {
    if (approvalElapsed) setElapsedSeen(true);
  }, [approvalElapsed]);
  useEffect(() => {
    if (pendingId !== null) setElapsedSeen(false);
  }, [pendingId]);

  // ---- #153: spine, fade, fold -------------------------------------------
  // A parked run keeps the persisted `running` status, so `status` alone
  // cannot tell "working" from "waiting for you" — the pending approval can,
  // and a run that is waiting for a human must not look like it is burning
  // cycles. The spine therefore pulses only for `running` with no approval
  // outstanding; every other frame keeps the identical rail, connectors and
  // status read with no motion at all.
  const parked = pending !== null;
  const spineLive = isRunning && !parked;
  const foldable = foldableRun(run, sorted);
  // One latch instead of a remount on the fold boundary: the rows stay
  // mounted either way, so a row's own open/closed state survives the round
  // trip and expanding restores the original objects in the original order.
  const [foldExpanded, setFoldExpanded] = useState(false);
  const folded = foldable && !foldExpanded;
  // Stable per run, so the fold button's `aria-controls` survives every step
  // frame and stays unique when a thread renders several runs at once.
  const timelineId = `nex-agent-timeline-${run.run_id}`;
  const handleFoldToggle = useCallback(() => {
    setFoldExpanded((v) => !v);
  }, []);
  // Stable per group: the identity is the head step's seq — unique within the
  // run (only one group starts at a given seq) and invariant across the
  // per-frame re-sort/re-group. Derived from seq, not from a sanitised text
  // blob: two non-consecutive groups with the same tool+target share that
  // text, and a non-latin target can sanitise to "" and collide even harder.
  const groupId = (id: string): string => `${timelineId}-${id}`;

  // ---- THE ONE SUMMARY LINE (#230 P3 acceptance) ------------------------
  // "Completion = ONE summary line carrying real results only — no
  // duplicated COMPLETED blocks."
  //
  // The header USED to render a status pill, and the folded-run summary
  // below it rendered its own counts, and for a completed run the reader
  // saw the word "Completed" twice with two different-looking blocks under
  // it. Now there is exactly one line: the status word plus the real
  // results of THIS run, counted from its own steps — files actually
  // written, commands actually run, tool calls actually made.
  //
  // "No changes needed" is the honest read for a finished run that touched
  // no file, and it is only ever shown for a run that genuinely finished:
  // an errored or cancelled run never gets to claim it did nothing wrong.
  const counts = foldCounts(sorted);
  const ranCalls = counts.calls;
  // Distinct files actually written/edited: dedupe on the target path each
  // step reports. A step whose target can't be resolved is excluded — we
  // never invent a file, and two steps touching the same path count once.
  const changedFiles = new Set<string>();
  for (const step of sorted) {
    if (step.kind !== "tool_call") continue;
    if (step.tool_name !== "write_file" && step.tool_name !== "edit_file") continue;
    if (isFailedStatus(step)) continue;
    const target = stepTarget(step);
    if (target === null) continue;
    changedFiles.add(target.text);
  }
  const filesChanged = changedFiles.size;
  const finishedClean = run.status === "completed";
  const summaryParts: string[] = [];
  if (filesChanged > 0) summaryParts.push(tp(locale, "changedFiles", filesChanged));
  if (counts.commands > 0) {
    summaryParts.push(countedPhrase(locale, "agent.summaryCommands", counts.commands, NOUN_COMMANDS));
  }
  if (summaryParts.length === 0) {
    summaryParts.push(
      finishedClean
        ? ranCalls > 0
          ? tr(locale, "agent.summaryNoChanges")
          : tr(locale, "agent.summaryNothing")
        : ranCalls > 0
          ? tp(locale, "toolCalls", ranCalls)
          : tr(locale, "agent.summaryNothing"),
    );
  } else if (ranCalls > 0) {
    summaryParts.push(tp(locale, "toolCalls", ranCalls));
  }
  const summaryText = summaryParts.join(" · ");
  // Cancelled runs say what a cancel means, because a cancelled run that
  // had already written files must not read as "done".
  const cancelledNote = run.status === "cancelled" && ranCalls === 0;

  // ---- THE ERROR BLOCK (SYSTEM §8) --------------------------------------
  // "Errors answer: what happened / what survived / next action."
  //
  // The run's error used to be a bare string inside the header toolbar, in
  // a `white-space: nowrap` span — which meant a long provider error was
  // CLIPPED at the toolbar's width, and nothing said what survived or what
  // to do next. The error now lives in its own block, always visible, never
  // folded away, answering all three questions in words.
  // A GENUINE failure, not every stop. `budget_exhausted` is excluded on
  // purpose: it is a pause with its own affordance (the Continue button in
  // this very header) and a summary line that already says what ran, so a
  // three-line failure block on top of it would be the loudest thing in
  // the run for a state that is waiting on a decision the user is being
  // offered. `error` and `spend_limit_exceeded` stopped on their own and
  // get the block.
  const isErrorRun = run.status === "error" || run.status === "spend_limit_exceeded";
  const survived =
    ranCalls > 0 || sorted.length > 0
      ? [
          ...(filesChanged > 0 ? [tp(locale, "changedFiles", filesChanged)] : []),
          ...(ranCalls > 0 ? [tp(locale, "toolCalls", ranCalls)] : []),
        ].join(" · ")
      : tr(locale, "agent.errorSurvivedNone");
  const nextAction =
    run.status === "spend_limit_exceeded"
      ? t("agent.errorNextSpend")
      : t("agent.errorNextGeneric");

  return (
    <section
      className={
        "nex-agent-run " + runStateClass(run.status) + (parked ? " nex-agent-run-parked" : "")
      }
      aria-label={t("agent.runAria", { id: run.run_id })}
    >
      <header className="nex-agent-run-header">
        {/* THE ONE completion line. Status word + real results, once. The
            spinner/dot that sit beside the word are the same motion rule the
            spine follows: a parked run keeps the persisted `running` status
            but is waiting on a human, not burning cycles. */}
        <span className={statusClass(run.status)} role="status">
          {isRunning && !parked && (
            <span className="nex-spinner nex-agent-spinner" aria-hidden="true" />
          )}
          {isPaused && <span className="nex-agent-paused-dot" aria-hidden="true" />}
          <span className="nex-agent-run-status">{statusLabel(run.status, locale)}</span>
          <span className="nex-agent-run-summary">{summaryText}</span>
          {cancelledNote && (
            <span className="nex-agent-run-summary-note">
              {t("agent.cancelledNote")}
            </span>
          )}
        </span>
        <span className="nex-agent-run-meta">
          <span className="nex-tag nex-tag-mono" title={t("agent.modelTitle")}>
            {run.model || "agent"}
          </span>
          <span className="nex-agent-run-id">run {run.run_id}</span>
        </span>
        <M3Toolbar
          label={t("agent.controlsAria", { id: run.run_id })}
          className="nex-agent-run-actions"
        >
          {isRunning && onPause && (
            <M3Button variant="quiet" size="sm" onClick={() => onPause(run.run_id)}>
              {t("agent.pause")}
            </M3Button>
          )}
          {isPaused && onResume && (
            <M3Button variant="secondary" size="sm" onClick={() => onResume(run.run_id)}>
              {t("agent.resume")}
            </M3Button>
          )}
          {isActive && (
            <M3Button variant="quiet" size="sm" onClick={() => onCancel(run.run_id)}>
              {t("agent.cancel")}
            </M3Button>
          )}
          {run.status === "budget_exhausted" && (
            <M3Button variant="secondary" size="sm" onClick={() => onContinue(run.run_id)}>
              {t("agent.continue")}
            </M3Button>
          )}
        </M3Toolbar>
      </header>

      {/* The error block: what happened / what survived / next action, in
          that order, in words. Outside the timeline, so it is never
          collapsed by the run fold nor dimmed by the temporal fade
          (SYSTEM §8: errors persist, findable, never faded). */}
      {isErrorRun && (
        <div className="nex-agent-failure" role="alert">
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">{t("agent.errorWhat")}</span>
            <span className="nex-agent-failure-value">
              {run.error ?? t("agent.errorWhatUnknown")}
            </span>
          </p>
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">{t("agent.errorSurvived")}</span>
            <span className="nex-agent-failure-value">{survived}</span>
          </p>
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">{t("agent.errorNext")}</span>
            <span className="nex-agent-failure-value">{nextAction}</span>
          </p>
        </div>
      )}

      {pending && (
        <div className="nex-agent-approval nex-fade-in" role="region" aria-label={t("agent.approvalTitle")}>
          <div className="nex-agent-approval-text">
            <strong>{pending.name}</strong> {t("agent.approvalSuffix")}
            <pre className="nex-agent-step-args nex-tag-mono">{pending.arguments}</pre>
          </div>
          {!approvalElapsed && (
            <div className="nex-agent-approval-actions">
              <M3Button
                variant="primary"
                size="sm"
                onClick={() => onResolveApproval(run.run_id, pending.call_id, true)}
              >
                {t("agent.approve")}
              </M3Button>
              <M3Button
                variant="destructive"
                size="sm"
                onClick={() => onResolveApproval(run.run_id, pending.call_id, false)}
              >
                {t("agent.deny")}
              </M3Button>
            </div>
          )}
          {pendingDeadline && remaining !== null && (
            <ApprovalTimeoutNotice remaining={remaining} expired={approvalElapsed} />
          )}
        </div>
      )}
      {!pending && run.status === "cancelled" && elapsedSeen && (
        <p className="nex-agent-approval-timeout nex-agent-approval-timeout-confirmed" role="status">
          {t("agent.approvalExpired")}
        </p>
      )}

      {sorted.length === 0 ? (
        <p className="nex-agent-empty">{isActive ? t("agent.runStarting") : t("agent.noSteps")}</p>
      ) : (
        <>
          {/* `hidden` and never a `display` override here: the attribute keeps
              every row mounted (so expanding restores the same objects, the
              same order, and each row's own open state) while taking the
              folded sequence out of sight and out of the a11y tree. The id
              is what the fold button's `aria-controls` points at, and is
              qualified by run id so two runs in one thread never collide. */}
          <div className="nex-agent-timeline" id={timelineId} hidden={folded}>
            <AgentSpine live={spineLive} />
            <div className="nex-agent-steps">
              {groupSteps(sorted).map((item, idx, all) => {
                const isLast = idx === all.length - 1;
                const protectedRow = isGroup(item)
                  ? item.steps.some(isProtectedRow)
                  : isProtectedRow(item);
                const failed = isGroup(item)
                  ? item.steps.some(isFailedStatus)
                  : isFailedStatus(item);
                const slotClass =
                  "nex-agent-step-slot" +
                  (isLast && isActive && !protectedRow ? " nex-agent-step-slot-current" : "") +
                  // The same predicate the fold gate and the fade ladder read:
                  // a denied or cancelled call keeps the failure tone too.
                  (failed ? " nex-agent-step-slot-failed" : "");
                const fade = isLast || protectedRow ? "" : fadeClass(all.length - 1 - idx);
                // #230 P3: "NO model monologue". A `model_turn`'s
                // observation is the model's own narration, and the old
                // rule auto-opened the current row — so on a live run the
                // raw thinking text streamed into the thread, unasked,
                // ahead of the answer. It now stays folded like every other
                // row and opens on request. Tool rows keep the auto-open:
                // a running command's output is the thing the user is
                // watching, and MAP row 8 asks for it.
                const autoOpen =
                  isLast &&
                  isActive &&
                  (isGroup(item) ? true : item.kind !== "model_turn");
                if (!isGroup(item)) {
                  return (
                    <div key={`${run.run_id}-${item.seq}`} className={slotClass}>
                      <StepSection
                        step={item}
                        defaultOpen={autoOpen}
                        fade={fade}
                        live={spineLive}
                        runId={run.run_id}
                        onExpandStep={onExpandStep}
                        onCollapseStep={onCollapseStep}
                      />
                    </div>
                  );
                }
                // A group of one is not a group — it renders as the plain
                // row, so the extra disclosure never wraps a single item.
                if (item.steps.length === 1) {
                  return (
                    <div key={`${run.run_id}-${item.steps[0].seq}`} className={slotClass}>
                      <StepSection
                        step={item.steps[0]}
                        defaultOpen={autoOpen}
                        fade={fade}
                        live={spineLive}
                        runId={run.run_id}
                        onExpandStep={onExpandStep}
                        onCollapseStep={onCollapseStep}
                      />
                    </div>
                  );
                }
                return (
                  <div key={`${run.run_id}-${item.id}`} className={slotClass}>
                    <StepGroupSection
                      group={item}
                      defaultOpen={autoOpen}
                      fade={fade}
                      live={spineLive}
                      runId={run.run_id}
                      timelineId={groupId(item.id)}
                      onExpandStep={onExpandStep}
                      onCollapseStep={onCollapseStep}
                    />
                  </div>
                );
              })}
            </div>
          </div>
          {foldable && (
            <StepFoldSummary
              expanded={!folded}
              timelineId={timelineId}
              onToggle={handleFoldToggle}
            />
          )}
        </>
      )}
    </section>
  );
}

export default memo(AgentRunSteps);
