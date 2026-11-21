//! All TUI state in one `App`, advanced by `apply(UiEvent) -> LoopCmd`.
//!
//! The app is pure with respect to I/O: it never reads a terminal, spawns a
//! thread or calls a host. The main loop feeds it events and executes the
//! returned [`LoopCmd`]. Every state that survives a resize, overlay or error
//! (draft, scroll, focus, selection) lives here, never in the renderer. Run
//! state is derived *only* from real host frames — it is never guessed.

use std::path::Path;

use nexora_core::error::CliExitCode;

use crate::error::redact_value;
use crate::run::WireFrame;

use super::capabilities::Capabilities;
use super::composer::{Composer, Vertical};
use super::events::{Key, UiEvent};
use super::layout::TIMELINE_MIN;

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
/// viewport (brief §8.1 Focus Memory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    None,
    Help,
}

/// The small set of extra context lines drawn above the composer. Deferred
/// features (F5/R/C/A/D/I/Retry, #165) are deliberately absent.
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
fn sanitize(value: &str) -> String {
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
}

impl App {
    /// Build the initial idle app focused on the composer.
    #[must_use]
    pub(crate) fn new(caps: Capabilities, snapshot: StatusSnapshot, cols: u16, rows: u16) -> Self {
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
        }
    }

    /// Clamp the raw scroll to a concrete viewport height.
    #[must_use]
    pub(crate) fn effective_scroll(&self, viewport_rows: usize) -> usize {
        let max = self.timeline.len().saturating_sub(viewport_rows);
        self.scroll.min(max)
    }

    fn follow_tail(&mut self) {
        self.scroll = usize::MAX;
        self.scroll_held = false;
    }

    fn set_transient(&mut self, message: &str) {
        self.transient = Some(message.to_string());
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
        self.scroll_held = next < max;
        self.dirty = true;
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
                let entry = entry_from_frame(&frame);
                if let Some(state) = state_from_frame(&frame) {
                    self.state = state;
                }
                self.timeline.push(entry);
                if !self.scroll_held {
                    self.follow_tail();
                }
                self.dirty = true;
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
            UiEvent::Key(key) => return self.apply_key(key),
        }
        LoopCmd::Continue
    }

    #[allow(clippy::too_many_lines)]
    fn apply_key(&mut self, key: Key) -> LoopCmd {
        // An overlay consumes every key except close/Esc, and never edits the
        // draft or approves anything (brief §5.4, §10).
        if self.overlay != Overlay::None {
            self.dirty = true;
            match key {
                Key::Esc | Key::Enter | Key::F1 => self.overlay = Overlay::None,
                _ => {}
            }
            return LoopCmd::Continue;
        }

        match key {
            Key::CtrlC => {
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
            Key::F1 => {
                self.overlay = Overlay::Help;
                self.dirty = true;
            }
            Key::CtrlK => {
                // Honest deferral — no fake palette (issue #164 contract).
                self.set_transient("Command palette is not available in this build");
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
                    self.scroll_held = true;
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
                // F5/R/C/A/D/I/Retry are deferred to #165. When the timeline
                // has focus they are consumed (no fake action); while typing
                // in the composer they remain ordinary printable characters.
                if self.focus == Focus::Timeline && matches!(ch, 'R' | 'C' | 'A' | 'D' | 'I') {
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
