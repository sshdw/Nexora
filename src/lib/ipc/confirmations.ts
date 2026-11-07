//! Confirmation gate + data-management destructive commands (FR-013).
//!
//! Server-side destructive-action confirmations (NEX-SEC-004): the backend
//! raises a BLOCKING NATIVE OS CONFIRMATION DIALOG and only mints a
//! single-use id once the user accepts, then the destructive command consumes
//! that id exactly once. A bare IPC caller can therefore no longer obtain an
//! id unattended, and the id is bound to the exact operation it was minted
//! for — the old caller-supplied phrase/boolean gates are gone.
//!
//! Consequence for callers: there is NO in-app confirm step for these paths.
//! The native dialog is the single, honest confirmation. Rejections mean the
//! user cancelled at the native dialog and NOTHING ran.

import { invoke } from "./core";

/** Scope a minted confirmation id is bound to. Ids never cross scopes. The
 * set mirrors the Rust `scope_of` allowlist (`application/confirmations.rs`)
 * exactly: `terminal`, `data_management`, `version_control`, `refactor`,
 * `privacy`, `release`, `export`. */
export type ConfirmationScope =
  | "terminal"
  | "data_management"
  | "version_control"
  | "refactor"
  | "privacy"
  | "release"
  | "export";

/** Operation identities for the confirmation-gated destructive paths. These
 * literals must match the Rust `OP_*` constants (`application/
 * confirmations.rs`) exactly — the backend binds the id to them and
 * recomputes that binding on consume, so a drift refuses every call. A
 * source check in `commands/confirmations.rs` pins them.
 *
 * Cross-module internals: exported for the sibling `ipc/` area modules whose
 * destructive wrappers mint ids (git writes, deletes, exports, refactor,
 * issue filing, privacy wipe) — deliberately NOT re-exported through the
 * `lib/tauri.ts` barrel, which keeps the historical public surface. */
export const OP_DELETE_PROMPT = "delete_prompt";
export const OP_DELETE_CONVERSATION = "delete_conversation";
export const OP_CLEAR_ALL_DATA = "clear_application_data";
export const OP_GIT_STAGE = "git_stage";
export const OP_GIT_UNSTAGE = "git_unstage";
export const OP_GIT_COMMIT = "git_commit";
export const OP_GIT_PUSH = "git_push";
export const OP_REFACTOR_APPLY = "refactor_apply";
export const OP_PRIVACY_WIPE = "privacy_wipe";
export const OP_CREATE_ISSUE = "create_issue_for_finding";
export const OP_EXPORT_CONVERSATION_TO_FILE = "export_conversation_to_file";
export const OP_EXPORT_SETUP = "export_setup";
export const OP_EXPORT_SETUP_TO_FILE = "export_setup_to_file";

/** Request one single-use confirmation id for `operation` via
 * `request_confirmation`.
 *
 * This shows a native OS confirmation dialog and only resolves once the user
 * accepts; it REJECTS (with `kind: "confirmationRequired"`) on cancel, and
 * nothing is minted or executed in that case. The operation identity is what
 * the user is shown and what the id is bound to — never a free-form summary
 * the renderer could decouple from what actually runs.
 *
 * @param scope Confirmation scope the id is bound to.
 * @param operation Operation identity (command text, or an `OP_*` literal).
 * @param target Optional extra binding: workspace-relative `cwd` for terminal
 * runs, the affected row id for row-scoped deletes, and the exported
 * conversation's id for the conversation-scoped export operations.
 */
export function requestConfirmation(
  scope: ConfirmationScope,
  operation: string,
  target?: { cwd?: string | null; targetId?: number | null },
): Promise<string> {
  return invoke<string>("request_confirmation", {
    scope,
    operation,
    cwd: target?.cwd ?? null,
    targetId: target?.targetId ?? null,
  });
}

/** Clear ALL local application data (conversations, messages, attachments,
 * prompts, provider metadata, settings) via the existing Phase 9
 * `clear_application_data` command.
 *
 * The native OS confirmation dialog this raises IS the confirmation — there is
 * no separate in-app confirm step, and the id it mints is bound to clear-all
 * specifically, so an id the user approved for a single-row delete cannot be
 * spent here. Cancelling at the dialog rejects and deletes nothing.
 * Keyring credentials are not touched (they never lived in SQLite). */
export async function clearApplicationData(): Promise<void> {
  const confirmationId = await requestConfirmation(
    "data_management",
    OP_CLEAR_ALL_DATA,
  );
  return invoke<void>("clear_application_data", { confirmationId });
}
