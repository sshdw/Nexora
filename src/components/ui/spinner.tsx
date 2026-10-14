/* ============================================================
   LINEAGE — generated source, hand-adapted (#230 Phase 0)

   Generator    npx shadcn@latest add spinner (registry source read,
                NOT installed — the CLI is forbidden, see
                NEXORA-COMPONENT-SKILL.md "Installs").
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Reference    shadcn/ui "spinner" — MIT (shadcn/ui)
   License      MIT

   The spinner member of the #230 Phase 0 primitive set (issue #230
   "primitive set (button/input/badge/separator/skeleton/spinner)").
   Used by M3Button's loading state and the M3LoadingIndicator.

   Divergences:
     1. `lucide-react` is a DROPPED dep — the glyph is the repo's
        `LoaderIcon` from src/components/icons.tsx (registered
        there, never an inline one-off).
     2. `role="status"` + `aria-label` are NOT baked in: the
        consuming surface owns the announcement, and SYSTEM §8
        forbids a spinner as the sole proof of work. When the
        caller passes neither, the glyph is decorative
        (`aria-hidden`) rather than announcing a bare "Loading".

   The spin is a CSS utility; the global prefers-reduced-motion gate
   (src/styles/motion.css) collapses it to a single static frame.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"
import { LoaderIcon } from "../icons"

function Spinner({ className, ...props }: React.ComponentProps<"svg">) {
  return (
    <LoaderIcon
      className={cn("size-4 animate-spin", className)}
      {...props}
    />
  )
}

export { Spinner }
