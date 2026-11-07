//! GitHub panel data hook (AC-206).
//!
//! Owns the Issues & PRs + Actions panel's data machines over the read-only
//! wrappers (`gh_issues` / `gh_pulls` / `gh_actions` / `gh_runners` /
//! `gh_action_log`) plus the fix-loop's `createTask` write. Loads fire on
//! mount and on every kind/state change (reads are cheap GETs) plus manual
//! refresh — no watching or live polling. A successful list/actions load
//! clears the transient selection and fix-loop state, exactly like the
//! panel's own loaders did before the extraction. The fix task's
//! title/description/steps are built by the caller (they are presentation
//! strings); this store runs the guarded create. Lazy job logs degrade to
//! the honest unavailable state on failure instead of erroring the section.

import { useCallback, useRef, useState } from "react";

import {
  createTask,
  ghActionLog,
  ghActions,
  ghIssues,
  ghPulls,
  ghRunners,
  type CommandError,
  type GhActionJob,
  type GhActionLog,
  type GhActionRun,
  type GhIssue,
  type GhPull,
  type GhRateLimit,
  type GhRunner,
  type GhState,
  type GhToolReadiness,
} from "./tauri";

export interface GhStore {
  /** Shared in-flight flag for the list and actions loads (the Refresh
   * button's disabled/label state across both tabs). */
  loading: boolean;
  owner: string | null;
  repo: string | null;
  authenticated: boolean;
  rateLimited: boolean;
  rate: GhRateLimit | null;
  issues: GhIssue[];
  pulls: GhPull[];
  /** List-tab (issues/pulls) error. */
  error: string | null;
  /** Selected list item number (toggle), reset by a successful load. */
  selected: number | null;
  loadIssues: (state: GhState) => Promise<void>;
  loadPulls: (state: GhState) => Promise<void>;
  selectItem: (number_: number) => void;
  clearItemSelection: () => void;
  runs: GhActionRun[];
  totalRuns: number;
  actionsError: string | null;
  /** Selected actions-run id (toggle), reset by a successful load. */
  selectedRun: number | null;
  loadActions: () => Promise<boolean>;
  selectRun: (id: number) => void;
  /** Reset everything the kind switch clears (selections + fix state). */
  clearSelections: () => void;
  runners: GhRunner[];
  runnersTotal: number;
  runnersNeedsAuth: boolean;
  localTools: GhToolReadiness[];
  runnersLoading: boolean;
  runnersError: string | null;
  loadRunners: () => Promise<void>;
  jobLogs: Record<number, GhActionLog>;
  logsLoading: Record<number, boolean>;
  showJobLog: (job: GhActionJob) => Promise<void>;
  fixBusy: number | null;
  fixCreated: number | null;
  fixFailed: boolean;
  /** Create the prefilled fix task; the caller builds the strings. */
  createFixTask: (
    runId: number,
    task: { title: string; description: string; steps: string[] }
  ) => Promise<void>;
}

function toMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    typeof (error as CommandError).message === "string"
  ) {
    return (error as CommandError).message;
  }
  return "Issues & PRs are unavailable.";
}

export function useGh(): GhStore {
  const [loading, setLoading] = useState(false);
  const [owner, setOwner] = useState<string | null>(null);
  const [repo, setRepo] = useState<string | null>(null);
  const [authenticated, setAuthenticated] = useState(true);
  const [rateLimited, setRateLimited] = useState(false);
  const [rate, setRate] = useState<GhRateLimit | null>(null);
  const [issues, setIssues] = useState<GhIssue[]>([]);
  const [pulls, setPulls] = useState<GhPull[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [runs, setRuns] = useState<GhActionRun[]>([]);
  const [totalRuns, setTotalRuns] = useState(0);
  const [selectedRun, setSelectedRun] = useState<number | null>(null);
  const [fixBusy, setFixBusy] = useState<number | null>(null);
  const [fixCreated, setFixCreated] = useState<number | null>(null);
  const [fixFailed, setFixFailed] = useState(false);
  const [jobLogs, setJobLogs] = useState<Record<number, GhActionLog>>({});
  const [logsLoading, setLogsLoading] = useState<Record<number, boolean>>({});
  const [runners, setRunners] = useState<GhRunner[]>([]);
  const [runnersTotal, setRunnersTotal] = useState(0);
  const [runnersNeedsAuth, setRunnersNeedsAuth] = useState(false);
  const [localTools, setLocalTools] = useState<GhToolReadiness[]>([]);
  const [runnersLoading, setRunnersLoading] = useState(false);
  const [actionsError, setActionsError] = useState<string | null>(null);
  const [runnersError, setRunnersError] = useState<string | null>(null);

  // Synchronous in-flight guard shared by the list and actions loaders:
  // the render-state guard the panel used is replaced by a ref so the
  // guard is exact at call time and the loaders' identities stay stable
  // (the tab effect must refire on kind/state changes only).
  const loadingRef = useRef(false);
  const runnersLoadingRef = useRef(false);

  const loadIssues = useCallback(async (state: GhState) => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    setError(null);
    try {
      const result = await ghIssues(state);
      setIssues(result.items);
      setOwner(result.owner);
      setRepo(result.repo);
      setAuthenticated(result.authenticated);
      setRateLimited(result.rate_limited);
      setRate(result.rate_limit);
      setSelected(null);
    } catch (err) {
      setError(toMessage(err));
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, []);

  const loadPulls = useCallback(async (state: GhState) => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    setError(null);
    try {
      const result = await ghPulls(state);
      setPulls(result.items);
      setOwner(result.owner);
      setRepo(result.repo);
      setAuthenticated(result.authenticated);
      setRateLimited(result.rate_limited);
      setRate(result.rate_limit);
      setSelected(null);
    } catch (err) {
      setError(toMessage(err));
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, []);

  const loadActions = useCallback(async (): Promise<boolean> => {
    if (loadingRef.current) return false;
    loadingRef.current = true;
    setLoading(true);
    setActionsError(null);
    try {
      const result = await ghActions();
      setRuns(result.runs);
      setTotalRuns(result.total_runs);
      setOwner(result.owner);
      setRepo(result.repo);
      setAuthenticated(result.authenticated);
      setRateLimited(result.rate_limited);
      setRate(result.rate_limit);
      setSelectedRun(null);
      setFixCreated(null);
      setFixFailed(false);
      setJobLogs({});
      setLogsLoading({});
      return true;
    } catch (err) {
      setActionsError(toMessage(err));
      return false;
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, []);

  /** Load the runners detect batch (repo self-hosted runners + the local
   * `PATH` probe) alongside the Actions tab. A failed fetch keeps the
   * previous runners state and surfaces the failure as an inline notice —
   * the section degrades to its last render plus the error instead of
   * blanking the checklist or failing silently. */
  const loadRunners = useCallback(async () => {
    if (runnersLoadingRef.current) return;
    runnersLoadingRef.current = true;
    setRunnersLoading(true);
    setRunnersError(null);
    try {
      const result = await ghRunners();
      setRunners(result.runners);
      setRunnersTotal(result.runners_total);
      setRunnersNeedsAuth(result.runners_needs_auth);
      setLocalTools(result.local);
    } catch (err) {
      setRunnersError(toMessage(err));
    } finally {
      runnersLoadingRef.current = false;
      setRunnersLoading(false);
    }
  }, []);

  /** Fetch one job's log tail on job-expand (lazy `gh_action_log`). A
   * failed fetch degrades to the unavailable state — the panel points at
   * GitHub instead. */
  const showJobLog = useCallback(
    async (job: GhActionJob) => {
      if (jobLogs[job.id] !== undefined || logsLoading[job.id] === true) return;
      setLogsLoading((prev) => ({ ...prev, [job.id]: true }));
      try {
        const log = await ghActionLog(job.run_id, job.id);
        setJobLogs((prev) => ({ ...prev, [job.id]: log }));
      } catch {
        setJobLogs((prev) => ({
          ...prev,
          [job.id]: {
            job_id: job.id,
            run_id: job.run_id,
            log_excerpt: "",
            log_truncated: false,
            log_unavailable: true,
            log_needs_auth: false,
            log_redacted: false,
          },
        }));
      } finally {
        setLogsLoading((prev) => {
          const next = { ...prev };
          delete next[job.id];
          return next;
        });
      }
    },
    [jobLogs, logsLoading]
  );

  /** Create the prefilled fix task (the fix loop entry). Creating the task
   * starts nothing — the user reviews and runs it from Tasks with approvals
   * and budgets intact. */
  const createFixTask = useCallback(
    async (
      runId: number,
      task: { title: string; description: string; steps: string[] }
    ) => {
      if (fixBusy !== null) return;
      setFixBusy(runId);
      setFixFailed(false);
      setFixCreated(null);
      try {
        const id = await createTask(
          task.title,
          task.description,
          null,
          null,
          null,
          task.steps,
          task.steps.length
        );
        setFixCreated(id);
      } catch {
        setFixFailed(true);
      } finally {
        setFixBusy(null);
      }
    },
    [fixBusy]
  );

  const selectItem = useCallback((number_: number) => {
    setSelected((current) => (current === number_ ? null : number_));
  }, []);

  const clearItemSelection = useCallback(() => setSelected(null), []);

  const selectRun = useCallback((id: number) => {
    setSelectedRun((current) => (current === id ? null : id));
  }, []);

  const clearSelections = useCallback(() => {
    setSelected(null);
    setSelectedRun(null);
    setFixCreated(null);
    setFixFailed(false);
  }, []);

  return {
    loading,
    owner,
    repo,
    authenticated,
    rateLimited,
    rate,
    issues,
    pulls,
    error,
    selected,
    loadIssues,
    loadPulls,
    selectItem,
    clearItemSelection,
    runs,
    totalRuns,
    actionsError,
    selectedRun,
    loadActions,
    selectRun,
    clearSelections,
    runners,
    runnersTotal,
    runnersNeedsAuth,
    localTools,
    runnersLoading,
    runnersError,
    loadRunners,
    jobLogs,
    logsLoading,
    showJobLog,
    fixBusy,
    fixCreated,
    fixFailed,
    createFixTask,
  };
}
