//! Sessions panel (D-06 / §6.5) — the shell's only route back to history.
//!
//! Tabs are not persisted and there is no sidebar, so this panel is what
//! makes a conversation that is not already open reachable. These pins cover
//! the honest filter (title only — never a pretend full-text search), the
//! two empty states, the store's own grouping, and `aria-current` on the row
//! that is already open.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Conversation } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import { readRepoFile } from "../test/readRepoFile";
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
    onRename: vi.fn(async () => true),
    onArchive: vi.fn(async () => true),
    onRestore: vi.fn(async () => true),
    onDelete: vi.fn(async () => true),
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

// The row's second line is a fact the old panel lost: when the conversation
// was last touched, in the reader's locale. It is the row's only date, so a
// regression here is invisible in a title-only assertion.
describe("sessions panel — row meta", () => {
  it("names the conversation's last activity on every row", () => {
    renderPanel({
      conversations: [conversation({ id: 1, title: "Alpha thread", updated_at: 1_700_000_000 })],
    });
    const row = screen.getByRole("button", { name: /Alpha thread/ });
    const meta = row.querySelector(".nex-sessions-row-meta");
    expect(meta).not.toBeNull();
    expect(meta).toHaveTextContent(/\d/);
  });
});

const rowActions = (title: string) =>
  screen.getByRole("toolbar", { name: `Actions for ${title}` });

const action = (title: string, name: string) =>
  within(rowActions(title)).getByRole("button", { name });

describe("sessions panel — rename", () => {
  it("edits in place with the current title prefilled and focused", () => {
    renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));

    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    expect(input).toHaveValue("Alpha thread");
    expect(document.activeElement).toBe(input);
    // The row itself steps aside while it is an input.
    expect(screen.queryByRole("button", { name: /Alpha thread/ })).toBeNull();
  });

  it("commits the trimmed title on Enter and returns focus to the row", async () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "  Renamed thread  " } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(onRename).toHaveBeenCalledWith(1, "Renamed thread"));
    const row = await screen.findByRole("button", { name: /Alpha thread/ });
    await waitFor(() => expect(document.activeElement).toBe(row));
  });

  it("cancels on Escape without calling the store", async () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "never saved" } });
    fireEvent.keyDown(input, { key: "Escape" });

    expect(onRename).not.toHaveBeenCalled();
    expect(screen.queryByRole("textbox", { name: "Rename conversation" })).toBeNull();
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("button", { name: /Alpha thread/ })),
    );
  });

  it("rejects an empty or whitespace-only title", () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onRename).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
  });

  it("does not write a title that did not change", () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onRename).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
  });

  it("commits on blur, the way a click elsewhere expects, without stealing focus back", async () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "Blurred thread" } });

    // The reader aimed at another control; that control keeps focus.
    const elsewhere = filter();
    elsewhere.focus();
    fireEvent.blur(input);

    await waitFor(() => expect(onRename).toHaveBeenCalledWith(1, "Blurred thread"));
    expect(document.activeElement).toBe(elsewhere);
  });

  it("does not poison the next Enter commit after Escape on another row", () => {
    // Reviewer repro: Esc on Alpha left `skipBlur` set (unmount fires no
    // blur), so renaming Beta + Enter returned early — 0 store calls.
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const abandoned = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(abandoned, { target: { value: "Discarded title" } });
    fireEvent.keyDown(abandoned, { key: "Escape" });
    expect(onRename).not.toHaveBeenCalled();

    fireEvent.click(action("Beta thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "Beta renamed" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // SYNCHRONOUS probe, before any timer runs. On f6e8e33 the guard
    // swallowed this Enter and left the input mounted; the pending
    // return-focus timer later blur-committed it, which is exactly how a
    // waitFor-based assertion passed on the old code.
    expect(screen.queryByRole("textbox", { name: "Rename conversation" })).toBeNull();
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith(2, "Beta renamed");
  });

  it("does not poison the next Enter commit after an empty-title cancel", () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const empty = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(empty, { target: { value: "   " } });
    fireEvent.keyDown(empty, { key: "Enter" });
    expect(onRename).not.toHaveBeenCalled();

    fireEvent.click(action("Beta thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "Beta renamed" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // Same synchronous contract: no waitFor, no timer advance.
    expect(screen.queryByRole("textbox", { name: "Rename conversation" })).toBeNull();
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith(2, "Beta renamed");
  });

  it("does not poison the next Enter commit after an unchanged-title cancel", () => {
    const { onRename } = renderPanel();
    fireEvent.click(action("Alpha thread", "Rename conversation"));
    const unchanged = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.keyDown(unchanged, { key: "Enter" });
    expect(onRename).not.toHaveBeenCalled();

    fireEvent.click(action("Beta thread", "Rename conversation"));
    const input = screen.getByRole("textbox", { name: "Rename conversation" });
    fireEvent.change(input, { target: { value: "Beta renamed" } });
    fireEvent.keyDown(input, { key: "Enter" });

    // Same synchronous contract: no waitFor, no timer advance.
    expect(screen.queryByRole("textbox", { name: "Rename conversation" })).toBeNull();
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith(2, "Beta renamed");
  });
});

describe("sessions panel — archive and restore", () => {
  it("archives an active row and restores an archived one", () => {
    const { onArchive, onRestore } = renderPanel();
    fireEvent.click(action("Alpha thread", "Archive conversation"));
    expect(onArchive).toHaveBeenCalledWith(1);
    expect(onRestore).not.toHaveBeenCalled();

    fireEvent.click(action("Old alpha notes", "Restore conversation"));
    expect(onRestore).toHaveBeenCalledWith(3);
  });

  it("offers only the action the row's own status allows", () => {
    renderPanel();
    expect(
      within(rowActions("Alpha thread")).queryByRole("button", {
        name: "Restore conversation",
      }),
    ).toBeNull();
    expect(
      within(rowActions("Old alpha notes")).queryByRole("button", {
        name: "Archive conversation",
      }),
    ).toBeNull();
  });

  it("disables every row action while a store write is in flight", () => {
    renderPanel({ busy: true });
    for (const name of [
      "Rename conversation",
      "Archive conversation",
      "Delete conversation",
    ]) {
      expect(action("Alpha thread", name)).toBeDisabled();
    }
  });
});

describe("sessions panel — delete", () => {
  it("asks in-app first, naming the conversation, and calls nothing yet", () => {
    const { onDelete } = renderPanel();
    fireEvent.click(action("Alpha thread", "Delete conversation"));

    expect(screen.getByRole("dialog", { name: "Delete conversation" })).toBeInTheDocument();
    expect(
      screen.getByText("Delete “Alpha thread” permanently? This cannot be undone."),
    ).toBeInTheDocument();
    expect(onDelete).not.toHaveBeenCalled();
  });

  it("cancels without deleting", () => {
    const { onDelete } = renderPanel();
    fireEvent.click(action("Alpha thread", "Delete conversation"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onDelete).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    // The row is still there — cancel is not a quiet delete.
    expect(screen.getByRole("button", { name: /Alpha thread/ })).toBeInTheDocument();
  });

  it("deletes only after the explicit confirm, then closes the dialog", async () => {
    const { onDelete } = renderPanel();
    fireEvent.click(action("Alpha thread", "Delete conversation"));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    expect(onDelete).toHaveBeenCalledWith(1);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("keeps the row's native OS gate downstream — one in-app confirm, one store call", () => {
    // R6-06a: `delete_conversation_permanently` is gated by the backend's
    // confirmation registry, and the store's `remove` owns that prompt. The
    // panel adds the in-app question; it must not invent a second backend
    // path or call the store twice.
    const { onDelete } = renderPanel();
    fireEvent.click(action("Alpha thread", "Delete conversation"));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(onDelete).toHaveBeenCalledTimes(1);
  });
});

describe("sessions panel — action reveal", () => {
  it("reveals by opacity, never display, so every action stays in the tab order", () => {
    const sheet = readRepoFile("src/styles/dockPanels.css");
    const start = sheet.indexOf(".nex-sessions-row-actions {");
    expect(start, ".nex-sessions-row-actions has no rule").toBeGreaterThan(-1);
    const body = sheet.slice(start, sheet.indexOf("}", start));
    expect(body).toMatch(/opacity:\s*0/);
    expect(body).toMatch(/pointer-events:\s*none/);
    expect(body).not.toMatch(/display:\s*none/);
    expect(body).not.toMatch(/visibility:\s*hidden/);

    const reveal = sheet.slice(
      sheet.indexOf(".nex-sessions-item:hover .nex-sessions-row-actions,"),
    );
    expect(reveal.slice(0, reveal.indexOf("}"))).toMatch(/opacity:\s*1/);
    expect(reveal).toContain(".nex-sessions-item:focus-within .nex-sessions-row-actions");
  });

  it("fades the viewport's bottom edge into the panel surface", () => {
    // A row sliced by the panel's bottom edge reads as a rendering bug unless
    // the edge itself says "more below". The old overlay painted a background
    // gradient — NEXORA-THEME.md §2 bans gradient declarations — so the fade
    // is now an alpha mask on the scroller: it erases content instead of
    // painting anything over it, and adds no element that could intercept a
    // pointer.
    const sheet = readRepoFile("src/styles/dockPanels.css");
    expect(sheet, "the painted fade overlay still exists").not.toContain(
      ".nex-sessions::after",
    );
    // The mask is the sheet's only ramp — nothing is painted with a gradient.
    expect(sheet).not.toMatch(/background:[^;]*gradient/);
    const start = sheet.indexOf(".nex-sessions .nex-sessions-scroll {");
    expect(start, "sessions scroll mask rule is missing").toBeGreaterThan(-1);
    const body = sheet.slice(start, sheet.indexOf("}", start));
    expect(body).toMatch(
      /mask-image:\s*linear-gradient\(to bottom, black calc\(100% - var\(--space-xl\)\), transparent\)/,
    );
  });

  it("reserves scroll padding so the last row clears the fade", () => {
    const sheet = readRepoFile("src/styles/dockPanels.css");
    const start = sheet.indexOf(".nex-sessions-scroll {");
    expect(start).toBeGreaterThan(-1);
    const body = sheet.slice(start, sheet.indexOf("}", start));
    expect(body).toMatch(/padding-block-end:\s*var\(--space-xl\)/);
  });

  it("portals row-action tooltips out of the scroll container", () => {
    // Real capture: the always-mounted, centered bubble of the last action
    // extended to x=394 inside a 272 px scroller, so `scrollWidth` was 313
    // and the panel grew a horizontal bar even with the tooltip hidden.
    const { container } = renderPanel();
    const scroll = container.querySelector(".nex-sessions-scroll");
    expect(scroll).not.toBeNull();
    expect(scroll?.querySelector('[data-slot="tooltip-bubble"]')).toBeNull();

    const bubbles = [...document.body.querySelectorAll('[data-slot="tooltip-bubble"]')];
    expect(bubbles.length).toBeGreaterThan(0);
    expect(bubbles.some((bubble) => bubble.textContent === "Rename conversation")).toBe(
      true,
    );

    // Second guard: the sheet refuses sideways overflow even if a future
    // always-mounted child tries to widen the list again.
    const sheet = readRepoFile("src/styles/dockPanels.css");
    const start = sheet.indexOf(".nex-sessions-scroll {");
    expect(start).toBeGreaterThan(-1);
    const body = sheet.slice(start, sheet.indexOf("}", start));
    expect(body).toMatch(/overflow-x:\s*hidden/);
  });
});
