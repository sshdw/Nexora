//! Git inspection plus guarded write operations for the opened workspace
//! (application layer).
//!
//! This module owns the git path behind the thin `git_info` / `git_file_diff`
//! IPC commands (one feature area, minimal surface) plus the write commands
//! `git_stage` / `git_unstage` / `git_commit` / `git_push` and the
//! AI-generated commit message (`git_generate_commit_message`):
//!
//! - Reads: status (changed/staged/untracked files), recent commits with
//!   per-commit file statistics and heuristic risk signals (timeline view),
//!   per-file unified diffs, and per-commit unified diffs. Everything
//!   read-only is additive history detail; no new writes exist here.
//! - Writes: staging, unstaging, committing (author = local git config only),
//!   and pushing to the preconfigured `origin` remote. There is no pull/fetch,
//!   branch, merge, rebase, stash, or amend-of-others path here.
//! - Everything is scoped to the enclosing repository of the canonical
//!   workspace root, with the same guards as the read path (lexical path
//!   validation, workspace-prefix check, symlink-ancestor backstop,
//!   fixed-vocabulary [`VersionControlError`]).
//! - Every write requires a live single-use confirmation id minted in the
//!   `version_control` scope by the `request_confirmation` command — which
//!   only mints after the user accepts a **blocking native OS dialog** — and
//!   bound to the exact operation being performed ([`OP_GIT_STAGE`] /
//!   [`OP_GIT_UNSTAGE`] / [`OP_GIT_COMMIT`] / [`OP_GIT_PUSH`]). The id is
//!   consumed atomically on first use; forged, expired, reused, cross-scope,
//!   and different-operation ids refuse with
//!   [`VersionControlError::Unconfirmed`] before git is touched. This is the
//!   server-side confirmation gate (SEC-03), the same shape as the terminal
//!   and data-management paths — no caller-supplied boolean exists on these
//!   paths anymore.
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

mod commit_message;
mod mutate;
mod paths;
mod status;

// The re-exports keep every original single-file path
// (`crate::application::version_control::<item>`) valid for the command
// layer; `unused_imports` is allowed because reachability — not a current
// consumer — is the point (mirrors the `#![allow(dead_code)]` rationale in
// `application/mod.rs`).
#[allow(unused_imports)]
pub(crate) use commit_message::{
    build_commit_explain_prompt, build_commit_prompt, explain_commit, generate_commit_message,
    sanitize_ai_message, sanitize_commit_explanation, CommitExplainError, CommitExplanation,
    CommitMessageError, GeneratedCommitMessage,
};
#[allow(unused_imports)]
pub(crate) use mutate::{
    git_commit, git_push, git_stage, git_unstage, is_conventional_message, validate_commit_message,
};
#[allow(unused_imports)]
pub(crate) use status::{
    git_commit_diff, git_file_diff, git_info, GitCommit, GitCommitDiff, GitFileDiff, GitFileStatus,
    GitInfo,
};

use std::path::{Path, PathBuf};

use crate::application::workspace::strip_verbatim;

/// Largest per-file diff returned before it is truncated with a notice.
pub(crate) const MAX_DIFF_BYTES: usize = 256 * 1024;

/// Cap for the commit-diff excerpt fed to the commit-explain prompt — the
/// same 64 KiB summary shape as the commit-message and terminal-explain
/// paths. Longer diffs are cut with a notice, and the cut is flagged to the
/// model and the caller.
pub(crate) const MAX_COMMIT_EXPLAIN_BYTES: usize = 64 * 1024;

/// Longest commit-explanation text returned, in characters.
const MAX_COMMIT_EXPLANATION_CHARS: usize = 4000;

/// Added-plus-removed line count at or above which a commit reads
/// `"large-diff"`.
const LARGE_DIFF_LINES: usize = 500;

/// Changed-file count at or above which a commit reads `"many-files"`.
const MANY_FILES_COUNT: usize = 10;
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

#[cfg(test)]
mod tests {
    use super::commit_message::staged_diff_summary;
    use super::*;
    use crate::application::confirmations::{
        ConfirmationRegistry, ConfirmationTarget, OP_GIT_COMMIT, OP_GIT_PUSH, OP_GIT_STAGE,
        OP_GIT_UNSTAGE, SCOPE_VERSION_CONTROL,
    };
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

    /// Mint one fresh single-use id for `operation` (each guarded write
    /// consumes its id atomically, so every confirmed call mints its own).
    /// Returns the id together with the registry that must consume it.
    fn mint(operation: &'static str) -> (String, ConfirmationRegistry) {
        let registry = ConfirmationRegistry::new();
        let id = registry
            .request(SCOPE_VERSION_CONTROL, &ConfirmationTarget::new(operation))
            .expect("mint confirmation id");
        (id, registry)
    }

    /// [`git_stage`] with a freshly minted [`OP_GIT_STAGE`] id.
    fn stage(dir: &Path, paths: &[String]) -> Result<usize, VersionControlError> {
        let (id, confirmations) = mint(OP_GIT_STAGE);
        git_stage(dir, paths, &id, &confirmations)
    }

    /// [`git_unstage`] with a freshly minted [`OP_GIT_UNSTAGE`] id.
    fn unstage(dir: &Path, paths: &[String]) -> Result<usize, VersionControlError> {
        let (id, confirmations) = mint(OP_GIT_UNSTAGE);
        git_unstage(dir, paths, &id, &confirmations)
    }

    /// [`git_commit`] with a freshly minted [`OP_GIT_COMMIT`] id.
    fn commit(dir: &Path, message: &str) -> Result<String, VersionControlError> {
        let (id, confirmations) = mint(OP_GIT_COMMIT);
        git_commit(dir, message, &id, &confirmations)
    }

    /// [`git_push`] with a freshly minted [`OP_GIT_PUSH`] id.
    fn push(dir: &Path, remote: &str) -> Result<(), VersionControlError> {
        let (id, confirmations) = mint(OP_GIT_PUSH);
        git_push(dir, remote, &id, &confirmations)
    }

    #[test]
    fn writes_require_a_server_verified_confirmation_id() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        std::fs::write(dir.join("note.txt"), "hello").expect("seed file");
        // Forged ids refuse without touching git: the gate is the server-side
        // registry, never a caller-supplied flag.
        assert_eq!(
            git_stage(
                &dir,
                &["note.txt".to_string()],
                "forged-id",
                &ConfirmationRegistry::new(),
            )
            .expect_err("stage needs a minted id"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_unstage(
                &dir,
                &["note.txt".to_string()],
                "forged-id",
                &ConfirmationRegistry::new(),
            )
            .expect_err("unstage needs a minted id"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_commit(
                &dir,
                "feat(test): add note",
                "forged-id",
                &ConfirmationRegistry::new(),
            )
            .expect_err("commit needs a minted id"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            git_push(&dir, "origin", "forged-id", &ConfirmationRegistry::new(),)
                .expect_err("push needs a minted id"),
            VersionControlError::Unconfirmed
        );
        // An id minted for one operation cannot drive a sibling write: the
        // binding covers the exact operation.
        let (stage_id, confirmations) = mint(OP_GIT_STAGE);
        assert_eq!(
            git_commit(&dir, "feat(test): add note", &stage_id, &confirmations)
                .expect_err("a stage id must not authorize a commit"),
            VersionControlError::Unconfirmed
        );
        assert_eq!(
            format!("{}", VersionControlError::Unconfirmed),
            "explicit confirmation is required before this git write can run"
        );
        // Nothing was staged behind the refusals.
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

        let staged =
            stage(&dir, &["note.txt".to_string(), "new.txt".to_string()]).expect("stage reads");
        assert_eq!(staged, 2);
        let info = git_info(&dir, None).expect("info reads");
        assert!(info.files.iter().all(|file| file.status == "staged"));

        let unstaged = unstage(&dir, &["note.txt".to_string()]).expect("unstage");
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
            let err = stage(&dir, &[raw.to_string()]).expect_err("escaping stage denied");
            assert_eq!(err, VersionControlError::InvalidPath);
            let err = unstage(&dir, &[raw.to_string()]).expect_err("escaping unstage denied");
            assert_eq!(err, VersionControlError::InvalidPath);
        }
        // Empty and overlong batches refuse without touching git.
        let err = stage(&dir, &[]).expect_err("empty batch denied");
        assert_eq!(err, VersionControlError::InvalidPath);
        let big: Vec<String> = (0..=MAX_STAGE_PATHS)
            .map(|i| format!("f-{i}.txt"))
            .collect();
        let err = stage(&dir, &big).expect_err("overlong batch denied");
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
        let err = stage(&dir, &["link/secret.txt".to_string()])
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
        stage(&dir, &["note.txt".to_string()]).expect("stage");

        // Shape validation runs before any tree comparison, so these refuse
        // regardless of staged state.
        for bad in [
            "",
            "   ",
            "x".repeat(MAX_COMMIT_MESSAGE_LEN + 1).as_str(),
            format!("feat(test): {}", "s".repeat(MAX_COMMIT_SUBJECT_LEN)).as_str(),
        ] {
            let err = commit(&dir, bad).expect_err("bad message must be denied");
            assert_eq!(err, VersionControlError::InvalidMessage, "input: {bad:?}");
        }
        assert_eq!(
            format!("{}", VersionControlError::InvalidMessage),
            "the commit message is invalid"
        );

        // Well-formed but non-conventional messages still commit:
        // conventional shape is enforced only on the AI path (sanitize),
        // never as a commit gate.
        let loose = commit(&dir, "first version").expect("loose message commits");
        assert_eq!(loose.len(), 40);

        std::fs::write(dir.join("note.txt"), "hello again").expect("modify again");
        stage(&dir, &["note.txt".to_string()]).expect("stage again");

        let hash = commit(&dir, "feat(test): add note\n\nFirst test note.").expect("valid commit");
        assert_eq!(hash.len(), 40);
        let info = git_info(&dir, None).expect("info reads");
        assert!(info.files.is_empty());
        assert_eq!(info.commits.len(), 2);
        // Full summary is the first line; author is the local git config.
        assert_eq!(info.commits[0].message, "feat(test): add note");
        assert_eq!(info.commits[0].author, "nexora-test");
        assert_eq!(info.commits[0].hash, hash);

        // Committing with nothing staged refuses (no empty commits).
        let err =
            commit(&dir, "fix(test): nothing staged").expect_err("empty commit must be denied");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_without_staged_changes_on_unborn_head_is_denied() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        local_identity(&repo);
        let err =
            commit(&dir, "feat(test): empty tree").expect_err("unborn empty commit must be denied");
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
            let err = push(&dir, remote).expect_err("non-origin push denied");
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
        push(&dir, "origin").expect("push to local origin");
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
        let err = push(&dir, "origin").expect_err("diverged push must fail");
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
        let err = push(&dir, "origin").expect_err("detached push denied");
        assert_eq!(err, VersionControlError::GitFailed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_force_push_code_path_exists() {
        // Static guard: outside comments and this test module, the service
        // must contain no force-push flag or forced refspec. The module docs
        // mention `--force` only to document its absence, so comment lines
        // (trimmed lines starting with `/`) are excluded first.
        // AC-207 split: the production source now spans five files, so scan
        // all of them (only `mod.rs` carries the `#[cfg(test)]` marker).
        let sources = [
            include_str!("mod.rs"),
            include_str!("status.rs"),
            include_str!("paths.rs"),
            include_str!("mutate.rs"),
            include_str!("commit_message.rs"),
        ];
        let code: String = sources
            .iter()
            .map(|source| {
                let (code, _) = source.split_once("#[cfg(test)]").unwrap_or((source, ""));
                code.lines()
                    .filter(|line| !line.trim_start().starts_with('/'))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
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
        let err = stage(&dir, &[format!("../{SENTINEL}.txt")]).expect_err("traversal denied");
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
        stage(&dir, &["note.txt".to_string()]).expect("stage");
        let (summary, truncated) = staged_diff_summary(&dir).expect("summary reads");
        assert!(!truncated);
        assert!(summary.contains("note.txt"), "summary: {summary}");
        let prompt = build_commit_prompt(&summary, truncated);
        assert!(prompt.contains("conventional commit message"));
        assert!(prompt.contains("note.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Timeline: per-commit stats, risk signals, commit diffs, explain input
    // -----------------------------------------------------------------------

    /// Write `rel` under `repo`, stage it, and commit with `message` under an
    /// explicit author identity (the shared `commit_file` helper always signs
    /// as `nexora-test`; timeline tests need mixed authorship).
    fn commit_file_as(
        repo: &git2::Repository,
        rel: &str,
        content: &[u8],
        message: &str,
        name: &str,
        email: &str,
    ) {
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
        let signature = git2::Signature::now(name, email).expect("test signature");
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

    /// N lines of `line {i}` text for large-diff fixtures.
    fn big_text_lines(count: usize) -> Vec<u8> {
        use std::fmt::Write as _;
        let mut text = String::new();
        for i in 0..count {
            let _ = writeln!(text, "line {i}");
        }
        text.into_bytes()
    }

    #[test]
    fn timeline_stats_count_files_and_lines() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "a.txt", b"l1\nl2\n", "add a");
        // Second commit: modify `a.txt` (one removed, two added lines) and
        // add `b.txt` (one added line).
        std::fs::write(dir.join("a.txt"), "l1\nl2 changed\nl3\n").expect("modify a");
        std::fs::write(dir.join("b.txt"), "b1\n").expect("seed b");
        let mut index = repo.index().expect("test index");
        index.add_path(Path::new("a.txt")).expect("stage a");
        index.add_path(Path::new("b.txt")).expect("stage b");
        index.write().expect("write test index");
        let tree_id = index.write_tree().expect("write test tree");
        let tree = repo.find_tree(tree_id).expect("find test tree");
        let head = repo
            .head()
            .expect("test head")
            .peel_to_commit()
            .expect("test commit");
        let signature = test_signature();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "change a and add b",
            &tree,
            &[&head],
        )
        .expect("test commit");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits.len(), 2);
        let newest = &info.commits[0];
        assert_eq!(newest.message, "change a and add b");
        assert_eq!(newest.files_changed, 2);
        assert_eq!(newest.insertions, 3);
        assert_eq!(newest.deletions, 1);
        assert!(newest.risk_signals.is_empty());
        let root = &info.commits[1];
        assert_eq!(root.files_changed, 1);
        assert_eq!(root.insertions, 2);
        assert_eq!(root.deletions, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_risk_large_diff_and_many_files() {
        // Large diff: 600 added lines in one commit.
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "big.txt", &big_text_lines(600), "add big file");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits[0].risk_signals, vec!["large-diff"]);
        let _ = std::fs::remove_dir_all(&dir);

        // Many files: eleven new paths in one commit.
        let dir = temp_root();
        let repo = init_repo(&dir);
        for index in 0..11 {
            let rel = format!("many-{index}.txt");
            let workdir = repo.workdir().expect("test repo has a workdir");
            std::fs::write(workdir.join(&rel), "x\n").expect("seed many file");
            let mut index_repo = repo.index().expect("test index");
            index_repo
                .add_path(Path::new(&rel))
                .expect("stage many file");
            index_repo.write().expect("write test index");
        }
        let tree_id = repo
            .index()
            .expect("test index")
            .write_tree()
            .expect("write test tree");
        let tree = repo.find_tree(tree_id).expect("find test tree");
        let signature = test_signature();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "add many files",
            &tree,
            &[],
        )
        .expect("test commit");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits[0].files_changed, 11);
        assert_eq!(info.commits[0].risk_signals, vec!["many-files"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_risk_binary_and_merge_commit() {
        // Binary content: a null byte marks the commit.
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "blob.bin", b"a\0b", "add binary");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits[0].risk_signals, vec!["binary"]);
        let _ = std::fs::remove_dir_all(&dir);

        // Merge commit: two parents read `merge-commit` even with an empty
        // tree diff against the first parent.
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "base.txt", b"base\n", "base commit");
        commit_file(&repo, "side.txt", b"side\n", "side commit");
        let second = repo
            .head()
            .expect("test head")
            .peel_to_commit()
            .expect("test commit");
        let first = second.parent(0).expect("test parent");
        let tree = second.tree().expect("test tree");
        let signature = test_signature();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "merge side",
            &tree,
            &[&second, &first],
        )
        .expect("test merge");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits[0].risk_signals, vec!["merge-commit"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_risk_unfamiliar_author() {
        // Unfamiliar author: two commits by alice, one by bob — only bob's
        // commit is flagged (order-independent lookup).
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file_as(
            &repo,
            "a.txt",
            b"a\n",
            "alice one",
            "alice",
            "a@example.com",
        );
        commit_file_as(
            &repo,
            "b.txt",
            b"b\n",
            "alice two",
            "alice",
            "a@example.com",
        );
        commit_file_as(&repo, "c.txt", b"c\n", "bob one", "bob", "b@example.com");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits.len(), 3);
        for entry in &info.commits {
            if entry.author == "bob" {
                assert_eq!(entry.risk_signals, vec!["unfamiliar-author"]);
            } else {
                assert!(entry.risk_signals.is_empty(), "alice: {entry:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_risk_signal_order_is_fixed() {
        // Signal order is fixed: one commit holding a large text change plus
        // a binary file reads `large-diff` before `binary`.
        let dir = temp_root();
        let repo = init_repo(&dir);
        let workdir = repo.workdir().expect("test repo has a workdir");
        std::fs::write(workdir.join("big.txt"), big_text_lines(600)).expect("seed big");
        std::fs::write(workdir.join("blob.bin"), b"a\0b").expect("seed binary");
        let mut index = repo.index().expect("test index");
        index.add_path(Path::new("big.txt")).expect("stage big");
        index.add_path(Path::new("blob.bin")).expect("stage binary");
        index.write().expect("write test index");
        let tree_id = index.write_tree().expect("write test tree");
        let tree = repo.find_tree(tree_id).expect("find test tree");
        let signature = test_signature();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "big plus binary",
            &tree,
            &[],
        )
        .expect("test commit");
        let info = git_info(&dir, None).expect("timeline reads");
        assert_eq!(info.commits[0].risk_signals, vec!["large-diff", "binary"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_diff_reads_unified_text_and_rejects_bad_hashes() {
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "note.txt", b"line one\nline two\n", "add note");
        commit_file(
            &repo,
            "note.txt",
            b"line one\nline two changed\n",
            "update note",
        );
        let info = git_info(&dir, None).expect("timeline reads");
        let hash = info.commits[0].hash.clone();
        let detail = git_commit_diff(&dir, hash.as_str()).expect("commit diff reads");
        assert_eq!(detail.hash, hash);
        assert_eq!(detail.files, vec!["note.txt"]);
        assert_eq!(detail.files_overflow, 0);
        assert!(!detail.binary);
        assert!(!detail.truncated);
        assert!(detail.diff.contains("-line two"), "diff: {}", detail.diff);
        assert!(
            detail.diff.contains("+line two changed"),
            "diff: {}",
            detail.diff
        );

        // Malformed hashes (short, non-hex, empty, traversal-shaped) and
        // well-formed but unknown hashes all fail with fixed vocabulary.
        for bad in [
            String::new(),
            "   ".to_string(),
            "abc".to_string(),
            "a".repeat(7),
            "z".repeat(40),
            "0".repeat(40),
            "../evil".to_string(),
        ] {
            let err = git_commit_diff(&dir, bad.as_str()).expect_err("bad hash must be denied");
            assert_eq!(err, VersionControlError::GitFailed, "input: {bad:?}");
            assert_eq!(format!("{err}"), "the git operation failed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_explain_rejects_commits_without_changes() {
        // A merge commit whose tree equals its first parent changes no files:
        // explanation refuses with fixed vocabulary before any provider is
        // touched (no keyring needed).
        let dir = temp_root();
        let repo = init_repo(&dir);
        commit_file(&repo, "base.txt", b"base\n", "base commit");
        commit_file(&repo, "side.txt", b"side\n", "side commit");
        let second = repo
            .head()
            .expect("test head")
            .peel_to_commit()
            .expect("test commit");
        let first = second.parent(0).expect("test parent");
        let tree = second.tree().expect("test tree");
        let signature = test_signature();
        let oid = repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "merge side",
                &tree,
                &[&second, &first],
            )
            .expect("test merge");
        let db = crate::infrastructure::database::in_memory_database();
        let err = explain_commit(&db, &dir, oid.to_string().as_str(), "openai", "m")
            .expect_err("empty commit diff must be refused");
        assert!(
            matches!(err, CommitExplainError::InvalidInput),
            "unexpected: {err:?}"
        );
        assert_eq!(format!("{err}"), "the commit diff to explain is invalid");
        // Malformed hashes refuse the same way.
        let err =
            explain_commit(&db, &dir, "nope", "openai", "m").expect_err("bad hash must be refused");
        assert!(matches!(err, CommitExplainError::InvalidInput));
        assert_eq!(
            format!(
                "{}",
                CommitExplainError::Request(
                    crate::application::execution::RequestError::UnknownProvider {
                        name: "x".to_string(),
                    }
                )
            ),
            "the commit explanation could not be generated"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_explain_prompt_flags_truncation_and_sanitizer_caps() {
        let prompt = build_commit_explain_prompt("abc by alice", "diff text", false);
        assert!(prompt.contains("what changed"));
        assert!(prompt.contains("abc by alice"));
        assert!(prompt.contains("diff text"));
        assert!(!prompt.contains("truncated to fit"));
        let prompt = build_commit_explain_prompt("abc by alice", "diff text", true);
        assert!(prompt.contains("truncated to fit"));

        let explanation = sanitize_commit_explanation("```\nThe commit adds logging.\n```");
        assert_eq!(explanation, "The commit adds logging.");
        let explanation = sanitize_commit_explanation("Sure! The commit adds logging.");
        assert_eq!(explanation, "Sure! The commit adds logging.");
        let explanation = sanitize_commit_explanation("");
        assert!(!explanation.is_empty());
        let explanation = sanitize_commit_explanation("   \n```\n  ");
        assert!(!explanation.is_empty());
        let explanation = sanitize_commit_explanation("a\0b");
        assert!(!explanation.contains('\0'));
        let long = "x".repeat(MAX_COMMIT_EXPLANATION_CHARS + 100);
        let explanation = sanitize_commit_explanation(&long);
        assert!(explanation.chars().count() <= MAX_COMMIT_EXPLANATION_CHARS);
    }
}
