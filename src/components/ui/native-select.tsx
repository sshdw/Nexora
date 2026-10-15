/* ============================================================
   LINEAGE — generated source, hand-adapted (#230 Phase 0)

   Generator    npx shadcn@latest add native-select (registry source
                read, NOT installed — the CLI is forbidden, see
                NEXORA-COMPONENT-SKILL.md "Installs").
   Registry     shadcn/ui — style "radix-nova", base "radix"
   Reference    shadcn/ui "native-select" — MIT (shadcn/ui)
   License      MIT

   The select member of the #230 Phase 0 text-field primitive set
   (NEXORA-DESIGN-SYSTEM.md §3; MAP row 13 settings forms use it).
   A styled native `<select>` — no Radix, no popover: the browser's
   own menu is the accessible, keyboard-complete control, and the
   settings rows want exactly that.

   Divergences from the registry recipe:
     1. `lucide-react` is a DROPPED dep — the chevron is the repo's
        `ChevronDownIcon` from src/components/icons.tsx.
     2. `@/lib/utils` for `cn` (same as ui/button.tsx).
     3. No resting shadow; `ring-3` not an arbitrary ring width; no
        `dark:` colour overrides (dark is the default root).

   Every colour resolves to a shadcn semantic token in
   src/index.css; there is no raw value here.
   ============================================================ */
import * as React from "react"
import { cn } from "@/lib/utils"
import { ChevronDownIcon } from "../icons"

function NativeSelect({
  className,
  ...props
}: React.ComponentProps<"select">) {
  return (
    <div
      data-slot="native-select-wrapper"
      className="group/native-select relative w-fit has-[select:disabled]:opacity-50"
    >
      <select
        data-slot="native-select"
        className={cn(
          "h-8 w-full min-w-0 appearance-none rounded-md border border-input bg-transparent py-1 pr-8 pl-2.5 text-sm text-foreground transition-[color,box-shadow] outline-none",
          "focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
          "aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20",
          "disabled:pointer-events-none disabled:cursor-not-allowed",
          className
        )}
        {...props}
      />
      <ChevronDownIcon
        className="pointer-events-none absolute top-1/2 right-2.5 size-4 -translate-y-1/2 text-muted-foreground select-none"
        aria-hidden="true"
      />
    </div>
  )
}

function NativeSelectOption({ ...props }: React.ComponentProps<"option">) {
  return <option data-slot="native-select-option" {...props} />
}

function NativeSelectOptGroup({ ...props }: React.ComponentProps<"optgroup">) {
  return <optgroup data-slot="native-select-optgroup" {...props} />
}

export { NativeSelect, NativeSelectOption, NativeSelectOptGroup }
