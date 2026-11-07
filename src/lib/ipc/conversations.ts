//! Conversation IPC area (AC-205): conversation CRUD, history, the chat
//! `send_message` execution path (commands/conversations.rs), the permanent
//! delete (commands/data_management.rs), and per-conversation context stats
//! (commands/context.rs).

import { invoke } from "./core";
import { OP_DELETE_CONVERSATION, requestConfirmation } from "./confirmations";

/** A conversation row as persisted by the backend (DATABASE.md §7.1). */
export interface Conversation {
  id: number;
  title: string;
  status: "active" | "archived";
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
  /** Canonical workspace root the conversation belongs to (v6 per-folder
   * history; `null` for pre-picker rows). */
  workspace_root: string | null;
}

/** List all conversations via the `list_conversations` command.
 * The backend owns ordering (`updated_at DESC`); the frontend does not re-sort. */
export function listConversations(): Promise<Conversation[]> {
  return invoke<Conversation[]>("list_conversations");
}

/** Create a conversation via the `create_conversation` command.
 * The backend requires a non-empty title (<=500 chars per the schema CHECK). */
export function createConversation(title: string): Promise<number> {
  return invoke<number>("create_conversation", { title });
}

/** One persisted `messages` row (DATABASE.md §7.2). */
export interface Message {
  id: number;
  conversation_id: number;
  role: "user" | "assistant";
  content: string;
  provider_id: number | null;
  model_name: string | null;
  created_at: number; // seconds since unix epoch
}

/** Load a conversation's persisted messages via `conversation_history`, in the
 * backend's chronological order (`created_at` ascending). The backend is the
 * only source of history; the UI never invents or reorders messages. */
export function conversationHistory(conversationId: number): Promise<Message[]> {
  return invoke<Message[]>("conversation_history", { conversationId });
}

/** Rename a conversation via `rename_conversation` (FR-002, FR-006). */
export function renameConversation(id: number, title: string): Promise<void> {
  return invoke<void>("rename_conversation", { id, title });
}

/** Archive an active conversation via `archive_conversation` (FR-006). */
export function archiveConversation(id: number): Promise<void> {
  return invoke<void>("archive_conversation", { id });
}

/** Restore an archived conversation to active via `restore_conversation` (FR-006). */
export function restoreConversation(id: number): Promise<void> {
  return invoke<void>("restore_conversation", { id });
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}

/** Normalized AI response returned by `send_message`. */
export interface AiResponse {
  content: string;
  model: string;
  tool_calls: ToolCall[];
  usage?: TokenUsage | null;
}

/** Send a user message and return/ persist the AI response.
 * `attachmentIds` names the draft attachments to link to the created user
 * message; they become part of the AI request context (FR-008).
 * The backend resolves the provider row and keyring credential before any
 * outbound request is made, so missing credentials fail before sending. */
export function sendMessage(
  conversationId: number,
  content: string,
  provider: string,
  model: string,
  attachmentIds: number[],
): Promise<AiResponse> {
  return invoke<AiResponse>("send_message", {
    conversationId,
    content,
    provider,
    model,
    attachmentIds,
  });
}

/** Permanently delete one conversation and the messages/attachments that
 * cascade from it, via `delete_conversation_permanently` (FR-002).
 *
 * The native OS confirmation dialog this raises IS the confirmation — the
 * caller does NOT show its own confirm dialog, and the id it mints is bound
 * to this exact row, so an id the user approved for another conversation (or
 * for clear-all) is refused. Cancelling at the native dialog rejects and
 * deletes nothing. */
export async function deleteConversationPermanently(id: number): Promise<void> {
  const confirmationId = await requestConfirmation(
    "data_management",
    OP_DELETE_CONVERSATION,
    { targetId: id },
  );
  return invoke<void>("delete_conversation_permanently", {
    id,
    confirmationId,
  });
}

// ---- Context panel (read-only stats + diff render) ----------------------

/** Read-only per-conversation context stats (`conversation_context_stats`).
 * Token sums are `0` with `has_token_data: false` ("n/a") where no persisted
 * usage exists; nothing is ever estimated silently. Timestamps are Unix
 * seconds. */
export interface ConversationContextStats {
  conversation_id: number;
  title: string;
  provider: string | null;
  provider_display: string | null;
  model: string | null;
  context_limit: number;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  reasoning_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  has_token_data: boolean;
  total_cost_micro_usd: number;
  usage_percent: number;
  message_count: number;
  user_message_count: number;
  assistant_message_count: number;
  tool_call_count: number;
  other_step_count: number;
  created_at: number;
  updated_at: number;
}

/** Load one conversation's read-only context stats via
 * `conversation_context_stats`. */
export function conversationContextStats(
  conversationId: number,
): Promise<ConversationContextStats> {
  return invoke<ConversationContextStats>("conversation_context_stats", {
    conversationId,
  });
}
