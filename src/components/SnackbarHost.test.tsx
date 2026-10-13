//! Host behavior pins for `SnackbarProvider` (NEX-UX-003, NEX-UI-001).
//!
//! The provider mounts the single fixed undo-capable region: `showSnack`
//! renders the primitive with its message + single action, the action press
//! runs its callback then dismisses, and the primitive's own timer
//! auto-dismisses. Fake timers bound the waits — presence/absence only, no
//! elapsed-time assertions (policy).

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import { SnackbarProvider, useSnackbar } from "./SnackbarHost";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function ShowButton({ onAction }: { onAction: () => void }) {
  const { showSnack } = useSnackbar();
  return (
    <button
      type="button"
      onClick={() =>
        showSnack("“Report” deleted.", {
          actionLabel: "Undo",
          onAction,
        })
      }
    >
      show
    </button>
  );
}

function renderHost(onAction: () => void = vi.fn()) {
  return render(
    <LocaleProvider>
      <SnackbarProvider>
        <ShowButton onAction={onAction} />
      </SnackbarProvider>
    </LocaleProvider>,
  );
}

describe("SnackbarProvider", () => {
  it("renders no snackbar until showSnack is called", () => {
    renderHost();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("shows the message with its single action on showSnack", () => {
    renderHost();
    fireEvent.click(screen.getByRole("button", { name: "show" }));
    expect(screen.getByRole("status")).toHaveTextContent("“Report” deleted.");
    expect(
      screen.getByRole("button", { name: "Undo" }),
    ).toBeInTheDocument();
  });

  it("runs the action then dismisses on action press", () => {
    const onAction = vi.fn();
    renderHost(onAction);
    fireEvent.click(screen.getByRole("button", { name: "show" }));
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(onAction).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("auto-dismisses when the primitive timer fires", () => {
    renderHost();
    fireEvent.click(screen.getByRole("button", { name: "show" }));
    expect(screen.getByRole("status")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(8000);
    });
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("a second showSnack replaces the first", () => {
    function ShowTwo() {
      const { showSnack } = useSnackbar();
      return (
        <>
          <button type="button" onClick={() => showSnack("first")}>
            first-btn
          </button>
          <button type="button" onClick={() => showSnack("second")}>
            second-btn
          </button>
        </>
      );
    }
    render(
      <LocaleProvider>
        <SnackbarProvider>
          <ShowTwo />
        </SnackbarProvider>
      </LocaleProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "first-btn" }));
    expect(screen.getByRole("status")).toHaveTextContent("first");
    fireEvent.click(screen.getByRole("button", { name: "second-btn" }));
    expect(screen.getByRole("status")).toHaveTextContent("second");
  });

  it("throws outside the provider", () => {
    function Lonely() {
      useSnackbar();
      return null;
    }
    expect(() => render(<Lonely />)).toThrow(
      "useSnackbar must be used inside SnackbarProvider",
    );
  });
});
