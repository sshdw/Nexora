//! Repo-audit IPC command: the Tauri side of the read-only repository audit.
//!
//! Thin translation only (ARCHITECTURE.md §5): the single `repo_audit`
//! command resolves the effective workspace root (the stored
//! `agent.workspace_root` or the default `agent_workspace` directory,
//! exactly like the version-control commands), delegates to the
//! application-layer audit engine ([`crate::application::repo_audit`]), and
//! maps failures into secret-free [`CommandError`] values. No business logic
//! lives here beyond that translation.
//!
//! Command-shape decision (one feature area, one command): the panel always
//! renders the whole report together (findings grouped by kind, scan
//! accounting, skip notices), so one batch round trip covers it — the same
//! batching rationale as `git_info`. There is no watching or live re-audit:
//! the panel runs the scan manually. Findings never modify code (no auto-fix
//! path exists anywhere in this slice).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, State};

use crate::application::repo_audit::RepoAuditReport;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::CommandError;
use super::workspace::default_root;

/// Run the read-only static analysis over the effective workspace root:
/// `.rs` / `.ts` / `.tsx` files, capped server-side with skip notices.
/// Returns the finding report (kinds, severities, `file:line` evidence, and
/// capped excerpts) plus the scan accounting. Read-only: no workspace write
/// of any kind happens on this path.
#[tauri::command]
pub(crate) fn repo_audit(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<RepoAuditReport, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::repo_audit::audit_workspace(&root).map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = include_str!("repo_audit.rs");
    const SERVICE: &str = include_str!("../application/repo_audit.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn audit_error_mapping_is_classified_and_secret_free() {
        use crate::application::repo_audit::RepoAuditError;
        use crate::commands::error::ErrorKind;
        let mapped = CommandError::from(RepoAuditError::InvalidRoot);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(RepoAuditError::Io);
        assert_eq!(mapped.kind, ErrorKind::Io);
        assert!(safe_message(&mapped));
    }

    /// Static wiring check: the audit command stays a pure workspace read —
    /// no writes, no process spawning, no network of its own, and no fix or
    /// refactor application path. Needles are built with `concat!` so this
    /// test's own source never matches them verbatim.
    #[test]
    fn repo_audit_stays_a_pure_read_without_fix_application() {
        assert!(
            SOURCE.contains("audit_workspace("),
            "repo_audit must delegate to the application-layer engine"
        );
        for needle in [
            concat!("fs", "::write"),
            concat!("create", "_dir"),
            concat!("File", "::create"),
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("apply", "_fix"),
            concat!("auto", "_fix"),
            concat!("refactor", "_apply"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/repo_audit.rs must stay read-only, found {needle:?}"
            );
        }
        // The engine's test scaffolding makes temp directories inside
        // `#[cfg(test)]`, so the directory-creation needle only applies to
        // the thin command layer above — the service is checked for file
        // writes and fix-application paths instead.
        for needle in [
            concat!("fs", "::write"),
            concat!("File", "::create"),
            concat!("apply", "_fix"),
        ] {
            assert!(
                !SERVICE.contains(needle),
                "application/repo_audit.rs must never write, found {needle:?}"
            );
        }
    }
}
