//! Regression: `formatRelativeTime` must not throw when the browser reports
//! a non-BCP-47 locale (WebKit's JavaScriptCore on a C/POSIX-locale box
//! reports `navigator.language === "c"`, and `new Intl.DateTimeFormat("c")`
//! throws `RangeError` there — V8/Chromium accepts it, so only the real
//! Tauri build crashed, unmounting the conversation list).

import { afterEach, describe, expect, it, vi } from "vitest";

import { formatBytes, formatRelativeTime } from "./format";

const DAY_S = 86_400;

/** Override the read-only `navigator.language` getter for one test. */
function stubLanguage(tag: string): void {
  vi.spyOn(window.navigator, "language", "get").mockReturnValue(tag);
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("formatRelativeTime with an invalid browser locale (JSC reports \"c\")", () => {
  it("returns a string instead of throwing (day-old timestamp)", () => {
    stubLanguage("c");
    const dayAgo = Math.floor(Date.now() / 1_000) - DAY_S;
    expect(() => formatRelativeTime(dayAgo)).not.toThrow();
    expect(typeof formatRelativeTime(dayAgo)).toBe("string");
  });

  it("returns a string instead of throwing (previous-year timestamp)", () => {
    stubLanguage("c");
    const lastYear = new Date();
    lastYear.setFullYear(lastYear.getFullYear() - 1);
    expect(() => formatRelativeTime(Math.floor(lastYear.getTime() / 1_000))).not.toThrow();
  });

  it("falls back to en-US formatting rather than crashing (same-day timestamp)", () => {
    stubLanguage("c");
    // 12h ago is same-day in most timezones and exercises `formatLocale`
    // via the hour/minute path... but may cross midnight; either branch
    // must simply not throw.
    const halfDayAgo = Math.floor(Date.now() / 1_000) - 12 * 3_600;
    expect(() => formatRelativeTime(halfDayAgo)).not.toThrow();
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
