//! Version-control IPC area (AC-205): git inspection, guarded writes, and
//! the two AI assists (commands/version_control.rs).

import { invoke } from "./core";
import {
  OP_GIT_ABORT_MERGE,
  OP_GIT_BRANCH_CREATE,
  OP_GIT_BRANCH_DELETE,
  OP_GIT_BRANCH_DELETE_FORCE,
  OP_GIT_BRANCH_SWITCH,
  OP_GIT_COMMIT,
  OP_GIT_PULL,
  OP_GIT_PUSH,
  OP_GIT_RESOLVE_CONFLICT,
  OP_GIT_STAGE,
  OP_GIT_STASH_POP,
  OP_GIT_STASH_PUSH,
  OP_GIT_UNSTAGE,
  requestConfirmation,
} from "./confirmations";

// ---- Version control (git inspection, guarded writes, timeline) --------
// Git inspection for the opened workspace: branch + changed files + recent
// commits (with per-commit stats and risk signals) in one `git_info` round
// trip, per-file unified diffs via `git_file_diff` (lazy, server-side capped
// with a truncation notice), per-commit diffs via `git_commit_diff` (lazy,
// same cap shape), guarded writes (`git_stage` / `git_unstage` / `git_commit`
// / `git_push`, each with an explicit per-call confirmation), and two AI
// assists through the existing execution path (`git_generate_commit_message`
// for staged changes, `git_explain_commit` for one historical commit — both
// keyring-only, nothing persisted, copy-only results).
// Payloads stay snake_case like every other backend struct; command args are
// camelCase (`limit`, `path`, `hash`, `commitHash`).

/** One changed file: repository-relative path plus a fixed-vocabulary
 * status (`"modified"`, `"staged"`, `"untracked"`, `"deleted"`, `"renamed"`). */
export interface GitFileStatus {
  path: string;
  status: string;
}

/** One recent commit: full hash plus summary, author name, Unix-seconds
 * time, per-commit file statistics, and heuristic risk signals. The message
 * is the commit summary (first line) only. `risk_signals` carries zero or
 * more of the fixed vocabulary `"large-diff"`, `"many-files"`, `"binary"`,
 * `"merge-commit"`, `"unfamiliar-author"` (computed locally, no AI). */
export interface GitCommit {
  hash: string;
  message: string;
  author: string;
  time: number; // seconds since unix epoch
  files_changed: number;
  insertions: number;
  deletions: number;
  risk_signals: string[];
}

/** One conflicted file in the index (`UU`, `AA`, `UD`, `DU`, `DD`). */
export interface GitConflictFile {
  path: string;
  status: string;
}

/** Aggregate read-only git view: current branch (`null` when detached or
 * unborn), changed files sorted by path, and recent commits newest-first.
 * `files` is capped server-side (500, sorted order) with any remainder
 * reported in `files_overflow` (a count only, never content). */
export interface GitInfo {
  branch: string | null;
  files: GitFileStatus[];
  files_overflow: number;
  commits: GitCommit[];
  conflicts?: GitConflictFile[];
  merge_in_progress?: boolean;
}

/** One per-file unified diff, capped server-side (256 KiB) with a truncation
 * notice. Binary content reports `binary` with an empty diff. */
export interface GitFileDiff {
  path: string;
  diff: string;
  truncated: boolean;
  binary: boolean;
}

/** Load the aggregate git view for the effective workspace root via
 * `git_info`. `limit` selects how many recent commits to include (backend
 * clamps to 1..100); the panel passes a small page such as 20. */
export function gitInfo(limit?: number): Promise<GitInfo> {
  return invoke<GitInfo>("git_info", { limit: limit ?? null });
}

/** Load one file's unified diff via `git_file_diff`. `path` is the
 * repository-relative path from `GitInfo.files`; traversal attempts fail
 * with a fixed-vocabulary error. */
export function gitFileDiff(path: string): Promise<GitFileDiff> {
  return invoke<GitFileDiff>("git_file_diff", { path });
}

/** One historical commit's unified diff (`git_commit_diff`): the full hash
 * as resolved, the capped diff text, and the changed repository-relative
 * paths (sorted, capped server-side with the remainder in
 * `files_overflow`). Binary deltas are omitted with a placeholder line;
 * `binary` reports whether any delta was binary. */
export interface GitCommitDiff {
  hash: string;
  diff: string;
  truncated: boolean;
  binary: boolean;
  files: string[];
  files_overflow: number;
}

/** Load one historical commit's unified diff via `git_commit_diff`. `hash`
 * is the full hash from `GitInfo.commits` (prefixes are never guessed);
 * malformed or unknown hashes fail with a fixed-vocabulary error. */
export function gitCommitDiff(hash: string): Promise<GitCommitDiff> {
  return invoke<GitCommitDiff>("git_commit_diff", { hash });
}

/** Stage `paths` (repository-relative) into the index via `git_stage`.
 * Returns how many were staged.
 *
 * SEC-03: the backend consumes a live single-use confirmation id minted for
 * `git_stage` in the `version_control` scope — this wrapper mints it through
 * the blocking native OS dialog first, so cancelling at the dialog rejects
 * and stages nothing (no caller-supplied boolean exists on this path). */
export async function gitStage(paths: string[]): Promise<number> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_STAGE
  );
  return invoke<number>("git_stage", { paths, confirmationId });
}

/** Unstage `paths` (repository-relative) back to `HEAD` via `git_unstage`.
 * Returns how many were unstaged; confirmed like `gitStage` (the id is
 * minted for `git_unstage`, so a stage id cannot authorize it). */
export async function gitUnstage(paths: string[]): Promise<number> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_UNSTAGE
  );
  return invoke<number>("git_unstage", { paths, confirmationId });
}

/** Commit the staged index with `message` via `git_commit`. Returns the new
 * commit hash. The backend validates the message and refuses without the
 * confirmation id this wrapper mints through the native OS dialog (SEC-03,
 * same gate as `gitStage`). */
export async function gitCommit(message: string): Promise<string> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_COMMIT
  );
  return invoke<string>("git_commit", { message, confirmationId });
}

/** Push the current branch to `origin` via `git_push` (never forced; only
 * `origin` is accepted backend-side). Confirmed like `gitStage` — the id is
 * minted for `git_push` specifically. */
export async function gitPush(): Promise<void> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_PUSH
  );
  return invoke<void>("git_push", { remote: "origin", confirmationId });
}

/** AI-generated commit message for the staged changes
 * (`git_generate_commit_message`). `truncated_input` reports whether the
 * staged summary fed to the model was truncated server-side. */
export interface GeneratedCommitMessage {
  message: string;
  truncated_input: boolean;
}

/** Generate a conventional-commit message for the staged changes via
 * `git_generate_commit_message`, using the existing AI execution path
 * (keyring-only credentials, nothing persisted). */
export function gitGenerateCommitMessage(
  provider: string,
  model: string
): Promise<GeneratedCommitMessage> {
  return invoke<GeneratedCommitMessage>("git_generate_commit_message", {
    provider,
    model,
  });
}

/** AI explanation of one historical commit (`git_explain_commit`).
 * `explanation` says what changed and why it matters; it is copy-only text
 * (never auto-applied — the user copies it manually). `truncated_input`
 * reports whether the commit diff fed to the model was truncated
 * server-side. */
export interface CommitExplanation {
  explanation: string;
  truncated_input: boolean;
}

/** Explain one historical commit via `git_explain_commit`, using the
 * existing AI execution path (keyring-only credentials, nothing
 * persisted). `commitHash` is the full hash from `GitInfo.commits`. */
export function gitExplainCommit(
  commitHash: string,
  provider: string,
  model: string
): Promise<CommitExplanation> {
  return invoke<CommitExplanation>("git_explain_commit", {
    commitHash,
    provider,
    model,
  });
}

// ---- Extended Git Workflow, Conflict Resolution & /review --------------

export interface GitFetchResult {
  remote: string;
  updated_refs: number;
}

export interface GitPullResult {
  status: "up_to_date" | "fast_forward" | "merged" | "conflicts" | string;
  commit_hash: string | null;
  conflicts: GitConflictFile[];
}

export interface GitBranch {
  name: string;
  is_current: boolean;
  upstream: string | null;
  last_commit_hash: string | null;
}

export interface GitStashEntry {
  index: number;
  message: string;
  hash: string;
}

export interface GitStashPopResult {
  status: "applied" | "conflicts" | string;
  conflicts: GitConflictFile[];
}

export interface GitConflictDiff {
  path: string;
  status: string;
  ours: string;
  theirs: string;
  working: string;
  truncated: boolean;
  binary: boolean;
}

export interface GitReviewDiff {
  scope: "working" | "branch" | string;
  base_branch: string | null;
  files: string[];
  files_overflow: number;
  diff: string;
  truncated: boolean;
  redacted: boolean;
  prompt: string;
}

/** Fetch remote-tracking refs from `origin`. */
export function gitFetch(remote = "origin"): Promise<GitFetchResult> {
  return invoke<GitFetchResult>("git_fetch", { remote });
}

/** Pull the current branch from `origin`. Reports conflicts cleanly without
 * throwing. Confirmed via `git_pull`. */
export async function gitPull(remote = "origin"): Promise<GitPullResult> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_PULL
  );
  return invoke<GitPullResult>("git_pull", { remote, confirmationId });
}

/** List local branches in the workspace repository. */
export function gitBranches(): Promise<GitBranch[]> {
  return invoke<GitBranch[]>("git_branches");
}

/** Create a new local branch at `HEAD`, optionally switching to it. */
export async function gitBranchCreate(
  name: string,
  switchTo = true
): Promise<GitBranch> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_BRANCH_CREATE
  );
  return invoke<GitBranch>("git_branch_create", {
    name,
    switch: switchTo,
    confirmationId,
  });
}

/** Switch to an existing local branch. */
export async function gitBranchSwitch(name: string): Promise<void> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_BRANCH_SWITCH
  );
  return invoke<void>("git_branch_switch", { name, confirmationId });
}

/** Delete a local branch. Force-deleting an unmerged branch mints
 * `git_branch_delete_force`. */
export async function gitBranchDelete(
  name: string,
  force = false
): Promise<void> {
  const confirmationId = await requestConfirmation(
    "version_control",
    force ? OP_GIT_BRANCH_DELETE_FORCE : OP_GIT_BRANCH_DELETE
  );
  return invoke<void>("git_branch_delete", { name, force, confirmationId });
}

/** List stash entries (newest first, index 0 = top). */
export function gitStashList(): Promise<GitStashEntry[]> {
  return invoke<GitStashEntry[]>("git_stash_list");
}

/** Save local changes onto the stash stack. */
export async function gitStashPush(
  message?: string | null
): Promise<GitStashEntry> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_STASH_PUSH
  );
  return invoke<GitStashEntry>("git_stash_push", {
    message: message ?? null,
    confirmationId,
  });
}

/** Pop a stash entry by zero-based index (default 0). */
export async function gitStashPop(index = 0): Promise<GitStashPopResult> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_STASH_POP
  );
  return invoke<GitStashPopResult>("git_stash_pop", { index, confirmationId });
}

/** Load conflict preview (`ours`, `theirs`, and `working`) for one conflicted
 * path. */
export function gitConflictDiff(path: string): Promise<GitConflictDiff> {
  return invoke<GitConflictDiff>("git_conflict_diff", { path });
}

/** Resolve one conflicted file via `"ours"` or `"theirs"` and stage the
 * resolved result. Returns the remaining conflict count. */
export async function gitResolveConflict(
  path: string,
  strategy: "ours" | "theirs"
): Promise<number> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_RESOLVE_CONFLICT
  );
  return invoke<number>("git_resolve_conflict", {
    path,
    strategy,
    confirmationId,
  });
}

/** Safely abort an in-progress merge or pull. */
export async function gitAbortMerge(): Promise<void> {
  const confirmationId = await requestConfirmation(
    "version_control",
    OP_GIT_ABORT_MERGE
  );
  return invoke<void>("git_abort_merge", { confirmationId });
}

/** Prepare a secret-redacted diff and `/review` prompt for `"working"` or
 * `"branch"` scope. */
export function gitPrepareReviewDiff(
  scope: "working" | "branch" = "working",
  baseBranch?: string | null
): Promise<GitReviewDiff> {
  return invoke<GitReviewDiff>("git_prepare_review_diff", {
    scope,
    baseBranch: baseBranch ?? null,
  });
}
