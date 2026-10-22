/**
 * Zen enter cascade — the JS half (OD-D4, SYSTEM §7.2).
 *
 * Entering zen runs a three-layer chrome exit: sidebar, tab strip, then
 * pane header, 40ms apart, each 189ms. The CSS in src/styles/tabs.css
 * owns the keyframes; this module owns the one number the stylesheet
 * cannot: how long the app keeps `.nex-zen-exiting` mounted before the
 * chrome is removed from layout at `.nex-zen`.
 *
 * 189ms is 0.55 × the 343ms settle entrance (SYSTEM §7.1), the approved
 * exit ratio for surfaces that stay mounted. The total is the LAST
 * layer's delay plus its duration, not the sum of all three:
 *
 *     sidebar  0ms + 189ms = 189ms
 *     strip   40ms + 189ms = 229ms
 *     header  80ms + 189ms = 269ms  <- total
 *
 * Reduced motion: the CSS gate collapses the animations AND this helper
 * returns 0, so the state flips in the same commit. Both halves are
 * needed — the gate cannot reach a setTimeout, and a 0ms timer still
 * costs a frame. Leaving zen is instant by design: the chrome returns,
 * it does not animate in.
 */

/** Delay between chrome layers, matching the 40ms step in tabs.css. */
export const ZEN_EXIT_STEP_MS = 40;

/** Per-layer exit duration: 0.55 × the 343ms settle entrance. */
export const ZEN_EXIT_LAYER_MS = 189;

/** Delay before `.nex-zen` replaces `.nex-zen-exiting`: 80 + 189. */
export const ZEN_EXIT_TOTAL_MS = ZEN_EXIT_LAYER_MS + 2 * ZEN_EXIT_STEP_MS;

/**
 * How long to hold `.nex-zen-exiting` before setting zen.
 *
 * A pure function on purpose: the reduced-motion branch is table-tested
 * instead of being buried in an effect. `reduced` is the value from
 * `prefersReducedMotion()` read at the START of the interaction.
 */
export function zenExitDelayMs(reduced: boolean): number {
  return reduced ? 0 : ZEN_EXIT_TOTAL_MS;
}
