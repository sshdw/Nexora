//! Test-generation IPC command: the Tauri side of the read-only test-draft
//! generation.
//!
//! Thin translation only (ARCHITECTURE.md §5): the single `testgen_drafts`
//! command resolves the effective workspace root (the stored
//! `agent.workspace_root` or the default `agent_workspace` directory,
//! exactly like the version-control and repo-audit commands), delegates to
//! the application-layer generator ([`crate::application::testgen`]), and
//! maps failures into secret-free [`CommandError`] values. No business logic
//! lives here beyond that translation.
//!
//! Command-shape decision (one feature area, one command): the panel renders
//! the whole draft set together (drafts, target counts, overflow notice), so
//! one batch round trip covers it — the same batching rationale as
//! `repo_audit` (see `commands/repo_audit.rs`). Drafts are response data
//! only: nothing on this path writes to the workspace (the user copies
//! drafts manually — applying is a separate slice), and generation is
//! template-based locally, never an agent run, so no approval/budget gate
//! is entered or bypassed.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};

use crate::application::testgen::TestgenReport;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Generate draft Rust `#[test]` scaffolds for untested public functions
/// over the effective workspace root: runs the read-only repo audit, then
/// derives template-based drafts (capped server-side with an overflow
/// count). Read-only: no workspace write of any kind happens on this path.
///
/// Runs on the runtime blocking pool like `repo_audit`: the underlying scan
/// walks the same workspace sources, so a synchronous command would run on
/// the main thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn testgen_drafts(app: AppHandle) -> Result<TestgenReport, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::testgen::generate_testgen_report(&root).map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("testgen_drafts blocking task failed: {err}");
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

    use crate::application::repo_audit::RepoAuditError;
    use crate::commands::error::ErrorKind;

    const SOURCE: &str = include_str!("testgen.rs");

    /// The testgen command reuses the audit error taxonomy, so its failures
    /// stay classified and secret-free through the existing mapping (no new
    /// error variant, no new mapping arm).
    #[test]
    fn testgen_error_mapping_reuses_audit_taxonomy() {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        for (err, kind) in [
            (RepoAuditError::InvalidRoot, ErrorKind::InvalidInput),
            (RepoAuditError::Io, ErrorKind::Io),
        ] {
            let mapped = CommandError::from(err);
            assert_eq!(mapped.kind, kind);
            assert!(
                !SECRET_SENTINELS
                    .iter()
                    .any(|needle| mapped.message.to_lowercase().contains(needle)),
                "testgen errors stay secret-free, got {:?}",
                mapped.message
            );
        }
    }

    /// Static wiring check: the testgen command is declared `async` and moves
    /// the workspace scan onto the runtime blocking pool (NEX-IPC-001), like
    /// `repo_audit` whose engine it reuses.
    #[test]
    fn testgen_drafts_runs_on_the_blocking_pool() {
        assert!(
            SOURCE.contains("async fn testgen_drafts("),
            "testgen_drafts must be an async command so it never runs on the main thread"
        );
        assert!(
            SOURCE.contains("spawn_blocking"),
            "testgen_drafts must move the workspace scan onto the blocking pool"
        );
    }

    /// Boundary smoke: the exact body the command moves onto the blocking
    /// pool completes there and returns a draft report. The fixture is the
    /// command module's own directory (small, always present, read-only):
    /// no fixture setup of its own, so the read-only static check above
    /// keeps holding for this file.
    #[test]
    fn testgen_body_completes_through_the_async_boundary() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let report = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(move || {
                crate::application::testgen::generate_testgen_report(&root)
            })
            .await
            .expect("blocking task joins without panicking")
        })
        .expect("draft generation over the command directory succeeds");
        assert!(
            report.files_scanned > 0,
            "the command directory holds scanned sources"
        );
    }

    /// Static wiring check: the testgen command stays a pure workspace
    /// read returning response data — no writes, no process spawning, no
    /// network, and no draft/test application path. Needles are built with
    /// `concat!` so this test's own source never matches them verbatim.
    #[test]
    fn testgen_drafts_stays_a_pure_read_without_write_application() {
        assert!(
            SOURCE.contains("generate_testgen_report("),
            "testgen_drafts must delegate to the application-layer generator"
        );
        for needle in [
            concat!("fs", "::write"),
            concat!("create", "_dir"),
            concat!("File", "::create"),
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("apply", "_draft"),
            concat!("write", "_test"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/testgen.rs must stay read-only, found {needle:?}"
            );
        }
    }
}
