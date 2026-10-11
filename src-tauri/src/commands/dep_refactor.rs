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

use tauri::{AppHandle, Manager};

use crate::application::dep_inventory::DepInventory;
use crate::application::refactor_apply::RefactorApplyResult;
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Read-only dependency inventory over the effective workspace root: the
/// cargo table from `src-tauri/Cargo.lock` and the npm table from
/// `package-lock.json` (locked name/version/source rows, capped server-side).
/// Missing or unparsable lockfiles yield empty tables, never errors.
///
/// Runs on the runtime blocking pool like `repo_audit`: the inventory reads
/// workspace lockfiles, so a synchronous command would run on the main
/// thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn dep_inventory(app: AppHandle) -> Result<DepInventory, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        crate::application::dep_inventory::inventory_workspace(&root).map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("dep_inventory blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the dependency inventory could not read the workspace",
            ))
        }
    }
}

/// Apply one confirmed dead-code removal: delete workspace-relative `.rs`
/// lines `[start_line, end_line]` (1-based, inclusive) for a finding of an
/// allowlisted kind. The backend re-verifies every guard (confirmation,
/// kind, git-repo presence, tracked-clean file, range, `pub `-declaration
/// line) before writing; the file was clean beforehand, so `git checkout --
/// <path>` reverts. Unsafe kinds, unconfirmed calls, and non-git workspaces
/// refuse with classified errors.
///
/// Runs on the blocking pool like [`dep_inventory`]: the backend re-reads
/// the file plus git state, so a synchronous command would run on the main
/// thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn refactor_apply(
    path: String,
    start_line: usize,
    end_line: usize,
    kind: String,
    confirmed: bool,
    app: AppHandle,
) -> Result<RefactorApplyResult, CommandError> {
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
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
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            log::error!("refactor_apply blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the file could not be read or written",
            ))
        }
    }
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

    /// Static wiring check (NEX-IPC-001): both workspace-touching commands
    /// are declared `async` and move their file/git work onto the runtime
    /// blocking pool, matching the `send_message` precedent.
    #[test]
    fn dep_refactor_commands_run_on_the_blocking_pool() {
        for name in ["dep_inventory", "refactor_apply"] {
            assert!(
                SOURCE.contains(&format!("async fn {name}(")),
                "{name} must be an async command so it never runs on the main thread"
            );
        }
        assert!(
            SOURCE.contains("spawn_blocking"),
            "the dep/refactor commands must move blocking work onto the blocking pool"
        );
    }

    /// Scratch workspace scaffolding for the boundary smokes below.
    #[cfg(test)]
    mod boundary {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        pub(super) fn temp_root() -> PathBuf {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock reads")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("nexora-dep-cmd-test-{nanos}-{id}"));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            dir
        }

        /// Drive one `'static` closure through the production scheduling
        /// shape (`spawn_blocking` awaited from the async runtime) and join
        /// it, proving the moved command body completes off-thread.
        pub(super) fn through_boundary<T, F>(work: F) -> T
        where
            T: Send + 'static,
            F: FnOnce() -> T + Send + 'static,
        {
            tauri::async_runtime::block_on(async {
                tauri::async_runtime::spawn_blocking(work)
                    .await
                    .expect("blocking task joins without panicking")
            })
        }
    }

    /// Boundary smoke: `dep_inventory` reads both lockfiles through the
    /// async boundary and reports one cargo plus one npm row.
    #[test]
    fn dep_inventory_body_completes_through_the_async_boundary() {
        let root = boundary::temp_root();
        std::fs::create_dir_all(root.join("src-tauri")).expect("create lockfile parent");
        std::fs::write(
            root.join("src-tauri").join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n",
        )
        .expect("seed cargo lockfile");
        std::fs::write(
            root.join("package-lock.json"),
            r#"{"lockfileVersion": 1, "dependencies": {"react": {"version": "16.0.0"}}}"#,
        )
        .expect("seed npm lockfile");

        let inventory = boundary::through_boundary({
            let root = root.clone();
            move || crate::application::dep_inventory::inventory_workspace(&root)
        })
        .expect("inventory succeeds through the boundary");
        assert_eq!(inventory.cargo_total, 1);
        assert_eq!(inventory.npm_total, 1);
        assert_eq!(inventory.cargo[0].name, "serde");
        assert_eq!(inventory.npm[0].name, "react");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Boundary smoke: `refactor_apply` removes one confirmed dead-code line
    /// through the async boundary on a scratch git repository.
    #[test]
    fn refactor_apply_body_completes_through_the_async_boundary() {
        use std::path::Path;

        let root = boundary::temp_root();
        {
            let repo = git2::Repository::init(&root).expect("init scratch repo");
            std::fs::create_dir_all(root.join("src")).expect("create source parent");
            std::fs::write(
                root.join("src").join("lib.rs"),
                "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\npub fn orphaned() {}\n",
            )
            .expect("seed source file");
            let mut index = repo.index().expect("test index");
            index
                .add_path(Path::new("src/lib.rs"))
                .expect("stage test file");
            index.write().expect("write test index");
            let tree_id = index.write_tree().expect("write test tree");
            let tree = repo.find_tree(tree_id).expect("find test tree");
            let signature =
                git2::Signature::now("nexora-test", "nexora-test@example.com").expect("signs");
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "test: seed commit",
                &tree,
                &[],
            )
            .expect("seed commit");
        }

        let result = boundary::through_boundary({
            let root = root.clone();
            move || {
                crate::application::refactor_apply::refactor_apply(
                    &root,
                    "src/lib.rs",
                    4,
                    4,
                    "dead-code-candidate",
                    true,
                )
            }
        })
        .expect("apply succeeds through the boundary");
        assert_eq!(result.removed_lines, 1);
        assert!(
            result.verified,
            "the post-apply re-read verifies the removal"
        );
        let content =
            std::fs::read_to_string(root.join("src").join("lib.rs")).expect("reads result");
        assert_eq!(
            content,
            "/// Kept entry.\npub fn kept() {}\n/// Orphaned entry.\n"
        );
        let _ = std::fs::remove_dir_all(root);
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
