//! Import / Export modals (FR-010, FR-011).
//!
//! Presentation only: the dialogs render through the shared ModalShell
//! primitive (0.2.2 component layer — .nex-dialog* token system plus
//! Esc/trap/initial-focus/restore semantics) and delegate all behavior to
//! the `useImportExport` hook, which in turn calls the existing backend
//! commands. Export is read-only against stored data; import is atomic in
//! the backend, so a failed import leaves no partial rows.

import M3Button from "./M3Button";
import ModalShell from "./Modal";
import { useStrings } from "../lib/useLocale";
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
  const { t } = useStrings();

  const runExport = () => {
    void exportTo(conversationId, conversationTitle);
  };

  return (
    <ModalShell title={t("io.exportTitle")} busy={busy} onClose={onClose}>
      <div className="nex-io-body">
        {error && (
          <p className="nex-dialog-error nex-fade-in" role="alert">
            {error.message}
          </p>
        )}
        {exportSucceeded && !error && (
          <p className="nex-io-status is-ok nex-fade-in" role="status">
            {t("io.exportOk")}
          </p>
        )}
        <p className="nex-io-hint">
          {t("io.exportHint", { title: conversationTitle })}
        </p>
      </div>
      <div className="nex-dialog-actions">
        <M3Button variant="quiet" onClick={onClose} disabled={busy}>
          {exportSucceeded ? t("common.done") : t("common.cancel")}
        </M3Button>
        <M3Button
          variant="primary"
          loading={busy}
          onClick={runExport}
        >
          {busy ? t("io.exporting") : exportSucceeded ? t("io.exportAgain") : t("io.chooseLocation")}
        </M3Button>
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
  const { t } = useStrings();

  const runImport = () => {
    void importFrom().then((newId) => {
      if (newId !== null) onImported(newId);
    });
  };

  return (
    <ModalShell title={t("io.importTitle")} busy={busy} onClose={onClose}>
      <div className="nex-io-body">
        {error && (
          <p className="nex-dialog-error nex-fade-in" role="alert">
            {error.message}
          </p>
        )}
        {importedId !== null && !error && (
          <p className="nex-io-status is-ok nex-fade-in" role="status">
            {t("io.importOk")}
          </p>
        )}
        <p className="nex-io-hint">
          {t("io.importHint")}
        </p>
      </div>
      <div className="nex-dialog-actions">
        <M3Button variant="quiet" onClick={onClose} disabled={busy}>
          {importedId !== null ? t("common.done") : t("common.cancel")}
        </M3Button>
        <M3Button
          variant="primary"
          loading={busy}
          onClick={runImport}
        >
          {busy ? t("io.importing") : t("io.chooseFile")}
        </M3Button>
      </div>
    </ModalShell>
  );
}
