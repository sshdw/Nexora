import * as React from "react";
import { cn } from "@/lib/utils";
import { CheckIcon, ChevronDownIcon } from "../icons";
import { Popover, PopoverContent, PopoverTrigger } from "./popover";

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export interface SelectProps {
  id?: string;
  value: string;
  options: readonly SelectOption[];
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  "aria-label"?: string;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
  className?: string;
}

/**
 * Custom monochrome dropdown select backed by Radix Popover + accessible
 * combobox/listbox semantics. Replaces native `<select>` elements so the OS
 * never paints platform-styled menus.
 *
 * Uses strictly Nexora CSS tokens (`bg-popover`, `text-foreground`,
 * `border-input`, `bg-accent`, `ring-ring`, etc.) and honors
 * `prefers-reduced-motion`.
 */
export function Select({
  id,
  value,
  options,
  onChange,
  placeholder,
  disabled = false,
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  "aria-describedby": ariaDescribedBy,
  className,
}: SelectProps) {
  const [open, setOpen] = React.useState(false);
  const [highlightedIndex, setHighlightedIndex] = React.useState(-1);
  const listboxId = React.useId();
  const triggerRef = React.useRef<HTMLButtonElement>(null);

  const selectedOption = options.find((opt) => opt.value === value);
  const displayLabel =
    selectedOption?.label ?? placeholder ?? options[0]?.label ?? "";

  const enabledIndices = React.useMemo(
    () =>
      options.reduce<number[]>((acc, opt, idx) => {
        if (!opt.disabled) acc.push(idx);
        return acc;
      }, []),
    [options]
  );

  React.useEffect(() => {
    if (!open) return;
    const currentIdx = options.findIndex(
      (opt) => opt.value === value && !opt.disabled
    );
    setHighlightedIndex(
      currentIdx >= 0 ? currentIdx : (enabledIndices[0] ?? -1)
    );
  }, [open, options, value, enabledIndices]);

  const commit = (nextValue: string) => {
    setOpen(false);
    if (nextValue !== value) {
      onChange(nextValue);
    }
    triggerRef.current?.focus();
  };

  const moveHighlight = (delta: 1 | -1) => {
    if (enabledIndices.length === 0) return;
    const pos = enabledIndices.indexOf(highlightedIndex);
    const nextPos =
      pos < 0
        ? delta === 1
          ? 0
          : enabledIndices.length - 1
        : (pos + delta + enabledIndices.length) % enabledIndices.length;
    setHighlightedIndex(enabledIndices[nextPos] ?? -1);
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (disabled) return;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (!open) {
          setOpen(true);
        } else {
          moveHighlight(1);
        }
        break;
      case "ArrowUp":
        event.preventDefault();
        if (!open) {
          setOpen(true);
        } else {
          moveHighlight(-1);
        }
        break;
      case "Home":
        if (open && enabledIndices.length > 0) {
          event.preventDefault();
          setHighlightedIndex(enabledIndices[0] ?? -1);
        }
        break;
      case "End":
        if (open && enabledIndices.length > 0) {
          event.preventDefault();
          setHighlightedIndex(enabledIndices[enabledIndices.length - 1] ?? -1);
        }
        break;
      case "Enter":
      case " ":
        if (open && highlightedIndex >= 0) {
          const opt = options[highlightedIndex];
          if (opt && !opt.disabled) {
            event.preventDefault();
            commit(opt.value);
          }
        }
        break;
      case "Escape":
        if (open) {
          event.preventDefault();
          setOpen(false);
          triggerRef.current?.focus();
        }
        break;
    }
  };

  return (
    <Popover open={open} onOpenChange={(next) => !disabled && setOpen(next)}>
      <PopoverTrigger asChild>
        <button
          ref={triggerRef}
          id={id}
          type="button"
          role="combobox"
          aria-expanded={open}
          aria-haspopup="listbox"
          aria-controls={open ? listboxId : undefined}
          aria-label={ariaLabel}
          aria-labelledby={ariaLabelledBy}
          aria-describedby={ariaDescribedBy}
          disabled={disabled}
          onKeyDown={handleKeyDown}
          data-slot="select-trigger"
          className={cn(
            "inline-flex h-9 w-full min-w-0 items-center justify-between gap-2 rounded-md border border-input bg-card px-2.5 py-1.5 text-left text-sm text-foreground outline-none",
            "transition-[border-color,background-color,color] duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
            "hover:border-foreground/30 hover:bg-accent/40",
            "focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
            "disabled:cursor-not-allowed disabled:opacity-50",
            className
          )}
        >
          <span
            className={cn(
              "min-w-0 flex-1 truncate",
              !selectedOption && "text-muted-foreground"
            )}
          >
            {displayLabel}
          </span>
          <ChevronDownIcon
            size={16}
            aria-hidden="true"
            className={cn(
              "size-3.5 shrink-0 text-muted-foreground transition-transform duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
              open && "rotate-180"
            )}
          />
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        sideOffset={4}
        onKeyDown={handleKeyDown}
        className="z-[var(--nex-sys-z-snackbar)] max-h-60 w-[var(--radix-popover-trigger-width)] min-w-[10rem] gap-0.5 overflow-y-auto p-1"
      >
        <div
          id={listboxId}
          role="listbox"
          aria-label={ariaLabel}
          aria-labelledby={ariaLabelledBy}
          className="flex flex-col gap-0.5"
        >
          {options.map((opt, idx) => {
            const isSelected = opt.value === value;
            const isHighlighted = idx === highlightedIndex;
            return (
              <button
                key={opt.value}
                type="button"
                role="option"
                aria-selected={isSelected}
                disabled={opt.disabled}
                onMouseEnter={() => !opt.disabled && setHighlightedIndex(idx)}
                onClick={() => !opt.disabled && commit(opt.value)}
                className={cn(
                  "flex w-full items-center justify-between gap-2 rounded-sm px-2 py-1.5 text-left text-sm text-popover-foreground outline-none",
                  "transition-colors duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
                  isHighlighted && "bg-accent text-accent-foreground",
                  isSelected && "font-medium text-foreground",
                  opt.disabled && "cursor-not-allowed opacity-50"
                )}
              >
                <span className="min-w-0 flex-1 truncate">{opt.label}</span>
                {isSelected && (
                  <CheckIcon
                    size={16}
                    aria-hidden="true"
                    className="size-3.5 shrink-0 text-foreground"
                  />
                )}
              </button>
            );
          })}
        </div>
      </PopoverContent>
    </Popover>
  );
}
