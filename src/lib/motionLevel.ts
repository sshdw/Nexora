//! Adaptive motion level (OD-D10·motion — NEXORA-DESIGN-SYSTEM.md §7.3,
//! landed by review 06 D-03/D-10, P7/P7b).
//!
//! Writes `data-motion-level` on `<html>` (`documentElement`, the same
//! element useAppearance.ts writes `data-theme` on). The CSS half of the
//! contract lives in src/styles/motion.css: exactly three tier override
//! blocks keyed on that attribute, plus the reduced-motion guard that
//! pins L1 regardless of it.
//!
//! Contract rules implemented here, each traceable to §7.3:
//!   - Levels are `l3` «живой» / `l2` «плавный» (default) / `l1` «тихий»
//!     (lowercase attribute values, matching the repo's lowercase
//!     data-theme values; the doc's L3/L2/L1 names).
//!   - Auto signal is repeat use of a surface, NEVER time: 1–3 uses →
//!     l3, 4–40 → l2, 41+ → l1; 21 days idle moves one level up.
//!   - The level is recomputed at the START of an interaction
//!     (recordSurfaceUse — the call sites are the surface entrance PRs:
//!     dialog, popover, tab create, view swap), so it cannot change
//!     mid-animation by construction. No `document.getAnimations()`.
//!   - Manual l3/l2/l1 (persisted choice) disables the counters
//!     entirely — no counter reads, no counter writes. The counters
//!     freeze in place and resume when auto is restored.
//!   - Storage is `localStorage` key `nexora.motion` — device-local,
//!     zero Rust (the SQLite settings key space is a closed allowlist).
//!     Unavailable or corrupt storage → l2 with adaptation off.
//!   - `prefers-reduced-motion` always yields l1; no manual choice can
//!     override it. The OS preference is re-read live, mirroring the
//!     fail-closed pattern of useReducedMotion.ts.
//!   - Only CHARACTER adapts (press, tab create, surface entrances,
//!     stagger — the tokens the tier blocks override). INFORMATION never
//!     adapts: progress, run status, approval, error, streaming. Those
//!     surfaces must never call recordSurfaceUse().

import { prefersReducedMotion, REDUCED_MOTION_QUERY } from "./useReducedMotion";

/** The three adaptive levels, as written into `data-motion-level`.
 *  Lowercase to match the repo's attribute-value convention
 *  (`data-theme="light"` in useAppearance.ts). */
export type MotionLevel = "l3" | "l2" | "l1";

export const MOTION_LEVELS: readonly MotionLevel[] = ["l3", "l2", "l1"];

/** Storage key (§7.3: `nexora.motion`, device-local). */
export const MOTION_STORAGE_KEY = "nexora.motion";

/** Attribute set on `<html>`. */
export const MOTION_LEVEL_ATTRIBUTE = "data-motion-level";

/** §7.3: "21 days idle moves one level up." */
const IDLE_STEP_MS = 21 * 24 * 60 * 60 * 1000;

/** Level the CSS tiers see when storage is unavailable or corrupt. */
const FALLBACK_LEVEL: MotionLevel = "l2";

/** Auto bands (§7.3): repeat use of a surface. */
function band(count: number): MotionLevel {
  if (count <= 3) return "l3";
  if (count <= 40) return "l2";
  return "l1";
}

/** One level up the ladder (toward livelier), capped at l3. */
function oneLevelUp(level: MotionLevel): MotionLevel {
  if (level === "l1") return "l2";
  return "l3";
}

function isMotionLevel(value: unknown): value is MotionLevel {
  return typeof value === "string" && (MOTION_LEVELS as readonly string[]).includes(value);
}

interface StoredRecord {
  /** Frozen auto counters (present whether or not adaptation is active,
   *  so a manual detour does not erase the history it froze). */
  auto: { count: number; lastUse: number | null };
  /** Persisted manual choice, or null for auto adaptation. */
  manual: MotionLevel | null;
}

const FRESH_RECORD: StoredRecord = { auto: { count: 0, lastUse: null }, manual: null };

/** Read + validate the persisted record. `usable:false` = unavailable or
 *  corrupt storage → l2 with adaptation off (§7.3); the counters are
 *  then never written. */
function readRecord(): { record: StoredRecord; usable: boolean } {
  let raw: string | null = null;
  try {
    if (typeof window === "undefined" || typeof window.localStorage === "undefined") {
      return { record: FRESH_RECORD, usable: false };
    }
    raw = window.localStorage.getItem(MOTION_STORAGE_KEY);
  } catch {
    return { record: FRESH_RECORD, usable: false };
  }
  if (raw === null) return { record: FRESH_RECORD, usable: true };
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) throw new Error("not an object");
    const entry = parsed as Record<string, unknown>;
    const auto = entry["auto"];
    if (typeof auto !== "object" || auto === null) throw new Error("bad auto");
    const { count, lastUse } = auto as Record<string, unknown>;
    if (typeof count !== "number" || !Number.isSafeInteger(count) || count < 0) {
      throw new Error("bad count");
    }
    if (lastUse !== null && (typeof lastUse !== "number" || !Number.isFinite(lastUse))) {
      throw new Error("bad lastUse");
    }
    const manual = entry["manual"];
    if (manual !== null && !isMotionLevel(manual)) throw new Error("bad manual");
    return {
      record: {
        auto: { count, lastUse: lastUse as number | null },
        manual: manual as MotionLevel | null,
      },
      usable: true,
    };
  } catch {
    return { record: FRESH_RECORD, usable: false };
  }
}

function writeRecord(record: StoredRecord): void {
  try {
    window.localStorage.setItem(MOTION_STORAGE_KEY, JSON.stringify(record));
  } catch {
    // Write failures leave the in-memory level authoritative; the next
    // launch falls back per readRecord()'s unavailable-storage rule.
  }
}

/** Level derived from the record BEFORE any interaction of this session
 *  (startup). Auto mode applies the 21-day idle bump here without
 *  counting this launch as a use. */
function deriveStartupLevel(record: StoredRecord, usable: boolean): MotionLevel {
  if (!usable) return FALLBACK_LEVEL;
  if (record.manual !== null) return record.manual;
  let level = band(record.auto.count);
  if (
    record.auto.lastUse !== null &&
    Date.now() - record.auto.lastUse * 1000 >= IDLE_STEP_MS
  ) {
    level = oneLevelUp(level);
  }
  return level;
}

function setAttribute(level: MotionLevel): void {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute(MOTION_LEVEL_ATTRIBUTE, level);
}

/** Subscribe to live `prefers-reduced-motion` changes (Safari <14 uses
 *  the deprecated listener API — same fallback as useReducedMotion.ts). */
function onReducedMotionChange(listener: () => void): void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
  const list = window.matchMedia(REDUCED_MOTION_QUERY);
  if (typeof list.addEventListener === "function") {
    list.addEventListener("change", listener);
    return;
  }
  list.addListener(listener);
}

let initialized = false;
let current: MotionLevel = FALLBACK_LEVEL;
let recordCache: StoredRecord = FRESH_RECORD;
let usableCache = true;

/**
 * Startup wiring: decide the level and write the attribute. Idempotent —
 * the reduced-motion subscription is installed once; later calls
 * re-evaluate and re-apply.
 *
 * Call once from src/main.tsx bootstrap(), before the first paint of
 * any animated surface, so entrance/press tiers are in place up front.
 */
export function applyMotionLevel(): MotionLevel {
  if (!initialized) {
    initialized = true;
    onReducedMotionChange(() => {
      void applyMotionLevel();
    });
  }
  const read = readRecord();
  recordCache = read.record;
  usableCache = read.usable;
  current = prefersReducedMotion() ? "l1" : deriveStartupLevel(read.record, read.usable);
  setAttribute(current);
  return current;
}

/**
 * The "start of an interaction" hook (§7.3). Surface entrance PRs call
 * this when a dialog/popover/tab/view interaction BEGINS — the level is
 * recomputed here, before the entrance animation starts, so it cannot
 * change mid-animation by construction.
 *
 * Auto mode counts the use, persists the counters (lastUse in unix
 * seconds — the repo timestamp convention) and re-applies. Manual mode
 * keeps its level and touches no counters (frozen in place). Under
 * reduced motion nothing is recorded — adaptation tracks the motion the
 * user actually experienced — and the level stays l1.
 */
export function recordSurfaceUse(): MotionLevel {
  if (!initialized) applyMotionLevel();
  if (prefersReducedMotion()) {
    current = "l1";
    setAttribute(current);
    return current;
  }
  if (!usableCache || recordCache.manual !== null) {
    // Unavailable/corrupt storage: l2, adaptation off (no counter
    // writes). Manual: the level is chosen, counters disabled (§7.3).
    setAttribute(current);
    return current;
  }
  const nowSeconds = Math.floor(Date.now() / 1000);
  let level = band(recordCache.auto.count + 1);
  if (
    recordCache.auto.lastUse !== null &&
    nowSeconds * 1000 - recordCache.auto.lastUse * 1000 >= IDLE_STEP_MS
  ) {
    level = oneLevelUp(level);
  }
  recordCache = {
    auto: { count: recordCache.auto.count + 1, lastUse: nowSeconds },
    manual: null,
  };
  writeRecord(recordCache);
  current = level;
  setAttribute(current);
  return current;
}

/** The level currently applied to `<html>` (or the fallback before the
 *  first apply call). For QA and tests: pin `nexora.motion` before a
 *  run so frames stay reproducible (§7.3). */
export function getMotionLevel(): MotionLevel {
  return current;
}

/**
 * Persist a manual level choice (§7.3's manual override; the settings
 * toggle UI itself is not part of this package). `null` returns to auto
 * adaptation. Manual mode freezes the counters — they are neither read
 * nor written while a choice is active, and they resume unchanged when
 * auto is restored.
 */
export function setManualMotionLevel(level: MotionLevel | null): MotionLevel {
  if (!initialized) applyMotionLevel();
  recordCache = { auto: recordCache.auto, manual: level };
  usableCache = true;
  if (level === null) {
    try {
      // Clear the persisted record entirely: with no manual choice and
      // no counters of its own this session, a fresh device is the
      // honest auto state. (The in-memory counters above keep the
      // session coherent; persistence restarts clean.)
      window.localStorage.removeItem(MOTION_STORAGE_KEY);
    } catch {
      // Unavailable storage: nothing to remove; the level still applies.
    }
  } else {
    writeRecord(recordCache);
  }
  current = prefersReducedMotion() ? "l1" : (level ?? band(recordCache.auto.count));
  setAttribute(current);
  return current;
}
