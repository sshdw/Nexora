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
        let child = command.spawn()?;
        let pgid = libc::pid_t::try_from(child.id())
            .map_err(|_| io::Error::other("spawned process id exceeds pid_t range"))?;

        Ok((
            child,
            Self {
                pgid,
                killed: Mutex::new(false),
            },
        ))
    }

    pub(crate) fn kill_tree(&self) -> io::Result<()> {
        let mut killed = self
            .killed
            .lock()
            .map_err(|_| io::Error::other("process-tree group mutex poisoned"))?;
        if *killed {
            return Ok(());
        }

        // SAFETY: `self.pgid` is the positive process group ID created via `process_group(0)`
        // for the spawned child, and sending `SIGKILL` has no memory-safety invariants.
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
