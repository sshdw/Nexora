//! One-conversation message data hook.
//!
//! Loads a conversation's persisted messages via `conversation_history` and
//! drives message exchange through the existing `send_message` flow (FR-003,
//! FR-005). The backend is the source of truth: history is fetched whole and
//! **replaced**, never appended locally, so reopening or reloading a
//! conversation can never duplicate messages.
//!
//! `send` delegates to the backend, which persists the user message, runs the
//! AI request, and — only on success — persists the assistant message. After
//! success **or** failure the local list is refreshed from the backend so it
//! always reflects persisted state: a failed request keeps the persisted user
//! message and never manufactures a fake assistant message (DATABASE.md §7.2).

import { useCallback, useEffect, useRef, useState } from "react";

import {
  type CommandError,
  type Message,
  conversationHistory,
  sendMessage,
} from "./tauri";

export interface ConversationStore {
  /** Persisted messages in chronological order. */
  messages: Message[];
  /** Whether history is currently being fetched. */
  loading: boolean;
  /** Classified error from a history load or a send request. */
  error: CommandError | null;
  /** Whether a send request is in flight. */
  sending: boolean;
  /** Refresh the message list from the backend. */
  reload: () => Promise<void>;
  /** Send `content` with the selected provider/model and refresh history. */
  send: (content: string, provider: string, model: string) => Promise<void>;
}

export function useConversation(conversationId: number | null): ConversationStore {
  const [messages, setMessages] = useState<Message[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<CommandError | null>(null);
  const [sending, setSending] = useState<boolean>(false);

  // The conversation this hook instance currently represents. Responses that
  // arrive after the selection has moved to another conversation are stale and
  // must never be applied: a slow history fetch for A cannot overwrite B's
  // messages when B is selected (rapid conversation switching).
  const activeConversationRef = useRef<number | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    if (conversationId === null) {
      setMessages([]);
      return;
    }
    const requestedId = conversationId;
    setLoading(true);
    setError(null);
    try {
      const data = await conversationHistory(requestedId);
      // Only the history belonging to the currently selected conversation may
      // update visible state.
      if (activeConversationRef.current !== requestedId) return;
      // Replace, never append: reopening/refreshing cannot duplicate messages.
      setMessages(data);
    } catch (e) {
      if (activeConversationRef.current !== requestedId) return;
      setMessages([]);
      setError(toCommandError(e));
    } finally {
      if (activeConversationRef.current === requestedId) {
        setLoading(false);
      }
    }
  }, [conversationId]);

  // Load history whenever the conversation is (re)selected. The ref is updated
  // synchronously here, before the async reload can complete, so any in-flight
  // response for a previous conversation is detected as stale.
  useEffect(() => {
    activeConversationRef.current = conversationId;
    setMessages([]);
    setError(null);
    if (conversationId !== null) {
      reload();
    }
  }, [conversationId, reload]);

  const send = useCallback(
    async (content: string, provider: string, model: string): Promise<void> => {
      if (conversationId === null) return;
      setSending(true);
      setError(null);
      try {
        await sendMessage(conversationId, content, provider, model);
        // Success: reflect the persisted user + assistant messages.
        await reload();
      } catch (e) {
        // The backend persisted the user message but produced no assistant
        // message. Refresh so the UI shows the persisted user message, then
        // surface the classified error (FR-003; DATABASE.md §7.2).
        await reload();
        setError(toCommandError(e));
      } finally {
        setSending(false);
      }
    },
    [conversationId, reload],
  );

  return { messages, loading, error, sending, reload, send };
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
  return { kind: "unknown", message: "Unable to reach the local backend." };
}
