//! Archive-undo pin for `ConversationItem` (NEX-UX-003).
//!
//! Archiving is the reversible soft-delete (`archive_conversation` /
//! `restore_conversation` round-trip): it runs immediately and the snackbar
//! offers a bounded Undo that restores — but only when the backend call
//! actually succeeded. A failed archive shows no success snackbar (the
//! failure lands in the store error instead), and a failed Undo restore is
//! surfaced honestly instead of silently dismissing. Permanent deletion
//! stays on the native OS prompt (NEX-SEC-004) and is untouched here. No
//! timing assertions (policy).

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Conversation } from "../lib/tauri";
import { LocaleProvider } from "../lib/useLocale";
import ConversationItem from "./ConversationItem";
import { SnackbarProvider } from "./SnackbarHost";

const { mockInvoke } = vi.hoisted(() => {
  const mockInvoke = vi.fn(async () => null);
  return { mockInvoke };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

afterEach(() => {
  cleanup();
});

const conversation: Conversation = {
  id: 3,
  title: "Trip plan",
  status: "active",
  created_at: 1000,
  updated_at: 1000,
  workspace_root: null,
};

function renderItem(props: {
  onArchive: (id: number) => Promise<boolean>;
  onRestore: (id: number) => Promise<boolean>;
}) {
  return render(
    <LocaleProvider>
      <SnackbarProvider>
        <ul>
          <ConversationItem
            conversation={conversation}
            selected={false}
            archived={false}
            onSelect={() => {}}
            onExport={() => {}}
            onRename={async () => true}
            onArchive={props.onArchive}
            onRestore={props.onRestore}
            onDelete={() => {}}
          />
        </ul>
      </SnackbarProvider>
    </LocaleProvider>,
  );
}

describe("ConversationItem archive undo", () => {
  it("archives immediately and offers Undo that restores", async () => {
    const onArchive = vi.fn(async () => true);
    const onRestore = vi.fn(async () => true);
    renderItem({ onArchive, onRestore });
    fireEvent.click(
      screen.getByRole("button", { name: "Archive conversation" }),
    );
    // The success snackbar is gated on the threaded archive result.
    await screen.findByRole("status");
    expect(onArchive).toHaveBeenCalledTimes(1);
    expect(onArchive).toHaveBeenCalledWith(3);
    expect(screen.getByRole("status")).toHaveTextContent(
      "“Trip plan” archived.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await act(async () => {});
    expect(onRestore).toHaveBeenCalledTimes(1);
    expect(onRestore).toHaveBeenCalledWith(3);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("failed archive shows no success snackbar", async () => {
    const onArchive = vi.fn(async () => false);
    const onRestore = vi.fn(async () => true);
    renderItem({ onArchive, onRestore });
    fireEvent.click(
      screen.getByRole("button", { name: "Archive conversation" }),
    );
    await act(async () => {});
    expect(onArchive).toHaveBeenCalledTimes(1);
    expect(onArchive).toHaveBeenCalledWith(3);
    // The row was never archived: no success claim, no Undo for it.
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });

  it("failed restore via Undo is surfaced honestly", async () => {
    const onArchive = vi.fn(async () => true);
    const onRestore = vi.fn(async () => false);
    renderItem({ onArchive, onRestore });
    fireEvent.click(
      screen.getByRole("button", { name: "Archive conversation" }),
    );
    await screen.findByRole("status");
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    // The failed restore replaces the dismissed Undo snackbar with an
    // honest failure notice instead of vanishing as if it had succeeded.
    await screen.findByText("Could not restore “Trip plan”.");
    expect(onRestore).toHaveBeenCalledTimes(1);
    expect(onRestore).toHaveBeenCalledWith(3);
  });
});
