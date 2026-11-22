//! Tooltip primitive (library) — #230 Phase 0.
//!
//! A short clarification bubble on the popover surface. Inline mode shows
//! after a hover delay and equally on keyboard focus
//! (`group-focus-within`), so pointer and keyboard parity hold. The bubble
//! is `aria-hidden`: the trigger's `aria-label` always carries the
//! information, so the tooltip is never its sole carrier (SYSTEM §8: no
//! hover-only signal).
//!
//! Painted from shadcn tokens (popover surface + hairline border + the
//! floating-layer shadow), never a `--nex-sys-*` value. The inline reveal
//! is a CSS transition on the global reduced-motion gate; portal mode is
//! shown/hidden by state and adds no transition of its own.
//!
//! `portal` mode (R6-06 defect fix): an always-mounted absolute bubble is
//! laid out inside its trigger's scroll container, so a bubble centered on
//! the last row action widened `.nex-sessions-scroll` from 272 to 313 px
//! even while hidden. In portal mode the bubble is a `position: fixed`
//! sibling of the app in `document.body`; it contributes nothing to the
//! scroller's `scrollWidth`. It is right-aligned to the trigger so it never
//! leaves the viewport on the panel's trailing edge.

import { useRef, useState, type ReactElement } from "react";
import { createPortal } from "react-dom";

import { cn } from "../lib/utils";

export interface TooltipProps {
  /** One-line clarification, mirrored by the trigger's accessible name. */
  label: string;
  children: ReactElement;
  /** Render the bubble in a body portal (for scroll containers). */
  portal?: boolean;
  /** Side relative to the trigger (defaults to "bottom"). */
  side?: "top" | "bottom";
  /** Horizontal alignment in inline mode (defaults to "center"). */
  align?: "center" | "start" | "end";
}

interface BubblePosition {
  top: number;
  left: number;
}

export default function Tooltip({
  label,
  children,
  portal = false,
  side = "bottom",
  align = "center",
}: TooltipProps) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  const [position, setPosition] = useState<BubblePosition | null>(null);

  const show = () => {
    const anchor = anchorRef.current;
    if (anchor === null) return;
    const rect = anchor.getBoundingClientRect();
    setPosition({
      top: side === "top" ? rect.top - 8 : rect.bottom + 6,
      left: rect.right,
    });
  };

  const hide = () => setPosition(null);

  if (!portal) {
    const sideClass =
      side === "top"
        ? "bottom-full mb-2 translate-y-1 group-hover/tooltip:translate-y-0 group-focus-within/tooltip:translate-y-0"
        : "top-full mt-1.5 -translate-y-1 group-hover/tooltip:translate-y-0 group-focus-within/tooltip:translate-y-0";
    const alignClass =
      align === "start"
        ? "left-0"
        : align === "end"
          ? "right-0"
          : "left-1/2 -translate-x-1/2";
    return (
      <span className="group/tooltip relative inline-flex">
        {children}
        <span
          data-slot="tooltip-bubble"
          data-side={side}
          className={cn(
            "pointer-events-none absolute z-50",
            sideClass,
            alignClass,
            "rounded-md border border-border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-[var(--nex-sys-elevation-level1)]",
            "opacity-0 transition-[opacity,transform] duration-150",
            "group-hover/tooltip:opacity-100 group-focus-within/tooltip:opacity-100"
          )}
          aria-hidden="true"
        >
          {label}
        </span>
      </span>
    );
  }

  return (
    <span
      ref={anchorRef}
      className="group/tooltip relative inline-flex"
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {children}
      {createPortal(
        <span
          data-slot="tooltip-bubble"
          data-side={side}
          className={cn(
            "pointer-events-none fixed z-50 -translate-x-full",
            side === "top" && "-translate-y-full",
            "rounded-md border border-border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-[var(--nex-sys-elevation-level1)]",
            position === null ? "hidden" : "block"
          )}
          style={
            position === null
              ? undefined
              : { top: position.top, left: position.left }
          }
          aria-hidden="true"
        >
          {label}
        </span>,
        document.body
      )}
    </span>
  );
}
