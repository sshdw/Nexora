//! Prompt Library IPC area (AC-205): the `prompts` rows (DATABASE.md §7.3)
//! and their CRUD commands (FR-007).

import { invoke } from "./core";
import { OP_DELETE_PROMPT, requestConfirmation } from "./confirmations";

/** One `prompts` row as persisted (DATABASE.md §7.3). */
export interface Prompt {
  id: number;
  title: string;
  content: string;
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** List every saved prompt via `list_prompts`. The backend returns rows in
 * creation order; the Prompt Library screen sorts by `updated_at` locally. */
export function listPrompts(): Promise<Prompt[]> {
  return invoke<Prompt[]>("list_prompts");
}

/** Create a prompt via `create_prompt` and return its schema-assigned id
 * (FR-007; DATABASE.md §7.3). */
export function createPrompt(title: string, content: string): Promise<number> {
  return invoke<number>("create_prompt", { title, content });
}

/** Update a prompt's `title` / `content` via `update_prompt` (FR-007). */
export function updatePrompt(id: number, title: string, content: string): Promise<void> {
  return invoke<void>("update_prompt", { id, title, content });
}

/** Permanently delete one prompt via `delete_prompt_permanently`.
 *
 * The native OS confirmation dialog this raises IS the confirmation — the
 * caller does NOT show its own confirm dialog, and the id it mints is bound
 * to this exact row, so an id the user approved for another prompt (or for
 * clear-all) is refused. Cancelling at the native dialog rejects and deletes
 * nothing (FR-007). */
export async function deletePrompt(id: number): Promise<void> {
  const confirmationId = await requestConfirmation("data_management", OP_DELETE_PROMPT, {
    targetId: id,
  });
  return invoke<void>("delete_prompt_permanently", {
    id,
    confirmationId,
  });
}
