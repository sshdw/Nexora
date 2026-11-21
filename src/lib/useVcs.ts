//! Version-control data hook (AC-206 + Stage 10 M20/P37/P38).
//!
//! Owns the VCS panel's git data machine over the IPC wrappers:
//! - Reads: `git_info` (branch, changed files, commits, conflicts, merge state),
//!   `git_branches`, `git_stash_list`, lazy `git_file_diff`, `git_commit_diff`,
//!   `git_conflict_diff`, and `git_prepare_review_diff` (with secret redaction).
//! - Guarded writes (server-side confirmation gate): `git_stage`, `git_unstage`,
//!   `git_commit`, `git_push`, `git_pull`, `git_branch_create`,
//!   `git_branch_switch`, `git_branch_delete`, `git_stash_push`, `git_stash_pop`,
//!   `git_resolve_conflict`, `git_abort_merge`, plus `git_fetch`.
//! - AI assists: `git_generate_commit_message`, `git_explain_commit`.

import { useCallback, useEffect, useRef, useState } from "react";

import {
  getSetting,
  gitAbortMerge,
  gitBranchCreate,
  gitBranchDelete,
  gitBranches,
  gitBranchSwitch,
  gitCommit,
  gitCommitDiff,
  gitConflictDiff,
  gitExplainCommit,
  gitFetch,
  gitFileDiff,
  gitGenerateCommitMessage,
  gitInfo,
  gitPrepareReviewDiff,
  gitPull,
  gitPush,
  gitResolveConflict,
  gitStage,
  gitStashList,
  gitStashPop,
  gitStashPush,
  gitUnstage,
  type CommandError,
  type CommitExplanation,
  type GitBranch,
  type GitCommitDiff,
  type GitConflictDiff,
  type GitFileDiff,
  type GitInfo,
  type GitReviewDiff,
  type GitStashEntry,
} from "./tauri";
import { getLocale, tr } from "./strings";

const COMMIT_PAGE = 20;

/** Explain state for the selected commit: the AI explanation (copy-only),
 * or the failure. `copied` confirms the Copy press without a timer. */
export interface CommitExplainState {
  loading: boolean;
  explanation: string | null;
  truncatedInput: boolean;
  error: string | null;
  copied: boolean;
}

export const IDLE_EXPLAIN: CommitExplainState = {
  loading: false,
  explanation: null,
  truncatedInput: false,
  error: null,
  copied: false,
};

export interface VcsStore {
  info: GitInfo | null;
  branches: GitBranch[];
  stashes: GitStashEntry[];
  loading: boolean;
  error: string | null;
  selectedPath: string | null;
  diff: GitFileDiff | null;
  diffLoading: boolean;
  diffError: string | null;
  selectedConflictPath: string | null;
  conflictDiff: GitConflictDiff | null;
  conflictDiffLoading: boolean;
  conflictDiffError: string | null;
  selectedCommit: string | null;
  commitDiff: GitCommitDiff | null;
  commitDiffLoading: boolean;
  commitDiffError: string | null;
  /** In-flight write label (path, `commit`, `push`, `fetch`, `pull`, etc.). */
  acting: string | null;
  actionError: string | null;
  actionNotice: string | null;
  commitMessage: string;
  setCommitMessage: (message: string) => void;
  generating: boolean;
  generatedTruncated: boolean;
  /** Two-press push: first call arms, the second sends. */
  pushArmed: boolean;
  /** Two-press abort merge/pull: first call arms, the second aborts. */
  abortArmed: boolean;
  /** Branch awaiting explicit force-delete confirmation after unmerged refusal. */
  forceDeleteBranch: string | null;
  explain: CommitExplainState;
  reviewDiff: GitReviewDiff | null;
  reviewLoading: boolean;
  reviewError: string | null;
  reviewCopied: boolean;
  /** Load the aggregate (mount: `false` resets selections; refresh: `true`). */
  loadInfo: (keepSelection: boolean) => Promise<void>;
  /** Manual refresh: keeps the selection and reloads its diffs. */
  refresh: () => void;
  loadDiff: (path: string) => Promise<void>;
  loadConflictDiff: (path: string) => Promise<void>;
  loadCommitDiff: (hash: string) => Promise<void>;
  toggleStage: (path: string, staged: boolean) => Promise<void>;
  commit: () => Promise<void>;
  generate: () => Promise<void>;
  push: () => Promise<void>;
  fetchOrigin: () => Promise<void>;
  pullOrigin: () => Promise<void>;
  createBranch: (name: string, switchTo?: boolean) => Promise<boolean>;
  switchBranch: (name: string) => Promise<void>;
  deleteBranch: (name: string, force?: boolean) => Promise<void>;
  cancelForceDelete: () => void;
  stashPush: (message?: string) => Promise<boolean>;
  stashPop: (index?: number) => Promise<void>;
  resolveConflict: (path: string, strategy: "ours" | "theirs") => Promise<void>;
  abortMerge: () => Promise<void>;
  prepareReview: (
    scope: "working" | "branch",
    baseBranch?: string | null
  ) => Promise<void>;
  copyReviewPrompt: (text: string) => Promise<void>;
  explainCommit: (hash: string) => Promise<void>;
  copyExplanation: (text: string) => Promise<void>;
  setNotice: (notice: string | null) => void;
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

export function useVcs(): VcsStore {
  const [info, setInfo] = useState<GitInfo | null>(null);
  const [branches, setBranches] = useState<GitBranch[]>([]);
  const [stashes, setStashes] = useState<GitStashEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [diff, setDiff] = useState<GitFileDiff | null>(null);
  const [diffLoading, setDiffLoading] = useState<boolean>(false);
  const [diffError, setDiffError] = useState<string | null>(null);
  const [selectedConflictPath, setSelectedConflictPath] = useState<
    string | null
  >(null);
  const [conflictDiff, setConflictDiff] = useState<GitConflictDiff | null>(
    null
  );
  const [conflictDiffLoading, setConflictDiffLoading] =
    useState<boolean>(false);
  const [conflictDiffError, setConflictDiffError] = useState<string | null>(
    null
  );
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const [acting, setActing] = useState<string | null>(null);
  const [commitMessage, setCommitMessage] = useState<string>("");
  const [generating, setGenerating] = useState<boolean>(false);
  const [generatedTruncated, setGeneratedTruncated] = useState<boolean>(false);
  const [pushArmed, setPushArmed] = useState<boolean>(false);
  const [abortArmed, setAbortArmed] = useState<boolean>(false);
  const [forceDeleteBranch, setForceDeleteBranch] = useState<string | null>(
    null
  );
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const [commitDiff, setCommitDiff] = useState<GitCommitDiff | null>(null);
  const [commitDiffLoading, setCommitDiffLoading] = useState<boolean>(false);
  const [commitDiffError, setCommitDiffError] = useState<string | null>(null);
  const [explain, setExplain] = useState<CommitExplainState>(IDLE_EXPLAIN);
  const [reviewDiff, setReviewDiff] = useState<GitReviewDiff | null>(null);
  const [reviewLoading, setReviewLoading] = useState<boolean>(false);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [reviewCopied, setReviewCopied] = useState<boolean>(false);
  const explainingRef = useRef<string | null>(null);
  const selectedConflictPathRef = useRef<string | null>(null);
  selectedConflictPathRef.current = selectedConflictPath;

  const loadInfo = useCallback(async (keepSelection: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const data = await gitInfo(COMMIT_PAGE);
      setInfo(data);
      // Secondary workflow metadata loads best-effort so partial mocks or
      // unborn repositories never block the main VCS status panel.
      const [branchList, stashList] = await Promise.all([
        gitBranches().catch(() => [] as GitBranch[]),
        gitStashList().catch(() => [] as GitStashEntry[]),
      ]);
      setBranches(branchList);
      setStashes(stashList);
      if (!keepSelection) {
        setSelectedPath(null);
        setDiff(null);
        setDiffError(null);
        setSelectedConflictPath(null);
        setConflictDiff(null);
        setConflictDiffError(null);
        setSelectedCommit(null);
        setCommitDiff(null);
        setCommitDiffError(null);
        setExplain(IDLE_EXPLAIN);
      } else if (
        selectedConflictPathRef.current &&
        !(data.conflicts ?? []).some(
          (c) => c.path === selectedConflictPathRef.current
        )
      ) {
        setSelectedConflictPath(null);
        setConflictDiff(null);
        setConflictDiffError(null);
      }
    } catch (e) {
      setError(toMessage(e));
      setInfo(null);
      setBranches([]);
      setStashes([]);
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

  const loadConflictDiff = useCallback(async (path: string) => {
    setSelectedConflictPath(path);
    setConflictDiffLoading(true);
    setConflictDiffError(null);
    setConflictDiff(null);
    try {
      setConflictDiff(await gitConflictDiff(path));
    } catch (e) {
      setConflictDiffError(toMessage(e));
    } finally {
      setConflictDiffLoading(false);
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

  const refresh = useCallback(() => {
    setPushArmed(false);
    setAbortArmed(false);
    setForceDeleteBranch(null);
    void loadInfo(true).then(() => {
      if (selectedPath) void loadDiff(selectedPath);
      if (selectedCommit) void loadCommitDiff(selectedCommit);
      if (selectedConflictPath) void loadConflictDiff(selectedConflictPath);
    });
  }, [
    loadInfo,
    loadDiff,
    loadCommitDiff,
    loadConflictDiff,
    selectedPath,
    selectedCommit,
    selectedConflictPath,
  ]);

  const toggleStage = useCallback(
    async (path: string, staged: boolean) => {
      setActing(path);
      setActionError(null);
      setActionNotice(null);
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
    [loadInfo]
  );

  const commit = useCallback(async () => {
    const message = commitMessage.trim();
    if (message === "") return;
    setActing("commit");
    setActionError(null);
    setActionNotice(null);
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

  const generate = useCallback(async () => {
    setGenerating(true);
    setActionError(null);
    setActionNotice(null);
    try {
      const [provider, model] = await Promise.all([
        getSetting("provider.selected"),
        getSetting("provider.model"),
      ]);
      if (!provider || !model) {
        setActionError(tr(getLocale(), "vcs.needProviderGenerate"));
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

  const push = useCallback(async () => {
    if (!pushArmed) {
      setPushArmed(true);
      return;
    }
    setPushArmed(false);
    setActing("push");
    setActionError(null);
    setActionNotice(null);
    try {
      await gitPush();
      await loadInfo(true);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [loadInfo, pushArmed]);

  const fetchOrigin = useCallback(async () => {
    setActing("fetch");
    setActionError(null);
    setActionNotice(null);
    try {
      const result = await gitFetch("origin");
      setActionNotice(
        tr(getLocale(), "vcs.fetchDone", { n: result.updated_refs })
      );
      await loadInfo(true);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [loadInfo]);

  const pullOrigin = useCallback(async () => {
    setActing("pull");
    setActionError(null);
    setActionNotice(null);
    try {
      const result = await gitPull("origin");
      const locale = getLocale();
      if (result.status === "conflicts") {
        setActionNotice(
          tr(locale, "vcs.pullConflicts", { n: result.conflicts.length })
        );
      } else if (result.status === "fast_forward") {
        setActionNotice(tr(locale, "vcs.pullFastForward"));
      } else if (result.status === "merged") {
        setActionNotice(tr(locale, "vcs.pullMerged"));
      } else {
        setActionNotice(tr(locale, "vcs.pullUpToDate"));
      }
      await loadInfo(true);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [loadInfo]);

  const createBranch = useCallback(
    async (name: string, switchTo = true): Promise<boolean> => {
      const trimmed = name.trim();
      if (!trimmed) return false;
      setActing("branch-create");
      setActionError(null);
      setActionNotice(null);
      try {
        await gitBranchCreate(trimmed, switchTo);
        await loadInfo(true);
        return true;
      } catch (e) {
        setActionError(toMessage(e));
        return false;
      } finally {
        setActing(null);
      }
    },
    [loadInfo]
  );

  const switchBranch = useCallback(
    async (name: string) => {
      setActing("branch-switch");
      setActionError(null);
      setActionNotice(null);
      setForceDeleteBranch(null);
      try {
        await gitBranchSwitch(name);
        await loadInfo(false);
      } catch (e) {
        setActionError(toMessage(e));
      } finally {
        setActing(null);
      }
    },
    [loadInfo]
  );

  const deleteBranch = useCallback(
    async (name: string, force = false) => {
      setActing("branch-delete");
      setActionError(null);
      setActionNotice(null);
      try {
        await gitBranchDelete(name, force);
        setForceDeleteBranch(null);
        await loadInfo(true);
      } catch (e) {
        const msg = toMessage(e);
        if (!force && msg.toLowerCase().includes("unmerged")) {
          setForceDeleteBranch(name);
        } else {
          setActionError(msg);
        }
      } finally {
        setActing(null);
      }
    },
    [loadInfo]
  );

  const cancelForceDelete = useCallback(() => {
    setForceDeleteBranch(null);
  }, []);

  const stashPush = useCallback(
    async (message?: string): Promise<boolean> => {
      setActing("stash-push");
      setActionError(null);
      setActionNotice(null);
      try {
        await gitStashPush(message?.trim() ? message.trim() : null);
        await loadInfo(true);
        return true;
      } catch (e) {
        setActionError(toMessage(e));
        return false;
      } finally {
        setActing(null);
      }
    },
    [loadInfo]
  );

  const stashPop = useCallback(
    async (index = 0) => {
      setActing(`stash-pop-${index}`);
      setActionError(null);
      setActionNotice(null);
      try {
        const result = await gitStashPop(index);
        const locale = getLocale();
        if (result.status === "conflicts") {
          setActionNotice(
            tr(locale, "vcs.stashPopConflicts", { n: result.conflicts.length })
          );
        } else {
          setActionNotice(tr(locale, "vcs.stashPopApplied"));
        }
        await loadInfo(true);
      } catch (e) {
        setActionError(toMessage(e));
      } finally {
        setActing(null);
      }
    },
    [loadInfo]
  );

  const resolveConflict = useCallback(
    async (path: string, strategy: "ours" | "theirs") => {
      setActing(`resolve:${path}`);
      setActionError(null);
      setActionNotice(null);
      try {
        await gitResolveConflict(path, strategy);
        if (selectedConflictPath === path) {
          setSelectedConflictPath(null);
          setConflictDiff(null);
        }
        await loadInfo(true);
      } catch (e) {
        setActionError(toMessage(e));
      } finally {
        setActing(null);
      }
    },
    [loadInfo, selectedConflictPath]
  );

  const abortMerge = useCallback(async () => {
    if (!abortArmed) {
      setAbortArmed(true);
      return;
    }
    setAbortArmed(false);
    setActing("abort-merge");
    setActionError(null);
    setActionNotice(null);
    try {
      await gitAbortMerge();
      setSelectedConflictPath(null);
      setConflictDiff(null);
      await loadInfo(false);
    } catch (e) {
      setActionError(toMessage(e));
    } finally {
      setActing(null);
    }
  }, [abortArmed, loadInfo]);

  const prepareReview = useCallback(
    async (scope: "working" | "branch", baseBranch?: string | null) => {
      setReviewLoading(true);
      setReviewError(null);
      setReviewCopied(false);
      try {
        const result = await gitPrepareReviewDiff(scope, baseBranch ?? null);
        setReviewDiff(result);
      } catch (e) {
        setReviewError(toMessage(e));
      } finally {
        setReviewLoading(false);
      }
    },
    []
  );

  const copyReviewPrompt = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setReviewCopied(true);
    } catch {
      setReviewCopied(false);
      setReviewError(tr(getLocale(), "common.copyFailed"));
    }
  }, []);

  const explainCommit = useCallback(async (hash: string) => {
    if (explainingRef.current !== null) return;
    explainingRef.current = hash;
    setExplain((prev) => ({
      ...prev,
      loading: true,
      error: null,
      copied: false,
    }));
    try {
      const [provider, model] = await Promise.all([
        getSetting("provider.selected"),
        getSetting("provider.model"),
      ]);
      if (!provider || !model) {
        setExplain((prev) => ({
          ...prev,
          loading: false,
          error: tr(getLocale(), "term.needProviderExplain"),
        }));
        return;
      }
      const result: CommitExplanation = await gitExplainCommit(
        hash,
        provider,
        model
      );
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

  const copyExplanation = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setExplain((prev) => ({ ...prev, copied: true }));
    } catch {
      setExplain((prev) => ({
        ...prev,
        copied: false,
        error: tr(getLocale(), "common.copyFailed"),
      }));
    }
  }, []);

  return {
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
    setCommitMessage,
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
    loadInfo,
    refresh,
    loadDiff,
    loadConflictDiff,
    loadCommitDiff,
    toggleStage,
    commit,
    generate,
    push,
    fetchOrigin,
    pullOrigin,
    createBranch,
    switchBranch,
    deleteBranch,
    cancelForceDelete,
    stashPush,
    stashPop,
    resolveConflict,
    abortMerge,
    prepareReview,
    copyReviewPrompt,
    explainCommit,
    copyExplanation,
    setNotice: setActionNotice,
  };
}
