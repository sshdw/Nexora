/* ============================================================
   Context budget (#220) — the composer's context panel slider.

   WHAT IT IS. The conversation's own usage is already measured
   against the MODEL's context limit, which the user cannot move.
   This is the user's working budget for the conversation: the
   number they actually intend to fill, expressed in the same
   token unit. The panel then answers the question the model
   limit cannot — "am I near the budget I set?" — and says so
   when the budget is exceeded: `ContextPanel` compares
   `total_tokens` against this value and swaps the reading for
   `ctx.budgetExceeded` with the destructive tone, which is the
   behaviour `ctx.budgetHint` promises the reader. This module
   owns the number; the panel owns that reading.

   WHY localStorage AND NOT THE BACKEND. The backend validates
   every `set_setting` key against a fixed domain allowlist
   (`validate_setting`, src-tauri/src/commands/settings.rs), and
   #220 is a zero-Rust-diff change, so a new key would be
   rejected. The app already has the precedent for a device-local
   preference that is not a backend setting: the interface
   language (`nexora.locale`, src/lib/useLocale.ts) and the
   onboarding-completed flag. Same reasoning, same store, and it
   is what the value is: a per-device preference for one surface,
   not workspace data.

   HONEST DEFAULTS. The value is clamped to [MIN, MAX] on read
   as well as on write, so a hand-edited or stale localStorage
   entry can never put the slider outside its own range or make
   the panel print a nonsense budget. Reads never throw: a
   storage failure degrades to the default.
   ============================================================ */

import { useCallback, useEffect, useState } from "react";

import { getLocale, tr } from "./strings";

/** localStorage key for the per-device context budget. */
export const CONTEXT_BUDGET_KEY = "nexora.context.budget_tokens";

/** Slider floor: below this a conversation cannot hold a usable turn. */
export const CONTEXT_BUDGET_MIN = 8_000;
/** Slider ceiling: at/above the largest context windows in the app. */
export const CONTEXT_BUDGET_MAX = 256_000;
/** Step: 8k. One step is a visible thumb move, not a twitch. */
export const CONTEXT_BUDGET_STEP = 8_000;
/** Used when nothing is stored (or storage is unavailable). */
export const CONTEXT_BUDGET_DEFAULT = 128_000;

/** Clamp any number into the slider's own range, and snap it to a step. */
export function clampContextBudget(value: number): number {
  if (!Number.isFinite(value)) return CONTEXT_BUDGET_DEFAULT;
  const clamped = Math.min(CONTEXT_BUDGET_MAX, Math.max(CONTEXT_BUDGET_MIN, value));
  const steps = Math.round((clamped - CONTEXT_BUDGET_MIN) / CONTEXT_BUDGET_STEP);
  const snapped = CONTEXT_BUDGET_MIN + steps * CONTEXT_BUDGET_STEP;
  return Math.min(CONTEXT_BUDGET_MAX, Math.max(CONTEXT_BUDGET_MIN, snapped));
}

/** Parse a stored value; `null` for anything unusable. */
export function parseContextBudget(raw: string | null): number | null {
  if (raw === null) return null;
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed)) return null;
  return clampContextBudget(parsed);
}

/** Read the stored budget (clamped), or the default. */
export function readContextBudget(): number {
  try {
    return (
      parseContextBudget(window.localStorage.getItem(CONTEXT_BUDGET_KEY)) ??
      CONTEXT_BUDGET_DEFAULT
    );
  } catch {
    // Private mode, disabled storage, a hostile value: the panel
    // still renders, on the default.
    return CONTEXT_BUDGET_DEFAULT;
  }
}

/** Persist a budget. Best-effort: a rejected write costs the
 *  preference, never the panel. */
export function writeContextBudget(value: number): void {
  try {
    window.localStorage.setItem(CONTEXT_BUDGET_KEY, String(clampContextBudget(value)));
  } catch {
    // Best-effort by design (see useLocale.persistLocale).
  }
}

export interface ContextBudgetStore {
  /** Current budget in tokens (always inside [MIN, MAX]). */
  budget: number;
  /** Live update from the slider thumb — renders immediately. */
  setBudget: (value: number) => void;
  /** Thumb released / value committed — persists. Split from
   *  `setBudget` so dragging does not write on every pixel. */
  commitBudget: (value: number) => void;
  /** Last persist failure, cleared on the next commit. */
  error: string | null;
}

/**
 * The panel's budget state: localStorage-backed, clamped, and
 * the same value the slider renders and the used-against-budget
 * reading is computed from — one number, not a display copy.
 */
export function useContextBudget(): ContextBudgetStore {
  const [budget, setBudgetState] = useState<number>(readContextBudget);
  const [error, setError] = useState<string | null>(null);

  // A second panel (split view) writes the same key; re-read on
  // mount so both panes agree.
  useEffect(() => {
    setBudgetState(readContextBudget());
  }, []);

  const setBudget = useCallback((value: number) => {
    setBudgetState(clampContextBudget(value));
  }, []);

  const commitBudget = useCallback((value: number) => {
    const next = clampContextBudget(value);
    setBudgetState(next);
    setError(null);
    try {
      window.localStorage.setItem(CONTEXT_BUDGET_KEY, String(next));
    } catch {
      setError(tr(getLocale(), "ctx.budgetSaveFailed"));
    }
  }, []);

  return { budget, setBudget, commitBudget, error };
}