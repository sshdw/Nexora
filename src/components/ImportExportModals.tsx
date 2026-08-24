//! Import / Export modals (FR-010, FR-011).
//!
//! Presentation only: the dialogs render through the shared ModalShell
//! primitive (0.2.2 component layer — .nex-dialog* token system plus
//! Esc/trap/initial-focus/restore semantics) and delegate all behavior to
//! the `useImportExport` hook, which in turn calls the existing backend
//! commands. Export is read-only against stored data; import is atomic in
//! the backend, so a failed import leaves no partial rows.

import ModalShell from "./Modal";
import type { ImportExportStore } from "../lib/useImportExport";

export interface ExportModalProps {
  conversationId: number;
  conversationTitle: string;
  store: ImportExportStore;
  onClose: () => void;
}

export function ExportModal({
  conversationId,
  conversationTitle,
  store,
  onClose,
}: ExportModalProps) {
  const { busy, error, exportSucceeded, exportTo } = store;

  const runExport = () => {
    void exportTo(conversationId, conversationTitle);
  };

  return (
    <ModalShell title="Export conversation" busy={busy} onClose={onClose}>
      {error && (
        <p className="nex-dialog-error" role="alert">
          {error.message}
        </p>
      )}
      {exportSucceeded && !error && (
        <p className="nex-io-status is-ok" role="status">
          Export complete. The conversation was written to the file you chose.
          Stored data was not modified.
        </p>
      )}
      <p className="nex-io-hint">
        Saves “{conversationTitle}” as a local Nexora conversation JSON file.
        Messages are exported in their stored order.
      </p>
      <div className="nex-dialog-actions">
        <button
          type="button"
          className="nex-btn nex-btn-ghost"
          onClick={onClose}
          disabled={busy}
        >
          {exportSucceeded ? "Done" : "Cancel"}
        </button>
        <button
          type="button"
          className="nex-btn nex-btn-primary"
          onClick={runExport}
          disabled={busy}
          aria-busy={busy}
        >
          {busy ? "Exporting…" : exportSucceeded ? "Export again" : "Choose location"}
        </button>
      </div>
    </ModalShell>
  );
}

export interface ImportModalProps {
  store: ImportExportStore;
  /** Called after a conversation was imported so the list reloads and the
   * new conversation is opened. */
  onImported: (newId: number) => void;
  onClose: () => void;
}

export function ImportModal({ store, onImported, onClose }: ImportModalProps) {
  const { busy, error, importedId, importFrom } = store;

  const runImport = () => {
    void importFrom().then((newId) => {
      if (newId !== null) onImported(newId);
    });
  };

  return (
    <ModalShell title="Import conversation" busy={busy} onClose={onClose}>
      {error && (
        <p className="nex-dialog-error" role="alert">
          {error.message}
        </p>
      )}
      {importedId !== null && !error && (
        <p className="nex-io-status is-ok" role="status">
          Import complete. The conversation was added to your list.
        </p>
      )}
      <p className="nex-io-hint">
        Choose a Nexora conversation export file (.json). The file is validated
        before anything is written; an unsupported file is rejected without
        leaving partial data behind.
      </p>
      <div className="nex-dialog-actions">
        <button
          type="button"
          className="nex-btn nex-btn-ghost"
          onClick={onClose}
          disabled={busy}
        >
          {importedId !== null ? "Done" : "Cancel"}
        </button>
        <button
          type="button"
          className="nex-btn nex-btn-primary"
          onClick={runImport}
          disabled={busy}
          aria-busy={busy}
        >
          {busy ? "Importing…" : "Choose file"}
        </button>
      </div>
    </ModalShell>
  );
}
