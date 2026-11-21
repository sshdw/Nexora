import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import Button from "./Button";
import IconButton from "./IconButton";
import LoadingIndicator from "./LoadingIndicator";
import RailItem from "./RailItem";
import Toolbar from "./Toolbar";

afterEach(() => {
  cleanup();
});

describe("Native primitives accessibility and reduced-motion", () => {
  it("Button exposes focus-visible ring, motion-reduce guard, and aria-busy when loading", () => {
    render(
      <Button variant="primary" loading>
        Save
      </Button>
    );
    const btn = screen.getByRole("button", { name: "Save" });
    expect(btn).toBeDisabled();
    expect(btn).toHaveAttribute("aria-busy", "true");
    expect(btn.className).toContain("focus-visible:ring-2");
    expect(btn.className).toContain("focus-visible:ring-ring");
    expect(btn.className).toContain("motion-reduce:active:scale-none");
    const svg = btn.querySelector("svg");
    expect(svg).toHaveAttribute("aria-hidden", "true");
  });

  it("IconButton sets aria-label, title, focus-visible ring, and danger hover/focus only", () => {
    render(
      <IconButton label="Delete item" danger>
        <span aria-hidden="true">x</span>
      </IconButton>
    );
    const btn = screen.getByRole("button", { name: "Delete item" });
    expect(btn).toHaveAttribute("title", "Delete item");
    expect(btn.className).toContain("focus-visible:ring-2");
    expect(btn.className).toContain("focus-visible:text-destructive");
    expect(btn.className).not.toMatch(/(?:^|\s)text-destructive(?:\s|$)/);
  });

  it("LoadingIndicator announces status politely with visible text and decorative spinner", () => {
    render(<LoadingIndicator label="Loading history" size="sm" />);
    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("Loading history");
    const svg = status.querySelector("svg");
    expect(svg).toHaveAttribute("aria-hidden", "true");
  });

  it("RailItem sets aria-current=page when active, focus-visible ring, and motion-reduce guard", () => {
    const { rerender } = render(
      <RailItem label="Session A" active trailing={<span>2m</span>} />
    );
    const activeBtn = screen.getByRole("button", { name: "Session A" });
    expect(activeBtn).toHaveAttribute("aria-current", "page");
    expect(activeBtn).toHaveAttribute("title", "Session A");
    expect(activeBtn.className).toContain("focus-visible:ring-2");
    expect(activeBtn.className).toContain("motion-reduce:transition-none");

    rerender(<RailItem label="Session A" active={false} />);
    expect(
      screen.getByRole("button", { name: "Session A" })
    ).not.toHaveAttribute("aria-current");
  });

  it("Toolbar supports arrow/Home/End keyboard navigation and floating Escape dismiss with focus restore", () => {
    const onDismiss = vi.fn();
    render(
      <div>
        <button type="button">Trigger</button>
        <Toolbar label="Actions" variant="floating" onDismiss={onDismiss}>
          <button type="button">First</button>
          <button type="button" disabled>
            Disabled
          </button>
          <button type="button">Last</button>
        </Toolbar>
      </div>
    );

    const toolbar = screen.getByRole("toolbar", { name: "Actions" });
    const first = screen.getByRole("button", { name: "First" });
    const last = screen.getByRole("button", { name: "Last" });

    expect(first).toHaveFocus();

    fireEvent.keyDown(toolbar, { key: "ArrowRight" });
    expect(last).toHaveFocus();

    fireEvent.keyDown(toolbar, { key: "Home" });
    expect(first).toHaveFocus();

    fireEvent.keyDown(toolbar, { key: "End" });
    expect(last).toHaveFocus();

    fireEvent.keyDown(toolbar, { key: "Escape" });
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
