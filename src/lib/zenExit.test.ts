import { describe, expect, it } from "vitest";

import {
  ZEN_EXIT_LAYER_MS,
  ZEN_EXIT_STEP_MS,
  ZEN_EXIT_TOTAL_MS,
  zenExitDelayMs,
} from "./zenExit";

/**
 * The JS half of the zen cascade (OD-D4, SYSTEM §7.2).
 *
 * These numbers are a contract, not an implementation detail: the CSS
 * keyframes in tabs.css and the `setTimeout` in App.tsx must agree, or
 * the chrome is removed mid-animation (content jumps) or the exiting
 * class lingers after the chrome is already gone. The table below is
 * the whole lifecycle.
 */
describe("zen enter cascade — JS lifecycle", () => {
  it("pins the three-layer timing chain", () => {
    // 189ms per layer (0.55 × the 343ms settle entrance), 40ms apart.
    expect(ZEN_EXIT_STEP_MS).toBe(40);
    expect(ZEN_EXIT_LAYER_MS).toBe(189);
    // 80ms (last delay) + 189ms (last layer), NOT 3 × 189.
    expect(ZEN_EXIT_TOTAL_MS).toBe(269);
    expect(ZEN_EXIT_TOTAL_MS).toBe(ZEN_EXIT_LAYER_MS + 2 * ZEN_EXIT_STEP_MS);
  });

  it("holds the exiting class for the cascade, or not at all under reduced motion", () => {
    const table: Array<[reduced: boolean, delay: number]> = [
      [false, 269],
      [true, 0],
    ];
    for (const [reduced, delay] of table) {
      expect(zenExitDelayMs(reduced)).toBe(delay);
    }
  });

  it("never returns a negative or fractional delay", () => {
    for (const reduced of [false, true]) {
      const delay = zenExitDelayMs(reduced);
      expect(delay).toBeGreaterThanOrEqual(0);
      expect(Number.isInteger(delay)).toBe(true);
    }
  });
});
