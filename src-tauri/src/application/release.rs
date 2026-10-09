//! Release readiness + issue automation: the operator close-the-loop surface.
//!
//! What lives here (and what deliberately does not):
//!
//! - [`collect_release_status`] aggregates the honest release signals the
//!   backend can already compute — manifest version parity (`package.json` /
//!   `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` below the workspace
//!   root), migration facts (`MIGRATIONS.len()` vs the applied
//!   `schema_version`), the newest pre-update snapshot presence in the
//!   caller-given backup directory, and the local `gh` CLI presence probe.
//!   Read-only and network-free: missing manifests yield `None` versions (never
//!   an error), a missing backup directory yields no snapshot (never an
//!   error). The checklist badges render from these facts; nothing here is
//!   hand-waved, and nothing is persisted (no new tables).
//! - [`file_issue_with`] files one GitHub issue from caller-confirmed input
//!   through the user's own `gh` CLI (`gh issue create --title … --body …`,
//!   `cwd` = workspace root so the repo resolves itself). Auth stays with the
//!   CLI: this module never accepts, stores, logs, or echoes a token — the
//!   only values reaching the subprocess are the title/body strings plus the
//!   fixed `GH_PROMPT_DISABLED=1` env (so a non-signed-in CLI fails fast with
//!   a classified error instead of opening an interactive prompt on a null
//!   stdin). The body follows the fixed EN `Location / Problem / Fix / Verify`
//!   template built by [`build_issue_body`].
//! - Manifest parsing is stdlib-only by the same binding as the dependency
//!   inventory ([`crate::application::dep_inventory`]): the two JSON manifests
//!   parse through `serde_json` (already a dependency), `Cargo.toml` reads
//!   through a line scanner over its `[package]` block (no TOML parser, no
//!   new dependency).
//!
//! Update *checking* (latest release vs the running build) stays in
//! [`crate::application::github::check_update`]: it needs the network, while
//! this status is a cheap local computation the panel refreshes freely.

use std::io::ErrorKind as IoErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::infrastructure::database::{Database, DatabaseError, MIGRATIONS};

/// Manifest version caps: versions are short dotted strings; anything longer
/// is treated as absent (a corrupt manifest must not poison parity).
const MAX_VERSION_CHARS: usize = 64;

/// Issue input caps, mirroring the debt backlog vocabulary (titles 1..=200,
/// locations at most 1024, details at most 4000 chars).
const MAX_ISSUE_TITLE_CHARS: usize = 200;
const MAX_ISSUE_LOCATION_CHARS: usize = 1024;
const MAX_ISSUE_DETAIL_CHARS: usize = 4000;

/// Fixed-vocabulary issue sources (where the filing was raised from). Unknown
/// sources are rejected, never echoed.
const ISSUE_SOURCES: [&str; 3] = ["debt", "audit", "release-check"];

/// Snapshot file name prefix written by
/// [`crate::application::system::snapshot_to`].
const SNAPSHOT_PREFIX: &str = "nexora-backup-";

/// Env disabling `gh` interactive prompts so a non-signed-in CLI fails fast
/// instead of blocking on the nulled stdin.
const GH_NO_PROMPT_ENV: &str = "GH_PROMPT_DISABLED";

/// Lowercased `gh` stderr needles classifying an auth refusal (matched,
/// never echoed).
const GH_NOT_SIGNED_IN_NEEDLES: [&str; 4] = [
    "not logged in",
    "gh auth login",
    "authentication required",
    "could not authenticate",
];

/// Lowercased `gh` stderr needles classifying a non-GitHub workspace
/// (matched, never echoed).
const GH_NO_REPO_NEEDLES: [&str; 4] = [
    "not a git repository",
    "no git remotes",
    "no repository",
    "not a github",
];

/// Classified, secret-free failures of the release path. Messages are fixed
/// vocabulary: no token, no path, no manifest content, no `gh` stderr text
/// (which may quote repo paths or user names).
#[derive(Debug)]
pub(crate) enum ReleaseError {
    /// The workspace root is not a readable directory.
    InvalidRoot,
    /// A filesystem read failed for the workspace root or backup directory.
    Io,
    /// A `SQLite` operation failed.
    Database(DatabaseError),
    /// Caller-supplied issue input was rejected (fixed message, never echoes
    /// the rejected value).
    InvalidInput { message: String },
    /// The filing was invoked without the required explicit confirmation.
    Unconfirmed,
    /// The `gh` CLI binary could not be started (missing from `PATH`).
    GhMissing,
    /// The `gh` CLI is not signed in (`gh auth login` needed).
    GhNotSignedIn,
    /// The workspace is not filed under a `github.com` repository, so
    /// `gh issue create` has nowhere to file.
    NoGitHubRepo,
    /// The `gh` CLI refused or failed the create (exit status, unparseable
    /// output). The raw stderr stays in the server log only.
    GhFailed,
}

impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRoot => write!(f, "the workspace folder is not available for release"),
            Self::Io => write!(f, "the release status could not read the workspace"),
            Self::Database(err) => write!(f, "release database failure: {err}"),
            Self::InvalidInput { message } => write!(f, "{message}"),
            Self::Unconfirmed => write!(
                f,
                "explicit confirmation is required before filing a GitHub issue"
            ),
            Self::GhMissing => write!(f, "the GitHub CLI (gh) is not installed or not on PATH"),
            Self::GhNotSignedIn => write!(
                f,
                "the GitHub CLI is not signed in — run `gh auth login` in a terminal"
            ),
            Self::NoGitHubRepo => write!(
                f,
                "the workspace is not filed under a github.com repository"
            ),
            Self::GhFailed => write!(f, "the GitHub issue could not be created"),
        }
    }
}

impl std::error::Error for ReleaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DatabaseError> for ReleaseError {
    fn from(err: DatabaseError) -> Self {
        Self::Database(err)
    }
}

impl From<rusqlite::Error> for ReleaseError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(DatabaseError::Sqlite(err))
    }
}

/// Newest pre-update snapshot found in the backup directory: file name (never
/// the full path — the app-data path may carry a user name) plus size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SnapshotPresence {
    pub file_name: String,
    pub size_bytes: u64,
}

/// Release readiness facts, all computed (never hand-waved). The panel maps
/// these to pass/warn/fail badges; missing manifests and a missing backup
/// directory surface as `None`, never as errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReleaseStatus {
    /// Running build version (`CARGO_PKG_VERSION`).
    pub app_version: String,
    /// `package.json` `version` below the workspace root (`None` when the
    /// manifest is missing or unreadable).
    pub package_json_version: Option<String>,
    /// `src-tauri/Cargo.toml` `[package]` `version` (`None` when missing).
    pub cargo_version: Option<String>,
    /// `src-tauri/tauri.conf.json` `version` (`None` when missing).
    pub tauri_conf_version: Option<String>,
    /// True only when all three manifests are present and equal.
    pub versions_agree: bool,
    /// Migrations known to this build (`MIGRATIONS.len()`).
    pub migration_count: u64,
    /// Migrations applied to the database (`MAX(schema_version)`).
    pub schema_version: i64,
    /// Highest migration version known to this build.
    pub schema_target: i64,
    /// `schema_target - schema_version` (0 when fully migrated).
    pub pending_migrations: i64,
    /// Newest snapshot in the backup directory (`None` when never snapshotted).
    pub snapshot: Option<SnapshotPresence>,
    /// Whether the `gh` CLI probes present on `PATH`.
    pub gh_available: bool,
    /// First `gh --version` line's version token (`None` when unparseable).
    pub gh_version: Option<String>,
}

/// One filed issue: the canonical URL `gh` printed plus the trailing issue
/// number parsed from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CreatedIssue {
    pub url: String,
    pub number: u64,
}

/// Validated, caller-confirmed issue input: title, optional location
/// (`path:line` for findings), optional fix detail, and the fixed-vocabulary
/// source for the link-back line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssueInput {
    pub title: String,
    pub location: Option<String>,
    pub detail: Option<String>,
    pub source: String,
}

// ---------------------------------------------------------------------------
// Manifest versions
// ---------------------------------------------------------------------------

/// Read one manifest version below `root`, or `None` when the file is
/// missing, unreadable, or carries no usable version. Best-effort by design:
/// a production workspace folder is user data, not a checkout, so absent
/// manifests are an honest unknown — never an error.
fn read_manifest(root: &Path, relative: &str, parse: fn(&str) -> Option<String>) -> Option<String> {
    let text = std::fs::read_to_string(root.join(relative)).ok()?;
    parse(&text)
}

/// Parse the `version` string out of a JSON manifest (`package.json`,
/// `tauri.conf.json`). `None` for invalid JSON, a missing key, or a
/// non-string / over-long value.
#[must_use]
pub(crate) fn parse_json_manifest_version(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = value.get("version")?.as_str()?;
    if version.is_empty() || version.chars().count() > MAX_VERSION_CHARS {
        return None;
    }
    Some(version.to_string())
}

/// Parse the `[package]` `version` out of `Cargo.toml` with a stdlib-only
/// line scanner (no TOML parser — same binding as the dependency inventory).
/// Only `version = "…"` lines *after* the `[package]` header and *before*
/// the next section header count, so dependency `version` requirements never
/// leak in. `None` when the block or a usable value is absent.
#[must_use]
pub(crate) fn parse_cargo_manifest_version(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("version") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = rest.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            let version = &value[1..value.len() - 1];
            if !version.is_empty() && version.chars().count() <= MAX_VERSION_CHARS {
                return Some(version.to_string());
            }
            return None;
        }
    }
    None
}

/// True only when all three manifest versions are present and equal. A
/// missing manifest is a disagree (the panel renders *which* manifest is
/// unknown from the individual fields).
#[must_use]
pub(crate) fn versions_agree(
    package_json: Option<&str>,
    cargo: Option<&str>,
    tauri_conf: Option<&str>,
) -> bool {
    match (package_json, cargo, tauri_conf) {
        (Some(first), Some(second), Some(third)) => first == second && second == third,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Issue body + output parsing (pure)
// ---------------------------------------------------------------------------

/// Build the fixed EN `Location / Problem / Fix / Verify` body for one
/// issue. `problem` is the confirmed title text; `app_version` stamps the
/// link-back trailer. Every section is always present so the template never
/// ships an empty heading.
#[must_use]
pub(crate) fn build_issue_body(
    location: Option<&str>,
    problem: &str,
    detail: Option<&str>,
    source: &str,
    app_version: &str,
) -> String {
    let location = location.unwrap_or("Not recorded — see Problem.");
    let fix = detail.unwrap_or("Triage from Location, then update this issue with the chosen fix.");
    format!(
        "## Location\n{location}\n\n\
         ## Problem\n{problem}\n\n\
         ## Fix\n{fix}\n\n\
         ## Verify\n\
         - Re-run the originating check and confirm it passes.\n\
         - Confirm no new findings in the same area.\n\n\
         ---\n\
         Filed from Nexora {app_version} with `gh issue create` (source: {source}). \
         Auth stays in your gh CLI — Nexora stores no GitHub tokens."
    )
}

/// Parse `gh issue create` stdout into `(number, url)`: the first
/// whitespace-separated token starting with `http`, with the issue number as
/// its trailing path segment. `None` when no usable URL is present.
#[must_use]
pub(crate) fn parse_created_issue(stdout: &str) -> Option<CreatedIssue> {
    let url = stdout
        .split_whitespace()
        .find(|token| token.starts_with("http"))?;
    let number = url.rsplit('/').next()?.parse::<u64>().ok()?;
    Some(CreatedIssue {
        url: url.to_string(),
        number,
    })
}

/// Parse the version token out of `gh --version` output (`gh version
/// 2.74.2 (…)` → `2.74.2`). `None` for anything else.
#[must_use]
pub(crate) fn parse_gh_version(output: &str) -> Option<String> {
    let first = output.lines().next()?;
    let mut tokens = first.split_whitespace();
    if tokens.next()? != "gh" || tokens.next()? != "version" {
        return None;
    }
    let version = tokens.next()?;
    if version.is_empty() || version.chars().count() > MAX_VERSION_CHARS {
        return None;
    }
    Some(version.to_string())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate caller-supplied issue input. The `confirmed` flag carries the
/// destructive-action confirmation pattern (bare IPC callers cannot file):
/// `false` refuses with [`ReleaseError::Unconfirmed`]. Length caps mirror
/// the debt backlog; messages are fixed vocabulary and never echo the
/// rejected value.
///
/// # Errors
///
/// Returns [`ReleaseError::Unconfirmed`] without confirmation, or
/// [`ReleaseError::InvalidInput`] for an empty/over-long title, an unknown
/// source, or over-long location/detail.
pub(crate) fn validate_issue_input(
    title: &str,
    location: Option<&str>,
    detail: Option<&str>,
    source: &str,
    confirmed: bool,
) -> Result<IssueInput, ReleaseError> {
    if !confirmed {
        return Err(ReleaseError::Unconfirmed);
    }
    let title_len = title.chars().count();
    if title.trim().is_empty() || title_len > MAX_ISSUE_TITLE_CHARS {
        return Err(ReleaseError::InvalidInput {
            message: "the issue title must be 1..200 characters".to_string(),
        });
    }
    if !ISSUE_SOURCES.contains(&source) {
        return Err(ReleaseError::InvalidInput {
            message: "the issue source is invalid".to_string(),
        });
    }
    if location.is_some_and(|value| value.chars().count() > MAX_ISSUE_LOCATION_CHARS) {
        return Err(ReleaseError::InvalidInput {
            message: "the issue location must be at most 1024 characters".to_string(),
        });
    }
    if detail.is_some_and(|value| value.chars().count() > MAX_ISSUE_DETAIL_CHARS) {
        return Err(ReleaseError::InvalidInput {
            message: "the issue detail must be at most 4000 characters".to_string(),
        });
    }
    Ok(IssueInput {
        title: title.to_string(),
        location: location.map(str::to_string),
        detail: detail.map(str::to_string),
        source: source.to_string(),
    })
}

// ---------------------------------------------------------------------------
// gh CLI probing + filing
// ---------------------------------------------------------------------------

/// Probe one `gh`-compatible binary (`--version`): `available` is false when
/// the binary cannot be started; the version token is best-effort.
fn probe_gh_with(gh_bin: &str) -> (bool, Option<String>) {
    let output = Command::new(gh_bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Err(_) => (false, None),
        Ok(output) if !output.status.success() => (false, None),
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            (true, parse_gh_version(&text))
        }
    }
}

/// Whether the `gh` CLI probes present on `PATH`, plus its version token.
#[must_use]
pub(crate) fn gh_probe() -> (bool, Option<String>) {
    probe_gh_with("gh")
}

/// Classify a failed `gh issue create` run from its stderr (matched, never
/// echoed — stderr may quote repo paths or user names).
fn classify_gh_failure(stderr: &str) -> ReleaseError {
    let lowered = stderr.to_lowercase();
    if GH_NOT_SIGNED_IN_NEEDLES
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return ReleaseError::GhNotSignedIn;
    }
    if GH_NO_REPO_NEEDLES
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        return ReleaseError::NoGitHubRepo;
    }
    // The raw detail stays in the server log only (see `file_issue_with`).
    ReleaseError::GhFailed
}

/// File one validated issue through `gh_bin` with `cwd` as the repo. Only the
/// title/body strings reach the subprocess (argv, never a shell — no
/// injection), plus the fixed no-prompt env. Auth is the CLI's own.
///
/// # Errors
///
/// Returns [`ReleaseError::GhMissing`] when the binary cannot be started,
/// [`ReleaseError::GhNotSignedIn`] / [`ReleaseError::NoGitHubRepo`] for the
/// classified refusals, [`ReleaseError::GhFailed`] for any other refusal
/// (stderr is logged server-side, never returned).
pub(crate) fn file_issue_with(
    cwd: &Path,
    gh_bin: &str,
    input: &IssueInput,
    app_version: &str,
) -> Result<CreatedIssue, ReleaseError> {
    let body = build_issue_body(
        input.location.as_deref(),
        &input.title,
        input.detail.as_deref(),
        &input.source,
        app_version,
    );
    let output = Command::new(gh_bin)
        .args(["issue", "create", "--title", &input.title, "--body", &body])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(GH_NO_PROMPT_ENV, "1")
        .output()
        .map_err(|err| {
            if err.kind() == IoErrorKind::NotFound {
                ReleaseError::GhMissing
            } else {
                log::error!("gh issue create spawn failed: {err}");
                ReleaseError::GhFailed
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!(
            "gh issue create refused (status {}): {stderr}",
            output.status
        );
        return Err(classify_gh_failure(&stderr));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_created_issue(&stdout).ok_or_else(|| {
        log::error!("gh issue create printed no parseable URL: {stdout}");
        ReleaseError::GhFailed
    })
}

// ---------------------------------------------------------------------------
// Status aggregation
// ---------------------------------------------------------------------------

/// Highest migration version known to this build.
fn schema_target() -> i64 {
    MIGRATIONS
        .iter()
        .map(|(version, _)| *version)
        .max()
        .unwrap_or(0)
}

/// Newest snapshot in `backup_dir` (lexicographic max — the file name carries
/// the Unix timestamp). A missing directory means "never snapshotted" and
/// yields `None`, never an error; only the name (never the path) is reported.
///
/// # Errors
///
/// Returns [`ReleaseError::Io`] when the directory cannot be listed and
/// [`ReleaseError::Database`] when a metadata read fails.
fn newest_snapshot(backup_dir: &Path) -> Result<Option<SnapshotPresence>, ReleaseError> {
    let entries = match std::fs::read_dir(backup_dir) {
        Err(err) if err.kind() == IoErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReleaseError::Io),
        Ok(entries) => entries,
    };
    let mut newest: Option<(String, u64)> = None;
    for entry in entries {
        let entry = entry.map_err(|_| ReleaseError::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_snapshot = name.starts_with(SNAPSHOT_PREFIX)
            && std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("db"));
        if !is_snapshot {
            continue;
        }
        let size_bytes = entry.metadata().map_err(|_| ReleaseError::Io)?.len();
        if newest.as_ref().is_none_or(|(current, _)| name > *current) {
            newest = Some((name, size_bytes));
        }
    }
    Ok(newest.map(|(file_name, size_bytes)| SnapshotPresence {
        file_name,
        size_bytes,
    }))
}

/// Collect the release readiness facts: manifest versions below `root`,
/// migration facts from `db`, the newest snapshot in `backup_dir`, and the
/// local `gh` probe. Read-only and network-free.
///
/// # Errors
///
/// Returns [`ReleaseError::InvalidRoot`] when `root` is not a readable
/// directory, [`ReleaseError::Io`] when the backup directory cannot be
/// listed, [`ReleaseError::Database`] when the schema version query fails.
pub(crate) fn collect_release_status(
    db: &Database,
    root: &Path,
    backup_dir: &Path,
) -> Result<ReleaseStatus, ReleaseError> {
    if !root.is_dir() {
        return Err(ReleaseError::InvalidRoot);
    }
    let package_json_version = read_manifest(root, "package.json", parse_json_manifest_version);
    let cargo_version = read_manifest(root, "src-tauri/Cargo.toml", parse_cargo_manifest_version);
    let tauri_conf_version = read_manifest(
        root,
        "src-tauri/tauri.conf.json",
        parse_json_manifest_version,
    );
    let conn = db.lock().map_err(ReleaseError::from)?;
    let schema_version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;
    drop(conn);
    let target = schema_target();
    let (gh_available, gh_version) = gh_probe();
    Ok(ReleaseStatus {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        versions_agree: versions_agree(
            package_json_version.as_deref(),
            cargo_version.as_deref(),
            tauri_conf_version.as_deref(),
        ),
        package_json_version,
        cargo_version,
        tauri_conf_version,
        migration_count: u64::try_from(MIGRATIONS.len()).unwrap_or(0),
        pending_migrations: target.saturating_sub(schema_version),
        schema_version,
        schema_target: target,
        snapshot: newest_snapshot(backup_dir)?,
        gh_available,
        gh_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    const SECRET_SENTINELS: [&str; 6] = [
        "sk-secret",
        "credential",
        "api_key",
        "api-key",
        "ghp_",
        "bearer ",
    ];

    fn assert_secret_free(text: &str) {
        let lowered = text.to_lowercase();
        for sentinel in SECRET_SENTINELS {
            assert!(
                !lowered.contains(sentinel),
                "release surface must stay secret-free, found {sentinel:?} in {text}"
            );
        }
    }

    #[test]
    fn json_manifest_versions_parse_or_absent() {
        assert_eq!(
            parse_json_manifest_version(r#"{"name":"nexora","version":"1.5.0"}"#),
            Some("1.5.0".to_string())
        );
        assert_eq!(parse_json_manifest_version(r#"{"name":"nexora"}"#), None);
        assert_eq!(parse_json_manifest_version("not json"), None);
        assert_eq!(
            parse_json_manifest_version(r#"{"version":12}"#),
            None,
            "non-string versions are absent"
        );
        assert_eq!(
            parse_json_manifest_version(r#"{"version":""}"#),
            None,
            "empty versions are absent"
        );
    }

    #[test]
    fn cargo_manifest_version_reads_only_the_package_block() {
        let text = "[package]\nname = \"nexora\"\nversion = \"1.5.0\"\n\n\
            [dependencies]\nserde = { version = \"1.0\" }\n";
        assert_eq!(
            parse_cargo_manifest_version(text),
            Some("1.5.0".to_string())
        );
        // A dependency-only manifest must not leak a requirement as the app
        // version.
        let deps_only = "[dependencies]\nserde = { version = \"9.9.9\" }\n";
        assert_eq!(parse_cargo_manifest_version(deps_only), None);
        assert_eq!(
            parse_cargo_manifest_version("[package]\nname = \"x\"\n"),
            None
        );
        assert_eq!(parse_cargo_manifest_version(""), None);
    }

    #[test]
    fn version_parity_needs_all_three_present_and_equal() {
        assert!(versions_agree(Some("1.5.0"), Some("1.5.0"), Some("1.5.0")));
        assert!(!versions_agree(Some("1.5.0"), Some("1.5.1"), Some("1.5.0")));
        assert!(!versions_agree(Some("1.5.0"), Some("1.5.0"), None));
        assert!(!versions_agree(None, None, None));
    }

    #[test]
    fn issue_body_carries_location_problem_fix_verify() {
        let body = build_issue_body(
            Some("src-tauri/src/application/example.rs:42"),
            "Unwrap hotspot on user input",
            Some("Replace with a validated error."),
            "audit",
            "1.5.0",
        );
        for heading in ["## Location", "## Problem", "## Fix", "## Verify"] {
            assert!(body.contains(heading), "missing {heading} in:\n{body}");
        }
        assert!(body.contains("src-tauri/src/application/example.rs:42"));
        assert!(body.contains("Unwrap hotspot on user input"));
        assert!(body.contains("Replace with a validated error."));
        assert_secret_free(&body);
    }

    #[test]
    fn issue_body_never_ships_empty_sections() {
        let body = build_issue_body(None, "Title", None, "debt", "1.5.0");
        for heading in ["## Location", "## Problem", "## Fix", "## Verify"] {
            assert!(body.contains(heading), "missing {heading} in:\n{body}");
        }
        assert!(body.contains("Title"));
        assert_secret_free(&body);
    }

    #[test]
    fn created_issue_parses_url_and_number() {
        let parsed = parse_created_issue("https://github.com/o/r/issues/123\n")
            .expect("parses the printed URL");
        assert_eq!(parsed.number, 123);
        assert_eq!(parsed.url, "https://github.com/o/r/issues/123");
        assert!(parse_created_issue("").is_none());
        assert!(parse_created_issue("no url here").is_none());
        assert!(parse_created_issue("https://github.com/o/r/issues/notanumber").is_none());
    }

    #[test]
    fn gh_version_parses_the_first_line_token() {
        assert_eq!(
            parse_gh_version("gh version 2.74.2 (2024-01-01)\n"),
            Some("2.74.2".to_string())
        );
        assert_eq!(parse_gh_version(""), None);
        assert_eq!(parse_gh_version("git version 2.43.0\n"), None);
    }

    #[test]
    fn issue_input_validation_rejects_without_echo() {
        // Unconfirmed callers cannot file, even with valid input.
        let err = validate_issue_input("t", None, None, "debt", false)
            .expect_err("unconfirmed must refuse");
        assert!(
            matches!(err, ReleaseError::Unconfirmed),
            "unconfirmed must refuse, got {err}"
        );
        // Empty titles and unknown sources refuse with fixed text.
        let err = validate_issue_input("", None, None, "debt", true).expect_err("empty refuses");
        assert!(matches!(err, ReleaseError::InvalidInput { .. }));
        let err = validate_issue_input("t", None, None, "carrier-pigeon", true)
            .expect_err("source refuses");
        assert!(matches!(err, ReleaseError::InvalidInput { .. }));
        assert_secret_free(&err.to_string());
        // Over-long values refuse; in-cap values pass through.
        let long = "x".repeat(MAX_ISSUE_TITLE_CHARS + 1);
        assert!(validate_issue_input(&long, None, None, "debt", true).is_err());
        let input = validate_issue_input("Fix it", Some("a.rs:1"), Some("d"), "audit", true)
            .expect("valid input passes");
        assert_eq!(input.source, "audit");
    }

    #[test]
    fn missing_gh_binary_fails_honestly() {
        let input = validate_issue_input("Fix it", None, None, "debt", true).expect("valid");
        let err = file_issue_with(
            Path::new("."),
            "nexora-definitely-absent-gh-binary",
            &input,
            "1.5.0",
        )
        .expect_err("absent binary must refuse");
        assert!(
            matches!(err, ReleaseError::GhMissing),
            "absent binary must refuse honestly, got {err}"
        );
        assert_secret_free(&err.to_string());
        let (available, version) = probe_gh_with("nexora-definitely-absent-gh-binary");
        assert!(!available);
        assert_eq!(version, None);
    }

    #[test]
    fn gh_failure_classification_never_echoes_stderr() {
        let planted = "error: not logged in, run `gh auth login` (user sk-secret-PLANTED)";
        let err = classify_gh_failure(planted);
        assert!(
            matches!(err, ReleaseError::GhNotSignedIn),
            "auth refusal must classify, got {err}"
        );
        assert_secret_free(&err.to_string());
        assert!(!err.to_string().contains("PLANTED"));
        let err = classify_gh_failure("boom sk-secret-PLANTED");
        assert!(
            matches!(err, ReleaseError::GhFailed),
            "other refusals fail honestly, got {err}"
        );
        assert!(!err.to_string().contains("PLANTED"));
    }

    /// Status over a fixture workspace: agreeing manifests, one snapshot, a
    /// fully migrated in-memory database. No network, no `gh` dependency
    /// (availability is reported as-is, never asserted).
    #[test]
    fn status_computes_parity_migrations_and_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "nexora-release-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let src_tauri = root.join("src-tauri");
        let backups = root.join("backups");
        std::fs::create_dir_all(&src_tauri).expect("fixture dirs");
        std::fs::create_dir_all(&backups).expect("fixture backups");
        std::fs::write(root.join("package.json"), r#"{"version":"1.5.0"}"#).expect("pkg");
        std::fs::write(
            src_tauri.join("Cargo.toml"),
            "[package]\nversion = \"1.5.0\"\n",
        )
        .expect("cargo");
        std::fs::write(src_tauri.join("tauri.conf.json"), r#"{"version":"1.5.0"}"#).expect("conf");
        std::fs::write(backups.join("nexora-backup-1700000000.db"), b"fake").expect("snap");

        let db = in_memory_database();
        let status = collect_release_status(&db, &root, &backups).expect("status computes");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(status.package_json_version.as_deref(), Some("1.5.0"));
        assert_eq!(status.cargo_version.as_deref(), Some("1.5.0"));
        assert_eq!(status.tauri_conf_version.as_deref(), Some("1.5.0"));
        assert!(status.versions_agree);
        assert_eq!(
            status.migration_count,
            u64::try_from(MIGRATIONS.len()).unwrap_or(0)
        );
        assert!(status.migration_count > 0);
        assert_eq!(status.schema_version, status.schema_target);
        assert_eq!(status.pending_migrations, 0);
        let snapshot = status.snapshot.as_ref().expect("snapshot found");
        assert_eq!(snapshot.file_name, "nexora-backup-1700000000.db");
        assert!(snapshot.size_bytes > 0);
        let json = serde_json::to_string(&status).expect("status serializes");
        assert_secret_free(&json);
    }

    /// Status over an empty directory: unknown versions disagree honestly, no
    /// snapshot — and still no error.
    #[test]
    fn status_over_missing_manifests_is_unknown_not_error() {
        let root = std::env::temp_dir().join(format!(
            "nexora-release-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        std::fs::create_dir_all(&root).expect("fixture dir");
        let db = in_memory_database();
        let status =
            collect_release_status(&db, &root, &root.join("backups")).expect("unknown, not error");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(status.package_json_version, None);
        assert!(!status.versions_agree);
        assert_eq!(status.snapshot, None);
        assert_eq!(status.pending_migrations, 0);
    }

    #[test]
    fn status_refuses_non_directories() {
        let db = in_memory_database();
        let missing = std::env::temp_dir().join(format!(
            "nexora-release-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        let err = collect_release_status(&db, &missing, &missing).expect_err("refuses");
        assert!(
            matches!(err, ReleaseError::InvalidRoot),
            "non-directories refuse, got {err}"
        );
    }

    /// Static invocation-shape check: the `gh` subprocess receives exactly
    /// `issue create --title … --body …` (argv, never a shell) with prompts
    /// disabled — no auth flags, no auth headers, no credential store
    /// anywhere on this path. Auth is the CLI's own. Needles are built with
    /// `concat!` so this test's own source never matches them verbatim.
    #[test]
    fn gh_invocation_passes_only_title_and_body() {
        const SOURCE: &str = include_str!("release.rs");
        for needle in [
            "\"issue\"",
            "\"create\"",
            "\"--title\"",
            "\"--body\"",
            "GH_PROMPT_DISABLED",
            "Stdio::null",
        ] {
            assert!(
                SOURCE.contains(needle),
                "the gh invocation shape must exist, missing {needle:?}"
            );
        }
        for needle in [
            concat!("--tok", "en"),
            concat!("GH", "_TOKEN"),
            concat!("GITHUB", "_TOKEN"),
            concat!("Authoriz", "ation"),
            concat!("key", "ring"),
            concat!("--head", "er"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "only title/body strings may reach gh, found {needle:?}"
            );
        }
    }
}
