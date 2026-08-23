//! Conversation import/export orchestration (FR-010, FR-011).
//!
//! Thin frontend integration over the existing backend commands
//! (`export_conversation_to_file`, `import_conversation`) and the native
//! Tauri file dialogs (`@tauri-apps/plugin-dialog`). The backend remains the
//! source of truth: export is read-only against the database and import is
//! atomic (all inserts in one transaction), so no partial state can exist on
//! failure. Every failure path surfaces the backend's classified
//! [`CommandError`] and never leaves a busy state behind. Fully local —
//! no network access is involved.

import { useCallback, useState } from "react";

import { open, save } from "@tauri-apps/plugin-dialog";
import { readTextFile } from "@tauri-apps/plugin-fs";

import {
  type CommandError,
  exportConversationToFile,
  importConversation,
} from "./tauri";

export interface ImportExportStore {
  /** Whether an export or import operation is in flight. */
  busy: boolean;
  /** Classified error from the last export/import attempt. */
  error: CommandError | null;
  /** Set after a successful export; cleared before the next attempt. */
  exportSucceeded: boolean;
  /** Set after a successful import; carries the new conversation id. */
  importedId: number | null;
  /** Export `conversationId` via a native save dialog (FR-010). Dialog
   * cancellation is a normal outcome, not an error. */
  exportTo: (conversationId: number, title: string) => Promise<boolean>;
  /** Pick a `.json` export document with the native open dialog and import
   * it (FR-011). Returns true when a conversation was created. */
  importFrom: () => Promise<number | null>;
  clearStatus: () => void;
}

export function useImportExport(): ImportExportStore {
  const [busy, setBusy] = useState<boolean>(false);
  const [error, setError] = useState<CommandError | null>(null);
  const [exportSucceeded, setExportSucceeded] = useState<boolean>(false);
  const [importedId, setImportedId] = useState<number | null>(null);

  const exportTo = useCallback(
    async (conversationId: number, title: string): Promise<boolean> => {
      setError(null);
      setExportSucceeded(false);
      let path: string | null = null;
      try {
        // Native OS save dialog; local file only, no upload path exists.
        path = await save({
          title: "Export conversation",
          defaultPath: suggestedFileName(title),
          filters: [{ name: "Nexora conversation", extensions: ["json"] }],
        });
      } catch (e) {
        setError(toCommandError(e));
        return false;
      }
      // Cancellation is a normal outcome — nothing was written.
      if (path === null) return false;

      setBusy(true);
      try {
        await exportConversationToFile(conversationId, path);
        setExportSucceeded(true);
        return true;
      } catch (e) {
        setError(toCommandError(e));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const importFrom = useCallback(async (): Promise<number | null> => {
    setError(null);
    setImportedId(null);
    let selection: string | string[] | null = null;
    try {
      selection = await open({
        multiple: false,
        title: "Import conversation",
        filters: [{ name: "Nexora conversation export", extensions: ["json"] }],
      });
    } catch (e) {
      setError(toCommandError(e));
      return null;
    }
    // Cancellation is a normal outcome — nothing was read.
    if (selection === null || Array.isArray(selection)) return null;

    setBusy(true);
    try {
      const json = await readTextFile(selection);
      const newId = await importConversation(json);
      setImportedId(newId);
      return newId;
    } catch (e) {
      setError(toCommandError(e));
      return null;
    } finally {
      setBusy(false);
    }
  }, []);

  const clearStatus = useCallback((): void => {
    setError(null);
    setExportSucceeded(false);
    setImportedId(null);
  }, []);

  return { busy, error, exportSucceeded, importedId, exportTo, importFrom, clearStatus };
}

/** File-system-safe default name derived from the conversation title. */
function suggestedFileName(title: string): string {
  const cleaned = title
    .replace(/[\\/:*?"<>|]/g, "_")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 80);
  const base = cleaned === "" ? "conversation" : cleaned;
  return `${base}.json`;
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
