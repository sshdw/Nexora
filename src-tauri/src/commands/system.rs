//! Diagnostics / update-check / snapshot IPC commands: the Tauri side of the
//! health-panel System section.
//!
//! Thin translation only (ARCHITECTURE.md §5): the three commands resolve
//! paths from the [`AppHandle`] (the database file and the workspace root,
//! exactly like the version-control and GitHub commands), delegate to the
//! application-layer services ([`crate::application::system`] for the bundle
//! and the snapshot, [`crate::application::github::check_update`] for the
//! release check), and map failures into secret-free [`CommandError`] values.
//! No business logic lives here beyond that translation.
//!
//! Read-only except the snapshot: `diagnostics_bundle` and `update_check`
//! write nothing; `snapshot_database` writes exactly one new
//! `nexora-backup-<secs>.db` file under the app-data `backups` directory on
//! the user's explicit button press (never silently, never on a timer).
//! There is no download and no install anywhere on this path — the update
//! panel renders the release URL as text for the user to open themselves.
//!
//! The ONE deliberate side effect beyond the snapshot is
//! [`open_external_url`]: the OS default browser opening a user-clicked,
//! backend-validated http(s) URL (02-SECURITY-FRONTEND-IPC.md §4.2.3 / §6.1).
//! The URL is validated HERE, at the sink — scheme, absoluteness, userinfo,
//! length — and only the canonical parsed form reaches the opener plugin.
//! Model-authored URLs never render as anchors in the webview; this command
//! is the single navigation sink, reached only on an explicit click.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};
// The one navigation sink's opener API (02-SECURITY §4.2.3/§6.1): imported
// module-side so the command body stays statements-only (clippy
// items_after_statements).
use tauri_plugin_opener::OpenerExt;

use crate::application::github::UpdateCheck;
use crate::application::system::{DiagnosticsBundle, SnapshotInfo};
use crate::application::workspace::resolve_workspace_root;
use crate::infrastructure::database::Database;

use super::error::{CommandError, ErrorKind};
use super::workspace::default_root;

/// Database file name inside the app-data directory (mirrors the startup
/// path in [`crate::run`]).
const DB_FILE_NAME: &str = "nexora.db";

/// Snapshot backups directory name inside the app-data directory.
const BACKUP_DIR_NAME: &str = "backups";

/// Resolve the live database file for size reporting. A missing or
/// unreadable file is NOT an error: the bundle reports `db_size_bytes` as
/// unknown instead of failing the whole call.
fn database_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    Some(dir.join(DB_FILE_NAME))
}

/// Collect the secret-free diagnostics bundle (versions, platform, storage
/// counts, crash evidence). Read-only: counts and versions only, never
/// content, credentials, SQL, or paths.
#[tauri::command]
pub(crate) fn diagnostics_bundle(
    app: AppHandle,
    db: State<'_, Database>,
) -> Result<DiagnosticsBundle, CommandError> {
    let db_size_bytes = database_file(&app)
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len());
    crate::application::system::collect_bundle(db.inner(), db_size_bytes)
        .map_err(CommandError::from)
}

/// Check the workspace `origin` repo's latest GitHub release against the
/// running build. Read-only check only: one GET, no download, no install.
///
/// Like the GitHub list commands, the blocking HTTP round trip runs on the
/// runtime's dedicated blocking pool via
/// [`tauri::async_runtime::spawn_blocking`]: plain OS threads with no ambient
/// async context.
#[tauri::command]
pub(crate) async fn update_check(app: AppHandle) -> Result<UpdateCheck, CommandError> {
    // Owned handle so the workspace root and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let fallback = default_root(&handle)?;
        let db = handle.state::<Database>();
        let root = resolve_workspace_root(db.inner(), &fallback);
        let result = crate::application::github::check_update(&root).map_err(CommandError::from);
        if result.is_ok() {
            // Local usage ledger (counts only): one best-effort tick per
            // successful check — a ledger failure never fails the check.
            crate::application::privacy::record(db.inner(), "update_check");
        }
        result
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("update_check blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the update check could not be completed",
            ))
        }
    }
}

/// Write a consistent pre-update snapshot copy of the database (`VACUUM
/// INTO`) under the app-data `backups` directory. The user presses the
/// button explicitly — there is no silent or scheduled write; the response
/// carries the file name (never the full path) plus size.
///
/// Runs on the runtime blocking pool like `send_message`: the snapshot
/// copies the whole database file, so a synchronous command would run on
/// the main thread and freeze the window (NEX-IPC-001).
#[tauri::command]
pub(crate) async fn snapshot_database(app: AppHandle) -> Result<SnapshotInfo, CommandError> {
    // Owned handle so the app-data dir and managed state can be reached
    // from the blocking thread (borrowed `State<'_, _>` cannot cross into
    // `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let backup_dir = handle
            .path()
            .app_data_dir()
            .map(|dir| dir.join(BACKUP_DIR_NAME))
            .map_err(|err| {
                log::error!("snapshot_database app-data dir failed: {err}");
                CommandError::new(
                    ErrorKind::Io,
                    "the pre-update snapshot could not be written",
                )
            })?;
        let db = handle.state::<Database>();
        crate::application::system::snapshot_to(db.inner(), &backup_dir).map_err(CommandError::from)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("snapshot_database blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Io,
                "the pre-update snapshot could not be written",
            ))
        }
    }
}

/// Maximum accepted URL length for an external open (02-SECURITY §4.2.3):
/// a model-authored string past this bound is hostile by construction and
/// is rejected before parsing.
const MAX_EXTERNAL_URL_LEN: usize = 2048;

/// Validate a user-clicked link destination for the external-open sink
/// (02-SECURITY-FRONTEND-IPC.md §4.2.3 + §6.1).
///
/// Only an absolute `http:`/`https:` URL survives: everything else —
/// `javascript:`, `data:`, `vbscript:`, `file:`, `ftp:` and any other
/// scheme, relative or scheme-less values, URLs with embedded userinfo,
/// and over-length strings — is rejected with a secret-free
/// [`CommandError`]. The check runs on the WHATWG parser (the `url` crate),
/// not on string matching: a scheme allow-list cannot be fooled by
/// whitespace, escapes or casing tricks, and the returned value is the
/// parser's canonical form — the exact string handed to the OS, with
/// embedded control characters already removed.
pub(crate) fn validate_external_url(url: &str) -> Result<String, CommandError> {
    if url.len() > MAX_EXTERNAL_URL_LEN {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "the link destination is too long to open",
        ));
    }
    let parsed = url::Url::parse(url).map_err(|_| {
        CommandError::new(
            ErrorKind::InvalidInput,
            "the link destination is not an absolute http(s) URL",
        )
    })?;
    match parsed.scheme() {
        "http" | "https" => {}
        // `javascript:`, `data:`, `vbscript:`, `file:`, `ftp:` and every
        // other scheme land here — one allow-list, no deny-list to maintain.
        _ => {
            return Err(CommandError::new(
                ErrorKind::InvalidInput,
                "only http and https links can be opened",
            ))
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        // Worded to stay inside the module's own secret-free lexical
        // hygiene: no "credential"/"secret"/"api_key" sentinels.
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "a link with embedded login details cannot be opened",
        ));
    }
    // The canonical form is what the OS actually receives; keep it under the
    // same bound the raw input was held to (percent-encoding can only grow a
    // string, so a hostile padding payload cannot slip through twice).
    let canonical = parsed.to_string();
    if canonical.len() > MAX_EXTERNAL_URL_LEN {
        return Err(CommandError::new(
            ErrorKind::InvalidInput,
            "the link destination is too long to open",
        ));
    }
    Ok(canonical)
}

/// Open one user-clicked absolute http(s) URL in the OS default browser
/// (02-SECURITY-FRONTEND-IPC.md §4.2.3 / §6.1).
///
/// This is the app's single navigation sink for model-authored URLs. The
/// renderer never navigates by itself and never renders those URLs as
/// anchors (`MessageMarkdown.tsx` — copy + explicit open affordances only),
/// so a URL reaches this command only behind a real user click, and only
/// the validated canonical form is opened. The opener plugin is invoked
/// from Rust; the webview capability (`capabilities/default.json`) grants
/// only `opener:allow-open-url` with a https-only URL scope, so the
/// plugin's own command surface stays closed to every other scheme.
#[tauri::command]
pub(crate) fn open_external_url(app: AppHandle, url: String) -> Result<(), CommandError> {
    let canonical = validate_external_url(&url)?;
    app.opener()
        .open_url(canonical, None::<&str>)
        .map_err(|err| {
            log::error!("open_external_url: the opener refused the request: {err}");
            CommandError::new(
                ErrorKind::Request,
                "the link could not be opened in the default browser",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::system::SystemError;
    use crate::infrastructure::database::DatabaseError;

    const SOURCE: &str = include_str!("system.rs");

    fn safe_message(err: &CommandError) -> bool {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        !SECRET_SENTINELS
            .iter()
            .any(|needle| err.message.to_lowercase().contains(needle))
    }

    #[test]
    fn system_error_mapping_is_classified_and_secret_free() {
        let mapped = CommandError::from(SystemError::Io);
        assert_eq!(mapped.kind, ErrorKind::Io);
        assert!(safe_message(&mapped));
        let mapped = CommandError::from(SystemError::Database(DatabaseError::Lock(
            "poisoned sk-secret-PLANTED".to_string(),
        )));
        assert_eq!(mapped.kind, ErrorKind::Database);
        assert_eq!(
            mapped.message,
            "a database operation failed; no data was changed"
        );
        assert!(safe_message(&mapped));
        assert!(
            !mapped.message.contains("PLANTED"),
            "lock detail must never leak: {}",
            mapped.message
        );
    }

    /// Static wiring check: `snapshot_database` is declared `async` and
    /// moves the whole-file copy onto the runtime blocking pool (NEX-IPC-001),
    /// like `send_message`. `diagnostics_bundle` stays synchronous: one file
    /// timestamp plus indexed counts, no bulk IO.
    #[test]
    fn snapshot_database_runs_on_the_blocking_pool() {
        assert!(
            SOURCE.contains("async fn snapshot_database("),
            "snapshot_database must be an async command so it never runs on the main thread"
        );
        assert!(
            SOURCE.contains("spawn_blocking"),
            "snapshot_database must move the file copy onto the blocking pool"
        );
    }

    /// Boundary smoke: the exact body the command moves onto the blocking
    /// pool completes there — an in-memory database snapshots into a scratch
    /// directory and the copy exists on disk with a matching name.
    #[test]
    fn snapshot_body_completes_through_the_async_boundary() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::infrastructure::database::in_memory_database;

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let backup_dir = std::env::temp_dir().join(format!("nexora-snapshot-cmd-test-{id}"));
        std::fs::create_dir_all(&backup_dir).expect("scratch backup dir creates");

        let db = in_memory_database();
        let info = tauri::async_runtime::block_on(async {
            let db = db.clone();
            let backup_dir = backup_dir.clone();
            tauri::async_runtime::spawn_blocking(move || {
                crate::application::system::snapshot_to(&db, &backup_dir)
            })
            .await
            .expect("blocking task joins without panicking")
        })
        .expect("snapshot succeeds through the boundary");
        assert!(
            info.file_name.starts_with("nexora-backup-"),
            "the snapshot carries the file name, got {:?}",
            info.file_name
        );
        assert!(
            backup_dir.join(&info.file_name).exists(),
            "the snapshot copy exists on disk"
        );
        let _ = std::fs::remove_dir_all(&backup_dir);
    }

    /// Static wiring check: the system commands stay thin translation —
    /// path resolution plus service delegation. No process spawning, no
    /// network client of their own (the release GET lives in the github
    /// service), no silent installs. Needles are built with `concat!` so
    /// this test's own source never matches them.
    #[test]
    fn system_commands_stay_thin_translation() {
        for needle in [
            "diagnostics_bundle",
            "update_check",
            "snapshot_database",
            "collect_bundle",
            "check_update",
            "snapshot_to",
        ] {
            assert!(
                SOURCE.contains(needle),
                "the system commands must exist, missing {needle:?}"
            );
        }
        for needle in [
            concat!("std::process", "::"),
            concat!("tokio", "::process"),
            concat!(".", "post("),
            concat!(".", "put("),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/system.rs must not write processes or mutate over HTTP, found {needle:?}"
            );
        }
    }

    // ---- open_external_url: the §4.2.3 validator table (§4.2.8 fixtures) --
    // One row per hostile class the owner decision names: every rejected
    // fixture must come back as a classified, secret-free InvalidInput, and
    // every accepted fixture must survive as the parser's canonical form.

    fn assert_rejected(url: &str) {
        let err = validate_external_url(url).expect_err(url);
        assert_eq!(err.kind, ErrorKind::InvalidInput, "fixture {url:?}");
        assert!(
            safe_message(&err),
            "rejection message must stay secret-free for {url:?}"
        );
    }

    #[test]
    fn validator_rejects_every_non_http_scheme() {
        for url in [
            "javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox(1)",
            "file:///C:/Windows/System32",
            "ftp://example.com/pub",
            // Any other scheme: one allow-list, not a deny-list.
            "chrome://settings",
        ] {
            assert_rejected(url);
        }
    }

    #[test]
    fn validator_rejects_scheme_tricks_through_the_parser_not_string_match() {
        // WHATWG parsing strips tab/newline and leading/trailing spaces
        // BEFORE the scheme is read, so these all parse — as javascript:.
        // A regex deny-list would miss them; the scheme allow-list cannot.
        for url in [
            "java\nscript:alert(1)",
            "java\tscript:alert(1)",
            " javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
        ] {
            assert_rejected(url);
        }
    }

    #[test]
    fn validator_rejects_relative_and_schemeless_values() {
        for url in [
            "",
            "example.com/page",
            "./relative/page",
            // Protocol-relative: NOT an absolute URL (RelativeUrlWithoutBase).
            "//example.com/page",
            "/absolute/path",
            // Empty host after the scheme fails to parse.
            "https://",
        ] {
            assert_rejected(url);
        }
    }

    #[test]
    fn validator_rejects_userinfo() {
        for url in [
            "https://user:pass@example.com/repo",
            "https://user@example.com",
            "http://token@example.com/x",
        ] {
            assert_rejected(url);
        }
    }

    #[test]
    fn validator_rejects_oversize_urls() {
        let oversize = format!("https://example.com/?q={}", "a".repeat(2049));
        assert_rejected(&oversize);
        // Exactly at the bound is fine — the limit is inclusive.
        let at_bound = format!("https://example.com/?q={}", "a".repeat(2025));
        assert!(validate_external_url(&at_bound).is_ok());
    }

    #[test]
    fn validator_accepts_absolute_http_and_https_canonically() {
        let canonical = validate_external_url("https://example.com").expect("https accepts");
        assert_eq!(canonical, "https://example.com/");

        let canonical = validate_external_url("http://example.com/a?b=1#c").expect("http accepts");
        assert_eq!(canonical, "http://example.com/a?b=1#c");

        // Scheme and host are case-insensitive per WHATWG; the canonical
        // form is what the OS actually receives.
        let canonical = validate_external_url("HTTPS://EXAMPLE.COM").expect("case accepts");
        assert_eq!(canonical, "https://example.com/");

        // Embedded tab/newline are stripped by the parser before the scheme
        // check, so this survives as the clean https URL it canonically is.
        let canonical = validate_external_url("ht\ntps://example.com").expect("stripped");
        assert_eq!(canonical, "https://example.com/");

        // WHATWG quirk pinned on purpose: the extra slashes collapse into the
        // authority, so `http:///a` is an absolute http URL with host "a" —
        // inside the §4.2.3 allow-list, not a rejection.
        let canonical = validate_external_url("http:///a").expect("collapsed slashes");
        assert_eq!(canonical, "http://a/");
    }

    /// Wiring pin: the command exists, validates BEFORE opening, and the
    /// webview capability carries only the narrow https-scoped permission —
    /// never `opener:default` (which would grant `open_path` and http).
    /// Read from the real capability file so a scope edit cannot silently
    /// widen the surface.
    #[test]
    fn open_external_url_wiring_is_narrow() {
        assert!(
            SOURCE.contains("fn open_external_url"),
            "the open_external_url command must exist"
        );
        assert!(
            SOURCE.contains("use tauri_plugin_opener::OpenerExt"),
            "the open must go through the opener plugin's Rust API"
        );
        let capability = include_str!("../../capabilities/default.json");
        assert!(
            capability.contains("\"opener:allow-open-url\""),
            "the capability must grant allow-open-url"
        );
        assert!(
            capability.contains("\"https://*\""),
            "the URL scope must be restricted to https"
        );
        assert!(
            !capability.contains("opener:default"),
            "opener:default is too broad (grants open_path + http) — §6.1"
        );
        assert!(
            !capability.contains("allow-open-path"),
            "open_path must never be granted"
        );
    }
}
