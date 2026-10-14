//! Context budget (#220) — the slider's value, not its pixels.
//!
//! The slider is only worth having if its number is real, so these
//! pins cover the three ways that number could quietly stop being
//! real: out-of-range values from a stale or hand-edited store,
//! unparseable junk in the store, and a store that throws.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  CONTEXT_BUDGET_DEFAULT,
  CONTEXT_BUDGET_KEY,
  CONTEXT_BUDGET_MAX,
  CONTEXT_BUDGET_MIN,
  CONTEXT_BUDGET_STEP,
  clampContextBudget,
  parseContextBudget,
  readContextBudget,
  writeContextBudget,
} from "./useContextBudget";

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("context budget — clamping", () => {
  it("keeps a value that is already in range and on a step", () => {
    expect(clampContextBudget(64_000)).toBe(64_000);
  });

  it("snaps an off-step value onto the slider's own step", () => {
    expect(clampContextBudget(65_000)).toBe(64_000);
    expect(clampContextBudget(70_000)).toBe(72_000);
  });

  it("clamps below the floor and above the ceiling", () => {
    expect(clampContextBudget(0)).toBe(CONTEXT_BUDGET_MIN);
    expect(clampContextBudget(-5_000)).toBe(CONTEXT_BUDGET_MIN);
    expect(clampContextBudget(10_000_000)).toBe(CONTEXT_BUDGET_MAX);
  });

  it("lands on a step from both ends of the range", () => {
    // The ceiling has to be reachable exactly, or the last step of
    // the track would be unusable.
    expect(clampContextBudget(CONTEXT_BUDGET_MAX)).toBe(CONTEXT_BUDGET_MAX);
    for (const value of [CONTEXT_BUDGET_MIN, CONTEXT_BUDGET_MAX, 100_000, 240_000]) {
      const snapped = clampContextBudget(value);
      expect((snapped - CONTEXT_BUDGET_MIN) % CONTEXT_BUDGET_STEP).toBe(0);
      expect(snapped).toBeGreaterThanOrEqual(CONTEXT_BUDGET_MIN);
      expect(snapped).toBeLessThanOrEqual(CONTEXT_BUDGET_MAX);
    }
  });

  it("falls back to the default for anything that is not a number", () => {
    expect(clampContextBudget(Number.NaN)).toBe(CONTEXT_BUDGET_DEFAULT);
    expect(clampContextBudget(Number.POSITIVE_INFINITY)).toBe(CONTEXT_BUDGET_DEFAULT);
  });
});

describe("context budget — stored value", () => {
  it("parses a stored number, snapped, and rejects junk", () => {
    // 65536 is a power of two, not a slider step: parsing snaps it
    // the same way a drag does, so what is read is always a value
    // the control could itself have produced.
    expect(parseContextBudget("65536")).toBe(64_000);
    expect(parseContextBudget("64000")).toBe(64_000);
    expect(parseContextBudget(null)).toBeNull();
    expect(parseContextBudget("")).toBeNull();
    expect(parseContextBudget("banana")).toBeNull();
  });

  it("reads the default when nothing is stored", () => {
    expect(readContextBudget()).toBe(CONTEXT_BUDGET_DEFAULT);
  });

  it("round-trips through the store", () => {
    writeContextBudget(96_000);
    expect(window.localStorage.getItem(CONTEXT_BUDGET_KEY)).toBe("96000");
    expect(readContextBudget()).toBe(96_000);
  });

  it("stores a clamped value, never an out-of-range one", () => {
    writeContextBudget(999_999);
    expect(window.localStorage.getItem(CONTEXT_BUDGET_KEY)).toBe(String(CONTEXT_BUDGET_MAX));
  });

  it("degrades to the default when the store throws", () => {
    const getItem = vi
      .spyOn(window.localStorage.__proto__, "getItem")
      .mockImplementation(() => {
        throw new Error("storage disabled");
      });
    expect(readContextBudget()).toBe(CONTEXT_BUDGET_DEFAULT);
    getItem.mockRestore();
  });

  it("swallows a write failure rather than throwing at the caller", () => {
    const setItem = vi
      .spyOn(window.localStorage.__proto__, "setItem")
      .mockImplementation(() => {
        throw new Error("quota exceeded");
      });
    expect(() => writeContextBudget(64_000)).not.toThrow();
    setItem.mockRestore();
  });
});