//! Store tests for `useAudit` (AC-206).
//!
//! The hook owns the audit panel's four manual-run machines. The pins cover
//! the panel's documented contract: mounting never scans (no IPC without a
//! button), each machine is single-flight, results land in their own slots,
//! and the apply's frontend validation mirrors the backend guards before any
//! write is attempted (the native-dialog mint is visible as the
//! `request_confirmation` call).

import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useAudit } from "./useAudit";

const { mockInvoke, failNext, park } = vi.hoisted(() => {
  const fixtures = {
    report: {
      findings: [],
      findings_overflow: 0,
      files_scanned: 3,
      files_skipped: 0,
      skipped: [],
      skipped_overflow: 0,
    },
    drafts: {
      drafts: [],
      drafts_overflow: 0,
      targets_considered: 1,
      files_scanned: 3,
      audit_overflow: 0,
    },
    deps: {
      cargo: [],
      cargo_total: 0,
      cargo_overflow: 0,
      npm: [],
      npm_total: 0,
      npm_overflow: 0,
    },
    applied: {
      path: "src-tauri/src/lib.rs",
      start_line: 10,
      end_line: 12,
      removed_lines: 3,
      file_lines: 40,
      removed_preview: "fn old() {}",
      verified: true,
    },
  };
  const failNext = { command: "" };
  // A manually resolved gate for the single-flight pin: while active, the
  // audit scan parks until the test releases it.
  const park = { active: false, release: (): void => {} };
  const mockInvoke = vi.fn(
    async (command: string, args?: Record<string, unknown>) => {
      void args;
      if (failNext.command === command) {
        failNext.command = "";
        throw { kind: "io", message: "scan refused" };
      }
      switch (command) {
        case "request_confirmation":
          return "confirm-id-1";
        case "repo_audit":
          if (park.active) {
            return new Promise<typeof fixtures.report>((resolve) => {
              park.release = () =>
                resolve(JSON.parse(JSON.stringify(fixtures.report)));
            });
          }
          return JSON.parse(JSON.stringify(fixtures.report));
        case "testgen_drafts":
          return JSON.parse(JSON.stringify(fixtures.drafts));
        case "dep_inventory":
          return JSON.parse(JSON.stringify(fixtures.deps));
        case "refactor_apply":
          return fixtures.applied;
        default:
          throw new Error(`unexpected command ${command}`);
      }
    }
  );
  return { mockInvoke, failNext, park };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

beforeEach(() => {
  mockInvoke.mockClear();
  failNext.command = "";
  park.active = false;
});

describe("useAudit", () => {
  it("mounts idle: no scan runs without the button", () => {
    renderHook(() => useAudit());
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("runs the audit into its own slot", async () => {
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.run();
    });
    expect(mockInvoke).toHaveBeenCalledWith("repo_audit");
    expect(result.current.report?.files_scanned).toBe(3);
    expect(result.current.running).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("collapses a rapid second run into the in-flight scan", async () => {
    park.active = true;
    const { result } = renderHook(() => useAudit());
    let first!: Promise<void>;
    act(() => {
      first = result.current.run();
    });
    // The scan is parked on the manual gate; a second press must no-op.
    await act(async () => {
      await result.current.run();
    });
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    await act(async () => {
      park.release();
      await first;
    });
    expect(result.current.report).not.toBeNull();
  });

  it("routes a failed scan into the shared scan error", async () => {
    failNext.command = "repo_audit";
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.run();
    });
    expect(result.current.error).toBe("scan refused");
    expect(result.current.report).toBeNull();
  });

  it("generates drafts and loads the inventory into their slots", async () => {
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.generate();
    });
    expect(mockInvoke).toHaveBeenCalledWith("testgen_drafts");
    expect(result.current.drafts?.targets_considered).toBe(1);
    expect(result.current.copiedKey).toBeNull();
    await act(async () => {
      await result.current.loadDeps();
    });
    expect(mockInvoke).toHaveBeenCalledWith("dep_inventory");
    expect(result.current.deps).not.toBeNull();
    expect(result.current.depsError).toBeNull();
  });

  it("refuses to apply without a usable path and never invokes the write", async () => {
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.applyRemoval("   ", "10", "12", true);
    });
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "refactor_apply")
    ).toBe(false);
    expect(result.current.applyError).not.toBeNull();
    expect(result.current.applyResult).toBeNull();
    expect(result.current.applyRunning).toBe(false);
  });

  it("refuses to apply without the confirm tick", async () => {
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.applyRemoval(
        "src-tauri/src/lib.rs",
        "10",
        "12",
        false
      );
    });
    expect(
      mockInvoke.mock.calls.some(([command]) => command === "refactor_apply")
    ).toBe(false);
    expect(result.current.applyError).not.toBeNull();
  });

  it("applies a confirmed removal through the mint-and-consume gate", async () => {
    const { result } = renderHook(() => useAudit());
    await act(async () => {
      await result.current.applyRemoval(
        "src-tauri/src/lib.rs",
        "10",
        "12",
        true
      );
    });
    expect(mockInvoke).toHaveBeenCalledWith("request_confirmation", {
      scope: "refactor",
      operation: "refactor_apply",
      cwd: null,
      targetId: null,
    });
    expect(mockInvoke).toHaveBeenCalledWith("refactor_apply", {
      path: "src-tauri/src/lib.rs",
      startLine: 10,
      endLine: 12,
      kind: "dead-code-candidate",
      confirmationId: "confirm-id-1",
    });
    expect(result.current.applyResult?.removed_lines).toBe(3);
    expect(result.current.applyError).toBeNull();
  });
});
