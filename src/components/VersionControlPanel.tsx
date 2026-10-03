//! Version-control panel: workspace git status, commit timeline with
//! per-commit stats and risk signals, per-file and per-commit diffs, AI
//! commit explanation, plus guarded staging, committing (with an
//! AI-generated conventional-commit message), and pushing to `origin`.
//!
//! Presentational over the `git_info` / `git_file_diff` / `git_commit_diff`
//! IPC wrappers plus the write wrappers (`git_stage` / `git_unstage` /
//! `git_commit` / `git_push` / `git_generate_commit_message` /
//! `git_explain_commit`): `git_info` batches branch + changed files +
//! commits (with stats and signals) in one round trip, file and commit diffs
//! load lazily when selected (server-side capped with a truncation notice).
//! Every write passes the backend's explicit per-call confirmation and
//! refreshes the panel afterwards. Diff rendering reuses the existing
//! `DiffView` classifier from `AgentRunSteps` — no new diff engine — inside
//! a keyboard-focusable scroll region. There is no file watching or live
//! refresh: the panel reloads on mount and on explicit Refresh only.
//!
//! Wiring decision (no new rail entry): the timeline lives as a tab inside
//! this VCS screen — "Working tree" (status, file diffs, commit composer,
//! push) and "Timeline" (history nodes with stats + risk badges, commit
//! detail with diff + Explain). One destination, two views.
//!
//! Commit explanation mirrors the terminal panel's error-intelligence UX:
//! the Explain press sends the capped commit diff through the existing AI
//! execution path and renders a copy-only explanation — never auto-applied —
//! with the disclosure notice, the truncation notice, a synchronous
//! in-flight guard against double-spend, and an empty-input guard (commits
//! with no changed files cannot be explained).
//!
//! Display rules: hashes render shortened (7 chars), times render relative,
//! statuses and risk signals render as fixed-vocabulary labels, overflows
//! report counts only ("+N more") — no raw backend values. The panel never
//! animates on entry (instant render under reduced motion).

import { useCallback, useEffect, useRef, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import {
  getSetting,
  gitCommit,
  gitCommitDiff,
  gitExplainCommit,
  gitFileDiff,
  gitGenerateCommitMessage,
  gitInfo,
  gitPush,
  gitStage,
  gitUnstage,
  type CommandError,
  type CommitExplanation,
  type GitCommit,
  type GitCommitDiff,
  type GitFileDiff,
  type GitInfo,
} from "../lib/tauri";
import { DiffView } from "./AgentRunSteps";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

const COMMIT_PAGE = 20;
const SHORT_HASH_LEN = 7;

type VcsTab = "working" | "timeline";

/** Explain state for the selected commit: the AI explanation (copy-only),
 * or the failure. `copied` confirms the Copy press without a timer. */
interface CommitExplainState {
  loading: boolean;
  explanation: string | null;
  truncatedInput: boolean;
  error: string | null;
  copied: boolean;
}

const IDLE_EXPLAIN: CommitExplainState = {
  loading: false,
  explanation: null,
  truncatedInput: false,
  error: null,
  copied: false,
};

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

/** Fixed-vocabulary file status label; unknown values fall back to a
 * capitalized echo of the backend token (the backend only sends its fixed
 * set, so this branch is defensive). */
function statusLabel(status: string): string {
  switch (status) {
    case "modified":
      return "Modified";
    case "staged":
      return "Staged";
    case "untracked":
      return "Untracked";
    case "deleted":
      return "Deleted";
    case "renamed":
      return "Renamed";
    default:
      return status.charAt(0).toUpperCase() + status.slice(1);
  }
}

/** Fixed-vocabulary risk-signal label; unknown values fall back to a
 * capitalized echo of the backend token (same defensive shape as
 * `statusLabel`). */
function riskLabel(signal: string): string {
  switch (signal) {
    case "large-diff":
      return "Large diff";
    case "many-files":
      return "Many files";
    case "binary":
      return "Binary";
    case "merge-commit":
      return "Merge";
    case "unfamiliar-author":
      return "Unfamiliar author";
    default:
      return signal.charAt(0).toUpperCase() + signal.slice(1);
  }
}

/** Compact stats line for one timeline node: counts only, never content. */
function statsLine(commit: GitCommit): string {
  const files = commit.files_changed === 1 ? "1 file" : `${commit.files_changed} files`;
  return `${files} · +${commit.insertions} −${commit.deletions}`;
}

function shortHash(hash: string): string {
  return hash.length > SHORT_HASH_LEN ? hash.slice(0, SHORT_HASH_LEN) : hash;
}

export interface VersionControlPanelProps {
  onClose: () => void;
  /** Palette-raised action (refresh / focus-commit). The token identifies
   * the request so a re-render never replays it; mount already loads, so
   * refresh is an idempotent re-read that also keeps the selection. */
  request?: { token: number; action: "refresh" | "focus-commit" } | null;
}

export default function VersionControlPanel({ onClose, request = null }: VersionControlPanelProps) {
  const [tab, setTab] = useState<VcsTab>("working");
  const [info, setInfo] = useState<GitInfo | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitFileDiff | null>(null);
  const [diffLoading, setDiffLoading] = useState<boolean>(false);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [acting, setActing] = useState<string | null>(null);
  const [commitMessage, setCommitMessage] = useState<string>("");
  const [generating, setGenerating] = useState<boolean>(false);
  const [generatedTruncated, setGeneratedTruncated] = useState<boolean>(false);
  const [pushArmed, setPushArmed] = useState<boolean>(false);
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const [commitDiff, setCommitDiff] = useState<GitCommitDiff | null>(null);
  const [commitDiffLoading, setCommitDiffLoading] = useState<boolean>(false);
  const [commitDiffError, setCommitDiffError] = useState<string | null>(null);
  const [explain, setExplain] = useState<CommitExplainState>(IDLE_EXPLAIN);
  const commitRef = useRef<HTMLTextAreaElement>(null);
  // Synchronous in-flight explain guard: `explain.loading` lands after
  // re-render, so two rapid Explain presses could both pass the state guard
  // and stack two paid AI requests. The ref flips in the same tick (same
  // shape as the terminal panel's per-block guard, single-flight here
  // because only the selected commit can be explained).
  const explainingRef = useRef<string | null>(null);

  const loadInfo = useCallback(async (keepSelection: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const data = await gitInfo(COMMIT_PAGE);
      setInfo(data);
      if (!keepSelection) {
        setSelectedPath(null);
        setDiff(null);
        setDiffError(null);
        setSelectedCommit(null);
        setCommitDiff(null);
        setCommitDiffError(null);
        setExplain(IDLE_EXPLAIN);
      }
    } catch (e) {
      setError(toMessage(e));
      setInfo(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadInfo(false);
  }, [loadInfo]);

  const loadDiff = useCallback(async (path: string) => {
    setSelectedPath(path);
    setDiffLoading(true);
    setDiffError(null);
    setDiff(null);
    try {
      setDiff(await gitFileDiff(path));
    } catch (e) {
      setDiffError(toMessage(e));
    } finally {
      setDiffLoading(false);
    }
  }, []);

  const loadCommitDiff = useCallback(async (hash: string) => {
    setSelectedCommit(hash);
    setCommitDiffLoading(true);
    setCommitDiffError(null);
    setCommitDiff(null);
    setExplain(IDLE_EXPLAIN);
    try {
      setCommitDiff(await gitCommitDiff(hash));
    } catch (e) {
      setCommitDiffError(toMessage(e));
    } finally {
      setCommitDiffLoading(false);
    }
  }, []);

  // Refresh keeps the selections (and reloads their diffs) so a manual
  // refresh never loses the reader's place.
  const handleRefresh = useCallback(() => {
    setPushArmed(false);
    void loadInfo(true).then(() => {
      if (selectedPath) void loadDiff(selectedPath);
      if (selectedCommit) void loadCommitDiff(selectedCommit);
    });
  }, [loadInfo, loadDiff, loadCommitDiff, selectedPath, selectedCommit]);

  // Palette requests (same handlers as the panel's own buttons — no
  // duplicated logic). Refresh reuses handleRefresh; commit-focus moves
  // focus to the commit composer once the panel has loaded.
  const refreshRef = useRef(handleRefresh);
  refreshRef.current = handleRefresh;
  const seenRequestRef = useRef<number | null>(null);
  useEffect(() => {
    if (!request || seenRequestRef.current === request.token) return;
    if (request.action === "refresh") {
      seenRequestRef.current = request.token;
      refreshRef.current();
      return;
    }
    // Focus-commit while the panel is still loading retries when the load
    // settles; a failed load (nothing to focus) still consumes the token
    // so it never fires unexpectedly on a later successful load.
    if (loading) return;
    seenRequestRef.current = request.token;
    if (!info) return;
    setTab("working");
    commitRef.current?.focus();
  }, [request, loading, info]);

  const handleToggleStage = useCallback(
    async (path: string, staged: boolean) => {
      setActing(path);
      setActionError(null);
      try {
        if (staged) {
          await gitUnstage([path]);
        } else {
          await gitStage([path]);
        }
        await loadInfo(true);
      } catch (e) {
        setActionError(toMessage(e));
      } finally {
        setActing(null);
      }
    },
    [loadInfo],
  );

  const handleCommit = useCallback(async () => {
    const message = commitMessage.trim();
    if (message === "") return;
    setActing("commit");
    setActionError(null);
    try {
      await gitCommit(message);
      setCommitMessage("");
      setGeneratedTruncated(false);
      await loadInfo(true);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [commitMessage, loadInfo]);

  const handleGenerate = useCallback(async () => {
    setGenerating(true);
    setActionError(null);
    try {
      const [provider, model] = await Promise.all([
        getSetting("provider.selected"),
        getSetting("provider.model"),
      ]);
      if (!provider || !model) {
        setActionError("Select a provider and model first (Settings), then generate.");
        return;
      }
      const generated = await gitGenerateCommitMessage(provider, model);
      setCommitMessage(generated.message);
      setGeneratedTruncated(generated.truncated_input);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setGenerating(false);
    }
  }, []);

  const handlePush = useCallback(async () => {
    if (!pushArmed) {
      setPushArmed(true);
      return;
    }
    setPushArmed(false);
    setActing("push");
    setActionError(null);
    try {
      await gitPush();
      await loadInfo(true);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [loadInfo, pushArmed]);

  // Commit explanation: explain the selected commit's capped diff through
  // the existing AI execution path (same persisted provider/model settings
  // as commit-message generation). The result is copy-only — this handler
  // never feeds it back into a write, so nothing auto-applies.
  const handleExplainCommit = useCallback(async (hash: string) => {
    if (explainingRef.current !== null) return;
    explainingRef.current = hash;
    setExplain((prev) => ({ ...prev, loading: true, error: null, copied: false }));
    try {
      const [provider, model] = await Promise.all([
        getSetting("provider.selected"),
        getSetting("provider.model"),
      ]);
      if (!provider || !model) {
        setExplain((prev) => ({
          ...prev,
          loading: false,
          error: "Select a provider and model first (Settings), then explain.",
        }));
        return;
      }
      const result: CommitExplanation = await gitExplainCommit(hash, provider, model);
      setExplain({
        loading: false,
        explanation: result.explanation,
        truncatedInput: result.truncated_input,
        error: null,
        copied: false,
      });
    } catch (e) {
      setExplain((prev) => ({ ...prev, loading: false, error: toMessage(e) }));
    } finally {
      explainingRef.current = null;
    }
  }, []);

  const handleCopyExplain = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setExplain((prev) => ({ ...prev, copied: true }));
    } catch {
      setExplain((prev) => ({
        ...prev,
        copied: false,
        error: "Copy failed — select the text manually.",
      }));
    }
  }, []);

  const stagedCount = info?.files.filter((file) => file.status === "staged").length ?? 0;
  const explainable = commitDiff !== null && commitDiff.files.length > 0;

  return (
    <div className="nex-vcs" role="group" aria-label="Version control">
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">Version Control</h2>
          <p className="nex-vcs-subtitle">
            {info?.branch ? `On branch ${info.branch}.` : "Git status for the open workspace."}{" "}
            Manual refresh only — writes ask first and never force-push.
          </p>
        </div>
        <div className="nex-vcs-header-actions">
          <M3Button variant="quiet" onClick={handleRefresh} disabled={loading}>
            {loading ? "Refreshing…" : "Refresh"}
          </M3Button>
          <M3Button variant="quiet" onClick={onClose}>
            Back to conversations
          </M3Button>
        </div>
      </header>

      <div
        className="nex-vcs-tabs"
        role="tablist"
        aria-label="Version control views"
      >
        <button
          type="button"
          role="tab"
          aria-selected={tab === "working"}
          className={"nex-vcs-tab" + (tab === "working" ? " is-selected" : "")}
          onClick={() => setTab("working")}
        >
          Working tree
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "timeline"}
          className={"nex-vcs-tab" + (tab === "timeline" ? " is-selected" : "")}
          onClick={() => setTab("timeline")}
        >
          Timeline
        </button>
      </div>

      <div className="nex-vcs-body">
        {loading ? (
          <M3LoadingIndicator label="Loading version control" />
        ) : error || !info ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error ?? "Version control is unavailable."}
          </div>
        ) : (
          <>
            {actionError && (
              <div className="nex-composer-error nex-fade-in" role="alert">
                {actionError}
              </div>
            )}
            {tab === "working" ? (
              <>
                <section className="nex-vcs-section" aria-label="Changed files">
                  <h3 className="nex-vcs-section-title">
                    Changed files: {info.files.length}
                  </h3>
                  {info.files.length === 0 ? (
                    <p className="nex-agent-empty">No changes — the working tree is clean.</p>
                  ) : (
                    <>
                      <ul className="nex-vcs-file-list">
                        {info.files.map((file) => {
                          const selected = file.path === selectedPath;
                          const staged = file.status === "staged";
                          const busy = acting === file.path;
                          return (
                            <li key={file.path} className="nex-vcs-file-row">
                              <button
                                type="button"
                                className={"nex-vcs-file" + (selected ? " is-selected" : "")}
                                aria-current={selected ? "true" : undefined}
                                onClick={() => void loadDiff(file.path)}
                              >
                                <span className="nex-tag nex-tag-mono nex-vcs-file-status">
                                  {statusLabel(file.status)}
                                </span>
                                <span
                                  className="nex-tag nex-tag-mono nex-vcs-file-path"
                                  title={file.path}
                                >
                                  {file.path}
                                </span>
                              </button>
                              <M3Button
                                variant="quiet"
                                size="sm"
                                disabled={busy || acting !== null}
                                onClick={() => void handleToggleStage(file.path, staged)}
                              >
                                {busy ? "Working…" : staged ? "Unstage" : "Stage"}
                              </M3Button>
                            </li>
                          );
                        })}
                      </ul>
                      {info.files_overflow > 0 && (
                        <p className="nex-vcs-notice" role="note">
                          +{info.files_overflow} more — the list is capped; refine or commit to
                          shrink it.
                        </p>
                      )}
                    </>
                  )}
                </section>

                {selectedPath && (
                  <section className="nex-vcs-section" aria-label={`Diff for ${selectedPath}`}>
                    <h3 className="nex-vcs-section-title">
                      Diff: <span className="nex-tag nex-tag-mono">{selectedPath}</span>
                    </h3>
                    {diffLoading ? (
                      <M3LoadingIndicator label={`Loading diff for ${selectedPath}`} />
                    ) : diffError || !diff ? (
                      <div className="nex-composer-error nex-fade-in" role="alert">
                        {diffError ?? "The diff is unavailable."}
                      </div>
                    ) : diff.binary ? (
                      <p className="nex-agent-empty">Binary file — no text diff available.</p>
                    ) : diff.diff === "" ? (
                      <p className="nex-agent-empty">No changes recorded for this file.</p>
                    ) : (
                      <>
                        {diff.truncated && (
                          <p className="nex-vcs-notice" role="note">
                            Diff truncated at 256 KiB — showing the first part.
                          </p>
                        )}
                        <div
                          className="nex-vcs-diff-scroll"
                          role="region"
                          aria-label={`Unified diff for ${selectedPath}`}
                          tabIndex={0}
                        >
                          <DiffView observation={diff.diff} />
                        </div>
                      </>
                    )}
                  </section>
                )}

                <section className="nex-vcs-section" aria-label="Commit staged changes">
                  <h3 className="nex-vcs-section-title">Commit: {stagedCount} staged</h3>
                  <textarea
                    ref={commitRef}
                    className="nex-composer-input"
                    rows={3}
                    placeholder="type(scope): subject"
                    aria-label="Commit message"
                    value={commitMessage}
                    disabled={acting !== null || generating}
                    onChange={(event) => setCommitMessage(event.target.value)}
                  />
                  {generatedTruncated && (
                    <p className="nex-vcs-notice" role="note">
                      The staged diff sent to the provider was truncated to 64 KiB — review the
                      message before committing.
                    </p>
                  )}
                  <div className="nex-vcs-header-actions">
                    <M3Button
                      variant="quiet"
                      size="sm"
                      disabled={generating || acting !== null}
                      onClick={() => void handleGenerate()}
                    >
                      {generating ? "Generating…" : "Generate message"}
                    </M3Button>
                    <M3Button
                      variant="primary"
                      size="sm"
                      disabled={
                        acting !== null || generating || commitMessage.trim() === "" || stagedCount === 0
                      }
                      onClick={() => void handleCommit()}
                    >
                      {acting === "commit" ? "Committing…" : "Commit"}
                    </M3Button>
                  </div>
                  <p className="nex-vcs-notice" role="note">
                    Generate sends the staged diff (up to 64 KiB) to the configured provider —
                    review before committing.
                  </p>
                </section>

                <section className="nex-vcs-section" aria-label="Push to origin">
                  <h3 className="nex-vcs-section-title">Push</h3>
                  <div className="nex-vcs-header-actions">
                    <M3Button
                      variant="secondary"
                      size="sm"
                      disabled={acting !== null || !info.branch}
                      onClick={() => void handlePush()}
                      title={info.branch ? "Push the current branch to origin" : "Detached HEAD cannot be pushed"}
                    >
                      {acting === "push" ? "Pushing…" : pushArmed ? "Confirm push to origin" : "Push to origin"}
                    </M3Button>
                  </div>
                  {pushArmed && info.branch && acting === null && (
                    <p className="nex-vcs-notice" role="note">
                      Push sends the current branch to origin — click again to confirm.
                    </p>
                  )}
                  {!info.branch && (
                    <p className="nex-agent-empty">Detached HEAD — pushing is unavailable.</p>
                  )}
                </section>
              </>
            ) : (
              <>
                <section className="nex-vcs-section" aria-label="Commit timeline">
                  <h3 className="nex-vcs-section-title">Timeline: {info.commits.length}</h3>
                  {info.commits.length === 0 ? (
                    <p className="nex-agent-empty">No commits yet.</p>
                  ) : (
                    <ol className="nex-vcs-timeline">
                      {info.commits.map((commit) => {
                        const selected = commit.hash === selectedCommit;
                        return (
                          <li key={commit.hash} className="nex-vcs-timeline-node">
                            <span className="nex-vcs-timeline-dot" aria-hidden="true" />
                            <button
                              type="button"
                              className={
                                "nex-vcs-timeline-card" + (selected ? " is-selected" : "")
                              }
                              aria-current={selected ? "true" : undefined}
                              onClick={() => void loadCommitDiff(commit.hash)}
                            >
                              <span className="nex-vcs-timeline-row">
                                <span
                                  className="nex-tag nex-tag-mono nex-vcs-commit-hash"
                                  title={commit.hash}
                                >
                                  {shortHash(commit.hash)}
                                </span>
                                <span
                                  className="nex-vcs-commit-message"
                                  title={commit.message || undefined}
                                >
                                  {commit.message || "(no message)"}
                                </span>
                              </span>
                              <span className="nex-vcs-commit-meta">
                                {commit.author || "unknown"}
                                {" · "}
                                <time dateTime={new Date(commit.time * 1000).toISOString()}>
                                  {formatRelativeTime(commit.time)}
                                </time>
                                {" · "}
                                {statsLine(commit)}
                              </span>
                              {commit.risk_signals.length > 0 && (
                                <span className="nex-vcs-risk-row">
                                  {commit.risk_signals.map((signal) => (
                                    <span
                                      key={signal}
                                      className="nex-tag nex-tag-mono nex-vcs-risk"
                                    >
                                      {riskLabel(signal)}
                                    </span>
                                  ))}
                                </span>
                              )}
                            </button>
                          </li>
                        );
                      })}
                    </ol>
                  )}
                </section>

                {selectedCommit && (
                  <section
                    className="nex-vcs-section"
                    aria-label={`Commit ${shortHash(selectedCommit)}`}
                  >
                    <h3 className="nex-vcs-section-title">
                      Commit: <span className="nex-tag nex-tag-mono">{shortHash(selectedCommit)}</span>
                    </h3>
                    {commitDiffLoading ? (
                      <M3LoadingIndicator label={`Loading commit ${shortHash(selectedCommit)}`} />
                    ) : commitDiffError || !commitDiff ? (
                      <div className="nex-composer-error nex-fade-in" role="alert">
                        {commitDiffError ?? "The commit diff is unavailable."}
                      </div>
                    ) : (
                      <>
                        <p className="nex-vcs-commit-meta">
                          {commitDiff.files.length === 1
                            ? "1 file changed"
                            : `${commitDiff.files.length} files changed`}
                          {commitDiff.binary ? " · includes binary content" : ""}
                        </p>
                        {commitDiff.files.length === 0 ? (
                          <p className="nex-agent-empty">
                            This commit changed no files — there is nothing to explain.
                          </p>
                        ) : (
                          <>
                            <ul className="nex-vcs-file-list">
                              {commitDiff.files.map((path) => (
                                <li key={path} className="nex-vcs-file-row">
                                  <span
                                    className="nex-tag nex-tag-mono nex-vcs-file-path"
                                    title={path}
                                  >
                                    {path}
                                  </span>
                                </li>
                              ))}
                            </ul>
                            {commitDiff.files_overflow > 0 && (
                              <p className="nex-vcs-notice" role="note">
                                +{commitDiff.files_overflow} more — the list is capped.
                              </p>
                            )}
                          </>
                        )}
                        {commitDiff.diff !== "" && (
                          <>
                            {commitDiff.truncated && (
                              <p className="nex-vcs-notice" role="note">
                                Diff truncated at 256 KiB — showing the first part.
                              </p>
                            )}
                            <div
                              className="nex-vcs-diff-scroll"
                              role="region"
                              aria-label={`Unified diff for commit ${shortHash(selectedCommit)}`}
                              tabIndex={0}
                            >
                              <DiffView observation={commitDiff.diff} />
                            </div>
                          </>
                        )}
                        <div className="nex-vcs-header-actions">
                          <M3Button
                            variant="quiet"
                            size="sm"
                            disabled={explain.loading || !explainable}
                            onClick={() => void handleExplainCommit(commitDiff.hash)}
                            title="Explain this commit with the configured provider"
                          >
                            {explain.loading
                              ? "Explaining…"
                              : explain.explanation
                                ? "Explain again"
                                : "Explain"}
                          </M3Button>
                          {explain.explanation && (
                            <M3Button
                              variant="quiet"
                              size="sm"
                              onClick={() => void handleCopyExplain(explain.explanation ?? "")}
                            >
                              {explain.copied ? "Copied" : "Copy explanation"}
                            </M3Button>
                          )}
                        </div>
                        <p className="nex-vcs-notice" role="note">
                          Explain sends this commit&apos;s diff (up to 64 KiB) to the configured
                          provider — the explanation is copy-only and never applied.
                        </p>
                        {explain.truncatedInput && (
                          <p className="nex-vcs-notice" role="note">
                            The commit diff sent to the provider was truncated to 64 KiB — the
                            explanation covers only what is shown.
                          </p>
                        )}
                        {explain.error && (
                          <div className="nex-composer-error nex-fade-in" role="alert">
                            {explain.error}
                          </div>
                        )}
                        {explain.explanation && (
                          <p className="nex-vcs-explanation">{explain.explanation}</p>
                        )}
                      </>
                    )}
                  </section>
                )}
              </>
            )}
          </>
        )}
      </div>
    </div>
  );
}
