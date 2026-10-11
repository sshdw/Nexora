//! Steps accordion for one agent run (Task 5.1 + 5.2).
//!
//! Presentational: kind-grouped collapsible sections (`model_turn` |
//! `tool_call` | `approval`), run status pill, live append, bare inline
//! Approve/Deny on `ApprovalRequested`, Continue on `BudgetExhausted`, pause/
//! resume controls, and per-tool body renderers — terminal for
//! `execute_command` (command line + stdout/stderr with distinct separator) and
//! diff viewer for `write_file` (headers, hunks, +/- gutters, mono,
//! collapsible). Chronological placement is owned by the thread view.

import { useEffect, useState } from "react";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";
import { getLocale, tr, type Locale } from "../lib/strings";
import { useStrings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3Toolbar from "./M3Toolbar";

export interface AgentRunStepsProps {
  run: AgentRunView;
  onResolveApproval: (callId: string, approved: boolean) => void;
  onCancel: () => void;
  onContinue: (extraSteps: number) => void;
  onPause?: () => void;
  onResume?: () => void;
  onExpandStep?: (seq: number) => void;
  onCollapseStep?: (seq: number) => void;
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

function StepSection({
  step,
  defaultOpen,
  onExpand,
  onCollapse,
}: {
  step: AgentStepView;
  defaultOpen: boolean;
  onExpand: () => void;
  onCollapse: () => void;
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

  return (
    <div className="nex-agent-step">
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
              onClick={onExpand}
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
              <M3Button variant="quiet" size="sm" onClick={onExpand}>
                {t("common.retry")}
              </M3Button>
            </div>
          )}
          {expanded && (
            <M3Button variant="quiet" size="sm" onClick={onCollapse}>
              {t("agent.showLess")}
            </M3Button>
          )}
        </div>
      )}
    </div>
  );
}

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

export default function AgentRunSteps({
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
  const sorted = [...run.steps].sort((a, b) => a.seq - b.seq);
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

  return (
    <section className="nex-agent-run" aria-label={t("agent.runAria", { id: run.run_id })}>
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
            <M3Button variant="quiet" size="sm" onClick={onPause}>
              {t("agent.pause")}
            </M3Button>
          )}
          {isPaused && onResume && (
            <M3Button variant="secondary" size="sm" onClick={onResume}>
              {t("agent.resume")}
            </M3Button>
          )}
          {isActive && (
            <M3Button variant="quiet" size="sm" onClick={onCancel}>
              {t("agent.cancel")}
            </M3Button>
          )}
          {run.status === "budget_exhausted" && (
            <M3Button variant="secondary" size="sm" onClick={() => onContinue(10)}>
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
                onClick={() => onResolveApproval(pending.call_id, true)}
              >
                {t("agent.approve")}
              </M3Button>
              <M3Button
                variant="destructive"
                size="sm"
                onClick={() => onResolveApproval(pending.call_id, false)}
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
        <div className="nex-agent-steps">
          {sorted.map((step, idx) => {
            const isLast = idx === sorted.length - 1;
            const defaultOpen = isActive ? isLast : false;
            return (
              <StepSection
                key={`${run.run_id}-${step.seq}`}
                step={step}
                defaultOpen={defaultOpen}
                onExpand={() => onExpandStep?.(step.seq)}
                onCollapse={() => onCollapseStep?.(step.seq)}
              />
            );
          })}
        </div>
      )}
    </section>
  );
}
