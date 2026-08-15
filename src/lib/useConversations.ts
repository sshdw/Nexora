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
  createConversation,
  listConversations,
} from "./tauri";

const DEFAULT_NEW_CONVERSATION_TITLE = "New Conversation";

export interface ConversationsStore {
  conversations: Conversation[];
  loading: boolean;
  error: CommandError | null;
  creating: boolean;
  reload: () => Promise<void>;
  create: () => Promise<number | null>;
}

export function useConversations(): ConversationsStore {
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<CommandError | null>(null);
  const [creating, setCreating] = useState<boolean>(false);

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

  useEffect(() => {
    reload();
  }, [reload]);

  return { conversations, loading, error, creating, reload, create };
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
