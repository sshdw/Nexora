import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LOCALE_KEY, LocaleProvider } from "../lib/useLocale";
import PlanDecisionPanel from "./PlanDecisionPanel";

afterEach(() => {
  localStorage.clear();
  cleanup();
  vi.restoreAllMocks();
});

describe("PlanDecisionPanel (M09 / P04)", () => {
  it("renders Start implementation? prompt with Yes, No, and Custom reply in EN and RU", () => {
    const onDecide = vi.fn();
    render(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} onDecide={onDecide} />
      </LocaleProvider>
    );

    expect(screen.getByText("Start implementation?")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Yes" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "No" })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Custom reply" })
    ).toBeInTheDocument();

    cleanup();
    localStorage.setItem(LOCALE_KEY, "ru");
    render(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} onDecide={onDecide} />
      </LocaleProvider>
    );
    expect(screen.getByText("Начать реализацию?")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Да" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Нет" })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Свой ответ" })
    ).toBeInTheDocument();
  });

  it("submits accepted on Yes and rejected on No", async () => {
    const onDecide = vi.fn(async () => {});
    render(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} onDecide={onDecide} />
      </LocaleProvider>
    );

    fireEvent.click(screen.getByRole("button", { name: "Yes" }));
    await waitFor(() => {
      expect(onDecide).toHaveBeenCalledWith(42, "accepted", undefined);
    });

    fireEvent.click(screen.getByRole("button", { name: "No" }));
    await waitFor(() => {
      expect(onDecide).toHaveBeenCalledWith(42, "rejected", undefined);
    });
  });

  it("validates non-empty custom reply and forwards trimmed instructions", async () => {
    const onDecide = vi.fn(async () => {});
    render(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} onDecide={onDecide} />
      </LocaleProvider>
    );

    fireEvent.click(screen.getByRole("button", { name: "Custom reply" }));
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(
      screen.getByText("Enter instructions before sending.")
    ).toBeInTheDocument();
    expect(onDecide).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("Custom reply"), {
      target: { value: "  Keep step 1 and skip tests for now  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(onDecide).toHaveBeenCalledWith(
        42,
        "custom_reply",
        "Keep step 1 and skip tests for now"
      );
    });
  });

  it("respects disabled state and resets custom reply drawer when runId changes", () => {
    const onDecide = vi.fn(async () => {});
    const { rerender } = render(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} disabled onDecide={onDecide} />
      </LocaleProvider>
    );

    expect(screen.getByRole("button", { name: "Yes" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "No" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Custom reply" })).toBeDisabled();

    rerender(
      <LocaleProvider>
        <PlanDecisionPanel runId={42} disabled={false} onDecide={onDecide} />
      </LocaleProvider>
    );
    fireEvent.click(screen.getByRole("button", { name: "Custom reply" }));
    expect(screen.getByLabelText("Custom reply")).toBeInTheDocument();

    rerender(
      <LocaleProvider>
        <PlanDecisionPanel runId={43} disabled={false} onDecide={onDecide} />
      </LocaleProvider>
    );
    expect(screen.queryByRole("textbox")).toBeNull();
  });
});
