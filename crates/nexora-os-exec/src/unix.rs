use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Mutex;

pub(crate) struct PlatformHandle {
    pgid: libc::pid_t,
    killed: Mutex<bool>,
}

impl PlatformHandle {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        command.process_group(0);
        let mut child = command.spawn()?;
        let Ok(pgid) = libc::pid_t::try_from(child.id()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("spawned process id exceeds pid_t range"));
        };
        if pgid <= 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "spawned process id must be strictly positive",
            ));
        }

        Ok((
            child,
            Self {
                pgid,
                killed: Mutex::new(false),
            },
        ))
    }

    pub(crate) fn kill_tree(&self) -> io::Result<()> {
        if self.pgid <= 0 {
            return Err(io::Error::other(
                "refusing to kill non-positive process group id",
            ));
        }

        let mut killed = self
            .killed
            .lock()
            .map_err(|_| io::Error::other("process-tree group mutex poisoned"))?;
        if *killed {
            return Ok(());
        }

        // SAFETY: `self.pgid` is verified strictly positive (`> 0`) and corresponds to
        // the dedicated process group created via `process_group(0)` for the spawned
        // child; sending `SIGKILL` has no memory-safety invariants.
        let rc = unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
        if rc == 0 {
            *killed = true;
            return Ok(());
        }

        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            *killed = true;
            return Ok(());
        }

        Err(err)
    }
}

impl Drop for PlatformHandle {
    fn drop(&mut self) {
        let _ = self.kill_tree();
    }
}

#[cfg(test)]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if raw_pid <= 0 {
        return false;
    }
    // SAFETY: Signal 0 only probes process existence without delivering a signal.
    let rc = unsafe { libc::kill(raw_pid, 0) };
    if rc == 0 {
        return true;
    }
    matches!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_tree_rejects_non_positive_pgid() {
        for bad_pgid in [0, -1, libc::pid_t::MIN] {
            let handle = PlatformHandle {
                pgid: bad_pgid,
                killed: Mutex::new(false),
            };
            assert!(
                handle.kill_tree().is_err(),
                "kill_tree must reject non-positive pgid {bad_pgid}"
            );
        }
    }
}
