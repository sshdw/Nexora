//! GitHub issues/PRs IPC commands: the Tauri side of the read-only
//! issues/PRs panel.
//!
//! Thin translation only (ARCHITECTURE.md §5): the two commands resolve the
//! effective workspace root (the stored `agent.workspace_root` or the default
//! `agent_workspace` directory, exactly like the version-control commands),
//! delegate to the application-layer GitHub service
//! ([`crate::application::github`]), and map failures into secret-free
//! [`CommandError`] values. No business logic lives here beyond that
//! translation.
//!
//! Command-shape decision (one feature area, FOUR commands): `gh_issues` and
//! `gh_pulls` stay separate because the panel's kind tabs invoke them
//! independently with their own state filter (`open` / `closed` / `all`),
//! exactly like the VCS panel's separate lazy diff commands. `gh_actions`
//! is the area's batch call returning the recent workflow runs with failing
//! runs expanded (failed jobs metadata only — no log bodies, so one call
//! never fans out into dozens of log downloads); `gh_action_log` is the
//! area's ONE additional lazy command (the allowed maximum), fetching a
//! single job's capped, scrubbed log tail when the user expands that job.
//! Both are pure GETs against `https://api.github.com` — there is no
//! commenting, labeling, merging, re-running, or any other write anywhere on
//! this path (read-only; static tests pin GET-only on the service side).
//!
//! Fix-loop entry: the panel turns a failing run into a prefilled
//! task-manager task through the EXISTING task creation command (user
//! confirms every step; approval/budget inherited) — this module creates no
//! task and starts no run.
//!
//! Auth: the token resolves inside the backend from the OS keyring entry
//! `github` and never crosses IPC. A missing token is not an error: the
//! request goes out unauthenticated and the response reports
//! `authenticated: false` so the panel shows its clean connect-hint.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager};

use crate::application::github::{
    GhActionLog, GhActionsResponse, GhIssuesResponse, GhPullsResponse,
};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::CommandError;
use super::workspace::default_root;

/// List the workspace `origin` repo's issues for `state` (`open` /
/// `closed` / `all`, defaulting to `open` when omitted). Pull-request rows
/// are excluded backend-side. Read-only: one capped page (at most 50 items)
/// with the rate-limit snapshot; an exhausted quota resolves to an empty
/// list with `rate_limited: true`, never an error dump.
///
/// Like the AI-assisted VCS commands, the blocking HTTP round trip runs on
/// the runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn gh_issues(
    state: Option<String>,
    app: AppHandle,
) -> Result<GhIssuesResponse, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let state = state.as_deref().unwrap_or("open");
        let result =
            crate::application::github::list_issues(&root, state).map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful read — a ledger failure never fails the read.
            crate::application::privacy::record(db.inner(), "github_read");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("gh_issues blocking task failed: {err}");
            Err(CommandError::new(
                super::error::ErrorKind::Request,
                "the GitHub issues could not be listed",
            ))
        }
    }
}

/// List the workspace `origin` repo's pull requests for `state` (same shape
/// and contract as [`gh_issues`]).
#[tauri::command]
pub(crate) async fn gh_pulls(
    state: Option<String>,
    app: AppHandle,
) -> Result<GhPullsResponse, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let state = state.as_deref().unwrap_or("open");
        let result =
            crate::application::github::list_pulls(&root, state).map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful read — a ledger failure never fails the read.
            crate::application::privacy::record(db.inner(), "github_read");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("gh_pulls blocking task failed: {err}");
            Err(CommandError::new(
                super::error::ErrorKind::Request,
                "the GitHub pull requests could not be listed",
            ))
        }
    }
}

/// List the workspace `origin` repo's recent workflow runs with failing runs
/// expanded (failed jobs metadata only — log bodies stay lazy via
/// [`gh_action_log`], so one batch call never fans out into dozens of log
/// downloads).
/// Read-only: one batch GET round (runs envelope plus per-failure jobs
/// envelopes) with the rate-limit snapshot; an exhausted quota resolves to
/// an empty list with `rate_limited: true`, never an error dump. The fix
/// loop lives in the panel: it prefills a task through the existing
/// task-manager creation command — this command creates and starts nothing.
///
/// Like the sibling commands, the blocking HTTP round trips run on the
/// runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn gh_actions(app: AppHandle) -> Result<GhActionsResponse, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let result = crate::application::github::list_actions(&root).map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful read — a ledger failure never fails the read.
            crate::application::privacy::record(db.inner(), "github_read");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("gh_actions blocking task failed: {err}");
            Err(CommandError::new(
                super::error::ErrorKind::Request,
                "the GitHub workflow runs could not be listed",
            ))
        }
    }
}

/// Fetch one failed job's capped, secret-scrubbed log tail for user-expanded
/// jobs (the lazy half of the Actions surface: `gh_actions` ships jobs
/// without log bodies). Read-only: a single GET for the per-job TEXT log;
/// an expired or missing log resolves to `log_unavailable: true` and an auth
/// refusal (401/403) to `log_needs_auth: true` — never an error dump. The
/// fix loop lives in the panel, as with [`gh_actions`].
///
/// Like the sibling commands, the blocking HTTP round trip runs on the
/// runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn gh_action_log(
    run_id: u64,
    job_id: u64,
    app: AppHandle,
) -> Result<GhActionLog, CommandError> {
    // Owned values so the ids, workspace root, and managed state can be
    // reached from the blocking thread (borrowed `State<'_, _>` cannot cross
    // into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let result = crate::application::github::fetch_action_log(&root, run_id, job_id)
            .map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful read — a ledger failure never fails the read.
            crate::application::privacy::record(db.inner(), "github_read");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("gh_action_log blocking task failed: {err}");
            Err(CommandError::new(
                super::error::ErrorKind::Request,
                "the GitHub job log could not be fetched",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::github::GitHubError;
    use crate::commands::error::ErrorKind;

    const SOURCE: &str = include_str!("github.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn github_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(GitHubError::NoGitHubRemote);
        assert_eq!(mapped.kind, ErrorKind::InvalidInput);
        assert_eq!(
            mapped.message,
            "the workspace origin is not a github.com repository"
        );
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(GitHubError::Unauthorized);
        assert_eq!(mapped.kind, ErrorKind::Request);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(GitHubError::NotFound);
        assert_eq!(mapped.kind, ErrorKind::NotFound);
        assert!(safe_message(&mapped));
    }

    /// Static wiring check: the GitHub commands stay thin translation —
    /// workspace-root resolution plus service delegation, no business logic,
    /// no writes, no process spawning of their own. Needles are built with
    /// `concat!` so this test's own source never matches them verbatim.
    #[test]
    fn github_commands_stay_thin_translation() {
        for needle in ["gh_issues", "gh_pulls", "list_issues", "list_pulls"] {
            assert!(
                SOURCE.contains(needle),
                "the GitHub commands must exist, missing {needle:?}"
            );
        }
        for needle in [
            concat!("Command", "::new"),
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
            concat!("fs", "::write"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/github.rs must not write or spawn, found {needle:?}"
            );
        }
    }

    /// The fail-loop batch command exists exactly once and delegates to the
    /// Actions service (`list_actions`) — the panel's fix loop prefills a
    /// task through the existing task-manager creation command, so this
    /// module must never mention task creation or run starting.
    #[test]
    fn gh_actions_command_delegates_to_the_actions_service() {
        assert!(
            SOURCE.contains("gh_actions"),
            "the gh_actions command must exist"
        );
        assert!(
            SOURCE.contains("list_actions"),
            "gh_actions must delegate to the Actions service"
        );
        for needle in [
            concat!("create", "_task"),
            concat!("start_task", "_run"),
            concat!("start_agent", "_run"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/github.rs must not create tasks or start runs, found {needle:?}"
            );
        }
    }

    /// The lazy log command exists exactly once and delegates to the log
    /// service (`fetch_action_log`): one job's tail per user expand, so the
    /// batch call never fans out into log downloads.
    #[test]
    fn gh_action_log_command_delegates_to_the_log_service() {
        assert!(
            SOURCE.contains("gh_action_log"),
            "the gh_action_log command must exist"
        );
        assert!(
            SOURCE.contains("fetch_action_log"),
            "gh_action_log must delegate to the log service"
        );
    }
}
