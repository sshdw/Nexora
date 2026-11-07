//! Read-side git inspection: status, log, diffs, and repo summary.
//!
//! Split from `version_control.rs` (AC-207): everything that only *reads*
//! the repository lives here; guarded writes, path validation, and the AI
//! commit-message generation are siblings.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths::{assert_within_repo, clean_repo_path};
use super::{
    canonical_workdir, current_branch, open_workspace_repo, VersionControlError,
    DEFAULT_LOG_ENTRIES, LARGE_DIFF_LINES, MANY_FILES_COUNT, MAX_DIFF_BYTES, MAX_LOG_ENTRIES,
    MAX_STATUS_FILES,
};

/// One changed file: its repository-relative path plus a fixed-vocabulary
/// status (`"modified"`, `"staged"`, `"untracked"`, `"deleted"`, `"renamed"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFileStatus {
    /// Repository-relative forward-slash path.
    pub path: String,
    /// Fixed-vocabulary change kind (never backend internals).
    pub status: String,
}

/// One recent commit: full hash plus summary, author name, Unix-seconds
/// time, per-commit file statistics, and heuristic risk signals. The message
/// is the commit summary (first line) only, so payloads stay small. Stats and
/// signals are computed locally from the tree diff against the first parent
/// (no network, no AI): `files_changed` counts the changed paths,
/// `insertions`/`deletions` count added/removed lines, and `risk_signals`
/// carries zero or more of the fixed vocabulary `"large-diff"`,
/// `"many-files"`, `"binary"`, `"merge-commit"`, `"unfamiliar-author"`.
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
    /// Number of paths changed by this commit (against its first parent).
    pub files_changed: usize,
    /// Added lines in this commit (against its first parent).
    pub insertions: usize,
    /// Removed lines in this commit (against its first parent).
    pub deletions: usize,
    /// Heuristic risk signals, fixed vocabulary, in signal order (see
    /// [`commit_risk_signals`]).
    pub risk_signals: Vec<String>,
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

/// One log entry mid-assembly: identity plus locally computed statistics.
/// [`read_log`] maps these onto [`GitCommit`] with page-relative signals.
struct RawCommit {
    hash: String,
    message: String,
    author: String,
    time: i64,
    files_changed: usize,
    insertions: usize,
    deletions: usize,
    binary: bool,
    is_merge: bool,
}

/// Read up to `limit` recent commits from `HEAD`, newest first. A repository
/// with no commits yet (unborn `HEAD`) yields an empty list, not an error.
///
/// Every entry carries locally computed file statistics plus heuristic risk
/// signals (see [`commit_risk_signals`]): the `"unfamiliar-author"` signal is
/// relative to the returned page — the modal (most frequent, newest wins
/// ties) author counts as familiar, any other author as unfamiliar.
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
    let mut raw: Vec<RawCommit> = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid.map_err(|_| VersionControlError::GitFailed)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|_| VersionControlError::GitFailed)?;
        let stats = commit_stats(repo, &commit)?;
        raw.push(RawCommit {
            hash: oid.to_string(),
            message: commit
                .summary_bytes()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default(),
            author: String::from_utf8_lossy(commit.author().name_bytes()).into_owned(),
            time: commit.time().seconds(),
            files_changed: stats.files_changed,
            insertions: stats.insertions,
            deletions: stats.deletions,
            binary: stats.binary,
            is_merge: commit.parent_count() > 1,
        });
    }
    // The modal author of the page (most frequent; newest commit wins ties
    // by first-seen order) counts as familiar.
    let mut counts: Vec<(String, usize)> = Vec::new();
    for entry in &raw {
        match counts.iter_mut().find(|(name, _)| *name == entry.author) {
            Some((_, count)) => *count += 1,
            None => counts.push((entry.author.clone(), 1)),
        }
    }
    let mut familiar: Option<&str> = None;
    let mut best = 0;
    for (name, count) in &counts {
        if *count > best {
            best = *count;
            familiar = Some(name);
        }
    }
    let familiar = familiar.unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|entry| {
            let risk_signals = commit_risk_signals(
                entry.files_changed,
                entry.insertions,
                entry.deletions,
                entry.binary,
                entry.is_merge,
                entry.author.as_str() != familiar,
            );
            GitCommit {
                hash: entry.hash,
                message: entry.message,
                author: entry.author,
                time: entry.time,
                files_changed: entry.files_changed,
                insertions: entry.insertions,
                deletions: entry.deletions,
                risk_signals,
            }
        })
        .collect())
}

/// File statistics for one commit's tree diff.
struct CommitStats {
    files_changed: usize,
    insertions: usize,
    deletions: usize,
    binary: bool,
}

/// Diff statistics for `commit` against its first parent (against the empty
/// tree for a root commit): changed-path count, added/removed lines, and
/// whether any delta is binary (a missing patch or a null byte, the same
/// detection the per-file diff path uses). Context is zero — stats need no
/// surrounding lines — so the walk stays cheap.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] when a tree or diff read fails.
fn commit_stats(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
) -> Result<CommitStats, VersionControlError> {
    let new_tree = commit.tree().map_err(|_| VersionControlError::GitFailed)?;
    let old_tree = if commit.parent_count() > 0 {
        Some(
            commit
                .parent(0)
                .map_err(|_| VersionControlError::GitFailed)?
                .tree()
                .map_err(|_| VersionControlError::GitFailed)?,
        )
    } else {
        None
    };
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    let diff = repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let stats = diff.stats().map_err(|_| VersionControlError::GitFailed)?;
    let mut binary = false;
    // `Deltas` walks `0..count` in order, so the enumerated index is the
    // delta index `Patch::from_diff` needs. Binary detection reads the
    // delta flags first (libgit2 renders binary deltas as printable
    // `Binary files differ` text, so content sniffing alone would miss
    // them); a missing patch or a null byte is the backstop.
    for (index, delta) in diff.deltas().enumerate() {
        if delta.flags().is_binary() {
            binary = true;
            break;
        }
        let Ok(Some(mut patch)) = git2::Patch::from_diff(&diff, index) else {
            binary = true;
            break;
        };
        let Ok(buf) = patch.to_buf() else {
            binary = true;
            break;
        };
        let bytes: &[u8] = &buf;
        if bytes.contains(&0) {
            binary = true;
            break;
        }
    }
    Ok(CommitStats {
        files_changed: stats.files_changed(),
        insertions: stats.insertions(),
        deletions: stats.deletions(),
        binary,
    })
}

/// Heuristic risk signals for one commit, computed locally without AI.
/// Fixed vocabulary in fixed order: `"large-diff"` (added plus removed lines
/// at or above [`LARGE_DIFF_LINES`]), `"many-files"` (changed paths at or
/// above [`MANY_FILES_COUNT`]), `"binary"` (at least one binary delta),
/// `"merge-commit"` (more than one parent), `"unfamiliar-author"` (the author
/// differs from the page's modal author — see [`read_log`]).
#[must_use]
fn commit_risk_signals(
    files_changed: usize,
    insertions: usize,
    deletions: usize,
    binary: bool,
    is_merge: bool,
    unfamiliar_author: bool,
) -> Vec<String> {
    let mut signals = Vec::new();
    if insertions.saturating_add(deletions) >= LARGE_DIFF_LINES {
        signals.push("large-diff".to_string());
    }
    if files_changed >= MANY_FILES_COUNT {
        signals.push("many-files".to_string());
    }
    if binary {
        signals.push("binary".to_string());
    }
    if is_merge {
        signals.push("merge-commit".to_string());
    }
    if unfamiliar_author {
        signals.push("unfamiliar-author".to_string());
    }
    signals
}

/// Resolve `raw` (a full 40-hex commit hash, as returned by [`read_log`])
/// to its commit. Only full hashes are accepted: prefixes stay unresolved
/// rather than guessed. Any failure — bad shape or unknown hash — is the
/// fixed-vocabulary [`VersionControlError::GitFailed`], so a hash (which the
/// frontend already holds) is never echoed.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] when the hash is malformed or
/// unknown.
pub(super) fn find_commit_by_hash<'repo>(
    repo: &'repo git2::Repository,
    raw: &str,
) -> Result<git2::Commit<'repo>, VersionControlError> {
    let trimmed = raw.trim();
    if trimmed.len() != 40 || !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VersionControlError::GitFailed);
    }
    let oid = git2::Oid::from_str(trimmed).map_err(|_| VersionControlError::GitFailed)?;
    repo.find_commit(oid)
        .map_err(|_| VersionControlError::GitFailed)
}

/// Unified diff of one historical commit (against its first parent, against
/// the empty tree for a root commit), capped at [`MAX_DIFF_BYTES`] with a
/// truncation notice. Binary deltas are skipped with an inline placeholder
/// line (content omitted); `binary` reports whether any delta was binary.
/// `files` lists the changed repository-relative paths, sorted, capped at
/// [`MAX_STATUS_FILES`] with the remainder in `files_overflow`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct GitCommitDiff {
    /// Full commit hash, as resolved.
    pub hash: String,
    /// Unified diff text (empty when the commit changed no content).
    pub diff: String,
    /// Whether `diff` was cut at [`MAX_DIFF_BYTES`].
    pub truncated: bool,
    /// Whether any changed file is binary (its content is omitted).
    pub binary: bool,
    /// Changed repository-relative paths, sorted, capped.
    pub files: Vec<String>,
    /// Number of changed paths omitted beyond [`MAX_STATUS_FILES`].
    pub files_overflow: usize,
}

/// Unified diff of the commit `raw_hash` (full hash only), for the timeline
/// detail view. Read-only; no working-tree state is touched.
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] for a malformed or unknown
/// hash; see [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_commit_diff(
    workspace_root: &Path,
    raw_hash: &str,
) -> Result<GitCommitDiff, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let commit = find_commit_by_hash(&repo, raw_hash)?;
    let hash = commit.id().to_string();
    let new_tree = commit.tree().map_err(|_| VersionControlError::GitFailed)?;
    let old_tree = if commit.parent_count() > 0 {
        Some(
            commit
                .parent(0)
                .map_err(|_| VersionControlError::GitFailed)?
                .tree()
                .map_err(|_| VersionControlError::GitFailed)?,
        )
    } else {
        None
    };
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    let diff = repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut paths: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut binary = false;
    // `Deltas` walks `0..count` in order, so the enumerated index is the
    // delta index `Patch::from_diff` needs.
    for (index, delta) in diff.deltas().enumerate() {
        let delta_path = delta
            .new_file()
            .path_bytes()
            .or_else(|| delta.old_file().path_bytes())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        if !delta_path.is_empty() {
            paths.push(delta_path);
        }
        // Binary deltas carry no printable patch (see `commit_stats`): skip
        // the content and note the omission once below.
        if delta.flags().is_binary() {
            binary = true;
            continue;
        }
        let Ok(Some(mut patch)) = git2::Patch::from_diff(&diff, index) else {
            binary = true;
            continue;
        };
        let Ok(buf) = patch.to_buf() else {
            binary = true;
            continue;
        };
        let bytes: &[u8] = &buf;
        if bytes.contains(&0) {
            binary = true;
            continue;
        }
        text.push_str(&String::from_utf8_lossy(bytes));
        if text.len() >= MAX_DIFF_BYTES {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    let overflow = paths.len().saturating_sub(MAX_STATUS_FILES);
    paths.truncate(MAX_STATUS_FILES);
    // Binary deltas contribute the placeholder, not content: the truncation
    // cut below applies to the emitted text only.
    let mut out = text;
    if binary {
        out.push_str("(binary files changed by this commit have their content omitted)\n");
    }
    let truncated = out.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    Ok(GitCommitDiff {
        hash,
        diff: out,
        truncated,
        binary,
        files: paths,
        files_overflow: overflow,
    })
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
/// Per-file unified diff for `raw_path` (repository-relative), capped at
/// [`MAX_DIFF_BYTES`] with `truncated` set, against `HEAD` (staged plus
/// unstaged, untracked included). A file with no changes yields an empty
/// diff; binary content yields `binary` with an empty diff.
///
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
