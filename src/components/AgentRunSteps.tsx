//! Steps accordion for one agent run (Task 5.1).
//!
//! Presentational: kind-grouped collapsible sections (`model_turn` |
//! `tool_call` | `approval`), run status pill, live append, bare inline
//! Approve/Deny on `ApprovalRequested`, Continue on `BudgetExhausted`, and
//! cancel affordance. Chronological placement is owned by the thread view.

import { useState } from "react";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";

export interface AgentRunStepsProps {
  run: AgentRunView;
  onResolveApproval: (callId: string, approved: boolean) => void;
  onCancel: () => void;
  onContinue: (extraSteps: number) => void;
}

function statusClass(status: string): string {
  switch (status) {
    case "running":
      return "nex-tag nex-agent-status-running";
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

function statusLabel(status: string): string {
  switch (status) {
    case "running":
      return "Running";
    case "completed":
      return "Completed";
    case "cancelled":
      return "Cancelled";
    case "budget_exhausted":
      return "Budget exhausted";
    case "spend_limit_exceeded":
      return "Spend limit exceeded";
    case "error":
      return "Error";
    default:
      return status;
  }
}

function kindLabel(step: AgentStepView): string {
  if (step.kind === "model_turn") return "Model turn";
  if (step.kind === "tool_call") return step.tool_name ?? "Tool call";
  if (step.kind === "approval") return `Approval · ${step.tool_name ?? ""}`.trim();
  return step.kind;
}

function StepSection({ step, defaultOpen }: { step: AgentStepView; defaultOpen: boolean }) {
  const [open, setOpen] = useState<boolean>(defaultOpen);
  const isLong = (step.observation?.length ?? 0) > 2000;
  const observation = step.observation ?? "";
  const displayObservation = isLong && !open ? observation.slice(0, 2000) + "…" : observation;

  return (
    <div className="nex-agent-step">
      <button
        type="button"
        className="nex-agent-step-header"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="nex-agent-step-seq">#{step.seq}</span>
        <span className="nex-agent-step-kind">{kindLabel(step)}</span>
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
          {step.arguments && (
            <pre className="nex-agent-step-args nex-tag-mono" aria-label="Tool arguments">
              {step.arguments}
            </pre>
          )}
          {displayObservation && <pre className="nex-agent-step-observation">{displayObservation}</pre>}
          {isLong && (
            <button type="button" className="nex-btn nex-btn-ghost nex-btn-sm" onClick={() => setOpen((v) => !v)}>
              {open ? "Show less" : "Show more"}
            </button>
          )}
          {step.kind === "model_turn" && !displayObservation && (
            <span className="nex-agent-step-empty">No content</span>
          )}
        </div>
      )}
    </div>
  );
}

export default function AgentRunSteps({ run, onResolveApproval, onCancel, onContinue }: AgentRunStepsProps) {
  // Group steps by kind: model_turn | tool_call | approval — each collapsible.
  // Defaulting: collapsed for model_turn (final answer already in thread),
  // expanded for latest step while running, collapsed for older tool_call steps.
  const sorted = [...run.steps].sort((a, b) => a.seq - b.seq);
  const isRunning = run.status === "running";
  const pending = run.pending_approval;

  return (
    <section className="nex-agent-run" aria-label={`Agent run ${run.run_id}`}>
      <header className="nex-agent-run-header">
        <span className={statusClass(run.status)} role="status">
          {isRunning && <span className="nex-spinner nex-agent-spinner" aria-hidden="true" />}
          {statusLabel(run.status)}
        </span>
        <span className="nex-agent-run-meta">
          <span className="nex-tag nex-tag-mono" title="Model">
            {run.model || "agent"}
          </span>
          <span className="nex-agent-run-id">run {run.run_id}</span>
        </span>
        <span className="nex-agent-run-actions">
          {isRunning && (
            <button type="button" className="nex-btn nex-btn-ghost nex-btn-sm" onClick={onCancel}>
              Cancel
            </button>
          )}
          {run.status === "budget_exhausted" && (
            <button type="button" className="nex-btn nex-btn-tonal nex-btn-sm" onClick={() => onContinue(10)}>
              Continue
            </button>
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
        </span>
      </header>

      {pending && (
        <div className="nex-agent-approval nex-fade-in" role="region" aria-label="Approval required">
          <div className="nex-agent-approval-text">
            <strong>{pending.name}</strong> requested approval
            <pre className="nex-agent-step-args nex-tag-mono">{pending.arguments}</pre>
          </div>
          <div className="nex-agent-approval-actions">
            <button
              type="button"
              className="nex-btn nex-btn-primary nex-btn-sm"
              onClick={() => onResolveApproval(pending.call_id, true)}
            >
              Approve
            </button>
            <button
              type="button"
              className="nex-btn nex-btn-outline nex-btn-sm"
              onClick={() => onResolveApproval(pending.call_id, false)}
            >
              Deny
            </button>
          </div>
        </div>
      )}

      {sorted.length === 0 ? (
        <p className="nex-agent-empty">{isRunning ? "Waiting for steps…" : "No steps recorded."}</p>
      ) : (
        <div className="nex-agent-steps">
          {sorted.map((step, idx) => {
            const isLast = idx === sorted.length - 1;
            // Latest step expanded while running, older collapsed.
            const defaultOpen = isRunning ? isLast : false;
            return <StepSection key={`${run.run_id}-${step.seq}`} step={step} defaultOpen={defaultOpen} />;
          })}
        </div>
      )}
    </section>
  );
}
