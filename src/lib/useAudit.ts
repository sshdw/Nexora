//! Code-audit data hook (AC-206).
//!
//! Owns the audit panel's four manual-run machines over their wrappers:
//! `repo_audit` (findings scan), `testgen_drafts` (template scaffolds derived
//! from a fresh scan), `dep_inventory` (read-only lockfile tables), and the
//! confirmed `refactor_apply` write. Mounting never scans — every machine is
//! a guarded manual run (rapid presses collapse into the active one), which
//! is the panel's documented contract. The apply's frontend validation
//! mirrors the backend guards (non-empty path, positive integer range,
//! explicit confirm tick); the backend re-verifies everything before
//! writing. The copy buttons ride the same machines (clipboard only — the
//! copy-failure renders in the drafts error slot).

import { useCallback, useState } from "react";

import {
  depInventory,
  refactorApply,
  repoAudit,
  testgenDrafts,
  type CommandError,
  type DepInventory,
  type RefactorApplyResult,
  type RepoAuditReport,
  type TestDraft,
  type TestgenReport,
} from "./tauri";
import { getLocale, tr } from "./strings";

export interface AuditStore {
  report: RepoAuditReport | null;
  running: boolean;
  error: string | null;
  /** Run the read-only audit; no-op while a scan is in flight. */
  run: () => Promise<void>;
  drafts: TestgenReport | null;
  draftsRunning: boolean;
  draftsError: string | null;
  /** Key of the draft whose code was copied last (`path:line:fn`). */
  copiedKey: string | null;
  /** Generate draft scaffolds; no-op while a generation is in flight.
   * Clears the previous copy marker. */
  generate: () => Promise<void>;
  /** Copy one draft's code; failure renders in the drafts error slot. */
  copyDraft: (draft: TestDraft) => Promise<void>;
  deps: DepInventory | null;
  depsRunning: boolean;
  depsError: string | null;
  /** Load the lockfile inventory; no-op while a load is in flight. */
  loadDeps: () => Promise<void>;
  applyResult: RefactorApplyResult | null;
  applyRunning: boolean;
  applyError: string | null;
  /** Apply one confirmed dead-code removal (raw form input; the hook
   * mirrors the backend's validation before sending). No-op while an
   * apply is in flight. */
  applyRemoval: (
    pathRaw: string,
    startRaw: string,
    endRaw: string,
    confirmed: boolean
  ) => Promise<void>;
  /** Route a caller-side failure (the shared issue-filing dialog) into the
   * same scan-level error slot this store renders. */
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

export function useAudit(): AuditStore {
  const [report, setReport] = useState<RepoAuditReport | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<TestgenReport | null>(null);
  const [draftsRunning, setDraftsRunning] = useState(false);
  const [draftsError, setDraftsError] = useState<string | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [deps, setDeps] = useState<DepInventory | null>(null);
  const [depsRunning, setDepsRunning] = useState(false);
  const [depsError, setDepsError] = useState<string | null>(null);
  const [applyRunning, setApplyRunning] = useState(false);
  const [applyResult, setApplyResult] = useState<RefactorApplyResult | null>(
    null
  );
  const [applyError, setApplyError] = useState<string | null>(null);

  // Synchronous in-flight guard: rapid Run presses collapse into the active
  // scan instead of stacking backend walks.
  const run = useCallback(async () => {
    if (running) return;
    setRunning(true);
    setError(null);
    try {
      setReport(await repoAudit());
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setRunning(false);
    }
  }, [running]);

  // Same guard shape for draft generation: one batch round trip at a time.
  const generate = useCallback(async () => {
    if (draftsRunning) return;
    setDraftsRunning(true);
    setDraftsError(null);
    setCopiedKey(null);
    try {
      setDrafts(await testgenDrafts());
    } catch (err) {
      setDraftsError(toMessage(err));
    } finally {
      setDraftsRunning(false);
    }
  }, [draftsRunning]);

  const copyDraft = useCallback(async (draft: TestDraft) => {
    const key = `${draft.path}:${draft.line}:${draft.fn_name}`;
    try {
      await navigator.clipboard.writeText(draft.code);
      setCopiedKey(key);
    } catch {
      setDraftsError(tr(getLocale(), "common.copyFailed"));
    }
  }, []);

  // Same guard shape for the dependency inventory: one batch round trip at a
  // time. Inventory only — the backend never writes on this path.
  const loadDeps = useCallback(async () => {
    if (depsRunning) return;
    setDepsRunning(true);
    setDepsError(null);
    try {
      setDeps(await depInventory());
    } catch (err) {
      setDepsError(toMessage(err));
    } finally {
      setDepsRunning(false);
    }
  }, [depsRunning]);

  // Safe apply: one confirmed dead-code removal per press. Frontend
  // validation mirrors the backend guards (non-empty path, positive integer
  // range, explicit tick); the backend re-verifies everything before writing.
  const applyRemoval = useCallback(
    async (
      pathRaw: string,
      startRaw: string,
      endRaw: string,
      confirmed: boolean
    ) => {
      if (applyRunning) return;
      const path = pathRaw.trim();
      const start = Number(startRaw);
      const end = Number(endRaw);
      if (
        path === "" ||
        !Number.isInteger(start) ||
        !Number.isInteger(end) ||
        start < 1 ||
        end < start
      ) {
        setApplyError(tr(getLocale(), "refactor.fillAll"));
        return;
      }
      if (!confirmed) {
        setApplyError(tr(getLocale(), "refactor.needConfirm"));
        return;
      }
      setApplyRunning(true);
      setApplyError(null);
      setApplyResult(null);
      try {
        setApplyResult(
          await refactorApply(path, start, end, "dead-code-candidate")
        );
      } catch (err) {
        setApplyError(toMessage(err));
      } finally {
        setApplyRunning(false);
      }
    },
    [applyRunning]
  );

  return {
    report,
    running,
    error,
    run,
    drafts,
    draftsRunning,
    draftsError,
    copiedKey,
    generate,
    copyDraft,
    deps,
    depsRunning,
    depsError,
    loadDeps,
    applyResult,
    applyRunning,
    applyError,
    applyRemoval,
    reportError: setError,
  };
}
