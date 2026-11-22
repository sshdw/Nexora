//! Pinned approval prompt for a parked agent run (`Allow once`,
//! `Allow for run`, `Deny`) with live deadline countdown.

import { memo, useEffect, useState } from "react";

import type { AgentRunView } from "../../lib/useAgentRun";
import { useStrings } from "../../lib/useLocale";
import Button from "../Button";
import { formatApprovalTarget, formatCountdown, remainingSecs } from "./labels";

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

export interface ApprovalPanelProps {
  run: AgentRunView;
  onResolveApproval: (
    runId: number,
    callId: string,
    approved: boolean,
    scope?: "single" | "group"
  ) => void;
}

function ApprovalPanel({ run, onResolveApproval }: ApprovalPanelProps) {
  const { t } = useStrings();
  const pending = run.status === "interrupted" ? null : run.pending_approval;
  const pendingDeadline = pending && pending.timeout_secs > 0 ? pending : null;
  const pendingId = pending?.call_id ?? null;

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
  const underMinute = remaining !== null && remaining <= 60;

  useEffect(() => {
    if (!pendingDeadline || reduceMotion) return;
    const intervalMs = underMinute ? 1000 : 30000;
    const id = window.setInterval(() => setNow(Date.now() / 1000), intervalMs);
    return () => window.clearInterval(id);
  }, [pendingDeadline, pendingId, reduceMotion, underMinute]);

  useEffect(() => {
    if (!pendingDeadline || !reduceMotion) return;
    if (remaining === null || remaining <= 0) return;
    const id = window.setTimeout(
      () => setNow(Date.now() / 1000),
      remaining * 1000
    );
    return () => window.clearTimeout(id);
  }, [pendingDeadline, pendingId, reduceMotion, remaining]);

  const approvalElapsed = remaining !== null && remaining <= 0;
  const [elapsedSeen, setElapsedSeen] = useState(false);
  useEffect(() => {
    if (approvalElapsed) setElapsedSeen(true);
  }, [approvalElapsed]);
  useEffect(() => {
    if (pendingId !== null) setElapsedSeen(false);
  }, [pendingId]);

  if (!pending) {
    if (run.status === "cancelled" && elapsedSeen) {
      return (
        <p
          className="nex-agent-approval-timeout nex-agent-approval-timeout-confirmed"
          role="status"
        >
          {t("agent.approvalExpired")}
        </p>
      );
    }
    return null;
  }

  const targetSummary = formatApprovalTarget(pending.arguments);

  return (
    <div
      className="nex-agent-approval nex-fade-in"
      role="region"
      aria-label={t("agent.approvalTitle")}
    >
      <div className="nex-agent-approval-text">
        <div className="nex-agent-approval-headline">
          <strong>{pending.name}</strong> {t("agent.approvalSuffix")}
        </div>
        {pending.metadata?.shell && (
          <div className="nex-agent-approval-meta">
            <p>{t(`agent.category.${pending.metadata.shell.category}`)}</p>
            <p>
              {pending.metadata.shell.sandbox_posture === "Verified"
                ? t("sandbox.verified")
                : t("agent.shellFallback")}
            </p>
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
          </div>
        )}
        {pending.metadata?.sensitive_write && (
          <p className="nex-agent-approval-meta">{t("agent.sensitiveWrite")}</p>
        )}
        {targetSummary ? (
          <div className="nex-agent-approval-target nex-tag-mono">
            {targetSummary}
          </div>
        ) : pending.arguments ? (
          <pre className="nex-agent-step-args nex-tag-mono">
            {pending.arguments}
          </pre>
        ) : null}
      </div>
      {!approvalElapsed && (
        <div className="nex-agent-approval-actions">
          <Button
            variant="primary"
            size="sm"
            onClick={() =>
              onResolveApproval(run.run_id, pending.call_id, true)
            }
          >
            {t("agent.allowOnce")}
          </Button>
          {pending.name !== "execute_command" &&
            pending.metadata?.allow_for_run && (
              <Button
                variant="quiet"
                size="sm"
                onClick={() =>
                  onResolveApproval(run.run_id, pending.call_id, true, "group")
                }
              >
                {t("agent.allowRun")}
              </Button>
            )}
          <Button
            variant="quiet"
            size="sm"
            onClick={() =>
              onResolveApproval(run.run_id, pending.call_id, false)
            }
          >
            {t("agent.deny")}
          </Button>
        </div>
      )}
      {pendingDeadline && remaining !== null && (
        <ApprovalTimeoutNotice
          remaining={remaining}
          expired={approvalElapsed}
        />
      )}
    </div>
  );
}

export default memo(ApprovalPanel);
