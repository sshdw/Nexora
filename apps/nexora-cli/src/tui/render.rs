//! Pure draw functions for the four-zone TUI (brief §5, plan §2).
//!
//! Nothing in this module mutates [`App`], reads a terminal or calls a host.
//! `TestBackend` drives the exact same functions as the live loop, so render
//! tests need no real terminal. Colour is strictly monochrome (grey/dim, no
//! accent hue), and every status also carries a text or glyph marker so the
//! no-colour and ASCII modes keep all information.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, EntryKind, Overlay, RunState};
use super::layout::{
    self, Metrics, Resolution, HINTS_ROWS, RULE_ROWS, STATUS_ROWS, TOO_SMALL_MESSAGE,
};

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
    if app.overlay == Overlay::Help {
        render_help(frame, app, timeline_area);
    } else {
        render_timeline(frame, app, timeline_area);
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

/// Build plain timeline lines from the (already whitelisted, sanitised)
/// entries, applying the scroll offset. No fabricated statistics or progress.
fn timeline_lines(app: &App, height: usize) -> Vec<Line<'static>> {
    let g = app.caps.glyphs;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &app.timeline {
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
    if app.scroll_held {
        lines.push(Line::from(Span::styled(
            format!("{} new events", g.arrow),
            muted(app),
        )));
    }
    let start = app.effective_scroll(height);
    lines.into_iter().skip(start).take(height).collect()
}

fn render_timeline(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = timeline_lines(app, usize::from(area.height));
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
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
    if let Some(message) = &app.transient {
        return message.clone();
    }
    if app.exit_confirm {
        return "Ctrl+C again to exit · Esc defer".to_string();
    }
    if app.cancel_pending {
        return "cancellation pending · PgUp/PgDn scroll · F1 help".to_string();
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

const HELP_LINES: &[&str] = &[
    "KEYS — help (Esc or Enter to close)",
    "Enter send / close overlay    Ctrl+J newline    Shift+Enter newline",
    "Up/Down composer or timeline   Left/Right composer caret",
    "Tab / Shift+Tab focus          PgUp/PgDn page    Home/End line or ends",
    "Ctrl+C cancel run / clear / confirm exit    Ctrl+D exit when idle+empty",
    "Ctrl+K commands (not in this build)   F1 this help",
    "F5 / R / C / A / D / I / Retry: deferred to a later build",
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
