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
    parse_git_head, short_workspace, App, Focus, LoopCmd, Overlay, RunState, StatusSnapshot,
    TimelineEntry,
};
use super::capabilities::{resolve, CapabilityInput};
use super::events::Key;
use super::glyphs::GlyphSet;
use super::layout::{self, Resolution};
use super::render;
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

#[test]
fn ctrl_k_is_deferred_no_fake_palette() {
    let mut a = app();
    a.apply(super::events::UiEvent::Key(Key::CtrlK));
    let text = draw(&a, 100, 30);
    assert!(
        text.contains("Command palette is not available in this build"),
        "honest deferral:\n{text}"
    );
    assert!(!text.contains("Recent commands"), "no fake palette list");
}

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
