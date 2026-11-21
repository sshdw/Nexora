//! Extended Git workflow operations: fetch, pull, branch management, and stash.
//!
//! All mutating operations consume a single-use server-side confirmation id
//! (`SCOPE_VERSION_CONTROL`) via [`require_confirmation`]. Remote operations
//! (`fetch` / `pull`) remain restricted to [`ALLOWED_PUSH_REMOTE`] (`origin`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::application::confirmations::{
    ConfirmationRegistry, OP_GIT_BRANCH_CREATE, OP_GIT_BRANCH_DELETE, OP_GIT_BRANCH_DELETE_FORCE,
    OP_GIT_BRANCH_SWITCH, OP_GIT_PULL, OP_GIT_STASH_POP, OP_GIT_STASH_PUSH,
};

use super::conflicts::{is_merge_in_progress, read_conflicts, GitConflictFile};
use super::mutate::require_confirmation;
use super::{current_branch, open_workspace_repo, VersionControlError, ALLOWED_PUSH_REMOTE};

/// Maximum accepted length of a local branch name.
const MAX_BRANCH_NAME_LEN: usize = 200;

/// Maximum accepted length of a stash message.
const MAX_STASH_MESSAGE_LEN: usize = 200;

/// Result of fetching from `origin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitFetchResult {
    /// Remote name fetched (`"origin"`).
    pub remote: String,
    /// Number of remote-tracking tips inspected/available after fetch.
    pub updated_refs: usize,
}

/// Result of pulling the current branch from `origin`.
///
/// When a merge conflict occurs, `status` is `"conflicts"` and `conflicts`
/// lists the conflicted files — the command succeeds cleanly without crashing
/// so the UI can present conflict resolution controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitPullResult {
    /// Fixed-vocabulary pull outcome: `"up_to_date"`, `"fast_forward"`,
    /// `"merged"`, or `"conflicts"`.
    pub status: String,
    /// New `HEAD` commit hash when the branch moved, or [`None`] when
    /// up-to-date or halted with conflicts.
    pub commit_hash: Option<String>,
    /// Conflicted files when `status == "conflicts"`, empty otherwise.
    pub conflicts: Vec<GitConflictFile>,
}

/// One local branch entry for the branch switcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitBranch {
    /// Short branch name (e.g. `"main"`, `"feat/auth"`).
    pub name: String,
    /// Whether this branch is currently checked out (`HEAD`).
    pub is_current: bool,
    /// Configured upstream shorthand (e.g. `"origin/main"`), if any.
    pub upstream: Option<String>,
    /// Full hash of the branch tip commit, if resolvable.
    pub last_commit_hash: Option<String>,
}

/// One stash entry from `git stash list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitStashEntry {
    /// Zero-based stash index (`0` = `stash@{0}`).
    pub index: usize,
    /// Stash summary message.
    pub message: String,
    /// Full commit hash of the stash entry.
    pub hash: String,
}

/// Result of popping a stash entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GitStashPopResult {
    /// `"applied"` when clean, or `"conflicts"` if popping produced index
    /// conflicts.
    pub status: String,
    /// Conflicted files if `status == "conflicts"`.
    pub conflicts: Vec<GitConflictFile>,
}

/// Validate a caller-supplied local branch name against git ref rules and
/// safe length/character bounds.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidBranch`] for any rejected shape.
pub(crate) fn validate_branch_name(raw: &str) -> Result<String, VersionControlError> {
    let name = raw.trim();
    if name.is_empty() || name.len() > MAX_BRANCH_NAME_LEN {
        return Err(VersionControlError::InvalidBranch);
    }
    if name.starts_with('-')
        || name.starts_with('.')
        || name.ends_with('.')
        || name.starts_with('/')
        || name.ends_with('/')
        || Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name == "@"
    {
        return Err(VersionControlError::InvalidBranch);
    }
    for ch in name.chars() {
        if ch.is_control()
            || ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        {
            return Err(VersionControlError::InvalidBranch);
        }
    }
    if !git2::Branch::name_is_valid(name).unwrap_or(false) {
        return Err(VersionControlError::InvalidBranch);
    }
    Ok(name.to_string())
}

/// Fetch remote-tracking refs from `remote_name` (must be `"origin"`).
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidRemote`] for non-allowlisted remotes,
/// or [`VersionControlError::GitFailed`] if fetching fails.
pub(crate) fn git_fetch(
    workspace_root: &Path,
    remote_name: &str,
) -> Result<GitFetchResult, VersionControlError> {
    if remote_name != ALLOWED_PUSH_REMOTE {
        return Err(VersionControlError::InvalidRemote);
    }
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let mut remote = repo
        .find_remote(ALLOWED_PUSH_REMOTE)
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut fetch_opts = git2::FetchOptions::new();
    let empty_refspecs: [&str; 0] = [];
    remote
        .fetch(&empty_refspecs, Some(&mut fetch_opts), None)
        .map_err(|_| VersionControlError::GitFailed)?;
    let prefix = format!("refs/remotes/{ALLOWED_PUSH_REMOTE}/");
    let updated_refs = repo
        .references()
        .map_err(|_| VersionControlError::GitFailed)?
        .flatten()
        .filter(|r| r.name().is_ok_and(|n| n.starts_with(&prefix)))
        .count();
    Ok(GitFetchResult {
        remote: ALLOWED_PUSH_REMOTE.to_string(),
        updated_refs,
    })
}

/// Pull the current branch from `remote_name` (`"origin"`), performing a
/// fast-forward when possible or a 3-way merge when histories diverged.
///
/// If a 3-way merge produces conflicts, this function **does not fail**: it
/// leaves the repository in the standard git merge-conflict state and returns
/// `Ok(GitPullResult { status: "conflicts", ... })` so the user can resolve
/// conflicts or abort cleanly.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidRemote`],
/// [`VersionControlError::MergeInProgress`],
/// [`VersionControlError::Unconfirmed`], or [`VersionControlError::GitFailed`].
pub(crate) fn git_pull(
    workspace_root: &Path,
    remote_name: &str,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<GitPullResult, VersionControlError> {
    if remote_name != ALLOWED_PUSH_REMOTE {
        return Err(VersionControlError::InvalidRemote);
    }
    let (_, repo) = open_workspace_repo(workspace_root)?;
    if is_merge_in_progress(&repo) || !read_conflicts(&repo)?.is_empty() {
        return Err(VersionControlError::MergeInProgress);
    }
    let branch = current_branch(&repo).ok_or(VersionControlError::GitFailed)?;
    require_confirmation(confirmations, confirmation_id, OP_GIT_PULL)?;

    // Fetch current branch from origin.
    {
        let mut remote = repo
            .find_remote(ALLOWED_PUSH_REMOTE)
            .map_err(|_| VersionControlError::GitFailed)?;
        let refspec = format!("refs/heads/{branch}:refs/remotes/{ALLOWED_PUSH_REMOTE}/{branch}");
        let mut fetch_opts = git2::FetchOptions::new();
        remote
            .fetch(&[refspec.as_str()], Some(&mut fetch_opts), None)
            .map_err(|_| VersionControlError::GitFailed)?;
    }

    let remote_ref_name = format!("refs/remotes/{ALLOWED_PUSH_REMOTE}/{branch}");
    let remote_ref = repo
        .find_reference(&remote_ref_name)
        .map_err(|_| VersionControlError::GitFailed)?;
    let fetch_commit = repo
        .reference_to_annotated_commit(&remote_ref)
        .map_err(|_| VersionControlError::GitFailed)?;

    let (analysis, _) = repo
        .merge_analysis(&[&fetch_commit])
        .map_err(|_| VersionControlError::GitFailed)?;

    if analysis.is_up_to_date() {
        return Ok(GitPullResult {
            status: "up_to_date".to_string(),
            commit_hash: None,
            conflicts: Vec::new(),
        });
    }

    if analysis.is_fast_forward() {
        let local_ref_name = format!("refs/heads/{branch}");
        let mut local_ref = repo
            .find_reference(&local_ref_name)
            .map_err(|_| VersionControlError::GitFailed)?;
        let target_oid = fetch_commit.id();
        let commit_obj = repo
            .find_object(target_oid, Some(git2::ObjectType::Commit))
            .map_err(|_| VersionControlError::GitFailed)?;
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.safe();
        repo.checkout_tree(&commit_obj, Some(&mut checkout))
            .map_err(|_| VersionControlError::GitFailed)?;
        local_ref
            .set_target(target_oid, "fast-forward pull")
            .map_err(|_| VersionControlError::GitFailed)?;
        repo.set_head(&local_ref_name)
            .map_err(|_| VersionControlError::GitFailed)?;
        return Ok(GitPullResult {
            status: "fast_forward".to_string(),
            commit_hash: Some(target_oid.to_string()),
            conflicts: Vec::new(),
        });
    }

    if analysis.is_normal() {
        return perform_normal_merge(&repo, &branch, &fetch_commit);
    }

    Err(VersionControlError::GitFailed)
}

fn perform_normal_merge(
    repo: &git2::Repository,
    branch: &str,
    fetch_commit: &git2::AnnotatedCommit<'_>,
) -> Result<GitPullResult, VersionControlError> {
    let mut merge_opts = git2::MergeOptions::new();
    let mut checkout_opts = git2::build::CheckoutBuilder::new();
    checkout_opts
        .allow_conflicts(true)
        .conflict_style_merge(true);
    repo.merge(
        &[fetch_commit],
        Some(&mut merge_opts),
        Some(&mut checkout_opts),
    )
    .map_err(|_| VersionControlError::GitFailed)?;

    let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    if index.has_conflicts() {
        let conflicts = read_conflicts(repo)?;
        return Ok(GitPullResult {
            status: "conflicts".to_string(),
            commit_hash: None,
            conflicts,
        });
    }

    let tree_id = index
        .write_tree()
        .map_err(|_| VersionControlError::GitFailed)?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|_| VersionControlError::GitFailed)?;
    let head_commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|_| VersionControlError::GitFailed)?;
    let remote_commit = repo
        .find_commit(fetch_commit.id())
        .map_err(|_| VersionControlError::GitFailed)?;
    let signature = repo
        .signature()
        .map_err(|_| VersionControlError::GitFailed)?;
    let message = format!("Merge branch '{branch}' of {ALLOWED_PUSH_REMOTE}");
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&head_commit, &remote_commit],
        )
        .map_err(|_| VersionControlError::GitFailed)?;
    repo.cleanup_state()
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(GitPullResult {
        status: "merged".to_string(),
        commit_hash: Some(oid.to_string()),
        conflicts: Vec::new(),
    })
}

/// List local branches, sorted with the current branch first then
/// alphabetically by name.
///
/// # Errors
///
/// See [`open_workspace_repo`].
pub(crate) fn git_branches(workspace_root: &Path) -> Result<Vec<GitBranch>, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut out = Vec::new();
    for item in branches {
        let (branch, _) = item.map_err(|_| VersionControlError::GitFailed)?;
        let Some(name) = branch
            .name()
            .map_err(|_| VersionControlError::GitFailed)?
            .map(str::to_string)
        else {
            continue;
        };
        let is_current = branch.is_head();
        let upstream = branch
            .upstream()
            .ok()
            .and_then(|u| u.name().ok().flatten().map(str::to_string));
        let last_commit_hash = branch
            .get()
            .peel_to_commit()
            .ok()
            .map(|c| c.id().to_string());
        out.push(GitBranch {
            name,
            is_current,
            upstream,
            last_commit_hash,
        });
    }
    out.sort_by(|a, b| {
        b.is_current
            .cmp(&a.is_current)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Create a new local branch at `HEAD`, optionally switching to it.
///
/// Requires `confirmation_id` minted for [`OP_GIT_BRANCH_CREATE`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidBranch`],
/// [`VersionControlError::MergeInProgress`],
/// [`VersionControlError::Unconfirmed`], or [`VersionControlError::GitFailed`].
pub(crate) fn git_branch_create(
    workspace_root: &Path,
    raw_name: &str,
    switch: bool,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<GitBranch, VersionControlError> {
    let name = validate_branch_name(raw_name)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    if switch && (is_merge_in_progress(&repo) || !read_conflicts(&repo)?.is_empty()) {
        return Err(VersionControlError::MergeInProgress);
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_BRANCH_CREATE)?;

    let head_commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|_| VersionControlError::GitFailed)?;
    let hash = head_commit.id().to_string();
    repo.branch(&name, &head_commit, false)
        .map_err(|_| VersionControlError::GitFailed)?;

    if switch {
        let ref_name = format!("refs/heads/{name}");
        repo.set_head(&ref_name)
            .map_err(|_| VersionControlError::GitFailed)?;
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.safe();
        repo.checkout_head(Some(&mut checkout))
            .map_err(|_| VersionControlError::GitFailed)?;
    }

    Ok(GitBranch {
        name,
        is_current: switch,
        upstream: None,
        last_commit_hash: Some(hash),
    })
}

/// Switch to an existing local branch using a safe checkout.
///
/// Requires `confirmation_id` minted for [`OP_GIT_BRANCH_SWITCH`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidBranch`],
/// [`VersionControlError::MergeInProgress`],
/// [`VersionControlError::Unconfirmed`], or [`VersionControlError::GitFailed`].
pub(crate) fn git_branch_switch(
    workspace_root: &Path,
    raw_name: &str,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<(), VersionControlError> {
    let name = validate_branch_name(raw_name)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    if is_merge_in_progress(&repo) || !read_conflicts(&repo)?.is_empty() {
        return Err(VersionControlError::MergeInProgress);
    }
    let ref_name = format!("refs/heads/{name}");
    let target_ref = repo
        .find_reference(&ref_name)
        .map_err(|_| VersionControlError::InvalidBranch)?;
    let target_obj = target_ref
        .peel(git2::ObjectType::Commit)
        .map_err(|_| VersionControlError::GitFailed)?;

    require_confirmation(confirmations, confirmation_id, OP_GIT_BRANCH_SWITCH)?;

    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(&target_obj, Some(&mut checkout))
        .map_err(|_| VersionControlError::GitFailed)?;
    repo.set_head(&ref_name)
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(())
}

/// Delete a local branch. Refuses to delete the currently checked-out branch.
///
/// When `allow_unmerged` is `false`, refuses with
/// [`VersionControlError::BranchNotMerged`] if the branch tip is not an
/// ancestor of `HEAD`. When `allow_unmerged` is `true`, consumes
/// [`OP_GIT_BRANCH_DELETE_FORCE`] instead of [`OP_GIT_BRANCH_DELETE`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidBranch`],
/// [`VersionControlError::BranchNotMerged`],
/// [`VersionControlError::Unconfirmed`], or [`VersionControlError::GitFailed`].
pub(crate) fn git_branch_delete(
    workspace_root: &Path,
    raw_name: &str,
    allow_unmerged: bool,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<(), VersionControlError> {
    let name = validate_branch_name(raw_name)?;
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let mut branch = repo
        .find_branch(&name, git2::BranchType::Local)
        .map_err(|_| VersionControlError::InvalidBranch)?;
    if branch.is_head() {
        return Err(VersionControlError::InvalidBranch);
    }

    if !allow_unmerged {
        let head_oid = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map(|c| c.id())
            .map_err(|_| VersionControlError::GitFailed)?;
        let branch_oid = branch
            .get()
            .peel_to_commit()
            .map(|c| c.id())
            .map_err(|_| VersionControlError::GitFailed)?;
        let is_merged = head_oid == branch_oid
            || repo
                .graph_descendant_of(head_oid, branch_oid)
                .unwrap_or(false);
        if !is_merged {
            return Err(VersionControlError::BranchNotMerged);
        }
    }

    let op = if allow_unmerged {
        OP_GIT_BRANCH_DELETE_FORCE
    } else {
        OP_GIT_BRANCH_DELETE
    };
    require_confirmation(confirmations, confirmation_id, op)?;

    branch
        .delete()
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(())
}

/// List all stash entries in the repository (newest first, index 0 = top).
///
/// # Errors
///
/// See [`open_workspace_repo`].
pub(crate) fn git_stash_list(
    workspace_root: &Path,
) -> Result<Vec<GitStashEntry>, VersionControlError> {
    let (_, mut repo) = open_workspace_repo(workspace_root)?;
    let mut entries = Vec::new();
    repo.stash_foreach(|index, message, oid| {
        entries.push(GitStashEntry {
            index,
            message: message.to_string(),
            hash: oid.to_string(),
        });
        true
    })
    .map_err(|_| VersionControlError::GitFailed)?;
    Ok(entries)
}

/// Save working-tree and staged changes (including untracked files) onto the
/// stash stack.
///
/// Requires `confirmation_id` minted for [`OP_GIT_STASH_PUSH`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidMessage`],
/// [`VersionControlError::Unconfirmed`], or [`VersionControlError::GitFailed`].
pub(crate) fn git_stash_push(
    workspace_root: &Path,
    raw_message: Option<&str>,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<GitStashEntry, VersionControlError> {
    let message: Option<String> = match raw_message {
        Some(raw) => {
            if raw.contains('\0') {
                return Err(VersionControlError::InvalidMessage);
            }
            let trimmed = raw.trim();
            if trimmed.len() > MAX_STASH_MESSAGE_LEN {
                return Err(VersionControlError::InvalidMessage);
            }
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        None => None,
    };
    let (_, mut repo) = open_workspace_repo(workspace_root)?;
    require_confirmation(confirmations, confirmation_id, OP_GIT_STASH_PUSH)?;
    let signature = repo
        .signature()
        .map_err(|_| VersionControlError::GitFailed)?;
    let stash_msg = message.unwrap_or_else(|| "WIP".to_string());
    let oid = repo
        .stash_save(
            &signature,
            &stash_msg,
            Some(git2::StashFlags::INCLUDE_UNTRACKED),
        )
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(GitStashEntry {
        index: 0,
        message: stash_msg,
        hash: oid.to_string(),
    })
}

/// Pop a stash entry by zero-based `index` (`0` = latest stash).
///
/// Requires `confirmation_id` minted for [`OP_GIT_STASH_POP`]. If popping
/// produces conflicts, returns `status: "conflicts"` cleanly instead of
/// failing.
///
/// # Errors
///
/// Returns [`VersionControlError::MergeInProgress`],
/// [`VersionControlError::Unconfirmed`], or
/// [`VersionControlError::GitFailed`].
pub(crate) fn git_stash_pop(
    workspace_root: &Path,
    index: usize,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<GitStashPopResult, VersionControlError> {
    let (_, mut repo) = open_workspace_repo(workspace_root)?;
    if is_merge_in_progress(&repo) || !read_conflicts(&repo)?.is_empty() {
        return Err(VersionControlError::MergeInProgress);
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_STASH_POP)?;

    let mut apply_opts = git2::StashApplyOptions::new();
    let pop_res = repo.stash_pop(index, Some(&mut apply_opts));
    let conflicts = read_conflicts(&repo)?;
    if !conflicts.is_empty() {
        return Ok(GitStashPopResult {
            status: "conflicts".to_string(),
            conflicts,
        });
    }
    pop_res.map_err(|_| VersionControlError::GitFailed)?;
    Ok(GitStashPopResult {
        status: "applied".to_string(),
        conflicts: Vec::new(),
    })
}
