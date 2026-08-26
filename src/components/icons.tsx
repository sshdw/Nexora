//! Minimal, generic UI icons (no brand assets).
//! Generic icons only — not AI/robot imagery. 0.3.0 adds the
//! conversation-row action set (export/rename/archive/restore/
//! delete) and the send arrow, all on the same stroke grid.

import type { ComponentPropsWithoutRef } from "react";

export function PlusIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M3 8h10M8 3v10"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function SearchIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M6.5 11.5a5 5 0 100-10 5 5 0 000 10z"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M10.5 10.5l2 2"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function BookIcon(props: ComponentPropsWithoutRef<"svg">) {
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
        d="M3 3.5h12v11l.9"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M6 5.5v7M9 5.5v7M12 5.5v7"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <path
        d="M6 12.6l4-4.1 0 9"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function SettingsIcon(props: ComponentPropsWithoutRef<"svg">) {
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
      <circle cx="9" cy="9" r="2.4" stroke="currentColor" strokeWidth="1.4" />
      <path
        d="M9 1.8l1.06 2.02 2.26.37 1.58-1.7 1.62 1.62-1.7 1.58.37 2.26L16.2 9l-2.01 1.06-.37 2.26 1.7 1.58-1.62 1.62-1.58-1.7-2.26.37L9 16.2l-1.06-2.01-2.26-.37-1.58 1.7-1.62-1.62 1.7-1.58-.37-2.26L1.8 9l2.01-1.06.37-2.26-1.7-1.58 1.62-1.62 1.58 1.7 2.26-.37L9 1.8z"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Icon for the sidebar "Import conversation" entry (FR-011): an arrow
 * arriving into a tray, matching the 18px stroke style of the other icons. */
export function ImportIcon(props: ComponentPropsWithoutRef<"svg">) {
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
        d="M9 2.5v7.5m0 0l3-3m-3 3l-3-3"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M2.5 11.5v2a1.5 1.5 0 001.5 1.5h10a1.5 1.5 0 001.5-1.5v-2"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Export action: an arrow leaving a tray (mirror of ImportIcon). */
export function ExportIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M8 10V2.5m0 0L5.5 5M8 2.5L10.5 5"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M2.5 10.5v2A1.5 1.5 0 004 14h8a1.5 1.5 0 001.5-1.5v-2"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Rename / edit action: a pencil. */
export function PencilIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M11.3 2.4a1.6 1.6 0 012.3 2.3l-8 8L2 13.9l1.2-3.5 8.1-8z"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Archive action: a box with a lid slot. */
export function ArchiveIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <rect
        x="2"
        y="2.5"
        width="12"
        height="3.4"
        rx="1"
        stroke="currentColor"
        strokeWidth="1.4"
      />
      <path
        d="M3.4 5.9v6.6A1.5 1.5 0 004.9 14h6.2a1.5 1.5 0 001.5-1.5V5.9"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <path
        d="M6.4 8.5h3.2"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Restore action: an archive box with an upward arrow. */
export function UnarchiveIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <rect
        x="2"
        y="2.5"
        width="12"
        height="3.4"
        rx="1"
        stroke="currentColor"
        strokeWidth="1.4"
      />
      <path
        d="M3.4 5.9v6.6A1.5 1.5 0 004.9 14h6.2a1.5 1.5 0 001.5-1.5V5.9"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
      <path
        d="M8 12V8.8m0 0L6.4 10.4M8 8.8l1.6 1.6"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Delete action: a trash can. */
export function TrashIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M2.5 4.5h11M6.5 2.5h3M4 4.5l.7 8.1A1.5 1.5 0 006.2 14h3.6a1.5 1.5 0 001.5-1.4l.7-8.1"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M6.7 7.2v4M9.3 7.2v4"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** Send action: an upward arrow (the composed composer's primary). */
export function ArrowUpIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M8 13V3m0 0L3.5 7.5M8 3l4.5 4.5"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function PaperclipIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M13.2 7.6l-4.9 4.9a3.4 3.4 0 01-4.8-4.8l5.3-5.3a2.3 2.3 0 013.2 3.2L6.7 10.9a1.15 1.15 0 01-1.6-1.6l4.5-4.5"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function CloseIcon(props: ComponentPropsWithoutRef<"svg">) {
  return (
    <svg
      aria-hidden="true"
      focusable="false"
      width="12"
      height="12"
      viewBox="0 0 16 16"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <path
        d="M4 4l8 8M12 4l-8 8"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
