//! M3E toolbars — docked row + floating selection popover (library).
//!
//! OFFICIALLY-supported desktop pattern ([toolbars], [bars-docked],
//! [doc-cards]; skill components map row 15): docked rows carry persistent
//! chrome; floating toolbars appear only as selection-context popovers
//! anchored to the selection. Floating enters with scale+fade from the
//! anchor on medium2 · emphasized-decelerate (skill motion.md §7,
//! .nex-pop-enter); reduced motion resolves to fade-only/instant via the
//! motion.css gate. Both variants expose role="toolbar" with arrow-key
//! movement between focusable children and Esc-to-dismiss on floating.

import { useEffect, useRef, type CSSProperties, type ReactNode } from "react";

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export interface M3ToolbarProps {
  /** Accessible group name (required). */
  label: string;
  variant?: "docked" | "floating";
  /** Floating only: caller-supplied anchor position (top/left). The
   * consumer owns anchoring (position via `style`) and outside-click
   * dismissal — the toolbar owns focus and arrow-key movement only. */
  style?: CSSProperties;
  /** Floating only: called on Escape (dismiss the selection context).
   * The consumer owns dismissal side effects (e.g. outside-click
   * handling and unmounting); focus is restored to the pre-popover
   * element here and again on unmount. */
  onDismiss?: () => void;
  /** Floating only: move focus into the toolbar on mount (default true —
   * popovers take focus; set false to keep focus at the anchor). */
  autoFocus?: boolean;
  className?: string;
  children: ReactNode;
}

export default function M3Toolbar({
  label,
  variant = "docked",
  style,
  onDismiss,
  autoFocus = true,
  className = "",
  children,
}: M3ToolbarProps) {
  const barRef = useRef<HTMLDivElement>(null);
  const floating = variant === "floating";
  // Element focused before the floating popover mounted — restored on
  // unmount and on the Escape/onDismiss path below. Anchoring (`style`)
  // and outside-click dismissal stay with the consumer.
  const restoreTargetRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!floating) return;
    restoreTargetRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    if (autoFocus) {
      const first = barRef.current?.querySelector<HTMLElement>(
        FOCUSABLE_SELECTOR,
      );
      first?.focus();
    }
    return () => {
      restoreTargetRef.current?.focus();
      restoreTargetRef.current = null;
    };
  }, [floating, autoFocus]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const bar = barRef.current;
    if (!bar) return;
    if (event.key === "Escape" && floating) {
      event.stopPropagation();
      onDismiss?.();
      restoreTargetRef.current?.focus();
      return;
    }
    if (
      event.key !== "ArrowRight" &&
      event.key !== "ArrowLeft" &&
      event.key !== "ArrowDown" &&
      event.key !== "ArrowUp" &&
      event.key !== "Home" &&
      event.key !== "End"
    ) {
      return;
    }
    const items = Array.from(
      bar.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    );
    if (items.length === 0) return;
    const active = document.activeElement;
    let index = items.indexOf(active as HTMLElement);
    if (index === -1) index = 0;
    event.preventDefault();
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        items[(index + 1) % items.length]?.focus();
        break;
      case "ArrowLeft":
      case "ArrowUp":
        items[(index - 1 + items.length) % items.length]?.focus();
        break;
      case "Home":
        items[0]?.focus();
        break;
      case "End":
        items[items.length - 1]?.focus();
        break;
    }
  };

  return (
    <div
      ref={barRef}
      role="toolbar"
      aria-label={label}
      style={style}
      onKeyDown={handleKeyDown}
      className={
        (floating ? "nex-m3-toolbar-floating nex-pop-enter" : "nex-m3-toolbar-docked") +
        (className ? " " + className : "")
      }
    >
      {children}
    </div>
  );
}
