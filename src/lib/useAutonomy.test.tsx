//! Store tests for `useAutonomy` (NEX-FE-003).
//!
//! One store instance is shared by Settings and the conversation header, so
//! a write through the instance is visible to every consumer immediately.
//! Mocks mirror the real wrapper shapes in `src/lib/tauri.ts`: commands are
//! snake_case (`get_setting`, `set_setting`), args are camelCase (`key`,
//! `value`). No timing assertions (policy).

import { act, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AutonomyStore } from "./useAutonomy";
import { AUTONOMY_KEY, useAutonomy } from "./useAutonomy";

const { mockInvoke, settings, failNextSet } = vi.hoisted(() => {
  const settings = new Map<string, string | null>();
  const failNextSet = { current: false };
  const mockInvoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_setting") {
      const key = (args as { key: string }).key;
      return settings.has(key) ? (settings.get(key) as string | null) : null;
    }
    if (command === "set_setting") {
      if (failNextSet.current) {
        failNextSet.current = false;
        throw { kind: "io", message: "disk refused" };
      }
      const { key, value } = args as { key: string; value: string | null };
      settings.set(key, value);
      return null;
    }
    throw new Error(`unexpected command ${command}`);
  });
  return { mockInvoke, settings, failNextSet };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  settings.clear();
  failNextSet.current = false;
});

describe("useAutonomy", () => {
  it("loads the persisted mode", async () => {
    settings.set(AUTONOMY_KEY, "supervised");
    const { result } = renderHook(() => useAutonomy());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.mode).toBe("supervised");
    expect(result.current.error).toBeNull();
  });

  it("falls back to semi_autonomous when nothing parseable is stored", async () => {
    settings.set(AUTONOMY_KEY, "bogus");
    const { result } = renderHook(() => useAutonomy());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.mode).toBe("semi_autonomous");
  });

  it("persists writes and rolls back with a message on failure", async () => {
    const { result } = renderHook(() => useAutonomy());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let failure: string | null = "unset";
    await act(async () => {
      failure = await result.current.setMode("full_autonomous");
    });
    expect(failure).toBeNull();
    expect(result.current.mode).toBe("full_autonomous");
    expect(settings.get(AUTONOMY_KEY)).toBe("full_autonomous");
    expect(mockInvoke).toHaveBeenCalledWith("set_setting", {
      key: AUTONOMY_KEY,
      value: "full_autonomous",
    });

    failNextSet.current = true;
    await act(async () => {
      failure = await result.current.setMode("supervised");
    });
    expect(failure).toBe("disk refused");
    expect(result.current.mode).toBe("full_autonomous");
    expect(result.current.error).toBe("disk refused");
  });

  it("shares one instance between two surfaces: a write in one reflects in both", async () => {
    function Reader({
      autonomy,
      label,
    }: {
      autonomy: AutonomyStore;
      label: string;
    }) {
      return (
        <div>
          <span data-testid={`mode-${label}`}>{autonomy.mode}</span>
          <button
            type="button"
            onClick={() => void autonomy.setMode("full_autonomous")}
          >
            set-{label}
          </button>
        </div>
      );
    }
    function Harness() {
      const autonomy = useAutonomy();
      return (
        <div>
          <Reader autonomy={autonomy} label="settings" />
          <Reader autonomy={autonomy} label="header" />
        </div>
      );
    }
    render(<Harness />);
    // Both surfaces start on the persisted default (no focus-resync needed).
    await waitFor(() =>
      expect(screen.getByTestId("mode-settings")).toHaveTextContent("semi_autonomous"),
    );
    expect(screen.getByTestId("mode-header")).toHaveTextContent("semi_autonomous");
    // A change made in the header surface reflects in Settings immediately.
    fireEvent.click(screen.getByRole("button", { name: "set-header" }));
    await waitFor(() =>
      expect(screen.getByTestId("mode-settings")).toHaveTextContent("full_autonomous"),
    );
    expect(screen.getByTestId("mode-header")).toHaveTextContent("full_autonomous");
  });
});
