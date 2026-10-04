//! Agent workspace folder: setting keys, guard, and recent list.
//!
//! The agent's filesystem tools are confined to one workspace root
//! (`application/agent/tools.rs` via `commands/agent.rs`). This module owns
//! the user-facing side of that root (ARCHITECTURE.md §5, application layer):
//!
//! - [`WORKSPACE_ROOT_KEY`] (`agent.workspace_root`): the chosen root,
//!   canonicalized before it is stored. Absent means the default root
//!   (the `agent_workspace` directory under the app-data dir, i.e. the
//!   pre-picker behavior).
//! - [`WORKSPACE_RECENT_KEY`] (`agent.workspace_recent`): a JSON array of up
//!   to [`WORKSPACE_RECENT_MAX`] (5) canonical paths, most-recent first.
//!
//! The guard ([`validate_workspace_root`]) rejects non-existent paths, UNC
//! network paths, per-platform system directories (and anything under them),
//! and drive/filesystem roots before anything is persisted. [`push_recent`]
//! maintains the 5-entry ring buffer. [`resolve_workspace_root`] re-validates
//! the stored setting on every use (re-canonicalise + blocklist) and falls
//! back to the default when it no longer resolves, so a path that becomes a
//! symlink/junction after being saved cannot widen the tool scope.
//!
//! Multi-root (registry): the active root lives in [`WORKSPACE_ROOT_KEY`]
//! and the registry in [`WORKSPACE_ROOTS_KEY`] (a JSON array of canonical
//! paths, most-recent first, with NO cap — registering one more root must
//! never silently evict another). [`WORKSPACE_RECENT_KEY`] stays a 5-entry
//! MRU ring for the single-root picker history only; pre-split installs may
//! still carry registry entries there, so [`stored_registry`] merges both
//! keys. [`list_roots`] / [`register_root`] / [`unregister_root`] own the
//! registry; nesting is refused ([`paths_overlap`]) so registered roots stay
//! disjoint. Every root-aware feature resolves through
//! [`resolve_workspace_root`], so switching the active root moves all of them
//! with no per-feature changes.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;

/// Setting key for the chosen agent workspace root.
pub(crate) const WORKSPACE_ROOT_KEY: &str = "agent.workspace_root";

/// Setting key for the recent workspace roots (JSON array, most-recent first).
/// MRU history for the single-root picker only — NOT the registry (see
/// [`WORKSPACE_ROOTS_KEY`]).
pub(crate) const WORKSPACE_RECENT_KEY: &str = "agent.workspace_recent";

/// Setting key for the registered workspace roots (JSON array of canonical
/// paths, most-recent first). Unbounded on purpose: the registry must never
/// silently evict entries, so it lives apart from the 5-entry recent ring.
pub(crate) const WORKSPACE_ROOTS_KEY: &str = "agent.workspace_roots";

/// Maximum number of recent workspace roots kept.
pub(crate) const WORKSPACE_RECENT_MAX: usize = 5;

/// Longest workspace root accepted (matches the `conversations.workspace_root`
/// `CHECK (length <= 1024)` bound).
pub(crate) const WORKSPACE_ROOT_MAX_LEN: usize = 1024;

/// Guard failure for a workspace root candidate. Carries no secret: only the
/// rejected path shape is described, never a credential or message payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceError {
    /// The candidate path is not an acceptable workspace root.
    Invalid(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid workspace root: {reason}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Validate `raw` as a workspace root candidate and return its canonical form.
///
/// Steps: trim, reject empty / overlong / null-byte / UNC paths, canonicalize
/// via the filesystem (non-existent paths fail here), require a directory,
/// then reject per-platform system directories (and children) and
/// drive/filesystem roots. The returned path is the canonicalized absolute
/// path with any Windows verbatim prefix stripped, ready to store.
pub(crate) fn validate_workspace_root(raw: &str) -> Result<PathBuf, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::Invalid(
            "path must not be empty".to_string(),
        ));
    }
    if trimmed.contains('\0') {
        return Err(WorkspaceError::Invalid(
            "path contains a null byte".to_string(),
        ));
    }
    if trimmed.len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(WorkspaceError::Invalid(
            "path exceeds 1024 characters".to_string(),
        ));
    }
    if is_unc_text(trimmed) {
        return Err(WorkspaceError::Invalid(
            "UNC network paths are not allowed".to_string(),
        ));
    }
    let canonical = std::fs::canonicalize(trimmed)
        .map_err(|_| WorkspaceError::Invalid("path does not exist".to_string()))?;
    let canonical = strip_verbatim(canonical);
    if is_unc_path(&canonical) {
        return Err(WorkspaceError::Invalid(
            "UNC network paths are not allowed".to_string(),
        ));
    }
    if !canonical.is_dir() {
        return Err(WorkspaceError::Invalid(
            "path is not a directory".to_string(),
        ));
    }
    if is_system_path(&canonical) {
        return Err(WorkspaceError::Invalid(
            "a system directory is not allowed".to_string(),
        ));
    }
    if is_drive_root(&canonical) {
        return Err(WorkspaceError::Invalid(
            "a drive root is not allowed".to_string(),
        ));
    }
    let text = canonical.to_string_lossy();
    if text.len() > WORKSPACE_ROOT_MAX_LEN {
        return Err(WorkspaceError::Invalid(
            "canonical path exceeds 1024 characters".to_string(),
        ));
    }
    Ok(canonical)
}

/// Whether `path` is a guarded system directory or lives under one
/// (case-insensitive, either separator).
///
/// Windows: `%SystemRoot%` on any drive (`C:\Windows`, `D:\Windows`, ...),
/// plus `C:\Program Files`, `C:\Program Files (x86)`, `C:\ProgramData`,
/// `C:\Users`, `C:\Windows.old`, and `C:\$Recycle.Bin`. Unix: `/etc`, `/usr`,
/// `/bin`, `/sbin`, `/boot`, `/dev`, `/proc`, `/sys`, `/System`, `/Library`.
/// The active list is selected with `cfg(windows)`; this widens the previous
/// `C:\Windows`-only guard so the agent cannot be scoped into a system
/// directory on another drive or a Unix system tree.
pub(crate) fn is_system_path(path: &Path) -> bool {
    is_system_path_inner(path, true)
}

/// Whether `path` is a protected system location for file attachments
/// (case-insensitive, either separator).
///
/// Same single blocklist as [`is_system_path`] except the `C:\Users` tree is
/// excluded: every legitimate user file lives under `C:\Users`, so the
/// workspace-root guard cannot be reused for attachments. The any-drive
/// `%SystemRoot%` rule still applies.
pub(crate) fn is_system_file_path(path: &Path) -> bool {
    is_system_path_inner(path, false)
}

fn is_system_path_inner(path: &Path, include_users_tree: bool) -> bool {
    let normalized = normalize_guard_path(path);
    #[cfg(windows)]
    {
        // `%SystemRoot%` on any drive: `<letter>:\Windows` or a child of it.
        let bytes = normalized.as_bytes();
        if normalized.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let rest = &normalized[2..];
            if rest == r"\windows" || rest.starts_with(r"\windows\") {
                return true;
            }
        }
        for entry in [
            r"c:\program files",
            r"c:\program files (x86)",
            r"c:\programdata",
            r"c:\users",
            r"c:\windows.old",
            r"c:\$recycle.bin",
        ] {
            if !include_users_tree && entry == r"c:\users" {
                continue;
            }
            if normalized == entry || normalized.starts_with(&format!("{entry}\\")) {
                return true;
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = include_users_tree;
        for entry in [
            r"\etc",
            r"\usr",
            r"\bin",
            r"\sbin",
            r"\boot",
            r"\dev",
            r"\proc",
            r"\sys",
            r"\system",
            r"\library",
        ] {
            if normalized == entry || normalized.starts_with(&format!("{entry}\\")) {
                return true;
            }
        }
        false
    }
}

/// Whether `path` is a UNC network path (`\\server\share`, either separator).
/// Scoping the agent to a network share would hand tool access to a location
/// outside the local machine's guard assumptions, so it is rejected.
pub(crate) fn is_unc_path(path: &Path) -> bool {
    is_unc_text(path.to_string_lossy().as_ref())
}

/// Whether raw path text names a UNC network path. Checked on the untrimmed
/// input (before canonicalization) so non-existent shares still fail with the
/// UNC reason rather than the generic missing-path reason. A verbatim `\\?\`
/// prefix denotes a local path, not a share — except `\\?\UNC\server\share`,
/// which is a share in verbatim form.
fn is_unc_text(text: &str) -> bool {
    let normalized = text.replace('/', "\\");
    if let Some(rest) = normalized.strip_prefix(r"\\?\") {
        return rest.len() > 4 && rest[..4].eq_ignore_ascii_case(r"UNC\");
    }
    normalized.starts_with(r"\\")
}

/// Lowercase, separator-agnostic, verbatim-prefix-free form of `path` for
/// guard comparisons.
fn normalize_guard_path(path: &Path) -> String {
    let mut text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        text = format!(r"\\{rest}");
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    let lowered = text.to_lowercase().replace('/', "\\");
    lowered.trim_end_matches('\\').to_string()
}

/// Whether `path` is a drive root (`C:\`, `C:/`, `C:`) or a bare filesystem
/// root (`/`). Such roots would scope the tools to an entire drive.
pub(crate) fn is_drive_root(path: &Path) -> bool {
    let mut text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        text = format!(r"\\{rest}");
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    let normalized = text.replace('/', "\\");
    let trimmed = normalized.trim_end_matches('\\');
    // `C:` or `C` (drive without separator) after trimming.
    if trimmed.len() == 2
        && trimmed.as_bytes()[1] == b':'
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
    {
        return true;
    }
    // `C:\` trims to `C:`; `/` trims to empty.
    if trimmed.is_empty() {
        return true;
    }
    // No parent means filesystem root on this platform.
    path.parent().is_none()
}

/// Strip the Windows verbatim (`\\?\`) prefix so stored paths stay readable
/// and comparable with non-verbatim joins.
pub(crate) fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// Parse the stored recent list (JSON array of strings). Corrupt or absent
/// values yield an empty list rather than an error.
#[must_use]
pub(crate) fn parse_recent(raw: Option<&str>) -> Vec<String> {
    let Some(text) = raw else {
        return Vec::new();
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<String>>(trimmed) {
        Ok(items) => items
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(WORKSPACE_RECENT_MAX)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Insert `new_root` at the front of the recent list, de-duplicated and capped
/// at [`WORKSPACE_RECENT_MAX`], and return the JSON to store.
#[must_use]
pub(crate) fn push_recent(existing_raw: Option<&str>, new_root: &str) -> String {
    let mut items = parse_recent(existing_raw);
    items.retain(|item| item != new_root);
    items.insert(0, new_root.to_string());
    items.truncate(WORKSPACE_RECENT_MAX);
    serde_json::to_string(&items).unwrap_or_else(|_| format!(r#"["{new_root}"]"#))
}

/// Parse the stored registry ([`WORKSPACE_ROOTS_KEY`], JSON array of
/// strings). Same tolerance as [`parse_recent`] (corrupt or absent values
/// yield an empty list) but with NO cap — the registry never evicts.
#[must_use]
pub(crate) fn parse_registry(raw: Option<&str>) -> Vec<String> {
    let Some(text) = raw else {
        return Vec::new();
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<String>>(trimmed) {
        Ok(items) => items
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Insert `new_root` at the front of the registry, de-duplicated and
/// uncapped, and return the JSON to store.
#[must_use]
pub(crate) fn push_registry(existing_raw: Option<&str>, new_root: &str) -> String {
    let mut items = parse_registry(existing_raw);
    items.retain(|item| item != new_root);
    items.insert(0, new_root.to_string());
    serde_json::to_string(&items).unwrap_or_else(|_| format!(r#"["{new_root}"]"#))
}

/// Resolve the effective workspace root for tool scoping: the stored
/// [`WORKSPACE_ROOT_KEY`] when it still validates as a workspace root, else
/// `default_root` (the pre-picker `agent_workspace` behavior).
///
/// The stored path is re-validated on every use (re-canonicalise, then the
/// UNC / system-directory / drive-root blocklist), not only when it is saved:
/// a stored path can become a symlink or junction pointing at a guarded
/// location after the fact, and must then fall back to the default.
#[must_use]
pub(crate) fn resolve_workspace_root(db: &Database, default_root: &Path) -> PathBuf {
    let stored = SettingsService::new(db).read(WORKSPACE_ROOT_KEY);
    if let Ok(Some(value)) = stored {
        let trimmed = value.trim();
        if !trimmed.is_empty()
            && !trimmed.contains('\0')
            && trimmed.len() <= WORKSPACE_ROOT_MAX_LEN
            && !is_unc_text(trimmed)
        {
            if let Ok(canonical) = std::fs::canonicalize(trimmed) {
                let canonical = strip_verbatim(canonical);
                if canonical.is_dir()
                    && !is_unc_path(&canonical)
                    && !is_system_path(&canonical)
                    && !is_drive_root(&canonical)
                    && canonical.to_string_lossy().len() <= WORKSPACE_ROOT_MAX_LEN
                {
                    return canonical;
                }
            }
        }
    }
    default_root.to_path_buf()
}

/// Multi-root registry view: the active root plus every registered root.
///
/// Storage decision (documented per task scope): the registry is the
/// [`WORKSPACE_ROOTS_KEY`] settings entry (an unbounded JSON array of
/// canonical paths, most-recent first) plus [`WORKSPACE_ROOT_KEY`] (the
/// active root). No `SQLite` migration: the registry is a plain list of paths
/// with no relational joins, and the settings store already owns workspace
/// keys (including the `set_setting` syntactic validation in
/// `commands/settings.rs`). `active` is the resolved effective root (the
/// stored value when it still validates, else `default_root`); `roots` is
/// the active root first, then the stored registry entries de-duplicated.
/// Stale entries (deleted directories) are listed as-is — removal is explicit
/// via [`unregister_root`], never silent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RootsList {
    /// The effective root every `resolve_workspace_root` consumer follows.
    pub active: String,
    /// Every registered root, active first, de-duplicated.
    pub roots: Vec<String>,
}

/// List the registry: the resolved active root plus the stored entries.
///
/// Scope decision (documented per task scope): switching is via this shared
/// active-root state, not per-command root params — every feature that calls
/// [`resolve_workspace_root`] (git panel, audit, terminal, agent runs,
/// GitHub lists, flags, diagnostics, dep tools, `.nexora/` init/profiles)
/// follows the active root with zero per-feature changes. Search and the
/// conversation list are NOT root-scoped (global `SQLite` content; conversation
/// rows only carry a `workspace_root` tag for per-folder history).
#[must_use]
pub(crate) fn list_roots(db: &Database, default_root: &Path) -> RootsList {
    let active = resolve_workspace_root(db, default_root);
    let active_text = active.to_string_lossy().to_string();
    let mut roots = vec![active_text.clone()];
    for item in stored_registry(db) {
        if item != active_text && !roots.contains(&item) {
            roots.push(item);
        }
    }
    RootsList {
        active: active_text,
        roots,
    }
}

/// Register `raw` as a root and make it the active root (idempotent).
///
/// Validation: the shared [`validate_workspace_root`] guard (must exist,
/// canonicalized, no UNC / system / drive-root) plus registry rules —
/// duplicates collapse to set-active (the registry already de-duplicates), and
/// nesting is REFUSED in both directions (a root inside another root, or a
/// root containing an existing root): tool scoping, audit, and git ops all
/// assume disjoint roots, and nested roots would double-scan and confuse the
/// per-folder history tags. Only entries that still resolve on disk
/// participate in the overlap check; stale entries are dead weight awaiting
/// explicit removal, never a veto.
///
/// Persistence touches three keys: [`WORKSPACE_ROOT_KEY`] (the new active
/// root), [`WORKSPACE_ROOTS_KEY`] (the unbounded registry — never truncated,
/// so no registration silently evicts another), and [`WORKSPACE_RECENT_KEY`]
/// (the 5-entry MRU ring for the single-root picker history only).
pub(crate) fn register_root(db: &Database, raw: &str) -> Result<PathBuf, WorkspaceError> {
    let canonical = validate_workspace_root(raw)?;
    let text = canonical.to_string_lossy().to_string();
    for other in stored_registry(db) {
        if other == text {
            continue;
        }
        let Ok(other_canonical) = std::fs::canonicalize(other.as_str()) else {
            continue;
        };
        let other_canonical = strip_verbatim(other_canonical);
        if paths_overlap(&canonical, &other_canonical) {
            return Err(WorkspaceError::Invalid(
                "path overlaps an existing root".to_string(),
            ));
        }
    }
    let service = SettingsService::new(db);
    service
        .write(WORKSPACE_ROOT_KEY, Some(text.as_str()))
        .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    let existing_registry = service.read(WORKSPACE_ROOTS_KEY).ok().flatten();
    let next_registry = push_registry(existing_registry.as_deref(), text.as_str());
    service
        .write(WORKSPACE_ROOTS_KEY, Some(next_registry.as_str()))
        .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    let existing = service.read(WORKSPACE_RECENT_KEY).ok().flatten();
    let next = push_recent(existing.as_deref(), text.as_str());
    service
        .write(WORKSPACE_RECENT_KEY, Some(next.as_str()))
        .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    Ok(canonical)
}

/// Unregister `raw` from the registry and return the updated view.
///
/// Matching is by normalized text (separator/case/verbatim-insensitive), so a
/// stale entry for a deleted directory can still be removed — `validate` is
/// deliberately NOT used here (it requires existence). The entry is forgotten
/// from both the registry ([`WORKSPACE_ROOTS_KEY`]) and the picker history
/// ([`WORKSPACE_RECENT_KEY`]). Removing the active
/// root clears [`WORKSPACE_ROOT_KEY`], so resolution falls back to the
/// default root; every root-aware feature follows on its next manual refresh
/// (no watching/HMR on switch — documented out of scope). Unknown paths fail
/// with fixed vocabulary. Errors and results are secret-free: fixed strings
/// plus caller-supplied paths only (paths are user data, rendered raw).
pub(crate) fn unregister_root(
    db: &Database,
    raw: &str,
    default_root: &Path,
) -> Result<RootsList, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::Invalid(
            "path must not be empty".to_string(),
        ));
    }
    let service = SettingsService::new(db);
    let stored = stored_registry(db);
    let wanted = normalize_guard_path(Path::new(trimmed));
    let matched = stored
        .iter()
        .find(|item| normalize_guard_path(Path::new(item.as_str())) == wanted)
        .cloned();
    let Some(matched) = matched else {
        return Err(WorkspaceError::Invalid(
            "path is not a registered root".to_string(),
        ));
    };
    let drop_matched = |items: Vec<String>| {
        items
            .into_iter()
            .filter(|item| {
                normalize_guard_path(Path::new(item.as_str())) != wanted && item != &matched
            })
            .collect::<Vec<String>>()
    };
    let registry_raw = service.read(WORKSPACE_ROOTS_KEY).ok().flatten();
    let kept_registry = drop_matched(parse_registry(registry_raw.as_deref()));
    service
        .write(
            WORKSPACE_ROOTS_KEY,
            Some(
                serde_json::to_string(&kept_registry)
                    .unwrap_or_else(|_| "[]".to_string())
                    .as_str(),
            ),
        )
        .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    let recent_raw = service.read(WORKSPACE_RECENT_KEY).ok().flatten();
    let kept_recent = drop_matched(parse_recent(recent_raw.as_deref()));
    service
        .write(
            WORKSPACE_RECENT_KEY,
            Some(
                serde_json::to_string(&kept_recent)
                    .unwrap_or_else(|_| "[]".to_string())
                    .as_str(),
            ),
        )
        .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    let stored_active = service.read(WORKSPACE_ROOT_KEY).ok().flatten();
    let active_is_removed = stored_active
        .as_deref()
        .is_some_and(|active| normalize_guard_path(Path::new(active.trim())) == wanted);
    if active_is_removed {
        service
            .delete(WORKSPACE_ROOT_KEY)
            .map_err(|_| WorkspaceError::Invalid("the root could not be saved".to_string()))?;
    }
    Ok(list_roots(db, default_root))
}

/// Read the stored registry entries (tolerant of corrupt input).
///
/// The registry lives in [`WORKSPACE_ROOTS_KEY`] (uncapped). Entries still
/// sitting in the legacy [`WORKSPACE_RECENT_KEY`] ring (pre-split installs,
/// where the ring doubled as the registry) are merged in — registry first,
/// then legacy extras — so upgrading never loses registered roots.
#[must_use]
fn stored_registry(db: &Database) -> Vec<String> {
    let service = SettingsService::new(db);
    let mut merged = parse_registry(service.read(WORKSPACE_ROOTS_KEY).ok().flatten().as_deref());
    for item in parse_recent(service.read(WORKSPACE_RECENT_KEY).ok().flatten().as_deref()) {
        if !merged.contains(&item) {
            merged.push(item);
        }
    }
    merged
}

/// Whether two canonical roots overlap: equal, or one contains the other.
/// Both inputs must already be canonicalized; comparison reuses the
/// separator/case/verbatim-insensitive guard normalization.
#[must_use]
fn paths_overlap(a: &Path, b: &Path) -> bool {
    let norm_a = normalize_guard_path(a);
    let norm_b = normalize_guard_path(b);
    norm_a == norm_b
        || norm_b.starts_with(&format!("{norm_a}\\"))
        || norm_a.starts_with(&format!("{norm_b}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_base() -> PathBuf {
        // The OS temp dir can itself sit under the blocklist (e.g. TEMP under
        // `C:\Users`); in that case scratch under the crate target dir, which
        // lives outside every blocklist entry on dev machines and is ignored
        // by version control.
        let tmp = std::env::temp_dir();
        if !is_system_path(&tmp) && !is_drive_root(&tmp) {
            return tmp;
        }
        let scratch = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("nexora-test-tmp");
        std::fs::create_dir_all(&scratch).expect("create test scratch base");
        scratch.canonicalize().unwrap_or(scratch)
    }

    fn temp_dir() -> PathBuf {
        let base = test_base();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!("nexora-workspace-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.canonicalize().unwrap_or(dir)
    }

    #[test]
    fn guard_rejects_nonexistent_path() {
        let missing = std::env::temp_dir().join("nexora-workspace-missing-9f3c2a1e");
        let _ = std::fs::remove_dir_all(&missing);
        let err = validate_workspace_root(missing.to_string_lossy().as_ref())
            .expect_err("missing path must be rejected");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path does not exist"
        );
    }

    #[test]
    fn guard_rejects_empty_path() {
        let err = validate_workspace_root("   ").expect_err("empty must be rejected");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path must not be empty"
        );
    }

    #[cfg(windows)]
    #[test]
    fn guard_rejects_windows_blocklist_entries() {
        // Every Windows blocklist entry: the directory itself, a child, and a
        // case/separator variant. Pure `is_system_path` checks (no FS needed)
        // so each entry is covered even where the directory does not exist.
        for entry in [
            r"C:\Windows",
            r"D:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\Users",
            r"C:\Windows.old",
            r"C:\$Recycle.Bin",
        ] {
            assert!(is_system_path(Path::new(entry)), "{entry} blocked");
            assert!(
                is_system_path(&Path::new(entry).join("child")),
                "{entry} child blocked"
            );
        }
        assert!(is_system_path(Path::new(r"d:\windows\system32")));
        assert!(is_system_path(Path::new("C:/Windows")));
        assert!(is_system_path(Path::new(r"c:\PROGRAM FILES\app")));
        assert!(is_system_path(Path::new(r"C:\Users\alice")));
        assert!(!is_system_path(Path::new(r"C:\dev\proj")));
        assert!(!is_system_path(Path::new(r"D:\data")));
        // End-to-end through the guard on Windows, where C:\Windows exists.
        if Path::new(r"C:\Windows").is_dir() {
            let err =
                validate_workspace_root(r"C:\Windows").expect_err("system dir must be rejected");
            assert_eq!(
                format!("{err}"),
                "invalid workspace root: a system directory is not allowed"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn guard_rejects_unix_blocklist_entries() {
        for entry in [
            "/etc", "/usr", "/bin", "/sbin", "/boot", "/dev", "/proc", "/sys", "/System",
            "/Library",
        ] {
            assert!(is_system_path(Path::new(entry)), "{entry} blocked");
            assert!(
                is_system_path(&Path::new(entry).join("child")),
                "{entry} child blocked"
            );
        }
        assert!(!is_system_path(Path::new("/home/alice")));
    }

    #[test]
    fn guard_rejects_unc_paths() {
        assert!(is_unc_path(Path::new(r"\\server\share")));
        assert!(is_unc_path(Path::new("//server/share")));
        assert!(!is_unc_path(Path::new(r"C:\dev\proj")));
        assert!(!is_unc_path(Path::new("/home/alice")));
        // A verbatim local path is not a share; the verbatim UNC form is.
        assert!(!is_unc_text(r"\\?\C:\dev\proj"));
        assert!(is_unc_text(r"\\?\UNC\server\share"));
        let err = validate_workspace_root(r"\\server\share\no-such-workspace-9f3c2a1e")
            .expect_err("UNC must be rejected");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: UNC network paths are not allowed"
        );
        let err = validate_workspace_root("//server/share/no-such-workspace-9f3c2a1e")
            .expect_err("UNC with forward slashes must be rejected");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: UNC network paths are not allowed"
        );
    }

    #[test]
    fn guard_rejects_drive_roots() {
        assert!(is_drive_root(Path::new(r"C:\")));
        assert!(is_drive_root(Path::new(r"C:/")));
        assert!(is_drive_root(Path::new(r"C:")));
        assert!(is_drive_root(Path::new("/")));
        assert!(!is_drive_root(Path::new(r"C:\Users")));
        if Path::new(r"C:\").is_dir() {
            let err = validate_workspace_root(r"C:\").expect_err("drive root must be rejected");
            assert_eq!(
                format!("{err}"),
                "invalid workspace root: a drive root is not allowed"
            );
        }
    }

    #[test]
    fn guard_accepts_valid_directory_and_canonicalizes() {
        let dir = temp_dir();
        let resolved =
            validate_workspace_root(dir.to_string_lossy().as_ref()).expect("valid dir passes");
        assert!(resolved.is_absolute());
        assert!(resolved.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_ring_buffer_keeps_five_most_recent_first() {
        let mut raw: Option<String> = None;
        for name in ["a", "b", "c", "d", "e", "f"] {
            let next = push_recent(raw.as_deref(), name);
            raw = Some(next);
        }
        let items = parse_recent(raw.as_deref());
        assert_eq!(items, vec!["f", "e", "d", "c", "b"]);
    }

    #[test]
    fn recent_push_deduplicates_and_moves_to_front() {
        let first = push_recent(None, "a");
        let second = push_recent(Some(&first), "b");
        let third = push_recent(Some(&second), "a");
        assert_eq!(parse_recent(Some(&third)), vec!["a", "b"]);
    }

    #[test]
    fn recent_parse_tolerates_corrupt_input() {
        assert_eq!(parse_recent(None), Vec::<String>::new());
        assert_eq!(parse_recent(Some("not json")), Vec::<String>::new());
        assert_eq!(parse_recent(Some("")), Vec::<String>::new());
    }

    #[test]
    fn registry_parse_and_push_are_uncapped() {
        assert_eq!(parse_registry(None), Vec::<String>::new());
        assert_eq!(parse_registry(Some("not json")), Vec::<String>::new());
        let mut raw: Option<String> = None;
        for name in ["a", "b", "c", "d", "e", "f", "g"] {
            let next = push_registry(raw.as_deref(), name);
            raw = Some(next);
        }
        assert_eq!(
            parse_registry(raw.as_deref()),
            vec!["g", "f", "e", "d", "c", "b", "a"]
        );
        // Re-adding moves to the front without duplicating.
        let moved = push_registry(raw.as_deref(), "c");
        assert_eq!(
            parse_registry(Some(&moved)),
            vec!["c", "g", "f", "e", "d", "b", "a"]
        );
    }

    #[test]
    fn registry_holds_more_than_five_roots_without_eviction() {
        // Fix 1 regression: the registry used to share the 5-entry recent
        // ring, so registering a 6th root silently evicted the oldest. The
        // split registry key is unbounded; the recent ring stays a 5-entry
        // MRU history only.
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let mut dirs = Vec::new();
        let mut canonicals = Vec::new();
        for _ in 0..6 {
            let dir = temp_dir();
            let canon = strip_verbatim(std::fs::canonicalize(&dir).expect("root resolves"))
                .to_string_lossy()
                .to_string();
            register_root(&db, dir.to_string_lossy().as_ref()).expect("register root");
            dirs.push(dir);
            canonicals.push(canon);
        }
        let listed = list_roots(&db, &fallback);
        assert_eq!(listed.roots.len(), 6, "no registry entry may be evicted");
        for canon in &canonicals {
            assert!(
                listed.roots.contains(canon),
                "missing registry entry {canon}"
            );
        }
        assert_eq!(listed.active, canonicals[5]);
        // The picker-history ring still caps at 5 (the 5 most recent).
        let service = crate::application::settings::SettingsService::new(&db);
        let recent_raw = service
            .read(WORKSPACE_RECENT_KEY)
            .ok()
            .flatten()
            .expect("recent ring stored");
        let recent = parse_recent(Some(&recent_raw));
        assert_eq!(recent.len(), 5);
        assert!(!recent.contains(&canonicals[0]));
        for canon in &canonicals[1..] {
            assert!(recent.contains(canon), "recent ring drops oldest: {canon}");
        }
        for dir in dirs.into_iter().chain([fallback]) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn registry_merges_legacy_recent_entries() {
        // Pre-split installs kept the registry in the 5-entry recent ring;
        // upgrading must not lose those roots even before the next register.
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let dir_a = temp_dir();
        let dir_b = temp_dir();
        let canon_a = strip_verbatim(std::fs::canonicalize(&dir_a).expect("A resolves"))
            .to_string_lossy()
            .to_string();
        let canon_b = strip_verbatim(std::fs::canonicalize(&dir_b).expect("B resolves"))
            .to_string_lossy()
            .to_string();
        let service = crate::application::settings::SettingsService::new(&db);
        service
            .write(WORKSPACE_ROOT_KEY, Some(canon_a.as_str()))
            .expect("write legacy active");
        service
            .write(
                WORKSPACE_RECENT_KEY,
                Some(
                    serde_json::to_string(&vec![canon_b.clone(), canon_a.clone()])
                        .expect("legacy ring JSON")
                        .as_str(),
                ),
            )
            .expect("write legacy ring");
        let listed = list_roots(&db, &fallback);
        assert_eq!(listed.active, canon_a);
        assert!(listed.roots.contains(&canon_a));
        assert!(listed.roots.contains(&canon_b));
        for dir in [fallback, dir_a, dir_b] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn unregister_forgets_registry_and_recent_entries() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let dir_a = temp_dir();
        let dir_b = temp_dir();
        register_root(&db, dir_a.to_string_lossy().as_ref()).expect("register A");
        register_root(&db, dir_b.to_string_lossy().as_ref()).expect("register B");
        unregister_root(&db, dir_a.to_string_lossy().as_ref(), &fallback).expect("remove A");
        let service = crate::application::settings::SettingsService::new(&db);
        let registry_raw = service
            .read(WORKSPACE_ROOTS_KEY)
            .ok()
            .flatten()
            .expect("registry stored");
        let recent_raw = service
            .read(WORKSPACE_RECENT_KEY)
            .ok()
            .flatten()
            .expect("recent stored");
        let registry = parse_registry(Some(&registry_raw));
        let recent = parse_recent(Some(&recent_raw));
        assert_eq!(registry.len(), 1);
        assert_eq!(recent.len(), 1);
        assert_eq!(registry, recent);
        let canon_a = strip_verbatim(std::fs::canonicalize(&dir_a).expect("A resolves"))
            .to_string_lossy()
            .to_string();
        assert!(!registry.contains(&canon_a));
        for dir in [fallback, dir_a, dir_b] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn nested_path_refused_on_the_shared_registry_path() {
        // Fix 2 regression: the legacy `set_workspace_root` command used to
        // validate existence only and write the keys directly, so a nested
        // path slipped past the disjoint-roots check. It now delegates to
        // `register_root` (pinned by the delegation test in
        // `commands/workspace.rs`), so the nested attempt below — the exact
        // call the legacy setter makes — is refused with fixed vocabulary.
        let db = crate::infrastructure::database::in_memory_database();
        let outer = temp_dir();
        register_root(&db, outer.to_string_lossy().as_ref()).expect("register outer");
        let inner = outer.join("child-legacy-3c9d");
        std::fs::create_dir_all(&inner).expect("create child dir");
        let err = register_root(&db, inner.to_string_lossy().as_ref())
            .expect_err("legacy-shaped nested add must be refused");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path overlaps an existing root"
        );
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn tool_scope_resolves_setting_root_over_default() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        // No setting: the default (pre-picker behavior) is used.
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        // A stored directory wins over the default (returned canonicalized).
        let chosen = temp_dir();
        crate::application::settings::SettingsService::new(&db)
            .write(WORKSPACE_ROOT_KEY, Some(chosen.to_string_lossy().as_ref()))
            .expect("write setting");
        let expected = strip_verbatim(std::fs::canonicalize(&chosen).expect("stored dir resolves"));
        assert_eq!(resolve_workspace_root(&db, &fallback), expected);
        // A stored path that no longer exists falls back to the default.
        crate::application::settings::SettingsService::new(&db)
            .write(
                WORKSPACE_ROOT_KEY,
                Some(fallback.join("gone-4d2a").to_string_lossy().as_ref()),
            )
            .expect("write missing");
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        // A stored directory deleted after being saved falls back too.
        let doomed = temp_dir();
        crate::application::settings::SettingsService::new(&db)
            .write(WORKSPACE_ROOT_KEY, Some(doomed.to_string_lossy().as_ref()))
            .expect("write doomed");
        assert_eq!(
            resolve_workspace_root(&db, &fallback),
            strip_verbatim(doomed.canonicalize().unwrap_or(doomed.clone()))
        );
        std::fs::remove_dir_all(&doomed).expect("delete stored dir");
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        let _ = std::fs::remove_dir_all(&fallback);
        let _ = std::fs::remove_dir_all(temp_dir());
    }

    #[cfg(windows)]
    #[test]
    fn tool_scope_revalidates_stored_system_path() {
        // A stored path inside the blocklist is rejected on every use, even
        // though it names an existing directory: the guard re-runs at resolve
        // time in case the value became a symlink/junction after being saved.
        if !Path::new(r"C:\Windows").is_dir() {
            return;
        }
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        crate::application::settings::SettingsService::new(&db)
            .write(WORKSPACE_ROOT_KEY, Some(r"C:\Windows"))
            .expect("write system path");
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        let _ = std::fs::remove_dir_all(&fallback);
    }

    #[test]
    fn roots_register_two_roots_and_switch_active() {
        // Acceptance core: two real local roots registered, active switches,
        // and the shared resolver (used by git panel, audit, terminal, ...)
        // follows the switch.
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let root_a = temp_dir();
        let root_b = temp_dir();
        register_root(&db, root_a.to_string_lossy().as_ref()).expect("register A");
        let canon_a = strip_verbatim(std::fs::canonicalize(&root_a).expect("root A resolves"))
            .to_string_lossy()
            .to_string();
        let canon_b = strip_verbatim(std::fs::canonicalize(&root_b).expect("root B resolves"))
            .to_string_lossy()
            .to_string();
        register_root(&db, root_b.to_string_lossy().as_ref()).expect("register B");
        let listed = list_roots(&db, &fallback);
        assert_eq!(listed.roots.len(), 2, "two disjoint roots registered");
        assert!(listed.roots.contains(&canon_a));
        assert!(listed.roots.contains(&canon_b));
        assert_eq!(listed.active, canon_b);
        assert_eq!(
            resolve_workspace_root(&db, &fallback),
            PathBuf::from(&canon_b)
        );
        // Switch back to A via idempotent re-add: registry size unchanged.
        register_root(&db, root_a.to_string_lossy().as_ref()).expect("re-add A");
        let switched = list_roots(&db, &fallback);
        assert_eq!(switched.roots.len(), 2);
        assert_eq!(switched.active, canon_a);
        assert_eq!(
            resolve_workspace_root(&db, &fallback),
            PathBuf::from(&canon_a)
        );
        for dir in [fallback, root_a, root_b] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn roots_reject_nesting_in_both_directions() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let outer = temp_dir();
        register_root(&db, outer.to_string_lossy().as_ref()).expect("register outer");
        let inner = outer.join("child-7e1b");
        std::fs::create_dir_all(&inner).expect("create child dir");
        let err = register_root(&db, inner.to_string_lossy().as_ref())
            .expect_err("nested root must be refused");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path overlaps an existing root"
        );
        // And the reverse: a fresh registry holding the child refuses the parent.
        let db2 = crate::infrastructure::database::in_memory_database();
        register_root(&db2, inner.to_string_lossy().as_ref()).expect("register inner");
        let err = register_root(&db2, outer.to_string_lossy().as_ref())
            .expect_err("containing root must be refused");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path overlaps an existing root"
        );
        // Registry state untouched by the refused adds.
        assert_eq!(list_roots(&db, &fallback).roots.len(), 1);
        let _ = std::fs::remove_dir_all(&outer);
        let _ = std::fs::remove_dir_all(&fallback);
    }

    #[test]
    fn roots_reject_missing_paths_and_unknown_removal() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let missing = std::env::temp_dir().join("nexora-roots-missing-9f3c2a1e");
        let _ = std::fs::remove_dir_all(&missing);
        let err = register_root(&db, missing.to_string_lossy().as_ref())
            .expect_err("missing path must be rejected");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path does not exist"
        );
        let err = unregister_root(&db, missing.to_string_lossy().as_ref(), &fallback)
            .expect_err("unknown removal must fail");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path is not a registered root"
        );
        let err = unregister_root(&db, "   ", &fallback).expect_err("empty removal must fail");
        assert_eq!(
            format!("{err}"),
            "invalid workspace root: path must not be empty"
        );
        let _ = std::fs::remove_dir_all(&fallback);
    }

    #[test]
    fn roots_remove_active_falls_back_to_default() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        let root_a = temp_dir();
        let root_b = temp_dir();
        register_root(&db, root_a.to_string_lossy().as_ref()).expect("register A");
        register_root(&db, root_b.to_string_lossy().as_ref()).expect("register B");
        let canon_b = strip_verbatim(std::fs::canonicalize(&root_b).expect("root B resolves"))
            .to_string_lossy()
            .to_string();
        // Remove the active root (B): the registry keeps A, active falls back
        // to the default until the user switches again.
        let after = unregister_root(&db, root_b.to_string_lossy().as_ref(), &fallback)
            .expect("remove active");
        assert!(!after.roots.contains(&canon_b));
        assert_eq!(after.active, fallback.to_string_lossy().to_string());
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        // Stale entries (deleted dirs) remain removable by normalized text.
        std::fs::remove_dir_all(&root_a).expect("delete root A");
        unregister_root(&db, root_a.to_string_lossy().as_ref(), &fallback)
            .expect("stale entry removable");
        assert_eq!(list_roots(&db, &fallback).roots.len(), 1);
        let _ = std::fs::remove_dir_all(&root_b);
        let _ = std::fs::remove_dir_all(&fallback);
    }

    #[test]
    fn roots_errors_are_secret_free() {
        let db = crate::infrastructure::database::in_memory_database();
        let fallback = temp_dir();
        for err in [
            register_root(&db, "").expect_err("empty rejected"),
            unregister_root(&db, "C:\\nope-9f3c2a1e", &fallback).expect_err("unknown rejected"),
        ] {
            let text = format!("{err}").to_lowercase();
            for needle in ["sk-", "secret", "credential", "api_key", "bearer"] {
                assert!(
                    !text.contains(needle),
                    "error leaks a secret marker: {text:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&fallback);
    }

    /// Static wiring check: the git panel and the audit command stay on the
    /// shared resolver, so switching the active root moves both features with
    /// no per-feature changes. Needles use `concat!` so this test's own
    /// source never matches them verbatim.
    #[test]
    fn root_aware_features_share_the_active_root_resolver() {
        const VCS: &str = include_str!("../commands/version_control.rs");
        const AUDIT: &str = include_str!("../commands/repo_audit.rs");
        const RESOLVER: &str = concat!("resolve", "_workspace_root");
        assert!(
            VCS.contains(RESOLVER),
            "version_control commands must resolve the shared active root"
        );
        assert!(
            AUDIT.contains(RESOLVER),
            "repo_audit must resolve the shared active root"
        );
        for source in [VCS, AUDIT] {
            for needle in [
                concat!("C", ":\\"),
                concat!("/home", "/"),
                concat!("canonical", "ize"),
            ] {
                assert!(
                    !source.contains(needle),
                    "root-aware commands must not hardcode roots, found {needle:?}"
                );
            }
        }
    }
}
