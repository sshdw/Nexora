/* ============================================================
   LINEAGE — generated source, hand-adapted (#230 Phase 0)

   Generator    npx shadcn@latest add textarea (registry source read,
                NOT installed — the CLI is forbidden, see
                NEXORA-COMPONENT-SKILL.md "Installs").
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Reference    shadcn/ui "textarea" — MIT (shadcn/ui)
   License      MIT

   Part of the #230 Phase 0 text-field primitive set
   (NEXORA-DESIGN-SYSTEM.md §3). Same divergences as ui/input.tsx:
   `@/lib/utils` for `cn`, no resting shadow, no arbitrary ring
   width (`ring-3`), no `dark:` colour overrides (dark is the
   default root here). A min-height replaces the registry's
   `field-sizing-content` so the field is predictable without
   relying on a newer sizing keyword.

   Every colour resolves to a shadcn semantic token in
   src/index.css; there is no raw value here.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"

function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(
        "flex min-h-16 w-full rounded-md border border-input bg-transparent px-2.5 py-2 text-sm text-foreground transition-[color,box-shadow] outline-none",
        "placeholder:text-muted-foreground selection:bg-primary/25 selection:text-foreground",
        "focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20",
        "disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
}

export { Textarea }
