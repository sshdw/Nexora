//! Read-only projection sources for the TUI depth stage (#165 6b).
//!
//! The shipped binary now reads real projections from `nexora_runtime`:
//!
//! - `RuntimeSessionSource` lists persisted conversations through
//!   `ConversationService`, reads their newest run/step rows for the status
//!   vocabulary, and filters through the local `SearchService`; opening a
//!   session is still read-only and NEVER starts an agent;
//! - `RuntimeDiffSource` calls the version-control read service
//!   (`git_prepare_review_diff`) with its byte cap and mandatory secret
//!   redaction, then maps the unified text onto the render DTOs. It never
//!   applies, stages or writes anything;
//! - `RuntimeApprovalView` reads the live run's parked approval through the
//!   runtime inspector. A/D are answered through `permissions::resolve_approval`
//!   by the event loop (see `super::mod`); this view only observes;
//! - `OsClipboardSink` is a best-effort OS clipboard adapter (`false` on any
//!   failure). Tests inject scripted sources through [`Sources::scripted`].
//!
//! The command catalog is the one purely local source: it is derived from the
//! real Clap tree, so the palette shares one grammar with `--help`. Every
//! string entering a projection passes the existing [`sanitize`](super::app::sanitize)
//! helper (control-character stripping + secret redaction).
//!
//! Data access stays read-only: a missing database is reported `Unavailable`,
//! never created by a view.

#![allow(clippy::module_name_repetitions)]

use std::path::PathBuf;

use clap::CommandFactory;
use nexora_core::storage::repository::agent_runs::AgentRunRepository;
use nexora_runtime::application::agent::inspect::live_run_state;
use nexora_runtime::application::conversations::ConversationService;
use nexora_runtime::application::search::LocalSearchService;
use nexora_runtime::application::version_control::git_prepare_review_diff;
use nexora_runtime::infrastructure::database::Database;

use super::app::sanitize;

/// A backend answer that is either real (owned) or honestly absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness<T> {
    Ready(T),
    Unavailable,
}

/// Persisted run-status vocabulary only (`NEXORA-CLI-PLAN.md` §4). The TUI must
/// not invent transitional states such as `paused` for a stored session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Running,
    Completed,
    Cancelled,
    BudgetExhausted,
    SpendLimitExceeded,
    Error,
}

impl SessionStatus {
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted => "budget_exhausted",
            Self::SpendLimitExceeded => "spend_limit_exceeded",
            Self::Error => "error",
        }
    }

    /// Map one *persisted* `agent_runs.status` value. Unknown values are not
    /// fabricated into a known state; they are dropped by the source.
    fn from_persisted(status: &str) -> Option<Self> {
        match status {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            "budget_exhausted" => Some(Self::BudgetExhausted),
            "spend_limit_exceeded" => Some(Self::SpendLimitExceeded),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// A search-friendly, owned session row. `active` distinguishes the live
/// session from history; it is supplied by the source, never guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) status: SessionStatus,
    pub(crate) age_label: String,
    pub(crate) active: bool,
}

/// Risk taxonomy from the shared approval policy (`NEXORA-CLI-PLAN.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskClass {
    ReadOnly,
    Mutating,
    DangerousShell,
}

impl RiskClass {
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "ReadOnly",
            Self::Mutating => "Mutating",
            Self::DangerousShell => "DangerousShell",
        }
    }
}

/// One pending approval shown as an inline timeline block (never an overlay).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalRequest {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) cwd: String,
    pub(crate) risk: RiskClass,
    pub(crate) reason: String,
}

/// A diff line's semantic class; rendered as a `+`/`-`/space column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) header: String,
    pub(crate) lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileDiff {
    pub(crate) path: String,
    pub(crate) plus: u32,
    pub(crate) minus: u32,
    pub(crate) hunks: Vec<Hunk>,
    /// A thin touch marker is drawn only when the source marks the file touched.
    pub(crate) touched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffSnapshot {
    pub(crate) files: Vec<FileDiff>,
}

/// Whether a palette command is a real UI action or a CLI grammar entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandOrigin {
    Cli,
    Ui,
}

/// Availability of a palette entry. Backend/CLI actions that are not wired to
/// a TUI action carry a fixed reason; genuinely wired actions are available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    /// Canonical grammar path (e.g. `config set`) or a local id (`ui.sessions`).
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) about: String,
    pub(crate) origin: CommandOrigin,
    pub(crate) availability: Availability,
}

// ---- read-only traits ----------------------------------------------------

pub(crate) trait SessionSource {
    fn list(&self) -> Readiness<Vec<SessionSummary>>;

    /// Filtered view. The default is a local projection over [`Self::list`],
    /// which is all a scripted source can honestly provide. The shipped source
    /// overrides it with the runtime's FTS-backed local search service.
    fn search(&self, query: &str) -> Readiness<Vec<SessionSummary>> {
        let needle = query.trim().to_lowercase();
        match self.list() {
            Readiness::Ready(rows) => Readiness::Ready(
                rows.into_iter()
                    .filter(|row| {
                        needle.is_empty()
                            || row.title.to_lowercase().contains(&needle)
                            || row.id.contains(&needle)
                            || row.status.label().contains(&needle)
                    })
                    .collect(),
            ),
            Readiness::Unavailable => Readiness::Unavailable,
        }
    }
}

pub(crate) trait DiffSource {
    fn current(&self) -> Readiness<DiffSnapshot>;
}

pub(crate) trait ApprovalView {
    fn pending(&self) -> Readiness<Option<ApprovalRequest>>;
}

pub(crate) trait CommandCatalog {
    fn commands(&self) -> Readiness<Vec<CommandSpec>>;
}

pub(crate) trait ClipboardSink {
    /// `true` only when a real copy succeeded. A failed OS adapter returns
    /// `false`; no text or OS error is surfaced to the UI.
    fn copy(&self, text: &str) -> bool;
}

// ---- honest absence (also used by the scripted bundle) -------------------

#[cfg(test)]
pub(crate) struct UnavailableSessionSource;

#[cfg(test)]
impl SessionSource for UnavailableSessionSource {
    fn list(&self) -> Readiness<Vec<SessionSummary>> {
        Readiness::Unavailable
    }
}

#[cfg(test)]
pub(crate) struct UnavailableDiffSource;

#[cfg(test)]
impl DiffSource for UnavailableDiffSource {
    fn current(&self) -> Readiness<DiffSnapshot> {
        Readiness::Unavailable
    }
}

#[cfg(test)]
pub(crate) struct UnavailableApprovalView;

#[cfg(test)]
impl ApprovalView for UnavailableApprovalView {
    fn pending(&self) -> Readiness<Option<ApprovalRequest>> {
        Readiness::Unavailable
    }
}

// ---- real, read-only runtime sources -------------------------------------

/// Human-readable age from a persisted Unix-seconds timestamp. Counts only;
/// never a path, title or payload.
fn age_label(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        });
    let age = now.saturating_sub(timestamp).max(0);
    match age {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", age / 60),
        3_600..=86_399 => format!("{}h ago", age / 3_600),
        _ => format!("{}d ago", age / 86_400),
    }
}

/// Sessions backed by the shared conversation + run repositories.
pub(crate) struct RuntimeSessionSource {
    /// Test-only in-memory override; `None` in the shipped binary.
    db: Option<Database>,
}

impl RuntimeSessionSource {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { db: None }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_db(db: Database) -> Self {
        Self { db: Some(db) }
    }

    fn open_db(&self) -> Option<Database> {
        self.db.clone().or_else(crate::store::open_existing_db)
    }

    fn rows(&self) -> Readiness<Vec<SessionSummary>> {
        let Some(db) = self.open_db() else {
            return Readiness::Unavailable;
        };
        let Ok(conversations) = ConversationService::new(&db).list() else {
            return Readiness::Unavailable;
        };
        let live = crate::run::active_conversation_id();
        let mut out = Vec::new();
        for conversation in conversations {
            // Run/step read paths supply the persisted status. A conversation
            // with no run has no status to show and is omitted, not invented.
            let Ok(runs) = AgentRunRepository::new(&db).list_runs_by_conversation(conversation.id)
            else {
                return Readiness::Unavailable;
            };
            let Some(newest) = runs.first() else {
                continue;
            };
            let Some(status) = SessionStatus::from_persisted(&newest.status) else {
                continue;
            };
            out.push(SessionSummary {
                // Integer rowids, rendered as decimal strings: no re-labelling.
                id: conversation.id.to_string(),
                title: sanitize(&conversation.title),
                status,
                age_label: age_label(conversation.updated_at),
                active: live == Some(conversation.id),
            });
        }
        Readiness::Ready(out)
    }
}

impl SessionSource for RuntimeSessionSource {
    fn list(&self) -> Readiness<Vec<SessionSummary>> {
        self.rows()
    }

    fn search(&self, query: &str) -> Readiness<Vec<SessionSummary>> {
        let rows = match self.rows() {
            Readiness::Ready(rows) => rows,
            Readiness::Unavailable => return Readiness::Unavailable,
        };
        let query = query.trim();
        if query.is_empty() {
            return Readiness::Ready(rows);
        }
        let Some(db) = self.open_db() else {
            return Readiness::Unavailable;
        };
        let Ok(results) = LocalSearchService::new(&db).search(query) else {
            return Readiness::Unavailable;
        };
        // A message match points at its owning conversation; the FTS service
        // already sanitised query syntax, and the rows here are persisted
        // facts only.
        let mut ids: Vec<i64> = results
            .conversations
            .into_iter()
            .map(|conversation| conversation.id)
            .collect();
        ids.extend(
            results
                .message_matches
                .into_iter()
                .map(|message| message.conversation_id),
        );
        ids.sort_unstable();
        ids.dedup();
        Readiness::Ready(
            rows.into_iter()
                .filter(|row| {
                    row.id
                        .parse::<i64>()
                        .is_ok_and(|id| ids.binary_search(&id).is_ok())
                })
                .collect(),
        )
    }
}

/// Diff backed by the read-only version-control service. The workspace is
/// captured once (tests inject a synthetic repository).
pub(crate) struct RuntimeDiffSource {
    workspace: PathBuf,
}

impl RuntimeDiffSource {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_workspace(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

/// Map the byte-capped, secret-redacted unified diff text onto render DTOs.
/// Every line passes through `sanitize` again as defence in depth.
fn parse_diff(files: &[String], text: &str) -> DiffSnapshot {
    let mut by_path: Vec<FileDiff> = files
        .iter()
        .map(|path| FileDiff {
            path: sanitize(path),
            plus: 0,
            minus: 0,
            hunks: Vec::new(),
            touched: true,
        })
        .collect();
    // A single-file diff may omit the `+++ b/` header; still attribute its
    // hunks to that file rather than dropping them.
    let mut current = (by_path.len() == 1).then(|| by_path[0].path.clone());
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current = Some(sanitize(path));
            continue;
        }
        if line.starts_with("diff --git ") || line.starts_with("--- ") {
            continue;
        }
        if line.starts_with("@@") {
            if let Some(index) = current
                .as_ref()
                .and_then(|path| by_path.iter().position(|file| &file.path == path))
            {
                by_path[index].hunks.push(Hunk {
                    header: sanitize(line),
                    lines: Vec::new(),
                });
            }
            continue;
        }
        let (kind, body) = if let Some(body) = line.strip_prefix('+') {
            (DiffLineKind::Add, body)
        } else if let Some(body) = line.strip_prefix('-') {
            (DiffLineKind::Remove, body)
        } else {
            (
                DiffLineKind::Context,
                line.strip_prefix(' ').unwrap_or(line),
            )
        };
        if let Some(file) = current
            .as_ref()
            .and_then(|path| by_path.iter_mut().find(|file| &file.path == path))
        {
            if let Some(hunk) = file.hunks.last_mut() {
                match kind {
                    DiffLineKind::Add => file.plus = file.plus.saturating_add(1),
                    DiffLineKind::Remove => file.minus = file.minus.saturating_add(1),
                    DiffLineKind::Context => {}
                }
                hunk.lines.push(DiffLine {
                    kind,
                    text: sanitize(body),
                });
            }
        }
    }
    DiffSnapshot { files: by_path }
}

impl DiffSource for RuntimeDiffSource {
    fn current(&self) -> Readiness<DiffSnapshot> {
        match git_prepare_review_diff(&self.workspace, "working", None) {
            Ok(diff) => Readiness::Ready(parse_diff(&diff.files, &diff.diff)),
            Err(_) => Readiness::Unavailable,
        }
    }
}

/// Approval cards read from the live run inspector. Never resolves a park.
pub(crate) struct RuntimeApprovalView {
    /// Test-only in-memory override; `None` in the shipped binary.
    db: Option<Database>,
}

impl RuntimeApprovalView {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { db: None }
    }
}

fn risk_for(name: &str, shell: bool) -> RiskClass {
    if shell || name == "execute_command" {
        return RiskClass::DangerousShell;
    }
    if matches!(
        name,
        "write_file" | "edit_file" | "apply_patch" | "move_file" | "delete_file"
    ) {
        return RiskClass::Mutating;
    }
    RiskClass::ReadOnly
}

fn json_string(arguments: &str, key: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Map one live approval snapshot onto the render DTO. All strings are
/// sanitised; the call id is the server-owned identity the A/D path resolves.
fn approval_from_snapshot(
    pending: &nexora_runtime::application::agent::approval::PendingApprovalSnapshot,
) -> ApprovalRequest {
    let risk = risk_for(&pending.name, pending.metadata.shell.is_some());
    let command =
        json_string(&pending.arguments, "command").unwrap_or_else(|| pending.name.clone());
    let cwd = json_string(&pending.arguments, "cwd").unwrap_or_default();
    ApprovalRequest {
        id: sanitize(&pending.call_id),
        command: sanitize(&command),
        cwd: sanitize(&cwd),
        risk,
        reason: sanitize(&format!(
            "{} tool awaits {} consent",
            pending.name,
            risk.label()
        )),
    }
}

impl ApprovalView for RuntimeApprovalView {
    fn pending(&self) -> Readiness<Option<ApprovalRequest>> {
        let Some(db) = self.db.clone().or_else(crate::store::open_existing_db) else {
            return Readiness::Unavailable;
        };
        let Some(conversation_id) = crate::run::active_conversation_id() else {
            return Readiness::Ready(None);
        };
        let registry = crate::run::registry();
        let Ok(live) = live_run_state(&db, registry.as_ref(), conversation_id) else {
            return Readiness::Unavailable;
        };
        Readiness::Ready(
            live.and_then(|state| state.pending_approval)
                .as_ref()
                .map(approval_from_snapshot),
        )
    }
}

/// Best-effort OS clipboard. Any failure is a plain `false`, never a diagnostic
/// containing clipboard contents or an OS error.
pub(crate) struct OsClipboardSink;

impl ClipboardSink for OsClipboardSink {
    fn copy(&self, text: &str) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        clipboard.set_text(text.to_string()).is_ok()
    }
}

// ---- command catalog (real Clap grammar) ---------------------------------

/// A local action that genuinely runs inside this TUI (no backend required).
#[must_use]
pub(crate) fn ui_action(path: &str, label: &str, about: &str) -> CommandSpec {
    CommandSpec {
        path: path.to_string(),
        label: label.to_string(),
        about: about.to_string(),
        origin: CommandOrigin::Ui,
        availability: Availability::Available,
    }
}

/// Walk the real Clap tree once. The returned paths are the subcommand grammar
/// that `nexora --help` exposes; argument ids are appended to `about` so a test
/// can prove the walk visited `get_arguments()` too. All strings are owned and
/// sanitised. `run` is genuinely wired to the composer, so it is available;
/// the remaining shell-only commands carry an honest unavailable reason.
#[must_use]
pub(crate) fn clap_commands() -> Vec<CommandSpec> {
    fn walk(command: &clap::Command, prefix: &str, out: &mut Vec<CommandSpec>) {
        let name = command.get_name().to_string();
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        let about = command
            .get_about()
            .map_or_else(String::new, std::string::ToString::to_string);
        let args: Vec<String> = command
            .get_arguments()
            .map(|arg| arg.get_id().as_str().to_string())
            .collect();
        let about = if args.is_empty() {
            about
        } else {
            format!("{about} [args: {}]", args.join(" "))
        };
        let availability = if path == "run" {
            Availability::Available
        } else {
            Availability::Unavailable(
                "not available from the TUI in this build; run this command from a shell"
                    .to_string(),
            )
        };
        out.push(CommandSpec {
            path: sanitize(&path),
            label: sanitize(&name),
            about: sanitize(&about),
            origin: CommandOrigin::Cli,
            availability,
        });
        for child in command.get_subcommands() {
            // Auto-generated help/version are not user commands in the catalog.
            if child.get_name() == "help" {
                continue;
            }
            walk(child, &path, out);
        }
    }

    let mut out = Vec::new();
    // The root command has no subcommand path of its own; start with children.
    let root = crate::Cli::command();
    for child in root.get_subcommands() {
        if child.get_name() == "help" {
            continue;
        }
        walk(child, "", &mut out);
    }
    out
}

/// Local palette section: the only commands that can actually be run here.
#[must_use]
pub(crate) fn local_actions() -> Vec<CommandSpec> {
    vec![
        ui_action(
            "ui.sessions",
            "Open sessions",
            "browse session history (opens a view; no agent is started)",
        ),
        ui_action(
            "ui.diff",
            "View diff",
            "inspect the current diff (view only; never applies changes)",
        ),
        ui_action("ui.help", "Help", "show keybindings and local actions"),
        ui_action(
            "ui.focus_timeline",
            "Focus timeline",
            "move keyboard focus to the activity timeline",
        ),
    ]
}

pub(crate) struct ClapCommandCatalog;

impl CommandCatalog for ClapCommandCatalog {
    fn commands(&self) -> Readiness<Vec<CommandSpec>> {
        let mut out = local_actions();
        out.extend(clap_commands());
        Readiness::Ready(out)
    }
}

// ---- injected bundle -----------------------------------------------------

/// All sources used by the main loop. `Sources::shipped()` is the only
/// implementation reachable from `main`; `Sources::scripted()` is `cfg(test)`.
pub(crate) struct Sources {
    sessions: Box<dyn SessionSource>,
    diff: Box<dyn DiffSource>,
    approval: Box<dyn ApprovalView>,
    commands: Box<dyn CommandCatalog>,
    clipboard: Box<dyn ClipboardSink>,
}

impl Sources {
    #[must_use]
    pub(crate) fn shipped() -> Self {
        Self {
            sessions: Box::new(RuntimeSessionSource::new()),
            diff: Box::new(RuntimeDiffSource::new()),
            approval: Box::new(RuntimeApprovalView::new()),
            commands: Box::new(ClapCommandCatalog),
            clipboard: Box::new(OsClipboardSink),
        }
    }

    pub(crate) fn load_sessions(&self) -> Readiness<Vec<SessionSummary>> {
        self.sessions.list()
    }

    pub(crate) fn search_sessions(&self, query: &str) -> Readiness<Vec<SessionSummary>> {
        self.sessions.search(query)
    }

    pub(crate) fn load_diff(&self) -> Readiness<DiffSnapshot> {
        self.diff.current()
    }

    pub(crate) fn load_approval(&self) -> Readiness<Option<ApprovalRequest>> {
        self.approval.pending()
    }

    pub(crate) fn commands(&self) -> Vec<CommandSpec> {
        match self.commands.commands() {
            Readiness::Ready(commands) => commands,
            Readiness::Unavailable => Vec::new(),
        }
    }

    pub(crate) fn copy(&self, text: &str) -> bool {
        self.clipboard.copy(text)
    }
}

// ---- cfg(test) injection: never reachable from the shipped binary --------

#[cfg(test)]
pub(crate) use scripts::{
    ScriptedApprovalView, ScriptedClipboardSink, ScriptedDiffSource, ScriptedSessionSource,
};

#[cfg(test)]
impl Sources {
    /// Test-only bundle: `None` keeps a source honestly unavailable, so a
    /// single test can mix available and absent projections. The nested
    /// option is intentional: available-with-no-pending differs from absent.
    #[allow(clippy::option_option)]
    #[must_use]
    pub(crate) fn scripted(
        sessions: Option<Vec<SessionSummary>>,
        diff: Option<DiffSnapshot>,
        approval: Option<Option<ApprovalRequest>>,
        copy_ok: bool,
    ) -> Self {
        let sessions: Box<dyn SessionSource> = match sessions {
            Some(list) => Box::new(ScriptedSessionSource { sessions: list }),
            None => Box::new(UnavailableSessionSource),
        };
        let diff: Box<dyn DiffSource> = match diff {
            Some(snapshot) => Box::new(ScriptedDiffSource { snapshot }),
            None => Box::new(UnavailableDiffSource),
        };
        let approval: Box<dyn ApprovalView> = match approval {
            Some(request) => Box::new(ScriptedApprovalView { request }),
            None => Box::new(UnavailableApprovalView),
        };
        Self {
            sessions,
            diff,
            approval,
            commands: Box::new(ClapCommandCatalog),
            clipboard: Box::new(ScriptedClipboardSink { ok: copy_ok }),
        }
    }
}

#[cfg(test)]
mod scripts {
    use super::*;

    pub(crate) struct ScriptedSessionSource {
        pub(crate) sessions: Vec<SessionSummary>,
    }

    impl SessionSource for ScriptedSessionSource {
        fn list(&self) -> Readiness<Vec<SessionSummary>> {
            Readiness::Ready(
                self.sessions
                    .iter()
                    .map(|s| SessionSummary {
                        id: sanitize(&s.id),
                        title: sanitize(&s.title),
                        status: s.status,
                        age_label: sanitize(&s.age_label),
                        active: s.active,
                    })
                    .collect(),
            )
        }
    }

    pub(crate) struct ScriptedDiffSource {
        pub(crate) snapshot: DiffSnapshot,
    }

    fn clean_diff(diff: &DiffSnapshot) -> DiffSnapshot {
        DiffSnapshot {
            files: diff
                .files
                .iter()
                .map(|f| FileDiff {
                    path: sanitize(&f.path),
                    plus: f.plus,
                    minus: f.minus,
                    touched: f.touched,
                    hunks: f
                        .hunks
                        .iter()
                        .map(|h| Hunk {
                            header: sanitize(&h.header),
                            lines: h
                                .lines
                                .iter()
                                .map(|l| DiffLine {
                                    kind: l.kind,
                                    text: sanitize(&l.text),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    impl DiffSource for ScriptedDiffSource {
        fn current(&self) -> Readiness<DiffSnapshot> {
            Readiness::Ready(clean_diff(&self.snapshot))
        }
    }

    pub(crate) struct ScriptedApprovalView {
        pub(crate) request: Option<ApprovalRequest>,
    }

    impl ApprovalView for ScriptedApprovalView {
        fn pending(&self) -> Readiness<Option<ApprovalRequest>> {
            Readiness::Ready(self.request.as_ref().map(|r| ApprovalRequest {
                id: sanitize(&r.id),
                command: sanitize(&r.command),
                cwd: sanitize(&r.cwd),
                risk: r.risk,
                reason: sanitize(&r.reason),
            }))
        }
    }

    pub(crate) struct ScriptedClipboardSink {
        pub(crate) ok: bool,
    }

    impl ClipboardSink for ScriptedClipboardSink {
        fn copy(&self, _text: &str) -> bool {
            self.ok
        }
    }
}

#[cfg(test)]
mod real_source_tests {
    use std::path::Path;

    use super::*;
    use nexora_core::storage::database::in_memory_database;
    use nexora_runtime::application::agent::access::AccessProfile;
    use nexora_runtime::application::agent::approval::{ApprovalMetadata, PendingApprovalSnapshot};
    use nexora_runtime::application::agent::shell_policy::{ShellAnalysis, ShellCategory};

    const SECRET: &str = "sk-live-sentinel-0000000000000001";

    #[test]
    fn real_session_source_maps_only_persisted_run_rows() {
        let db = in_memory_database();
        let conversation = ConversationService::new(&db)
            .create("Alpha session")
            .expect("conversation");
        AgentRunRepository::new(&db)
            .create_run(Some(conversation), "test-model", "supervised")
            .expect("run row");

        let source = RuntimeSessionSource::with_db(db);
        let Readiness::Ready(rows) = source.list() else {
            panic!("real source must be ready over a migrated store");
        };
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, conversation.to_string(), "integer rowid, unchanged");
        assert_eq!(row.title, "Alpha session");
        assert_eq!(row.status, SessionStatus::Running);
        assert!(!row.age_label.is_empty());
        // `active` is supplied by the live-run table, never inferred from the
        // stored status; this test does not pin a particular global value.

        // Empty query is the full list; a query never fabricates a row.
        assert_eq!(source.search(""), source.list());
        for query in ["Alpha", "missing"] {
            if let Readiness::Ready(found) = source.search(query) {
                assert!(found.iter().all(|candidate| rows.contains(candidate)));
            } else {
                panic!("search over an available store must stay available");
            }
        }
    }

    #[test]
    fn real_approval_mapping_uses_server_identity_and_redacts() {
        let pending = PendingApprovalSnapshot {
            call_id: "approval-42".to_string(),
            name: "execute_command".to_string(),
            arguments: format!(r#"{{"command":"echo {SECRET}","cwd":"/work/project"}}"#),
            timeout_secs: 600,
            requested_at: 1,
            expires_at: 601,
            metadata: ApprovalMetadata {
                access_profile: AccessProfile::Standard,
                sandbox_posture: "per-command-consent",
                checkpoints_available: false,
                allow_for_run: false,
                sensitive_write: true,
                shell: Some(ShellAnalysis {
                    category: ShellCategory::GeneralShell,
                    packages: Vec::new(),
                    packages_exact: false,
                    uncertain: false,
                    sandbox_posture: "per-command-consent",
                    long_running: false,
                    long_running_pattern: None,
                }),
            },
        };
        let request = approval_from_snapshot(&pending);
        assert_eq!(request.id, "approval-42", "server-owned identity");
        assert_eq!(request.cwd, "/work/project");
        assert_eq!(request.risk, RiskClass::DangerousShell);
        assert!(request.reason.contains("DangerousShell"));
        assert!(!request.command.contains(SECRET), "command was redacted");
        assert!(!request.id.contains('\u{1b}'));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git must be available for the synthetic repo test");
        assert!(
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    #[test]
    fn real_diff_source_reads_a_synthetic_repo_without_applying_changes() {
        let dir = std::env::temp_dir().join(format!(
            "nexora-cli-diff-{}-{}",
            std::process::id(),
            "working"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("synthetic repo");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "test@example.invalid"]);
        git(&dir, &["config", "user.name", "Test"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\n").expect("initial file");
        git(&dir, &["add", "a.txt"]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        let commits_before = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-list", "--count", "HEAD"])
                .current_dir(&dir)
                .output()
                .expect("git rev-list")
                .stdout,
        )
        .trim()
        .to_string();
        std::fs::write(&file, "one\ntwo\nthree\n").expect("worktree change");

        let source = RuntimeDiffSource::with_workspace(dir.clone());
        let Readiness::Ready(snapshot) = source.current() else {
            panic!("real diff source must be ready over a git worktree");
        };
        assert_eq!(snapshot.files.len(), 1);
        let file_diff = &snapshot.files[0];
        assert_eq!(file_diff.path, "a.txt");
        assert!(file_diff.touched);
        assert!(file_diff.plus >= 1);
        assert!(file_diff.hunks.iter().any(|hunk| hunk
            .lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Add && line.text == "three")));

        // Viewing is read-only: neither the file nor the commit graph changed.
        assert_eq!(
            std::fs::read_to_string(&file).expect("file"),
            "one\ntwo\nthree\n"
        );
        let commits_after = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-list", "--count", "HEAD"])
                .current_dir(&dir)
                .output()
                .expect("git rev-list")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(commits_before, commits_after, "no commit from a diff view");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_text_parsing_sanitizes_every_line() {
        let text = format!("@@ -1 +1 @@\n-old\n+token = {SECRET}\n context\n");
        let snapshot = parse_diff(&["a.txt".to_string()], &text);
        let rendered = format!("{snapshot:?}");
        assert!(!rendered.contains(SECRET));
        let file = &snapshot.files[0];
        assert_eq!(file.plus, 1);
        assert_eq!(file.minus, 1);
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(file.hunks[0].lines.len(), 3);
    }
}
