//! Dialog-primitive pins for `ModalShell` (AC-211 minimum set, review 03
//! §6.3 item 4 — the confirm-dialog primitive destructive flows reuse).
//!
//! The module owns only accessibility semantics, so the pins are exactly
//! those: the labelled dialog with initial focus on the first focusable
//! control, Esc and backdrop-click dismissal deferred through the exit
//! window (busy blocks both), Tab/Shift+Tab cycling trapped inside the
//! card, focus restored to the opener on close, background siblings made
//! `inert` while open, and the sheet variant carrying no backdrop click
//! path. No business logic lives here — flows stay with callers.

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import ModalShell from "./Modal";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("ModalShell — dialog semantics", () => {
  it("renders a labelled modal dialog and focuses the first focusable control", () => {
    render(
      <ModalShell title="Delete file?" onClose={() => {}}>
        <button type="button">Cancel</button>
        <button type="button">Delete</button>
      </ModalShell>
    );
    const dialog = screen.getByRole("dialog", { name: "Delete file?" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(screen.getByText("Delete file?")).toBeInTheDocument();
    // Initial focus: the first focusable control in the card.
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Cancel" })
    );
  });

  it("falls back to focusing the card when it has no focusable child", () => {
    render(
      <ModalShell title="Nothing to press" onClose={() => {}}>
        <p>plain text</p>
      </ModalShell>
    );
    const dialog = screen.getByRole("dialog", { name: "Nothing to press" });
    expect(document.activeElement).toBe(dialog);
  });

  it("closes on Escape through the exit window", async () => {
    const onClose = vi.fn();
    render(
      <ModalShell title="Delete file?" onClose={onClose}>
        <button type="button">Cancel</button>
      </ModalShell>
    );
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Delete file?" }), {
      key: "Escape",
    });
    // The shell stays mounted while the exit fade runs; the parent's
    // onClose fires once the window expires.
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("ignores repeated Escape requests while the exit is already running", async () => {
    const onClose = vi.fn();
    render(
      <ModalShell title="Delete file?" onClose={onClose}>
        <button type="button">Cancel</button>
      </ModalShell>
    );
    const dialog = screen.getByRole("dialog", { name: "Delete file?" });
    fireEvent.keyDown(dialog, { key: "Escape" });
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("closes on a backdrop click but not on a click inside the card", async () => {
    const onClose = vi.fn();
    render(
      <ModalShell title="Delete file?" onClose={onClose}>
        <button type="button">Cancel</button>
      </ModalShell>
    );
    // The card stops propagation: pressing inside must not dismiss.
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).not.toHaveBeenCalled();
    // The backdrop (role=presentation wrapper) is the dismiss surface.
    fireEvent.click(screen.getByRole("presentation"));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("blocks dismissal entirely while busy — Esc and backdrop alike", () => {
    const onClose = vi.fn();
    render(
      <ModalShell title="Working…" onClose={onClose} busy>
        <button type="button">Cancel</button>
      </ModalShell>
    );
    // Fake timers so the assertion covers the whole (never-armed) exit
    // window: even after advancing past it, no close may have fired.
    vi.useFakeTimers();
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Working…" }), {
      key: "Escape",
    });
    fireEvent.click(screen.getByRole("presentation"));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("ModalShell — focus trap and restoration", () => {
  it("cycles Tab from the last control back to the first", () => {
    render(
      <ModalShell title="Delete file?" onClose={() => {}}>
        <button type="button">Cancel</button>
        <button type="button">Delete</button>
      </ModalShell>
    );
    const last = screen.getByRole("button", { name: "Delete" });
    last.focus();
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Delete file?" }), {
      key: "Tab",
    });
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Cancel" })
    );
  });

  it("cycles Shift+Tab from the first control back to the last", () => {
    render(
      <ModalShell title="Delete file?" onClose={() => {}}>
        <button type="button">Cancel</button>
        <button type="button">Delete</button>
      </ModalShell>
    );
    // Initial focus already sits on the first control.
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Delete file?" }), {
      key: "Tab",
      shiftKey: true,
    });
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Delete" })
    );
  });

  it("restores focus to the opener after closing", async () => {
    const onClose = vi.fn();
    // The opener is focused BEFORE the shell mounts, so the mount effect
    // captures it as the restore target.
    render(<button type="button">Open</button>);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    const modal = render(
      <ModalShell title="Delete file?" onClose={onClose}>
        <button type="button">Cancel</button>
      </ModalShell>
    );
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Delete file?" }), {
      key: "Escape",
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    // The caller answers onClose by unmounting the shell; the cleanup
    // effect restores focus to the captured opener.
    modal.unmount();
    expect(document.activeElement).toBe(opener);
  });

  it("makes background siblings inert while open and releases them on close", async () => {
    const onClose = vi.fn();
    const modal = render(
      <div>
        <button type="button">Background</button>
        <ModalShell title="Delete file?" onClose={onClose}>
          <button type="button">Cancel</button>
        </ModalShell>
      </div>
    );
    const background = screen.getByRole("button", { name: "Background" });
    expect(background.inert).toBe(true);

    fireEvent.keyDown(screen.getByRole("dialog", { name: "Delete file?" }), {
      key: "Escape",
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    // Unmounting the shell (the caller's response to onClose) releases the
    // inert background.
    modal.unmount();
    expect(background.inert).toBe(false);
  });
});

describe("ModalShell — sheet variant", () => {
  it("closes via Escape but offers no backdrop click path", async () => {
    const onClose = vi.fn();
    render(
      <ModalShell title="Composer options" onClose={onClose} sheet>
        <button type="button">Done</button>
      </ModalShell>
    );
    const presentation = screen.getByRole("presentation");
    // A sheet has no backdrop: clicking outside the card dismisses nothing.
    fireEvent.click(presentation);
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(
      screen.getByRole("dialog", { name: "Composer options" }),
      {
        key: "Escape",
      }
    );
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });
});
