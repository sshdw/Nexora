// Display-only helpers for rendering backend data.
// Timestamps from the backend are Unix seconds (SQLite `unixepoch()`),
// per DATABASE.md §7.1, so they are scaled to milliseconds for `Date`.

const SECOND_MS = 1_000;
const MINUTE_MS = 60 * SECOND_MS;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

function sameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

function formatLocale(date: Date, options: Intl.DateTimeFormatOptions): string {
  return new Intl.DateTimeFormat(navigator.language, options).format(date);
}

/** Format a conversation's `updated_at` (seconds) as a calm, compact label. */
export function formatRelativeTime(seconds: number): string {
  const ms = seconds * SECOND_MS;
  const now = Date.now();
  const diffMs = now - ms;

  // Freshly created or within the last 30s.
  if (diffMs < 30_000) return "Just now";
  if (diffMs < HOUR_MS) return `${Math.floor(diffMs / MINUTE_MS)}m`;
  if (diffMs < DAY_MS) return `${Math.floor(diffMs / HOUR_MS)}h`;

  const date = new Date(ms);
  const today = new Date();
  const yesterday = new Date(now - DAY_MS);

  if (sameDay(date, today)) return formatLocale(date, { hour: "numeric", minute: "2-digit" });
  if (sameDay(date, yesterday)) return "Yesterday";
  if (date.getFullYear() === today.getFullYear()) {
    return formatLocale(date, { month: "short", day: "numeric" });
  }
  return formatLocale(date, { month: "short", day: "numeric", year: "numeric" });
}

/** Format a byte count as a compact human label ("1.2 MB"); returns null for
 * unknown sizes so callers can omit the line entirely. */
export function formatBytes(bytes: number | null): string | null {
  if (bytes === null || bytes < 0) return null;
  if (bytes < 1_000) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = -1;
  do {
    value /= 1_000;
    unit += 1;
  } while (value >= 1_000 && unit < units.length - 1);
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/** Best-effort media type for a file name, used only as optional attachment
 * metadata (DATABASE.md §7.4 `mime_type` is nullable). This is NOT an
 * allowlist: any file type can be attached; unknown extensions yield null. */
const MIME_BY_EXTENSION: Record<string, string> = {
  pdf: "application/pdf",
  txt: "text/plain",
  md: "text/markdown",
  csv: "text/csv",
  json: "application/json",
  xml: "application/xml",
  html: "text/html",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  zip: "application/zip",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
};

export function guessMimeType(fileName: string): string | null {
  const dot = fileName.lastIndexOf(".");
  if (dot < 0 || dot === fileName.length - 1) return null;
  return MIME_BY_EXTENSION[fileName.slice(dot + 1).toLowerCase()] ?? null;
}
