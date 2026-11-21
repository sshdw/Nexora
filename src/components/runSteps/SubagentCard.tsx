//! Expandable visible subagent card rendered inside `AgentRunSteps` (`M19`).

import { memo, useId, useState } from "react";

import type { SubagentView } from "../../lib/useAgentRun";
import { tr } from "../../lib/strings";
import { useStrings } from "../../lib/useLocale";
import M3Button from "../M3Button";

export interface SubagentCardProps {
  runId: number;
  subagent: SubagentView;
  onKillSubagent?: (runId: number, subagentId: string) => void;
}

function subagentStatusKey(status: string):
  | "agent.subagentStatusRunning"
  | "agent.subagentStatusCompleted"
  | "agent.subagentStatusFailed"
  | "agent.subagentStatusCancelled"
  | "agent.subagentStatusTimedOut" {
  switch (status) {
    case "running":
      return "agent.subagentStatusRunning";
    case "completed":
      return "agent.subagentStatusCompleted";
    case "cancelled":
      return "agent.subagentStatusCancelled";
    case "timed_out":
      return "agent.subagentStatusTimedOut";
    default:
      return "agent.subagentStatusFailed";
  }
}

function subagentAccessKey(access: string):
  | "agent.subagentAccessReadOnly"
  | "agent.subagentAccessWorkspaceWrite"
  | "agent.subagentAccessFullAccess" {
  switch (access) {
    case "workspace_write":
      return "agent.subagentAccessWorkspaceWrite";
    case "full_access":
      return "agent.subagentAccessFullAccess";
    default:
      return "agent.subagentAccessReadOnly";
  }
}

function SubagentCard({ runId, subagent, onKillSubagent }: SubagentCardProps) {
  const { locale, t } = useStrings();
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  const isRunning = subagent.status === "running";
  const modelLabel = subagent.effort
    ? `${subagent.model} · ${subagent.effort}`
    : subagent.model;

  return (
    <article
      className="nex-subagent-card"
      data-subagent-id={subagent.subagent_id}
      data-subagent-status={subagent.status}
    >
      <div className="nex-subagent-header">
        <button
          type="button"
          className="nex-subagent-toggle"
          aria-expanded={expanded}
          aria-controls={bodyId}
          onClick={() => setExpanded((prev) => !prev)}
        >
          <span
            className="nex-subagent-chevron"
            aria-hidden="true"
            data-expanded={expanded ? "true" : "false"}
          >
            {expanded ? "▾" : "▸"}
          </span>
          <span className="nex-subagent-title">{subagent.title}</span>
          <span className="nex-tag">
            {t(subagentStatusKey(subagent.status))}
          </span>
          <span className="nex-tag nex-tag-mono">{modelLabel}</span>
          <span className="nex-tag">
            {t(subagentAccessKey(subagent.access))}
          </span>
          {subagent.worktree_isolated && (
            <span className="nex-tag nex-tag-mono">
              {t("agent.subagentWorktree")}
            </span>
          )}
          {subagent.patch_applied && (
            <span className="nex-tag">{t("agent.subagentPatchApplied")}</span>
          )}
          {subagent.patch_conflict && (
            <span className="nex-tag nex-subagent-badge-conflict">
              {t("agent.subagentPatchConflict")}
            </span>
          )}
          <span className="nex-subagent-turns">
            {tr(locale, "agent.subagentStepsCount", {
              taken: subagent.steps_taken,
              max: subagent.max_turns,
            })}
          </span>
        </button>

        {isRunning && onKillSubagent && (
          <div className="nex-subagent-actions">
            <M3Button
              variant="quiet"
              size="sm"
              aria-label={tr(locale, "agent.subagentStopAria", {
                title: subagent.title,
              })}
              onClick={() => onKillSubagent(runId, subagent.subagent_id)}
            >
              {t("agent.subagentStop")}
            </M3Button>
          </div>
        )}
      </div>

      {expanded && (
        <div id={bodyId} className="nex-subagent-body">
          {subagent.steps.length > 0 ? (
            <ol className="nex-subagent-steps">
              {subagent.steps.map((step) => (
                <li key={step.seq} className="nex-subagent-step">
                  <div className="nex-subagent-step-row">
                    <span className="nex-subagent-step-kind">
                      {step.tool_name ?? t("agent.kindModelTurn")}
                    </span>
                    {step.target && (
                      <code className="nex-subagent-step-target">
                        {step.target}
                      </code>
                    )}
                    {step.status && (
                      <span className="nex-subagent-step-status">
                        {step.status}
                      </span>
                    )}
                  </div>
                  {step.observation && (
                    <p className="nex-subagent-step-obs">{step.observation}</p>
                  )}
                </li>
              ))}
            </ol>
          ) : (
            <p className="nex-subagent-empty">{t("agent.subagentNoSteps")}</p>
          )}

          {subagent.result_summary && (
            <div className="nex-subagent-summary">
              <div className="nex-subagent-section-label">
                {t("agent.subagentResultHeading")}
              </div>
              <p className="nex-subagent-summary-text">
                {subagent.result_summary}
              </p>
            </div>
          )}

          {subagent.error && (
            <div className="nex-subagent-error" role="status">
              <div className="nex-subagent-section-label">
                {t("agent.subagentErrorHeading")}
              </div>
              <p className="nex-subagent-error-text">{subagent.error}</p>
            </div>
          )}
        </div>
      )}
    </article>
  );
}

export default memo(SubagentCard);
