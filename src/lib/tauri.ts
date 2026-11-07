//! Tauri IPC layer — public barrel (AC-205).
//!
//! Thin, typed wrappers around the existing Phase 10.2 `#[tauri::command]`
//! functions. No business logic lives here — commands are delegated to the
//! backend verbatim. Field names mirror the Rust structs serialized via serde
//! (snake_case). Timestamps are Unix seconds (SQLite `unixepoch()`), per
//! DATABASE.md §7.1.
//!
//! The surface is split per command area under `lib/ipc/`; this file stays
//! the single import site (`import { … } from "./lib/tauri"`) so no consumer
//! churns and every historical export name survives with the same signature.
//! The invoke contract (casing rule, `CommandError`) lives in
//! `lib/ipc/core.ts`; the mint-and-consume confirmation gate and its
//! cross-module `OP_*` operation literals live in `lib/ipc/confirmations.ts`.
//! Two modules are re-exported explicitly because they carry names that were
//! NOT exported before the split and must stay that way: `core` (its `invoke`
//! re-export) and `confirmations` (its `OP_*` constants). Everything else is
//! a plain `export *`; the `lib/ipc/barrel.test.ts` parity test pins the
//! exact runtime surface against a snapshot list.

export { type CommandError } from "./ipc/core";
export * from "./ipc/conversations";
export * from "./ipc/providers";
export * from "./ipc/settings";
export {
  type ConfirmationScope,
  clearApplicationData,
  requestConfirmation,
} from "./ipc/confirmations";
export * from "./ipc/search";
export * from "./ipc/prompts";
export * from "./ipc/attachments";
export * from "./ipc/agent";
export * from "./ipc/tasks";
export * from "./ipc/workspace";
export * from "./ipc/terminal";
export * from "./ipc/importExport";
export * from "./ipc/versionControl";
export * from "./ipc/github";
export * from "./ipc/audit";
export * from "./ipc/deps";
export * from "./ipc/debt";
export * from "./ipc/docs";
export * from "./ipc/flags";
export * from "./ipc/system";
