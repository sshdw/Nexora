//! Last-session memory (O-13) — the shell reopens what you were reading.
//!
//! The module stores ONE conversation id in `localStorage` and validates
//! nothing beyond "a positive integer": which ids still exist is App's
//! question, not this module's. These pins cover the read contract (absent,
//! valid, garbage), the write contract (remember / forget) and the one
//! behavioural promise in the header — a rejected read or write degrades to
//! "nothing remembered" instead of throwing at startup.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { readLastSession, rememberSession } from "./lastSession";

const KEY = "nexora.lastSession";

beforeEach(() => {
  window.localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("lastSession — read", () => {
  it("returns null when no session was remembered", () => {
    expect(readLastSession()).toBeNull();
  });

  it("returns the stored positive integer id", () => {
    window.localStorage.setItem(KEY, "42");
    expect(readLastSession()).toBe(42);
  });

  it("rejects every value that is not a positive integer", () => {
    // 0 and negatives are not row ids; a float or a non-number is corruption
    // (a hand-edited store, a future format), never a session to reopen.
    for (const raw of ["0", "-3", "1.5", "abc", "", "Infinity", "NaN"]) {
      window.localStorage.setItem(KEY, raw);
      expect(readLastSession(), `raw=${JSON.stringify(raw)}`).toBeNull();
    }
  });

  it("degrades to null when localStorage rejects the read", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage denied");
    });
    expect(readLastSession()).toBeNull();
  });
});

describe("lastSession — write", () => {
  it("remembers an id and forgets it on null", () => {
    rememberSession(7);
    expect(window.localStorage.getItem(KEY)).toBe("7");
    expect(readLastSession()).toBe(7);

    rememberSession(9);
    expect(readLastSession()).toBe(9);

    rememberSession(null);
    expect(window.localStorage.getItem(KEY)).toBeNull();
    expect(readLastSession()).toBeNull();
  });

  it("swallows a rejected write", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });
    expect(() => rememberSession(7)).not.toThrow();
  });

  it("swallows a rejected remove", () => {
    vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => {
      throw new Error("storage denied");
    });
    expect(() => rememberSession(null)).not.toThrow();
  });
});
