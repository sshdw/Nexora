//! One step row: the body renderers (terminal, diff), the MAP-row-8 grammar
//! line, and the memoized collapsible section (AC-208 split, per the review
//! §3.1 plan — moved verbatim from `AgentRunSteps.tsx`).

import { memo, useCallback, useState } from "react";

import type { AgentStepView } from "../../lib/useAgentRun";
import { tr, type Locale } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";
import { FILE_TOOLS } from "./grouping";
import {
  diffCounts,
  hasToolVerb,
  parseToolArgs,
  ruPlural,
  splitPathTarget,
  stepTarget,
  toolVerb,
  verifyWord,
} from "./labels";

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
export function StepLine({
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
  // Only genuine file targets get the directory/name split. Commands,
  // queries and unknown-tool names render whole as the file-name slot so a
  // non-path value is never forced through a path split. `identity.text` is
  // the single source of truth for both parts.
  const isFilePath = step.tool_name !== null && FILE_TOOLS.has(step.tool_name);
  const { dir, name } = isFilePath && identity !== null ? splitPathTarget(identity.text) : { dir: "", name: "" };
  return (
    <>
      <span className="nex-agent-step-action">{action}</span>
      {identity !== null && (
        <span className="nex-agent-step-target nex-tag-mono" title={identity.title}>
          {isFilePath ? (
            <>
              <span className="nex-agent-step-target-dir">{dir}</span>
              <span className="nex-agent-step-target-name">{name}</span>
            </>
          ) : (
            <span className="nex-agent-step-target-name">{identity.text}</span>
          )}
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

export const StepSection = memo(function StepSection({
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

/** Re-exported for the folder index: `diffCounts` is part of the original
 * public surface of `AgentRunSteps.tsx` (consumed via the barrel). */
export { diffCounts } from "./labels";
