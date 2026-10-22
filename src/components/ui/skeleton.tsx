/* ============================================================
   LINEAGE — generated source, hand-adapted (#230 Phase 1)

   Generator    npx shadcn@latest add skeleton (registry source
                read, NOT installed — the CLI is forbidden, see
                NEXORA-COMPONENT-SKILL.md "Installs").
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Reference    shadcn/ui "skeleton" — MIT (shadcn/ui)
   License      MIT

   The loading placeholder for the sidebar conversation list
   (NEXORA-COMPONENT-MAP.md row 19: skeletons mirror the coming
   layout — ROWS, not generic blocks — and row 1, whose sidebar
   composition pairs the list with `SidebarMenuSkeleton`).

   Divergences from the registry recipe, all deliberate:

     1. `import { cn } from "cn"` → `@/lib/utils` (the repo's
        generated `cn`), same divergence as ui/button.tsx and
        ui/input.tsx.
     2. NO `variant`/`size` cva map. shadcn's registry file
        carries variants that encode sizes; Nexora's shape and
        motion come from the theme (§2), and a skeleton is a
        placeholder whose size is set by the layout it stands in
        for. Every call site therefore names its own geometry —
        which is what makes the skeleton mirror the row it
        replaces instead of drifting from it.
     3. `bg-accent` stays: on the black theme `--accent` is the
        neutral hover/raise tone (oklch 0.25), so the placeholder
        is a tone step above the surface with no hue in it —
        the same register as the rest of the mono system.

   The pulse is a CSS animation, so the ONE global
   prefers-reduced-motion gate (src/styles/motion.css) collapses
   it to a single static frame; nothing here starts a JS
   timeline. The skeleton is decorative — the consumer owns the
   `role="status"` / `aria-busy` announcement.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"

function Skeleton({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="skeleton"
      aria-hidden="true"
      className={cn(
        "animate-pulse rounded-md bg-accent [animation-delay:300ms] [animation-fill-mode:both]",
        className,
      )}
      {...props}
    />
  )
}

export { Skeleton }
