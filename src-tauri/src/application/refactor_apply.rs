//! Safe refactor application: the first WRITE path toward the workspace.
//!
//! Allowlist decision (Phase 1, binding — documented here, enforced below):
//! the ONLY appliable finding kind is the repo-audit engine's
//! `dead-code-candidate` ([`crate::application::repo_audit`]) on Rust
//! (`.rs`) files, applied as an explicit per-line-range removal the user
//! pastes and confirms. Everything else is NEVER auto-applied and refuses
//! with [`RefactorApplyError::UnsafeKind`]:
//!
//! - `todo-debt` / `FIXME` markers — intent is unknowable statically; only
//!   the author knows the fix.
//! - `unwrap-hotspot` — replacing a panic path changes runtime behavior.
//! - `oversized-file` / `oversized-function` — splits are design decisions.
//! - `missing-docs` — generated docs would be invented content.
//! - `error-swallowed` — changing error handling changes behavior.
//! - `unchecked-result` — removing a suppression changes lint behavior.
//! - `suspicious-clone` — removing a clone changes ownership behavior.
//! - TypeScript files entirely — TS refactoring is out of scope (Rust only);
//!   the audit engine scans TS as text without type information.
//! - Formatting-adjacent cleanups — `rustfmt` owns those, never this path.
//!
//! Write-path guards (safety first — the bar is highest here because this is
//! the first command that mutates workspace files):
//!
//! - Git-write surface choice: `git2` directly (the existing `git2 = "0.21"`
//!   dependency, same as [`crate::application::version_control`]). No process
//!   spawning (`std::process::Command` appears nowhere here), no new dep.
//!   `version_control.rs` was NOT extended because its private helpers own
//!   the git-panel surface; this module owns its own guards instead.
//! - Git-repo precondition: the workspace root must discover an enclosing
//!   non-bare repository, or the apply refuses with
//!   [`RefactorApplyError::NotARepository`] — outside git, every apply
//!   refuses with a clean error.
//! - Clean-file precondition: the touched file must be tracked and
//!   unmodified (`git2` status empty), so the post-apply `git diff` is
//!   exactly this apply — reversibility proof. Dirty, untracked, or ignored
//!   files refuse with [`RefactorApplyError::UncleanFile`].
//! - Explicit user confirm: `confirmed: false` refuses without touching the
//!   filesystem ([`RefactorApplyError::Unconfirmed`]) — the same per-call
//!   confirmation gate as the git writes, not a parallel mechanism.
//! - Declaration-line check: the first removed line must trim-start to
//!   `pub ` (a public-item declaration — the dead-code candidate shape), or
//!   the apply refuses with [`RefactorApplyError::ContentMismatch`]. The
//!   range is capped at [`MAX_APPLY_LINES`] lines so one apply cannot gut a
//!   file.
//! - Post-apply verification hook shape: the result carries the re-read line
//!   count plus a `verified` flag (removed snippet absent, line count moved
//!   exactly by the removed span). Revert path: the file was clean before,
//!   so `git checkout -- <path>` (or `git diff`) restores it — the panel
//!   states this on every apply.
//! - Best-effort check-then-write: the guards run before the write, so a
//!   concurrent external writer could interleave between check and write —
//!   re-run `git diff` after applying to confirm the change is exactly this
//!   removal.
//!
//! Errors are fixed-vocabulary with no paths, no file content, and no diff
//! text (workspace content may contain user names or secrets).

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::application::agent::tools::is_within_workspace;
use crate::application::workspace::strip_verbatim;

/// The only finding kinds this path may apply, in frontend display order.
/// Every other audit kind refuses with [`RefactorApplyError::UnsafeKind`].
pub(crate) const APPLIABLE_KINDS: [&str; 1] = ["dead-code-candidate"];

/// Most lines one apply may remove (1-based inclusive range). The cap keeps
/// one confirmed apply from gutting a file.
pub(crate) const MAX_APPLY_LINES: usize = 10;

/// Longest removed-text preview returned in the result (a preview, never the
/// whole file).
pub(crate) const MAX_REMOVED_PREVIEW_BYTES: usize = 8 * 1024;

/// Longest workspace-relative `path` argument accepted.
const MAX_PATH_LEN: usize = 1024;

/// Secret-free failures for the safe-apply path. Variants carry no payload,
/// so formatting one can never leak a path, file content, or credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefactorApplyError {
    /// The workspace is not inside a git repository (or it is bare).
    NotARepository,
    /// The apply was invoked without the explicit per-call confirmation.
    Unconfirmed,
    /// The finding kind is outside the [`APPLIABLE_KINDS`] allowlist.
    UnsafeKind,
    /// A caller-supplied path escaped the repository workdir, was overlong,
    /// or was not a Rust source file.
    InvalidPath,
    /// The touched file is untracked, ignored, or already modified.
    UncleanFile,
    /// The line range is unusable (empty, inverted, over the cap, or past
    /// the end of the file).
    InvalidRange,
    /// The target lines no longer look like a public-item declaration.
    ContentMismatch,
    /// A git operation failed.
    GitFailed,
    /// A filesystem read or write failed.
    Io,
}

impl std::fmt::Display for RefactorApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository => {
                write!(f, "the workspace is not inside a git repository")
            }
            Self::Unconfirmed => write!(
                f,
                "explicit confirmation is required before a refactor can be applied"
            ),
            Self::UnsafeKind => write!(
                f,
                "this finding kind is never auto-applied; only confirmed dead-code removals apply"
            ),
            Self::InvalidPath => write!(f, "the file path is invalid"),
            Self::UncleanFile => write!(
                f,
                "the file has uncommitted changes or is untracked; commit or revert first"
            ),
            Self::InvalidRange => write!(f, "the line range is invalid"),
            Self::ContentMismatch => write!(
                f,
                "the target lines no longer match a public item declaration"
            ),
            Self::GitFailed => write!(f, "the git operation failed"),
            Self::Io => write!(f, "the file could not be read or written"),
        }
    }
}

impl std::error::Error for RefactorApplyError {}

/// One applied refactor: what was removed, the re-read file shape, and the
/// verification flag. Revert with `git checkout -- <path>` — the file was
/// clean before the apply, so the working-tree diff is exactly this removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RefactorApplyResult {
    /// Workspace-relative forward-slash path, as requested.
    pub path: String,
    /// 1-based first removed line.
    pub start_line: usize,
    /// 1-based last removed line (inclusive).
    pub end_line: usize,
    /// Lines removed (`end_line - start_line + 1`).
    pub removed_lines: usize,
    /// File line count after the apply (re-read from disk).
    pub file_lines: usize,
    /// Removed text preview (capped at [`MAX_REMOVED_PREVIEW_BYTES`]).
    pub removed_preview: String,
    /// Post-apply verification: the re-read line count moved by exactly the
    /// removed span and the removed declaration line is absent.
    pub verified: bool,
}

/// Validate a caller-supplied workspace-relative Rust path. Rejects absolute
/// paths (both separators, including drive and UNC forms), parent
/// references, `.git` components, null bytes, empty and overlong input, and
/// non-`.rs` files — all with the single fixed-vocabulary
/// [`RefactorApplyError::InvalidPath`].
///
/// # Errors
///
/// Returns [`RefactorApplyError::InvalidPath`] for any rejected shape.
fn clean_apply_path(raw: &str) -> Result<PathBuf, RefactorApplyError> {
    if raw.contains('\0') {
        return Err(RefactorApplyError::InvalidPath);
    }
    let normalized = raw.trim().replace('\\', "/");
    if normalized.is_empty() || normalized.len() > MAX_PATH_LEN {
        return Err(RefactorApplyError::InvalidPath);
    }
    if normalized.starts_with('/') {
        return Err(RefactorApplyError::InvalidPath);
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(RefactorApplyError::InvalidPath);
    }
    let mut rel = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(_) => rel.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RefactorApplyError::InvalidPath);
            }
        }
    }
    if rel.as_os_str().is_empty() {
        return Err(RefactorApplyError::InvalidPath);
    }
    if rel
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(RefactorApplyError::InvalidPath);
    }
    if rel.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return Err(RefactorApplyError::InvalidPath);
    }
    Ok(rel)
}

/// Truncate the removed-text preview to [`MAX_REMOVED_PREVIEW_BYTES`]
/// (char-boundary safe).
#[must_use]
fn truncate_preview(text: &str) -> String {
    if text.len() <= MAX_REMOVED_PREVIEW_BYTES {
        return text.to_string();
    }
    let mut end = MAX_REMOVED_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Remove 1-based inclusive lines `[start, end]` from `lines`, returning the
/// removed text. Caller bounds-checks; this function only splices.
fn splice_lines(lines: &[String], start: usize, end: usize) -> (String, String) {
    let removed: Vec<&str> = lines[start - 1..end].iter().map(String::as_str).collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len().saturating_sub(end - start + 1));
    kept.extend(lines[..start - 1].iter().map(String::as_str));
    kept.extend(lines[end..].iter().map(String::as_str));
    (removed.join("\n"), kept.join("\n"))
}

/// Apply one confirmed dead-code removal: delete workspace-relative `.rs`
/// lines `[start_line, end_line]` (1-based, inclusive) for a finding of an
/// [`APPLIABLE_KINDS`] kind.
///
/// Preconditions (in order): explicit `confirmed`, allowlisted `kind`, valid
/// Rust path inside a non-bare enclosing git repository, tracked-and-clean
/// file, usable range, and a `pub `-declaration first line. The file is
/// re-read after the write for the verification flag.
///
/// # Errors
///
/// See [`RefactorApplyError`]: every guard refuses before any write, and
/// outside git the apply refuses with [`RefactorApplyError::NotARepository`].
pub(crate) fn refactor_apply(
    workspace_root: &Path,
    raw_path: &str,
    start_line: usize,
    end_line: usize,
    kind: &str,
    confirmed: bool,
) -> Result<RefactorApplyResult, RefactorApplyError> {
    if !confirmed {
        return Err(RefactorApplyError::Unconfirmed);
    }
    if !APPLIABLE_KINDS.contains(&kind) {
        return Err(RefactorApplyError::UnsafeKind);
    }
    let rel = clean_apply_path(raw_path)?;
    if start_line == 0
        || end_line == 0
        || end_line < start_line
        || end_line - start_line + 1 > MAX_APPLY_LINES
    {
        return Err(RefactorApplyError::InvalidRange);
    }
    let canon_ws = std::fs::canonicalize(workspace_root)
        .map(strip_verbatim)
        .map_err(|_| RefactorApplyError::GitFailed)?;
    let repo =
        git2::Repository::discover(&canon_ws).map_err(|_| RefactorApplyError::NotARepository)?;
    if repo.workdir().is_none() {
        return Err(RefactorApplyError::NotARepository);
    }
    let canon_repo = repo
        .workdir()
        .ok_or(RefactorApplyError::NotARepository)
        .and_then(|workdir| {
            std::fs::canonicalize(workdir)
                .map(strip_verbatim)
                .map_err(|_| RefactorApplyError::GitFailed)
        })?;
    let joined = canon_ws.join(&rel);
    if !is_within_workspace(&canon_ws, &joined) || !is_within_workspace(&canon_repo, &joined) {
        return Err(RefactorApplyError::InvalidPath);
    }
    // Symlink guard: `fs::write` follows symlinks, so a lexical containment
    // check alone could mutate a target outside the repository — a committed
    // `.rs` symlink pointing elsewhere refuses before any read.
    if std::fs::symlink_metadata(&joined).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(RefactorApplyError::InvalidPath);
    }
    // Parent-dir symlinks resolve too: the canonical target must stay inside
    // both the workspace and the repository workdir. A missing file skips
    // this (later guards refuse it); nothing here relaxes a guard.
    if let Ok(canon_target) = std::fs::canonicalize(&joined).map(strip_verbatim) {
        if !is_within_workspace(&canon_ws, &canon_target)
            || !is_within_workspace(&canon_repo, &canon_target)
        {
            return Err(RefactorApplyError::InvalidPath);
        }
    }
    let spec = rel.to_string_lossy().replace('\\', "/");
    // The git status path is repository-relative (the workspace root may be a
    // subdirectory of the workdir); the result keeps the workspace-relative
    // `spec` the caller passed.
    let repo_spec = joined
        .strip_prefix(&canon_repo)
        .map_err(|_| RefactorApplyError::InvalidPath)?
        .to_string_lossy()
        .replace('\\', "/");
    let status = repo
        .status_file(Path::new(&repo_spec))
        .map_err(|_| RefactorApplyError::GitFailed)?;
    if !status.is_empty() {
        return Err(RefactorApplyError::UncleanFile);
    }
    let text = std::fs::read_to_string(&joined).map_err(|_| RefactorApplyError::Io)?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    if end_line > lines.len() {
        return Err(RefactorApplyError::InvalidRange);
    }
    let first = lines[start_line - 1].trim_start();
    if !first.starts_with("pub ") {
        return Err(RefactorApplyError::ContentMismatch);
    }
    let (removed, kept) = splice_lines(&lines, start_line, end_line);
    let mut next = kept;
    if text.ends_with('\n') && !next.is_empty() {
        next.push('\n');
    }
    std::fs::write(&joined, &next).map_err(|_| RefactorApplyError::Io)?;
    // Post-apply verification hook: re-read from disk and confirm the shape
    // moved by exactly the removed span with the declaration line gone.
    let reread = std::fs::read_to_string(&joined).map_err(|_| RefactorApplyError::Io)?;
    let reread_lines: Vec<&str> = reread.lines().collect();
    let declaration = first.to_string();
    let verified = reread_lines.len() + (end_line - start_line + 1) == lines.len()
        && !reread_lines
            .iter()
            .any(|line| line.trim_start() == declaration);
    Ok(RefactorApplyResult {
        path: spec,
        start_line,
        end_line,
        removed_lines: end_line - start_line + 1,
        file_lines: reread_lines.len(),
        removed_preview: truncate_preview(&removed),
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "nexora-refactor-apply-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("test workspace creates");
        root
    }

    fn with_cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    /// Initialize a git repository at `root` with `rel` committed clean, so
    /// the apply preconditions (repo presence, tracked-clean file) hold.
    fn init_repo_with_file(root: &Path, rel: &str, content: &str) {
        let repo = git2::Repository::init(root).expect("repo inits");
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("test parent creates");
        }
        std::fs::write(&full, content).expect("test file writes");
        let mut index = repo.index().expect("index reads");
        index.add_path(Path::new(rel)).expect("path stages");
        index.write().expect("index writes");
        let tree_id = index.write_tree().expect("tree writes");
        let tree = repo.find_tree(tree_id).expect("tree reads");
        let signature =
            git2::Signature::now("nexora-test", "nexora-test@example.com").expect("signs");
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "test commit",
            &tree,
            &[],
        )
        .expect("commits");
    }

    fn apply(
        root: &Path,
        path: &str,
        start: usize,
        end: usize,
        kind: &str,
        confirmed: bool,
    ) -> Result<RefactorApplyResult, RefactorApplyError> {
        refactor_apply(root, path, start, end, kind, confirmed)
    }

    /// Stage `rels` (repository-relative) and commit on top of HEAD, for
    /// tests that need more than one commit (symlink and subdir shapes).
    fn commit_paths(root: &Path, rels: &[&str], message: &str) {
        let repo = git2::Repository::open(root).expect("repo opens");
        let mut index = repo.index().expect("index reads");
        for rel in rels {
            index.add_path(Path::new(rel)).expect("path stages");
        }
        index.write().expect("index writes");
        let tree_id = index.write_tree().expect("tree writes");
        let tree = repo.find_tree(tree_id).expect("tree reads");
        let signature =
            git2::Signature::now("nexora-test", "nexora-test@example.com").expect("signs");
        let head = repo
            .head()
            .ok()
            .and_then(|reference| reference.peel_to_commit().ok());
        let mut parents = Vec::new();
        if let Some(ref commit) = head {
            parents.push(commit);
        }
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .expect("commits");
    }

    #[test]
    fn outside_git_refuses_cleanly() {
        let root = test_root();
        std::fs::write(root.join("lonely.rs"), "pub fn lonely() {}\n").expect("writes");
        let err = apply(&root, "lonely.rs", 1, 1, "dead-code-candidate", true)
            .expect_err("outside git must refuse");
        assert_eq!(err, RefactorApplyError::NotARepository);
        assert!(
            !format!("{err}").contains("lonely"),
            "the refusal carries no path"
        );
        with_cleanup(&root);
    }

    #[test]
    fn unconfirmed_and_unsafe_kinds_refuse_before_any_write() {
        let root = test_root();
        init_repo_with_file(&root, "src/lib.rs", "/// Entry.\npub fn orphaned() {}\n");
        for kind in [
            "unwrap-hotspot",
            "todo-debt",
            "oversized-file",
            "oversized-function",
            "missing-docs",
            "error-swallowed",
            "unchecked-result",
            "suspicious-clone",
            "invented-kind",
        ] {
            let err =
                apply(&root, "src/lib.rs", 2, 2, kind, true).expect_err("unsafe kinds must refuse");
            assert_eq!(err, RefactorApplyError::UnsafeKind, "kind {kind:?}");
        }
        let err = apply(&root, "src/lib.rs", 2, 2, "dead-code-candidate", false)
            .expect_err("unconfirmed must refuse");
        assert_eq!(err, RefactorApplyError::Unconfirmed);
        let content = std::fs::read_to_string(root.join("src/lib.rs")).expect("reads");
        assert_eq!(
            content, "/// Entry.\npub fn orphaned() {}\n",
            "refusals write nothing"
        );
        with_cleanup(&root);
    }

    #[test]
    fn non_rust_paths_and_escapes_refuse() {
        let root = test_root();
        init_repo_with_file(&root, "src/lib.rs", "/// Entry.\npub fn orphaned() {}\n");
        for path in [
            "src/panel.ts",
            "../escape.rs",
            "C:/windows/path.rs",
            "",
            ".git/hooks/x.rs",
            "src/lib.rs ",
        ] {
            let trimmed_ok = path == "src/lib.rs ";
            let outcome = apply(&root, path, 1, 1, "dead-code-candidate", true);
            if trimmed_ok {
                // Trailing whitespace trims to a valid path — but line 1 is
                // the doc comment, so it must refuse on content instead.
                assert_eq!(
                    outcome.expect_err("doc-comment line must refuse"),
                    RefactorApplyError::ContentMismatch
                );
            } else {
                assert_eq!(
                    outcome.expect_err("bad paths must refuse"),
                    RefactorApplyError::InvalidPath,
                    "path {path:?}"
                );
            }
        }
        with_cleanup(&root);
    }

    #[test]
    fn dirty_or_untracked_files_refuse() {
        let root = test_root();
        init_repo_with_file(&root, "src/lib.rs", "/// Entry.\npub fn orphaned() {}\n");
        std::fs::write(
            root.join("src/lib.rs"),
            "/// Entry.\npub fn orphaned() {}\n// dirty\n",
        )
        .expect("dirties");
        let err = apply(&root, "src/lib.rs", 2, 2, "dead-code-candidate", true)
            .expect_err("dirty files must refuse");
        assert_eq!(err, RefactorApplyError::UncleanFile);
        std::fs::write(root.join("src/fresh.rs"), "pub fn fresh() {}\n").expect("writes");
        let err = apply(&root, "src/fresh.rs", 1, 1, "dead-code-candidate", true)
            .expect_err("untracked files must refuse");
        assert_eq!(err, RefactorApplyError::UncleanFile);
        with_cleanup(&root);
    }

    #[test]
    fn ranges_and_non_declaration_lines_refuse() {
        let root = test_root();
        init_repo_with_file(
            &root,
            "src/lib.rs",
            "/// Entry.\npub fn orphaned() {}\nfn private() {}\n",
        );
        // Zero, inverted, past-the-end, and over-cap ranges.
        for (start, end) in [(0, 1), (3, 2), (1, 99), (1, MAX_APPLY_LINES + 1)] {
            let err = apply(&root, "src/lib.rs", start, end, "dead-code-candidate", true)
                .expect_err("bad ranges must refuse");
            assert_eq!(
                err,
                RefactorApplyError::InvalidRange,
                "range {start}..{end}"
            );
        }
        // Doc-comment and private lines are not public declarations.
        for line in [1, 3] {
            let err = apply(&root, "src/lib.rs", line, line, "dead-code-candidate", true)
                .expect_err("non-pub lines must refuse");
            assert_eq!(err, RefactorApplyError::ContentMismatch, "line {line}");
        }
        with_cleanup(&root);
    }

    #[test]
    fn committed_symlink_refuses_before_any_write() {
        let root = test_root();
        let outside = test_root();
        let secret = outside.join("secret.rs");
        std::fs::write(&secret, "pub fn secret() {}\n").expect("outside target writes");
        let link_rel = "src/link.rs";
        let link_full = root.join(link_rel);
        if let Some(parent) = link_full.parent() {
            std::fs::create_dir_all(parent).expect("test parent creates");
        }
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&secret, &link_full).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&secret, &link_full).is_ok();
        #[cfg(not(any(unix, windows)))]
        let linked = false;
        if !linked {
            eprintln!("skipping symlink test: the platform refused symlink creation");
            with_cleanup(&root);
            with_cleanup(&outside);
            return;
        }
        git2::Repository::init(&root).expect("repo inits");
        commit_paths(&root, &[link_rel], "commit a symlink");
        let err = apply(&root, link_rel, 1, 1, "dead-code-candidate", true)
            .expect_err("a committed symlink must refuse");
        assert_eq!(err, RefactorApplyError::InvalidPath);
        let content = std::fs::read_to_string(&secret).expect("outside target reads");
        assert_eq!(
            content, "pub fn secret() {}\n",
            "the refusal writes nothing through the link"
        );
        with_cleanup(&root);
        with_cleanup(&outside);
    }

    #[test]
    fn workspace_subdir_of_repo_applies_to_the_right_file() {
        let root = test_root();
        // Decoy: the same workspace-relative shape at the repo root is a
        // shorter file, so joining the path to the repo workdir instead of
        // the workspace root would hit the wrong file (or range-refuse).
        init_repo_with_file(
            &root,
            "src/lib.rs",
            "/// Root entry.\npub fn root_fn() {}\n",
        );
        let workspace = root.join("sub");
        std::fs::create_dir_all(workspace.join("src")).expect("test parent creates");
        std::fs::write(
            workspace.join("src/lib.rs"),
            "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\npub fn orphaned() {}\n",
        )
        .expect("test file writes");
        commit_paths(&root, &["sub/src/lib.rs"], "commit the workspace file");
        let result = apply(&workspace, "src/lib.rs", 4, 4, "dead-code-candidate", true)
            .expect("the subdir-workspace apply runs");
        assert_eq!(result.path, "src/lib.rs");
        assert_eq!(result.removed_lines, 1);
        assert_eq!(result.file_lines, 3);
        assert!(result.verified, "the post-apply re-read verifies");
        let sub_content = std::fs::read_to_string(workspace.join("src/lib.rs")).expect("reads");
        assert_eq!(
            sub_content,
            "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\n"
        );
        let root_content = std::fs::read_to_string(root.join("src/lib.rs")).expect("reads");
        assert_eq!(
            root_content, "/// Root entry.\npub fn root_fn() {}\n",
            "the repo-root decoy stays untouched"
        );
        with_cleanup(&root);
    }

    #[test]
    fn confirmed_dead_code_removal_applies_with_reversibility_proof() {
        let root = test_root();
        init_repo_with_file(
            &root,
            "src/lib.rs",
            "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\npub fn orphaned() {}\n",
        );
        let result = apply(&root, "src/lib.rs", 4, 4, "dead-code-candidate", true)
            .expect("the confirmed apply runs");
        assert_eq!(result.path, "src/lib.rs");
        assert_eq!(result.removed_lines, 1);
        assert_eq!(result.file_lines, 3);
        assert!(result.verified, "the post-apply re-read verifies");
        assert!(
            result.removed_preview.contains("pub fn orphaned()"),
            "the preview names the removal, got {:?}",
            result.removed_preview
        );
        let content = std::fs::read_to_string(root.join("src/lib.rs")).expect("reads");
        assert_eq!(
            content,
            "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\n"
        );
        // Reversibility proof: the working-tree diff is exactly this removal.
        let repo = git2::Repository::open(&root).expect("repo opens");
        let status = repo
            .status_file(Path::new("src/lib.rs"))
            .expect("status reads");
        assert!(
            status.is_wt_modified(),
            "the apply shows as a worktree modification, got {status:?}"
        );
        with_cleanup(&root);
    }

    #[test]
    fn errors_are_secret_free() {
        for err in [
            RefactorApplyError::NotARepository,
            RefactorApplyError::Unconfirmed,
            RefactorApplyError::UnsafeKind,
            RefactorApplyError::InvalidPath,
            RefactorApplyError::UncleanFile,
            RefactorApplyError::InvalidRange,
            RefactorApplyError::ContentMismatch,
            RefactorApplyError::GitFailed,
            RefactorApplyError::Io,
        ] {
            let rendered = format!("{err}");
            for sentinel in ["sk-", "secret", "credential", "api_key", ".rs", "/"] {
                assert!(
                    !rendered.to_lowercase().contains(sentinel),
                    "error {err:?} must stay secret-free, rendered {rendered:?}"
                );
            }
        }
    }
}
