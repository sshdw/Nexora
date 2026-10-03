//! M3E segmented button group (library primitive).
//!
//! OFFICIAL button-group grammar ([doc-cards] Day/Week/Month, [comp-grid]
//! Going triplet; skill components.md "Segmented controls"): 2–5 persistent
//! choices, single-select, selected segment carries the container tone +
//! emphasized label. Standard variant: pill container, pill segments.
//! Connected variant: flush inner edges with asymmetric inner corners
//! (contract §Shape, [doc-cards]). Selection settles with the
//! default-speed spring curve; reduced motion resolves to an instant
//! tone/weight swap via the motion.css gate.
//!
//! Semantics: `radio` (default — preference/choice groups, radiogroup +
//! aria-checked) or `tabs` (view switchers that show/hide panels, preserving
//! the existing tablist/tab pattern). Arrow keys move and select (automatic
//! activation), Home/End jump to the ends.

import { useRef } from "react";

export interface M3SegmentOption<T extends string> {
  value: T;
  label: string;
  /** Per-option accessible name when the visible label is abbreviated. */
  ariaLabel?: string;
  disabled?: boolean;
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
}: M3SegmentedGroupProps<T>) {
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const isTabs = semantics === "tabs";

  const focusOption = (index: number) => {
    const count = options.length;
    const next = (index + count) % count;
    const target = itemRefs.current[next];
    if (target && !target.disabled) {
      target.focus();
      const option = options[next];
      if (option && !option.disabled && option.value !== value) {
        onChange(option.value);
      }
    }
  };

  const handleKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    switch (event.key) {
      case "ArrowRight":
      case "ArrowDown":
        event.preventDefault();
        focusOption(index + 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        event.preventDefault();
        focusOption(index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusOption(0);
        break;
      case "End":
        event.preventDefault();
        focusOption(options.length - 1);
        break;
    }
  };

  return (
    <div
      className={
        "nex-seg" + (variant === "connected" ? " nex-seg-connected" : "")
      }
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
            className={selected ? "is-active" : undefined}
            tabIndex={!isTabs && !selected ? -1 : undefined}
            disabled={optionDisabled}
            onClick={() => onChange(option.value)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
