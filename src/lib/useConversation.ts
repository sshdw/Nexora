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
//!
//! `send` resolves with the classified [`CommandError`] of a failed send
//! (`null` on success) so the caller can react — e.g. restore the sent text
//! into the composer after an offline/network failure — without this hook
//! owning composer state. Restoration is purely frontend state: it persists
//! nothing and never touches conversation history (DATABASE.md §7.2).
//!
//! One `listen` on `"agent-run-event"`: an agent run persists its final
//! assistant message before emitting the terminal `finished` frame, so that
//! frame triggers a `reload()` of the active conversation. Rehydration stays
//! the source of truth — the persisted list replaces the live one, never
//! appended locally.

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  type AgentRunEventPayload,
  type CommandError,
  type Message,
  conversationHistory,
  sendMessage,
} from "./tauri";
import { getLocale, tr } from "./strings";

export interface ConversationStore {
  /** Persisted messages in chronological order. */
  messages: Message[];
  /** Whether history is currently being fetched. */
  loading: boolean;
  /** Classified error from a history load or a send request. */
  error: CommandError | null;
  /** Whether a send request is in flight. */
  sending: boolean;
  /** Refresh the message list from the backend. `silent` refreshes in place:
   * no loading flip and the rendered messages stay while the fetch is in
   * flight — used when a finished agent run persists its final message. */
  reload: (options?: { silent?: boolean }) => Promise<void>;
  /** Send `content` with the selected provider/model and refresh history.
   * `attachmentIds` names the draft attachments to link to this message.
   * Resolves with the classified error of a failed send (`null` on success). */
  send: (
    content: string,
    provider: string,
    model: string,
    attachmentIds: number[],
  ) => Promise<CommandError | null>;
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

  const reload = useCallback(
    async (options?: { silent?: boolean }): Promise<void> => {
      const silent = options?.silent === true;
      if (conversationId === null) {
        setMessages([]);
        return;
      }
      const requestedId = conversationId;
      // A background refresh must not blank a populated thread: keep the
      // rendered messages and leave `loading` false while it is in flight.
      if (!silent) setLoading(true);
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
        // A failed background refresh keeps the rendered thread; the error is
        // still surfaced. A full reload keeps its existing blank-and-error.
        if (!silent) setMessages([]);
        setError(toCommandError(e));
      } finally {
        if (!silent && activeConversationRef.current === requestedId) {
          setLoading(false);
        }
      }
    },
    [conversationId],
  );

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

  // A run's terminal frame means its final assistant message is persisted:
  // re-read the active conversation's history so it appears without a
  // conversation switch. Foreign-conversation frames are ignored by the same
  // stale guard `useAgentRun` applies to its run list; `reload()` replaces
  // the list rather than appending to it.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    const setup = async (): Promise<void> => {
      try {
        const stop = await listen<AgentRunEventPayload>("agent-run-event", (event) => {
          const payload = event.payload;
          if (payload.type !== "finished") return;
          if (payload.event.conversation_id !== activeConversationRef.current) return;
          void reload({ silent: true });
        });
        if (cancelled) stop();
        else unlisten = stop;
      } catch {
        // Listen failed; the thread still rehydrates on conversation switch.
      }
    };

    void setup();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [reload]);

  const send = useCallback(
    async (
      content: string,
      provider: string,
      model: string,
      attachmentIds: number[],
    ): Promise<CommandError | null> => {
      if (conversationId === null) return null;
      setSending(true);
      setError(null);
      try {
        await sendMessage(conversationId, content, provider, model, attachmentIds);
        // Success: reflect the persisted user + assistant messages.
        await reload();
        return null;
      } catch (e) {
        // The backend persisted the user message but produced no assistant
        // message. Refresh so the UI shows the persisted user message, then
        // surface the classified error (FR-003; DATABASE.md §7.2). The error
        // is also returned so the caller can restore the composer draft —
        // frontend state only; nothing here writes or removes history.
        const failure = toCommandError(e);
        await reload();
        setError(failure);
        return failure;
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
  return { kind: "unknown", message: tr(getLocale(), "common.backendUnreachable") };
}
