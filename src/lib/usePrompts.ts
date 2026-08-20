//! Prompt Library data hook.
//!
//! Orchestrates the Prompt Library screen's list/create/update/delete flow over
//! the existing Tauri commands (FR-007). The backend is the source of truth: the
//! library is loaded whole from `list_prompts` and mutations call the command,
//! then the list is re-fetched so it always reflects persisted state. Rows are
//! sorted by `updated_at` descending for display (the acceptance contract for the
//! Prompt Library list), independent of the backend's creation-ordered rows.

import { useCallback, useEffect, useState } from "react";

import {
  type CommandError,
  type Prompt,
  createPrompt,
  deletePrompt,
  listPrompts,
  updatePrompt,
} from "./tauri";

export interface PromptsStore {
  /** Saved prompts sorted by `updated_at` descending. */
  prompts: Prompt[];
  /** Whether the list is currently being fetched. */
  loading: boolean;
  /** Classified error from any prompt-library operation. */
  error: CommandError | null;
  /** Whether a create/update/delete is in flight. */
  working: boolean;
  /** Refresh the list from the backend. */
  reload: () => Promise<void>;
  /** Create a prompt and refresh the list; returns its id, or null on error. */
  create: (title: string, content: string) => Promise<number | null>;
  /** Update a prompt and refresh the list; false when the call failed. */
  update: (id: number, title: string, content: string) => Promise<boolean>;
  /** Permanently delete a prompt and refresh the list; false on error. */
  remove: (id: number) => Promise<boolean>;
}

export function usePrompts(): PromptsStore {
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [working, setWorking] = useState<boolean>(false);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const data = await listPrompts();
      // The backend lists in creation order; the Prompt Library presents the
      // most recently edited prompts first (AC for the list screen).
      setPrompts([...data].sort((a, b) => b.updated_at - a.updated_at));
    } catch (e) {
      setPrompts([]);
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const create = useCallback(
    async (title: string, content: string): Promise<number | null> => {
      setWorking(true);
      setError(null);
      try {
        const id = await createPrompt(title, content);
        await reload();
        return id;
      } catch (e) {
        setError(toCommandError(e));
        return null;
      } finally {
        setWorking(false);
      }
    },
    [reload],
  );

  // Shared runner for update/delete: call the command, then refresh so the list
  // reflects the mutation. Returns whether the operation succeeded.
  const run = useCallback(
    async (operation: () => Promise<void>): Promise<boolean> => {
      setWorking(true);
      setError(null);
      try {
        await operation();
        await reload();
        return true;
      } catch (e) {
        setError(toCommandError(e));
        return false;
      } finally {
        setWorking(false);
      }
    },
    [reload],
  );

  const update = useCallback(
    (id: number, title: string, content: string) =>
      run(() => updatePrompt(id, title, content)),
    [run],
  );

  const remove = useCallback((id: number) => run(() => deletePrompt(id)), [run]);

  useEffect(() => {
    void reload();
  }, [reload]);

  return { prompts, loading, error, working, reload, create, update, remove };
}

function toCommandError(error: unknown): CommandError {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).kind === "string" &&
    typeof (error as CommandError).message === "string"
  ) {
    return { kind: (error as CommandError).kind, message: (error as CommandError).message };
  }
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error) return { kind: "unknown", message: error.message };
  return { kind: "unknown", message: "Unable to reach the local database." };
}