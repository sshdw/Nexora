//! Workspace path confinement for the file tools.
//!
//! Split from `executor.rs` (AC-207): the pure path layer — `resolve_path`
//! canonicalization, lexical normalization, symlink-escape re-verification,
//! and `is_within_workspace` — lives here; the registry's dispatch stays in
//! [`super::executor`].

use std::path::{Component, Path, PathBuf};

use super::executor::ToolError;

pub(super) fn resolve_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    // Reject null bytes etc.
    if requested.contains('\0') {
        return Err(ToolError::InvalidArguments(
            "path contains null byte".to_string(),
        ));
    }
    let requested_path = Path::new(requested);
    let joined = if requested_path.is_absolute() {
        PathBuf::from(requested)
    } else {
        workspace_root.join(requested)
    };

    // Use lexical normalization + canonical check
    if !is_within_workspace(workspace_root, &joined) {
        return Err(ToolError::PathTraversal(requested.to_string()));
    }

    // Canonical check over every existing path prefix. The lexical check above
    // cannot see through symlinks, and checking only the final component (or
    // only the immediate parent) misses the depth>=2 escape where an
    // intermediate component is a link to the outside while deeper parents do
    // not exist yet (e.g. `ws/link/newsub/file.txt` with `link` -> outside):
    // walking each existing prefix catches the escape at the `link` level.
    let canon_ws = canonical_root(workspace_root);
    check_prefixes_for_escape(workspace_root, &canon_ws, &joined, requested)?;

    // Also ensure normalized joined still within workspace after resolving symlinks if exists
    // If file exists, try canonicalize and re-check
    if joined.exists() {
        if let Ok(canonical) = joined.canonicalize() {
            if !is_within_workspace(&canon_ws, &canonical) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
            // Return canonical for existing file to be precise
            // But keep original joined for write where file may not exist?
            // For existing we can return canonical
            return Ok(canonical);
        }
    } else if std::fs::symlink_metadata(&joined).is_ok() {
        // Dangling final symlink: `exists` follows links and reports false,
        // but a write would follow the link target. Resolve the link itself.
        verify_prefix_within(workspace_root, &canon_ws, &joined, requested)?;
    }

    Ok(normalize_lexically(&joined))
}

/// Canonical form of the workspace root for escape comparisons. The root
/// exists in every tool call, so `canonicalize` succeeds; the lexical
/// fallback only covers exotic failure modes without changing behavior.
pub(super) fn canonical_root(workspace_root: &Path) -> PathBuf {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(&absolutize(workspace_root)))
}

/// Reject `requested` when any existing prefix of `joined` (intermediate
/// directory, final file, or dangling link) resolves outside the workspace.
/// Non-existent prefixes are skipped: they will be created through
/// [`ensure_parent_dirs`], which re-verifies after each level.
pub(super) fn check_prefixes_for_escape(
    workspace_root: &Path,
    canon_ws: &Path,
    joined: &Path,
    requested: &str,
) -> Result<(), ToolError> {
    for prefix in existing_prefixes(workspace_root, joined) {
        verify_prefix_within(workspace_root, canon_ws, &prefix, requested)?;
    }
    Ok(())
}

/// Lexical prefixes of `joined` below the workspace root that currently exist
/// on disk (existence via `symlink_metadata`, so dangling links count).
/// Ordered outermost-first so the escape is reported at the link level.
pub(super) fn existing_prefixes(workspace_root: &Path, joined: &Path) -> Vec<PathBuf> {
    let lex = normalize_lexically(joined);
    let ws_norm = normalize_lexically(&absolutize(workspace_root));
    let ws_count = ws_norm.components().count();
    let comps: Vec<std::path::Component<'_>> = lex.components().collect();
    if comps.len() < ws_count {
        return Vec::new();
    }
    let mut prefix = workspace_root.to_path_buf();
    let mut out = Vec::with_capacity(comps.len().saturating_sub(ws_count));
    for comp in &comps[ws_count..] {
        prefix.push(comp.as_os_str());
        if std::fs::symlink_metadata(&prefix).is_ok() {
            out.push(prefix.clone());
        }
    }
    out
}

/// Verify one existing prefix stays inside the workspace after resolving
/// links. Uses `symlink_metadata` (never follows) first: links are resolved
/// explicitly via `canonicalize` (live target) or `read_link` against the
/// canonical parent (dangling target); regular files/dirs are canonicalized
/// to catch mount or rename oddities.
pub(super) fn verify_prefix_within(
    workspace_root: &Path,
    canon_ws: &Path,
    prefix: &Path,
    requested: &str,
) -> Result<(), ToolError> {
    let meta = match std::fs::symlink_metadata(prefix) {
        Ok(meta) => meta,
        // Raced away between the existence check and now: nothing to follow.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(ToolError::Io(format!(
                "failed to read path metadata for '{requested}': {e}"
            )));
        }
    };
    if meta.file_type().is_symlink() {
        if let Ok(canon) = prefix.canonicalize() {
            if !is_within_workspace(canon_ws, &canon) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
        } else {
            // Dangling link: resolve its target against the canonical parent
            // so `link -> /outside` (or `../outside`) is still caught even
            // though `canonicalize` cannot complete.
            let target = std::fs::read_link(prefix).map_err(|e| {
                ToolError::Io(format!("failed to read link for '{requested}': {e}"))
            })?;
            let parent = prefix.parent().unwrap_or(workspace_root);
            let parent_canon = parent
                .canonicalize()
                .unwrap_or_else(|_| normalize_lexically(parent));
            let target_path = PathBuf::from(&target);
            let resolved = if target_path.is_absolute() {
                target_path
            } else {
                parent_canon.join(target)
            };
            if !is_within_workspace(canon_ws, &normalize_lexically(&resolved)) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
        }
    } else if let Ok(canon) = prefix.canonicalize() {
        if !is_within_workspace(canon_ws, &canon) {
            return Err(ToolError::PathTraversal(requested.to_string()));
        }
    }
    Ok(())
}

/// Re-verify an existing file/directory just before use (TOCTOU guard for
/// read/edit/command-cwd/list/search-scope after `resolve_path`).
pub(super) fn verify_existing_within(
    workspace_root: &Path,
    canon_ws: &Path,
    path: &Path,
    requested: &str,
) -> Result<(), ToolError> {
    verify_prefix_within(workspace_root, canon_ws, path, requested)
}

/// Reject when the final write target is itself a link escaping the
/// workspace. A write would follow the link, so this must run after the
/// parents exist and immediately before `fs::write`.
pub(super) fn reject_final_symlink_escape(
    canon_ws: &Path,
    resolved: &Path,
    requested: &str,
) -> Result<(), ToolError> {
    let meta = match std::fs::symlink_metadata(resolved) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(ToolError::Io(format!(
                "failed to read path metadata for '{requested}': {e}"
            )));
        }
    };
    if meta.file_type().is_symlink() {
        if let Ok(canon) = resolved.canonicalize() {
            if !is_within_workspace(canon_ws, &canon) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
        } else {
            let target = std::fs::read_link(resolved).map_err(|e| {
                ToolError::Io(format!("failed to read link for '{requested}': {e}"))
            })?;
            let parent = resolved.parent().unwrap_or(canon_ws);
            let parent_canon = parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf());
            let target_path = PathBuf::from(&target);
            let relocated = if target_path.is_absolute() {
                target_path
            } else {
                parent_canon.join(target)
            };
            if !is_within_workspace(canon_ws, &normalize_lexically(&relocated)) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
        }
    }
    Ok(())
}

/// Create a lexically-within-workspace parent chain one level at a time,
/// refusing to pass through a link that points outside *before* creating
/// anything beneath it. Every pre-existing level and every freshly created
/// level is canonicalized and re-verified (canonicalize-after-create), so a
/// `ws/link/newsub` write with `link -> outside` fails at `link` without
/// creating `outside/newsub`.
pub(super) fn ensure_parent_dirs(
    workspace_root: &Path,
    canon_ws: &Path,
    parent: &Path,
    requested: &str,
) -> Result<(), ToolError> {
    // `parent` derives from `resolve_path` output, which is already
    // lexically normalized, so its components can be used directly.
    let ws_norm = normalize_lexically(&absolutize(workspace_root));
    let ws_count = ws_norm.components().count();
    let comps: Vec<std::path::Component<'_>> = parent.components().collect();
    if comps.len() < ws_count {
        return Err(ToolError::PathTraversal(requested.to_string()));
    }
    let mut prefix = workspace_root.to_path_buf();
    for comp in &comps[ws_count..] {
        prefix.push(comp.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(_) => {
                // Already there (dir, file, or link): must resolve inside.
                verify_prefix_within(workspace_root, canon_ws, &prefix, requested)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&prefix) {
                    Ok(()) => {}
                    Err(create_err) => {
                        // Lost a creation race: re-verify whatever is there
                        // now; an escape still reports traversal, anything
                        // else keeps the original I/O error.
                        if std::fs::symlink_metadata(&prefix).is_ok() {
                            verify_prefix_within(workspace_root, canon_ws, &prefix, requested)?;
                        }
                        return Err(ToolError::Io(format!(
                            "failed to create parent directories: {create_err}"
                        )));
                    }
                }
                // Canonicalize-after-create: the new directory must land
                // inside the workspace.
                let canon = prefix.canonicalize().map_err(|e| {
                    ToolError::Io(format!("failed to create parent directories: {e}"))
                })?;
                if !is_within_workspace(canon_ws, &canon) {
                    return Err(ToolError::PathTraversal(requested.to_string()));
                }
            }
            Err(e) => {
                return Err(ToolError::Io(format!(
                    "failed to read path metadata for '{requested}': {e}"
                )));
            }
        }
    }
    // Final re-verification of the now-fully-existing parent.
    let canon_parent = parent
        .canonicalize()
        .map_err(|e| ToolError::Io(format!("failed to create parent directories: {e}")))?;
    if !is_within_workspace(canon_ws, &canon_parent) {
        return Err(ToolError::PathTraversal(requested.to_string()));
    }
    Ok(())
}

pub(crate) fn is_within_workspace(workspace_root: &Path, target: &Path) -> bool {
    // Handle Windows verbatim prefix (\\?\) and case-insensitivity
    let ws_str = path_to_comparable_string(&normalize_lexically(&absolutize(workspace_root)));
    let tgt_str = path_to_comparable_string(&normalize_lexically(&absolutize(target)));
    // Ensure ws does not end with separator for clean prefix check
    let ws_trimmed = ws_str.trim_end_matches(['/', '\\']).to_string();
    if tgt_str == ws_trimmed {
        return true;
    }
    // Check that target starts with workspace + separator
    let sep = if ws_trimmed.contains('\\') { "\\" } else { "/" };
    // On Windows, both separators are valid; check both
    if tgt_str.starts_with(&format!("{ws_trimmed}{sep}")) {
        return true;
    }
    if cfg!(windows) {
        // Also check forward slash variant
        if tgt_str.starts_with(&format!("{ws_trimmed}/"))
            || tgt_str.starts_with(&format!("{ws_trimmed}\\"))
        {
            return true;
        }
    }
    false
}

pub(super) fn path_to_comparable_string(p: &Path) -> String {
    let mut s = p.to_string_lossy().to_string();
    // Strip Windows verbatim prefix \\?\ if present
    if s.starts_with(r"\\?\") {
        s = s[4..].to_string();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
        // Normalise separators to backslash for consistent prefix
        s = s.replace('/', "\\");
    }
    s
}

pub(super) fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

pub(super) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut components: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => {
                components.clear();
                components.push(Component::Prefix(prefix));
            }
            Component::RootDir => {
                // Keep prefix if exists, then root
                // Remove any prior Normal/CurDir/ParentDir after root
                // Find last prefix
                let mut prefix_opt = None;
                for c in &components {
                    if matches!(c, Component::Prefix(_)) {
                        prefix_opt = Some(*c);
                    }
                }
                components.clear();
                if let Some(p) = prefix_opt {
                    components.push(p);
                }
                components.push(Component::RootDir);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = components.last().copied() {
                    match last {
                        Component::Normal(_) => {
                            components.pop();
                        }
                        Component::RootDir | Component::Prefix(_) => {
                            // stay at root / cannot go above prefix
                        }
                        Component::ParentDir => components.push(Component::ParentDir),
                        Component::CurDir => {
                            components.pop();
                            components.push(Component::ParentDir);
                        }
                    }
                } else {
                    components.push(Component::ParentDir);
                }
            }
            Component::Normal(_) => components.push(comp),
        }
    }
    let mut out = PathBuf::new();
    for c in components {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}
