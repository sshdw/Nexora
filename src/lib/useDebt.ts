//! Debt-backlog data hook (AC-206).
//!
//! Owns the Debt panel's data machine over the five debt wrappers: the list
//! loads from the persisted rows on mount and after every mutation (the
//! backend is the single source of truth — there are no live events on this
//! path). Importing runs the read-only scanners manually and inserts only
//! fresh findings; mutations re-fetch the list without toggling the list
//! loading flag (only the affected row shows its busy state), exactly like
//! the panel did before the extraction. `create` reports its failure to the
//! caller (the manual-add form renders it inline) without touching the
//! shared list error, preserving the panel's two error slots.

import { useCallback, useEffect, useState } from "react";

import {
  createDebtItem,
  deleteDebtItem,
  importDebtFromAudit,
  listDebtItems,
  updateDebtItemStatus,
  type CommandError,
  type DebtImportResult,
  type DebtItem,
} from "./tauri";

export interface DebtStore {
  items: DebtItem[];
  /** Whether the full list is being (re)fetched. */
  loading: boolean;
  /** Shared list-level error (load, import, status change, delete). */
  error: string | null;
  /** Whether the audit import is in flight. */
  importing: boolean;
  /** The row a status change / delete is running for. */
  busyId: number | null;
  /** Whether the manual-add create is in flight. */
  saving: boolean;
  reload: () => Promise<void>;
  /** Import fresh audit findings; returns the counts for the caller's
   * notice, or null on failure (the shared error carries the message). */
  importFromAudit: () => Promise<DebtImportResult | null>;
  /** Move one row through the triage lifecycle; no-op while another row
   * is busy or the status is unchanged. */
  changeStatus: (item: DebtItem, status: string) => Promise<void>;
  /** Delete one row (the caller owns its confirm dialog). */
  remove: (id: number) => Promise<void>;
  /** Add a manual entry; re-fetches on success. Returns the classified
   * error message on failure (null on success) — the caller renders it
   * in the form slot, not the shared list error. */
  create: (
    title: string,
    severity: string,
    location: string | null,
    note: string | null
  ) => Promise<string | null>;
  /** Route a caller-side failure (the shared issue-filing dialog) into the
   * same list-level error slot this store renders. */
  reportError: (message: string) => void;
}

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

export function useDebt(): DebtStore {
  const [items, setItems] = useState<DebtItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setItems(await listDebtItems());
    } catch (err) {
      setItems([]);
      setError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // Same in-flight guard shape as the audit panel: rapid Import presses
  // collapse into the active run instead of stacking backend scans. The
  // post-import refresh swaps the rows without the list loading flag.
  const importFromAudit =
    useCallback(async (): Promise<DebtImportResult | null> => {
      if (importing) return null;
      setImporting(true);
      setError(null);
      try {
        const result = await importDebtFromAudit();
        setItems(await listDebtItems());
        return result;
      } catch (err) {
        setError(toMessage(err));
        return null;
      } finally {
        setImporting(false);
      }
    }, [importing]);

  const changeStatus = useCallback(
    async (item: DebtItem, status: string) => {
      if (busyId !== null || item.status === status) return;
      setBusyId(item.id);
      setError(null);
      try {
        await updateDebtItemStatus(item.id, status);
        setItems(await listDebtItems());
      } catch (err) {
        setError(toMessage(err));
      } finally {
        setBusyId(null);
      }
    },
    [busyId]
  );

  const remove = useCallback(
    async (id: number) => {
      if (busyId !== null) return;
      setBusyId(id);
      setError(null);
      try {
        await deleteDebtItem(id);
        setItems(await listDebtItems());
      } catch (err) {
        setError(toMessage(err));
      } finally {
        setBusyId(null);
      }
    },
    [busyId]
  );

  const create = useCallback(
    async (
      title: string,
      severity: string,
      location: string | null,
      note: string | null
    ): Promise<string | null> => {
      if (saving) return null;
      setSaving(true);
      setError(null);
      try {
        await createDebtItem(title, severity, location, note);
        setItems(await listDebtItems());
        return null;
      } catch (err) {
        // The manual-add form owns this error slot; the shared list error
        // stays untouched so the failure renders where the form is.
        return toMessage(err);
      } finally {
        setSaving(false);
      }
    },
    [saving]
  );

  return {
    items,
    loading,
    error,
    importing,
    busyId,
    saving,
    reload,
    importFromAudit,
    changeStatus,
    remove,
    create,
    reportError: setError,
  };
}
