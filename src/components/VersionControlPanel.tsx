//! Version-control panel: workspace git status, commit timeline with
//! per-commit stats and risk signals, per-file and per-commit diffs, AI
//! commit explanation, plus guarded staging, committing (with an
//! AI-generated conventional-commit message), and pushing to `origin`.
//!
//! Presentational over the `useVcs` data hook (AC-206), which owns the
//! `git_info` / `git_file_diff` / `git_commit_diff` reads and the write
//! wrappers (`git_stage` / `git_unstage` / `git_commit` / `git_push` /
//! `git_generate_commit_message` / `git_explain_commit`): `git_info` batches
//! branch + changed files + commits (with stats and signals) in one round
//! trip, file and commit diffs load lazily when selected (server-side capped
//! with a truncation notice). Every write passes the backend's explicit
//! per-call confirmation and refreshes the panel afterwards. Diff rendering
//! reuses the existing `DiffView` classifier from `AgentRunSteps` — no new
//! diff engine — inside a keyboard-focusable scroll region. There is no file
//! watching or live refresh: the panel reloads on mount and on explicit
//! Refresh only.
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

import { useEffect, useRef, useState } from "react";

import { formatRelativeTime } from "../lib/format";
import { useVcs } from "../lib/useVcs";
import { DiffView } from "./AgentRunSteps";
import Button from "./Button";
import LoadingIndicator from "./LoadingIndicator";
import { getLocale, tp, tr, type Locale } from "../lib/strings";
import { useStrings } from "../lib/useLocale";

const SHORT_HASH_LEN = 7;

type VcsTab = "working" | "timeline";

/** Fixed-vocabulary file status label; unknown values fall back to a
 * capitalized echo of the backend token (the backend only sends its fixed
 * set, so this branch is defensive). */
function statusLabel(status: string, locale: Locale = getLocale()): string {
  switch (status) {
    case "modified":
      return tr(locale, "vcs.statusModified");
    case "staged":
      return tr(locale, "vcs.statusStaged");
    case "untracked":
      return tr(locale, "vcs.statusUntracked");
    case "deleted":
      return tr(locale, "vcs.statusDeleted");
    case "renamed":
      return tr(locale, "vcs.statusRenamed");
    case "conflicted":
      return tr(locale, "vcs.statusConflicted");
    default:
      return status.charAt(0).toUpperCase() + status.slice(1);
  }
}

/** Fixed-vocabulary risk-signal label; unknown values fall back to a
 * capitalized echo of the backend token (same defensive shape as
 * `statusLabel`). */
function riskLabel(signal: string, locale: Locale = getLocale()): string {
  switch (signal) {
    case "large-diff":
      return tr(locale, "vcs.riskLargeDiff");
    case "many-files":
      return tr(locale, "vcs.riskManyFiles");
    case "binary":
      return tr(locale, "vcs.riskBinary");
    case "merge-commit":
      return tr(locale, "vcs.riskMerge");
    case "unfamiliar-author":
      return tr(locale, "vcs.riskUnfamiliar");
    default:
      return signal.charAt(0).toUpperCase() + signal.slice(1);
  }
}

/** Compact stats line for one timeline node: counts only, never content. */
function statsLine(
  commit: {
    files_changed: number;
    insertions: number;
    deletions: number;
  },
  locale: Locale = getLocale()
): string {
  const files = tp(locale, "files", commit.files_changed);
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
  /** Optional handoff to stage a prompt (e.g. conflict resolution or `/review`)
   * into the active conversation composer and return to chat. */
  onSendToAgent?: (prompt: string) => void;
}

export default function VersionControlPanel({
  onClose,
  request = null,
  onSendToAgent,
}: VersionControlPanelProps) {
  const { locale, t, tp: tpn } = useStrings();
  const vcs = useVcs();
  const {
    info,
    branches,
    stashes,
    loading,
    error,
    selectedPath,
    diff,
    diffLoading,
    diffError,
    selectedConflictPath,
    conflictDiff,
    conflictDiffLoading,
    conflictDiffError,
    selectedCommit,
    commitDiff,
    commitDiffLoading,
    commitDiffError,
    acting,
    actionError,
    actionNotice,
    commitMessage,
    generating,
    generatedTruncated,
    pushArmed,
    abortArmed,
    forceDeleteBranch,
    explain,
    reviewDiff,
    reviewLoading,
    reviewError,
    reviewCopied,
  } = vcs;
  const [tab, setTab] = useState<VcsTab>("working");
  const [selectedBranch, setSelectedBranch] = useState<string>("");
  const [newBranchName, setNewBranchName] = useState<string>("");
  const [stashMessage, setStashMessage] = useState<string>("");
  const commitRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (branches.length === 0) {
      setSelectedBranch("");
      return;
    }
    if (!branches.some((b) => b.name === selectedBranch)) {
      const current = branches.find((b) => b.is_current) ?? branches[0];
      setSelectedBranch(current.name);
    }
  }, [branches, selectedBranch]);

  // Palette requests (same handlers as the panel's own buttons — no
  // duplicated logic). Refresh reuses the store's refresh; commit-focus
  // moves focus to the commit composer once the panel has loaded.
  const refreshRef = useRef(vcs.refresh);
  refreshRef.current = vcs.refresh;
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

  const stagedCount =
    info?.files.filter((file) => file.status === "staged").length ?? 0;
  const conflicts = info?.conflicts ?? [];
  const mergeInProgress = Boolean(info?.merge_in_progress);
  const showConflictSection = conflicts.length > 0 || mergeInProgress;
  const explainable = commitDiff !== null && commitDiff.files.length > 0;

  const handleAskAgentResolve = async (path: string, status: string) => {
    const prompt = [
      `Resolve the git merge conflict in \`${path}\` (status: ${status}).`,
      "Inspect the conflict markers in the workspace, preserve the intended behavior from both sides where appropriate, remove all conflict markers, and verify the file builds cleanly.",
    ].join("\n\n");
    if (onSendToAgent) {
      onSendToAgent(prompt);
      return;
    }
    try {
      await navigator.clipboard.writeText(prompt);
      vcs.setNotice(t("vcs.agentPromptCopied"));
    } catch {
      vcs.setNotice(null);
    }
  };

  return (
    <div className="nex-vcs" role="group" aria-label={t("vcs.group")}>
      <header className="nex-vcs-header">
        <div className="nex-vcs-heading">
          <h2 className="nex-vcs-title">{t("vcs.title")}</h2>
          <p className="nex-vcs-subtitle">
            {info?.branch
              ? t("vcs.subtitleBranch", { branch: info.branch })
              : t("vcs.subtitleBare")}{" "}
            {t("vcs.subtitleSuffix")}
          </p>
        </div>
        <div className="nex-vcs-header-actions">
          <Button variant="quiet" onClick={vcs.refresh} disabled={loading}>
            {loading ? t("vcs.refreshing") : t("vcs.refresh")}
          </Button>
          <Button variant="quiet" onClick={onClose}>
            {t("common.backToConversations")}
          </Button>
        </div>
      </header>

      <div
        className="nex-vcs-tabs"
        role="tablist"
        aria-label={t("vcs.tabsAria")}
      >
        <button
          type="button"
          role="tab"
          aria-selected={tab === "working"}
          className={"nex-vcs-tab" + (tab === "working" ? " is-selected" : "")}
          onClick={() => setTab("working")}
        >
          {t("vcs.tabWorking")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "timeline"}
          className={"nex-vcs-tab" + (tab === "timeline" ? " is-selected" : "")}
          onClick={() => setTab("timeline")}
        >
          {t("vcs.tabTimeline")}
        </button>
      </div>

      <div className="nex-vcs-body">
        {loading ? (
          <LoadingIndicator label={t("vcs.loading")} />
        ) : error || !info ? (
          <div className="nex-composer-error nex-fade-in" role="alert">
            {error ?? t("vcs.unavailable")}
          </div>
        ) : (
          <>
            {actionError && (
              <div className="nex-composer-error nex-fade-in" role="alert">
                {actionError}
              </div>
            )}
            {actionNotice && (
              <p className="nex-vcs-notice" role="status">
                {actionNotice}
              </p>
            )}
            {tab === "working" ? (
              <>
                {showConflictSection && (
                  <section
                    className="nex-vcs-section nex-vcs-conflict-section"
                    aria-label={t("vcs.conflictsTitle", {
                      n: conflicts.length,
                    })}
                  >
                    <div className="nex-vcs-conflict-header">
                      <div className="nex-vcs-conflict-heading">
                        <h3 className="nex-vcs-section-title">
                          {t("vcs.conflictsTitle", { n: conflicts.length })}
                        </h3>
                        {mergeInProgress && (
                          <span className="nex-tag nex-tag-mono nex-vcs-conflict-badge">
                            {t("vcs.mergeInProgress")}
                          </span>
                        )}
                      </div>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={acting !== null}
                        onClick={() => void vcs.abortMerge()}
                      >
                        {acting === "abort-merge"
                          ? t("vcs.abortingMerge")
                          : abortArmed
                            ? t("vcs.confirmAbortMerge")
                            : t("vcs.abortMerge")}
                      </Button>
                    </div>
                    {abortArmed && acting === null && (
                      <p className="nex-vcs-notice" role="note">
                        {t("vcs.abortMergeNotice")}
                      </p>
                    )}
                    {conflicts.length === 0 ? (
                      <p className="nex-vcs-notice" role="note">
                        {t("vcs.mergeReadyToCommit")}
                      </p>
                    ) : (
                      <ul className="nex-vcs-file-list">
                        {conflicts.map((conflict) => {
                          const selected =
                            conflict.path === selectedConflictPath;
                          const busy = acting === `resolve:${conflict.path}`;
                          return (
                            <li
                              key={conflict.path}
                              className="nex-vcs-conflict-row"
                            >
                              <button
                                type="button"
                                className={
                                  "nex-vcs-file" +
                                  (selected ? " is-selected" : "")
                                }
                                aria-current={selected ? "true" : undefined}
                                onClick={() =>
                                  void vcs.loadConflictDiff(conflict.path)
                                }
                              >
                                <span className="nex-tag nex-tag-mono nex-vcs-conflict-badge">
                                  {conflict.status}
                                </span>
                                <span
                                  className="nex-tag nex-tag-mono nex-vcs-file-path"
                                  title={conflict.path}
                                >
                                  {conflict.path}
                                </span>
                              </button>
                              <div className="nex-vcs-conflict-actions">
                                <Button
                                  variant="quiet"
                                  size="sm"
                                  disabled={busy || acting !== null}
                                  onClick={() =>
                                    void vcs.resolveConflict(
                                      conflict.path,
                                      "ours"
                                    )
                                  }
                                >
                                  {t("vcs.acceptOurs")}
                                </Button>
                                <Button
                                  variant="quiet"
                                  size="sm"
                                  disabled={busy || acting !== null}
                                  onClick={() =>
                                    void vcs.resolveConflict(
                                      conflict.path,
                                      "theirs"
                                    )
                                  }
                                >
                                  {t("vcs.acceptTheirs")}
                                </Button>
                                <Button
                                  variant="quiet"
                                  size="sm"
                                  disabled={acting !== null}
                                  onClick={() =>
                                    void handleAskAgentResolve(
                                      conflict.path,
                                      conflict.status
                                    )
                                  }
                                >
                                  {t("vcs.askAgentResolve")}
                                </Button>
                              </div>
                            </li>
                          );
                        })}
                      </ul>
                    )}

                    {selectedConflictPath && (
                      <div
                        className="nex-vcs-section"
                        aria-label={t("vcs.conflictPreviewAria", {
                          path: selectedConflictPath,
                        })}
                      >
                        <h4 className="nex-vcs-section-title">
                          {t("vcs.conflictPreviewTitle")}{" "}
                          <span className="nex-tag nex-tag-mono">
                            {selectedConflictPath}
                          </span>
                        </h4>
                        {conflictDiffLoading ? (
                          <LoadingIndicator
                            label={t("vcs.conflictLoading", {
                              path: selectedConflictPath,
                            })}
                          />
                        ) : conflictDiffError || !conflictDiff ? (
                          <div
                            className="nex-composer-error nex-fade-in"
                            role="alert"
                          >
                            {conflictDiffError ?? t("vcs.conflictUnavailable")}
                          </div>
                        ) : conflictDiff.binary ? (
                          <p className="nex-agent-empty">
                            {t("vcs.binaryFile")}
                          </p>
                        ) : (
                          <>
                            {conflictDiff.truncated && (
                              <p className="nex-vcs-notice" role="note">
                                {t("vcs.diffTruncated")}
                              </p>
                            )}
                            {conflictDiff.working !== "" && (
                              <div className="nex-vcs-conflict-pane">
                                <span className="nex-vcs-conflict-pane-label">
                                  {t("vcs.conflictWorking")}
                                </span>
                                <div
                                  className="nex-vcs-diff-scroll"
                                  role="region"
                                  aria-label={t("vcs.conflictPreviewAria", {
                                    path: selectedConflictPath,
                                  })}
                                  tabIndex={0}
                                >
                                  <DiffView
                                    observation={conflictDiff.working}
                                  />
                                </div>
                              </div>
                            )}
                            <div className="nex-vcs-conflict-panes">
                              <div className="nex-vcs-conflict-pane">
                                <span className="nex-vcs-conflict-pane-label">
                                  {t("vcs.conflictOurs")}
                                </span>
                                <pre className="nex-vcs-conflict-code">
                                  {conflictDiff.ours || "(deleted)"}
                                </pre>
                              </div>
                              <div className="nex-vcs-conflict-pane">
                                <span className="nex-vcs-conflict-pane-label">
                                  {t("vcs.conflictTheirs")}
                                </span>
                                <pre className="nex-vcs-conflict-code">
                                  {conflictDiff.theirs || "(deleted)"}
                                </pre>
                              </div>
                            </div>
                          </>
                        )}
                      </div>
                    )}
                  </section>
                )}

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.changedTitle", { n: info.files.length })}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.changedTitle", { n: info.files.length })}
                  </h3>
                  {info.files.length === 0 ? (
                    <p className="nex-agent-empty">{t("vcs.cleanTree")}</p>
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
                                className={
                                  "nex-vcs-file" +
                                  (selected ? " is-selected" : "")
                                }
                                aria-current={selected ? "true" : undefined}
                                onClick={() => void vcs.loadDiff(file.path)}
                              >
                                <span className="nex-tag nex-tag-mono nex-vcs-file-status">
                                  {statusLabel(file.status, locale)}
                                </span>
                                <span
                                  className="nex-tag nex-tag-mono nex-vcs-file-path"
                                  title={file.path}
                                >
                                  {file.path}
                                </span>
                              </button>
                              <Button
                                variant="quiet"
                                size="sm"
                                disabled={busy || acting !== null}
                                onClick={() =>
                                  void vcs.toggleStage(file.path, staged)
                                }
                              >
                                {busy
                                  ? t("vcs.working")
                                  : staged
                                    ? t("vcs.unstage")
                                    : t("vcs.stage")}
                              </Button>
                            </li>
                          );
                        })}
                      </ul>
                      {info.files_overflow > 0 && (
                        <p className="nex-vcs-notice" role="note">
                          {t("vcs.filesCapped", { n: info.files_overflow })}
                        </p>
                      )}
                    </>
                  )}
                </section>

                {selectedPath && (
                  <section
                    className="nex-vcs-section"
                    aria-label={t("vcs.diffAria", { path: selectedPath })}
                  >
                    <h3 className="nex-vcs-section-title">
                      {t("vcs.diffTitle")}{" "}
                      <span className="nex-tag nex-tag-mono">
                        {selectedPath}
                      </span>
                    </h3>
                    {diffLoading ? (
                      <LoadingIndicator
                        label={t("vcs.diffLoading", { path: selectedPath })}
                      />
                    ) : diffError || !diff ? (
                      <div
                        className="nex-composer-error nex-fade-in"
                        role="alert"
                      >
                        {diffError ?? t("vcs.diffUnavailable")}
                      </div>
                    ) : diff.binary ? (
                      <p className="nex-agent-empty">{t("vcs.binaryFile")}</p>
                    ) : diff.diff === "" ? (
                      <p className="nex-agent-empty">
                        {t("vcs.noChangesFile")}
                      </p>
                    ) : (
                      <>
                        {diff.truncated && (
                          <p className="nex-vcs-notice" role="note">
                            {t("vcs.diffTruncated")}
                          </p>
                        )}
                        <div
                          className="nex-vcs-diff-scroll"
                          role="region"
                          aria-label={t("vcs.diffRegion", {
                            path: selectedPath,
                          })}
                          tabIndex={0}
                        >
                          <DiffView observation={diff.diff} />
                        </div>
                      </>
                    )}
                  </section>
                )}

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.commitSection", { n: stagedCount })}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.commitSection", { n: stagedCount })}
                  </h3>
                  <textarea
                    ref={commitRef}
                    className="nex-composer-input"
                    rows={3}
                    placeholder={t("vcs.commitPh")}
                    aria-label={t("vcs.commitAria")}
                    value={commitMessage}
                    disabled={acting !== null || generating}
                    onChange={(event) =>
                      vcs.setCommitMessage(event.target.value)
                    }
                  />
                  {generatedTruncated && (
                    <p className="nex-vcs-notice" role="note">
                      {t("vcs.genTruncated")}
                    </p>
                  )}
                  <div className="nex-vcs-header-actions">
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={generating || acting !== null}
                      onClick={() => void vcs.generate()}
                    >
                      {generating ? t("vcs.generating") : t("vcs.generate")}
                    </Button>
                    <Button
                      variant="primary"
                      size="sm"
                      disabled={
                        acting !== null ||
                        generating ||
                        commitMessage.trim() === "" ||
                        stagedCount === 0
                      }
                      onClick={() => void vcs.commit()}
                    >
                      {acting === "commit"
                        ? t("vcs.committing")
                        : t("vcs.commit")}
                    </Button>
                  </div>
                  <p className="nex-vcs-notice" role="note">
                    {t("vcs.genNotice")}
                  </p>
                </section>

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.remoteSection")}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.remoteSection")}
                  </h3>
                  <div className="nex-vcs-header-actions">
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={acting !== null}
                      onClick={() => void vcs.fetchOrigin()}
                    >
                      {acting === "fetch" ? t("vcs.fetching") : t("vcs.fetch")}
                    </Button>
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={acting !== null || !info.branch}
                      onClick={() => void vcs.pullOrigin()}
                    >
                      {acting === "pull" ? t("vcs.pulling") : t("vcs.pull")}
                    </Button>
                    <Button
                      variant="secondary"
                      size="sm"
                      disabled={acting !== null || !info.branch}
                      onClick={() => void vcs.push()}
                      title={
                        info.branch
                          ? t("vcs.pushTitleOk")
                          : t("vcs.pushTitleDetached")
                      }
                    >
                      {acting === "push"
                        ? t("vcs.pushing")
                        : pushArmed
                          ? t("vcs.confirmPush")
                          : t("vcs.push")}
                    </Button>
                  </div>
                  {pushArmed && info.branch && acting === null && (
                    <p className="nex-vcs-notice" role="note">
                      {t("vcs.pushNotice")}
                    </p>
                  )}
                  {!info.branch && (
                    <p className="nex-agent-empty">{t("vcs.detached")}</p>
                  )}
                </section>

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.branchesSection")}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.branchesSection")}
                  </h3>
                  {branches.length > 0 && (
                    <div className="nex-vcs-inline-form">
                      <select
                        className="nex-vcs-select"
                        aria-label={t("vcs.branchSelectAria")}
                        value={selectedBranch}
                        disabled={acting !== null}
                        onChange={(event) => {
                          setSelectedBranch(event.target.value);
                          vcs.cancelForceDelete();
                        }}
                      >
                        {branches.map((branch) => (
                          <option key={branch.name} value={branch.name}>
                            {branch.is_current
                              ? `* ${branch.name}`
                              : branch.name}
                          </option>
                        ))}
                      </select>
                      <Button
                        variant="quiet"
                        size="sm"
                        disabled={
                          acting !== null ||
                          !selectedBranch ||
                          selectedBranch === info.branch
                        }
                        onClick={() => void vcs.switchBranch(selectedBranch)}
                      >
                        {acting === "branch-switch"
                          ? t("vcs.branchSwitching")
                          : t("vcs.branchSwitch")}
                      </Button>
                      <Button
                        variant="quiet"
                        size="sm"
                        disabled={
                          acting !== null ||
                          !selectedBranch ||
                          selectedBranch === info.branch
                        }
                        onClick={() =>
                          void vcs.deleteBranch(selectedBranch, false)
                        }
                      >
                        {acting === "branch-delete"
                          ? t("vcs.branchDeleting")
                          : t("vcs.branchDelete")}
                      </Button>
                    </div>
                  )}
                  {forceDeleteBranch && (
                    <div className="nex-vcs-notice" role="alert">
                      <p>
                        {t("vcs.branchForceDeletePrompt", {
                          name: forceDeleteBranch,
                        })}
                      </p>
                      <div className="nex-vcs-header-actions">
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={acting !== null}
                          onClick={() =>
                            void vcs.deleteBranch(forceDeleteBranch, true)
                          }
                        >
                          {t("vcs.branchForceDeleteConfirm", {
                            name: forceDeleteBranch,
                          })}
                        </Button>
                        <Button
                          variant="quiet"
                          size="sm"
                          disabled={acting !== null}
                          onClick={vcs.cancelForceDelete}
                        >
                          {t("vcs.cancel")}
                        </Button>
                      </div>
                    </div>
                  )}
                  <div className="nex-vcs-inline-form">
                    <input
                      type="text"
                      className="nex-vcs-input"
                      placeholder={t("vcs.branchNewPh")}
                      aria-label={t("vcs.branchNewAria")}
                      value={newBranchName}
                      disabled={acting !== null}
                      onChange={(event) => setNewBranchName(event.target.value)}
                    />
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={
                        acting !== null || newBranchName.trim() === ""
                      }
                      onClick={async () => {
                        const ok = await vcs.createBranch(newBranchName, true);
                        if (ok) setNewBranchName("");
                      }}
                    >
                      {acting === "branch-create"
                        ? t("vcs.branchCreating")
                        : t("vcs.branchCreate")}
                    </Button>
                  </div>
                </section>

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.stashSection", { n: stashes.length })}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.stashSection", { n: stashes.length })}
                  </h3>
                  <div className="nex-vcs-inline-form">
                    <input
                      type="text"
                      className="nex-vcs-input"
                      placeholder={t("vcs.stashMessagePh")}
                      aria-label={t("vcs.stashMessageAria")}
                      value={stashMessage}
                      disabled={acting !== null}
                      onChange={(event) => setStashMessage(event.target.value)}
                    />
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={acting !== null || info.files.length === 0}
                      onClick={async () => {
                        const ok = await vcs.stashPush(stashMessage);
                        if (ok) setStashMessage("");
                      }}
                    >
                      {acting === "stash-push"
                        ? t("vcs.stashing")
                        : t("vcs.stashPush")}
                    </Button>
                  </div>
                  {stashes.length === 0 ? (
                    <p className="nex-agent-empty">{t("vcs.stashEmpty")}</p>
                  ) : (
                    <ul className="nex-vcs-file-list">
                      {stashes.map((entry) => (
                        <li
                          key={`${entry.index}-${entry.hash}`}
                          className="nex-vcs-stash-row"
                        >
                          <div className="nex-vcs-stash-meta">
                            <span className="nex-tag nex-tag-mono">
                              stash@{"{"}
                              {entry.index}
                              {"}"}
                            </span>
                            <span
                              className="nex-vcs-commit-message"
                              title={entry.message}
                            >
                              {entry.message}
                            </span>
                          </div>
                          <Button
                            variant="quiet"
                            size="sm"
                            disabled={acting !== null}
                            onClick={() => void vcs.stashPop(entry.index)}
                          >
                            {acting === `stash-pop-${entry.index}`
                              ? t("vcs.stashPopping")
                              : t("vcs.stashPop")}
                          </Button>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.reviewSection")}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.reviewSection")}
                  </h3>
                  <div className="nex-vcs-header-actions">
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={reviewLoading || acting !== null}
                      onClick={() => void vcs.prepareReview("working")}
                    >
                      {reviewLoading
                        ? t("vcs.reviewPreparing")
                        : t("vcs.reviewWorking")}
                    </Button>
                    <Button
                      variant="quiet"
                      size="sm"
                      disabled={
                        reviewLoading || acting !== null || !info.branch
                      }
                      onClick={() => void vcs.prepareReview("branch")}
                    >
                      {t("vcs.reviewBranch")}
                    </Button>
                  </div>
                  {reviewError && (
                    <div
                      className="nex-composer-error nex-fade-in"
                      role="alert"
                    >
                      {reviewError}
                    </div>
                  )}
                  {reviewDiff && (
                    <>
                      {reviewDiff.redacted && (
                        <p className="nex-vcs-notice" role="note">
                          {t("vcs.reviewRedacted")}
                        </p>
                      )}
                      {reviewDiff.truncated && (
                        <p className="nex-vcs-notice" role="note">
                          {t("vcs.diffTruncated")}
                        </p>
                      )}
                      {reviewDiff.diff === "" ? (
                        <p className="nex-agent-empty">
                          {t("vcs.reviewEmpty")}
                        </p>
                      ) : (
                        <>
                          <div
                            className="nex-vcs-diff-scroll"
                            role="region"
                            aria-label={t("vcs.reviewPreviewRegion")}
                            tabIndex={0}
                          >
                            <DiffView observation={reviewDiff.diff} />
                          </div>
                          <div className="nex-vcs-header-actions">
                            <Button
                              variant="quiet"
                              size="sm"
                              onClick={() =>
                                void vcs.copyReviewPrompt(reviewDiff.prompt)
                              }
                            >
                              {reviewCopied
                                ? t("vcs.copied")
                                : t("vcs.reviewCopy")}
                            </Button>
                            {onSendToAgent && (
                              <Button
                                variant="primary"
                                size="sm"
                                onClick={() => onSendToAgent(reviewDiff.prompt)}
                              >
                                {t("vcs.reviewSend")}
                              </Button>
                            )}
                          </div>
                        </>
                      )}
                    </>
                  )}
                </section>
              </>
            ) : (
              <>
                <section
                  className="nex-vcs-section"
                  aria-label={t("vcs.timelineTitle", {
                    n: info.commits.length,
                  })}
                >
                  <h3 className="nex-vcs-section-title">
                    {t("vcs.timelineTitle", { n: info.commits.length })}
                  </h3>
                  {info.commits.length === 0 ? (
                    <p className="nex-agent-empty">{t("vcs.noCommits")}</p>
                  ) : (
                    <ol className="nex-vcs-timeline">
                      {info.commits.map((commit) => {
                        const selected = commit.hash === selectedCommit;
                        return (
                          <li
                            key={commit.hash}
                            className="nex-vcs-timeline-node"
                          >
                            <span
                              className="nex-vcs-timeline-dot"
                              aria-hidden="true"
                            />
                            <button
                              type="button"
                              className={
                                "nex-vcs-timeline-card" +
                                (selected ? " is-selected" : "")
                              }
                              aria-current={selected ? "true" : undefined}
                              onClick={() =>
                                void vcs.loadCommitDiff(commit.hash)
                              }
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
                                  {commit.message || t("vcs.noMessage")}
                                </span>
                              </span>
                              <span className="nex-vcs-commit-meta">
                                {commit.author || t("vcs.unknownAuthor")}
                                {" · "}
                                <time
                                  dateTime={new Date(
                                    commit.time * 1000
                                  ).toISOString()}
                                >
                                  {formatRelativeTime(commit.time, locale)}
                                </time>
                                {" · "}
                                {statsLine(commit, locale)}
                              </span>
                              {commit.risk_signals.length > 0 && (
                                <span className="nex-vcs-risk-row">
                                  {commit.risk_signals.map((signal) => (
                                    <span
                                      key={signal}
                                      className="nex-tag nex-tag-mono nex-vcs-risk"
                                    >
                                      {riskLabel(signal, locale)}
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
                    aria-label={t("vcs.commitAriaTitle", {
                      hash: shortHash(selectedCommit),
                    })}
                  >
                    <h3 className="nex-vcs-section-title">
                      {t("vcs.commitTitle")}{" "}
                      <span className="nex-tag nex-tag-mono">
                        {shortHash(selectedCommit)}
                      </span>
                    </h3>
                    {commitDiffLoading ? (
                      <LoadingIndicator
                        label={t("vcs.commitLoading", {
                          hash: shortHash(selectedCommit),
                        })}
                      />
                    ) : commitDiffError || !commitDiff ? (
                      <div
                        className="nex-composer-error nex-fade-in"
                        role="alert"
                      >
                        {commitDiffError ?? t("vcs.commitDiffUnavailable")}
                      </div>
                    ) : (
                      <>
                        <p className="nex-vcs-commit-meta">
                          {tpn("filesChanged", commitDiff.files.length)}
                          {commitDiff.binary ? t("vcs.binarySuffix") : ""}
                        </p>
                        {commitDiff.files.length === 0 ? (
                          <p className="nex-agent-empty">
                            {t("vcs.noFilesExplain")}
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
                                {t("vcs.filesCappedShort", {
                                  n: commitDiff.files_overflow,
                                })}
                              </p>
                            )}
                          </>
                        )}
                        {commitDiff.diff !== "" && (
                          <>
                            {commitDiff.truncated && (
                              <p className="nex-vcs-notice" role="note">
                                {t("vcs.diffTruncated")}
                              </p>
                            )}
                            <div
                              className="nex-vcs-diff-scroll"
                              role="region"
                              aria-label={t("vcs.commitDiffRegion", {
                                hash: shortHash(selectedCommit),
                              })}
                              tabIndex={0}
                            >
                              <DiffView observation={commitDiff.diff} />
                            </div>
                          </>
                        )}
                        <div className="nex-vcs-header-actions">
                          <Button
                            variant="quiet"
                            size="sm"
                            disabled={explain.loading || !explainable}
                            onClick={() =>
                              void vcs.explainCommit(commitDiff.hash)
                            }
                            title={t("vcs.explainTitle")}
                          >
                            {explain.loading
                              ? t("vcs.explaining")
                              : explain.explanation
                                ? t("vcs.explainAgain")
                                : t("vcs.explain")}
                          </Button>
                          {explain.explanation && (
                            <Button
                              variant="quiet"
                              size="sm"
                              onClick={() =>
                                void vcs.copyExplanation(
                                  explain.explanation ?? ""
                                )
                              }
                            >
                              {explain.copied
                                ? t("vcs.copied")
                                : t("vcs.copyExplanation")}
                            </Button>
                          )}
                        </div>
                        <p className="nex-vcs-notice" role="note">
                          {t("vcs.explainNotice")}
                        </p>
                        {explain.truncatedInput && (
                          <p className="nex-vcs-notice" role="note">
                            {t("vcs.explainTruncated")}
                          </p>
                        )}
                        {explain.error && (
                          <div
                            className="nex-composer-error nex-fade-in"
                            role="alert"
                          >
                            {explain.error}
                          </div>
                        )}
                        {explain.explanation && (
                          <p className="nex-vcs-explanation">
                            {explain.explanation}
                          </p>
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
