//! Hardened `stdio` MCP transport:
//! - Clean sanitized environment (`env_clear()` + minimal OS essentials +
//!   explicit user-configured env vars only; never inherits full host env or
//!   Nexora/provider secrets).
//! - Newline-delimited JSON-RPC 2.0 (`initialize`, `notifications/initialized`,
//!   `tools/list`, `tools/call`).
//! - Hard timeouts, cancellation polling, and strictly bounded line sizes.
//! - OS process-tree containment and cleanup on drop/cancel/timeout via
//!   `nexora_os_exec::spawn_in_process_tree`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nexora_os_exec::{is_sensitive_env_key, spawn_in_process_tree, ProcessTreeChild};
use serde_json::Value;

use crate::application::agent::control::CancellationToken;

use super::jsonrpc::{
    format_tool_call_result, initialize_params, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, McpToolDescriptor, McpToolsListResult, MAX_JSONRPC_PAYLOAD_BYTES,
    MAX_TOOLS_LIST_PAGES, MAX_TOOLS_PER_SERVER,
};
use super::McpError;

/// Default hard timeout per MCP JSON-RPC round trip.
pub(crate) const DEFAULT_STDIO_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum unrelated/notification lines skipped while waiting for one response.
const MAX_SKIPPED_MESSAGES: usize = 256;

/// Minimal OS environment variables allowed to pass through to a stdio MCP child.
/// Full host environment is NEVER inherited (`Command::env_clear`).
#[cfg(windows)]
const SAFE_OS_ENV_KEYS: &[&str] = &[
    "PATH",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
];

#[cfg(not(windows))]
const SAFE_OS_ENV_KEYS: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "TERM", "USER", "LOGNAME",
];

/// Build the sanitized environment map for a stdio MCP subprocess from a host
/// environment iterator and explicit user-configured entries.
#[must_use]
pub(crate) fn build_sanitized_env_from(
    host_env: impl IntoIterator<Item = (String, String)>,
    user_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (k, v) in host_env {
        let allowed = SAFE_OS_ENV_KEYS
            .iter()
            .any(|safe| safe.eq_ignore_ascii_case(&k));
        if allowed && !is_sensitive_env_key(&k) {
            env.insert(k, v);
        }
    }
    for (k, v) in user_env {
        if !k.is_empty() && !k.contains('=') && !k.contains('\0') && !v.contains('\0') {
            env.insert(k.clone(), v.clone());
        }
    }
    env
}

enum LineEvent {
    Line(String),
    Oversized,
    Eof,
    IoError,
}

/// Read a single newline-delimited frame from `reader` with a hard byte limit.
/// Prevents OOM from a hostile server streaming endless bytes without `\n`.
fn read_bounded_line(reader: &mut impl BufRead, max_bytes: usize) -> LineEvent {
    let mut buf = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return LineEvent::IoError,
        };
        if available.is_empty() {
            if buf.is_empty() {
                return LineEvent::Eof;
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            return LineEvent::Line(text);
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if buf.len().saturating_add(pos) > max_bytes {
                reader.consume(pos + 1);
                return LineEvent::Oversized;
            }
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            return LineEvent::Line(text);
        }
        let chunk_len = available.len();
        if buf.len().saturating_add(chunk_len) > max_bytes {
            return LineEvent::Oversized;
        }
        buf.extend_from_slice(available);
        reader.consume(chunk_len);
    }
}

/// Active `stdio` MCP client managing a contained OS process tree.
pub(crate) struct StdioMcpClient {
    child: ProcessTreeChild,
    stdin: Option<ChildStdin>,
    rx: Receiver<LineEvent>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    next_id: u64,
}

impl StdioMcpClient {
    /// Spawn the configured stdio MCP command in a killable OS process tree
    /// with a clean sanitized environment and perform the MCP `initialize` +
    /// `notifications/initialized` handshake.
    ///
    /// # Errors
    /// Returns a secret-free [`McpError`].
    pub(crate) fn connect(
        program: &str,
        args: &[String],
        user_env: &BTreeMap<String, String>,
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<Self, McpError> {
        if token.is_cancelled() {
            return Err(McpError::Cancelled);
        }
        let trimmed_program = program.trim();
        if trimmed_program.is_empty() || trimmed_program.contains('\0') {
            return Err(McpError::InvalidConfig("MCP stdio command is empty"));
        }

        let sanitized_env = build_sanitized_env_from(std::env::vars(), user_env);
        let mut cmd = Command::new(trimmed_program);
        cmd.args(args)
            .env_clear()
            .envs(&sanitized_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = spawn_in_process_tree(&mut cmd)
            .map_err(|_| McpError::Transport("failed to launch MCP stdio server process"))?;

        let stdin = child
            .take_stdin()
            .ok_or(McpError::Transport("MCP stdio server stdin unavailable"))?;
        let stdout = child
            .take_stdout()
            .ok_or(McpError::Transport("MCP stdio server stdout unavailable"))?;
        let stderr = child.take_stderr();

        let (tx, rx) = mpsc::channel::<LineEvent>();
        let stdout_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let ev = read_bounded_line(&mut reader, MAX_JSONRPC_PAYLOAD_BYTES);
                let stop = matches!(
                    ev,
                    LineEvent::Eof | LineEvent::IoError | LineEvent::Oversized
                );
                if tx.send(ev).is_err() || stop {
                    break;
                }
            }
        });

        let stderr_thread = stderr.map(|mut err_pipe| {
            std::thread::spawn(move || {
                // Drain stderr silently in bounded chunks so the child cannot
                // deadlock on a full pipe buffer; never log raw server output.
                let mut sink = [0u8; 4096];
                while let Ok(n) = err_pipe.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            })
        });

        let mut client = Self {
            child,
            stdin: Some(stdin),
            rx,
            stdout_thread: Some(stdout_thread),
            stderr_thread,
            next_id: 1,
        };

        client.initialize(timeout, token)?;
        Ok(client)
    }

    fn send_json(&mut self, payload: &impl serde::Serialize) -> Result<(), McpError> {
        let mut line = serde_json::to_string(payload)
            .map_err(|_| McpError::Protocol("failed to encode JSON-RPC payload"))?;
        line.push('\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(McpError::Transport("MCP stdio stdin closed"))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| McpError::Transport("failed to write to MCP stdio server"))
    }

    fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<Value, McpError> {
        if token.is_cancelled() {
            let _ = self.child.kill_tree();
            return Err(McpError::Cancelled);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req = JsonRpcRequest::new(id, method, params);
        self.send_json(&req)?;

        let deadline = Instant::now() + timeout;
        let mut skipped = 0usize;
        loop {
            if token.is_cancelled() {
                let _ = self.child.kill_tree();
                return Err(McpError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = self.child.kill_tree();
                return Err(McpError::Timeout);
            }
            let wait = (deadline - now).min(Duration::from_millis(50));
            match self.rx.recv_timeout(wait) {
                Ok(LineEvent::Line(line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) else {
                        skipped += 1;
                        if skipped > MAX_SKIPPED_MESSAGES {
                            let _ = self.child.kill_tree();
                            return Err(McpError::Protocol(
                                "MCP stdio server emitted excessive non-JSON-RPC output",
                            ));
                        }
                        continue;
                    };
                    if !resp.matches_id(id) {
                        skipped += 1;
                        if skipped > MAX_SKIPPED_MESSAGES {
                            let _ = self.child.kill_tree();
                            return Err(McpError::Protocol(
                                "MCP stdio server emitted too many unrelated messages",
                            ));
                        }
                        continue;
                    }
                    if let Some(err) = resp.error {
                        return Err(McpError::JsonRpcError { code: err.code });
                    }
                    return Ok(resp.result.unwrap_or(Value::Null));
                }
                Ok(LineEvent::Oversized) => {
                    let _ = self.child.kill_tree();
                    return Err(McpError::ResponseTooLarge);
                }
                Ok(LineEvent::Eof | LineEvent::IoError) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(McpError::Transport(
                        "MCP stdio server terminated before responding",
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn initialize(&mut self, timeout: Duration, token: &CancellationToken) -> Result<(), McpError> {
        let _ = self.request("initialize", Some(initialize_params()), timeout, token)?;
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        self.send_json(&notif)?;
        Ok(())
    }

    /// Discover tools via `tools/list` (with bounded cursor pagination).
    pub(crate) fn list_tools(
        &mut self,
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<Vec<McpToolDescriptor>, McpError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOLS_LIST_PAGES {
            let params = cursor.as_ref().map(|c| serde_json::json!({ "cursor": c }));
            let raw = self.request("tools/list", params, timeout, token)?;
            let page: McpToolsListResult = serde_json::from_value(raw).map_err(|_| {
                McpError::Protocol("MCP server returned malformed tools/list result")
            })?;
            for tool in page.tools {
                if all.len() >= MAX_TOOLS_PER_SERVER {
                    return Ok(all);
                }
                all.push(tool);
            }
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(all)
    }

    /// Invoke one tool via `tools/call`, returning `(bounded_output, is_error)`.
    pub(crate) fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<(String, bool), McpError> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments,
        });
        let raw = self.request("tools/call", Some(params), timeout, token)?;
        Ok(format_tool_call_result(&raw))
    }
}

impl Drop for StdioMcpClient {
    fn drop(&mut self) {
        // Close stdin first, then kill the entire OS process tree so no
        // grandchild process survives and stdout/stderr readers unblock.
        drop(self.stdin.take());
        let _ = self.child.kill_tree();
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_env_strips_host_secrets_and_keeps_explicit_vars() {
        let host = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-host-secret".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-ant-secret".to_string()),
            ("NEXORA_INTERNAL_TOKEN".to_string(), "tok-123".to_string()),
            (
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "aws-secret".to_string(),
            ),
            ("RANDOM_HOST_VAR".to_string(), "leak".to_string()),
        ];
        let mut user = BTreeMap::new();
        user.insert("MCP_MODE".to_string(), "readonly".to_string());

        let env = build_sanitized_env_from(host, &user);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("MCP_MODE").map(String::as_str), Some("readonly"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert!(!env.contains_key("NEXORA_INTERNAL_TOKEN"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.contains_key("RANDOM_HOST_VAR"));
    }

    #[test]
    fn bounded_line_reader_rejects_oversized_frames() {
        let data = b"{\"ok\":true}\n";
        let mut cur = std::io::Cursor::new(&data[..]);
        match read_bounded_line(&mut cur, 64) {
            LineEvent::Line(s) => assert_eq!(s, "{\"ok\":true}"),
            _ => panic!("expected normal line"),
        }

        let huge = [b'x'; 200];
        let mut cur2 = std::io::Cursor::new(&huge[..]);
        assert!(matches!(
            read_bounded_line(&mut cur2, 100),
            LineEvent::Oversized
        ));
    }

    #[test]
    fn stdio_client_performs_jsonrpc_handshake_list_and_call_and_cleans_up() {
        // Spawn a tiny portable command that speaks newline-delimited JSON-RPC.
        // Uses PowerShell on Windows or sh on Unix.
        let token = CancellationToken::new();
        let mut user_env = BTreeMap::new();
        user_env.insert("NEXORA_TEST_EXPLICIT".to_string(), "visible".to_string());

        #[cfg(windows)]
        let (program, args) = (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                r#"
while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ($line -match '"method"\s*:\s*"initialize"') {
    [Console]::Out.WriteLine('{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}')
  } elseif ($line -match '"method"\s*:\s*"tools/list"') {
    [Console]::Out.WriteLine('{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo_env","description":"Echo test env","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]}}')
  } elseif ($line -match '"method"\s*:\s*"tools/call"') {
    $val = $env:NEXORA_TEST_EXPLICIT
    $leak = $env:OPENAI_API_KEY
    [Console]::Out.WriteLine(('{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"explicit=' + $val + ';leak=' + $leak + '"}],"isError":false}}'))
  }
}
"#
                .to_string(),
            ],
        );

        #[cfg(not(windows))]
        let (program, args) = (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                r#"
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}}}}' ;;
    *'"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo_env","description":"Echo test env","inputSchema":{"type":"object"}}]}}' ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"explicit=%s;leak=%s"}],"isError":false}}\n' "$NEXORA_TEST_EXPLICIT" "$OPENAI_API_KEY" ;;
  esac
done
"#
                .to_string(),
            ],
        );

        let mut client =
            StdioMcpClient::connect(&program, &args, &user_env, Duration::from_secs(15), &token)
                .expect("connect stdio mcp");
        let tools = client
            .list_tools(Duration::from_secs(10), &token)
            .expect("list tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo_env");

        let (output, is_err) = client
            .call_tool(
                "echo_env",
                &serde_json::json!({"msg": "hi"}),
                Duration::from_secs(10),
                &token,
            )
            .expect("call tool");
        assert!(!is_err);
        assert_eq!(output, "explicit=visible;leak=");
        // Dropping `client` terminates the process tree cleanly.
        drop(client);
    }
}
