//! Contained loading indicator for waits under ~5s (library) — #230 Phase 0.
//!
//! The waiting state for a bounded read. `role="status"` announces
//! politely, and the visible text label always accompanies the glyph
//! so reduced motion (spinner frozen by the global gate) loses no
//! information. SYSTEM §8: a spinner is never the sole proof of work,
//! so the label is required.
//!
//! Painted from shadcn tokens (`--muted-foreground` text) — no
//! `--nex-sys-*` value remains. The glyph is the shared `Spinner`
//! (ui/spinner), so every loading state in the rebuild reads the same.

import { cn } from "../lib/utils";
import { Spinner } from "./ui/spinner";

export interface M3LoadingIndicatorProps {
  /** Short waiting description, e.g. "Loading messages". Announced and
   * shown as text — never motion alone. */
  label: string;
  size?: "sm" | "md";
}

export default function M3LoadingIndicator({
  label,
  size = "md",
}: M3LoadingIndicatorProps) {
  return (
    <div
      className={cn(
        "inline-flex items-center gap-2 text-muted-foreground",
        size === "sm" ? "text-xs" : "text-sm",
      )}
      role="status"
    >
      <Spinner className={size === "sm" ? "size-3" : "size-4"} aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}
