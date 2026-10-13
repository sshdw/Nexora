/* ============================================================
   LINEAGE — copied source, not an npm dependency (#218)

   Upstream      Origin UI — Tooltip (default registry)
   Repository    https://github.com/origin-space/originui
                 (redirects to https://github.com/cosscom/ui)
   Commit        107dc0a1f011cd99ff047e2beef8201947a17ff2
                 ("feat(composer): preserve structured
                 references and support actions (#872)",
                 2026-10-13)
   File          apps/origin/registry/default/ui/tooltip.tsx
   License       AGPL-3.0 (per repository LICENSE)

   Why copied: the Origin UI Slider copied alongside this file
   imports the registry's Tooltip, so the pair has to move
   together to keep the vendored slider honest. It is also the
   one overlay primitive every future panel needs.

   Nexora changes:
     1. Import path `@/registry/default/lib/utils` → `@/lib/utils`.
     2. `"use client"` dropped (this is a Vite app).
     3. Nothing else. The `animate-in`/`fade-in-0`/`zoom-in-95`
        utilities it mounts are from `tw-animate-css`, and the
        global reduced-motion block in src/index.css collapses
        them to a single frame under
        `prefers-reduced-motion: reduce`.
   ============================================================ */
"use client";

import { Tooltip as TooltipPrimitive } from "radix-ui";
import type * as React from "react";
import { cn } from "@/lib/utils";

function TooltipProvider({
  delayDuration = 0,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Provider>) {
  return (
    <TooltipPrimitive.Provider
      data-slot="tooltip-provider"
      delayDuration={delayDuration}
      {...props}
    />
  );
}

function Tooltip({
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Root>) {
  return (
    <TooltipProvider>
      <TooltipPrimitive.Root data-slot="tooltip" {...props} />
    </TooltipProvider>
  );
}

function TooltipTrigger({
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Trigger>) {
  return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />;
}

function TooltipContent({
  className,
  sideOffset = 4,
  showArrow = false,
  children,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Content> & {
  showArrow?: boolean;
}) {
  return (
    <TooltipPrimitive.Portal>
      <TooltipPrimitive.Content
        className={cn(
          "fade-in-0 zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 relative z-50 max-w-70 animate-in rounded-md border bg-popover px-3 py-1.5 text-popover-foreground text-sm data-[state=closed]:animate-out",
          className,
        )}
        data-slot="tooltip-content"
        sideOffset={sideOffset}
        {...props}
      >
        {children}
        {showArrow && (
          <TooltipPrimitive.Arrow className="-my-px fill-popover drop-shadow-[0_1px_0_var(--border)]" />
        )}
      </TooltipPrimitive.Content>
    </TooltipPrimitive.Portal>
  );
}

export { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger };