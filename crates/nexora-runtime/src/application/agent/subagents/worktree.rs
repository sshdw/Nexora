//! Parallel write isolation via temporary `git worktree` + branch (`S11-08`).
//!
//! Writing subagents in a Git repository can optionally execute inside an
//! isolated temporary worktree created outside the parent workspace. On
//! completion, tracked and untracked changes are transferred back as a clean
//! patch (`git apply --check` followed by `git apply`, serialized per parent
//! run). Conflicts fail cleanly without corrupting the parent working tree,
//! and worktree + temporary branch cleanup is mandatory on completion, kill,
//! error, and `Drop`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Result of transferring isolated worktree edits back into the parent repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchTransferOutcome {
    /// Subagent produced no workspace edits.
    NoChanges,
    /// Patch applied cleanly into the parent repository working tree.
    Applied { changed_files: Vec<String> },
    /// Patch conflicted with current parent workspace state; parent left untouched.
    Conflict {
        message: String,
        changed_files: Vec<String>,
    },
}

/// RAII guard owning one isolated `git worktree` directory and temporary branch.
#[derive(Debug)]
pub(crate) struct WorktreeIsolation {
    repo_root: PathBuf,
    worktree_path: PathBuf,
    subagent_workspace: PathBuf,
    branch_name: String,
    cleaned: AtomicBool,
}

impl WorktreeIsolation {
    /// Create an isolated worktree + temporary branch rooted at `HEAD`.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error if `workspace_root` is not inside a Git
    /// repository with a valid `HEAD` commit or if `git worktree add` fails.
    pub(crate) fn create(
        workspace_root: &Path,
        parent_run_id: i64,
        subagent_id: &str,
    ) -> Result<Self, String> {
        let top = run_git(workspace_root, &["rev-parse", "--show-toplevel"])?;
        if !top.status.success() {
            return Err(
                "git worktree isolation requires the workspace to be inside a Git repository"
                    .to_string(),
            );
        }
        let repo_root = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim());
        let head = run_git(&repo_root, &["rev-parse", "--verify", "HEAD"])?;
        if !head.status.success() {
            return Err(
                "git worktree isolation requires a Git repository with at least one commit (HEAD)"
                    .to_string(),
            );
        }

        let rel_workspace = relative_workspace(&repo_root, workspace_root);
        let nonce = next_worktree_nonce();
        let safe_id: String = subagent_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .take(32)
            .collect();
        let safe_id = if safe_id.is_empty() {
            "sub".to_string()
        } else {
            safe_id
        };
        let branch_name = format!("nexora/subagent-{parent_run_id}-{safe_id}-{nonce}");
        let base_dir = std::env::temp_dir().join("nexora-subagent-worktrees");
        std::fs::create_dir_all(&base_dir)
            .map_err(|err| format!("failed to prepare worktree directory: {err}"))?;
        let worktree_path = base_dir.join(format!("wt-{parent_run_id}-{safe_id}-{nonce}"));

        let wt_str = worktree_path.to_string_lossy().to_string();
        let add_out = run_git(
            &repo_root,
            &["worktree", "add", "-b", &branch_name, &wt_str, "HEAD"],
        )?;
        if !add_out.status.success() {
            let _ = std::fs::remove_dir_all(&worktree_path);
            let _ = run_git(&repo_root, &["branch", "-D", &branch_name]);
            let stderr = String::from_utf8_lossy(&add_out.stderr).trim().to_string();
            return Err(format!("failed to create git worktree: {stderr}"));
        }

        let worktree_path = canonicalize_path(&worktree_path);

        let subagent_workspace = if rel_workspace.as_os_str().is_empty() {
            worktree_path.clone()
        } else {
            worktree_path.join(rel_workspace)
        };

        Ok(Self {
            repo_root,
            worktree_path,
            subagent_workspace,
            branch_name,
            cleaned: AtomicBool::new(false),
        })
    }

    /// Workspace directory the subagent runner should operate inside.
    #[must_use]
    pub(crate) fn workspace_path(&self) -> &Path {
        &self.subagent_workspace
    }

    /// Root of the isolated temporary worktree (for tests/diagnostics).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn worktree_root(&self) -> &Path {
        &self.worktree_path
    }

    /// Temporary branch name created for this worktree (for tests/diagnostics).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn branch_name(&self) -> &str {
        &self.branch_name
    }

    /// Extract all tracked + untracked edits from the worktree and apply them
    /// cleanly to the parent repository under `apply_lock`.
    ///
    /// Uses `git apply --check` before applying so conflicts fail cleanly
    /// without mutating the parent working tree.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_to_parent(&self, apply_lock: &Mutex<()>) -> PatchTransferOutcome {
        if self.cleaned.load(Ordering::SeqCst) || !self.worktree_path.exists() {
            return PatchTransferOutcome::NoChanges;
        }
        // Stage tracked modifications, deletions, and new untracked files
        // (respecting .gitignore) inside the isolated worktree only.
        match run_git(&self.worktree_path, &["add", "-A"]) {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                return PatchTransferOutcome::Conflict {
                    message: format!(
                        "failed to stage worktree changes: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    changed_files: Vec::new(),
                };
            }
            Err(err) => {
                return PatchTransferOutcome::Conflict {
                    message: err,
                    changed_files: Vec::new(),
                };
            }
        }

        let changed_files = run_git(
            &self.worktree_path,
            &["diff", "--cached", "--name-only", "HEAD"],
        )
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

        let diff_out = match run_git(
            &self.worktree_path,
            &["diff", "--cached", "--binary", "HEAD"],
        ) {
            Ok(out) if out.status.success() => out.stdout,
            Ok(out) => {
                return PatchTransferOutcome::Conflict {
                    message: format!(
                        "failed to compute worktree diff: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    changed_files,
                };
            }
            Err(err) => {
                return PatchTransferOutcome::Conflict {
                    message: err,
                    changed_files,
                };
            }
        };

        if diff_out.is_empty() {
            return PatchTransferOutcome::NoChanges;
        }

        let _guard = apply_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Strict dry-run check first: parent working tree stays untouched on conflict.
        match run_git_with_stdin(&self.repo_root, &["apply", "--check", "-"], &diff_out) {
            Ok(check) if check.status.success() => {}
            Ok(check) => {
                let stderr = String::from_utf8_lossy(&check.stderr).trim().to_string();
                return PatchTransferOutcome::Conflict {
                    message: if stderr.is_empty() {
                        "worktree patch conflicts with current workspace changes".to_string()
                    } else {
                        format!("worktree patch conflict: {stderr}")
                    },
                    changed_files,
                };
            }
            Err(err) => {
                return PatchTransferOutcome::Conflict {
                    message: err,
                    changed_files,
                };
            }
        }

        match run_git_with_stdin(&self.repo_root, &["apply", "-"], &diff_out) {
            Ok(apply) if apply.status.success() => PatchTransferOutcome::Applied { changed_files },
            Ok(apply) => {
                let stderr = String::from_utf8_lossy(&apply.stderr).trim().to_string();
                PatchTransferOutcome::Conflict {
                    message: if stderr.is_empty() {
                        "failed to apply worktree patch cleanly".to_string()
                    } else {
                        format!("worktree patch apply failed: {stderr}")
                    },
                    changed_files,
                }
            }
            Err(err) => PatchTransferOutcome::Conflict {
                message: err,
                changed_files,
            },
        }
    }

    /// Idempotently remove the temporary worktree directory and delete the
    /// temporary branch. Always invoked on completion, kill, error, and `Drop`.
    pub(crate) fn cleanup(&self) {
        if self.cleaned.swap(true, Ordering::SeqCst) {
            return;
        }
        let wt_str = self.worktree_path.to_string_lossy().to_string();
        let _ = run_git(&self.repo_root, &["worktree", "remove", "--force", &wt_str]);
        if self.worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&self.worktree_path);
        }
        let _ = run_git(&self.repo_root, &["worktree", "prune"]);
        let _ = run_git(&self.repo_root, &["branch", "-D", &self.branch_name]);
    }
}

impl Drop for WorktreeIsolation {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn canonicalize_path(path: &Path) -> PathBuf {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = canon.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    canon
}

fn relative_workspace(repo_root: &Path, workspace_root: &Path) -> PathBuf {
    let canon_repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let canon_ws = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    canon_ws
        .strip_prefix(&canon_repo)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn next_worktree_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    git_command(cwd, args)
        .output()
        .map_err(|err| format!("failed to execute git: {err}"))
}

fn run_git_with_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> Result<Output, String> {
    let mut child = git_command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to execute git: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_bytes)
            .map_err(|err| format!("failed to write patch to git: {err}"))?;
    }
    child
        .wait_with_output()
        .map_err(|err| format!("failed to wait for git: {err}"))
}
