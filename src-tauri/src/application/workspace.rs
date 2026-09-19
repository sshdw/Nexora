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

use std::path::{Path, PathBuf};

use crate::application::settings::SettingsService;
use crate::infrastructure::database::Database;

/// Setting key for the chosen agent workspace root.
pub(crate) const WORKSPACE_ROOT_KEY: &str = "agent.workspace_root";

/// Setting key for the recent workspace roots (JSON array, most-recent first).
pub(crate) const WORKSPACE_RECENT_KEY: &str = "agent.workspace_recent";

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
}
