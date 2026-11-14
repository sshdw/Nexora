//! The run row itself: the ONE line (status + steps + duration + real
//! results), the error block, the approval park with its countdown, the
//! Activity Spine, and the foldable timeline composing the step sections
//! (AC-208 split, per the review §3.1 plan — moved verbatim from
//! `AgentRunSteps.tsx`, which re-exports this module).

import { memo, useCallback, useEffect, useMemo, useState } from "react";

import type { AgentRunView } from "../../lib/useAgentRun";
import { tp, tr } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";
import M3Toolbar from "../M3Toolbar";
import {
  countedPhrase,
  fadeClass,
  formatCountdown,
  formatRunDuration,
  isFailedStatus,
  isProtectedRow,
  NOUN_COMMANDS,
  remainingSecs,
  runStateClass,
  statusLabel,
  stepTarget,
} from "./labels";
import { foldCounts, foldableRun, groupSteps, isGroup } from "./grouping";
import { StepSection } from "./StepSection";
import { StepGroupSection } from "./StepGroupSection";

export interface AgentRunStepsProps {
  run: AgentRunView;
  /** Every callback carries its own `runId`: the parent owns the binding so
   * the props stay referentially stable across renders. An inline arrow per
   * row (one per run, rebuilt on every message frame) would defeat the memo
   * below and re-render every run in the thread each time any run advances. */
  onResolveApproval: (
    runId: number,
    callId: string,
    approved: boolean,
    scope?: "single" | "group"
  ) => void;
  onCancel: (runId: number) => void;
  onContinue: (runId: number) => void;
  onPause?: (runId: number) => void;
  onResume?: (runId: number) => void;
  onExpandStep?: (runId: number, seq: number) => void;
  onCollapseStep?: (runId: number, seq: number) => void;
  /** AC-102/ADD-08 lazy steps: fired when this run's step list is (or
   *  becomes) visible with no steps loaded — the store dedupes and fetches
   *  the persisted list once. The parent owns the binding so the prop stays
   *  referentially stable across frames, like every callback above. */
  onLoadRunSteps?: (runId: number) => void;
}

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
 *  Round 5 (D6) merged this control into the run's ONE line: the line IS
 *  the disclosure now, so the counts and the action no longer live in two
 *  places. The component is gone; the CSS keeps the line's hover tone.
 */

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
      <p
        className="nex-agent-approval-timeout nex-agent-approval-timeout-expired"
        role="status"
      >
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
      : t("agent.approvalExpiresIn", {
          time: formatCountdown(remaining, locale),
        });
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
  onLoadRunSteps,
}: AgentRunStepsProps) {
  const { locale, t } = useStrings();
  const sorted = useMemo(
    () => [...run.steps].sort((a, b) => a.seq - b.seq),
    [run.steps]
  );
  const isRunning = run.status === "running";
  const isPaused = run.status === "paused";
  const isActive = isRunning || isPaused;
  const pending = run.status === "interrupted" ? null : run.pending_approval;
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
    ? remainingSecs(
        pendingDeadline.timeout_secs,
        pendingDeadline.requested_at,
        now
      )
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
    const id = window.setTimeout(
      () => setNow(Date.now() / 1000),
      remaining * 1000
    );
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
  // `null` follows the data (a finished long run starts folded); a click
  // writes an explicit override that survives every step frame.
  const [manualExpanded, setManualExpanded] = useState<boolean | null>(null);
  const collapsed = manualExpanded === null ? foldable : !manualExpanded;
  // Stable per run, so the line's `aria-controls` survives every step frame
  // and stays unique when a thread renders several runs at once.
  const timelineId = `nex-agent-timeline-${run.run_id}`;
  const handleLineToggle = useCallback(() => {
    setManualExpanded((value) => {
      const currentCollapsed = value === null ? foldable : !value;
      return currentCollapsed;
    });
  }, [foldable]);
  // AC-102/ADD-08 lazy steps: a run whose step list is visible but not
  // loaded (rehydrated unloaded — live runs arrive with theirs) asks the
  // store once per visibility change; the store dedupes (loaded / in-flight
  // guards), so a StrictMode double-effect is harmless and a failed fetch
  // retries on the next collapse/expand cycle.
  const sortedCount = sorted.length;
  useEffect(() => {
    if (!collapsed && sortedCount === 0) onLoadRunSteps?.(run.run_id);
  }, [collapsed, sortedCount, onLoadRunSteps, run.run_id]);
  // Stable per group: the identity is the head step's seq — unique within the
  // run (only one group starts at a given seq) and invariant across the
  // per-frame re-sort/re-group. Derived from seq, not from a sanitised text
  // blob: two non-consecutive groups with the same tool+target share that
  // text, and a non-latin target can sanitise to "" and collide even harder.
  const groupId = (id: string): string => `${timelineId}-${id}`;

  // ---- THE ONE LINE (D6, carrying #230 P3's real results) --------------
  // The run is no longer a card with a header: it is ONE compact line in the
  // thread. The line says what the run is doing and how many steps it has —
  // "Working… · 3 steps" while live, "Done in 7 steps · 12s" when finished.
  // The real results (files actually written, commands actually run, tool
  // calls actually made) are still counted from this run's own steps and
  // shown once, in the expanded detail row. "No changes needed" is only ever
  // said for a run that genuinely finished: an errored or cancelled run never
  // gets to claim it did nothing wrong.
  const counts = foldCounts(sorted);
  const ranCalls = counts.calls;
  // Distinct files actually written/edited: dedupe on the target path each
  // step reports. A step whose target can't be resolved is excluded — we
  // never invent a file, and two steps touching the same path count once.
  const changedFiles = new Set<string>();
  for (const step of sorted) {
    if (step.kind !== "tool_call") continue;
    if (step.tool_name !== "write_file" && step.tool_name !== "edit_file")
      continue;
    if (isFailedStatus(step)) continue;
    const target = stepTarget(step);
    if (target === null) continue;
    changedFiles.add(target.text);
  }
  const filesChanged = changedFiles.size;
  const finishedClean = run.status === "completed";
  const summaryParts: string[] = [];
  if (filesChanged > 0)
    summaryParts.push(tp(locale, "changedFiles", filesChanged));
  if (counts.commands > 0) {
    summaryParts.push(
      countedPhrase(
        locale,
        "agent.summaryCommands",
        counts.commands,
        NOUN_COMMANDS
      )
    );
  }
  if (summaryParts.length === 0) {
    summaryParts.push(
      finishedClean
        ? ranCalls > 0
          ? tr(locale, "agent.summaryNoChanges")
          : tr(locale, "agent.summaryNothing")
        : ranCalls > 0
          ? tp(locale, "toolCalls", ranCalls)
          : tr(locale, "agent.summaryNothing")
    );
  } else if (ranCalls > 0) {
    summaryParts.push(tp(locale, "toolCalls", ranCalls));
  }
  const summaryText = summaryParts.join(" · ");
  // AC-102/ADD-08: while the persisted step list is still unloaded, every
  // count on the line reads the run's persisted `total_steps` column — never
  // the (still empty) in-memory list — so a completed 12-step run cannot
  // flash "Done in 0 steps" in the moment before its lazy fetch resolves.
  // Any step on screen (loaded list or live frames) switches the counts back
  // to the real rows, which is how live runs keep their per-frame tallies.
  const unloaded = !run.steps_loaded && sorted.length === 0;
  const lineStepCount = unloaded ? (run.total_steps ?? 0) : sorted.length;
  // Cancelled runs say what a cancel means, because a cancelled run that
  // had already written files must not read as "done". While the step list
  // is unloaded the persisted count stands in for the tool tally, so an
  // unloaded cancelled run with recorded steps never claims "nothing ran".
  const cancelledNote =
    run.status === "cancelled" &&
    (unloaded ? lineStepCount === 0 : ranCalls === 0);

  // The line's own sentence. `tp(locale, "steps", n)` carries the declensed
  // noun; the duration is appended only when `finished_at` is real.
  const stepPhrase = tp(locale, "steps", lineStepCount);
  const durationText = formatRunDuration(
    run.started_at,
    run.finished_at,
    locale
  );
  let lineText: string;
  if (isRunning && lineStepCount === 0) {
    lineText = tr(locale, "agent.runStarting");
  } else if (isRunning) {
    lineText = tr(locale, "agent.workingSteps", { steps: stepPhrase });
  } else if (run.status === "completed") {
    lineText = tr(locale, "agent.doneIn", { steps: stepPhrase });
  } else if (lineStepCount === 0) {
    lineText = statusLabel(run.status, locale);
  } else {
    lineText = `${statusLabel(run.status, locale)} · ${stepPhrase}`;
  }
  if (durationText !== null) lineText = `${lineText} · ${durationText}`;
  if (cancelledNote)
    lineText = `${lineText} · ${tr(locale, "agent.cancelledNote")}`;

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
  const isErrorRun =
    run.status === "error" || run.status === "spend_limit_exceeded";
  const survived =
    ranCalls > 0 || sorted.length > 0
      ? [
          ...(filesChanged > 0
            ? [tp(locale, "changedFiles", filesChanged)]
            : []),
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
        "nex-agent-run " +
        runStateClass(run.status) +
        (parked ? " nex-agent-run-parked" : "")
      }
      aria-label={t("agent.runAria", { id: run.run_id })}
    >
      <div className="nex-agent-run-line">
        {/* THE ONE line. Status + step count + real duration, once. The
            spinner/dot beside the word follow the spine's motion rule: a
            parked run keeps the persisted `running` status but is waiting on
            a human, not burning cycles. The line is the disclosure: pressing
            it opens the step list in place, with no card around it. An
            unloaded foldable run (AC-102/ADD-08) has no rows to show yet but
            MUST still render as the disclosure — a static line here would
            leave a folded run with no way to expand, so its steps could
            never lazy-load. */}
        {sorted.length === 0 && !(foldable && !run.steps_loaded) ? (
          <span className="nex-agent-run-disclosure is-static" role="status">
            {isRunning && !parked && (
              <span
                className="nex-spinner nex-agent-spinner"
                aria-hidden="true"
              />
            )}
            {isPaused && (
              <span className="nex-agent-paused-dot" aria-hidden="true" />
            )}
            <span className="nex-agent-run-status">{lineText}</span>
          </span>
        ) : (
          <button
            type="button"
            className="nex-agent-run-disclosure"
            aria-expanded={!collapsed}
            aria-controls={timelineId}
            onClick={handleLineToggle}
          >
            {isRunning && !parked && (
              <span
                className="nex-spinner nex-agent-spinner"
                aria-hidden="true"
              />
            )}
            {isPaused && (
              <span className="nex-agent-paused-dot" aria-hidden="true" />
            )}
            <span className="nex-agent-run-status" role="status">
              {lineText}
            </span>
          </button>
        )}
        <M3Toolbar
          label={t("agent.controlsAria", { id: run.run_id })}
          className="nex-agent-run-actions"
        >
          {isRunning && onPause && (
            <M3Button
              variant="quiet"
              size="sm"
              onClick={() => onPause(run.run_id)}
            >
              {t("agent.pause")}
            </M3Button>
          )}
          {isPaused && onResume && (
            <M3Button
              variant="secondary"
              size="sm"
              onClick={() => onResume(run.run_id)}
            >
              {t("agent.resume")}
            </M3Button>
          )}
          {isActive && (
            <M3Button
              variant="quiet"
              size="sm"
              onClick={() => onCancel(run.run_id)}
            >
              {t("agent.cancel")}
            </M3Button>
          )}
          {run.status === "budget_exhausted" && (
            <M3Button
              variant="secondary"
              size="sm"
              onClick={() => onContinue(run.run_id)}
            >
              {t("agent.continue")}
            </M3Button>
          )}
        </M3Toolbar>
      </div>

      {/* The error block: what happened / what survived / next action, in
          that order, in words. Outside the timeline, so it is never
          collapsed by the run fold nor dimmed by the temporal fade
          (SYSTEM §8: errors persist, findable, never faded). */}
      {run.status === "interrupted" && (
        <p className="nex-agent-run-summary">{t("agent.interruptedHistory")}</p>
      )}
      {isErrorRun && (
        <div className="nex-agent-failure" role="alert">
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">
              {t("agent.errorWhat")}
            </span>
            <span className="nex-agent-failure-value">
              {run.error ?? t("agent.errorWhatUnknown")}
            </span>
          </p>
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">
              {t("agent.errorSurvived")}
            </span>
            <span className="nex-agent-failure-value">{survived}</span>
          </p>
          <p className="nex-agent-failure-line">
            <span className="nex-agent-failure-key">
              {t("agent.errorNext")}
            </span>
            <span className="nex-agent-failure-value">{nextAction}</span>
          </p>
        </div>
      )}

      {pending && (
        <div
          className="nex-agent-approval nex-fade-in"
          role="region"
          aria-label={t("agent.approvalTitle")}
        >
          <div className="nex-agent-approval-text">
            <strong>{pending.name}</strong> {t("agent.approvalSuffix")}
            {pending.metadata?.shell && (
              <>
                <p>{t(`agent.category.${pending.metadata.shell.category}`)}</p>
                <p>{t("agent.shellFallback")}</p>
                {pending.metadata.shell.packages.length > 0 && (
                  <p>{pending.metadata.shell.packages.join(", ")}</p>
                )}
                {pending.metadata.shell.category === "DependencyInstall" &&
                  !pending.metadata.shell.packages_exact && (
                    <p>{t("agent.packagesUnknown")}</p>
                  )}
                {pending.metadata.shell.uncertain && (
                  <p>{t("agent.shellUncertain")}</p>
                )}
              </>
            )}
            {pending.metadata?.sensitive_write && (
              <p>{t("agent.sensitiveWrite")}</p>
            )}
            <pre className="nex-agent-step-args nex-tag-mono">
              {pending.arguments}
            </pre>
          </div>
          {!approvalElapsed && (
            <div className="nex-agent-approval-actions">
              <M3Button
                variant="primary"
                size="sm"
                onClick={() =>
                  onResolveApproval(run.run_id, pending.call_id, true)
                }
              >
                {t("agent.allowOnce")}
              </M3Button>
              {pending.name !== "execute_command" &&
                pending.metadata?.allow_for_run && (
                  <M3Button
                    variant="quiet"
                    size="sm"
                    onClick={() =>
                      onResolveApproval(
                        run.run_id,
                        pending.call_id,
                        true,
                        "group"
                      )
                    }
                  >
                    {t("agent.allowRun")}
                  </M3Button>
                )}
              {/* Deny is a GHOST, not a red button (O-02: "Approve primary,
                  Deny ghost"). Declining is the safe answer, so it must not
                  wear the error colour that the brief reserves for states;
                  one filled control per decision keeps the primary obvious
                  without making the refusal look dangerous. */}
              <M3Button
                variant="quiet"
                size="sm"
                onClick={() =>
                  onResolveApproval(run.run_id, pending.call_id, false)
                }
              >
                {t("agent.deny")}
              </M3Button>
            </div>
          )}
          {pendingDeadline && remaining !== null && (
            <ApprovalTimeoutNotice
              remaining={remaining}
              expired={approvalElapsed}
            />
          )}
        </div>
      )}
      {!pending && run.status === "cancelled" && elapsedSeen && (
        <p
          className="nex-agent-approval-timeout nex-agent-approval-timeout-confirmed"
          role="status"
        >
          {t("agent.approvalExpired")}
        </p>
      )}

      {!collapsed && sorted.length > 0 && (
        <div className="nex-agent-run-detail">
          <span className="nex-agent-run-summary">{summaryText}</span>
          <span className="nex-agent-run-meta">
            <span
              className="nex-tag nex-tag-mono"
              title={t("agent.modelTitle")}
            >
              {run.model || "agent"}
            </span>
            <span className="nex-agent-run-id">run {run.run_id}</span>
          </span>
        </div>
      )}

      {sorted.length > 0 && (
        <>
          {/* `hidden` and never a `display` override here: the attribute keeps
              every row mounted (so expanding restores the same objects, the
              same order, and each row's own open state) while taking the
              folded sequence out of sight and out of the a11y tree. The id
              is what the line's `aria-controls` points at, and is qualified
              by run id so two runs in one thread never collide. */}
          <div
            className="nex-agent-timeline"
            id={timelineId}
            hidden={collapsed}
          >
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
                  (isLast && isActive && !protectedRow
                    ? " nex-agent-step-slot-current"
                    : "") +
                  // The same predicate the fold gate and the fade ladder read:
                  // a denied or cancelled call keeps the failure tone too.
                  (failed ? " nex-agent-step-slot-failed" : "");
                const fade =
                  isLast || protectedRow ? "" : fadeClass(all.length - 1 - idx);
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
                    <div
                      key={`${run.run_id}-${item.seq}`}
                      className={slotClass}
                    >
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
                    <div
                      key={`${run.run_id}-${item.steps[0].seq}`}
                      className={slotClass}
                    >
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
        </>
      )}
    </section>
  );
}

export default memo(AgentRunSteps);
