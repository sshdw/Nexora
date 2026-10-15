/* ============================================================
   LINEAGE — from-pattern reimplementation (#220)

   Pattern     Radix `Slider` (MIT, @radix-ui/react-slider,
               re-exported by the unified `radix-ui` package
               this app already depends on) + the shadcn/ui
               component recipe around it.
   Command     npx shadcn@latest add slider, read as a
               pattern, then rewritten here for the black theme.
   Reference   shadcn/ui "slider" — MIT (shadcn/ui)
   License     MIT

   What is deliberately NOT here: Origin UI. Origin UI is
   AGPL-3.0 and #222 kept it out of this tree on purpose; no
   line of it was read, copied or paraphrased for this file.
   The behaviour below is Radix's (drag, pointer capture,
   Home/End/arrow keys, focus management, ARIA value
   plumbing) reached through the shadcn wrapper shape, so the
   slider inherits its accessibility from the primitive
   instead of re-deriving it.

   Two rewrites against the generated recipe, both the mono
   theme in src/index.css speaking:

     1. The generated thumb is `bg-white`, which is not a
        shadcn semantic token — under a light theme it would
        be a white dot on a pale track. It is `bg-primary`
        here: the light fill, the same tone the range fill
        uses, so the active position is read as one thing.
     2. The generated thumb carries a permanent
        `ring-ring/50` halo. The hairline rules say a control
        announces itself with a border and a focus ring, not
        with a resting glow, so the ring is `focus-visible`
        only — 2px solid at 80% with a 2px offset, like every
        other focus in this theme (#235).

   And one bug fix against it: the recipe sizes the track with
   `data-horizontal:h-1`, which Tailwind compiles to
   `[data-horizontal]` — but Radix sets `data-orientation`, so
   that declaration never matched and the track collapsed to
   zero height (a slider whose rail is invisible). The rules
   here are written against the attribute Radix actually sets,
   so the rail is real in both orientations.

   Every colour below is a shadcn semantic token
   (`--muted`, `--primary`, `--ring`), so the control
   re-themes from src/index.css with no edit here.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"
import { Slider as SliderPrimitive } from "radix-ui"

function Slider({
  className,
  defaultValue,
  value,
  min = 0,
  max = 100,
  thumbProps,
  ...props
}: React.ComponentProps<typeof SliderPrimitive.Root> & {
  /** Props for the generated thumb(s). This is where a control's
   *  ACCESSIBLE NAME has to go: `role="slider"` is on the thumb,
   *  not on the root, so an `aria-label` passed to the Root lands
   *  on a `<span>` that is not the control and is never announced
   *  as one. Radix puts the thumb's own `aria-label` in the right
   *  place; this wrapper renders the thumbs, so it takes them here. */
  thumbProps?: React.ComponentProps<typeof SliderPrimitive.Thumb>
}) {
  // Radix renders one thumb per value. The recipe's fallback —
  // `[min, max]` for a single-thumb slider — would give a
  // two-thumb control for `value={5}`, so an uncontrolled root
  // with no default falls back to the documented single value.
  const _values = React.useMemo(
    () =>
      Array.isArray(value)
        ? value
        : Array.isArray(defaultValue)
          ? defaultValue
          : [min],
    [value, defaultValue, min]
  )

  return (
    <SliderPrimitive.Root
      data-slot="slider"
      defaultValue={defaultValue}
      value={value}
      min={min}
      max={max}
      className={cn(
        "relative flex w-full touch-none items-center select-none data-disabled:opacity-50 data-[orientation=vertical]:h-full data-[orientation=vertical]:min-h-40 data-[orientation=vertical]:w-auto data-[orientation=vertical]:flex-col",
        className
      )}
      {...props}
    >
      <SliderPrimitive.Track
        data-slot="slider-track"
        className="relative grow overflow-hidden rounded-full bg-muted data-[orientation=horizontal]:h-1.5 data-[orientation=horizontal]:w-full data-[orientation=vertical]:h-full data-[orientation=vertical]:w-1.5"
      >
        <SliderPrimitive.Range
          data-slot="slider-range"
          className="absolute bg-primary data-[orientation=horizontal]:h-full data-[orientation=vertical]:w-full"
        />
      </SliderPrimitive.Track>
      {Array.from({ length: _values.length }, (_, index) => (
        <SliderPrimitive.Thumb
          data-slot="slider-thumb"
          key={index}
          className="relative block size-3 shrink-0 rounded-full border border-primary bg-primary ring-ring transition-[color,box-shadow] select-none hover:ring-2 focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-background focus-visible:outline-hidden active:ring-2 disabled:pointer-events-none disabled:opacity-50"
          {...thumbProps}
        />
      ))}
    </SliderPrimitive.Root>
  )
}

export { Slider }