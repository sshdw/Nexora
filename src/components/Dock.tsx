//! Edge-attached Dock — Round 4 (R4-01 / R4-07 / R4-09).
//!
//! A vertical icon-only rail glued to the left edge of the window:
//! New → Workspace → Sessions → Activity → separator → Settings.
//!
//! Geometry (R4-01): left edge flush (`left: 0`), width 60 px (60–64 range),
//! content height ≈ 253 px (240–270 range), vertically centred in the window,
//! left side flat, right side rounded ~30 px (exception to the 4 / 8 / 12
//! shape scale), 5 px internal gap (exception to the spacing scale),
//! 44 × 44 px button hit areas.
//!
//! Motion (R4-07): CSS transitions/keyframes only — transform/opacity, never
//! width/height/top/left. Durations/easings from src/styles/motion.css
//! (--motion-duration-short2 100 ms hover/focus, --motion-duration-press 90 ms,
//! --motion-easing-standard). No motion/framer-motion/gsap/react-spring/auto-animate.
//! Reduced motion honoured by the global gate in src/styles/motion.css.
//!
//! Tokens: every colour/duration/radius/shape/space comes from
//! src/styles/motion.css, src/styles.css or src/index.css. The only literals
//! are the R4-01 documented exceptions (5 px gap, 30 px radius,
//! 6 px attention dot, 44 × 44 hit area) and framework scale steps. The rail
//! WIDTH is not a literal: it reads `--dock-width` from src/index.css, which
//! is also what the conversation stage reserves, so the two can never
//! disagree about how much room the rail takes.

import type { ComponentPropsWithoutRef, ReactNode } from "react";

import { cn } from "@/lib/utils";
import { ActivityIcon, PlusIcon } from "./icons";

type DockPanel = "workspace" | "sessions" | "activity" | "settings";

export interface DockProps {
  /** Panel currently open; drives `active` fill + `aria-expanded`. */
  activePanel?: DockPanel | null;
  /** Attention count shown on the Activity button (static amber dot). */
  attentionCount?: number;
  /** New command — never a destination, never `active`. */
  onNew?: () => void;
  /** Opens / closes a dock panel. */
  onPanelToggle?: (panel: DockPanel) => void;
}

/* ------------------------------------------------------------------ */
/* Local icons (kept inside Dock per ownership boundary — icons.tsx is
   not touched). 18 px stroke grid, same style as src/components/icons.tsx. */
/* ------------------------------------------------------------------ */

function FolderIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="18"
      height="18"
      viewBox="0 0 18 18"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M2 6h6l2 2h8a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h4z"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function SessionsIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="18"
      height="18"
      viewBox="0 0 18 18"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <rect
        x="2"
        y="3"
        width="14"
        height="4"
        rx="0.5"
        stroke="currentColor"
        strokeWidth="1.4"
        fill="none"
        strokeLinejoin="round"
      />
      <rect
        x="2"
        y="6"
        width="14"
        height="4"
        rx="0.5"
        stroke="currentColor"
        strokeWidth="1.4"
        fill="none"
        strokeLinejoin="round"
      />
      <rect
        x="2"
        y="9"
        width="14"
        height="4"
        rx="0.5"
        stroke="currentColor"
        strokeWidth="1.4"
        fill="none"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function SlidersIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="18"
      height="18"
      viewBox="0 0 18 18"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <line
        x1="4"
        y1="6"
        x2="14"
        y2="6"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <circle cx="4" cy="6" r="1.2" fill="currentColor" />
      <circle cx="14" cy="6" r="1.2" fill="currentColor" />
      <line
        x1="4"
        y1="12"
        x2="12"
        y2="12"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <circle cx="4" cy="12" r="1.2" fill="currentColor" />
      <circle cx="12" cy="12" r="1.2" fill="currentColor" />
    </svg>
  );
}

/* ------------------------------------------------------------------ */
/* Dock button + tooltip bubble + separator */
/* ------------------------------------------------------------------ */

interface DockButtonProps {
  id: string;
  kind: "command" | "panel";
  panel?: DockPanel;
  label: string;
  tooltip: string;
  icon: ReactNode;
  isActive?: boolean;
  hasAttention?: boolean;
  onClick?: () => void;
}

function DockButton({
  kind,
  panel,
  label,
  tooltip,
  icon,
  isActive = false,
  hasAttention = false,
  onClick,
}: DockButtonProps) {
  const expanded = kind === "panel" && isActive;

  return (
    <button
      type="button"
      aria-expanded={kind === "panel" ? expanded : undefined}
      aria-controls={kind === "panel" ? `nex-dock-panel-${panel}` : undefined}
      aria-label={label}
      className={cn(
        "group/dock-btn relative flex size-11 shrink-0 items-center justify-center rounded-[var(--shape-full)] border border-transparent outline-none [&_svg]:pointer-events-none [&_svg]:shrink-0",
        /* hover/focus tone — 100 ms, standard easing (transform/opacity only) */
        "transition-[transform,opacity]",
        "[transition-duration:var(--motion-duration-short2)]",
        /* press — 90 ms scale in, instant release */
        "active:[transform:scale(var(--motion-scale-press))]",
        "active:[transition-duration:var(--motion-duration-press),var(--motion-duration-short2),var(--motion-duration-short2)]",
        /* rest */
        "bg-transparent text-[var(--color-on-surface-variant)]",
        "hover:bg-[var(--state-hover)] hover:text-[var(--color-on-surface)]",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background focus-visible:z-10",
        /* active (10 % white fill + white icon) */
        expanded
          ? "bg-[var(--state-selected)] text-[var(--color-on-surface)]"
          : ""
      )}
      onClick={onClick}
    >
      <span className="inline-flex items-center justify-center" aria-hidden="true">
        {icon}
      </span>
      {hasAttention && (
        <span
          className="absolute top-2 right-2 size-1.5 rounded-full bg-[var(--color-primary)]"
          aria-hidden="true"
        />
      )}
      {/* Tooltip bubble: overlay surface, outside the capsule (paints to the
          right of the button), hover/focus reveal, not clipped. */}
      <span
        aria-hidden="true"
        className={cn(
          "absolute left-full top-1/2 -translate-y-1/2 translate-x-[var(--space-xxs)]",
          "z-[var(--nex-sys-z-tooltip)] pointer-events-none",
          "bg-[var(--color-surface-floating)] border border-[var(--color-outline-variant)] rounded-[var(--shape-small)]",
          "px-[var(--space-xs)] py-[var(--space-xxs)]",
          "text-[var(--color-popover-foreground)] text-sm font-medium whitespace-nowrap",
          "opacity-0 transition-opacity",
          "[transition-duration:var(--motion-duration-short2)]",
          "[transition-timing-function:var(--motion-easing-standard)]",
          "group-hover/dock-btn:opacity-100 group-focus-within/dock-btn:opacity-100"
        )}
      >
        {tooltip}
      </span>
    </button>
  );
}

function Separator() {
  return (
    <span
      role="separator"
      aria-orientation="horizontal"
      aria-hidden="true"
      className="mx-auto w-6 h-[1px] shrink-0 bg-[var(--color-outline-variant)]"
    />
  );
}

/* ------------------------------------------------------------------ */
/* Dock */
/* ------------------------------------------------------------------ */

export default function Dock({
  activePanel = null,
  attentionCount = 0,
  onNew,
  onPanelToggle,
}: DockProps) {
  const isActive = (panel: DockPanel) => activePanel === panel;
  const activityAttention = attentionCount > 0;
  const activityLabel = activityAttention
    ? `Activity, ${attentionCount} needs you`
    : "Activity";

  return (
    <nav
      role="navigation"
      aria-label="Dock"
      className="fixed left-0 top-1/2 -translate-y-1/2 z-[var(--nex-sys-z-popover)]"
      style={{
        // R4-01: left edge flush, vertically centred; right side rounded ~30 px
        background: "var(--color-surface-elevated)",
        border: `1px solid var(--color-shell-seam)`,
        borderLeft: "none",
        borderRadius: "0 30px 30px 0",
        paddingTop: "var(--space-2xl)",    // 32 px — clears the 30 px top curve
        paddingBottom: "var(--space-2xl)", // 32 px — clears the 30 px bottom curve
        paddingLeft: "var(--space-xs)",    // 8 px
        paddingRight: "var(--space-xs)",   // 8 px
        // R4-01: 60 px rail, from the ONE layout token the conversation
        // stage also reserves (`padding-inline: var(--dock-width)`).
        width: "var(--dock-width)",
      }}
    >
      <div className="flex flex-col gap-[5px] items-center">
        {/* New — command, never active */}
        <DockButton
          id="new"
          kind="command"
          label="New"
          tooltip="New"
          icon={<PlusIcon />}
          onClick={onNew}
        />
        {/* Workspace — panel */}
        <DockButton
          id="workspace"
          kind="panel"
          panel="workspace"
          label="Workspace"
          tooltip="Workspace"
          icon={<FolderIcon />}
          isActive={isActive("workspace")}
          onClick={() => onPanelToggle?.("workspace")}
        />
        {/* Sessions — panel */}
        <DockButton
          id="sessions"
          kind="panel"
          panel="sessions"
          label="Sessions"
          tooltip="Sessions"
          icon={<SessionsIcon />}
          isActive={isActive("sessions")}
          onClick={() => onPanelToggle?.("sessions")}
        />
        {/* Activity — panel + static attention dot + count label */}
        <DockButton
          id="activity"
          kind="panel"
          panel="activity"
          label={activityLabel}
          tooltip={activityLabel}
          icon={<ActivityIcon />}
          isActive={isActive("activity")}
          hasAttention={activityAttention}
          onClick={() => onPanelToggle?.("activity")}
        />
        <Separator />
        {/* Settings — panel (gear/sliders, not a sun) */}
        <DockButton
          id="settings"
          kind="panel"
          panel="settings"
          label="Settings"
          tooltip="Settings"
          icon={<SlidersIcon />}
          isActive={isActive("settings")}
          onClick={() => onPanelToggle?.("settings")}
        />
      </div>
    </nav>
  );
}
