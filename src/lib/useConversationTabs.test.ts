//! Store-identity + behavior pins for `useConversationTabs` (NEX-FE-002).
//!
//! The returned store object must keep a stable reference across renders
//! that change no tab state — App's tab/split/zen keydown effect and the
//! palette-registry memo both depend on it, and a fresh literal every
//! render tore the listener down and rebuilt the registry on every
//! keystroke render. No timing assertions (policy).

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useConversationTabs } from "./useConversationTabs";

describe("useConversationTabs store identity", () => {
  it("returns a stable reference across re-renders without tab-state change", () => {
    const { result, rerender } = renderHook(() => useConversationTabs());
    const first = result.current;
    rerender();
    rerender();
    expect(result.current).toBe(first);
  });

  it("returns a new reference carrying the update after a mutation", () => {
    const { result } = renderHook(() => useConversationTabs());
    const first = result.current;
    act(() => {
      result.current.open(1);
    });
    expect(result.current).not.toBe(first);
    expect(result.current.openIds).toEqual([1]);
    expect(result.current.activeId).toBe(1);
    expect(result.current.splitId).toBeNull();
  });

  it("keeps the reference for a no-op open of the already-active tab", () => {
    const { result } = renderHook(() => useConversationTabs());
    act(() => {
      result.current.open(1);
    });
    const settled = result.current;
    act(() => {
      result.current.open(1);
    });
    expect(result.current).toBe(settled);
  });

  it("pins close-neighbour activation and prune behavior", () => {
    const { result } = renderHook(() => useConversationTabs());
    act(() => {
      result.current.open(1);
      result.current.open(2);
      result.current.open(3);
    });
    expect(result.current.activeId).toBe(3);
    // Closing the active tab activates the nearest surviving neighbour
    // (previous position first).
    act(() => {
      result.current.close(3);
    });
    expect(result.current.openIds).toEqual([1, 2]);
    expect(result.current.activeId).toBe(2);
    // Prune drops ids the backend no longer lists.
    act(() => {
      result.current.prune(new Set([1]));
    });
    expect(result.current.openIds).toEqual([1]);
    expect(result.current.activeId).toBe(1);
  });
});
