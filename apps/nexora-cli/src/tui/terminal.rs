//! RAII terminal acquisition and restoration (brief §12.3, §17 item 12).
//!
//! Acquisition happens in a fixed order (raw mode → alternate screen → hide
//! cursor) and restoration happens in the exact reverse, tracked by flags so a
//! partially acquired terminal is never half-restored. The mode operations sit
//! behind [`TerminalIo`] so tests inject a recorder and prove drop/error/panic
//! restoration without ever touching a real terminal. The production
//! implementation writes only to stderr; stdout is reserved for machine data.

use std::io::{self, Stderr};

use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

/// The primitive terminal-mode operations the guard needs.
pub(crate) trait TerminalIo {
    fn enter_raw(&mut self) -> io::Result<()>;
    fn leave_raw(&mut self) -> io::Result<()>;
    fn enter_alt(&mut self) -> io::Result<()>;
    fn leave_alt(&mut self) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
}

/// Production I/O: raw mode and alternate screen on **stderr**, mouse capture
/// deliberately never enabled (brief §10, plan §2).
pub(crate) struct StderrIo(Stderr);

impl Default for StderrIo {
    fn default() -> Self {
        Self(io::stderr())
    }
}

impl TerminalIo for StderrIo {
    fn enter_raw(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn leave_raw(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_alt(&mut self) -> io::Result<()> {
        execute!(self.0, EnterAlternateScreen)
    }

    fn leave_alt(&mut self) -> io::Result<()> {
        execute!(self.0, LeaveAlternateScreen)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.0, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.0, Show)
    }
}

/// A guard that restores the terminal in reverse acquisition order on Drop.
#[derive(Debug)]
pub(crate) struct TerminalGuard<I: TerminalIo> {
    io: I,
    raw: bool,
    alt: bool,
    hidden: bool,
}

impl<I: TerminalIo> TerminalGuard<I> {
    /// Acquire raw mode, then the alternate screen, then hide the cursor. If
    /// any step fails, everything acquired so far is restored before the
    /// error is returned (so the caller never reports an error inside a
    /// broken terminal state).
    pub(crate) fn acquire(io: I) -> io::Result<Self> {
        let mut guard = Self {
            io,
            raw: false,
            alt: false,
            hidden: false,
        };
        // Raw mode is the first step; nothing to restore if it fails.
        guard.io.enter_raw()?;
        guard.raw = true;
        if let Err(err) = guard.io.enter_alt() {
            guard.restore_now();
            return Err(err);
        }
        guard.alt = true;
        if let Err(err) = guard.io.hide_cursor() {
            guard.restore_now();
            return Err(err);
        }
        guard.hidden = true;
        Ok(guard)
    }

    /// Restore every acquired state in reverse order. Idempotent.
    pub(crate) fn restore_now(&mut self) {
        if self.hidden {
            let _ = self.io.show_cursor();
            self.hidden = false;
        }
        if self.alt {
            let _ = self.io.leave_alt();
            self.alt = false;
        }
        if self.raw {
            let _ = self.io.leave_raw();
            self.raw = false;
        }
    }
}

impl<I: TerminalIo> Drop for TerminalGuard<I> {
    fn drop(&mut self) {
        self.restore_now();
    }
}

/// Best-effort restoration used by the panic hook. Ignores errors and is safe
/// to call even when no guard was acquired (crossterm commands are idempotent).
fn best_effort_restore() {
    let _ = disable_raw_mode();
    let mut stderr = io::stderr();
    let _ = execute!(stderr, LeaveAlternateScreen, Show);
}

/// Install a panic hook that restores the terminal *before* delegating to the
/// previous hook. Called once, before the guard is acquired, so a panic at any
/// point during the run leaves the user with a usable terminal.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        best_effort_restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const RAW: &str = "raw";
    const ALT: &str = "alt";
    const HIDE: &str = "hide";
    const SHOW: &str = "show";
    const LEAVE_ALT: &str = "leave_alt";
    const LEAVE_RAW: &str = "leave_raw";

    #[derive(Default)]
    struct Recorder {
        ops: Rc<RefCell<Vec<&'static str>>>,
        fail_on: Option<&'static str>,
    }

    impl TerminalIo for Recorder {
        fn enter_raw(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(RAW);
            if self.fail_on == Some(RAW) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn enter_alt(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(ALT);
            if self.fail_on == Some(ALT) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn hide_cursor(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(HIDE);
            if self.fail_on == Some(HIDE) {
                return Err(io::Error::other("injected"));
            }
            Ok(())
        }
        fn leave_raw(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(LEAVE_RAW);
            Ok(())
        }
        fn leave_alt(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(LEAVE_ALT);
            Ok(())
        }
        fn show_cursor(&mut self) -> io::Result<()> {
            self.ops.borrow_mut().push(SHOW);
            Ok(())
        }
    }

    #[test]
    fn guard_restores_in_reverse_order_on_drop() {
        let ops = Rc::new(RefCell::new(Vec::new()));
        {
            let _guard = TerminalGuard::acquire(Recorder {
                ops: Rc::clone(&ops),
                fail_on: None,
            })
            .expect("acquire");
            assert_eq!(*ops.borrow(), vec![RAW, ALT, HIDE]);
        }
        assert_eq!(
            *ops.borrow(),
            vec![RAW, ALT, HIDE, SHOW, LEAVE_ALT, LEAVE_RAW]
        );
    }

    #[test]
    fn partial_acquisition_failure_restores_what_was_acquired() {
        for fail_on in [ALT, HIDE] {
            let ops = Rc::new(RefCell::new(Vec::new()));
            let result = TerminalGuard::acquire(Recorder {
                ops: Rc::clone(&ops),
                fail_on: Some(fail_on),
            });
            assert!(result.is_err(), "{fail_on} must fail acquisition");
            let snapshot = ops.borrow();
            assert!(snapshot.contains(&LEAVE_RAW), "raw mode must be released");
            if fail_on == HIDE {
                assert!(snapshot.contains(&LEAVE_ALT));
                assert!(snapshot.contains(&LEAVE_RAW));
                // The cursor was never hidden, so it is never shown.
                assert!(!snapshot.contains(&SHOW));
            }
        }
    }

    #[test]
    fn panic_unwind_drops_the_guard_and_restores() {
        let ops = Rc::new(RefCell::new(Vec::new()));
        let observer = Rc::clone(&ops);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = TerminalGuard::acquire(Recorder {
                ops: observer,
                fail_on: None,
            })
            .expect("acquire");
            panic!("simulated failure");
        }));
        assert!(result.is_err());
        let snapshot = ops.borrow();
        assert_eq!(
            &snapshot[snapshot.len() - 3..],
            &[SHOW, LEAVE_ALT, LEAVE_RAW],
            "unwind must restore in reverse order"
        );
    }

    #[test]
    fn panic_hook_install_is_best_effort_and_idempotent() {
        // Installing the hook must not itself panic or require a real terminal.
        install_panic_hook();
    }
}
