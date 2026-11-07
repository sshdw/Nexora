//! Repository-relative path validation for the git commands.
//!
//! Split from `version_control.rs` (AC-207): lexical path cleaning and the
//! workdir-containment assertions shared by the read and write halves.

use std::path::{Component, Path, PathBuf};

use crate::application::agent::tools::is_within_workspace;
use crate::application::workspace::strip_verbatim;

use super::{VersionControlError, MAX_PATH_LEN, MAX_STAGE_PATHS};

/// Validate a caller-supplied repository-relative path.
/// Rejects absolute paths (both separators, including driveSmoke and UNC
/// forms), parent references, `.git` components, null bytes, empty input,
/// and overlong input — all with the single fixed-vocabulary
/// [`VersionControlError::InvalidPath`].
///
/// # Errors
///
/// Returns [`VersionControlError::InvalidPath`] for any rejected shape.
pub(super) fn clean_repo_path(raw: &str) -> Result<PathBuf, VersionControlError> {
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

/// Backstop for symlinks anywhere along a repository-relative path
/// (including symlinked intermediate directories): resolve the nearest
/// existing ancestor through the filesystem, re-attach the unresolved tail,
/// and re-check containment. A request like `link/secret` where `link`
/// escapes the workdir refuses even though the lexical check passed.
pub(super) fn assert_within_repo(canon_repo: &Path, rel: &Path) -> Result<(), VersionControlError> {
    let joined = canon_repo.join(rel);
    if !is_within_workspace(canon_repo, &joined) {
        return Err(VersionControlError::InvalidPath);
    }
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
            if !is_within_workspace(canon_repo, &resolved) {
                return Err(VersionControlError::InvalidPath);
            }
            return Ok(());
        }
        match ancestor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => ancestor = parent,
            _ => return Ok(()),
        }
    }
}
/// Validate a stage/unstage batch into repository-relative paths: non-empty,
/// bounded at [`MAX_STAGE_PATHS`], each lexically clean.
pub(super) fn clean_stage_paths(raw_paths: &[String]) -> Result<Vec<PathBuf>, VersionControlError> {
    if raw_paths.is_empty() || raw_paths.len() > MAX_STAGE_PATHS {
        return Err(VersionControlError::InvalidPath);
    }
    raw_paths.iter().map(|raw| clean_repo_path(raw)).collect()
}
