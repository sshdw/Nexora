//! Automatic post-edit compiler/typecheck diagnostics (S11-10).
//!
//! After a successful `write_file` or `edit_file`, runs a fast, bounded,
//! read-only diagnostic check (`tsc --noEmit` for `.ts`/`.tsx`, `cargo check
//! --message-format=short` for `.rs`) when the corresponding project manifest
//! exists within `workspace_root`.
//!
//! Hard invariants:
//! - Never holds a blocking sandbox lease (`SANDBOX_LEASE` is never touched).
//! - Hard wall-clock timeout (`DEFAULT_DIAGNOSTICS_TIMEOUT`, 6s) + cooperative
//!   cancellation via [`CancellationToken`]; timed-out processes are killed.
//! - Gracefully no-ops (`None`) when the manifest or binary is absent, when
//!   the check succeeds cleanly, or when it times out — never fails the edit.
//! - Output is redacted via [`crate::application::agent::redaction`] and
//!   strictly bounded in line count and byte length.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::application::agent::control::CancellationToken;
use crate::application::agent::redaction::redact_command_output;

/// Hard timeout for a single post-edit diagnostic check.
pub(crate) const DEFAULT_DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(6);

/// Maximum number of diagnostic lines attached to a tool result.
pub(crate) const MAX_DIAGNOSTIC_LINES: usize = 15;

/// Maximum byte length of the formatted diagnostic block.
pub(crate) const MAX_DIAGNOSTIC_BYTES: usize = 2_048;

/// Maximum bytes read from each child pipe (stdout / stderr).
const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Poll interval while waiting for the diagnostic child process.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticTool {
    TypeScript,
    Rust,
}

impl DiagnosticTool {
    const fn label(self) -> &'static str {
        match self {
            Self::TypeScript => "tsc",
            Self::Rust => "cargo check",
        }
    }

    const fn manifest_name(self) -> &'static str {
        match self {
            Self::TypeScript => "tsconfig.json",
            Self::Rust => "Cargo.toml",
        }
    }
}

/// Run scoped post-edit diagnostics for `edited_file` inside `workspace_root`.
///
/// Returns `Some(formatted_block)` when compiler/typecheck errors were found,
/// or `None` on clean check, unsupported extension, missing manifest/tool,
/// cancellation, or timeout.
pub(crate) fn run_post_edit_diagnostics(
    workspace_root: &Path,
    edited_file: &Path,
    token: &CancellationToken,
) -> Option<String> {
    run_post_edit_diagnostics_with_timeout(
        workspace_root,
        edited_file,
        token,
        DEFAULT_DIAGNOSTICS_TIMEOUT,
    )
}

pub(crate) fn run_post_edit_diagnostics_with_timeout(
    workspace_root: &Path,
    edited_file: &Path,
    token: &CancellationToken,
    timeout: Duration,
) -> Option<String> {
    if token.is_cancelled() {
        return None;
    }
    let (tool, manifest_dir) = detect_tool_and_manifest(workspace_root, edited_file)?;
    let cmd = build_command(tool, &manifest_dir, workspace_root);
    let (success, raw_output) = run_bounded_command(cmd, timeout, token)?;
    if success {
        return None;
    }
    let edited_hint = edited_file
        .strip_prefix(&manifest_dir)
        .ok()
        .or_else(|| edited_file.strip_prefix(workspace_root).ok())
        .map_or_else(
            || {
                edited_file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            },
            |p| p.to_string_lossy().replace('\\', "/"),
        );
    format_diagnostics(tool, &edited_hint, &raw_output)
}

/// Determine the diagnostic tool and nearest manifest directory within
/// `workspace_root` for `edited_file`.
pub(crate) fn detect_tool_and_manifest(
    workspace_root: &Path,
    edited_file: &Path,
) -> Option<(DiagnosticTool, PathBuf)> {
    let ext = edited_file.extension()?.to_str()?;
    let tool = match ext.to_ascii_lowercase().as_str() {
        "ts" | "tsx" => DiagnosticTool::TypeScript,
        "rs" => DiagnosticTool::Rust,
        _ => return None,
    };

    let manifest_name = tool.manifest_name();
    let mut cursor = edited_file.parent()?;
    loop {
        if !cursor.starts_with(workspace_root) {
            break;
        }
        if cursor.join(manifest_name).is_file() {
            return Some((tool, cursor.to_path_buf()));
        }
        if cursor == workspace_root {
            break;
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        cursor = parent;
    }
    None
}

fn build_command(tool: DiagnosticTool, manifest_dir: &Path, workspace_root: &Path) -> Command {
    let mut cmd = match tool {
        DiagnosticTool::Rust => {
            let mut c = Command::new("cargo");
            c.args(["check", "--message-format=short", "--color=never"]);
            c
        }
        DiagnosticTool::TypeScript => {
            let local_tsc = find_local_tsc(manifest_dir, workspace_root);
            let mut c = match local_tsc {
                Some(path) => Command::new(path),
                None => {
                    #[cfg(windows)]
                    {
                        Command::new("tsc.cmd")
                    }
                    #[cfg(not(windows))]
                    {
                        Command::new("tsc")
                    }
                }
            };
            c.args(["--noEmit", "--pretty", "false"]);
            c
        }
    };
    cmd.current_dir(manifest_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn find_local_tsc(manifest_dir: &Path, workspace_root: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let bin_name = "tsc.cmd";
    #[cfg(not(windows))]
    let bin_name = "tsc";

    for dir in [manifest_dir, workspace_root] {
        let candidate = dir.join("node_modules").join(".bin").join(bin_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run `cmd` with a hard timeout and bounded pipe capture.
///
/// Returns `Some((exit_success, combined_stdout_stderr))`, or `None` if the
/// process failed to spawn, timed out, or was cancelled.
pub(crate) fn run_bounded_command(
    mut cmd: Command,
    timeout: Duration,
    token: &CancellationToken,
) -> Option<(bool, String)> {
    if token.is_cancelled() {
        return None;
    }
    let mut child = cmd.spawn().ok()?;
    let stdout_handle = child.stdout.take().map(spawn_bounded_reader);
    let stderr_handle = child.stderr.take().map(spawn_bounded_reader);

    let start = Instant::now();
    let status = loop {
        if token.is_cancelled() || start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let mut combined = String::with_capacity(stdout.len() + stderr.len() + 1);
    combined.push_str(&String::from_utf8_lossy(&stdout));
    if !combined.is_empty() && !combined.ends_with('\n') && !stderr.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&String::from_utf8_lossy(&stderr));
    Some((status.success(), combined))
}

fn spawn_bounded_reader<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while buf.len() < MAX_CAPTURE_BYTES {
            let remaining = MAX_CAPTURE_BYTES - buf.len();
            let to_read = remaining.min(chunk.len());
            match reader.read(&mut chunk[..to_read]) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        buf
    })
}

/// Filter, redact, prioritize, and bound raw compiler/typecheck output.
pub(crate) fn format_diagnostics(
    tool: DiagnosticTool,
    edited_hint: &str,
    raw_output: &str,
) -> Option<String> {
    let (redacted, _) = redact_command_output(raw_output);
    let basename = Path::new(edited_hint)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(edited_hint);

    let mut matching_file: Vec<&str> = Vec::new();
    let mut other_errors: Vec<&str> = Vec::new();

    for raw_line in redacted.lines() {
        let line = raw_line.trim();
        if line.is_empty() || !is_diagnostic_error_line(tool, line) {
            continue;
        }
        let norm = line.replace('\\', "/");
        if (!edited_hint.is_empty() && norm.contains(edited_hint))
            || (!basename.is_empty() && norm.contains(basename))
        {
            matching_file.push(line);
        } else {
            other_errors.push(line);
        }
    }

    let total = matching_file.len() + other_errors.len();
    if total == 0 {
        return None;
    }

    let mut ordered = matching_file;
    ordered.extend(other_errors);

    let mut body = String::new();
    let mut shown = 0usize;
    for line in ordered.into_iter().take(MAX_DIAGNOSTIC_LINES) {
        let next_len = body.len() + line.len() + usize::from(!body.is_empty());
        if next_len > MAX_DIAGNOSTIC_BYTES {
            break;
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
        shown += 1;
    }

    if shown == 0 {
        // First line alone exceeded MAX_DIAGNOSTIC_BYTES: include a safe prefix.
        let first = redacted
            .lines()
            .map(str::trim)
            .find(|l| is_diagnostic_error_line(tool, l))?;
        let end = floor_char_boundary(first, MAX_DIAGNOSTIC_BYTES.saturating_sub(3));
        body.push_str(&first[..end]);
        body.push_str("...");
        shown = 1;
    }

    let mut out = format!(
        "\n\n[Post-edit diagnostics ({}): {} error(s)]\n{}",
        tool.label(),
        total,
        body
    );
    if total > shown {
        let _ = write!(out, "\n... (+{} more)", total - shown);
    }
    Some(out)
}

fn is_diagnostic_error_line(tool: DiagnosticTool, line: &str) -> bool {
    match tool {
        DiagnosticTool::TypeScript => line.contains("error TS") || line.contains(": error "),
        DiagnosticTool::Rust => {
            line.contains(": error[")
                || line.contains(": error:")
                || line.starts_with("error[")
                || line.starts_with("error:")
        }
    }
}

fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::temp_workspace;
    use std::fs;

    #[test]
    fn missing_manifest_is_noop() {
        let ws = temp_workspace();
        let file = ws.join("src").join("app.ts");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "const x: number = 'bad';").unwrap();

        assert_eq!(detect_tool_and_manifest(&ws, &file), None);
        assert_eq!(
            run_post_edit_diagnostics(&ws, &file, &CancellationToken::new()),
            None
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn unsupported_extension_is_noop() {
        let ws = temp_workspace();
        fs::write(ws.join("Cargo.toml"), "[package]\nname=\"demo\"\n").unwrap();
        let file = ws.join("README.md");
        fs::write(&file, "# hello").unwrap();

        assert_eq!(detect_tool_and_manifest(&ws, &file), None);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn detects_nearest_manifest_within_workspace() {
        let ws = temp_workspace();
        let sub = ws.join("crates").join("pkg");
        fs::create_dir_all(sub.join("src")).unwrap();
        fs::write(sub.join("Cargo.toml"), "[package]\nname=\"pkg\"\n").unwrap();
        let file = sub.join("src").join("lib.rs");
        fs::write(&file, "pub fn x() {}").unwrap();

        assert_eq!(
            detect_tool_and_manifest(&ws, &file),
            Some((DiagnosticTool::Rust, sub.clone()))
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn format_diagnostics_bounds_redacts_and_prioritizes_edited_file() {
        let mut raw = String::new();
        for i in 0..20 {
            let _ = writeln!(
                raw,
                "src/other.ts({i},1): error TS2322: Type 'string' is not assignable to type 'number'."
            );
        }
        raw.push_str(
            "src/target.ts(4,7): error TS2304: Cannot find name 'sk-ant-api03-1234567890abcdef1234567890abcdef'.\n",
        );

        let formatted = format_diagnostics(DiagnosticTool::TypeScript, "src/target.ts", &raw)
            .expect("diagnostics produced");
        assert!(formatted.contains("[Post-edit diagnostics (tsc): 21 error(s)]"));
        let lines: Vec<&str> = formatted.lines().collect();
        // Line 0 is empty, line 1 is empty, line 2 is header, line 3 is first diagnostic.
        assert!(
            lines[3].contains("src/target.ts(4,7)"),
            "edited file diagnostic must be prioritized first: {formatted}"
        );
        assert!(
            !formatted.contains("sk-ant-api03-1234567890abcdef1234567890abcdef"),
            "secrets must be redacted from diagnostics"
        );
        assert!(
            formatted.contains("... (+6 more)"),
            "excess diagnostics must be bounded to MAX_DIAGNOSTIC_LINES: {formatted}"
        );
    }

    #[test]
    fn timeout_kills_slow_process_and_returns_none() {
        let token = CancellationToken::new();
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 6 127.0.0.1 >nul"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 5"]);
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let start = Instant::now();
        let outcome = run_bounded_command(cmd, Duration::from_millis(150), &token);
        let elapsed = start.elapsed();

        assert_eq!(outcome, None, "slow diagnostic check must time out to None");
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout must bound execution promptly, took {elapsed:?}"
        );
    }
}
