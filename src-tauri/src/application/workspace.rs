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
//! The guard ([`validate_workspace_root`]) rejects non-existent paths,
//! `C:\Windows` (and anything under it), and drive/filesystem roots before
//! anything is persisted. [`push_recent`] maintains the 5-entry ring buffer.
//! [`resolve_workspace_root`] is the single source the tool scope reads: the
//! stored setting when it names an existing directory, else the default.

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
/// Steps: trim, reject empty / overlong / null-byte paths, canonicalize via
/// the filesystem (non-existent paths fail here), require a directory, then
/// reject `C:\Windows` (and children) and drive/filesystem roots. The returned
/// path is the canonicalized absolute path with any Windows verbatim prefix
/// stripped, ready to store.
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
    let canonical = std::fs::canonicalize(trimmed)
        .map_err(|_| WorkspaceError::Invalid("path does not exist".to_string()))?;
    let canonical = strip_verbatim(canonical);
    if !canonical.is_dir() {
        return Err(WorkspaceError::Invalid(
            "path is not a directory".to_string(),
        ));
    }
    if is_system_path(&canonical) {
        return Err(WorkspaceError::Invalid(
            "the Windows system directory is not allowed".to_string(),
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

/// Whether `path` is `C:\Windows` or lives under it (case-insensitive, either
/// separator). Only the `C:` system directory is guarded, per spec.
pub(crate) fn is_system_path(path: &Path) -> bool {
    let mut text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        text = format!(r"\\{rest}");
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    let lowered = text.to_lowercase().replace('/', "\\");
    let trimmed = lowered.trim_end_matches('\\');
    trimmed == r"c:\windows" || trimmed.starts_with(r"c:\windows\")
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
fn strip_verbatim(path: PathBuf) -> PathBuf {
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
/// [`WORKSPACE_ROOT_KEY`] when it names an existing directory, else
/// `default_root` (the pre-picker `agent_workspace` behavior).
#[must_use]
pub(crate) fn resolve_workspace_root(db: &Database, default_root: &Path) -> PathBuf {
    let stored = SettingsService::new(db).read(WORKSPACE_ROOT_KEY);
    if let Ok(Some(value)) = stored {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            let candidate = PathBuf::from(&trimmed);
            if candidate.is_dir() {
                return candidate;
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

    fn temp_dir() -> PathBuf {
        let base = std::env::temp_dir();
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

    #[test]
    fn guard_rejects_windows_system_directory() {
        assert!(is_system_path(Path::new(r"C:\Windows")));
        assert!(is_system_path(Path::new(r"c:\windows\system32")));
        assert!(is_system_path(Path::new("C:/Windows")));
        assert!(!is_system_path(Path::new(r"C:\Users\alice")));
        // End-to-end through the guard on Windows, where C:\Windows exists.
        if Path::new(r"C:\Windows").is_dir() {
            let err =
                validate_workspace_root(r"C:\Windows").expect_err("system dir must be rejected");
            assert_eq!(
                format!("{err}"),
                "invalid workspace root: the Windows system directory is not allowed"
            );
        }
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
        // A stored directory wins over the default.
        let chosen = temp_dir();
        crate::application::settings::SettingsService::new(&db)
            .write(WORKSPACE_ROOT_KEY, Some(chosen.to_string_lossy().as_ref()))
            .expect("write setting");
        assert_eq!(
            resolve_workspace_root(&db, &fallback),
            PathBuf::from(chosen.to_string_lossy().to_string())
        );
        // A stored path that no longer exists falls back to the default.
        crate::application::settings::SettingsService::new(&db)
            .write(
                WORKSPACE_ROOT_KEY,
                Some(fallback.join("gone-4d2a").to_string_lossy().as_ref()),
            )
            .expect("write missing");
        assert_eq!(resolve_workspace_root(&db, &fallback), fallback);
        let _ = std::fs::remove_dir_all(&fallback);
        let _ = std::fs::remove_dir_all(temp_dir());
    }
}
