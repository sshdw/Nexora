//! Workspace terminal IPC area (AC-205): user-authored commands dispatched
//! through the agent tool path (commands/terminal.rs). Survives the terminal
//! UI removal (#219) — see `src/lib/tauri.test.ts`.

import { invoke } from "./core";
import { requestConfirmation } from "./confirmations";

// ---- Workspace terminal (user-authored commands via the agent tool path) -
// One thin command area over the existing `execute_command` tool:
// `terminal_run` dispatches a real tool call (workspace-scoped cwd,
// hard timeout, bounded capture, truncation with notice) and returns its
// combined output with `truncated`/`success` display flags;
// `terminal_kill` cancels the active run's token (the executor kills the
// child). Single session: at most one run is active. Payloads stay
// snake_case; command args are camelCase (`command`, `cwd`,
// `confirmationId`), like every other command.

/** Combined output of one finished terminal run (`terminal_run`).
 * `success` is false when the tool path rendered its non-zero-exit
 * marker; the status text stays inside `output` (no structured code
 * crosses IPC). `truncated` mirrors the tool path's truncation notice
 * inside `output`. Timestamps: none (runs are session-only). */
export interface TerminalRunResult {
  run_id: number;
  output: string;
  truncated: boolean;
  success: boolean;
}

/** Run one workspace command via `terminal_run`. The call blocks until the
 * tool path returns (completion, timeout kill, or stop kill).
 *
 * Clicking Run *requests* the approval; the backend then raises a blocking
 * NATIVE OS confirmation dialog showing this exact command and working
 * directory, and mints a single-use id only if the user accepts. The id is
 * bound to this command + cwd, so it cannot authorize a different run.
 * Cancelling at the native dialog rejects and runs nothing — the panel shows
 * the message and no output block is marked failed.
 *
 * `cwd` is workspace-relative (`null` = workspace root); absolute escape is
 * refused backend-side, and a refused `cwd` does not consume the id.
 */
export async function terminalRun(command: string, cwd: string | null): Promise<TerminalRunResult> {
  const confirmationId = await requestConfirmation("terminal", command, { cwd });
  return invoke<TerminalRunResult>("terminal_run", {
    command,
    cwd,
    confirmationId,
  });
}

/** Stop the active terminal run via `terminal_kill`. Returns whether a run
 * was active (and is now cancelled); `false` means nothing was running. */
export function terminalKill(): Promise<boolean> {
  return invoke<boolean>("terminal_kill");
}

/** AI diagnosis of one failed terminal run (`terminal_explain`).
 * `explanation` says what went wrong, `suggested_fix` is copy-only text
 * (never auto-applied — the user copies it or retypes the command
 * manually). `truncated_input` reports whether the failed output fed to
 * the model was truncated server-side. */
export interface ErrorExplanation {
  explanation: string;
  suggested_fix: string;
  truncated_input: boolean;
}

/** Explain one failed terminal run via `terminal_explain`, using the
 * existing AI execution path (keyring-only credentials, nothing
 * persisted). `exitContext` is the short display line for the failure
 * (e.g. the exit badge text); `null` sends output only. */
export function terminalExplain(
  output: string,
  exitContext: string | null,
  provider: string,
  model: string,
): Promise<ErrorExplanation> {
  return invoke<ErrorExplanation>("terminal_explain", {
    output,
    exitContext,
    provider,
    model,
  });
}
