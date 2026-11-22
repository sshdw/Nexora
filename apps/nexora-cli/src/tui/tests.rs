//! `TestBackend` suite for the phase-5 TUI (issue #164, brief §17 items 2/4/5/6/11/12).
//!
//! Every test renders through the same pure `render::render` function as the
//! live loop, using `ratatui::backend::TestBackend` — no real terminal, no
//! raw mode, no sleeps and no network. Frames come from the existing
//! `WireFrame` seam; a scripted streaming host supplies a terminal frame last.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use ratatui::backend::TestBackend;
use ratatui::buffer::Cell;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use serde_json::json;

use super::app::{
    fold, parse_git_head, short_workspace, App, ApprovalDecision, EntryKind, Focus, LoopCmd,
    Overlay, RenderRow, RunState, StatusSnapshot, TimelineEntry, FOLD_THRESHOLD,
};
use super::capabilities::{resolve, CapabilityInput};
use super::events::{Key, UiEvent};
use super::glyphs::GlyphSet;
use super::layout::{self, Resolution};
use super::render;
use super::sources::{
    ApprovalRequest, Availability, CommandOrigin, DiffLine, DiffLineKind, DiffSnapshot, FileDiff,
    Hunk, RiskClass, SessionStatus, SessionSummary, Sources,
};
use super::terminal::{TerminalGuard, TerminalIo};
use crate::run::WireFrame;

const SECRET: &str = "sk-live-sentinel-1234567890";

fn caps() -> super::capabilities::Capabilities {
    resolve(CapabilityInput {
        color: "auto",
        ascii: false,
        motion: "quiet",
        no_color: None,
        term: Some("xterm-256color"),
        lang: Some("en_US.UTF-8"),
    })
}

fn caps_ascii() -> super::capabilities::Capabilities {
    resolve(CapabilityInput {
        color: "auto",
        ascii: true,
        motion: "quiet",
        no_color: None,
        term: Some("xterm-256color"),
        lang: Some("en_US.UTF-8"),
    })
}

fn caps_no_color() -> super::capabilities::Capabilities {
    resolve(CapabilityInput {
        color: "never",
        ascii: false,
        motion: "none",
        no_color: Some("1"),
        term: Some("xterm-256color"),
        lang: Some("en_US.UTF-8"),
    })
}

fn snapshot() -> StatusSnapshot {
    StatusSnapshot {
        workspace: "nx".to_string(),
        git_branch: Some("main".to_string()),
        provider: Some("openai".to_string()),
        model: Some("gpt-5.6-terra".to_string()),
        autonomy: "semi_autonomous".to_string(),
    }
}

fn app() -> App {
    App::new(caps(), snapshot(), 100, 30)
}

fn frame(event_type: &str) -> WireFrame {
    WireFrame::new(event_type)
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

fn draw(app: &App, cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("test terminal");
    terminal
        .draw(|f| render::render(f, app))
        .expect("draw must succeed");
    buffer_text(terminal.backend().buffer())
}

// ---- status line ---------------------------------------------------------

#[test]
fn status_line_shows_where_which_state_from_snapshots() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(
        frame("run_started").with("status", json!("running")),
    ));
    assert_eq!(a.state, RunState::Running);
    let text = draw(&a, 100, 30);
    for needle in [
        "NEXORA",
        "nx",
        "main",
        "openai/gpt-5.6-terra",
        "semi-auto",
        "RUNNING",
    ] {
        assert!(text.contains(needle), "status missing '{needle}':\n{text}");
    }
}

#[test]
fn status_line_omits_token_counts_price_and_full_path() {
    assert_eq!(short_workspace("/home/ubuntu/nexora"), "nexora");
    assert_eq!(short_workspace("C:\\Users\\dev\\nx"), "nx");
    let a = app(); // workspace is the short "nx", never a full path
    let text = draw(&a, 100, 30);
    for forbidden in ["/home/", "C:\\", "tok", "$", "1.25", "v0.1.0"] {
        assert!(
            !text.contains(forbidden),
            "status leaked '{forbidden}':\n{text}"
        );
    }
}

#[test]
fn status_line_truncates_provider_model_in_narrow() {
    let mut snapshot = snapshot();
    snapshot.provider = Some("anthropic".to_string());
    snapshot.model = Some("claude-sonnet-4-20250514".to_string());
    let a = App::new(caps(), snapshot, 100, 30);
    let wide = draw(&a, 100, 30);
    let narrow = draw(&a, 50, 30);
    // Narrow uses a 12-char cap, wide a 24-char cap.
    assert!(
        wide.contains("anthropic/claude-sonnet-"),
        "wide label:\n{wide}"
    );
    assert!(narrow.contains("anthropic/cl"), "narrow label:\n{narrow}");
    assert!(
        !narrow.contains("claude-sonnet-4-20250514"),
        "narrow must truncate"
    );
}

// ---- timeline ------------------------------------------------------------

#[test]
fn timeline_renders_only_received_frames_in_order() {
    let mut a = app();
    for kind in ["run_started", "tool_call_completed", "run_finished"] {
        a.apply(super::events::UiEvent::Frame(frame(kind)));
    }
    let text = draw(&a, 100, 30);
    let first = text.find("run_started").expect("first frame");
    let second = text.find("tool_call_completed").expect("second frame");
    let third = text.find("Completed").expect("terminal frame");
    assert!(
        first < second && second < third,
        "frames out of order:\n{text}"
    );
}

#[test]
fn timeline_uses_spine_glyphs_from_glyph_set() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    a.apply(super::events::UiEvent::Frame(frame("run_finished")));
    let uni = draw(&a, 100, 30);
    assert!(
        uni.contains(GlyphSet::unicode().spine),
        "unicode spine:\n{uni}"
    );
    assert!(
        uni.contains(GlyphSet::unicode().elbow),
        "unicode elbow:\n{uni}"
    );

    let mut ascii_app = App::new(caps_ascii(), snapshot(), 100, 30);
    ascii_app.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    ascii_app.apply(super::events::UiEvent::Frame(frame("run_finished")));
    let ascii = draw(&ascii_app, 100, 30);
    assert!(
        ascii.contains(GlyphSet::ascii().spine),
        "ascii spine:\n{ascii}"
    );
    assert!(
        ascii.contains(GlyphSet::ascii().elbow),
        "ascii elbow:\n{ascii}"
    );
}

#[test]
fn timeline_terminal_frame_is_plain_completed_text() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(
        frame("run_finished").with("status", json!("completed")),
    ));
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("✓ Completed"),
        "plain terminal frame:\n{text}"
    );
    for fabricated in ["tok/s", "files touched", "tests passed", "18s"] {
        assert!(!text.contains(fabricated), "fabricated stat '{fabricated}'");
    }
}

#[test]
fn timeline_does_not_fabricate_stats_when_fields_absent() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    let text = draw(&a, 100, 30);
    for fabricated in ["%", "tok", "files", "tests", "s elapsed"] {
        assert!(
            !text.contains(fabricated),
            "invented stat '{fabricated}':\n{text}"
        );
    }
}

#[test]
fn frame_with_unknown_type_uses_safe_fixed_label() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(frame("mystery_event")));
    let text = draw(&a, 100, 30);
    assert!(text.contains("Event"), "fixed label missing:\n{text}");
    assert!(
        !text.contains("mystery_event"),
        "raw unknown type leaked:\n{text}"
    );
}

#[test]
fn render_never_contains_secret_sentinel() {
    let mut a = app();
    a.apply(super::events::UiEvent::Frame(
        frame("tool_call_completed").with("message", json!(SECRET)),
    ));
    let text = draw(&a, 100, 30);
    assert!(!text.contains(SECRET), "secret leaked into buffer:\n{text}");
    assert!(
        text.contains("[redacted]"),
        "secret must be redacted:\n{text}"
    );
}

// ---- composer / hints ----------------------------------------------------

#[test]
fn composer_shows_prefix_and_placeholder_when_empty() {
    let a = app();
    let text = draw(&a, 100, 30);
    assert!(text.contains(GlyphSet::unicode().prefix), "prefix:\n{text}");
    assert!(
        text.contains("Ask Nexora anything..."),
        "placeholder:\n{text}"
    );
}

#[test]
fn composer_wraps_draft_and_keeps_it_editable() {
    let mut a = app();
    a.composer.set_draft("a".repeat(80));
    assert!(a.composer.visual_lines(20).len() > 1, "draft must wrap");
    let text = draw(&a, 50, 30);
    assert!(
        text.contains("aaaa"),
        "wrapped draft must be visible:\n{text}"
    );
    // It remains editable after wrapping.
    a.apply(super::events::UiEvent::Key(Key::Left));
    a.apply(super::events::UiEvent::Key(Key::Char('Z')));
    assert!(
        a.composer.draft().contains('Z'),
        "wrapped draft still editable"
    );
}

#[test]
fn hints_are_single_line_and_state_specific() {
    let idle = app();
    let mut running = app();
    running.state = RunState::Running;
    let metrics = match layout::resolve(100, 30) {
        Resolution::Usable(m) => m,
        Resolution::TooSmall => panic!("usable"),
    };
    let idle_hint = render::hint_text(&idle, metrics);
    let running_hint = render::hint_text(&running, metrics);
    assert!(!idle_hint.contains('\n') && !running_hint.contains('\n'));
    assert_ne!(idle_hint, running_hint, "hints must be state-specific");
    assert!(idle_hint.contains("Enter send"));
    assert!(running_hint.contains("cancel"));
}

// ---- breakpoints ---------------------------------------------------------

#[test]
fn breakpoint_wide_caps_feed_width() {
    let a = app();
    let Resolution::Usable(metrics) = layout::resolve(140, 30) else {
        panic!("wide must be usable")
    };
    assert_eq!(metrics.feed_width, layout::FEED_MAX_COLS);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).expect("terminal");
    terminal.draw(|f| render::render(f, &a)).expect("draw");
    let buffer = terminal.backend().buffer();
    for x in 0..metrics.feed_x {
        assert_eq!(
            buffer.cell((x, 0)).map(Cell::symbol),
            Some(" "),
            "left gutter"
        );
    }
    assert_eq!(
        buffer.cell((metrics.feed_x, 0)).map(Cell::symbol),
        Some("N"),
        "feed starts at the centred offset"
    );
}

#[test]
fn breakpoint_medium_uses_single_column() {
    let metrics = match layout::resolve(100, 30) {
        Resolution::Usable(m) => m,
        Resolution::TooSmall => panic!("medium must be usable"),
    };
    assert_eq!(metrics.width_class, layout::WidthClass::Medium);
    assert_eq!(metrics.feed_x, 0);
    assert_eq!(metrics.feed_width, 100);
}

#[test]
fn breakpoint_narrow_hides_metadata_and_wraps_paths() {
    let a = app();
    let text = draw(&a, 50, 30);
    assert!(
        !text.contains("semi-auto"),
        "narrow hides autonomy metadata"
    );
    assert!(!text.contains("main"), "narrow hides git branch metadata");
    let metrics = match layout::resolve(50, 30) {
        Resolution::Usable(m) => m,
        Resolution::TooSmall => panic!("narrow must be usable"),
    };
    assert!(metrics.wrap_paths && !metrics.show_metadata);
    // Actions are still available.
    assert!(text.contains("Enter send"));
}

#[test]
fn breakpoint_short_compacts_hints_and_composer() {
    let metrics = match layout::resolve(100, 20) {
        Resolution::Usable(m) => m,
        Resolution::TooSmall => panic!("short must be usable"),
    };
    assert!(metrics.short);
    assert_eq!(metrics.composer_lines, layout::COMPOSER_TEXT_MAX_SHORT);
    assert_eq!(metrics.timeline_min, layout::TIMELINE_MIN_SHORT);
    let a = App::new(caps(), snapshot(), 100, 20);
    let text = draw(&a, 100, 20);
    assert!(
        text.contains("Enter send") || text.contains("scroll"),
        "short hints:\n{text}"
    );
}

#[test]
fn breakpoint_too_small_renders_only_enlarge_message() {
    let a = app();
    let text = draw(&a, 30, 8);
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(layout::TOO_SMALL_MESSAGE),
        "exact message:\n{text}"
    );
    assert!(!text.contains("NEXORA"), "no status zone in too-small mode");
    assert!(
        !text.contains("Ask Nexora"),
        "no composer in too-small mode"
    );
}

// ---- state survival / scroll --------------------------------------------

#[test]
fn resize_preserves_scroll_offset_draft_and_focus() {
    let mut a = app();
    a.composer.set_draft("draft".to_string());
    a.focus = Focus::Timeline;
    a.scroll = 3;
    a.apply(super::events::UiEvent::Resize(80, 24));
    assert_eq!(a.composer.draft(), "draft");
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 3);
    assert_eq!(a.size, (80, 24));
}

#[test]
fn scroll_hold_keeps_offset_when_new_frames_arrive() {
    let mut a = app();
    for i in 0..10 {
        a.apply(super::events::UiEvent::Frame(
            frame("tool_call_started").with("name", json!(format!("step{i}"))),
        ));
    }
    a.focus = Focus::Timeline;
    a.apply(super::events::UiEvent::Key(Key::Up));
    assert!(a.scroll_held, "Up must engage scroll hold");
    let held = a.scroll;
    a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    assert_eq!(a.scroll, held, "new frames must not move a held viewport");
    assert!(a.scroll_held);
}

#[test]
fn end_key_clears_scroll_hold_and_follows_tail() {
    let mut a = app();
    for _ in 0..10 {
        a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    }
    a.focus = Focus::Timeline;
    a.apply(super::events::UiEvent::Key(Key::Up));
    assert!(a.scroll_held);
    a.apply(super::events::UiEvent::Key(Key::End));
    assert!(!a.scroll_held, "End releases scroll hold");
    assert_eq!(a.scroll, usize::MAX, "End follows the tail");
}

// ---- draft / overlay / send ---------------------------------------------

#[test]
fn draft_survives_help_overlay_open_close() {
    let mut a = app();
    a.composer.set_draft("keep me".to_string());
    a.apply(super::events::UiEvent::Key(Key::F1));
    assert_eq!(a.overlay, Overlay::Help);
    a.apply(super::events::UiEvent::Key(Key::Esc));
    assert_eq!(a.overlay, Overlay::None);
    assert_eq!(a.composer.draft(), "keep me");
}

#[test]
fn draft_survives_send_error() {
    let mut a = app();
    a.composer.set_draft("fix auth".to_string());
    assert!(matches!(
        a.apply(super::events::UiEvent::Key(Key::Enter)),
        LoopCmd::Submit(_)
    ));
    a.apply(super::events::UiEvent::HostRejected);
    assert_eq!(
        a.composer.draft(),
        "fix auth",
        "error must not destroy the draft"
    );
    assert!(a
        .timeline
        .iter()
        .any(|e| matches!(e.kind, super::app::EntryKind::Error)));
}

#[test]
fn sent_task_enters_history_exactly_once() {
    let mut a = app();
    a.composer.set_draft("task".to_string());
    assert!(matches!(
        a.apply(super::events::UiEvent::Key(Key::Enter)),
        LoopCmd::Submit(_)
    ));
    a.apply(super::events::UiEvent::HostAccepted);
    // A second accept (duplicate delivery) must not duplicate the entry.
    a.apply(super::events::UiEvent::HostAccepted);
    let tasks: Vec<&TimelineEntry> = a
        .timeline
        .iter()
        .filter(|entry| entry.label == "task")
        .collect();
    assert_eq!(tasks.len(), 1, "task entered history exactly once");
    assert!(a.composer.is_empty());
}

#[test]
fn esc_never_approves_and_never_clears_draft() {
    let mut a = app();
    a.composer.set_draft("draft".to_string());
    a.apply(super::events::UiEvent::Frame(frame("approval_required")));
    assert_eq!(a.state, RunState::WaitingApproval);
    a.apply(super::events::UiEvent::Key(Key::Esc));
    assert_eq!(a.composer.draft(), "draft");
    assert_eq!(a.state, RunState::WaitingApproval, "Esc must not approve");
}

#[test]
fn ctrl_c_first_requests_cancel_then_ignores_repeat() {
    let mut a = app();
    a.composer.set_draft("task".to_string());
    a.apply(super::events::UiEvent::Key(Key::Enter));
    a.apply(super::events::UiEvent::HostAccepted); // run active
    assert!(a.run_active);
    let first = a.apply(super::events::UiEvent::Key(Key::CtrlC));
    assert_eq!(first, LoopCmd::RequestCancel);
    assert!(a.cancel_pending);
    let second = a.apply(super::events::UiEvent::Key(Key::CtrlC));
    assert_eq!(second, LoopCmd::Continue, "repeat must be ignored");
}

#[test]
fn ctrl_c_idle_empty_requires_second_press_to_exit() {
    let mut a = app();
    let first = a.apply(super::events::UiEvent::Key(Key::CtrlC));
    assert_eq!(first, LoopCmd::Continue);
    assert!(a.exit_confirm);
    let second = a.apply(super::events::UiEvent::Key(Key::CtrlC));
    assert!(matches!(second, LoopCmd::Quit(_)));
}

#[test]
fn ctrl_j_inserts_newline_and_enter_does_not_on_paste() {
    let mut a = app();
    // A bracketed multi-line paste arrives as LF bytes -> CtrlJ newlines.
    for _ in 0..3 {
        a.apply(super::events::UiEvent::Key(Key::CtrlJ));
    }
    assert_eq!(a.composer.draft().matches('\n').count(), 3);
    assert!(
        a.timeline
            .iter()
            .all(|e| e.kind != super::app::EntryKind::User),
        "paste must never auto-send"
    );
    // An explicit Enter is the only thing that sends.
    let cmd = a.apply(super::events::UiEvent::Key(Key::Enter));
    assert!(matches!(cmd, LoopCmd::Submit(_)));
}

#[test]
fn tab_cycles_focus_between_composer_and_timeline() {
    let mut a = app();
    assert_eq!(a.focus, Focus::Composer);
    a.apply(super::events::UiEvent::Key(Key::Tab));
    assert_eq!(a.focus, Focus::Timeline);
    a.apply(super::events::UiEvent::Key(Key::Tab));
    assert_eq!(a.focus, Focus::Composer);
    a.apply(super::events::UiEvent::Key(Key::BackTab));
    assert_eq!(a.focus, Focus::Timeline);
}

#[test]
fn page_up_down_scroll_timeline_by_page() {
    let mut a = app();
    for _ in 0..30 {
        a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    }
    a.focus = Focus::Timeline;
    a.apply(super::events::UiEvent::Key(Key::PageUp));
    let up = a.scroll;
    a.apply(super::events::UiEvent::Key(Key::PageDown));
    assert!(a.scroll > up, "PageDown moves below PageUp");
}

#[test]
fn f1_opens_help_overlay_and_esc_closes_it() {
    let mut a = app();
    a.apply(super::events::UiEvent::Key(Key::F1));
    assert_eq!(a.overlay, Overlay::Help);
    let text = draw(&a, 100, 30);
    assert!(text.contains("KEYS"), "help overlay:\n{text}");
    a.apply(super::events::UiEvent::Key(Key::Esc));
    assert_eq!(a.overlay, Overlay::None);
}

// (The old `ctrl_k_is_deferred_no_fake_palette` assertion was replaced by the
//  real palette suite below: phase 6a ships a genuine, Clap-derived palette.)

// ---- capability fallbacks ------------------------------------------------

#[test]
fn ascii_glyph_map_applies_to_spine_and_separators() {
    let mut a = App::new(caps_ascii(), snapshot(), 100, 30);
    a.apply(super::events::UiEvent::Frame(frame("tool_call_started")));
    a.apply(super::events::UiEvent::Frame(frame("run_finished")));
    let text = draw(&a, 100, 30);
    assert!(text.contains('|'), "ASCII spine");
    assert!(text.contains("+--"), "ASCII elbow");
    assert!(text.contains('>'), "ASCII composer prefix");
    assert!(text.contains("---"), "ASCII rule");
    assert!(text.contains("[ok] Completed"), "ASCII success marker");
}

#[test]
fn no_color_renders_same_text_with_no_style_modifiers() {
    let a = app();
    let no_color = App::new(caps_no_color(), snapshot(), 100, 30);
    let colored_text = draw(&a, 100, 30);
    let plain_text = draw(&no_color, 100, 30);
    assert_eq!(
        colored_text, plain_text,
        "no-colour mode must keep identical text"
    );

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| render::render(f, &no_color))
        .expect("draw");
    for cell in &terminal.backend().buffer().content {
        assert_eq!(cell.fg, Color::Reset, "no-colour cell has a foreground");
        assert_eq!(cell.bg, Color::Reset, "no-colour cell has a background");
        assert!(
            cell.modifier.is_empty() || cell.modifier == Modifier::REVERSED,
            "only the caret may carry a modifier, got {:?}",
            cell.modifier
        );
    }
}

#[test]
fn ascii_and_no_color_keep_all_actions_available() {
    let caps = resolve(CapabilityInput {
        color: "never",
        ascii: true,
        motion: "none",
        no_color: Some("1"),
        term: Some("dumb"),
        lang: Some("C"),
    });
    let mut a = App::new(caps, snapshot(), 100, 30);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Enter send"), "actions remain in hints");
    assert!(text.contains("Ask Nexora anything..."), "composer remains");
    assert!(
        matches!(
            a.apply(super::events::UiEvent::Key(Key::Enter)),
            LoopCmd::Continue
        ),
        "empty Enter is a no-op, not a crash"
    );
    a.composer.set_draft("task".to_string());
    assert!(
        matches!(
            a.apply(super::events::UiEvent::Key(Key::Enter)),
            LoopCmd::Submit(_)
        ),
        "sending remains available"
    );
}

// ---- scripted host + terminal guard ------------------------------------

#[test]
fn scripted_host_finished_frame_is_last_in_timeline() {
    let frames = ScriptedStreamingHost::frames();
    let mut a = app();
    for f in frames {
        a.apply(super::events::UiEvent::Frame(f));
    }
    assert_eq!(a.timeline.len(), 3);
    assert!(
        matches!(
            a.timeline.last().map(|e| &e.kind),
            Some(super::app::EntryKind::Terminal)
        ),
        "terminal frame must be last"
    );
    let text = draw(&a, 100, 30);
    let completed = text.find("Completed").expect("terminal frame rendered");
    let composer = text.find("Ask Nexora").expect("composer rendered");
    assert!(
        completed < composer,
        "terminal frame must be in the timeline:\n{text}"
    );
}

#[test]
fn terminal_guard_restores_on_drop_error_and_panic() {
    #[derive(Default)]
    struct Recorder {
        ops: Rc<RefCell<Vec<&'static str>>>,
        fail: Option<&'static str>,
    }
    impl TerminalIo for Recorder {
        fn enter_raw(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("raw");
            if self.fail == Some("raw") {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn enter_alt(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("alt");
            if self.fail == Some("alt") {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("hide");
            if self.fail == Some("hide") {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn leave_raw(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("leave_raw");
            Ok(())
        }
        fn leave_alt(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("leave_alt");
            Ok(())
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push("show");
            Ok(())
        }
    }

    // Normal drop: reverse order.
    let ops = Rc::new(RefCell::new(Vec::new()));
    {
        let _guard = TerminalGuard::acquire(Recorder {
            ops: Rc::clone(&ops),
            fail: None,
        })
        .expect("acquire");
        assert_eq!(*ops.borrow(), vec!["raw", "alt", "hide"]);
    }
    assert_eq!(
        *ops.borrow(),
        vec!["raw", "alt", "hide", "show", "leave_alt", "leave_raw"]
    );

    // Error mid-acquisition: partial state is restored.
    let err_ops = Rc::new(RefCell::new(Vec::new()));
    assert!(TerminalGuard::acquire(Recorder {
        ops: Rc::clone(&err_ops),
        fail: Some("alt"),
    })
    .is_err());
    assert!(err_ops.borrow().contains(&"leave_raw"));

    // Panic unwind: Drop still restores.
    let panic_ops = Rc::new(RefCell::new(Vec::new()));
    let observer = Rc::clone(&panic_ops);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = TerminalGuard::acquire(Recorder {
            ops: observer,
            fail: None,
        })
        .expect("acquire");
        panic!("simulated");
    }));
    assert!(result.is_err());
    let tail: Vec<&str> = panic_ops.borrow().iter().rev().take(3).copied().collect();
    assert_eq!(tail, vec!["leave_raw", "leave_alt", "show"]);
}

/// Deterministic streaming host used only by tests: emits two progress frames
/// then a terminal `run_finished` frame LAST (never fabricating statistics).
struct ScriptedStreamingHost;

impl ScriptedStreamingHost {
    fn frames() -> Vec<WireFrame> {
        vec![
            frame("run_started").with("status", json!("running")),
            frame("tool_call_completed").with("name", json!("Read")),
            frame("run_finished").with("status", json!("completed")),
        ]
    }
}

#[test]
fn git_head_and_workspace_helpers_are_honest() {
    assert_eq!(
        parse_git_head("ref: refs/heads/main\n").as_deref(),
        Some("main")
    );
    assert_eq!(parse_git_head("0123456789abcdef\n"), None);
    assert_eq!(short_workspace(""), "workspace");
}

// ===========================================================================
// Phase 6a (issue #165) — UI-only TUI depth scaffold.
//
// Every source below is injected through `test_sources` (`cfg(test)` only).
// The shipped sources are `Unavailable`, so no test here can accidentally
// exercise fabricated backend data in the real binary. No real terminal, no
// raw mode, no sleeps, no network and no file writes are involved.
// ===========================================================================

fn drive(a: &mut App, cmd: LoopCmd, sources: &Sources) {
    match cmd {
        LoopCmd::LoadSessions => {
            let result = sources.load_sessions();
            a.apply(UiEvent::SessionsLoaded(result));
        }
        LoopCmd::LoadDiff => {
            let result = sources.load_diff();
            a.apply(UiEvent::DiffLoaded(result));
        }
        LoopCmd::LoadApproval => {
            let result = sources.load_approval();
            a.apply(UiEvent::ApprovalLoaded(result));
        }
        LoopCmd::Copy(text) => {
            let ok = sources.copy(&text);
            a.apply(UiEvent::CopyResult(ok));
        }
        // Palette/local choices and session opening already applied their
        // honest state in `App`; the loop has no backend to run here.
        LoopCmd::Continue
        | LoopCmd::RunCommand(_)
        | LoopCmd::OpenSession(_)
        | LoopCmd::Submit(_)
        | LoopCmd::RequestCancel
        | LoopCmd::Quit(_) => {}
    }
}

fn key(a: &mut App, k: Key, sources: &Sources) {
    let cmd = a.apply(UiEvent::Key(k));
    drive(a, cmd, sources);
}

fn session(
    id: &str,
    title: &str,
    status: SessionStatus,
    age: &str,
    active: bool,
) -> SessionSummary {
    SessionSummary {
        id: id.to_string(),
        title: title.to_string(),
        status,
        age_label: age.to_string(),
        active,
    }
}

fn request(command: &str, cwd: &str, risk: RiskClass, reason: &str) -> ApprovalRequest {
    ApprovalRequest {
        id: "req-1".to_string(),
        command: command.to_string(),
        cwd: cwd.to_string(),
        risk,
        reason: reason.to_string(),
    }
}

fn dline(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: text.to_string(),
    }
}

fn hunk(header: &str, lines: Vec<DiffLine>) -> Hunk {
    Hunk {
        header: header.to_string(),
        lines,
    }
}

fn file(path: &str, plus: u32, minus: u32, hunks: Vec<Hunk>, touched: bool) -> FileDiff {
    FileDiff {
        path: path.to_string(),
        plus,
        minus,
        hunks,
        touched,
    }
}

fn diff(files: Vec<FileDiff>) -> DiffSnapshot {
    DiffSnapshot { files }
}

fn sample_diff() -> DiffSnapshot {
    diff(vec![
        file(
            "src/auth.rs",
            24,
            8,
            vec![hunk(
                "@@ -1,3 +1,5 @@",
                vec![
                    dline(DiffLineKind::Context, " fn login() {"),
                    dline(DiffLineKind::Add, "+  let token = refresh();"),
                    dline(DiffLineKind::Remove, "-  let token = old();"),
                ],
            )],
            true,
        ),
        file(
            "src/session.rs",
            3,
            1,
            vec![hunk(
                "@@ -9,2 +9,3 @@",
                vec![
                    dline(DiffLineKind::Context, " struct Session;"),
                    dline(DiffLineKind::Add, "+  id: u64,"),
                ],
            )],
            false,
        ),
    ])
}

fn sessions_sources() -> Sources {
    super::test_sources(
        Some(vec![
            session(
                "1",
                "Fix authentication",
                SessionStatus::Completed,
                "18m ago",
                true,
            ),
            session(
                "2",
                "Refactor API client",
                SessionStatus::Cancelled,
                "yesterday",
                false,
            ),
        ]),
        None,
        None,
        false,
    )
}

fn diff_sources() -> Sources {
    super::test_sources(None, Some(sample_diff()), None, false)
}

fn approval_sources() -> Sources {
    super::test_sources(
        None,
        None,
        Some(Some(request(
            "npm test -- --updateSnapshot",
            "C:\\projects\\nx",
            RiskClass::DangerousShell,
            "This command may rewrite snapshot files.",
        ))),
        false,
    )
}

fn open_sessions(a: &mut App, sources: &Sources) {
    key(a, Key::CtrlK, sources);
    for ch in "sessions".chars() {
        key(a, Key::Char(ch), sources);
    }
    key(a, Key::Enter, sources);
    assert_eq!(a.overlay, Overlay::Sessions, "palette must open sessions");
}

fn open_diff(a: &mut App, sources: &Sources) {
    key(a, Key::CtrlK, sources);
    for ch in "diff".chars() {
        key(a, Key::Char(ch), sources);
    }
    key(a, Key::Enter, sources);
    assert_eq!(a.overlay, Overlay::Diff, "palette must open diff");
}

fn load_approval(a: &mut App, sources: &Sources) {
    let cmd = a.apply(UiEvent::Frame(frame("approval_required")));
    assert_eq!(cmd, LoopCmd::LoadApproval);
    drive(a, cmd, sources);
    assert!(a.approval_request.is_some());
}

fn app_with_approval(sources: &Sources) -> App {
    let mut a = app();
    load_approval(&mut a, sources);
    a.focus = Focus::Timeline;
    a
}

// ---- sessions overlay ----------------------------------------------------

#[test]
fn sessions_overlay_opens_and_closes_preserving_run_draft_and_viewport() {
    let sources = sessions_sources();
    let mut a = app();
    a.composer.set_draft("keep me".to_string());
    a.focus = Focus::Timeline;
    a.scroll = 4;
    a.scroll_held = true;
    let state = a.state;
    open_sessions(&mut a, &sources);
    assert_eq!(a.composer.draft(), "keep me", "overlay must not edit draft");
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.overlay, Overlay::None);
    assert_eq!(a.composer.draft(), "keep me");
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 4);
    assert!(a.scroll_held);
    assert!(!a.run_active);
    assert!(!a.submit_pending);
    assert_eq!(a.state, state);
}

#[test]
fn sessions_filter_narrows_entries_and_empty_result_is_explained() {
    let sources = sessions_sources();
    let mut a = app();
    open_sessions(&mut a, &sources);
    for ch in "fix".chars() {
        key(&mut a, Key::Char(ch), &sources);
    }
    assert_eq!(a.filtered_sessions().len(), 1);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Fix authentication"), "{text}");
    assert!(!text.contains("Refactor API client"), "{text}");
    for _ in 0..3 {
        key(&mut a, Key::Backspace, &sources);
    }
    for ch in "zzz".chars() {
        key(&mut a, Key::Char(ch), &sources);
    }
    assert!(a.filtered_sessions().is_empty());
    let text = draw(&a, 100, 30);
    assert!(text.contains("No sessions match the filter"), "{text}");
}

#[test]
fn sessions_open_does_not_auto_run() {
    let sources = sessions_sources();
    let mut a = app();
    a.composer.set_draft("unfinished".to_string());
    open_sessions(&mut a, &sources);
    let timeline_before = a.timeline.len();
    let cmd = a.apply(UiEvent::Key(Key::Enter));
    assert!(matches!(cmd, LoopCmd::OpenSession(id) if id == "1"));
    assert_eq!(a.overlay, Overlay::None);
    assert!(!a.run_active, "open must not start a run");
    assert!(!a.submit_pending, "open must not submit");
    assert_eq!(a.state, RunState::Idle);
    assert_eq!(a.timeline.len(), timeline_before);
    assert_eq!(a.composer.draft(), "unfinished");
    let text = draw(&a, 100, 30);
    assert!(text.contains("no agent was started"), "{text}");
}

#[test]
fn sessions_delete_requires_confirm_and_is_unavailable_in_shipped_build() {
    let sources = sessions_sources();
    let mut a = app();
    open_sessions(&mut a, &sources);
    let before = a.sessions.len();
    key(&mut a, Key::Char('D'), &sources);
    assert!(a.sessions_delete_armed);
    assert_eq!(a.sessions.len(), before);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Press D again to confirm delete"), "{text}");
    key(&mut a, Key::Char('D'), &sources);
    assert!(!a.sessions_delete_armed);
    assert_eq!(a.sessions.len(), before, "no session was actually deleted");
    assert_eq!(a.overlay, Overlay::Sessions);
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("not available in this build; nothing was deleted"),
        "{text}"
    );
}

#[test]
fn sessions_unavailable_source_renders_honest_message() {
    let sources = super::test_sources(None, None, None, false);
    let mut a = app();
    open_sessions(&mut a, &sources);
    assert!(a.sessions_unavailable);
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("Session history is not available in this build"),
        "{text}"
    );
    assert!(!text.contains("Fix authentication"), "no fake sessions");
}

// ---- diff overlay --------------------------------------------------------

#[test]
fn diff_overlay_renders_grouped_files_with_plus_minus_and_totals() {
    let sources = diff_sources();
    let mut a = app();
    open_diff(&mut a, &sources);
    let text = draw(&a, 100, 30);
    for needle in [
        "src/auth.rs",
        "+24 -8",
        "src/session.rs",
        "+3 -1",
        "total +27 -9",
        "file 1/2",
        "hunk 1/1",
        "+  let token = refresh();",
        "-  let token = old();",
    ] {
        assert!(text.contains(needle), "diff missing '{needle}':\n{text}");
    }
}

#[test]
fn diff_view_never_applies_or_writes_changes() {
    let sources = diff_sources();
    let mut a = app();
    open_diff(&mut a, &sources);
    let timeline_before = a.timeline.len();
    for k in [Key::Right, Key::PageDown, Key::Down, Key::Char('C')] {
        key(&mut a, k, &sources);
    }
    assert_eq!(
        a.timeline.len(),
        timeline_before,
        "viewing emits no run event"
    );
    assert!(!a.run_active && !a.submit_pending);
    assert_eq!(a.diff, Some(sample_diff()), "the snapshot is never mutated");
    let text = draw(&a, 100, 30);
    assert!(!text.contains("applied"), "{text}");
    assert!(!text.contains("written to disk"), "{text}");
}

#[test]
fn diff_unavailable_source_renders_honest_message() {
    let sources = super::test_sources(None, None, None, false);
    let mut a = app();
    open_diff(&mut a, &sources);
    assert!(a.diff_unavailable);
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("Diff is not available in this build; nothing was opened or changed"),
        "{text}"
    );
}

fn multi_hunk_diff() -> DiffSnapshot {
    diff(vec![
        file(
            "a.rs",
            1,
            0,
            vec![
                hunk("@@ -1 +1 @@", vec![dline(DiffLineKind::Context, " a")]),
                hunk("@@ -5 +5 @@", vec![dline(DiffLineKind::Context, " b")]),
            ],
            false,
        ),
        file(
            "b.rs",
            1,
            0,
            vec![
                hunk("@@ -1 +1 @@", vec![dline(DiffLineKind::Context, " c")]),
                hunk("@@ -5 +5 @@", vec![dline(DiffLineKind::Context, " d")]),
                hunk("@@ -9 +9 @@", vec![dline(DiffLineKind::Context, " e")]),
            ],
            false,
        ),
        file(
            "c.rs",
            1,
            0,
            vec![hunk(
                "@@ -1 +1 @@",
                vec![dline(DiffLineKind::Context, " f")],
            )],
            false,
        ),
    ])
}

#[test]
fn diff_jump_map_renders_file_i_of_n_and_hunk_j_of_m() {
    let sources = super::test_sources(None, Some(multi_hunk_diff()), None, false);
    let mut a = app();
    open_diff(&mut a, &sources);
    let text = draw(&a, 100, 30);
    assert!(text.contains("file 1/3 · hunk 1/2"), "{text}");
}

#[test]
fn diff_jump_map_next_prev_file_and_hunk_clamp_and_update_coordinates() {
    let sources = super::test_sources(None, Some(multi_hunk_diff()), None, false);
    let mut a = app();
    open_diff(&mut a, &sources);

    key(&mut a, Key::PageDown, &sources);
    assert_eq!(a.diff_hunk, 1);
    key(&mut a, Key::PageDown, &sources);
    assert_eq!(a.diff_hunk, 1, "hunk must clamp at the last hunk");
    key(&mut a, Key::PageUp, &sources);
    assert_eq!(a.diff_hunk, 0);
    key(&mut a, Key::PageUp, &sources);
    assert_eq!(a.diff_hunk, 0, "hunk must clamp at the first hunk");

    key(&mut a, Key::Right, &sources);
    assert_eq!(a.diff_file, 1);
    assert_eq!(a.diff_hunk, 0, "changing file resets hunk/line");
    key(&mut a, Key::Right, &sources);
    assert_eq!(a.diff_file, 2);
    key(&mut a, Key::Right, &sources);
    assert_eq!(a.diff_file, 2, "file must clamp at the last file");
    key(&mut a, Key::Left, &sources);
    assert_eq!(a.diff_file, 1);

    key(&mut a, Key::PageDown, &sources);
    let text = draw(&a, 100, 30);
    assert!(text.contains("file 2/3 · hunk 2/3"), "{text}");
}

#[test]
fn diff_close_restores_timeline_position_and_reopen_restores_diff_position() {
    let sources = diff_sources();
    let mut a = app();
    for _ in 0..10 {
        a.apply(UiEvent::Frame(frame("tool_call_started")));
    }
    a.focus = Focus::Timeline;
    a.scroll = 5;
    a.scroll_held = true;
    open_diff(&mut a, &sources);
    key(&mut a, Key::Right, &sources);
    key(&mut a, Key::Down, &sources);
    let coords = (a.diff_file, a.diff_hunk, a.diff_line);
    assert_eq!(coords, (1, 0, 1));

    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.scroll, 5, "timeline position restored");
    assert!(a.scroll_held);
    assert_eq!(a.focus, Focus::Timeline);

    open_diff(&mut a, &sources);
    assert_eq!(
        (a.diff_file, a.diff_hunk, a.diff_line),
        coords,
        "diff coordinates survive close/reopen"
    );
}

// ---- inline approval block ----------------------------------------------

#[test]
fn approval_block_shows_command_cwd_and_risk_before_decision() {
    let sources = approval_sources();
    let a = app_with_approval(&sources);
    assert_eq!(a.approval_decision, ApprovalDecision::Pending);
    let text = draw(&a, 100, 30);
    for needle in [
        "! Approval required",
        "npm test -- --updateSnapshot",
        "Working directory:",
        "C:\\projects\\nx",
        "Reason / risk:",
        "This command may rewrite snapshot files.",
        "DangerousShell",
        "[A] Approve once",
        "[D] Deny",
        "[I] Inspect",
        "[Esc] Decide later",
    ] {
        assert!(
            text.contains(needle),
            "approval missing '{needle}':\n{text}"
        );
    }
}

#[test]
fn approval_inspect_expands_command_without_deciding() {
    let long = "npm test -- --updateSnapshot --runInBand --coverage --verbose --ci";
    let sources = super::test_sources(
        None,
        None,
        Some(Some(request(long, "/work", RiskClass::Mutating, "reason"))),
        false,
    );
    let mut a = app_with_approval(&sources);
    let truncated = draw(&a, 100, 30);
    assert!(!truncated.contains(long), "long command is collapsed first");
    key(&mut a, Key::Char('I'), &sources);
    assert!(a.approval_inspect);
    assert_eq!(a.approval_decision, ApprovalDecision::Pending);
    let expanded = draw(&a, 100, 30);
    assert!(
        expanded.contains(long),
        "Inspect must show the full command"
    );
    assert!(!expanded.contains("Approved locally"));
    assert!(!expanded.contains("Denied locally"));
}

#[test]
fn approval_pending_never_auto_resolves_on_tick_frame_or_overlay_toggle() {
    let sources = approval_sources();
    let mut a = app_with_approval(&sources);
    for _ in 0..100 {
        a.apply(UiEvent::Tick);
    }
    a.apply(UiEvent::Frame(frame("tool_call_completed")));
    key(&mut a, Key::F1, &sources);
    key(&mut a, Key::Esc, &sources);
    key(&mut a, Key::CtrlK, &sources);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.approval_decision, ApprovalDecision::Pending);
    assert_eq!(a.state, RunState::WaitingApproval);
}

#[test]
fn approval_esc_is_defer_not_a_decision() {
    let sources = approval_sources();
    let mut a = app_with_approval(&sources);
    a.composer.set_draft("draft".to_string());
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.approval_decision, ApprovalDecision::Pending);
    assert_eq!(a.state, RunState::WaitingApproval);
    assert_eq!(a.composer.draft(), "draft");
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("Deferred") || text.contains("still pending"),
        "{text}"
    );
    assert!(!text.contains("Approved locally"));
    assert!(!text.contains("Denied locally"));
}

#[test]
fn approval_approve_deny_and_run_cancel_are_distinct() {
    let sources = approval_sources();

    let mut approved = app_with_approval(&sources);
    key(&mut approved, Key::Char('A'), &sources);
    assert_eq!(approved.approval_decision, ApprovalDecision::Approved);
    assert!(!approved.cancel_pending);

    let mut denied = app_with_approval(&sources);
    key(&mut denied, Key::Char('D'), &sources);
    assert_eq!(denied.approval_decision, ApprovalDecision::Denied);
    assert!(!denied.cancel_pending);

    let mut cancelled = app_with_approval(&sources);
    let cmd = cancelled.apply(UiEvent::Key(Key::CtrlC));
    assert_eq!(cmd, LoopCmd::RequestCancel);
    assert_eq!(cancelled.approval_decision, ApprovalDecision::Pending);
    assert!(cancelled.cancel_pending);
    // A repeat is swallowed, just like a normal run cancel.
    assert_eq!(cancelled.apply(UiEvent::Key(Key::CtrlC)), LoopCmd::Continue);
}

#[test]
fn approval_decision_is_local_only_and_never_fabricates_resume() {
    let sources = approval_sources();
    let mut a = app_with_approval(&sources);
    let state = a.state;
    key(&mut a, Key::Char('A'), &sources);
    assert!(!a.run_active && !a.submit_pending);
    assert_eq!(a.state, state);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Approved locally"), "{text}");
    assert!(
        text.contains("decision recorded locally; no agent backend in this build"),
        "{text}"
    );
    for fabricated in ["resumed", "Resuming", "agent continued", "agent resumed"] {
        assert!(
            !text.contains(fabricated),
            "fabricated resume: {fabricated}"
        );
    }
}

// ---- command palette -----------------------------------------------------

#[test]
fn palette_clap_entries_match_help_grammar() {
    use clap::CommandFactory;

    fn clap_paths(command: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let name = command.get_name().to_string();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix} {name}")
        };
        out.push(path.clone());
        for child in command.get_subcommands() {
            if child.get_name() == "help" {
                continue;
            }
            clap_paths(child, &path, out);
        }
    }

    let mut expected = Vec::new();
    let root = crate::Cli::command();
    for child in root.get_subcommands() {
        if child.get_name() == "help" {
            continue;
        }
        clap_paths(child, "", &mut expected);
    }
    expected.sort();

    let catalog = super::sources().commands();
    let mut actual: Vec<String> = catalog
        .iter()
        .filter(|spec| spec.origin == CommandOrigin::Cli)
        .map(|spec| spec.path.clone())
        .collect();
    actual.sort();
    assert_eq!(
        actual, expected,
        "palette grammar must equal --help grammar"
    );

    // Walking `get_arguments()` too: every Clap argument id is carried into
    // the projection's `about`, proving the same tree was visited.
    for child in root.get_subcommands() {
        if child.get_name() == "help" {
            continue;
        }
        let path = child.get_name().to_string();
        let spec = catalog
            .iter()
            .find(|spec| spec.origin == CommandOrigin::Cli && spec.path == path)
            .unwrap_or_else(|| panic!("missing spec for {path}"));
        for argument in child.get_arguments() {
            let id = argument.get_id().as_str();
            assert!(
                spec.about.contains(id) || spec.path.contains(id),
                "argument '{id}' from {path} missing in palette projection"
            );
        }
    }
}

#[test]
fn palette_unavailable_actions_are_disabled_with_reason() {
    let catalog = super::sources().commands();
    let mut a = app();
    a.set_command_specs(catalog);
    assert!(a.filtered_commands().iter().any(|spec| {
        spec.origin == CommandOrigin::Cli
            && matches!(&spec.availability, Availability::Unavailable(reason) if !reason.is_empty())
    }));
    key(&mut a, Key::CtrlK, &super::sources());
    for ch in "run".chars() {
        key(&mut a, Key::Char(ch), &super::sources());
    }
    let text = draw(&a, 100, 30);
    assert!(text.contains("[unavailable:"), "{text}");
    let sources = super::sources();
    let cmd = a.apply(UiEvent::Key(Key::Enter));
    drive(&mut a, cmd.clone(), &sources);
    assert!(!matches!(cmd, LoopCmd::Submit(_)));
    assert_eq!(a.overlay, Overlay::None);
    let text = draw(&a, 100, 30);
    assert!(text.contains("not available"), "{text}");
}

#[test]
fn palette_opens_filters_and_runs_local_action_only() {
    let sources = super::sources();
    let mut a = app();
    key(&mut a, Key::CtrlK, &sources);
    assert_eq!(a.overlay, Overlay::Palette);
    for ch in "diff".chars() {
        key(&mut a, Key::Char(ch), &sources);
    }
    assert_eq!(a.filtered_commands().len(), 1);
    let cmd = a.apply(UiEvent::Key(Key::Enter));
    drive(&mut a, cmd.clone(), &sources);
    assert!(!matches!(cmd, LoopCmd::Submit(_)));
    assert_eq!(a.overlay, Overlay::Diff);
    assert!(!a.run_active);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn palette_close_preserves_run_draft_and_viewport() {
    let sources = super::sources();
    let mut a = app();
    a.composer.set_draft("draft".to_string());
    a.focus = Focus::Timeline;
    a.scroll = 6;
    a.scroll_held = true;
    key(&mut a, Key::CtrlK, &sources);
    for ch in "help".chars() {
        key(&mut a, Key::Char(ch), &sources);
    }
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.overlay, Overlay::None);
    assert_eq!(a.composer.draft(), "draft");
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 6);
    assert!(a.scroll_held);
}

#[test]
fn help_overlay_lists_phase6_keys_and_no_fake_actions() {
    let sources = super::sources();
    let mut a = app();
    key(&mut a, Key::F1, &sources);
    let text = draw(&a, 100, 30);
    assert!(text.contains("KEYS"), "{text}");
    for key_text in ["Ctrl+K", "f freeze", "D delete", "PgUp/PgDn", "Esc defer"] {
        assert!(
            text.contains(key_text),
            "help missing '{key_text}':\n{text}"
        );
    }
    assert!(!text.contains("deferred to a later build"), "{text}");
    assert!(!text.contains("not in this build"), "{text}");
    assert!(!text.contains("F5"), "unhandled keys must not be listed");
    assert!(!text.contains("Retry"), "no fabricated retry action");
}

// ---- micro-UX ------------------------------------------------------------

#[test]
fn scroll_hold_hint_shows_new_event_count_and_enter_returns_to_live() {
    let sources = super::sources();
    let mut a = app();
    for _ in 0..10 {
        a.apply(UiEvent::Frame(frame("tool_call_started")));
    }
    a.focus = Focus::Timeline;
    key(&mut a, Key::Up, &sources);
    assert!(a.scroll_held);
    for _ in 0..3 {
        a.apply(UiEvent::Frame(frame("tool_call_completed")));
    }
    let text = draw(&a, 100, 30);
    assert!(text.contains("3 new events"), "{text}");
    key(&mut a, Key::Enter, &sources);
    assert!(!a.scroll_held, "Enter returns to the live tail");
    assert_eq!(a.scroll, usize::MAX);
    assert!(a.composer.is_empty(), "return-to-live did not submit");
}

#[test]
fn event_density_guard_folds_consecutive_identical_frames_preserving_order_and_counts() {
    let mut a = app();
    for _ in 0..5 {
        a.apply(UiEvent::Frame(
            frame("tool_call_completed").with("name", json!("Read")),
        ));
    }
    a.apply(UiEvent::Frame(frame("run_started")));
    let rows = fold(&a.timeline, &[], FOLD_THRESHOLD);
    assert_eq!(rows.len(), 2, "five identical tool rows fold into one");
    match &rows[0] {
        RenderRow::Folded { count, label, .. } => {
            assert_eq!(*count, 5);
            assert_eq!(label, "tool_call_completed");
        }
        RenderRow::Entry(..) => panic!("expected a folded row"),
    }
    assert!(matches!(rows[1], RenderRow::Entry(5, _)), "order preserved");
    let text = draw(&a, 100, 30);
    assert!(text.contains("5 repeated tool_call_completed"), "{text}");
    assert!(text.contains("run_started"), "{text}");
}

#[test]
fn fold_expansion_restores_original_entries_in_order() {
    let mut a = app();
    for _ in 0..5 {
        a.apply(UiEvent::Frame(
            frame("tool_call_completed").with("name", json!("Read")),
        ));
    }
    a.apply(UiEvent::Frame(frame("run_started")));
    let rows = fold(&a.timeline, &[0], FOLD_THRESHOLD);
    assert_eq!(rows.len(), 6);
    for (index, row) in rows.iter().enumerate() {
        match row {
            RenderRow::Entry(original, entry) => {
                assert_eq!(*original, index);
                assert_eq!(entry, &a.timeline[index]);
            }
            RenderRow::Folded { .. } => panic!("expanded anchor must not fold"),
        }
    }
    // The Inspect key switches the view without deleting history.
    let sources = super::sources();
    a.focus = Focus::Timeline;
    key(&mut a, Key::Char('I'), &sources);
    assert_eq!(a.expanded, vec![0]);
    assert_eq!(a.timeline.len(), 6, "folding never removes run events");
}

#[test]
fn errors_and_approvals_are_never_folded() {
    let mut a = app();
    for _ in 0..5 {
        a.apply(UiEvent::Frame(
            frame("tool_call_completed").with("name", json!("Read")),
        ));
    }
    for _ in 0..3 {
        a.apply(UiEvent::Frame(frame("approval_required")));
    }
    for _ in 0..3 {
        a.apply(UiEvent::HostRejected);
    }
    let rows = fold(&a.timeline, &[], FOLD_THRESHOLD);
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, RenderRow::Folded { .. }))
            .count(),
        1,
        "only the tool group folds"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, RenderRow::Entry(_, entry) if entry.label == "approval_required"))
            .count(),
        3
    );
    assert_eq!(
        rows.iter()
            .filter(
                |row| matches!(row, RenderRow::Entry(_, entry) if entry.kind == EntryKind::Error)
            )
            .count(),
        3
    );
}

#[test]
fn focus_memory_restores_focus_and_selection_after_each_overlay() {
    let sources = super::test_sources(
        Some(vec![
            session("1", "One", SessionStatus::Running, "now", true),
            session("2", "Two", SessionStatus::Completed, "ago", false),
            session("3", "Three", SessionStatus::Error, "old", false),
        ]),
        Some(sample_diff()),
        None,
        false,
    );
    let mut a = app();
    a.focus = Focus::Timeline;
    a.scroll = 7;
    a.scroll_held = true;

    key(&mut a, Key::F1, &sources);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 7);
    assert!(a.scroll_held);

    open_sessions(&mut a, &sources);
    key(&mut a, Key::Down, &sources);
    assert_eq!(a.sessions_selection, 1);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 7);

    open_sessions(&mut a, &sources);
    assert_eq!(a.sessions_selection, 1, "selection survives close/reopen");
    key(&mut a, Key::Esc, &sources);

    open_diff(&mut a, &sources);
    key(&mut a, Key::Right, &sources);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 7);

    key(&mut a, Key::CtrlK, &sources);
    key(&mut a, Key::Esc, &sources);
    assert_eq!(a.focus, Focus::Timeline);
    assert_eq!(a.scroll, 7);
    assert_eq!(a.composer.draft(), "");
}

#[test]
fn approval_return_point_reports_local_decision_without_fabricated_resume() {
    let sources = approval_sources();
    let mut a = app_with_approval(&sources);
    key(&mut a, Key::Char('D'), &sources);
    assert_eq!(a.approval_decision, ApprovalDecision::Denied);
    assert!(a.approval_return.is_some());
    let text = draw(&a, 100, 30);
    assert!(text.contains("Denied locally"), "{text}");
    assert!(
        text.contains("decision recorded locally; no agent backend in this build"),
        "{text}"
    );
    assert!(!text.contains("resumed"), "no fabricated continuation");
    assert!(!a.run_active && !a.submit_pending);
}

#[test]
fn file_touch_trace_appears_only_with_diff_data() {
    let sources = diff_sources();
    let unicode = GlyphSet::unicode().spine;
    let ascii = GlyphSet::ascii().spine;
    assert_eq!((unicode, ascii), ("│", "|"), "glyph map from the brief");

    // Unicode: the touched row carries the spine, the untouched one does not.
    let mut a = app();
    open_diff(&mut a, &sources);
    let text = draw(&a, 100, 30);
    let touched = format!("{unicode} src/auth.rs");
    let untouched = format!("{unicode} src/session.rs");
    assert!(
        text.contains(touched.as_str()),
        "touched marker from data: {text}"
    );
    assert!(
        !text.contains(untouched.as_str()),
        "untouched file has no marker: {text}"
    );

    // ASCII: the very same trace uses the ASCII spine from GlyphSet.
    let mut ascii_app = App::new(caps_ascii(), snapshot(), 100, 30);
    open_diff(&mut ascii_app, &sources);
    let ascii_text = draw(&ascii_app, 100, 30);
    let ascii_touched = format!("{ascii} src/auth.rs");
    let ascii_untouched = format!("{ascii} src/session.rs");
    assert!(
        ascii_text.contains(ascii_touched.as_str()),
        "ASCII touch marker from data: {ascii_text}"
    );
    assert!(
        !ascii_text.contains(ascii_untouched.as_str()),
        "ASCII untouched file has no marker: {ascii_text}"
    );
    assert!(
        !ascii_text.contains(unicode),
        "ASCII mode must not emit the Unicode spine: {ascii_text}"
    );

    // No source data: no marker in either mode.
    let unavailable = super::test_sources(None, None, None, false);
    let mut b = app();
    open_diff(&mut b, &unavailable);
    let text = draw(&b, 100, 30);
    assert!(
        !text.contains(unicode),
        "no touch trace without diff data: {text}"
    );
    assert!(
        !text.contains(ascii),
        "no touch trace without diff data: {text}"
    );

    let mut ascii_empty = App::new(caps_ascii(), snapshot(), 100, 30);
    open_diff(&mut ascii_empty, &unavailable);
    let text = draw(&ascii_empty, 100, 30);
    assert!(
        !text.contains(unicode),
        "no touch trace without diff data: {text}"
    );
    assert!(
        !text.contains(ascii),
        "no touch trace without diff data: {text}"
    );
}

#[test]
fn quiet_copy_shows_copied_only_after_successful_copy() {
    let sources = super::test_sources(None, Some(sample_diff()), None, false);
    let mut a = app();
    open_diff(&mut a, &sources);
    key(&mut a, Key::Char('C'), &sources);
    assert!(!a.copied_label, "shipped sink must not claim a copy");
    let text = draw(&a, 100, 30);
    assert!(!text.contains("Copied"), "{text}");
    assert!(text.contains("nothing was copied"), "{text}");

    let ok_sources = super::test_sources(None, Some(sample_diff()), None, true);
    let mut b = app();
    open_diff(&mut b, &ok_sources);
    key(&mut b, Key::Char('C'), &ok_sources);
    assert!(b.copied_label);
    assert!(draw(&b, 100, 30).contains("Copied"));
}

#[test]
fn output_freeze_is_distinct_from_agent_pause_and_counts_new_events() {
    let sources = super::sources();
    let mut a = app();
    a.apply(UiEvent::Frame(
        frame("run_started").with("status", json!("running")),
    ));
    a.focus = Focus::Timeline;
    key(&mut a, Key::Char('f'), &sources);
    assert!(a.output_frozen);
    assert_eq!(
        a.state,
        RunState::Running,
        "freeze must not pause the agent"
    );
    assert!(!a.run_active);
    for _ in 0..2 {
        a.apply(UiEvent::Frame(frame("tool_call_completed")));
    }
    assert_eq!(a.new_events_count(), 2);
    let text = draw(&a, 100, 30);
    assert!(text.contains("live output paused · 2 new events"), "{text}");
    assert!(text.contains("RUNNING"), "{text}");
    assert!(!text.contains("PAUSED"), "{text}");
}

#[test]
fn output_freeze_keeps_frozen_rows_visible_when_scroll_is_at_tail() {
    let sources = super::sources();
    let mut a = app();
    a.apply(UiEvent::Frame(
        frame("run_started").with("status", json!("running")),
    ));
    // Use the ACTUAL rendered timeline height (not TIMELINE_MIN, which is only
    // the layout floor), so `effective_scroll(height)` in the renderer can be
    // pushed beyond `frozen_at_len` by the frames added after freezing.
    let Resolution::Usable(metrics) = layout::resolve(100, 30) else {
        panic!("30-row terminal must be usable")
    };
    let viewport = usize::from(30u16.saturating_sub(
        layout::STATUS_ROWS + 2 * layout::RULE_ROWS + metrics.composer_lines + layout::HINTS_ROWS,
    ));
    // Seed more than a viewport plus five frames while still following the
    // tail, so a full-timeline scroll clamp would exceed the frozen slice.
    for i in 0..viewport + 5 {
        a.apply(UiEvent::Frame(
            frame("assistant_progress").with("detail", json!(format!("frozen-{i}"))),
        ));
    }
    assert_eq!(
        a.scroll,
        usize::MAX,
        "freeze must engage while following tail"
    );
    assert!(!a.scroll_held, "no manual scroll hold was engaged");
    a.focus = Focus::Timeline;
    let frozen_len = a.timeline.len();

    key(&mut a, Key::Char('f'), &sources);
    assert!(a.output_frozen);
    assert_eq!(a.visible_len(), frozen_len);
    assert!(
        a.effective_scroll(viewport) > 0,
        "tail clamp must stay inside the frozen slice"
    );

    // Add enough new frames that the OLD full-timeline clamp (tail - viewport)
    // would skip past every frozen row and render an empty viewport.
    for i in 0..viewport + 5 {
        a.apply(UiEvent::Frame(
            frame("assistant_progress").with("detail", json!(format!("live-{i}"))),
        ));
    }
    assert_eq!(a.new_events_count(), viewport + 5);
    assert_eq!(
        a.state,
        RunState::Running,
        "freeze must not pause the agent"
    );

    let text = draw(&a, 100, 30);
    let last_frozen = format!("frozen-{}", viewport + 4);
    assert!(
        text.contains(&last_frozen),
        "frozen content must remain visible:\n{text}"
    );
    assert!(
        !text.contains("live-"),
        "new frames must not leak into the frozen view:\n{text}"
    );
    assert!(
        text.contains(&format!("live output paused · {} new events", viewport + 5)),
        "honest new-event counter:\n{text}"
    );
    assert!(text.contains("RUNNING"), "state unchanged:\n{text}");
    assert!(!text.contains("PAUSED"), "not an agent pause:\n{text}");
}

#[test]
fn output_freeze_return_to_live_replays_all_events() {
    let sources = super::sources();
    let mut a = app();
    a.apply(UiEvent::Frame(frame("run_started")));
    a.apply(UiEvent::Frame(frame("tool_call_started")));
    a.apply(UiEvent::Frame(frame("tool_call_completed")));
    a.focus = Focus::Timeline;
    key(&mut a, Key::Char('f'), &sources);
    assert_eq!(a.visible_len(), 3);
    a.apply(UiEvent::Frame(frame("assistant_progress")));
    a.apply(UiEvent::Frame(frame("diff_summary")));
    assert_eq!(a.visible_len(), 3, "frozen view keeps its original length");
    key(&mut a, Key::Enter, &sources);
    assert!(!a.output_frozen);
    assert_eq!(a.visible_len(), 5);
    let text = draw(&a, 100, 30);
    for label in [
        "run_started",
        "tool_call_started",
        "tool_call_completed",
        "assistant_progress",
        "diff_summary",
    ] {
        assert!(text.contains(label), "missing '{label}':\n{text}");
    }
}

#[test]
fn no_op_honesty_renders_only_from_a_real_frame() {
    let mut a = app();
    assert!(
        !draw(&a, 100, 30).contains("No changes needed"),
        "do not invent a no-op result"
    );
    a.apply(UiEvent::Frame(
        frame("assistant_message").with("message", json!("No changes needed")),
    ));
    assert!(
        draw(&a, 100, 30).contains("No changes needed"),
        "render only real frame text"
    );
}

#[test]
fn context_freshness_and_undo_are_absent_without_backend_data() {
    let sources = super::sources();
    let mut a = app();
    let text = draw(&a, 100, 30);
    for fabricated in ["Undo", "stale context", "context is stale", "resume agent"] {
        assert!(
            !text.contains(fabricated),
            "no backend proof for '{fabricated}'"
        );
    }
    key(&mut a, Key::F1, &sources);
    let help = draw(&a, 100, 30);
    assert!(!help.contains("Undo"), "no undo action is advertised");
    assert!(!help.contains("Ctrl+Z"), "Ctrl+Z is deliberately unused");
}

#[test]
fn resize_preserves_overlay_selection_filter_and_coordinates() {
    let sources = super::test_sources(
        Some(vec![
            session("1", "One", SessionStatus::Running, "now", true),
            session("2", "Two", SessionStatus::Completed, "ago", false),
        ]),
        Some(sample_diff()),
        None,
        false,
    );
    let mut a = app();
    open_sessions(&mut a, &sources);
    for ch in "o".chars() {
        key(&mut a, Key::Char(ch), &sources);
    }
    key(&mut a, Key::Down, &sources);
    a.apply(UiEvent::Resize(80, 24));
    assert_eq!(a.overlay, Overlay::Sessions);
    assert_eq!(a.sessions_selection, 1);
    assert_eq!(a.sessions_filter, "o");
    assert_eq!(a.size, (80, 24));

    key(&mut a, Key::Esc, &sources);
    open_diff(&mut a, &sources);
    key(&mut a, Key::Right, &sources);
    key(&mut a, Key::Down, &sources);
    a.apply(UiEvent::Resize(120, 40));
    assert_eq!(a.overlay, Overlay::Diff);
    assert_eq!((a.diff_file, a.diff_hunk, a.diff_line), (1, 0, 1));
}

#[test]
fn ascii_no_color_keep_all_overlay_workflows_available() {
    let caps = resolve(CapabilityInput {
        color: "never",
        ascii: true,
        motion: "none",
        no_color: Some("1"),
        term: Some("dumb"),
        lang: Some("C"),
    });
    let mut a = App::new(caps, snapshot(), 100, 30);
    let sessions = sessions_sources();
    open_sessions(&mut a, &sessions);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Sessions"), "{text}");
    assert!(text.contains("Fix authentication"), "{text}");
    assert!(text.contains("Esc back"), "ASCII hints: {text}");
    key(&mut a, Key::Esc, &sessions);

    let diff = diff_sources();
    open_diff(&mut a, &diff);
    let text = draw(&a, 100, 30);
    assert!(text.contains("Diff"), "{text}");
    assert!(text.contains("src/auth.rs"), "{text}");
    key(&mut a, Key::Esc, &diff);

    let approval = approval_sources();
    load_approval(&mut a, &approval);
    let text = draw(&a, 100, 30);
    assert!(text.contains("! Approval required"), "{text}");
    assert!(text.contains("[A] Approve once"), "{text}");
}
