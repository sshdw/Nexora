//! Safe External CLI Adapter (`S11-11b`) behind [`ProviderExecutor`].
//!
//! Invokes the user's own officially installed and authenticated `codex` or
//! `claude` CLI binaries as opt-in local subprocesses — **never** reading or
//! touching closed Claude Pro/Max OAuth tokens or credential files directly.
//!
//! # Security invariants
//! - **Clean environment**: every spawned subprocess uses `Command::env_clear()`
//!   populated strictly with minimal OS essentials (never inheriting full host
//!   env, Nexora API keys, cloud credentials, or provider tokens).
//! - **No credential forwarding**: the `credential` argument of
//!   [`ProviderExecutor::execute`] is intentionally ignored and never written
//!   to env, args, or stdin.
//! - **Process-tree containment**: every child is launched via
//!   [`nexora_os_exec::spawn_in_process_tree`] and terminated via
//!   [`ProcessTreeChild::kill_tree`] on timeout, cancellation, output overflow,
//!   or drop.
//! - **Hard bounds**: strict wall-clock timeout and strict stdout/stderr byte
//!   caps prevent runaway processes or OOM.
//! - **Stdin prompt transport**: prompts travel over `stdin` rather than CLI
//!   arguments so conversation text never appears in OS process tables.

#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexora_core::provider::{
    AiRequest, AiResponse, AiRole, CancellationToken, ExecutorError, ProviderExecutor, StreamEvent,
};
use nexora_os_exec::{is_sensitive_env_key, spawn_in_process_tree};
use serde::{Deserialize, Serialize};

/// Internal provider name for the opt-in Codex CLI external adapter.
pub(crate) const CLI_CODEX_NAME: &str = "cli_codex";
/// User-facing display name for the Codex CLI adapter.
pub(crate) const CLI_CODEX_DISPLAY_NAME: &str = "Codex CLI";
/// Default model shortlist for the Codex CLI adapter.
pub(crate) const CLI_CODEX_MODELS: &[&str] = &["codex-cli-default"];

/// Internal provider name for the opt-in Claude CLI external adapter.
pub(crate) const CLI_CLAUDE_NAME: &str = "cli_claude";
/// User-facing display name for the Claude CLI adapter.
pub(crate) const CLI_CLAUDE_DISPLAY_NAME: &str = "Claude CLI";
/// Default model shortlist for the Claude CLI adapter.
pub(crate) const CLI_CLAUDE_MODELS: &[&str] = &["claude-cli-default"];

/// Default hard timeout for one external CLI completion.
pub(crate) const DEFAULT_CLI_TIMEOUT: Duration = Duration::from_mins(2);

/// Absolute upper bound on an external CLI completion timeout.
pub(crate) const MAX_CLI_TIMEOUT: Duration = Duration::from_mins(5);

/// Hard timeout for `--version` availability probes.
pub(crate) const CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Maximum stdout bytes accepted from an external CLI completion (1 MiB).
pub(crate) const MAX_CLI_OUTPUT_BYTES: usize = 1024 * 1024;

/// Maximum stdout bytes read from a `--version` availability probe (4 KiB).
const MAX_PROBE_OUTPUT_BYTES: usize = 4096;

/// Maximum stderr bytes drained before discarding (64 KiB).
const MAX_CLI_STDERR_BYTES: usize = 64 * 1024;

/// Maximum length of a sanitized CLI version string surfaced over IPC.
const MAX_VERSION_LEN: usize = 120;

/// Poll interval while waiting for a child process or cancellation.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Minimal OS environment variables allowed to pass through to an external CLI
/// subprocess. Full host environment is NEVER inherited (`Command::env_clear`).
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
    "APPDATA",
    "LOCALAPPDATA",
];

#[cfg(not(windows))]
const SAFE_OS_ENV_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TERM",
    "USER",
    "LOGNAME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];

/// Supported official external CLI adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliAdapterKind {
    /// Officially installed OpenAI `codex` CLI.
    Codex,
    /// Officially installed Anthropic `claude` CLI.
    Claude,
}

impl CliAdapterKind {
    /// All supported external CLI adapters in stable display order.
    pub(crate) const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    /// Short IPC identifier (`"codex"` / `"claude"`).
    #[must_use]
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    /// Internal provider name registered in `providers` and `ExecutorRegistry`.
    #[must_use]
    pub(crate) const fn provider_name(self) -> &'static str {
        match self {
            Self::Codex => CLI_CODEX_NAME,
            Self::Claude => CLI_CLAUDE_NAME,
        }
    }

    /// User-facing display name.
    #[must_use]
    pub(crate) const fn display_name(self) -> &'static str {
        match self {
            Self::Codex => CLI_CODEX_DISPLAY_NAME,
            Self::Claude => CLI_CLAUDE_DISPLAY_NAME,
        }
    }

    /// Official executable name resolved from sanitized `PATH`.
    #[must_use]
    pub(crate) const fn binary_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    /// Default placeholder model identifier (never passed as `--model`).
    #[must_use]
    pub(crate) const fn default_model(self) -> &'static str {
        match self {
            Self::Codex => CLI_CODEX_MODELS[0],
            Self::Claude => CLI_CLAUDE_MODELS[0],
        }
    }

    /// Parse an adapter ID (`"codex"` / `"claude"`) or provider name
    /// (`"cli_codex"` / `"cli_claude"`).
    #[must_use]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "codex" | CLI_CODEX_NAME => Some(Self::Codex),
            "claude" | CLI_CLAUDE_NAME => Some(Self::Claude),
            _ => None,
        }
    }
}

/// True when `provider_name` identifies an opt-in external CLI adapter.
#[must_use]
pub(crate) fn is_cli_adapter_provider(provider_name: &str) -> bool {
    matches!(provider_name, CLI_CODEX_NAME | CLI_CLAUDE_NAME)
}

/// Secret-free detection and opt-in status for one external CLI adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CliAdapterStatus {
    pub id: &'static str,
    pub provider_name: &'static str,
    pub display_name: &'static str,
    pub installed: bool,
    pub version: Option<String>,
    pub enabled: bool,
}

/// Build the sanitized environment map for an external CLI subprocess from
/// `host_env`.
///
/// Only minimal OS essentials from `SAFE_OS_ENV_KEYS` pass through; any key
/// flagged by [`is_sensitive_env_key`] or carrying secret/Nexora markers is
/// stripped unconditionally.
#[must_use]
pub(crate) fn build_sanitized_cli_env_from(
    host_env: impl IntoIterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in host_env {
        let allowed = SAFE_OS_ENV_KEYS
            .iter()
            .any(|safe| safe.eq_ignore_ascii_case(&key));
        if !allowed || is_sensitive_env_key(&key) || is_blocked_cli_env_key(&key) {
            continue;
        }
        if key.contains('\0') || value.contains('\0') {
            continue;
        }
        env.insert(key, value);
    }
    env
}

fn is_blocked_cli_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("NEXORA_")
        || upper.contains("API_KEY")
        || upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("CREDENTIAL")
}

/// Validate an optional model override passed as a CLI flag value.
/// Rejects leading `-` (flag injection), whitespace, control chars, or
/// oversized strings.
#[must_use]
pub(crate) fn is_safe_cli_model_arg(model: &str) -> bool {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.len() > 128 || trimmed.starts_with('-') {
        return false;
    }
    trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'))
}

/// Build non-interactive CLI arguments for `kind` and optional `model`.
#[must_use]
fn build_cli_args(kind: CliAdapterKind, model: &str) -> Vec<String> {
    let custom_model = model.trim() != kind.default_model() && is_safe_cli_model_arg(model.trim());
    match kind {
        CliAdapterKind::Codex => {
            let mut args = vec![
                "exec".to_string(),
                "--skip-git-repo-check".to_string(),
                "-".to_string(),
            ];
            if custom_model {
                args.push("-m".to_string());
                args.push(model.trim().to_string());
            }
            args
        }
        CliAdapterKind::Claude => {
            let mut args = vec![
                "-p".to_string(),
                "--output-format".to_string(),
                "text".to_string(),
            ];
            if custom_model {
                args.push("--model".to_string());
                args.push(model.trim().to_string());
            }
            args
        }
    }
}

/// Format an [`AiRequest`]'s conversation messages into a plain UTF-8 prompt
/// delivered over `stdin` (never via CLI args).
#[must_use]
pub(crate) fn format_request_prompt(request: &AiRequest) -> String {
    let mut out = String::new();
    for message in request.messages.iter() {
        let content = message.composed_content();
        if content.trim().is_empty() {
            continue;
        }
        let role_label = match message.role {
            AiRole::System => "System",
            AiRole::User => "User",
            AiRole::Assistant => "Assistant",
            AiRole::Tool => "Tool",
        };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(role_label);
        out.push_str(":\n");
        out.push_str(&content);
    }
    out
}

/// Sanitize a single-line `--version` output for safe IPC display.
#[must_use]
fn sanitize_version_line(raw: &str) -> Option<String> {
    let first = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    let cleaned: String = first
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_VERSION_LEN)
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

enum SubprocessOutcome {
    Exited { code: Option<i32>, stdout: Vec<u8> },
    TimedOut,
    Cancelled,
    OutputTooLarge,
    SpawnFailed,
}

fn spawn_stdout_reader(
    mut pipe: std::process::ChildStdout,
    max_stdout_bytes: usize,
    overflow: Arc<AtomicBool>,
    killer: nexora_os_exec::ProcessTreeHandle,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.len().saturating_add(n) > max_stdout_bytes {
                        overflow.store(true, Ordering::SeqCst);
                        let _ = killer.kill_tree();
                        break;
                    }
                    out.extend_from_slice(&chunk[..n]);
                }
            }
        }
        out
    })
}

fn spawn_stderr_drainer(mut pipe: std::process::ChildStderr) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut drained = 0usize;
        let mut sink = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut sink) {
            if n == 0 {
                break;
            }
            drained = drained.saturating_add(n);
            let _ = drained.min(MAX_CLI_STDERR_BYTES);
        }
    })
}

fn cleanup_child_and_pipes(
    child: &mut nexora_os_exec::ProcessTreeChild,
    stdout_handle: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr_handle: Option<std::thread::JoinHandle<()>>,
) {
    let _ = child.kill_tree();
    let _ = child.wait();
    if let Some(h) = stdout_handle {
        let _ = h.join();
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
}

/// Execute `program` with `args` inside a killable OS process tree with a
/// clean sanitized environment, optional `stdin_payload`, hard `timeout`,
/// cooperative `CancellationToken`, and strict `max_stdout_bytes`.
fn run_contained_cli(
    program: &str,
    args: &[String],
    stdin_payload: Option<&[u8]>,
    timeout: Duration,
    max_stdout_bytes: usize,
    token: &CancellationToken,
) -> SubprocessOutcome {
    if token.is_cancelled() {
        return SubprocessOutcome::Cancelled;
    }

    let sanitized_env = build_sanitized_cli_env_from(std::env::vars());
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(&sanitized_env)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let Ok(mut child) = spawn_in_process_tree(&mut cmd) else {
        return SubprocessOutcome::SpawnFailed;
    };

    if let Some(payload) = stdin_payload {
        if let Some(mut stdin_pipe) = child.take_stdin() {
            let owned = payload.to_vec();
            std::thread::spawn(move || {
                let _ = stdin_pipe.write_all(&owned);
                let _ = stdin_pipe.flush();
            });
        }
    }

    let overflow_flag = Arc::new(AtomicBool::new(false));
    let kill_handle = child.kill_handle();
    let stdout_handle = child.take_stdout().map(|pipe| {
        spawn_stdout_reader(
            pipe,
            max_stdout_bytes,
            Arc::clone(&overflow_flag),
            kill_handle.clone(),
        )
    });
    let stderr_handle = child.take_stderr().map(spawn_stderr_drainer);

    let deadline = Instant::now() + timeout;
    let exit_status = loop {
        if token.is_cancelled() {
            cleanup_child_and_pipes(&mut child, stdout_handle, stderr_handle);
            return SubprocessOutcome::Cancelled;
        }
        if overflow_flag.load(Ordering::SeqCst) {
            cleanup_child_and_pipes(&mut child, stdout_handle, stderr_handle);
            return SubprocessOutcome::OutputTooLarge;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    cleanup_child_and_pipes(&mut child, stdout_handle, stderr_handle);
                    return SubprocessOutcome::TimedOut;
                }
                std::thread::sleep(WAIT_POLL_INTERVAL);
            }
            Err(_) => {
                cleanup_child_and_pipes(&mut child, stdout_handle, stderr_handle);
                return SubprocessOutcome::SpawnFailed;
            }
        }
    };

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }

    if overflow_flag.load(Ordering::SeqCst) {
        return SubprocessOutcome::OutputTooLarge;
    }

    SubprocessOutcome::Exited {
        code: exit_status.code(),
        stdout,
    }
}

/// Probe whether the official CLI for `kind` is installed and executable in
/// the sanitized `PATH`, returning its sanitized version line if available.
#[must_use]
pub(crate) fn probe_cli_adapter(kind: CliAdapterKind) -> (bool, Option<String>) {
    let args = vec!["--version".to_string()];
    let token = CancellationToken::new();
    match run_contained_cli(
        kind.binary_name(),
        &args,
        None,
        CLI_PROBE_TIMEOUT,
        MAX_PROBE_OUTPUT_BYTES,
        &token,
    ) {
        SubprocessOutcome::Exited {
            code: Some(0),
            stdout,
        } => {
            let text = String::from_utf8_lossy(&stdout);
            (true, sanitize_version_line(&text))
        }
        _ => (false, None),
    }
}

/// Detect all supported external CLI adapters (`codex`, `claude`) and merge
/// with caller-supplied `is_enabled` opt-in status.
#[must_use]
pub(crate) fn detect_cli_adapters<F>(is_enabled: F) -> Vec<CliAdapterStatus>
where
    F: Fn(&str) -> bool,
{
    detect_cli_adapters_with_probe(probe_cli_adapter, is_enabled)
}

/// Testable availability probe accepting an injected probe closure.
#[must_use]
pub(crate) fn detect_cli_adapters_with_probe<P, F>(probe: P, is_enabled: F) -> Vec<CliAdapterStatus>
where
    P: Fn(CliAdapterKind) -> (bool, Option<String>),
    F: Fn(&str) -> bool,
{
    CliAdapterKind::ALL
        .into_iter()
        .map(|kind| {
            let (installed, version) = probe(kind);
            CliAdapterStatus {
                id: kind.id(),
                provider_name: kind.provider_name(),
                display_name: kind.display_name(),
                installed,
                version,
                enabled: is_enabled(kind.provider_name()),
            }
        })
        .collect()
}

/// Concrete [`ProviderExecutor`] delegating to an officially installed external
/// CLI (`codex` or `claude`) inside a clean, killable OS process tree.
pub(crate) struct CliAdapterExecutor {
    kind: CliAdapterKind,
    command_override: Option<(String, Vec<String>)>,
    max_output_bytes: usize,
    default_timeout: Duration,
}

impl CliAdapterExecutor {
    /// Build an executor for the official `codex` CLI.
    #[must_use]
    pub(crate) fn codex() -> Self {
        Self::new(CliAdapterKind::Codex)
    }

    /// Build an executor for the official `claude` CLI.
    #[must_use]
    pub(crate) fn claude() -> Self {
        Self::new(CliAdapterKind::Claude)
    }

    /// Build an executor for `kind` with production safety limits.
    #[must_use]
    pub(crate) fn new(kind: CliAdapterKind) -> Self {
        Self {
            kind,
            command_override: None,
            max_output_bytes: MAX_CLI_OUTPUT_BYTES,
            default_timeout: DEFAULT_CLI_TIMEOUT,
        }
    }

    /// Test-only constructor overriding the executable, arguments, timeout, and
    /// output bound to verify containment without live CLIs.
    #[cfg(test)]
    pub(crate) fn with_test_override(
        kind: CliAdapterKind,
        program: String,
        args: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            kind,
            command_override: Some((program, args)),
            max_output_bytes,
            default_timeout: timeout,
        }
    }

    fn run(
        &self,
        request: &AiRequest,
        _credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        // Deliberately ignore `_credential`: external CLI adapters rely only on
        // the user's officially authenticated external CLI and never receive
        // Nexora API keys or closed OAuth tokens.
        let prompt = format_request_prompt(request);
        if prompt.trim().is_empty() {
            return Err(ExecutorError::InvalidRequest);
        }

        let (program, args) = match &self.command_override {
            Some((prog, args)) => (prog.as_str(), args.clone()),
            None => (
                self.kind.binary_name(),
                build_cli_args(self.kind, &request.model),
            ),
        };

        let timeout = request
            .request_timeout
            .unwrap_or(self.default_timeout)
            .min(MAX_CLI_TIMEOUT);

        match run_contained_cli(
            program,
            &args,
            Some(prompt.as_bytes()),
            timeout,
            self.max_output_bytes,
            token,
        ) {
            SubprocessOutcome::Exited {
                code: Some(0),
                stdout,
            } => {
                let text = String::from_utf8_lossy(&stdout).trim().to_string();
                if text.is_empty() {
                    log::warn!(
                        "{} cli adapter returned empty output",
                        self.kind.provider_name()
                    );
                    return Err(ExecutorError::UnexpectedResponse);
                }
                Ok(AiResponse {
                    content: text,
                    model: request.model.clone(),
                    tool_calls: Vec::new(),
                    usage: None,
                })
            }
            SubprocessOutcome::Exited { code, .. } => {
                log::warn!(
                    "{} cli adapter exited with non-zero status ({:?})",
                    self.kind.provider_name(),
                    code
                );
                Err(ExecutorError::Failure)
            }
            SubprocessOutcome::TimedOut => {
                log::warn!(
                    "{} cli adapter timed out and process tree was killed",
                    self.kind.provider_name()
                );
                Err(ExecutorError::Network)
            }
            SubprocessOutcome::Cancelled => Err(ExecutorError::Cancelled),
            SubprocessOutcome::OutputTooLarge => {
                log::warn!(
                    "{} cli adapter exceeded bounded stdout cap and was killed",
                    self.kind.provider_name()
                );
                Err(ExecutorError::UnexpectedResponse)
            }
            SubprocessOutcome::SpawnFailed => {
                log::warn!(
                    "{} cli adapter binary could not be launched",
                    self.kind.provider_name()
                );
                Err(ExecutorError::ProviderUnavailable)
            }
        }
    }
}

impl ProviderExecutor for CliAdapterExecutor {
    fn execute(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
    ) -> Result<AiResponse, ExecutorError> {
        self.run(request, credential, token)
    }

    fn execute_stream_events(
        &self,
        request: &AiRequest,
        credential: &str,
        token: &CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<AiResponse, ExecutorError> {
        let response = self.run(request, credential, token)?;
        if !response.content.is_empty() {
            on_event(StreamEvent::TextDelta(response.content.clone()));
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexora_core::provider::AiMessage;

    fn sample_request(provider: &str, model: &str, prompt: &str) -> AiRequest {
        AiRequest {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: Arc::new(vec![AiMessage {
                role: AiRole::User,
                content: prompt.to_string(),
                attachments: Vec::new(),
                tool_calls: Vec::new(),
                tool_result: None,
            }]),
            tools: Arc::new(Vec::new()),
            model_config: None,
            request_timeout: None,
        }
    }

    #[cfg(windows)]
    fn shell_cmd(script: &str) -> (String, Vec<String>) {
        (
            "powershell.exe".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                script.to_string(),
            ],
        )
    }

    #[cfg(not(windows))]
    fn shell_cmd(script: &str) -> (String, Vec<String>) {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), script.to_string()],
        )
    }

    #[test]
    fn sanitized_env_strips_nexora_and_provider_secrets_while_keeping_os_essentials() {
        let fake_host = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-live-secret".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-ant-secret".to_string()),
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "closed-tok".to_string(),
            ),
            ("NEXORA_DB_KEY".to_string(), "db-secret".to_string()),
            (
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "aws-secret".to_string(),
            ),
            ("GITHUB_TOKEN".to_string(), "ghp_secret".to_string()),
            ("RANDOM_CUSTOM_VAR".to_string(), "not-essential".to_string()),
        ];

        let sanitized = build_sanitized_cli_env_from(fake_host);
        assert_eq!(sanitized.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(!sanitized.contains_key("OPENAI_API_KEY"));
        assert!(!sanitized.contains_key("ANTHROPIC_API_KEY"));
        assert!(!sanitized.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!sanitized.contains_key("NEXORA_DB_KEY"));
        assert!(!sanitized.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!sanitized.contains_key("GITHUB_TOKEN"));
        assert!(!sanitized.contains_key("RANDOM_CUSTOM_VAR"));
    }

    #[test]
    fn safe_model_arg_rejects_flags_whitespace_and_control_chars() {
        assert!(is_safe_cli_model_arg("gpt-5.2"));
        assert!(is_safe_cli_model_arg("claude-sonnet-4-5"));
        assert!(!is_safe_cli_model_arg("--dangerously-skip-permissions"));
        assert!(!is_safe_cli_model_arg("-x"));
        assert!(!is_safe_cli_model_arg("model with space"));
        assert!(!is_safe_cli_model_arg(""));
        assert!(!is_safe_cli_model_arg("bad\nmodel"));
    }

    #[test]
    fn cli_adapter_executes_via_stdin_clears_env_and_streams_delta() {
        #[cfg(windows)]
        let script = "$inputText = [Console]::In.ReadToEnd(); Write-Output ('ECHO:' + $inputText.Trim() + '|SECRET:' + $env:OPENAI_API_KEY)";
        #[cfg(not(windows))]
        let script = "body=$(cat); printf 'ECHO:%s|SECRET:%s\n' \"$body\" \"${OPENAI_API_KEY:-}\"";

        let (prog, args) = shell_cmd(script);
        let executor = CliAdapterExecutor::with_test_override(
            CliAdapterKind::Codex,
            prog,
            args,
            Duration::from_secs(10),
            MAX_CLI_OUTPUT_BYTES,
        );

        let req = sample_request(CLI_CODEX_NAME, CLI_CODEX_MODELS[0], "hello from nexora");
        let token = CancellationToken::new();
        let deltas = std::sync::Mutex::new(Vec::new());

        let resp = executor
            .execute_stream_events(&req, "sk-must-never-leak", &token, &|ev| {
                if let StreamEvent::TextDelta(text) = ev {
                    deltas.lock().expect("lock").push(text);
                }
            })
            .expect("cli execution succeeds");

        assert!(resp.content.contains("hello from nexora"));
        assert!(
            resp.content.ends_with("|SECRET:"),
            "OPENAI_API_KEY must be empty in cleared env, got: {}",
            resp.content
        );
        assert!(!resp.content.contains("sk-must-never-leak"));
        assert_eq!(deltas.lock().expect("lock").len(), 1);
    }

    #[test]
    fn cli_adapter_kills_process_tree_on_timeout() {
        #[cfg(windows)]
        let script = "Start-Sleep -Seconds 30";
        #[cfg(not(windows))]
        let script = "sleep 30";

        let (prog, args) = shell_cmd(script);
        let executor = CliAdapterExecutor::with_test_override(
            CliAdapterKind::Claude,
            prog,
            args,
            Duration::from_millis(250),
            MAX_CLI_OUTPUT_BYTES,
        );
        let req = sample_request(CLI_CLAUDE_NAME, CLI_CLAUDE_MODELS[0], "ping");
        let start = Instant::now();
        let err = executor
            .execute(&req, "", &CancellationToken::new())
            .expect_err("must time out");
        assert!(matches!(err, ExecutorError::Network));
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "process tree must be killed promptly on timeout"
        );
    }

    #[test]
    fn cli_adapter_kills_process_tree_on_cancellation() {
        #[cfg(windows)]
        let script = "Start-Sleep -Seconds 30";
        #[cfg(not(windows))]
        let script = "sleep 30";

        let (prog, args) = shell_cmd(script);
        let executor = CliAdapterExecutor::with_test_override(
            CliAdapterKind::Codex,
            prog,
            args,
            Duration::from_secs(30),
            MAX_CLI_OUTPUT_BYTES,
        );
        let token = CancellationToken::new();
        let token_clone = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            token_clone.cancel();
        });

        let req = sample_request(CLI_CODEX_NAME, CLI_CODEX_MODELS[0], "ping");
        let start = Instant::now();
        let err = executor.execute(&req, "", &token).expect_err("must cancel");
        assert!(matches!(err, ExecutorError::Cancelled));
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "process tree must be killed promptly on cancel"
        );
    }

    #[test]
    fn cli_adapter_enforces_bounded_output() {
        #[cfg(windows)]
        let script = "Write-Output ('A' * 4096)";
        #[cfg(not(windows))]
        let script = "head -c 4096 /dev/zero | tr '\\0' 'A'";

        let (prog, args) = shell_cmd(script);
        let executor = CliAdapterExecutor::with_test_override(
            CliAdapterKind::Codex,
            prog,
            args,
            Duration::from_secs(10),
            256,
        );
        let req = sample_request(CLI_CODEX_NAME, CLI_CODEX_MODELS[0], "big");
        let err = executor
            .execute(&req, "", &CancellationToken::new())
            .expect_err("oversized stdout must fail");
        assert!(matches!(err, ExecutorError::UnexpectedResponse));
    }

    #[test]
    fn detect_cli_adapters_reports_both_adapters_and_opt_in_state() {
        let statuses = detect_cli_adapters_with_probe(
            |kind| match kind {
                CliAdapterKind::Codex => (true, Some("codex 0.50.0".to_string())),
                CliAdapterKind::Claude => (false, None),
            },
            |provider_name| provider_name == CLI_CODEX_NAME,
        );
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].id, "codex");
        assert_eq!(statuses[0].provider_name, CLI_CODEX_NAME);
        assert!(statuses[0].installed);
        assert_eq!(statuses[0].version.as_deref(), Some("codex 0.50.0"));
        assert!(statuses[0].enabled);

        assert_eq!(statuses[1].id, "claude");
        assert_eq!(statuses[1].provider_name, CLI_CLAUDE_NAME);
        assert!(!statuses[1].installed);
        assert!(statuses[1].version.is_none());
        assert!(!statuses[1].enabled);
    }
}
