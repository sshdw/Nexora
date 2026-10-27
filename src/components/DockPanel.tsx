//! Dock panel shell — the overlay surface a Dock destination opens (R4-01/D-06).
//!
//! The Dock is the only navigation in the shell, so every one of its five
//! destinations has to lead somewhere. This is the shared surface those
//! destinations open into: a 300 px overlay card that floats beside the rail
//! and never moves the thread or the composer (D-06: "панель открывается
//! поверх workspace и не сдвигает thread/composer").
//!
//! Geometry (register §6.2, re-anchored to the edge-attached rail of R4-01):
//!   width 300 px · radius 12 (shape.medium) · overlay surface · one soft
//!   shadow · no glass, no blur. It sits one spacing step to the right of the
//!   rail and is vertically centred with it, so the pair reads as one object.
//!
//! Dismissal is the register's three ways — re-click the Dock button (the
//! owner of that gesture is Dock/App), a click outside, and Escape. A close
//! button is added as the fourth, because the other three all assume a
//! pointer or a keyboard and the panel is a real surface, not a tooltip.
//!
//! Focus: opening moves focus into the panel (its first control, or the panel
//! itself when it has none); closing returns it to the Dock button that opened
//! it. The panel is non-modal — `aria-modal` is false, the thread behind it
//! stays readable and reachable, which is what D-06 asks for.

import { useEffect, useId, useRef } from "react";
import type { ReactNode } from "react";

import { useStrings } from "../lib/useLocale";
import M3IconButton from "./M3IconButton";
import { CloseIcon } from "./icons";

export type DockPanelId = "workspace" | "sessions" | "activity" | "settings";

export interface DockPanelProps {
  /** Which destination this panel belongs to (drives ids + focus return). */
  id: DockPanelId;
  title: string;
  /** Quiet count/summary on the header's trailing edge, when there is one. */
  meta?: string;
  onClose: () => void;
  children: ReactNode;
}

export default function DockPanel({ id, title, meta, onClose, children }: DockPanelProps) {
  const { t } = useStrings();
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();

  // Dismissal: Escape from anywhere, and any pointer press outside the card
  // that is not on the Dock itself (re-clicking the Dock button is the Dock's
  // own toggle — closing here as well would close and reopen it in one click).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      // A dialog on top (Settings modal, command palette) owns Escape first.
      if (document.querySelector('[role="dialog"][aria-modal="true"]') !== null) return;
      event.preventDefault();
      onClose();
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (target === null) return;
      if (ref.current?.contains(target)) return;
      if (target instanceof Element && target.closest(".nex-dock") !== null) return;
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    // `pointerdown`, not `click`: the panel should be gone before the thing
    // underneath reacts, so one click outside dismisses without also
    // activating whatever it landed on.
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, [onClose]);

  // Focus moves in on open and back to the trigger on close. The trigger is
  // found by data attribute rather than held as a ref because the Dock owns
  // its own buttons; this keeps the two components independent.
  //
  // The landing spot is the first control in the panel's BODY — its search
  // field, its one action, its first row — never the close button in the
  // header: "the first focusable" put the ring on ×, which read as "this is
  // selected" and popped its tooltip over the panel's own content. A panel
  // with no control at all focuses its own box, which is why the box carries
  // tabIndex={-1}.
  useEffect(() => {
    const node = ref.current;
    if (node === null) return;
    const body = node.querySelector<HTMLElement>(".nex-dock-panel-body");
    const target =
      body?.querySelector<HTMLElement>("[data-dock-panel-autofocus]") ??
      body?.querySelector<HTMLElement>(
        'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      ) ??
      node;
    target.focus();
    const triggerId = id;
    return () => {
      document
        .querySelector<HTMLElement>(`[data-dock-trigger="${triggerId}"]`)
        ?.focus();
    };
  }, [id]);

  return (
    <div
      ref={ref}
      className="nex-dock-panel"
      role="dialog"
      aria-modal="false"
      aria-labelledby={titleId}
      tabIndex={-1}
    >
      <header className="nex-dock-panel-head">
        <h2 className="nex-dock-panel-title" id={titleId}>
          {title}
        </h2>
        {meta !== undefined && <span className="nex-dock-panel-meta">{meta}</span>}
        <M3IconButton
          label={t("common.close")}
          size="sm"
          className="nex-dock-panel-close"
          onClick={onClose}
        >
          <CloseIcon />
        </M3IconButton>
      </header>
      <div className="nex-dock-panel-body">{children}</div>
    </div>
  );
}
