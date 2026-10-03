//! Confirmation dialog (0.3.0 visual reset).
//!
//! Shared destructive/irreversible-action confirmation built on the
//! ModalShell primitive, replacing the previous native window.confirm
//! chrome so the whole flow stays inside Nexora's dialog system
//! (same behavior: explicit confirm required, cancel path, focus
//! management). The confirm action is the filled destructive style —
//! intensification happens only at the confirm step (NEXORA
//! ADAPTATION), never as idle styling.

import M3Button from "./M3Button";
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
        <M3Button variant="quiet" onClick={onCancel} disabled={busy}>
          {cancelLabel}
        </M3Button>
        <M3Button
          variant={danger ? "destructive" : "primary"}
          filled={danger}
          loading={busy}
          onClick={onConfirm}
        >
          {confirmLabel}
        </M3Button>
      </div>
    </ModalShell>
  );
}
