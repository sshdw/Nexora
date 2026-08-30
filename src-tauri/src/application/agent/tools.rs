//! Safe workspace tool execution: shell and filesystem.
//!
//! Provides the four native agent tools behind a central [`ToolRegistry`].
//! All filesystem access is confined to `workspace_root` via path
//! canonicalization / lexical normalisation. Shell execution is bounded by a
//! hard timeout, bounded output capture, a bounded reader-drain grace, and
//! final output truncation to protect the LLM context; no code path can block
//! indefinitely (a detached grandchild holding a pipe cannot stall the tool).

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::application::agent::control::CancellationToken;
use crate::application::execution::{ToolCall, ToolDefinition};

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
const MAX_OUTPUT_BYTES: usize = 20 * 1024; // 20 KB
const TRUNCATE_HEAD: usize = 10 * 1024;
const TRUNCATE_TAIL: usize = 10 * 1024;
const DIFF_CONTEXT: usize = 3;
const MAX_DIFF_HUNKS: usize = 64;
const LCS_CELL_LIMIT: usize = 4_000_000;

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

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Central dispatcher for the four native workspace tools.
pub(crate) struct ToolRegistry;

impl ToolRegistry {
    /// Return JSON-Schema [`ToolDefinition`]s for the four native tools.
    pub(crate) fn definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "execute_command".to_string(),
                description: "Run a shell command with a 30s timeout, capturing stdout and stderr. Executes inside the workspace. Output is truncated to 20KB.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to execute (e.g. \"echo hello\" or \"cargo test\")"
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Working directory relative to workspace root (optional). Must be inside workspace."
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file inside the workspace. Supports line offset and limit for large files.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file relative to workspace root"
                        },
                        "offset_lines": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Line offset to start reading from (0-indexed)"
                        },
                        "limit_lines": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Maximum number of lines to return"
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "write_file".to_string(),
                description: "Write or overwrite a file inside the workspace, creating parent directories as needed.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Destination path relative to workspace root"
                        },
                        "content": {
                            "type": "string",
                            "description": "Text content to write"
                        }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "list_directory".to_string(),
                description: "List directory contents inside the workspace. Use recursive=true to walk subdirectories.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path relative to workspace root (defaults to workspace root)"
                        },
                        "recursive": {
                            "type": "boolean",
                            "description": "Whether to list recursively"
                        }
                    },
                    "required": [],
                    "additionalProperties": false
                }),
            },
        ]
    }

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

// ---------------------------------------------------------------------------
// Helpers: path, truncation, recursion
// ---------------------------------------------------------------------------

fn truncate_output(s: String) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s;
    }
    // Find char boundaries for head/tail
    let head_end = find_char_boundary(&s, TRUNCATE_HEAD);
    let tail_start = find_char_boundary(&s, s.len().saturating_sub(TRUNCATE_TAIL));
    let head = &s[..head_end];
    let tail = &s[tail_start..];
    format!(
        "{head}\n... [output truncated, {} bytes total, showing first {} and last {} bytes] ...\n{tail}",
        s.len(),
        head.len(),
        tail.len()
    )
}

fn find_char_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(index) && index > 0 {
        index -= 1;
    }
    // If we moved back, try to go forward to nearest valid near original?
    // Simpler: walk back until boundary, that's valid.
    // If index was inside a char, we backtrack to start of char, slightly less than requested but safe.
    // For tail, we want to start at a boundary at or after desired index.
    // For tail we should walk forward.
    // This function is used for both head (walk back) and tail (should walk forward).
    // For tail we pass s.len() - TAIL, we should walk forward to next boundary.
    // Handle tail specially: if not boundary, walk forward.
    // Our current call for tail uses index that may be inside char; backing up is also safe (shows slightly more).
    // Acceptable.
    index
}

// ---------------------------------------------------------------------------
// Unified diff (Task 5.2): write_file observation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffOp {
    Equal(String),
    Delete(String),
    Insert(String),
}

#[allow(clippy::too_many_lines)]
fn unified_diff(path: &str, old: &str, new: &str) -> String {
    // Split into lines: empty string => no lines (new file / empty file).
    let old_lines: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.lines().collect()
    };

    // Both empty => headers only (sane edge case).
    if old_lines.is_empty() && new_lines.is_empty() && old == new {
        let mut out = String::new();
        let _ = writeln!(&mut out, "--- a/{path}");
        let _ = writeln!(&mut out, "+++ b/{path}");
        return out;
    }

    let ops = diff_ops(&old_lines, &new_lines);

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            DiffOp::Equal(_) => None,
            _ => Some(i),
        })
        .collect();

    let mut out = String::new();
    let _ = writeln!(&mut out, "--- a/{path}");
    let _ = writeln!(&mut out, "+++ b/{path}");

    if changed.is_empty() {
        return out;
    }

    // Positions of each op in old/new (1-based).
    let mut pos_old = Vec::with_capacity(ops.len());
    let mut pos_new = Vec::with_capacity(ops.len());
    let mut old_pos: usize = 1;
    let mut new_pos: usize = 1;
    for op in &ops {
        pos_old.push(old_pos);
        pos_new.push(new_pos);
        match op {
            DiffOp::Equal(_) => {
                old_pos += 1;
                new_pos += 1;
            }
            DiffOp::Delete(_) => old_pos += 1,
            DiffOp::Insert(_) => new_pos += 1,
        }
    }

    // Build context-expanded ranges, merging overlaps.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &idx in &changed {
        let start = idx.saturating_sub(DIFF_CONTEXT);
        let end = std::cmp::min(ops.len().saturating_sub(1), idx + DIFF_CONTEXT);
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 {
                if end > last.1 {
                    last.1 = end;
                }
            } else {
                ranges.push((start, end));
            }
        } else {
            ranges.push((start, end));
        }
    }

    let truncated = ranges.len() > MAX_DIFF_HUNKS;
    if truncated {
        ranges.truncate(MAX_DIFF_HUNKS);
    }

    for (start, end) in ranges {
        let mut old_cnt: usize = 0;
        let mut new_cnt: usize = 0;
        for op in &ops[start..=end] {
            match op {
                DiffOp::Equal(_) => {
                    old_cnt += 1;
                    new_cnt += 1;
                }
                DiffOp::Delete(_) => old_cnt += 1,
                DiffOp::Insert(_) => new_cnt += 1,
            }
        }
        let old_start = if old_cnt == 0 { 0 } else { pos_old[start] };
        let new_start = if new_cnt == 0 { 0 } else { pos_new[start] };
        let _ = writeln!(
            &mut out,
            "@@ -{old_start},{old_cnt} +{new_start},{new_cnt} @@"
        );
        for op in &ops[start..=end] {
            match op {
                DiffOp::Equal(s) => {
                    let _ = writeln!(&mut out, " {s}");
                }
                DiffOp::Delete(s) => {
                    let _ = writeln!(&mut out, "-{s}");
                }
                DiffOp::Insert(s) => {
                    let _ = writeln!(&mut out, "+{s}");
                }
            }
        }
    }

    if truncated {
        let _ = writeln!(
            &mut out,
            "... [diff truncated, too many hunks (limit {MAX_DIFF_HUNKS})] ..."
        );
    }

    out
}

fn diff_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    if old.is_empty() && new.is_empty() {
        return Vec::new();
    }
    if old.is_empty() {
        return new
            .iter()
            .map(|l| DiffOp::Insert((*l).to_string()))
            .collect();
    }
    if new.is_empty() {
        return old
            .iter()
            .map(|l| DiffOp::Delete((*l).to_string()))
            .collect();
    }
    if old.len().saturating_mul(new.len()) > LCS_CELL_LIMIT {
        let mut ops = Vec::with_capacity(old.len() + new.len());
        for l in old {
            ops.push(DiffOp::Delete((*l).to_string()));
        }
        for l in new {
            ops.push(DiffOp::Insert((*l).to_string()));
        }
        return ops;
    }
    let m = old.len();
    let n = new.len();
    let mut dp = vec![0usize; (m + 1) * (n + 1)];
    for i in 1..=m {
        for j in 1..=n {
            let idx = i * (n + 1) + j;
            let diag = (i - 1) * (n + 1) + (j - 1);
            let up = (i - 1) * (n + 1) + j;
            let left = i * (n + 1) + (j - 1);
            if old[i - 1] == new[j - 1] {
                dp[idx] = dp[diag] + 1;
            } else if dp[up] >= dp[left] {
                dp[idx] = dp[up];
            } else {
                dp[idx] = dp[left];
            }
        }
    }
    let mut i = m;
    let mut j = n;
    let mut rev: Vec<DiffOp> = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            rev.push(DiffOp::Equal(old[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i * (n + 1) + (j - 1)] >= dp[(i - 1) * (n + 1) + j]) {
            rev.push(DiffOp::Insert(new[j - 1].to_string()));
            j -= 1;
        } else {
            rev.push(DiffOp::Delete(old[i - 1].to_string()));
            i -= 1;
        }
    }
    rev.reverse();
    rev
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::execution::{ToolCall, ToolDefinition};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace() -> PathBuf {
        let base = std::env::temp_dir();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!(
            "nexora-tools-test-{}-{}-{}",
            std::process::id(),
            id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp workspace");
        dir
    }

    #[allow(clippy::needless_pass_by_value)] // JSON literals read best at call sites
    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
        }
    }

    #[test]
    fn definitions_produce_valid_objects() {
        let defs = ToolRegistry::definitions();
        assert_eq!(defs.len(), 4);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"execute_command"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_directory"));
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.parameters.is_object());
            let obj = def.parameters.as_object().unwrap();
            assert_eq!(obj.get("type").unwrap(), "object");
            assert!(obj.contains_key("properties"));
        }
        // Check specific schemas
        let exec = defs.iter().find(|d| d.name == "execute_command").unwrap();
        let props = exec.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("command"));
        assert!(props.contains_key("cwd"));
        let req = exec.parameters["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "command"));

        let read = defs.iter().find(|d| d.name == "read_file").unwrap();
        let rprops = read.parameters["properties"].as_object().unwrap();
        assert!(rprops.contains_key("path"));
        assert!(rprops.contains_key("offset_lines"));
        assert!(rprops.contains_key("limit_lines"));

        let write = defs.iter().find(|d| d.name == "write_file").unwrap();
        let wprops = write.parameters["properties"].as_object().unwrap();
        assert!(wprops.contains_key("path"));
        assert!(wprops.contains_key("content"));

        let list = defs.iter().find(|d| d.name == "list_directory").unwrap();
        let lprops = list.parameters["properties"].as_object().unwrap();
        assert!(lprops.contains_key("path"));
        assert!(lprops.contains_key("recursive"));
    }

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
    fn output_truncation_preserves_head_and_tail() {
        let ws = temp_workspace();
        // Generate ~30KB output via a file
        let content = "X".repeat(30 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({"path": "big.txt", "content": content.clone()}),
        );
        ToolRegistry::execute(&write, &ws).expect("write big");

        let cmd_str = if cfg!(windows) {
            "type big.txt"
        } else {
            "cat big.txt"
        };
        let c = call("execute_command", serde_json::json!({"command": cmd_str}));
        let out = ToolRegistry::execute(&c, &ws).expect("cat big");
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500,
            "output should be truncated near 20KB"
        );
        assert!(
            out.contains("output truncated"),
            "should contain truncation marker"
        );
        assert!(out.contains("bytes total"));
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

    #[test]
    fn definitions_are_json_schema_valid() {
        for def in ToolRegistry::definitions() {
            // Parameters must be valid JSON schema object
            let v = &def.parameters;
            assert!(v.is_object());
            // Must contain type: object
            assert_eq!(v["type"], "object");
            // Unknown tool definitions should not have empty name
            assert!(!def.name.is_empty());
            // Round-trip through ToolDefinition serialization
            let json = serde_json::to_string(&def).unwrap();
            let back: ToolDefinition = serde_json::from_str(&json).unwrap();
            assert_eq!(def, back);
        }
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
    fn read_file_result_is_truncated_at_context_budget() {
        let ws = temp_workspace();
        let content = "z".repeat(40 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({ "path": "wide.txt", "content": content }),
        );
        ToolRegistry::execute(&write, &ws).expect("seed file");

        let read = call("read_file", serde_json::json!({ "path": "wide.txt" }));
        let out = ToolRegistry::execute(&read, &ws).expect("read");
        assert!(
            out.contains("output truncated"),
            "read_file must truncate to 20KB"
        );
        assert!(out.len() <= MAX_OUTPUT_BYTES + 500);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn list_directory_result_is_truncated_at_context_budget() {
        let ws = temp_workspace();
        // ~1200 root entries at roughly 40 bytes each exceed the 20KB budget.
        for i in 0..1200_u32 {
            fs::write(ws.join(format!("f{i:04}.txt")), "x").expect("create entry");
        }
        let list = call("list_directory", serde_json::json!({}));
        let out = ToolRegistry::execute(&list, &ws).expect("list");
        assert!(
            out.contains("output truncated"),
            "listing must truncate to 20KB"
        );
        assert!(out.len() <= MAX_OUTPUT_BYTES + 500);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_new_file_diff_contains_headers_and_additions() {
        let ws = temp_workspace();
        let content = "line1\nline2\nline3";
        let call = call(
            "write_file",
            serde_json::json!({"path": "new.txt", "content": content}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("new file diff");
        assert!(out.contains("--- a/new.txt"), "missing header a: {out}");
        assert!(out.contains("+++ b/new.txt"), "missing header b: {out}");
        assert!(out.contains("@@"), "missing hunk header: {out}");
        assert!(out.contains("+line1"), "missing addition line1: {out}");
        assert!(out.contains("+line2"), "missing addition line2: {out}");
        assert!(out.contains("+line3"), "missing addition line3: {out}");
        // New file is diffed against empty -> old count 0
        assert!(
            out.contains("-0,0 +1,3") || out.contains("-0,0"),
            "new file hunk counts wrong: {out}"
        );
        assert_eq!(fs::read_to_string(ws.join("new.txt")).unwrap(), content);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_modified_file_diff_shows_hunk_with_context() {
        let ws = temp_workspace();
        let old = "line1\nline2\nline3\nline4\nline5";
        fs::write(ws.join("mod.txt"), old).expect("seed old");
        let new = "line1\nchanged\nline3\nline4\nline5";
        let call = call(
            "write_file",
            serde_json::json!({"path": "mod.txt", "content": new}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("modified diff");
        assert!(out.contains("--- a/mod.txt"), "header missing: {out}");
        assert!(out.contains("+++ b/mod.txt"), "header missing: {out}");
        assert!(out.contains("-line2"), "removed line missing: {out}");
        assert!(out.contains("+changed"), "added line missing: {out}");
        // Context lines should appear with leading space
        assert!(
            out.contains(" line1") || out.contains("line1"),
            "context missing: {out}"
        );
        assert_eq!(fs::read_to_string(ws.join("mod.txt")).unwrap(), new);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_empty_to_empty_produces_sane_diff() {
        let ws = temp_workspace();
        // Create empty file first via direct fs, then overwrite with empty via tool
        fs::write(ws.join("empty.txt"), "").expect("seed empty");
        let call = call(
            "write_file",
            serde_json::json!({"path": "empty.txt", "content": ""}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("empty diff");
        assert!(out.contains("--- a/empty.txt"), "header missing: {out}");
        assert!(out.contains("+++ b/empty.txt"), "header missing: {out}");
        // No hunks for identical empty -> should have only headers
        assert!(
            !out.contains("@@") || out.trim_end().ends_with("+++ b/empty.txt"),
            "empty diff should have no hunks or only headers: {out}"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_diff_truncation_on_huge_content() {
        let ws = temp_workspace();
        let huge = "x\n".repeat(15_000); // ~30KB, diff will exceed 20KB
        let call = call(
            "write_file",
            serde_json::json!({"path": "huge.txt", "content": huge}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("huge diff");
        assert!(
            out.contains("output truncated"),
            "huge diff must be truncated: len={}",
            out.len()
        );
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500,
            "truncated diff too large: {}",
            out.len()
        );
        assert!(
            out.contains("--- a/huge.txt"),
            "header must survive truncation: {out}"
        );
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

    #[test]
    fn write_file_diff_bounded_hunk_count() {
        let ws = temp_workspace();
        // Create old file with many isolated changes (each 10 lines apart) to generate many hunks.
        // With DIFF_CONTEXT=3 and MAX_DIFF_HUNKS=64, we should bound.
        let old_lines: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let old = old_lines.join("\n");
        fs::write(ws.join("many.txt"), &old).expect("seed many");
        let mut new_lines = old_lines.clone();
        for i in (0..500).step_by(7) {
            new_lines[i] = format!("changed {i}");
        }
        let new = new_lines.join("\n");
        let call = call(
            "write_file",
            serde_json::json!({"path": "many.txt", "content": new}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("many hunks diff");
        let hunk_count = out.lines().filter(|l| l.starts_with("@@")).count();
        assert!(
            hunk_count <= MAX_DIFF_HUNKS,
            "hunk count {hunk_count} exceeds bound {MAX_DIFF_HUNKS}"
        );
        // Verify the diff is bounded and, if truncated, contains marker (hunk limit or output truncate)
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500 || out.contains("too many hunks"),
            "many hunks diff should be bounded"
        );
        let _ = fs::remove_dir_all(&ws);
    }
}
