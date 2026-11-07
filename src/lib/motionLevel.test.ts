//! Adaptive motion level wiring (OD-D10·motion — NEXORA-DESIGN-SYSTEM.md
//! §7.3), covering src/lib/motionLevel.ts.
//!
//! §7.3: "QA and tests pin `nexora.motion` before a run, so frames stay
//! reproducible" — every test here seeds (or clears) the storage key
//! BEFORE calling applyMotionLevel(), exactly as a QA run would.
//!
//! The module holds state at module scope (initialized flag, record
//! cache), so each test re-imports it via vi.resetModules() + dynamic
//! import — the same isolation pattern motion.test.tsx uses for its
//! reduced-motion JS half.

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  MOTION_LEVEL_ATTRIBUTE,
  MOTION_STORAGE_KEY,
  type MotionLevel,
} from "./motionLevel";
import { REDUCED_MOTION_QUERY } from "./useReducedMotion";

const DAY_MS = 24 * 60 * 60 * 1000;

/** matchMedia mock whose `matches` answer can be flipped mid-test; the
 *  registered change listeners are captured so a test can fire them. */
function mockMatchMedia(reduced: boolean): { setReduced: (next: boolean) => void } {
  let matches = reduced;
  const listeners = new Set<(event: { matches: boolean }) => void>();
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query === REDUCED_MOTION_QUERY ? matches : false,
    media: query,
    onchange: null,
    addEventListener: (type: string, listener: (event: { matches: boolean }) => void) => {
      if (type === "change") listeners.add(listener);
    },
    removeEventListener: (type: string, listener: (event: { matches: boolean }) => void) => {
      if (type === "change") listeners.delete(listener);
    },
    addListener: (listener: (event: { matches: boolean }) => void) => listeners.add(listener),
    removeListener: (listener: (event: { matches: boolean }) => void) => listeners.delete(listener),
    dispatchEvent: () => false,
  }));
  return {
    setReduced(next: boolean) {
      matches = next;
      for (const listener of listeners) listener({ matches: next });
    },
  };
}

function seed(record: unknown): void {
  window.localStorage.setItem(MOTION_STORAGE_KEY, JSON.stringify(record));
}

function stored(): unknown {
  const raw = window.localStorage.getItem(MOTION_STORAGE_KEY);
  return raw === null ? null : JSON.parse(raw);
}

function level(): string | null {
  return document.documentElement.getAttribute(MOTION_LEVEL_ATTRIBUTE);
}

/** Fresh module instance against the current jsdom globals. */
async function fresh(): Promise<typeof import("./motionLevel")> {
  vi.resetModules();
  return import("./motionLevel");
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.localStorage.clear();
  document.documentElement.removeAttribute(MOTION_LEVEL_ATTRIBUTE);
});

describe("motion level — startup", () => {
  it("starts a fresh device at l3 «живой» and writes the attribute on <html>", async () => {
    mockMatchMedia(false);
    const mod = await fresh();
    expect(mod.applyMotionLevel()).toBe("l3");
    expect(level()).toBe("l3");
    expect(mod.getMotionLevel()).toBe("l3");
  });

  it("bands persisted auto counts per §7.3: 1–3 → l3, 4–40 → l2, 41+ → l1", async () => {
    mockMatchMedia(false);
    for (const [count, expected] of [
      [0, "l3"],
      [3, "l3"],
      [4, "l2"],
      [40, "l2"],
      [41, "l1"],
      [900, "l1"],
    ] as const) {
      window.localStorage.clear();
      seed({ auto: { count, lastUse: Math.floor(Date.now() / 1000) }, manual: null });
      const mod = await fresh();
      expect(mod.applyMotionLevel(), `count ${count}`).toBe(expected);
    }
  });

  it("moves one level up after 21 days idle", async () => {
    mockMatchMedia(false);
    const cases: Array<[number, number, MotionLevel]> = [
      // [count, idle days, expected]
      [41, 21, "l2"],
      [41, 20, "l1"],
      [4, 30, "l3"],
      [0, 30, "l3"],
    ];
    for (const [count, idleDays, expected] of cases) {
      window.localStorage.clear();
      seed({
        auto: { count, lastUse: Math.floor((Date.now() - idleDays * DAY_MS) / 1000) },
        manual: null,
      });
      const mod = await fresh();
      expect(mod.applyMotionLevel(), `${count} uses idle ${idleDays}d`).toBe(expected);
    }
  });

  it("applies a persisted manual choice without touching the frozen counters", async () => {
    mockMatchMedia(false);
    const lastUse = Math.floor(Date.now() / 1000);
    seed({ auto: { count: 12, lastUse }, manual: "l1" });
    const mod = await fresh();
    expect(mod.applyMotionLevel()).toBe("l1");
    expect(level()).toBe("l1");
    // Startup performs no counter writes.
    expect(stored()).toEqual({ auto: { count: 12, lastUse }, manual: "l1" });
  });

  it("falls back to l2 with adaptation off when storage is corrupt or unavailable", async () => {
    mockMatchMedia(false);
    window.localStorage.setItem(MOTION_STORAGE_KEY, "{not json");
    const corrupt = await fresh();
    expect(corrupt.applyMotionLevel()).toBe("l2");
    expect(corrupt.recordSurfaceUse()).toBe("l2");
    // No counter writes against a corrupt record.
    expect(window.localStorage.getItem(MOTION_STORAGE_KEY)).toBe("{not json");

    vi.resetModules();
    // Unavailable storage: no localStorage at all (the property gone —
    // the same signal a denied store produces). Restored in finally so
    // the leak cannot poison sibling tests.
    const original = Object.getOwnPropertyDescriptor(window, "localStorage");
    try {
      Object.defineProperty(window, "localStorage", {
        configurable: true,
        value: undefined,
      });
      const unavailable = await fresh();
      expect(unavailable.applyMotionLevel()).toBe("l2");
      expect(unavailable.recordSurfaceUse()).toBe("l2");
    } finally {
      if (original) Object.defineProperty(window, "localStorage", original);
    }
  });

  it("treats a malformed record the same as a corrupt one", async () => {
    mockMatchMedia(false);
    for (const bad of [
      { auto: { count: -1, lastUse: null }, manual: null },
      { auto: { count: "many", lastUse: null }, manual: null },
      { auto: { count: 1, lastUse: "tuesday" }, manual: null },
      { auto: { count: 1, lastUse: null }, manual: "l9" },
      { manual: "l2" },
      42,
    ]) {
      window.localStorage.clear();
      window.localStorage.setItem(MOTION_STORAGE_KEY, JSON.stringify(bad));
      const mod = await fresh();
      expect(mod.applyMotionLevel(), JSON.stringify(bad)).toBe("l2");
    }
  });
});

describe("motion level — interaction recording", () => {
  it("counts surface uses and steps down the ladder at the §7.3 boundaries", async () => {
    mockMatchMedia(false);
    window.localStorage.clear();
    const mod = await fresh();
    mod.applyMotionLevel();
    // Uses 1–3: l3. The 4th use crosses into l2.
    expect(mod.recordSurfaceUse()).toBe("l3");
    expect(mod.recordSurfaceUse()).toBe("l3");
    expect(mod.recordSurfaceUse()).toBe("l3");
    expect(mod.recordSurfaceUse()).toBe("l2");
    // …and the persisted counters moved with them.
    expect(stored()).toMatchObject({ auto: { count: 4 }, manual: null });
    const lastUse = (stored() as { auto: { lastUse: number } }).auto.lastUse;
    expect(lastUse).toBeGreaterThan(Math.floor((Date.now() - 60_000) / 1000));
  });

  it("does not count a use while reduced motion is on", async () => {
    const media = mockMatchMedia(true);
    window.localStorage.clear();
    const mod = await fresh();
    mod.applyMotionLevel();
    expect(mod.recordSurfaceUse()).toBe("l1");
    expect(level()).toBe("l1");
    expect(window.localStorage.getItem(MOTION_STORAGE_KEY)).toBeNull();
    // Adaptation resumes from where it froze when reduce lifts.
    media.setReduced(false);
    expect(mod.recordSurfaceUse()).toBe("l3");
  });

  it("keeps a manual choice and freezes the counters under recording", async () => {
    mockMatchMedia(false);
    const lastUse = Math.floor(Date.now() / 1000);
    seed({ auto: { count: 12, lastUse }, manual: "l2" });
    const mod = await fresh();
    mod.applyMotionLevel();
    expect(mod.recordSurfaceUse()).toBe("l2");
    // The frozen counters were neither read nor written (§7.3).
    expect(stored()).toEqual({ auto: { count: 12, lastUse }, manual: "l2" });
  });

  it("recomputes with the 21-day idle bump at the start of an interaction", async () => {
    mockMatchMedia(false);
    seed({
      auto: { count: 41, lastUse: Math.floor((Date.now() - 22 * DAY_MS) / 1000) },
      manual: null,
    });
    const mod = await fresh();
    expect(mod.applyMotionLevel()).toBe("l2"); // idle bump at startup
    // The interaction's START is still idle (lastUse is 22 days old),
    // so the welcome-back use keeps the bumped level before the reset
    // counter takes over on the next one.
    expect(mod.recordSurfaceUse()).toBe("l2");
    expect(mod.recordSurfaceUse()).toBe("l1"); // idle reset, band(43)
    expect((stored() as { auto: { lastUse: number } }).auto.lastUse)
      .toBeGreaterThanOrEqual(Math.floor((Date.now() - 5_000) / 1000));
  });
});

describe("motion level — manual choice and reduced-motion override", () => {
  it("persists a manual choice, then returns to auto with counters frozen intact", async () => {
    mockMatchMedia(false);
    window.localStorage.clear();
    const mod = await fresh();
    mod.applyMotionLevel();
    mod.recordSurfaceUse();
    mod.recordSurfaceUse();
    mod.setManualMotionLevel("l3");
    expect(mod.getMotionLevel()).toBe("l3");
    expect(stored()).toMatchObject({ manual: "l3", auto: { count: 2 } });
    // Recording while manual: no counter change.
    mod.recordSurfaceUse();
    expect(stored()).toMatchObject({ auto: { count: 2 } });
    // Back to auto: the frozen counters resume.
    mod.setManualMotionLevel(null);
    expect(mod.getMotionLevel()).toBe("l3"); // band(2) — still inside 1–3
    expect(window.localStorage.getItem(MOTION_STORAGE_KEY)).toBeNull();
  });

  it("prefers-reduced-motion overrides a manual choice, live", async () => {
    const media = mockMatchMedia(false);
    window.localStorage.clear();
    const mod = await fresh();
    mod.setManualMotionLevel("l3");
    expect(level()).toBe("l3");
    // The OS preference flips mid-session — the module re-applies.
    media.setReduced(true);
    // setReduced fires the change listener synchronously; the module's
    // handler re-runs applyMotionLevel().
    expect(level()).toBe("l1");
    media.setReduced(false);
    expect(level()).toBe("l3");
  });

  it("stays l1 when matchMedia is entirely unavailable (fail-closed)", async () => {
    mockMatchMedia(false);
    vi.stubGlobal("matchMedia", undefined);
    const mod = await fresh();
    expect(mod.applyMotionLevel()).toBe("l1");
    expect(level()).toBe("l1");
  });
});
