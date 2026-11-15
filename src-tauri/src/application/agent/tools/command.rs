//! Tool command execution: OS process-tree spawn, streaming secret redaction,
//! bounded capture, and bounded drain.
//!
//! Split from `executor.rs` (AC-207): the shell-command plumbing — argument
//! validation, `ToolRegistry::execute_command_at_boundary`, the stream
//! readers, and the limits/drain constants — lives here; the registry's
//! dispatch stays in [`super::executor`].

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
use nexora_os_exec::spawn_in_process_tree;
use serde_json::Value;

use crate::application::agent::control::CancellationToken;
use crate::application::agent::redaction::{redact_command_output, StreamRedactor};

use super::executor::ToolError;
use super::output::truncate_output;
use super::path_safety::{resolve_path, resolve_terminal_cwd};
use super::ToolRegistry;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap accumulated per output stream before chunks are discarded;
/// prevents unbounded memory growth from runaway child output.
pub(super) const MAX_CAPTURE_BYTES: usize = 1024 * 1024; // 1 MiB per stream
/// How long the completed process's pipe readers may still take to reach EOF
/// (e.g. because an orphaned grandchild inherited the pipe) before they are
/// deliberately leaked and the captured output is used as-is.
pub(super) const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Much shorter drain bound used specifically on the cancellation path
/// (Task 3.2): the process tree has been killed and the run must return
/// promptly (acceptance "within ~1s"). Streams that stay open past this are
/// leaked via the same bounded `drain_reader` mechanism rather than blocking
/// the cancel.
pub(super) const CANCEL_DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Poll / chunk-batch timeout used while waiting on an active child process.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Upper bound (in bytes) of a single emitted streaming text batch so live UI
/// frames stay bounded on UTF-8 char boundaries.
const STREAM_CHUNK_MAX_BYTES: usize = 16 * 1024;
/// Marker appended when a stream did not close within [`DRAIN_GRACE`].
pub(crate) const STREAM_STILL_OPEN_MARKER: &str =
    "[warning: output stream still open after grace period; capture may be partial]";

/// Incremental live updates delivered while a shell command runs.
///
/// Every chunk emitted through [`CommandStreamUpdate::Chunk`] and the command
/// preview in [`CommandStreamUpdate::Started`] are already UTF-8 safe and
/// scrubbed for secrets (`[REDACTED]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandStreamUpdate {
    Started { command: String },
    Chunk { stream: &'static str, text: String },
    Done { truncated: bool, killed: bool },
}

impl ToolRegistry {
    // Tool implementations
    // -----------------------------------------------------------------------

    /// [`execute_command`](Self::execute) with injectable execution limits
    /// and cooperative cancellation.
    #[cfg(test)]
    pub(super) fn execute_command_with_limits(
        args: &Value,
        workspace_root: &Path,
        limits: &CommandLimits,
        token: &CancellationToken,
    ) -> Result<String, ToolError> {
        Self::execute_command_at_boundary(
            args,
            workspace_root,
            limits,
            token,
            None,
            &mut || {},
            None,
        )
    }

    #[cfg(test)]
    pub(super) fn execute_command_with_limits_streaming(
        args: &Value,
        workspace_root: &Path,
        limits: &CommandLimits,
        token: &CancellationToken,
        on_stream: &mut dyn FnMut(CommandStreamUpdate),
    ) -> Result<String, ToolError> {
        Self::execute_command_at_boundary(
            args,
            workspace_root,
            limits,
            token,
            None,
            &mut || {},
            Some(on_stream),
        )
    }

    pub(super) fn execute_command_at_boundary(
        args: &Value,
        workspace_root: &Path,
        limits: &CommandLimits,
        token: &CancellationToken,
        bound: Option<&Path>,
        started: &mut dyn FnMut(),
        on_stream: Option<&mut dyn FnMut(CommandStreamUpdate)>,
    ) -> Result<String, ToolError> {
        let (command, resolved_cwd) = validated_command_args(args, workspace_root)?;

        if bound.is_some_and(|target| target != resolved_cwd) {
            return Err(ToolError::ConsentInvalidated);
        }
        Self::run_command_with_limits(
            platform_shell(&command, &["/D", "/C"])?,
            &command,
            &resolved_cwd,
            workspace_root,
            false,
            limits,
            token,
            started,
            on_stream,
        )
    }

    pub(super) fn execute_terminal_command(
        args: &Value,
        workspace_root: &Path,
        token: &CancellationToken,
        on_stream: Option<&mut dyn FnMut(CommandStreamUpdate)>,
    ) -> Result<String, ToolError> {
        let (command, cwd) = command_args(args, workspace_root, resolve_terminal_cwd)?;
        Self::run_command_with_limits(
            platform_shell(&command, &["/D", "/C"])?,
            &command,
            &cwd,
            workspace_root,
            true,
            &CommandLimits::default(),
            token,
            &mut || {},
            on_stream,
        )
    }

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)] // Streaming lifecycle plus trusted workspace launch context.
    fn run_command_with_limits(
        mut cmd: Command,
        raw_command: &str,
        resolved_cwd: &Path,
        workspace_root: &Path,
        terminal: bool,
        limits: &CommandLimits,
        token: &CancellationToken,
        started: &mut dyn FnMut(),
        mut on_stream: Option<&mut dyn FnMut(CommandStreamUpdate)>,
    ) -> Result<String, ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        cmd.current_dir(resolved_cwd)
            .stdin(Stdio::null()) // a command must never read host stdin
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Forward only explicitly selected benign environment entries, including
        // the per-command path. Shell startup hooks and credential sockets never inherit.
        let env: Vec<_> = [
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "WINDIR",
            "LANG",
            "LC_ALL",
            "TERM",
        ]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
        .collect();
        cmd.env_clear().envs(env);
        let mut spawn = || {
            if terminal {
                super::super::sandbox::spawn_terminal(&mut cmd, workspace_root)
            } else {
                super::super::sandbox::spawn_authorized(&mut cmd, workspace_root)
            }
        };
        #[cfg(not(test))]
        let mut child = spawn()?;
        #[cfg(test)]
        let mut child = match spawn() {
            Err(ToolError::ConsentInvalidated) if !super::super::sandbox::has_permit() => {
                spawn_in_process_tree(&mut cmd)
                    .map_err(|_| ToolError::Io("test command launch failed".into()))?
            }
            result => result?,
        };
        started();

        if let Some(cb) = on_stream.as_deref_mut() {
            let (scrubbed_cmd, _) = redact_command_output(raw_command);
            cb(CommandStreamUpdate::Started {
                command: scrubbed_cmd,
            });
        }

        // Move both pipes into reader threads that perform UTF-8-safe
        // streaming secret redaction, emit incremental chunks (when requested),
        // and bound accumulation at `capture_cap`.
        let (chunk_tx, chunk_rx) = if on_stream.is_some() {
            let (tx, rx) = std::sync::mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let mut stdout_handle = child
            .take_stdout()
            .map(|out| spawn_stream_reader("stdout", out, limits.capture_cap, chunk_tx.clone()));
        let mut stderr_handle = child
            .take_stderr()
            .map(|err| spawn_stream_reader("stderr", err, limits.capture_cap, chunk_tx));

        let start = Instant::now();
        let status = loop {
            if let Some(rx) = chunk_rx.as_ref() {
                drain_live_chunks(rx, &mut on_stream);
            }

            if let Some(s) = child
                .try_wait()
                .map_err(|e| ToolError::Io(format!("wait failed: {e}")))?
            {
                break s;
            }
            if token.is_cancelled() {
                let _ = child.kill_tree();
                let _ = child.wait();
                let (stdout_cap, _) = drain_reader(&mut stdout_handle, CANCEL_DRAIN_GRACE);
                let (stderr_cap, _) = drain_reader(&mut stderr_handle, CANCEL_DRAIN_GRACE);
                if let Some(rx) = chunk_rx.as_ref() {
                    drain_live_chunks(rx, &mut on_stream);
                }
                if let Some(cb) = on_stream.as_deref_mut() {
                    cb(CommandStreamUpdate::Done {
                        truncated: stdout_cap.truncated || stderr_cap.truncated,
                        killed: true,
                    });
                }
                return Err(ToolError::Cancelled);
            }
            if start.elapsed() > limits.timeout {
                let _ = child.kill_tree();
                let _ = child.wait();
                let (stdout_cap, _) = drain_reader(&mut stdout_handle, limits.drain_grace);
                let (stderr_cap, _) = drain_reader(&mut stderr_handle, limits.drain_grace);
                if let Some(rx) = chunk_rx.as_ref() {
                    drain_live_chunks(rx, &mut on_stream);
                }
                if let Some(cb) = on_stream.as_deref_mut() {
                    cb(CommandStreamUpdate::Done {
                        truncated: stdout_cap.truncated || stderr_cap.truncated,
                        killed: true,
                    });
                }
                return Err(ToolError::Timeout(format!(
                    "command timed out after {} seconds",
                    limits.timeout.as_secs()
                )));
            }

            if let Some(rx) = chunk_rx.as_ref() {
                wait_for_live_chunks(rx, &mut on_stream, POLL_INTERVAL);
            } else {
                std::thread::sleep(POLL_INTERVAL);
            }
        };

        // Bounded drain of both streams: wait up to the grace period for each
        // reader to reach EOF, then kill any orphaned background descendants
        // still attached to the OS process tree.
        let (stdout_cap, stdout_open) = drain_reader(&mut stdout_handle, limits.drain_grace);
        let (stderr_cap, stderr_open) = drain_reader(&mut stderr_handle, limits.drain_grace);
        let _ = child.kill_tree();
        if let Some(rx) = chunk_rx.as_ref() {
            drain_live_chunks(rx, &mut on_stream);
        }

        let capture_truncated = stdout_cap.truncated || stderr_cap.truncated;
        let dropped_total = stdout_cap
            .dropped_bytes
            .saturating_add(stderr_cap.dropped_bytes);

        let mut combined = String::new();
        if !stdout_cap.text.is_empty() {
            combined.push_str(&stdout_cap.text);
            if !stdout_cap.text.ends_with('\n') {
                combined.push('\n');
            }
        }
        if !stderr_cap.text.is_empty() {
            if !combined.is_empty() {
                combined.push_str("--- stderr ---\n");
            }
            combined.push_str(&stderr_cap.text);
            if !stderr_cap.text.ends_with('\n') {
                combined.push('\n');
            }
        }
        if combined.is_empty() {
            if status.success() {
                combined = "(no output)\n".to_string();
            } else {
                combined = format!("command exited with status {status}\n");
            }
        } else if !status.success() {
            let _ = writeln!(combined, "\n[command exited with status {status}]");
        }

        if capture_truncated {
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
            let _ = writeln!(
                combined,
                "[warning: command stream capture truncated at {} bytes (MAX_CAPTURE_BYTES); {dropped_total} additional bytes discarded]",
                limits.capture_cap
            );
        }

        if stdout_open || stderr_open {
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(STREAM_STILL_OPEN_MARKER);
            combined.push('\n');
        }

        // Defense-in-depth scrub across stream boundaries before context
        // truncation/spilling or return.
        let (combined, _) = redact_command_output(&combined);
        let combined_len = combined.len();
        let output = truncate_output(combined);
        let budget_truncated = output.len() != combined_len
            || output.contains("[output truncated,")
            || output.contains("[truncated:");

        if let Some(cb) = on_stream {
            cb(CommandStreamUpdate::Done {
                truncated: capture_truncated || budget_truncated,
                killed: false,
            });
        }

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Command execution plumbing: injectable limits, bounded capture, bounded drain
// ---------------------------------------------------------------------------

/// Validate `execute_command` arguments and resolve the working directory.
pub(super) fn validated_command_args(
    args: &Value,
    workspace_root: &Path,
) -> Result<(String, PathBuf), ToolError> {
    command_args(args, workspace_root, resolve_path)
}

fn command_args(
    args: &Value,
    workspace_root: &Path,
    resolve_cwd: fn(&Path, &str) -> Result<PathBuf, ToolError>,
) -> Result<(String, PathBuf), ToolError> {
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments("missing required field 'command'".into()))?;
    if command.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "field 'command' must not be empty".into(),
        ));
    }
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .unwrap_or(".");
    let resolved = resolve_cwd(workspace_root, cwd)?;
    if resolved.is_file() {
        return Err(ToolError::InvalidArguments(format!(
            "cwd '{cwd}' is a file, not a directory"
        )));
    }
    if !resolved.exists() {
        return Err(ToolError::Io(format!("cwd does not exist: '{cwd}'")));
    }
    if !resolved.is_dir() {
        return Err(ToolError::Io(format!("cwd is not a directory: '{cwd}'")));
    }
    Ok((command.to_owned(), resolved))
}

fn platform_shell(command: &str, windows_flags: &[&str]) -> Result<Command, ToolError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let mut shell = Command::new(
            nexora_os_exec::platform_shell_path()
                .map_err(|_| ToolError::Io("trusted system shell unavailable".into()))?,
        );
        shell
            .args(windows_flags)
            .arg(command)
            .creation_flags(CREATE_NO_WINDOW);
        Ok(shell)
    }
    #[cfg(not(windows))]
    {
        let _ = windows_flags;
        let mut shell = Command::new("/bin/sh");
        shell.arg("-c").arg(command);
        Ok(shell)
    }
}

/// Execution limits injected into
/// [`ToolRegistry::execute_command_at_boundary`].
#[derive(Debug, Clone, Copy)]
pub(super) struct CommandLimits {
    /// Hard timeout for the child process.
    pub(super) timeout: Duration,
    /// Grace granted to pipe readers to reach EOF once the process has exited.
    pub(super) drain_grace: Duration,
    /// Maximum redacted bytes accumulated per output stream before further
    /// output is discarded with an explicit truncation notice.
    pub(super) capture_cap: usize,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            timeout: COMMAND_TIMEOUT,
            drain_grace: DRAIN_GRACE,
            capture_cap: MAX_CAPTURE_BYTES,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct CapturedStream {
    pub(super) text: String,
    pub(super) truncated: bool,
    pub(super) dropped_bytes: usize,
}

type StreamChunkMsg = (&'static str, String);

/// Terminal of one output-stream reader thread: the channel delivering the
/// EOF signal, the shared redacted capture state, plus the thread handle.
pub(super) type StreamReader = Option<(Receiver<()>, Arc<Mutex<CapturedStream>>, JoinHandle<()>)>;

fn record_scrubbed_segment(
    stream_name: &'static str,
    scrubbed: &str,
    capture_cap: usize,
    shared: &Arc<Mutex<CapturedStream>>,
    chunk_tx: Option<&Sender<StreamChunkMsg>>,
) -> bool {
    if scrubbed.is_empty() {
        return shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .truncated;
    }

    let (emit_slice, truncated_now): (Option<String>, bool) = {
        let mut state = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.truncated {
            state.dropped_bytes = state.dropped_bytes.saturating_add(scrubbed.len());
            (None, true)
        } else {
            let room = capture_cap.saturating_sub(state.text.len());
            if room == 0 {
                state.truncated = true;
                state.dropped_bytes = state.dropped_bytes.saturating_add(scrubbed.len());
                (None, true)
            } else {
                let mut take = scrubbed.len().min(room);
                while take > 0 && !scrubbed.is_char_boundary(take) {
                    take -= 1;
                }
                let accepted = &scrubbed[..take];
                if !accepted.is_empty() {
                    state.text.push_str(accepted);
                }
                if take < scrubbed.len() {
                    state.truncated = true;
                    state.dropped_bytes = state.dropped_bytes.saturating_add(scrubbed.len() - take);
                }
                (
                    (!accepted.is_empty()).then(|| accepted.to_string()),
                    state.truncated,
                )
            }
        }
    };

    if let (Some(tx), Some(chunk)) = (chunk_tx, emit_slice) {
        let _ = tx.send((stream_name, chunk));
    }
    truncated_now
}

/// Move `stream` into a reader thread that UTF-8 decodes and secret-redacts in
/// flight, emits chunks up to `capture_cap`, and continues draining the OS pipe
/// until EOF so the child cannot block on a full pipe.
#[allow(clippy::needless_pass_by_value)] // spawned closure must own the stream
pub(super) fn spawn_stream_reader(
    stream_name: &'static str,
    mut stream: impl Read + Send + 'static,
    capture_cap: usize,
    chunk_tx: Option<Sender<StreamChunkMsg>>,
) -> (Receiver<()>, Arc<Mutex<CapturedStream>>, JoinHandle<()>) {
    let (eof_tx, eof_rx) = std::sync::mpsc::channel();
    let shared = Arc::new(Mutex::new(CapturedStream::default()));
    let shared_cloned = Arc::clone(&shared);
    let handle = std::thread::spawn(move || {
        let mut redactor = StreamRedactor::for_command();
        let mut buf = [0_u8; 8192];
        let mut capped = false;
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if capped {
                        let mut state = shared_cloned
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.dropped_bytes = state.dropped_bytes.saturating_add(n);
                        continue;
                    }
                    let scrubbed = redactor.push_bytes(&buf[..n]);
                    capped = record_scrubbed_segment(
                        stream_name,
                        &scrubbed,
                        capture_cap,
                        &shared_cloned,
                        chunk_tx.as_ref(),
                    );
                }
            }
        }
        if !capped {
            let (tail, _) = redactor.finish();
            let _ = record_scrubbed_segment(
                stream_name,
                &tail,
                capture_cap,
                &shared_cloned,
                chunk_tx.as_ref(),
            );
        }
        drop(chunk_tx);
        let _ = eof_tx.send(());
    });
    (eof_rx, shared, handle)
}

/// Wait briefly for a live chunk, then coalesce and flush any pending chunks on
/// UTF-8 boundaries.
fn wait_for_live_chunks(
    rx: &Receiver<StreamChunkMsg>,
    on_stream: &mut Option<&mut dyn FnMut(CommandStreamUpdate)>,
    timeout: Duration,
) {
    match rx.recv_timeout(timeout) {
        Ok((first_stream, first_text)) => {
            let mut pending = vec![(first_stream, first_text)];
            while let Ok(item) = rx.try_recv() {
                pending.push(item);
            }
            emit_coalesced_chunks(pending, on_stream);
        }
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
    }
}

fn drain_live_chunks(
    rx: &Receiver<StreamChunkMsg>,
    on_stream: &mut Option<&mut dyn FnMut(CommandStreamUpdate)>,
) {
    let mut pending = Vec::new();
    while let Ok(item) = rx.try_recv() {
        pending.push(item);
    }
    if !pending.is_empty() {
        emit_coalesced_chunks(pending, on_stream);
    }
}

fn emit_coalesced_chunks(
    pending: Vec<StreamChunkMsg>,
    on_stream: &mut Option<&mut dyn FnMut(CommandStreamUpdate)>,
) {
    let Some(cb) = on_stream.as_deref_mut() else {
        return;
    };

    let mut current_stream: Option<&'static str> = None;
    let mut buffer = String::new();

    for (stream, text) in pending {
        if text.is_empty() {
            continue;
        }
        if let Some(active) = current_stream {
            if active != stream {
                flush_bounded_utf8_chunks(active, &mut buffer, cb);
                current_stream = Some(stream);
            }
        } else {
            current_stream = Some(stream);
        }
        buffer.push_str(&text);
        if buffer.len() >= STREAM_CHUNK_MAX_BYTES {
            if let Some(active) = current_stream {
                flush_bounded_utf8_chunks(active, &mut buffer, cb);
            }
        }
    }

    if let Some(active) = current_stream {
        if !buffer.is_empty() {
            flush_bounded_utf8_chunks(active, &mut buffer, cb);
        }
    }
}

fn flush_bounded_utf8_chunks(
    stream: &'static str,
    buffer: &mut String,
    cb: &mut dyn FnMut(CommandStreamUpdate),
) {
    let mut start = 0;
    while start < buffer.len() {
        let remaining = buffer.len() - start;
        let take = if remaining <= STREAM_CHUNK_MAX_BYTES {
            remaining
        } else {
            let mut cut = start + STREAM_CHUNK_MAX_BYTES;
            while cut > start && !buffer.is_char_boundary(cut) {
                cut -= 1;
            }
            if cut == start {
                // Single char wider than budget shouldn't happen; advance one char.
                buffer[start..]
                    .chars()
                    .next()
                    .map_or(remaining, char::len_utf8)
            } else {
                cut - start
            }
        };
        let piece = &buffer[start..start + take];
        cb(CommandStreamUpdate::Chunk {
            stream,
            text: piece.to_string(),
        });
        start += take;
    }
    buffer.clear();
}

/// Boundedly wait for one reader thread to reach EOF and return the redacted
/// capture together with whether the grace period expired (`true` = stream still open).
pub(super) fn drain_reader(stream: &mut StreamReader, grace: Duration) -> (CapturedStream, bool) {
    match stream.take() {
        Some((rx, shared, handle)) => {
            let closed = rx.recv_timeout(grace);
            let mut state = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let captured = std::mem::take(&mut *state);
            if closed.is_ok() {
                let _ = handle.join();
                (captured, false)
            } else {
                std::mem::forget(handle);
                (captured, true)
            }
        }
        None => (CapturedStream::default(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::temp_workspace;

    #[test]
    fn streaming_execute_command_emits_redacted_utf8_chunks_and_done() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let cmd = if cfg!(windows) {
            "echo first-line & echo Authorization: Bearer super_secret_token_99 & echo err-line 1>&2"
        } else {
            "printf 'first-line\\n'; sleep 0.05; printf 'Authorization: Bearer super_secret_token_99\\n'; printf 'err-line\\n' >&2"
        };
        let args = serde_json::json!({ "command": cmd });
        let mut updates = Vec::new();
        let out = ToolRegistry::execute_command_with_limits_streaming(
            &args,
            &ws,
            &CommandLimits::default(),
            &token,
            &mut |u| updates.push(u),
        )
        .expect("command succeeds");

        assert!(out.contains("first-line"));
        assert!(out.contains("Bearer [REDACTED]"));
        assert!(out.contains("err-line"));
        assert!(!out.contains("super_secret_token_99"));

        assert!(matches!(
            updates.first(),
            Some(CommandStreamUpdate::Started { .. })
        ));
        assert!(matches!(
            updates.last(),
            Some(CommandStreamUpdate::Done {
                truncated: false,
                killed: false
            })
        ));

        let streamed: String = updates
            .iter()
            .filter_map(|u| match u {
                CommandStreamUpdate::Chunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(streamed.contains("first-line"));
        assert!(streamed.contains("Bearer [REDACTED]"));
        assert!(!streamed.contains("super_secret_token_99"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_capture_cap_surfaces_honest_truncation_notice() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        let cmd = format!("echo {}", "A".repeat(600));
        let args = serde_json::json!({ "command": cmd });
        let limits = CommandLimits {
            timeout: Duration::from_secs(10),
            drain_grace: Duration::from_millis(500),
            capture_cap: 128,
        };
        let mut saw_truncated_done = false;
        let out = ToolRegistry::execute_command_with_limits_streaming(
            &args,
            &ws,
            &limits,
            &token,
            &mut |u| {
                if let CommandStreamUpdate::Done { truncated, killed } = u {
                    assert!(!killed);
                    saw_truncated_done = truncated;
                }
            },
        )
        .expect("command finishes within capture cap");

        assert!(saw_truncated_done, "Done update must report truncated=true");
        assert!(
            out.contains("MAX_CAPTURE_BYTES") && out.contains("truncated at 128 bytes"),
            "output must surface honest capture-cap truncation notice, got: {out}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
