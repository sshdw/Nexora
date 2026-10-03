//! Read-only git inspection for the opened workspace (application layer).
//!
//! This module owns the git read path behind the two thin `git_info` /
//! `git_file_diff` IPC commands (one feature area, minimal surface): status
//! (changed/staged/untracked files), recent commits, and per-file unified
//! diffs. Everything is read-only — no staging, committing, pushing, branch
//! ops, or writes of any kind — and everything is scoped to the enclosing
//! repository of the canonical workspace root:
//!
//! - The workspace root is canonicalized first; [`git2::Repository::discover`]
//!   finds the enclosing repository upward from there. A workspace outside
//!   any repository refuses with [`VersionControlError::NotARepository`], and
//!   bare repositories (no workdir) refuse the same way.
//! - The only caller-supplied path (the diff `path` argument) goes through
//!   the `project_dir` guard pattern: lexical validation (no absolute paths,
//!   no parent references, no `.git` components, no null bytes, bounded
//!   length), a lexical [`is_within_workspace`](crate::application::agent::tools::is_within_workspace)
//!   prefix check against the canonical repository workdir, and a
//!   canonicalize-and-recheck backstop for symlinks (including symlinked
//!   intermediate directories, resolved via the nearest existing ancestor).
//!   Traversal attempts fail with [`VersionControlError::InvalidPath`].
//! - Failures are secret-free: [`VersionControlError`] carries no payload, so
//!   formatting it can never leak diff content (which may contain user
//!   secrets), credentials, SQL, or file content. The command layer maps it to
//!   fixed-vocabulary [`CommandError`](crate::commands::error::CommandError)
//!   text and deliberately logs no git internals.
//! - Diffs are capped at [`MAX_DIFF_BYTES`] (256 KiB) with a `truncated` flag;
//!   binary content is detected by a null byte and reported as `binary` with
//!   an empty diff rather than guessed as text.
//!
//! There is no file watching or live refresh: the frontend reloads explicitly
//! (manual Refresh only).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::application::agent::tools::is_within_workspace;
use crate::application::workspace::strip_verbatim;

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

/// Secret-free failures for read-only git inspection.
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
    /// A git operation failed (status, log, or diff read).
    GitFailed,
}

impl std::fmt::Display for VersionControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository => {
                write!(f, "the workspace is not inside a git repository")
            }
            Self::InvalidPath => write!(f, "the file path is invalid"),
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

/// Validate a caller-supplied diff path into a repository-relative path.
///
/// Rejects absolute paths (both separators, including driveSmoke and UNC
/// forms), parent references, `.git` components, null bytes, empty input,
/// and overlong input — all with the single fixed-vocabulary
/// [`VersionControlError::InvalidPath`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidPath`] for any rejected shape.
fn clean_diff_path(raw: &str) -> Result<PathBuf, VersionControlError> {
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
    let rel = clean_diff_path(raw_path)?;
    let joined = canon_repo.join(&rel);
    if !is_within_workspace(&canon_repo, &joined) {
        return Err(VersionControlError::InvalidPath);
    }
    // Backstop for symlinks anywhere along the path (including symlinked
    // intermediate directories): resolve the nearest existing ancestor
    // through the filesystem, re-attach the unresolved tail, and re-check
    // containment. A request like `link/secret` where `link` escapes the
    // workdir refuses even though the lexical check above passed.
    {
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
                if !is_within_workspace(&canon_repo, &resolved) {
                    return Err(VersionControlError::InvalidPath);
                }
                break;
            }
            match ancestor.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => ancestor = parent,
                _ => break,
            }
        }
    }
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
}
