//! Repeated-calls group row (AC-208 split, per the review §3.1 plan — moved
//! verbatim from `AgentRunSteps.tsx`): the group header carries the SAME
//! action/target line as a single row plus a count; expanding restores the
//! same `StepSection` rows in the same order behind `hidden`.

import { memo, useState } from "react";

import type { AgentStepView } from "../../lib/useAgentRun";
import { tp } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";
import { isProtectedRow, stepTarget } from "./labels";
import type { StepGroup } from "./grouping";
import { StepLine, StepSection } from "./StepSection";

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
export const StepGroupSection = memo(function StepGroupSection({
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
  const head: AgentStepView = group.steps[0];
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
