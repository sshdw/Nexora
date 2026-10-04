//! Dependency-inventory and safe-refactor IPC commands: the Tauri side of the
//! read-only lockfile inventory plus the first workspace WRITE path.
//!
//! Thin translation only (ARCHITECTURE.md §5): each command resolves the
//! effective workspace root (the stored `agent.workspace_root` or the default
//! `agent_workspace` directory, exactly like the repo-audit and
//! version-control commands), delegates to its application-layer service
//! ([`crate::application::dep_inventory`],
//! [`crate::application::refactor_apply`]), and maps failures into
//! secret-free [`CommandError`] values. No business logic lives here beyond
//! that translation.
//!
//! Command-shape decision (one feature area, two commands — the slice maximum):
//! `dep_inventory` batches the cargo + npm tables in a single round trip
//! because the panel always renders them together (the same batching rationale
//! as `git_info`); `refactor_apply` stays separate because it carries
//! per-apply arguments (path, line range, finding kind, explicit
//! confirmation) and mutates exactly one file range per call. There is no
//! install/update/remove path anywhere in this slice — the inventory is
//! read-only, and the apply allowlist covers confirmed dead-code removals
//! only (see [`crate::application::refactor_apply`] for the never-applied
//! kinds).

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, State};

use crate::application::dep_inventory::DepInventory;
use crate::application::refactor_apply::RefactorApplyResult;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::CommandError;
use super::workspace::default_root;

/// Read-only dependency inventory over the effective workspace root: the
/// cargo table from `src-tauri/Cargo.lock` and the npm table from
/// `package-lock.json` (locked name/version/source rows, capped server-side).
/// Missing or unparsable lockfiles yield empty tables, never errors.
#[tauri::command]
pub(crate) fn dep_inventory(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<DepInventory, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::dep_inventory::inventory_workspace(&root).map_err(CommandError::from)
}

/// Apply one confirmed dead-code removal: delete workspace-relative `.rs`
/// lines `[start_line, end_line]` (1-based, inclusive) for a finding of an
/// allowlisted kind. The backend re-verifies every guard (confirmation,
/// kind, git-repo presence, tracked-clean file, range, `pub `-declaration
/// line) before writing; the file was clean beforehand, so `git checkout --
/// <path>` reverts. Unsafe kinds, unconfirmed calls, and non-git workspaces
/// refuse with classified errors.
#[tauri::command]
pub(crate) fn refactor_apply(
    path: String,
    start_line: usize,
    end_line: usize,
    kind: String,
    confirmed: bool,
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<RefactorApplyResult, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    crate::application::refactor_apply::refactor_apply(
        &root,
        path.as_str(),
        start_line,
        end_line,
        kind.as_str(),
        confirmed,
    )
    .map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::application::dep_inventory::DepInventoryError;
    use crate::application::refactor_apply::RefactorApplyError;
    use crate::commands::error::ErrorKind;

    const SOURCE: &str = include_str!("dep_refactor.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn inventory_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(DepInventoryError::InvalidRoot);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(DepInventoryError::Io);
        assert_eq!(mapped.kind, ErrorKind::Io);
        assert!(safe_message(&mapped));
    }

    #[test]
    fn apply_error_mapping_is_classified_and_secret_free() {
        for (err, kind) in [
            (RefactorApplyError::NotARepository, ErrorKind::InvalidInput),
            (
                RefactorApplyError::Unconfirmed,
                ErrorKind::ConfirmationRequired,
            ),
            (RefactorApplyError::UnsafeKind, ErrorKind::InvalidInput),
            (RefactorApplyError::InvalidPath, ErrorKind::InvalidInput),
            (RefactorApplyError::UncleanFile, ErrorKind::InvalidInput),
            (RefactorApplyError::InvalidRange, ErrorKind::InvalidInput),
            (RefactorApplyError::ContentMismatch, ErrorKind::InvalidInput),
            (RefactorApplyError::GitFailed, ErrorKind::Io),
            (RefactorApplyError::Io, ErrorKind::Io),
        ] {
            let mapped = CommandError::from(err);
            assert_eq!(mapped.kind, kind);
            assert!(safe_message(&mapped), "apply errors stay secret-free");
        }
    }

    /// Static wiring check: `dep_inventory` stays a pure workspace read and
    /// `refactor_apply` stays the only write — no process spawning or network
    /// on either path, and no install/update/remove surface. Needles are built
    /// with `concat!` so this test's own source never matches them verbatim.
    #[test]
    fn dep_refactor_commands_hold_their_read_write_contract() {
        assert!(
            SOURCE.contains("inventory_workspace("),
            "dep_inventory must delegate to the application-layer inventory"
        );
        assert!(
            SOURCE.contains("refactor_apply("),
            "refactor_apply must delegate to the application-layer applier"
        );
        for needle in [
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("install", "_dep"),
            concat!("update", "_dep"),
            concat!("auto", "_fix"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/dep_refactor.rs must hold its contract, found {needle:?}"
            );
        }
    }
}
