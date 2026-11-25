//! Composer-adjacent Plan decision prompt (`M09` / `P04`).
//! Shown when the active conversation has a completed Plan run awaiting the
//! user's decision (`"Start implementation?"` -> `Yes` / `No` / `Custom reply`).

import { useEffect, useState } from "react";

import { formatIpcError, type PlanDecisionKind } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import { Button } from "./ui/button";

export interface PlanDecisionPanelProps {
  runId: number;
  disabled?: boolean;
  onDecide: (
    runId: number,
    decision: PlanDecisionKind,
    reply?: string
  ) => Promise<void> | void;
}

export default function PlanDecisionPanel({
  runId,
  disabled = false,
  onDecide,
}: PlanDecisionPanelProps) {
  const { t } = useStrings();
  const [replyOpen, setReplyOpen] = useState(false);
  const [replyText, setReplyText] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    setReplyOpen(false);
    setReplyText("");
    setValidationError(null);
    setSubmitting(false);
  }, [runId]);

  const busy = disabled || submitting;

  const submitDecision = async (decision: PlanDecisionKind, reply?: string) => {
    if (busy) return;
    setValidationError(null);
    setSubmitting(true);
    try {
      await onDecide(runId, decision, reply);
    } catch (cause) {
      setValidationError(formatIpcError(cause));
    } finally {
      setSubmitting(false);
    }
  };

  const submitCustomReply = async () => {
    if (busy) return;
    const trimmed = replyText.trim();
    if (!trimmed) {
      setValidationError(t("plan.emptyReplyError"));
      return;
    }
    await submitDecision("custom_reply", trimmed);
  };

  return (
    <section
      className="nex-plan-decision-panel"
      aria-label={t("plan.promptTitle")}
      data-run-id={runId}
    >
      <div className="nex-plan-decision-header">
        <span className="nex-plan-decision-title">{t("plan.promptTitle")}</span>
        <div className="nex-plan-decision-actions">
          <Button
            type="button"
            size="sm"
            variant="default"
            disabled={busy}
            onClick={() => void submitDecision("accepted")}
          >
            {submitting ? t("plan.submitting") : t("plan.yes")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={busy}
            onClick={() => void submitDecision("rejected")}
          >
            {t("plan.no")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy}
            aria-expanded={replyOpen}
            onClick={() => {
              setValidationError(null);
              setReplyOpen((open) => !open);
            }}
          >
            {t("plan.customReply")}
          </Button>
        </div>
      </div>

      {replyOpen && (
        <form
          className="nex-plan-decision-reply"
          onSubmit={(e) => {
            e.preventDefault();
            void submitCustomReply();
          }}
        >
          <textarea
            className="nex-plan-decision-textarea"
            aria-label={t("plan.customReply")}
            rows={2}
            value={replyText}
            disabled={busy}
            placeholder={t("plan.customPlaceholder")}
            onChange={(e) => {
              setValidationError(null);
              setReplyText(e.target.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submitCustomReply();
              }
            }}
          />
          <div className="nex-plan-decision-reply-actions">
            <Button
              type="button"
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => {
                setValidationError(null);
                setReplyOpen(false);
              }}
            >
              {t("plan.cancelReply")}
            </Button>
            <Button type="submit" size="sm" variant="default" disabled={busy}>
              {submitting ? t("plan.submitting") : t("plan.sendReply")}
            </Button>
          </div>
        </form>
      )}

      {validationError && (
        <p className="nex-plan-decision-error" role="alert">
          {validationError}
        </p>
      )}
    </section>
  );
}
