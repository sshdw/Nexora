//! Composer popover geometry — pure arithmetic, tested with the captured
//! window's numbers. jsdom cannot measure layout, so the hook is not tested
//! here; these are the two decisions it makes.

import { describe, expect, it } from "vitest";

import {
  COMPOSER_POPOVER_GAP,
  popoverMaxHeightFromCard,
  popoverOffsetFromCard,
} from "./composerPopover";

describe("composer popover — offset from the card", () => {
  it("puts the popover's bottom 8px above the card, not above the trigger", () => {
    // Real capture: card top 379, model/context trigger row around 447.
    const offset = popoverOffsetFromCard(447, 379);
    expect(offset).toBe(76);
    // The popover's bottom is `triggerTop - sideOffset`.
    expect(447 - offset).toBe(379 - COMPOSER_POPOVER_GAP);
  });

  it("follows the trigger when attachments or wrapping move it down", () => {
    // A taller trigger row is still inside the same card.
    expect(popoverOffsetFromCard(500, 379)).toBe(129);
    expect(500 - popoverOffsetFromCard(500, 379)).toBe(371);
    // A card that starts higher moves the offset with it.
    expect(popoverOffsetFromCard(420, 300)).toBe(128);
    expect(420 - popoverOffsetFromCard(420, 300)).toBe(292);
  });

  it("never asks for less than the gap when the trigger is above the card", () => {
    expect(popoverOffsetFromCard(100, 379)).toBe(COMPOSER_POPOVER_GAP);
  });

  it("falls back to the gap when a rectangle is unmeasurable", () => {
    expect(popoverOffsetFromCard(Number.NaN, 379)).toBe(COMPOSER_POPOVER_GAP);
    expect(popoverOffsetFromCard(447, Number.POSITIVE_INFINITY)).toBe(
      COMPOSER_POPOVER_GAP,
    );
  });
});

describe("composer popover — height cap", () => {
  it("caps the body to the space above the card, leaving the gap and viewport padding", () => {
    // 379 - 8 (card gap) - 8 (viewport top) = 363.
    expect(popoverMaxHeightFromCard(379)).toBe(363);
    expect(popoverMaxHeightFromCard(800)).toBe(784);
  });

  it("never returns a negative height", () => {
    expect(popoverMaxHeightFromCard(4)).toBe(0);
    expect(popoverMaxHeightFromCard(Number.NaN)).toBe(0);
  });
});
