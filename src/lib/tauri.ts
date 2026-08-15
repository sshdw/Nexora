//! Tauri IPC layer.
//!
//! Thin, typed wrappers around the existing Phase 10.2 `#[tauri::command]`
//! functions. No business logic lives here — commands are delegated to the
//! backend verbatim. Field names mirror the Rust structs serialized via serde
//! (snake_case). Timestamps are Unix seconds (SQLite `unixepoch()`), per
//! DATABASE.md §7.1.

import { invoke } from "@tauri-apps/api/core";

/** A conversation row as persisted by the backend (DATABASE.md §7.1). */
export interface Conversation {
  id: number;
  title: string;
  status: "active" | "archived";
  created_at: number; // seconds since unix epoch
  updated_at: number; // seconds since unix epoch
}

/** Safe, secret-free command error returned to the frontend (commands/error.rs). */
export interface CommandError {
  kind: string;
  message: string;
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
