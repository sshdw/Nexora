//! The app's icon layer — one library, one weight, one size grid.
//!
//! Every glyph in the UI comes from Phosphor (`@phosphor-icons/react`) at
//! `weight="regular"`. The exported names are the names the app already
//! used (`PlusIcon`, `ArrowUpIcon`, …) so a call site keeps reading the
//! same word while the whole app changes weight and grid in one place.
//!
//! ONE DOCUMENTED EXCEPTION: `StopIcon` is filled. At 16 px a hollow
//! square reads as a checkbox, not as "stop the run", so the stop glyph
//! is the single filled icon in the product (R6-01). It is built through
//! `filledIcon` below, and `iconsInvariant.test.ts` pins that the fill
//! weight appears exactly once, in that helper.
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
  ArrowDown,
  ArrowUp,
  BookOpen,
  BoxArrowUp,
  CaretDown,
  Check,
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

/** The ONE filled glyph (see the module header): `StopIcon` only. */
function filledIcon(Glyph: Icon, displayName: string) {
  function FilledIcon({ size = 16, ...rest }: UiIconProps) {
    return (
      <Glyph
        size={size}
        weight="fill"
        color="currentColor"
        aria-hidden="true"
        focusable="false"
        {...rest}
      />
    );
  }
  FilledIcon.displayName = displayName;
  return FilledIcon;
}

// ---- the app's existing names, mapped to the closest Phosphor glyph ----

export const PlusIcon = uiIcon(Plus, "PlusIcon");
export const SearchIcon = uiIcon(MagnifyingGlass, "SearchIcon");
export const ChevronDownIcon = uiIcon(CaretDown, "ChevronDownIcon");
export const ArrowUpIcon = uiIcon(ArrowUp, "ArrowUpIcon");
export const ArrowDownIcon = uiIcon(ArrowDown, "ArrowDownIcon");
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

/** Stop: the live-run state of the composer's one action button (D2).
 *  FILLED — the one documented weight exception (R6-01): a hollow square
 *  at 16 px reads as a checkbox, and this glyph means "stop". */
export const StopIcon = filledIcon(Square, "StopIcon");
/** Workspace folder: the composer's workspace control (D3 / R6-03). */
export const FolderIcon = uiIcon(FolderSimple, "FolderIcon");
/** Sessions: the Dock's history destination. */
export const StackIcon = uiIcon(Stack, "StackIcon");
/** Settings: the Dock's preferences destination. */
export const SlidersIcon = uiIcon(SlidersHorizontal, "SlidersIcon");

// ---- glyphs the shell gained in Round 6 -------------------------------

/** Selected row in the model picker (R6-02). */
export const CheckIcon = uiIcon(Check, "CheckIcon");

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
          1.15:1 on dark — a clickable control nobody can see.

          SOLID, never dashed (R6-09). A dashed ring reads as a loading
          spinner; the empty state is a present-but-empty ring, and the
          VALUE arc is what makes a known percentage differ from it. With
          no data there is no arc, so the ring stays visibly empty rather
          than pretending to be 0%. */}
      <circle
        cx="8"
        cy="8"
        r={radius}
        stroke="var(--muted-foreground)"
        strokeWidth="1.5"
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

export interface ChatGptMarkIconProps {
  size?: UiIconSize;
  className?: string;
}

/** Monochrome OpenAI / ChatGPT blossom mark for the official Sign in with
 *  ChatGPT control and composer plan-usage notice. Paints with `currentColor`
 *  only so it adapts to every theme without raw color literals. */
export function ChatGptMarkIcon({
  size = 16,
  className,
}: ChatGptMarkIconProps) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className}
    >
      <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.2a.076.076 0 0 1 .071 0l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z" />
    </svg>
  );
}
