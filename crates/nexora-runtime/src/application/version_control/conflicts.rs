//! Conflict detection, preview, per-file resolution, and safe merge/pull abort.
//!
//! Covers M20 / P37–P38 conflict handling:
//! - Inspects the git index conflict entries (`UU`, `AA`, `UD`, `DU`, `DD`) and
//!   repository merge/pull-in-progress state.
//! - Loads capped, binary-safe conflict previews (`ours`, `theirs`, and
//!   working-tree content with conflict markers).
//! - Resolves individual conflicted files via `ours` or `theirs`, writes the
//!   selected content (or removes the file when that side deleted it), and
//!   stages the resolved file into the index.
//! - Safely aborts an in-progress merge / pull by resetting to `HEAD` and
//!   cleaning up repository merge state (`MERGE_HEAD`, `MERGE_MSG`, etc.).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::application::confirmations::{
    ConfirmationRegistry, OP_GIT_ABORT_MERGE, OP_GIT_RESOLVE_CONFLICT,
};

use super::mutate::require_confirmation;
use super::paths::{assert_within_repo, clean_repo_path};
use super::{
    canonical_workdir, open_workspace_repo, VersionControlError, MAX_DIFF_BYTES, MAX_STATUS_FILES,
};

/// One conflicted file in the index with its two-letter git conflict code
/// (`"UU"`, `"AA"`, `"UD"`, `"DU"`, or `"DD"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitConflictFile {
    /// Repository-relative forward-slash path.
    pub path: String,
    /// Two-letter conflict code (`"UU"`, `"AA"`, `"UD"`, `"DU"`, `"DD"`).
    pub status: String,
}

/// Conflict preview for a single conflicted file: `ours` (stage 2), `theirs`
/// (stage 3), and current working-tree content (with conflict markers if
/// present), capped at [`MAX_DIFF_BYTES`] and binary-checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitConflictDiff {
    /// Repository-relative forward-slash path.
    pub path: String,
    /// Two-letter conflict code (`"UU"`, `"AA"`, `"UD"`, `"DU"`, `"DD"`).
    pub status: String,
    /// Content from stage 2 (`ours`), or empty when deleted/absent or binary.
    pub ours: String,
    /// Content from stage 3 (`theirs`), or empty when deleted/absent or binary.
    pub theirs: String,
    /// Current working-tree content (often containing `<<<<<<<` markers),
    /// or empty when absent or binary.
    pub working: String,
    /// Whether any section was truncated at [`MAX_DIFF_BYTES`].
    pub truncated: bool,
    /// Whether any section contained binary content (NUL byte).
    pub binary: bool,
}

/// Classify a git2 [`git2::IndexConflict`] into its two-letter conflict code.
fn classify_conflict(conflict: &git2::IndexConflict) -> &'static str {
    match (
        conflict.ancestor.is_some(),
        conflict.our.is_some(),
        conflict.their.is_some(),
    ) {
        (true, true, true) => "UU",
        (false, true, true) => "AA",
        (_, true, false) => "UD",
        (_, false, true) => "DU",
        (_, false, false) => "DD",
    }
}

/// Extract the repository-relative path of an [`git2::IndexConflict`].
fn conflict_path(conflict: &git2::IndexConflict) -> Option<String> {
    let entry = conflict
        .our
        .as_ref()
        .or(conflict.their.as_ref())
        .or(conflict.ancestor.as_ref())?;
    let path = String::from_utf8_lossy(&entry.path).into_owned();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Read all conflicted files from the repository index, sorted by path and
/// capped at [`MAX_STATUS_FILES`].
///
/// # Errors
///
/// Returns [`VersionControlError::GitFailed`] if reading the index fails.
pub fn read_conflicts(
    repo: &git2::Repository,
) -> Result<Vec<GitConflictFile>, VersionControlError> {
    let index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let conflicts = index
        .conflicts()
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut out = Vec::new();
    for item in conflicts {
        let conflict = item.map_err(|_| VersionControlError::GitFailed)?;
        if let Some(path) = conflict_path(&conflict) {
            out.push(GitConflictFile {
                path,
                status: classify_conflict(&conflict).to_string(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.truncate(MAX_STATUS_FILES);
    Ok(out)
}

/// Whether the repository currently has an in-progress merge, pull, rebase,
/// cherry-pick, or revert state.
#[must_use]
pub fn is_merge_in_progress(repo: &git2::Repository) -> bool {
    repo.state() != git2::RepositoryState::Clean
}

fn read_blob_text(
    repo: &git2::Repository,
    entry: Option<&git2::IndexEntry>,
) -> Result<(String, bool, bool), VersionControlError> {
    let Some(entry) = entry else {
        return Ok((String::new(), false, false));
    };
    let blob = repo
        .find_blob(entry.id)
        .map_err(|_| VersionControlError::GitFailed)?;
    let bytes = blob.content();
    if bytes.contains(&0) {
        return Ok((String::new(), false, true));
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    let truncated = text.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok((text, truncated, false))
}

fn read_workdir_text(path: &Path) -> Result<(String, bool, bool), VersionControlError> {
    if !path.is_file() {
        return Ok((String::new(), false, false));
    }
    let bytes = std::fs::read(path).map_err(|_| VersionControlError::GitFailed)?;
    if bytes.contains(&0) {
        return Ok((String::new(), false, true));
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let truncated = text.len() > MAX_DIFF_BYTES;
    if truncated {
        let mut end = MAX_DIFF_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok((text, truncated, false))
}

/// Load conflict preview (`ours`, `theirs`, and `working`) for one conflicted
/// path in the repository.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidPath`] when the path escapes the
/// workdir or is not currently conflicted; see [`open_workspace_repo`] for
/// repository failures.
pub fn git_conflict_diff(
    workspace_root: &Path,
    raw_path: &str,
) -> Result<GitConflictDiff, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rel = clean_repo_path(raw_path)?;
    assert_within_repo(&canon_repo, &rel)?;
    let spec = rel.to_string_lossy().replace('\\', "/");

    let index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let conflict = index
        .conflict_get(&rel)
        .map_err(|_| VersionControlError::InvalidPath)?;
    let status = classify_conflict(&conflict).to_string();

    let (ours, ours_trunc, ours_bin) = read_blob_text(&repo, conflict.our.as_ref())?;
    let (theirs, theirs_trunc, theirs_bin) = read_blob_text(&repo, conflict.their.as_ref())?;
    let (working, working_trunc, working_bin) = read_workdir_text(&canon_repo.join(&rel))?;

    let binary = ours_bin || theirs_bin || working_bin;
    if binary {
        return Ok(GitConflictDiff {
            path: spec,
            status,
            ours: String::new(),
            theirs: String::new(),
            working: String::new(),
            truncated: false,
            binary: true,
        });
    }

    Ok(GitConflictDiff {
        path: spec,
        status,
        ours,
        theirs,
        working,
        truncated: ours_trunc || theirs_trunc || working_trunc,
        binary: false,
    })
}

/// Resolve one conflicted file using `"ours"` or `"theirs"`, writing the
/// chosen version to the workdir (or deleting it if that side removed the
/// file) and staging the resolution in the index.
///
/// Returns the number of remaining conflicted files in the index.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidInput`] for an unknown strategy,
/// [`VersionControlError::InvalidPath`] for an invalid or non-conflicted path,
/// [`VersionControlError::Unconfirmed`] for a refused confirmation, or
/// [`VersionControlError::GitFailed`] on git/filesystem errors.
pub fn git_resolve_conflict(
    workspace_root: &Path,
    raw_path: &str,
    strategy: &str,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<usize, VersionControlError> {
    let use_ours = match strategy {
        "ours" => true,
        "theirs" => false,
        _ => return Err(VersionControlError::InvalidInput),
    };
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rel = clean_repo_path(raw_path)?;
    assert_within_repo(&canon_repo, &rel)?;

    let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let conflict = index
        .conflict_get(&rel)
        .map_err(|_| VersionControlError::InvalidPath)?;

    require_confirmation(confirmations, confirmation_id, OP_GIT_RESOLVE_CONFLICT)?;

    let chosen = if use_ours {
        conflict.our
    } else {
        conflict.their
    };
    let target_file = canon_repo.join(&rel);
    if let Some(entry) = chosen {
        let blob = repo
            .find_blob(entry.id)
            .map_err(|_| VersionControlError::GitFailed)?;
        if let Some(parent) = target_file.parent() {
            std::fs::create_dir_all(parent).map_err(|_| VersionControlError::GitFailed)?;
        }
        std::fs::write(&target_file, blob.content()).map_err(|_| VersionControlError::GitFailed)?;
        index
            .conflict_remove(&rel)
            .map_err(|_| VersionControlError::GitFailed)?;
        index
            .add_path(&rel)
            .map_err(|_| VersionControlError::GitFailed)?;
    } else {
        if target_file.exists() {
            std::fs::remove_file(&target_file).map_err(|_| VersionControlError::GitFailed)?;
        }
        index
            .conflict_remove(&rel)
            .map_err(|_| VersionControlError::GitFailed)?;
        let _ = index.remove_path(&rel);
    }
    index.write().map_err(|_| VersionControlError::GitFailed)?;

    let remaining = read_conflicts(&repo)?.len();
    Ok(remaining)
}

/// Safely abort an in-progress merge or pull: resets the index and workdir
/// back to `HEAD` and clears `MERGE_HEAD` / repository state files.
///
/// Requires `confirmation_id` minted for [`OP_GIT_ABORT_MERGE`].
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] for a refused confirmation,
/// [`VersionControlError::GitFailed`] when no merge/conflict is in progress or
/// when resetting fails.
pub fn git_abort_merge(
    workspace_root: &Path,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<(), VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let has_conflicts = repo.index().is_ok_and(|idx| idx.has_conflicts());
    if !is_merge_in_progress(&repo) && !has_conflicts {
        return Err(VersionControlError::GitFailed);
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_ABORT_MERGE)?;

    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().remove_untracked(false);
    repo.reset(head.as_object(), git2::ResetType::Hard, Some(&mut checkout))
        .map_err(|_| VersionControlError::GitFailed)?;
    repo.cleanup_state()
        .map_err(|_| VersionControlError::GitFailed)?;
    Ok(())
}
