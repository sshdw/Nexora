//! Git inspection plus guarded write operations for the opened workspace
//! (application layer).
//!
//! This module owns the git path behind the thin `git_info` / `git_file_diff`
//! IPC commands (one feature area, minimal surface) plus the write commands
//! `git_stage` / `git_unstage` / `git_commit` / `git_push` and the
//! AI-generated commit message (`git_generate_commit_message`):
//!
//! - Reads: status (changed/staged/untracked files), recent commits, and
//!   per-file unified diffs. Everything read-only is unchanged from the base.
//! - Writes: staging, unstaging, committing (author = local git config only),
//!   and pushing to the preconfigured `origin` remote. There is no pull/fetch,
//!   branch, merge, rebase, stash, or amend-of-others path here.
//! - Everything is scoped to the enclosing repository of the canonical
//!   workspace root, with the same guards as the read path (lexical path
//!   validation, workspace-prefix check, symlink-ancestor backstop,
//!   fixed-vocabulary [`VersionControlError`]).
//! - Every write requires an explicit per-call `confirmed` flag and refuses
//!   with [`VersionControlError::Unconfirmed`] without it. Gate-reuse note:
//!   the agent [`ApprovalGate`](crate::application::agent::approval::ApprovalGate)
//!   governs in-flight agent tool calls (it parks a live run until a user
//!   resolves it); a direct IPC command has no run, no park, and no autonomy
//!   mode to consult, so the gate object cannot apply. Direct IPC therefore
//!   reuses the established destructive-action pattern instead — the explicit
//!   confirmation gate from data management (`confirmed: true` per call,
//!   surfacing as [`CommandError`](crate::commands::error::CommandError)
//!   `ConfirmationRequired`) — rather than inventing a parallel mechanism.
//! - Push is never forced: the push path builds a plain
//!   `refs/heads/<branch>:refs/heads/<branch>` refspec with no `+` prefix and
//!   no `--force` flag exists anywhere in this module (a test asserts the
//!   source contains no force flag). Only the fixed allowlist remote
//!   (`origin`) is accepted; arbitrary remotes/URLs are refused with
//!   [`VersionControlError::InvalidRemote`].
//! - Failures are secret-free: [`VersionControlError`] carries no payload, so
//!   formatting it can never leak diff content (which may contain user
//!   secrets), credentials, SQL, or file content.
//! - Diffs are capped at [`MAX_DIFF_BYTES`] (256 KiB) with a `truncated` flag;
//!   binary content is detected by a null byte and reported as `binary` with
//!   an empty diff rather than guessed as text.
//!
//! There is no file watching or live refresh: the frontend reloads explicitly
//! (manual Refresh only).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::application::agent::tools::is_within_workspace;
use crate::application::execution::{AiMessage, AiRequest, AiRole, RequestError};
use crate::application::workspace::strip_verbatim;
use crate::infrastructure::database::Database;

use super::agent::runner::DEFAULT_REQUEST_TIMEOUT;

/// Largest per-file diff returned before it is truncated with a notice.
pub(crate) const MAX_DIFF_BYTES: usize = 256 * 1024;

/// Upper bound for the log `limit` argument (clamped, never an error).
pub(crate) const MAX_LOG_ENTRIES: u32 = 100;

/// Default number of commits returned when the caller passes no limit.
pub(crate) const DEFAULT_LOG_ENTRIES: u32 = 20;

/// Largest changed-file list returned by `git_info` before it is capped with
/// an overflow count. Untracked dumps (an un-ignored `node_modules`, a
/// vendored tree) can otherwise make the list slow and unbounded.
pub(crate) const MAX_STATUS_FILES: usize = 500;

/// Longest diff `path` argument accepted (matches the workspace-root bound).
const MAX_PATH_LEN: usize = 1024;

/// Longest commit message accepted, in characters (subject plus body).
pub(crate) const MAX_COMMIT_MESSAGE_LEN: usize = 500;

/// Longest commit subject (first line) accepted, in characters.
pub(crate) const MAX_COMMIT_SUBJECT_LEN: usize = 100;

/// Most paths accepted in one stage/unstage batch.
pub(crate) const MAX_STAGE_PATHS: usize = 500;

/// Cap for the staged-diff summary fed to the commit-message prompt.
pub(crate) const MAX_COMMIT_PROMPT_BYTES: usize = 64 * 1024;

/// The only remote [`git_push`] may target. The command takes the remote name
/// and validates it against this fixed allowlist: arbitrary remotes and URLs
/// can never reach the push path.
pub(crate) const ALLOWED_PUSH_REMOTE: &str = "origin";

/// Fixed-vocabulary commit types accepted by [`is_conventional_message`].
const COMMIT_TYPES: [&str; 11] = [
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

/// Secret-free failures for git inspection and guarded writes.
///
/// Every variant renders as fixed category text: formatting a
/// [`VersionControlError`] can never leak diff content, a credential, SQL, or
/// a stored payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VersionControlError {
    /// The workspace is not inside a git repository (or it is bare).
    NotARepository,
    /// A caller-supplied file path escaped the repository workdir or was
    /// otherwise unusable.
    InvalidPath,
    /// A write was invoked without the explicit per-call confirmation flag.
    Unconfirmed,
    /// A commit message was empty, overlong, or had an unusable subject line.
    InvalidMessage,
    /// A push named a remote outside the fixed allowlist.
    InvalidRemote,
    /// A git operation failed (status, log, diff read, or write).
    GitFailed,
}

impl std::fmt::Display for VersionControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository => {
                write!(f, "the workspace is not inside a git repository")
            }
            Self::InvalidPath => write!(f, "the file path is invalid"),
            Self::Unconfirmed => write!(
                f,
                "explicit confirmation is required before this git write can run"
            ),
            Self::InvalidMessage => write!(f, "the commit message is invalid"),
            Self::InvalidRemote => write!(f, "the git remote is not allowed"),
            Self::GitFailed => write!(f, "the git operation failed"),
        }
    }
}

impl std::error::Error for VersionControlError {}

/// One changed file: its repository-relative path plus a fixed-vocabulary
/// status (`"modified"`, `"staged"`, `"untracked"`, `"deleted"`, `"renamed"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFileStatus {
    /// Repository-relative forward-slash path.
    pub path: String,
    /// Fixed-vocabulary change kind (never backend internals).
    pub status: String,
}

/// One recent commit: full hash plus summary, author name, and Unix-seconds
/// time. The message is the commit summary (first line) only, so payloads
/// stay small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitCommit {
    /// Full commit hash (hex); the frontend shortens it for display.
    pub hash: String,
    /// Commit summary (first line; empty when the commit has none).
    pub message: String,
    /// Commit author name (empty when unrepresentable).
    pub author: String,
    /// Commit time, seconds since the Unix epoch.
    pub time: i64,
}

/// Aggregate read-only view backing the version-control panel: the current
/// branch (absent when detached or unborn), the changed-file list, and the
/// recent commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitInfo {
    /// Current branch short name, or [`None`] when detached/unborn.
    pub branch: Option<String>,
    /// Changed files, sorted by path, capped at [`MAX_STATUS_FILES`].
    pub files: Vec<GitFileStatus>,
    /// Number of changed files omitted beyond [`MAX_STATUS_FILES`] (a count
    /// only — never file content, so it stays secret-free).
    pub files_overflow: usize,
    /// Recent commits, newest first.
    pub commits: Vec<GitCommit>,
}

/// One per-file unified diff, capped at [`MAX_DIFF_BYTES`] with a truncation
/// notice. Binary content reports `binary` with an empty diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFileDiff {
    /// Repository-relative forward-slash path, as requested.
    pub path: String,
    /// Unified diff text (empty when the file has no changes or is binary).
    pub diff: String,
    /// Whether `diff` was cut at [`MAX_DIFF_BYTES`].
    pub truncated: bool,
    /// Whether the file content is binary (null byte detected).
    pub binary: bool,
}

/// Classify a [`git2::Status`] bitset into the fixed status vocabulary.
///
/// Priority is worktree-first: an untracked entry is always `"untracked"`,
/// worktree modifications win over staged ones, and anything else staged
/// reads `"staged"`. Conflicted entries fall through to `"modified"`.
fn classify_status(status: git2::Status) -> &'static str {
    if status.is_wt_new() {
        "untracked"
    } else if status.is_wt_deleted() {
        "deleted"
    } else if status.is_wt_renamed() {
        "renamed"
    } else if status.is_wt_modified() || status.is_wt_typechange() {
        "modified"
    } else if status.is_index_new()
        || status.is_index_modified()
        || status.is_index_deleted()
        || status.is_index_renamed()
        || status.is_index_typechange()
    {
        "staged"
    } else {
        "modified"
    }
}

/// Canonicalize `workspace_root` (it must exist and be a directory) and open
/// the enclosing repository.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] when the root is unusable or a
/// git operation fails, [`VersionControlError::NotARepository`] when no
/// enclosing repository exists or it is bare.
fn open_workspace_repo(
    workspace_root: &Path,
) -> Result<(PathBuf, git2::Repository), VersionControlError> {
    let canon_ws = std::fs::canonicalize(workspace_root)
        .map(strip_verbatim)
        .map_err(|_| VersionControlError::GitFailed)?;
    if !canon_ws.is_dir() {
        return Err(VersionControlError::GitFailed);
    }
    let repo =
        git2::Repository::discover(&canon_ws).map_err(|_| VersionControlError::NotARepository)?;
    if repo.workdir().is_none() {
        return Err(VersionControlError::NotARepository);
    }
    Ok((canon_ws, repo))
}

/// Canonical repository workdir for path scoping.
///
/// # Errors
///
/// Returns [`VersionControlError::NotARepository`] for a bare repository,
/// [`VersionControlError::GitFailed`] when the workdir cannot be resolved.
fn canonical_workdir(repo: &git2::Repository) -> Result<PathBuf, VersionControlError> {
    let workdir = repo.workdir().ok_or(VersionControlError::NotARepository)?;
    std::fs::canonicalize(workdir)
        .map(strip_verbatim)
        .map_err(|_| VersionControlError::GitFailed)
}

/// Current branch short name, or [`None`] when detached or unborn.
fn current_branch(repo: &git2::Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if head.is_branch() {
        head.shorthand().ok().map(str::to_string)
    } else {
        None
    }
}

/// Read the changed-file list (staged, worktree, and untracked), sorted by
/// path and capped at [`MAX_STATUS_FILES`] with an overflow count.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] when the status read fails.
fn read_status(
    repo: &git2::Repository,
) -> Result<(Vec<GitFileStatus>, usize), VersionControlError> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut files: Vec<GitFileStatus> = statuses
        .iter()
        .filter_map(|entry| {
            let path = String::from_utf8_lossy(entry.path_bytes()).into_owned();
            if path.is_empty() {
                return None;
            }
            Some(GitFileStatus {
                path,
                status: classify_status(entry.status()).to_string(),
            })
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let overflow = files.len().saturating_sub(MAX_STATUS_FILES);
    files.truncate(MAX_STATUS_FILES);
    Ok((files, overflow))
}

/// Read up to `limit` recent commits from `HEAD`, newest first. A repository
/// with no commits yet (unborn `HEAD`) yields an empty list, not an error.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] when the log walk fails.
fn read_log(repo: &git2::Repository, limit: usize) -> Result<Vec<GitCommit>, VersionControlError> {
    let mut walk = repo.revwalk().map_err(|_| VersionControlError::GitFailed)?;
    walk.set_sorting(git2::Sort::TIME)
        .map_err(|_| VersionControlError::GitFailed)?;
    if walk.push_head().is_err() {
        return Ok(Vec::new());
    }
    let mut commits = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid.map_err(|_| VersionControlError::GitFailed)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|_| VersionControlError::GitFailed)?;
        commits.push(GitCommit {
            hash: oid.to_string(),
            message: commit
                .summary_bytes()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default(),
            author: String::from_utf8_lossy(commit.author().name_bytes()).into_owned(),
            time: commit.time().seconds(),
        });
    }
    Ok(commits)
}

/// Aggregate read-only git view for `workspace_root`: branch, changed files,
/// and the `limit` most recent commits (`limit` clamps to `1..=MAX_LOG_ENTRIES`,
/// defaulting to [`DEFAULT_LOG_ENTRIES`] when [`None`]).
///
/// # Errors
///
/// See [`open_workspace_repo`], [`read_status`], and [`read_log`].
pub(crate) fn git_info(
    workspace_root: &Path,
    limit: Option<u32>,
) -> Result<GitInfo, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let count = limit
        .unwrap_or(DEFAULT_LOG_ENTRIES)
        .clamp(1, MAX_LOG_ENTRIES) as usize;
    let (files, files_overflow) = read_status(&repo)?;
    Ok(GitInfo {
        branch: current_branch(&repo),
        files,
        files_overflow,
        commits: read_log(&repo, count)?,
    })
}

/// Validate a caller-supplied repository-relative path.
/// Rejects absolute paths (both separators, including driveSmoke and UNC
/// forms), parent references, `.git` components, null bytes, empty input,
/// and overlong input — all with the single fixed-vocabulary
/// [`VersionControlError::InvalidPath`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidPath`] for any rejected shape.
fn clean_repo_path(raw: &str) -> Result<PathBuf, VersionControlError> {
    if raw.contains('\0') {
        return Err(VersionControlError::InvalidPath);
    }
    let normalized = raw.trim().replace('\\', "/");
    if normalized.is_empty() || normalized.len() > MAX_PATH_LEN {
        return Err(VersionControlError::InvalidPath);
    }
    if normalized.starts_with('/') {
        return Err(VersionControlError::InvalidPath);
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(VersionControlError::InvalidPath);
    }
    let mut rel = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(_) => rel.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VersionControlError::InvalidPath);
            }
        }
    }
    if rel.as_os_str().is_empty() {
        return Err(VersionControlError::InvalidPath);
    }
    if rel
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(VersionControlError::InvalidPath);
    }
    Ok(rel)
}

/// Backstop for symlinks anywhere along a repository-relative path
/// (including symlinked intermediate directories): resolve the nearest
/// existing ancestor through the filesystem, re-attach the unresolved tail,
/// and re-check containment. A request like `link/secret` where `link`
/// escapes the workdir refuses even though the lexical check passed.
fn assert_within_repo(canon_repo: &Path, rel: &Path) -> Result<(), VersionControlError> {
    let joined = canon_repo.join(rel);
    if !is_within_workspace(canon_repo, &joined) {
        return Err(VersionControlError::InvalidPath);
    }
    let mut ancestor: &Path = &joined;
    loop {
        if std::fs::symlink_metadata(ancestor).is_ok() {
            let canon = ancestor
                .canonicalize()
                .map(strip_verbatim)
                .map_err(|_| VersionControlError::InvalidPath)?;
            let tail = joined
                .strip_prefix(ancestor)
                .map_err(|_| VersionControlError::InvalidPath)?;
            let resolved = canon.join(tail);
            if !is_within_workspace(canon_repo, &resolved) {
                return Err(VersionControlError::InvalidPath);
            }
            return Ok(());
        }
        match ancestor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => ancestor = parent,
            _ => return Ok(()),
        }
    }
}

/// Per-file unified diff for `raw_path` (repository-relative), capped at
/// [`MAX_DIFF_BYTES`] with `truncated` set, against `HEAD` (staged plus
/// unstaged, untracked included). A file with no changes yields an empty
/// diff; binary content yields `binary` with an empty diff.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidPath`] when the path escapes the
/// repository workdir or is otherwise unusable; see [`open_workspace_repo`]
/// for the remaining failures.
pub(crate) fn git_file_diff(
    workspace_root: &Path,
    raw_path: &str,
) -> Result<GitFileDiff, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rel = clean_repo_path(raw_path)?;
    assert_within_repo(&canon_repo, &rel)?;
    let spec = rel.to_string_lossy().replace('\\', "/");
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let mut opts = git2::DiffOptions::new();
    opts.pathspec(&spec)
        .context_lines(3)
        .include_untracked(true)
        .recurse_untracked_dirs(true);
    let diff = repo
        .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut text = String::new();
    let mut binary = false;
    // `Deltas` walks `0..count` in order, so the enumerated index is the
    // delta index `Patch::from_diff` needs.
    for (index, delta) in diff.deltas().enumerate() {
        let matches = delta
            .old_file()
            .path_bytes()
            .is_some_and(|bytes| bytes == spec.as_bytes())
            || delta
                .new_file()
                .path_bytes()
                .is_some_and(|bytes| bytes == spec.as_bytes());
        if !matches {
            continue;
        }
        // `from_diff` yields [`None`] for unchanged or binary files. The
        // pathspec limits the diff to this file and a delta exists for it,
        // so [`None`] here means binary content (unmodified deltas are never
        // emitted without opting into them).
        let Some(mut patch) =
            git2::Patch::from_diff(&diff, index).map_err(|_| VersionControlError::GitFailed)?
        else {
            binary = true;
            text.clear();
            break;
        };
        let buf = patch.to_buf().map_err(|_| VersionControlError::GitFailed)?;
        let bytes: &[u8] = &buf;
        if bytes.contains(&0) {
            binary = true;
            text.clear();
            break;
        }
        text.push_str(&String::from_utf8_lossy(bytes));
    }
    if binary {
        return Ok(GitFileDiff {
            path: spec,
            diff: String::new(),
            truncated: false,
            binary: true,
        });
    }
    let truncated = text.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(GitFileDiff {
        path: spec,
        diff: text,
        truncated,
        binary: false,
    })
}

// ---------------------------------------------------------------------------
// Guarded write operations: stage / unstage / commit / push
// ---------------------------------------------------------------------------

/// Require the explicit per-call confirmation flag for a write. Direct IPC
/// commands cannot park on the agent [`ApprovalGate`](crate::application::agent::approval::ApprovalGate)
/// (no run, no park, no autonomy mode), so writes reuse the destructive-action
/// confirmation pattern: `confirmed: false` refuses without touching git.
fn require_confirmed(confirmed: bool) -> Result<(), VersionControlError> {
    if confirmed {
        Ok(())
    } else {
        Err(VersionControlError::Unconfirmed)
    }
}

/// Validate a stage/unstage batch into repository-relative paths: non-empty,
/// bounded at [`MAX_STAGE_PATHS`], each lexically clean.
fn clean_stage_paths(raw_paths: &[String]) -> Result<Vec<PathBuf>, VersionControlError> {
    if raw_paths.is_empty() || raw_paths.len() > MAX_STAGE_PATHS {
        return Err(VersionControlError::InvalidPath);
    }
    raw_paths.iter().map(|raw| clean_repo_path(raw)).collect()
}

/// Stage `raw_paths` (repository-relative) into the index.
///
/// Returns the number of paths staged. Requires `confirmed`; refuses empty
/// and overlong batches and any path that escapes the repository workdir
/// (same guards as the read path).
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] without confirmation,
/// [`VersionControlError::InvalidPath`] for a bad batch or an escaping path;
/// see [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_stage(
    workspace_root: &Path,
    raw_paths: &[String],
    confirmed: bool,
) -> Result<usize, VersionControlError> {
    require_confirmed(confirmed)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rels = clean_stage_paths(raw_paths)?;
    for rel in &rels {
        assert_within_repo(&canon_repo, rel)?;
    }
    let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    index
        .add_all(rels.iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|_| VersionControlError::GitFailed)?;
    index.write().map_err(|_| VersionControlError::GitFailed)?;
    Ok(rels.len())
}

/// Unstage `raw_paths` (repository-relative): reset the index entries to
/// `HEAD` (or drop them when `HEAD` is unborn).
///
/// Returns the number of paths unstaged. Requires `confirmed`; same path
/// guards as [`git_stage`]. Unstaging a path with no staged change is a
/// successful no-op.
///
/// # Errors
///
/// See [`git_stage`].
pub(crate) fn git_unstage(
    workspace_root: &Path,
    raw_paths: &[String],
    confirmed: bool,
) -> Result<usize, VersionControlError> {
    require_confirmed(confirmed)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rels = clean_stage_paths(raw_paths)?;
    for rel in &rels {
        assert_within_repo(&canon_repo, rel)?;
    }
    if let Some(head) = repo.head().ok().and_then(|head| head.peel_to_commit().ok()) {
        let object = head.into_object();
        repo.reset_default(Some(&object), rels.iter())
            .map_err(|_| VersionControlError::GitFailed)?;
    } else {
        // Unborn `HEAD`: unstage by dropping the index entries.
        let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
        index
            .remove_all(rels.iter(), None)
            .map_err(|_| VersionControlError::GitFailed)?;
        index.write().map_err(|_| VersionControlError::GitFailed)?;
    }
    Ok(rels.len())
}

/// Validate a caller-supplied commit message: non-empty after trimming,
/// bounded at [`MAX_COMMIT_MESSAGE_LEN`] characters, with a non-empty subject
/// (first line) bounded at [`MAX_COMMIT_SUBJECT_LEN`] characters. Returns the
/// trimmed message.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidMessage`] for any rejected shape.
pub(crate) fn validate_commit_message(raw: &str) -> Result<String, VersionControlError> {
    if raw.contains('\0') {
        return Err(VersionControlError::InvalidMessage);
    }
    let message = raw.trim().replace("\r\n", "\n");
    let message = message.trim().to_string();
    if message.is_empty() {
        return Err(VersionControlError::InvalidMessage);
    }
    if message.chars().count() > MAX_COMMIT_MESSAGE_LEN {
        return Err(VersionControlError::InvalidMessage);
    }
    let subject = message.lines().next().unwrap_or_default();
    if subject.trim().is_empty() {
        return Err(VersionControlError::InvalidMessage);
    }
    if subject.chars().count() > MAX_COMMIT_SUBJECT_LEN {
        return Err(VersionControlError::InvalidMessage);
    }
    Ok(message)
}

/// Whether `message` opens with a conventional-commit subject:
/// `type(scope): subject` or `type: subject` with a fixed-vocabulary type, a
/// non-empty parenthesized scope when present, and a non-empty subject after
/// `": "`.
#[must_use]
pub(crate) fn is_conventional_message(message: &str) -> bool {
    let subject = message.lines().next().unwrap_or_default();
    let Some((head, rest)) = subject.split_once(':') else {
        return false;
    };
    if !rest.starts_with(' ') || rest.trim().is_empty() {
        return false;
    }
    let (commit_type, scope) = match head.split_once('(') {
        Some((commit_type, scope)) => {
            if !scope.ends_with(')') || scope.len() < 3 {
                return false;
            }
            let scope = &scope[..scope.len() - 1];
            if scope.trim().is_empty() || scope.chars().any(char::is_whitespace) {
                return false;
            }
            (commit_type, Some(scope))
        }
        None => (head, None),
    };
    if !COMMIT_TYPES.contains(&commit_type) {
        return false;
    }
    let _ = scope;
    !commit_type.is_empty()
}

/// Commit the staged index with `raw_message`, returning the new commit hash.
///
/// The author comes from the local git config only
/// ([`git2::Repository::signature`]): this path never amends another author.
/// Requires `confirmed`; empty commits (a tree identical to `HEAD`, or
/// nothing staged on an unborn `HEAD`) refuse with
/// [`VersionControlError::GitFailed`].
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] without confirmation,
/// [`VersionControlError::InvalidMessage`] for a bad message; see
/// [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_commit(
    workspace_root: &Path,
    raw_message: &str,
    confirmed: bool,
) -> Result<String, VersionControlError> {
    require_confirmed(confirmed)?;
    let message = validate_commit_message(raw_message)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let tree_id = index
        .write_tree()
        .map_err(|_| VersionControlError::GitFailed)?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|_| VersionControlError::GitFailed)?;
    let head: Option<git2::Commit<'_>> =
        repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    match &head {
        Some(commit) if commit.tree_id() == tree_id => {
            return Err(VersionControlError::GitFailed);
        }
        None if tree.is_empty() => return Err(VersionControlError::GitFailed),
        _ => {}
    }
    let parents: Vec<&git2::Commit<'_>> = head.iter().collect();
    let signature = repo
        .signature()
        .map_err(|_| VersionControlError::GitFailed)?;
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &parents,
        )
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(oid.to_string())
}

/// Push the current branch to `remote_name`, which must equal
/// [`ALLOWED_PUSH_REMOTE`] (`origin`): arbitrary remotes and URLs are refused
/// before any repository is touched.
///
/// The push is never forced: the refspec is a plain
/// `refs/heads/<branch>:refs/heads/<branch>` update with no `+` prefix, and
/// no force flag exists anywhere in this module. Detached or unborn `HEAD`
/// refuses (there is no branch to push). Authentication reuses the user's
/// existing git credential setup; no new credential input exists on this
/// path.
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] without confirmation,
/// [`VersionControlError::InvalidRemote`] for a non-allowlisted remote; see
/// [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_push(
    workspace_root: &Path,
    remote_name: &str,
    confirmed: bool,
) -> Result<(), VersionControlError> {
    require_confirmed(confirmed)?;
    if remote_name != ALLOWED_PUSH_REMOTE {
        return Err(VersionControlError::InvalidRemote);
    }
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let branch = current_branch(&repo).ok_or(VersionControlError::GitFailed)?;
    let mut remote = repo
        .find_remote(ALLOWED_PUSH_REMOTE)
        .map_err(|_| VersionControlError::GitFailed)?;
    // Plain fast-forward update only: no `+` prefix, no force flag.
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], None)
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// AI-generated conventional-commit message
// ---------------------------------------------------------------------------

/// Narrow prompt turning a staged-diff summary into a conventional-commit
/// message. The model must reply with only the message: a
/// `type(scope): subject` first line plus an optional body.
pub(crate) fn build_commit_prompt(summary: &str, truncated: bool) -> String {
    let mut prompt = String::from(
        "Write a conventional commit message for the staged git changes summarized below.\n\
         Reply with ONLY the commit message, no code fences, no explanation.\n\
         The first line must be `type(scope): subject` where type is one of \
         feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert; \
         scope is a short lowercase area name; subject is an imperative short summary \
         under 72 characters. After a blank line, add 1-3 short body lines describing \
         what changed and why.\n",
    );
    if truncated {
        prompt.push_str(
            "Note: the change summary was truncated to fit; describe only what is shown.\n",
        );
    }
    prompt.push_str("Staged changes:\n");
    prompt.push_str(summary);
    prompt
}

/// Coerce raw model output into a valid conventional-commit message: strip
/// code fences, take the first conventional subject line as the subject plus
/// the following body lines, and fall back to a `chore(workspace):` subject
/// when no conventional line is present. The result always satisfies
/// [`validate_commit_message`] and [`is_conventional_message`].
pub(crate) fn sanitize_ai_message(raw: &str) -> String {
    let stripped = raw.trim().replace("\r\n", "\n");
    let all: Vec<&str> = stripped
        .lines()
        .map(str::trim_end)
        .filter(|line| {
            let line = line.trim();
            line != "```" && !line.starts_with("```")
        })
        .collect();
    // Keep interior blank lines (subject/body separator) while trimming
    // leading and trailing empties.
    let start = all
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(all.len());
    let end = all
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let lines = if start < end { &all[start..end] } else { &[] };
    let conventional_at = lines.iter().position(|line| is_conventional_message(line));
    let mut message = if let Some(index) = conventional_at {
        let mut out = vec![lines[index].trim().to_string()];
        for line in lines.iter().skip(index + 1) {
            out.push((*line).to_string());
        }
        out.join("\n").trim().to_string()
    } else {
        let first = lines
            .iter()
            .find(|line| !line.trim().is_empty())
            .map_or("", |line| *line)
            .trim();
        let subject: String = first.chars().take(72).collect();
        let subject = subject.trim();
        if subject.is_empty() {
            "chore(workspace): update files".to_string()
        } else {
            format!("chore(workspace): {subject}")
        }
    };
    // Enforce the backend bounds so the sanitized message always validates.
    if message.chars().count() > MAX_COMMIT_MESSAGE_LEN {
        let mut end = MAX_COMMIT_MESSAGE_LEN;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message = message.trim_end().to_string();
    }
    let mut split: Vec<String> = message.split('\n').map(str::to_string).collect();
    if let Some(subject) = split.first() {
        if subject.chars().count() > MAX_COMMIT_SUBJECT_LEN {
            let mut end = MAX_COMMIT_SUBJECT_LEN;
            while !subject.is_char_boundary(end) {
                end -= 1;
            }
            split[0] = subject[..end].trim_end().to_string();
            // Truncating the subject could only break conventional shape by
            // cutting the scope/type; re-check and fall back if needed.
            let rejoined = split.join("\n");
            if is_conventional_message(&rejoined) {
                return rejoined;
            }
            let fallback_subject: String = rejoined
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(72)
                .collect();
            return format!("chore(workspace): {}", fallback_subject.trim());
        }
    }
    debug_assert!(validate_commit_message(&message).is_ok());
    debug_assert!(is_conventional_message(&message));
    message
}

/// AI-generated commit message plus whether the staged summary was truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeneratedCommitMessage {
    /// Sanitized conventional-commit message (subject plus optional body).
    pub message: String,
    /// Whether the staged summary fed to the model was truncated at
    /// [`MAX_COMMIT_PROMPT_BYTES`].
    pub truncated_input: bool,
}

/// Failures for AI commit-message generation: workspace/git problems or the
/// shared AI execution failure. Both sides stay secret-free.
#[derive(Debug)]
pub(crate) enum CommitMessageError {
    /// The staged summary could not be read (not a repository, git failure).
    VersionControl(VersionControlError),
    /// The AI request failed (unknown provider, missing credentials,
    /// provider failure). Carries no prompt or diff content.
    Request(RequestError),
}

impl std::fmt::Display for CommitMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionControl(inner) => write!(f, "{inner}"),
            Self::Request(_) => write!(f, "the commit message could not be generated"),
        }
    }
}

impl std::error::Error for CommitMessageError {}

impl From<VersionControlError> for CommitMessageError {
    fn from(err: VersionControlError) -> Self {
        Self::VersionControl(err)
    }
}

impl From<RequestError> for CommitMessageError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// Summarize the staged (index vs `HEAD`) diff, capped at
/// [`MAX_COMMIT_PROMPT_BYTES`] with a truncation flag. An empty summary means
/// nothing is staged.
///
/// # Errors
///
/// See [`open_workspace_repo`].
fn staged_diff_summary(workspace_root: &Path) -> Result<(String, bool), VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut text = String::new();
    // `Deltas` walks `0..count` in order, so the enumerated index is the
    // delta index `Patch::from_diff` needs.
    for (delta_index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path_bytes()
            .or_else(|| delta.old_file().path_bytes())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        text.push_str("file: ");
        text.push_str(&path);
        text.push('\n');
        if let Some(mut patch) = git2::Patch::from_diff(&diff, delta_index)
            .map_err(|_| VersionControlError::GitFailed)?
        {
            let buf = patch.to_buf().map_err(|_| VersionControlError::GitFailed)?;
            let bytes: &[u8] = &buf;
            if bytes.contains(&0) {
                text.push_str("(binary file, content omitted)\n");
            } else {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        text.push('\n');
        if text.len() >= MAX_COMMIT_PROMPT_BYTES {
            break;
        }
    }
    let truncated = text.len() > MAX_COMMIT_PROMPT_BYTES;
    if truncated {
        let mut end = MAX_COMMIT_PROMPT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok((text, truncated))
}

/// Generate a conventional-commit message for the staged changes through the
/// existing AI execution path: the staged diff summary (capped at
/// [`MAX_COMMIT_PROMPT_BYTES`]) feeds a narrow prompt to a single text-only
/// [`AiRequest`] executed by the shared
/// [`RequestExecutionService`](crate::application::execution::RequestExecutionService)
/// — the same execution boundary `send_message` and the agent-run bridge use.
/// Credentials resolve from the OS keyring only; no new key input exists on
/// this path, and nothing is persisted (no conversation message is created).
/// The model output is coerced through [`sanitize_ai_message`], so the
/// returned message always validates.
///
/// # Errors
///
/// Returns [`CommitMessageError::VersionControl`] when the workspace is not a
/// repository or nothing is staged, [`CommitMessageError::Request`] when AI
/// execution fails.
pub(crate) fn generate_commit_message(
    db: &Database,
    workspace_root: &Path,
    provider: &str,
    model: &str,
) -> Result<GeneratedCommitMessage, CommitMessageError> {
    use crate::application::execution::RequestExecutionService;
    let (summary, truncated) = staged_diff_summary(workspace_root)?;
    if summary.trim().is_empty() {
        return Err(VersionControlError::GitFailed.into());
    }
    let prompt = build_commit_prompt(&summary, truncated);
    let request = AiRequest {
        provider: provider.to_string(),
        model: model.to_string(),
        messages: vec![AiMessage {
            role: AiRole::User,
            content: prompt,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }],
        tools: Vec::new(),
        model_config: None,
        request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
    };
    let response = RequestExecutionService::new(db).execute(&request)?;
    Ok(GeneratedCommitMessage {
        message: sanitize_ai_message(&response.content),
        truncated_input: truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Canonical scratch directory outside any repository under test.
    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!("nexora-vcs-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp root");
        strip_verbatim(dir.canonicalize().expect("canonicalize temp root"))
    }

    /// Fresh repository with test user identity (no global config needed:
    /// commits carry an explicit signature).
    fn init_repo(dir: &Path) -> git2::Repository {
        git2::Repository::init(dir).expect("init test repository")
    }

    fn test_signature() -> git2::Signature<'static> {
        git2::Signature::now("nexora-test", "nexora-test@example.com").expect("test signature")
    }

    /// Write `rel` under `repo`, stage it, and commit with `message`.
    fn commit_file(repo: &git2::Repository, rel: &str, content: &[u8], message: &str) {
        let workdir = repo.workdir().expect("test repo has a workdir");
        let path = workdir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create test parent");
        }
        std::fs::write(&path, content).expect("write test file");
        let mut index = repo.index().expect("test index");
        index.add_path(Path::new(rel)).expect("stage test file");
        index.write().expect("write test index");
        let tree_id = index.write_tree().expect("write test tree");
        let tree = repo.find_tree(tree_id).expect("find test tree");
        let signature = test_signature();
        let parents: Vec<git2::Commit<'_>> = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .expect("test commit");
    }

    #[test]
    fn status_lists_untracked_then_log_after_commit() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        std::fs::write(dir.join("note.txt"), "hello").expect("seed untracked");
        let info = git_info(&dir, None).expect("git info reads");
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files[0].path, "note.txt");
        assert_eq!(info.files[0].status, "untracked");
        assert!(info.commits.is_empty());

        commit_file(&repo, "note.txt", b"hello", "add note");
        let info = git_info(&dir, None).expect("git info reads after commit");
        assert!(info.files.is_empty());
        assert_eq!(info.commits.len(), 1);
        assert_eq!(info.commits[0].message, "add note");
        assert_eq!(info.commits[0].author, "nexora-test");
        assert!(info.commits[0].time > 0);
        assert_eq!(info.commits[0].hash.len(), 40);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modified_file_diff_shows_unified_hunks() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "note.txt", b"line one\nline two\n", "add note");
        std::fs::write(dir.join("note.txt"), "line one\nline two changed\n").expect("modify");
        let info = git_info(&dir, None).expect("git info reads");
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files[0].status, "modified");

        let diff = git_file_diff(&dir, "note.txt").expect("diff reads");
        assert!(!diff.binary);
        assert!(!diff.truncated);
        assert!(diff.diff.contains("-line two"), "diff: {}", diff.diff);
        assert!(
            diff.diff.contains("+line two changed"),
            "diff: {}",
            diff.diff
        );

        // A clean file yields an empty, non-truncated diff rather than an error.
        commit_file(
            &repo,
            "note.txt",
            b"line one\nline two changed\n",
            "update note",
        );
        let clean = git_file_diff(&dir, "note.txt").expect("clean diff reads");
        assert_eq!(clean.diff, "");
        assert!(!clean.truncated);
        assert!(!clean.binary);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn traversal_paths_are_denied() {
        let dir = temp_root();
        let _repo = init_repo(&dir);
        for raw in [
            "../evil.txt",
            "..\\evil.txt",
            "/abs/path.txt",
            "C:/evil.txt",
            "C:\\evil.txt",
            "",
            "   ",
            ".git/config",
            "sub/../../evil.txt",
            "sub/../../../evil.txt",
        ] {
            let err = git_file_diff(&dir, raw).expect_err("traversal must be denied");
            assert_eq!(err, VersionControlError::InvalidPath, "input: {raw:?}");
            assert_eq!(format!("{err}"), "the file path is invalid");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_repository_refuses_with_fixed_text() {
        let dir = temp_root();
        let err = git_info(&dir, None).expect_err("plain dir is not a repo");
        assert_eq!(err, VersionControlError::NotARepository);
        assert_eq!(
            format!("{err}"),
            "the workspace is not inside a git repository"
        );
        let err = git_file_diff(&dir, "note.txt").expect_err("plain dir diff refuses");
        assert_eq!(err, VersionControlError::NotARepository);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_diff_is_truncated_with_notice() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        let big: Vec<u8> = (0..(MAX_DIFF_BYTES + 64 * 1024))
            .map(|i| b'a' + u8::try_from(i % 26).expect("remainder fits in u8"))
            .collect();
        commit_file(&repo, "big.txt", b"seed\n", "add big");
        std::fs::write(dir.join("big.txt"), &big).expect("write oversized file");
        let diff = git_file_diff(&dir, "big.txt").expect("oversized diff reads");
        assert!(!diff.binary);
        assert!(diff.truncated);
        assert!(diff.diff.len() <= MAX_DIFF_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_never_echo_caller_content() {
        const SENTINEL: &str = "sk-test-sentinel-7d1e9a";
        let dir = temp_root();
        let _repo = init_repo(&dir);
        // A traversal-shaped path carrying a secret-looking payload still
        // fails with fixed vocabulary that echoes nothing.
        let err = git_file_diff(&dir, &format!("../{SENTINEL}.txt")).expect_err("traversal denied");
        assert!(!format!("{err}").contains(SENTINEL));
        // Every variant's display text is fixed vocabulary.
        assert_eq!(
            format!("{}", VersionControlError::GitFailed),
            "the git operation failed"
        );
        assert!(!format!("{}", VersionControlError::GitFailed).contains(SENTINEL));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn branch_name_is_reported() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        // Unborn HEAD: no branch, no commits, but the call still succeeds.
        let info = git_info(&dir, None).expect("unborn repo reads");
        assert!(info.commits.is_empty());
        commit_file(&repo, "note.txt", b"hello", "add note");
        let info = git_info(&dir, None).expect("born repo reads");
        assert!(info.branch.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_limit_is_clamped() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        for index in 0..5 {
            commit_file(
                &repo,
                &format!("file-{index}.txt"),
                b"x",
                &format!("commit {index}"),
            );
        }
        let info = git_info(&dir, Some(2)).expect("limited log reads");
        assert_eq!(info.commits.len(), 2);
        let info = git_info(&dir, Some(0)).expect("zero limit clamps to one");
        assert_eq!(info.commits.len(), 1);
        let info = git_info(&dir, Some(10_000)).expect("huge limit clamps");
        assert_eq!(info.commits.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlinked_intermediate_dir_is_denied() {
        let dir = temp_root();
        let _repo = init_repo(&dir);
        let outside = temp_root();
        std::fs::write(outside.join("secret.txt"), "outside").expect("seed outside file");
        let link = dir.join("link");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if !made {
            // Symlink creation needs elevated privilege on some setups
            // (Windows without Developer Mode): skip gracefully.
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }
        // A file reached through the escaping link refuses ...
        let err = git_file_diff(&dir, "link/secret.txt").expect_err("escaping link must be denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        // ... and so does an absent path routed through it (the nearest
        // existing ancestor — the link itself — already escapes).
        let err =
            git_file_diff(&dir, "link/missing.txt").expect_err("escaping link must be denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn untracked_tree_is_capped_with_overflow_count() {
        let dir = temp_root();
        let _repo = init_repo(&dir);
        let total = MAX_STATUS_FILES + 37;
        for index in 0..total {
            let name = format!("bulk-{index:05}.txt");
            std::fs::write(dir.join(&name), "bulk").expect("seed bulk untracked");
        }
        let info = git_info(&dir, None).expect("capped status reads");
        assert_eq!(info.files.len(), MAX_STATUS_FILES);
        assert_eq!(info.files_overflow, 37);
        // The cap keeps the first entries by sort order (zero-padded names
        // sort numerically).
        assert_eq!(info.files[0].path, "bulk-00000.txt");
        assert_eq!(info.files[MAX_STATUS_FILES - 1].path, "bulk-00499.txt");
        assert!(info.files.iter().all(|file| file.status == "untracked"));
        let _ = std::fs::remove_dir_all(&dir);

        // Small trees report no overflow.
        let dir = temp_root();
        let _repo = init_repo(&dir);
        std::fs::write(dir.join("note.txt"), "hello").expect("seed untracked");
        let info = git_info(&dir, None).expect("small status reads");
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files_overflow, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detached_head_reports_no_branch_but_reads_commits() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "note.txt", b"hello", "add note");
        let oid = repo
            .head()
            .expect("test head")
            .peel_to_commit()
            .expect("test commit")
            .id();
        repo.set_head_detached(oid).expect("detach test head");
        let info = git_info(&dir, None).expect("detached info reads");
        assert!(info.branch.is_none());
        assert_eq!(info.commits.len(), 1);
        assert_eq!(info.commits[0].message, "add note");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Guarded writes: stage / unstage / commit / push
    // -----------------------------------------------------------------------

    /// Local git identity for commits under test (`repo.signature()` reads
    /// the local config only — the commit path never invents an author).
    fn local_identity(repo: &git2::Repository) {
        let mut config = repo.config().expect("test config");
        config
            .set_str("user.name", "nexora-test")
            .expect("set test name");
        config
            .set_str("user.email", "nexora-test@example.com")
            .expect("set test email");
    }

    #[test]
    fn writes_require_explicit_confirmation() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        std::fs::write(dir.join("note.txt"), "hello").expect("seed file");
        assert_eq!(
            git_stage(&dir, &["note.txt".to_string()], false).expect_err("stage needs confirm"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_unstage(&dir, &["note.txt".to_string()], false).expect_err("unstage needs confirm"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_commit(&dir, "feat(test): add note", false).expect_err("commit needs confirm"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_push(&dir, "origin", false).expect_err("push needs confirm"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            format!("{}", VersionControlError::Unconfirmed),
            "explicit confirmation is required before this git write can run"
        );
        // Nothing was staged behind the refusal.
        let info = git_info(&dir, None).expect("info reads");
        assert!(info.files.iter().all(|file| file.status == "untracked"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_unstage_round_trip_updates_status() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "note.txt", b"hello", "add note");
        std::fs::write(dir.join("note.txt"), "hello changed").expect("modify");
        std::fs::write(dir.join("new.txt"), "new").expect("seed untracked");

        let staged = git_stage(&dir, &["note.txt".to_string(), "new.txt".to_string()], true)
            .expect("stage reads");
        assert_eq!(staged, 2);
        let info = git_info(&dir, None).expect("info reads");
        assert!(info.files.iter().all(|file| file.status == "staged"));

        let unstaged = git_unstage(&dir, &["note.txt".to_string()], true).expect("unstage");
        assert_eq!(unstaged, 1);
        let info = git_info(&dir, None).expect("info reads after unstage");
        let statuses: std::collections::HashMap<&str, &str> = info
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.status.as_str()))
            .collect();
        assert_eq!(statuses["note.txt"], "modified");
        assert_eq!(statuses["new.txt"], "staged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_rejects_escaping_and_oversized_batches() {
        let dir = temp_root();
        let _repo = init_repo(&dir);
        for raw in ["../evil.txt", ".git/config", ""] {
            let err = git_stage(&dir, &[raw.to_string()], true).expect_err("escaping stage denied");
            assert_eq!(err, VersionControlError::InvalidPath);
            let err =
                git_unstage(&dir, &[raw.to_string()], true).expect_err("escaping unstage denied");
            assert_eq!(err, VersionControlError::InvalidPath);
        }
        // Empty and overlong batches refuse without touching git.
        let err = git_stage(&dir, &[], true).expect_err("empty batch denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        let big: Vec<String> = (0..=MAX_STAGE_PATHS)
            .map(|i| format!("f-{i}.txt"))
            .collect();
        let err = git_stage(&dir, &big, true).expect_err("overlong batch denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_through_escaping_symlink_is_denied() {
        let dir = temp_root();
        let _repo = init_repo(&dir);
        let outside = temp_root();
        std::fs::write(outside.join("secret.txt"), "outside").expect("seed outside file");
        let link = dir.join("link");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if !made {
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }
        let err = git_stage(&dir, &["link/secret.txt".to_string()], true)
            .expect_err("escaping stage must be denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn commit_validates_message_and_uses_local_config_author() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        std::fs::write(dir.join("note.txt"), "hello").expect("seed file");
        git_stage(&dir, &["note.txt".to_string()], true).expect("stage");

        // Shape validation runs before any tree comparison, so these refuse
        // regardless of staged state.
        for bad in [
            "",
            "   ",
            "x".repeat(MAX_COMMIT_MESSAGE_LEN + 1).as_str(),
            format!("feat(test): {}", "s".repeat(MAX_COMMIT_SUBJECT_LEN)).as_str(),
        ] {
            let err = git_commit(&dir, bad, true).expect_err("bad message must be denied");
            assert_eq!(err, VersionControlError::InvalidMessage, "input: {bad:?}");
        }
        assert_eq!(
            format!("{}", VersionControlError::InvalidMessage),
            "the commit message is invalid"
        );

        // Well-formed but non-conventional messages still commit:
        // conventional shape is enforced only on the AI path (sanitize),
        // never as a commit gate.
        let loose = git_commit(&dir, "first version", true).expect("loose message commits");
        assert_eq!(loose.len(), 40);

        std::fs::write(dir.join("note.txt"), "hello again").expect("modify again");
        git_stage(&dir, &["note.txt".to_string()], true).expect("stage again");

        let hash = git_commit(&dir, "feat(test): add note\n\nFirst test note.", true)
            .expect("valid commit");
        assert_eq!(hash.len(), 40);
        let info = git_info(&dir, None).expect("info reads");
        assert!(info.files.is_empty());
        assert_eq!(info.commits.len(), 2);
        // Full summary is the first line; author is the local git config.
        assert_eq!(info.commits[0].message, "feat(test): add note");
        assert_eq!(info.commits[0].author, "nexora-test");
        assert_eq!(info.commits[0].hash, hash);

        // Committing with nothing staged refuses (no empty commits).
        let err = git_commit(&dir, "fix(test): nothing staged", true)
            .expect_err("empty commit must be denied");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_without_staged_changes_on_unborn_head_is_denied() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        let err = git_commit(&dir, "feat(test): empty tree", true)
            .expect_err("unborn empty commit must be denied");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_allows_only_origin_and_never_forces() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        // Non-allowlisted remotes refuse before any repository is touched.
        for remote in ["upstream", "fork", "https://example.com/r.git", ""] {
            let err = git_push(&dir, remote, true).expect_err("non-origin push denied");
            assert_eq!(
                err,
                VersionControlError::InvalidRemote,
                "remote: {remote:?}"
            );
        }
        assert_eq!(
            format!("{}", VersionControlError::InvalidRemote),
            "the git remote is not allowed"
        );

        // Local bare repository as `origin`: push works fully offline.
        let bare_dir = temp_root();
        git2::Repository::init_bare(&bare_dir).expect("init bare remote");
        repo.remote("origin", bare_dir.to_str().expect("bare path is unicode"))
            .expect("add origin remote");
        commit_file(&repo, "note.txt", b"hello", "add note");
        // `commit_file` stages through the index directly; commit the same
        // way the service would to keep the workdir state realistic.
        git_push(&dir, "origin", true).expect("push to local origin");
        let bare = git2::Repository::open_bare(&bare_dir).expect("open bare");
        let local_oid = repo
            .head()
            .expect("local head")
            .peel_to_commit()
            .expect("local commit")
            .id();
        let branch = current_branch(&repo).expect("test branch");
        let remote_oid = bare
            .find_reference(&format!("refs/heads/{branch}"))
            .expect("remote branch exists")
            .peel_to_commit()
            .expect("remote commit")
            .id();
        assert_eq!(local_oid, remote_oid);

        // A diverged history must NOT be overwritten: without any force flag
        // the non-fast-forward push fails instead of clobbering the remote.
        let clone_dir = temp_root();
        let cloned =
            git2::Repository::clone(bare_dir.to_str().expect("bare path is unicode"), &clone_dir)
                .expect("clone bare");
        local_identity(&cloned);
        commit_file(&cloned, "other.txt", b"other", "add other");
        {
            let branch = current_branch(&cloned).expect("clone branch");
            let mut remote = cloned.find_remote("origin").expect("clone origin");
            let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
            remote
                .push(&[refspec.as_str()], None)
                .expect("clone pushes first");
        }
        commit_file(&repo, "diverged.txt", b"diverged", "diverge local");
        let err = git_push(&dir, "origin", true).expect_err("diverged push must fail");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bare_dir);
        let _ = std::fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn push_from_detached_head_is_denied() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        commit_file(&repo, "note.txt", b"hello", "add note");
        let oid = repo
            .head()
            .expect("test head")
            .peel_to_commit()
            .expect("test commit")
            .id();
        repo.set_head_detached(oid).expect("detach test head");
        let err = git_push(&dir, "origin", true).expect_err("detached push denied");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_force_push_code_path_exists() {
        // Static guard: outside comments and this test module, the service
        // must contain no force-push flag or forced refspec. The module docs
        // mention `--force` only to document its absence, so comment lines
        // (trimmed lines starting with `/`) are excluded first.
        let source = include_str!("version_control.rs");
        let (code, _) = source
            .split_once("#[cfg(test)]")
            .expect("test module marker");
        let code: String = code
            .lines()
            .filter(|line| !line.trim_start().starts_with('/'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("--force"),
            "a force-push flag must never exist on the git path"
        );
        assert!(
            !code.contains("+refs/"),
            "a forced refspec prefix must never exist on the git path"
        );
    }

    #[test]
    fn commit_message_shapes_validate() {
        for good in [
            "feat(git): add staging",
            "fix(ui): repair overflow line",
            "docs(readme): refresh setup",
            "chore(workspace): update files",
            "revert: back out bad push",
            "feat(git): add staging\n\nBody line one.\nBody line two.",
        ] {
            assert!(validate_commit_message(good).is_ok(), "input: {good:?}");
            assert!(is_conventional_message(good), "input: {good:?}");
        }
        for bad in [
            "no colon at all",
            "feat no colon",
            "feat:nospace",
            "unknown(git): bad type",
            "feat(): empty scope",
            "feat(has space): bad scope",
            "feat(scope):",
            "feat(scope):   ",
        ] {
            assert!(!is_conventional_message(bad), "input: {bad:?}");
        }
        assert!(!is_conventional_message(""));
    }

    #[test]
    fn sanitize_ai_message_always_yields_valid_conventional() {
        let long_subject = format!("feat(git): {}", "s".repeat(200));
        let overlong = format!("feat(git): ok\n\n{}", "b".repeat(MAX_COMMIT_MESSAGE_LEN));
        let cases = [
            "feat(git): add staging\n\nStages files through the index.",
            "```\nfix(ui): repair overflow\n\nShows the count.\n```",
            "Here is your message:\n\nfeat(git): add staging\n\nBody here.",
            "some prose without any conventional line at all",
            "",
            "   ",
            "```",
            long_subject.as_str(),
            overlong.as_str(),
        ];
        for raw in cases {
            let message = sanitize_ai_message(raw);
            assert!(
                validate_commit_message(&message).is_ok(),
                "sanitized must validate, raw: {raw:?} got: {message:?}"
            );
            assert!(
                is_conventional_message(&message),
                "sanitized must be conventional, raw: {raw:?} got: {message:?}"
            );
        }
        // A conventional subject survives with its body intact.
        let message = sanitize_ai_message("fix(ui): repair overflow\n\nShows the count.");
        assert_eq!(message, "fix(ui): repair overflow\n\nShows the count.");
        // Fences and leading prose are stripped, not echoed.
        let message = sanitize_ai_message("Sure! ```\nfeat(git): add staging\n```");
        assert_eq!(message, "feat(git): add staging");
    }

    #[test]
    fn new_error_variants_stay_secret_free() {
        const SENTINEL: &str = "sk-test-sentinel-9c3f1b";
        for err in [
            VersionControlError::Unconfirmed,
            VersionControlError::InvalidMessage,
            VersionControlError::InvalidRemote,
        ] {
            assert!(!format!("{err}").contains(SENTINEL));
        }
        let dir = temp_root();
        let _repo = init_repo(&dir);
        // A traversal-shaped batch carrying a secret-looking payload still
        // fails with fixed vocabulary that echoes nothing.
        let err =
            git_stage(&dir, &[format!("../{SENTINEL}.txt")], true).expect_err("traversal denied");
        assert!(!format!("{err}").contains(SENTINEL));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_summary_feeds_the_commit_prompt() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "note.txt", b"line one\n", "add note");
        // Nothing staged: the summary is empty and generation input is absent.
        let (summary, truncated) = staged_diff_summary(&dir).expect("summary reads");
        assert!(summary.trim().is_empty());
        assert!(!truncated);

        std::fs::write(dir.join("note.txt"), "line one\nline two\n").expect("modify");
        git_stage(&dir, &["note.txt".to_string()], true).expect("stage");
        let (summary, truncated) = staged_diff_summary(&dir).expect("summary reads");
        assert!(!truncated);
        assert!(summary.contains("note.txt"), "summary: {summary}");
        let prompt = build_commit_prompt(&summary, truncated);
        assert!(prompt.contains("conventional commit message"));
        assert!(prompt.contains("note.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
