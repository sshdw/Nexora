use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::{spawn_sandboxed_in_process_tree, ProcessTreeChild};

static PROBE_SEQ: AtomicU64 = AtomicU64::new(1);

const PROBE_CANARY: &str = "nexora-sandbox-private-probe-canary";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Verification result produced by [`verify_platform_sandbox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxVerificationStatus {
    /// OS sandbox spawned a boundary probe and proved:
    /// (1) outside-workspace write failed,
    /// (2) private-directory read failed, and
    /// (3) in-workspace benign operation succeeded.
    Verified { mechanism: String },
    /// The current OS / kernel / host tooling lacks an enforceable native sandbox.
    Unavailable { reason: String },
    /// A platform sandbox was configured or started, but at least one mandatory
    /// boundary probe assertion failed.
    Failed { reason: String },
}

impl SandboxVerificationStatus {
    /// Returns `true` only when the boundary probe actively verified isolation.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// Returns the verified OS isolation mechanism name when `Verified`.
    #[must_use]
    pub fn mechanism(&self) -> Option<&str> {
        match self {
            Self::Verified { mechanism } => Some(mechanism.as_str()),
            Self::Unavailable { .. } | Self::Failed { .. } => None,
        }
    }

    /// Returns the human/IPC failure or unavailability detail when unverified.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Verified { .. } => None,
            Self::Unavailable { reason } | Self::Failed { reason } => Some(reason.as_str()),
        }
    }

    /// Maps verification status onto the authoritative Nexora policy posture string.
    #[must_use]
    pub fn posture(&self) -> &'static str {
        if self.is_verified() {
            "Verified"
        } else {
            "PerCommandConsent"
        }
    }
}

/// Pure probe result extracted from the sandboxed boundary-probe child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub outside_write_blocked: bool,
    pub private_read_blocked: bool,
    pub workspace_check_succeeded: bool,
    pub detail: Option<String>,
}

/// Evaluates raw probe check outcomes into a fail-closed [`SandboxVerificationStatus`].
#[must_use]
pub fn evaluate_probe_outcome(
    mechanism: &str,
    outcome: Result<ProbeOutcome, String>,
) -> SandboxVerificationStatus {
    match outcome {
        Err(reason) => SandboxVerificationStatus::Failed { reason },
        Ok(probe) => {
            if !probe.outside_write_blocked {
                return SandboxVerificationStatus::Failed {
                    reason: format!(
                        "sandbox probe write outside workspace succeeded unexpectedly{}",
                        probe
                            .detail
                            .as_deref()
                            .map_or_else(String::new, |d| format!(" ({d})"))
                    ),
                };
            }
            if !probe.private_read_blocked {
                return SandboxVerificationStatus::Failed {
                    reason: format!(
                        "sandbox probe read of private directory succeeded unexpectedly{}",
                        probe
                            .detail
                            .as_deref()
                            .map_or_else(String::new, |d| format!(" ({d})"))
                    ),
                };
            }
            if !probe.workspace_check_succeeded {
                return SandboxVerificationStatus::Failed {
                    reason: format!(
                        "sandbox probe benign check inside workspace failed{}",
                        probe
                            .detail
                            .as_deref()
                            .map_or_else(String::new, |d| format!(" ({d})"))
                    ),
                };
            }
            SandboxVerificationStatus::Verified {
                mechanism: mechanism.to_owned(),
            }
        }
    }
}

/// Filesystem and environment boundary policy enforced for sandboxed child processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub workspace_root: PathBuf,
    pub writable_roots: Vec<PathBuf>,
    pub deny_read_paths: Vec<PathBuf>,
    pub deny_write_paths: Vec<PathBuf>,
    pub sanitize_env: bool,
}

impl SandboxPolicy {
    /// Builds the default fail-closed workspace policy masking user private directories.
    #[must_use]
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        let root = canonicalize_clean(workspace_root.as_ref())
            .unwrap_or_else(|_| workspace_root.as_ref().to_path_buf());
        let mut deny_write_paths = Vec::new();
        for protected in [".nexora", ".git", ".git/hooks"] {
            let candidate = root.join(protected);
            if candidate.exists() {
                deny_write_paths.push(candidate);
            }
        }
        let mut deny_read_paths = canonical_prefixes(&default_private_user_dirs());
        // Discover lexical private stores under this workspace without ever entering them.
        let mut pending = if deny_read_paths.iter().any(|p| root.starts_with(p)) {
            Vec::new()
        } else {
            vec![root.clone()]
        };
        let mut visited = 0;
        while let Some(path) = pending.pop() {
            visited += 1;
            if visited > 100_000 {
                deny_read_paths.push(root.clone());
                break;
            }
            if private_store_path(&path)
                || path
                    .file_name()
                    .is_some_and(|name| name == "for human do not open tis")
            {
                deny_read_paths.push(path);
                continue;
            }
            if sensitive_write_path(&path) {
                deny_write_paths.push(path.clone());
            }
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    match std::fs::read_dir(&path) {
                        Ok(entries) => {
                            for entry in entries {
                                if let Ok(entry) = entry {
                                    pending.push(entry.path());
                                } else {
                                    deny_read_paths.push(root.clone());
                                    break;
                                }
                            }
                        }
                        Err(err)
                            if err.kind() == io::ErrorKind::NotFound
                                && is_writable_build_dir(&root, &path) => {}
                        Err(_) => {
                            deny_read_paths.push(root.clone());
                            break;
                        }
                    }
                }
                Ok(_) => {}
                Err(err)
                    if err.kind() == io::ErrorKind::NotFound
                        && is_writable_build_dir(&root, &path) => {}
                Err(_) => {
                    deny_read_paths.push(root.clone());
                    break;
                }
            }
        }
        deny_read_paths.sort();
        deny_read_paths.dedup();
        deny_write_paths.sort();
        deny_write_paths.dedup();
        Self {
            workspace_root: root.clone(),
            writable_roots: vec![root],
            deny_read_paths,
            deny_write_paths,
            sanitize_env: true,
        }
    }
}

/// Configuration passed to [`spawn_sandboxed_in_process_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpawnConfig {
    pub policy: SandboxPolicy,
}

impl SandboxSpawnConfig {
    /// Creates a sandbox spawn config bound to `workspace_root`.
    #[must_use]
    pub fn for_workspace(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            policy: SandboxPolicy::for_workspace(workspace_root),
        }
    }
}

/// Returns private user directories that must stay inaccessible inside the OS sandbox
/// (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, Nexora app-data/config directories, etc.).
#[must_use]
pub fn default_private_user_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home_dir) = home.map(PathBuf::from) {
        for rel in [
            ".ssh",
            ".aws",
            ".gnupg",
            ".kube",
            ".config/gh",
            ".config/nexora",
            ".local/share/nexora",
            ".mozilla",
            ".config/google-chrome",
            ".config/chromium",
            "Library/Keychains",
            "Library/Application Support",
            "Library/Containers",
        ] {
            dirs.push(home_dir.join(rel));
        }
    }
    for key in [
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ] {
        if let Some(base) = std::env::var_os(key) {
            let base_path = PathBuf::from(base);
            dirs.push(base_path.clone());
            dirs.push(base_path.join("com.nexora.app"));
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

pub(crate) struct ProbeSpec<'a> {
    pub workspace_root: &'a Path,
    pub outside_file: &'a Path,
    pub private_file: &'a Path,
    pub in_workspace_file: &'a Path,
}

struct ProbeTempGuard {
    outside_dir: PathBuf,
    private_dir: PathBuf,
    in_workspace_file: PathBuf,
}

impl Drop for ProbeTempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.in_workspace_file);
        let _ = std::fs::remove_dir_all(&self.outside_dir);
        let _ = std::fs::remove_dir_all(&self.private_dir);
    }
}

/// Runs the mandatory platform boundary probe against `workspace_root`.
///
/// Spawns an actual OS-sandboxed child attempting:
/// 1. an outside-workspace write (must fail),
/// 2. a private-directory read (must fail), and
/// 3. an in-workspace benign write/read/cleanup check (must succeed).
#[must_use]
pub fn verify_platform_sandbox(workspace_root: &Path) -> SandboxVerificationStatus {
    verify_platform_sandbox_with_deny_paths(workspace_root, &[])
}

/// Same as [`verify_platform_sandbox`], masking `extra_deny_read_paths` in addition to
/// [`default_private_user_dirs`]. Useful for testing synthetic private directories.
#[must_use]
#[allow(clippy::too_many_lines)] // Keep synthetic fixture ownership and three assertions in one visible lifetime.
pub fn verify_platform_sandbox_with_deny_paths(
    workspace_root: &Path,
    extra_deny_read_paths: &[PathBuf],
) -> SandboxVerificationStatus {
    if !workspace_root.is_dir() {
        return SandboxVerificationStatus::Unavailable {
            reason: "workspace root does not exist or is not a directory".to_owned(),
        };
    }

    let mechanism = match platform_mechanism_available() {
        Ok(name) => name,
        Err(status) => return status,
    };

    let Ok(canon_ws) = canonicalize_clean(workspace_root) else {
        return SandboxVerificationStatus::Unavailable {
            reason: "failed to canonicalize workspace root".to_owned(),
        };
    };

    let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(now) => now.as_nanos(),
        Err(_) => {
            return SandboxVerificationStatus::Failed {
                reason: "probe clock unavailable".into(),
            }
        }
    };
    let base_tmp = std::env::temp_dir();
    let canon_tmp = canonicalize_clean(&base_tmp).unwrap_or_else(|_| base_tmp.clone());

    let outside_dir = base_tmp.join(format!(
        "nexora-sb-out-{}-{stamp}-{seq}",
        std::process::id()
    ));
    let private_dir = base_tmp.join(format!(
        "nexora-sb-priv-{}-{stamp}-{seq}",
        std::process::id()
    ));
    let in_ws_file = canon_ws.join(format!(
        ".nexora-sandbox-probe-{}-{stamp}-{seq}.tmp",
        std::process::id()
    ));
    if std::fs::create_dir(&outside_dir).is_err() {
        return SandboxVerificationStatus::Unavailable {
            reason: "could not reserve outside probe fixture".into(),
        };
    }
    if std::fs::create_dir(&private_dir).is_err() {
        let _ = std::fs::remove_dir(&outside_dir);
        return SandboxVerificationStatus::Unavailable {
            reason: "could not reserve private probe fixture".into(),
        };
    }
    let _guard = ProbeTempGuard {
        outside_dir: outside_dir.clone(),
        private_dir: private_dir.clone(),
        in_workspace_file: in_ws_file.clone(),
    };
    let canon_out = canonicalize_clean(&outside_dir).unwrap_or_else(|_| outside_dir.clone());
    let canon_priv = canonicalize_clean(&private_dir).unwrap_or_else(|_| private_dir.clone());
    if canon_out.starts_with(&canon_ws) || canon_priv.starts_with(&canon_ws) {
        return SandboxVerificationStatus::Unavailable {
            reason: "temporary directory resides inside workspace root; cannot probe outside write boundary".to_owned(),
        };
    }
    let _ = &canon_tmp;

    let outside_file = canon_out.join("escape.txt");
    let private_file = canon_priv.join("secret.txt");
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&private_file)
        .and_then(|mut file| file.write_all(PROBE_CANARY.as_bytes()))
        .is_err()
    {
        return SandboxVerificationStatus::Unavailable {
            reason: "could not materialize synthetic private probe file".to_owned(),
        };
    }

    let mut policy = SandboxPolicy::for_workspace(&canon_ws);
    policy.deny_read_paths.push(canon_priv);
    policy
        .deny_read_paths
        .extend(extra_deny_read_paths.iter().cloned());

    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&in_ws_file)
        .is_err()
    {
        return SandboxVerificationStatus::Failed {
            reason: "could not create ordinary in-workspace probe fixture".into(),
        };
    }
    let spec = ProbeSpec {
        workspace_root: &canon_ws,
        outside_file: &outside_file,
        private_file: &private_file,
        in_workspace_file: &in_ws_file,
    };
    let mut cmd = build_platform_probe_command(&spec);
    let mut child = match spawn_sandboxed_in_process_tree(&mut cmd, &SandboxSpawnConfig { policy })
    {
        Ok(child) => child,
        Err(err) => {
            return SandboxVerificationStatus::Failed {
                reason: format!(
                    "sandbox boundary probe launch failed (OS code {})",
                    err.raw_os_error().unwrap_or(0)
                ),
            };
        }
    };

    let outcome = collect_probe_result(&mut child, &outside_file);
    evaluate_probe_outcome(mechanism, outcome)
}

pub(crate) fn canonical_prefixes(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| canonicalize_clean(p).unwrap_or_else(|_| p.clone()))
        .collect()
}

pub(crate) fn canonicalize_clean(path: &Path) -> io::Result<PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let raw = canon.to_string_lossy();
        if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{stripped}")));
        }
        if let Some(stripped) = raw.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canon)
}

fn collect_probe_result(
    child: &mut ProcessTreeChild,
    outside_file: &Path,
) -> Result<ProbeOutcome, String> {
    let capture = |pipe: Option<Box<dyn Read + Send>>| {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = bytes.clone();
        if let Some(mut reader) = pipe {
            std::thread::spawn(move || {
                let mut chunk = [0u8; 1024];
                while let Ok(count) = reader.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    if let Ok(mut buf) = shared.lock() {
                        let remaining = 8192usize.saturating_sub(buf.len());
                        buf.extend_from_slice(&chunk[..count.min(remaining)]);
                    }
                }
                let _ = tx.send(());
            });
        }
        (bytes, rx)
    };
    let (stdout_buf, stdout_done) = capture(
        child
            .take_stdout()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );
    let (stderr_buf, stderr_done) = capture(
        child
            .take_stderr()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );
    let start = Instant::now();
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > PROBE_TIMEOUT {
                    let _ = child.kill_tree();
                    let _ = child.wait();
                    return Err("sandbox boundary probe timed out".to_owned());
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(err) => return Err(format!("wait on sandbox probe failed: {err}")),
        }
    };
    let _ = child.kill_tree();
    let _ = stdout_done.recv_timeout(Duration::from_millis(250));
    let _ = stderr_done.recv_timeout(Duration::from_millis(250));
    let stdout =
        String::from_utf8_lossy(&stdout_buf.lock().map_err(|_| "probe capture unavailable")?)
            .into_owned();
    let stderr =
        String::from_utf8_lossy(&stderr_buf.lock().map_err(|_| "probe capture unavailable")?)
            .into_owned();
    if !exit_status.success() {
        return Err("sandbox boundary probe exited unsuccessfully".into());
    }

    let outside_write_blocked = stdout.contains("OUT_WRITE_BLOCKED")
        && !stdout.contains("OUT_WRITE_ALLOWED")
        && !outside_file.exists()
        && (!cfg!(windows)
            || (stdout.contains("OWN_WRITE_BLOCKED") && !stdout.contains("OWN_WRITE_ALLOWED")));
    let private_read_blocked = stdout.contains("PRIV_READ_BLOCKED")
        && !stdout.contains("PRIV_READ_ALLOWED")
        && !stdout.contains(PROBE_CANARY)
        && !stderr.contains(PROBE_CANARY);
    let workspace_check_succeeded = stdout.contains("IN_WS_OK")
        && !stdout.contains("IN_WS_FAIL")
        && (!cfg!(windows) || stdout.contains("NEXORA_WS_CANARY"));

    Ok(ProbeOutcome {
        outside_write_blocked,
        private_read_blocked,
        workspace_check_succeeded,
        detail: None,
    })
}

pub(crate) fn collect_sanitized_env(command: &Command, sanitize: bool) -> Vec<(String, String)> {
    // Command has no stable accessor for its env_clear bit. Never reconstruct
    // ambient inheritance: forward only explicitly supplied variables. This also
    // prevents shell startup hooks and nonstandard secret names leaking by default.
    let mut map: Vec<(String, String)> = Vec::new();

    for (key, maybe_val) in command.get_envs() {
        let Some(key_str) = key.to_str() else {
            continue;
        };
        map.retain(|(existing, _)| !env_key_eq(existing, key_str));
        if let Some(val) = maybe_val {
            if let Ok(val_str) = val.to_os_string().into_string() {
                map.push((key_str.to_owned(), val_str));
            }
        }
    }

    if sanitize {
        map.retain(|(key, _)| !is_sensitive_env_key(key));
    }
    map
}

fn env_key_eq(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Returns `true` when an environment variable name matches a host secret/credential pattern.
#[must_use]
pub fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("API_KEY")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("PASSWD")
        || upper.contains("PRIVATE_KEY")
        || upper.ends_with("_TOKEN")
        || upper == "TOKEN"
        || upper.ends_with("_CREDENTIAL")
        || upper.ends_with("_CREDENTIALS")
        || upper == "SSH_AUTH_SOCK"
        || upper == "SSH_AGENT_PID"
        || upper == "GPG_AGENT_INFO"
        || upper == "KUBECONFIG"
        || upper == "DOCKER_AUTH_CONFIG"
        || upper == "GOOGLE_APPLICATION_CREDENTIALS"
        || upper.starts_with("AWS_ACCESS_KEY")
        || upper.starts_with("AWS_SESSION")
        || upper.starts_with("OPENAI_")
        || upper.starts_with("ANTHROPIC_")
        || upper.starts_with("GEMINI_")
        || upper.starts_with("GOOGLE_API")
        || upper.starts_with("AZURE_OPENAI")
        || (upper.starts_with("NEXORA_")
            && !matches!(
                upper.as_str(),
                "NEXORA_PROBE_OUTSIDE" | "NEXORA_PROBE_PRIVATE" | "NEXORA_PROBE_IN_WS"
            ))
}

fn platform_mechanism_available() -> Result<&'static str, SandboxVerificationStatus> {
    #[cfg(windows)]
    {
        crate::windows_sandbox::probe_mechanism_available()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux_sandbox::probe_mechanism_available()
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos_sandbox::probe_mechanism_available()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err(SandboxVerificationStatus::Unavailable {
            reason: "unsupported host OS for native sandboxing".to_owned(),
        })
    }
}

fn build_platform_probe_command(spec: &ProbeSpec<'_>) -> Command {
    #[cfg(windows)]
    {
        crate::windows_sandbox::build_probe_command(spec)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux_sandbox::build_probe_command(spec)
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos_sandbox::build_probe_command(spec)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = spec;
        Command::new("false")
    }
}

pub(crate) fn spawn_sandboxed_platform(
    command: &mut Command,
    config: &SandboxSpawnConfig,
) -> io::Result<ProcessTreeChild> {
    validate_policy(&config.policy, command)?;
    #[cfg(windows)]
    {
        let (child, handle, guard) = crate::windows_sandbox::spawn_sandboxed(command, config)?;
        Ok(ProcessTreeChild::from_windows_restricted(
            child,
            handle,
            Box::new(guard),
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let (child, handle) = crate::linux_sandbox::spawn_sandboxed(command, config)?;
        Ok(ProcessTreeChild::from_std(child, handle))
    }
    #[cfg(target_os = "macos")]
    {
        let (child, handle, guard) = crate::macos_sandbox::spawn_sandboxed(command, config)?;
        Ok(ProcessTreeChild::from_std_with_guard(
            child,
            handle,
            Box::new(guard),
        ))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = (command, config);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native OS sandbox is not implemented for this target",
        ))
    }
}

/// Reject aliases that could turn a writable workspace inode into an outside file.
/// Unsupported or excessively large trees fail closed rather than partially scan.
pub(crate) fn validate_policy(policy: &SandboxPolicy, command: &Command) -> io::Result<()> {
    validate_command_strings(command)?;
    let root = canonicalize_clean(&policy.workspace_root)?;
    if private_store_path(&root) || root.parent().is_none() || !root.is_dir() {
        return Err(io::Error::other("invalid sandbox workspace"));
    }
    for writable in &policy.writable_roots {
        if canonicalize_clean(writable)? != root {
            return Err(io::Error::other(
                "additional writable roots are unsupported",
            ));
        }
    }
    if let Some(cwd) = command.get_current_dir() {
        if !canonicalize_clean(cwd)?.starts_with(&root) {
            return Err(io::Error::other("sandbox cwd outside workspace"));
        }
    }
    let denied: Vec<_> = policy
        .deny_read_paths
        .iter()
        .map(|p| canonicalize_clean(p).unwrap_or_else(|_| p.clone()))
        .collect();
    if denied.iter().any(|p| root.starts_with(p)) {
        return Err(io::Error::other("workspace overlaps private data"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut pending = vec![root];
        let mut visited = 0;
        while let Some(path) = pending.pop() {
            visited += 1;
            if visited > 100_000 {
                return Err(io::Error::other("workspace validation bound exceeded"));
            }
            if denied.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() || (meta.is_file() && meta.nlink() != 1) {
                return Err(io::Error::other("workspace contains a filesystem alias"));
            }
            if meta.is_dir() {
                for entry in std::fs::read_dir(path)? {
                    pending.push(entry?.path());
                }
            }
        }
    }
    Ok(())
}

#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Normalized ASCII case.
pub fn private_store_path(path: &Path) -> bool {
    let value = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let parts: Vec<_> = value.split('/').filter(|part| !part.is_empty()).collect();
    parts.iter().any(|part| {
        matches!(
            *part,
            ".ssh"
                | ".aws"
                | ".gnupg"
                | ".kube"
                | ".azure"
                | ".git-credentials"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | "keychains"
                | "keychain"
                | "credentials"
                | "vault"
        ) || part.ends_with(".keychain")
            || part.ends_with(".keychain-db")
    }) || parts.windows(2).any(|pair| {
        matches!(
            pair,
            [
                ".config",
                "gh" | "gcloud" | "microsoft-edge" | "git" | "google-chrome" | "chromium"
            ] | [".docker", "config.json"]
                | ["share", "keyrings"]
                | [
                    "microsoft",
                    "protect" | "crypto" | "tokenbroker" | "identitycache"
                ]
                | [".mozilla" | "application support", "firefox"]
                | ["firefox", "profiles"]
                | ["chrome" | "edge" | "chromium" | "vivaldi", "user data"]
                | ["opera software", "opera stable" | "opera gx stable"]
                | ["bravesoftware", "brave-browser"]
                | ["application support", "google"]
        )
    }) || parts.contains(&"safari")
}

fn validate_command_strings(command: &Command) -> io::Result<()> {
    let has_nul = |value: &std::ffi::OsStr| value.to_string_lossy().contains('\0');
    if has_nul(command.get_program()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox command program contains NUL byte",
        ));
    }
    for arg in command.get_args() {
        if has_nul(arg) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandbox command argument contains NUL byte",
            ));
        }
    }
    for (key, maybe_val) in command.get_envs() {
        let key_str = key.to_string_lossy();
        if key_str.contains('\0') || key_str.contains('=') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sandbox environment key contains invalid character",
            ));
        }
        if let Some(val) = maybe_val {
            if has_nul(val) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "sandbox environment value contains NUL byte",
                ));
            }
        }
    }
    Ok(())
}

#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)] // Normalized ASCII case.
pub fn sensitive_write_path(path: &Path) -> bool {
    let value = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let parts: Vec<_> = value.split('/').collect();
    parts.iter().any(|part| {
        part.starts_with(".env")
            || part.ends_with(".pem")
            || part.ends_with(".key")
            || part.starts_with("id_rsa")
            || part.starts_with("id_ed25519")
            || part.starts_with("id_ecdsa")
            || part.starts_with("id_dsa")
            || *part == ".nexora"
            || *part == ".git"
    })
}

/// Returns `true` when `path` is a recognized build/cache/artifact directory (or a
/// descendant inside one) strictly beneath `workspace_root`, excluding all protected
/// or sensitive paths (`.git`, `.nexora`, `.env*`, private stores, etc.).
#[must_use]
pub fn is_writable_build_dir(workspace_root: &Path, path: &Path) -> bool {
    if private_store_path(path) || sensitive_write_path(path) {
        return false;
    }
    let Ok(rel) = path.strip_prefix(workspace_root) else {
        return false;
    };
    let mut matched_build_component = false;
    for comp in rel.components() {
        let std::path::Component::Normal(raw) = comp else {
            return false;
        };
        let name = raw.to_string_lossy().to_ascii_lowercase();
        if name == ".git"
            || name == ".nexora"
            || name.starts_with(".env")
            || name == "for human do not open tis"
        {
            return false;
        }
        if matches!(
            name.as_str(),
            "target"
                | "dist"
                | "build"
                | "out"
                | "coverage"
                | ".vite"
                | ".cache"
                | ".turbo"
                | ".next"
                | ".nuxt"
                | ".parcel-cache"
                | ".svelte-kit"
                | "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
        ) {
            matched_build_component = true;
        }
    }
    matched_build_component
}

/// Returns `true` for the top-most build/cache directory root (e.g. `ws/target`,
/// `ws/dist`, `ws/node_modules/.cache`, `ws/src-tauri/target`), and `false` for
/// nested subdirectories inside it.
#[must_use]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_writable_build_dir_root(workspace_root: &Path, path: &Path) -> bool {
    if !is_writable_build_dir(workspace_root, path) {
        return false;
    }
    path.parent()
        .is_none_or(|parent| !is_writable_build_dir(workspace_root, parent))
}

#[cfg(unix)]
pub(crate) struct WorkspaceEntry {
    pub path: PathBuf,
    pub directory: bool,
    pub writable: bool,
}
#[cfg(unix)]
pub(crate) fn workspace_entries(policy: &SandboxPolicy) -> io::Result<Vec<WorkspaceEntry>> {
    use std::os::unix::fs::MetadataExt;
    let root = canonicalize_clean(&policy.workspace_root)?;
    let deny_read = canonical_prefixes(&policy.deny_read_paths);
    let deny_write = canonical_prefixes(&policy.deny_write_paths);
    let mut pending = vec![root];
    let mut result = Vec::new();
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        if visited > 100_000 {
            return Err(io::Error::other("workspace validation bound exceeded"));
        }
        if private_store_path(&path) || deny_read.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() || (meta.is_file() && meta.nlink() != 1) {
            return Err(io::Error::other("workspace contains a filesystem alias"));
        }
        let directory = meta.is_dir();
        let writable = meta.is_file()
            && !sensitive_write_path(&path)
            && !deny_write.iter().any(|p| path.starts_with(p));
        if directory {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        if directory || meta.is_file() {
            result.push(WorkspaceEntry {
                path,
                directory,
                writable,
            });
        }
    }
    Ok(result)
}
