//! M3E icon button (library primitive).
//!
//! OFFICIAL grammar (skill components.md "Icon buttons"): quiet at rest,
//! accent only on small interactive states, fast-effects color/state fades,
//! press = 12% layer. `label` is required: it becomes both the aria-label
//! and the tooltip text, so icon-only controls always carry an accessible
//! name with keyboard-focus parity (skill components.md + accessibility.md).
//! Sizes: sm 28 (in-row actions) · md 36 (headers) · lg 44. Reduced motion:
//! state fades collapse via the motion.css gate.

import type { ButtonHTMLAttributes, ReactNode } from "react";

import Tooltip from "./Tooltip";

export interface M3IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  /** Accessible name (required): aria-label + tooltip + title. */
  label: string;
  size?: "sm" | "md" | "lg";
  /** Error tone on hover/press only (never idle color). */
  danger?: boolean;
  children: ReactNode;
}

export default function M3IconButton({
  label,
  size = "md",
  danger = false,
  className = "",
  children,
  type = "button",
  ...rest
}: M3IconButtonProps) {
  const classes = [
    "nex-icon-btn",
    size === "sm" ? "nex-icon-btn-sm" : "",
    size === "lg" ? "nex-icon-btn-lg" : "",
    danger ? "nex-icon-btn-danger" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Tooltip label={label}>
      <button
        type={type}
        className={classes}
        aria-label={label}
        title={label}
        {...rest}
      >
        {children}
      </button>
    </Tooltip>
  );
}
