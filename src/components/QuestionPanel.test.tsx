import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PendingQuestionView } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import QuestionPanel from "./QuestionPanel";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function makeQuestion(
  overrides: Partial<PendingQuestionView> = {}
): PendingQuestionView {
  return {
    question_id: 11,
    run_id: 42,
    conversation_id: 7,
    call_id: "call_q11",
    question: "Which test framework should we run?",
    options: ["Vitest", "Playwright"],
    multi_select: false,
    allow_free_text: true,
    created_at: 1_700_000_000,
    ...overrides,
  };
}

describe("QuestionPanel (Stage 6 PR 6A)", () => {
  it("validates non-empty submission and submits single-select + free text", async () => {
    const onSubmit = vi.fn(async () => {});
    render(
      <LocaleProvider>
        <QuestionPanel question={makeQuestion()} onSubmit={onSubmit} />
      </LocaleProvider>
    );

    fireEvent.click(screen.getByRole("button", { name: "Submit answer" }));
    expect(
      screen.getByText(/Select an option or enter a response before submitting/)
    ).toBeInTheDocument();
    expect(onSubmit).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("radio", { name: "Vitest" }));
    fireEvent.change(screen.getByLabelText("Additional details"), {
      target: { value: "run unit tests first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit answer" }));

    await waitFor(() => {
      expect(onSubmit).toHaveBeenCalledWith(
        ["Vitest"],
        "run unit tests first"
      );
    });
  });

  it("supports multi-select checkboxes and resets when question_id changes", async () => {
    const onSubmit = vi.fn(async () => {});
    const { rerender } = render(
      <LocaleProvider>
        <QuestionPanel
          question={makeQuestion({ multi_select: true, allow_free_text: false })}
          onSubmit={onSubmit}
        />
      </LocaleProvider>
    );

    expect(screen.queryByLabelText("Additional details")).toBeNull();

    fireEvent.click(screen.getByRole("checkbox", { name: "Vitest" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Playwright" }));
    fireEvent.click(screen.getByRole("button", { name: "Submit answer" }));

    await waitFor(() => {
      expect(onSubmit).toHaveBeenCalledWith(["Vitest", "Playwright"], undefined);
    });

    rerender(
      <LocaleProvider>
        <QuestionPanel
          question={makeQuestion({
            question_id: 12,
            question: "Confirm branch?",
            options: ["main", "dev"],
            multi_select: false,
          })}
          onSubmit={onSubmit}
        />
      </LocaleProvider>
    );

    expect(screen.getByText("Confirm branch?")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "main" })).not.toBeChecked();
  });
});
