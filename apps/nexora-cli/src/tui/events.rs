//! Input abstraction and `std::sync::mpsc` wiring for the TUI (brief §2.5).
//!
//! Crossterm is reached only through Ratatui's re-export; the input thread
//! translates its events into the small, owned [`Key`]/[`UiEvent`] vocabulary
//! that `App::apply` consumes. Nothing here is an async runtime: one input
//! thread, one optional run thread, one receiver in the main loop, and a
//! shutdown flag checked on every 100 ms poll.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::run::WireFrame;

/// Poll interval for the input thread and `recv_timeout` in the main loop.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The small set of logical keys the TUI understands. Printable characters
/// are carried in `Char`; everything else is an explicit variant so the
/// keyboard contract (brief §10) is exhaustive and testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    ShiftEnter,
    Esc,
    Tab,
    BackTab,
    PageUp,
    PageDown,
    Home,
    End,
    Backspace,
    Delete,
    /// Reliable newline fallback (`Ctrl+J`, or a lone LF from a paste).
    CtrlJ,
    CtrlC,
    CtrlD,
    CtrlK,
    F1,
    /// An ordinary printable character.
    Char(char),
    /// Any key we deliberately do not act on (including deferred shortcuts).
    Ignored,
}

/// Everything that can change `App` state. `WireFrame` carries a JSON map and
/// is `PartialEq` but not `Eq`, so this enum is `PartialEq` only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UiEvent {
    Key(Key),
    Resize(u16, u16),
    Tick,
    /// A real host frame, forwarded in emission order by the run thread.
    Frame(WireFrame),
    /// The host accepted a submitted task; the draft may now enter history.
    HostAccepted,
    /// The host rejected a submitted task; the draft must survive.
    HostRejected,
    /// The run thread has finished forwarding frames.
    HostDone,
}

/// Translate one crossterm event into zero or one logical UI event. Ignores key
/// release/repeat events and mouse events (mouse capture is never enabled).
/// The owned `Event` comes straight from the input thread's `event::read()`;
/// keeping it by value avoids a borrow that cannot outlive that thread.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub(crate) fn map_event(event: Event) -> Option<UiEvent> {
    match event {
        Event::Resize(cols, rows) => Some(UiEvent::Resize(cols, rows)),
        Event::Key(key) => map_key(key).map(UiEvent::Key),
        // Mouse capture is OFF; paste and focus events carry no TUI action.
        _ => None,
    }
}

fn map_key(key: KeyEvent) -> Option<Key> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && !alt && key.code == KeyCode::Char('c') {
        return Some(Key::CtrlC);
    }
    if ctrl && !alt && key.code == KeyCode::Char('d') {
        return Some(Key::CtrlD);
    }
    if ctrl && !alt && key.code == KeyCode::Char('k') {
        return Some(Key::CtrlK);
    }
    if ctrl && !alt && key.code == KeyCode::Char('j') {
        return Some(Key::CtrlJ);
    }
    if alt {
        return Some(Key::Ignored);
    }

    Some(match key.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => {
            if shift {
                Key::ShiftEnter
            } else {
                Key::Enter
            }
        }
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::F(1) => Key::F1,
        // A lone LF is how many terminals deliver Ctrl+J or a pasted newline;
        // treating it as a newline (never Enter) is what keeps paste from
        // auto-sending (brief §5.4).
        KeyCode::Char('\n') => Key::CtrlJ,
        KeyCode::Char(ch) if ch.is_control() => Key::Ignored,
        KeyCode::Char(ch) => Key::Char(ch),
        _ => Key::Ignored,
    })
}

/// The send half of the UI channel plus the shutdown flag shared with workers.
#[derive(Debug, Clone)]
pub(crate) struct EventSender {
    tx: Sender<UiEvent>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl EventSender {
    /// Send an event; a closed receiver means the main loop has exited.
    pub(crate) fn send(&self, event: UiEvent) {
        use std::sync::atomic::Ordering;
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        let _ = self.tx.send(event);
    }
}

/// Create a channel and its shared shutdown flag.
#[must_use]
pub(crate) fn channel() -> (
    EventSender,
    Receiver<UiEvent>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let (tx, rx) = mpsc::channel();
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        EventSender {
            tx,
            shutdown: Arc::clone(&shutdown),
        },
        rx,
        shutdown,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }

    #[test]
    fn control_keys_map_to_the_logical_vocabulary() {
        for (code, mods, expected) in [
            (KeyCode::Char('c'), KeyModifiers::CONTROL, Key::CtrlC),
            (KeyCode::Char('d'), KeyModifiers::CONTROL, Key::CtrlD),
            (KeyCode::Char('k'), KeyModifiers::CONTROL, Key::CtrlK),
            (KeyCode::Char('j'), KeyModifiers::CONTROL, Key::CtrlJ),
            (KeyCode::Char('\n'), KeyModifiers::NONE, Key::CtrlJ),
        ] {
            assert_eq!(map_event(key(code, mods)), Some(UiEvent::Key(expected)));
        }
    }

    #[test]
    fn paste_newlines_are_never_enter() {
        for code in [KeyCode::Char('\n'), KeyCode::Char('j')] {
            let mods = if code == KeyCode::Char('j') {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            };
            assert_eq!(map_event(key(code, mods)), Some(UiEvent::Key(Key::CtrlJ)));
        }
        assert_eq!(
            map_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(UiEvent::Key(Key::Enter))
        );
    }

    #[test]
    fn shift_enter_and_resize_are_preserved() {
        assert_eq!(
            map_event(key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(UiEvent::Key(Key::ShiftEnter))
        );
        assert_eq!(
            map_event(Event::Resize(80, 24)),
            Some(UiEvent::Resize(80, 24))
        );
    }
}
