//! Import / export IPC area (AC-205): conversation JSON export/import
//! (FR-010, FR-011; commands/import_export.rs) and the VS Code settings +
//! MCP + portable-setup documents (WS-E.2).

import { invoke } from "./core";
import {
  OP_EXPORT_CONVERSATION_TO_FILE,
  OP_EXPORT_SETUP,
  OP_EXPORT_SETUP_TO_FILE,
  requestConfirmation,
} from "./confirmations";

// ---- Import / Export (FR-010, FR-011) ---------------------------------
// Thin wrappers over the existing Phase 8 application-layer services,
// already exposed as Tauri commands (commands/import_export.rs).

/** Export one conversation to the JSON document at `path` via
 * `export_conversation_to_file`. Read-only against the database; the
 * document preserves persisted message order (FR-010).
 *
 * SEC-03: the backend consumes a live single-use confirmation id minted for
 * `export_conversation_to_file` in the `export` scope, bound to this exact
 * conversation row — this wrapper mints it through the blocking native OS
 * dialog first, so cancelling at the dialog rejects and writes nothing. */
export async function exportConversationToFile(
  id: number,
  path: string
): Promise<void> {
  const confirmationId = await requestConfirmation(
    "export",
    OP_EXPORT_CONVERSATION_TO_FILE,
    { targetId: id }
  );
  return invoke<void>("export_conversation_to_file", {
    conversationId: id,
    path,
    confirmationId,
  });
}

/** Import one conversation from the `.json` export document at the
 * dialog-picked absolute `path` via `import_conversation`; returns the
 * schema-assigned id of the newly created conversation (FR-011).
 *
 * SEC-01: the document is read BACKEND-side (`.json`-only allowlist, path
 * containment, and the 16 MiB cap fire before any byte is read) — the
 * renderer passes the picked path, never file content, and holds no `fs`
 * plugin grant to read it itself. The backend inserts atomically —
 * validation failure leaves no partial rows behind. */
export function importConversation(path: string): Promise<number> {
  return invoke<number>("import_conversation", { path });
}

// ---- Setup import/export (WS-E.2) --------------------------------------
// VS Code settings + MCP servers import and the portable Nexora setup
// document. Reports echo caller-supplied key names only (never values);
// denials carry fixed-vocabulary reasons.

/** One successfully translated key: source key plus the Nexora key written. */
export interface ImportedEntry {
  source_key: string;
  nexora_key: string;
}

/** One rejected key: source key plus a fixed-vocabulary reason. */
export interface DeniedEntry {
  source_key: string;
  reason: string;
}

/** Per-key outcome of a setup import: translated, skipped (no Nexora
 * counterpart, never guessed), and denied (fixed-vocabulary reason). */
export interface SetupImportReport {
  imported: ImportedEntry[];
  skipped: string[];
  denied: DeniedEntry[];
}

/** Import a VS Code `settings.json` document; returns the per-key report.
 * Only the mappable subset translates (`workbench.colorTheme`); everything
 * else is reported as skipped. */
export function importVscodeSettings(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_vscode_settings", { json });
}

/** Import an MCP servers document (`{ "mcpServers": { ... } }`); returns the
 * per-server report. Validated servers replace the stored `mcp.servers` list. */
export function importMcpServers(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_mcp_servers", { json });
}

/** Export the current Nexora setup (settings plus routing profiles and
 * feature flags) to its portable JSON document.
 *
 * SEC-03: the backend consumes a live single-use confirmation id minted for
 * `export_setup` in the `export` scope — this wrapper mints it through the
 * blocking native OS dialog first, so cancelling at the dialog rejects and
 * nothing is serialized. */
export async function exportSetup(): Promise<string> {
  const confirmationId = await requestConfirmation("export", OP_EXPORT_SETUP);
  return invoke<string>("export_setup", { confirmationId });
}

/** Export the current Nexora setup to the JSON document at `path`.
 *
 * SEC-03: same gate as `exportSetup`, with the id minted for
 * `export_setup_to_file`; the destination is never overwritten (the backend
 * refuses an existing file). */
export async function exportSetupToFile(path: string): Promise<void> {
  const confirmationId = await requestConfirmation(
    "export",
    OP_EXPORT_SETUP_TO_FILE
  );
  return invoke<void>("export_setup_to_file", { path, confirmationId });
}

/** Import a portable Nexora setup document; returns the per-key report. */
export function importSetup(json: string): Promise<SetupImportReport> {
  return invoke<SetupImportReport>("import_setup", { json });
}
