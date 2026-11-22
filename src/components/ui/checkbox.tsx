import { cn } from "@/lib/utils";
import { CheckIcon } from "../icons";

export interface CheckboxProps {
  id?: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  "aria-label"?: string;
  "aria-labelledby"?: string;
  "aria-describedby"?: string;
  className?: string;
}

/**
 * Custom monochrome checkbox with accessible `role="checkbox"` semantics.
 * Replaces native `<input type="checkbox">` so browsers never paint default
 * blue checkboxes.
 *
 * Uses strictly Nexora CSS tokens (`border-input`, `bg-primary`,
 * `text-primary-foreground`, `ring-ring`, etc.) and honors
 * `prefers-reduced-motion`.
 */
export function Checkbox({
  id,
  checked,
  onCheckedChange,
  disabled = false,
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  "aria-describedby": ariaDescribedBy,
  className,
}: CheckboxProps) {
  return (
    <button
      id={id}
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={ariaLabel}
      aria-labelledby={ariaLabelledBy}
      aria-describedby={ariaDescribedBy}
      disabled={disabled}
      data-state={checked ? "checked" : "unchecked"}
      data-slot="checkbox"
      onClick={() => {
        if (!disabled) onCheckedChange(!checked);
      }}
      className={cn(
        "peer inline-flex size-4 shrink-0 items-center justify-center rounded-[4px] border border-input bg-transparent text-primary-foreground outline-none",
        "transition-[background-color,border-color,color] duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
        "hover:border-foreground/40",
        "data-[state=checked]:border-primary data-[state=checked]:bg-primary data-[state=checked]:text-primary-foreground",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
    >
      {checked && (
        <CheckIcon
          size={16}
          aria-hidden="true"
          className="pointer-events-none size-3 shrink-0 text-current"
        />
      )}
    </button>
  );
}
