import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { WorkQueueItem } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import WorkQueueStrip from "./WorkQueueStrip";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const SAMPLE_ITEMS: WorkQueueItem[] = [
  {
    id: 1,
    conversation_id: 7,
    content: "First follow-up",
    provider: "openai",
    model: "gpt-5",
    effort: null,
    position: 0,
    status: "queued",
    created_at: 100,
    updated_at: 100,
  },
  {
    id: 2,
    conversation_id: 7,
    content: "Second follow-up",
    provider: "openai",
    model: "gpt-5",
    effort: "high",
    position: 1,
    status: "queued",
    created_at: 101,
    updated_at: 101,
  },
];

describe("WorkQueueStrip (Stage 6 PR 6A)", () => {
  it("renders FIFO items with reorder, inline edit, and cancel controls", async () => {
    const onMove = vi.fn(async () => {});
    const onEdit = vi.fn(async () => {});
    const onCancel = vi.fn(async () => {});
    const onRunNext = vi.fn(async () => {});

    render(
      <LocaleProvider>
        <WorkQueueStrip
          items={SAMPLE_ITEMS}
          paused={false}
          hasActiveRun={true}
          onMove={onMove}
          onEdit={onEdit}
          onCancel={onCancel}
          onRunNext={onRunNext}
        />
      </LocaleProvider>
    );

    expect(screen.getByText("First follow-up")).toBeInTheDocument();
    expect(screen.getByText("Second follow-up")).toBeInTheDocument();

    const upButtons = screen.getAllByRole("button", { name: "Move up" });
    const downButtons = screen.getAllByRole("button", { name: "Move down" });
    expect(upButtons[0]).toBeDisabled();
    expect(downButtons[1]).toBeDisabled();

    fireEvent.click(downButtons[0]);
    expect(onMove).toHaveBeenCalledWith(1, "down");

    fireEvent.click(screen.getAllByRole("button", { name: "Edit" })[0]);
    const input = screen.getByRole("textbox", { name: "Edit" });
    fireEvent.change(input, { target: { value: "Edited first task" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(onEdit).toHaveBeenCalledWith(1, "Edited first task");
    });

    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[1]);
    expect(onCancel).toHaveBeenCalledWith(2);
  });

  it("shows paused banner and Run next only when paused without an active run", () => {
    const onRunNext = vi.fn(async () => {});
    const { rerender } = render(
      <LocaleProvider>
        <WorkQueueStrip
          items={SAMPLE_ITEMS}
          paused={true}
          hasActiveRun={false}
          onMove={vi.fn()}
          onEdit={vi.fn()}
          onCancel={vi.fn()}
          onRunNext={onRunNext}
        />
      </LocaleProvider>
    );

    expect(screen.getByRole("status")).toHaveTextContent(/Queue paused/);
    const runNextBtn = screen.getByRole("button", { name: "Run next" });
    fireEvent.click(runNextBtn);
    expect(onRunNext).toHaveBeenCalledTimes(1);

    rerender(
      <LocaleProvider>
        <WorkQueueStrip
          items={SAMPLE_ITEMS}
          paused={true}
          hasActiveRun={true}
          onMove={vi.fn()}
          onEdit={vi.fn()}
          onCancel={vi.fn()}
          onRunNext={onRunNext}
        />
      </LocaleProvider>
    );
    expect(screen.queryByRole("button", { name: "Run next" })).toBeNull();
  });
});
