//! Exclusivity pins for `useOverlayRouter` (NEX-UX-001 remainder).
//!
//! Merged #204 centralized the eleven overlay flags here with per-opener
//! resets plus the `active` discriminated view and the `closeAll` contract;
//! App's overlay branch renders exactly one branch by construction. These
//! pins prove every opener leaves exactly its own flag set (including the
//! terminal opener, which previously left audit/debt/docs/gh/release
//! untouched) and that `active`/`hasOverlay` always agree with the flags.
//! No timing assertions (policy).

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useOverlayRouter, type OverlayKind } from "./useOverlayRouter";

const ALL_FLAGS = [
  "libraryOpen",
  "settingsOpen",
  "vcsOpen",
  "activityOpen",
  "terminalOpen",
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
  | "openTerminal"
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
  ["terminalOpen", "terminal", "openTerminal"],
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

  it("openTerminal closes audit/debt/docs/gh/release (NEX-UX-001 remainder fix)", () => {
    const { result } = renderHook(() => useOverlayRouter());
    for (const open of [
      "openAudit",
      "openDebt",
      "openGh",
      "openDocs",
      "openRelease",
    ] as const) {
      act(() => {
        result.current[open]();
      });
      act(() => {
        result.current.openTerminal();
      });
      expect(result.current.terminalOpen).toBe(true);
      expect(result.current.auditOpen).toBe(false);
      expect(result.current.debtOpen).toBe(false);
      expect(result.current.ghOpen).toBe(false);
      expect(result.current.docsOpen).toBe(false);
      expect(result.current.releaseOpen).toBe(false);
      expect(result.current.active).toBe("terminal");
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
