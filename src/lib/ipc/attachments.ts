//! Attachment IPC area (AC-205): draft attachment rows (DATABASE.md §7.4)
//! and their commands (FR-008).

import { invoke } from "./core";

/** One persisted `attachments` row (DATABASE.md §7.4). A draft attachment has
 * `message_id: null`; a sent attachment carries the user message it belongs
 * to. The absolute `file_path` is backend bookkeeping and is never rendered. */
export interface Attachment {
  id: number;
  conversation_id: number;
  message_id: number | null;
  file_name: string;
  file_path: string;
  file_size_bytes: number | null;
  mime_type: string | null;
}

/** Attach a local-file reference to the conversation as a draft attachment via
 * the existing `attach_file` command (FR-008). No content is uploaded; only
 * metadata (name, path, size, media type) is persisted locally.
 *
 * SEC-01: the file size is derived BACKEND-side from the picked file's own
 * metadata and returned in the persisted `Attachment` row — the renderer
 * supplies only the dialog-picked `filePath`, the display `fileName`, and the
 * optional `mimeType` hint, and holds no `fs` plugin grant to stat the file
 * itself. */
export function attachFile(
  conversationId: number,
  fileName: string,
  filePath: string,
  mimeType: string | null
): Promise<Attachment> {
  return invoke<Attachment>("attach_file", {
    conversationId,
    fileName,
    filePath,
    mimeType,
  });
}

/** List the conversation's draft attachments (`message_id` IS NULL) via
 * `list_attachments`. Historical, message-linked rows are not included. */
export function listAttachments(conversationId: number): Promise<Attachment[]> {
  return invoke<Attachment[]>("list_attachments", { conversationId });
}

/** Hard-delete one draft attachment via `remove_attachment`. Other rows,
 * messages, and the conversation are untouched. */
export function removeAttachment(id: number): Promise<void> {
  return invoke<void>("remove_attachment", { id });
}
