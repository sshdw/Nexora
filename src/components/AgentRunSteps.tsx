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
//!     long sequence collapses into one counted summary row. Expanding
//!     restores the SAME step objects in the SAME order with each row's own
//!     open/closed state intact (rows are hidden, never unmounted).
//!
//! Callbacks arrive pre-bound to their run id and stable, so the memoized
//! root re-renders only when this run's own data changes (#148).

import { memo, useCallback, useEffect, useMemo, useState } from "react";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";
import { getLocale, tr, type Locale, type StringKey } from "../lib/strings";
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

/** A row the user must always be able to find: an approval gate, a failed
 * tool call, or a failed full-output fetch. These never fade (§13.9: "do not
 * fade important final results, errors, approvals") and they block folding
 * (§13.29: failure stays findable). */
function isProtectedRow(step: AgentStepView): boolean {
  return step.kind === "approval" || step.status === "failed" || step.observation_error !== null;
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
 * approval (the gate keeps its own Approve/Deny live), no failed row, no
 * failed full-output fetch — and the sequence must be long enough for the
 * summary to pay for itself and contain at least one tool call to count. */
function foldableRun(run: AgentRunView, sorted: AgentStepView[]): boolean {
  if (!isTerminalStatus(run.status)) return false;
  if (run.pending_approval !== null) return false;
  if (sorted.length < FOLD_MIN_STEPS) return false;
  if (!sorted.some((step) => step.kind === "tool_call")) return false;
  return !sorted.some(isProtectedRow);
}

function kindLabel(step: AgentStepView, locale: Locale = getLocale()): string {
  if (step.kind === "model_turn") return tr(locale, "agent.kindModelTurn");
  if (step.kind === "tool_call") return step.tool_name ?? tr(locale, "agent.kindToolCall");
  if (step.kind === "approval") return tr(locale, "agent.kindApproval", { tool: step.tool_name ?? "" }).trim();
  return step.kind;
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

/** Nouns the fold summary counts. EN needs singular/plural, RU the full
 * one/few/many triple — the same shape `ruPlural` already applies to the
 * `agent.showFull` byte unit, kept at the call site so the catalog holds one
 * `{n} {unit}` template per bucket instead of nine near-identical strings. */
interface CountedNoun {
  en: [string, string];
  ru: [string, string, string];
}

const NOUN_TOOL_CALLS: CountedNoun = {
  en: ["tool call", "tool calls"],
  ru: ["вызов", "вызова", "вызовов"],
};
const NOUN_FILES: CountedNoun = {
  en: ["file", "files"],
  ru: ["файл", "файла", "файлов"],
};
const NOUN_COMMANDS: CountedNoun = {
  en: ["command", "commands"],
  ru: ["команда", "команды", "команд"],
};
const NOUN_SEARCHES: CountedNoun = {
  en: ["search", "searches"],
  ru: ["поиск", "поиска", "поисков"],
};

/** One counted noun phrase for the fold summary: `12 tool calls`, `4 файла`.
 * The template (`{n} {unit}`) is localized in the catalog and interpolated by
 * `tr`; the noun declines here — EN takes the singular at 1 and the plural
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

export function DiffView({ observation }: { observation: string | null }) {
  const { t } = useStrings();
  if (!observation) return <span className="nex-agent-step-empty">{t("agent.noChanges")}</span>;
  const lines = observation.split("\n");
  // Remove trailing empty line from final split if observation ends with newline
  // Keep it as is for rendering; filter will handle.
  return (
    <div className="nex-agent-diff" role="group" aria-label={t("agent.fileDiff")}>
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

const StepSection = memo(function StepSection({
  step,
  defaultOpen,
  fade,
  runId,
  onExpandStep,
  onCollapseStep,
}: {
  step: AgentStepView;
  defaultOpen: boolean;
  /** Temporal-fade bucket class ("" = current row), applied by the parent
   * decision in `fadeClass` and read by CSS only. */
  fade: string;
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
  const isDiff = step.kind === "tool_call" && step.tool_name === "write_file";
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
        <span className="nex-agent-step-kind">{kindLabel(step, locale)}</span>
        {step.tool_name && step.kind === "tool_call" && (
          <span className="nex-agent-step-tool">{step.tool_name}</span>
        )}
        {step.status && <span className="nex-tag nex-tag-mono nex-agent-step-status">{step.status}</span>}
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

/** The folded-run disclosure (§13.33). One button that changes state — the
 * counts the hidden rows stood for, then the same control that brings them
 * back. Zero-count buckets are dropped rather than printed as `0 searches`;
 * the leading tool-call total is always shown, so the line can never read
 * empty. */
const StepFoldSummary = memo(function StepFoldSummary({
  steps,
  expanded,
  onToggle,
}: {
  steps: AgentStepView[];
  expanded: boolean;
  onToggle: () => void;
}) {
  const { locale } = useStrings();
  const counts = foldCounts(steps);
  const parts = [countedPhrase(locale, "agent.foldCalls", counts.calls, NOUN_TOOL_CALLS)];
  if (counts.files > 0) {
    parts.push(countedPhrase(locale, "agent.foldFiles", counts.files, NOUN_FILES));
  }
  if (counts.commands > 0) {
    parts.push(countedPhrase(locale, "agent.foldCommands", counts.commands, NOUN_COMMANDS));
  }
  if (counts.searches > 0) {
    parts.push(countedPhrase(locale, "agent.foldSearches", counts.searches, NOUN_SEARCHES));
  }
  return (
    <button type="button" className="nex-agent-fold" aria-expanded={expanded} onClick={onToggle}>
      <span className="nex-agent-fold-label">{parts.join(" · ")}</span>
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
  const handleFoldToggle = useCallback(() => {
    setFoldExpanded((v) => !v);
  }, []);

  return (
    <section
      className={
        "nex-agent-run " + runStateClass(run.status) + (parked ? " nex-agent-run-parked" : "")
      }
      aria-label={t("agent.runAria", { id: run.run_id })}
    >
      <header className="nex-agent-run-header">
        <span className={statusClass(run.status)} role="status">
          {isRunning && <span className="nex-spinner nex-agent-spinner" aria-hidden="true" />}
          {isPaused && <span className="nex-agent-paused-dot" aria-hidden="true" />}
          {statusLabel(run.status, locale)}
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
          {run.status === "spend_limit_exceeded" && run.error && (
            <span className="nex-agent-run-error nex-fade-in" role="alert">
              {run.error}
            </span>
          )}
          {run.status === "error" && run.error && (
            <span className="nex-agent-run-error" role="alert">
              {run.error}
            </span>
          )}
        </M3Toolbar>
      </header>

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
        <p className="nex-agent-empty">{isActive ? t("agent.waitingSteps") : t("agent.noSteps")}</p>
      ) : (
        <>
          {/* `hidden` and never a `display` override here: the attribute keeps
              every row mounted (so expanding restores the same objects, the
              same order, and each row's own open state) while taking the
              folded sequence out of sight and out of the a11y tree. */}
          <div className="nex-agent-timeline" hidden={folded}>
            <AgentSpine live={spineLive} />
            <div className="nex-agent-steps">
              {sorted.map((step, idx) => {
                const isLast = idx === sorted.length - 1;
                const protectedRow = isProtectedRow(step);
                const slotClass =
                  "nex-agent-step-slot" +
                  (isLast && isActive && !protectedRow ? " nex-agent-step-slot-current" : "") +
                  (step.status === "failed" ? " nex-agent-step-slot-failed" : "");
                return (
                  <div key={`${run.run_id}-${step.seq}`} className={slotClass}>
                    <StepSection
                      step={step}
                      defaultOpen={isLast && isActive}
                      fade={isLast || protectedRow ? "" : fadeClass(sorted.length - 1 - idx)}
                      runId={run.run_id}
                      onExpandStep={onExpandStep}
                      onCollapseStep={onCollapseStep}
                    />
                  </div>
                );
              })}
            </div>
          </div>
          {foldable && (
            <StepFoldSummary steps={sorted} expanded={!folded} onToggle={handleFoldToggle} />
          )}
        </>
      )}
    </section>
  );
}

export default memo(AgentRunSteps);
