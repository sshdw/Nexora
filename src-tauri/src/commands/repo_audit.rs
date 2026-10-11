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

use tauri::{AppHandle, Manager};

use crate::application::repo_audit::RepoAuditReport;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Run the read-only static analysis over the effective workspace root:
/// `.rs` / `.ts` / `.tsx` files, capped server-side with skip notices.
/// Returns the finding report (kinds, severities, `file:line` evidence, and
/// capped excerpts) plus the scan accounting. Read-only: no workspace write
/// of any kind happens on this path.
///
/// # Threading
///
/// Like `send_message`, the workspace walk is blocking end to end (up to
/// `MAX_AUDIT_FILES` files of `MAX_FILE_BYTES` bytes each), so the body runs
/// on the runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context. A synchronous command would run on the main thread and
/// freeze the whole window for the scan (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn repo_audit(app: AppHandle) -> Result<RepoAuditReport, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::repo_audit::audit_workspace(&root).map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("repo_audit blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the repository audit could not read the workspace",
            ))
        }
    }
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

    /// Static wiring check: the audit command runs off the main thread — it
    /// is declared `async` and moves the workspace walk onto the runtime
    /// blocking pool, matching the `send_message` precedent (NEX-IPC-001).
    #[test]
    fn repo_audit_runs_on_the_blocking_pool() {
        assert!(
            SOURCE.contains("async fn repo_audit("),
            "repo_audit must be an async command so it never runs on the main thread"
        );
        assert!(
            SOURCE.contains("spawn_blocking"),
            "repo_audit must move the workspace walk onto the blocking pool"
        );
    }

    /// Boundary smoke: the exact body the command moves onto the blocking
    /// pool completes there and returns a real report. The fixture is the
    /// command module's own directory (small, always present, read-only):
    /// no fixture setup of its own, so the read-only static check above
    /// keeps holding for this file.
    #[test]
    fn repo_audit_body_completes_through_the_async_boundary() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let report = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(move || {
                crate::application::repo_audit::audit_workspace(&root)
            })
            .await
            .expect("blocking task joins without panicking")
        })
        .expect("audit of the command directory succeeds");
        assert!(
            report.files_scanned > 0,
            "the command directory holds scanned sources"
        );
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
