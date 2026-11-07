//! Store tests for `useSpendLimit` (AC-211 minimum set, review 03 §6.3 item 5).
//!
//! The per-run spend guard is a budget guardrail, so the pins cover the
//! documented contract end to end: the persisted integer loads, every
//! "no limit" shape (`null`, empty, non-integer, non-positive) parses to
//! `null`, a valid write lands as the stringified micro-USD integer under
//! `agent.spend_limit_micro_usd`, clearing deletes the key, and the
//! value-domain rejection (non-integer, negative, NaN) fires no IPC at all
//! and reports `invalidInput` instead. Backend failures surface through
//! `error` as `CommandError`s without throwing.

import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SPEND_LIMIT_KEY, useSpendLimit } from "./useSpendLimit";

const { mockInvoke, failNext, baseImpl } = vi.hoisted(() => {
  const failNext = { command: "" };
  const baseImpl = async (
    command: string,
    args?: Record<string, unknown>,
  ): Promise<unknown> => {
    void args;
    if (failNext.command === command) {
      failNext.command = "";
      throw { kind: "io", message: "backend refused" };
    }
    switch (command) {
      case "get_setting":
        return null;
      case "set_setting":
      case "delete_setting":
        return null;
      default:
        throw new Error(`unexpected command ${command}`);
    }
  };
  const mockInvoke = vi.fn(baseImpl);
  return { mockInvoke, failNext, baseImpl };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  // Restore the base implementation: tests below install per-test
  // implementations and a leftover one would bleed into the next test.
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(baseImpl);
  failNext.command = "";
});

describe("useSpendLimit — load", () => {
  it("loads the persisted integer limit", async () => {
    mockInvoke.mockImplementation(async (command: string) => {
      if (command === "get_setting") return "7500";
      return null;
    });
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockInvoke).toHaveBeenCalledWith("get_setting", { key: SPEND_LIMIT_KEY });
    expect(result.current.limitMicroUsd).toBe(7500);
    expect(result.current.error).toBeNull();
  });

  it("parses every no-limit shape to null without an error", async () => {
    for (const stored of [null, "", "  ", "abc", "0", "-5", "2.5"]) {
      mockInvoke.mockImplementation(async (command: string) => {
        if (command === "get_setting") return stored;
        return null;
      });
      const { result } = renderHook(() => useSpendLimit());
      await waitFor(() => expect(result.current.loading).toBe(false));
      expect(result.current.limitMicroUsd).toBeNull();
      expect(result.current.error).toBeNull();
    }
  });

  it("surfaces a failed load as a CommandError and stops loading", async () => {
    failNext.command = "get_setting";
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.limitMicroUsd).toBeNull();
    expect(result.current.error).toEqual({ kind: "io", message: "backend refused" });
  });
});

describe("useSpendLimit — persist", () => {
  it("persists a valid limit under the setting key and mirrors it", async () => {
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let accepted = false;
    await act(async () => {
      accepted = await result.current.setLimit(2500);
    });
    expect(accepted).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: SPEND_LIMIT_KEY,
      value: "2500",
    });
    expect(result.current.limitMicroUsd).toBe(2500);
    expect(result.current.saving).toBe(false);
  });

  it("clears the limit by deleting the setting key", async () => {
    mockInvoke.mockImplementation(async (command: string) => {
      if (command === "get_setting") return "2500";
      return null;
    });
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.limitMicroUsd).toBe(2500));

    let accepted = false;
    await act(async () => {
      accepted = await result.current.setLimit(null);
    });
    expect(accepted).toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("delete_setting", { key: SPEND_LIMIT_KEY });
    expect(result.current.limitMicroUsd).toBeNull();
  });

  it("rejects non-integer, negative, and NaN values with no IPC at all", async () => {
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.loading).toBe(false));

    for (const invalid of [1.5, -5, Number.NaN]) {
      let accepted = true;
      await act(async () => {
        accepted = await result.current.setLimit(invalid);
      });
      expect(accepted).toBe(false);
      expect(result.current.error?.kind).toBe("invalidInput");
      expect(result.current.error?.message).toBe(
        "The spend limit must be a non-negative integer.",
      );
    }
    // The value-domain rejection is local: no write command ever fired.
    expect(mockInvoke.mock.calls.some(([command]) => command === "set_setting")).toBe(false);
    expect(mockInvoke.mock.calls.some(([command]) => command === "delete_setting")).toBe(false);
  });

  it("surfaces a failed write and keeps the previous mirror value", async () => {
    const { result } = renderHook(() => useSpendLimit());
    await waitFor(() => expect(result.current.loading).toBe(false));
    failNext.command = "set_setting";

    let accepted = true;
    await act(async () => {
      accepted = await result.current.setLimit(2500);
    });
    expect(accepted).toBe(false);
    expect(result.current.error).toEqual({ kind: "io", message: "backend refused" });
    expect(result.current.limitMicroUsd).toBeNull();
    expect(result.current.saving).toBe(false);
  });
});
