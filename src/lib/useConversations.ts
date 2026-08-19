//! Conversation list data hook.
//!
//! Orchestrates loading via the `list_conversations` command and creating
//! conversations via `create_conversation`, exposing loading / error / empty
//! state to the UI. The backend owns sorting (updated_at DESC); this hook
//! preserves the order received.

import { useCallback, useEffect, useState } from "react";

import {
  type CommandError,
  type Conversation,
  archiveConversation,
  createConversation,
  deleteConversation,
  listConversations,
  renameConversation,
  restoreConversation,
} from "./tauri";

const DEFAULT_NEW_CONVERSATION_TITLE = "New Conversation";

export interface ConversationsStore {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  creating: boolean;
  /** Whether a rename/archive/restore/delete operation is in flight. */
  working: boolean;
  reload: () => Promise<void>;
  create: () => Promise<number | null>;
  /** Rename a conversation (FR-002, FR-006). */
  rename: (id: number, title: string) => Promise<void>;
  /** Archive an active conversation (FR-006). */
  archive: (id: number) => Promise<void>;
  /** Restore an archived conversation (FR-006). */
  restore: (id: number) => Promise<void>;
  /** Delete a conversation and reload the list (FR-002). */
  remove: (id: number) => Promise<void>;
}

export function useConversations(): ConversationsStore {
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [creating, setCreating] = useState<boolean>(false);
  const [working, setWorking] = useState<boolean>(false);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const data = await listConversations();
      setConversations(data);
    } catch (e) {
      setConversations([]);
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const create = useCallback(async (): Promise<number | null> => {
    setCreating(true);
    setError(null);
    try {
      const id = await createConversation(DEFAULT_NEW_CONVERSATION_TITLE);
      await reload();
      return id;
    } catch (e) {
      setError(toCommandError(e));
      return null;
    } finally {
      setCreating(false);
    }
  }, [reload]);

  // Shared runner for the per-conversation management operations: run the
  // backend command, then refresh the list from the backend so the sidebar
  // reflects the resulting title/status membership (FR-002, FR-006).
  const run = useCallback(
    async (operation: () => Promise<void>): Promise<void> => {
      setWorking(true);
      setError(null);
      try {
        await operation();
        await reload();
      } catch (e) {
        setError(toCommandError(e));
      } finally {
        setWorking(false);
      }
    },
    [reload],
  );

  const rename = useCallback(
    (id: number, title: string) => run(() => renameConversation(id, title)),
    [run],
  );
  const archive = useCallback((id: number) => run(() => archiveConversation(id)), [run]);
  const restore = useCallback((id: number) => run(() => restoreConversation(id)), [run]);
  const remove = useCallback((id: number) => run(() => deleteConversation(id)), [run]);

  useEffect(() => {
    reload();
  }, [reload]);

  return {
    conversations,
    loading,
    error,
    creating,
    working,
    reload,
    create,
    rename,
    archive,
    restore,
    remove,
  };
}

function toCommandError(error: unknown): CommandError {
  if (isCommandError(error)) return { kind: error.kind, message: error.message };
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error) return { kind: "unknown", message: error.message };
  return {
    kind: "unknown",
    message: "Unable to reach the local database.",
  };
}

function isCommandError(value: unknown): value is CommandError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as CommandError).kind === "string" &&
    typeof (value as CommandError).message === "string"
  );
}
