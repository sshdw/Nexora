//! Tooltip primitive (0.2.2 component layer).
//!
//! A short clarification bubble on the inverse-surface pair. Shows after a
//! hover delay and equally on keyboard focus (:focus-within), so pointer
//! and keyboard parity hold (skill accessibility.md §1). The bubble is
//! aria-hidden: the trigger's aria-label always carries the information,
//! so the tooltip is never its sole carrier.

import type { ReactElement } from "react";

export interface TooltipProps {
  /** One-line clarification, mirrored by the trigger's accessible name. */
  label: string;
  children: ReactElement;
}

export default function Tooltip({ label, children }: TooltipProps) {
  return (
    <span className="nex-tooltip">
      {children}
      <span className="nex-tooltip-bubble" aria-hidden="true">
        {label}
      </span>
    </span>
  );
}
