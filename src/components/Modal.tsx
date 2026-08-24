//! Dialog primitive (0.2.2 component layer).
//!
//! Shared modal chrome for every screen: scrim, elevated card, title and
//! action row come from the .nex-dialog* token-driven system in
//! components.css; this module owns only the accessibility semantics the
//! audit requires of dialogs (§9.5): Esc to close, initial focus, Tab
//! cycling trapped inside the card, and focus restoration on close.
//! It renders no business logic — callers keep full control of flows.
//!
//! While `busy`, Esc and backdrop clicks are ignored so an in-flight
//! operation cannot be dismissed mid-write (existing behavior preserved).

import { useEffect, useRef, type KeyboardEvent, type ReactNode } from "react";

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

export interface ModalShellProps {
  title: string;
  onClose: () => void;
  children: ReactNode;
  /** When true the dialog cannot be dismissed (operation in flight). */
  busy?: boolean;
}

export default function ModalShell({
  title,
  onClose,
  busy = false,
  children,
}: ModalShellProps) {
  const cardRef = useRef<HTMLDivElement>(null);
  const restoreRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    restoreRef.current = document.activeElement as HTMLElement | null;
    const card = cardRef.current;
    // Initial focus: keep it if the browser already placed it (an
    // autoFocus child); otherwise the first focusable control, falling
    // back to the card so Tab starts the cycle from a known position.
    if (card && !card.contains(document.activeElement)) {
      const first = card.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      (first ?? card).focus();
    }
    return () => {
      const previous = restoreRef.current;
      if (previous && previous.isConnected) previous.focus();
    };
  }, []);

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      if (busy) return;
      event.stopPropagation();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    // Focus trap: cycle Tab/Shift+Tab within the card.
    const card = cardRef.current;
    if (!card) return;
    const focusable = Array.from(
      card.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    );
    if (focusable.length === 0) {
      event.preventDefault();
      card.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey) {
      if (active === first || active === card) {
        event.preventDefault();
        last.focus();
      }
    } else if (active === last || !card.contains(active)) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div
      className="nex-dialog-backdrop"
      role="presentation"
      onClick={busy ? undefined : onClose}
      onKeyDown={handleKeyDown}
    >
      <div
        ref={cardRef}
        className="nex-dialog-card"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
      >
        <h3 className="nex-dialog-title">{title}</h3>
        {children}
      </div>
    </div>
  );
}
