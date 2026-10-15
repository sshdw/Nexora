/* ============================================================
   LINEAGE — generated source, hand-adapted (#230 Phase 0)

   Generator    npx shadcn@latest add input (registry source read,
                NOT installed — the CLI is forbidden, see
                NEXORA-COMPONENT-SKILL.md "Installs").
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Reference    shadcn/ui "input" — MIT (shadcn/ui)
   License      MIT

   Created in #230 Phase 0 as the shared TEXT-FIELD primitive the
   rebuild was missing (NEXORA-DESIGN-SYSTEM.md §3: "Missing shared
   primitive: text input/select/textarea — Phase 0 creates it; raw
   elements migrate to it"). The MAP row 13 (Settings forms) is the
   first consumer; every later panel migrates its raw `<input>` here.

   Divergences from the registry recipe, all deliberate:

     1. `import { cn } from "cn"` → `@/lib/utils` (the repo's
        generated `cn`), same divergence as ui/button.tsx.
     2. No `shadow-xs`: SYSTEM §2 keeps shadows for floating layers
        only. A field separates by tone + a hairline `--input`
        border, never a resting shadow.
     3. `focus-visible:ring-[3px]` → `focus-visible:ring-3`: an
        arbitrary value is forbidden (SYSTEM §1); `ring-3` is the
        same 3px on the theme's `--ring` colour.
     4. No `dark:` colour overrides — this app's dark is the
        default `:root`, so the base classes already paint it.

   Every colour resolves to a shadcn semantic token in
   src/index.css; there is no raw value here.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "flex h-8 w-full min-w-0 rounded-md border border-input bg-transparent px-2.5 py-1 text-sm text-foreground transition-[color,box-shadow] outline-none",
        "placeholder:text-muted-foreground selection:bg-primary selection:text-primary-foreground",
        "file:inline-flex file:h-7 file:border-0 file:bg-transparent file:text-sm file:font-medium file:text-foreground",
        "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
        "aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20",
        "disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
}

export { Input }
