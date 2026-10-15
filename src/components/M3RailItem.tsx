//! Navigation-rail item (library primitive — the sidebar row) — #230 Phase 0.
//!
//! The rail grammar as Nexora's sidebar embodies it: one full-width
//! row with an optional leading icon and a label. Active = a thin
//! light indicator at the leading edge plus a surface tone step —
//! never a colour fill (MAP row 1: "Active row = thin indicator +
//! surface shift, never amber fill"). `aria-current` announces the
//! destination; native `<button>` keeps Enter/Space parity.
//!
//! Painted from shadcn tokens (`--accent`/`--muted`/`--foreground`/
//! `--muted-foreground`/`--primary`), never a `--nex-sys-*` value.
//! The tone fades are CSS transitions the global
//! reduced-motion gate collapses.
//!
//! `trailing` (#230 Phase 1) is a second slot OUTSIDE the
//! truncating label span. A row that composes a truncating title
//! with a fixed meta line (MAP row 3: "title (ellipsis) + relative
//! date muted") cannot put the date inside the label span: that span
//! IS the ellipsis box, so a child date ran straight into the title
//! with no gap and no flex context — the exact defect P1 inherited
//! ("Новый разговорТолько что"). The date is also the caller's to
//! keep `shrink-0`: only the title may be compressed, so the date
//! can never be the part that clips.

import type { ButtonHTMLAttributes, ReactNode } from "react";

import { cn } from "../lib/utils";

export interface M3RailItemProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  /** Visible + accessible label (aria-label + title). */
  label: string;
  /** Outlined/rest icon (optional — text-only rows omit it). */
  icon?: ReactNode;
  /** Filled variant shown while active (falls back to `icon`). */
  activeIcon?: ReactNode;
  /** Selected destination: tone step + thin light indicator + aria-current. */
  active?: boolean;
  /** Meta line after the label — relative date, count, status. Rendered
   * beside the truncating label, never inside it. */
  trailing?: ReactNode;
  children?: ReactNode;
}

export default function M3RailItem({
  label,
  icon,
  activeIcon,
  active = false,
  className = "",
  children,
  trailing,
  type = "button",
  ...rest
}: M3RailItemProps) {
  const iconNode = active && activeIcon ? activeIcon : icon;
  return (
    <button
      type={type}
      className={cn(
        "relative flex h-8 w-full items-center gap-2 rounded-md px-2 text-sm transition-colors outline-none",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        active
          ? "bg-accent font-medium text-foreground"
          : "text-muted-foreground hover:bg-muted hover:text-foreground",
        className,
      )}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      title={label}
      {...rest}
    >
      {active ? (
        <span
          className="absolute top-1/2 left-0 h-4 w-0.5 -translate-y-1/2 rounded-full bg-tick"
          aria-hidden="true"
        />
      ) : null}
      {iconNode ? (
        <span className="inline-flex shrink-0" aria-hidden="true">
          {iconNode}
        </span>
      ) : null}
      <span className="min-w-0 flex-1 truncate">{children ?? label}</span>
      {trailing ? <span className="flex shrink-0 items-center">{trailing}</span> : null}
    </button>
  );
}
