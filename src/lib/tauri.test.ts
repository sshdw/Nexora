//! The workspace-terminal IPC wrappers survive the terminal UI removal
//! (#219), and the Rust commands they call stay registered.
//!
//! `TerminalPanel.tsx` and `TerminalEntry.tsx` are deleted, so nothing in the
//! frontend calls these three functions any more. They are kept anyway: the
//! backend `terminal_run` / `terminal_kill` pair is the contract, and the
//! panel returns with the wrappers untouched. A deletion here (or a rename of
//! the IPC command names) would quietly break that contract, so the wrappers
//! are pinned rather than left to dead-code reasoning.
//!
//! `terminal_explain` is pinned alongside them because the Rust module also
//! registers it; the frontend wrapper still exists for the same reason.

import { beforeEach, describe, expect, it, vi } from "vitest";

// The parameters stay declared even though this stub never reads them: they
// are load-bearing for `mockInvoke.mock.calls[n][0]` typing under tsc, and the
// repo's eslint config sets no `argsIgnorePattern`, so a `_` prefix alone
// would still trip `no-unused-vars`. The `void` statements below make the
// parameters genuinely read without changing the stub's behaviour.
const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
    void cmd;
    void args;
    return null;
  }),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import {
  terminalExplain,
  terminalKill,
  terminalRun,
  type ErrorExplanation,
  type TerminalRunResult,
} from "./tauri";

const RUN_RESULT: TerminalRunResult = {
  run_id: 7,
  output: "ok\n",
  truncated: false,
  success: true,
};

beforeEach(() => {
  mockInvoke.mockReset();
  // `request_confirmation` mints the single-use id the run is bound to.
  mockInvoke.mockImplementation(async (cmd: string) =>
    cmd === "request_confirmation" ? "confirm-id-1" : RUN_RESULT,
  );
});

describe("workspace terminal IPC wrappers (#219)", () => {
  it("exports both wrappers even though no UI calls them any more", () => {
    expect(typeof terminalRun).toBe("function");
    expect(typeof terminalKill).toBe("function");
    expect(typeof terminalExplain).toBe("function");
  });

  it("terminalRun still invokes `terminal_run` with the minted confirmation id", async () => {
    const result = await terminalRun("git status", null);
    expect(result).toEqual(RUN_RESULT);
    expect(mockInvoke.mock.calls[0][0]).toBe("request_confirmation");
    expect(mockInvoke.mock.calls[1]).toEqual([
      "terminal_run",
      { command: "git status", cwd: null, confirmationId: "confirm-id-1" },
    ]);
  });

  it("terminalRun forwards a workspace-relative cwd unchanged", async () => {
    await terminalRun("ls", "packages/app");
    expect(mockInvoke.mock.calls[1]).toEqual([
      "terminal_run",
      { command: "ls", cwd: "packages/app", confirmationId: "confirm-id-1" },
    ]);
  });

  it("terminalKill still invokes `terminal_kill` and returns the backend flag", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => (cmd === "terminal_kill" ? true : null));
    await expect(terminalKill()).resolves.toBe(true);
    expect(mockInvoke).toHaveBeenCalledWith("terminal_kill");
  });

  it("terminalExplain still invokes `terminal_explain` and returns its result", async () => {
    const explained: ErrorExplanation = {
      explanation: "the path does not exist",
      suggested_fix: "check the spelling",
      truncated_input: false,
    };
    mockInvoke.mockImplementation(async () => explained);
    await expect(terminalExplain("boom", "non-zero exit", "openai", "gpt")).resolves.toEqual(
      explained,
    );
    expect(mockInvoke.mock.calls[0]).toEqual([
      "terminal_explain",
      { output: "boom", exitContext: "non-zero exit", provider: "openai", model: "gpt" },
    ]);
  });
});
