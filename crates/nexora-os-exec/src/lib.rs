//! Cross-platform OS process-tree spawning and termination.
//!
//! Spawns commands inside a killable OS process containment boundary:
//! - Windows: an anonymous Job Object configured with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, terminated with `TerminateJobObject`
//!   and closed on stop, timeout, or drop.
//! - Unix: a dedicated process group (`CommandExt::process_group(0)`),
//!   terminated with `libc::killpg(..., SIGKILL)` on stop, timeout, or drop.

use std::io;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::Arc;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

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
pub struct ProcessTreeChild {
    child: Child,
    handle: ProcessTreeHandle,
    reaped: bool,
}

impl std::fmt::Debug for ProcessTreeChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessTreeChild")
            .field("id", &self.child.id())
            .field("reaped", &self.reaped)
            .finish_non_exhaustive()
    }
}

impl ProcessTreeChild {
    /// Returns the OS-assigned process identifier of the direct child process.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
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
        let status = self.child.try_wait()?;
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
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// Takes ownership of the child's piped standard input handle, if configured.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Takes ownership of the child's piped standard output handle, if configured.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Takes ownership of the child's piped standard error handle, if configured.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        let _ = self.handle.kill_tree();
        if !self.reaped {
            let _ = self.child.wait();
            self.reaped = true;
        }
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
    Ok(ProcessTreeChild {
        child,
        handle: ProcessTreeHandle {
            inner: Arc::new(handle),
        },
        reaped: false,
    })
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
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let unique = format!(
                "{prefix}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&dir).expect("create temp test dir");
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
}
