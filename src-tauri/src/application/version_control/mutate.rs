//! Guarded write operations: stage, unstage, commit, push.
//!
//! Split from `version_control.rs` (AC-207): every repository *mutation*,
//! its single-use confirmation gate, and the commit-message validation gate
//! live here; reads, path validation, and AI message generation are
//! siblings.

use std::path::Path;

use crate::application::confirmations::{
    ConfirmationRegistry, ConfirmationTarget, OP_GIT_COMMIT, OP_GIT_PUSH, OP_GIT_STAGE,
    OP_GIT_UNSTAGE, SCOPE_VERSION_CONTROL,
};

use super::paths::{assert_within_repo, clean_stage_paths};
use super::{
    canonical_workdir, current_branch, open_workspace_repo, VersionControlError,
    ALLOWED_PUSH_REMOTE, COMMIT_TYPES, MAX_COMMIT_MESSAGE_LEN, MAX_COMMIT_SUBJECT_LEN,
};

// ---------------------------------------------------------------------------
// Guarded write operations: stage / unstage / commit / push
// ---------------------------------------------------------------------------

/// Consume the single-use confirmation id for one guarded write (SEC-03).
///
/// Direct IPC commands cannot park on the agent
/// [`ApprovalGate`](crate::application::agent::approval::ApprovalGate) (no
/// run, no park, no autonomy mode), so the writes reuse the destructive-action
/// confirmation gate: `confirmation_id` must be a live single-use id minted
/// in the `version_control` scope by `request_confirmation` — which only
/// mints after the user accepts a blocking native OS dialog — and bound to
/// `operation`. The consume is atomic: forged, expired, reused, cross-scope,
/// and different-operation ids refuse without touching git.
pub(super) fn require_confirmation(
    confirmations: &ConfirmationRegistry,
    confirmation_id: &str,
    operation: &str,
) -> Result<(), VersionControlError> {
    if confirmations.consume(
        SCOPE_VERSION_CONTROL,
        confirmation_id,
        &ConfirmationTarget::new(operation),
    ) {
        Ok(())
    } else {
        Err(VersionControlError::Unconfirmed)
    }
}
/// Stage `raw_paths` (repository-relative) into the index.
///
/// Returns the number of paths staged. Requires `confirmation_id` — a live
/// single-use id minted for [`OP_GIT_STAGE`] in the `version_control` scope
/// (see [`require_confirmation`]); the id is consumed only after the batch
/// and path guards pass, so a malformed call costs the user nothing (the
/// `terminal_run` precedent). Refuses empty and overlong batches and any
/// path that escapes the repository workdir (same guards as the read path).
/// Unmatched pathspecs fail instead of silently staging nothing
/// (`CHECK_PATHSPEC`), so stale or mistyped paths surface as an error while
/// the count still reflects the batch.
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] for a refused confirmation,
/// [`VersionControlError::InvalidPath`] for a bad batch or an escaping path,
/// [`VersionControlError::GitFailed`] when staging fails (including an
/// unmatched pathspec); see [`open_workspace_repo`] for the remaining
/// failures.
pub(crate) fn git_stage(
    workspace_root: &Path,
    raw_paths: &[String],
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<usize, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rels = clean_stage_paths(raw_paths)?;
    for rel in &rels {
        assert_within_repo(&canon_repo, rel)?;
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_STAGE)?;
    let mut index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    index
        .add_all(rels.iter(), git2::IndexAddOption::CHECK_PATHSPEC, None)
        .map_err(|_| VersionControlError::GitFailed)?;
    index.write().map_err(|_| VersionControlError::GitFailed)?;
    Ok(rels.len())
}

/// Unstage `raw_paths` (repository-relative): reset the index entries to
/// `HEAD` (or drop them when `HEAD` is unborn).
///
/// Returns the number of paths unstaged. Requires `confirmation_id` minted
/// for [`OP_GIT_UNSTAGE`]; same path guards and consume-after-validation
/// order as [`git_stage`]. Unstaging a path with no staged change is a
/// successful no-op.
///
/// # Errors
///
/// See [`git_stage`].
pub(crate) fn git_unstage(
    workspace_root: &Path,
    raw_paths: &[String],
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<usize, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let canon_repo = canonical_workdir(&repo)?;
    let rels = clean_stage_paths(raw_paths)?;
    for rel in &rels {
        assert_within_repo(&canon_repo, rel)?;
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_UNSTAGE)?;
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
/// Requires `confirmation_id` minted for [`OP_GIT_COMMIT`]; the message is
/// validated before the id is consumed, so a malformed message costs the user
/// nothing. Empty commits (a tree identical to `HEAD`, or nothing staged on
/// an unborn `HEAD`) refuse with [`VersionControlError::GitFailed`].
///
/// # Errors
///
/// Returns [`VersionControlError::Unconfirmed`] for a refused confirmation,
/// [`VersionControlError::InvalidMessage`] for a bad message; see
/// [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_commit(
    workspace_root: &Path,
    raw_message: &str,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<String, VersionControlError> {
    let message = validate_commit_message(raw_message)?;
    require_confirmation(confirmations, confirmation_id, OP_GIT_COMMIT)?;
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
/// Returns [`VersionControlError::Unconfirmed`] for a refused confirmation,
/// [`VersionControlError::InvalidRemote`] for a non-allowlisted remote; see
/// [`open_workspace_repo`] for the remaining failures.
pub(crate) fn git_push(
    workspace_root: &Path,
    remote_name: &str,
    confirmation_id: &str,
    confirmations: &ConfirmationRegistry,
) -> Result<(), VersionControlError> {
    if remote_name != ALLOWED_PUSH_REMOTE {
        return Err(VersionControlError::InvalidRemote);
    }
    require_confirmation(confirmations, confirmation_id, OP_GIT_PUSH)?;
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
