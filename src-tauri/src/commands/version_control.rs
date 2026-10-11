//! Version-control IPC commands: the Tauri side of git inspection and guarded
//! writes.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves the
//! effective workspace root (the stored `agent.workspace_root` or the default
//! `agent_workspace` directory, exactly like the workspace commands),
//! delegates to the application-layer version-control service
//! ([`crate::application::version_control`]), and maps failures into
//! secret-free [`CommandError`] values. No business logic lives here beyond
//! that translation.
//!
//! Command-shape decision (one feature area, nine commands): `git_info`
//! batches branch + status + log in a single round trip because the panel
//! always renders them together, while `git_file_diff` loads each unified
//! diff lazily on selection (diffs are size-capped server-side, but there is
//! no reason to fetch all of them up front). The timeline detail loads one
//! historical commit's unified diff lazily through `git_commit_diff` (same
//! cap shape). The four writes stay separate —
//! `git_stage` / `git_unstage` take a path batch plus an explicit
//! `confirmed` flag, `git_commit` takes the validated message plus
//! `confirmed` and returns the new hash, `git_push` takes the allowlisted
//! remote name plus `confirmed` — because each carries different arguments
//! and the panel invokes them independently. `git_generate_commit_message`
//! takes the provider/model names and runs the blocking AI round trip on the
//! blocking pool (like `send_message`); `git_explain_commit` takes a commit
//! hash plus the provider/model names and explains that commit the same way
//! (like `terminal_explain`: copy-only result, never auto-applied).
//!
//! Approval-gate note: the agent `ApprovalGate` parks live agent tool calls
//! and cannot apply to direct IPC commands (no run, no park, no autonomy
//! mode). Writes therefore reuse the destructive-action confirmation pattern:
//! every write refuses with `ConfirmationRequired` unless its per-call
//! `confirmed` flag is `true` — the same gate shape as data management,
//! not a parallel mechanism.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};

use crate::application::version_control::{
    CommitExplainError, CommitExplanation, GeneratedCommitMessage, GitCommitDiff, GitFileDiff,
    GitInfo,
};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Aggregate read-only git view for the effective workspace root: current
/// branch, changed files, and the `limit` most recent commits (backend clamps
/// the limit; the frontend passes a small page such as 20).
///
/// # Threading
///
/// Like `send_message`, every git command below is blocking end to end
/// (subprocess spawns plus diff reads), so each body runs on the runtime's
/// dedicated blocking pool via [`tauri::async_runtime::spawn_blocking`]:
/// plain OS threads with no ambient async context. Synchronous commands
/// would run on the main thread and freeze the whole window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn git_info(limit: Option<u32>, app: AppHandle) -> Result<GitInfo, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_info(&root, limit).map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("git_info blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Per-file unified diff for `path` (repository-relative), capped server-side
/// with a truncation notice. The path is validated to stay inside the
/// repository workdir; traversal attempts fail with a fixed-vocabulary error.
///
/// Runs on the blocking pool like [`git_info`].
#[tauri::command]
pub(crate) async fn git_file_diff(
    path: String,
    app: AppHandle,
) -> Result<GitFileDiff, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_file_diff(&root, path.as_str())
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_file_diff blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Per-commit unified diff for the timeline detail view: `hash` is the full
/// commit hash from `GitInfo.commits` (prefixes are never guessed). The diff
/// is capped server-side with a truncation notice; binary deltas are omitted
/// with a placeholder. Unknown or malformed hashes fail with a
/// fixed-vocabulary error.
///
/// Runs on the blocking pool like [`git_info`].
#[tauri::command]
pub(crate) async fn git_commit_diff(
    hash: String,
    app: AppHandle,
) -> Result<GitCommitDiff, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_commit_diff(&root, hash.as_str())
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_commit_diff blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Stage `paths` (repository-relative) into the index, returning how many
/// were staged. Refuses with `ConfirmationRequired` unless `confirmed` is
/// `true`; escaping paths fail with a fixed-vocabulary error.
///
/// Runs on the blocking pool like [`git_info`].
#[tauri::command]
pub(crate) async fn git_stage(
    paths: Vec<String>,
    confirmed: bool,
    app: AppHandle,
) -> Result<usize, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_stage(&root, &paths, confirmed)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_stage blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Unstage `paths` (repository-relative) back to `HEAD`, returning how many
/// were unstaged. Same confirmation and path guards as [`git_stage`].
///
/// Runs on the blocking pool like [`git_info`].
#[tauri::command]
pub(crate) async fn git_unstage(
    paths: Vec<String>,
    confirmed: bool,
    app: AppHandle,
) -> Result<usize, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_unstage(&root, &paths, confirmed)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_unstage blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Commit the staged index with `message`, returning the new commit hash.
/// The message is validated backend-side (non-empty, bounded, usable subject
/// line); the author comes from the local git config only. Refuses with
/// `ConfirmationRequired` unless `confirmed` is `true`.
///
/// Runs on the blocking pool like [`git_info`].
#[tauri::command]
pub(crate) async fn git_commit(
    message: String,
    confirmed: bool,
    app: AppHandle,
) -> Result<String, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_commit(&root, message.as_str(), confirmed)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_commit blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Push the current branch to `remote` (only `"origin"` is accepted).
/// Never forced: the backend builds a plain fast-forward refspec and no force
/// flag exists on the path. Refuses with `ConfirmationRequired` unless
/// `confirmed` is `true`.
///
/// Runs on the blocking pool like [`git_info`]; the network round trip must
/// never sit on the main thread.
#[tauri::command]
pub(crate) async fn git_push(
    remote: String,
    confirmed: bool,
    app: AppHandle,
) -> Result<(), CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::git_push(&root, remote.as_str(), confirmed)
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("git_push blocking task failed: {err}");
            Err(CommandError::new(ErrorKind::Io, "the git operation failed"))
        }
    }
}

/// Generate a conventional-commit message for the staged changes through the
/// existing AI execution path (keyring-only credentials, nothing persisted).
///
/// # Threading
///
/// Like `send_message`, the provider round trip is blocking end to end, so
/// the body runs on the runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn git_generate_commit_message(
    provider: String,
    model: String,
    app: AppHandle,
) -> Result<GeneratedCommitMessage, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::generate_commit_message(
            db.inner(),
            &root,
            provider.as_str(),
            model.as_str(),
        )
        .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("git_generate_commit_message blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the commit message could not be generated",
            ))
        }
    }
}

/// Explain one historical commit: what it changed and why it matters, through
/// the existing AI execution path (keyring-only credentials, nothing
/// persisted). The commit diff is capped server-side with a truncation
/// notice; the returned text is copy-only — never auto-applied.
///
/// Like `send_message` and `terminal_explain`, the provider round trip is
/// blocking end to end, so the body runs on the runtime's dedicated blocking
/// pool. The commit diff may contain secrets: it is capped and truncated,
/// never logged, and never echoed by the classified error.
///
/// # Errors
///
/// Classified [`CommandError`]s for malformed/unknown hashes and commits
/// with no changed files (`InvalidInput`) or AI execution failures
/// (`Request`: unknown provider, missing credentials, provider failure).
#[tauri::command]
pub(crate) async fn git_explain_commit(
    commit_hash: String,
    provider: String,
    model: String,
    app: AppHandle,
) -> Result<CommitExplanation, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::version_control::explain_commit(
            db.inner(),
            &root,
            commit_hash.as_str(),
            provider.as_str(),
            model.as_str(),
        )
        .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("git_explain_commit blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the commit explanation could not be generated",
            ))
        }
    }
}

impl From<CommitExplainError> for CommandError {
    fn from(err: CommitExplainError) -> Self {
        match err {
            // Both sides stay secret-free: the commit diff is never echoed.
            CommitExplainError::InvalidInput => Self::new(
                ErrorKind::InvalidInput,
                "the commit diff to explain is invalid",
            ),
            CommitExplainError::Request(inner) => Self::from(inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::version_control;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn commit_explain_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(CommitExplainError::InvalidInput);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert_eq!(mapped.message, "the commit diff to explain is invalid");
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(CommitExplainError::Request(
            crate::application::execution::RequestError::UnknownProvider {
                name: "openai".to_string(),
            },
        ));
        assert_eq!(mapped.kind, ErrorKind::Request);
        assert!(safe_message(&mapped));
    }

    /// Static wiring check: the commit-explain path must reuse the shared AI
    /// execution boundary (`RequestExecutionService`) with a tool-free
    /// request — no new provider plumbing — and the explanation must never
    /// reach a write path (no auto-apply of AI text). The command bodies need
    /// `State<'_, _>` and cannot be invoked here, so this source check pins
    /// the delegation instead. Needles are built with `concat!` so this
    /// test's own source never matches them verbatim.
    #[test]
    fn commit_explain_reuses_the_shared_ai_boundary_without_auto_execution() {
        const SOURCE: &str = include_str!("version_control.rs");
        const SERVICE: &str = include_str!("../application/version_control.rs");
        assert!(
            SERVICE.contains("RequestExecutionService::new("),
            "the explain service must execute through the shared boundary"
        );
        for needle in ["git_explain_commit", "explain_commit"] {
            assert!(
                SOURCE.contains(needle) || SERVICE.contains(needle),
                "the explain path must exist, missing {needle:?}"
            );
        }
        // The explanation must never be fed back into a write: no stage,
        // commit, or push call may originate from the explain result on
        // either side. Needles are built with `concat!` so this test's own
        // source never matches them verbatim.
        for needle in [
            concat!("explanation", "_for_commit"),
            concat!("auto", "_apply"),
            concat!("apply", "_explanation"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "no auto-apply path may exist, found {needle:?}"
            );
            assert!(
                !SERVICE.contains(needle),
                "no auto-apply path may exist, found {needle:?}"
            );
        }
    }

    /// Static wiring check: the timeline read (`git_commit_diff`) stays a
    /// pure git read — no writes, no process spawning, no network of its own.
    #[test]
    fn commit_diff_read_stays_a_pure_git_read() {
        const SOURCE: &str = include_str!("version_control.rs");
        assert!(
            SOURCE.contains("git_commit_diff("),
            "git_commit_diff must delegate to the application-layer service"
        );
        for needle in [
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/version_control.rs must not spawn processes itself, found {needle:?}"
            );
        }
    }

    /// Static wiring check (NEX-IPC-001): every blocking git command is
    /// declared `async` and moves its subprocess/diff work onto the runtime
    /// blocking pool, matching the `send_message` precedent. The two AI git
    /// commands already had this shape; the seven below are the conversion.
    #[test]
    fn blocking_git_commands_run_on_the_blocking_pool() {
        const SOURCE: &str = include_str!("version_control.rs");
        for name in [
            "git_info",
            "git_file_diff",
            "git_commit_diff",
            "git_stage",
            "git_unstage",
            "git_commit",
            "git_push",
        ] {
            assert!(
                SOURCE.contains(&format!("async fn {name}(")),
                "{name} must be an async command so it never runs on the main thread"
            );
        }
        assert!(
            SOURCE.contains("spawn_blocking"),
            "the git commands must move blocking work onto the blocking pool"
        );
    }

    /// Boundary-smoke scaffolding: scratch repositories under the process
    /// temp dir (mirrors the application-layer fixtures, but owned here so
    /// the command boundary shape is exercised, not the service alone).
    #[cfg(test)]
    mod boundary {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        pub(super) fn temp_repo_dir() -> PathBuf {
            // Uniqueness comes from a nanos timestamp plus the in-process
            // counter (this file textually pins "no process spawning", so no
            // process-id helper may be named here, even in comments).
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock reads")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("nexora-vcs-cmd-test-{nanos}-{id}"));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            dir
        }

        pub(super) fn signature() -> git2::Signature<'static> {
            git2::Signature::now("nexora-test", "nexora-test@example.com").expect("signs")
        }

        /// Initialise a repo at `dir` with `rel` committed (explicit
        /// signature: no global git config needed).
        pub(super) fn init_repo_with_file(dir: &Path, rel: &str, content: &[u8]) {
            let repo = git2::Repository::init(dir).expect("init scratch repo");
            let full = dir.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("create test parent");
            }
            std::fs::write(&full, content).expect("seed test file");
            let mut index = repo.index().expect("test index");
            index.add_path(Path::new(rel)).expect("stage test file");
            index.write().expect("write test index");
            let tree_id = index.write_tree().expect("write test tree");
            let tree = repo.find_tree(tree_id).expect("find test tree");
            let signature = signature();
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "test: seed commit",
                &tree,
                &[],
            )
            .expect("seed commit");
        }

        /// Drive one `'static` closure through the production scheduling
        /// shape (`spawn_blocking` awaited from the async runtime) and join
        /// it, proving the moved command body completes off-thread.
        pub(super) fn through_boundary<T, F>(work: F) -> T
        where
            T: Send + 'static,
            F: FnOnce() -> T + Send + 'static,
        {
            tauri::async_runtime::block_on(async {
                tauri::async_runtime::spawn_blocking(work)
                    .await
                    .expect("blocking task joins without panicking")
            })
        }
    }

    /// Boundary smoke: the read commands (`git_info`, `git_file_diff`,
    /// `git_commit_diff`) complete through the async boundary on a real
    /// repository with a committed file plus a dirty workdir edit.
    #[test]
    fn git_reads_complete_through_the_async_boundary() {
        let dir = boundary::temp_repo_dir();
        boundary::init_repo_with_file(&dir, "note.txt", b"hello\n");
        std::fs::write(dir.join("note.txt"), "hello world\n").expect("dirty workdir");

        let info = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_info(&dir, None)
        })
        .expect("git_info succeeds through the boundary");
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.commits.len(), 1);

        let file_diff = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_file_diff(&dir, "note.txt")
        })
        .expect("git_file_diff succeeds through the boundary");
        assert!(
            file_diff.diff.contains("hello world"),
            "the workdir edit surfaces in the diff"
        );

        let hash = info.commits[0].hash.clone();
        let commit_diff = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_commit_diff(&dir, hash.as_str())
        })
        .expect("git_commit_diff succeeds through the boundary");
        assert!(
            commit_diff.diff.contains("hello"),
            "the committed content surfaces in the commit diff"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Boundary smoke: the write commands (`git_stage`, `git_unstage`,
    /// `git_commit`) complete through the async boundary with the same
    /// staged counts and commit hash the synchronous path produced.
    #[test]
    fn git_writes_complete_through_the_async_boundary() {
        let dir = boundary::temp_repo_dir();
        let repo = git2::Repository::init(&dir).expect("init scratch repo");
        // Repo-local identity so `git_commit` (which reads the local
        // config via `repo.signature()`) succeeds without any global git
        // config, as on CI runners.
        {
            let mut config = repo.config().expect("open scratch config");
            config
                .set_str("user.name", "Nexora Test")
                .expect("set scratch name");
            config
                .set_str("user.email", "test@nexora.local")
                .expect("set scratch email");
        }
        drop(repo);
        std::fs::write(dir.join("note.txt"), "hello\n").expect("seed untracked");

        let staged = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_stage(&dir, &["note.txt".to_string()], true)
        })
        .expect("git_stage succeeds through the boundary");
        assert_eq!(staged, 1);

        let unstaged = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_unstage(&dir, &["note.txt".to_string()], true)
        })
        .expect("git_unstage succeeds through the boundary");
        assert_eq!(unstaged, 1);

        let restaged = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_stage(&dir, &["note.txt".to_string()], true)
        })
        .expect("git_stage succeeds through the boundary");
        assert_eq!(restaged, 1);

        let hash = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_commit(&dir, "test: add note", true)
        })
        .expect("git_commit succeeds through the boundary");
        assert!(!hash.is_empty(), "the commit returns its hash");

        let info = boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_info(&dir, None)
        })
        .expect("git_info succeeds through the boundary");
        assert!(info.files.is_empty(), "the workdir is clean after commit");
        assert_eq!(info.commits.len(), 1);
        assert_eq!(info.commits[0].hash, hash);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Boundary smoke: `git_push` completes through the async boundary
    /// against a local bare `origin` (fully offline) and the remote tracks
    /// the local head.
    #[test]
    fn git_push_completes_through_the_async_boundary() {
        let dir = boundary::temp_repo_dir();
        let repo = git2::Repository::init(&dir).expect("init scratch repo");
        let bare_dir = boundary::temp_repo_dir();
        git2::Repository::init_bare(&bare_dir).expect("init bare remote");
        repo.remote("origin", bare_dir.to_str().expect("bare path is unicode"))
            .expect("add origin remote");
        drop(repo);
        boundary::init_repo_with_file(&dir, "note.txt", b"hello\n");

        boundary::through_boundary({
            let dir = dir.clone();
            move || version_control::git_push(&dir, "origin", true)
        })
        .expect("git_push succeeds through the boundary");

        let local = git2::Repository::open(&dir).expect("open local");
        let local_oid = local
            .head()
            .expect("local head")
            .peel_to_commit()
            .expect("local commit")
            .id();
        let bare = git2::Repository::open_bare(&bare_dir).expect("open bare");
        let branch = local
            .head()
            .expect("local head")
            .shorthand()
            .expect("branch name")
            .to_string();
        let remote_oid = bare
            .find_reference(&format!("refs/heads/{branch}"))
            .expect("remote branch exists")
            .peel_to_commit()
            .expect("remote commit")
            .id();
        assert_eq!(local_oid, remote_oid);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(bare_dir);
    }
}
