//! Pure draw functions for the four-zone TUI (brief §5, plan §2, phase 6a #165).
//!
//! Nothing in this module mutates [`App`], reads a terminal or calls a host.
//! `TestBackend` drives the exact same functions as the live loop, so render
//! tests need no real terminal. Colour is strictly monochrome (grey/dim, no
//! accent hue), and every status also carries a text or glyph marker so the
//! no-colour and ASCII modes keep all information. Overlays are drawn in the
//! timeline zone; the composer and its draft stay visible underneath.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::app::{fold, App, ApprovalDecision, EntryKind, RenderRow, RunState, FOLD_THRESHOLD};
use super::layout::{
    self, Metrics, Resolution, HINTS_ROWS, RULE_ROWS, STATUS_ROWS, TOO_SMALL_MESSAGE,
};
use super::sources::{ApprovalRequest, Availability, DiffLineKind};

/// Muted/secondary styling. In no-colour mode this is the empty style — no
/// foreground, background or modifier — so the text is identical but plain.
fn muted(app: &App) -> Style {
    if app.caps.color {
        Style::new().fg(Color::Gray)
    } else {
        Style::new()
    }
}

/// Style for the current-focus marker: a white foreground when colour is on,
/// plain text otherwise. Still monochrome — no hue and no underline.
fn active(app: &App) -> Style {
    if app.caps.color {
        Style::new().fg(Color::White)
    } else {
        Style::new()
    }
}

/// Reversed style used for a selection marker (works in every mode; contrast,
/// never hue, carries selection per brief §12.5).
fn selected() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// The reversed style used for the composer caret (works in every mode).
fn caret_style() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// Draw the whole frame: either the too-small message or the four zones.
pub(crate) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    match layout::resolve(area.width, area.height) {
        Resolution::TooSmall => {
            let paragraph = Paragraph::new(TOO_SMALL_MESSAGE).wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        Resolution::Usable(metrics) => render_usable(frame, app, metrics),
    }
}

fn render_usable(frame: &mut Frame<'_>, app: &App, metrics: Metrics) {
    let area = frame.area();
    let feed = Rect::new(metrics.feed_x, area.y, metrics.feed_width, area.height);

    let [status_area, rule_top, timeline_area, rule_mid, composer_area, hints_area] =
        Layout::vertical([
            Constraint::Length(STATUS_ROWS),
            Constraint::Length(RULE_ROWS),
            Constraint::Min(0),
            Constraint::Length(RULE_ROWS),
            Constraint::Length(metrics.composer_lines),
            Constraint::Length(HINTS_ROWS),
        ])
        .areas(feed);

    render_status(frame, app, status_area, metrics);
    frame.render_widget(app.caps.glyphs.rule_line(feed.width), rule_top);
    match app.overlay {
        super::app::Overlay::Help => render_help(frame, app, timeline_area),
        super::app::Overlay::Sessions => render_sessions(frame, app, timeline_area),
        super::app::Overlay::Diff => render_diff(frame, app, timeline_area),
        super::app::Overlay::Palette => render_palette(frame, app, timeline_area),
        super::app::Overlay::None => render_timeline(frame, app, timeline_area),
    }
    frame.render_widget(app.caps.glyphs.rule_line(feed.width), rule_mid);
    render_composer(frame, app, composer_area);
    render_hints(frame, app, hints_area, metrics);
}

/// Shorten the `provider/model` label to a bounded number of characters.
/// Narrow terminals get a shorter label (brief §5.3, §5.5).
fn model_label(app: &App, max: usize) -> String {
    let provider = app.snapshot.provider.as_deref().unwrap_or("no-provider");
    let model = app.snapshot.model.as_deref().unwrap_or("no-model");
    let full = format!("{provider}/{}", model.replace(' ', ""));
    full.chars().take(max).collect()
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect, metrics: Metrics) {
    let dot = app.caps.glyphs.dot;
    let state = app.state.token();
    let model_max = if metrics.width_class == layout::WidthClass::Narrow {
        12
    } else {
        24
    };
    let model = model_label(app, model_max);

    let mut spans = vec![
        Span::styled("NEXORA", active(app)),
        Span::raw("  "),
        Span::raw(app.snapshot.workspace.clone()),
    ];
    if metrics.show_metadata {
        if let Some(branch) = &app.snapshot.git_branch {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(branch.clone(), muted(app)));
        }
    }
    spans.push(Span::raw("   "));
    spans.push(Span::styled(model, muted(app)));
    if metrics.show_metadata {
        spans.push(Span::raw(format!(" {dot} ")));
        spans.push(Span::styled(
            short_autonomy(&app.snapshot.autonomy),
            muted(app),
        ));
    }
    spans.push(Span::raw(format!(" {dot} ")));
    spans.push(Span::styled(state, active(app)));

    // Cancel feedback is a real state marker, not decoration.
    if app.cancel_pending {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("cancel pending", muted(app)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn short_autonomy(autonomy: &str) -> &'static str {
    match autonomy {
        "semi_autonomous" => "semi-auto",
        "full_autonomous" => "full-auto",
        _ => "supervised",
    }
}

/// Append one entry row using the activity-spine glyph vocabulary.
fn push_entry(app: &App, lines: &mut Vec<Line<'static>>, entry: &super::app::TimelineEntry) {
    let g = app.caps.glyphs;
    match entry.kind {
        EntryKind::User => {
            lines.push(Line::from(Span::raw(entry.label.clone())));
        }
        EntryKind::Event => {
            let mut spans = vec![
                Span::styled(format!("{} ", g.spine), muted(app)),
                Span::raw(entry.label.clone()),
            ];
            if let Some(detail) = &entry.detail {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(detail.clone(), muted(app)));
            }
            lines.push(Line::from(spans));
        }
        EntryKind::Terminal => {
            let mut spans = vec![
                Span::styled(format!("{} ", g.elbow), muted(app)),
                Span::styled(g.check.to_string(), active(app)),
                Span::raw(format!(" {}", entry.label)),
            ];
            if let Some(detail) = &entry.detail {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(detail.clone(), muted(app)));
            }
            lines.push(Line::from(spans));
        }
        EntryKind::Error => {
            lines.push(Line::from(vec![
                Span::raw("! "),
                Span::raw(entry.label.clone()),
            ]));
        }
    }
}

/// Inline approval block. The command, cwd, reason and risk class are shown
/// BEFORE any decision; the decision text appears only after the core confirms
/// it (never on a key press, and never a claim that an agent resumed).
fn approval_lines(app: &App, request: &ApprovalRequest) -> Vec<Line<'static>> {
    let header = match app.approval_decision {
        ApprovalDecision::Pending => "! Approval required",
        ApprovalDecision::Approved => "Approved by core",
        ApprovalDecision::Denied => "Denied by core",
    };
    let mut lines = vec![Line::from(Span::styled(header.to_string(), active(app)))];
    let expand = app.approval_inspect || app.approval_decision != ApprovalDecision::Pending;
    let command = if expand || request.command.chars().count() <= 40 {
        request.command.clone()
    } else {
        let shortened: String = request.command.chars().take(40).collect();
        format!("{shortened}...")
    };
    lines.push(Line::from(vec![
        Span::raw("  command: "),
        Span::raw(command),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  Working directory: "),
        Span::raw(request.cwd.clone()),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  Reason / risk: "),
        Span::raw(request.reason.clone()),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  risk class: "),
        Span::raw(request.risk.label().to_string()),
    ]));
    match app.approval_decision {
        ApprovalDecision::Pending => lines.push(Line::from(Span::raw(
            "  [A] Approve once   [D] Deny   [I] Inspect   [Esc] Decide later",
        ))),
        _ => lines.push(Line::from(Span::styled(
            "  decision confirmed by the agent backend; no local resume",
            muted(app),
        ))),
    }
    lines
}

/// Build plain timeline lines from the (already whitelisted, sanitised)
/// entries, applying the density guard, scroll offset and view-only freeze.
fn timeline_lines(app: &App, height: usize) -> Vec<Line<'static>> {
    let g = app.caps.glyphs;
    let visible = &app.timeline[..app.visible_len()];
    let rows = fold(visible, &app.expanded, FOLD_THRESHOLD);
    let mut base: Vec<Line<'static>> = Vec::new();
    for row in &rows {
        match row {
            RenderRow::Entry(_, entry) => push_entry(app, &mut base, entry),
            RenderRow::Folded { label, count, .. } => {
                base.push(Line::from(vec![
                    Span::styled(format!("{} ", g.spine), muted(app)),
                    Span::raw(format!("{count} repeated {label}")),
                    Span::styled("  [I] expand", muted(app)),
                ]));
            }
        }
    }
    let start = app.effective_scroll(height);
    let mut lines: Vec<Line<'static>> = base.into_iter().skip(start).take(height).collect();
    // The approval block and view hints stay visible regardless of scroll.
    if let Some(request) = &app.approval_request {
        lines.extend(approval_lines(app, request));
    }
    if app.scroll_held {
        let new = app.timeline.len().saturating_sub(app.held_at_len);
        lines.push(Line::from(Span::styled(
            format!("{} {new} new events", g.arrow),
            muted(app),
        )));
    }
    if app.output_frozen {
        lines.push(Line::from(Span::styled(
            format!("live output paused · {} new events", app.new_events_count()),
            muted(app),
        )));
    }
    lines
}

fn render_timeline(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = timeline_lines(app, usize::from(area.height));
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Sessions overlay: title/status/age rows, instant filter and a marker that
/// reads through contrast (not colour). No fake sessions are ever shown.
fn render_sessions(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = vec![Line::from(Span::styled("Sessions", active(app)))];
    if app.sessions_unavailable {
        lines.push(Line::from(Span::styled(
            "Session history is not available in this build",
            muted(app),
        )));
    } else {
        let rows = app.filtered_sessions();
        if rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "No sessions match the filter",
                muted(app),
            )));
        } else {
            for (index, session) in rows.iter().enumerate() {
                let chosen = index == app.sessions_selection;
                let marker = if chosen { ">" } else { " " };
                let mut spans = vec![
                    Span::styled(
                        marker.to_string(),
                        if chosen { selected() } else { Style::new() },
                    ),
                    Span::raw(format!(" {}  ", session.title)),
                    Span::styled(session.status.label().to_string(), muted(app)),
                    Span::raw("  "),
                    Span::styled(session.age_label.clone(), muted(app)),
                ];
                if session.active {
                    spans.push(Span::raw("  (active)"));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    lines.push(Line::from(Span::styled(
        format!("> filter: {}", app.sessions_filter),
        active(app),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Diff overlay: grouped by file with `+N -M`, a `+`/`-` column, the jump map
/// `file i/N · hunk j/M`, and a thin touch marker only from source data.
fn render_diff(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let g = app.caps.glyphs;
    let mut lines = vec![Line::from(vec![
        Span::styled("Diff", active(app)),
        Span::raw(if app.copied_label { "  Copied" } else { "" }),
    ])];
    if app.diff_unavailable {
        lines.push(Line::from(Span::styled(
            "Diff is not available in this build; nothing was opened or changed",
            muted(app),
        )));
    } else if let Some(snapshot) = &app.diff {
        // Grouped file rows, each with a touch marker drawn only from data.
        let mut total_plus = 0u32;
        let mut total_minus = 0u32;
        for (index, file) in snapshot.files.iter().enumerate() {
            total_plus = total_plus.saturating_add(file.plus);
            total_minus = total_minus.saturating_add(file.minus);
            let mark = if index == app.diff_file { ">" } else { " " };
            let mut spans = vec![Span::raw(mark.to_string())];
            if file.touched {
                // Same spine glyph as the timeline: `│` in Unicode, `|` in ASCII.
                spans.push(Span::styled(format!("{} ", g.spine), muted(app)));
            } else {
                spans.push(Span::raw("  ".to_string()));
            }
            spans.push(Span::raw(file.path.clone()));
            spans.push(Span::raw(format!("  +{} -{}", file.plus, file.minus)));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(Span::styled(
            format!("total +{total_plus} -{total_minus}"),
            muted(app),
        )));
        if let Some(file) = snapshot.files.get(app.diff_file) {
            let hunks = file.hunks.len();
            lines.push(Line::from(Span::styled(
                format!(
                    "file {}/{} · hunk {}/{}",
                    app.diff_file + 1,
                    snapshot.files.len(),
                    (app.diff_hunk + 1).min(hunks.max(1)),
                    hunks.max(1)
                ),
                muted(app),
            )));
            if let Some(hunk) = file.hunks.get(app.diff_hunk) {
                lines.push(Line::from(Span::styled(hunk.header.clone(), muted(app))));
                for (index, diff_line) in hunk.lines.iter().enumerate() {
                    let column = match diff_line.kind {
                        DiffLineKind::Add => "+",
                        DiffLineKind::Remove => "-",
                        DiffLineKind::Context => " ",
                    };
                    let marker = if index == app.diff_line { ">" } else { " " };
                    let text = format!("{marker}{column} {}", diff_line.text);
                    lines.push(Line::from(Span::styled(
                        text,
                        if index == app.diff_line {
                            selected()
                        } else {
                            Style::new()
                        },
                    )));
                }
            }
        } else {
            lines.push(Line::from(Span::styled(
                "No diff is available for the current run",
                muted(app),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Command palette: the filter reacts immediately; unavailable actions are
/// marked with their reason and never run.
fn render_palette(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = vec![
        Line::from(Span::styled("Command", active(app))),
        Line::from(Span::styled(
            format!("> {}", app.palette_filter),
            active(app),
        )),
    ];
    let rows = app.filtered_commands();
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "No commands match the filter",
            muted(app),
        )));
    } else {
        for (index, command) in rows.iter().enumerate() {
            let chosen = index == app.palette_selection;
            let marker = if chosen { ">" } else { " " };
            let recent = if app.palette_recent.contains(&command.path) {
                " (recent)"
            } else {
                ""
            };
            let reason = match &command.availability {
                Availability::Available => String::new(),
                Availability::Unavailable(text) => format!("  [unavailable: {text}]"),
            };
            let text = format!("{marker} {}{recent}{reason}", command.label);
            lines.push(Line::from(Span::styled(
                text,
                if chosen { selected() } else { Style::new() },
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Build the composer lines plus the (row, column) of the caret, both relative
/// to the composer rectangle. The first line carries the `›` prefix; wrapped
/// continuation lines are indented so the caret stays on the right glyph.
fn composer_lines(app: &App, width: u16) -> (Vec<Line<'static>>, (u16, u16)) {
    let g = app.caps.glyphs;
    let text_width = width.saturating_sub(2).max(1);
    if app.composer.is_empty() {
        let line = Line::from(vec![
            Span::raw(format!("{} ", g.prefix)),
            Span::styled(" ", caret_style()),
            Span::styled("Ask Nexora anything...", muted(app)),
        ]);
        return (vec![line], (0, 2));
    }

    let visuals = app.composer.visual_lines(text_width);
    let cursor = app.composer.cursor();
    let active_row = visuals
        .iter()
        .rposition(|line| cursor >= line.start)
        .unwrap_or(0);
    let mut out = Vec::with_capacity(visuals.len());
    let mut caret = (0u16, 2u16);
    for (row, visual) in visuals.iter().enumerate() {
        let prefix = if row == 0 {
            format!("{} ", g.prefix)
        } else {
            "  ".to_string()
        };
        let mut spans = vec![Span::raw(prefix)];
        if row == active_row {
            let column = app.composer.draft()[visual.start..cursor].chars().count();
            let chars: Vec<char> = visual.text.chars().collect();
            let before: String = chars.iter().take(column).collect();
            let under = chars.get(column).copied();
            let after: String = chars
                .iter()
                .skip(column + usize::from(under.is_some()))
                .collect();
            spans.push(Span::raw(before));
            spans.push(Span::styled(
                under.map_or(" ".to_string(), |ch| ch.to_string()),
                caret_style(),
            ));
            spans.push(Span::raw(after));
            caret = (
                u16::try_from(row).unwrap_or(0),
                2 + u16::try_from(column).unwrap_or(0),
            );
        } else {
            spans.push(Span::raw(visual.text.clone()));
        }
        out.push(Line::from(spans));
    }
    (out, caret)
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (lines, (row, col)) = composer_lines(app, area.width);
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
    let x = area.x.saturating_add(col);
    let y = area.y.saturating_add(row);
    if x < area.right() && y < area.bottom() {
        frame.set_cursor_position(Position::new(x, y));
    }
}

/// A single, compact, state-specific hint line (brief §10, §5.5 short mode).
pub(crate) fn hint_text(app: &App, metrics: Metrics) -> String {
    // Transient feedback (confirm prompts, honest copy/decision results)
    // wins even inside an overlay, so it is never silently swallowed.
    if let Some(message) = &app.transient {
        return message.clone();
    }
    match app.overlay {
        super::app::Overlay::Sessions => {
            return "Type to filter · Up/Down select · Enter open (no agent started) · D delete · Esc back"
                .to_string()
        }
        super::app::Overlay::Diff => {
            return "Left/Right file · PgUp/PgDn hunk · Up/Down lines · C copy · Esc back"
                .to_string()
        }
        super::app::Overlay::Palette => {
            return "Type to filter · Up/Down select · Enter run · Esc back".to_string()
        }
        super::app::Overlay::Help => return "Esc or Enter to close".to_string(),
        super::app::Overlay::None => {}
    }
    if app.exit_confirm {
        return "Ctrl+C again to exit · Esc defer".to_string();
    }
    if app.cancel_pending {
        return "cancellation pending · PgUp/PgDn scroll · F1 help".to_string();
    }
    if app.output_frozen {
        return format!(
            "live output paused · {} new events · Enter/End return to live",
            app.new_events_count()
        );
    }
    if app.approval_request.is_some() && app.approval_decision == ApprovalDecision::Pending {
        return "[A] Approve once [D] Deny [I] Inspect [Esc] Decide later".to_string();
    }
    let unicode = !app.caps.ascii;
    let up_down = if unicode { "↑/↓" } else { "Up/Down" };
    if metrics.short {
        return match app.state {
            RunState::Idle => "Enter send · Ctrl+J nl · F1 help".to_string(),
            _ => format!("{up_down} scroll · Ctrl+C cancel · F1 help"),
        };
    }
    match app.state {
        RunState::Idle => {
            "Enter send · Ctrl+J newline · Ctrl+K commands · F1 help · Ctrl+C exit".to_string()
        }
        RunState::Running => {
            format!("{up_down} scroll · PgUp/PgDn page · Ctrl+C cancel · F1 help")
        }
        RunState::WaitingApproval => format!("{up_down} scroll · Esc defer · F1 help"),
        RunState::Paused | RunState::Failed => {
            format!("{up_down} scroll · F1 help · Ctrl+C cancel")
        }
    }
}

fn render_hints(frame: &mut Frame<'_>, app: &App, area: Rect, metrics: Metrics) {
    let line = Line::from(Span::styled(hint_text(app, metrics), muted(app)));
    frame.render_widget(Paragraph::new(line), area);
}

/// Only keys this build actually handles are listed; no "deferred" actions.
const HELP_LINES: &[&str] = &[
    "KEYS — help (Esc or Enter to close)",
    "Enter send / select / close    Ctrl+J newline    Shift+Enter newline",
    "Up/Down composer, timeline, lists   Left/Right caret or diff file",
    "Tab / Shift+Tab focus   PgUp/PgDn page or hunk   Home/End line or tail",
    "Ctrl+K palette   F1 this help   Ctrl+C cancel run / clear / confirm exit",
    "Ctrl+D exit when idle+empty   f freeze view   Enter/End return to live",
    "Sessions: type filter · Up/Down · Enter open (no agent) · D delete · Esc back",
    "Diff: Left/Right file · PgUp/PgDn hunk · Up/Down line · C copy · Esc back",
    "Approval: A approve · D deny · I inspect · Esc defer · Ctrl+C cancel run",
    "Overlays preserve draft, run and viewport; no agent is started here",
];

fn render_help(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines: Vec<Line<'static>> = HELP_LINES
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == 0 { active(app) } else { Style::new() };
            Line::from(Span::styled((*text).to_string(), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}
