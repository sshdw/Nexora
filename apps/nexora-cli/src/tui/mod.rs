//! Interactive TUI shell (issue #164) with the phase-6a UI depth scaffold
//! (issue #165): sessions/diff overlays, inline approvals, Clap-derived palette
//! and micro-UX — all backed by read-only sources that are `Unavailable` in the
//! shipped binary (brief §5/§6/§8/§10/§12).
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
mod sources;
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
use sources::Sources;
use terminal::{StderrIo, TerminalGuard};

/// The only source bundle linked into the shipped binary: real read-only
/// runtime projections, the Clap grammar, and a best-effort OS clipboard.
#[must_use]
fn sources() -> Sources {
    Sources::shipped()
}

/// Test-only injection mirroring `ScriptedRunHost`: never reachable from `main`.
#[cfg(test)]
#[allow(clippy::option_option)]
#[must_use]
fn test_sources(
    sessions: Option<Vec<sources::SessionSummary>>,
    diff: Option<sources::DiffSnapshot>,
    approval: Option<Option<sources::ApprovalRequest>>,
    copy_ok: bool,
) -> Sources {
    Sources::scripted(sessions, diff, approval, copy_ok)
}

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

/// Resolve one parked approval through the shared runtime service. Returns
/// `false` on every failure (no active run, unreadable store, stale request):
/// the park stays pending and the UI makes no decision claim.
fn respond_approval(id: &str, approved: bool) -> bool {
    let (Some(run_id), Some(db)) = (
        crate::run::active_run_id(),
        crate::store::open_existing_db(),
    ) else {
        return false;
    };
    let registry = crate::run::registry();
    nexora_runtime::application::agent::permissions::resolve_approval(
        registry.as_ref(),
        &db,
        run_id,
        id,
        approved,
        None,
    )
    .is_ok()
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
    let sources = sources();
    let mut app = App::new(resolved, snapshot, size.width, size.height);
    app.set_command_specs(sources.commands());

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
        &sources,
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

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    receiver: &Receiver<UiEvent>,
    sender: &EventSender,
    cancel: &CancelSignal,
    run_handle: &mut Option<JoinHandle<()>>,
    caps: &CapabilityArgs,
    sources: &Sources,
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
                // Palette/local choices and session opening already applied
                // their honest state in `App`; no backend action is invented.
                app::LoopCmd::Continue
                | app::LoopCmd::RunCommand(_)
                | app::LoopCmd::OpenSession(_) => {}
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
                app::LoopCmd::LoadSessions => {
                    let result = sources.load_sessions();
                    app.apply(UiEvent::SessionsLoaded(result));
                }
                app::LoopCmd::SearchSessions(query) => {
                    let result = sources.search_sessions(&query);
                    app.apply(UiEvent::SessionsLoaded(result));
                }
                app::LoopCmd::RespondApproval { id, approved } => {
                    // A/D reach the real approval gate. The UI moves the
                    // decision only after the core confirms the resolution.
                    if respond_approval(&id, approved) {
                        app.apply(UiEvent::ApprovalResponded { id, approved });
                    } else {
                        app.apply(UiEvent::ApprovalResponseFailed);
                    }
                }
                app::LoopCmd::RequestPause => {
                    if let Some(run_id) = crate::run::active_run_id() {
                        let _ = crate::run::registry().pause(run_id);
                    }
                }
                app::LoopCmd::RequestResume => {
                    if let Some(run_id) = crate::run::active_run_id() {
                        let _ = crate::run::registry().resume(run_id);
                    }
                }
                app::LoopCmd::LoadDiff => {
                    let result = sources.load_diff();
                    app.apply(UiEvent::DiffLoaded(result));
                }
                app::LoopCmd::LoadApproval => {
                    let result = sources.load_approval();
                    app.apply(UiEvent::ApprovalLoaded(result));
                }
                app::LoopCmd::Copy(text) => {
                    let ok = sources.copy(&text);
                    app.apply(UiEvent::CopyResult(ok));
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
