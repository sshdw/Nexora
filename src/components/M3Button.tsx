//! M3E common button (library primitive).
//!
//! OFFICIAL common-button grammar (skill components.md "Buttons"): label-large
//! 14/500, emphasized weight for the primary only, one primary per region,
//! press = 12% state layer (+ optional fast-spatial micro-morph on the
//! whitelisted send/create/confirm moments). Rests on the shared .nex-btn*
//! system in components.css — this module owns variant mapping, the loading
//! state (spinner + label, control disabled, aria-busy) and keyboard parity
//! (native <button>: Enter/Space). Reduced motion: transitions collapse via
//! the motion.css gate; the loading spinner freezes into a static ring while
//! the visible label keeps the information.

import type { ButtonHTMLAttributes, ReactNode } from "react";

/** Variant mapping (contract button canon, [player]/[doc-cards]):
 * primary = filled action (one per region) · secondary = tonal container
 * action · quiet = text-only action · destructive = error-toned action
 * (text at rest; filled only at the confirm step via `filled`). */
export type M3ButtonVariant =
  | "primary"
  | "secondary"
  | "quiet"
  | "destructive";

export interface M3ButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  variant?: M3ButtonVariant;
  size?: "md" | "sm";
  /** Busy work in flight: disables the control, announces aria-busy and
   * shows the inline spinner beside the label (skill components.md §1
   * loading row). Reduced motion: static ring + label text. */
  loading?: boolean;
  /** Press micro-morph (scale + radius easing on the OFFICIAL
   * expressive-fast-spatial pair). Reserved for send/create/confirm
   * moments only — never for quiet/reading-surface actions. */
  expressive?: boolean;
  /** Full-width block layout (e.g. the sidebar creation action). */
  block?: boolean;
  /** Intensified confirm step for destructive actions (filled error fill).
   * NEXORA ADAPTATION: destructive intensifies only at confirm, never idle. */
  filled?: boolean;
  children: ReactNode;
}

const VARIANT_CLASS: Record<M3ButtonVariant, string> = {
  primary: "nex-btn-primary",
  secondary: "nex-btn-tonal",
  quiet: "nex-btn-ghost",
  destructive: "nex-btn-danger",
};

export default function M3Button({
  variant = "quiet",
  size = "md",
  loading = false,
  expressive = false,
  block = false,
  filled = false,
  disabled = false,
  className = "",
  children,
  type = "button",
  ...rest
}: M3ButtonProps) {
  const busy = disabled || loading;
  const classes = [
    "nex-btn",
    filled && variant === "destructive"
      ? "nex-btn-danger-filled"
      : VARIANT_CLASS[variant],
    size === "sm" ? "nex-btn-sm" : "",
    expressive ? "nex-btn-expressive" : "",
    block ? "nex-m3-btn-block" : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      type={type}
      className={classes}
      disabled={busy}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading && <span className="nex-spinner" aria-hidden="true" />}
      {children}
    </button>
  );
}
