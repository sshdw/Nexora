//! Agent workspace folder state hook (1.3.0).
//!
//! Loads the effective root via `get_workspace_root` and the 5-entry recent
//! list via `list_workspace_recent`. Folder picking uses the Tauri dialog
//! plugin (`directory: true`); the chosen path is persisted backend-side via
//! `set_workspace_root`, which canonicalizes, guards, and maintains the
//! recent ring. The backend owns validation; this hook preserves order.

import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";

import {
  type CommandError,
  getWorkspaceRoot,
  listWorkspaceRecent,
  setWorkspaceRoot,
} from "./tauri";

export interface WorkspaceStore {
  root: string | null;
  recent: string[];
  loading: boolean;
  saving: boolean;
  error: CommandError | null;
  reload: () => Promise<void>;
  /** Open the native folder picker and persist the chosen root. */
  pickFolder: () => Promise<string | null>;
  /** Persist a root from the recent list. */
  selectRecent: (path: string) => Promise<string | null>;
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
  return { kind: "unknown", message: "Unable to reach the workspace folder." };
}

export function useWorkspace(): WorkspaceStore {
  const [root, setRoot] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);
  const [error, setError] = useState<CommandError | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const [current, recents] = await Promise.all([
        getWorkspaceRoot(),
        listWorkspaceRecent(),
      ]);
      setRoot(current);
      setRecent(recents);
    } catch (e) {
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const persist = useCallback(
    async (path: string): Promise<string | null> => {
      setSaving(true);
      setError(null);
      try {
        const canonical = await setWorkspaceRoot(path);
        setRoot(canonical);
        setRecent(await listWorkspaceRecent());
        return canonical;
      } catch (e) {
        setError(toCommandError(e));
        return null;
      } finally {
        setSaving(false);
      }
    },
    [],
  );

  const pickFolder = useCallback(async (): Promise<string | null> => {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected !== "string" || !selected) return null;
    return persist(selected);
  }, [persist]);

  const selectRecent = useCallback(
    async (path: string): Promise<string | null> => persist(path),
    [persist],
  );

  useEffect(() => {
    void reload();
  }, [reload]);

  return { root, recent, loading, saving, error, reload, pickFolder, selectRecent };
}
