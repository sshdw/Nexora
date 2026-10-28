//! Activity panel (D-06 / §6.5) — "what is the agent doing", honestly.
//!
//! G-23: the persisted run rows carry no `pending_approval` and no current
//! step, so this panel draws exactly one real feed — the task queue — and
//! never invents a "Needs you" section or a progress bar. These pins cover
//! the three states of that fetch (loading, empty, error), the store's own
//! grouping, and the row contract: a task with a conversation opens it, a
//! task without one is a statement, not a button that does nothing.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentTask } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import ActivityPanel from "./ActivityPanel";

const { mockListTasks } = vi.hoisted(() => ({ mockListTasks: vi.fn() }));

vi.mock("../lib/tauri", () => ({ listTasks: mockListTasks }));

afterEach(cleanup);

beforeEach(() => {
  mockListTasks.mockReset();
});

function task(overrides: Partial<AgentTask> = {}): AgentTask {
  return {
    id: 1,
    title: "Write report",
    description: null,
    status: "pending",
    conversation_id: null,
    provider: "openai",
    model: "model-id",
    max_steps: 5,
    current_step: 0,
    total_steps: 1,
    report: null,
    run_id: null,
    created_at: 1000,
    updated_at: 1000,
    ...overrides,
  };
}

function renderPanel() {
  const onOpenTasks = vi.fn();
  const onOpenConversation = vi.fn();
  const view = render(
    <LocaleProvider>
      <ActivityPanel onOpenTasks={onOpenTasks} onOpenConversation={onOpenConversation} />
    </LocaleProvider>,
  );
  return { ...view, onOpenTasks, onOpenConversation };
}

describe("activity panel — fetch states", () => {
  it("shows the loading note until the queue answers", async () => {
    let resolve: (rows: AgentTask[]) => void = () => {};
    mockListTasks.mockImplementation(
      () => new Promise<AgentTask[]>((r) => {
        resolve = r;
      }),
    );
    renderPanel();
    expect(screen.getByText(/Loading/)).toBeInTheDocument();

    resolve([]);
    await screen.findByText("Nothing running");
    expect(screen.queryByText(/Loading/)).toBeNull();
  });

  it("says nothing is running without inventing a feed", async () => {
    mockListTasks.mockResolvedValue([]);
    renderPanel();
    await screen.findByText("Nothing running");
    expect(screen.queryByRole("alert")).toBeNull();
    // The footer to the queue's real home is always present.
    expect(screen.getByRole("button", { name: "Open tasks" })).toBeInTheDocument();
  });

  it("surfaces a failed fetch in words and still renders the empty state", async () => {
    mockListTasks.mockRejectedValue(new Error("backend offline"));
    renderPanel();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("backend offline");
    expect(screen.getByText("Nothing running")).toBeInTheDocument();
  });
});

describe("activity panel — grouping", () => {
  it("groups running, queued and settled rows under their own headings", async () => {
    mockListTasks.mockResolvedValue([
      task({ id: 1, title: "Running one", status: "running", conversation_id: 10, current_step: 2, total_steps: 5 }),
      task({ id: 2, title: "Queued one", status: "pending" }),
      task({ id: 3, title: "Done one", status: "completed" }),
      task({ id: 4, title: "Failed one", status: "failed" }),
    ]);
    const { container } = renderPanel();
    await screen.findByText("Running one");

    const groups = [...container.querySelectorAll(".nex-dock-panel-group")].map(
      (node) => node.textContent,
    );
    expect(groups).toEqual(["Running", "Queued", "Recent"]);
    // A running row's meta is the real step count, not a fabricated bar.
    expect(screen.getByText("2/5")).toBeInTheDocument();
    // The settled list repeats the status word because neighbours differ.
    expect(screen.getByText("Completed")).toBeInTheDocument();
    expect(screen.getByText("Failed")).toBeInTheDocument();
  });

  it("caps the settled list at four rows", async () => {
    mockListTasks.mockResolvedValue(
      Array.from({ length: 6 }, (_, i) =>
        task({ id: i + 1, title: `Settled ${i + 1}`, status: "completed" }),
      ),
    );
    renderPanel();
    await screen.findByText("Settled 1");
    expect(screen.queryByText("Settled 5")).toBeNull();
    expect(screen.queryByText("Settled 6")).toBeNull();
  });
});

describe("activity panel — rows", () => {
  it("opens the conversation a task belongs to", async () => {
    mockListTasks.mockResolvedValue([
      task({ id: 1, title: "Linked task", status: "running", conversation_id: 42 }),
    ]);
    const { onOpenConversation } = renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: /Linked task/ }));
    expect(onOpenConversation).toHaveBeenCalledWith(42);
  });

  it("renders a task with no conversation as a statement, not a dead button", async () => {
    mockListTasks.mockResolvedValue([
      task({ id: 1, title: "Standalone task", status: "running", conversation_id: null }),
    ]);
    const { container } = renderPanel();
    await screen.findByText("Standalone task");
    expect(screen.queryByRole("button", { name: /Standalone task/ })).toBeNull();
    expect(container.querySelector(".nex-sessions-row.is-static")).toHaveTextContent(
      "Standalone task",
    );
  });

  it("hands the full surface to the Tasks overlay", async () => {
    mockListTasks.mockResolvedValue([]);
    const { onOpenTasks } = renderPanel();
    await screen.findByText("Nothing running");
    fireEvent.click(screen.getByRole("button", { name: "Open tasks" }));
    expect(onOpenTasks).toHaveBeenCalledTimes(1);
  });
});
