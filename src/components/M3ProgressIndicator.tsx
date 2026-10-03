//! M3E progress indicators for processes over ~5s (library).
//!
//! OFFICIAL grammar (skill components.md "Progress indicators"):
//! linear/circular × determinate/indeterminate; stop indicator (4dp dot)
//! required when track contrast drops below 3:1 (`lowContrastTrack`); one
//! indicator per group, never per item. Quiet neutral placement near reading
//! surfaces; the OFFICIAL flat style (wavy is an open decision, default
//! flat). Determinate always renders the % text so the reduced-motion
//! fallback (static fill + text via the motion.css gate, no loop) keeps full
//! information. role="progressbar" with value semantics when determinate.

import { useStrings } from "../lib/useLocale";

export interface M3ProgressIndicatorProps {
  kind?: "linear" | "circular";
  /** 0–100 determinate value; null/undefined = indeterminate. */
  value?: number | null;
  /** Accessible name, e.g. "Exporting conversation". */
  label: string;
  /** Show the numeric % beside determinate indicators (default true —
   * this is the reduced-motion information carrier). */
  showValue?: boolean;
  /** Track contrast <3:1: render the OFFICIAL stop-indicator dot at the
   * fill head (determinate linear only). */
  lowContrastTrack?: boolean;
  size?: "sm" | "md";
}

function clampValue(value: number): number {
  if (Number.isNaN(value)) return 0;
  return Math.min(100, Math.max(0, value));
}

export default function M3ProgressIndicator({
  kind = "linear",
  value = null,
  label,
  showValue = true,
  lowContrastTrack = false,
  size = "md",
}: M3ProgressIndicatorProps) {
  const { t } = useStrings();
  const determinate = value !== null && value !== undefined;
  const clamped = determinate ? clampValue(value) : 0;
  const rounded = Math.round(clamped);
  const sizeClass = size === "sm" ? " nex-m3-progress-sm" : "";
  const indeterminateText = t("progress.loading");
  const percentText = t("progress.percent", { n: rounded });

  if (kind === "circular") {
    // Geometry is value math, not design tokens: pathLength normalizes the
    // ring to 100 units so the dash split reads directly as percent.
    return (
      <div className={"nex-m3-progress-circular-wrap" + sizeClass}>
        <div
          className={
            "nex-m3-progress-circular" +
            sizeClass +
            (determinate ? "" : " is-indeterminate")
          }
          role="progressbar"
          aria-label={label}
          aria-valuemin={determinate ? 0 : undefined}
          aria-valuemax={determinate ? 100 : undefined}
          aria-valuenow={determinate ? rounded : undefined}
          aria-valuetext={determinate ? percentText : indeterminateText}
        >
          <svg
            className="nex-m3-progress-circular-svg"
            viewBox="0 0 32 32"
            aria-hidden="true"
          >
            <circle
              className="nex-m3-progress-circular-track"
              cx="16"
              cy="16"
              r="13"
            />
            <circle
              className="nex-m3-progress-circular-fill"
              cx="16"
              cy="16"
              r="13"
              pathLength={100}
              strokeDasharray={
                determinate ? `${rounded} 100` : "28 72"
              }
              transform="rotate(-90 16 16)"
            />
          </svg>
        </div>
        {determinate && showValue && (
          <span className="nex-m3-progress-value">{rounded}%</span>
        )}
        {/* Indeterminate carries no % text, so expose a text equivalent:
          * screen-reader announcement via the progressbar (aria-valuetext)
          * plus a text node for the reduced-motion static fallback. */}
        {!determinate && <span className="nex-sr-only">{indeterminateText}</span>}
      </div>
    );
  }

  return (
    <div className={"nex-m3-progress-linear-wrap" + sizeClass}>
      <div
        className={
          "nex-m3-progress-linear" + (determinate ? "" : " is-indeterminate")
        }
        role="progressbar"
        aria-label={label}
        aria-valuemin={determinate ? 0 : undefined}
        aria-valuemax={determinate ? 100 : undefined}
        aria-valuenow={determinate ? rounded : undefined}
        aria-valuetext={determinate ? percentText : indeterminateText}
      >
        <span
          className="nex-m3-progress-linear-fill"
          aria-hidden="true"
          style={determinate ? { width: `${rounded}%` } : undefined}
        />
        {determinate && lowContrastTrack && (
          <span
            className="nex-m3-progress-stop"
            aria-hidden="true"
            style={{ left: `${rounded}%` }}
          />
        )}
      </div>
      {determinate && showValue && (
        <span className="nex-m3-progress-value">{rounded}%</span>
      )}
      {/* Indeterminate carries no % text — same text equivalent as the
        * circular branch above for the reduced-motion static fallback. */}
      {!determinate && <span className="nex-sr-only">{indeterminateText}</span>}
    </div>
  );
}
