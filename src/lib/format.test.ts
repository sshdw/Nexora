//! Regression: `formatRelativeTime` must not throw when the browser reports
//! a non-BCP-47 locale (WebKit's JavaScriptCore on a C/POSIX-locale box
//! reports `navigator.language === "c"`, and `new Intl.DateTimeFormat("c")`
//! throws `RangeError` there — both engines reject `"c"` (verified on
//! node/V8: constructor, `supportedLocalesOf` and `getCanonicalLocales`
//! all throw), so only the real Tauri build crashed because only there
//! was the reported tag ever `"c"`, unmounting the conversation list).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { formatBytes, formatRelativeTime } from "./format";

/** Fixed instant all invalid-locale tests freeze time at (2026-10-15,
 * a Thursday): timestamps below are picked against it so they land on an
 * `Intl` branch deterministically instead of depending on when the suite
 * runs. Expected values are computed with an explicit `en-US` formatter
 * at assertion time, so they hold in any host timezone. */
const FROZEN_NOW_MS = new Date("2026-10-15T12:00:00.000Z").getTime();
const DAY_S = 86_400;

/** Override the read-only `navigator.language` getter for one test. */
function stubLanguage(tag: string): void {
  vi.spyOn(window.navigator, "language", "get").mockReturnValue(tag);
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("formatRelativeTime with an invalid browser locale (JSC reports \"c\")", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(FROZEN_NOW_MS);
  });

  it("formats through the en-US fallback on the month/day Intl branch", () => {
    stubLanguage("c");
    // Five days before the frozen instant: past the compact-label and
    // yesterday branches, same year — reaches `formatLocale` with
    // `{ month: "short", day: "numeric" }`. Pre-fix this threw
    // `RangeError`; post-fix it formats as en-US.
    const ts = Math.floor(FROZEN_NOW_MS / 1_000) - 5 * DAY_S;
    const expected = new Intl.DateTimeFormat("en-US", {
      month: "short",
      day: "numeric",
    }).format(new Date(ts * 1_000));
    expect(formatRelativeTime(ts)).toBe(expected);
  });

  it("formats through the en-US fallback on the month/day/year Intl branch", () => {
    stubLanguage("c");
    // Previous calendar year: reaches `formatLocale` with
    // `{ month: "short", day: "numeric", year: "numeric" }`.
    const ts = Math.floor(new Date("2025-06-15T12:00:00.000Z").getTime() / 1_000);
    const expected = new Intl.DateTimeFormat("en-US", {
      month: "short",
      day: "numeric",
      year: "numeric",
    }).format(new Date(ts * 1_000));
    expect(formatRelativeTime(ts)).toBe(expected);
  });

  it("falls back to en-US formatting rather than crashing (Intl branch shape)", () => {
    stubLanguage("c");
    // Same month/day branch as above: assert the en-US shape as well as
    // the exact value, so a silent locale-table change cannot pass.
    const ts = Math.floor(FROZEN_NOW_MS / 1_000) - 5 * DAY_S;
    const label = formatRelativeTime(ts);
    expect(label).toMatch(/^[A-Z][a-z]{2} \d{1,2}$/);
    const expected = new Intl.DateTimeFormat("en-US", {
      month: "short",
      day: "numeric",
    }).format(new Date(ts * 1_000));
    expect(label).toBe(expected);
  });
});

describe("formatRelativeTime with a valid browser locale (behaviour unchanged)", () => {
  it("keeps the compact labels", () => {
    stubLanguage("en-US");
    const nowS = Math.floor(Date.now() / 1_000);
    expect(formatRelativeTime(nowS)).toBe("Just now");
    expect(formatRelativeTime(nowS - 5 * 60)).toBe("5m");
    expect(formatRelativeTime(nowS - 2 * 3_600)).toBe("2h");
    expect(formatRelativeTime(nowS - DAY_S)).toBe("Yesterday");
  });

  it("keeps formatBytes behaviour", () => {
    expect(formatBytes(null)).toBeNull();
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(1_500)).toBe("1.5 KB");
  });
});
