import { useEffect, useState } from "react";

/**
 * Reduced-motion gate for JavaScript-driven animation (#218).
 *
 * The stylesheet half of the gate lives in src/index.css and
 * src/styles/motion.css: under `prefers-reduced-motion: reduce`
 * every CSS animation and transition collapses to a single frame.
 * A stylesheet cannot reach a clock that JavaScript owns — a
 * requestAnimationFrame loop, a GSAP tween, a setTimeout chain —
 * so every animated component in the stack branches on this hook
 * instead and renders its final static frame directly.
 *
 * Semantics:
 *   - `true`  reduced motion requested: render the static end
 *             state, start no timeline, schedule no frame loop.
 *   - `false` motion allowed: run the animation as authored.
 *
 * Fail-closed by design. Environments with no `matchMedia`
 * (older WebViews, non-DOM test harnesses) report `true`: a
 * missing capability means the component renders still rather
 * than starting motion nobody asked to suppress.
 */

/** The one query this gate reads. Exported so tests and components
 *  assert against the same string the CSS block matches. */
export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

/** Current OS-level motion preference, read synchronously. */
export function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return true;
  }
  return window.matchMedia(REDUCED_MOTION_QUERY).matches;
}

/**
 * Subscribes to the OS-level motion preference and re-renders when
 * it changes, so a user flipping the setting mid-session takes
 * effect without a reload.
 *
 * Returns `true` when motion must be collapsed.
 */
export function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState<boolean>(prefersReducedMotion);

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
      return;
    }
    const list = window.matchMedia(REDUCED_MOTION_QUERY);
    const onChange = (event: MediaQueryListEvent): void => setReduced(event.matches);
    setReduced(list.matches);
    // Safari below 14 only implements the deprecated listener API.
    if (typeof list.addEventListener === "function") {
      list.addEventListener("change", onChange);
      return () => list.removeEventListener("change", onChange);
    }
    list.addListener(onChange);
    return () => list.removeListener(onChange);
  }, []);

  return reduced;
}