//! Snackbar host: the single undo-capable transient-feedback surface
//! (NEX-UX-003, NEX-UI-001).
//!
//! `Snackbar` shipped as an unconsumed primitive; `SnackbarProvider` mounts
//! the one fixed host region (App root, above Sidebar + every overlay) and
//! `useSnackbar().showSnack` is how delete/archive handlers offer a bounded
//! Undo window. At most one snackbar is visible: a new `showSnack` replaces
//! the previous one (the superseded undo already committed or still commits
//! through its own owner — the host never owns the action, it only reports
//! intent, same calm-close convention as the primitive).
//!
//! Bounded window: undo-capable callers pass `UNDO_WINDOW_MS` (the same
//! delay the primitive auto-dismisses actionable snackbars with), so the
//! action and its undo affordance expire together. No new motion: the
//! primitive's enter is covered by the global prefers-reduced-motion gate
//! (motion.css), dismissal is instant unmount.

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";

import Snackbar from "./Snackbar";

/** Bounded Undo window, ms — matches the primitive's actionable timeout so
 * the deferred action commits exactly when its snackbar expires. */
export const UNDO_WINDOW_MS = 8000;

export interface ShowSnackOptions {
  /** Optional single action (e.g. localized "Undo"). */
  actionLabel?: string;
  /** Runs when the action is pressed (before dismissal). */
  onAction?: () => void;
  /** Overrides the auto-dismiss delay (defaults to the primitive's). */
  timeoutMs?: number;
}

interface SnackState {
  key: number;
  message: string;
  actionLabel?: string;
  onAction?: () => void;
  timeoutMs?: number;
}

export interface SnackbarHost {
  showSnack: (message: string, options?: ShowSnackOptions) => void;
  dismissSnack: () => void;
}

const SnackbarContext = createContext<SnackbarHost | null>(null);

export function useSnackbar(): SnackbarHost {
  const host = useContext(SnackbarContext);
  if (host === null) {
    throw new Error("useSnackbar must be used inside SnackbarProvider");
  }
  return host;
}

export function SnackbarProvider({ children }: { children: ReactNode }) {
  const [snack, setSnack] = useState<SnackState | null>(null);
  const keyRef = useRef(0);

  const dismissSnack = useCallback(() => {
    setSnack(null);
  }, []);

  const showSnack = useCallback((message: string, options?: ShowSnackOptions) => {
    keyRef.current += 1;
    const key = keyRef.current;
    setSnack({
      key,
      message,
      actionLabel: options?.actionLabel,
      onAction: options?.onAction,
      timeoutMs: options?.timeoutMs,
    });
  }, []);

  const value = useMemo(
    () => ({ showSnack, dismissSnack }),
    [showSnack, dismissSnack],
  );

  return (
    <SnackbarContext.Provider value={value}>
      {children}
      {snack !== null && (
        <Snackbar
          key={snack.key}
          message={snack.message}
          actionLabel={snack.actionLabel}
          onAction={snack.onAction}
          onDismiss={dismissSnack}
          timeoutMs={snack.timeoutMs}
        />
      )}
    </SnackbarContext.Provider>
  );
}
