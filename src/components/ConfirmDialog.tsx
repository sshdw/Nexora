//! Confirmation dialog (0.3.0 visual reset).
//!
//! Shared destructive/irreversible-action confirmation built on the
//! ModalShell primitive, replacing the previous native window.confirm
//! chrome so the whole flow stays inside Nexora's dialog system
//! (same behavior: explicit confirm required, cancel path, focus
//! management). The confirm action is the filled destructive style —
//! intensification happens only at the confirm step (NEXORA
//! ADAPTATION), never as idle styling.

import ModalShell from "./Modal";

export interface ConfirmDialogProps {
  title: string;
  body: string;
  confirmLabel: string;
  cancelLabel?: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function ConfirmDialog({
  title,
  body,
  confirmLabel,
  cancelLabel = "Cancel",
  danger = false,
  busy = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  return (
    <ModalShell title={title} busy={busy} onClose={onCancel}>
      <div className="nex-io-body">
        <p className="nex-io-hint">{body}</p>
      </div>
      <div className="nex-dialog-actions">
        <button
          type="button"
          className="nex-btn nex-btn-ghost"
          onClick={onCancel}
          disabled={busy}
        >
          {cancelLabel}
        </button>
        <button
          type="button"
          className={
            "nex-btn " + (danger ? "nex-btn-danger-filled" : "nex-btn-primary")
          }
          onClick={onConfirm}
          disabled={busy}
          aria-busy={busy}
        >
          {busy ? <span className="nex-spinner" aria-hidden="true" /> : null}
          {confirmLabel}
        </button>
      </div>
    </ModalShell>
  );
}
