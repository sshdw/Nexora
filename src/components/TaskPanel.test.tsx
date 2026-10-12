//! Delete-confirm + bounded-undo pins for `TaskPanel` (NEX-UX-002/003).
//!
//! Task deletion keeps an explicit in-app `ConfirmDialog` (the hard delete
//! is the one truly irreversible op here — no native blocking prompt), and
//! confirming stages a bounded Undo window: the row hides immediately, the
//! `delete_task` IPC only fires when the window expires, and Undo cancels
//! the timer so the row returns with zero IPC. Mocks mirror the real
//! wrapper shapes in `src/lib/tauri.ts` (snake_case commands, camelCase
//! `taskId` arg). Fake timers bound the waits — presence/absence and IPC
//! calls only, no elapsed-time assertions (policy).

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentTask, AgentTaskStep } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import { SnackbarProvider } from "./SnackbarHost";
import TaskPanel from "./TaskPanel";

const taskRow: AgentTask = {
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
};

const stepRow: AgentTaskStep = {
  id: 10,
  task_id: 1,
  seq: 1,
  title: "Draft outline",
  status: "pending",
  result: null,
  run_id: null,
  started_at: 1000,
  finished_at: null,
};

const { mockInvoke, mockListen, deletedIds } = vi.hoisted(() => {
  const deletedIds: number[] = [];
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    if (command === "list_tasks") return [{ ...taskRow }];
    if (command === "list_task_steps") return [{ ...stepRow }];
    if (command === "delete_task") {
      deletedIds.push((args as { taskId: number }).taskId);
      return null;
    }
    throw new Error(`unexpected command ${command}`);
  });
  const mockListen = vi.fn(async () => () => {});
  return { mockInvoke, mockListen, deletedIds };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

beforeEach(() => {
  mockInvoke.mockClear();
  mockListen.mockClear();
  deletedIds.length = 0;
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function renderPanel() {
  return render(
    <LocaleProvider>
      <SnackbarProvider>
        <TaskPanel onClose={() => {}} defaultProvider={null} defaultModel={null} />
      </SnackbarProvider>
    </LocaleProvider>,
  );
}

async function openDeleteDialog() {
  renderPanel();
  await waitFor(() =>
    expect(screen.getByText("Write report")).toBeInTheDocument(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("Delete task?");
  return dialog;
}

describe("TaskPanel delete with bounded undo", () => {
  it("asks in-app (ConfirmDialog) instead of a blocking native prompt", async () => {
    const dialog = await openDeleteDialog();
    // Nothing fired yet — the delete is still unconfirmed.
    expect(mockInvoke.mock.calls.some(([c]) => c === "delete_task")).toBe(
      false,
    );
    // Fake timers from here: the pending-delete + snackbar windows below
    // are the only timer-driven behavior under test.
    vi.useFakeTimers();
    // Confirming hides the row and offers Undo; the IPC still has not fired.
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete" }),
    );
    expect(screen.queryByText("Write report")).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent(
      "“Write report” deleted.",
    );
    expect(
      screen.getByRole("button", { name: "Undo" }),
    ).toBeInTheDocument();
    expect(mockInvoke.mock.calls.some(([c]) => c === "delete_task")).toBe(
      false,
    );
  });

  it("Undo restores the row with zero IPC", async () => {
    await openDeleteDialog();
    const dialog = screen.getByRole("dialog");
    vi.useFakeTimers();
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(screen.getByText("Write report")).toBeInTheDocument();
    expect(screen.queryByRole("status")).toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    expect(deletedIds).toEqual([]);
  });

  it("commits the delete_task IPC when the window expires", async () => {
    await openDeleteDialog();
    const dialog = screen.getByRole("dialog");
    vi.useFakeTimers();
    fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete" }),
    );
    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    // The mocked IPC resolves immediately; flush the commit microtasks,
    // then assert synchronously (no waitFor under fake timers).
    await act(async () => {});
    expect(mockInvoke).toHaveBeenCalledWith("delete_task", { taskId: 1 });
    expect(deletedIds).toEqual([1]);
  });

  it("cancelling the dialog deletes nothing", async () => {
    const dialog = await openDeleteDialog();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByText("Write report")).toBeInTheDocument();
    expect(screen.queryByRole("status")).toBeNull();
    vi.useFakeTimers();
    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    expect(deletedIds).toEqual([]);
  });
});
