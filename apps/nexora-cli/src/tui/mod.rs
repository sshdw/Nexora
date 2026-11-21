//! Phase-5 interactive TUI shell (issue #164, brief §5/§10/§12).
//!
//! Entered only from `main` when the bare command runs on three real terminals.
//! One input thread polls Crossterm (through `ratatui::crossterm`), an optional
//! run thread drives the existing `RunHost` seam and forwards real frames, and
//! the main loop applies [`UiEvent`]s to a single [`App`] and redraws only when
//! dirty. There is no second event model, no async runtime and no fabricated
//! agent state. The [`TerminalGuard`] is scoped so the alternate screen is left
//! before any error is reported.

mod app;
mod capabilities;
mod composer;
mod events;
mod glyphs;
mod layout;
mod render;
mod terminal;

#[cfg(test)]
mod tests;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use nexora_core::error::CliExitCode;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event;
use ratatui::Terminal;

use crate::error::CliError;
use crate::run::CapabilityArgs;

use app::{App, StatusSnapshot};
use capabilities::Capabilities;
use events::{channel, map_event, EventSender, UiEvent, POLL_INTERVAL};
use terminal::{StderrIo, TerminalGuard};

/// A cancellation request shared with the run thread. The mutex + condvar
/// mirror the project's run/forwarder pattern without any async runtime.
#[derive(Clone)]
struct CancelSignal {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl CancelSignal {
    fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn request(&self) {
        let (lock, cv) = &*self.inner;
        if let Ok(mut requested) = lock.lock() {
            *requested = true;
        }
        cv.notify_all();
    }

    fn is_requested(&self) -> bool {
        self.inner.0.lock().is_ok_and(|requested| *requested)
    }
}

fn terminal_error() -> CliError {
    CliError::validation("the terminal could not be initialized for interactive mode")
}

/// Enter the TUI. Returns the exit code reflecting the last command outcome.
/// The guard is a local of `run_inner`, so it is always dropped before this
/// function returns — including on error — and `main` reports afterwards.
pub(crate) fn run(caps: &CapabilityArgs) -> Result<CliExitCode, CliError> {
    terminal::install_panic_hook();
    let resolved = capabilities::from_env(caps);
    let snapshot = StatusSnapshot::detect();
    run_inner(caps, resolved, snapshot)
}

#[allow(clippy::too_many_lines)]
fn run_inner(
    caps: &CapabilityArgs,
    resolved: Capabilities,
    snapshot: StatusSnapshot,
) -> Result<CliExitCode, CliError> {
    let guard = TerminalGuard::acquire(StderrIo::default()).map_err(|_| terminal_error())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stderr())).map_err(|_| terminal_error())?;
    let size = terminal.size().map_err(|_| terminal_error())?;

    let (sender, receiver, shutdown) = channel();
    let cancel = CancelSignal::new();
    let mut app = App::new(resolved, snapshot, size.width, size.height);

    let input_handle = spawn_input(sender.clone(), Arc::clone(&shutdown));
    let mut run_handle: Option<JoinHandle<()>> = None;

    let outcome = event_loop(
        &mut terminal,
        &mut app,
        &receiver,
        &sender,
        &cancel,
        &mut run_handle,
        caps,
    );

    // Signal both workers and join before leaving the alternate screen.
    shutdown.store(true, Ordering::Release);
    cancel.request();
    let _ = input_handle.join();
    if let Some(handle) = run_handle.take() {
        let _ = handle.join();
    }
    // `guard` and `terminal` drop here, in reverse order, before `outcome`
    // is returned to `main` for reporting.
    drop(terminal);
    drop(guard);
    outcome
}

#[allow(clippy::too_many_lines)]
fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    receiver: &Receiver<UiEvent>,
    sender: &EventSender,
    cancel: &CancelSignal,
    run_handle: &mut Option<JoinHandle<()>>,
    caps: &CapabilityArgs,
) -> Result<CliExitCode, CliError> {
    loop {
        if app.dirty {
            terminal
                .draw(|frame| render::render(frame, app))
                .map_err(|_| terminal_error())?;
            app.dirty = false;
        }

        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(event) => match app.apply(event) {
                app::LoopCmd::Continue => {}
                app::LoopCmd::Submit(task) => {
                    if run_handle
                        .as_ref()
                        .is_none_or(std::thread::JoinHandle::is_finished)
                    {
                        *run_handle = Some(spawn_run(
                            sender.clone(),
                            task,
                            caps.color.clone(),
                            caps.ascii,
                            caps.motion.clone(),
                            cancel.clone(),
                        ));
                    }
                }
                app::LoopCmd::RequestCancel => {
                    cancel.request();
                    // One request through the existing host seam; repeats are
                    // swallowed by `App::cancel_pending` before reaching here.
                    crate::run::host().request_cancel();
                }
                app::LoopCmd::Quit(code) => return Ok(code),
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                app.apply(UiEvent::Tick);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(app.last_exit),
        }
    }
}

fn spawn_input(sender: EventSender, shutdown: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !shutdown.load(Ordering::Acquire) {
            let Ok(available) = event::poll(Duration::from_millis(100)) else {
                break;
            };
            if available {
                match event::read() {
                    Ok(evt) => {
                        if let Some(ui) = map_event(evt) {
                            sender.send(ui);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_run(
    sender: EventSender,
    prompt: String,
    color: String,
    ascii: bool,
    motion: String,
    cancel: CancelSignal,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let launch = crate::run::tui_launch(prompt, color, ascii, motion);
        let Ok(launch) = launch else {
            sender.send(UiEvent::HostRejected);
            sender.send(UiEvent::HostDone);
            return;
        };
        if let Ok(frames) = crate::run::host().start(&launch) {
            sender.send(UiEvent::HostAccepted);
            for frame in frames {
                if cancel.is_requested() {
                    break;
                }
                sender.send(UiEvent::Frame(frame));
            }
            sender.send(UiEvent::HostDone);
        } else {
            // Fixed, secret-free: the draft survives on every host failure.
            sender.send(UiEvent::HostRejected);
            sender.send(UiEvent::HostDone);
        }
    })
}
