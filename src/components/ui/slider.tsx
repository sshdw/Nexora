/* ============================================================
   LINEAGE — copied source, not an npm dependency (#218)

   Upstream      Origin UI — Slider (default registry)
   Repository    https://github.com/origin-space/originui
                 (redirects to https://github.com/cosscom/ui)
   Commit        107dc0a1f011cd99ff047e2beef8201947a17ff2
                 ("feat(composer): preserve structured
                 references and support actions (#872)",
                 2026-10-13)
   File          apps/origin/registry/default/ui/slider.tsx
   License       AGPL-3.0 (per repository LICENSE)

   Why copied: Origin UI is distributed as registry source, not a
   package. This slider is the minimum that proves the pattern
   for a primitive: one file, Radix under the hood, classes
   expressed purely in shadcn's semantic tokens (`bg-primary`,
   `bg-muted`, `ring-ring`) so a consumer re-themes it by changing
   vars and nothing else.

   Nexora changes:
     1. Import path `@/registry/default/lib/utils` → `@/lib/utils`
        and the Tooltip import → `@/components/ui/tooltip`, which
        is the same Origin UI Tooltip copied in this PR.
     2. `"use client"` dropped (this is a Vite app).
     3. Black-theme restraint: `shadow-sm` on the thumb dropped.
        This theme separates by hairline, and a soft shadow is
        exactly the "fill/shadow where a line would do" the
        direction forbids.
     4. Single-value thumbs render as `internalValues.map(...)`
        instead of the upstream `Array.from(...)` + `<Fragment>`
        dance. Same output, fewer nodes.

   Not changed: the Radix Root/Track/Range/Thumb structure, the
   controlled/uncontrolled value handling, or the optional
   drag-tooltip behaviour.
   ============================================================ */
import { Slider as SliderPrimitive } from "radix-ui";
import * as React from "react";
import { cn } from "@/lib/utils";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

function Slider({
  className,
  defaultValue,
  value,
  min = 0,
  max = 100,
  showTooltip = false,
  tooltipContent,
  ...props
}: React.ComponentProps<typeof SliderPrimitive.Root> & {
  showTooltip?: boolean;
  tooltipContent?: (value: number) => React.ReactNode;
}) {
  const [internalValues, setInternalValues] = React.useState<number[]>(
    Array.isArray(value)
      ? value
      : Array.isArray(defaultValue)
        ? defaultValue
        : [min, max],
  );

  React.useEffect(() => {
    if (value !== undefined) {
      setInternalValues(Array.isArray(value) ? value : [value]);
    }
  }, [value]);

  const handleValueChange = (newValue: number[]) => {
    setInternalValues(newValue);
    props.onValueChange?.(newValue);
  };

  const [showTooltipState, setShowTooltipState] = React.useState(false);

  const handlePointerDown = () => {
    if (showTooltip) {
      setShowTooltipState(true);
    }
  };

  const handlePointerUp = React.useCallback(() => {
    if (showTooltip) {
      setShowTooltipState(false);
    }
  }, [showTooltip]);

  React.useEffect(() => {
    if (showTooltip) {
      document.addEventListener("pointerup", handlePointerUp);
      return () => {
        document.removeEventListener("pointerup", handlePointerUp);
      };
    }
  }, [showTooltip, handlePointerUp]);

  const renderThumb = (value: number) => {
    const thumb = (
      <SliderPrimitive.Thumb
        className="block size-4 shrink-0 rounded-full border border-primary bg-background outline-none ring-ring/50 transition-[color,box-shadow] hover:ring-4 focus-visible:ring-4 disabled:pointer-events-none disabled:opacity-50"
        data-slot="slider-thumb"
        onPointerDown={handlePointerDown}
      />
    );

    if (!showTooltip) return thumb;

    return (
      <TooltipProvider>
        <Tooltip open={showTooltipState}>
          <TooltipTrigger asChild>{thumb}</TooltipTrigger>
          <TooltipContent
            className="px-2 py-1 text-xs"
            side={props.orientation === "vertical" ? "right" : "top"}
            sideOffset={8}
          >
            <p>{tooltipContent ? tooltipContent(value) : value}</p>
          </TooltipContent>
        </Tooltip>
      </TooltipProvider>
    );
  };

  return (
    <SliderPrimitive.Root
      className={cn(
        "relative flex w-full touch-none select-none items-center data-[orientation=vertical]:h-full data-[orientation=vertical]:min-h-44 data-[orientation=vertical]:w-auto data-[orientation=vertical]:flex-col data-[disabled]:opacity-50",
        className,
      )}
      data-slot="slider"
      defaultValue={defaultValue}
      max={max}
      min={min}
      onValueChange={handleValueChange}
      value={value}
      {...props}
    >
      <SliderPrimitive.Track
        className={cn(
          "relative grow overflow-hidden rounded-full bg-muted data-[orientation=horizontal]:h-1.5 data-[orientation=vertical]:h-full data-[orientation=horizontal]:w-full data-[orientation=vertical]:w-1.5",
        )}
        data-slot="slider-track"
      >
        <SliderPrimitive.Range
          className={cn(
            "absolute bg-primary data-[orientation=horizontal]:h-full data-[orientation=vertical]:w-full",
          )}
          data-slot="slider-range"
        />
      </SliderPrimitive.Track>
      {internalValues.map((thumbValue, index) => (
        <React.Fragment key={String(index)}>
          {renderThumb(thumbValue)}
        </React.Fragment>
      ))}
    </SliderPrimitive.Root>
  );
}

export { Slider };