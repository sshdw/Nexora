//! Store tests for `useDebt` (AC-206).
//!
//! The hook owns the Debt panel's whole data machine, so the pins cover the
//! contract the panel renders against: mount load, mutation-then-refetch
//! (without the list loading flag), the import notice payload, and the two
//! error slots — shared list errors for load/import/status/delete versus
//! the returned message for the manual-add create (the form renders that
//! one inline). Mocks mirror the real wrapper shapes: commands snake_case,
//! args camelCase.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DebtItem } from "./tauri";
import { useDebt } from "./useDebt";

function debtRow(overrides: Partial<DebtItem> = {}): DebtItem {
  return {
    id: 1,
    title: "old logger",
    source: "manual",
    severity: "warning",
    status: "open",
    location: null,
    note: null,
    audit_key: null,
    created_at: 1000,
    updated_at: 1000,
    ...overrides,
  };
}

const { mockInvoke, rows, failNext } = vi.hoisted(() => {
  const rows: DebtItem[] = [];
  const failNext = { command: "" };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      void args;
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "io", message: "backend refused" };
      }
      switch (command) {
        case "list_debt_items":
          return [...rows];
        case "create_debt_item": {
          const id = rows.length + 100;
          rows.push(debtRow(id, args));
          return id;
        }
        case "update_debt_item_status": {
          const { debtId, status } = args as { debtId: number; status: string };
          const row = rows.find((item) => item.id === debtId);
          if (row) row.status = status as DebtItem["status"];
          return null;
        }
        case "delete_debt_item": {
          const { debtId } = args as { debtId: number };
          const index = rows.findIndex((item) => item.id === debtId);
          if (index >= 0) rows.splice(index, 1);
          return null;
        }
        case "import_debt_from_audit":
          return { inserted: 2, skipped: 1 };
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  // Small helper hoisted alongside: build a row for create with the args.
  function debtRow(id: number, args?: Record<string, unknown>): DebtItem {
    const input = (args ?? {}) as {
      title?: string;
      severity?: string;
      location?: string | null;
      note?: string | null;
    };
    return {
      id,
      title: input.title ?? "manual row",
      source: "manual",
      severity: (input.severity as DebtItem["severity"]) ?? "info",
      status: "open",
      location: input.location ?? null,
      note: input.note ?? null,
      audit_key: null,
      created_at: 1000,
      updated_at: 1000,
    };
  }
  return { mockInvoke, rows, failNext };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  rows.length = 0;
  rows.push(debtRow({ id: 1, title: "old logger" }));
  failNext.command = "";
});

describe("useDebt", () => {
  it("loads the persisted rows on mount", async () => {
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.items).toHaveLength(1);
    expect(result.current.error).toBeNull();
    expect(mockInvoke).toHaveBeenCalledWith("list_debt_items");
  });

  it("reports a failed load in the shared error slot", async () => {
    failNext.command = "list_debt_items";
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.items).toEqual([]);
    expect(result.current.error).toBe("backend refused");
  });

  it("changes a row's status and re-fetches without the list loading flag", async () => {
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    const listCalls = mockInvoke.mock.calls.filter(
      ([command]) => command === "list_debt_items"
    ).length;

    await act(async () => {
      await result.current.changeStatus(result.current.items[0], "accepted");
    });
    expect(mockInvoke).toHaveBeenCalledWith("update_debt_item_status", {
      debtId: 1,
      status: "accepted",
    });
    expect(
      mockInvoke.mock.calls.filter(([command]) => command === "list_debt_items")
        .length
    ).toBe(listCalls + 1);
    expect(result.current.items[0].status).toBe("accepted");
    expect(result.current.busyId).toBeNull();
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("deletes a row and re-fetches", async () => {
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.remove(1);
    });
    expect(mockInvoke).toHaveBeenCalledWith("delete_debt_item", { debtId: 1 });
    await waitFor(() => expect(result.current.items).toHaveLength(0));
  });

  it("imports audit findings, returns the counts and re-fetches", async () => {
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    let outcome: { inserted: number; skipped: number } | null = null;
    await act(async () => {
      outcome = await result.current.importFromAudit();
    });
    expect(outcome).toEqual({ inserted: 2, skipped: 1 });
    expect(result.current.importing).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("routes a failed import into the shared error and returns null", async () => {
    failNext.command = "import_debt_from_audit";
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    let outcome: unknown = "unset";
    await act(async () => {
      outcome = await result.current.importFromAudit();
    });
    expect(outcome).toBeNull();
    expect(result.current.error).toBe("backend refused");
  });

  it("creates a row, re-fetches and returns null on success", async () => {
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    let failure: string | null = "unset";
    await act(async () => {
      failure = await result.current.create(
        "new row",
        "info",
        "src/a.rs:1",
        "note"
      );
    });
    expect(failure).toBeNull();
    expect(mockInvoke).toHaveBeenCalledWith("create_debt_item", {
      title: "new row",
      severity: "info",
      location: "src/a.rs:1",
      note: "note",
    });
    expect(result.current.items.some((item) => item.title === "new row")).toBe(
      true
    );
    // The shared list error stays untouched — the form owns create failures.
    expect(result.current.error).toBeNull();
    expect(result.current.saving).toBe(false);
  });

  it("returns the classified create failure for the form slot only", async () => {
    failNext.command = "create_debt_item";
    const { result } = renderHook(() => useDebt());
    await waitFor(() => expect(result.current.loading).toBe(false));
    let failure: string | null = null;
    await act(async () => {
      failure = await result.current.create("doomed", "info", null, null);
    });
    expect(failure).toBe("backend refused");
    expect(result.current.error).toBeNull();
    expect(result.current.saving).toBe(false);
  });
});
