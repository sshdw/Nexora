//! Sessions panel (D-06 / §6.5) — the shell's only route back to history.
//!
//! Tabs are not persisted and there is no sidebar, so this panel is what
//! makes a conversation that is not already open reachable. These pins cover
//! the honest filter (title only — never a pretend full-text search), the
//! two empty states, the store's own grouping, and `aria-current` on the row
//! that is already open.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Conversation } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import SessionsPanel, { type SessionsPanelProps } from "./SessionsPanel";

afterEach(cleanup);

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 1,
    title: "Alpha thread",
    status: "active",
    created_at: 1000,
    updated_at: 1000,
    workspace_root: "C:\\src\\app",
    ...overrides,
  };
}

function renderPanel(overrides: Partial<SessionsPanelProps> = {}) {
  const props: SessionsPanelProps = {
    conversations: [
      conversation({ id: 1, title: "Alpha thread" }),
      conversation({ id: 2, title: "Beta thread" }),
      conversation({ id: 3, title: "Old alpha notes", status: "archived" }),
    ],
    loading: false,
    activeId: null,
    onOpen: vi.fn(),
    ...overrides,
  };
  const view = render(
    <LocaleProvider>
      <SessionsPanel {...props} />
    </LocaleProvider>,
  );
  return { ...props, ...view };
}

const filter = () => screen.getByRole("searchbox", { name: "Filter sessions" });

describe("sessions panel — list", () => {
  it("lists every conversation with its title", () => {
    renderPanel();
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Beta thread/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Old alpha notes/ })).toBeInTheDocument();
  });

  it("groups active and archived without re-sorting", () => {
    const { container } = renderPanel();
    const groups = [...container.querySelectorAll(".nex-dock-panel-group")].map(
      (node) => node.textContent,
    );
    expect(groups).toEqual(["Active", "Archived"]);
    // The store is already `updated_at DESC`; the partition keeps that order.
    const rows = [...container.querySelectorAll(".nex-sessions-row-title")].map(
      (node) => node.textContent,
    );
    expect(rows).toEqual(["Alpha thread", "Beta thread", "Old alpha notes"]);
  });

  it("marks the open conversation with aria-current", () => {
    renderPanel({ activeId: 2 });
    expect(screen.getByRole("button", { name: /Beta thread/ })).toHaveAttribute(
      "aria-current",
      "true",
    );
    expect(screen.getByRole("button", { name: /Alpha thread/ })).not.toHaveAttribute(
      "aria-current",
    );
  });

  it("opens the conversation the row names", () => {
    const { onOpen } = renderPanel();
    fireEvent.click(screen.getByRole("button", { name: /Beta thread/ }));
    expect(onOpen).toHaveBeenCalledWith(2);
  });
});

describe("sessions panel — filter", () => {
  it("filters by title, case-insensitively and across both groups", () => {
    renderPanel();
    fireEvent.change(filter(), { target: { value: "ALPHA" } });
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Old alpha notes/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Beta thread/ })).toBeNull();
  });

  it("trims the query and restores the full list when cleared", () => {
    renderPanel();
    fireEvent.change(filter(), { target: { value: "   " } });
    expect(screen.getByRole("button", { name: /Beta thread/ })).toBeInTheDocument();

    fireEvent.change(filter(), { target: { value: "beta" } });
    expect(screen.queryByRole("button", { name: /Alpha thread/ })).toBeNull();

    fireEvent.change(filter(), { target: { value: "" } });
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
  });

  it("says the filter matched nothing — distinct from an empty store", () => {
    renderPanel();
    fireEvent.change(filter(), { target: { value: "no such title" } });
    expect(screen.getByText("No sessions match that filter")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
  });

  it("says there are no sessions at all, not that a filter failed", () => {
    renderPanel({ conversations: [] });
    expect(screen.getByText("No sessions yet")).toBeInTheDocument();
    expect(screen.queryByText("No sessions match that filter")).toBeNull();
  });
});

describe("sessions panel — loading", () => {
  it("shows the loading note instead of an empty-state claim", () => {
    renderPanel({ conversations: [], loading: true });
    expect(screen.getByText(/Loading/)).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
    // The filter stays available: the store may resolve while the user types.
    expect(filter()).toBeInTheDocument();
  });
});
