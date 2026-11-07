//! Pure label/format/predicate helpers for the agent-run steps tree
//! (AC-208 split, per the review §3.1 plan — moved verbatim from
//! `AgentRunSteps.tsx`). No React, no JSX: these are the strings and
//! classes the step renderers and the run line read.

import type { AgentStepView } from "../../lib/useAgentRun";
import { getLocale, tr, type Locale, type StringKey } from "../../lib/strings";

export function statusLabel(
  status: string,
  locale: Locale = getLocale()
): string {
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
export function runStateClass(status: string): string {
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
export function isTerminalStatus(status: string): boolean {
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
export function isFailedStatus(step: AgentStepView): boolean {
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
export function isProtectedRow(step: AgentStepView): boolean {
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
export function fadeClass(age: number): string {
  if (age <= 1) return "";
  return age === 2 ? "nex-agent-step-age-1" : "nex-agent-step-age-2";
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
export function hasToolVerb(step: AgentStepView): boolean {
  return step.tool_name !== null && TOOL_VERBS[step.tool_name] !== undefined;
}

export function toolVerb(
  step: AgentStepView,
  locale: Locale = getLocale()
): string {
  const key = step.tool_name ? TOOL_VERBS[step.tool_name] : undefined;
  return tr(locale, key ?? "agent.actTool");
}

export function parseToolArgs(
  json: string | null
): Record<string, unknown> | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** The one argument this tool acted on — the file it touched, the query it
 *  searched, the command it ran. Returns null when the tool has no
 *  single obvious target, and the row then shows no file slot at all
 *  rather than a placeholder. */
export function stepTarget(
  step: AgentStepView
): { text: string; title: string } | null {
  const args = parseToolArgs(step.arguments);
  if (args === null) return null;
  if (step.tool_name === "execute_command") {
    const command = typeof args.command === "string" ? args.command : null;
    return command === null
      ? null
      : { text: command, title: tr(getLocale(), "agent.commandTitle") };
  }
  const path = typeof args.path === "string" ? args.path : null;
  if (path !== null)
    return { text: path, title: tr(getLocale(), "agent.pathTitle") };
  const query = typeof args.query === "string" ? args.query : null;
  if (query !== null)
    return { text: query, title: tr(getLocale(), "agent.queryTitle") };
  return null;
}

/** Split a filesystem path into directory part (with trailing separator) and
 * file name, at the last separator. `src/lib/db.rs` → `dir="src/lib/"`,
 * `name="db.rs"`; `src/` → `dir="src/"`, `name=""`; `db.rs` → `dir=""`,
 * `name="db.rs"`. The file name is what the reader needs, so it stays
 * visible; only the directory prefix is elided. Single source of truth is
 * the same `identity.text` the slot already renders — nothing is parsed
 * twice. */
export function splitPathTarget(text: string): { dir: string; name: string } {
  const sep = text.lastIndexOf("/");
  if (sep < 0) return { dir: "", name: text };
  return { dir: text.slice(0, sep + 1), name: text.slice(sep + 1) };
}

/** The persisted step status → the word shown. `null` is the LIVE shape
 *  (a step row with no status yet), which reads as running while its run
 *  is live and as nothing at all once it is not. */
export function verifyWord(
  status: string | null,
  live: boolean,
  locale: Locale = getLocale()
): string | null {
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
export function diffCounts(
  observation: string | null
): { added: number; removed: number } | null {
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

/** RU noun declension for a count (байт/байта/байтов): same one/few/many
 * rule as `pluralForm` in strings.ts, as a tiny call-site triple since the
 * `agent.showFull` template carries the count inline with `{unit}`. */
export function ruPlural(n: number, forms: [string, string, string]): string {
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
export interface CountedNoun {
  en: [string, string];
  ru: [string, string, string];
}

export const NOUN_COMMANDS: CountedNoun = {
  en: ["command", "commands"],
  ru: ["команда", "команды", "команд"],
};

/** One counted noun phrase for the run summary line: `3 команды`.
 * The template (`{n} {unit}`) is localized in the catalog and interpolated by
 * `tr`; the noun declines here - EN takes the singular at 1 and the plural
 * otherwise, RU walks the one/few/many triple through `ruPlural` above. */
export function countedPhrase(
  locale: Locale,
  key: StringKey,
  n: number,
  noun: CountedNoun
): string {
  const unit =
    locale === "ru" ? ruPlural(n, noun.ru) : n === 1 ? noun.en[0] : noun.en[1];
  return tr(locale, key, { n, unit });
}

/** Real elapsed time for the one-line run summary. `null` unless the data
 * actually carries both ends: a live run has no `finished_at`, and a legacy
 * frame with `started_at: 0` cannot claim a duration it does not know. */
export function formatRunDuration(
  startedAt: number,
  finishedAt: number | null,
  locale: Locale = getLocale()
): string | null {
  if (finishedAt === null || startedAt <= 0 || finishedAt < startedAt)
    return null;
  const total = Math.max(0, Math.round(finishedAt - startedAt));
  if (total < 60) return tr(locale, "agent.durationSeconds", { n: total });
  return tr(locale, "agent.durationMinutes", {
    m: Math.floor(total / 60),
    s: total % 60,
  });
}

export function formatCountdown(totalSecs: number, locale: Locale): string {
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
export function remainingSecs(
  timeoutSecs: number,
  requestedAt: number,
  now: number
): number {
  return Math.min(timeoutSecs, Math.max(0, requestedAt + timeoutSecs - now));
}
