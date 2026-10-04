//! Read-only GitHub issues / pull-requests lists for the workspace repo.
//!
//! The panel ("issues/PRs") renders the `origin` repository's issues and pull
//! requests through the public GitHub REST API (`https://api.github.com`,
//! `github.com` only — no Enterprise host). This module owns the whole
//! read-only path:
//!
//! - remote detection: the enclosing repository's `origin` URL is parsed into
//!   `(owner, repo)` ([`parse_github_remote`]); anything that is not a
//!   `github.com` remote refuses with [`GitHubError::NoGitHubRemote`];
//! - auth: the token resolves from the OS keyring entry `github` through the
//!   existing [`CredentialStore`](crate::infrastructure::providers::credentials::CredentialStore)
//!   — the same keyring story as the AI providers, no new auth mechanism. A
//!   missing or unreadable entry is NOT an error: the request goes out
//!   unauthenticated (60 requests/hour shared quota) and the response reports
//!   `authenticated: false` so the panel can show its clean connect-hint;
//! - HTTP: the existing `reqwest` blocking client (already a dependency with
//!   the `blocking` + `rustls` features — no new crate). GET only: the module
//!   performs exactly two request shapes, `GET /repos/{owner}/{repo}/issues`
//!   and `GET /repos/{owner}/{repo}/pulls`, one page of at most
//!   [`MAX_ITEMS`] items each. There is no POST/PATCH/PUT/DELETE anywhere on
//!   this path (pinned by the `read_path_uses_get_only` test);
//! - rate-limit honesty: `x-ratelimit-limit/remaining/reset` surface on every
//!   response, and an exhausted quota (403 with `remaining == 0`, or 429)
//!   resolves to an empty list with `rate_limited: true` — never a silent
//!   empty, never an error dump;
//! - secrecy: the token is borrowed for the header only and never stored,
//!   logged, or echoed; every [`GitHubError`] message is fixed vocabulary.
//!
//! Command-shape decision (one feature area, TWO commands): `gh_issues` and
//! `gh_pulls` stay separate because the panel's kind tabs invoke them
//! independently with their own state filter, exactly like the VCS panel's
//! separate lazy diff commands.
//!
//! Actions fail-loop extension (read-only CI surface for the same panel):
//! [`list_actions`] reports the recent workflow runs through three GET-only
//! endpoints — the runs envelope (`GET .../actions/runs`), the per-run jobs
//! envelope (`GET .../actions/runs/{run_id}/jobs`, fetched only for failing
//! runs), and the per-job TEXT log (`GET .../actions/jobs/{job_id}/logs`,
//! redirect-following download — deliberately not the per-run ZIP, which
//! would need a zip dependency for a 200-line tail). Each failed job keeps
//! its failed step names plus a capped tail (last [`LOG_TAIL_LINES`] lines
//! under [`LOG_MAX_CHARS`] chars) scrubbed by [`redact_secrets`], since CI
//! logs may echo secrets. The fix loop creates a task-manager task prefilled
//! with the failure context (existing task path — approval/budget inherited);
//! there is no auto-fix and no re-run (no POST anywhere on this path).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::infrastructure::providers::credentials::CredentialStore;

/// Keyring entry holding the optional GitHub personal-access token, inside
/// the shared `nexora` service namespace. Reuses the provider credential
/// story: `CredentialStore::read` returns `None` when the user never stored
/// one, which the panel treats as the clean connect-hint state.
pub(crate) const GITHUB_KEYRING_ENTRY: &str = "github";

/// GitHub REST base. `github.com` only — Enterprise hosts are out of scope
/// and refused by [`parse_github_remote`].
const GITHUB_API_BASE: &str = "https://api.github.com";

/// One page of at most this many items (`per_page` + a defensive cut).
pub(crate) const MAX_ITEMS: usize = 50;

/// Workflow runs listed per `gh_actions` call (`per_page` + defensive cut).
pub(crate) const MAX_RUNS: usize = 20;

/// Failing runs expanded with jobs/logs per `gh_actions` call (the rest
/// render as metadata rows pointing at GitHub).
pub(crate) const MAX_FAILED_RUNS_EXPANDED: usize = 5;

/// Jobs fetched per expanded run (`per_page` + defensive cut), of which at
/// most [`MAX_FAILED_JOBS_KEPT`] failed jobs are kept.
const MAX_JOBS_FETCHED: usize = 30;
pub(crate) const MAX_FAILED_JOBS_KEPT: usize = 5;

/// Log tail: the last this many lines of a failed job's text log.
pub(crate) const LOG_TAIL_LINES: usize = 200;

/// Log tail: at most this many chars (cut from the front when longer).
pub(crate) const LOG_MAX_CHARS: usize = 24_000;

/// Oversized text-log downloads keep only their last this many bytes (the
/// failure is at the tail) before decoding.
const LOG_DOWNLOAD_BYTES: usize = 512_000;

/// Item bodies longer than this (chars) are cut server-side with
/// `body_truncated: true`.
pub(crate) const MAX_BODY_CHARS: usize = 8_000;

/// Per-request wall-clock bound so a stalled API call cannot hang the panel.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Fixed-vocabulary state filter accepted by [`list_issues`] / [`list_pulls`].
/// Anything else refuses with [`GitHubError::InvalidState`].
pub(crate) const LIST_STATES: [&str; 3] = ["open", "closed", "all"];

/// Classified, secret-free failures of the GitHub read path. Messages are
/// fixed vocabulary: no token, no URL, no body text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitHubError {
    /// The workspace is not inside a usable git repository.
    NotARepository,
    /// The `origin` remote is absent or is not a `github.com` repository.
    NoGitHubRemote,
    /// The state filter is not one of `open` / `closed` / `all`.
    InvalidState,
    /// Transport, timeout, or response-parse failure (including unexpected
    /// API statuses).
    RequestFailed,
    /// A stored token was sent and the API rejected it (401).
    Unauthorized,
    /// The `owner/repo` was not found on `github.com` (404).
    NotFound,
    /// The API refused the request for a non-quota reason (non-quota 403).
    Forbidden,
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARepository => write!(f, "the workspace is not inside a git repository"),
            Self::NoGitHubRemote => {
                write!(f, "the workspace origin is not a github.com repository")
            }
            Self::InvalidState => write!(f, "the state filter is invalid"),
            Self::RequestFailed => write!(f, "the GitHub request could not be completed"),
            Self::Unauthorized => write!(f, "the stored GitHub token was rejected"),
            Self::NotFound => write!(f, "the GitHub repository was not found"),
            Self::Forbidden => write!(f, "the GitHub request was refused"),
        }
    }
}

impl std::error::Error for GitHubError {}

/// Rate-limit snapshot echoed from the response headers (`None` when the
/// header was absent — rendered as "unknown", never as zero).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct GhRateLimit {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset: Option<u64>,
}

/// One issue row: fixed-vocabulary metadata plus the capped body. Pull-request
/// rows served by the `/issues` endpoint are filtered out before this shape
/// is built, so `gh_issues` never mixes kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub labels: Vec<String>,
    pub comments: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub html_url: Option<String>,
    pub body: String,
    pub body_truncated: bool,
}

/// One pull-request row: fixed-vocabulary metadata plus the capped body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhPull {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub draft: bool,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub comments: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub html_url: Option<String>,
    pub body: String,
    pub body_truncated: bool,
}

/// One `gh_issues` response: the resolved repo, the echo of the state filter,
/// the capped items, whether a token was sent, and the rate-limit snapshot.
/// A quota-exhausted API answers with empty `items` and `rate_limited: true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhIssuesResponse {
    pub owner: String,
    pub repo: String,
    pub state: String,
    pub items: Vec<GhIssue>,
    pub authenticated: bool,
    pub rate_limited: bool,
    pub rate_limit: GhRateLimit,
}

/// One `gh_pulls` response: same envelope as [`GhIssuesResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhPullsResponse {
    pub owner: String,
    pub repo: String,
    pub state: String,
    pub items: Vec<GhPull>,
    pub authenticated: bool,
    pub rate_limited: bool,
    pub rate_limit: GhRateLimit,
}

/// One failed CI job inside a failing workflow run: fixed-vocabulary
/// metadata, the failed step names (at most 10), and the capped,
/// secret-scrubbed log tail. `log_unavailable` means the log was expired or
/// never uploaded (empty download) or its fetch failed — the panel points
/// at GitHub instead. `log_redacted` flags a scrubbed excerpt so the panel
/// can warn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhActionJob {
    pub id: u64,
    pub run_id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub html_url: Option<String>,
    pub failed_steps: Vec<String>,
    pub log_excerpt: String,
    pub log_truncated: bool,
    pub log_unavailable: bool,
    pub log_redacted: bool,
}

/// One workflow run: fixed-vocabulary metadata plus the failed jobs (only
/// failing runs expand — at most [`MAX_FAILED_JOBS_KEPT`], with
/// `jobs_truncated` marking the remainder). Non-failing runs carry an empty
/// `failed_jobs` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhActionRun {
    pub id: u64,
    pub name: String,
    pub head_branch: Option<String>,
    pub head_sha: Option<String>,
    pub event: String,
    pub status: String,
    pub conclusion: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub html_url: Option<String>,
    pub failed_jobs: Vec<GhActionJob>,
    pub jobs_truncated: bool,
}

/// One `gh_actions` response: the resolved repo, the recent runs (at most
/// [`MAX_RUNS`], newest first per the API), the API's `total_count`, whether
/// a token was sent, and the rate-limit snapshot. A quota-exhausted API
/// answers with empty `runs` and `rate_limited: true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GhActionsResponse {
    pub owner: String,
    pub repo: String,
    pub runs: Vec<GhActionRun>,
    pub total_runs: u64,
    pub authenticated: bool,
    pub rate_limited: bool,
    pub rate_limit: GhRateLimit,
}

/// Minimal GitHub item shape: only the fields the panel renders. Unknown or
/// missing fields fall back to fixed vocabulary — the panel never invents
/// content.
#[derive(Debug, Deserialize)]
struct RawItem {
    number: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    /// Present (any shape) on `/issues` rows that are pull requests.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    head: Option<RawRef>,
    #[serde(default)]
    base: Option<RawRef>,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLabel {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRef {
    #[serde(default, rename = "ref")]
    ref_name: Option<String>,
}

/// Minimal workflow-runs envelope: only the fields the panel renders.
#[derive(Debug, Deserialize)]
struct RawRuns {
    #[serde(default)]
    total_count: u64,
    #[serde(default)]
    workflow_runs: Vec<RawRun>,
}

#[derive(Debug, Deserialize)]
struct RawRun {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

/// Minimal jobs envelope for one expanded run.
#[derive(Debug, Deserialize)]
struct RawJobs {
    #[serde(default)]
    jobs: Vec<RawJob>,
}

#[derive(Debug, Deserialize)]
struct RawJob {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    steps: Vec<RawStep>,
}

#[derive(Debug, Deserialize)]
struct RawStep {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}

/// Parse an `origin` remote URL into `(owner, repo)`.
///
/// Accepted (all `github.com` only):
/// - `https://github.com/owner/repo` (optional `.git`, optional trailing `/`)
/// - `http://github.com/owner/repo` (same options)
/// - `git@github.com:owner/repo` (scp-like, optional `.git`)
/// - `ssh://git@github.com/owner/repo` (optional `.git`)
///
/// Anything else — Enterprise hosts, non-GitHub URLs, missing parts, deeper
/// paths, illegal characters — yields `None`.
pub(crate) fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let mut path: Option<&str> = None;
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ] {
        if let Some(rest) = url.strip_prefix(prefix) {
            path = Some(rest);
            break;
        }
    }
    let path = path?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    if !is_repo_part(owner) || !is_repo_part(repo) {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Owner/repo charset (`A–Z a–z 0–9 . _ -`): strict so the parts can be
/// interpolated into the request path without quoting surprises.
fn is_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-')
}

/// Resolve the `origin` remote of the enclosing repository into
/// `(owner, repo)`.
///
/// # Errors
///
/// Returns [`GitHubError::NotARepository`] when no enclosing repository
/// exists (or it is bare), [`GitHubError::NoGitHubRemote`] when `origin` is
/// absent or is not a `github.com` remote.
fn resolve_origin(workspace_root: &Path) -> Result<(String, String), GitHubError> {
    let canon = std::fs::canonicalize(workspace_root).map_err(|_| GitHubError::NotARepository)?;
    let repo = git2::Repository::discover(&canon).map_err(|_| GitHubError::NotARepository)?;
    if repo.workdir().is_none() {
        return Err(GitHubError::NotARepository);
    }
    let remote = repo
        .find_remote("origin")
        .map_err(|_| GitHubError::NoGitHubRemote)?;
    let url = remote.url().map_err(|_| GitHubError::NoGitHubRemote)?;
    parse_github_remote(url).ok_or(GitHubError::NoGitHubRemote)
}

/// Read the optional GitHub token from the OS keyring. A missing entry,
/// an empty value, or an unreachable keyring all resolve to `None` — the
/// caller then sends the request unauthenticated instead of failing.
fn read_token() -> Option<String> {
    let token = CredentialStore::read(GITHUB_KEYRING_ENTRY).ok()??;
    if token.trim().is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Validate the state filter against [`LIST_STATES`].
///
/// # Errors
///
/// Returns [`GitHubError::InvalidState`] for anything outside the vocabulary.
fn check_state(state: &str) -> Result<&str, GitHubError> {
    if LIST_STATES.contains(&state) {
        Ok(state)
    } else {
        Err(GitHubError::InvalidState)
    }
}

/// Cut an item body to [`MAX_BODY_CHARS`] chars, reporting the cut.
fn cap_body(body: Option<String>) -> (String, bool) {
    let text = body.unwrap_or_default();
    if text.chars().count() > MAX_BODY_CHARS {
        (text.chars().take(MAX_BODY_CHARS).collect(), true)
    } else {
        (text, false)
    }
}

/// Keep only allowlisted states; the API only ever sends `open`/`closed`,
/// and anything else renders as `unknown` rather than echoing raw text.
fn clean_state(state: Option<String>) -> String {
    match state.as_deref() {
        Some("open" | "closed") => state.unwrap_or_default(),
        _ => "unknown".to_string(),
    }
}

fn clean_title(title: Option<String>) -> String {
    let title = title.unwrap_or_default();
    if title.trim().is_empty() {
        "(no title)".to_string()
    } else {
        title
    }
}

fn clean_author(user: Option<RawUser>) -> String {
    user.and_then(|user| user.login).map_or_else(
        || "unknown".to_string(),
        |login| {
            if login.trim().is_empty() {
                "unknown".to_string()
            } else {
                login
            }
        },
    )
}

/// Label names only (at most 20 — the API bounds the rest by shape, and the
/// panel renders tokens, not prose).
fn clean_labels(labels: Vec<RawLabel>) -> Vec<String> {
    labels
        .into_iter()
        .filter_map(|label| label.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .take(20)
        .collect()
}

fn to_issue(raw: RawItem) -> GhIssue {
    let (body, body_truncated) = cap_body(raw.body);
    GhIssue {
        number: raw.number,
        title: clean_title(raw.title),
        state: clean_state(raw.state),
        author: clean_author(raw.user),
        labels: clean_labels(raw.labels),
        comments: raw.comments.unwrap_or(0),
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        html_url: raw.html_url,
        body,
        body_truncated,
    }
}

fn to_pull(raw: RawItem) -> GhPull {
    let (body, body_truncated) = cap_body(raw.body);
    GhPull {
        number: raw.number,
        title: clean_title(raw.title),
        state: clean_state(raw.state),
        author: clean_author(raw.user),
        draft: raw.draft.unwrap_or(false),
        head_ref: raw.head.and_then(|head| head.ref_name),
        base_ref: raw.base.and_then(|base| base.ref_name),
        comments: raw.comments.unwrap_or(0),
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        html_url: raw.html_url,
        body,
        body_truncated,
    }
}

/// Read one `x-ratelimit-*` header as `u64` (`None` when absent/unparsable).
fn rate_header(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
}

fn rate_snapshot(headers: &reqwest::header::HeaderMap) -> GhRateLimit {
    GhRateLimit {
        limit: rate_header(headers, "x-ratelimit-limit"),
        remaining: rate_header(headers, "x-ratelimit-remaining"),
        reset: rate_header(headers, "x-ratelimit-reset"),
    }
}

/// Outcome of one list fetch against `base_url` (production: [`GITHUB_API_BASE`):
/// the raw items plus the rate snapshot. A quota-exhausted API (403 with
/// `remaining == 0`, or 429) resolves to empty items with `rate_limited`.
fn fetch_from(
    base_url: &str,
    owner: &str,
    repo: &str,
    endpoint: &str,
    state: &str,
    token: Option<&str>,
) -> Result<(Vec<RawItem>, GhRateLimit, bool), GitHubError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Nexora")
        .build()
        .map_err(|_| GitHubError::RequestFailed)?;
    let url =
        format!("{base_url}/repos/{owner}/{repo}/{endpoint}?state={state}&per_page={MAX_ITEMS}");
    let mut request = client
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|_| GitHubError::RequestFailed)?;
    let rate_limit = rate_snapshot(response.headers());
    let rate_limited = rate_limit.remaining == Some(0);
    match response.status().as_u16() {
        200 => {
            let items: Vec<RawItem> = response.json().map_err(|_| GitHubError::RequestFailed)?;
            Ok((
                items.into_iter().take(MAX_ITEMS).collect(),
                rate_limit,
                false,
            ))
        }
        401 => Err(GitHubError::Unauthorized),
        403 if rate_limited => Ok((Vec::new(), rate_limit, true)),
        403 => Err(GitHubError::Forbidden),
        404 => Err(GitHubError::NotFound),
        429 => Ok((Vec::new(), rate_limit, true)),
        _ => Err(GitHubError::RequestFailed),
    }
}

/// List the workspace `origin` repo's issues (pull-request rows excluded).
///
/// # Errors
///
/// Returns [`GitHubError::NotARepository`] / [`GitHubError::NoGitHubRemote`]
/// when the workspace has no usable `github.com` origin,
/// [`GitHubError::InvalidState`] for a bad filter, and the classified
/// transport/auth failures otherwise. A missing keyring token is NOT an
/// error — the request goes out unauthenticated with `authenticated: false`.
pub(crate) fn list_issues(
    workspace_root: &Path,
    state: &str,
) -> Result<GhIssuesResponse, GitHubError> {
    let state = check_state(state)?.to_string();
    let (owner, repo) = resolve_origin(workspace_root)?;
    let token = read_token();
    let (raw, rate_limit, rate_limited) = fetch_from(
        GITHUB_API_BASE,
        &owner,
        &repo,
        "issues",
        &state,
        token.as_deref(),
    )?;
    let items = raw
        .into_iter()
        .filter(|item| item.pull_request.is_none())
        .map(to_issue)
        .collect();
    Ok(GhIssuesResponse {
        owner,
        repo,
        state,
        items,
        authenticated: token.is_some(),
        rate_limited,
        rate_limit,
    })
}

/// List the workspace `origin` repo's pull requests.
///
/// # Errors
///
/// Same contract as [`list_issues`].
pub(crate) fn list_pulls(
    workspace_root: &Path,
    state: &str,
) -> Result<GhPullsResponse, GitHubError> {
    let state = check_state(state)?.to_string();
    let (owner, repo) = resolve_origin(workspace_root)?;
    let token = read_token();
    let (raw, rate_limit, rate_limited) = fetch_from(
        GITHUB_API_BASE,
        &owner,
        &repo,
        "pulls",
        &state,
        token.as_deref(),
    )?;
    let items = raw.into_iter().map(to_pull).collect();
    Ok(GhPullsResponse {
        owner,
        repo,
        state,
        items,
        authenticated: token.is_some(),
        rate_limited,
        rate_limit,
    })
}

/// Failed-run vocabulary: conclusions that make a run a fix-loop candidate.
/// `success` / `neutral` / `skipped` need no fix; `stale` is a scheduling
/// artefact, not a failure; `unknown` (unparseable API text) never expands —
/// the panel points at GitHub instead.
fn is_failed_conclusion(conclusion: &str) -> bool {
    matches!(
        conclusion,
        "failure" | "timed_out" | "cancelled" | "action_required"
    )
}

/// A run expands (jobs + log tails) only when it completed with a failed
/// conclusion. In-flight runs render as metadata rows.
fn run_is_failed(run: &GhActionRun) -> bool {
    run.status == "completed" && is_failed_conclusion(&run.conclusion)
}

/// Keep only allowlisted run conclusions; anything else renders as `unknown`
/// rather than echoing raw text.
fn clean_conclusion(conclusion: Option<String>) -> String {
    match conclusion.as_deref() {
        Some(
            "success" | "failure" | "neutral" | "cancelled" | "skipped" | "timed_out"
            | "action_required" | "stale",
        ) => conclusion.unwrap_or_default(),
        _ => "unknown".to_string(),
    }
}

/// Keep only allowlisted run/job statuses; anything else renders as `unknown`.
fn clean_run_status(status: Option<String>) -> String {
    match status.as_deref() {
        Some("queued" | "in_progress" | "completed" | "requested" | "waiting" | "pending") => {
            status.unwrap_or_default()
        }
        _ => "unknown".to_string(),
    }
}

/// Workflow trigger events are open vocabulary (`push`, `pull_request`,
/// ...): echo the trimmed value capped at 64 chars, `unknown` when empty.
fn clean_event(event: Option<String>) -> String {
    let event = event.unwrap_or_default();
    let trimmed = event.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    trimmed.chars().take(64).collect()
}

fn to_action_run(raw: RawRun) -> GhActionRun {
    GhActionRun {
        id: raw.id,
        name: clean_title(raw.name),
        head_branch: raw.head_branch,
        head_sha: raw.head_sha,
        event: clean_event(raw.event),
        status: clean_run_status(raw.status),
        conclusion: clean_conclusion(raw.conclusion),
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        html_url: raw.html_url,
        failed_jobs: Vec::new(),
        jobs_truncated: false,
    }
}

/// Failed step names of one job (at most 10, each capped at 120 chars).
fn failed_step_names(steps: Vec<RawStep>) -> Vec<String> {
    steps
        .into_iter()
        .filter(|step| step.conclusion.as_deref().is_some_and(is_failed_conclusion))
        .filter_map(|step| step.name)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().take(120).collect::<String>())
        .take(10)
        .collect()
}

/// Keep the last [`LOG_TAIL_LINES`] lines under [`LOG_MAX_CHARS`] chars,
/// reporting any cut. The failure is at the tail, so cuts drop the head.
fn tail_log(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(LOG_TAIL_LINES);
    let joined = lines[start..].join("\n");
    let count = joined.chars().count();
    if count > LOG_MAX_CHARS {
        let skip = count - LOG_MAX_CHARS;
        (joined.chars().skip(skip).collect(), true)
    } else {
        (joined, start > 0)
    }
}

/// Token prefixes scrubbed from log excerpts (prefix kept, value replaced).
const SECRET_PREFIXES: [&str; 13] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "sk-ant-",
    "sk-proj-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "AKIA",
];

/// Scrub probable secret values from a log excerpt: known token prefixes
/// keep their prefix with the value replaced, and `Bearer <value>` (any
/// ASCII case) keeps the scheme with the value replaced. Returns the
/// scrubbed text and whether anything was replaced (the panel warns when
/// true). Short runs under 4 value chars are left alone so ordinary words
/// like `sk-` prose are never mangled.
fn redact_secrets(text: &str) -> (String, bool) {
    fn is_token_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'-' | b'~' | b'+' | b'/' | b'.' | b'=')
    }
    let bytes = text.as_bytes();
    let mut scrubbed = String::with_capacity(text.len());
    let mut index = 0;
    let mut hit = false;
    // `index` starts at 0 and advances by ASCII runs or whole chars, so it
    // is always a char boundary and the slicing below is safe.
    while index < bytes.len() {
        if index + 7 <= bytes.len() && bytes[index..index + 7].eq_ignore_ascii_case(b"bearer ") {
            scrubbed.push_str(&text[index..index + 7]);
            index += 7;
            let value_start = index;
            while index < bytes.len() && is_token_byte(bytes[index]) {
                index += 1;
            }
            if index > value_start {
                scrubbed.push_str("[redacted]");
                hit = true;
            }
            continue;
        }
        let mut prefix_len = 0;
        for prefix in SECRET_PREFIXES {
            if text[index..].starts_with(prefix) {
                prefix_len = prefix.len();
                break;
            }
        }
        if prefix_len == 0 {
            match text[index..].chars().next() {
                Some(ch) => {
                    scrubbed.push(ch);
                    index += ch.len_utf8();
                }
                None => break,
            }
            continue;
        }
        scrubbed.push_str(&text[index..index + prefix_len]);
        index += prefix_len;
        let value_start = index;
        while index < bytes.len() && is_token_byte(bytes[index]) {
            index += 1;
        }
        if index - value_start >= 4 {
            scrubbed.push_str("[redacted]");
            hit = true;
        } else {
            scrubbed.push_str(&text[value_start..index]);
        }
    }
    (scrubbed, hit)
}

/// Outcome of one per-job text-log download.
struct LogDownload {
    /// Decoded tail bytes (empty when the log is missing).
    text: String,
    /// The download was cut (oversized) — the excerpt flags truncation.
    cut: bool,
}

/// GET one JSON envelope (`{...}`) for the Actions path: runs and jobs both
/// answer envelopes, unlike the issues/pulls lists. Same auth, timeout, and
/// status contract as [`fetch_from`]: quota exhaustion (403 with
/// `remaining == 0`, or 429) resolves to `Ok((None, rate, true))`.
fn fetch_json_value(
    base_url: &str,
    path: &str,
    token: Option<&str>,
) -> Result<(Option<serde_json::Value>, GhRateLimit, bool), GitHubError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Nexora")
        .build()
        .map_err(|_| GitHubError::RequestFailed)?;
    let mut request = client
        .get(format!("{base_url}{path}"))
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|_| GitHubError::RequestFailed)?;
    let rate_limit = rate_snapshot(response.headers());
    let rate_limited = rate_limit.remaining == Some(0);
    match response.status().as_u16() {
        200 => {
            let value: serde_json::Value =
                response.json().map_err(|_| GitHubError::RequestFailed)?;
            Ok((Some(value), rate_limit, false))
        }
        401 => Err(GitHubError::Unauthorized),
        403 if rate_limited => Ok((None, rate_limit, true)),
        403 => Err(GitHubError::Forbidden),
        404 => Err(GitHubError::NotFound),
        429 => Ok((None, rate_limit, true)),
        _ => Err(GitHubError::RequestFailed),
    }
}

/// GET one failed job's text log (`.../actions/jobs/{job_id}/logs`).
///
/// Deliberately the per-job TEXT endpoint, not the per-run ZIP
/// (`.../runs/{run_id}/logs` returns a ZIP archive: extracting a 200-line
/// tail from it would need a zip dependency, out of scope). No `Accept:
/// vnd.github+json` header here — the endpoint serves `text/plain`
/// (possibly via redirect, which the client follows). Oversized downloads
/// keep only their last [`LOG_DOWNLOAD_BYTES`] bytes before decoding; 404 /
/// 410 (expired or never uploaded) resolve to an empty download, which the
/// caller renders as `log_unavailable`.
fn fetch_log_text(
    base_url: &str,
    owner: &str,
    repo: &str,
    job_id: u64,
    token: Option<&str>,
) -> Result<LogDownload, GitHubError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Nexora")
        .build()
        .map_err(|_| GitHubError::RequestFailed)?;
    let mut request = client.get(format!(
        "{base_url}/repos/{owner}/{repo}/actions/jobs/{job_id}/logs"
    ));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|_| GitHubError::RequestFailed)?;
    match response.status().as_u16() {
        200 => {
            let bytes = response.bytes().map_err(|_| GitHubError::RequestFailed)?;
            let all: &[u8] = bytes.as_ref();
            let (cut, tail) = if all.len() > LOG_DOWNLOAD_BYTES {
                (true, &all[all.len() - LOG_DOWNLOAD_BYTES..])
            } else {
                (false, all)
            };
            Ok(LogDownload {
                text: String::from_utf8_lossy(tail).into_owned(),
                cut,
            })
        }
        401 => Err(GitHubError::Unauthorized),
        403 => Err(GitHubError::Forbidden),
        404 | 410 => Ok(LogDownload {
            text: String::new(),
            cut: false,
        }),
        _ => Err(GitHubError::RequestFailed),
    }
}

/// Build one failed job row: failed step names plus the capped, scrubbed log
/// tail. Any log-fetch failure degrades to `log_unavailable` — one expired
/// log never fails the whole response.
fn build_failed_job(
    base_url: &str,
    owner: &str,
    repo: &str,
    token: Option<&str>,
    run_id: u64,
    raw: RawJob,
) -> GhActionJob {
    let failed_steps = failed_step_names(raw.steps);
    let (log_excerpt, log_truncated, log_unavailable, log_redacted) =
        match fetch_log_text(base_url, owner, repo, raw.id, token) {
            Ok(download) if download.text.is_empty() => (String::new(), false, true, false),
            Ok(download) => {
                let (tail, tail_cut) = tail_log(&download.text);
                let (scrubbed, redacted) = redact_secrets(&tail);
                (scrubbed, tail_cut || download.cut, false, redacted)
            }
            Err(_) => (String::new(), false, true, false),
        };
    GhActionJob {
        id: raw.id,
        run_id,
        name: clean_title(raw.name),
        status: clean_run_status(raw.status),
        conclusion: clean_conclusion(raw.conclusion),
        started_at: raw.started_at,
        completed_at: raw.completed_at,
        html_url: raw.html_url,
        failed_steps,
        log_excerpt,
        log_truncated,
        log_unavailable,
        log_redacted,
    }
}

/// Expand one failing run with its failed jobs (at most
/// [`MAX_FAILED_JOBS_KEPT`]). Any jobs-fetch failure leaves the run with an
/// empty job list — the panel still points at GitHub.
fn expand_failed_run(
    base_url: &str,
    owner: &str,
    repo: &str,
    token: Option<&str>,
    run: &mut GhActionRun,
) {
    let fetched = fetch_json_value(
        base_url,
        &format!(
            "/repos/{owner}/{repo}/actions/runs/{}/jobs?per_page={MAX_JOBS_FETCHED}",
            run.id
        ),
        token,
    );
    let Ok((Some(value), _, _)) = fetched else {
        return;
    };
    let envelope: RawJobs = match serde_json::from_value(value) {
        Ok(envelope) => envelope,
        Err(_) => return,
    };
    let mut failed: Vec<RawJob> = envelope
        .jobs
        .into_iter()
        .filter(|job| job.conclusion.as_deref().is_some_and(is_failed_conclusion))
        .collect();
    let total_failed = failed.len();
    failed.truncate(MAX_FAILED_JOBS_KEPT);
    run.jobs_truncated = total_failed > failed.len();
    for raw in failed {
        run.failed_jobs
            .push(build_failed_job(base_url, owner, repo, token, run.id, raw));
    }
}

/// List the workspace `origin` repo's recent workflow runs with failing runs
/// expanded (failed jobs + capped, scrubbed log tails).
///
/// Read-only: three GET shapes only (runs envelope, per-run jobs envelope,
/// per-job text log). Sub-call failures degrade to empty jobs / unavailable
/// logs — only the top-level runs fetch can fail the call.
///
/// # Errors
///
/// Returns [`GitHubError::NotARepository`] / [`GitHubError::NoGitHubRemote`]
/// when the workspace has no usable `github.com` origin, and the classified
/// transport/auth failures otherwise. A missing keyring token is NOT an
/// error — the requests go out unauthenticated with `authenticated: false`.
pub(crate) fn list_actions(workspace_root: &Path) -> Result<GhActionsResponse, GitHubError> {
    let (owner, repo) = resolve_origin(workspace_root)?;
    let token = read_token();
    let (value, rate_limit, rate_limited) = fetch_json_value(
        GITHUB_API_BASE,
        &format!("/repos/{owner}/{repo}/actions/runs?per_page={MAX_RUNS}"),
        token.as_deref(),
    )?;
    let Some(value) = value else {
        return Ok(GhActionsResponse {
            owner,
            repo,
            runs: Vec::new(),
            total_runs: 0,
            authenticated: token.is_some(),
            rate_limited: true,
            rate_limit,
        });
    };
    let envelope: RawRuns =
        serde_json::from_value(value).map_err(|_| GitHubError::RequestFailed)?;
    let mut runs: Vec<GhActionRun> = Vec::new();
    let mut expanded = 0_usize;
    for raw in envelope.workflow_runs.into_iter().take(MAX_RUNS) {
        let mut run = to_action_run(raw);
        if expanded < MAX_FAILED_RUNS_EXPANDED && run_is_failed(&run) {
            expanded += 1;
            expand_failed_run(GITHUB_API_BASE, &owner, &repo, token.as_deref(), &mut run);
        }
        runs.push(run);
    }
    Ok(GhActionsResponse {
        owner,
        repo,
        runs,
        total_runs: envelope.total_count,
        authenticated: token.is_some(),
        rate_limited,
        rate_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];

    fn safe_message(message: &str) -> bool {
        !SECRET_SENTINELS
            .iter()
            .any(|needle| message.to_lowercase().contains(needle))
    }

    #[test]
    fn error_messages_are_fixed_vocabulary_and_secret_free() {
        // The token must never be echoed: formatting an error next to a
        // planted secret keeps the secret out of the text.
        const PLANTED: &str = "ghp_plantedsecret00";
        let errors = [
            GitHubError::NotARepository,
            GitHubError::NoGitHubRemote,
            GitHubError::InvalidState,
            GitHubError::RequestFailed,
            GitHubError::Unauthorized,
            GitHubError::NotFound,
            GitHubError::Forbidden,
        ];
        for error in errors {
            let message = error.to_string();
            assert!(!message.is_empty());
            assert!(safe_message(&message), "must stay secret-free: {message:?}");
        }
        for error in errors {
            let message = format!("{error} {PLANTED}");
            assert!(message.contains(PLANTED));
            assert!(
                !error.to_string().contains(PLANTED),
                "error text itself must not carry the token"
            );
        }
    }

    #[test]
    fn remote_parsing_accepts_github_shapes_only() {
        for (url, owner, repo) in [
            ("https://github.com/sshdw/Nexora", "sshdw", "Nexora"),
            ("https://github.com/sshdw/Nexora.git", "sshdw", "Nexora"),
            ("https://github.com/sshdw/Nexora/", "sshdw", "Nexora"),
            ("http://github.com/o/r.git", "o", "r"),
            ("git@github.com:sshdw/Nexora.git", "sshdw", "Nexora"),
            ("git@github.com:sshdw/Nexora", "sshdw", "Nexora"),
            ("ssh://git@github.com/sshdw/Nexora.git", "sshdw", "Nexora"),
            ("  https://github.com/o/my.repo-1_2  ", "o", "my.repo-1_2"),
        ] {
            assert_eq!(
                parse_github_remote(url),
                Some((owner.to_string(), repo.to_string())),
                "must parse {url:?}"
            );
        }
        for url in [
            "https://enterprise.example.com/o/r.git",
            "https://github.com/only-owner",
            "https://github.com//repo",
            "https://github.com/o/r/extra",
            "https://gitlab.com/o/r.git",
            "git@gitlab.com:o/r.git",
            "https://github.com/o/re po",
            "https://github.com/o/r?tab=x",
            "",
            "not a url",
        ] {
            assert_eq!(parse_github_remote(url), None, "must refuse {url:?}");
        }
    }

    #[test]
    fn state_filter_accepts_the_fixed_vocabulary_only() {
        for state in ["open", "closed", "all"] {
            assert_eq!(check_state(state), Ok(state));
        }
        for state in ["", "OPEN", "merged", "open,closed", "all "] {
            assert_eq!(check_state(state), Err(GitHubError::InvalidState));
        }
    }

    #[test]
    fn issue_mapping_caps_bodies_and_fixes_vocabulary() {
        let raw: RawItem = serde_json::from_str(
            r#"{
                "number": 106,
                "title": " ",
                "state": "bogus",
                "body": "hello",
                "user": {},
                "labels": [{"name": " bug "}, {}, {"name": ""}],
                "comments": 3,
                "created_at": "2026-09-01T00:00:00Z",
                "html_url": "https://github.com/o/r/issues/106",
                "pull_request": {"url": "https://api.github.com/x"}
            }"#,
        )
        .expect("fixture parses");
        assert!(raw.pull_request.is_some());
        let issue = to_issue(raw);
        assert_eq!(issue.title, "(no title)");
        assert_eq!(issue.state, "unknown");
        assert_eq!(issue.author, "unknown");
        assert_eq!(issue.labels, vec!["bug".to_string()]);
        assert!(!issue.body_truncated);

        let long = "x".repeat(MAX_BODY_CHARS + 1);
        let raw: RawItem = serde_json::from_str(&format!(
            r#"{{"number": 1, "title": "t", "state": "open", "body": "{long}"}}"#
        ))
        .expect("long fixture parses");
        let issue = to_issue(raw);
        assert!(issue.body_truncated);
        assert_eq!(issue.body.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn pull_mapping_reads_refs_and_draft() {
        let raw: RawItem = serde_json::from_str(
            r#"{
                "number": 7,
                "title": "Add panel",
                "state": "open",
                "user": {"login": "octo"},
                "draft": true,
                "head": {"ref": "task/gh-issues"},
                "base": {"ref": "main"},
                "comments": 1,
                "body": null
            }"#,
        )
        .expect("fixture parses");
        let pull = to_pull(raw);
        assert_eq!(pull.author, "octo");
        assert!(pull.draft);
        assert_eq!(pull.head_ref.as_deref(), Some("task/gh-issues"));
        assert_eq!(pull.base_ref.as_deref(), Some("main"));
        assert_eq!(pull.body, "");
        assert!(!pull.body_truncated);
    }

    /// Serve one scripted response (status + JSON body + rate headers) over
    /// plain HTTP on localhost. Hermetic: loopback only, no live network.
    fn serve_once(
        status: u16,
        body: Vec<u8>,
        rate: Option<(String, String, String)>,
        seen_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("loopback address");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&raw);
            seen_auth.store(
                head.contains("Bearer test-token"),
                std::sync::atomic::Ordering::SeqCst,
            );
            let rate_headers = rate.map_or_else(String::new, |(limit, remaining, reset)| {
                format!(
                    "X-Ratelimit-Limit: {limit}\r\nX-Ratelimit-Remaining: {remaining}\r\nX-Ratelimit-Reset: {reset}\r\n"
                )
            });
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                 {rate_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://{addr}")
    }

    #[test]
    fn fetch_parses_items_and_rate_headers_over_get() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let base = serve_once(
            200,
            br#"[{"number":106,"title":"Ship panel","state":"open","user":{"login":"octo"},"labels":[],"comments":2,"body":"hi"}]"#.to_vec(),
            Some(("60".to_string(), "59".to_string(), "1234567890".to_string())),
            std::sync::Arc::clone(&seen),
        );
        let (items, rate, limited) =
            fetch_from(&base, "o", "r", "issues", "open", None).expect("200 parses");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 106);
        assert_eq!(
            rate,
            GhRateLimit {
                limit: Some(60),
                remaining: Some(59),
                reset: Some(1_234_567_890),
            }
        );
        assert!(!limited);
        assert!(!seen.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn fetch_sends_the_token_and_reports_quota_exhaustion_as_state() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let base = serve_once(
            200,
            b"[]".to_vec(),
            Some(("5000".to_string(), "4999".to_string(), "123".to_string())),
            std::sync::Arc::clone(&seen),
        );
        let (_, rate, limited) =
            fetch_from(&base, "o", "r", "pulls", "open", Some("test-token")).expect("200 parses");
        assert!(!limited);
        assert_eq!(rate.remaining, Some(4999));
        // The server thread may still be accepting; poll briefly without a
        // timing assert (timing asserts are forbidden).
        for _ in 0..100 {
            if seen.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(seen.load(std::sync::atomic::Ordering::SeqCst));

        // A 403 with remaining == 0 is quota exhaustion: empty items with
        // `rate_limited`, not an error.
        let base = serve_once(
            403,
            br#"{"message":"API rate limit exceeded"}"#.to_vec(),
            Some(("60".to_string(), "0".to_string(), "999".to_string())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let (items, rate, limited) = fetch_from(&base, "o", "r", "issues", "open", None)
            .expect("quota exhaustion is state, not error");
        assert!(items.is_empty());
        assert!(limited);
        assert_eq!(rate.remaining, Some(0));
    }

    #[test]
    fn fetch_classifies_auth_and_missing_repos() {
        let base = serve_once(
            401,
            b"{}".to_vec(),
            None,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(
            fetch_from(&base, "o", "r", "issues", "open", Some("bad")).map(|_| ()),
            Err(GitHubError::Unauthorized)
        );
        let base = serve_once(
            404,
            b"{}".to_vec(),
            None,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(
            fetch_from(&base, "o", "r", "issues", "open", None).map(|_| ()),
            Err(GitHubError::NotFound)
        );
        let base = serve_once(
            403,
            b"{}".to_vec(),
            Some(("60".to_string(), "12".to_string(), "999".to_string())),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(
            fetch_from(&base, "o", "r", "issues", "open", None).map(|_| ()),
            Err(GitHubError::Forbidden)
        );
    }

    #[test]
    fn action_conclusions_classify_failures_only() {
        for conclusion in ["failure", "timed_out", "cancelled", "action_required"] {
            assert!(
                is_failed_conclusion(conclusion),
                "{conclusion:?} must expand"
            );
        }
        for conclusion in [
            "success", "neutral", "skipped", "stale", "unknown", "", "bogus",
        ] {
            assert!(
                !is_failed_conclusion(conclusion),
                "{conclusion:?} must never expand"
            );
        }
        // Only completed runs with a failed conclusion expand; in-flight or
        // successful runs render as metadata rows.
        let failed = GhActionRun {
            id: 1,
            name: "ci".to_string(),
            head_branch: None,
            head_sha: None,
            event: "push".to_string(),
            status: "completed".to_string(),
            conclusion: "failure".to_string(),
            created_at: None,
            updated_at: None,
            html_url: None,
            failed_jobs: Vec::new(),
            jobs_truncated: false,
        };
        assert!(run_is_failed(&failed));
        let running = GhActionRun {
            status: "in_progress".to_string(),
            ..failed.clone()
        };
        assert!(!run_is_failed(&running));
        let green = GhActionRun {
            status: "completed".to_string(),
            conclusion: "success".to_string(),
            ..failed.clone()
        };
        assert!(!run_is_failed(&green));
    }

    #[test]
    fn action_mapping_fixes_vocabulary_and_failed_steps() {
        let raw: RawRun = serde_json::from_str(
            r#"{
                "id": 42,
                "name": " ",
                "head_branch": "main",
                "head_sha": "abc123",
                "event": "push",
                "status": "bogus",
                "conclusion": "bogus",
                "html_url": "https://github.com/o/r/actions/runs/42"
            }"#,
        )
        .expect("fixture parses");
        let run = to_action_run(raw);
        assert_eq!(run.name, "(no title)");
        assert_eq!(run.status, "unknown");
        assert_eq!(run.conclusion, "unknown");
        assert!(run.failed_jobs.is_empty());
        assert!(!run.jobs_truncated);

        let steps = vec![
            RawStep {
                name: Some(" build ".to_string()),
                conclusion: Some("failure".to_string()),
            },
            RawStep {
                name: Some("ok".to_string()),
                conclusion: Some("success".to_string()),
            },
            RawStep {
                name: Some(String::new()),
                conclusion: Some("timed_out".to_string()),
            },
            RawStep {
                name: None,
                conclusion: Some("failure".to_string()),
            },
        ];
        assert_eq!(failed_step_names(steps), vec!["build".to_string()]);
    }

    #[test]
    fn log_tail_keeps_the_last_lines_under_both_caps() {
        let lines: Vec<String> = (1..=500).map(|n| format!("line {n}")).collect();
        let (tail, truncated) = tail_log(&lines.join("\n"));
        assert!(truncated);
        let kept: Vec<&str> = tail.lines().collect();
        assert_eq!(kept.len(), LOG_TAIL_LINES);
        assert_eq!(kept[0], "line 301");
        assert_eq!(kept[LOG_TAIL_LINES - 1], "line 500");

        let (tail, truncated) = tail_log("a\nb");
        assert!(!truncated);
        assert_eq!(tail, "a\nb");

        let wide = "y".repeat(LOG_MAX_CHARS + 10);
        let (tail, truncated) = tail_log(&wide);
        assert!(truncated);
        assert_eq!(tail.chars().count(), LOG_MAX_CHARS);
    }

    #[test]
    fn log_redaction_scrubs_token_values_and_reports() {
        const PLANTED_GH: &str = "ghp_plantedvalue0123456789";
        const PLANTED_BEARER: &str = "supersecretbearer01";
        const PLANTED_SK: &str = "sk-ant-plantedvalue99";
        let text = format!(
            "token {PLANTED_GH} failed\nAuthorization: Bearer {PLANTED_BEARER}\nkey {PLANTED_SK} here\nplain prose stays"
        );
        let (scrubbed, hit) = redact_secrets(&text);
        assert!(hit);
        assert!(!scrubbed.contains(PLANTED_GH));
        assert!(!scrubbed.contains(PLANTED_BEARER));
        assert!(!scrubbed.contains(PLANTED_SK));
        assert!(scrubbed.contains("[redacted]"));
        assert!(scrubbed.contains("plain prose stays"));

        let (scrubbed, hit) = redact_secrets("nothing secret here\njust lines");
        assert!(!hit);
        assert_eq!(scrubbed, "nothing secret here\njust lines");
    }

    #[test]
    fn actions_envelope_parses_runs_jobs_and_rate_state() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let base = serve_once(
            200,
            br#"{"total_count":1,"workflow_runs":[{"id":42,"name":"CI","head_branch":"main","status":"completed","conclusion":"success"}]}"#.to_vec(),
            Some(("60".to_string(), "59".to_string(), "1234567890".to_string())),
            std::sync::Arc::clone(&seen),
        );
        let (value, rate, limited) =
            fetch_json_value(&base, "/repos/o/r/actions/runs?per_page=20", None)
                .expect("200 parses");
        assert!(!limited);
        assert_eq!(rate.remaining, Some(59));
        let envelope: RawRuns =
            serde_json::from_value(value.expect("envelope present")).expect("envelope parses");
        assert_eq!(envelope.total_count, 1);
        assert_eq!(envelope.workflow_runs.len(), 1);
        let run = to_action_run(envelope.workflow_runs.into_iter().next().expect("one run"));
        assert_eq!(run.id, 42);
        assert_eq!(run.conclusion, "success");
        assert!(!run_is_failed(&run));
    }

    /// Static proof of the read-only contract: the service performs GET
    /// requests only — no POST/PATCH/PUT/DELETE anywhere. Needles are built
    /// with `concat!` so this test's own source never matches them.
    #[test]
    fn read_path_uses_get_only() {
        const SOURCE: &str = include_str!("github.rs");
        assert!(
            SOURCE.contains(".get("),
            "the GitHub read path must issue GET requests"
        );
        for needle in [
            concat!(".", "post("),
            concat!(".", "put("),
            concat!(".", "patch("),
            concat!(".", "delete("),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "the GitHub path must stay read-only, found {needle:?}"
            );
        }
    }
}
