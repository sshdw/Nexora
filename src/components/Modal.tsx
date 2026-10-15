//! Dialog primitive (library) — #230 Phase 0.
//!
//! Shared modal chrome for every screen. This module owns only the
//! accessibility semantics the audit requires of dialogs: Esc to
//! close, initial focus, Tab cycling trapped inside the card, focus
//! restoration on close, and background inertness. It renders no
//! business logic — callers keep full control of flows.
//!
//! While `busy`, Esc and backdrop clicks are ignored so an in-flight
//! operation cannot be dismissed mid-write (existing behavior
//! preserved).
//!
//! #230 Phase 0 moves the chrome onto the black theme's tokens
//! (`--popover`/`--popover-foreground`/`--border`/`--background`
//! scrim) and a single hairline border + floating-layer shadow — no
//! `--nex-sys-*` value and no `nex-*` class remains. The exit is a
//! plain opacity transition on the utilities layer, collapsed by the
//! global reduced-motion gate; `EXIT_MS` stays in sync with it.

import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

import { cn } from "../lib/utils";

const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

/** Exit-animation window. Mirrors the opacity transition below;
 * collapses to 0 when the user prefers reduced motion (the global CSS
 * gate already renders the exit instantly). */
const EXIT_MS = matchMedia("(prefers-reduced-motion: reduce)").matches
  ? 0
  : 200;

export interface ModalShellProps {
  title: string;
  onClose: () => void;
  children: ReactNode;
  /** When true the dialog cannot be dismissed (operation in flight). */
  busy?: boolean;
  /** Top-anchored variant for launchers (command palette): the card docks
   * near the viewport top instead of centering. */
  align?: "center" | "top";
}

export default function ModalShell({
  title,
  onClose,
  busy = false,
  align = "center",
  children,
}: ModalShellProps) {
  const cardRef = useRef<HTMLDivElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);
  const restoreRef = useRef<HTMLElement | null>(null);
  // Exit motion: the shell stays mounted while the card/scrim fade,
  // then the parent's onClose unmounts for real. Purely presentational —
  // dismissal semantics (Esc/backdrop/Cancel) and focus restoration are
  // unchanged.
  const [closing, setClosing] = useState(false);
  const closingRef = useRef(false);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const requestClose = () => {
    if (busy || closingRef.current) return;
    closingRef.current = true;
    setClosing(true);
  };

  useEffect(() => {
    if (!closing) return;
    const timer = window.setTimeout(() => onCloseRef.current(), EXIT_MS);
    return () => window.clearTimeout(timer);
  }, [closing]);

  useEffect(() => {
    // Capture the restore target before inerting: if focus currently sits in
    // the background, the inert walk below will move it to <body>.
    restoreRef.current = document.activeElement as HTMLElement | null;

    // Background inertness: keep only the ancestor path to the dialog live
    // and mark every off-path sibling at each level `inert` (pointer events
    // and AT virtual cursor both excluded). The walk stops below
    // <html> so <head> is never touched.
    const inerted: HTMLElement[] = [];
    let node: HTMLElement | null = backdropRef.current;
    while (node) {
      const parent = node.parentElement;
      if (!parent || parent === document.documentElement) break;
      for (const child of Array.from(parent.children)) {
        if (child !== node && child instanceof HTMLElement && !child.inert) {
          child.inert = true;
          inerted.push(child);
        }
      }
      node = parent;
    }

    const card = cardRef.current;
    // Initial focus: keep it if the browser already placed it (an
    // autoFocus child); otherwise the first focusable control, falling
    // back to the card so Tab starts the cycle from a known position.
    if (card && !card.contains(document.activeElement)) {
      const first = card.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      (first ?? card).focus();
    }
    return () => {
      for (const element of inerted) element.inert = false;
      const previous = restoreRef.current;
      if (previous && previous.isConnected) previous.focus();
    };
  }, []);

  // shortcut:dialog.close / shortcut:dialog.trap — dialog-owned keys stay
  // inline in the shared shell (every dialog inherits them).
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      if (busy) return;
      event.stopPropagation();
      requestClose();
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
      ref={backdropRef}
      className={cn(
        "fixed inset-0 z-50 flex bg-background/80 p-4 transition-opacity duration-200",
        align === "top" ? "items-start justify-center pt-12" : "items-center justify-center",
        closing ? "opacity-0" : "opacity-100",
      )}
      role="presentation"
      onClick={busy ? undefined : requestClose}
      onKeyDown={handleKeyDown}
    >
      <div
        ref={cardRef}
        className={cn(
          "relative w-full max-w-lg rounded-lg border border-border bg-popover p-5 text-popover-foreground shadow-lg outline-none",
          "transition-opacity duration-200",
          closing ? "opacity-0" : "opacity-100",
        )}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
      >
        <h3 className="mb-3 text-base font-medium text-foreground">{title}</h3>
        {children}
      </div>
    </div>
  );
}
