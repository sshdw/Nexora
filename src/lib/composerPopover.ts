//! Composer popover geometry (R6-02 / R6-05 defect fix).
//!
//! The model and context triggers live INSIDE the composer card, in its
//! trailing row. Radix measures `sideOffset` from the trigger's own edge, so
//! `side="top"` + `sideOffset={8}` put the popover's bottom eight pixels
//! above the TRIGGER — which is still inside the card, overlapping it by
//! most of the trigger row. The captured window showed both popovers ending
//! at y≈453 while the card starts at y=379.
//!
//! The fix is arithmetic, not a hardcoded 80 px: measure the trigger's top
//! and the card's top on open, then add the distance between them to the
//! gap. The popover's bottom is `triggerTop - sideOffset`, so this helper
//! makes that equal to `cardTop - gap`:
//!
//!   sideOffset = (triggerTop - cardTop) + gap
//!   triggerTop - sideOffset = cardTop - gap
//!
//! Attachments and wrapped rows move `triggerTop`, so the offset follows
//! them. The same measurement caps the popover's height to the space
//! between the app header and the card (`cardTop - headerBottom - 16`),
//! which is what keeps the context body from covering the header or the
//! budget card; the body itself scrolls.
//!
//! jsdom measures no layout, so the pure functions below are unit-tested
//! with the real captured numbers; the hook only supplies the rectangles.

import { useRef, useState, type RefObject } from "react";

/** The visible gap between the popover and the card's top border. */
export const COMPOSER_POPOVER_GAP = 8;
/** Keep the popover's top edge this far from the viewport's top edge. */
export const COMPOSER_POPOVER_VIEWPORT_PADDING = 8;

/**
 * `sideOffset` that puts the popover's bottom `gap` pixels above the card,
 * not above the trigger. Never less than the gap itself.
 */
export function popoverOffsetFromCard(
  triggerTop: number,
  cardTop: number,
  gap: number = COMPOSER_POPOVER_GAP,
): number {
  if (!Number.isFinite(triggerTop) || !Number.isFinite(cardTop)) return gap;
  return Math.max(gap, triggerTop - cardTop + gap);
}

/**
 * Maximum content height that still leaves `gap` above the card and
 * `padding` below the app header:
 *
 *   cardTop - headerBottom - gap - padding
 *
 * At 1200×800 (card top 379, tab strip bottom 56) that is 307 px.
 */
export function popoverMaxHeightFromCard(
  cardTop: number,
  headerBottom: number,
  gap: number = COMPOSER_POPOVER_GAP,
  padding: number = COMPOSER_POPOVER_VIEWPORT_PADDING,
): number {
  if (!Number.isFinite(cardTop) || !Number.isFinite(headerBottom)) return 0;
  return Math.max(0, cardTop - headerBottom - gap - padding);
}

export interface ComposerPopoverLayout {
  /** Attach to the Radix trigger (the button inside the card). */
  triggerRef: RefObject<HTMLButtonElement | null>;
  /** Pass to `PopoverContent.sideOffset`. */
  sideOffset: number;
  /** Pass to `PopoverContent.style.maxHeight` once measured. */
  maxHeight: number | undefined;
  /** Call when the popover opens, before its content mounts. */
  measure: () => void;
}

/**
 * Measure the trigger against the composer card on open. The card is the
 * nearest `.nex-composer-shell`; both popovers in this row share it.
 */
export function useComposerPopoverLayout(): ComposerPopoverLayout {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [sideOffset, setSideOffset] = useState<number>(COMPOSER_POPOVER_GAP);
  const [maxHeight, setMaxHeight] = useState<number | undefined>(undefined);

  const measure = () => {
    const trigger = triggerRef.current;
    if (trigger === null) return;
    const card = trigger.closest<HTMLElement>(".nex-composer-shell");
    if (card === null) return;

    const triggerRect = trigger.getBoundingClientRect();
    const cardRect = card.getBoundingClientRect();
    // jsdom returns a zero rect for every element; do not turn that into a
    // zero-height popover. In the app the card always has a real box.
    if (cardRect.width === 0 && cardRect.height === 0) return;

    const header = document.querySelector<HTMLElement>(".nex-tabstrip");
    const headerBottom = header?.getBoundingClientRect().bottom ?? 0;

    setSideOffset(popoverOffsetFromCard(triggerRect.top, cardRect.top));
    setMaxHeight(popoverMaxHeightFromCard(cardRect.top, headerBottom));
  };

  return { triggerRef, sideOffset, maxHeight, measure };
}
