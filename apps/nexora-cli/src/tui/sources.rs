//! Read-only projection sources for the phase-6a TUI depth scaffold (#165).
//!
//! The shipped binary contains **no backend data**: sessions, diffs and pending
//! approvals all report [`Readiness::Unavailable`], and the clipboard sink
//! returns `false`. Only `#[cfg(test)]` scripts ever supply projections, mirroring
//! the `RunHost`/`ScriptedRunHost` seam in `crate::run`. The command catalog is
//! the one exception: it is derived from the real Clap tree below, so the palette
//! shares exactly one grammar with `--help` (brief §6.8, plan §6 P6).
//!
//! Every string entering a projection is passed through the existing
//! [`sanitize`](super::app::sanitize) helper (control-character stripping +
//! secret redaction). No source here opens `SQLite`, git, the network, the keyring
//! or a file: they are pure, read-only adapters.

#![allow(clippy::module_name_repetitions)]

use clap::CommandFactory;

use super::app::sanitize;

/// A backend answer that is either real (owned) or honestly absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness<T> {
    Ready(T),
    Unavailable,
}

/// Persisted run-status vocabulary only (`NEXORA-CLI-PLAN.md` §4). The TUI must
/// not invent transitional states such as `paused` for a stored session.
// The projection vocabulary is consumed by the renderer, but in this UI-only
// stage only `#[cfg(test)]` scripts ever construct these variants.
#[allow(dead_code)]
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
// Only `#[cfg(test)]` approval scripts construct a risk class in this stage.
#[allow(dead_code)]
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
// Only `#[cfg(test)]` diff scripts construct diff lines in this stage.
#[allow(dead_code)]
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

/// Availability of a palette entry. Backend/CLI actions carried by the TUI are
/// disabled with a fixed reason; local UI actions are available.
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
    /// `true` only when a real copy succeeded. The shipped sink returns `false`.
    fn copy(&self, text: &str) -> bool;
}

// ---- shipped implementations (no backend data) --------------------------

pub(crate) struct UnavailableSessionSource;

impl SessionSource for UnavailableSessionSource {
    fn list(&self) -> Readiness<Vec<SessionSummary>> {
        Readiness::Unavailable
    }
}

pub(crate) struct UnavailableDiffSource;

impl DiffSource for UnavailableDiffSource {
    fn current(&self) -> Readiness<DiffSnapshot> {
        Readiness::Unavailable
    }
}

pub(crate) struct UnavailableApprovalView;

impl ApprovalView for UnavailableApprovalView {
    fn pending(&self) -> Readiness<Option<ApprovalRequest>> {
        Readiness::Unavailable
    }
}

pub(crate) struct UnavailableClipboardSink;

impl ClipboardSink for UnavailableClipboardSink {
    fn copy(&self, _text: &str) -> bool {
        false
    }
}

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
/// sanitised; entries carry an honest "not available from the TUI" reason.
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
        out.push(CommandSpec {
            path: sanitize(&path),
            label: sanitize(&name),
            about: sanitize(&about),
            origin: CommandOrigin::Cli,
            availability: Availability::Unavailable(
                "not available from the TUI in this build; run this command from a shell"
                    .to_string(),
            ),
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
            sessions: Box::new(UnavailableSessionSource),
            diff: Box::new(UnavailableDiffSource),
            approval: Box::new(UnavailableApprovalView),
            commands: Box::new(ClapCommandCatalog),
            clipboard: Box::new(UnavailableClipboardSink),
        }
    }

    pub(crate) fn load_sessions(&self) -> Readiness<Vec<SessionSummary>> {
        self.sessions.list()
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
