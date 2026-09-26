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
            "edit_file" => Self::edit_file(&args, workspace_root),
            "search_files" => Self::search_files(&args, workspace_root),
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

    /// Exact-once in-place edit: `old_text`/`new_text` replace mode or
    /// `insert_after`/`new_text` insert mode (exactly one of the two anchors).
    ///
    /// The anchor must occur exactly once: zero matches fail with
    /// `no exact match found`, two or more fail with
    /// `pattern matches N locations, refusing to guess` — the tool never
    /// guesses. New text is spliced verbatim (callers include newlines).
    /// Files must be valid UTF-8; the workspace guard is shared with
    /// `read_file`/`write_file`.
    fn edit_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let new_text = args
            .get("new_text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::InvalidArguments("missing required field 'new_text'".to_string())
            })?;
        let old_text = args.get("old_text").and_then(Value::as_str);
        let insert_after = args.get("insert_after").and_then(Value::as_str);
        let anchor = match (old_text, insert_after) {
            (Some(_), Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "supply exactly one of 'old_text' or 'insert_after'".to_string(),
                ));
            }
            (None, None) => {
                return Err(ToolError::InvalidArguments(
                    "missing required field 'old_text' or 'insert_after'".to_string(),
                ));
            }
            (Some(old), None) => EditAnchor::Replace(old),
            (None, Some(after)) => EditAnchor::InsertAfter(after),
        };
        if anchor.text().is_empty() {
            return Err(ToolError::InvalidArguments(
                "anchor text must not be empty".to_string(),
            ));
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
        let bytes = std::fs::read(&resolved)
            .map_err(|e| ToolError::Io(format!("failed to read file '{path}': {e}")))?;
        let content = String::from_utf8(bytes).map_err(|_| {
            ToolError::Io(format!("file is not valid UTF-8, cannot edit: '{path}'"))
        })?;

        let occurrences = content.match_indices(anchor.text()).count();
        if occurrences == 0 {
            return Err(ToolError::Io("no exact match found".to_string()));
        }
        if occurrences > 1 {
            return Err(ToolError::Io(format!(
                "pattern matches {occurrences} locations, refusing to guess"
            )));
        }
        let updated = match anchor {
            EditAnchor::Replace(old) => content.replacen(old, new_text, 1),
            EditAnchor::InsertAfter(after) => {
                content.replacen(after, &format!("{after}{new_text}"), 1)
            }
        };

        std::fs::write(&resolved, &updated)
            .map_err(|e| ToolError::Io(format!("failed to write file '{path}': {e}")))?;

        Ok(truncate_output(unified_diff(path, &content, &updated)))
    }

    /// Regex-lite grep over the workspace: one `path:line:text` hit per
    /// matching line, honouring `max_matches` (default 50, hard cap 200).
    ///
    /// Binary files (NUL-byte heuristic), files over 5 MiB, unreadable files,
    /// and symbolic links are skipped; the walk never leaves the workspace.
    /// Long lines are middle-truncated with an edge-kept notice.
    fn search_files(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let pattern = args.get("pattern").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'pattern'".to_string())
        })?;
        if pattern.is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'pattern' must not be empty".to_string(),
            ));
        }
        if pattern.len() > MAX_SEARCH_PATTERN_BYTES {
            return Err(ToolError::InvalidArguments(format!(
                "field 'pattern' exceeds {MAX_SEARCH_PATTERN_BYTES} bytes"
            )));
        }
        let regex = LiteRegex::compile(pattern);
        let scope_arg = args
            .get("directory")
            .and_then(Value::as_str)
            .or_else(|| args.get("path").and_then(Value::as_str));
        let scope = match scope_arg {
            Some(dir) if !dir.trim().is_empty() => resolve_path(workspace_root, dir)?,
            _ => workspace_root.to_path_buf(),
        };
        if !scope.exists() {
            return Err(ToolError::Io(format!(
                "directory not found: '{}'",
                scope_arg.unwrap_or("")
            )));
        }
        if !scope.is_dir() {
            return Err(ToolError::Io(format!(
                "search scope is not a directory: '{}'",
                scope_arg.unwrap_or("")
            )));
        }
        if !is_within_workspace(workspace_root, &scope) {
            return Err(ToolError::PathTraversal(
                scope_arg.unwrap_or("").to_string(),
            ));
        }
        let max_matches = match args.get("max_matches") {
            None => SEARCH_DEFAULT_MAX_MATCHES,
            Some(value) => {
                let raw = value.as_u64().ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "field 'max_matches' must be a non-negative integer".to_string(),
                    )
                })?;
                usize::try_from(raw)
                    .unwrap_or(usize::MAX)
                    .clamp(1, SEARCH_HARD_CAP_MATCHES)
            }
        };

        let mut hits: Vec<String> = Vec::new();
        let mut capped = false;
        walk_search(
            &scope,
            workspace_root,
            &regex,
            max_matches,
            &mut hits,
            &mut capped,
        )?;
        if hits.is_empty() {
            return Ok("(no matches)".to_string());
        }
        let mut out = hits.join("\n");
        if capped {
            let _ = writeln!(
                out,
                "\n[match cap reached: showing first {max_matches} matches (cap {SEARCH_HARD_CAP_MATCHES})]"
            );
        }
        Ok(truncate_output(out))
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

/// Edit mode selected by which anchor argument was supplied.
#[derive(Debug, Clone, Copy)]
enum EditAnchor<'a> {
    Replace(&'a str),
    InsertAfter(&'a str),
}

impl<'a> EditAnchor<'a> {
    fn text(self) -> &'a str {
        match self {
            Self::Replace(old) | Self::InsertAfter(old) => old,
        }
    }
}

// ---------------------------------------------------------------------------
// Regex-lite search (`search_files`)
// ---------------------------------------------------------------------------

/// Default/maximum matches returned by `search_files`.
const SEARCH_DEFAULT_MAX_MATCHES: usize = 50;
const SEARCH_HARD_CAP_MATCHES: usize = 200;
/// Patterns longer than this are rejected rather than compiled.
const MAX_SEARCH_PATTERN_BYTES: usize = 10 * 1024;
/// Files larger than this are skipped (binary-ish / unbounded-read guard).
const MAX_SEARCH_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Bytes sniffed for NUL when deciding a file is binary.
const BINARY_SNIFF_BYTES: usize = 8000;
/// Display budget for one hit line before middle truncation.
const SEARCH_LINE_BUDGET_BYTES: usize = 1000;
const SEARCH_LINE_EDGE_BYTES: usize = 500;
/// Total atom tests for one line before the match gives up (hang guard for
/// pathological quantifiers on huge single-line files).
const SEARCH_MATCH_STEP_BUDGET: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchAtom {
    Lit(char),
    Dot,
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchQuant {
    One,
    ZeroMore,
    OneMore,
    ZeroOne,
}

#[derive(Debug, Clone, Copy)]
struct SearchToken {
    atom: SearchAtom,
    quant: SearchQuant,
}

/// Regex-lite pattern: literals, `.`, `*`/`+`/`?` on the preceding element,
/// `^`/`$` anchors, `\d \D \w \W \s \S`. Every other metacharacter
/// (`( ) [ ] { } |`) matches literally. Matching is case-sensitive.
#[derive(Debug, Clone)]
struct LiteRegex {
    anchored_start: bool,
    anchored_end: bool,
    tokens: Vec<SearchToken>,
}

impl LiteRegex {
    fn compile(pattern: &str) -> Self {
        let mut anchored_start = false;
        let mut body = pattern;
        if let Some(rest) = body.strip_prefix('^') {
            anchored_start = true;
            body = rest;
        }
        let mut anchored_end = false;
        let mut tokens: Vec<SearchToken> = Vec::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    let atom = match chars.next() {
                        None => SearchAtom::Lit('\\'),
                        Some('d') => SearchAtom::Digit,
                        Some('D') => SearchAtom::NotDigit,
                        Some('w') => SearchAtom::Word,
                        Some('W') => SearchAtom::NotWord,
                        Some('s') => SearchAtom::Space,
                        Some('S') => SearchAtom::NotSpace,
                        Some(other) => SearchAtom::Lit(other),
                    };
                    tokens.push(SearchToken {
                        atom,
                        quant: SearchQuant::One,
                    });
                }
                '.' => tokens.push(SearchToken {
                    atom: SearchAtom::Dot,
                    quant: SearchQuant::One,
                }),
                // A trailing unescaped `$` anchors; anywhere else it is literal.
                // (`\$` is consumed by the escape arm above, so it stays literal.)
                '$' if chars.peek().is_none() => anchored_end = true,
                '*' | '+' | '?' => {
                    if let Some(last) = tokens.last_mut() {
                        last.quant = match c {
                            '*' => SearchQuant::ZeroMore,
                            '+' => SearchQuant::OneMore,
                            _ => SearchQuant::ZeroOne,
                        };
                    } else {
                        tokens.push(SearchToken {
                            atom: SearchAtom::Lit(c),
                            quant: SearchQuant::One,
                        });
                    }
                }
                other => tokens.push(SearchToken {
                    atom: SearchAtom::Lit(other),
                    quant: SearchQuant::One,
                }),
            }
        }
        Self {
            anchored_start,
            anchored_end,
            tokens,
        }
    }

    fn is_match(&self, line: &str) -> bool {
        let chars: Vec<char> = line.chars().collect();
        if self.anchored_start {
            let mut budget = SEARCH_MATCH_STEP_BUDGET;
            self.match_at(&chars, 0, &mut budget)
                .is_some_and(|end| !self.anchored_end || end == chars.len())
        } else {
            (0..=chars.len()).any(|start| {
                let mut budget = SEARCH_MATCH_STEP_BUDGET;
                self.match_at(&chars, start, &mut budget)
                    .is_some_and(|end| !self.anchored_end || end == chars.len())
            })
        }
    }

    fn match_at(&self, chars: &[char], start: usize, budget: &mut usize) -> Option<usize> {
        self.match_tokens(0, chars, start, budget)
    }

    fn match_tokens(
        &self,
        token_index: usize,
        chars: &[char],
        char_index: usize,
        budget: &mut usize,
    ) -> Option<usize> {
        if token_index == self.tokens.len() {
            return Some(char_index);
        }
        let token = &self.tokens[token_index];
        match token.quant {
            SearchQuant::One => {
                if char_index < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index, budget)
                {
                    self.match_tokens(token_index + 1, chars, char_index + 1, budget)
                } else {
                    None
                }
            }
            SearchQuant::ZeroOne => {
                if char_index < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index, budget)
                {
                    if let Some(end) =
                        self.match_tokens(token_index + 1, chars, char_index + 1, budget)
                    {
                        return Some(end);
                    }
                }
                self.match_tokens(token_index + 1, chars, char_index, budget)
            }
            SearchQuant::ZeroMore | SearchQuant::OneMore => {
                let minimum = usize::from(token.quant == SearchQuant::OneMore);
                let mut run = 0;
                while char_index + run < chars.len()
                    && Self::atom_matches(token.atom, chars, char_index + run, budget)
                {
                    run += 1;
                }
                let mut take = run;
                loop {
                    if take >= minimum {
                        if let Some(end) =
                            self.match_tokens(token_index + 1, chars, char_index + take, budget)
                        {
                            return Some(end);
                        }
                    }
                    if take == 0 {
                        break;
                    }
                    take -= 1;
                    if take < minimum {
                        break;
                    }
                }
                None
            }
        }
    }

    fn atom_matches(atom: SearchAtom, chars: &[char], index: usize, budget: &mut usize) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let c = chars[index];
        match atom {
            SearchAtom::Lit(literal) => literal == c,
            SearchAtom::Dot => true,
            SearchAtom::Digit => c.is_ascii_digit(),
            SearchAtom::NotDigit => !c.is_ascii_digit(),
            SearchAtom::Word => c.is_alphanumeric() || c == '_',
            SearchAtom::NotWord => !(c.is_alphanumeric() || c == '_'),
            SearchAtom::Space => c.is_whitespace(),
            SearchAtom::NotSpace => !c.is_whitespace(),
        }
    }
}

fn walk_search(
    dir: &Path,
    workspace_root: &Path,
    regex: &LiteRegex,
    max_matches: usize,
    hits: &mut Vec<String>,
    capped: &mut bool,
) -> Result<(), ToolError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
        // Symbolic links are never followed (symlink escapes stay excluded).
        if entry
            .file_type()
            .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?
            .is_symlink()
        {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        if *capped {
            return Ok(());
        }
        // Defense against races/odd mounts: never leave the workspace.
        if !is_within_workspace(workspace_root, &path) {
            continue;
        }
        if path.is_dir() {
            walk_search(&path, workspace_root, regex, max_matches, hits, capped)?;
        } else if path.is_file() {
            search_one_file(&path, workspace_root, regex, max_matches, hits, capped)?;
        }
    }
    Ok(())
}

fn search_one_file(
    path: &Path,
    workspace_root: &Path,
    regex: &LiteRegex,
    max_matches: usize,
    hits: &mut Vec<String>,
    capped: &mut bool,
) -> Result<(), ToolError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| ToolError::Io(format!("failed to read file metadata: {e}")))?;
    if metadata.len() > MAX_SEARCH_FILE_BYTES {
        return Ok(());
    }
    let bytes =
        std::fs::read(path).map_err(|e| ToolError::Io(format!("failed to read file: {e}")))?;
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0) {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&bytes);
    let relative = path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    for (index, line) in text.lines().enumerate() {
        if regex.is_match(line) {
            if hits.len() >= max_matches {
                *capped = true;
                return Ok(());
            }
            hits.push(format!(
                "{}:{}:{}",
                relative,
                index + 1,
                shorten_hit_line(line)
            ));
        }
    }
    Ok(())
}

/// Middle-truncate one hit line, keeping both edges with a notice.
fn shorten_hit_line(line: &str) -> String {
    if line.len() <= SEARCH_LINE_BUDGET_BYTES {
        return line.to_string();
    }
    let head_end = floor_char_boundary(line, SEARCH_LINE_EDGE_BYTES);
    let tail_start = ceil_char_boundary(line, line.len().saturating_sub(SEARCH_LINE_EDGE_BYTES));
    format!(
        "{}... [line truncated, {} bytes, showing first {} and last {} bytes] ...{}",
        &line[..head_end],
        line.len(),
        head_end,
        line.len() - tail_start,
        &line[tail_start..]
    )
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    index = std::cmp::min(index, s.len());
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(s: &str, mut index: usize) -> usize {
    index = std::cmp::min(index, s.len());
    while index < s.len() && !s.is_char_boundary(index) {
        index += 1;
    }
    index
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

    #[test]
    fn edit_file_replaces_exact_once() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "hello brave world").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "brave", "new_text": "cold"}),
        );
        let out = ToolRegistry::execute(&edit, &ws).expect("exact-once replace");
        assert!(
            out.contains("-hello brave world"),
            "diff must show removal: {out}"
        );
        assert!(
            out.contains("+hello cold world"),
            "diff must show addition: {out}"
        );
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "hello cold world"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_no_match_errors() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "hello world").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "absent", "new_text": "x"}),
        );
        let res = ToolRegistry::execute(&edit, &ws);
        assert!(res.is_err(), "zero matches must fail");
        assert_eq!(res.unwrap_err().to_string(), "Error: no exact match found");
        // File untouched.
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "hello world"
        );
        // Insert mode with a missing anchor fails the same way.
        let insert = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "insert_after": "absent", "new_text": "x"}),
        );
        let res_insert = ToolRegistry::execute(&insert, &ws);
        assert!(res_insert.is_err());
        assert_eq!(
            res_insert.unwrap_err().to_string(),
            "Error: no exact match found"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_multi_match_refuses() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "aaa bbb aaa").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "aaa", "new_text": "zzz"}),
        );
        let res = ToolRegistry::execute(&edit, &ws);
        assert!(res.is_err(), "ambiguous matches must fail");
        assert_eq!(
            res.unwrap_err().to_string(),
            "Error: pattern matches 2 locations, refusing to guess"
        );
        // No fuzzy repair: the file is byte-identical afterwards.
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "aaa bbb aaa"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_insert_after_anchor() {
        let ws = temp_workspace();
        fs::write(ws.join("list.txt"), "line1\nline2\nline3").expect("seed");
        let insert = call(
            "edit_file",
            serde_json::json!({"path": "list.txt", "insert_after": "line2", "new_text": "\ninserted"}),
        );
        let out = ToolRegistry::execute(&insert, &ws).expect("insert");
        assert!(out.contains("+inserted"), "diff must show insertion: {out}");
        assert_eq!(
            fs::read_to_string(ws.join("list.txt")).unwrap(),
            "line1\nline2\ninserted\nline3"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_rejects_outside_workspace() {
        let ws = temp_workspace();
        fs::write(ws.join("inner.txt"), "unique-inner-content").expect("seed");
        // Traversal payload.
        let traversal = call(
            "edit_file",
            serde_json::json!({"path": "../outside.txt", "old_text": "a", "new_text": "b"}),
        );
        let res = ToolRegistry::execute(&traversal, &ws);
        assert!(res.is_err(), "traversal must be rejected");
        assert!(res.unwrap_err().to_string().starts_with("Error:"));
        // Absolute path outside the workspace.
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let absolute = call(
            "edit_file",
            serde_json::json!({"path": abs_outside, "old_text": "a", "new_text": "b"}),
        );
        assert!(
            ToolRegistry::execute(&absolute, &ws).is_err(),
            "absolute outside path must be rejected"
        );
        // Symlink escape: a link inside the workspace pointing outside must
        // not become an editing backdoor.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside_dir = temp_workspace();
            fs::write(outside_dir.join("secret.txt"), "outer-secret").expect("outside seed");
            let link = ws.join("escape.txt");
            symlink(outside_dir.join("secret.txt"), &link).expect("symlink");
            let via_link = call(
                "edit_file",
                serde_json::json!({"path": "escape.txt", "old_text": "outer-secret", "new_text": "pwned"}),
            );
            let res_link = ToolRegistry::execute(&via_link, &ws);
            assert!(res_link.is_err(), "symlink escape must be rejected");
            assert_eq!(
                fs::read_to_string(outside_dir.join("secret.txt")).unwrap(),
                "outer-secret",
                "outside file must be untouched"
            );
            let _ = fs::remove_dir_all(&outside_dir);
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn search_files_finds_pattern_with_caps() {
        let ws = temp_workspace();
        let mut body = String::new();
        for i in 0..10 {
            let _ = writeln!(body, "needle in haystack {i}");
        }
        body.push_str("no match here\n");
        fs::write(ws.join("hay.txt"), &body).expect("seed");

        // Literal match finds every matching line.
        let find = call("search_files", serde_json::json!({"pattern": "needle"}));
        let out = ToolRegistry::execute(&find, &ws).expect("search");
        assert_eq!(out.lines().filter(|l| l.contains("needle")).count(), 10);
        assert!(
            out.contains("hay.txt:1:"),
            "hit shape is path:line:text: {out}"
        );

        // Regex subset: `.+`, `\d`, anchors.
        let digits = call(
            "search_files",
            serde_json::json!({"pattern": "^needle.+\\d$"}),
        );
        let out_digits = ToolRegistry::execute(&digits, &ws).expect("anchored class search");
        assert_eq!(out_digits.lines().count(), 10);

        // max_matches is honoured and the cap notice is honest.
        let capped = call(
            "search_files",
            serde_json::json!({"pattern": "needle", "max_matches": 4}),
        );
        let out_capped = ToolRegistry::execute(&capped, &ws).expect("capped search");
        assert_eq!(
            out_capped.lines().filter(|l| l.contains("needle")).count(),
            4
        );
        assert!(out_capped.contains("[match cap reached: showing first 4 matches"));

        // Hard cap: a request above 200 is clamped, never honoured literally.
        let mut big = String::new();
        for _ in 0..300 {
            big.push_str("capped-needle\n");
        }
        fs::write(ws.join("big.txt"), &big).expect("seed big");
        let hard = call(
            "search_files",
            serde_json::json!({"pattern": "capped-needle", "max_matches": 5000}),
        );
        let out_hard = ToolRegistry::execute(&hard, &ws).expect("hard-cap search");
        assert_eq!(
            out_hard
                .lines()
                .filter(|l| l.contains("capped-needle"))
                .count(),
            200
        );
        assert!(out_hard.contains("(cap 200)"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn search_files_skips_binary_and_stays_in_workspace() {
        let ws = temp_workspace();
        fs::write(ws.join("plain.txt"), "visible-needle here").expect("seed text");
        // Binary file containing the same pattern plus NUL bytes is skipped.
        let mut binary = b"visible-needle here".to_vec();
        binary.extend_from_slice(&[0, 1, 2, 3]);
        binary.extend_from_slice(b"visible-needle again");
        fs::write(ws.join("blob.bin"), &binary).expect("seed binary");

        let find = call(
            "search_files",
            serde_json::json!({"pattern": "visible-needle"}),
        );
        let out = ToolRegistry::execute(&find, &ws).expect("search");
        assert!(out.contains("plain.txt"), "text hit missing: {out}");
        assert!(
            !out.contains("blob.bin"),
            "binary hit must be skipped: {out}"
        );

        // Traversal scope is rejected.
        let bad_scope = call(
            "search_files",
            serde_json::json!({"pattern": "visible-needle", "directory": "../"}),
        );
        assert!(
            ToolRegistry::execute(&bad_scope, &ws).is_err(),
            "scope outside workspace must be rejected"
        );

        // Symlink escape: a link inside the workspace pointing at an outside
        // file must not leak the outside file's contents.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside_dir = temp_workspace();
            fs::write(outside_dir.join("secret.txt"), "escaped-needle-xyz").expect("outside seed");
            symlink(outside_dir.join("secret.txt"), ws.join("leak.txt")).expect("symlink");
            let leak = call(
                "search_files",
                serde_json::json!({"pattern": "escaped-needle-xyz"}),
            );
            let out_leak = ToolRegistry::execute(&leak, &ws).expect("search runs");
            assert!(
                !out_leak.contains("escaped-needle-xyz"),
                "symlink target outside workspace must stay excluded: {out_leak}"
            );
            let _ = fs::remove_dir_all(&outside_dir);
        }
        let _ = fs::remove_dir_all(&ws);
    }
}
