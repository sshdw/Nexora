use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Command};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

struct RawJob(HANDLE);

// SAFETY: Windows Job Object kernel handles can be operated on and closed from
// any thread; all access is serialized through the enclosing `Mutex`.
unsafe impl Send for RawJob {}
// SAFETY: Windows Job Object kernel handles are thread-safe when synchronized by `Mutex`.
unsafe impl Sync for RawJob {}

pub(crate) struct PlatformHandle {
    job: Mutex<Option<RawJob>>,
}

impl PlatformHandle {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        let job = create_kill_on_close_job()?;

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                // SAFETY: `job` is a valid open handle created above and has not been closed yet.
                unsafe {
                    CloseHandle(job);
                }
                return Err(err);
            }
        };

        let proc_handle: HANDLE = child.as_raw_handle().cast::<c_void>();
        // SAFETY: `job` is a valid Job Object handle and `proc_handle` is a valid process handle owned by `child`.
        let assigned = unsafe { AssignProcessToJobObject(job, proc_handle) };
        if assigned == 0 {
            let assign_err = io::Error::last_os_error();
            // If a short-lived child already terminated before assignment, there is
            // no running process to manage; close the unused Job Object cleanly.
            if matches!(child.try_wait(), Ok(Some(_))) {
                // SAFETY: `job` is a valid handle and is discarded cleanly here.
                unsafe {
                    CloseHandle(job);
                }
                return Ok((
                    child,
                    Self {
                        job: Mutex::new(None),
                    },
                ));
            }
            let _ = child.kill();
            let _ = child.wait();
            // SAFETY: `job` is a valid handle and has not been closed yet.
            unsafe {
                CloseHandle(job);
            }
            return Err(assign_err);
        }

        Ok((
            child,
            Self {
                job: Mutex::new(Some(RawJob(job))),
            },
        ))
    }

    pub(crate) fn kill_tree(&self) -> io::Result<()> {
        let maybe_job = {
            let mut guard = self
                .job
                .lock()
                .map_err(|_| io::Error::other("process-tree job mutex poisoned"))?;
            guard.take()
        };

        let Some(job) = maybe_job else {
            return Ok(());
        };

        // SAFETY: `job.0` is an open Job Object handle taken exclusively from the mutex.
        let terminated = unsafe { TerminateJobObject(job.0, 1) };
        let term_err = if terminated == 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };

        // SAFETY: `job.0` must always be closed once `TerminateJobObject` completes.
        let closed = unsafe { CloseHandle(job.0) };
        let close_err = if closed == 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };

        if let Some(err) = term_err {
            return Err(err);
        }
        if let Some(err) = close_err {
            return Err(err);
        }
        Ok(())
    }
}

impl Drop for PlatformHandle {
    fn drop(&mut self) {
        let _ = self.kill_tree();
    }
}

fn create_kill_on_close_job() -> io::Result<HANDLE> {
    // SAFETY: Null security attributes and null name create an anonymous Job Object.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C struct where zero-initialization is valid.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    let info_len = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("job object info struct exceeds u32 size"))?;

    // SAFETY: `job` is valid and `info` points to a properly sized `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast::<c_void>(),
            info_len,
        )
    };
    if ok == 0 {
        let err = io::Error::last_os_error();
        // SAFETY: `job` is an open handle created above.
        unsafe {
            CloseHandle(job);
        }
        return Err(err);
    }

    Ok(job)
}

#[cfg(test)]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE,
    };

    const STILL_ACTIVE_CODE: u32 = 259;

    // SAFETY: Standard read-only `OpenProcess` query against a numeric PID; handle is always closed.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return false;
    }

    let mut exit_code: u32 = 0;
    // SAFETY: `handle` is open and valid, and `exit_code` is a valid out-pointer.
    let got_code = unsafe { GetExitCodeProcess(handle, std::ptr::from_mut(&mut exit_code)) };
    // SAFETY: `handle` is open and valid with `PROCESS_SYNCHRONIZE`.
    let wait_res = unsafe { WaitForSingleObject(handle, 0) };
    // SAFETY: Always close the temporary handle opened in this helper.
    unsafe {
        CloseHandle(handle);
    }

    got_code != 0 && exit_code == STILL_ACTIVE_CODE && wait_res == WAIT_TIMEOUT
}
