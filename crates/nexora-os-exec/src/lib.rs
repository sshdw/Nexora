//! Cross-platform OS process-tree spawning and termination.
//!
//! Spawns commands inside a killable OS process containment boundary:
//! - Windows: an anonymous Job Object configured with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, terminated with `TerminateJobObject`
//!   and closed on stop, timeout, or drop.
//! - Unix: a dedicated process group (`CommandExt::process_group(0)`),
//!   terminated with `libc::killpg(..., SIGKILL)` on stop, timeout, or drop.

use std::any::Any;
use std::io;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux_sandbox;
#[cfg(target_os = "macos")]
mod macos_sandbox;
mod sandbox;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_sandbox;

pub use sandbox::{
    default_private_user_dirs, evaluate_probe_outcome, is_sensitive_env_key, is_writable_build_dir,
    private_store_path, sensitive_write_path, verify_platform_sandbox,
    verify_platform_sandbox_with_deny_paths, ProbeOutcome, SandboxPolicy, SandboxSpawnConfig,
    SandboxVerificationStatus,
};

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(windows, unix)))]
compile_error!("nexora-os-exec requires either Windows or Unix target OS support");

/// Cloneable, thread-safe handle capable of terminating the entire spawned
/// process tree from another thread or cancellation hook.
#[derive(Clone)]
pub struct ProcessTreeHandle {
    inner: Arc<platform::PlatformHandle>,
}

impl std::fmt::Debug for ProcessTreeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessTreeHandle").finish_non_exhaustive()
    }
}

impl ProcessTreeHandle {
    /// Terminate the root process and every descendant process in its tree.
    ///
    /// Idempotent across repeated calls.
    ///
    /// # Errors
    /// Returns an `io::Error` if the underlying OS termination call fails for an
    /// active process tree.
    pub fn kill_tree(&self) -> io::Result<()> {
        self.inner.kill_tree()
    }
}

/// Child process spawned inside an OS process-tree containment boundary.
///
/// Dropping `ProcessTreeChild` automatically terminates any remaining processes
/// in its tree and reaps the direct child if it has not yet been waited on.
enum ChildBackend {
    Std(Child),
    #[cfg(windows)]
    WindowsRestricted(windows_sandbox::RestrictedChild),
}

pub struct ProcessTreeChild {
    backend: ChildBackend,
    handle: ProcessTreeHandle,
    reaped: bool,
    _guards: Vec<Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for ProcessTreeChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessTreeChild")
            .field("id", &self.id())
            .field("reaped", &self.reaped)
            .finish_non_exhaustive()
    }
}

impl ProcessTreeChild {
    #[allow(dead_code)]
    pub(crate) fn from_std(child: Child, handle: platform::PlatformHandle) -> Self {
        Self {
            backend: ChildBackend::Std(child),
            handle: ProcessTreeHandle {
                inner: Arc::new(handle),
            },
            reaped: false,
            _guards: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_std_with_guard(
        child: Child,
        handle: platform::PlatformHandle,
        guard: Box<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            backend: ChildBackend::Std(child),
            handle: ProcessTreeHandle {
                inner: Arc::new(handle),
            },
            reaped: false,
            _guards: vec![guard],
        }
    }

    #[cfg(windows)]
    pub(crate) fn from_windows_restricted(
        child: windows_sandbox::RestrictedChild,
        handle: platform::PlatformHandle,
        guard: Box<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            backend: ChildBackend::WindowsRestricted(child),
            handle: ProcessTreeHandle {
                inner: Arc::new(handle),
            },
            reaped: false,
            _guards: vec![guard],
        }
    }

    /// Returns the OS-assigned process identifier of the direct child process.
    #[must_use]
    pub fn id(&self) -> u32 {
        match &self.backend {
            ChildBackend::Std(child) => child.id(),
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.id(),
        }
    }

    /// Returns a cloneable, thread-safe handle that kills the entire process tree.
    #[must_use]
    pub fn kill_handle(&self) -> ProcessTreeHandle {
        self.handle.clone()
    }

    /// Terminates the direct child process and all of its descendants.
    ///
    /// # Errors
    /// Returns an `io::Error` if the underlying OS tree-termination call fails.
    pub fn kill_tree(&self) -> io::Result<()> {
        self.handle.kill_tree()
    }

    /// Non-blocking poll for whether the direct child process has exited.
    ///
    /// # Errors
    /// Returns an `io::Error` if polling the OS child handle fails.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = match &mut self.backend {
            ChildBackend::Std(child) => child.try_wait()?,
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.try_wait()?,
        };
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    /// Blocks until the direct child process exits and returns its exit status.
    ///
    /// # Errors
    /// Returns an `io::Error` if waiting on the OS child handle fails.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = match &mut self.backend {
            ChildBackend::Std(child) => child.wait()?,
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.wait()?,
        };
        self.reaped = true;
        Ok(status)
    }

    /// Takes ownership of the child's piped standard input handle, if configured.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        match &mut self.backend {
            ChildBackend::Std(child) => child.stdin.take(),
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.take_stdin(),
        }
    }

    /// Takes ownership of the child's piped standard output handle, if configured.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        match &mut self.backend {
            ChildBackend::Std(child) => child.stdout.take(),
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.take_stdout(),
        }
    }

    /// Takes ownership of the child's piped standard error handle, if configured.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        match &mut self.backend {
            ChildBackend::Std(child) => child.stderr.take(),
            #[cfg(windows)]
            ChildBackend::WindowsRestricted(child) => child.take_stderr(),
        }
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        let _ = self.handle.kill_tree();
        if !self.reaped {
            match &mut self.backend {
                ChildBackend::Std(child) => {
                    let _ = child.wait();
                }
                #[cfg(windows)]
                ChildBackend::WindowsRestricted(child) => {
                    let _ = child.wait();
                }
            }
            self.reaped = true;
        }
    }
}

/// Trusted platform shell path, independent of PATH and COMSPEC.
/// # Errors
/// Returns an error if the host OS cannot resolve its system shell.
pub fn platform_shell_path() -> io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    let path = windows_sandbox::windows_cmd_path();
    #[cfg(not(windows))]
    let path = std::path::PathBuf::from("/bin/sh");
    if path.is_absolute() && path.is_file() {
        Ok(path)
    } else {
        Err(io::Error::other("trusted system shell unavailable"))
    }
}

/// Spawns `command` inside an OS process-tree boundary (Job Object on Windows,
/// dedicated process group on Unix).
///
/// # Errors
/// Returns an `io::Error` if spawning the command or binding it to the OS
/// process-tree boundary fails.
pub fn spawn_in_process_tree(command: &mut Command) -> io::Result<ProcessTreeChild> {
    let (child, handle) = platform::PlatformHandle::spawn(command)?;
    Ok(ProcessTreeChild::from_std(child, handle))
}

/// Spawns `command` inside an OS sandbox and process-tree containment boundary.
///
/// # Errors
///
/// Returns an `io::Error` if the workspace boundary cannot be resolved, OS
/// sandbox setup fails, or spawning the sandboxed child process fails.
pub fn spawn_sandboxed_in_process_tree(
    command: &mut Command,
    config: &SandboxSpawnConfig,
) -> io::Result<ProcessTreeChild> {
    sandbox::spawn_sandboxed_platform(command, config)
}

/// Checks whether the platform sandbox verifies isolated execution at `workspace_root`
/// and fails closed if any mandatory boundary probe assertion does not pass.
#[must_use]
pub fn verify_workspace_sandbox(workspace_root: &Path) -> SandboxVerificationStatus {
    verify_platform_sandbox(workspace_root)
}

/// Terminates the direct child and all descendant processes associated with
/// `child`.
///
/// # Errors
/// Returns an `io::Error` if the underlying OS tree-termination call fails.
pub fn kill_tree(child: &ProcessTreeChild) -> io::Result<()> {
    child.kill_tree()
}

#[cfg(test)]
pub(crate) fn native_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) fn test_workspace_parent() -> std::path::PathBuf {
    let private = sandbox::canonical_prefixes(&default_private_user_dirs());
    let is_non_private_dir = |candidate: &Path| -> bool {
        if !candidate.is_dir() {
            return false;
        }
        let Ok(canon) = sandbox::canonicalize_clean(candidate) else {
            return false;
        };
        !private.iter().any(|p| canon.starts_with(p))
    };
    let candidates = [
        Some(std::env::temp_dir()),
        std::env::var_os("RUNNER_TEMP").map(std::path::PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    ];
    for candidate in candidates.into_iter().flatten() {
        if is_non_private_dir(&candidate) {
            return candidate;
        }
    }
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let unique = format!(
                "{prefix}-{}-{}-{seq}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            let dir = crate::test_workspace_parent().join(unique);
            std::fs::create_dir(&dir).expect("reserve temp test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for_pid_file(pid_file: &Path, timeout: Duration) -> u32 {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(raw) = std::fs::read_to_string(pid_file) {
                let trimmed = raw.trim().trim_start_matches('\u{feff}');
                if let Ok(pid) = trimmed.parse::<u32>() {
                    if pid > 0 {
                        return pid;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "timed out waiting for descendant PID in {}",
            pid_file.display()
        );
    }

    fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !platform::is_process_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        !platform::is_process_alive(pid)
    }

    fn spawn_parent_with_descendant(pid_file: &Path) -> ProcessTreeChild {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;

            let escaped_path = pid_file.display().to_string().replace('\'', "''");
            let ps_script = format!(
                "$p = Start-Process -FilePath 'ping.exe' -ArgumentList '-n','60','127.0.0.1' -WindowStyle Hidden -PassThru; \
                 [System.IO.File]::WriteAllText('{escaped_path}', [string]$p.Id); \
                 Start-Sleep -Seconds 60"
            );
            let mut cmd = Command::new("cmd");
            cmd.args([
                "/D",
                "/C",
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &ps_script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
            spawn_in_process_tree(&mut cmd).expect("spawn parent+descendant on windows")
        }

        #[cfg(unix)]
        {
            let escaped = pid_file.display().to_string().replace('\'', "'\"'\"'");
            let script = format!("sleep 60 & echo $! > '{escaped}'; wait");
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            spawn_in_process_tree(&mut cmd).expect("spawn parent+descendant on unix")
        }
    }

    #[test]
    fn kill_tree_terminates_parent_and_descendant_processes() {
        let temp = TempDirGuard::new("nexora-os-exec-kill");
        let pid_file = temp.path().join("descendant.pid");
        let mut child = spawn_parent_with_descendant(&pid_file);
        let parent_pid = child.id();
        let descendant_pid = wait_for_pid_file(&pid_file, Duration::from_secs(10));

        assert!(
            platform::is_process_alive(descendant_pid),
            "descendant PID {descendant_pid} should be alive before kill_tree"
        );

        kill_tree(&child).expect("kill_tree should succeed");
        // Calling kill_tree repeatedly must remain idempotent.
        child.kill_tree().expect("second kill_tree should succeed");
        let _ = child.wait();

        assert!(
            wait_until_dead(descendant_pid, Duration::from_secs(5)),
            "descendant PID {descendant_pid} should be terminated after kill_tree"
        );
        assert!(
            wait_until_dead(parent_pid, Duration::from_secs(5)),
            "parent PID {parent_pid} should be terminated after kill_tree"
        );
    }

    #[test]
    fn dropping_child_terminates_descendant_processes() {
        let temp = TempDirGuard::new("nexora-os-exec-drop");
        let pid_file = temp.path().join("descendant.pid");
        let child = spawn_parent_with_descendant(&pid_file);
        let parent_pid = child.id();
        let descendant_pid = wait_for_pid_file(&pid_file, Duration::from_secs(10));

        assert!(
            platform::is_process_alive(descendant_pid),
            "descendant PID {descendant_pid} should be alive before drop"
        );

        drop(child);

        assert!(
            wait_until_dead(descendant_pid, Duration::from_secs(5)),
            "descendant PID {descendant_pid} should be terminated when ProcessTreeChild drops"
        );
        assert!(
            wait_until_dead(parent_pid, Duration::from_secs(5)),
            "parent PID {parent_pid} should be terminated when ProcessTreeChild drops"
        );
    }

    #[test]
    fn fast_exiting_command_completes_cleanly() {
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd");
            c.args(["/D", "/C", "echo", "hello"])
                .creation_flags(0x0800_0000);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "echo hello"]);
            c
        };

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = spawn_in_process_tree(&mut cmd).expect("spawn echo");
        let status = child.wait().expect("wait echo");
        assert!(status.success());
    }

    #[test]
    fn evaluate_probe_outcome_fails_closed_on_any_breach() {
        assert_eq!(
            SandboxVerificationStatus::Unavailable {
                reason: "none".to_owned(),
            }
            .posture(),
            "PerCommandConsent"
        );
        assert_eq!(
            SandboxVerificationStatus::Failed {
                reason: "bad".to_owned(),
            }
            .posture(),
            "PerCommandConsent"
        );
        assert_eq!(
            SandboxVerificationStatus::Verified {
                mechanism: "test".to_owned(),
            }
            .posture(),
            "Verified"
        );

        let unblocked_write = evaluate_probe_outcome(
            "test-mech",
            Ok(ProbeOutcome {
                outside_write_blocked: false,
                private_read_blocked: true,
                workspace_check_succeeded: true,
                detail: None,
            }),
        );
        assert!(matches!(
            unblocked_write,
            SandboxVerificationStatus::Failed { .. }
        ));
        assert_eq!(unblocked_write.posture(), "PerCommandConsent");

        let unblocked_read = evaluate_probe_outcome(
            "test-mech",
            Ok(ProbeOutcome {
                outside_write_blocked: true,
                private_read_blocked: false,
                workspace_check_succeeded: true,
                detail: None,
            }),
        );
        assert!(matches!(
            unblocked_read,
            SandboxVerificationStatus::Failed { .. }
        ));

        let failed_ws = evaluate_probe_outcome(
            "test-mech",
            Ok(ProbeOutcome {
                outside_write_blocked: true,
                private_read_blocked: true,
                workspace_check_succeeded: false,
                detail: Some("ws write failed".to_owned()),
            }),
        );
        assert!(matches!(
            failed_ws,
            SandboxVerificationStatus::Failed { .. }
        ));

        let probe_err = evaluate_probe_outcome("test-mech", Err("spawn denied".to_owned()));
        assert!(matches!(
            probe_err,
            SandboxVerificationStatus::Failed { .. }
        ));

        let verified = evaluate_probe_outcome(
            "test-mech",
            Ok(ProbeOutcome {
                outside_write_blocked: true,
                private_read_blocked: true,
                workspace_check_succeeded: true,
                detail: None,
            }),
        );
        assert_eq!(
            verified,
            SandboxVerificationStatus::Verified {
                mechanism: "test-mech".to_owned(),
            }
        );
        assert_eq!(verified.posture(), "Verified");
    }

    #[test]
    fn default_private_user_dirs_include_sensitive_locations() {
        let dirs = default_private_user_dirs();
        let joined = dirs
            .iter()
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(";");
        assert!(joined.contains(".ssh"), "missing .ssh in {joined}");
        assert!(joined.contains(".aws"), "missing .aws in {joined}");
        assert!(joined.contains(".gnupg"), "missing .gnupg in {joined}");
        assert!(joined.contains(".kube"), "missing .kube in {joined}");
        assert!(joined.contains("nexora"), "missing nexora in {joined}");
    }

    #[test]
    fn verify_workspace_sandbox_enforces_real_isolation_boundaries() {
        let _native = crate::native_test_lock();
        let ws = TempDirGuard::new("nexora-sandbox-ws");
        let status = verify_workspace_sandbox(ws.path());
        if std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_some() {
            assert!(
                status.is_verified(),
                "mandatory native host proof failed: {status:?}"
            );
            #[cfg(target_os = "linux")]
            if let Ok(mechanism) = std::env::var("NEXORA_TEST_LINUX_SANDBOX_MECHANISM") {
                assert_eq!(
                    status,
                    SandboxVerificationStatus::Verified { mechanism },
                    "CI must prove the requested Linux backend"
                );
            }
        } else if !status.is_verified() {
            assert_eq!(status.posture(), "PerCommandConsent");
            eprintln!("native isolation proof unavailable on this host: {status:?}");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One native child proves the coupled IO, private-read and environment boundary.
    fn sandboxed_spawn_blocks_outside_write_private_read_and_strips_secrets() {
        let _native = crate::native_test_lock();

        let ws = TempDirGuard::new("nexora-sandbox-exec-ws");
        let verification = verify_workspace_sandbox(ws.path());
        if !verification.is_verified() {
            assert_eq!(verification.posture(), "PerCommandConsent");
            assert!(
                std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_none(),
                "mandatory native proof unavailable: {verification:?}"
            );
            eprintln!("native confinement test unavailable: {verification:?}");
            return;
        }
        let outside_dir = TempDirGuard::new("nexora-sandbox-outside");
        let private_dir = TempDirGuard::new("nexora-sandbox-private");

        let outside_target = outside_dir.path().join("escaped.txt");
        let private_file = private_dir.path().join("id_ed25519");
        std::fs::write(&private_file, "SECRET_PRIVATE_KEY_CANARY").expect("write private canary");
        let in_ws_target = ws.path().join("allowed.txt");
        std::fs::write(&in_ws_target, "initial").expect("create ordinary workspace fixture");

        let mut policy = SandboxPolicy::for_workspace(ws.path());
        policy
            .deny_read_paths
            .push(private_dir.path().to_path_buf());
        let config = SandboxSpawnConfig { policy };

        #[cfg(windows)]
        let mut cmd = {
            let sys_root = std::env::var_os("SystemRoot")
                .or_else(|| std::env::var_os("WINDIR"))
                .unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
            let mut c = Command::new(
                std::path::PathBuf::from(sys_root)
                    .join("System32")
                    .join("cmd.exe"),
            );
            let script = format!(
                "echo leaked > \"{out}\" 2>NUL & type \"{priv}\" 2>NUL & echo ws_ok > \"{inw}\" & echo BENIGN_TEST_VAR=%BENIGN_TEST_VAR% & echo API=%OPENAI_API_KEY% & echo AWS=%AWS_SECRET_ACCESS_KEY%",
                out = outside_target.display(),
                priv = private_file.display(),
                inw = in_ws_target.display(),
            );
            c.args(["/D", "/C", &script]);
            c
        };

        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("/bin/sh");
            let script = format!(
                "(printf 'leaked\\n' > '{out}') 2>/dev/null || true; \
                 cat '{priv}' 2>/dev/null || true; \
                 printf 'ws_ok\\n' > '{inw}'; \
                 env",
                out = outside_target.display(),
                priv = private_file.display(),
                inw = in_ws_target.display(),
            );
            c.args(["-c", &script]);
            c
        };

        #[cfg(windows)]
        cmd.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
        );
        cmd.current_dir(ws.path())
            .env("OPENAI_API_KEY", "sk-secret-must-be-stripped")
            .env("AWS_SECRET_ACCESS_KEY", "aws-secret-must-be-stripped")
            .env("NEXORA_SAFE_CUSTOM", "benign_env_value")
            .env("NEXORA_BENIGN_NON_SENSITIVE", "1")
            .env("BENIGN_TEST_VAR", "visible_in_sandbox");

        let mut child =
            spawn_sandboxed_in_process_tree(&mut cmd, &config).expect("spawn sandboxed child");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = captured.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let out = child.take_stdout();
        std::thread::spawn(move || {
            if let Some(mut out) = out {
                let mut buf = [0u8; 1024];
                while let Ok(count) = out.read(&mut buf) {
                    if count == 0 {
                        break;
                    }
                    let mut output = output.lock().unwrap();
                    let available = 8192usize.saturating_sub(output.len());
                    output.extend_from_slice(&buf[..count.min(available)]);
                }
            }
            let _ = tx.send(());
        });
        let err = child.take_stderr();
        let (err_tx, err_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(mut err) = err {
                let _ = std::io::copy(&mut err, &mut std::io::sink());
            }
            let _ = err_tx.send(());
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll sandbox test") {
                break status;
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill_tree();
                panic!("sandbox child timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let _ = child.kill_tree();
        let _ = rx.recv_timeout(Duration::from_secs(2));
        let _ = err_rx.recv_timeout(Duration::from_secs(2));
        let stdout = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
        assert!(status.success(), "sandboxed test script failed: {status:?}");

        assert!(
            !outside_target.exists(),
            "sandboxed child must NOT be able to write outside workspace ({})",
            outside_target.display()
        );
        assert!(
            !stdout.contains("SECRET_PRIVATE_KEY_CANARY"),
            "sandboxed child must NOT be able to read masked private dir; stdout={stdout}"
        );
        assert!(
            in_ws_target.is_file(),
            "sandboxed child MUST be allowed to write inside workspace ({})",
            in_ws_target.display()
        );
        assert!(
            !stdout.contains("sk-secret-must-be-stripped")
                && !stdout.contains("aws-secret-must-be-stripped"),
            "sensitive host env secrets must be stripped; stdout={stdout}"
        );
        assert!(
            stdout.contains("BENIGN_TEST_VAR=visible_in_sandbox"),
            "benign env vars should remain available: {stdout:?}"
        );
    }

    #[test]
    fn verify_workspace_sandbox_fails_closed_for_missing_workspace() {
        let missing = std::env::temp_dir().join("nexora-definitely-missing-workspace-dir-99999");
        let _ = std::fs::remove_dir_all(&missing);
        let status = verify_workspace_sandbox(&missing);
        assert!(!status.is_verified());
        assert_eq!(status.posture(), "PerCommandConsent");
    }

    #[test]
    fn hardlink_in_workspace_is_rejected_without_mutating_outside_target() {
        let _native = crate::native_test_lock();
        let ws = TempDirGuard::new("nexora-hardlink-ws");
        let outside = TempDirGuard::new("nexora-hardlink-out");
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "unchanged").expect("write outside target");
        let ws_link = ws.path().join("alias.txt");
        std::fs::hard_link(&outside_file, &ws_link).expect("create test hardlink");

        let mut cmd = Command::new(platform_shell_path().expect("platform shell"));
        #[cfg(windows)]
        cmd.args(["/D", "/C", "echo hijacked>alias.txt"]);
        #[cfg(unix)]
        cmd.args(["-c", "echo hijacked > alias.txt"]);
        cmd.current_dir(ws.path());

        let result = spawn_sandboxed_in_process_tree(
            &mut cmd,
            &SandboxSpawnConfig::for_workspace(ws.path()),
        );
        assert!(
            result.is_err(),
            "hardlink to outside inode must fail before spawn"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_file).expect("read outside target"),
            "unchanged"
        );
    }
}
