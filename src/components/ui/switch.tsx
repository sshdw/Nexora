import { cn } from "@/lib/utils";

export interface SwitchProps {
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
 * Monochrome toggle switch with accessible `role="switch"` semantics.
 * Uses strictly Nexora CSS tokens and honors `prefers-reduced-motion`.
 */
export function Switch({
  id,
  checked,
  onCheckedChange,
  disabled = false,
  "aria-label": ariaLabel,
  "aria-labelledby": ariaLabelledBy,
  "aria-describedby": ariaDescribedBy,
  className,
}: SwitchProps) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      aria-labelledby={ariaLabelledBy}
      aria-describedby={ariaDescribedBy}
      disabled={disabled}
      data-state={checked ? "checked" : "unchecked"}
      data-slot="switch"
      onClick={() => {
        if (!disabled) onCheckedChange(!checked);
      }}
      className={cn(
        "peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-[var(--border)] bg-[var(--secondary)] p-0.5 outline-none",
        "transition-[background-color,border-color] duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
        "hover:border-foreground/40",
        "data-[state=checked]:border-[var(--primary)] data-[state=checked]:bg-[var(--primary)]",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
    >
      <span
        aria-hidden="true"
        className={cn(
          "pointer-events-none block size-3.5 rounded-full transition-transform duration-[var(--nex-sys-motion-duration-fast)] ease-[var(--nex-sys-motion-easing-standard)] motion-reduce:transition-none",
          checked
            ? "translate-x-4 bg-[var(--primary-foreground)]"
            : "translate-x-0 bg-[var(--foreground)]"
        )}
      />
    </button>
  );
}
