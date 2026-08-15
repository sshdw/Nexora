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
