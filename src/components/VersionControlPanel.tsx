//! Read-only version-control panel: workspace git status, recent commits,
//! and per-file diffs (no staging/committing/pushing — those come next).
//!
//! Presentational over the `git_info` / `git_file_diff` IPC wrappers:
//! `git_info` batches branch + changed files + commits in one round trip,
//! diffs load lazily when a file is selected (server-side capped with a
//! truncation notice). Diff rendering reuses the existing `DiffView`
//! classifier from `AgentRunSteps` — no new diff engine — inside a
//! keyboard-focusable scroll region. There is no file watching or live
//! refresh: the panel reloads on mount and on explicit Refresh only.
//!
//! Display rules: hashes render shortened (7 chars), times render relative,
//! statuses render as fixed-vocabulary labels — no raw backend values. The
//! panel never animates on entry (instant render under reduced motion).

import { useCallback, useEffect, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import {
  gitFileDiff,
  gitInfo,
  type CommandError,
  type GitFileDiff,
  type GitInfo,
} from "../lib/tauri";
import { DiffView } from "./AgentRunSteps";
import M3Button from "./M3Button";
import M3LoadingIndicator from "./M3LoadingIndicator";

const COMMIT_PAGE = 20;
const SHORT_HASH_LEN = 7;

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

function shortHash(hash: string): string {
  return hash.length > SHORT_HASH_LEN ? hash.slice(0, SHORT_HASH_LEN) : hash;
}

export interface VersionControlPanelProps {
  onClose: () => void;
}

export default function VersionControlPanel({ onClose }: VersionControlPanelProps) {
  const [info, setInfo] = useState<GitInfo | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitFileDiff | null>(null);
  const [diffLoading, setDiffLoading] = useState<boolean>(false);
  const [diffError, setDiffError] = useState<string | null>(null);

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

  // Refresh keeps the selected file (and reloads its diff) so a manual
  // refresh never loses the reader's place.
  const handleRefresh = useCallback(() => {
    void loadInfo(true).then(() => {
      if (selectedPath) void loadDiff(selectedPath);
    });
  }, [loadInfo, loadDiff, selectedPath]);

  return (
    <div className="nex-vcs" aria-label="Version control">
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">Version Control</h2>
          <p className="nex-vcs-subtitle">
            {info?.branch ? `On branch ${info.branch}.` : "Read-only git status for the open workspace."}
            {" "}Manual refresh only — staging and committing come next.
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

      <div className="nex-vcs-body">
        {loading ? (
          <M3LoadingIndicator label="Loading version control" />
        ) : error || !info ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error ?? "Version control is unavailable."}
          </div>
        ) : (
          <>
            <section className="nex-vcs-section" aria-label="Changed files">
              <h3 className="nex-vcs-section-title">
                Changed files: {info.files.length}
              </h3>
              {info.files.length === 0 ? (
                <p className="nex-agent-empty">No changes — the working tree is clean.</p>
              ) : (
                <ul className="nex-vcs-file-list">
                  {info.files.map((file) => {
                    const selected = file.path === selectedPath;
                    return (
                      <li key={file.path}>
                        <button
                          type="button"
                          className={"nex-vcs-file" + (selected ? " is-selected" : "")}
                          aria-current={selected ? "true" : undefined}
                          onClick={() => void loadDiff(file.path)}
                        >
                          <span className="nex-tag nex-tag-mono nex-vcs-file-status">
                            {statusLabel(file.status)}
                          </span>
                          <span className="nex-tag nex-tag-mono nex-vcs-file-path" title={file.path}>
                            {file.path}
                          </span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
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

            <section className="nex-vcs-section" aria-label="Recent commits">
              <h3 className="nex-vcs-section-title">
                Recent commits: {info.commits.length}
              </h3>
              {info.commits.length === 0 ? (
                <p className="nex-agent-empty">No commits yet.</p>
              ) : (
                <ul className="nex-vcs-commit-list">
                  {info.commits.map((commit) => (
                    <li key={commit.hash} className="nex-vcs-commit">
                      <span
                        className="nex-tag nex-tag-mono nex-vcs-commit-hash"
                        title={commit.hash}
                      >
                        {shortHash(commit.hash)}
                      </span>
                      <span className="nex-vcs-commit-message" title={commit.message || undefined}>
                        {commit.message || "(no message)"}
                      </span>
                      <span className="nex-vcs-commit-meta">
                        {commit.author || "unknown"}
                        {" · "}
                        <time dateTime={new Date(commit.time * 1000).toISOString()}>
                          {formatRelativeTime(commit.time)}
                        </time>
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
