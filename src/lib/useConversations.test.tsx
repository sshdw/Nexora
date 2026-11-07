//! Store tests for `useConversations` (AC-211 minimum set, review 03 §6.3
//! item 3 — one of the two core-flow hooks).
//!
//! Pins the documented contract: the list loads on mount in backend order
//! (the backend owns sorting), `create` persists the locale default title and
//! reloads, every rename/archive/restore runs its command and refreshes the
//! list resolving `true`, and the destructive `remove` goes through the
//! backend's native confirmation mint (`request_confirmation` bound to
//! `delete_conversation`) — a cancel at that prompt resolves `false` and
//! deletes nothing. Failures land in `error` and resolve `false`; the list
//! load failure empties the list.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useConversations } from "./useConversations";

const { mockInvoke, failNext, state } = vi.hoisted(() => {
  const failNext = { command: "" };
  const state = {
    conversations: [
      {
        id: 2,
        title: "second",
        status: "active" as const,
        created_at: 900,
        updated_at: 950,
        workspace_root: null,
      },
      {
        id: 1,
        title: "first",
        status: "archived" as const,
        created_at: 100,
        updated_at: 200,
        workspace_root: null,
      },
    ],
  };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      void args;
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "db", message: "backend refused" };
      }
      switch (command) {
        case "list_conversations":
          return JSON.parse(JSON.stringify(state.conversations));
        case "create_conversation":
          state.conversations.unshift({
            id: 3,
            title: (args as { title: string }).title,
            status: "active" as const,
            created_at: 1000,
            updated_at: 1000,
            workspace_root: null,
          });
          return 3;
        case "rename_conversation":
        case "archive_conversation":
        case "restore_conversation":
          return null;
        case "request_confirmation":
          return "confirm-id-1";
        case "delete_conversation_permanently":
          return null;
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, state };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
  state.conversations = state.conversations.slice(0, 2);
});

describe("useConversations — load", () => {
  it("loads the list on mount and preserves the backend order", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockInvoke).toHaveBeenCalledWith("list_conversations");
    expect(
      result.current.conversations.map((conversation) => conversation.id)
    ).toEqual([2, 1]);
    expect(result.current.error).toBeNull();
  });

  it("empties the list and surfaces the error when the load fails", async () => {
    failNext.command = "list_conversations";
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.conversations).toEqual([]);
    expect(result.current.error).toEqual({
      kind: "db",
      message: "backend refused",
    });
  });
});

describe("useConversations — create and manage", () => {
  it("creates with the locale default title and reloads the list", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let created: number | null = null;
    await act(async () => {
      created = await result.current.create();
    });
    expect(created).toBe(3);
    expect(mockInvoke).toHaveBeenCalledWith("create_conversation", {
      title: "New Conversation",
    });
    // The list refreshed from the backend after the write.
    expect(
      mockInvoke.mock.calls.filter(
        ([command]) => command === "list_conversations"
      )
    ).toHaveLength(2);
    expect(result.current.conversations[0].id).toBe(3);
    expect(result.current.creating).toBe(false);
  });

  it("renames, archives, and restores through their commands and reloads", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let ok = false;
    await act(async () => {
      ok = await result.current.rename(2, "renamed");
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("rename_conversation", {
      id: 2,
      title: "renamed",
    });

    await act(async () => {
      ok = await result.current.archive(2);
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("archive_conversation", { id: 2 });

    await act(async () => {
      ok = await result.current.restore(1);
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("restore_conversation", { id: 1 });

    expect(result.current.working).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("reports a failed rename through error and resolves false", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));
    failNext.command = "rename_conversation";

    let ok = true;
    await act(async () => {
      ok = await result.current.rename(2, "nope");
    });
    expect(ok).toBe(false);
    expect(result.current.error).toEqual({
      kind: "db",
      message: "backend refused",
    });
    expect(result.current.working).toBe(false);
  });
});

describe("useConversations — permanent delete (native confirm gate)", () => {
  it("mints the row-bound confirmation, deletes, and reloads", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const lists = mockInvoke.mock.calls.filter(
      ([command]) => command === "list_conversations"
    ).length;

    let ok = false;
    await act(async () => {
      ok = await result.current.remove(2);
    });
    expect(ok).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "data_management",
      operation: "delete_conversation",
      cwd: null,
      targetId: 2,
    });
    expect(mockInvoke).toHaveBeenCalledWith("delete_conversation_permanently", {
      id: 2,
      confirmationId: "confirm-id-1",
    });
    expect(
      mockInvoke.mock.calls.filter(
        ([command]) => command === "list_conversations"
      ).length
    ).toBeGreaterThan(lists);
  });

  it("deletes nothing when the native prompt is cancelled", async () => {
    const { result } = renderHook(() => useConversations());
    await waitFor(() => expect(result.current.loading).toBe(false));
    // Exactly one call happens next — the confirmation mint, which rejects.
    mockInvoke.mockImplementationOnce(async (command: string) => {
      if (command === "request_confirmation") {
        throw { kind: "confirmationRequired", message: "cancelled" };
      }
      throw new Error(`unexpected command ${command}`);
    });

    let ok = true;
    await act(async () => {
      ok = await result.current.remove(2);
    });
    expect(ok).toBe(false);
    expect(
      mockInvoke.mock.calls.some(
        ([command]) => command === "delete_conversation_permanently"
      )
    ).toBe(false);
    expect(result.current.error?.kind).toBe("confirmationRequired");
  });
});
