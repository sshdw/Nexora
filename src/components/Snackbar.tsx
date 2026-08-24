//! Snackbar primitive (0.2.2 component layer).
//!
//! Transient post-action feedback on the inverse-surface pair with at most
//! one action (skill components.md — snackbars). Announced politely via
//! role="status"; actionable snackbars stay visible longer than plain ones.
//! Dismissal is the parent's decision: this component only reports intent,
//! so unmount removes it instantly (the calm-close convention used by every
//! transient surface in 0.2.2).
//!
//! Shipped in 0.2.2 as an unconsumed design-system primitive per the 0.2.1
//! audit (§11.12): wiring it into an existing flow would change product
//! behavior, which later 0.2.x stages own.

import { useEffect, useRef } from "react";

const DEFAULT_TIMEOUT_MS = 4000;
const ACTION_TIMEOUT_MS = 8000;

export interface SnackbarProps {
  message: string;
  /** Optional single action (e.g. "Undo"). */
  actionLabel?: string;
  onAction?: () => void;
  onDismiss: () => void;
  /** Overrides the default auto-dismiss delay. */
  timeoutMs?: number;
}

export default function Snackbar({
  message,
  actionLabel,
  onAction,
  onDismiss,
  timeoutMs,
}: SnackbarProps) {
  const dismissRef = useRef(onDismiss);
  useEffect(() => {
    dismissRef.current = onDismiss;
  }, [onDismiss]);

  useEffect(() => {
    const delay =
      timeoutMs ?? (actionLabel ? ACTION_TIMEOUT_MS : DEFAULT_TIMEOUT_MS);
    const timer = window.setTimeout(() => dismissRef.current(), delay);
    return () => window.clearTimeout(timer);
  }, [actionLabel, timeoutMs]);

  return (
    <div className="nex-snackbar" role="status">
      <span>{message}</span>
      {actionLabel !== undefined && (
        <button
          type="button"
          className="nex-snackbar-action"
          onClick={() => {
            onAction?.();
            dismissRef.current();
          }}
        >
          {actionLabel}
        </button>
      )}
    </div>
  );
}
