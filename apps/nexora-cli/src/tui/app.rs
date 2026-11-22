//! All TUI state in one `App`, advanced by `apply(UiEvent) -> LoopCmd`.
//!
//! The app is pure with respect to I/O: it never reads a terminal, spawns a
//! thread or calls a host. The main loop feeds it events and executes the
//! returned [`LoopCmd`]. Every state that survives a resize, overlay or error
//! (draft, scroll, focus, selection, overlay coordinates) lives here, never in
//! the renderer. Run state is derived *only* from real host frames — it is
//! never guessed. Backend-backed overlays hold only projections delivered by
//! [`UiEvent::SessionsLoaded`]/[`UiEvent::DiffLoaded`]/[`UiEvent::ApprovalLoaded`];
//! the shipped sources report `Unavailable`, so no data is fabricated here.

use std::path::Path;

use nexora_core::error::CliExitCode;

use crate::error::redact_value;
use crate::run::WireFrame;

use super::capabilities::Capabilities;
use super::composer::{Composer, Vertical};
use super::events::{Key, UiEvent};
use super::layout::TIMELINE_MIN;
use super::sources::{clap_commands, local_actions, Availability, CommandSpec, SessionSummary};

/// Where keyboard navigation currently acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Composer,
    Timeline,
}

/// The five-state status vocabulary from the brief (§5.3). Each is reachable
/// only through a real frame; `Idle` is the honest starting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    Idle,
    Running,
    Paused,
    WaitingApproval,
    Failed,
}

impl RunState {
    /// The exact, short status token shown in the status line.
    #[must_use]
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Running => "RUNNING",
            Self::Paused => "PAUSED",
            Self::WaitingApproval => "WAITING APPROVAL",
            Self::Failed => "FAILED",
        }
    }
}

/// Optional overlays. Opening/closing one never touches the run, draft or
/// viewport (brief §8.1 Focus Memory). Approvals are an INLINE timeline block,
/// deliberately not an overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    None,
    Help,
    Sessions,
    Diff,
    Palette,
}

/// A local, non-fabricated decision marker for the inline approval block.
/// `Pending` is the only state in which `A`/`D` are accepted; `Esc` always
/// leaves it pending and never resolves an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    Pending,
    Approved,
    Denied,
}

/// The small set of extra context lines drawn above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    User,
    Event,
    Terminal,
    Error,
}

/// One rendered timeline row, built from a whitelisted subset of a frame or
/// from a local, honest notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineEntry {
    pub(crate) kind: EntryKind,
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
}

impl TimelineEntry {
    fn new(kind: EntryKind, label: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            detail,
        }
    }
}

/// A rendered row after the density guard. `Folded` is a *view* of a run of
/// identical tool events; the original entries stay in `App::timeline`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderRow {
    Entry(usize, TimelineEntry),
    Folded {
        anchor: usize,
        label: String,
        count: usize,
    },
}

/// Run of identical tool events at which folding starts (SET: implementer
/// picks the concrete threshold and pins it with a test, plan §8).
pub(crate) const FOLD_THRESHOLD: usize = 3;

fn foldable(entry: &TimelineEntry) -> bool {
    matches!(entry.kind, EntryKind::Event)
        && entry.label.starts_with("tool_")
        && !entry.label.contains("approval")
}

/// Pure event-density guard (brief §8.3 F). Consecutive identical tool events
/// become one counted row unless their anchor is expanded. Errors, approvals,
/// terminal rows and user rows are never folded; order and counters survive.
#[must_use]
pub(crate) fn fold(
    timeline: &[TimelineEntry],
    expanded: &[usize],
    threshold: usize,
) -> Vec<RenderRow> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < timeline.len() {
        if foldable(&timeline[i]) {
            let mut j = i + 1;
            while j < timeline.len() && timeline[j] == timeline[i] {
                j += 1;
            }
            if j - i >= threshold && !expanded.contains(&i) {
                out.push(RenderRow::Folded {
                    anchor: i,
                    label: timeline[i].label.clone(),
                    count: j - i,
                });
            } else {
                for (offset, entry) in timeline[i..j].iter().enumerate() {
                    out.push(RenderRow::Entry(i + offset, entry.clone()));
                }
            }
            i = j;
        } else {
            out.push(RenderRow::Entry(i, timeline[i].clone()));
            i += 1;
        }
    }
    out
}

/// Normalised event types whose raw `type` string may be shown. Anything else
/// becomes the fixed `Event` label (brief §7.2, "unknown types → fixed label").
const KNOWN_TYPES: &[&str] = &[
    "run_started",
    "run_finished",
    "run_completed",
    "run_paused",
    "run_resumed",
    "run_cancelled",
    "run_failed",
    "assistant_message",
    "assistant_progress",
    "model_response",
    "final_response",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_failed",
    "approval_required",
    "approval_approved",
    "approval_denied",
    "step_budget_exhausted",
    "spend_limit_exceeded",
    "changed_files",
    "diff_summary",
    "provider_error",
    "network_error",
    "auth_error",
];

/// Fields a host frame is allowed to contribute to a timeline row. All other
/// fields (and all secrets) are discarded.
const DETAIL_FIELDS: &[&str] = &[
    "tool", "name", "path", "file", "command", "summary", "message", "detail", "result",
];

/// Remove control characters (including ANSI ESC) and redact key-shaped bytes
/// before any host-supplied string reaches the render buffer (brief §11/§18).
/// Shared with `sources` so *every* projection string passes one gate.
#[must_use]
pub(crate) fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let redacted = redact_value(&cleaned);
    redacted.chars().take(200).collect()
}

fn is_terminal_type(event_type: &str, status: Option<&str>) -> bool {
    matches!(event_type, "run_finished" | "run_completed") || status == Some("completed")
}

/// Derive a run state from one real frame. `None` means the frame carries no
/// lifecycle information and must not change the displayed state.
fn state_from_frame(frame: &WireFrame) -> Option<RunState> {
    let event_type = frame.get("type").and_then(serde_json::Value::as_str)?;
    let status = frame.get("status").and_then(serde_json::Value::as_str);
    match event_type {
        "run_started" | "run_resumed" => Some(RunState::Running),
        "run_paused" => Some(RunState::Paused),
        "approval_required" => Some(RunState::WaitingApproval),
        "run_finished" | "run_completed" => Some(RunState::Idle),
        "run_cancelled" | "run_failed" | "provider_error" | "network_error" | "auth_error"
            if status != Some("completed") =>
        {
            Some(RunState::Failed)
        }
        _ => None,
    }
}

/// Build one timeline row from a real frame, using only whitelisted fields.
fn entry_from_frame(frame: &WireFrame) -> TimelineEntry {
    let raw_type = frame
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let status = frame.get("status").and_then(serde_json::Value::as_str);
    let label = if KNOWN_TYPES.contains(&raw_type) {
        raw_type.to_string()
    } else {
        "Event".to_string()
    };
    let detail = DETAIL_FIELDS
        .iter()
        .find_map(|key| frame.get(key).and_then(serde_json::Value::as_str))
        .map(sanitize);
    if is_terminal_type(raw_type, status) {
        TimelineEntry::new(EntryKind::Terminal, "Completed", detail)
    } else {
        TimelineEntry::new(EntryKind::Event, label, detail)
    }
}

/// Turn a git `HEAD` file into a branch name, or `None` for a detached HEAD.
#[must_use]
pub(crate) fn parse_git_head(contents: &str) -> Option<String> {
    let trimmed = contents.trim();
    let branch = trimmed.strip_prefix("ref: refs/heads/")?;
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

/// Walk up from `start` looking for a readable `.git/HEAD` (real branch only).
fn detect_git_branch(start: &Path) -> Option<String> {
    let mut candidate = Some(start);
    for _ in 0..8 {
        let dir = candidate?;
        if let Ok(contents) = std::fs::read_to_string(dir.join(".git/HEAD")) {
            if let Some(branch) = parse_git_head(&contents) {
                return Some(sanitize(&branch));
            }
        }
        candidate = dir.parent();
    }
    None
}

/// Shorten a workspace path to its final component: never print a full path.
#[must_use]
pub(crate) fn short_workspace(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    if name.is_empty() {
        "workspace".to_string()
    } else {
        sanitize(name)
    }
}

/// The read-only facts shown in the status line. Built once on entry and never
/// guessed afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusSnapshot {
    pub(crate) workspace: String,
    pub(crate) git_branch: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) autonomy: String,
}

impl StatusSnapshot {
    /// Collect real facts from the cwd and the shared settings store. Missing
    /// or unreadable facts become `None`/defaults — never invented values.
    #[must_use]
    pub(crate) fn detect() -> Self {
        let cwd = std::env::current_dir().ok();
        let workspace = cwd
            .as_deref()
            .and_then(|path| path.to_str())
            .map_or_else(|| "workspace".to_string(), short_workspace);
        let git_branch = cwd.as_deref().and_then(detect_git_branch);
        let read = |key: &str| crate::store::read(key).ok().flatten().map(|v| sanitize(&v));
        let provider = read(nexora_core::config::SELECTED_PROVIDER_KEY);
        let model = read(nexora_core::config::SELECTED_MODEL_KEY);
        let autonomy = read(nexora_core::config::AUTONOMY_KEY)
            .unwrap_or_else(|| nexora_core::catalog::DEFAULT_AUTONOMY.to_string());
        Self {
            workspace,
            git_branch,
            provider,
            model,
            autonomy,
        }
    }
}

/// What the main loop should do after an event has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopCmd {
    Continue,
    /// Submit the draft to the host seam (spawns the run thread once).
    Submit(String),
    /// Request cancellation through the host seam (repeat presses ignored).
    RequestCancel,
    /// Leave the TUI with the last command's outcome.
    Quit(CliExitCode),
    /// Query the sessions source; the loop applies `SessionsLoaded`.
    LoadSessions,
    /// Query the runtime search service for the sessions filter.
    SearchSessions(String),
    /// Ask the core to resolve the parked approval (A/D). Never local-only.
    RespondApproval {
        id: String,
        approved: bool,
    },
    /// Ask the core to pause the active run (only core frames change state).
    RequestPause,
    /// Ask the core to resume the active run (only core frames change state).
    RequestResume,
    /// Query the diff source; the loop applies `DiffLoaded`.
    LoadDiff,
    /// Query the approval source; the loop applies `ApprovalLoaded`.
    LoadApproval,
    /// Ask the clipboard sink to copy; the loop applies `CopyResult`.
    Copy(String),
    /// A palette entry was chosen (local effect already applied by `App`).
    RunCommand(String),
    /// A session row was chosen; NO run is started by this action.
    OpenSession(String),
}

/// The complete TUI state. The booleans are independent interaction flags
/// (cancel feedback, exit confirmation, run/draft lifecycle), not a state
/// machine that can be collapsed into one enum without losing honesty.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(crate) struct App {
    pub(crate) caps: Capabilities,
    pub(crate) snapshot: StatusSnapshot,
    pub(crate) state: RunState,
    pub(crate) timeline: Vec<TimelineEntry>,
    /// Index of the top visible timeline row; `usize::MAX` means follow tail.
    pub(crate) scroll: usize,
    pub(crate) scroll_held: bool,
    pub(crate) composer: Composer,
    pub(crate) focus: Focus,
    pub(crate) overlay: Overlay,
    pub(crate) transient: Option<String>,
    pub(crate) cancel_pending: bool,
    pub(crate) exit_confirm: bool,
    pub(crate) run_active: bool,
    pub(crate) submit_pending: bool,
    pub(crate) size: (u16, u16),
    pub(crate) dirty: bool,
    pub(crate) last_exit: CliExitCode,
    transient_ticks: u32,

    // Focus memory: saved when an overlay opens, restored when it closes.
    pub(crate) focus_before_overlay: Focus,
    pub(crate) scroll_before_overlay: usize,
    pub(crate) scroll_held_before_overlay: bool,

    // Sessions overlay state (all coordinates are overlay-local).
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) sessions_filter: String,
    pub(crate) sessions_selection: usize,
    pub(crate) sessions_delete_armed: bool,
    pub(crate) sessions_unavailable: bool,

    // Diff overlay state; coordinates survive close and are restored on reopen.
    pub(crate) diff: Option<super::sources::DiffSnapshot>,
    pub(crate) diff_file: usize,
    pub(crate) diff_hunk: usize,
    pub(crate) diff_line: usize,
    pub(crate) diff_unavailable: bool,

    // Inline approval block (never an overlay).
    pub(crate) approval_request: Option<super::sources::ApprovalRequest>,
    pub(crate) approval_decision: ApprovalDecision,
    pub(crate) approval_inspect: bool,
    pub(crate) approval_return: Option<usize>,

    // View-only output freeze and density guard.
    pub(crate) output_frozen: bool,
    pub(crate) frozen_at_len: usize,
    pub(crate) held_at_len: usize,
    pub(crate) expanded: Vec<usize>,
    pub(crate) copied_label: bool,

    // Command palette state; `recent` is in-memory, bounded and secret-free.
    pub(crate) palette_filter: String,
    pub(crate) palette_commands: Vec<CommandSpec>,
    pub(crate) palette_selection: usize,
    pub(crate) palette_recent: Vec<String>,
}

/// Maximum number of in-memory recent palette entries (plan §8, bounded).
const PALETTE_RECENT_MAX: usize = 8;

impl App {
    /// Build the initial idle app focused on the composer. Palette commands
    /// come from the real Clap tree plus local actions (one grammar).
    #[must_use]
    pub(crate) fn new(caps: Capabilities, snapshot: StatusSnapshot, cols: u16, rows: u16) -> Self {
        let mut palette_commands = local_actions();
        palette_commands.extend(clap_commands());
        Self {
            caps,
            snapshot,
            state: RunState::Idle,
            timeline: Vec::new(),
            scroll: usize::MAX,
            scroll_held: false,
            composer: Composer::new(),
            focus: Focus::Composer,
            overlay: Overlay::None,
            transient: None,
            cancel_pending: false,
            exit_confirm: false,
            run_active: false,
            submit_pending: false,
            size: (cols, rows),
            dirty: true,
            last_exit: CliExitCode::Ok,
            transient_ticks: 0,
            focus_before_overlay: Focus::Composer,
            scroll_before_overlay: usize::MAX,
            scroll_held_before_overlay: false,
            sessions: Vec::new(),
            sessions_filter: String::new(),
            sessions_selection: 0,
            sessions_delete_armed: false,
            sessions_unavailable: false,
            diff: None,
            diff_file: 0,
            diff_hunk: 0,
            diff_line: 0,
            diff_unavailable: false,
            approval_request: None,
            approval_decision: ApprovalDecision::Pending,
            approval_inspect: false,
            approval_return: None,
            output_frozen: false,
            frozen_at_len: 0,
            held_at_len: 0,
            expanded: Vec::new(),
            copied_label: false,
            palette_filter: String::new(),
            palette_commands,
            palette_selection: 0,
            palette_recent: Vec::new(),
        }
    }

    /// Replace the palette catalog from the loop's `sources().commands()`.
    pub(crate) fn set_command_specs(&mut self, commands: Vec<CommandSpec>) {
        self.palette_commands = commands;
        self.dirty = true;
    }

    /// Clamp the raw scroll to a concrete viewport height of the *visible*
    /// slice. When output is frozen the slice ends at `frozen_at_len`; clamping
    /// against the full timeline would push `skip()` past the frozen rows and
    /// render an empty viewport (the frozen content must stay readable).
    #[must_use]
    pub(crate) fn effective_scroll(&self, viewport_rows: usize) -> usize {
        let max = self.visible_len().saturating_sub(viewport_rows);
        self.scroll.min(max)
    }

    /// Number of timeline rows currently visible (frozen view included).
    #[must_use]
    pub(crate) fn visible_len(&self) -> usize {
        if self.output_frozen {
            self.frozen_at_len.min(self.timeline.len())
        } else {
            self.timeline.len()
        }
    }

    /// Frames appended since the current frozen/hold viewpoint.
    #[must_use]
    pub(crate) fn new_events_count(&self) -> usize {
        self.timeline.len().saturating_sub(self.visible_len())
    }

    fn follow_tail(&mut self) {
        self.scroll = usize::MAX;
        self.scroll_held = false;
        self.output_frozen = false;
    }

    fn engage_scroll_hold(&mut self) {
        if !self.scroll_held {
            self.held_at_len = self.timeline.len();
        }
        self.scroll_held = true;
    }

    fn set_transient(&mut self, message: &str) {
        self.transient = Some(sanitize(message));
        self.transient_ticks = 0;
        self.dirty = true;
    }

    fn scroll_by(&mut self, delta: isize) {
        if self.timeline.is_empty() {
            return;
        }
        let viewport = usize::from(TIMELINE_MIN);
        let max = self.timeline.len().saturating_sub(viewport);
        let current = self.effective_scroll(viewport);
        let step = delta.unsigned_abs();
        let next = if delta < 0 {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step)
        };
        let next = next.min(max);
        self.scroll = next;
        if next < max {
            self.engage_scroll_hold();
        }
        self.dirty = true;
    }

    fn open_overlay(&mut self, overlay: Overlay) {
        if self.overlay == Overlay::None {
            self.focus_before_overlay = self.focus;
            self.scroll_before_overlay = self.scroll;
            self.scroll_held_before_overlay = self.scroll_held;
        }
        self.overlay = overlay;
        self.dirty = true;
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.focus = self.focus_before_overlay;
        self.scroll = self.scroll_before_overlay;
        self.scroll_held = self.scroll_held_before_overlay;
        self.sessions_delete_armed = false;
        self.copied_label = false;
        self.dirty = true;
    }

    fn toggle_freeze(&mut self) {
        if self.output_frozen {
            self.output_frozen = false;
        } else {
            self.output_frozen = true;
            self.frozen_at_len = self.timeline.len();
        }
        self.dirty = true;
    }

    /// A/D only *request* a decision: the core owns the park and must confirm
    /// before the local view changes. Esc never reaches this path.
    fn decide(&mut self, decision: ApprovalDecision) -> LoopCmd {
        let Some(request) = self.approval_request.as_ref() else {
            return LoopCmd::Continue;
        };
        let id = request.id.clone();
        self.approval_inspect = false;
        self.dirty = true;
        LoopCmd::RespondApproval {
            id,
            approved: decision == ApprovalDecision::Approved,
        }
    }

    pub(crate) fn filtered_sessions(&self) -> Vec<SessionSummary> {
        let needle = self.sessions_filter.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| {
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.id.to_lowercase().contains(&needle)
                    || s.status.label().contains(&needle)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn filtered_commands(&self) -> Vec<CommandSpec> {
        let needle = self.palette_filter.trim().to_lowercase();
        self.palette_commands
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.label.to_lowercase().contains(&needle)
                    || c.path.to_lowercase().contains(&needle)
                    || c.about.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    fn file_count(&self) -> usize {
        self.diff.as_ref().map_or(0, |d| d.files.len())
    }

    fn hunk_count(&self) -> usize {
        self.diff
            .as_ref()
            .and_then(|d| d.files.get(self.diff_file))
            .map_or(0, |f| f.hunks.len())
    }

    fn line_count(&self) -> usize {
        self.diff
            .as_ref()
            .and_then(|d| d.files.get(self.diff_file))
            .and_then(|f| f.hunks.get(self.diff_hunk))
            .map_or(0, |h| h.lines.len())
    }

    fn run_palette_command(&mut self, cmd: CommandSpec) -> LoopCmd {
        self.close_overlay();
        // Paths are static grammar ids/UI ids — never user text or a secret.
        self.palette_recent.push(cmd.path.clone());
        while self.palette_recent.len() > PALETTE_RECENT_MAX {
            self.palette_recent.remove(0);
        }
        match cmd.availability {
            Availability::Unavailable(reason) => {
                self.set_transient(&format!("{}: {}", cmd.label, reason));
                LoopCmd::RunCommand(cmd.path)
            }
            Availability::Available => match cmd.path.as_str() {
                "ui.sessions" => {
                    self.open_overlay(Overlay::Sessions);
                    LoopCmd::LoadSessions
                }
                "ui.diff" => {
                    self.open_overlay(Overlay::Diff);
                    LoopCmd::LoadDiff
                }
                "ui.help" => {
                    self.open_overlay(Overlay::Help);
                    LoopCmd::RunCommand(cmd.path)
                }
                "ui.focus_timeline" => {
                    self.focus = Focus::Timeline;
                    LoopCmd::RunCommand(cmd.path)
                }
                // The real agent run is wired: the palette entry submits the
                // current composer draft through the same path as Enter.
                "run" if !self.composer.is_empty() && !self.submit_pending => {
                    self.submit_pending = true;
                    LoopCmd::Submit(self.composer.draft().to_string())
                }
                "run" => {
                    self.set_transient("Write a task in the composer first");
                    LoopCmd::RunCommand(cmd.path)
                }
                _ => LoopCmd::RunCommand(cmd.path),
            },
        }
    }

    fn sessions_key(&mut self, key: Key) -> LoopCmd {
        match key {
            Key::Esc => self.close_overlay(),
            Key::Up => self.sessions_selection = self.sessions_selection.saturating_sub(1),
            Key::Down => {
                let count = self.filtered_sessions().len();
                if count > 0 {
                    self.sessions_selection = (self.sessions_selection + 1).min(count - 1);
                }
            }
            Key::Char('D') => {
                if self.sessions_delete_armed {
                    self.sessions_delete_armed = false;
                    self.set_transient(
                        "Session deletion is not available in this build; nothing was deleted",
                    );
                } else {
                    self.sessions_delete_armed = true;
                    self.set_transient(
                        "Press D again to confirm delete (not available in this build)",
                    );
                }
            }
            Key::Char(ch) => {
                self.sessions_filter.push(ch);
                self.sessions_selection = 0;
                self.sessions_delete_armed = false;
                return LoopCmd::SearchSessions(self.sessions_filter.clone());
            }
            Key::Backspace => {
                self.sessions_filter.pop();
                self.sessions_selection = 0;
                self.sessions_delete_armed = false;
                return LoopCmd::SearchSessions(self.sessions_filter.clone());
            }
            Key::Enter => {
                if let Some(session) = self
                    .filtered_sessions()
                    .get(self.sessions_selection)
                    .cloned()
                {
                    self.close_overlay();
                    self.set_transient("Session selected locally; no agent was started");
                    return LoopCmd::OpenSession(session.id);
                }
            }
            _ => {}
        }
        LoopCmd::Continue
    }

    fn diff_key(&mut self, key: Key) -> LoopCmd {
        match key {
            Key::Esc => self.close_overlay(),
            Key::Left => {
                self.diff_file = self.diff_file.saturating_sub(1);
                self.diff_hunk = 0;
                self.diff_line = 0;
            }
            Key::Right => {
                let count = self.file_count();
                if count > 0 {
                    self.diff_file = (self.diff_file + 1).min(count - 1);
                    self.diff_hunk = 0;
                    self.diff_line = 0;
                }
            }
            Key::PageUp => self.diff_hunk = self.diff_hunk.saturating_sub(1),
            Key::PageDown => {
                let count = self.hunk_count();
                if count > 0 {
                    self.diff_hunk = (self.diff_hunk + 1).min(count - 1);
                    self.diff_line = 0;
                }
            }
            Key::Up => self.diff_line = self.diff_line.saturating_sub(1),
            Key::Down => {
                let count = self.line_count();
                if count > 0 {
                    self.diff_line = (self.diff_line + 1).min(count - 1);
                }
            }
            Key::Char('C') => {
                let path = self
                    .diff
                    .as_ref()
                    .and_then(|d| d.files.get(self.diff_file))
                    .map_or_else(|| "diff".to_string(), |f| f.path.clone());
                return LoopCmd::Copy(path);
            }
            _ => {}
        }
        LoopCmd::Continue
    }

    fn palette_key(&mut self, key: Key) -> LoopCmd {
        match key {
            Key::Esc => self.close_overlay(),
            Key::Up => self.palette_selection = self.palette_selection.saturating_sub(1),
            Key::Down => {
                let count = self.filtered_commands().len();
                if count > 0 {
                    self.palette_selection = (self.palette_selection + 1).min(count - 1);
                }
            }
            Key::Backspace => {
                self.palette_filter.pop();
                self.palette_selection = 0;
            }
            Key::Char(ch) => {
                self.palette_filter.push(ch);
                self.palette_selection = 0;
            }
            Key::Enter => {
                if let Some(cmd) = self
                    .filtered_commands()
                    .get(self.palette_selection)
                    .cloned()
                {
                    return self.run_palette_command(cmd);
                }
            }
            _ => {}
        }
        LoopCmd::Continue
    }

    /// Apply one event, returning the single action the main loop must take.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply(&mut self, event: UiEvent) -> LoopCmd {
        match event {
            UiEvent::Resize(cols, rows) => {
                if cols > 0 && rows > 0 {
                    self.size = (cols, rows);
                    self.dirty = true;
                }
            }
            UiEvent::Tick => {
                if self.transient.is_some() {
                    self.transient_ticks += 1;
                    // ~3 s at 100 ms ticks; decoration only, never information.
                    if self.transient_ticks >= 30 {
                        self.transient = None;
                        self.dirty = true;
                    }
                }
            }
            UiEvent::Frame(frame) => {
                let event_type = frame
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let entry = entry_from_frame(&frame);
                if let Some(state) = state_from_frame(&frame) {
                    self.state = state;
                }
                self.timeline.push(entry);
                // A frozen view never follows the tail; new frames are still
                // appended and counted, but the state is untouched.
                if !self.scroll_held && !self.output_frozen {
                    self.follow_tail();
                }
                self.dirty = true;
                if event_type == "approval_required" {
                    return LoopCmd::LoadApproval;
                }
            }
            UiEvent::HostAccepted => {
                if self.submit_pending {
                    let task = self.composer.take();
                    self.timeline
                        .push(TimelineEntry::new(EntryKind::User, task, None));
                    self.submit_pending = false;
                    self.run_active = true;
                    // The exit code reflects the last command outcome, not
                    // the UI close (brief §0.1 / plan §5).
                    self.last_exit = CliExitCode::Ok;
                    self.follow_tail();
                    self.dirty = true;
                }
            }
            UiEvent::HostRejected => {
                self.submit_pending = false;
                self.run_active = false;
                self.last_exit = CliExitCode::Usage;
                self.timeline.push(TimelineEntry::new(
                    EntryKind::Error,
                    "Task could not be sent; draft preserved",
                    None,
                ));
                self.follow_tail();
                self.dirty = true;
            }
            UiEvent::HostDone => {
                self.run_active = false;
                self.cancel_pending = false;
                self.dirty = true;
            }
            UiEvent::SessionsLoaded(result) => {
                match result {
                    super::sources::Readiness::Ready(list) => {
                        self.sessions = list;
                        self.sessions_unavailable = false;
                        let max = self.filtered_sessions().len().saturating_sub(1);
                        self.sessions_selection = self.sessions_selection.min(max);
                    }
                    super::sources::Readiness::Unavailable => {
                        self.sessions.clear();
                        self.sessions_unavailable = true;
                        self.sessions_selection = 0;
                    }
                }
                self.dirty = true;
            }
            UiEvent::DiffLoaded(result) => {
                match result {
                    super::sources::Readiness::Ready(snapshot) => {
                        self.diff = Some(snapshot);
                        self.diff_unavailable = false;
                        // Coordinates clamp to the newly loaded data but are
                        // otherwise preserved (return position on reopen).
                        self.diff_file = self.diff_file.min(self.file_count().saturating_sub(1));
                        self.diff_hunk = self.diff_hunk.min(self.hunk_count().saturating_sub(1));
                        self.diff_line = self.diff_line.min(self.line_count().saturating_sub(1));
                    }
                    super::sources::Readiness::Unavailable => {
                        self.diff = None;
                        self.diff_unavailable = true;
                    }
                }
                self.dirty = true;
            }
            UiEvent::ApprovalLoaded(result) => {
                // A freshly loaded request has no local decision yet. Tick,
                // Frame and overlay toggles never touch `approval_decision`.
                self.approval_request = match result {
                    super::sources::Readiness::Ready(Some(request)) => Some(request),
                    _ => None,
                };
                self.approval_decision = ApprovalDecision::Pending;
                self.approval_inspect = false;
                self.approval_return = None;
                self.dirty = true;
            }
            UiEvent::ApprovalResponded { approved, .. } => {
                // Only a core confirmation makes the decision real. The return marker
                // is placed here, never on the key press.
                self.approval_decision = if approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                };
                self.approval_return = Some(self.timeline.len().saturating_sub(1));
                // Keep the confirmed card at its return point until the next real
                // approval read replaces it; never clear it on a key press.
                self.approval_inspect = false;
                self.dirty = true;
            }
            UiEvent::ApprovalResponseFailed => {
                // The park survives a failed response: still pending, no local claim.
                self.approval_decision = ApprovalDecision::Pending;
                self.set_transient(
                    "Approval response was not accepted; the request is still pending",
                );
                self.dirty = true;
            }
            UiEvent::CopyResult(ok) => {
                self.copied_label = ok;
                if ok {
                    self.set_transient("Copied");
                } else {
                    self.set_transient("Copy is not available in this build; nothing was copied");
                }
            }
            UiEvent::Key(key) => return self.apply_key(key),
        }
        LoopCmd::Continue
    }

    #[allow(clippy::too_many_lines)]
    fn apply_key(&mut self, key: Key) -> LoopCmd {
        // The inline approval block owns A/D/I/Esc while it is pending and the
        // timeline has focus; the composer keeps those keys as plain typing.
        if self.overlay == Overlay::None
            && self.focus == Focus::Timeline
            && self.approval_request.is_some()
            && self.approval_decision == ApprovalDecision::Pending
        {
            match key {
                Key::Char('A') => return self.decide(ApprovalDecision::Approved),
                Key::Char('D') => return self.decide(ApprovalDecision::Denied),
                Key::Char('I') => {
                    self.approval_inspect = !self.approval_inspect;
                    self.dirty = true;
                    return LoopCmd::Continue;
                }
                Key::Esc => {
                    self.set_transient("Decision deferred; approval is still pending");
                    return LoopCmd::Continue;
                }
                _ => {}
            }
        }

        // An overlay consumes every key; closing restores focus/scroll/draft.
        if self.overlay != Overlay::None {
            self.dirty = true;
            return match self.overlay {
                Overlay::Help => {
                    if matches!(key, Key::Esc | Key::Enter | Key::F1) {
                        self.close_overlay();
                    }
                    LoopCmd::Continue
                }
                Overlay::Sessions => self.sessions_key(key),
                Overlay::Diff => self.diff_key(key),
                Overlay::Palette => self.palette_key(key),
                Overlay::None => LoopCmd::Continue,
            };
        }

        match key {
            Key::CtrlC => {
                // An approval cancel is distinct from a local deny: it asks the
                // main loop to stop the run and leaves the decision Pending.
                if self.approval_request.is_some()
                    && self.approval_decision == ApprovalDecision::Pending
                {
                    if self.cancel_pending {
                        return LoopCmd::Continue;
                    }
                    self.cancel_pending = true;
                    self.set_transient("Cancellation requested; approval left pending");
                    return LoopCmd::RequestCancel;
                }
                if self.run_active {
                    if self.cancel_pending {
                        return LoopCmd::Continue;
                    }
                    self.cancel_pending = true;
                    self.set_transient("Cancellation requested; waiting for the run to stop");
                    return LoopCmd::RequestCancel;
                }
                if !self.composer.is_empty() {
                    self.composer = Composer::new();
                    self.exit_confirm = false;
                    self.dirty = true;
                    return LoopCmd::Continue;
                }
                if !self.exit_confirm {
                    self.exit_confirm = true;
                    self.set_transient("Press Ctrl+C again to exit");
                    return LoopCmd::Continue;
                }
                return LoopCmd::Quit(self.last_exit);
            }
            Key::CtrlD => {
                if !self.run_active && self.composer.is_empty() {
                    return LoopCmd::Quit(self.last_exit);
                }
                return LoopCmd::Continue;
            }
            _ => {
                // Any other key cancels the exit confirmation.
                self.exit_confirm = false;
            }
        }

        match key {
            Key::CtrlP => {
                if self.run_active {
                    self.set_transient("Pause requested; waiting for the core to confirm");
                    return LoopCmd::RequestPause;
                }
            }
            Key::CtrlR => {
                if self.run_active {
                    self.set_transient("Resume requested; waiting for the core to confirm");
                    return LoopCmd::RequestResume;
                }
            }
            Key::F1 => {
                self.open_overlay(Overlay::Help);
            }
            Key::CtrlK => {
                self.palette_filter.clear();
                self.palette_selection = 0;
                self.open_overlay(Overlay::Palette);
            }
            Key::Esc => {
                // Defer / close overlay only; never approves or clears a draft.
                self.dirty = true;
            }
            Key::Tab | Key::BackTab => {
                self.focus = match self.focus {
                    Focus::Composer => Focus::Timeline,
                    Focus::Timeline => Focus::Composer,
                };
                self.dirty = true;
            }
            Key::Up => {
                if self.focus == Focus::Timeline {
                    self.scroll_by(-1);
                } else {
                    self.composer
                        .move_visual(self.composer_width(), Vertical::Up);
                    self.dirty = true;
                }
            }
            Key::Down => {
                if self.focus == Focus::Timeline {
                    self.scroll_by(1);
                } else {
                    self.composer
                        .move_visual(self.composer_width(), Vertical::Down);
                    self.dirty = true;
                }
            }
            Key::Left => {
                self.composer.move_left();
                self.dirty = true;
            }
            Key::Right => {
                self.composer.move_right();
                self.dirty = true;
            }
            Key::PageUp => self.scroll_by(-(i32::from(TIMELINE_MIN) as isize)),
            Key::PageDown => self.scroll_by(i32::from(TIMELINE_MIN) as isize),
            Key::Home => {
                if self.focus == Focus::Timeline {
                    self.scroll = 0;
                    self.engage_scroll_hold();
                } else {
                    self.composer.move_line_start();
                }
                self.dirty = true;
            }
            Key::End => {
                if self.focus == Focus::Timeline {
                    self.follow_tail();
                } else {
                    self.composer.move_line_end();
                }
                self.dirty = true;
            }
            Key::Backspace => {
                self.composer.backspace();
                self.dirty = true;
            }
            Key::Delete => {
                self.composer.delete();
                self.dirty = true;
            }
            Key::CtrlJ | Key::ShiftEnter => {
                self.composer.insert_char('\n');
                self.dirty = true;
            }
            Key::Enter => {
                if self.focus == Focus::Timeline && (self.scroll_held || self.output_frozen) {
                    // Enter returns to the live tail; it does NOT submit.
                    self.follow_tail();
                    self.dirty = true;
                    return LoopCmd::Continue;
                }
                if self.submit_pending || self.composer.is_empty() {
                    return LoopCmd::Continue;
                }
                self.submit_pending = true;
                self.dirty = true;
                // Send only once: the host's accept/reject event finishes the
                // transaction. Repeats are swallowed by `submit_pending`.
                return LoopCmd::Submit(self.composer.draft().to_string());
            }
            Key::Char(ch) => {
                if self.focus == Focus::Timeline {
                    match ch {
                        'f' => self.toggle_freeze(),
                        'I' => {
                            // Inspect = expand folded tool groups. Toggling a
                            // second time collapses them; order is preserved.
                            let anchors: Vec<usize> =
                                fold(&self.timeline[..self.visible_len()], &[], FOLD_THRESHOLD)
                                    .iter()
                                    .filter_map(|row| match row {
                                        RenderRow::Folded { anchor, .. } => Some(*anchor),
                                        RenderRow::Entry(..) => None,
                                    })
                                    .collect();
                            if anchors.is_empty()
                                || anchors.iter().all(|anchor| self.expanded.contains(anchor))
                            {
                                self.expanded.clear();
                            } else {
                                self.expanded = anchors;
                            }
                        }
                        // Other action letters are consumed with no fake
                        // action; they remain printable in the composer.
                        _ => {}
                    }
                    self.dirty = true;
                } else {
                    self.composer.insert_char(ch);
                    self.dirty = true;
                }
            }
            Key::Ignored => {}
            // Ctrl+C/Ctrl+D always return from the pre-match above; listed
            // here only so this match is exhaustive over `Key`.
            Key::CtrlC | Key::CtrlD => {
                self.dirty = true;
            }
        }
        LoopCmd::Continue
    }

    fn composer_width(&self) -> u16 {
        self.size.0.saturating_sub(2).max(1)
    }
}
