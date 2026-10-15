//! Icon button (library primitive) — #230 Phase 0.
//!
//! The canonical icon-only control, an adapter over the vendored
//! `ui/button` (ghost variant, icon sizes). `label` is required: it
//! becomes both the aria-label and the tooltip text, so icon-only
//! controls always carry an accessible name with keyboard-focus
//! parity. Sizes: sm 28 (in-row actions) · md 32 (headers) · lg 36.
//! `danger` keeps the error tone on hover/focus only (never idle
//! colour), matching the destructive-role discipline.

import type { ButtonHTMLAttributes, ReactNode } from "react";

import { Button } from "./ui/button";
import Tooltip from "./Tooltip";

export interface M3IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  /** Accessible name (required): aria-label + tooltip + title. */
  label: string;
  size?: "sm" | "md" | "lg";
  /** Error tone on hover/focus only (never idle colour). */
  danger?: boolean;
  children: ReactNode;
}

const SIZE = { sm: "icon-sm", md: "icon", lg: "icon-lg" } as const;

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
    danger ? "hover:text-destructive focus-visible:text-destructive" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Tooltip label={label}>
      <Button
        type={type}
        variant="ghost"
        size={SIZE[size]}
        className={classes}
        aria-label={label}
        title={label}
        {...rest}
      >
        {children}
      </Button>
    </Tooltip>
  );
}
