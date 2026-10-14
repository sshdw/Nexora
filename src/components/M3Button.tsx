//! Common button (library primitive) — #230 Phase 0.
//!
//! The canonical button, now a thin adapter over the vendored
//! `ui/button` (NEXORA-DESIGN-SYSTEM.md §3: "Button (`M3Button` →
//! migrate to `ui/button`)"): this module owns only the Nexora
//! variant vocabulary, the loading state and the block/filled
//! modifiers, while `ui/button` owns the paint — every colour
//! resolves to a shadcn token in src/index.css, so the button is on
//! the black theme with no `--nex-sys-*` reference of its own.
//!
//! Variant mapping (contract button canon):
//!   primary     → `default`   (the one accent fill, one per region)
//!   secondary   → `secondary` (neutral tonal container action)
//!   quiet       → `ghost`     (text-only action)
//!   destructive → `destructive` (tinted at rest; `filled` intensifies
//!                 it to the solid error fill at the confirm step only —
//!                 NEXORA ADAPTATION: destructive never idles filled)
//!
//! `expressive` is retained for API compatibility: the press
//! feedback it used to add is now the base button's own active
//! translate, so no extra class is emitted and no caller changes.
//!
//! Loading disables the control, sets `aria-busy` and shows the
//! inline spinner beside the label — the label keeps the
//! information under reduced motion, when the spin freezes.

import type { ButtonHTMLAttributes, ReactNode } from "react";

import { Button } from "./ui/button";
import { Spinner } from "./ui/spinner";

/** Variant mapping (contract button canon): primary = filled action
 * (one per region) · secondary = tonal container action · quiet =
 * text-only action · destructive = error-toned action (text at rest;
 * filled only at the confirm step via `filled`). */
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
   * shows the inline spinner beside the label. Reduced motion: the spin
   * freezes and the label text carries the state. */
  loading?: boolean;
  /** Retained for API compatibility (see the module note). */
  expressive?: boolean;
  /** Full-width block layout (e.g. the sidebar creation action). */
  block?: boolean;
  /** Intensified confirm step for destructive actions (solid error
   * fill). NEXORA ADAPTATION: destructive intensifies only at confirm. */
  filled?: boolean;
  children: ReactNode;
}

const VARIANT: Record<M3ButtonVariant, "default" | "secondary" | "ghost" | "destructive"> = {
  primary: "default",
  secondary: "secondary",
  quiet: "ghost",
  destructive: "destructive",
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
  // `expressive` is accepted for API compatibility and deliberately has
  // no effect: the press feedback it used to add is now the base
  // button's own active translate. Referenced here so the linter does
  // not flag the destructured prop, and never forwarded to the DOM.
  void expressive;
  if (import.meta.env.DEV && filled && variant !== "destructive") {
    console.warn(
      `[M3Button] "filled" is ignored for variant "${variant}" (only "destructive" supports it).`,
    );
  }
  const classes = [
    block ? "w-full" : "",
    filled && variant === "destructive"
      ? "bg-destructive text-destructive-foreground hover:bg-destructive/90"
      : "",
    className,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Button
      type={type}
      variant={VARIANT[variant]}
      size={size === "sm" ? "sm" : "default"}
      className={classes}
      disabled={busy}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading && <Spinner data-icon="inline-start" aria-hidden="true" />}
      {children}
    </Button>
  );
}
