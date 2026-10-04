//! Agent workspace folder state hook (1.3.0; multi-root registry follow-up).
//!
//! Loads the effective root via `get_workspace_root`, the 5-entry MRU picker
//! history via `list_workspace_recent`, and the unbounded registry via
//! `roots_list` (the registry lives apart from the 5-entry ring so registering
//! one more root never evicts another). Folder picking uses the Tauri dialog
//! plugin (`directory: true`); the chosen path is persisted backend-side via
//! `set_workspace_root` (single active root — runs the identical validation
//! as `roots_add`, including the nesting refusal) or `roots_add` (register +
//! activate), which canonicalize, guard, and maintain the registry. Removal
//! (`roots_remove`) unregisters only — directories are never deleted. The
//! backend owns validation; this hook preserves order.

import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";

import {
  type CommandError,
  getWorkspaceRoot,
  listWorkspaceRecent,
  rootsAdd,
  rootsList,
  rootsRemove,
  setWorkspaceRoot,
} from "./tauri";
import { getLocale, tr } from "./strings";

export interface WorkspaceStore {
  root: string | null;
  recent: string[];
  /** Registered roots, active first (`roots_list`). Empty until loaded. */
  roots: string[];
  loading: boolean;
  saving: boolean;
  error: CommandError | null;
  reload: () => Promise<void>;
  /** Open the native folder picker and persist the chosen root. */
  pickFolder: () => Promise<string | null>;
  /** Persist a root from the recent list. */
  selectRecent: (path: string) => Promise<string | null>;
  /** Open the native folder picker and register the chosen root
   * (active on success). Returns the updated registry, or null. */
  addRootFolder: () => Promise<string[] | null>;
  /** Register `path` as a root and make it active. */
  addRoot: (path: string) => Promise<string[] | null>;
  /** Unregister `path` from the registry (never deletes directories). */
  removeRoot: (path: string) => Promise<string[] | null>;
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
  return { kind: "unknown", message: tr(getLocale(), "common.workspaceUnreachable") };
}

export function useWorkspace(): WorkspaceStore {
  const [root, setRoot] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const [roots, setRoots] = useState<string[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);
  const [error, setError] = useState<CommandError | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError(null);
    try {
      const [current, recents, registry] = await Promise.all([
        getWorkspaceRoot(),
        listWorkspaceRecent(),
        rootsList(),
      ]);
      setRoot(current);
      setRecent(recents);
      setRoots(registry.roots);
    } catch (e) {
      setError(toCommandError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const applyRegistry = useCallback(async (): Promise<string[]> => {
    const [current, recents, registry] = await Promise.all([
      getWorkspaceRoot(),
      listWorkspaceRecent(),
      rootsList(),
    ]);
    setRoot(current);
    setRecent(recents);
    setRoots(registry.roots);
    return registry.roots;
  }, []);

  const persist = useCallback(
    async (path: string): Promise<string | null> => {
      setSaving(true);
      setError(null);
      try {
        // `set_workspace_root` runs the identical registry validation as
        // `roots_add` backend-side (including the nesting refusal) and joins
        // the registry, so refresh the registry view alongside root+recent.
        const canonical = await setWorkspaceRoot(path);
        const [recents, registry] = await Promise.all([
          listWorkspaceRecent(),
          rootsList(),
        ]);
        setRoot(canonical);
        setRecent(recents);
        setRoots(registry.roots);
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

  const addRoot = useCallback(
    async (path: string): Promise<string[] | null> => {
      setSaving(true);
      setError(null);
      try {
        await rootsAdd(path);
        return await applyRegistry();
      } catch (e) {
        setError(toCommandError(e));
        return null;
      } finally {
        setSaving(false);
      }
    },
    [applyRegistry],
  );

  const removeRoot = useCallback(
    async (path: string): Promise<string[] | null> => {
      setSaving(true);
      setError(null);
      try {
        await rootsRemove(path);
        return await applyRegistry();
      } catch (e) {
        setError(toCommandError(e));
        return null;
      } finally {
        setSaving(false);
      }
    },
    [applyRegistry],
  );

  const addRootFolder = useCallback(async (): Promise<string[] | null> => {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected !== "string" || !selected) return null;
    return addRoot(selected);
  }, [addRoot]);

  useEffect(() => {
    void reload();
  }, [reload]);

  return { root, recent, roots, loading, saving, error, reload, pickFolder, selectRecent, addRoot, removeRoot, addRootFolder };
}
