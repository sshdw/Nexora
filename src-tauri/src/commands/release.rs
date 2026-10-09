//! Release-readiness + issue-automation IPC commands: the Tauri side of the
//! Release panel.
//!
//! Thin translation only (ARCHITECTURE.md §5): the two commands resolve
//! paths from the [`AppHandle`] (the workspace root like the version-control
//! and GitHub commands, the app-data `backups` directory like
//! `snapshot_database`), delegate to the application-layer release service
//! ([`crate::application::release`]), and map failures into secret-free
//! [`CommandError`] values. No business logic lives here beyond that
//! translation.
//!
//! Command-shape decision (one feature area, TWO commands): `release_status`
//! is the cheap local readiness aggregation (manifest parity, migration
//! facts, snapshot presence, `gh` probe — read-only, network-free, no new
//! tables); `create_issue_for_finding` files one caller-attested issue
//! through the user's own `gh` CLI (the CLI owns auth — no credential
//! material ever crosses IPC; only the confirmed title/location/detail/source
//! strings travel, and only title/body strings reach the subprocess as argv).
//! Its `confirmed: bool` is **renderer-attested, not Rust-verified** — unlike
//! the terminal and data-management paths, which mint a single-use id only
//! after the user accepts a blocking native OS dialog. Do not treat this
//! command as covered by the server-side confirmation gate; see
//! `application/confirmations.rs` for the coverage split and the open audit
//! items (#137 / SEC-005 and the remaining boolean gates).
//! The update signal stays on the existing `update_check` command (it needs
//! the network); the panel calls both.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::release::{CreatedIssue, ReleaseError, ReleaseStatus};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Snapshot backups directory name inside the app-data directory (mirrors
/// `commands::system`).
const BACKUP_DIR_NAME: &str = "backups";

/// Collect the release readiness facts (manifest parity, migration facts,
/// snapshot presence, `gh` probe). Read-only and network-free: missing
/// manifests or snapshots surface as unknowns, never errors.
#[tauri::command]
pub(crate) fn release_status(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<ReleaseStatus, CommandError> {
    let fallback = default_root(&app)?;
    let root = resolve_workspace_root(db.inner(), &fallback);
    let backup_dir = app
        .path()
        .app_data_dir()
        .map(|dir| dir.join(BACKUP_DIR_NAME))
        .map_err(|err| {
            log::error!("release_status app-data dir failed: {err}");
            CommandError::new(ErrorKind::Io, "the release status could not be collected")
        })?;
    crate::application::release::collect_release_status(db.inner(), &root, &backup_dir)
        .map_err(CommandError::from)
}

/// File one caller-attested issue through the user's `gh` CLI (`gh issue
/// create` with `cwd` = workspace root). The in-app confirm dialog owns the
/// confirmation UX and the backend requires the per-call `confirmed` flag, but
/// that flag is **renderer-attested**: a bare IPC caller passes
/// `confirmed: true` in one call, so — unlike the terminal and data-management
/// paths, which mint a single-use id only after the user accepts a blocking
/// native OS dialog — there is no Rust-side user-presence proof here. Tracked
/// as an open audit item; see `application/confirmations.rs` for the coverage
/// split. Only the confirmed strings travel; auth stays in the CLI.
///
/// Like the GitHub list commands, the blocking subprocess round trip runs on
/// the runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn create_issue_for_finding(
    title: String,
    location: Option<String>,
    detail: Option<String>,
    source: String,
    confirmed: bool,
    app: AppHandle,
) -> Result<CreatedIssue, CommandError> {
    // Owned values so the input, workspace root, and managed state can be
    // reached from the blocking thread (borrowed `State<'_, _>` cannot cross
    // into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let input = crate::application::release::validate_issue_input(
            &title,
            location.as_deref(),
            detail.as_deref(),
            &source,
            confirmed,
        )
        .map_err(CommandError::from)?;
        crate::application::release::file_issue_with(&root, "gh", &input, env!("CARGO_PKG_VERSION"))
            .map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("create_issue_for_finding blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the GitHub issue could not be created",
            ))
        }
    }
}

impl From<ReleaseError> for CommandError {
    fn from(err: ReleaseError) -> Self {
        match err {
            // The workspace path may contain a user name, so even the
            // fixed text names no path (same mapping as `repo_audit`).
            ReleaseError::InvalidRoot => Self::new(
                ErrorKind::InvalidInput,
                "the workspace folder is not available for release",
            ),
            ReleaseError::Io => Self::new(
                ErrorKind::Io,
                "the release status could not read the workspace",
            ),
            // The curated database text names no SQL and no stored value;
            // the raw detail stays in the server log only.
            ReleaseError::Database(inner) => Self::from(inner),
            // Fixed validation vocabulary that never echoes the rejected
            // value; the destructive-action confirmation pattern for the
            // unconfirmed call (like the terminal and the refactor apply).
            ReleaseError::InvalidInput { message } => Self::new(ErrorKind::InvalidInput, message),
            ReleaseError::Unconfirmed => Self::new(
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before filing a GitHub issue",
            ),
            // The `gh` CLI surface: honest, actionable, fixed vocabulary —
            // no credential material, no path, no CLI stderr text.
            ReleaseError::GhMissing => Self::new(
                ErrorKind::Io,
                "the GitHub CLI (gh) is not installed or not on PATH",
            ),
            ReleaseError::GhNotSignedIn => Self::new(
                ErrorKind::Request,
                "the GitHub CLI is not signed in — run `gh auth login` in a terminal",
            ),
            ReleaseError::NoGitHubRepo => Self::new(
                ErrorKind::InvalidInput,
                "the workspace is not filed under a github.com repository",
            ),
            ReleaseError::GhFailed => {
                Self::new(ErrorKind::Request, "the GitHub issue could not be created")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn release_error_mapping_is_classified_and_secret_free() {
        let cases = [
            (
                ReleaseError::InvalidRoot,
                ErrorKind::InvalidInput,
                "the workspace folder is not available for release",
            ),
            (
                ReleaseError::Io,
                ErrorKind::Io,
                "the release status could not read the workspace",
            ),
            (
                ReleaseError::InvalidInput {
                    message: "the issue title must be 1..200 characters".to_string(),
                },
                ErrorKind::InvalidInput,
                "the issue title must be 1..200 characters",
            ),
            (
                ReleaseError::Unconfirmed,
                ErrorKind::ConfirmationRequired,
                "explicit confirmation is required before filing a GitHub issue",
            ),
            (
                ReleaseError::GhMissing,
                ErrorKind::Io,
                "the GitHub CLI (gh) is not installed or not on PATH",
            ),
            (
                ReleaseError::GhNotSignedIn,
                ErrorKind::Request,
                "the GitHub CLI is not signed in — run `gh auth login` in a terminal",
            ),
            (
                ReleaseError::NoGitHubRepo,
                ErrorKind::InvalidInput,
                "the workspace is not filed under a github.com repository",
            ),
            (
                ReleaseError::GhFailed,
                ErrorKind::Request,
                "the GitHub issue could not be created",
            ),
            (
                ReleaseError::Database(crate::infrastructure::database::DatabaseError::Lock(
                    "sk-".into(),
                )),
                ErrorKind::Database,
                "a database operation failed; no data was changed",
            ),
        ];
        for (case, kind, message) in cases {
            let mapped: CommandError = case.into();
            assert_eq!(mapped.kind, kind);
            assert_eq!(mapped.message, message);
            assert!(safe_message(&mapped), "secret leaked into: {mapped:?}");
        }
    }

    /// Static wiring check: the release commands stay a thin translation
    /// over the application-layer service — workspace-root resolution plus
    /// service delegation, with the `gh` spawn living service-side. No
    /// process spawning and no credential handling of their own: only the
    /// confirmed title/location/detail/source strings travel, and only
    /// title/body strings reach the subprocess. This module creates no
    /// tasks, writes no files, and touches no SQL (no new tables on this
    /// path). Needles are built with `concat!` so this test's own source
    /// never matches them verbatim.
    #[test]
    fn release_commands_stay_thin_over_the_service() {
        const SOURCE: &str = include_str!("release.rs");
        for needle in [
            "release_status",
            "create_issue_for_finding",
            "collect_release_status(",
            "validate_issue_input(",
            "file_issue_with(",
        ] {
            assert!(
                SOURCE.contains(needle),
                "commands/release.rs must delegate to the release service, missing {needle:?}"
            );
        }
        for needle in [
            concat!("std::process", "::"),
            concat!("Command", "::new"),
            concat!("key", "ring"),
            concat!("tok", "en"),
            concat!("GITHUB", "_TOKEN"),
            concat!("SELECT", " "),
            concat!("INSERT", " "),
            concat!("fs", "::write"),
            concat!("create", "_task"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/release.rs must hold no spawn/credential/SQL/task logic, found {needle:?}"
            );
        }
    }
}
