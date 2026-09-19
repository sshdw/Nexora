//! Per-run spend guard hook (micro-USD budget).
//!
//! Loads the persisted `agent.spend_limit_micro_usd` setting via the existing
//! settings commands. `null` means "no limit" and preserves today's behaviour;
//! a positive integer caps the spend of new agent runs. The backend owns
//! parsing; this hook mirrors the stored value and validates input.

import { useCallback, useEffect, useState } from "react";

import {
  deleteSetting,
  getSetting,
  setSetting,
  type CommandError,
} from "./tauri";

/** Setting key backing the per-run spend guard (micro-USD). */
export const SPEND_LIMIT_KEY = "agent.spend_limit_micro_usd";

export interface SpendLimitStore {
  limitMicroUsd: number | null;
  loading: boolean;
  saving: boolean;
  error: CommandError | null;
  reload: () => Promise<void>;
  /** Persist `value` (`null` clears the limit). Returns false when rejected. */
  setLimit: (value: number | null) => Promise<boolean>;
}

function toCommandError(error: unknown): CommandError {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).kind === "string" &&
    typeof (error as CommandError).message === "string"
  ) {
    const e = error as CommandError;
    return { kind: e.kind, message: e.message };
  }
  if (typeof error === "string") return { kind: "unknown", message: error };
  if (error instanceof Error) return { kind: "unknown", message: error.message };
  return { kind: "unknown", message: "Unable to reach the spend limit setting." };
}

function parseStored(value: string | null): number | null {
  if (value === null) return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isInteger(parsed) || parsed <= 0) return null;
  return parsed;
}

export function useSpendLimit(): SpendLimitStore {
  const [limitMicroUsd, setLimitMicroUsd] = useState<number | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);
  const [error, setError] = useState<CommandError | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const stored = await getSetting(SPEND_LIMIT_KEY);
      setLimitMicroUsd(parseStored(stored));
    } catch (e) {
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const setLimit = useCallback(async (value: number | null): Promise<boolean> => {
    if (value !== null && (!Number.isInteger(value) || value < 0)) {
      setError({ kind: "invalidInput", message: "The spend limit must be a non-negative integer." });
      return false;
    }
    setSaving(true);
    setError(null);
    try {
      if (value === null) {
        await deleteSetting(SPEND_LIMIT_KEY);
        setLimitMicroUsd(null);
      } else {
        await setSetting(SPEND_LIMIT_KEY, String(value));
        setLimitMicroUsd(value);
      }
      return true;
    } catch (e) {
      setError(toCommandError(e));
      return false;
    } finally {
      setSaving(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  return { limitMicroUsd, loading, saving, error, reload, setLimit };
}
