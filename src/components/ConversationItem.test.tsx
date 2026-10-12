//! Archive-undo pin for `ConversationItem` (NEX-UX-003).
//!
//! Archiving is the reversible soft-delete (`archive_conversation` /
//! `restore_conversation` round-trip): it runs immediately and the snackbar
//! offers a bounded Undo that restores. Permanent deletion stays on the
//! native OS prompt (NEX-SEC-004) and is untouched here. No timing
//! assertions (policy).

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
  onArchive: (id: number) => void;
  onRestore: (id: number) => void;
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
            onRename={async () => {}}
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
  it("archives immediately and offers Undo that restores", () => {
    const onArchive = vi.fn();
    const onRestore = vi.fn();
    renderItem({ onArchive, onRestore });
    fireEvent.click(
      screen.getByRole("button", { name: "Archive conversation" }),
    );
    expect(onArchive).toHaveBeenCalledTimes(1);
    expect(onArchive).toHaveBeenCalledWith(3);
    expect(screen.getByRole("status")).toHaveTextContent(
      "“Trip plan” archived.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(onRestore).toHaveBeenCalledTimes(1);
    expect(onRestore).toHaveBeenCalledWith(3);
    expect(screen.queryByRole("status")).toBeNull();
  });
});
