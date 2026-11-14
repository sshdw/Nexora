//! Workspace path confinement for the file tools.
//!
//! Split from `executor.rs` (AC-207): the pure path layer — `resolve_path`
//! canonicalization, lexical normalization, symlink-escape re-verification,
//! and `is_within_workspace` — lives here; the registry's dispatch stays in
//! [`super::executor`].

use std::path::{Component, Path, PathBuf};

use super::executor::ToolError;

pub(crate) fn resolve_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    check_private(workspace_root)?;
    let requested_path = Path::new(requested);
    let lexical = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        workspace_root.join(requested_path)
    };
    check_private(&lexical)?;
    let resolved = resolve_confined_path(workspace_root, requested)?;
    check_private(&resolved)?;
    Ok(resolved)
}
/// Only the native-confirmed human terminal uses this cwd resolver. It retains
/// workspace confinement but does not apply the agent's private-store policy.
pub(super) fn resolve_terminal_cwd(
    workspace_root: &Path,
    requested: &str,
) -> Result<PathBuf, ToolError> {
    let resolved = resolve_confined_path(workspace_root, requested)?;
    verify_prefix_within(
        workspace_root,
        &canonical_root(workspace_root),
        &resolved,
        requested,
    )?;
    Ok(resolved)
}
fn resolve_confined_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, ToolError> {
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

    // Canonicalize the nearest existing parent for a new target too. This
    // preserves safe aliases while the later handle walk refuses raced links.
    let normalized = normalize_lexically(&joined);
    let mut suffix = Vec::new();
    for ancestor in normalized.ancestors() {
        if let Ok(canonical) = ancestor.canonicalize() {
            let mut target = canonical;
            for name in suffix.iter().rev() {
                target.push(name);
            }
            if !is_within_workspace(&canon_ws, &target) {
                return Err(ToolError::PathTraversal(requested.into()));
            }
            return Ok(target);
        }
        if let Some(name) = ancestor.file_name() {
            suffix.push(name.to_os_string());
        }
    }
    Err(ToolError::Io(
        "workspace target could not be resolved".into(),
    ))
}

/// Canonical form of the workspace root for escape comparisons. The root
/// exists in every tool call, so `canonicalize` succeeds; the lexical
/// fallback only covers exotic failure modes without changing behavior.
pub(crate) fn canonical_root(workspace_root: &Path) -> PathBuf {
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
    check_private(path)?;
    verify_prefix_within(workspace_root, canon_ws, path, requested)
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

pub(crate) fn path_to_comparable_string(p: &Path) -> String {
    let mut s = p.to_string_lossy().to_string();
    // Strip Windows verbatim prefix \\?\ if present
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if s.starts_with(r"\\?\") {
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

/// Private stores remain denied even when the selected workspace is one of
/// these directories or a canonical alias to one. No personal store is read
/// to derive this policy; tests use synthetic component trees only.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Entire path is already ASCII-lowercased.
pub(crate) fn private_store(path: &Path) -> bool {
    let value = path_to_comparable_string(path)
        .replace('\\', "/")
        .to_ascii_lowercase();
    let parts: Vec<_> = value.split('/').filter(|part| !part.is_empty()).collect();
    parts.iter().any(|part| {
        matches!(
            *part,
            ".ssh"
                | ".aws"
                | ".gnupg"
                | ".kube"
                | ".azure"
                | ".git-credentials"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | "keychains"
                | "keychain"
                | "credentials"
                | "vault"
        ) || part.ends_with(".keychain")
            || part.ends_with(".keychain-db")
    }) || parts.windows(2).any(|pair| {
        matches!(
            pair,
            [
                ".config",
                "gh" | "gcloud" | "microsoft-edge" | "git" | "google-chrome" | "chromium"
            ] | [".docker", "config.json"]
                | ["share", "keyrings"]
                | [
                    "microsoft",
                    "protect" | "crypto" | "tokenbroker" | "identitycache"
                ]
                | [".mozilla" | "application support", "firefox"]
                | ["firefox", "profiles"]
                | ["chrome" | "edge" | "chromium" | "vivaldi", "user data"]
                | ["opera software", "opera stable" | "opera gx stable"]
                | ["bravesoftware", "brave-browser"]
                | ["application support", "google"]
        )
    }) || parts.contains(&"safari")
}
pub(crate) fn check_private(path: &Path) -> Result<(), ToolError> {
    if private_store(path)
        || path
            .canonicalize()
            .is_ok_and(|canonical| private_store(&canonical))
    {
        Err(ToolError::Io(
            "private credential or browser store is protected".into(),
        ))
    } else {
        Ok(())
    }
}
pub(super) fn policy_target(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case(".nexora"))
}
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Entire path is already ASCII-lowercased.
pub(crate) fn sensitive_write(path: &Path) -> bool {
    let value = path_to_comparable_string(path)
        .replace('\\', "/")
        .to_ascii_lowercase();
    let parts: Vec<_> = value.split('/').collect();
    parts.iter().any(|part| {
        part.starts_with(".env")
            || part.ends_with(".pem")
            || part.ends_with(".key")
            || part.starts_with("id_rsa")
            || *part == ".nexora"
    }) || parts
        .windows(2)
        .any(|pair| matches!(pair, [".git", "hooks" | "config"]))
}

/// Open a directory through no-follow handles from the volume root. Ambient
/// authority is used only for that root, never for an untrusted target path.
fn absolute_dir_nofollow(path: &Path) -> Result<cap_std::fs::Dir, ToolError> {
    use cap_fs_ext::DirExt;
    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => root.push(component.as_os_str()),
            Component::Normal(name) => names.push(name),
            _ => return Err(ToolError::Io("invalid capability root".into())),
        }
    }
    let mut dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(cap_error)?;
    for name in names {
        dir = dir.open_dir_nofollow(name).map_err(cap_error)?;
    }
    Ok(dir)
}
fn cap_error(_: std::io::Error) -> ToolError {
    ToolError::Io("workspace capability I/O was refused".into())
}
/// Canonical aliases are resolved by `resolve_path` before this operation;
/// every subsequently opened component refuses links, including raced swaps.
pub(super) fn secure_dir(
    workspace: &Path,
    target: &Path,
    create: bool,
) -> Result<cap_std::fs::Dir, ToolError> {
    use cap_fs_ext::DirExt;
    check_private(workspace)?;
    check_private(target)?;
    let root = canonical_root(workspace);
    let target = normalize_lexically(target);
    if !is_within_workspace(&root, &target) {
        return Err(ToolError::PathTraversal("protected target".into()));
    }
    let relative: PathBuf = target
        .components()
        .skip(root.components().count())
        .collect();
    let mut dir = absolute_dir_nofollow(&root)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ToolError::Io("invalid relative capability path".into()));
        };
        if create {
            match dir.create_dir(name) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(cap_error(err)),
            }
        }
        dir = dir.open_dir_nofollow(name).map_err(cap_error)?;
    }
    Ok(dir)
}
fn secure_open(
    workspace: &Path,
    target: &Path,
    write: bool,
) -> Result<cap_std::fs::File, ToolError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    check_private(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| ToolError::Io("invalid file target".into()))?;
    let name = target
        .file_name()
        .ok_or_else(|| ToolError::Io("invalid file target".into()))?;
    let dir = secure_dir(workspace, parent, write)?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.follow(FollowSymlinks::No);
    if write {
        options.write(true).create(true).truncate(true);
    } else {
        options.read(true);
    }
    dir.open_with(name, &options).map_err(cap_error)
}
pub(super) fn secure_read(workspace: &Path, target: &Path) -> Result<Vec<u8>, ToolError> {
    use std::io::Read;
    let mut file = secure_open(workspace, target, false)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(cap_error)?;
    Ok(bytes)
}
pub(super) fn secure_read_limited(
    workspace: &Path,
    target: &Path,
    max: u64,
) -> Result<Vec<u8>, ToolError> {
    use std::io::Read;
    let file = secure_open(workspace, target, false)?;
    if file.metadata().map_err(cap_error)?.len() > max {
        return Err(ToolError::Io("file exceeds the read limit".into()));
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(cap_error)?;
    if bytes.len() as u64 > max {
        return Err(ToolError::Io("file exceeds the read limit".into()));
    }
    Ok(bytes)
}
pub(super) fn secure_write(
    workspace: &Path,
    target: &Path,
    content: &[u8],
) -> Result<(), ToolError> {
    use std::io::Write;
    let mut file = secure_open(workspace, target, true)?;
    file.write_all(content).map_err(cap_error)
}
pub(super) fn safe_ignore(
    workspace: &Path,
) -> Result<crate::application::project_dir::IgnoreRules, ToolError> {
    let candidate = workspace.join(".nexora/.nexoraignore");
    match std::fs::symlink_metadata(&candidate) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(crate::application::project_dir::parse_ignore_rules(""))
        }
        Err(err) => return Err(cap_error(err)),
        Ok(_) => {}
    }
    let path = resolve_path(workspace, ".nexora/.nexoraignore")?;
    let text = String::from_utf8(secure_read_limited(workspace, &path, 256 * 1024)?)
        .map_err(|_| ToolError::Io("workspace ignore file is not valid UTF-8".into()))?;
    Ok(crate::application::project_dir::parse_ignore_rules(&text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::{
        test_support::{call, temp_workspace},
        ToolRegistry,
    };
    use serde_json::json;
    #[test]
    fn all_read_surfaces_protect_synthetic_private_roots_and_children() {
        let workspace = temp_workspace();
        for relative in [
            ".ssh",
            ".aws",
            ".gnupg",
            ".kube",
            ".config/gh",
            "Chrome/User Data",
            "Edge/User Data",
            "Firefox/Profiles",
            "Library/Keychains",
        ] {
            let private = workspace.join(relative);
            std::fs::create_dir_all(&private).unwrap();
            std::fs::write(private.join("secret"), "SYNTHETIC_PRIVATE_MARKER").unwrap();
            for root in [&workspace, &private] {
                let path = if root == &workspace {
                    format!("{relative}/secret")
                } else {
                    "secret".into()
                };
                assert!(
                    ToolRegistry::execute(&call("read_file", json!({"path":path})), root).is_err()
                );
                if root == &private {
                    assert!(
                        ToolRegistry::execute(&call("list_directory", json!({})), root).is_err()
                    );
                    assert!(ToolRegistry::execute(
                        &call("search_files", json!({"pattern":"MARKER"})),
                        root
                    )
                    .is_err());
                }
            }
        }
        for tool in [
            call("list_directory", json!({"recursive":true})),
            call("search_files", json!({"pattern":"MARKER"})),
        ] {
            let output = ToolRegistry::execute(&tool, &workspace).unwrap();
            assert!(!output.contains("secret"));
            assert!(!output.contains("MARKER"));
        }
        assert!(resolve_path(&workspace, "../outside").is_err());
        assert!(resolve_path(&workspace, ".ssh/../public").is_err());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    fn link_dir(target: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "synthetic junction creation failed"
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }
    #[test]
    fn canonical_aliases_and_swapped_parent_links_never_redirect_file_io() {
        let workspace = temp_workspace();
        let outside = temp_workspace();
        let private = workspace.join(".ssh");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("secret"), "synthetic private").unwrap();
        std::fs::write(outside.join("file"), "outside sentinel").unwrap();
        link_dir(&private, &workspace.join("alias"));
        assert!(resolve_path(&workspace, "alias/secret").is_err());
        let root_alias = workspace.join("root-alias");
        link_dir(&private, &root_alias);
        assert!(resolve_path(&root_alias, "secret").is_err());
        let directory = workspace.join("normal");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("file"), "inside").unwrap();
        let resolved = resolve_path(&workspace, "normal/file").unwrap();
        std::fs::rename(&directory, workspace.join("old")).unwrap();
        link_dir(&outside, &directory);
        assert!(secure_read(&workspace, &resolved).is_err());
        assert!(secure_write(&workspace, &resolved, b"changed").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("file")).unwrap(),
            "outside sentinel"
        );
        std::fs::remove_dir(&directory).unwrap();
        link_dir(&private, &directory);
        assert!(secure_read(&workspace, &resolved).is_err());
        assert!(secure_write(&workspace, &resolved, b"changed").is_err());
        std::fs::remove_dir_all(workspace).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn safe_in_workspace_alias_is_supported_and_unsafe_ignore_does_not_clear_rules() {
        let workspace = temp_workspace();
        let directory = workspace.join("normal");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("file"), "inside").unwrap();
        link_dir(&directory, &workspace.join("alias"));
        let path = resolve_path(&workspace, "alias/file").unwrap();
        assert_eq!(secure_read(&workspace, &path).unwrap(), b"inside");
        assert!(safe_ignore(&workspace).is_ok());
        std::fs::create_dir(workspace.join(".nexora")).unwrap();
        std::fs::create_dir(workspace.join(".nexora/.nexoraignore")).unwrap();
        assert!(safe_ignore(&workspace).is_err());
        assert!(ToolRegistry::execute(
            &call("search_files", json!({"pattern":"inside"})),
            &workspace
        )
        .is_err());
        std::fs::remove_dir_all(workspace).unwrap();
    }
    #[test]
    fn missing_file_aliases_stay_supported_but_policy_and_sensitive_aliases_do_not_widen() {
        use crate::application::agent::approval::{ApprovalGate, AutonomyMode};
        let workspace = temp_workspace();
        let normal = workspace.join("normal");
        std::fs::create_dir(&normal).unwrap();
        link_dir(&normal, &workspace.join("alias"));
        ToolRegistry::execute(
            &call(
                "write_file",
                json!({"path":"alias/new.txt","content":"safe"}),
            ),
            &workspace,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(normal.join("new.txt")).unwrap(),
            "safe"
        );
        link_dir(&normal, &workspace.join(".env-alias"));
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous);
        gate.bind_workspace(&workspace);
        let request = call(
            "write_file",
            json!({"path":".env-alias/new.txt","content":"x"}),
        );
        assert!(gate.metadata(&request).sensitive_write);
        assert!(!gate.metadata(&request).allow_for_run);
        link_dir(&normal, &workspace.join(".nexora"));
        for request in [
            call(
                "write_file",
                json!({"path":".nexora/new.txt","content":"changed"}),
            ),
            call("read_file", json!({"path":".nexora/new.txt"})),
            call("list_directory", json!({"path":".nexora"})),
            call(
                "search_files",
                json!({"directory":".nexora","pattern":"safe"}),
            ),
        ] {
            assert!(ToolRegistry::execute(&request, &workspace).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(normal.join("new.txt")).unwrap(),
            "safe"
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }
}
