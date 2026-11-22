use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::sandbox::{
    collect_sanitized_env, is_writable_build_dir, is_writable_build_dir_root, ProbeSpec,
    SandboxPolicy, SandboxSpawnConfig, SandboxVerificationStatus,
};
use crate::windows::PlatformHandle;
use windows_sys::Win32::Foundation::{
    LocalFree, SetHandleInformation, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, HLOCAL,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, DENY_ACCESS,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    CopySid, CreateRestrictedToken, DeleteAce, EqualSid, FreeSid, GetAce, GetTokenInformation,
    SetTokenInformation, TokenDefaultDacl, TokenGroups, ACCESS_ALLOWED_ACE, ACE_HEADER,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL,
    TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_DELETE_CHILD,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAG_OVERLAPPED, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
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

const DISABLE_MAX_PRIVILEGE: u32 = 1;
// LPAC performs capability access checks in addition to the restricted-token pass.
// Filesystem confinement is AppContainer LPAC plus per-launch workspace capability ACLs.
const SYSTEM_READ_SID: &str = "S-1-1-0";
const STILL_ACTIVE_CODE: u32 = 259;
const WORKSPACE_ALLOW_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;
// Build/cache directories (`target/`, `dist/`, `node_modules/.cache/`, etc.) and
// per-invocation temp directories allow full build-artifact lifecycle (create, write,
// rename, delete) without `WRITE_DAC` or `WRITE_OWNER`. Top-level build roots retain
// a non-delete-shared handle in `WorkspaceAclGuard` so the root itself cannot be
// renamed, deleted, or swapped with a junction.
const BUILD_CHILD_ALLOW_MASK: u32 = WORKSPACE_ALLOW_MASK | DELETE | FILE_DELETE_CHILD;
const BUILD_PROTECTED_DENY_WRITE_MASK: u32 = FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const ERROR_DELETE_PENDING: i32 = 303;

static WIN_SPAWN_LOCK: Mutex<()> = Mutex::new(());
// Serializes only the brief AppContainer profile registry create/delete calls so
// concurrent sandbox spawns never race on Windows AppContainer registry state.
// Never held while a child process is running.
static PROFILE_LOCK: Mutex<()> = Mutex::new(());
static CONTAINER_SEQ: AtomicU64 = AtomicU64::new(1);
static CAP_SID_SEQ: AtomicU32 = AtomicU32::new(1);
static PIPE_SEQ: AtomicU64 = AtomicU64::new(1);
static TEMP_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

/// Returns a per-canonical-workspace mutex used ONLY during brief DACL read-modify-write
/// operations (`grant_tree` / `revoke`), never while child processes execute.
fn workspace_acl_lock(workspace: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = map.get(workspace).and_then(Weak::upgrade) {
        return existing;
    }
    map.retain(|_, weak| weak.strong_count() > 0);
    let lock = Arc::new(Mutex::new(()));
    map.insert(workspace.to_path_buf(), Arc::downgrade(&lock));
    lock
}

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
    // Pointer-aligned, fixed-size backing buffer required by `_PROC_THREAD_ATTRIBUTE_LIST`.
    buffer: Box<[usize]>,
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
        let words = size.div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words].into_boxed_slice();
        let ptr = buffer.as_mut_ptr().cast::<c_void>();
        // SAFETY: `buffer` is `usize`-aligned and at least `size` bytes wide.
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

    let policy = &config.policy;
    let workspace_canon =
        canonicalize_no_prefix(&policy.workspace_root).map_err(|e| tag("canonicalize", &e))?;
    let mut profile = ContainerProfile::new().map_err(|e| tag("container", &e))?;
    let system_sid = OwnedLocalSid::from_str(SYSTEM_READ_SID)?;
    let nonce = CAP_SID_SEQ.fetch_add(1, Ordering::Relaxed);
    let cap_sid = OwnedLocalSid::from_str(&format!(
        "S-1-5-21-1314411346-{}-{nonce}-1",
        std::process::id()
    ))?;
    // An AppContainer's own profile is an outside-workspace write exception in
    // Windows by default. Only boundary probes/tests create and receive those
    // generated paths, without ever mutating caller `command` or exposing them
    // to ordinary tool runs.
    let probe_storage = if command.get_envs().any(|(key, _)| {
        key == "NEXORA_PROBE_OUTSIDE" || (cfg!(test) && key == "NX_TEST_PROBE_STORAGE")
    }) {
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
        assert!(!text.is_null(), "ConvertSidToStringSidW returned null");
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
    let sids = [profile.sid, cap_sid.as_ptr()];
    let acl_guard = WorkspaceAclGuard::new(policy, &workspace_canon, &sids)?;
    let temp_dir = SandboxTempDir::new(&sids, &profile.moniker).map_err(|e| tag("temp_dir", &e))?;
    let base_token = open_current_token_for_restriction().map_err(|e| tag("open_token", &e))?;
    let restricted_token = create_restricted_token(
        base_token.as_raw_handle().cast(),
        cap_sid.as_ptr(),
        profile.sid,
        system_sid.as_ptr(),
    )
    .map_err(|e| tag("restricted_token", &e))?;

    let job_owned = create_sandbox_job().map_err(|e| tag("create_job", &e))?;
    let raw_job = job_owned.into_raw_handle().cast::<c_void>();
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
            return Err(io::Error::last_os_error());
        }
    }

    // Declare every buffer referenced by `UpdateProcThreadAttribute` before `attr_list`
    // so they are guaranteed to outlive `DeleteProcThreadAttributeList`.
    let mut inherited_handles = vec![child_stdin_raw];
    if !inherited_handles.contains(&child_stdout_raw) {
        inherited_handles.push(child_stdout_raw);
    }
    if !inherited_handles.contains(&child_stderr_raw) {
        inherited_handles.push(child_stderr_raw);
    }
    let inherited_handles = inherited_handles.into_boxed_slice();
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    // LPAC excludes implicit ALL APPLICATION PACKAGES filesystem grants.
    let opt_out = 1u32;

    let mut attr_list = AttributeList::new(3)?;
    attr_list.set_handle_list(&inherited_handles)?;
    attr_list
        .set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)
        .map_err(|e| tag("security_capabilities", &e))?;
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
    for key in ["USERPROFILE", "APPDATA"] {
        env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        env_entries.push((key.into(), workspace_canon.to_string_lossy().into_owned()));
    }
    env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case("LOCALAPPDATA"));
    env_entries.push((
        "LOCALAPPDATA".into(),
        temp_dir.root().to_string_lossy().into_owned(),
    ));
    for key in ["TEMP", "TMP"] {
        env_entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        env_entries.push((
            key.into(),
            temp_dir.ac_temp().to_string_lossy().into_owned(),
        ));
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

    // SAFETY: All pointers (`restricted_token`, `cmdline`, `env_block`, `cwd_wide`, `si`, `pi`)
    // remain valid for the duration of `CreateProcessAsUserW`. Casting from `&si` preserves full
    // `STARTUPINFOEXW` provenance for `EXTENDED_STARTUPINFO_PRESENT`.
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
            std::ptr::from_ref(&si).cast::<STARTUPINFOW>(),
            std::ptr::from_mut(&mut pi),
        )
    };
    // Release the attribute list immediately while all attribute value buffers are still live.
    drop(attr_list);
    // Close parent copies of child pipe ends immediately so EOF propagates cleanly.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);

    if created == 0 {
        let err = tag("CreateProcessAsUserW", &io::Error::last_os_error());
        return Err(err);
    }

    // SAFETY: `pi.hProcess` and `pi.hThread` were returned by `CreateProcessAsUserW`.
    let proc_owned = unsafe { OwnedHandle::from_raw_handle(pi.hProcess.cast()) };
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
        let err = io::Error::last_os_error();
        unsafe {
            let _ = TerminateProcess(proc_owned.as_raw_handle().cast(), 1);
        }
        return Err(err);
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
            _temp_dir: temp_dir,
            _profile: profile,
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
    let write_file = OpenOptions::new()
        .access_mode(FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES)
        .share_mode(0)
        .open(&name)?;
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
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut storage = vec![0u64; (size as usize).div_ceil(8)];
    let capacity = u32::try_from(storage.len() * std::mem::size_of::<u64>())
        .map_err(|_| io::Error::other("token groups buffer size overflow"))?;
    if unsafe {
        GetTokenInformation(
            token,
            TokenGroups,
            storage.as_mut_ptr().cast(),
            capacity,
            &raw mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let groups = storage.as_ptr().cast::<TOKEN_GROUPS>();
    let count = unsafe { (*groups).GroupCount } as usize;
    let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    let entries_bytes = count
        .checked_mul(std::mem::size_of::<SID_AND_ATTRIBUTES>())
        .and_then(|bytes| offset.checked_add(bytes))
        .ok_or_else(|| io::Error::other("invalid token groups size"))?;
    if entries_bytes > size as usize || size as usize > storage.len() * std::mem::size_of::<u64>() {
        return Err(io::Error::other("invalid token groups"));
    }
    let entries = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*groups).Groups).cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    for entry in entries {
        if entry.Attributes & 0xc000_0000 == 0xc000_0000 && !entry.Sid.is_null() {
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
    if result != ERROR_SUCCESS || dacl.is_null() {
        return Err(io::Error::other("sandbox default ACL unavailable"));
    }
    let info = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
    let result = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            std::ptr::from_ref(&info).cast(),
            u32::try_from(std::mem::size_of::<TOKEN_DEFAULT_DACL>()).unwrap_or(0),
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

fn create_sandbox_job() -> io::Result<OwnedHandle> {
    // SAFETY: Anonymous Job Object.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `job` is a newly created Job Object handle; `OwnedHandle` guarantees cleanup on error.
    let job_owned = unsafe { OwnedHandle::from_raw_handle(job.cast()) };
    let job_raw = job_owned.as_raw_handle().cast::<c_void>();

    // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C struct.
    let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    ext.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let ext_len = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("JOBOBJECT_EXTENDED_LIMIT_INFORMATION overflow"))?;
    // SAFETY: Valid `job_raw` and initialized `ext`.
    if unsafe {
        SetInformationJobObject(
            job_raw,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&ext).cast::<c_void>(),
            ext_len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
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
    // SAFETY: Valid `job_raw` and initialized `ui`.
    if unsafe {
        SetInformationJobObject(
            job_raw,
            JobObjectBasicUIRestrictions,
            std::ptr::from_ref(&ui).cast::<c_void>(),
            ui_len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    Ok(job_owned)
}

// The unique profile, temp dir, and grants live until the entire child tree is terminated.
// Never add grants or denies to private user/system directories.
pub(crate) struct WindowsSandboxGuard {
    acl: WorkspaceAclGuard,
    _temp_dir: SandboxTempDir,
    _profile: ContainerProfile,
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

/// Ephemeral per-invocation temp directory isolated to this sandbox's unique SIDs.
/// Windows `AppContainer` automatically rewrites child `TEMP`/`TMP` to
/// `%LOCALAPPDATA%\Packages\<moniker>\AC\Temp`, so we materialize that exact tree
/// inside the ephemeral per-invocation directory and delete it on Drop.
struct SandboxTempDir {
    root: PathBuf,
    ac_temp: PathBuf,
}

impl SandboxTempDir {
    fn new(sids: &[*mut c_void], moniker: &str) -> io::Result<Self> {
        let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "nexora-sb-tmp-{}-{stamp}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir(&dir)?;
        let canon = canonicalize_no_prefix(&dir).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&dir);
        })?;
        let packages = canon.join("Packages");
        let moniker_dir = packages.join(moniker.to_ascii_lowercase());
        let ac_dir = moniker_dir.join("AC");
        let ac_temp = ac_dir.join("Temp");
        let guard = Self {
            root: canon.clone(),
            ac_temp: ac_temp.clone(),
        };
        std::fs::create_dir_all(&ac_temp)?;

        let mut owned_sids = Vec::with_capacity(sids.len());
        for sid in sids {
            let mut copy = vec![0u64; 9];
            // SAFETY: `sid` is a valid Win32 SID pointer; 72 bytes exceeds `SECURITY_MAX_SID_SIZE` (68).
            if unsafe { CopySid(72, copy.as_mut_ptr().cast(), *sid) } == 0 {
                return Err(io::Error::last_os_error());
            }
            owned_sids.push(copy);
        }
        let child_ops = [(
            GRANT_ACCESS,
            BUILD_CHILD_ALLOW_MASK,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
        )];
        for path in [canon, packages, moniker_dir, ac_dir, ac_temp] {
            let handle = OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            apply_file_aces(&handle, &owned_sids, &child_ops)?;
        }
        Ok(guard)
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn ac_temp(&self) -> &Path {
        &self.ac_temp
    }
}

impl Drop for SandboxTempDir {
    fn drop(&mut self) {
        for attempt in 0..5 {
            match std::fs::remove_dir_all(&self.root) {
                Err(err) if err.kind() != io::ErrorKind::NotFound && attempt < 4 => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Ok(()) | Err(_) => return,
            }
        }
    }
}

struct ContainerProfile {
    moniker: String,
    name: Vec<u16>,
    sid: *mut c_void,
    cleanup_root: Option<PathBuf>,
}
impl ContainerProfile {
    fn storage_paths(&mut self) -> io::Result<(PathBuf, PathBuf)> {
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::Isolation::GetAppContainerFolderPath;
        let mut text = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(self.sid, &raw mut text) } == 0 || text.is_null() {
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
        let moniker = format!("Nexora.Sandbox.{}.{stamp}.{seq}", std::process::id());
        let name = to_wide(&moniker);
        let mut sid = std::ptr::null_mut();
        let _profile_guard = PROFILE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        if result < 0 || sid.is_null() {
            if result >= 0 {
                // SAFETY: `name` was just registered above.
                unsafe {
                    DeleteAppContainerProfile(name.as_ptr());
                }
            }
            return Err(io::Error::other("AppContainer profile unavailable"));
        }
        Ok(Self {
            moniker,
            name,
            sid,
            cleanup_root: None,
        })
    }
}
impl Drop for ContainerProfile {
    fn drop(&mut self) {
        let profile_guard = PROFILE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: no live process remains when ProcessTreeChild releases this guard.
        unsafe {
            DeleteAppContainerProfile(self.name.as_ptr());
            if !self.sid.is_null() {
                FreeSid(self.sid);
                self.sid = std::ptr::null_mut();
            }
        }
        drop(profile_guard);
        if let Some(root) = &self.cleanup_root {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

type AceOp = (i32, u32, u32);

fn is_transient_vanished(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::NotFound || err.raw_os_error() == Some(ERROR_DELETE_PENDING)
}

struct WorkspaceAclGuard {
    acl_lock: Arc<Mutex<()>>,
    files: Vec<File>,
    build_roots: Vec<PathBuf>,
    sids: Vec<Vec<u64>>,
}
impl WorkspaceAclGuard {
    fn new(policy: &SandboxPolicy, root: &Path, sids: &[*mut c_void]) -> io::Result<Self> {
        let mut owned_sids = Vec::with_capacity(sids.len());
        for sid in sids {
            let mut copy = vec![0u64; 9];
            // SAFETY: `sid` is a valid Win32 SID pointer; 72 bytes exceeds `SECURITY_MAX_SID_SIZE` (68).
            if unsafe { CopySid(72, copy.as_mut_ptr().cast(), *sid) } == 0 {
                return Err(io::Error::last_os_error());
            }
            owned_sids.push(copy);
        }
        let mut guard = Self {
            acl_lock: workspace_acl_lock(root),
            files: Vec::new(),
            build_roots: Vec::new(),
            sids: owned_sids,
        };
        guard.grant_tree(policy, root)?;
        Ok(guard)
    }

    #[allow(clippy::too_many_lines)] // Single validated walk guarantees alias rejection before any ACL mutation.
    fn grant_tree(&mut self, policy: &SandboxPolicy, root: &Path) -> io::Result<()> {
        let deny_read_prefixes = super::sandbox::canonical_prefixes(&policy.deny_read_paths);
        let deny_write_prefixes = super::sandbox::canonical_prefixes(&policy.deny_write_paths);
        let mut stack = vec![root.to_path_buf()];
        let mut retained_ops: Vec<(File, Vec<AceOp>)> = Vec::new();
        let mut transient_build_ops: Vec<(File, Vec<AceOp>)> = Vec::new();
        let mut build_hardlinks: HashMap<(u32, u32, u32), (u32, HashSet<PathBuf>)> = HashMap::new();
        let mut visited = 0usize;

        let _lock = self
            .acl_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Non-build directories stay structurally immutable (read-only) so absent protected
        // names (`.env*`, `.git`, `.nexora`) and parent rename/delete operations remain blocked.
        // Recognized build/cache directories (`target/`, `dist/`, `node_modules/.cache/`, etc.)
        // permit new file/subdirectory creation and artifact lifecycle inside them while
        // preserving hard blocks on protected paths, reparse points, and external hardlinks.
        while let Some(path) = stack.pop() {
            visited += 1;
            if visited > 100_000 {
                return Err(io::Error::other(
                    "workspace exceeds sandbox validation bound",
                ));
            }
            let in_build_lex = is_writable_build_dir(root, &path);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(err) if is_transient_vanished(&err) && in_build_lex => continue,
                Err(err) => return Err(err),
            };
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other("workspace contains a reparse point"));
            }
            let canonical = match super::sandbox::canonicalize_clean(&path) {
                Ok(c) => c,
                Err(err) if is_transient_vanished(&err) && in_build_lex => continue,
                Err(err) => return Err(err),
            };
            let denied_read = super::sandbox::private_store_path(&canonical)
                || deny_read_prefixes.iter().any(|p| canonical.starts_with(p));
            let denied_write = deny_write_prefixes.iter().any(|p| canonical.starts_with(p))
                || super::sandbox::sensitive_write_path(&canonical);
            let is_build_root = !denied_read
                && !denied_write
                && metadata.is_dir()
                && is_writable_build_dir_root(root, &canonical);
            let in_build_tree =
                !denied_read && !denied_write && is_writable_build_dir(root, &canonical);
            let is_protected_in_build = (denied_read || denied_write)
                && canonical
                    .parent()
                    .is_some_and(|parent| is_writable_build_dir(root, parent));
            let is_transient_build_child = in_build_tree && !is_build_root;

            let share = if is_transient_build_child {
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
            } else {
                FILE_SHARE_READ | FILE_SHARE_WRITE
            };
            let file = match OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(share)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
            {
                Ok(f) => f,
                Err(err) if is_transient_vanished(&err) && is_transient_build_child => continue,
                Err(err) => return Err(io::Error::other(format!("workspace handle: {err}"))),
            };
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: owned file handle and initialized output structure.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            let is_dir = info.dwFileAttributes & 0x10 != 0;
            if info.dwFileAttributes & 0x400 != 0 || is_dir != metadata.is_dir() {
                return Err(io::Error::other("workspace contains a filesystem alias"));
            }
            if !is_dir && info.nNumberOfLinks != 1 {
                if in_build_tree {
                    let key = (
                        info.dwVolumeSerialNumber,
                        info.nFileIndexHigh,
                        info.nFileIndexLow,
                    );
                    let entry = build_hardlinks
                        .entry(key)
                        .or_insert_with(|| (info.nNumberOfLinks, HashSet::new()));
                    if entry.0 != info.nNumberOfLinks {
                        return Err(io::Error::other("workspace contains a filesystem alias"));
                    }
                    entry.1.insert(canonical.clone());
                } else {
                    return Err(io::Error::other("workspace contains a filesystem alias"));
                }
            }

            let ops = if denied_read {
                if is_protected_in_build {
                    let inherit = if is_dir {
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                    } else {
                        0
                    };
                    vec![(DENY_ACCESS, BUILD_CHILD_ALLOW_MASK, inherit)]
                } else {
                    Vec::new()
                }
            } else if denied_write {
                if is_protected_in_build {
                    let inherit = if is_dir {
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                    } else {
                        0
                    };
                    vec![
                        (DENY_ACCESS, BUILD_PROTECTED_DENY_WRITE_MASK, inherit),
                        (GRANT_ACCESS, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, 0),
                    ]
                } else {
                    vec![(GRANT_ACCESS, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, 0)]
                }
            } else if is_build_root {
                self.build_roots.push(canonical.clone());
                vec![(
                    GRANT_ACCESS,
                    BUILD_CHILD_ALLOW_MASK,
                    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                )]
            } else if in_build_tree {
                if is_dir {
                    vec![(
                        GRANT_ACCESS,
                        BUILD_CHILD_ALLOW_MASK,
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                    )]
                } else {
                    vec![(GRANT_ACCESS, BUILD_CHILD_ALLOW_MASK, 0)]
                }
            } else if is_dir {
                vec![(GRANT_ACCESS, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, 0)]
            } else {
                vec![(GRANT_ACCESS, WORKSPACE_ALLOW_MASK, 0)]
            };

            if is_transient_build_child {
                transient_build_ops.push((file, ops));
            } else {
                retained_ops.push((file, ops));
            }

            if is_dir && !denied_read {
                match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry {
                                Ok(e) => stack.push(e.path()),
                                Err(err)
                                    if is_transient_vanished(&err) && is_transient_build_child => {}
                                Err(err) => return Err(err),
                            }
                        }
                    }
                    Err(err) if is_transient_vanished(&err) && is_transient_build_child => {}
                    Err(err) => return Err(err),
                }
            }
        }

        // Verify every multi-link inode discovered inside build directories has ALL of its
        // volume links accounted for inside build directories (zero outside or protected links).
        for (expected_links, seen_paths) in build_hardlinks.values() {
            if seen_paths.len() != *expected_links as usize {
                return Err(io::Error::other("workspace contains a filesystem alias"));
            }
        }

        // Retain handles before mutating any DACL so Drop revokes partial grants on error.
        let mut retained_apply = Vec::with_capacity(retained_ops.len());
        for (file, ops) in retained_ops {
            let idx = self.files.len();
            self.files.push(file);
            if !ops.is_empty() {
                retained_apply.push((idx, ops));
            }
        }
        for (idx, ops) in retained_apply {
            apply_file_aces(&self.files[idx], &self.sids, &ops)?;
        }
        for (file, ops) in transient_build_ops {
            if !ops.is_empty() {
                if let Err(err) = apply_file_aces(&file, &self.sids, &ops) {
                    if !is_transient_vanished(&err) {
                        return Err(err);
                    }
                }
            }
        }
        Ok(())
    }

    fn revoke(&mut self) {
        if self.files.is_empty() && self.build_roots.is_empty() {
            return;
        }
        let _lock = self
            .acl_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Retained non-delete-shared handles prevent rename/reparse races and allow
        // SID-specific removal without restoring stale snapshots over user ACL edits.
        for file in self.files.drain(..).rev() {
            let _ = revoke_file_aces(&file, &self.sids);
        }
        // Clean up any ACEs (including inherited ACEs on newly created artifacts) inside
        // build directories without disturbing other concurrent sandboxes' SIDs.
        let mut stack: Vec<PathBuf> = self.build_roots.drain(..).collect();
        let mut visited = 0usize;
        while let Some(dir_or_file) = stack.pop() {
            visited += 1;
            if visited > 100_000 {
                break;
            }
            let Ok(meta) = std::fs::symlink_metadata(&dir_or_file) else {
                continue;
            };
            if meta.file_attributes() & 0x400 != 0 {
                continue;
            }
            if let Ok(file) = OpenOptions::new()
                .access_mode(READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&dir_or_file)
            {
                let _ = revoke_file_aces(&file, &self.sids);
            }
            if meta.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&dir_or_file) {
                    for entry in entries.flatten() {
                        stack.push(entry.path());
                    }
                }
            }
        }
    }
}
impl Drop for WorkspaceAclGuard {
    fn drop(&mut self) {
        if !self.files.is_empty() || !self.build_roots.is_empty() {
            self.revoke();
        }
    }
}

fn apply_file_aces(file: &File, sids: &[Vec<u64>], ops: &[AceOp]) -> io::Result<()> {
    if sids.is_empty() || ops.is_empty() {
        return Ok(());
    }
    let mut dacl = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    // SAFETY: open file handle has READ_CONTROL and WRITE_DAC.
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
    let mut entries = Vec::with_capacity(sids.len() * ops.len());
    for &(mode, mask, inheritance) in ops {
        for sid in sids {
            entries.push(explicit_access(
                sid.as_ptr().cast_mut().cast(),
                mask,
                mode,
                inheritance,
            ));
        }
    }
    let count = u32::try_from(entries.len())
        .map_err(|_| io::Error::other("ACL entry count exceeds u32"))?;
    let mut new_dacl = std::ptr::null_mut();
    // SAFETY: ACL, entries, and SID pointers remain live until `LocalFree` below.
    let result = unsafe { SetEntriesInAclW(count, entries.as_ptr(), dacl, &raw mut new_dacl) };
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
        if !sd.is_null() {
            LocalFree(sd as HLOCAL);
        }
    }
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(result).unwrap_or(5),
        ))
    }
}

/// Removes both explicit and inherited ALLOW/DENY ACEs matching any SID in `sids`,
/// leaving all other ACEs (user, system, and concurrent sandboxes) intact.
fn revoke_file_aces(file: &File, sids: &[Vec<u64>]) -> io::Result<()> {
    if sids.is_empty() {
        return Ok(());
    }
    let mut dacl = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    // SAFETY: open file handle has READ_CONTROL and WRITE_DAC.
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
    let mut modified = false;
    if !dacl.is_null() {
        // SAFETY: `dacl` is a valid Win32 `ACL` inside `sd`.
        let ace_count = u32::from(unsafe { (*dacl).AceCount });
        let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        for index in (0..ace_count).rev() {
            let mut ace_ptr = std::ptr::null_mut();
            // SAFETY: `index < ace_count` and `dacl` is valid.
            if unsafe { GetAce(dacl, index, &raw mut ace_ptr) } == 0 || ace_ptr.is_null() {
                continue;
            }
            // SAFETY: every ACE begins with `ACE_HEADER`.
            let header = unsafe { *ace_ptr.cast::<ACE_HEADER>() };
            if !matches!(
                header.AceType,
                ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
            ) || usize::from(header.AceSize) <= sid_offset
            {
                continue;
            }
            // SAFETY: `ACCESS_ALLOWED_ACE` and `ACCESS_DENIED_ACE` share identical layout
            // (`ACE_HEADER` + `Mask: u32` + `SidStart`), verified by `AceSize > sid_offset`.
            let ace_sid = unsafe { ace_ptr.cast::<u8>().add(sid_offset).cast::<c_void>() };
            let matches_any = sids
                .iter()
                .any(|sid| unsafe { EqualSid(ace_sid, sid.as_ptr().cast_mut().cast()) } != 0);
            if matches_any && unsafe { DeleteAce(dacl, index) } != 0 {
                modified = true;
            }
        }
    }
    let write_result = if modified {
        // SAFETY: `dacl` was updated in-place via `DeleteAce` and remains live until `LocalFree(sd)`.
        unsafe {
            SetSecurityInfo(
                file.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        }
    } else {
        ERROR_SUCCESS
    };
    unsafe {
        if !sd.is_null() {
            LocalFree(sd as HLOCAL);
        }
    }
    if write_result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(write_result).unwrap_or(5),
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
            if !sd.is_null() {
                LocalFree(sd as HLOCAL);
            }
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
        let (err_tx, err_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(mut err) = err_pipe {
                let _ = std::io::copy(&mut err, &mut std::io::sink());
            }
            let _ = err_tx.send(());
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
        err_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("stderr reader did not finish after child exit");
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
        assert!(status.success(), "normal sandbox probe failed: {status}");
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
        assert!(
            probe_status.success(),
            "storage sandbox probe failed: {probe_status}"
        );
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
            assert!(status.success(), "icacls grant failed: {status}");
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
        assert!(
            status.success(),
            "protected names sandbox probe failed: {status}"
        );
        assert!(!ws.join(".env").exists());
        assert!(!ws.join(".git").exists());
        assert!(!ws.join(".nexora").exists());
        assert!(!ws.join("src2").exists());
        assert!(!src.join("new.txt").exists());
        assert!(!src.join("moved.txt").exists());
        assert!(src.is_dir());
        assert_eq!(std::fs::read_to_string(&existing).unwrap().trim(), "after");
    }

    fn has_ephemeral_sandbox_sid(path: &Path) -> bool {
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        let mut dacl = std::ptr::null_mut();
        let mut sd = std::ptr::null_mut();
        assert_eq!(
            unsafe {
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
            },
            ERROR_SUCCESS
        );
        let mut found = false;
        if !dacl.is_null() {
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            for index in 0..u32::from(unsafe { (*dacl).AceCount }) {
                let mut ace = std::ptr::null_mut();
                assert_ne!(unsafe { GetAce(dacl, index, &raw mut ace) }, 0);
                let header = unsafe { *ace.cast::<ACE_HEADER>() };
                if !matches!(
                    header.AceType,
                    ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE
                ) || usize::from(header.AceSize) <= sid_offset
                {
                    continue;
                }
                let ace_sid = unsafe { ace.cast::<u8>().add(sid_offset).cast::<c_void>() };
                let mut text = std::ptr::null_mut();
                if unsafe { ConvertSidToStringSidW(ace_sid, &raw mut text) } != 0 && !text.is_null()
                {
                    let mut len = 0;
                    while unsafe { *text.add(len) } != 0 {
                        len += 1;
                    }
                    let sid_str =
                        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
                    unsafe {
                        LocalFree(text as HLOCAL);
                    }
                    if sid_str.starts_with("S-1-15-2-")
                        || sid_str.starts_with("S-1-5-21-1314411346-")
                    {
                        found = true;
                        break;
                    }
                }
            }
        }
        unsafe {
            if !sd.is_null() {
                LocalFree(sd as HLOCAL);
            }
        }
        found
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Parallel stress test verifies barrier overlap, outputs, and exact ACL restoration.
    fn parallel_windows_sandbox_stress_runs_concurrently_without_acl_races() {
        const WORKERS: usize = 12;
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-par-{stamp}"));
        let target = ws.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let _guard = Fixture(ws.clone());
        let shared = ws.join("shared.txt");
        std::fs::write(&shared, "initial").unwrap();
        let protected = ws.join(".env");
        std::fs::write(&protected, "keep-secret").unwrap();

        let tracked = [&ws, &target, &shared, &protected];
        let before: Vec<_> = tracked
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();

        // Barrier AFTER spawn proves all 12 sandboxes are simultaneously live without
        // global serialization (a blocking SANDBOX_LEASE would deadlock here).
        let live_barrier = Arc::new(std::sync::Barrier::new(WORKERS));
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let ws_clone = ws.clone();
            let barrier = Arc::clone(&live_barrier);
            handles.push(std::thread::spawn(move || {
                let mut cmd = Command::new(windows_cmd_path());
                let script = format!(
                    "mkdir target\\w{worker} & echo worker-{worker}>target\\w{worker}\\tmp.txt & move /Y target\\w{worker}\\tmp.txt target\\w{worker}\\done.txt >NUL & echo ok>\"%TEMP%\\t{worker}.txt\" & type \"%TEMP%\\t{worker}.txt\" & echo PARALLEL_OK_{worker}"
                );
                cmd.current_dir(&ws_clone).args(["/D", "/C", &script]);
                let config = SandboxSpawnConfig::for_workspace(&ws_clone);
                let mut child = match crate::spawn_sandboxed_in_process_tree(&mut cmd, &config) {
                    Ok(child) => child,
                    Err(error) => {
                        assert!(
                            std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_none(),
                            "required parallel native sandbox worker {worker} failed: {error}"
                        );
                        barrier.wait();
                        return None;
                    }
                };
                // Hold all 12 sandboxed children alive simultaneously before releasing any.
                barrier.wait();

                let mut stdout_bytes = Vec::new();
                if let Some(mut out) = child.take_stdout() {
                    let _ = std::io::Read::read_to_end(&mut out, &mut stdout_bytes);
                }
                let start = std::time::Instant::now();
                let status = loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    assert!(
                        start.elapsed() < std::time::Duration::from_secs(15),
                        "parallel worker {worker} timed out"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                };
                // Verify cross-thread drop of ProcessTreeChild / WindowsSandboxGuard.
                std::thread::spawn(move || drop(child))
                    .join()
                    .expect("cross-thread ProcessTreeChild drop must succeed");
                Some((status, String::from_utf8_lossy(&stdout_bytes).into_owned()))
            }));
        }

        let mut verified_count = 0usize;
        for (worker, handle) in handles.into_iter().enumerate() {
            if let Some((status, stdout)) = handle.join().expect("parallel worker thread panicked")
            {
                assert!(
                    status.success(),
                    "parallel worker {worker} exited unsuccessfully: {status:?}, stdout={stdout}"
                );
                assert!(
                    stdout.contains(&format!("PARALLEL_OK_{worker}")),
                    "missing output from worker {worker}: {stdout}"
                );
                let out_file = target.join(format!("w{worker}")).join("done.txt");
                assert_eq!(
                    std::fs::read_to_string(&out_file).unwrap().trim(),
                    format!("worker-{worker}")
                );
                verified_count += 1;
            }
        }
        if std::env::var_os("NEXORA_REQUIRE_NATIVE_SANDBOX").is_some() {
            assert_eq!(verified_count, WORKERS);
            for worker in 0..WORKERS {
                let out_file = target.join(format!("w{worker}")).join("done.txt");
                assert!(
                    !has_ephemeral_sandbox_sid(&out_file),
                    "ephemeral sandbox SID leaked onto newly created artifact {}",
                    out_file.display()
                );
            }
        }
        let after: Vec<_> = tracked
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();
        assert_eq!(
            before, after,
            "all 12 concurrent sandboxes must cleanly restore original workspace ACLs"
        );
        assert_eq!(std::fs::read_to_string(&protected).unwrap(), "keep-secret");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // End-to-end proof of build-dir writes alongside protected-path hard blocks.
    fn build_directories_allow_new_files_while_hard_blocking_protected_and_outside_paths() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-build-{stamp}"));
        let outside = crate::test_workspace_parent().join(format!("nx-win-build-out-{stamp}"));
        let src = ws.join("src");
        let target = ws.join("target");
        let dist = ws.join("dist");
        let nm_cache = ws.join("node_modules").join(".cache");
        let nm_pkg = ws.join("node_modules").join("pkg");
        let git_hooks = ws.join(".git").join("hooks");
        let nexora_dir = ws.join(".nexora");
        for dir in [
            &outside,
            &src,
            &target,
            &dist,
            &nm_cache,
            &nm_pkg,
            &git_hooks,
            &nexora_dir,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let _g1 = Fixture(ws.clone());
        let _g2 = Fixture(outside.clone());

        std::fs::write(src.join("lib.rs"), "pub fn ok() {}").unwrap();
        let old_sub = target.join("old_sub");
        std::fs::create_dir_all(&old_sub).unwrap();
        std::fs::write(old_sub.join("stale.o"), "old-obj").unwrap();
        std::fs::write(target.join("stale.o"), "old-obj").unwrap();
        std::fs::write(target.join(".env"), "target-env-secret").unwrap();
        std::fs::write(nm_pkg.join("index.js"), "module.exports = 1;").unwrap();
        std::fs::write(ws.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(ws.join(".git").join("config"), "[core]\n").unwrap();
        std::fs::write(git_hooks.join("pre-commit"), "#!/bin/sh\n").unwrap();
        std::fs::write(nexora_dir.join("policy.json"), "{}").unwrap();
        std::fs::write(ws.join(".env"), "root-secret").unwrap();
        let outside_escape = outside.join("escaped.txt");

        let tracked = [
            &ws,
            &src,
            &target,
            &dist,
            &nm_cache,
            &ws.join(".git").join("HEAD"),
            &ws.join(".env"),
            &target.join(".env"),
        ];
        let before: Vec<_> = tracked
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();

        let script = format!(
            concat!(
                "mkdir target\\debug\\deps & ",
                "echo obj-tmp>target\\debug\\deps\\app.o.tmp & ",
                "move /Y target\\debug\\deps\\app.o.tmp target\\debug\\deps\\app.o >NUL & ",
                "move /Y target\\stale.o target\\stale.moved.o >NUL & ",
                "rmdir /S /Q target\\old_sub & ",
                "mkdir dist\\assets & ",
                "echo bundle>dist\\assets\\index.js & ",
                "mkdir node_modules\\.cache\\vite & ",
                "echo cached>node_modules\\.cache\\vite\\dep.js & ",
                "echo tmp-ok>\"%TEMP%\\rustc-tmp.txt\" & ",
                "type \"%TEMP%\\rustc-tmp.txt\" & ",
                "(echo pwn>.git\\HEAD) 2>NUL & ",
                "(echo pwn>.git\\config) 2>NUL & ",
                "(echo pwn>.git\\hooks\\pre-commit) 2>NUL & ",
                "(echo pwn>.nexora\\policy.json) 2>NUL & ",
                "(echo pwn>.env) 2>NUL & ",
                "(echo pwn>.env.local) 2>NUL & ",
                "(echo pwn>target\\.env) 2>NUL & ",
                "(ren target\\.env env.moved) 2>NUL & ",
                "(echo pwn>src\\new.rs) 2>NUL & ",
                "(echo pwn>node_modules\\evil.js) 2>NUL & ",
                "(ren target target2) 2>NUL & ",
                "(rmdir target) 2>NUL & ",
                "(echo pwn>\"{out}\") 2>NUL & ",
                "echo BUILD_SANDBOX_DONE & exit /b 0"
            ),
            out = outside_escape.display()
        );
        let mut cmd = Command::new(windows_cmd_path());
        cmd.current_dir(&ws).args(["/D", "/C", &script]);
        let Some((status, stdout)) =
            run_sandboxed_capture(&mut cmd, &SandboxSpawnConfig::for_workspace(&ws))
        else {
            return;
        };
        assert!(status.success(), "build sandbox probe failed: {status:?}");
        assert!(stdout.contains("tmp-ok"), "TEMP write failed: {stdout}");
        assert!(stdout.contains("BUILD_SANDBOX_DONE"));

        // New build artifacts & lifecycle inside build dirs must succeed.
        let built_obj = target.join("debug").join("deps").join("app.o");
        let built_dist = dist.join("assets").join("index.js");
        let built_cache = nm_cache.join("vite").join("dep.js");
        assert_eq!(
            std::fs::read_to_string(&built_obj).unwrap().trim(),
            "obj-tmp"
        );
        assert!(!target.join("stale.o").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("stale.moved.o"))
                .unwrap()
                .trim(),
            "old-obj"
        );
        assert!(
            !old_sub.exists(),
            "rmdir inside target/ should remove old build subdir"
        );
        assert_eq!(
            std::fs::read_to_string(&built_dist).unwrap().trim(),
            "bundle"
        );
        assert_eq!(
            std::fs::read_to_string(&built_cache).unwrap().trim(),
            "cached"
        );

        // Protected files, source dir creation, build-root rename, and outside paths remain blocked.
        assert_eq!(
            std::fs::read_to_string(ws.join(".git").join("HEAD")).unwrap(),
            "ref: refs/heads/main\n"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join(".git").join("config")).unwrap(),
            "[core]\n"
        );
        assert_eq!(
            std::fs::read_to_string(git_hooks.join("pre-commit")).unwrap(),
            "#!/bin/sh\n"
        );
        assert_eq!(
            std::fs::read_to_string(nexora_dir.join("policy.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(ws.join(".env")).unwrap(),
            "root-secret"
        );
        assert!(!ws.join(".env.local").exists());
        assert_eq!(
            std::fs::read_to_string(target.join(".env")).unwrap(),
            "target-env-secret"
        );
        assert!(!src.join("new.rs").exists());
        assert!(!ws.join("node_modules").join("evil.js").exists());
        assert!(target.is_dir() && !ws.join("target2").exists());
        assert!(!outside_escape.exists());

        // Exact ACL restoration on tracked paths and zero ephemeral SIDs on new build artifacts.
        let after: Vec<_> = tracked
            .iter()
            .map(|p| (acl_snapshot(p, false), acl_snapshot(p, true)))
            .collect();
        assert_eq!(before, after, "tracked ACLs must match before/after");
        for created in [&built_obj, &built_dist, &built_cache] {
            assert!(
                !has_ephemeral_sandbox_sid(created),
                "ephemeral sandbox SID leaked onto {}",
                created.display()
            );
        }
    }

    #[test]
    fn intra_build_dir_hardlink_is_allowed_while_outside_build_hardlink_fails_closed() {
        let _native = crate::native_test_lock();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-bhl-ws-{stamp}"));
        let outside = crate::test_workspace_parent().join(format!("nx-win-bhl-out-{stamp}"));
        let deps = ws.join("target").join("debug").join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _g1 = Fixture(ws.clone());
        let _g2 = Fixture(outside.clone());

        // Intra-target hardlink (same pattern cargo uses for target/debug/bin <-> deps/bin-hash).
        let dep_bin = deps.join("app-hash.exe");
        let top_bin = ws.join("target").join("debug").join("app.exe");
        std::fs::write(&dep_bin, "binary").unwrap();
        std::fs::hard_link(&dep_bin, &top_bin).unwrap();

        let mut ok_cmd = Command::new(windows_cmd_path());
        ok_cmd.current_dir(&ws).args(["/D", "/C", "echo INTRA_OK"]);
        let Some((status, stdout)) =
            run_sandboxed_capture(&mut ok_cmd, &SandboxSpawnConfig::for_workspace(&ws))
        else {
            return;
        };
        assert!(status.success());
        assert!(stdout.contains("INTRA_OK"));

        // External hardlink inside target/ MUST fail closed before spawn.
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, "untouched").unwrap();
        let evil_link = ws.join("target").join("evil.txt");
        std::fs::hard_link(&outside_file, &evil_link).unwrap();

        let mut bad_cmd = Command::new(windows_cmd_path());
        bad_cmd
            .current_dir(&ws)
            .args(["/D", "/C", "echo hacked>target\\evil.txt"]);
        let err = crate::spawn_sandboxed_in_process_tree(
            &mut bad_cmd,
            &SandboxSpawnConfig::for_workspace(&ws),
        )
        .unwrap_err();
        assert!(err.to_string().contains("alias"), "unexpected error: {err}");
        assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "untouched");
        std::fs::remove_file(&evil_link).unwrap();

        // Hardlink from target/ to protected ws/.env MUST also fail closed.
        let env_file = ws.join(".env");
        std::fs::write(&env_file, "secret-env").unwrap();
        let env_alias = ws.join("target").join("env-alias.txt");
        std::fs::hard_link(&env_file, &env_alias).unwrap();
        let mut bad_env_cmd = Command::new(windows_cmd_path());
        bad_env_cmd
            .current_dir(&ws)
            .args(["/D", "/C", "echo hacked>target\\env-alias.txt"]);
        let err2 = crate::spawn_sandboxed_in_process_tree(
            &mut bad_env_cmd,
            &SandboxSpawnConfig::for_workspace(&ws),
        )
        .unwrap_err();
        assert!(
            err2.to_string().contains("alias"),
            "unexpected error: {err2}"
        );
        assert_eq!(std::fs::read_to_string(&env_file).unwrap(), "secret-env");
    }

    #[test]
    fn nul_byte_in_command_or_env_is_rejected_before_spawn() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = crate::test_workspace_parent().join(format!("nx-win-nul-{stamp}"));
        std::fs::create_dir(&ws).unwrap();
        let _guard = Fixture(ws.clone());
        let config = SandboxSpawnConfig::for_workspace(&ws);

        let mut nul_arg = Command::new(windows_cmd_path());
        nul_arg
            .current_dir(&ws)
            .args(["/D", "/C", "echo ok\0injected"]);
        assert_eq!(
            crate::spawn_sandboxed_in_process_tree(&mut nul_arg, &config)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut bad_env_key = Command::new(windows_cmd_path());
        bad_env_key
            .current_dir(&ws)
            .args(["/D", "/C", "echo ok"])
            .env("BAD=KEY", "value");
        assert_eq!(
            crate::spawn_sandboxed_in_process_tree(&mut bad_env_key, &config)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let mut nul_env_val = Command::new(windows_cmd_path());
        nul_env_val
            .current_dir(&ws)
            .args(["/D", "/C", "echo ok"])
            .env("BENIGN", "ok\0INJECTED=1");
        assert_eq!(
            crate::spawn_sandboxed_in_process_tree(&mut nul_env_val, &config)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
