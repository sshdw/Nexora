//! M3E navigation-rail item (library primitive — the sidebar pattern).
//!
//! OFFICIAL rail grammar as Nexora's sidebar embodies it (skill
//! components.md "Navigation rail", contract §Shell): expanded-rail variant,
//! active = pill + tone step + emphasized label, filled icon when active vs
//! outlined at rest (OFFICIAL a11y guidance — shape + tone, never color
//! alone). The active background settles on the default-speed spring curve;
//! reduced motion resolves to an instant tone swap via the motion.css gate.
//! Destinations announce with aria-current; native <button> keyboard parity.

import type { ButtonHTMLAttributes, ReactNode } from "react";

export interface M3RailItemProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  /** Visible + accessible label (aria-label + title). */
  label: string;
  /** Outlined/rest icon (optional — text-only rows omit it). */
  icon?: ReactNode;
  /** Filled variant shown while active (falls back to `icon`). */
  activeIcon?: ReactNode;
  /** Selected destination: pill tone + emphasized label + aria-current. */
  active?: boolean;
  children?: ReactNode;
}

export default function M3RailItem({
  label,
  icon,
  activeIcon,
  active = false,
  className = "",
  children,
  type = "button",
  ...rest
}: M3RailItemProps) {
  const iconNode = active && activeIcon ? activeIcon : icon;
  return (
    <button
      type={type}
      className={
        "nex-m3-rail-item" +
        (active ? " is-active" : "") +
        (className ? " " + className : "")
      }
      aria-label={label}
      aria-current={active ? "page" : undefined}
      title={label}
      {...rest}
    >
      {iconNode ? (
        <span className="nex-m3-rail-icon" aria-hidden="true">
          {iconNode}
        </span>
      ) : null}
      <span className="nex-m3-rail-label">{children ?? label}</span>
    </button>
  );
}
