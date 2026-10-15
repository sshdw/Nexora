//! Tooltip primitive (library) — #230 Phase 0.
//!
//! A short clarification bubble on the popover surface. Shows after a
//! hover delay and equally on keyboard focus (`group-focus-within`),
//! so pointer and keyboard parity hold. The bubble is `aria-hidden`:
//! the trigger's `aria-label` always carries the information, so the
//! tooltip is never its sole carrier (SYSTEM §8: no hover-only
//! signal).
//!
//! Painted from shadcn tokens (popover surface + hairline border +
//! the floating-layer shadow), never a `--nex-sys-*` value. The
//! reveal is a CSS transition on the global reduced-motion gate.

import type { ReactElement } from "react";

import { cn } from "../lib/utils";

export interface TooltipProps {
  /** One-line clarification, mirrored by the trigger's accessible name. */
  label: string;
  children: ReactElement;
}

export default function Tooltip({ label, children }: TooltipProps) {
  return (
    <span className="group/tooltip relative inline-flex">
      {children}
      <span
        className={cn(
          "pointer-events-none absolute top-full left-1/2 z-50 mt-1.5 -translate-x-1/2",
          "rounded-md border border-border bg-popover px-2 py-1 text-xs whitespace-nowrap text-popover-foreground shadow-md",
          "opacity-0 transition-opacity duration-150",
          "group-hover/tooltip:opacity-100 group-focus-within/tooltip:opacity-100",
        )}
        aria-hidden="true"
      >
        {label}
      </span>
    </span>
  );
}
