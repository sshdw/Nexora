//! GitHub Issues & PRs + Actions panel: read-only lists for the workspace origin repo.
//!
//! Presentational over the `gh_issues` / `gh_pulls` / `gh_actions` /
//! `gh_runners` IPC wrappers:
//! one capped page (at most 50 items) per issues/PRs kind tab + state filter
//! (`open` / `closed` / `all`), plus the Actions tab (recent workflow runs
//! with failing runs expanded to failed jobs; each job's capped,
//! secret-scrubbed log tail loads lazily on job-expand; the Runners section
//! below lists the repo's self-hosted runners plus this machine's readiness
//! checklist). Loads on mount and on every kind/state change (reads are cheap
//! GETs), plus manual Refresh — there is no watching or live polling.
//! Read-only end to end: no commenting, labeling, merging, re-running,
//! runner registration, or
//! any other write exists anywhere on this path.
//!
//! Fix loop: a failing run offers "Create fix task", which prefills a
//! task-manager task through the existing `createTask` wrapper (title,
//! failure context, four review steps). Creating the task starts nothing —
//! the user reviews and runs it from Tasks with approvals and budgets
//! intact; there is no auto-fix path.
//!
//! Display rules (secret-free): states, labels, conclusions, and counts render as
//! fixed-vocabulary catalog tags (unknown tokens echo defensively); rows
//! show `#number · author` plus the capped backend body (at most 8000 chars
//! with a truncation notice); fetched log tails render capped with truncation and
//! redaction notices (an expired or missing log points at GitHub instead;
//! an auth refusal shows the sign-in hint).
//! No-token responses keep their lists and add
//! the connect-hint banner (never an error dump); quota exhaustion renders
//! the rate-limit state with the remaining/limit snapshot (never a silent
//! empty); a non-github.com origin renders the dedicated no-remote state.
//! The panel never animates on entry (instant render under reduced motion);
//! all visuals ride the shared panel/tag/notice primitives — zero new CSS,
//! zero raw values.

import { useCallback, useEffect, useState, type ReactNode } from "react";

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
} from "../lib/tauri";
import { useStrings, type Strings } from "../lib/useLocale";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";
import M3SegmentedGroup from "./M3SegmentedGroup";

export interface IssuesPanelProps {
  onClose: () => void;
}

type Kind = "issues" | "pulls" | "actions";

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

/** Backend state vocabulary echoed defensively (the API only ever sends
 * `open`/`closed`; anything else renders the catalog unknown tag). */
function stateTag(state: string, t: Strings["t"]): string {
  if (state === "open") return t("gh.tagOpen");
  if (state === "closed") return t("gh.tagClosed");
  return t("gh.tagUnknown");
}

/** Backend conclusion vocabulary echoed defensively (allowlisted
 * backend-side; anything else renders the catalog unknown tag). */
function conclusionTag(conclusion: string, t: Strings["t"]): string {
  if (conclusion === "failure" || conclusion === "timed_out" || conclusion === "action_required") {
    return t("gh.conclusionFailed");
  }
  if (conclusion === "success") return t("gh.conclusionSuccess");
  if (conclusion === "cancelled") return t("gh.conclusionCancelled");
  if (conclusion === "neutral" || conclusion === "skipped" || conclusion === "stale") {
    return t("gh.conclusionSkipped");
  }
  return t("gh.tagUnknown");
}

/** Backend run-status vocabulary echoed defensively. The queued family
 * (`queued`/`requested`/`waiting`/`pending` — all "not started yet") shares
 * one tag; anything else renders the catalog unknown tag. */
function runStatusTag(status: string, t: Strings["t"]): string {
  if (status === "completed") return t("gh.runCompleted");
  if (status === "in_progress") return t("gh.runInProgress");
  if (
    status === "queued" ||
    status === "requested" ||
    status === "waiting" ||
    status === "pending"
  ) {
    return t("gh.runQueued");
  }
  return t("gh.tagUnknown");
}

/** A run the fix loop can target: completed with a failed conclusion
 * (mirrors the backend's expansion rule). */
function runFailed(run: GhActionRun): boolean {
  if (run.status !== "completed") return false;
  return (
    run.conclusion === "failure" ||
    run.conclusion === "timed_out" ||
    run.conclusion === "cancelled" ||
    run.conclusion === "action_required"
  );
}

/** A run the fix-task button is offered for: completed with a genuine
 * failure conclusion. Cancelled runs still expand for visibility (they count
 * in `runFailed`) but offer no fix task — a deliberate cancel is not
 * breakage. */
function runFixable(run: GhActionRun): boolean {
  if (run.status !== "completed") return false;
  return (
    run.conclusion === "failure" ||
    run.conclusion === "timed_out" ||
    run.conclusion === "action_required"
  );
}

/** Short calendar date from an ISO timestamp (`YYYY-MM-DD`); the raw text
 * is echoed only when the timestamp does not parse. */
function shortDate(iso: string | null): string | null {
  if (iso === null || iso === "") return null;
  const time = Date.parse(iso);
  if (Number.isNaN(time)) return iso;
  return new Date(time).toISOString().slice(0, 10);
}

function RateNote({ rate }: { rate: GhRateLimit | null }): ReactNode {
  const { t } = useStrings();
  if (rate === null) return null;
  if (rate.limit == null || rate.remaining == null) {
    return (
      <p className="nex-vcs-notice" role="note">
        {t("gh.rateUnknown")}
      </p>
    );
  }
  return (
    <p className="nex-vcs-notice" role="note">
      {t("gh.rateNote", { remaining: rate.remaining, limit: rate.limit })}
    </p>
  );
}

interface ActionsViewProps {
  runs: GhActionRun[];
  totalRuns: number;
  selectedRun: GhActionRun | null;
  selectedRunId: number | null;
  onSelectRun: (id: number) => void;
  loading: boolean;
  fixBusy: number | null;
  fixCreated: number | null;
  fixFailed: boolean;
  onCreateFixTask: (run: GhActionRun) => void;
  jobLogs: Record<number, GhActionLog>;
  logsLoading: Record<number, boolean>;
  onShowLog: (job: GhActionJob) => void;
}

interface JobLogProps {
  job: GhActionJob;
  log: GhActionLog | null;
  loading: boolean;
  onShowLog: (job: GhActionJob) => void;
}

/** One failed job's lazy log: a Show-log button until fetched on
 * job-expand, then the capped tail — or the sign-in hint (auth refusal) and
 * the expired/missing text as two distinct states. */
function JobLog({ job, log, loading, onShowLog }: JobLogProps): ReactNode {
  const { t } = useStrings();
  if (log === null) {
    if (loading) {
      return (
        <p className="nex-vcs-notice" role="note">
          {t("gh.logLoading")}
        </p>
      );
    }
    return (
      <M3Button variant="quiet" size="sm" onClick={() => onShowLog(job)}>
        {t("gh.showLog")}
      </M3Button>
    );
  }
  if (log.log_needs_auth) {
    return (
      <p className="nex-vcs-notice" role="note">
        {t("gh.logNeedsAuth")}
      </p>
    );
  }
  if (log.log_unavailable || log.log_excerpt === "") {
    return (
      <p className="nex-vcs-notice" role="note">
        {t("gh.logUnavailable")}
      </p>
    );
  }
  return (
    <>
      <p className="nex-vcs-notice">
        {t("gh.logTitle")}
        {log.log_truncated && ` · ${t("gh.logTruncated")}`}
        {log.log_redacted && ` · ${t("gh.logRedacted")}`}
      </p>
      <pre className="nex-agent-terminal-stdout">{log.log_excerpt}</pre>
    </>
  );
}

/** Actions tab body: failing/passing runs list + run detail (failed jobs
 * with log tails) + the fix-task entry. All visuals ride the shared
 * panel/tag/notice primitives — zero new CSS, zero raw values. */
function ActionsView({
  runs,
  totalRuns,
  selectedRun,
  selectedRunId,
  onSelectRun,
  loading,
  fixBusy,
  fixCreated,
  fixFailed,
  onCreateFixTask,
  jobLogs,
  logsLoading,
  onShowLog,
}: ActionsViewProps): ReactNode {
  const { t } = useStrings();
  if (loading && runs.length === 0) {
    return <M3LoadingIndicator label={t("gh.loading")} />;
  }
  if (runs.length === 0) {
    return <p className="nex-agent-empty">{t("gh.emptyActions")}</p>;
  }
  const anyFailed = runs.some(runFailed);
  return (
    <>
      <p className="nex-vcs-notice" role="note">
        {t("gh.runsCount", { n: runs.length, total: totalRuns })}
      </p>
      {!anyFailed && (
        <p className="nex-vcs-notice" role="note">
          {t("gh.noFailedRuns")}
        </p>
      )}
      <ul className="nex-vcs-file-list" aria-label={t("gh.listAria")}>
        {runs.map((run) => (
          <li key={run.id} className="nex-vcs-file-row">
            <M3Button
              variant="quiet"
              onClick={() => onSelectRun(run.id)}
              aria-expanded={selectedRunId === run.id}
            >
              {run.name} #{run.id}
            </M3Button>{" "}
            <span className="nex-tag nex-tag-mono">
              {conclusionTag(run.conclusion, t)}
            </span>{" "}
            <span className="nex-tag nex-tag-mono">
              {runStatusTag(run.status, t)}
            </span>
          </li>
        ))}
      </ul>
      {selectedRun === null ? (
        <p className="nex-vcs-notice" role="note">
          {t("gh.selectHint")}
        </p>
      ) : (
        <section className="nex-vcs-section" aria-label={t("gh.detailAria")}>
          <h3 className="nex-vcs-section-title">
            {selectedRun.name} #{selectedRun.id}
          </h3>
          <p className="nex-vcs-notice">
            <span className="nex-tag nex-tag-mono">
              {conclusionTag(selectedRun.conclusion, t)}
            </span>{" "}
            <span className="nex-tag nex-tag-mono">
              {runStatusTag(selectedRun.status, t)}
            </span>{" "}
            {t("gh.runByLine", {
              id: selectedRun.id,
              branch: selectedRun.head_branch ?? t("gh.noBranch"),
            })}
            {shortDate(selectedRun.created_at) !== null &&
              ` · ${shortDate(selectedRun.created_at)}`}
            {selectedRun.head_sha !== null && selectedRun.head_sha !== "" && (
              <> · {selectedRun.head_sha.slice(0, 7)}</>
            )}
          </p>
          {selectedRun.html_url !== null && selectedRun.html_url !== "" && (
            <p className="nex-vcs-notice">
              {t("gh.githubRun")} {selectedRun.html_url}
            </p>
          )}
          <p className="nex-vcs-notice" role="note">
            {t("gh.rerunNote")}
          </p>
          {selectedRun.failed_jobs.length === 0 ? (
            <p className="nex-vcs-notice" role="note">
              {t("gh.noFailedJobs")}
            </p>
          ) : (
            <>
              <p className="nex-vcs-notice">
                {t("gh.jobsTitle", { n: selectedRun.failed_jobs.length })}
              </p>
              {selectedRun.failed_jobs.map((job) => (
                <article
                  key={job.id}
                  className="nex-agent-terminal"
                  aria-label={`${job.name} — ${conclusionTag(job.conclusion, t)}`}
                >
                  <div className="nex-agent-terminal-header">
                    <span className="nex-agent-terminal-command">{job.name}</span>
                    <span className="nex-tag nex-tag-mono">
                      {conclusionTag(job.conclusion, t)}
                    </span>
                  </div>
                  <div className="nex-agent-terminal-body">
                    {job.failed_steps.length > 0 && (
                      <p className="nex-vcs-notice">
                        {job.failed_steps.map((step) => (
                          <span key={step} className="nex-tag nex-tag-mono" title={step}>
                            {step}
                          </span>
                        ))}
                      </p>
                    )}
                    <JobLog
                      job={job}
                      log={jobLogs[job.id] ?? null}
                      loading={logsLoading[job.id] === true}
                      onShowLog={onShowLog}
                    />
                  </div>
                </article>
              ))}
              {selectedRun.jobs_truncated && (
                <p className="nex-vcs-notice" role="note">
                  {t("gh.jobsTruncated")}
                </p>
              )}
              {runFixable(selectedRun) && (
                <div className="nex-vcs-header-actions">
                  <M3Button
                    variant="primary"
                    size="sm"
                    disabled={fixBusy !== null}
                    onClick={() => onCreateFixTask(selectedRun)}
                  >
                    {fixBusy === selectedRun.id ? t("gh.creatingFixTask") : t("gh.createFixTask")}
                  </M3Button>
                </div>
              )}
              {fixCreated !== null && (
                <p className="nex-vcs-notice" role="note">
                  {t("gh.fixTaskCreated", { id: fixCreated })}
                </p>
              )}
              {fixFailed && (
                <div className="nex-composer-error nex-fade-in" role="alert">
                  {t("gh.fixTaskFailed")}
                </div>
              )}
            </>
          )}
        </section>
      )}
    </>
  );
}

/** Backend runner-status vocabulary echoed defensively (allowlisted
 * backend-side; anything else renders the catalog unknown tag). */
function runnerStatusTag(status: string, t: Strings["t"]): string {
  if (status === "online") return t("gh.runnerOnline");
  if (status === "offline") return t("gh.runnerOffline");
  return t("gh.tagUnknown");
}

interface RunnersViewProps {
  runners: GhRunner[];
  total: number;
  needsAuth: boolean;
  local: GhToolReadiness[];
  loading: boolean;
  error: string | null;
}

/** Runners section: the repo's self-hosted runners table plus this
 * machine's readiness checklist. An auth refusal (anonymous or
 * under-scoped token — listing needs admin scope) renders the honest
 * needs-token state; the local checklist always renders real `PATH`
 * results. All visuals ride the shared panel/tag/notice primitives — zero
 * new CSS, zero raw values. */
function RunnersView({ runners, total, needsAuth, local, loading, error }: RunnersViewProps): ReactNode {
  const { t } = useStrings();
  return (
    <section className="nex-vcs-section" aria-label={t("gh.runnersTitle")}>
      <h3 className="nex-vcs-section-title">{t("gh.runnersTitle")}</h3>
      <p className="nex-vcs-notice" role="note">
        {t("gh.runnersHint")}
      </p>
      {error !== null && (
        <div className="nex-composer-error nex-fade-in" role="alert">
          {error}
        </div>
      )}
      {loading && runners.length === 0 ? (
        <M3LoadingIndicator label={t("gh.runnersLoading")} />
      ) : needsAuth ? (
        <p className="nex-vcs-notice" role="note">
          {t("gh.runnersNeedToken")}
        </p>
      ) : runners.length === 0 ? (
        <p className="nex-agent-empty">{t("gh.runnersEmpty")}</p>
      ) : (
        <>
          <p className="nex-vcs-notice" role="note">
            {t("gh.runnersCount", { n: runners.length, total })}
          </p>
          <ul className="nex-vcs-file-list" aria-label={t("gh.runnersTitle")}>
            {runners.map((runner) => (
              <li key={runner.id} className="nex-vcs-file-row">
                <span className="nex-tag nex-tag-mono">{runner.name}</span>{" "}
                <span className="nex-tag nex-tag-mono">{runner.os}</span>{" "}
                <span className="nex-tag nex-tag-mono">
                  {runnerStatusTag(runner.status, t)}
                </span>{" "}
                <span className="nex-tag nex-tag-mono">
                  {runner.busy ? t("gh.runnerBusy") : t("gh.runnerIdle")}
                </span>
                {runner.labels.length > 0 && (
                  <>
                    {" "}
                    {runner.labels.map((label) => (
                      <span key={label} className="nex-tag nex-tag-mono" title={label}>
                        {label}
                      </span>
                    ))}
                  </>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
      <h3 className="nex-vcs-section-title">{t("gh.localTitle")}</h3>
      <p className="nex-vcs-notice" role="note">
        {t("gh.localHint")}
      </p>
      <ul className="nex-vcs-file-list" aria-label={t("gh.localTitle")}>
        {local.map((tool) => (
          <li key={tool.name} className="nex-vcs-file-row">
            <span className="nex-tag nex-tag-mono">{tool.name}</span>{" "}
            <span className="nex-tag nex-tag-mono">
              {tool.found ? t("gh.toolFound") : t("gh.toolMissing")}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export default function IssuesPanel({ onClose }: IssuesPanelProps) {
  const { t } = useStrings();
  const [kind, setKind] = useState<Kind>("issues");
  const [state, setState] = useState<GhState>("open");
  const [issues, setIssues] = useState<GhIssue[]>([]);
  const [pulls, setPulls] = useState<GhPull[]>([]);
  const [owner, setOwner] = useState<string | null>(null);
  const [repo, setRepo] = useState<string | null>(null);
  const [authenticated, setAuthenticated] = useState(true);
  const [rateLimited, setRateLimited] = useState(false);
  const [rate, setRate] = useState<GhRateLimit | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
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

  const load = useCallback(async () => {
    if (loading) return;
    setLoading(true);
    setError(null);
    try {
      if (kind === "issues") {
        const result = await ghIssues(state);
        setIssues(result.items);
        setOwner(result.owner);
        setRepo(result.repo);
        setAuthenticated(result.authenticated);
        setRateLimited(result.rate_limited);
        setRate(result.rate_limit);
      } else {
        const result = await ghPulls(state);
        setPulls(result.items);
        setOwner(result.owner);
        setRepo(result.repo);
        setAuthenticated(result.authenticated);
        setRateLimited(result.rate_limited);
        setRate(result.rate_limit);
      }
      setSelected(null);
    } catch (err) {
      setError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, [kind, state]);

  const loadActions = useCallback(async () => {
    if (loading) return;
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
    } catch (err) {
      setActionsError(toMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  /** Load the runners detect batch (repo self-hosted runners + the local
   * `PATH` probe) alongside the Actions tab. A failed fetch keeps the
   * previous runners state and surfaces the failure as an inline notice —
   * the section degrades to its last render plus the error instead of
   * blanking the checklist or failing silently. */
  const loadRunners = useCallback(async () => {
    if (runnersLoading) return;
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
      setRunnersLoading(false);
    }
    // Fixed deps (mirrors `loadActions` above): the guard reads the render
    // snapshot, so the identity stays stable and the tab effect below never
    // refires on loading flips.
  }, []);

  useEffect(() => {
    if (kind === "actions") {
      void loadActions();
      void loadRunners();
    } else {
      void load();
    }
  }, [kind, load, loadActions, loadRunners]);

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
    [jobLogs, logsLoading],
  );

  /** Prefill a task-manager task from a failing run (the fix loop entry).
   * Creating the task starts nothing — the user reviews and runs it from
   * Tasks with approvals and budgets intact. */
  const createFixTask = useCallback(
    async (run: GhActionRun) => {
      if (fixBusy !== null) return;
      setFixBusy(run.id);
      setFixFailed(false);
      setFixCreated(null);
      try {
        const branch = run.head_branch ?? t("gh.noBranch");
        const title = Array.from(`Fix CI: ${run.name} #${run.id}`).slice(0, 200).join("");
        const description = [
          `Failing GitHub Actions run ${owner ?? "?"}/${repo ?? "?"} — ${run.name} #${run.id}.`,
          `Branch: ${branch} · Event: ${run.event} · Conclusion: ${run.conclusion}.`,
          ...(run.html_url !== null ? [`Run: ${run.html_url}`] : []),
          ...(run.failed_jobs.length > 0
            ? [`Failed jobs: ${run.failed_jobs.map((job) => job.name).join(", ")}.`]
            : []),
          "Log excerpts are capped tails with possible secrets redacted — read the full log on GitHub.",
          "Re-run the workflow on GitHub after fixing (Nexora never re-runs workflows).",
        ].join("\n");
        const steps = [
          "Open the failing run on GitHub and read the full log",
          "Reproduce the failure locally",
          "Fix the cause and verify with the workspace checks",
          "Push and re-run the workflow on GitHub",
        ];
        const id = await createTask(title, description, null, null, null, steps, steps.length);
        setFixCreated(id);
      } catch {
        setFixFailed(true);
      } finally {
        setFixBusy(null);
      }
    },
    [fixBusy, owner, repo, t],
  );

  const noRemote = error !== null && error.includes("github.com");
  const actionsNoRemote =
    actionsError !== null && actionsError.includes("github.com");
  const items: Array<{ number: number }> =
    kind === "issues" ? issues : pulls;
  const selectedIssue =
    kind === "issues"
      ? issues.find((item) => item.number === selected) ?? null
      : null;
  const selectedPull =
    kind === "pulls"
      ? pulls.find((item) => item.number === selected) ?? null
      : null;
  const detail: GhIssue | GhPull | null = selectedIssue ?? selectedPull;
  const selectedActionRun =
    kind === "actions" ? runs.find((run) => run.id === selectedRun) ?? null : null;
  const subtitle =
    owner !== null && repo !== null
      ? `${t("gh.subtitle")} ${t("gh.repo", { owner, repo })}`
      : t("gh.subtitle");

  return (
    <div className="nex-vcs" role="group" aria-label={t("gh.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("gh.title")}</h2>
          <p className="nex-vcs-subtitle">{subtitle}</p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button
            variant="quiet"
            onClick={() => {
              if (kind === "actions") {
                void loadActions();
                void loadRunners();
              } else {
                void load();
              }
            }}
            disabled={loading}
          >
            {loading ? t("gh.refreshing") : t("gh.refresh")}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </M3Button>
        </div>
      </header>

      <div className="nex-vcs-body">
        <M3SegmentedGroup<Kind>
          label={t("gh.kindLabel")}
          semantics="tabs"
          options={[
            { value: "issues", label: t("gh.kindIssues") },
            { value: "pulls", label: t("gh.kindPulls") },
            { value: "actions", label: t("gh.kindActions") },
          ]}
          value={kind}
          onChange={(next) => {
            setKind(next);
            setSelected(null);
            setSelectedRun(null);
            setFixCreated(null);
            setFixFailed(false);
          }}
        />
        {kind !== "actions" ? (
          <M3SegmentedGroup<GhState>
            label={t("gh.stateLabel")}
            options={[
              { value: "open", label: t("gh.stateOpen") },
              { value: "closed", label: t("gh.stateClosed") },
              { value: "all", label: t("gh.stateAll") },
            ]}
            value={state}
            onChange={(next) => {
              setState(next);
              setSelected(null);
            }}
          />
        ) : (
          <p className="nex-vcs-notice" role="note">
            {t("gh.actionsHint")}
          </p>
        )}

        {!authenticated && error === null && (
          <p className="nex-vcs-notice" role="note">
            {t("gh.noToken")}
          </p>
        )}
        <RateNote rate={rate} />

        {loading && items.length === 0 && (
          <M3LoadingIndicator label={t("gh.loading")} />
        )}
        {kind === "actions" ? (
          actionsNoRemote ? (
            <p className="nex-agent-empty" role="note">
              {t("gh.noRemote")}
            </p>
          ) : (
            <>
              {actionsError !== null && (
                <div className="nex-composer-error nex-fade-in" role="alert">
                  {actionsError}
                </div>
              )}
              {rateLimited ? (
                <div className="nex-composer-error nex-fade-in" role="alert">
                  {t("gh.rateLimited")}
                </div>
              ) : (
                <ActionsView
                  runs={runs}
                  totalRuns={totalRuns}
                  selectedRun={selectedActionRun}
                  selectedRunId={selectedRun}
                  onSelectRun={(id) =>
                    setSelectedRun((current) => (current === id ? null : id))
                  }
                  loading={loading}
                  fixBusy={fixBusy}
                  fixCreated={fixCreated}
                  fixFailed={fixFailed}
                  onCreateFixTask={(run) => void createFixTask(run)}
                  jobLogs={jobLogs}
                  logsLoading={logsLoading}
                  onShowLog={(job) => void showJobLog(job)}
                />
              )}
              <RunnersView
                runners={runners}
                total={runnersTotal}
                needsAuth={runnersNeedsAuth}
                local={localTools}
                loading={runnersLoading}
                error={runnersError}
              />
            </>
          )
        ) : noRemote ? (
          <p className="nex-agent-empty" role="note">
            {t("gh.noRemote")}
          </p>
        ) : error !== null ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error}
          </div>
        ) : rateLimited ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {t("gh.rateLimited")}
          </div>
        ) : items.length === 0 && !loading ? (
          <p className="nex-agent-empty">
            {kind === "issues" ? t("gh.emptyIssues") : t("gh.emptyPulls")}
          </p>
        ) : (
          <>
            <ul className="nex-vcs-file-list" aria-label={t("gh.listAria")}>
              {(kind === "issues" ? issues : pulls).map((item) => (
                <li key={item.number} className="nex-vcs-file-row">
                  <M3Button
                    variant="quiet"
                    onClick={() =>
                      setSelected((current) =>
                        current === item.number ? null : item.number,
                      )
                    }
                    aria-expanded={selected === item.number}
                  >
                    #{item.number} · {item.title}
                  </M3Button>{" "}
                  <span className="nex-tag nex-tag-mono">
                    {stateTag(item.state, t)}
                  </span>{" "}
                  <span className="nex-tag nex-tag-mono">
                    {t("gh.byLine", { n: item.number, author: item.author })}
                  </span>
                </li>
              ))}
            </ul>
            {detail === null ? (
              <p className="nex-vcs-notice" role="note">
                {t("gh.selectHint")}
              </p>
            ) : (
              <section
                className="nex-vcs-section"
                aria-label={t("gh.detailAria")}
              >
                <h3 className="nex-vcs-section-title">
                  #{detail.number} · {detail.title}
                </h3>
                <p className="nex-vcs-notice">
                  <span className="nex-tag nex-tag-mono">
                    {stateTag(detail.state, t)}
                  </span>{" "}
                  {kind === "pulls" && (detail as GhPull).draft && (
                    <>
                      <span className="nex-tag nex-tag-mono">
                        {t("gh.tagDraft")}
                      </span>{" "}
                    </>
                  )}
                  {t("gh.byLine", { n: detail.number, author: detail.author })}
                  {shortDate(detail.created_at) !== null &&
                    ` · ${shortDate(detail.created_at)}`}
                  {" · "}
                  {t("gh.comments", { n: detail.comments })}
                </p>
                {kind === "pulls" &&
                  (detail as GhPull).head_ref !== null &&
                  (detail as GhPull).base_ref !== null && (
                    <p className="nex-vcs-notice">
                      <span className="nex-tag nex-tag-mono">
                        {t("gh.refsLine", {
                          head: (detail as GhPull).head_ref ?? "",
                          base: (detail as GhPull).base_ref ?? "",
                        })}
                      </span>
                    </p>
                  )}
                {kind === "issues" &&
                  (detail as GhIssue).labels.length > 0 && (
                    <p className="nex-vcs-notice">
                      {t("gh.labelsTitle")}:{" "}
                      {(detail as GhIssue).labels.map((label) => (
                        <span
                          key={label}
                          className="nex-tag nex-tag-mono"
                          title={label}
                        >
                          {label}
                        </span>
                      ))}
                    </p>
                  )}
                {detail.body_truncated && (
                  <p className="nex-vcs-notice" role="note">
                    {t("gh.bodyTruncated")}
                  </p>
                )}
                <pre className="nex-agent-terminal-stdout">
                  {detail.body === "" ? t("gh.noBody") : detail.body}
                </pre>
              </section>
            )}
          </>
        )}
      </div>
    </div>
  );
}
