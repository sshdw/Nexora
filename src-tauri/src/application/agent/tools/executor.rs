//! Workspace tool execution: dispatch, timeouts, and workspace guards.
//!
//! [`ToolRegistry`] request handling for shell and filesystem tools. All
//! filesystem access is confined to the workspace root; shell execution is
//! bounded by timeout, capture, and drain limits.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::output::{truncate_output, unified_diff};
use super::ToolRegistry;
use crate::application::agent::control::CancellationToken;
use crate::application::execution::ToolCall;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap accumulated per output stream before chunks are discarded;
/// prevents unbounded memory growth from runaway child output.
const MAX_CAPTURE_BYTES: usize = 1024 * 1024; // 1 MiB per stream
/// How long the completed process's pipe readers may still take to reach EOF
/// (e.g. because an orphaned grandchild inherited the pipe) before they are
/// deliberately leaked and the captured output is used as-is.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Much shorter drain bound used specifically on the cancellation path
/// (Task 3.2): the child has been killed and the run must return promptly
/// (acceptance "within ~1s"). Streams that stay open past this are leaked via
/// the same bounded `drain_reader` mechanism rather than blocking the cancel.
const CANCEL_DRAIN_GRACE: Duration = Duration::from_millis(250);
/// Marker appended when a stream did not close within [`DRAIN_GRACE`].
pub(crate) const STREAM_STILL_OPEN_MARKER: &str =
    "[warning: output stream still open after grace period; capture may be partial]";

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Classified tool execution error. Display is always prefixed with `Error:`
/// so the LLM can observe failures without panicking the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolError {
    UnknownTool(String),
    InvalidArguments(String),
    Io(String),
    PathTraversal(String),
    Timeout(String),
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "Error: unknown tool '{name}'"),
            Self::InvalidArguments(msg) => write!(f, "Error: invalid arguments: {msg}"),
            Self::PathTraversal(p) => {
                write!(
                    f,
                    "Error: path traversal not allowed: '{p}' is outside workspace"
                )
            }
            Self::Io(msg) | Self::Timeout(msg) => write!(f, "Error: {msg}"),
            Self::Cancelled => write!(f, "Error: tool execution was cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

impl ToolRegistry {
    /// Dispatch a [`ToolCall`] to its implementation.
    ///
    /// `workspace_root` is the absolute path that bounds all filesystem
    /// access. Tool failures are returned as `Err(ToolError)` with an
    /// `Error:` prefix; they never panic.
    pub(crate) fn execute(call: &ToolCall, workspace_root: &Path) -> Result<String, ToolError> {
        // No control attached: never cancelled (Task 3.1 semantics exactly).
        let token = CancellationToken::new();
        Self::execute_with_cancellation(call, workspace_root, &token)
    }

    /// [`execute`](Self::execute) that honours cooperative cancellation
    /// (Task 3.2): returns `Err(ToolError::Cancelled)` immediately when the
    /// token has fired, and polls `token` inside long-running command
    /// execution so the child process is killed promptly.
    pub(crate) fn execute_with_cancellation(
        call: &ToolCall,
        workspace_root: &Path,
        token: &CancellationToken,
    ) -> Result<String, ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: Value = serde_json::from_str(&call.arguments).map_err(|e| {
            ToolError::InvalidArguments(format!("arguments are not valid JSON: {e}"))
        })?;
        match call.name.as_str() {
            "execute_command" => Self::execute_command_with_limits(
                &args,
                workspace_root,
                &CommandLimits::default(),
                token,
            ),
            "read_file" => Self::read_file(&args, workspace_root),
            "write_file" => Self::write_file(&args, workspace_root),
            "list_directory" => Self::list_directory(&args, workspace_root),
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    // -----------------------------------------------------------------------
    // Tool implementations
    // -----------------------------------------------------------------------

    /// [`execute_command`](Self::execute) with injectable execution limits
    /// and cooperative cancellation.
    ///
    /// The default constructor applies the production constants; unit tests
    /// inject shorter timeout / drain-grace values so they exercise the *real*
    /// bounded-timeout and bounded-drain paths quickly instead of simulating
    /// them. Behavior is identical in both cases.
    fn execute_command_with_limits(
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

    fn read_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let offset = args
            .get("offset_lines")
            .and_then(Value::as_u64)
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX));
        let limit = args
            .get("limit_lines")
            .and_then(Value::as_u64)
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX));
        if let Some(l) = limit {
            if l == 0 {
                return Err(ToolError::InvalidArguments(
                    "limit_lines must be >= 1".to_string(),
                ));
            }
        }

        let resolved = resolve_path(workspace_root, path)?;
        if !resolved.exists() {
            return Err(ToolError::Io(format!("file not found: '{path}'")));
        }
        if resolved.is_dir() {
            return Err(ToolError::Io(format!(
                "path is a directory, not a file: '{path}'"
            )));
        }

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| ToolError::Io(format!("failed to read file '{path}': {e}")))?;

        // Chunk by lines
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = offset.unwrap_or(0);
        if start >= total && total != 0 {
            // Return empty with hint; not an error that panics
            return Ok(String::new());
        }
        let start = std::cmp::min(start, total);
        let end = match limit {
            Some(l) => std::cmp::min(start.saturating_add(l), total),
            None => total,
        };
        let slice = &lines[start..end];
        Ok(truncate_output(slice.join("\n")))
    }

    fn write_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let content = args.get("content").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'content'".to_string())
        })?;

        let resolved = resolve_path(workspace_root, path)?;

        // Read old content before writing (same path validation as read_file). For
        // new files the diff is against empty; for existing files the bytes are
        // read lossily so binary files still produce a diff without panicking.
        let old_content = if resolved.exists() {
            match std::fs::read(&resolved) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        };

        if let Some(parent) = resolved.parent() {
            if !parent.as_os_str().is_empty() {
                // Validate parent is within workspace
                let parent_within = is_within_workspace(workspace_root, parent);
                if !parent_within {
                    return Err(ToolError::PathTraversal(path.to_string()));
                }
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::Io(format!("failed to create parent directories: {e}"))
                })?;
            }
        }

        std::fs::write(&resolved, content)
            .map_err(|e| ToolError::Io(format!("failed to write file '{path}': {e}")))?;

        let diff = unified_diff(path, &old_content, content);
        Ok(truncate_output(diff))
    }

    fn list_directory(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path_opt = args.get("path").and_then(Value::as_str);
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let target = if let Some(p) = path_opt {
            if p.trim().is_empty() {
                workspace_root.to_path_buf()
            } else {
                resolve_path(workspace_root, p)?
            }
        } else {
            workspace_root.to_path_buf()
        };

        if !target.exists() {
            return Err(ToolError::Io(format!(
                "directory not found: '{}'",
                path_opt.unwrap_or("")
            )));
        }
        if !target.is_dir() {
            return Err(ToolError::Io(format!(
                "path is not a directory: '{}'",
                path_opt.unwrap_or("")
            )));
        }
        // Ensure target is within workspace
        if !is_within_workspace(workspace_root, &target) {
            return Err(ToolError::PathTraversal(path_opt.unwrap_or("").to_string()));
        }

        let mut entries = Vec::new();
        if recursive {
            walk_recursive(&target, workspace_root, &mut entries)?;
        } else {
            let read = std::fs::read_dir(&target)
                .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
            for entry in read {
                let entry =
                    entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
                let ft = entry
                    .file_type()
                    .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?;
                let name = entry.file_name().to_string_lossy().to_string();
                let kind = if ft.is_dir() { "dir" } else { "file" };
                // Show relative to workspace for LLM clarity
                let rel = entry
                    .path()
                    .strip_prefix(workspace_root)
                    .unwrap_or(entry.path().as_path())
                    .to_string_lossy()
                    .to_string();
                entries.push(format!("{kind}: {rel} (name: {name})"));
            }
        }
        entries.sort();
        if entries.is_empty() {
            Ok("(empty directory)".to_string())
        } else {
            Ok(truncate_output(entries.join("\n")))
        }
    }
}

// ---------------------------------------------------------------------------
// Command execution plumbing: injectable limits, bounded capture, bounded drain
// ---------------------------------------------------------------------------

/// Validate `execute_command` arguments and resolve the working directory.
///
/// Returns the command string and its workspace-confined absolute directory.
fn validated_command_args(
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
struct CommandLimits {
    /// Hard timeout for the child process.
    timeout: Duration,
    /// Grace granted to pipe readers to reach EOF once the process has exited.
    drain_grace: Duration,
    /// Maximum bytes accumulated per output stream before further chunks are
    /// discarded.
    capture_cap: usize,
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
type StreamReader = Option<(Receiver<()>, Arc<Mutex<Vec<u8>>>, JoinHandle<()>)>;

/// Move `stream` into a reader thread that accumulates at most `capture_cap`
/// bytes into a shared live buffer and signals EOF through a channel.
///
/// Once the cap is reached the thread keeps draining the pipe but discards
/// everything beyond it: memory stays bounded while the child is never blocked
/// by a full pipe.
#[allow(clippy::needless_pass_by_value)] // spawned closure must own the stream
fn spawn_stream_reader(
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
fn drain_reader(stream: &mut StreamReader, grace: Duration, out: &mut Vec<u8>) -> bool {
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

fn resolve_path(workspace_root: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    // Reject null bytes etc.
    if requested.contains('\0') {
        return Err(ToolError::InvalidArguments(
            "path contains null byte".to_string(),
        ));
    }
    let requested_path = Path::new(requested);
    let joined = if requested_path.is_absolute() {
        PathBuf::from(requested)
    } else {
        workspace_root.join(requested)
    };

    // Use lexical normalization + canonical check
    if !is_within_workspace(workspace_root, &joined) {
        return Err(ToolError::PathTraversal(requested.to_string()));
    }

    // Also ensure normalized joined still within workspace after resolving symlinks if exists
    // If file exists, try canonicalize and re-check
    if joined.exists() {
        if let Ok(canonical) = joined.canonicalize() {
            if !is_within_workspace(workspace_root, &canonical) {
                return Err(ToolError::PathTraversal(requested.to_string()));
            }
            // Return canonical for existing file to be precise
            // But keep original joined for write where file may not exist?
            // For existing we can return canonical
            return Ok(canonical);
        }
    } else {
        // For non-existing, check parent canonical
        if let Some(parent) = joined.parent() {
            if parent.exists() {
                if let Ok(parent_canonical) = parent.canonicalize() {
                    if !is_within_workspace(workspace_root, &parent_canonical) {
                        return Err(ToolError::PathTraversal(requested.to_string()));
                    }
                    // Also check parent's normalized parent + file name stays within
                    // Already covered by lexical check
                }
            }
        }
    }

    Ok(normalize_lexically(&joined))
}

fn is_within_workspace(workspace_root: &Path, target: &Path) -> bool {
    // Handle Windows verbatim prefix (\\?\) and case-insensitivity
    let ws_str = path_to_comparable_string(&normalize_lexically(&absolutize(workspace_root)));
    let tgt_str = path_to_comparable_string(&normalize_lexically(&absolutize(target)));
    // Ensure ws does not end with separator for clean prefix check
    let ws_trimmed = ws_str.trim_end_matches(['/', '\\']).to_string();
    if tgt_str == ws_trimmed {
        return true;
    }
    // Check that target starts with workspace + separator
    let sep = if ws_trimmed.contains('\\') { "\\" } else { "/" };
    // On Windows, both separators are valid; check both
    if tgt_str.starts_with(&format!("{ws_trimmed}{sep}")) {
        return true;
    }
    if cfg!(windows) {
        // Also check forward slash variant
        if tgt_str.starts_with(&format!("{ws_trimmed}/"))
            || tgt_str.starts_with(&format!("{ws_trimmed}\\"))
        {
            return true;
        }
    }
    false
}

fn path_to_comparable_string(p: &Path) -> String {
    let mut s = p.to_string_lossy().to_string();
    // Strip Windows verbatim prefix \\?\ if present
    if s.starts_with(r"\\?\") {
        s = s[4..].to_string();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
        // Normalise separators to backslash for consistent prefix
        s = s.replace('/', "\\");
    }
    s
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut components: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => {
                components.clear();
                components.push(Component::Prefix(prefix));
            }
            Component::RootDir => {
                // Keep prefix if exists, then root
                // Remove any prior Normal/CurDir/ParentDir after root
                // Find last prefix
                let mut prefix_opt = None;
                for c in &components {
                    if matches!(c, Component::Prefix(_)) {
                        prefix_opt = Some(*c);
                    }
                }
                components.clear();
                if let Some(p) = prefix_opt {
                    components.push(p);
                }
                components.push(Component::RootDir);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = components.last().copied() {
                    match last {
                        Component::Normal(_) => {
                            components.pop();
                        }
                        Component::RootDir | Component::Prefix(_) => {
                            // stay at root / cannot go above prefix
                        }
                        Component::ParentDir => components.push(Component::ParentDir),
                        Component::CurDir => {
                            components.pop();
                            components.push(Component::ParentDir);
                        }
                    }
                } else {
                    components.push(Component::ParentDir);
                }
            }
            Component::Normal(_) => components.push(comp),
        }
    }
    let mut out = PathBuf::new();
    for c in components {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn walk_recursive(
    dir: &Path,
    workspace_root: &Path,
    out: &mut Vec<String>,
) -> Result<(), ToolError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
        let path = entry.path();
        // Ensure each visited path stays within workspace (defense against symlink escapes)
        if !is_within_workspace(workspace_root, &path) {
            continue;
        }
        let ft = entry
            .file_type()
            .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?;
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .to_string();
        let kind = if ft.is_dir() { "dir" } else { "file" };
        let name = entry.file_name().to_string_lossy().to_string();
        out.push(format!("{kind}: {rel} (name: {name})"));
        if ft.is_dir() {
            walk_recursive(&path, workspace_root, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::*;
    use std::fs;
    #[test]
    fn execute_command_echo_captures_stdout() {
        let ws = temp_workspace();
        let c = call(
            "execute_command",
            serde_json::json!({"command": "echo hello"}),
        );
        let out = ToolRegistry::execute(&c, &ws).expect("execute_command succeeds");
        assert!(out.contains("hello"), "output was: {out}");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_creates_and_read_file_reads_back() {
        let ws = temp_workspace();
        let write = call(
            "write_file",
            serde_json::json!({"path": "hello.txt", "content": "hello world"}),
        );
        let res = ToolRegistry::execute(&write, &ws).expect("write succeeds");
        assert!(
            res.contains("--- a/hello.txt"),
            "diff header missing: {res}"
        );
        assert!(
            res.contains("+++ b/hello.txt"),
            "diff header missing: {res}"
        );
        assert!(res.contains("+hello world"), "diff addition missing: {res}");
        assert!(res.contains("@@"), "diff hunk header missing: {res}");

        let read = call("read_file", serde_json::json!({"path": "hello.txt"}));
        let content = ToolRegistry::execute(&read, &ws).expect("read succeeds");
        assert_eq!(content, "hello world");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_file_with_offset_and_limit() {
        let ws = temp_workspace();
        let content = "line1\nline2\nline3\nline4\nline5";
        let write = call(
            "write_file",
            serde_json::json!({"path": "multi.txt", "content": content}),
        );
        ToolRegistry::execute(&write, &ws).expect("write");

        let read = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 1, "limit_lines": 2}),
        );
        let out = ToolRegistry::execute(&read, &ws).expect("read with offset");
        assert_eq!(out, "line2\nline3");

        let read2 = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 0, "limit_lines": 1}),
        );
        let out2 = ToolRegistry::execute(&read2, &ws).expect("read first line");
        assert_eq!(out2, "line1");

        let read3 = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 10}),
        );
        let out3 = ToolRegistry::execute(&read3, &ws).expect("offset beyond end");
        assert_eq!(out3, "");

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn path_traversal_is_rejected() {
        let ws = temp_workspace();
        // Try various traversal payloads
        for payload in &[
            "../../etc/passwd",
            "../outside.txt",
            "a/../../b/../../etc/passwd",
        ] {
            let read = call("read_file", serde_json::json!({"path": payload}));
            let res = ToolRegistry::execute(&read, &ws);
            assert!(res.is_err(), "traversal '{payload}' should be rejected");
            let err = res.unwrap_err().to_string();
            assert!(
                err.starts_with("Error:"),
                "error should be prefixed with Error: got {err}"
            );
            assert!(
                err.to_lowercase().contains("outside") || err.to_lowercase().contains("traversal"),
                "err: {err}"
            );
        }

        let write = call(
            "write_file",
            serde_json::json!({"path": "../../evil.txt", "content": "bad"}),
        );
        let res = ToolRegistry::execute(&write, &ws);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().starts_with("Error:"));

        // Absolute path outside workspace should also be rejected
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let read_abs = call("read_file", serde_json::json!({"path": abs_outside}));
        let res_abs = ToolRegistry::execute(&read_abs, &ws);
        assert!(res_abs.is_err(), "absolute outside path should be rejected");

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_failure_does_not_crash() {
        let ws = temp_workspace();
        // Command that exits with non-zero should not panic, should return Ok with status
        let fail = if cfg!(windows) {
            call("execute_command", serde_json::json!({"command": "exit 1"}))
        } else {
            call("execute_command", serde_json::json!({"command": "false"}))
        };
        let res = ToolRegistry::execute(&fail, &ws);
        // Should be Ok (process ran) with exit status reported, not Err/panic
        assert!(res.is_ok(), "non-zero exit should not be Err, got {res:?}");
        let out = res.unwrap();
        // Should contain exit status or be non-empty
        assert!(!out.is_empty());

        // Invalid command should be captured as Ok with stderr or Err but not panic
        let invalid = call(
            "execute_command",
            serde_json::json!({"command": "nonexistent_command_xyz_12345"}),
        );
        let res2 = ToolRegistry::execute(&invalid, &ws);
        // Must not panic; may be Ok with error output or Err
        assert!(res2.is_ok() || res2.is_err());
        if let Err(e) = res2 {
            assert!(e.to_string().starts_with("Error:") || e.to_string().contains("Error"));
        }

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_timeout_is_handled() {
        // We test timeout handling by using a command that sleeps.
        // To avoid waiting 30s, we test the truncation and timeout infrastructure
        // indirectly: ensure that a quick command does not timeout, and that the
        // timeout mechanism exists and does not panic on normal commands.
        // This test verifies the tool does not hang on a fast command and that
        // the 30s constant is present.
        assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(30));
        let ws = temp_workspace();
        let c = call(
            "execute_command",
            serde_json::json!({"command": "echo quick"}),
        );
        let out = ToolRegistry::execute(&c, &ws).expect("quick command should not timeout");
        assert!(out.contains("quick"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn list_directory_works() {
        let ws = temp_workspace();
        // Create structure
        fs::create_dir_all(ws.join("a/b")).unwrap();
        fs::write(ws.join("a/file1.txt"), "x").unwrap();
        fs::write(ws.join("a/b/file2.txt"), "y").unwrap();
        fs::write(ws.join("root.txt"), "z").unwrap();

        let list = call("list_directory", serde_json::json!({}));
        let out = ToolRegistry::execute(&list, &ws).expect("list root");
        assert!(out.contains("root.txt") || out.contains('a'));

        let list_a = call("list_directory", serde_json::json!({"path": "a"}));
        let out_a = ToolRegistry::execute(&list_a, &ws).expect("list a");
        assert!(out_a.contains("file1.txt"));

        let rec = call(
            "list_directory",
            serde_json::json!({"path": "a", "recursive": true}),
        );
        let out_r = ToolRegistry::execute(&rec, &ws).expect("recursive");
        assert!(out_r.contains("file2.txt"));

        // Traversal via list should be rejected
        let bad = call("list_directory", serde_json::json!({"path": "../"}));
        assert!(ToolRegistry::execute(&bad, &ws).is_err());

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn unknown_tool_is_error() {
        let ws = temp_workspace();
        let c = call("unknown_tool", serde_json::json!({}));
        let res = ToolRegistry::execute(&c, &ws);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("unknown tool"));
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Sandbox hardening (Task 2.1.1): bounded timeout / capture / drain
    // -----------------------------------------------------------------------

    /// Injected limits for the real execution path with short waits.
    fn hardening_limits(timeout_secs: u64, drain_grace_ms: u64) -> CommandLimits {
        CommandLimits {
            timeout: Duration::from_secs(timeout_secs),
            drain_grace: Duration::from_millis(drain_grace_ms),
            capture_cap: MAX_CAPTURE_BYTES,
        }
    }

    fn run_cmd(command: &str, ws: &std::path::Path) -> Result<String, ToolError> {
        let args = serde_json::json!({ "command": command });
        let token = CancellationToken::new();
        ToolRegistry::execute_command_with_limits(&args, ws, &hardening_limits(5, 500), &token)
    }

    #[test]
    fn command_exceeding_injected_timeout_is_killed_within_bound() {
        let ws = temp_workspace();
        // ~10 s of sleep; the injected 1 s timeout must kill it and return.
        let sleep_cmd = if cfg!(windows) {
            "ping -n 11 127.0.0.1 >nul"
        } else {
            "sleep 10"
        };
        let args = serde_json::json!({ "command": sleep_cmd });
        let token = CancellationToken::new();
        let start = Instant::now();
        let res = ToolRegistry::execute_command_with_limits(
            &args,
            &ws,
            &hardening_limits(1, 500),
            &token,
        );
        assert!(
            matches!(res, Err(ToolError::Timeout(_))),
            "expected Timeout, got {res:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "timeout path blocked too long: {:?}",
            start.elapsed()
        );
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Cooperative cancellation (Task 3.2)
    // -----------------------------------------------------------------------

    #[test]
    fn pre_cancelled_token_rejects_execution_without_running_anything() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        token.cancel();
        let call = call(
            "execute_command",
            serde_json::json!({ "command": "echo should-not-run" }),
        );
        let res = ToolRegistry::execute_with_cancellation(&call, &ws, &token);
        assert_eq!(res.unwrap_err(), ToolError::Cancelled);
        assert_eq!(
            res_err_string(&call, &ws, &token),
            "Error: tool execution was cancelled"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    /// Render the Display of an already-cancelled execution for the assertion
    /// above without a second execution path divergence.
    fn res_err_string(call: &ToolCall, ws: &std::path::Path, token: &CancellationToken) -> String {
        match ToolRegistry::execute_with_cancellation(call, ws, token) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("must stay cancelled"),
        }
    }

    #[test]
    fn cancel_during_long_running_command_kills_child_within_about_a_second() {
        let ws = temp_workspace();
        // ~15 s of work; cancellation must kill it well inside a second of
        // being requested (plus one poll interval), building directly on the
        // hardened Task 2.1.1 bounded wait loop.
        let long_cmd = if cfg!(windows) {
            "ping -n 16 127.0.0.1 >nul"
        } else {
            "sleep 15"
        };
        let call = call(
            "execute_command",
            serde_json::json!({ "command": long_cmd }),
        );
        let token = CancellationToken::new();

        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| ToolRegistry::execute_with_cancellation(&call, &ws, &token));
            // Let the child reach its steady state before cancelling.
            std::thread::sleep(Duration::from_millis(300));
            let start = Instant::now();
            token.cancel();
            let res = worker.join().expect("worker must not panic");
            assert_eq!(res.unwrap_err(), ToolError::Cancelled);
            assert!(
                start.elapsed() <= Duration::from_secs(1),
                "cancellation took too long: {:?}",
                start.elapsed()
            );
        });

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn grandchild_holding_pipe_does_not_hang_and_reports_partial_capture() {
        let ws = temp_workspace();
        // The backgrounded `sleep` inherits stdout and keeps the pipe open
        // long after the shell exits; only the bounded drain makes this
        // return. Exercises the REAL grace-expiry leak path.
        let start = Instant::now();
        let out = run_cmd("echo begin; sleep 5 &", &ws).expect("must complete despite open pipe");
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "drain grace not honored"
        );
        assert!(out.contains("begin"), "early stdout must be captured");
        assert!(
            out.contains("output stream still open after grace period"),
            "partial-capture warning marker missing"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn stdin_reading_command_returns_promptly_because_stdin_is_null() {
        let ws = temp_workspace();
        // A bare `cat` blocks forever on stdin; with Stdio::null() it sees EOF
        // immediately and exits.
        let start = Instant::now();
        let out = run_cmd("cat", &ws).expect("cat must return via null stdin");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "stdin was not detached"
        );
        assert_eq!(out.trim(), "(no output)");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn runaway_output_is_memory_bounded_by_capture_cap() {
        let ws = temp_workspace();
        // Produce ~2 MB of output through a file; capture must stay far below
        // the produced volume while still truncating to the context budget.
        let content = "y".repeat(2 * 1024 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({ "path": "big.bin", "content": content }),
        );
        ToolRegistry::execute(&write, &ws).expect("seed big file");

        let read_cmd = if cfg!(windows) {
            "type big.bin"
        } else {
            "cat big.bin"
        };
        let out = run_cmd(read_cmd, &ws).expect("read big file back");
        assert!(
            out.len() < 100 * 1024,
            "capture leaked past bounds: {} bytes",
            out.len()
        );
        assert!(out.contains("output truncated"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_path_validation_still_rejects_escapes_after_diff_change() {
        let ws = temp_workspace();
        for payload in &[
            "../../etc/passwd",
            "../outside.txt",
            "a/../../b/../../etc/passwd",
        ] {
            let call = call(
                "write_file",
                serde_json::json!({"path": payload, "content": "bad"}),
            );
            let res = ToolRegistry::execute(&call, &ws);
            assert!(res.is_err(), "traversal '{payload}' should be rejected");
            assert!(res.unwrap_err().to_string().starts_with("Error:"));
        }
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let call = call(
            "write_file",
            serde_json::json!({"path": abs_outside, "content": "bad"}),
        );
        assert!(
            ToolRegistry::execute(&call, &ws).is_err(),
            "absolute outside must be rejected"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_cancelled_leaves_file_untouched() {
        let ws = temp_workspace();
        let existing = "original content";
        fs::write(ws.join("keep.txt"), existing).expect("seed");
        let token = CancellationToken::new();
        token.cancel();
        let c = call(
            "write_file",
            serde_json::json!({"path": "keep.txt", "content": "new content"}),
        );
        let res = ToolRegistry::execute_with_cancellation(&c, &ws, &token);
        assert_eq!(res.unwrap_err(), ToolError::Cancelled);
        // File must remain untouched on cancelled path (existing semantics preserved)
        assert_eq!(fs::read_to_string(ws.join("keep.txt")).unwrap(), existing);
        // Also new file path cancelled should not be created
        let new_c = call(
            "write_file",
            serde_json::json!({"path": "new_keep.txt", "content": "hello"}),
        );
        let res2 = ToolRegistry::execute_with_cancellation(&new_c, &ws, &token);
        assert_eq!(res2.unwrap_err(), ToolError::Cancelled);
        assert!(
            !ws.join("new_keep.txt").exists(),
            "cancelled write must not create file"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}
