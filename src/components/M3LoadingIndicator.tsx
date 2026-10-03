//! M3E contained loading indicator for waits under ~5s (library).
//!
//! OFFICIAL split (skill components.md "Loading indicators"): <200ms none ·
//! 200ms–5s loading indicator · >5s progress indicator. The contained variant
//! sits in a quiet tone pill with a bouncy expressive dot rhythm (transform/
//! opacity only, official spring-to-CSS bezier) — never accent-colored washes
//! near reading surfaces. role="status" announces politely; the visible text
//! label always accompanies the motion so reduced motion (static dots + text
//! via the motion.css gate) loses no information.

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
      className={
        "nex-m3-loading" + (size === "sm" ? " nex-m3-loading-sm" : "")
      }
      role="status"
      aria-label={label}
    >
      <span className="nex-m3-loading-dots" aria-hidden="true">
        <span className="nex-m3-loading-dot" />
        <span className="nex-m3-loading-dot" />
        <span className="nex-m3-loading-dot" />
      </span>
      <span className="nex-m3-loading-label">{label}</span>
    </div>
  );
}
