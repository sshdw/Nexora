//! Dock — behaviour, accessibility and the two hard gates.
//! R4-01 / R4-07 / R4-09.
//!
//! jsdom ships no layout and computes no cascade, so these pins
//! cover what jsdom CAN prove: role/label, ARIA wiring, the active
//! panel's state tokens, the static attention dot, the tooltip's
//! hide/reveal class declarations, and the global reduced-motion
//! gate in src/styles/motion.css. No timing assertions (repo policy).

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { readRepoFile } from "../test/readRepoFile";
import Dock, { type DockProps } from "./Dock";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderDock(overrides: Partial<DockProps> = {}) {
  const { activePanel, ...rest } = overrides;
  return render(
    <Dock
      activePanel={activePanel ?? null}
      onNew={vi.fn()}
      onPanelToggle={vi.fn()}
      {...rest}
    />,
  );
}

describe("dock — shell", () => {
  it("renders a navigation landmark labelled Dock", () => {
    renderDock();
    const nav = screen.getByRole("navigation", { name: "Dock" });
    expect(nav).toBeInTheDocument();
  });

  it("renders New → Sessions → Activity → Settings, and no Workspace", () => {
    renderDock();
    expect(screen.getByRole("button", { name: "New" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Activity" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    // R6-03/R6-07: the workspace picker lives in the composer now, so the
    // Dock destination is gone — absent, not disabled.
    expect(screen.queryByRole("button", { name: "Workspace" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Zen" })).toBeNull();
  });

  it("draws every Dock glyph on the 20 step", () => {
    // R6-09: the rail's glyphs were optically small against the 44px seat.
    renderDock();
    for (const name of ["New", "Sessions", "Activity", "Settings"]) {
      const svg = screen.getByRole("button", { name }).querySelector("svg");
      expect(svg, `${name} has no glyph`).not.toBeNull();
      expect(svg).toHaveAttribute("width", "20");
      expect(svg).toHaveAttribute("height", "20");
    }
  });
});

describe("dock — panel buttons", () => {
  it("exposes aria-expanded / aria-controls on panel buttons", () => {
    renderDock({ activePanel: "sessions" });
    const ses = screen.getByRole("button", { name: "Sessions" });
    expect(ses).toHaveAttribute("aria-expanded", "true");
    expect(ses).toHaveAttribute("aria-controls", "nex-dock-panel-sessions");

    const activity = screen.getByRole("button", { name: "Activity" });
    expect(activity).toHaveAttribute("aria-expanded", "false");
    expect(activity).toHaveAttribute("aria-controls", "nex-dock-panel-activity");
  });

  it("omits aria-expanded / aria-controls from the New command button", () => {
    renderDock();
    const newBtn = screen.getByRole("button", { name: "New" });
    expect(newBtn).not.toHaveAttribute("aria-expanded");
    expect(newBtn).not.toHaveAttribute("aria-controls");
  });

  it("paints the active panel with the selected-state tokens", () => {
    renderDock({ activePanel: "activity" });
    const active = screen.getByRole("button", { name: "Activity" });
    expect(active).toHaveClass("bg-[var(--state-selected)]");
    expect(active).toHaveClass("text-[var(--color-on-surface)]");
  });
});

describe("dock — attention dot", () => {
  it("renders a static amber dot on Activity when attention is set", () => {
    renderDock({ attentionCount: 3 });
    const activity = screen.getByRole("button", { name: "Activity, 3 needs you" });
    const dot = activity.querySelector('span[class*="bg-[var(--color-primary)]"]');
    expect(dot).not.toBeNull();
    expect(dot).toHaveClass("bg-[var(--color-primary)]");
    // static: no animation or transition classes on the dot
    expect(dot).not.toHaveClass(/transition/);
    expect(dot).not.toHaveClass(/animate/);
  });

  it("omits the dot when there is no attention", () => {
    renderDock({ attentionCount: 0 });
    const activity = screen.getByRole("button", { name: "Activity" });
    const dot = activity.querySelector('span[class*="bg-[var(--color-primary)]"]');
    expect(dot).toBeNull();
  });
});

describe("dock — tooltip", () => {
  it("renders the tooltip bubble, hidden by default", () => {
    renderDock();
    const btn = screen.getByRole("button", { name: "Sessions" });
    const tooltip = btn.querySelector('span[class*="opacity-0"]');
    expect(tooltip).not.toBeNull();
    expect(tooltip).toHaveClass("opacity-0");
    expect(tooltip).toHaveTextContent("Sessions");
  });

  it("declares the group-hover / group-focus-within reveal classes", () => {
    renderDock();
    const btn = screen.getByRole("button", { name: "Sessions" });
    const tooltip = btn.querySelector('span[class*="opacity-0"]');
    expect(tooltip).toHaveClass("group-hover/dock-btn:opacity-100");
    expect(tooltip).toHaveClass("group-focus-within/dock-btn:opacity-100");
  });

  it("does not render the tooltip bubble while its panel is expanded", () => {
    // Clicking Sessions keeps focus on the trigger; the panel paints over
    // the bubble's lane, which left a clipped "S" chip at its left edge.
    renderDock({ activePanel: "sessions" });
    const btn = screen.getByRole("button", { name: "Sessions" });
    expect(btn).toHaveAttribute("aria-expanded", "true");
    expect(btn.querySelector('span[class*="opacity-0"]')).toBeNull();

    // The other panel triggers keep their tooltip until they expand.
    const activity = screen.getByRole("button", { name: "Activity" });
    expect(activity.querySelector('span[class*="opacity-0"]')).not.toBeNull();
  });
});

describe("dock — reduced-motion gate", () => {
  it("collapses transition duration under prefers-reduced-motion", () => {
    const css = readRepoFile("src/styles/motion.css").replace(/\/\*[\s\S]*?\*\//g, "");
    const block = /@media \(prefers-reduced-motion: reduce\) \{([\s\S]*?)\n\}/.exec(css);
    expect(block).not.toBeNull();
    const declarations = block![1];
    expect(declarations).toMatch(/transition-duration:\s*0\.01ms\s*!important/);
  });
});
