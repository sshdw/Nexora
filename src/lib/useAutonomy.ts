//! Agent-autonomy default store (NEX-FE-003).
//!
//! `agent.autonomy` was mirrored in independent `useState`s in BOTH
//! `SettingsView` and `ConversationView`, each loading the persisted value on
//! mount — a change in one surface stayed stale in the other until remount or
//! the Settings focus-resync hack. This store is instantiated ONCE in `App`
//! (like `useSpendLimit`) and passed to both surfaces, so either surface's
//! change reflects in the other immediately. The conversation header keeps
//! owning the live-switch of running agents (`agentSetMode` loop stays in
//! `ConversationView`); this store only owns the persisted default.
//!
//! Writes are optimistic with rollback: a rejected persist restores the
//! previous mode (the old Settings behavior) so the selector never shows a
//! mode that was never persisted, and both surfaces stay consistent.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  getSetting,
  setSetting,
  type AutonomyMode,
} from "./tauri";
import { getLocale, tr } from "./strings";

/** Setting key backing the agent-autonomy default for new runs. */
export const AUTONOMY_KEY = "agent.autonomy";

/** Fallback when nothing (or nothing parseable) is persisted. */
export const AUTONOMY_DEFAULT: AutonomyMode = "semi_autonomous";

export function isAutonomyMode(value: string | null): value is AutonomyMode {
  return (
    value === "supervised" ||
    value === "semi_autonomous" ||
    value === "full_autonomous"
  );
}

export interface AutonomyStore {
  /** Persisted autonomy default for new runs (optimistic during writes). */
  mode: AutonomyMode;
  loading: boolean;
  /** Last persist failure, cleared on the next attempt. */
  error: string | null;
  reload: () => Promise<void>;
  /** Persist `mode` (optimistic, rolls back on failure). Resolves `null` on
   * success, the failure message on rejection (unlike `useSpendLimit`'s
   * boolean: the conversation header needs the message synchronously for
   * its composer error surface). */
  setMode: (mode: AutonomyMode) => Promise<string | null>;
}

export function useAutonomy(): AutonomyStore {
  const [mode, setModeState] = useState<AutonomyMode>(AUTONOMY_DEFAULT);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  // Ref mirror of `mode` so the optimistic write can capture the previous
  // value synchronously (no side effects inside a state updater).
  const modeRef = useRef<AutonomyMode>(AUTONOMY_DEFAULT);
  const syncMode = useCallback((next: AutonomyMode) => {
    modeRef.current = next;
    setModeState(next);
  }, []);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const stored = await getSetting(AUTONOMY_KEY);
      syncMode(isAutonomyMode(stored) ? stored : AUTONOMY_DEFAULT);
    } catch {
      // Offline-safe: settings live in local SQLite; a transient read
      // failure keeps the default without blocking startup.
      syncMode(AUTONOMY_DEFAULT);
    } finally {
      setLoading(false);
    }
  }, [syncMode]);

  const setMode = useCallback(async (next: AutonomyMode): Promise<string | null> => {
    const previous = modeRef.current;
    syncMode(next);
    setError(null);
    try {
      await setSetting(AUTONOMY_KEY, next);
      return null;
    } catch (e) {
      // Roll back the optimistic control: a rejected write must not leave
      // either surface showing a mode that was never persisted.
      syncMode(previous);
      const message =
        typeof e === "object" && e !== null && "message" in e
          ? String((e as { message: unknown }).message)
          : tr(getLocale(), "settings.autonomySaveFail");
      setError(message);
      return message;
    }
  }, [syncMode]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Memoized store identity (NEX-FE-002 audit): App passes the store object
  // as props to Settings and both conversation panes.
  return useMemo(
    () => ({ mode, loading, error, reload, setMode }),
    [mode, loading, error, reload, setMode],
  );
}
