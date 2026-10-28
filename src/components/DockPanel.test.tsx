//! Dock panel shell (R4-01 / D-06) — the surface every Dock destination opens.
//!
//! jsdom can prove the accessibility wiring and the three dismissal gestures
//! the register names (re-click the Dock button is the Dock's own job, click
//! outside, Escape) plus the close button. Focus is the part most likely to
//! rot silently: opening must land in the panel BODY (never the header ×),
//! and closing must return the ring to the Dock trigger that opened it.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../lib/useLocale";
import DockPanel, { type DockPanelProps } from "./DockPanel";

afterEach(cleanup);

function renderPanel(overrides: Partial<DockPanelProps> = {}) {
  const props: DockPanelProps = {
    id: "sessions",
    title: "Sessions",
    onClose: vi.fn(),
    children: <p>Panel body</p>,
    ...overrides,
  };
  return {
    ...render(
      <LocaleProvider>
        <DockPanel {...props} />
      </LocaleProvider>,
    ),
    props,
  };
}

describe("dock panel — surface", () => {
  it("renders a non-modal dialog named by its own title", () => {
    renderPanel({ meta: "12" });
    const dialog = screen.getByRole("dialog", { name: "Sessions" });
    expect(dialog).toHaveAttribute("aria-modal", "false");
    expect(dialog).toHaveClass("nex-dock-panel");
    expect(screen.getByText("12")).toHaveClass("nex-dock-panel-meta");
    expect(screen.getByText("Panel body")).toBeInTheDocument();
  });

  it("omits the meta slot when there is no summary", () => {
    const { container } = renderPanel();
    expect(container.querySelector(".nex-dock-panel-meta")).toBeNull();
  });

  it("closes from the header button, which carries its name", () => {
    const { props } = renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });
});

describe("dock panel — dismissal", () => {
  it("closes on Escape from anywhere", () => {
    const { props } = renderPanel();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("leaves Escape to a modal dialog on top", () => {
    // Settings / the command palette own Escape first. The guard is a live
    // DOM query, not a prop, because those overlays mount elsewhere.
    const modal = document.createElement("div");
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    document.body.append(modal);
    try {
      const { props } = renderPanel();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(props.onClose).not.toHaveBeenCalled();
    } finally {
      modal.remove();
    }
  });

  it("closes on a pointer press outside the card", () => {
    const { props } = renderPanel();
    fireEvent.pointerDown(document.body);
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("stays open for a press inside the card or on the Dock itself", () => {
    const { props } = renderPanel();
    fireEvent.pointerDown(screen.getByText("Panel body"));
    expect(props.onClose).not.toHaveBeenCalled();

    // The Dock's own toggle closes this panel; closing here as well would
    // close and reopen it in one click.
    const dock = document.createElement("nav");
    dock.className = "nex-dock";
    const trigger = document.createElement("button");
    dock.append(trigger);
    document.body.append(dock);
    try {
      fireEvent.pointerDown(trigger);
      expect(props.onClose).not.toHaveBeenCalled();
    } finally {
      dock.remove();
    }
  });
});

describe("dock panel — focus", () => {
  it("lands on the body's autofocus target, never the header close", () => {
    renderPanel({
      children: (
        <>
          <input data-dock-panel-autofocus aria-label="Filter sessions" />
          <button type="button">Second</button>
        </>
      ),
    });
    expect(screen.getByRole("textbox", { name: "Filter sessions" })).toHaveFocus();
    expect(screen.getByRole("button", { name: "Close" })).not.toHaveFocus();
  });

  it("falls back to the body's first control when nothing is marked", () => {
    renderPanel({
      children: (
        <>
          <button type="button">First action</button>
          <button type="button">Second action</button>
        </>
      ),
    });
    expect(screen.getByRole("button", { name: "First action" })).toHaveFocus();
  });

  it("focuses the panel itself when it has no control at all", () => {
    renderPanel();
    expect(screen.getByRole("dialog", { name: "Sessions" })).toHaveFocus();
  });

  it("returns focus to the Dock trigger that opened it", () => {
    const trigger = document.createElement("button");
    trigger.setAttribute("data-dock-trigger", "sessions");
    document.body.append(trigger);
    try {
      const { unmount } = renderPanel();
      unmount();
      expect(trigger).toHaveFocus();
    } finally {
      trigger.remove();
    }
  });
});
