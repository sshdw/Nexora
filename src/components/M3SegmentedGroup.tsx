//! Segmented button group (library primitive) — #230 Phase 0.
//!
//! 2–5 persistent choices, single-select, on the black theme's tokens
//! (`--muted` container, `--accent` hover, `--background` selected
//! segment, `--foreground`/`--muted-foreground` text). No `--nex-sys-*`
//! value remains. The standard variant is a quiet tonal container with
//! quiet segments; the connected variant is flush with a shared
//! hairline. Selection settles by tone/weight, no motion.
//!
//! Semantics: `radio` (default — preference/choice groups, radiogroup +
//! aria-checked) or `tabs` (view switchers, preserving the tablist/tab
//! pattern). Arrow keys move and select (automatic activation),
//! Home/End jump to the ends. All behaviour is unchanged from the M3E
//! original — only the paint moved.

import { useRef } from "react";
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
  /** Extra class on the group container (#220). Appended, never
   * replacing the base classes, so a caller that re-paints the group
   * leaves every other surface's geometry untouched. */
  className?: string;
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
  className = "",
}: M3SegmentedGroupProps<T>) {
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const isTabs = semantics === "tabs";
  const isConnected = variant === "connected";
  const isOptionDisabled = (index: number) =>
    disabled || options[index]?.disabled === true;
  // Roving tabindex anchor: the selected option, or — when nothing is
  // selected (value === null) — the first enabled option so the group
  // stays tabbable.
  const firstEnabledIndex = options.findIndex(
    (_, index) => !isOptionDisabled(index),
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
      className={cn(
        "inline-flex self-start items-center",
        isConnected
          ? "overflow-hidden rounded-lg border border-border"
          : "gap-0.5 rounded-lg bg-muted p-0.5",
        className,
      )}
      role={isTabs ? "tablist" : "radiogroup"}
      aria-label={label}
      aria-labelledby={labelledBy}
    >
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
              "inline-flex items-center gap-1.5 text-sm whitespace-nowrap transition-colors outline-none",
              "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
              "disabled:pointer-events-none disabled:opacity-50",
              isConnected
                ? "h-8 border-r border-border px-3 last:border-r-0"
                : "h-7 rounded-sm px-2.5",
              selected
                ? "bg-background font-medium text-foreground"
                : "text-muted-foreground hover:bg-accent hover:text-foreground",
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
