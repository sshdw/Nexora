//! Composer-adjacent clarifying question prompt (`ask_user_question`, Stage 6 PR 6A).
//!
//! Renders single-select or multi-select options plus optional free-text input
//! for the currently parked run while leaving other panes and tabs unblocked.

import { useEffect, useState } from "react";

import type { PendingQuestionView } from "../lib/tauri";
import { useStrings } from "../lib/useLocale";
import { Button } from "./ui/button";

export interface QuestionPanelProps {
  question: PendingQuestionView;
  onSubmit: (
    selectedOptions: string[],
    freeText?: string
  ) => Promise<void> | void;
}

export default function QuestionPanel({
  question,
  onSubmit,
}: QuestionPanelProps) {
  const { t } = useStrings();
  const [selected, setSelected] = useState<string[]>([]);
  const [freeText, setFreeText] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // Reset local form state whenever a new question id arrives.
  useEffect(() => {
    setSelected([]);
    setFreeText("");
    setValidationError(null);
    setSubmitting(false);
  }, [question.question_id]);

  const toggleOption = (option: string) => {
    setValidationError(null);
    if (question.multi_select) {
      setSelected((prev) =>
        prev.includes(option)
          ? prev.filter((item) => item !== option)
          : [...prev, option]
      );
    } else {
      setSelected((prev) => (prev[0] === option ? [] : [option]));
    }
  };

  const handleSubmit = async (event?: React.FormEvent) => {
    event?.preventDefault();
    if (submitting) return;
    const trimmedFree = freeText.trim();
    if (selected.length === 0 && trimmedFree === "") {
      setValidationError(t("agent.questionValidationEmpty"));
      return;
    }
    setValidationError(null);
    setSubmitting(true);
    try {
      await onSubmit(selected, trimmedFree || undefined);
    } catch (err) {
      const msg =
        err instanceof Error
          ? err.message
          : typeof err === "object" && err !== null && "message" in err
            ? String((err as { message: unknown }).message)
            : String(err);
      setValidationError(msg);
      setSubmitting(false);
    }
  };

  return (
    <section
      className="nex-question-panel nex-fade-in"
      aria-label={t("agent.questionTitle")}
      data-question-id={question.question_id}
    >
      <form className="nex-question-form" onSubmit={(e) => void handleSubmit(e)}>
        <div className="nex-question-header">
          <span className="nex-question-badge">{t("agent.questionTitle")}</span>
          {question.options.length > 0 && (
            <span className="nex-question-hint">
              {question.multi_select
                ? t("agent.questionMultiHint")
                : t("agent.questionSingleHint")}
            </span>
          )}
        </div>

        <p className="nex-question-prompt">{question.question}</p>

        {question.options.length > 0 && (
          <div
            className="nex-question-options"
            role={question.multi_select ? "group" : "radiogroup"}
            aria-label={question.question}
          >
            {question.options.map((option) => {
              const checked = selected.includes(option);
              return (
                <label
                  key={option}
                  className={
                    "nex-question-option" + (checked ? " is-selected" : "")
                  }
                >
                  <input
                    type={question.multi_select ? "checkbox" : "radio"}
                    name={`question-${question.question_id}`}
                    checked={checked}
                    disabled={submitting}
                    onChange={() => toggleOption(option)}
                  />
                  <span className="nex-question-option-label">{option}</span>
                </label>
              );
            })}
          </div>
        )}

        {question.allow_free_text && (
          <div className="nex-question-freetext">
            <label
              htmlFor={`question-free-${question.question_id}`}
              className="nex-question-freetext-label"
            >
              {t("agent.questionFreeTextLabel")}
            </label>
            <textarea
              id={`question-free-${question.question_id}`}
              className="nex-question-textarea"
              rows={2}
              value={freeText}
              disabled={submitting}
              placeholder={t("agent.questionFreeTextPlaceholder")}
              onChange={(e) => {
                setValidationError(null);
                setFreeText(e.target.value);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void handleSubmit();
                }
              }}
            />
          </div>
        )}

        {validationError && (
          <p className="nex-question-error" role="alert">
            {validationError}
          </p>
        )}

        <div className="nex-question-actions">
          <Button
            type="submit"
            size="sm"
            variant="default"
            disabled={submitting}
          >
            {submitting ? t("agent.questionSubmitting") : t("agent.questionSubmit")}
          </Button>
        </div>
      </form>
    </section>
  );
}
