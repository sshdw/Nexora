/* ============================================================
   LINEAGE — generated source, not hand-authored (#218)

   Generator    npx shadcn@latest add button
   Command      npx shadcn@latest init -y -t vite -b radix -p nova --css-variables
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Tool         shadcn@4.21.4 (see package.json devDependencies)
   License      MIT (shadcn/ui)

   Kept verbatim so the next `shadcn add` diffs cleanly against
   the registry. One divergence, both halves of it necessary
   together:

     the registry emits `import { cn } from "cn"`, which resolves
     through the `shadcn/tailwind.css` preset's bare-specifier
     convention. This repo does not ship that preset's alias, so
     the import points at the generated src/lib/utils.ts that
     `shadcn init` wrote for the same `cn`. Same function, same
     clsx + tailwind-merge implementation.

   shadcn/ui is MIT. The class string is expressed in shadcn's
   semantic tokens only, so it re-themes from the mono theme
   vars in src/index.css — except the three divergences noted
   on the base string and the variants below (light-fill
   hover/pressed steps; 2px ring + offset; the global press
   token, which is a motion token and not a colour).

   If this file is hand-edited, move this note onto the edit
   itself — an un-noted divergence from generated code is how
   vendored source rots.
   ============================================================ */
import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"
import { Slot } from "radix-ui"

// #230 P2 DIVERGENCE (NEXORA-THEME.md §6, owner-approved
// 2026-10-15) — the global press contract on the BASE string.
// The registry presses with a 1px nudge, `active:translate-y-px`,
// and two things about that are wrong for this theme: the nudge
// reads as a nudge (a pixel of travel, not a press), and it is
// the base's only active transform, so it lands on every variant
// while `transition-all` gives it the 150ms state-fade duration
// the contract reserves for hover/focus. The contract is ONE
// press — scale(0.985), 90ms in, instant out, on ALL controls —
// so the base string now:
//   - transitions an explicit property list instead of `all`, so
//     the press owns its own clock. A `transition-duration` list
//     aligns positionally with `transition-property` (the same
//     mechanic .nex-morph-pill uses in styles/components.css);
//   - gives `scale` 0s at rest (instant release) and
//     `var(--nex-sys-motion-duration-press)` while held, and
//     leaves every paint transition on
//     `--nex-sys-motion-duration-fast` — the same 150ms value
//     Tailwind's `transition-all` was already using, so nothing
//     about hover/focus/disabled changes;
//   - presses with `var(--nex-sys-motion-scale-press)`;
//   - drops the registry's `not-aria-[haspopup]` guard, because
//     the contract says ALL controls. No element in src/ sets
//     `aria-haspopup` today, so the guard matched nothing;
//   - and removes the scale entirely under reduced motion
//     (`motion-reduce:active:scale-none`) on top of the ONE
//     global gate in styles/motion.css, which collapses the
//     durations themselves. Both halves are needed: the gate
//     makes the press instant, this makes it not move at all.
// The two `--nex-sys-motion-*` names are declared in
// styles/motion.css (motion tokens live in their own sheet, not
// in the theme blocks) and are registered for the theme gate in
// src/theme.test.ts.
const buttonVariants = cva(
  "group/button inline-flex shrink-0 items-center justify-center rounded-lg border border-transparent bg-clip-padding text-sm font-medium whitespace-nowrap transition-[scale,background-color,color,border-color,box-shadow,opacity] [transition-duration:0s,var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast)] outline-none select-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background active:scale-[var(--nex-sys-motion-scale-press)] active:[transition-duration:var(--nex-sys-motion-duration-press),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast),var(--nex-sys-motion-duration-fast)] motion-reduce:active:scale-none disabled:pointer-events-none disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-2 aria-invalid:ring-destructive/20 dark:aria-invalid:border-destructive/50 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  {
    variants: {
      variant: {
        // #235 DIVERGENCE: the default is the LIGHT FILL
        // (`--primary` := #EDEAE7, dark text — single source in
        // src/index.css). Hover/pressed are explicit lightness
        // steps, NOT `bg-primary/80`: an opacity wash over dark
        // DARKENS, while the theme lightens on hover (#F5F3EF)
        // and darkens on press (#D8D4CE). Disabled is muted text
        // with NO fill and a hairline border (THEME §1) — never a
        // washed-out fill: `disabled:opacity-100` cancels the base
        // wash via tailwind-merge, so the text keeps its 5.88:1
        // (muted on the canvas) instead of being faded twice.
        default:
          "bg-primary text-primary-foreground hover:bg-primary-hover active:bg-primary-pressed disabled:border-border disabled:bg-transparent disabled:text-muted-foreground disabled:opacity-100",
        outline:
          "border-border bg-background hover:bg-muted hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:border-input dark:bg-input/30 dark:hover:bg-input/50",
        secondary:
          "bg-secondary text-secondary-foreground hover:bg-[color-mix(in_oklch,var(--secondary),var(--foreground)_5%)] aria-expanded:bg-secondary aria-expanded:text-secondary-foreground",
        ghost:
          "hover:bg-muted hover:text-foreground aria-expanded:bg-muted aria-expanded:text-foreground dark:hover:bg-muted/50",
        destructive:
          "bg-destructive/10 text-destructive hover:bg-destructive/20 focus-visible:border-destructive/40 focus-visible:ring-destructive/20 dark:bg-destructive/20 dark:hover:bg-destructive/30 dark:focus-visible:ring-destructive/40",
        // #235 DIVERGENCE: running-text links carry their
        // underline always (THEME §1: underline + weight, same
        // colour as text) — not only on hover.
        link: "text-primary underline underline-offset-4 hover:underline",
      },
      size: {
        default:
          "h-8 gap-1.5 px-2.5 has-data-[icon=inline-end]:pr-2 has-data-[icon=inline-start]:pl-2",
        xs: "h-6 gap-1 rounded-[min(var(--radius-md),10px)] px-2 text-xs in-data-[slot=button-group]:rounded-lg has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3",
        sm: "h-7 gap-1 rounded-[min(var(--radius-md),12px)] px-2.5 text-[0.8rem] in-data-[slot=button-group]:rounded-lg has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 [&_svg:not([class*='size-'])]:size-3.5",
        lg: "h-9 gap-1.5 px-2.5 has-data-[icon=inline-end]:pr-2 has-data-[icon=inline-start]:pl-2",
        icon: "size-8",
        "icon-xs":
          "size-6 rounded-[min(var(--radius-md),10px)] in-data-[slot=button-group]:rounded-lg [&_svg:not([class*='size-'])]:size-3",
        "icon-sm":
          "size-7 rounded-[min(var(--radius-md),12px)] in-data-[slot=button-group]:rounded-lg",
        "icon-lg": "size-9",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
)

function Button({
  className,
  variant = "default",
  size = "default",
  asChild = false,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
  }) {
  const Comp = asChild ? Slot.Root : "button"

  return (
    <Comp
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}

export { Button, buttonVariants }
