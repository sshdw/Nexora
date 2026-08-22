//! Draft attachment state for the active conversation (FR-008).
//!
//! Thin orchestration over the existing attachment IPC commands
//! (`attach_file`, `list_attachments`, `remove_attachment`) and the native
//! Tauri file picker (`@tauri-apps/plugin-dialog`). The backend is the source
//! of truth: drafts are loaded whole from `list_attachments` whenever the
//! conversation changes or after any mutation, so UI state can never drift
//! from persisted state. Only metadata is handled here — no file content is
//! read or uploaded; the local path stays backend bookkeeping and is never
//! rendered.
//!
//! Picker cancellation (`open` resolving to `null`) is a normal outcome, not
//! an error. Every failure path surfaces the backend's classified
//! [`CommandError`] and never leaves the UI stuck in a busy state.

import { useCallback, useEffect, useRef, useState } from "react";

import { open } from "@tauri-apps/plugin-dialog";
import { stat } from "@tauri-apps/plugin-fs";

import { guessMimeType } from "./format";
import {
  type Attachment,
  type CommandError,
  attachFile,
  listAttachments,
  removeAttachment,
} from "./tauri";

export interface AttachmentsStore {
  /** The conversation's draft attachments (backend order). */
  attachments: Attachment[];
  /** Classified error from a load, attach, or remove operation. */
  error: CommandError | null;
  /** Whether an attach or remove operation is in flight. */
  busy: boolean;
  /** Open the native file picker and attach every selected local file. */
  pickAndAttach: () => Promise<void>;
  /** Remove one draft attachment (backend + state). */
  remove: (id: number) => Promise<void>;
  /** Re-read the draft list from the backend (used after a send resolves). */
  refresh: () => Promise<void>;
}

export function useAttachments(conversationId: number | null): AttachmentsStore {
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [error, setError] = useState<CommandError | null>(null);
  const [busy, setBusy] = useState<boolean>(false);

  // Guards against stale responses after rapid conversation switching: only
  // results for the currently selected conversation may update state.
  const activeConversationRef = useRef<number | null>(null);

  const refresh = useCallback(async (): Promise<void> => {
    const requestedId = conversationId;
    if (requestedId === null) {
      setAttachments([]);
      return;
    }
    try {
      const drafts = await listAttachments(requestedId);
      if (activeConversationRef.current !== requestedId) return;
      setAttachments(drafts);
    } catch (e) {
      if (activeConversationRef.current !== requestedId) return;
      setAttachments([]);
      setError(toCommandError(e));
    }
  }, [conversationId]);

  // Load the persisted drafts whenever the conversation is (re)selected.
  useEffect(() => {
    activeConversationRef.current = conversationId;
    setAttachments([]);
    setError(null);
    void refresh();
  }, [conversationId, refresh]);

  const pickAndAttach = useCallback(async (): Promise<void> => {
    if (conversationId === null) return;
    setError(null);
    let selection: string | string[] | null = null;
    try {
      // Native OS file picker; local files only, no upload path exists.
      selection = await open({ multiple: true, title: "Attach files" });
    } catch (e) {
      setError(toCommandError(e));
      return;
    }
    // Cancellation is a normal outcome — nothing to attach.
    if (selection === null) return;

    const paths = Array.isArray(selection) ? selection : [selection];
    setBusy(true);
    try {
      for (const filePath of paths) {
        const fileName = baseName(filePath);
        // File size is optional metadata (DATABASE.md §7.4): stat when the
        // backend can see the picked file, otherwise persist without it.
        let sizeBytes: number | null = null;
        try {
          const meta = await stat(filePath);
          sizeBytes = meta.size;
        } catch {
          sizeBytes = null;
        }
        await attachFile(
          conversationId,
          fileName,
          filePath,
          sizeBytes,
          guessMimeType(fileName),
        );
      }
      // The backend is the source of truth: reload the whole draft list so
      // the visible order/ids match the persisted rows.
      await refresh();
    } catch (e) {
      setError(toCommandError(e));
      // Still refresh so partially attached files are visible.
      await refresh();
    } finally {
      setBusy(false);
    }
  }, [conversationId, refresh]);

  const remove = useCallback(
    async (id: number): Promise<void> => {
      setError(null);
      setBusy(true);
      try {
        await removeAttachment(id);
        await refresh();
      } catch (e) {
        setError(toCommandError(e));
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  return { attachments, error, busy, pickAndAttach, remove, refresh };
}

/** File name of a picked path, handling Windows and POSIX separators. */
function baseName(filePath: string): string {
  const normalized = filePath.replace(/\\/g, "/");
  const lastSlash = normalized.lastIndexOf("/");
  return lastSlash >= 0 ? normalized.slice(lastSlash + 1) : normalized;
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