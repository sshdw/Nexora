//! AI `/review` diff preparation with mandatory secret redaction.
//!
//! Builds a bounded unified diff for either the current working tree (staged +
//! unstaged vs `HEAD`) or the current branch against a base branch, then
//! scrubs all detected secrets through
//! [`crate::application::agent::redaction::redact_command_output`] before
//! returning the diff and `/review` prompt to the caller.

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::application::agent::redaction::redact_command_output;

use super::workflow::validate_branch_name;
use super::{open_workspace_repo, VersionControlError, MAX_COMMIT_PROMPT_BYTES, MAX_STATUS_FILES};

/// Prepared, secret-redacted diff payload for AI `/review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitReviewDiff {
    /// Review scope: `"working"` or `"branch"`.
    pub scope: String,
    /// Base branch name when `scope == "branch"`, or [`None`] for `"working"`.
    pub base_branch: Option<String>,
    /// Changed repository-relative paths, sorted and capped.
    pub files: Vec<String>,
    /// Number of changed paths omitted past [`MAX_STATUS_FILES`].
    pub files_overflow: usize,
    /// Unified diff text after mandatory secret redaction, capped at
    /// [`MAX_COMMIT_PROMPT_BYTES`].
    pub diff: String,
    /// Whether the diff was truncated at [`MAX_COMMIT_PROMPT_BYTES`].
    pub truncated: bool,
    /// Whether one or more secrets were detected and redacted before output.
    pub redacted: bool,
    /// Ready-to-send `/review` prompt containing the redacted diff.
    pub prompt: String,
}

/// Resolve the default base branch for `"branch"` review when the caller did
/// not specify one: prefers `main`, then `master`, then `origin/main`, then
/// `origin/master`.
fn resolve_base_commit<'a>(
    repo: &'a git2::Repository,
    requested_base: Option<&str>,
) -> Result<(String, git2::Commit<'a>), VersionControlError> {
    if let Some(raw) = requested_base {
        let name = validate_branch_name(raw)?;
        let commit = repo
            .revparse_single(&name)
            .and_then(|obj| obj.peel_to_commit())
            .map_err(|_| VersionControlError::InvalidBranch)?;
        return Ok((name, commit));
    }
    for candidate in ["main", "master", "origin/main", "origin/master"] {
        if let Ok(obj) = repo.revparse_single(candidate) {
            if let Ok(commit) = obj.peel_to_commit() {
                return Ok((candidate.to_string(), commit));
            }
        }
    }
    Err(VersionControlError::InvalidBranch)
}

fn collect_diff_text(
    diff: &git2::Diff<'_>,
) -> Result<(String, Vec<String>, usize, bool), VersionControlError> {
    let mut files = Vec::new();
    let mut text = String::new();
    let mut truncated = false;

    for (index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path_bytes()
            .or_else(|| delta.old_file().path_bytes())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        if !path.is_empty() {
            files.push(path.clone());
        }
        if truncated {
            continue;
        }
        if let Some(mut patch) =
            git2::Patch::from_diff(diff, index).map_err(|_| VersionControlError::GitFailed)?
        {
            let buf = patch.to_buf().map_err(|_| VersionControlError::GitFailed)?;
            let bytes: &[u8] = &buf;
            if bytes.contains(&0) {
                let _ = writeln!(text, "file: {path}\n(binary file, content omitted)");
            } else {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
        } else if !path.is_empty() {
            let _ = writeln!(text, "file: {path}\n(binary file, content omitted)");
        }
        if text.len() > MAX_COMMIT_PROMPT_BYTES {
            let mut end = MAX_COMMIT_PROMPT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
    }

    files.sort();
    files.dedup();
    let files_overflow = files.len().saturating_sub(MAX_STATUS_FILES);
    files.truncate(MAX_STATUS_FILES);

    Ok((text, files, files_overflow, truncated))
}

/// Build the `/review` prompt from an already-redacted diff.
fn build_review_prompt(
    scope: &str,
    base_branch: Option<&str>,
    files: &[String],
    redacted_diff: &str,
    truncated: bool,
    redacted: bool,
) -> String {
    let mut prompt = String::from("/review\n\n");
    match (scope, base_branch) {
        ("branch", Some(base)) => {
            let _ = writeln!(
                prompt,
                "Review the branch changes against `{base}` ({} file(s)).",
                files.len()
            );
        }
        _ => {
            let _ = writeln!(
                prompt,
                "Review the current working-tree changes ({} file(s)).",
                files.len()
            );
        }
    }
    if redacted {
        prompt.push_str("Note: potential secrets were automatically redacted as `[REDACTED]`.\n");
    }
    if truncated {
        prompt.push_str("Note: diff was truncated to 64 KiB.\n");
    }
    prompt.push_str(
        "Focus on correctness bugs, security issues, missing edge cases, and maintainability.\n\n```diff\n",
    );
    prompt.push_str(redacted_diff);
    if !redacted_diff.ends_with('\n') && !redacted_diff.is_empty() {
        prompt.push('\n');
    }
    prompt.push_str("```\n");
    prompt
}

/// Prepare a secret-redacted unified diff and `/review` prompt for either the
/// `"working"` tree (staged + unstaged vs `HEAD`) or `"branch"` (merge-base
/// with `base_branch` to `HEAD`).
///
/// Every diff passes through [`redact_command_output`] before leaving this
/// function so secrets are scrubbed prior to any AI prompt.
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidInput`] for an unknown scope,
/// [`VersionControlError::InvalidBranch`] when the base branch cannot be
/// resolved, or [`VersionControlError::GitFailed`] on git errors.
pub fn git_prepare_review_diff(
    workspace_root: &Path,
    scope: &str,
    base_branch: Option<&str>,
) -> Result<GitReviewDiff, VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let (resolved_base, raw_diff, files, files_overflow, truncated) = match scope {
        "working" => {
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let mut opts = git2::DiffOptions::new();
            opts.context_lines(3)
                .include_untracked(true)
                .recurse_untracked_dirs(true);
            let diff = repo
                .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))
                .map_err(|_| VersionControlError::GitFailed)?;
            let (text, files, overflow, trunc) = collect_diff_text(&diff)?;
            (None, text, files, overflow, trunc)
        }
        "branch" => {
            let (base_name, base_commit) = resolve_base_commit(&repo, base_branch)?;
            let head_commit = repo
                .head()
                .and_then(|h| h.peel_to_commit())
                .map_err(|_| VersionControlError::GitFailed)?;
            let merge_base_oid = repo
                .merge_base(base_commit.id(), head_commit.id())
                .unwrap_or_else(|_| base_commit.id());
            let base_tree = repo
                .find_commit(merge_base_oid)
                .and_then(|c| c.tree())
                .map_err(|_| VersionControlError::GitFailed)?;
            let head_tree = head_commit
                .tree()
                .map_err(|_| VersionControlError::GitFailed)?;
            let mut opts = git2::DiffOptions::new();
            opts.context_lines(3);
            let diff = repo
                .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut opts))
                .map_err(|_| VersionControlError::GitFailed)?;
            let (text, files, overflow, trunc) = collect_diff_text(&diff)?;
            (Some(base_name), text, files, overflow, trunc)
        }
        _ => return Err(VersionControlError::InvalidInput),
    };

    // Mandatory secret redaction before any AI prompt sees the diff.
    let (redacted_diff, was_redacted) = redact_command_output(&raw_diff);
    let prompt = build_review_prompt(
        scope,
        resolved_base.as_deref(),
        &files,
        &redacted_diff,
        truncated,
        was_redacted,
    );

    Ok(GitReviewDiff {
        scope: scope.to_string(),
        base_branch: resolved_base,
        files,
        files_overflow,
        diff: redacted_diff,
        truncated,
        redacted: was_redacted,
        prompt,
    })
}
