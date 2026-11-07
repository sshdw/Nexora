//! Shared IPC core (AC-205).
//!
//! Thin, typed wrappers around the existing Phase 10.2 `#[tauri::command]`
//! functions. No business logic lives here — commands are delegated to the
//! backend verbatim. Field names mirror the Rust structs serialized via serde
//! (snake_case). Timestamps are Unix seconds (SQLite `unixepoch()`), per
//! DATABASE.md §7.1.
//!
//! Casing rule (AGENTS.md): command ARGS are camelCase (`{ conversationId }`)
//! and map to snake_case Rust params (`conversation_id`); returned structs and
//! `agent-run-event` payloads stay snake_case. Never "align" one side to the
//! other. There is no client-side error mapping: `invoke` rejects with the
//! backend's secret-free `CommandError` (commands/error.rs) verbatim.

export { invoke } from "@tauri-apps/api/core";

/** Safe, secret-free command error returned to the frontend (commands/error.rs). */
export interface CommandError {
  kind: string;
  message: string;
}
