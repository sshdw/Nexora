use std::collections::HashSet;
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::Mutex;

use crate::sandbox::{
    collect_sanitized_env, ProbeSpec, SandboxPolicy, SandboxSpawnConfig, SandboxVerificationStatus,
};
use crate::windows::PlatformHandle;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::MutexGuard;
use windows_sys::Win32::Foundation::{
    LocalFree, SetHandleInformation, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, HLOCAL,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, REVOKE_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    CopySid, CreateRestrictedToken, FreeSid, GetTokenInformation, SetTokenInformation,
    TokenDefaultDacl, TokenGroups, ACL, DACL_SECURITY_INFORMATION, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_GROUPS,
    TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    PIPE_ACCESS_INBOUND, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

pub(crate) const MECHANISM_NAME: &str = "windows-lpac-restricted-job";

#[repr(C)]
struct DefaultDacl {
    dacl: *mut ACL,
}

const DISABLE_MAX_PRIVILEGE: u32 = 1;
// LPAC performs capability access checks in addition to the restricted-token pass.
// Filesystem confinement is AppContainer LPAC plus per-launch workspace capability ACLs.
const SYSTEM_READ_SID: &str = "S-1-1-0";
const STILL_ACTIVE_CODE: u32 = 259;
const WORKSPACE_ALLOW_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;
static WIN_SPAWN_LOCK: Mutex<()> = Mutex::new(());
static SANDBOX_LEASE: Mutex<()> = Mutex::new(());
struct ExclusiveLease(#[allow(dead_code)] MutexGuard<'static, ()>);
impl ExclusiveLease {
    fn acquire() -> Self {
        let guard = SANDBOX_LEASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self(guard)
    }
}
static CONTAINER_SEQ: AtomicU64 = AtomicU64::new(1);
static PIPE_SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) struct RestrictedChild {
    process: OwnedHandle,
    pid: u32,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    exit_status: Option<ExitStatus>,
}

impl RestrictedChild {
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::ExitStatusExt;

        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let raw = self.process.as_raw_handle().cast::<c_void>();
        // SAFETY: `raw` is a valid open process handle owned by `self.process`.
        let wait_res = unsafe { WaitForSingleObject(raw, 0) };
        if wait_res == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait_res != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: `raw` is signaled and valid; `code` is a valid out pointer.
        if unsafe { GetExitCodeProcess(raw, std::ptr::from_mut(&mut code)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let status = ExitStatus::from_raw(code);
        self.exit_status = Some(status);
        Ok(Some(status))
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::ExitStatusExt;
        use windows_sys::Win32::System::Threading::INFINITE;

        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let raw = self.process.as_raw_handle().cast::<c_void>();
        // SAFETY: `raw` is a valid open process handle owned by `self.process`.
        let wait_res = unsafe { WaitForSingleObject(raw, INFINITE) };
        if wait_res != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: `raw` is signaled and valid; `code` is a valid out pointer.
        if unsafe { GetExitCodeProcess(raw, std::ptr::from_mut(&mut code)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if code == STILL_ACTIVE_CODE {
            // Re-check once signaled; if a child genuinely returned 259, WaitForSingleObject
            // already confirmed termination.
        }
        let status = ExitStatus::from_raw(code);
        self.exit_status = Some(status);
        Ok(status)
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }
}

struct OwnedLocalSid {
    ptr: *mut c_void,
}

impl OwnedLocalSid {
    fn from_str(sid_str: &str) -> io::Result<Self> {
        let wide = to_wide(sid_str);
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated UTF-16 and `ptr` receives the allocated SID.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), std::ptr::from_mut(&mut ptr)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if ptr.is_null() {
            return Err(io::Error::other("ConvertStringSidToSidW returned null SID"));
        }
        Ok(Self { ptr })
    }

    fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for OwnedLocalSid {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `self.ptr` was allocated by `ConvertStringSidToSidW`.
            unsafe {
                LocalFree(self.ptr as HLOCAL);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}

struct AttributeList {
    buffer: Vec<u8>,
    initialized: bool,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size: usize = 0;
        // SAFETY: First call queries required byte size.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &raw mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; size];
        let ptr = buffer.as_mut_ptr().cast::<c_void>();
        // SAFETY: `buffer` has the exact size returned by `InitializeProcThreadAttributeList`.
        if unsafe { InitializeProcThreadAttributeList(ptr, count, 0, &raw mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            buffer,
            initialized: true,
        })
    }

    fn set_handle_list(&mut self, handles: &[HANDLE]) -> io::Result<()> {
        let size = std::mem::size_of_val(handles);
        // SAFETY: `self.as_mut_ptr()` is initialized and `handles` stays alive past `CreateProcessAsUserW`.
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast::<c_void>(),
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
        // SAFETY: initialized list; caller retains the value through process creation.
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute as usize,
                std::ptr::from_ref(value).cast(),
                std::mem::size_of::<T>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.buffer.as_mut_ptr().cast::<c_void>()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: `self.as_mut_ptr()` was initialized by `InitializeProcThreadAttributeList`.
            unsafe {
                DeleteProcThreadAttributeList(self.as_mut_ptr());
            }
            self.initialized = false;
        }
    }
}

#[allow(clippy::too_many_lines)] // Win32 suspended spawn keeps owned lifetimes visible through Job assignment.
pub(crate) fn spawn_sandboxed(
    command: &mut Command,
    config: &SandboxSpawnConfig,
) -> io::Result<(RestrictedChild, PlatformHandle, WindowsSandboxGuard)> {
    fn tag(step: &str, err: &io::Error) -> io::Error {
        io::Error::new(err.kind(), format!("windows sandbox {step} failed: {err}"))
    }

    let lease = ExclusiveLease::acquire();
    let policy = &config.policy;
    let workspace_canon =
        canonicalize_no_prefix(&policy.workspace_root).map_err(|e| tag("canonicalize", &e))?;
    let mut profile = ContainerProfile::new().map_err(|e| tag("container", &e))?;
    let system_sid = OwnedLocalSid::from_str(SYSTEM_READ_SID)?;
    let nonce = CONTAINER_SEQ.fetch_add(1, Ordering::Relaxed);
    let cap_sid = OwnedLocalSid::from_str(&format!(
        "S-1-5-21-1314411346-{}-{nonce}-1",
        std::process::id()
    ))?;
    // An AppContainer's own profile is an outside-workspace write exception in
    // Windows by default. Only boundary probes/tests create and receive those
    // generated paths, without ever mutating caller `command` or exposing them
    // to ordinary tool runs.
    let probe_storage = if command
        .get_envs()
        .any(|(key, _)| key == "NEXORA_PROBE_OUTSIDE" || key == "NX_TEST_PROBE_STORAGE")
    {
        Some(profile.storage_paths()?)
    } else {
        None
    };
    #[cfg(test)]
    let test_sids = if command.get_envs().any(|(key, _)| key == "NX_TEST_APP_SID") {
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        let mut text = std::ptr::null_mut();
        assert_ne!(
            unsafe { ConvertSidToStringSidW(profile.sid, &raw mut text) },
            0
        );
        let mut len = 0;
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe {
            LocalFree(text as HLOCAL);
        }
        Some((
            value,
            format!("S-1-5-21-1314411346-{}-{nonce}-1", std::process::id()),
        ))
    } else {
        None
    };
    let acl_guard =
        WorkspaceAclGuard::new(policy, &workspace_canon, &[profile.sid, cap_sid.as_ptr()])?;
    let base_token = open_current_token_for_restriction().map_err(|e| tag("open_token", &e))?;
    let restricted_token = create_restricted_token(
        base_token.as_raw_handle().cast(),
        cap_sid.as_ptr(),
        profile.sid,
        system_sid.as_ptr(),
    )
    .map_err(|e| tag("restricted_token", &e))?;

    let raw_job = create_sandbox_job().map_err(|e| tag("create_job", &e))?;
    let job_guard = PlatformHandle::from_sandbox_job(raw_job);
    let job = raw_job;

    // Serialize Win32 inheritable-handle creation + `CreateProcessAsUserW` across threads
    // so concurrent test/tool spawns cannot transiently inherit each other's pipe ends.
    let spawn_guard = WIN_SPAWN_LOCK
        .lock()
        .map_err(|_| io::Error::other("sandbox spawn lock poisoned"))?;

    let (stdin_read, stdin_write) = create_pipe_pair().map_err(|e| tag("stdin_pipe", &e))?;
    // Close the write side immediately so child stdin hits immediate EOF.
    drop(stdin_write);
    let (stdout_read, stdout_write) =
        create_output_pipe_pair().map_err(|e| tag("stdout_pipe", &e))?;
    let (stderr_read, stderr_write) =
        create_output_pipe_pair().map_err(|e| tag("stderr_pipe", &e))?;

    let child_stdin_raw = stdin_read.as_raw_handle().cast::<c_void>();
    let child_stdout_raw = stdout_write.as_raw_handle().cast::<c_void>();
    let child_stderr_raw = stderr_write.as_raw_handle().cast::<c_void>();

    for handle in [child_stdin_raw, child_stdout_raw, child_stderr_raw] {
        // SAFETY: Valid newly created pipe handles; enable inheritance for child only.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            // SAFETY: Close raw job handle on failure.
            return Err(io::Error::last_os_error());
        }
    }

    let mut inherited_handles = vec![child_stdin_raw];
    if !inherited_handles.contains(&child_stdout_raw) {
        inherited_handles.push(child_stdout_raw);
    }
    if !inherited_handles.contains(&child_stderr_raw) {
        inherited_handles.push(child_stderr_raw);
    }
    let mut attr_list = match AttributeList::new(3) {
        Ok(list) => list,
        Err(err) => {
            return Err(err);
        }
    };
    attr_list.set_handle_list(&inherited_handles)?;

    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    attr_list
        .set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)
        .map_err(|e| tag("security_capabilities", &e))?;
    // LPAC excludes implicit ALL APPLICATION PACKAGES filesystem grants.
    let opt_out = 1u32;
    attr_list
        .set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &opt_out,
        )
        .map_err(|e| tag("lpac_policy", &e))?;

    let cwd = command
        .get_current_dir()
        .and_then(|p| canonicalize_no_prefix(p).ok())
        .unwrap_or_else(|| workspace_canon.clone());
    let cwd_wide = to_wide_path(&cwd);
    let mut cmdline = build_command_line(command);
    let mut env_entries = collect_sanitized_env(command, policy.sanitize_env);
    for key in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP"] {
        env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        env_entries.push((key.into(), workspace_canon.to_string_lossy().into_owned()));
    }
    if let Some((ac_storage, local_state_storage)) = probe_storage {
        for (key, value) in [
            ("NX_SANDBOX_AC_STORAGE", ac_storage),
            ("NX_SANDBOX_STORAGE", local_state_storage),
        ] {
            env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
            env_entries.push((key.into(), value.to_string_lossy().into_owned()));
        }
    }
    #[cfg(test)]
    if let Some((app_sid, restrict_sid)) = test_sids {
        for (key, value) in [
            ("NX_TEST_APP_SID", app_sid),
            ("NX_TEST_RESTRICT_SID", restrict_sid),
        ] {
            env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
            env_entries.push((key.into(), value));
        }
    }
    let system_root = windows_cmd_path()
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new(r"C:\Windows"))
        .to_path_buf();
    env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case("SystemRoot"));
    env_entries.push((
        "SystemRoot".into(),
        system_root.to_string_lossy().into_owned(),
    ));
    let env_block = build_unicode_env_block(&env_entries);

    // SAFETY: `STARTUPINFOEXW` is a plain C struct; zero-init + `cb` set is valid.
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
        .map_err(|_| io::Error::other("STARTUPINFOEXW size overflow"))?;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = child_stdin_raw;
    si.StartupInfo.hStdOutput = child_stdout_raw;
    si.StartupInfo.hStdError = child_stderr_raw;
    si.lpAttributeList = attr_list.as_mut_ptr();

    // SAFETY: `PROCESS_INFORMATION` is a plain C struct; zero-init is valid.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_NO_WINDOW
        | CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | EXTENDED_STARTUPINFO_PRESENT;

    // SAFETY: All pointers (`restricted_token`, `cmdline`, `env_block`, `cwd_wide`, `si`, `pi`) remain valid for the duration of `CreateProcessAsUserW`.
    let created = unsafe {
        CreateProcessAsUserW(
            restricted_token.as_raw_handle().cast::<c_void>(),
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            flags,
            env_block.as_ptr().cast::<c_void>(),
            cwd_wide.as_ptr(),
            std::ptr::from_ref(&si.StartupInfo).cast::<STARTUPINFOW>(),
            std::ptr::from_mut(&mut pi),
        )
    };
    // Close parent copies of child pipe ends immediately so EOF propagates cleanly.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);

    if created == 0 {
        let err = tag("CreateProcessAsUserW", &io::Error::last_os_error());
        // SAFETY: `job` has not been wrapped in `PlatformHandle` yet.
        return Err(err);
    }

    // SAFETY: `pi.hProcess` and `pi.hThread` were returned by `CreateProcessAsUserW`.
    let proc_owned = unsafe { OwnedHandle::from_raw_handle(pi.hProcess.cast()) };
    // SAFETY: Close `hThread` handle cleanly after wrapping/resuming below? Keep handle to resume directly!
    let thread_owned = unsafe { OwnedHandle::from_raw_handle(pi.hThread.cast()) };

    // SAFETY: `job` and `pi.hProcess` are valid open handles; process is suspended.
    if unsafe { AssignProcessToJobObject(job, proc_owned.as_raw_handle().cast::<c_void>()) } == 0 {
        let err = io::Error::last_os_error();
        unsafe {
            let _ = TerminateProcess(proc_owned.as_raw_handle().cast(), 1);
        }
        return Err(err);
    }

    // SAFETY: exact suspended primary thread, already assigned to a non-breakaway job.
    if unsafe { ResumeThread(thread_owned.as_raw_handle().cast()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    drop(thread_owned);

    let child_stdout = ChildStdout::from(stdout_read);
    let child_stderr = ChildStderr::from(stderr_read);

    let handle = job_guard;
    drop(spawn_guard);
    Ok((
        RestrictedChild {
            process: proc_owned,
            pid: pi.dwProcessId,
            stdin: None,
            stdout: Some(child_stdout),
            stderr: Some(child_stderr),
            exit_status: None,
        },
        handle,
        WindowsSandboxGuard {
            acl: acl_guard,
            _profile: profile,
            _lease: lease,
        },
    ))
}

pub(crate) fn probe_mechanism_available() -> Result<&'static str, SandboxVerificationStatus> {
    let cmd_path = windows_cmd_path();
    if !cmd_path.is_file() {
        return Err(SandboxVerificationStatus::Unavailable {
            reason: format!("Windows shell not found at {}", cmd_path.display()),
        });
    }
    Ok(MECHANISM_NAME)
}

pub(crate) fn build_probe_command(spec: &ProbeSpec<'_>) -> Command {
    let mut cmd = Command::new(windows_cmd_path());
    cmd.env_clear();
    let script = "(echo escaped>\"%NEXORA_PROBE_OUTSIDE%\") 2>NUL & if exist \"%NEXORA_PROBE_OUTSIDE%\" (echo OUT_WRITE_ALLOWED) else (echo OUT_WRITE_BLOCKED) & (type \"%NEXORA_PROBE_PRIVATE%\" 2>NUL && echo PRIV_READ_ALLOWED || echo PRIV_READ_BLOCKED) & echo NEXORA_WS_CANARY>\"%NEXORA_PROBE_IN_WS%\" & if exist \"%NEXORA_PROBE_IN_WS%\" (type \"%NEXORA_PROBE_IN_WS%\" & echo IN_WS_OK) & (echo escaped>\"%NX_SANDBOX_STORAGE%\\nexora-own-storage-probe.txt\") 2>NUL & if exist \"%NX_SANDBOX_STORAGE%\\nexora-own-storage-probe.txt\" (echo OWN_WRITE_ALLOWED) else (echo OWN_WRITE_BLOCKED) & (echo escaped>\"%NX_SANDBOX_AC_STORAGE%\\nexora-own-ac-probe.txt\") 2>NUL & if exist \"%NX_SANDBOX_AC_STORAGE%\\nexora-own-ac-probe.txt\" (echo OWN_WRITE_ALLOWED) else (echo OWN_WRITE_BLOCKED) & exit /b 0";
    cmd.args(["/D", "/C", script]);
    cmd.current_dir(spec.workspace_root);
    cmd.env(
        "SystemRoot",
        windows_cmd_path()
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new(r"C:\Windows")),
    );
    cmd.env("NEXORA_PROBE_OUTSIDE", spec.outside_file);
    cmd.env("NEXORA_PROBE_PRIVATE", spec.private_file);
    cmd.env("NEXORA_PROBE_IN_WS", spec.in_workspace_file);
    cmd
}

pub(crate) fn windows_cmd_path() -> PathBuf {
    // Resolve through the OS, never PATH, COMSPEC or an inherited SystemRoot override.
    let mut buffer = vec![0u16; 32768];
    let len = unsafe {
        windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            32768,
        )
    };
    if len == 0 || len as usize >= buffer.len() {
        return PathBuf::new();
    }
    PathBuf::from(OsString::from_wide(&buffer[..len as usize])).join("cmd.exe")
}

fn create_pipe_pair() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read_h: HANDLE = std::ptr::null_mut();
    let mut write_h: HANDLE = std::ptr::null_mut();
    // SAFETY: Non-inheritable anonymous pipe created safely with null security attributes.
    if unsafe { CreatePipe(&raw mut read_h, &raw mut write_h, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `read_h` and `write_h` were newly created by `CreatePipe`.
    unsafe {
        Ok((
            OwnedHandle::from_raw_handle(read_h.cast()),
            OwnedHandle::from_raw_handle(write_h.cast()),
        ))
    }
}

/// Creates a pipe pair matching Rust `std::process::Command`'s `AnonPipe` contract:
/// parent read end opened with `FILE_FLAG_OVERLAPPED` (required by `ChildStdout`/`ChildStderr`)
/// and child write end opened synchronously (required by console tools calling `WriteFile`).
fn create_output_pipe_pair() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let seq = PIPE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let name = format!(
        r"\\.\pipe\nexora-sandbox-pipe-{}-{stamp}-{seq}",
        std::process::id()
    );
    let name_wide = to_wide(&name);
    let open_mode = PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE;
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name_wide` is a valid NUL-terminated UTF-16 pipe name; null security attributes
    // keep the parent read end non-inheritable.
    let read_h = unsafe {
        CreateNamedPipeW(
            name_wide.as_ptr(),
            open_mode,
            pipe_mode,
            1,
            65536,
            65536,
            0,
            std::ptr::null(),
        )
    };
    if read_h.is_null() || read_h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `read_h` is a newly created pipe server handle.
    let read_owned = unsafe { OwnedHandle::from_raw_handle(read_h.cast()) };
    let write_file = OpenOptions::new().write(true).open(&name)?;
    Ok((read_owned, OwnedHandle::from(write_file)))
}

fn open_current_token_for_restriction() -> io::Result<OwnedHandle> {
    let mut token = std::ptr::null_mut();
    // SAFETY: current process pseudo-handle is valid and token is an output pointer.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            &raw mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: API returned owned token handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(token.cast()) })
}

fn logon_sid(token: HANDLE) -> io::Result<Vec<u64>> {
    let mut size = 0;
    unsafe {
        GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &raw mut size);
    }
    let mut storage = vec![0u64; (size as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token,
            TokenGroups,
            storage.as_mut_ptr().cast(),
            size,
            &raw mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let groups = storage.as_ptr().cast::<TOKEN_GROUPS>();
    let count = unsafe { (*groups).GroupCount } as usize;
    let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    if offset + count * std::mem::size_of::<SID_AND_ATTRIBUTES>() > size as usize {
        return Err(io::Error::other("invalid token groups"));
    }
    let entries = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*groups).Groups).cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    for entry in entries {
        if entry.Attributes & 0xc000_0000 == 0xc000_0000 {
            let mut sid = vec![0u64; 9];
            if unsafe { CopySid(72, sid.as_mut_ptr().cast(), entry.Sid) } != 0 {
                return Ok(sid);
            }
        }
    }
    Err(io::Error::other("token logon SID unavailable"))
}

fn create_restricted_token(
    base: HANDLE,
    cap_sid: *mut c_void,
    app_sid: *mut c_void,
    system_sid: *mut c_void,
) -> io::Result<OwnedHandle> {
    let mut logon_sid = logon_sid(base)?;
    let logon_ptr = logon_sid.as_mut_ptr().cast();
    let restricting = [
        SID_AND_ATTRIBUTES {
            Sid: cap_sid,
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: system_sid,
            Attributes: 0,
        },
        SID_AND_ATTRIBUTES {
            Sid: logon_ptr,
            Attributes: 0,
        },
    ];
    let mut token = std::ptr::null_mut();
    // SAFETY: token and SID pointers are owned by the caller through this call.
    if unsafe {
        CreateRestrictedToken(
            base,
            DISABLE_MAX_PRIVILEGE,
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            3,
            restricting.as_ptr(),
            &raw mut token,
        )
    } == 0
    {
        return Err(io::Error::other(format!(
            "restrict: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: successful API returned a unique owned handle.
    let owned = unsafe { OwnedHandle::from_raw_handle(token.cast()) };
    let entries = [
        explicit_access(cap_sid, 0x1000_0000, GRANT_ACCESS, 0),
        explicit_access(app_sid, 0x1000_0000, GRANT_ACCESS, 0),
        explicit_access(logon_ptr, 0x1000_0000, GRANT_ACCESS, 0),
    ];
    let mut dacl = std::ptr::null_mut();
    // SAFETY: SID and token are live, returned ACL is released after the kernel copies it.
    let result = unsafe { SetEntriesInAclW(3, entries.as_ptr(), std::ptr::null(), &raw mut dacl) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::other("sandbox default ACL unavailable"));
    }
    let info = DefaultDacl { dacl };
    let result = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            std::ptr::from_ref(&info).cast(),
            u32::try_from(std::mem::size_of::<DefaultDacl>()).unwrap_or(0),
        )
    };
    unsafe {
        LocalFree(dacl as HLOCAL);
    }
    if result == 0 {
        return Err(io::Error::other(format!(
            "default_dacl: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(owned)
}

fn create_sandbox_job() -> io::Result<HANDLE> {
    // SAFETY: Anonymous Job Object.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C struct.
    let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    ext.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let ext_len = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("JOBOBJECT_EXTENDED_LIMIT_INFORMATION overflow"))?;
    // SAFETY: Valid `job` and initialized `ext`.
    if unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&ext).cast::<c_void>(),
            ext_len,
        )
    } == 0
    {
        let err = io::Error::last_os_error();
        return Err(err);
    }

    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    let ui_len = u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>())
        .map_err(|_| io::Error::other("JOBOBJECT_BASIC_UI_RESTRICTIONS overflow"))?;
    // SAFETY: Valid `job` and initialized `ui`.
    if unsafe {
        SetInformationJobObject(
            job,
            JobObjectBasicUIRestrictions,
            std::ptr::from_ref(&ui).cast::<c_void>(),
            ui_len,
        )
    } == 0
    {
        let err = io::Error::last_os_error();
        return Err(err);
    }

    Ok(job)
}

// The unique profile and grants live until the entire child tree is terminated.
// Never add grants or denies to private user/system directories.
pub(crate) struct WindowsSandboxGuard {
    acl: WorkspaceAclGuard,
    _profile: ContainerProfile,
    _lease: ExclusiveLease,
}
// SAFETY: fields are exclusively owned; raw SID memory is immutable, freed after ACL cleanup.
unsafe impl Send for WindowsSandboxGuard {}
// SAFETY: shared references do not mutate the SID or workspace handles.
unsafe impl Sync for WindowsSandboxGuard {}
impl Drop for WindowsSandboxGuard {
    fn drop(&mut self) {
        self.acl.revoke();
    }
}
struct ContainerProfile {
    name: Vec<u16>,
    sid: *mut c_void,
    cleanup_root: Option<PathBuf>,
}
impl ContainerProfile {
    fn storage_paths(&mut self) -> io::Result<(PathBuf, PathBuf)> {
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::Isolation::GetAppContainerFolderPath;
        let mut text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid, &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut path = std::ptr::null_mut();
        let result = unsafe { GetAppContainerFolderPath(text, &raw mut path) };
        unsafe {
            LocalFree(text as HLOCAL);
        }
        if result < 0 || path.is_null() {
            return Err(io::Error::other("container storage path unavailable"));
        }
        let mut len = 0;
        while unsafe { *path.add(len) } != 0 {
            len += 1;
        }
        let ac_storage = PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(path, len)
        }));
        unsafe {
            windows_sys::Win32::System::Com::CoTaskMemFree(path.cast());
        }
        let package_root = ac_storage
            .parent()
            .ok_or_else(|| io::Error::other("container package root unavailable"))?
            .to_path_buf();
        let local_state = package_root.join("LocalState");
        // This profile was uniquely reserved by CreateAppContainerProfile above.
        std::fs::create_dir_all(&ac_storage)?;
        std::fs::create_dir_all(&local_state)?;
        self.cleanup_root = Some(package_root);
        Ok((ac_storage, local_state))
    }
    fn new() -> io::Result<Self> {
        let seq = CONTAINER_SEQ.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let name = to_wide(&format!(
            "Nexora.Sandbox.{}.{stamp}.{seq}",
            std::process::id()
        ));
        let mut sid = std::ptr::null_mut();
        // SAFETY: unique NUL-terminated profile name, no network or user-file capabilities.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                std::ptr::null(),
                0,
                &raw mut sid,
            )
        };
        if result < 0 {
            return Err(io::Error::other("AppContainer profile unavailable"));
        }
        Ok(Self {
            name,
            sid,
            cleanup_root: None,
        })
    }
}
impl Drop for ContainerProfile {
    fn drop(&mut self) {
        // SAFETY: no live process remains when ProcessTreeChild releases this guard.
        unsafe {
            DeleteAppContainerProfile(self.name.as_ptr());
            FreeSid(self.sid);
        }
        if let Some(root) = &self.cleanup_root {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
struct WorkspaceAclGuard {
    files: Vec<(File, u32)>,
    sids: Vec<Vec<u64>>,
}
impl WorkspaceAclGuard {
    fn new(policy: &SandboxPolicy, root: &Path, sids: &[*mut c_void]) -> io::Result<Self> {
        let mut owned_sids = Vec::new();
        for sid in sids {
            let mut copy = vec![0u64; 9];
            if unsafe { CopySid(72, copy.as_mut_ptr().cast(), *sid) } == 0 {
                return Err(io::Error::last_os_error());
            }
            owned_sids.push(copy);
        }
        let mut guard = Self {
            files: Vec::new(),
            sids: owned_sids,
        };
        guard.grant_tree(policy, root)?;
        Ok(guard)
    }
    fn grant_tree(&mut self, policy: &SandboxPolicy, root: &Path) -> io::Result<()> {
        let deny_read_prefixes = super::sandbox::canonical_prefixes(&policy.deny_read_paths);
        let deny_write_prefixes = super::sandbox::canonical_prefixes(&policy.deny_write_paths);
        let mut stack = vec![root.to_path_buf()];
        // Non-inheriting per-object grants avoid recursive Win32 propagation through junctions.
        // Writable handles target existing ordinary files only. Directories stay read-only,
        // so absent private/policy names and parent rename/delete operations remain blocked.
        while let Some(path) = stack.pop() {
            if self.files.len() >= 100_000 {
                return Err(io::Error::other(
                    "workspace exceeds sandbox validation bound",
                ));
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other("workspace contains a reparse point"));
            }
            let file = OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
                .map_err(|e| io::Error::other(format!("workspace handle: {e}")))?;
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: owned file handle and initialized output structure.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            let is_dir = info.dwFileAttributes & 0x10 != 0;
            if info.dwFileAttributes & 0x400 != 0
                || is_dir != metadata.is_dir()
                || (!is_dir && info.nNumberOfLinks != 1)
            {
                return Err(io::Error::other("workspace contains a filesystem alias"));
            }
            let canonical = super::sandbox::canonicalize_clean(&path)?;
            let denied_read = super::sandbox::private_store_path(&canonical)
                || deny_read_prefixes.iter().any(|p| canonical.starts_with(p));
            let denied_write = deny_write_prefixes.iter().any(|p| canonical.starts_with(p));
            let mask = if denied_read {
                0
            } else if denied_write
                || metadata.is_dir()
                || super::sandbox::sensitive_write_path(&canonical)
            {
                FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
            } else {
                WORKSPACE_ALLOW_MASK
            };
            self.files.push((file, mask));
            if metadata.is_dir() && !denied_read {
                for entry in std::fs::read_dir(&path)? {
                    stack.push(entry?.path());
                }
            }
        }
        for (file, mask) in &self.files {
            if *mask != 0 {
                for sid in &self.sids {
                    update_file_ace(file, sid.as_ptr().cast_mut().cast(), *mask, GRANT_ACCESS)?;
                }
            }
        }
        Ok(())
    }
    fn revoke(&mut self) {
        // Retained non-delete-shared handles prevent rename/reparse races and allow
        // SID-specific removal without restoring stale snapshots over user ACL edits.
        for (file, _) in self.files.drain(..).rev() {
            for sid in &self.sids {
                let _ = update_file_ace(&file, sid.as_ptr().cast_mut().cast(), 0, REVOKE_ACCESS);
            }
        }
    }
}
impl Drop for WorkspaceAclGuard {
    fn drop(&mut self) {
        if !self.files.is_empty() {
            self.revoke();
        }
    }
}
fn update_file_ace(file: &File, sid: *mut c_void, mask: u32, mode: i32) -> io::Result<()> {
    let mut dacl = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    // SAFETY: retained file handle has READ_CONTROL and WRITE_DAC.
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(result).unwrap_or(5),
        ));
    }
    let inheritance = 0;
    let entry = explicit_access(sid, mask, mode, inheritance);
    let mut new_dacl = std::ptr::null_mut();
    // SAFETY: ACL and SID pointers are valid until their explicit frees below.
    let result = unsafe { SetEntriesInAclW(1, &raw const entry, dacl, &raw mut new_dacl) };
    let result = if result == ERROR_SUCCESS {
        unsafe {
            SetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null_mut(),
            )
        }
    } else {
        result
    };
    unsafe {
        if !new_dacl.is_null() {
            LocalFree(new_dacl as HLOCAL);
        }
        LocalFree(sd as HLOCAL);
    }
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(result).unwrap_or(5),
        ))
    }
}

fn explicit_access(
    psid: *mut c_void,
    mask: u32,
    access_mode: i32,
    inheritance: u32,
) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: access_mode,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: psid.cast::<u16>(),
        },
    }
}

fn canonicalize_no_prefix(path: &Path) -> io::Result<PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    let raw = canon.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        Ok(PathBuf::from(format!(r"\\{stripped}")))
    } else if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        Ok(PathBuf::from(stripped))
    } else {
        Ok(canon)
    }
}

fn build_command_line(command: &Command) -> Vec<u16> {
    if Path::new(command.get_program())
        .file_name()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("cmd.exe"))
    {
        let args: Vec<_> = command.get_args().collect();
        if let Some(index) = args
            .iter()
            .position(|arg| arg.to_string_lossy().eq_ignore_ascii_case("/C"))
        {
            if index + 2 == args.len() {
                let mut line = quote_windows_arg(&command.get_program().to_string_lossy());
                line.push_str(" /D /S /C \"");
                line.push_str(&args[index + 1].to_string_lossy());
                line.push('"');
                return to_wide(&line);
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    parts.push(command.get_program().to_string_lossy().into_owned());
    for arg in command.get_args() {
        parts.push(arg.to_string_lossy().into_owned());
    }
    let joined = parts
        .iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    to_wide(&joined)
}

fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .bytes()
            .any(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'"'))
    {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    out.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                }
                out.push(ch);
            }
        }
    }
    if backslashes > 0 {
        out.extend(std::iter::repeat_n('\\', backslashes * 2));
    }
    out.push('"');
    out
}

fn build_unicode_env_block(entries: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<(&str, &str)> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    sorted.sort_by(|a, b| {
        a.0.to_ascii_uppercase()
            .cmp(&b.0.to_ascii_uppercase())
            .then_with(|| a.0.cmp(b.0))
    });
    let mut seen: HashSet<String> = HashSet::with_capacity(sorted.len());
    let mut block = Vec::new();
    for (k, v) in sorted {
        if !seen.insert(k.to_ascii_uppercase()) {
            continue;
        }
        let line = format!("{k}={v}");
        block.extend(OsStr::new(&line).encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    block
}

fn to_wide(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = OsStr::new(s).encode_wide().collect();
    out.push(0);
    out
}

fn to_wide_path(path: &Path) -> Vec<u16> {
    let mut out: Vec<u16> = path.as_os_str().encode_wide().collect();
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn acl_snapshot(path: &Path, label: bool) -> Vec<Vec<u8>> {
        use windows_sys::Win32::Security::{GetAce, ACE_HEADER, LABEL_SECURITY_INFORMATION};
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        let mut acl = std::ptr::null_mut();
        let mut sd = std::ptr::null_mut();
        let flags = if label {
            LABEL_SECURITY_INFORMATION
        } else {
            DACL_SECURITY_INFORMATION
        };
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    file.as_raw_handle().cast(),
                    SE_FILE_OBJECT,
                    flags,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    if label {
                        std::ptr::null_mut()
                    } else {
                        &raw mut acl
                    },
                    if label {
                        &raw mut acl
                    } else {
                        std::ptr::null_mut()
                    },
                    &raw mut sd,
                )
            },
            0
        );
        let mut result = Vec::new();
        if !acl.is_null() {
            for index in 0..u32::from(unsafe { (*acl).AceCount }) {
                let mut ace = std::ptr::null_mut();
                assert_ne!(unsafe { GetAce(acl, index, &raw mut ace) }, 0);
                let size = usize::from(unsafe { (*ace.cast::<ACE_HEADER>()).AceSize });
                result.push(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) }.to_vec());
            }
        }
        unsafe {
            LocalFree(sd as HLOCAL);
        }
        result.sort();
        result
    }
    #[test]
    fn native_namespace_and_owner_boundary() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = crate::test_workspace_parent().join(format!(
            "nexora-native-namespace-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let _fixture = Fixture(root.clone());
        std::fs::write(root.join("ordinary.txt"), "initial").unwrap();
        std::fs::write(root.join(".env"), "synthetic-protected").unwrap();
        let mut command = Command::new(windows_cmd_path());
        let script = "%SystemRoot%\\System32\\icacls.exe . /grant \"*%NX_TEST_APP_SID%:(OI)(CI)F\" \"*%NX_TEST_RESTRICT_SID%:(OI)(CI)F\" /q >NUL 2>NUL & echo attack>.env.new 2>NUL & echo changed>.env 2>NUL & mkdir .nexora 2>NUL & ren ordinary.txt .env.new 2>NUL & del .env 2>NUL & echo updated>ordinary.txt & echo DONE";
        command
            .current_dir(&root)
            .args(["/D", "/C", script])
            .env("NX_TEST_APP_SID", "self");
        let paths = [&root, &root.join("ordinary.txt"), &root.join(".env")];
        let before: Vec<_> = paths
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();
        let config = SandboxSpawnConfig::for_workspace(&root);
        let result = crate::spawn_sandboxed_in_process_tree(&mut command, &config);
        let mut child = match result {
            Ok(child) => child,
            Err(error) => {
                assert!(
                    std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_none(),
                    "required native namespace launch failed: {error}"
                );
                eprintln!("native namespace proof unavailable: {error}");
                return;
            }
        };
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > std::time::Duration::from_secs(10) {
                child.kill_tree().unwrap();
                panic!("native namespace child timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "native namespace probe rejected: {status}"
        );
        drop(child);
        let after: Vec<_> = paths
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();
        assert_eq!(
            before, after,
            "sandbox must restore all synthetic ACLs and integrity labels"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ordinary.txt"))
                .unwrap()
                .trim(),
            "updated"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "synthetic-protected"
        );
        assert!(!root.join(".env.new").exists());
        assert!(!root.join(".nexora").exists());
    }

    fn run_sandboxed_capture(
        command: &mut Command,
        config: &SandboxSpawnConfig,
    ) -> Option<(std::process::ExitStatus, String)> {
        let mut child = match crate::spawn_sandboxed_in_process_tree(command, config) {
            Ok(child) => child,
            Err(error) => {
                assert!(
                    std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_none(),
                    "required native sandbox launch failed: {error}"
                );
                return None;
            }
        };
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = captured.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let out_pipe = child.take_stdout();
        std::thread::spawn(move || {
            if let Some(mut out) = out_pipe {
                let mut buf = [0u8; 1024];
                while let Ok(count) = std::io::Read::read(&mut out, &mut buf) {
                    if count == 0 {
                        break;
                    }
                    let mut guard = output.lock().unwrap();
                    guard.extend_from_slice(&buf[..count]);
                }
            }
            let _ = tx.send(());
        });
        let err_pipe = child.take_stderr();
        std::thread::spawn(move || {
            if let Some(mut err) = err_pipe {
                let _ = std::io::copy(&mut err, &mut std::io::sink());
            }
        });
        let start = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > std::time::Duration::from_secs(10) {
                let _ = child.kill_tree();
                panic!("native sandbox test child timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let _ = child.kill_tree();
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("stdout reader did not finish after child exit");
        let stdout = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
        Some((status, stdout))
    }

    #[test]
    fn appcontainer_own_ac_and_localstate_writes_are_blocked_and_unexposed_normally() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-ac-{stamp}"));
        std::fs::create_dir(&ws).unwrap();
        let _guard = Fixture(ws.clone());

        let mut normal = Command::new(windows_cmd_path());
        normal
            .current_dir(&ws)
            .args(["/D", "/C", "set NX_SANDBOX 2>NUL & exit /b 0"])
            .env("SYSTEMROOT", r"C:\InvalidFakeWindowsRoot");
        let config = SandboxSpawnConfig::for_workspace(&ws);
        let Some((status, normal_out)) = run_sandboxed_capture(&mut normal, &config) else {
            return;
        };
        assert!(status.success());
        assert!(!normal_out.contains("NX_SANDBOX_STORAGE="));
        assert!(!normal_out.contains("NX_SANDBOX_AC_STORAGE="));
        assert!(normal
            .get_envs()
            .all(|(k, _)| k != "NX_SANDBOX_STORAGE" && k != "NX_SANDBOX_AC_STORAGE"));

        let mut check_root = Command::new(windows_cmd_path());
        check_root
            .current_dir(&ws)
            .args(["/D", "/C", "echo SYSROOT=%SystemRoot%"])
            .env("SYSTEMROOT", r"C:\InvalidFakeWindowsRoot");
        let (_, sysroot_out) = run_sandboxed_capture(&mut check_root, &config).unwrap();
        assert!(!sysroot_out.contains("InvalidFakeWindowsRoot"));

        let mut probe_storage = Command::new(windows_cmd_path());
        probe_storage
            .current_dir(&ws)
            .env("NX_TEST_PROBE_STORAGE", "1")
            .args([
                "/D",
                "/C",
                "(echo x>\"%NX_SANDBOX_STORAGE%\\ls.txt\") 2>NUL & if exist \"%NX_SANDBOX_STORAGE%\\ls.txt\" (echo LS_ALLOWED) else (echo LS_BLOCKED) & (echo x>\"%NX_SANDBOX_AC_STORAGE%\\ac.txt\") 2>NUL & if exist \"%NX_SANDBOX_AC_STORAGE%\\ac.txt\" (echo AC_ALLOWED) else (echo AC_BLOCKED) & exit /b 0",
            ]);
        let (probe_status, probe_out) = run_sandboxed_capture(&mut probe_storage, &config).unwrap();
        assert!(probe_status.success());
        assert!(probe_out.contains("LS_BLOCKED") && !probe_out.contains("LS_ALLOWED"));
        assert!(probe_out.contains("AC_BLOCKED") && !probe_out.contains("AC_ALLOWED"));
    }

    #[test]
    fn lpac_blocks_permissive_everyone_inherited_acl_on_private_and_outside_dirs() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-lpac-ws-{stamp}"));
        let priv_dir = crate::test_workspace_parent().join(format!("nx-win-lpac-priv-{stamp}"));
        let out_dir = crate::test_workspace_parent().join(format!("nx-win-lpac-out-{stamp}"));
        std::fs::create_dir(&ws).unwrap();
        std::fs::create_dir(&priv_dir).unwrap();
        std::fs::create_dir(&out_dir).unwrap();
        let _g1 = Fixture(ws.clone());
        let _g2 = Fixture(priv_dir.clone());
        let _g3 = Fixture(out_dir.clone());
        let priv_file = priv_dir.join("secret.txt");
        let restricted_ws_dir = ws.join("for human do not open tis");
        std::fs::create_dir(&restricted_ws_dir).unwrap();
        let restricted_ws_file = restricted_ws_dir.join("secret.txt");
        std::fs::write(&priv_file, "LPAC_EVERYONE_CANARY_1").unwrap();
        std::fs::write(&restricted_ws_file, "LPAC_EVERYONE_CANARY_2").unwrap();

        for dir in [&priv_dir, &out_dir, &restricted_ws_dir] {
            let status = Command::new("icacls")
                .arg(dir)
                .args(["/grant", "*S-1-1-0:(OI)(CI)F", "/Q"])
                .status()
                .expect("icacls available");
            assert!(status.success());
        }

        let mut policy = SandboxPolicy::for_workspace(&ws);
        policy.deny_read_paths.push(priv_dir.clone());
        let config = SandboxSpawnConfig { policy };
        let out_target = out_dir.join("escape.txt");
        let mut cmd = Command::new(windows_cmd_path());
        cmd.current_dir(&ws)
            .env("P1", &priv_file)
            .env("P2", &restricted_ws_file)
            .env("POUT", &out_target)
            .args([
                "/D",
                "/C",
                "type \"%P1%\" 2>NUL & type \"%P2%\" 2>NUL & (echo escaped>\"%POUT%\") 2>NUL & echo DONE",
            ]);
        let Some((_, stdout)) = run_sandboxed_capture(&mut cmd, &config) else {
            return;
        };
        assert!(stdout.contains("DONE"));
        assert!(!stdout.contains("LPAC_EVERYONE_CANARY_1"));
        assert!(!stdout.contains("LPAC_EVERYONE_CANARY_2"));
        assert!(!out_target.exists());
    }

    #[test]
    fn hardlink_and_junction_aliases_are_rejected_and_outside_target_stays_untouched() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-alias-ws-{stamp}"));
        let out_dir = crate::test_workspace_parent().join(format!("nx-win-alias-out-{stamp}"));
        std::fs::create_dir(&ws).unwrap();
        std::fs::create_dir(&out_dir).unwrap();
        let _g1 = Fixture(ws.clone());
        let _g2 = Fixture(out_dir.clone());
        let outside_file = out_dir.join("target.txt");
        std::fs::write(&outside_file, "untouched").unwrap();
        let ws_hardlink = ws.join("hardlink.txt");
        std::fs::hard_link(&outside_file, &ws_hardlink).unwrap();

        let mut cmd = Command::new(windows_cmd_path());
        cmd.current_dir(&ws)
            .args(["/D", "/C", "echo hacked>hardlink.txt"]);
        let err = crate::spawn_sandboxed_in_process_tree(
            &mut cmd,
            &SandboxSpawnConfig::for_workspace(&ws),
        )
        .unwrap_err();
        if err.to_string().contains("AppContainer profile unavailable") {
            assert!(
                std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_none(),
                "required native sandbox alias check unavailable: {err}"
            );
            assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "untouched");
            return;
        }
        assert!(err.to_string().contains("alias"), "unexpected error: {err}");
        assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "untouched");
        std::fs::remove_file(&ws_hardlink).unwrap();

        let ws_junc = ws.join("junc");
        let mklink = Command::new(windows_cmd_path())
            .args([
                "/D",
                "/C",
                "mklink",
                "/J",
                &ws_junc.to_string_lossy(),
                &out_dir.to_string_lossy(),
            ])
            .status()
            .unwrap();
        assert!(mklink.success());
        let mut cmd2 = Command::new(windows_cmd_path());
        cmd2.current_dir(&ws).args(["/D", "/C", "echo DONE"]);
        let err2 = crate::spawn_sandboxed_in_process_tree(
            &mut cmd2,
            &SandboxSpawnConfig::for_workspace(&ws),
        )
        .unwrap_err();
        assert!(
            err2.to_string().contains("reparse") || err2.to_string().contains("alias"),
            "unexpected error: {err2}"
        );
    }

    #[test]
    fn missing_protected_names_and_parent_rename_delete_are_blocked() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-prot-{stamp}"));
        let src = ws.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let _guard = Fixture(ws.clone());
        let existing = src.join("existing.txt");
        std::fs::write(&existing, "before").unwrap();

        let mut cmd = Command::new(windows_cmd_path());
        cmd.current_dir(&ws).args([
            "/D",
            "/C",
            "echo x>.env 2>NUL & mkdir .git 2>NUL & mkdir .nexora 2>NUL & echo x>src\\new.txt 2>NUL & ren src src2 2>NUL & rmdir /s /q src 2>NUL & ren src\\existing.txt moved.txt 2>NUL & del /q src\\existing.txt 2>NUL & echo after>src\\existing.txt & exit /b 0",
        ]);
        let Some((status, _)) =
            run_sandboxed_capture(&mut cmd, &SandboxSpawnConfig::for_workspace(&ws))
        else {
            return;
        };
        assert!(status.success());
        assert!(!ws.join(".env").exists());
        assert!(!ws.join(".git").exists());
        assert!(!ws.join(".nexora").exists());
        assert!(!ws.join("src2").exists());
        assert!(!src.join("new.txt").exists());
        assert!(!src.join("moved.txt").exists());
        assert!(src.is_dir());
        assert_eq!(std::fs::read_to_string(&existing).unwrap().trim(), "after");
    }
}
