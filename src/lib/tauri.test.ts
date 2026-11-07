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
//!
//! SF-03/SEC-01 (fix/2.0-f1-security): the destructive wrappers (git writes,
//! `refactor_apply`, issue filing, `privacy_wipe`, exports) must MINT a
//! single-use confirmation id through `request_confirmation` — the blocking
//! native OS dialog — and forward it to the consuming command; the old
//! hardcoded `confirmed: true` flags are gone, and a wrapper that regresses
//! to a caller-supplied boolean (or skips the mint) fails these pins and is
//! refused by the backend with `ConfirmationRequired`. The SEC-01 wrappers
//! pin the backend-side read/size contract: the renderer forwards the
//! dialog-picked path (import) and takes the size from the persisted
//! attachment row (attach) — no webview file access on either path.

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
  attachFile,
  createIssueForFinding,
  exportConversationToFile,
  exportSetup,
  exportSetupToFile,
  gitCommit,
  gitPush,
  gitStage,
  gitUnstage,
  importConversation,
  privacyWipe,
  refactorApply,
  terminalExplain,
  terminalKill,
  terminalRun,
  type Attachment,
  type CreatedIssue,
  type ErrorExplanation,
  type RefactorApplyResult,
  type TerminalRunResult,
  type WipeResult,
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
    cmd === "request_confirmation" ? "confirm-id-1" : RUN_RESULT
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
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "terminal_kill" ? true : null
    );
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
    await expect(
      terminalExplain("boom", "non-zero exit", "openai", "gpt")
    ).resolves.toEqual(explained);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "terminal_explain",
      {
        output: "boom",
        exitContext: "non-zero exit",
        provider: "openai",
        model: "gpt",
      },
    ]);
  });
});

// ---- SF-03: mint-then-consume destructive wrappers -----------------------
// Each wrapper below used to hardcode `confirmed: true` in the invoke args;
// the backend now consumes a single-use id minted for the exact operation in
// its own scope. These pins freeze BOTH halves of the contract — the mint
// call (scope + operation identity, which is what the native dialog shows
// and what the id is bound to) and the consuming invoke (camelCase
// `confirmationId` arg per the casing rule) — because a drift on either side
// silently breaks the path (the backend refuses with ConfirmationRequired).

describe("destructive IPC wrappers mint then consume (SF-03)", () => {
  it("gitStage mints git_stage in the version_control scope and forwards the id", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-1" : 2
    );
    await expect(gitStage(["src/lib/a.ts"])).resolves.toBe(2);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "version_control",
        operation: "git_stage",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "git_stage",
      { paths: ["src/lib/a.ts"], confirmationId: "confirm-id-1" },
    ]);
  });

  it("gitUnstage mints git_unstage (its own operation, not a reusable stage id)", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-2" : 1
    );
    await expect(gitUnstage(["src/lib/a.ts"])).resolves.toBe(1);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "version_control",
        operation: "git_unstage",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "git_unstage",
      { paths: ["src/lib/a.ts"], confirmationId: "confirm-id-2" },
    ]);
  });

  it("gitCommit mints git_commit and forwards message + confirmationId", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-3" : "abc123"
    );
    await expect(gitCommit("fix: thing")).resolves.toBe("abc123");
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "version_control",
        operation: "git_commit",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "git_commit",
      { message: "fix: thing", confirmationId: "confirm-id-3" },
    ]);
  });

  it("gitPush mints git_push and keeps the allowlisted origin remote", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-4" : null
    );
    await expect(gitPush()).resolves.toBeNull();
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "version_control",
        operation: "git_push",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "git_push",
      { remote: "origin", confirmationId: "confirm-id-4" },
    ]);
  });

  it("refactorApply mints refactor_apply in the refactor scope and forwards the range", async () => {
    const applied: RefactorApplyResult = {
      path: "src-tauri/src/lib.rs",
      start_line: 10,
      end_line: 12,
      removed_lines: 3,
      file_lines: 40,
      removed_preview: "fn old() {}",
      verified: true,
    };
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-5" : applied
    );
    await expect(
      refactorApply("src-tauri/src/lib.rs", 10, 12, "dead-code-candidate")
    ).resolves.toEqual(applied);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "refactor",
        operation: "refactor_apply",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "refactor_apply",
      {
        path: "src-tauri/src/lib.rs",
        startLine: 10,
        endLine: 12,
        kind: "dead-code-candidate",
        confirmationId: "confirm-id-5",
      },
    ]);
  });

  it("createIssueForFinding no longer takes a confirmed boolean and mints in the release scope", async () => {
    const created: CreatedIssue = {
      url: "https://github.com/o/r/issues/9",
      number: 9,
    };
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-6" : created
    );
    await expect(
      createIssueForFinding("title", "debt", "src/a.rs:1", "detail")
    ).resolves.toEqual(created);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "release",
        operation: "create_issue_for_finding",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "create_issue_for_finding",
      {
        title: "title",
        location: "src/a.rs:1",
        detail: "detail",
        source: "debt",
        confirmationId: "confirm-id-6",
      },
    ]);
  });

  it("privacyWipe takes no confirmed argument and mints in the privacy scope", async () => {
    const wiped: WipeResult = { deleted_rows: 4 };
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-7" : wiped
    );
    await expect(privacyWipe()).resolves.toEqual(wiped);
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "privacy",
        operation: "privacy_wipe",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "privacy_wipe",
      { confirmationId: "confirm-id-7" },
    ]);
  });

  it("exportConversationToFile mints in the export scope bound to the conversation row", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-8" : null
    );
    await expect(
      exportConversationToFile(5, "C:/tmp/conv.json")
    ).resolves.toBeNull();
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "export",
        operation: "export_conversation_to_file",
        cwd: null,
        targetId: 5,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "export_conversation_to_file",
      {
        conversationId: 5,
        path: "C:/tmp/conv.json",
        confirmationId: "confirm-id-8",
      },
    ]);
  });

  it("exportSetup and exportSetupToFile mint their own export-scope operations", async () => {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-9" : "document"
    );
    await expect(exportSetup()).resolves.toBe("document");
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      { scope: "export", operation: "export_setup", cwd: null, targetId: null },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "export_setup",
      { confirmationId: "confirm-id-9" },
    ]);

    mockInvoke.mockReset();
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === "request_confirmation" ? "confirm-id-10" : null
    );
    await expect(exportSetupToFile("C:/tmp/setup.json")).resolves.toBeNull();
    expect(mockInvoke.mock.calls[0]).toEqual([
      "request_confirmation",
      {
        scope: "export",
        operation: "export_setup_to_file",
        cwd: null,
        targetId: null,
      },
    ]);
    expect(mockInvoke.mock.calls[1]).toEqual([
      "export_setup_to_file",
      { path: "C:/tmp/setup.json", confirmationId: "confirm-id-10" },
    ]);
  });

  it("a cancelled native mint rejects and never invokes the consuming command", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "request_confirmation") {
        // What `requestConfirmation` rejects with when the user cancels the
        // native dialog (commands/confirmations.rs mints nothing in that case).
        throw {
          kind: "confirmationRequired",
          message: "explicit confirmation is required",
        };
      }
      return null;
    });
    await expect(gitCommit("fix: thing")).rejects.toMatchObject({
      kind: "confirmationRequired",
    });
    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });
});

// ---- SEC-01: backend-side file access (import read, attachment size) -----
// The renderer holds no `fs` plugin grant any more: the import command reads
// the picked document backend-side and `attach_file` derives the size
// backend-side. The pins freeze the wire shapes that make that true — the
// picked path travels instead of file content, and no size argument exists.

describe("backend-side file access wrappers (SEC-01)", () => {
  it("importConversation forwards the dialog-picked path, not file content", async () => {
    mockInvoke.mockImplementation(async () => 42);
    await expect(importConversation("C:/picks/conv.json")).resolves.toBe(42);
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("import_conversation", {
      path: "C:/picks/conv.json",
    });
  });

  it("attachFile sends no size argument and returns the backend-derived row", async () => {
    const attached: Attachment = {
      id: 3,
      conversation_id: 5,
      message_id: null,
      file_name: "notes.txt",
      file_path: "C:/picks/notes.txt",
      file_size_bytes: 128,
      mime_type: "text/plain",
    };
    mockInvoke.mockImplementation(async () => attached);
    await expect(
      attachFile(5, "notes.txt", "C:/picks/notes.txt", "text/plain")
    ).resolves.toEqual(attached);
    expect(mockInvoke).toHaveBeenCalledWith("attach_file", {
      conversationId: 5,
      fileName: "notes.txt",
      filePath: "C:/picks/notes.txt",
      mimeType: "text/plain",
    });
  });
});
