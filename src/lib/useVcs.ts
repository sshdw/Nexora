//! Version-control data hook (AC-206).
//!
//! Owns the VCS panel's whole git data machine over the existing wrappers:
//! the `git_info` aggregate (with the 20-commit page), the lazy per-file
//! (`git_file_diff`) and per-commit (`git_commit_diff`) diffs, the guarded
//! writes (`git_stage` / `git_unstage` / `git_commit` / `git_push` — each
//! minting its confirmation through the blocking native OS dialog inside the
//! wrapper), and the two AI assists (`git_generate_commit_message` /
//! `git_explain_commit`, provider/model resolved from the persisted settings).
//! Every write refreshes the aggregate afterwards; the manual refresh keeps
//! the reader's selection and reloads its diffs. The component renders store
//! state and keeps pure interaction state (active tab, composer focus,
//! palette-request tokens).

import { useCallback, useEffect, useRef, useState } from "react";

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
  type CommitExplanation,
  type CommandError,
  type GitCommitDiff,
  type GitFileDiff,
  type GitInfo,
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
  loading: boolean;
  error: string | null;
  selectedPath: string | null;
  diff: GitFileDiff | null;
  diffLoading: boolean;
  diffError: string | null;
  selectedCommit: string | null;
  commitDiff: GitCommitDiff | null;
  commitDiffLoading: boolean;
  commitDiffError: string | null;
  /** In-flight write label: the affected path, or `commit` / `push`. */
  acting: string | null;
  actionError: string | null;
  commitMessage: string;
  setCommitMessage: (message: string) => void;
  generating: boolean;
  generatedTruncated: boolean;
  /** Two-press push: first call arms, the second sends. */
  pushArmed: boolean;
  explain: CommitExplainState;
  /** Load the aggregate (mount: `false` resets selections; refresh: `true`). */
  loadInfo: (keepSelection: boolean) => Promise<void>;
  /** Manual refresh: keeps the selection and reloads its diffs. */
  refresh: () => void;
  loadDiff: (path: string) => Promise<void>;
  loadCommitDiff: (hash: string) => Promise<void>;
  toggleStage: (path: string, staged: boolean) => Promise<void>;
  commit: () => Promise<void>;
  generate: () => Promise<void>;
  push: () => Promise<void>;
  explainCommit: (hash: string) => Promise<void>;
  copyExplanation: (text: string) => Promise<void>;
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
  // Synchronous in-flight explain guard: `explain.loading` lands after
  // re-render, so two rapid Explain presses could both pass the state guard
  // and stack two paid AI requests. The ref flips in the same tick (single
  // flight because only the selected commit can be explained).
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
  const refresh = useCallback(() => {
    setPushArmed(false);
    void loadInfo(true).then(() => {
      if (selectedPath) void loadDiff(selectedPath);
      if (selectedCommit) void loadCommitDiff(selectedCommit);
    });
  }, [loadInfo, loadDiff, loadCommitDiff, selectedPath, selectedCommit]);

  const toggleStage = useCallback(
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

  const commit = useCallback(async () => {
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

  const generate = useCallback(async () => {
    setGenerating(true);
    setActionError(null);
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
  const explainCommit = useCallback(async (hash: string) => {
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
          error: tr(getLocale(), "term.needProviderExplain"),
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
    loading,
    error,
    selectedPath,
    diff,
    diffLoading,
    diffError,
    selectedCommit,
    commitDiff,
    commitDiffLoading,
    commitDiffError,
    acting,
    actionError,
    commitMessage,
    setCommitMessage,
    generating,
    generatedTruncated,
    pushArmed,
    explain,
    loadInfo,
    refresh,
    loadDiff,
    loadCommitDiff,
    toggleStage,
    commit,
    generate,
    push,
    explainCommit,
    copyExplanation,
  };
}
