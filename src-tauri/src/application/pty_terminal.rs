//! Interactive workspace PTY terminal manager (`portable-pty`).
//!
//! Owns live OS pseudo-terminal sessions (`ConPTY` on Windows, POSIX PTY on
//! Unix), streams UTF-8 output chunks to the registered event sink, and
//! guarantees clean process teardown on explicit kill or application shutdown.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Tauri event channel emitted when a PTY session produces output.
pub const TERMINAL_PTY_OUTPUT_EVENT: &str = "terminal-pty-output";

/// Tauri event channel emitted when a PTY session's child process exits.
pub const TERMINAL_PTY_EXIT_EVENT: &str = "terminal-pty-exit";

const MIN_COLS: u16 = 2;
const MAX_COLS: u16 = 500;
const MIN_ROWS: u16 = 1;
const MAX_ROWS: u16 = 200;
const READ_BUFFER_BYTES: usize = 4096;

/// Metadata returned when an interactive PTY session is spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PtySessionInfo {
    pub session_id: String,
    pub shell_name: String,
    pub cwd: String,
}

/// Output chunk payload emitted on [`TERMINAL_PTY_OUTPUT_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PtyOutputEvent {
    pub session_id: String,
    pub data: String,
}

/// Exit payload emitted on [`TERMINAL_PTY_EXIT_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PtyExitEvent {
    pub session_id: String,
    pub exit_code: Option<u32>,
}

/// Classified, secret-free error variants for PTY operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyError {
    InvalidWorkingDirectory,
    SpawnFailed,
    SessionNotFound,
    WriteFailed,
    ResizeFailed,
    InternalLock,
}

/// Sink for streaming PTY output and exit notifications out of the reader thread.
pub trait PtyEventSink: Send + Sync + 'static {
    fn emit_output(&self, event: PtyOutputEvent);
    fn emit_exit(&self, event: PtyExitEvent);
}

struct TauriPtySink {
    app: AppHandle,
}

impl PtyEventSink for TauriPtySink {
    fn emit_output(&self, event: PtyOutputEvent) {
        let _ = self.app.emit(TERMINAL_PTY_OUTPUT_EVENT, event);
    }

    fn emit_exit(&self, event: PtyExitEvent) {
        let _ = self.app.emit(TERMINAL_PTY_EXIT_EVENT, event);
    }
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// Thread-safe registry of active interactive PTY sessions.
#[derive(Default)]
pub struct PtyTerminalManager {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
}

impl PtyTerminalManager {
    /// Construct an empty PTY terminal manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a new interactive shell session in `cwd` and emit PTY events to
    /// the Tauri `app_handle`.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::InvalidWorkingDirectory`] if `cwd` does not exist
    /// or is not a directory, [`PtyError::SpawnFailed`] if the OS PTY or shell
    /// process cannot be created, or [`PtyError::InternalLock`] if session
    /// state cannot be locked.
    pub fn spawn_session(
        &self,
        app_handle: AppHandle,
        cwd: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<PtySessionInfo, PtyError> {
        let sink: Arc<dyn PtyEventSink> = Arc::new(TauriPtySink { app: app_handle });
        self.spawn_session_with_sink(sink, cwd, cols, rows)
    }

    /// Spawn a new interactive shell session in `cwd` using a custom
    /// [`PtyEventSink`].
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::InvalidWorkingDirectory`], [`PtyError::SpawnFailed`],
    /// or [`PtyError::InternalLock`].
    pub fn spawn_session_with_sink(
        &self,
        sink: Arc<dyn PtyEventSink>,
        cwd: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<PtySessionInfo, PtyError> {
        if !cwd.is_dir() {
            return Err(PtyError::InvalidWorkingDirectory);
        }

        let size = clamped_pty_size(cols, rows);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).map_err(|err| {
            log::error!("failed to open native PTY: {err}");
            PtyError::SpawnFailed
        })?;

        let (shell_program, shell_name) = resolve_default_shell();
        let cmd = build_shell_command(&shell_program, cwd);

        let child = pair.slave.spawn_command(cmd).map_err(|err| {
            log::error!("failed to spawn PTY shell process: {err}");
            PtyError::SpawnFailed
        })?;
        // Drop the slave handle immediately so the master reader observes EOF
        // as soon as the shell process exits.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().map_err(|err| {
            log::error!("failed to clone PTY master reader: {err}");
            PtyError::SpawnFailed
        })?;
        let writer = pair.master.take_writer().map_err(|err| {
            log::error!("failed to take PTY master writer: {err}");
            PtyError::SpawnFailed
        })?;
        let killer = child.clone_killer();

        let session_id = Uuid::new_v4().to_string();
        let session = PtySession {
            master: pair.master,
            writer,
            killer,
        };

        {
            let mut guard = self.sessions.lock().map_err(|_| PtyError::InternalLock)?;
            guard.insert(session_id.clone(), session);
        }

        let reader_session_id = session_id.clone();
        let sessions_ref = Arc::clone(&self.sessions);
        let spawn_thread = std::thread::Builder::new()
            .name(format!("nexora-pty-{}", &session_id[..8]))
            .spawn(move || {
                run_reader_loop(reader, child, &reader_session_id, &sessions_ref, &sink);
            });

        if let Err(err) = spawn_thread {
            log::error!("failed to start PTY reader thread: {err}");
            let _ = self.kill_session(&session_id);
            return Err(PtyError::SpawnFailed);
        }

        Ok(PtySessionInfo {
            session_id,
            shell_name,
            cwd: cwd.to_string_lossy().to_string(),
        })
    }

    /// Write raw UTF-8 input / keystrokes into a live PTY session and flush.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::SessionNotFound`] when `session_id` is unknown,
    /// [`PtyError::WriteFailed`] on I/O failure, or [`PtyError::InternalLock`]
    /// if the session table lock is poisoned.
    pub fn write_session(&self, session_id: &str, data: &str) -> Result<(), PtyError> {
        let mut guard = self.sessions.lock().map_err(|_| PtyError::InternalLock)?;
        let session = guard.get_mut(session_id).ok_or(PtyError::SessionNotFound)?;
        session
            .writer
            .write_all(data.as_bytes())
            .and_then(|()| session.writer.flush())
            .map_err(|err| {
                log::warn!("PTY write failed for session {session_id}: {err}");
                PtyError::WriteFailed
            })
    }

    /// Resize a live PTY session's terminal grid.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::SessionNotFound`] when `session_id` is unknown,
    /// [`PtyError::ResizeFailed`] when the OS PTY rejects the resize, or
    /// [`PtyError::InternalLock`] if the session table lock is poisoned.
    pub fn resize_session(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), PtyError> {
        let guard = self.sessions.lock().map_err(|_| PtyError::InternalLock)?;
        let session = guard.get(session_id).ok_or(PtyError::SessionNotFound)?;
        let size = clamped_pty_size(cols, rows);
        session.master.resize(size).map_err(|err| {
            log::warn!("PTY resize failed for session {session_id}: {err}");
            PtyError::ResizeFailed
        })
    }

    /// Terminate a live PTY session, dropping its writer/master handles and
    /// killing the child process. Returns `true` if a live session was found.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError::InternalLock`] if the session table lock is poisoned.
    pub fn kill_session(&self, session_id: &str) -> Result<bool, PtyError> {
        let removed = {
            let mut guard = self.sessions.lock().map_err(|_| PtyError::InternalLock)?;
            guard.remove(session_id)
        };
        if let Some(session) = removed {
            teardown_session(session);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Terminate every active PTY session (used during orderly app shutdown).
    pub fn kill_all(&self) -> usize {
        let drained: Vec<PtySession> = match self.sessions.lock() {
            Ok(mut guard) => guard.drain().map(|(_, session)| session).collect(),
            Err(_) => return 0,
        };
        let count = drained.len();
        for session in drained {
            teardown_session(session);
        }
        count
    }

    /// Count currently tracked live PTY sessions.
    #[must_use]
    pub fn active_session_count(&self) -> usize {
        self.sessions.lock().map_or(0, |guard| guard.len())
    }
}

fn teardown_session(mut session: PtySession) {
    let _ = session.killer.kill();
    drop(session.writer);
    drop(session.master);
}

fn clamped_pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(MIN_ROWS, MAX_ROWS),
        cols: cols.clamp(MIN_COLS, MAX_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(windows)]
fn resolve_default_shell() -> (String, String) {
    if executable_on_path("pwsh.exe") {
        ("pwsh.exe".to_string(), "pwsh".to_string())
    } else {
        ("powershell.exe".to_string(), "powershell".to_string())
    }
}

#[cfg(windows)]
fn executable_on_path(file_name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(file_name).is_file()))
}

#[cfg(not(windows))]
fn resolve_default_shell() -> (String, String) {
    let shell_path = std::env::var("SHELL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string());
    let shell_name = Path::new(&shell_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("sh")
        .to_string();
    (shell_path, shell_name)
}

fn build_shell_command(shell_program: &str, cwd: &Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell_program);
    #[cfg(windows)]
    {
        cmd.arg("-NoLogo");
    }
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd
}

/// Incremental UTF-8 decoder that buffers incomplete trailing multi-byte
/// sequences across arbitrary byte-chunk boundaries.
#[derive(Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        if let Ok(valid) = std::str::from_utf8(&self.pending[..valid_up_to]) {
                            out.push_str(valid);
                        }
                    }
                    if let Some(invalid_len) = err.error_len() {
                        out.push(char::REPLACEMENT_CHARACTER);
                        self.pending.drain(..valid_up_to + invalid_len);
                    } else {
                        self.pending.drain(..valid_up_to);
                        break;
                    }
                }
            }
        }
        out
    }

    fn flush(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let trailing = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        trailing
    }
}

fn run_reader_loop(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    session_id: &str,
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    sink: &Arc<dyn PtyEventSink>,
) {
    let mut buf = [0u8; READ_BUFFER_BYTES];
    let mut decoder = Utf8StreamDecoder::default();

    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = decoder.push(&buf[..n]);
                if !chunk.is_empty() {
                    sink.emit_output(PtyOutputEvent {
                        session_id: session_id.to_string(),
                        data: chunk,
                    });
                }
            }
        }
    }

    let tail = decoder.flush();
    if !tail.is_empty() {
        sink.emit_output(PtyOutputEvent {
            session_id: session_id.to_string(),
            data: tail,
        });
    }

    let exit_code = child.wait().ok().map(|status| status.exit_code());

    if let Ok(mut guard) = sessions.lock() {
        guard.remove(session_id);
    }

    sink.emit_exit(PtyExitEvent {
        session_id: session_id.to_string(),
        exit_code,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, Instant};

    enum TestPtyEvent {
        Output(PtyOutputEvent),
        Exit(PtyExitEvent),
    }

    struct ChannelSink {
        tx: Sender<TestPtyEvent>,
    }

    impl PtyEventSink for ChannelSink {
        fn emit_output(&self, event: PtyOutputEvent) {
            let _ = self.tx.send(TestPtyEvent::Output(event));
        }

        fn emit_exit(&self, event: PtyExitEvent) {
            let _ = self.tx.send(TestPtyEvent::Exit(event));
        }
    }

    fn wait_for_output_containing(
        manager: &PtyTerminalManager,
        session_id: &str,
        rx: &Receiver<TestPtyEvent>,
        needle: &str,
        timeout: Duration,
    ) -> String {
        let deadline = Instant::now() + timeout;
        let mut combined = String::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Ok(TestPtyEvent::Output(ev)) => {
                    // ConPTY / PSReadLine queries cursor position (DSR ESC[6n)
                    // before showing the prompt; real xterm.js replies with
                    // ESC[row;colR via onData. Answer it in the headless test.
                    if ev.data.contains("\x1b[6n") {
                        let _ = manager.write_session(session_id, "\x1b[1;1R");
                    }
                    combined.push_str(&ev.data);
                    if combined.contains(needle) {
                        return combined;
                    }
                }
                Ok(TestPtyEvent::Exit(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        combined
    }

    fn wait_for_exit(rx: &Receiver<TestPtyEvent>, timeout: Duration) -> Option<PtyExitEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Ok(TestPtyEvent::Exit(ev)) => return Some(ev),
                Ok(TestPtyEvent::Output(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    #[test]
    fn utf8_stream_decoder_handles_split_multibyte_and_invalid_bytes() {
        let mut decoder = Utf8StreamDecoder::default();
        let word = "Привет";
        let bytes = word.as_bytes();

        // Split right in the middle of the first 2-byte Cyrillic character.
        assert_eq!(decoder.push(&bytes[..1]), "");
        assert_eq!(decoder.push(&bytes[1..5]), "Пр");
        assert_eq!(decoder.push(&bytes[5..]), "ивет");
        assert_eq!(decoder.flush(), "");

        // Invalid byte produces U+FFFD without dropping subsequent ASCII.
        assert_eq!(decoder.push(&[0xFF, b'o', b'k']), "\u{FFFD}ok");
    }

    #[test]
    fn clamped_pty_size_bounds_dimensions() {
        let low = clamped_pty_size(0, 0);
        assert_eq!(low.cols, MIN_COLS);
        assert_eq!(low.rows, MIN_ROWS);

        let high = clamped_pty_size(9999, 9999);
        assert_eq!(high.cols, MAX_COLS);
        assert_eq!(high.rows, MAX_ROWS);

        let normal = clamped_pty_size(120, 32);
        assert_eq!(normal.cols, 120);
        assert_eq!(normal.rows, 32);
    }

    #[test]
    fn unknown_session_returns_not_found_and_kill_is_idempotent() {
        let manager = PtyTerminalManager::new();
        assert_eq!(manager.active_session_count(), 0);
        assert_eq!(
            manager.write_session("missing", "hi\r"),
            Err(PtyError::SessionNotFound)
        );
        assert_eq!(
            manager.resize_session("missing", 80, 24),
            Err(PtyError::SessionNotFound)
        );
        assert_eq!(manager.kill_session("missing"), Ok(false));
    }

    #[test]
    fn spawn_rejects_missing_working_directory() {
        let manager = PtyTerminalManager::new();
        let (tx, _rx) = mpsc::channel();
        let sink: Arc<dyn PtyEventSink> = Arc::new(ChannelSink { tx });
        let missing =
            std::env::temp_dir().join(format!("nexora-pty-missing-dir-{}", Uuid::new_v4()));
        let err = manager
            .spawn_session_with_sink(sink, &missing, 80, 24)
            .expect_err("non-existent cwd must fail");
        assert_eq!(err, PtyError::InvalidWorkingDirectory);
        assert_eq!(manager.active_session_count(), 0);
    }

    #[test]
    fn manager_spawn_write_resize_kill_lifecycle() {
        let manager = PtyTerminalManager::new();
        let (tx, rx) = mpsc::channel();
        let sink: Arc<dyn PtyEventSink> = Arc::new(ChannelSink { tx });
        let cwd = std::env::temp_dir();

        let info = manager
            .spawn_session_with_sink(sink, &cwd, 80, 24)
            .expect("spawn PTY session");
        assert!(!info.session_id.is_empty());
        assert!(!info.shell_name.is_empty());
        assert_eq!(manager.active_session_count(), 1);

        manager
            .resize_session(&info.session_id, 100, 30)
            .expect("resize active PTY session");

        let sentinel = "NEXORA_PTY_MARKER_42";
        manager
            .write_session(&info.session_id, &format!("echo {sentinel}\r\n"))
            .expect("write echo command");

        let output = wait_for_output_containing(
            &manager,
            &info.session_id,
            &rx,
            sentinel,
            Duration::from_secs(15),
        );
        assert!(
            output.contains(sentinel),
            "expected PTY output to contain sentinel {sentinel:?}, got: {output:?}"
        );

        assert_eq!(manager.kill_session(&info.session_id), Ok(true));
        assert_eq!(manager.active_session_count(), 0);
        // Second kill is idempotent.
        assert_eq!(manager.kill_session(&info.session_id), Ok(false));

        let exit_ev = wait_for_exit(&rx, Duration::from_secs(10))
            .expect("expected PTY exit event after kill");
        assert_eq!(exit_ev.session_id, info.session_id);
    }
}
