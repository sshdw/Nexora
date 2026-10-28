//! Segmented button group (library primitive) — #230 Phase 0, Glide
//! Select in Phase C (R4-08).
//!
//! 2–5 persistent choices, single-select, on the black theme's tokens
//! (`--muted` container, `--accent` hover, `--background` selected
//! segment, `--foreground`/`--muted-foreground` text). No `--nex-sys-*`
//! value remains. The standard variant is a quiet tonal container with
//! quiet segments; the connected variant is flush with a shared
//! hairline.
//!
//! Glide Select (R4-08): the standard variant draws ONE shared sliding
//! pill behind the segments. Geometry lives in CSS — the component only
//! measures the selected segment's SEAT (its layout rect, in the
//! container's padding box) and publishes `--seg-indicator-x` /
//! `--seg-indicator-w` on the container, so no `left`/`width` is
//! animated from JS; the pill moves on `transform` at
//! `--motion-duration-short4`. The rects are exact, never
//! `offsetLeft`/`offsetWidth`: those round to whole pixels, and the
//! `1fr` tracks are fractional, so the rounded pair left the pill a
//! fraction of a pixel off its seat and (at the last seat) flush against
//! the track's end padding — the "stuck to one end" read in Phase C.
//! The component adds NO inset of its own: the pill's ring is the
//! container's padding on the ends and `--space-xxxs` top/bottom, both
//! declared once in `styles/components.css`. `data-ready="false"` until
//! the first successful measurement (no fly-in from x=0 on mount), and
//! `data-no-anim` for one frame after a resize or a label change (the
//! pill jumps instead of sliding across a layout swap). The global
//! reduced-motion gate in styles/motion.css collapses the transition.
//! See styles/components.css `.nex-seg` / `.nex-seg-indicator` for the
//! class contract this component emits.
//!
//! The indicator is decoration only: it is `aria-hidden`, unfocusable
//! and never in the accessibility tree. `indicator={false}` opts out;
//! the connected variant never draws it (its flush hairline is the
//! selection grammar, and a pill would paint over the dividers).
//!
//! Semantics: `radio` (default — preference/choice groups, radiogroup +
//! aria-checked) or `tabs` (view switchers, preserving the tablist/tab
//! pattern). Arrow keys move and select (automatic activation),
//! Home/End jump to the ends. All behaviour is unchanged from the M3E
//! original — only the paint and the pill moved.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";

import { cn } from "../lib/utils";

export interface M3SegmentOption<T extends string> {
  value: T;
  label: string;
  /** Per-option accessible name when the visible label is abbreviated. */
  ariaLabel?: string;
  disabled?: boolean;
  /** Optional mark before the label (#220) — an icon or a tick.
   * Decorative by contract: callers pass `aria-hidden` marks, and
   * the option's accessible name stays its label / ariaLabel. */
  leading?: ReactNode;
}

export interface M3SegmentedGroupProps<T extends string> {
  /** Accessible group name (aria-label), or use `labelledBy` to point at a
   * visible heading id (e.g. the Settings theme label). */
  label?: string;
  labelledBy?: string;
  options: readonly M3SegmentOption<T>[];
  value: T | null;
  onChange: (value: T) => void;
  variant?: "standard" | "connected";
  semantics?: "radio" | "tabs";
  disabled?: boolean;
  /** Glide Select sliding pill (R4-08). On for the standard variant by
   * default; the connected variant never draws it (flush hairline is
   * that variant's selection grammar). Pass `false` where a slide is
   * meaningless — e.g. a group laid out as a vertical list. */
  indicator?: boolean;
  /** Extra class on the group container (#220). Appended, never
   * replacing the base classes, so a caller that re-paints the group
   * leaves every other surface's geometry untouched. */
  className?: string;
}

/** Layout float → a CSS length without float noise. Whole pixels stay
 * whole ("2px"), fractional seats keep their fraction ("95.667px"):
 * rounding to integers is what made the pill drift off its segment. */
function toCssLength(value: number): number {
  return Math.round(value * 1000) / 1000;
}

/** One frame, with a timer fallback for environments without rAF. */
function nextFrame(callback: () => void): number {
  if (typeof requestAnimationFrame === "function") {
    return requestAnimationFrame(callback);
  }
  return window.setTimeout(callback, 0);
}

function cancelFrame(handle: number): void {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(handle);
  } else {
    window.clearTimeout(handle);
  }
}

export default function M3SegmentedGroup<T extends string>({
  label,
  labelledBy,
  options,
  value,
  onChange,
  variant = "standard",
  semantics = "radio",
  disabled = false,
  indicator = true,
  className = "",
}: M3SegmentedGroupProps<T>) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const readyRef = useRef(false);
  const previousValueRef = useRef<T | null>(value);
  const valueRef = useRef<T | null>(value);
  const optionsRef = useRef(options);
  const frameRef = useRef<number | null>(null);
  const [ready, setReady] = useState(false);

  // The measurement reads the latest selection without making the
  // observer/effect callbacks depend on array identity — callers pass a
  // fresh `options` array on every render.
  valueRef.current = value;
  optionsRef.current = options;

  const isTabs = semantics === "tabs";
  const isConnected = variant === "connected";
  const hasIndicator = indicator && !isConnected;
  const hasSelection = options.some((option) => option.value === value);
  const isOptionDisabled = (index: number) =>
    disabled || options[index]?.disabled === true;
  // Roving tabindex anchor: the selected option, or — when nothing is
  // selected (value === null) — the first enabled option so the group
  // stays tabbable.
  const firstEnabledIndex = options.findIndex(
    (_, index) => !isOptionDisabled(index),
  );

  /** Publish the selected segment's SEAT as the two sanctioned Glide
   * Select custom properties (plan §5.2). Values come from layout, never
   * from invented design values — and never from `offsetLeft` /
   * `offsetWidth`, which are rounded to whole pixels: equal-width `1fr`
   * tracks are fractional, so the rounded pair widened the pill by up to
   * a pixel and moved its left edge, leaving the outer end flush against
   * the track's padding instead of seated on it. The rects are exact and
   * are expressed in the pill's containing block (the padding box), so
   * the sheet's inset — container padding on the ends, `--space-xxxs`
   * vertically — is applied exactly once. */
  const measure = useCallback((): boolean => {
    const container = containerRef.current;
    if (container === null) return false;
    const selectedIndex = optionsRef.current.findIndex(
      (option) => option.value === valueRef.current,
    );
    const selectedButton =
      selectedIndex >= 0 ? itemRefs.current[selectedIndex] : null;
    if (!selectedButton) return false;
    const track = container.getBoundingClientRect();
    const seat = selectedButton.getBoundingClientRect();
    // A zero-width segment is not laid out (hidden tab, closed popover,
    // display:none) — leave `data-ready="false"` and measure later.
    if (seat.width <= 0) return false;
    // `clientLeft` is the border the padding box sits behind; without a
    // border (every group today) this is 0.
    const x = seat.left - track.left - container.clientLeft;
    container.style.setProperty(
      "--seg-indicator-x",
      `${toCssLength(x)}px`,
    );
    container.style.setProperty(
      "--seg-indicator-w",
      `${toCssLength(seat.width)}px`,
    );
    return true;
  }, []);

  /**
   * Measure, then either arm the transition (first successful
   * measurement) or keep the pill still for one frame (resize, locale
   * change, first selection appearing).
   */
  const settle = useCallback(
    (animate: boolean) => {
      const container = containerRef.current;
      if (container === null) return;
      const first = !readyRef.current;
      // Suppress the transition BEFORE the vars move, so the browser
      // never interpolates across a layout change.
      if (!animate && !first) {
        container.dataset.noAnim = "true";
      }
      const measured = measure();
      if (first) {
        if (measured) {
          readyRef.current = true;
          setReady(true);
        }
        return;
      }
      if (animate) return;
      if (frameRef.current !== null) cancelFrame(frameRef.current);
      frameRef.current = nextFrame(() => {
        frameRef.current = null;
        const current = containerRef.current;
        if (current !== null) delete current.dataset.noAnim;
      });
    },
    [measure],
  );

  // Re-measure on selection changes (glide) and on label changes
  // (locale swap: equal-width tracks keep the width, but the text can
  // still resolve late; jump instead of sliding).
  const labelSignature = options
    .map((option) => `${option.value}\u0000${option.label}`)
    .join("\u0001");

  useLayoutEffect(() => {
    const previous = previousValueRef.current;
    previousValueRef.current = value;
    // A selection appearing for the first time has nothing to glide
    // from — settle it in place.
    settle(previous !== null && previous !== value);
  }, [labelSignature, settle, value]);

  // Container AND every segment: a font swap can resize labels without
  // resizing the container, and a one-sided observer would miss it.
  useEffect(() => {
    const container = containerRef.current;
    if (container === null || typeof ResizeObserver === "undefined") {
      return undefined;
    }
    const observer = new ResizeObserver(() => {
      settle(false);
    });
    observer.observe(container);
    for (const button of itemRefs.current) {
      if (button !== null) observer.observe(button);
    }
    return () => {
      observer.disconnect();
    };
  }, [options.length, settle]);

  // Geist loads late: `document.fonts.ready` is the point after which
  // label widths are final. jsdom has no FontFaceSet — guarded.
  useEffect(() => {
    const fontsReady = document.fonts?.ready as Promise<unknown> | undefined;
    if (fontsReady === undefined) return undefined;
    let cancelled = false;
    void fontsReady.then(() => {
      if (!cancelled) settle(false);
    });
    return () => {
      cancelled = true;
    };
  }, [settle]);

  useEffect(
    () => () => {
      if (frameRef.current !== null) {
        cancelFrame(frameRef.current);
        frameRef.current = null;
      }
    },
    [],
  );

  const focusOption = (index: number, direction: 1 | -1 = 1) => {
    const count = options.length;
    // Advance past disabled options to the next enabled one instead of
    // stalling on a disabled stop.
    for (let step = 0; step < count; step += 1) {
      const next = (((index + count) % count) + count) % count;
      if (!isOptionDisabled(next)) {
        itemRefs.current[next]?.focus();
        const option = options[next];
        if (option && option.value !== value) {
          onChange(option.value);
        }
        return;
      }
      index += direction;
    }
  };

  // shortcut:segmented.move / shortcut:segmented.edges — scoped keys stay
  // inline (the focused group IS the scope; see lib/shortcuts.ts).
  const handleKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        event.preventDefault();
        focusOption(index + 1, 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        event.preventDefault();
        focusOption(index - 1, -1);
        break;
      case "Home":
        event.preventDefault();
        focusOption(0, 1);
        break;
      case "End":
        event.preventDefault();
        focusOption(options.length - 1, -1);
        break;
    }
  };

  return (
    <div
      ref={containerRef}
      className={cn(
        // `.nex-seg` owns the shared geometry (relative container,
        // equal-width 1fr tracks) and the indicator contract.
        "nex-seg",
        isConnected
          ? "nex-seg-connected overflow-hidden rounded-lg border border-border"
          : "gap-0.5 rounded-lg bg-muted p-0.5",
        className,
      )}
      data-ready={ready ? "true" : "false"}
      role={isTabs ? "tablist" : "radiogroup"}
      aria-label={label}
      aria-labelledby={labelledBy}
    >
      {hasIndicator && hasSelection ? (
        <span className="nex-seg-indicator" aria-hidden="true" />
      ) : null}
      {options.map((option, index) => {
        const selected = option.value === value;
        const optionDisabled = disabled || option.disabled;
        return (
          <button
            key={option.value}
            ref={(element) => {
              itemRefs.current[index] = element;
            }}
            type="button"
            role={isTabs ? "tab" : "radio"}
            aria-selected={isTabs ? selected : undefined}
            aria-checked={!isTabs ? selected : undefined}
            aria-label={option.ariaLabel}
            className={cn(
              "relative z-10 inline-flex min-w-0 items-center gap-1.5 text-sm whitespace-nowrap transition-colors outline-none",
              "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
              "disabled:pointer-events-none disabled:opacity-50",
              isConnected
                ? "h-8 border-r border-border px-3 last:border-r-0"
                : // No radius utility here: `components.css` gives the
                  // standard segment `--seg-radius`, the same custom
                  // property the pill consumes, so the two can never
                  // resolve to different corners (a `rounded-*` utility
                  // outranks a caller's rule and used to pin the segment
                  // at `--radius-sm` while the pill took the caller's
                  // larger radius).
                  "h-7 px-2.5",
              selected
                ? cn(
                    "font-medium text-foreground",
                    // With the pill drawn the tone comes from behind;
                    // without it the segment keeps its own fill so the
                    // selection is always legible.
                    !hasIndicator && "bg-background",
                  )
                : "text-muted-foreground hover:bg-accent hover:text-foreground",
              selected && "is-active",
            )}
            tabIndex={
              selected || (value === null && index === firstEnabledIndex)
                ? undefined
                : -1
            }
            disabled={optionDisabled}
            onClick={() => onChange(option.value)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {option.leading}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
