//! Steps accordion for one agent run (Task 5.1 + 5.2).
//!
//! Presentational: kind-grouped collapsible sections (`model_turn` |
//! `tool_call` | `approval`), run status pill, live append, bare inline
//! Approve/Deny on `ApprovalRequested`, Continue on `BudgetExhausted`, pause/
//! resume controls, and per-tool body renderers — terminal for
//! `execute_command` (command line + stdout/stderr with distinct separator) and
//! diff viewer for `write_file` (headers, hunks, +/- gutters, mono,
//! collapsible). Chronological placement is owned by the thread view.

import { useState } from "react";

import type { AgentRunView, AgentStepView } from "../lib/useAgentRun";

export interface AgentRunStepsProps {
  run: AgentRunView;
  onResolveApproval: (callId: string, approved: boolean) => void;
  onCancel: () => void;
  onContinue: (extraSteps: number) => void;
  onPause?: () => void;
  onResume?: () => void;
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

function statusLabel(status: string): string {
  switch (status) {
    case "running":
      return "Running";
    case "paused":
      return "Paused";
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

function parseToolArgs(json: string | null): Record<string, unknown> | null {
  if (!json) return null;
  try {
    return JSON.parse(json) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function TerminalView({ step }: { step: AgentStepView }) {
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
    <div className="nex-agent-terminal" aria-label="Terminal output">
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
        {!stdout && !hasStderr && <span className="nex-agent-step-empty">No output</span>}
      </div>
    </div>
  );
}

function DiffView({ observation }: { observation: string | null }) {
  if (!observation) return <span className="nex-agent-step-empty">No changes</span>;
  const lines = observation.split("\n");
  // Remove trailing empty line from final split if observation ends with newline
  // Keep it as is for rendering; filter will handle.
  return (
    <div className="nex-agent-diff" aria-label="File diff">
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
            ariaLabel = "removed file header";
          } else if (line.startsWith("+++ ")) {
            cls += " nex-agent-diff-header";
            gutter = "+";
            ariaLabel = "added file header";
          } else if (line.startsWith("@@")) {
            cls += " nex-agent-diff-hunk";
            gutter = "@";
            ariaLabel = "hunk header";
          } else if (line.startsWith("+") && !line.startsWith("+++")) {
            cls += " nex-agent-diff-added";
            gutter = "+";
            content = line.slice(1);
            ariaLabel = "added";
          } else if (line.startsWith("-") && !line.startsWith("---")) {
            cls += " nex-agent-diff-removed";
            gutter = "-";
            content = line.slice(1);
            ariaLabel = "removed";
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
            <div key={idx} className={cls} aria-label={ariaLabel}>
              <span className="nex-agent-diff-gutter" aria-hidden="true">
                {gutter}
              </span>
              <span className="nex-agent-diff-content">{content}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function StepSection({ step, defaultOpen }: { step: AgentStepView; defaultOpen: boolean }) {
  const [open, setOpen] = useState<boolean>(defaultOpen);
  const isLong = (step.observation?.length ?? 0) > 2000;
  const observation = step.observation ?? "";
  const displayObservation = isLong && !open ? observation.slice(0, 2000) + "…" : observation;
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
          {isTerminal ? (
            <TerminalView step={{ ...step, observation: displayObservation }} />
          ) : isDiff ? (
            <DiffView observation={displayObservation} />
          ) : (
            <>
              {step.arguments && (
                <pre className="nex-agent-step-args nex-tag-mono" aria-label="Tool arguments">
                  {step.arguments}
                </pre>
              )}
              {displayObservation && <pre className="nex-agent-step-observation">{displayObservation}</pre>}
              {step.kind === "model_turn" && !displayObservation && (
                <span className="nex-agent-step-empty">No content</span>
              )}
            </>
          )}
          {isLong && (
            <button type="button" className="nex-btn nex-btn-ghost nex-btn-sm" onClick={() => setOpen((v) => !v)}>
              {open ? "Show less" : "Show more"}
            </button>
          )}
          {/* For terminal/diff views that already handled observation, still show args if needed? */}
          {isTerminal && step.arguments && isLong && null}
        </div>
      )}
    </div>
  );
}

export default function AgentRunSteps({
  run,
  onResolveApproval,
  onCancel,
  onContinue,
  onPause,
  onResume,
}: AgentRunStepsProps) {
  const sorted = [...run.steps].sort((a, b) => a.seq - b.seq);
  const isRunning = run.status === "running";
  const isPaused = run.status === "paused";
  const isActive = isRunning || isPaused;
  const pending = run.pending_approval;

  return (
    <section className="nex-agent-run" aria-label={`Agent run ${run.run_id}`}>
      <header className="nex-agent-run-header">
        <span className={statusClass(run.status)} role="status">
          {isRunning && <span className="nex-spinner nex-agent-spinner" aria-hidden="true" />}
          {isPaused && <span className="nex-agent-paused-dot" aria-hidden="true" />}
          {statusLabel(run.status)}
        </span>
        <span className="nex-agent-run-meta">
          <span className="nex-tag nex-tag-mono" title="Model">
            {run.model || "agent"}
          </span>
          <span className="nex-agent-run-id">run {run.run_id}</span>
        </span>
        <span className="nex-agent-run-actions">
          {isRunning && onPause && (
            <button type="button" className="nex-btn nex-btn-ghost nex-btn-sm" onClick={onPause}>
              Pause
            </button>
          )}
          {isPaused && onResume && (
            <button type="button" className="nex-btn nex-btn-tonal nex-btn-sm" onClick={onResume}>
              Resume
            </button>
          )}
          {isActive && (
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
        <p className="nex-agent-empty">{isActive ? "Waiting for steps…" : "No steps recorded."}</p>
      ) : (
        <div className="nex-agent-steps">
          {sorted.map((step, idx) => {
            const isLast = idx === sorted.length - 1;
            const defaultOpen = isActive ? isLast : false;
            return <StepSection key={`${run.run_id}-${step.seq}`} step={step} defaultOpen={defaultOpen} />;
          })}
        </div>
      )}
    </section>
  );
}
