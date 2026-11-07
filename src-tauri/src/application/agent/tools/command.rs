//! Tool command execution: spawn, bounded capture, and drain.
//!
//! Split from `executor.rs` (AC-207): the shell-command plumbing — argument
//! validation, `ToolRegistry::execute_command_with_limits`, the stream
//! readers, and the limits/drain constants — lives here; the registry's
//! dispatch stays in [`super::executor`].

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::application::agent::control::CancellationToken;

use super::executor::ToolError;
use super::output::truncate_output;
use super::path_safety::{canonical_root, resolve_path, verify_existing_within};
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
/// (Task 3.2): the child has been killed and the run must return promptly
/// (acceptance "within ~1s"). Streams that stay open past this are leaked via
/// the same bounded `drain_reader` mechanism rather than blocking the cancel.
pub(super) const CANCEL_DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Marker appended when a stream did not close within [`DRAIN_GRACE`].
pub(crate) const STREAM_STILL_OPEN_MARKER: &str =
    "[warning: output stream still open after grace period; capture may be partial]";
impl ToolRegistry {
    // Tool implementations
    // -----------------------------------------------------------------------

    /// [`execute_command`](Self::execute) with injectable execution limits
    /// and cooperative cancellation.
    ///
    /// The default constructor applies the production constants; unit tests
    /// inject shorter timeout / drain-grace values so they exercise the *real*
    /// bounded-timeout and bounded-drain paths quickly instead of simulating
    /// them. Behavior is identical in both cases.
    pub(super) fn execute_command_with_limits(
        args: &Value,
        workspace_root: &Path,
        limits: &CommandLimits,
        token: &CancellationToken,
    ) -> Result<String, ToolError> {
        let (command, resolved_cwd) = validated_command_args(args, workspace_root)?;

        // Build platform-appropriate shell invocation
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        };
        cmd.current_dir(&resolved_cwd)
            .stdin(Stdio::null()) // a command must never read host stdin
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Io(format!("failed to spawn command: {e}")))?;

        // Take pipes and move each into a reader thread that accumulates at
        // most `capture_cap` bytes (discarding anything beyond) and hands its
        // buffer to a channel when EOF is reached. Threads that never reach
        // EOF (a grandchild still holding the pipe open) simply never send;
        // they are reaped below through the bounded drain, never through an
        // unbounded join.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut stdout_handle = stdout.map(|out| spawn_stream_reader(out, limits.capture_cap));
        let mut stderr_handle = stderr.map(|err| spawn_stream_reader(err, limits.capture_cap));

        // Poll with timeout, honouring cooperative cancellation: a fired
        // token kills the child promptly (within one poll interval) and
        // aborts with `ToolError::Cancelled` — this is the "instant cancel"
        // path that reaches running tool processes (Task 3.2).
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child
                .try_wait()
                .map_err(|e| ToolError::Io(format!("wait failed: {e}")))?
            {
                break s;
            }
            if token.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                // Cancellation must return promptly (acceptance: "within ~1s"):
                // drain the just-killed child's pipes only briefly — a
                // grandchild still holding them is leaked via the bounded
                // `drain_reader` grace mechanism, never block the cancel.
                let mut scratch = Vec::new();
                drain_reader(&mut stdout_handle, CANCEL_DRAIN_GRACE, &mut scratch);
                drain_reader(&mut stderr_handle, CANCEL_DRAIN_GRACE, &mut scratch);
                return Err(ToolError::Cancelled);
            }
            if start.elapsed() > limits.timeout {
                let _ = child.kill();
                let _ = child.wait();
                // Bounded drain on the kill path too: a grandchild still
                // holding a pipe must not block this error.
                let mut scratch = Vec::new();
                drain_reader(&mut stdout_handle, limits.drain_grace, &mut scratch);
                drain_reader(&mut stderr_handle, limits.drain_grace, &mut scratch);
                return Err(ToolError::Timeout(format!(
                    "command timed out after {} seconds",
                    limits.timeout.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        // Bounded drain of both streams: wait up to the grace period for each
        // reader to reach EOF. A stream whose pipe stays open past the grace
        // (orphaned grandchild) has its thread deliberately leaked so the tool
        // call can complete; the captured bytes so far are used as-is.
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let stdout_open = drain_reader(&mut stdout_handle, limits.drain_grace, &mut stdout_bytes);
        // Wait for stdout EOF first; if it timed out there is little reason to
        // keep waiting for stderr, but the grace bounds the total anyway.
        let stderr_open = drain_reader(&mut stderr_handle, limits.drain_grace, &mut stderr_bytes);

        let stdout_str = String::from_utf8_lossy(&stdout_bytes).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr_bytes).to_string();

        let mut combined = String::new();
        if !stdout_str.is_empty() {
            combined.push_str(&stdout_str);
            if !stdout_str.ends_with('\n') {
                combined.push('\n');
            }
        }
        if !stderr_str.is_empty() {
            if !combined.is_empty() {
                combined.push_str("--- stderr ---\n");
            }
            combined.push_str(&stderr_str);
            if !stderr_str.ends_with('\n') {
                combined.push('\n');
            }
        }
        if combined.is_empty() {
            // Still report exit status for empty output
            if status.success() {
                combined = "(no output)\n".to_string();
            } else {
                combined = format!("command exited with status {status}\n");
            }
        } else if !status.success() {
            let _ = writeln!(combined, "\n[command exited with status {status}]");
        }

        // A stream that never closed within the grace period is reported so
        // the model knows the captured output may be incomplete.
        if stdout_open || stderr_open {
            if !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(STREAM_STILL_OPEN_MARKER);
            combined.push('\n');
        }

        let truncated = truncate_output(combined);
        Ok(truncated)
    }
}
// ---------------------------------------------------------------------------
// Command execution plumbing: injectable limits, bounded capture, bounded drain
// ---------------------------------------------------------------------------

/// Validate `execute_command` arguments and resolve the working directory.
///
/// Returns the command string and its workspace-confined absolute directory.
///
/// NOTE (NEX-SEC-001 defence-in-depth, deliberately deferred): no static
/// command allowlist/profile is enforced here. A `sh -c` / `cmd /C` string
/// cannot be statically classified — quoting, `&&` chains, environment
/// indirection, and `powershell -EncodedCommand` all defeat prefix matching
/// — so an allowlist would either break legitimate coding runs or create
/// false confidence while adding a second policy surface to keep in sync
/// with the approval ladder. The enforced control is the NEX-SEC-001
/// deny-floor instead: the shell always parks for explicit user consent
/// (`RiskClass::DangerousShell` + the dispatch `Ask` non-collapse), and the
/// child's `cwd` stays workspace-confined with TOCTOU re-verification below.
pub(super) fn validated_command_args(
    args: &Value,
    workspace_root: &Path,
) -> Result<(String, PathBuf), ToolError> {
    let command = args.get("command").and_then(Value::as_str).ok_or_else(|| {
        ToolError::InvalidArguments("missing required field 'command'".to_string())
    })?;
    if command.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "field 'command' must not be empty".to_string(),
        ));
    }
    let cwd_opt = args.get("cwd").and_then(Value::as_str);

    // Resolve cwd inside workspace
    let resolved_cwd = if let Some(cwd) = cwd_opt {
        if cwd.trim().is_empty() {
            workspace_root.to_path_buf()
        } else {
            let p = resolve_path(workspace_root, cwd)?;
            // TOCTOU guard: the directory may have been swapped for a link
            // between `resolve_path` and the spawn below; re-verify the
            // canonical target before it becomes a child process cwd.
            verify_existing_within(workspace_root, &canonical_root(workspace_root), &p, cwd)?;
            if p.is_file() {
                return Err(ToolError::InvalidArguments(format!(
                    "cwd '{cwd}' is a file, not a directory"
                )));
            }
            // If the directory does not exist, treat as error
            if !p.exists() {
                return Err(ToolError::Io(format!("cwd does not exist: '{cwd}'")));
            }
            if !p.is_dir() {
                return Err(ToolError::Io(format!("cwd is not a directory: '{cwd}'")));
            }
            p
        }
    } else {
        workspace_root.to_path_buf()
    };
    Ok((command.to_string(), resolved_cwd))
}

/// Execution limits injected into
/// [`ToolRegistry::execute_command_with_limits`].
///
/// Unit tests construct this directly to exercise the real bounded-timeout /
/// bounded-drain / bounded-capture code paths quickly; production always uses
/// [`Default`].
#[derive(Debug, Clone, Copy)]
pub(super) struct CommandLimits {
    /// Hard timeout for the child process.
    pub(super) timeout: Duration,
    /// Grace granted to pipe readers to reach EOF once the process has exited.
    pub(super) drain_grace: Duration,
    /// Maximum bytes accumulated per output stream before further chunks are
    /// discarded.
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

/// Terminal of one output-stream reader thread: the channel delivering the
/// EOF signal, the shared live capture buffer, plus the thread handle (kept
/// only so it can be deliberately leaked when the drain grace expires).
pub(super) type StreamReader = Option<(Receiver<()>, Arc<Mutex<Vec<u8>>>, JoinHandle<()>)>;

/// Move `stream` into a reader thread that accumulates at most `capture_cap`
/// bytes into a shared live buffer and signals EOF through a channel.
///
/// Once the cap is reached the thread keeps draining the pipe but discards
/// everything beyond it: memory stays bounded while the child is never blocked
/// by a full pipe.
#[allow(clippy::needless_pass_by_value)] // spawned closure must own the stream
pub(super) fn spawn_stream_reader(
    mut stream: impl Read + Send + 'static,
    capture_cap: usize,
) -> (Receiver<()>, Arc<Mutex<Vec<u8>>>, JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let shared = Arc::new(Mutex::new(Vec::new()));
    let shared_cloned = Arc::clone(&shared);
    let handle = std::thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut buf = shared_cloned
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let room = capture_cap.saturating_sub(buf.len());
                    if room > 0 {
                        buf.extend_from_slice(&chunk[..std::cmp::min(n, room)]);
                    }
                }
            }
        }
        let _ = tx.send(()); // the receiving side may already be gone; harmless
    });
    (rx, shared, handle)
}

/// Boundedly wait for one reader thread to reach EOF into `out`.
///
/// Returns `true` when the stream did **not** close within `grace` (e.g. an
/// orphaned grandchild holding the pipe): the reader thread is then
/// *deliberately leaked* (`mem::forget`) so the tool call can complete and no
/// code path can block indefinitely. Any captured bytes are moved into `out`.
pub(super) fn drain_reader(stream: &mut StreamReader, grace: Duration, out: &mut Vec<u8>) -> bool {
    match stream.take() {
        Some((rx, shared, handle)) => {
            let closed = rx.recv_timeout(grace);
            if closed.is_ok() {
                let mut buf = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *out = std::mem::take(&mut *buf);
                false
            } else {
                // Grace expired (or the sender vanished unread): leak the
                // thread on purpose rather than blocking or aborting, but
                // preserve the partial capture accumulated so far.
                let mut buf = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *out = std::mem::take(&mut *buf);
                std::mem::forget(handle);
                true
            }
        }
        None => false,
    }
}
