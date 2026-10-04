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

use tauri::{AppHandle, State};

use crate::application::testgen::TestgenReport;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::CommandError;
use super::workspace::default_root;

/// Generate draft Rust `#[test]` scaffolds for untested public functions
/// over the effective workspace root: runs the read-only repo audit, then
/// derives template-based drafts (capped server-side with an overflow
/// count). Read-only: no workspace write of any kind happens on this path.
#[tauri::command]
pub(crate) fn testgen_drafts(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<TestgenReport, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::testgen::generate_testgen_report(&root).map_err(CommandError::from)
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
