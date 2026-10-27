//! The app's icon layer — one library, one weight, one size grid.
//!
//! Every glyph in the UI comes from Phosphor (`@phosphor-icons/react`) at
//! `weight="regular"`. The exported names are the names the app already
//! used (`PlusIcon`, `ArrowUpIcon`, …) so a call site keeps reading the
//! same word while the whole app changes weight and grid in one place.
//!
//! Sizes are the two steps the shell uses: 16 (default) and 20. A
//! component that needs a different optical size changes the layout, not
//! the glyph.
//!
//! Two shapes legitimately stay custom and live in the block at the
//! bottom of this file: the context-window ring, whose arc length IS the
//! data, and the brand mark (which is an image asset, not a glyph).
//!
//! `src/components/iconsInvariant.test.ts` scans every other `.tsx` in
//! `src/` and fails on a raw `<svg>`: this file is the only place a glyph
//! may be drawn.

import type { Icon, IconProps } from "@phosphor-icons/react";
import {
  ActivityIcon as PhosphorActivity,
  Archive,
  ArrowUp,
  BookOpen,
  BoxArrowUp,
  CaretDown,
  CircleNotch,
  FlagPennant,
  FolderSimple,
  GearSix,
  GitBranch,
  ListChecks,
  MagnifyingGlass,
  Paperclip,
  PencilSimple,
  Plus,
  Record,
  SealCheck,
  SlidersHorizontal,
  Square,
  Stack,
  Tag,
  Terminal,
  Trash,
  TrayArrowDown,
  TrayArrowUp,
  X,
} from "@phosphor-icons/react";

/** The two glyph sizes the shell allows. */
export type UiIconSize = 16 | 20;

/** The shared props every exported glyph accepts: SVG props plus the two
 *  size steps. Weight is fixed by this layer, never by a call site. */
export type UiIconProps = Omit<IconProps, "weight" | "size" | "ref"> & {
  size?: UiIconSize;
};

function uiIcon(Glyph: Icon, displayName: string) {
  function UiIcon({ size = 16, ...rest }: UiIconProps) {
    return (
      <Glyph
        size={size}
        weight="regular"
        color="currentColor"
        aria-hidden="true"
        focusable="false"
        {...rest}
      />
    );
  }
  UiIcon.displayName = displayName;
  return UiIcon;
}

// ---- the app's existing names, mapped to the closest Phosphor glyph ----

export const PlusIcon = uiIcon(Plus, "PlusIcon");
export const SearchIcon = uiIcon(MagnifyingGlass, "SearchIcon");
export const ChevronDownIcon = uiIcon(CaretDown, "ChevronDownIcon");
export const ArrowUpIcon = uiIcon(ArrowUp, "ArrowUpIcon");
export const PaperclipIcon = uiIcon(Paperclip, "PaperclipIcon");
export const CloseIcon = uiIcon(X, "CloseIcon");
export const SettingsIcon = uiIcon(GearSix, "SettingsIcon");
export const ActivityIcon = uiIcon(PhosphorActivity, "ActivityIcon");
export const TerminalIcon = uiIcon(Terminal, "TerminalIcon");
export const TaskIcon = uiIcon(ListChecks, "TaskIcon");
export const BranchIcon = uiIcon(GitBranch, "BranchIcon");
export const BookIcon = uiIcon(BookOpen, "BookIcon");
export const PencilIcon = uiIcon(PencilSimple, "PencilIcon");
export const TrashIcon = uiIcon(Trash, "TrashIcon");
export const ImportIcon = uiIcon(TrayArrowDown, "ImportIcon");
export const ExportIcon = uiIcon(TrayArrowUp, "ExportIcon");
export const ArchiveIcon = uiIcon(Archive, "ArchiveIcon");
export const UnarchiveIcon = uiIcon(BoxArrowUp, "UnarchiveIcon");
export const IssuesIcon = uiIcon(Record, "IssuesIcon");
export const DebtIcon = uiIcon(Tag, "DebtIcon");
export const ReleaseIcon = uiIcon(FlagPennant, "ReleaseIcon");
export const AuditIcon = uiIcon(SealCheck, "AuditIcon");
export const LoaderIcon = uiIcon(CircleNotch, "LoaderIcon");

// ---- glyphs the shell gained in Round 5 -------------------------------

/** Stop: the live-run state of the composer's one action button (D2). */
export const SquareIcon = uiIcon(Square, "SquareIcon");
/** Workspace folder: the read-only context row above the composer (D3). */
export const FolderIcon = uiIcon(FolderSimple, "FolderIcon");
/** Sessions: the Dock's history destination. */
export const StackIcon = uiIcon(Stack, "StackIcon");
/** Settings: the Dock's preferences destination. */
export const SlidersIcon = uiIcon(SlidersHorizontal, "SlidersIcon");

/* ================================================================== */
/* custom, not from the library                                       */
/*                                                                    */
/* The context ring is a DATA drawing, not an icon: the arc length is  */
/* the usage percentage, so it cannot be swapped for a Phosphor glyph  */
/* without losing the reading. Everything about it still obeys the     */
/* layer's contract — 16/20 grid, currentColor for the value, tokens   */
/* for the track, aria-hidden because the BUTTON carries the name.     */
/* ================================================================== */

export interface ContextRingIconProps {
  /** 0–100, or null when the backend reports no token data. */
  percent: number | null;
  size?: UiIconSize;
}

export function ContextRingIcon({ percent, size = 16 }: ContextRingIconProps) {
  const value =
    percent !== null && Number.isFinite(percent)
      ? Math.min(100, Math.max(0, percent))
      : null;
  const radius = 6;
  const circumference = 2 * Math.PI * radius;
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      data-state={value === null ? "na" : "value"}
    >
      {/* The track is the control's BOUNDARY, so it must clear 3:1 against
          the card even when there is no data: `--muted-foreground` measures
          5.64:1 on the dark card, 7.13:1 on the light card and 5.64:1 in
          high contrast. `--color-outline-variant` (the previous value) is
          1.15:1 on dark — a clickable control nobody can see. Dashed when
          there is no data: an empty solid ring would read as a real 0%, and
          the dash is what keeps "no data" distinct from "0% used". */}
      <circle
        cx="8"
        cy="8"
        r={radius}
        stroke="var(--muted-foreground)"
        strokeWidth="1.5"
        strokeDasharray={value === null ? "2 2" : undefined}
      />
      {value !== null && value > 0 && (
        /* The data mark. `currentColor` is the trigger's `--foreground`
           (17.14:1 dark, 18.81:1 light, 17.14:1 high contrast), so a known
           percentage is a bright arc over the quieter boundary — at 100% the
           arc closes into a full bright ring. */
        <circle
          cx="8"
          cy="8"
          r={radius}
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeDasharray={`${(value / 100) * circumference} ${circumference}`}
          transform="rotate(-90 8 8)"
        />
      )}
    </svg>
  );
}
