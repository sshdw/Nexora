//! Exclusivity pins for `useOverlayRouter` (NEX-UX-001 remainder).
//!
//! Merged #204 centralized the overlay flags here with per-opener resets
//! plus the `active` discriminated view and the `closeAll` contract; App's
//! overlay branch renders exactly one branch by construction. These pins
//! prove every opener leaves exactly its own flag set and that
//! `active`/`hasOverlay` always agree with the flags. No timing assertions
//! (policy).
//!
//! Nav trim (#219): the terminal flag/opener left the router with the
//! terminal UI, so its row and its dedicated cross-check are gone. The
//! cross-check they proved is stronger here now — it runs over EVERY pair of
//! remaining openers instead of terminal-vs-five, which covers the original
//! audit/debt/docs/gh/release cases plus the rest.

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useOverlayRouter, type OverlayKind } from "./useOverlayRouter";

const ALL_FLAGS = [
  "libraryOpen",
  "settingsOpen",
  "vcsOpen",
  "activityOpen",
  "tasksOpen",
  "auditOpen",
  "debtOpen",
  "ghOpen",
  "docsOpen",
  "releaseOpen",
] as const;

type Flag = (typeof ALL_FLAGS)[number];

type Opener =
  | "openLibrary"
  | "openSettings"
  | "openVcs"
  | "openActivity"
  | "openTasks"
  | "openAudit"
  | "openDebt"
  | "openGh"
  | "openDocs"
  | "openRelease";

const OPENER_FOR_FLAG: ReadonlyArray<readonly [Flag, OverlayKind, Opener]> = [
  ["libraryOpen", "library", "openLibrary"],
  ["settingsOpen", "settings", "openSettings"],
  ["vcsOpen", "vcs", "openVcs"],
  ["activityOpen", "activity", "openActivity"],
  ["tasksOpen", "tasks", "openTasks"],
  ["auditOpen", "audit", "openAudit"],
  ["debtOpen", "debt", "openDebt"],
  ["ghOpen", "gh", "openGh"],
  ["docsOpen", "docs", "openDocs"],
  ["releaseOpen", "release", "openRelease"],
];

describe("useOverlayRouter exclusivity", () => {
  it("starts with no overlay", () => {
    const { result } = renderHook(() => useOverlayRouter());
    expect(result.current.active).toBeNull();
    expect(result.current.hasOverlay).toBe(false);
    for (const flag of ALL_FLAGS) {
      expect(result.current[flag]).toBe(false);
    }
  });

  it.each(OPENER_FOR_FLAG)(
    "%s leaves only its own flag set with matching active",
    (flag, kind, opener) => {
      const { result } = renderHook(() => useOverlayRouter());
      act(() => {
        (result.current[opener] as () => void)();
      });
      for (const other of ALL_FLAGS) {
        expect(result.current[other]).toBe(other === flag);
      }
      expect(result.current.active).toBe(kind);
      expect(result.current.hasOverlay).toBe(true);
    },
  );

  it("opening a second overlay closes the first (single active by construction)", () => {
    const { result } = renderHook(() => useOverlayRouter());
    act(() => {
      result.current.openAudit();
    });
    expect(result.current.active).toBe("audit");
    act(() => {
      result.current.openTasks();
    });
    expect(result.current.auditOpen).toBe(false);
    expect(result.current.tasksOpen).toBe(true);
    expect(result.current.active).toBe("tasks");
  });

  // NEX-UX-001 remainder, generalized: the old pin was "openTerminal closes
  // audit/debt/docs/gh/release". With the terminal gone this walks every
  // ordered pair, so that contract (and its siblings) is asserted rather than
  // merely dropped.
  it("opening any overlay closes whichever one was already open", () => {
    for (const [firstFlag, , firstOpener] of OPENER_FOR_FLAG) {
      for (const [secondFlag, , secondOpener] of OPENER_FOR_FLAG) {
        if (firstFlag === secondFlag) continue;
        const { result } = renderHook(() => useOverlayRouter());
        act(() => {
          (result.current[firstOpener] as () => void)();
        });
        act(() => {
          (result.current[secondOpener] as () => void)();
        });
        expect(result.current[secondFlag]).toBe(true);
        expect(result.current[firstFlag]).toBe(false);
      }
    }
  });

  it("closeAll clears every overlay", () => {
    const { result } = renderHook(() => useOverlayRouter());
    act(() => {
      result.current.openLibrary();
    });
    act(() => {
      result.current.closeAll();
    });
    for (const flag of ALL_FLAGS) {
      expect(result.current[flag]).toBe(false);
    }
    expect(result.current.active).toBeNull();
    expect(result.current.hasOverlay).toBe(false);
  });
});
