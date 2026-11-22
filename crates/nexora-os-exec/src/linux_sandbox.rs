use std::ffi::CString;
use std::fs::File;
use std::io::{self, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::sandbox::{
    collect_sanitized_env, ProbeSpec, SandboxPolicy, SandboxSpawnConfig, SandboxVerificationStatus,
};
use crate::unix::PlatformHandle;

pub(crate) const MECHANISM_BWRAP_SECCOMP: &str = "linux-bwrap-seccomp";
pub(crate) const MECHANISM_LANDLOCK_SECCOMP: &str = "linux-landlock-seccomp";

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
const SECCOMP_SET_MODE_FILTER: libc::c_long = 1;

// Linux generic syscall numbers (Linux >= 5.13 across x86_64, aarch64, x86, riscv64).
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

const SYSTEM_READ_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/usr",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/alternatives",
    "/etc/localtime",
    "/etc/ssl/certs",
];

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

struct ResolvedLinuxRules {
    read_only_paths: Vec<File>,
    read_write_paths: Vec<File>,
}

pub(crate) fn probe_mechanism_available() -> Result<&'static str, SandboxVerificationStatus> {
    if build_seccomp_bpf().is_none() {
        return Err(SandboxVerificationStatus::Unavailable {
            reason: "unsupported Linux seccomp architecture".into(),
        });
    }
    if find_usable_bwrap().is_some() {
        return Ok(MECHANISM_BWRAP_SECCOMP);
    }
    if landlock_abi_version().is_some_and(|abi| abi >= 3) {
        return Ok(MECHANISM_LANDLOCK_SECCOMP);
    }
    Err(SandboxVerificationStatus::Unavailable {
        reason: "Linux requires usable bubblewrap or Landlock ABI 3 with seccomp".into(),
    })
}

pub(crate) fn spawn_sandboxed(
    command: &mut Command,
    config: &SandboxSpawnConfig,
) -> io::Result<(Child, PlatformHandle)> {
    let policy = &config.policy;
    let env = collect_sanitized_env(command, policy.sanitize_env);
    let cwd = command.get_current_dir().unwrap_or(&policy.workspace_root);
    if let Some(bwrap) = find_usable_bwrap() {
        return spawn_via_bwrap(&bwrap, command, policy, cwd, &env);
    }
    // Availability is probed again here. A CI run that requires bubblewrap
    // must fail if it became unavailable before launching the actual child.
    #[cfg(test)]
    if std::env::var("NEXORA_TEST_LINUX_SANDBOX_MECHANISM").as_deref()
        == Ok(MECHANISM_BWRAP_SECCOMP)
    {
        return Err(io::Error::other(
            "required test backend bubblewrap unavailable",
        ));
    }
    spawn_via_landlock_seccomp(command, policy, cwd, &env)
}

pub(crate) fn build_probe_command(spec: &ProbeSpec<'_>) -> Command {
    let mut cmd = Command::new("/bin/sh");
    let script = "if (printf 'escaped\\n' > \"$NEXORA_PROBE_OUTSIDE\") 2>/dev/null; then echo OUT_WRITE_ALLOWED; else echo OUT_WRITE_BLOCKED; fi; if /bin/cat \"$NEXORA_PROBE_PRIVATE\" >/dev/null 2>&1; then echo PRIV_READ_ALLOWED; else echo PRIV_READ_BLOCKED; fi; if printf 'ok\\n' > \"$NEXORA_PROBE_IN_WS\" && test \"$(/bin/cat \"$NEXORA_PROBE_IN_WS\")\" = ok; then echo IN_WS_OK; else echo IN_WS_FAIL; fi";
    cmd.args(["-c", script]);
    cmd.current_dir(spec.workspace_root);
    cmd.env("NEXORA_PROBE_OUTSIDE", spec.outside_file);
    cmd.env("NEXORA_PROBE_PRIVATE", spec.private_file);
    cmd.env("NEXORA_PROBE_IN_WS", spec.in_workspace_file);
    cmd
}

fn configure_bwrap_args_and_pins(
    wrapped: &mut Command,
    policy: &SandboxPolicy,
    cwd: &Path,
) -> io::Result<Vec<File>> {
    let workspace_canon = std::fs::canonicalize(&policy.workspace_root)
        .unwrap_or_else(|_| policy.workspace_root.clone());

    wrapped.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--cap-drop",
        "ALL",
    ]);
    // Build an allowlisted read-only runtime, never expose the host root,
    // /proc, /run, home directories, credential sockets or device aliases.
    for runtime in SYSTEM_READ_ROOTS {
        if Path::new(runtime).exists() {
            wrapped.arg("--ro-bind").arg(runtime).arg(runtime);
        }
    }
    wrapped.args(["--dev", "/dev"]);

    // Snapshot the namespace as read-only directories and individually pinned file
    // mounts. New private names on the host never become visible in this view.
    wrapped.arg("--tmpfs").arg(&workspace_canon);
    let workspace_fd = open_runtime(&workspace_canon)?;
    let mut pinned_files = Vec::new();
    for entry in crate::sandbox::workspace_entries(policy)? {
        if entry.directory {
            if entry.path != workspace_canon {
                wrapped.arg("--dir").arg(&entry.path);
            }
        } else {
            let file = open_workspace_file(
                &workspace_fd,
                entry
                    .path
                    .strip_prefix(&workspace_canon)
                    .map_err(|_| io::Error::other("workspace file escaped root"))?,
            )?;
            let source = format!("/proc/self/fd/{}", file.as_raw_fd());
            wrapped
                .arg(if entry.writable {
                    "--bind"
                } else {
                    "--ro-bind"
                })
                .arg(source)
                .arg(&entry.path);
            pinned_files.push(file);
        }
    }
    wrapped.arg("--remount-ro").arg(&workspace_canon);

    // Keep sensitive workspace subpaths read-only inside the writable workspace.
    for deny_write in &policy.deny_write_paths {
        if deny_write.exists() {
            wrapped.arg("--ro-bind").arg(deny_write).arg(deny_write);
        }
    }

    // Mask private user directories (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, app-data, synthetic probe dirs)
    // with unreadable empty tmpfs or `/dev/null` mask so host files are completely inaccessible.
    for deny_read in &policy.deny_read_paths {
        let target = std::fs::canonicalize(deny_read).unwrap_or_else(|_| deny_read.clone());
        if target.is_dir() {
            wrapped
                .arg("--tmpfs")
                .arg(&target)
                .arg("--remount-ro")
                .arg(&target);
        } else if target.is_file() {
            wrapped.arg("--ro-bind").arg("/dev/null").arg(&target);
        }
    }

    wrapped.args(["--remount-ro", "/dev", "--remount-ro", "/"]);
    wrapped.arg("--chdir").arg(cwd);
    Ok(pinned_files)
}

fn create_seccomp_memfd() -> io::Result<File> {
    // bubblewrap installs the filter after constructing its namespace; filtering
    // mount in pre_exec would either break setup or leave a setuid backend unfiltered.
    let filter =
        build_seccomp_bpf().ok_or_else(|| io::Error::other("unsupported seccomp architecture"))?;
    let name = c"nexora-seccomp";
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut filter_file = unsafe { File::from_raw_fd(fd) };
    for instruction in &filter {
        filter_file.write_all(&instruction.code.to_ne_bytes())?;
        filter_file.write_all(&[instruction.jt, instruction.jf])?;
        filter_file.write_all(&instruction.k.to_ne_bytes())?;
    }
    filter_file.rewind()?;
    Ok(filter_file)
}

fn spawn_via_bwrap(
    bwrap_bin: &Path,
    command: &Command,
    policy: &SandboxPolicy,
    cwd: &Path,
    env_entries: &[(String, String)],
) -> io::Result<(Child, PlatformHandle)> {
    let mut launch = Command::new(bwrap_bin);
    let mut pinned_files = configure_bwrap_args_and_pins(&mut launch, policy, cwd)?;
    let filter_file = create_seccomp_memfd()?;

    launch
        .arg("--seccomp")
        .arg(filter_file.as_raw_fd().to_string())
        .arg("--")
        .arg(command.get_program())
        .args(command.get_args());
    launch
        .env_clear()
        .envs(env_entries.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pinned_files.push(filter_file);
    // Clear CLOEXEC only in the forked launcher, never in the multithreaded parent.
    unsafe {
        launch.pre_exec(move || {
            for file in &pinned_files {
                if libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    PlatformHandle::spawn(&mut launch)
}

fn spawn_via_landlock_seccomp(
    command: &Command,
    policy: &SandboxPolicy,
    cwd: &Path,
    env_entries: &[(String, String)],
) -> io::Result<(Child, PlatformHandle)> {
    let abi = landlock_abi_version()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "Landlock ABI unsupported"))?;
    if abi < 3 {
        return Err(io::Error::other(
            "Landlock ABI below 3 cannot confine truncation",
        ));
    }
    let rules = resolve_landlock_rules(policy)?;
    let filter =
        build_seccomp_bpf().ok_or_else(|| io::Error::other("unsupported seccomp architecture"))?;

    let mut wrapped = Command::new(command.get_program());
    wrapped.args(command.get_args());
    wrapped.current_dir(cwd);
    wrapped.env_clear();
    for (k, v) in env_entries {
        wrapped.env(k, v);
    }
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // SAFETY: `pre_exec` executes only raw `prctl` / `syscall` / `open` / `close`
    // over pre-allocated `CString` paths without touching Rust locks or heap allocators.
    unsafe {
        wrapped.pre_exec(move || {
            apply_landlock_rules(abi, &rules)?;
            apply_seccomp_no_new_privs(&filter)?;
            Ok(())
        });
    }

    PlatformHandle::spawn(&mut wrapped)
}

fn resolve_landlock_rules(policy: &SandboxPolicy) -> io::Result<ResolvedLinuxRules> {
    for denied in &policy.deny_read_paths {
        let denied = std::fs::canonicalize(denied).unwrap_or_else(|_| denied.clone());
        if SYSTEM_READ_ROOTS
            .iter()
            .any(|runtime| denied.starts_with(runtime))
        {
            return Err(io::Error::other(
                "Landlock runtime overlaps denied data; bubblewrap required",
            ));
        }
    }
    let mut read_only_paths = Vec::new();
    for runtime in SYSTEM_READ_ROOTS {
        if Path::new(runtime).exists() {
            read_only_paths.push(open_runtime(Path::new(runtime))?);
        }
    }
    let root = std::fs::canonicalize(&policy.workspace_root)?;
    let workspace_fd = open_runtime(&root)?;
    let mut read_write_paths = Vec::new();
    // Never grant recursive workspace READ_FILE: future private subtrees remain
    // denied. Listing via native tools remains available through the file-tool API.
    for entry in crate::sandbox::workspace_entries(policy)? {
        if !entry.directory {
            let file = open_workspace_file(
                &workspace_fd,
                entry
                    .path
                    .strip_prefix(&root)
                    .map_err(|_| io::Error::other("workspace file escaped root"))?,
            )?;
            if entry.writable {
                read_write_paths.push(file);
            } else {
                read_only_paths.push(file);
            }
        }
    }
    read_write_paths.push(open_runtime(Path::new("/dev/null"))?);
    Ok(ResolvedLinuxRules {
        read_only_paths,
        read_write_paths,
    })
}
fn open_runtime(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("invalid runtime path"))?;
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn open_workspace_file(root: &File, relative: &Path) -> io::Result<File> {
    let parts: Vec<_> = relative.components().collect();
    let mut current = root.try_clone()?;
    for (index, part) in parts.iter().enumerate() {
        let std::path::Component::Normal(name) = part else {
            return Err(io::Error::other("invalid workspace component"));
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::other("invalid workspace component"))?;
        let flags = libc::O_PATH
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if index + 1 == parts.len() {
                0
            } else {
                libc::O_DIRECTORY
            };
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { File::from_raw_fd(fd) };
    }
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(current.as_raw_fd(), &raw mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(io::Error::other("workspace file is an alias"));
    }
    Ok(current)
}

fn landlock_abi_version() -> Option<i32> {
    // SAFETY: `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` queries ABI.
    let ver = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<LandlockRulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ver > 0 {
        i32::try_from(ver).ok()
    } else {
        None
    }
}

fn apply_landlock_rules(abi: i32, rules: &ResolvedLinuxRules) -> io::Result<()> {
    // Enable `PR_SET_NO_NEW_PRIVS` first; required by `landlock_restrict_self`.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut handled: u64 = LANDLOCK_ACCESS_FS_EXECUTE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        handled |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        handled |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }

    let read_access =
        LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;
    let rw_access = handled;

    let attr = LandlockRulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::from_ref(&attr),
            std::mem::size_of::<LandlockRulesetAttr>(),
            0u32,
        )
    };
    if ruleset_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let ruleset_fd_i32 =
        i32::try_from(ruleset_fd).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;

    let add_path_list = |paths: &[File], access: u64| -> io::Result<()> {
        for file in paths {
            let fd = file.as_raw_fd();
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &raw mut stat) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let allowed_access = if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                access
            } else {
                access
                    & (LANDLOCK_ACCESS_FS_READ_FILE
                        | LANDLOCK_ACCESS_FS_WRITE_FILE
                        | LANDLOCK_ACCESS_FS_EXECUTE
                        | LANDLOCK_ACCESS_FS_TRUNCATE)
            };
            let path_attr = LandlockPathBeneathAttr {
                allowed_access,
                parent_fd: fd,
            };
            if unsafe {
                libc::syscall(
                    SYS_LANDLOCK_ADD_RULE,
                    ruleset_fd_i32,
                    LANDLOCK_RULE_PATH_BENEATH,
                    std::ptr::from_ref(&path_attr),
                    0u32,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    };

    if let Err(err) = add_path_list(&rules.read_only_paths, read_access)
        .and_then(|()| add_path_list(&rules.read_write_paths, rw_access))
    {
        unsafe {
            libc::close(ruleset_fd_i32);
        }
        return Err(err);
    }

    let restricted = unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd_i32, 0u32) };
    let restrict_err = if restricted != 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        libc::close(ruleset_fd_i32);
    }
    if let Some(err) = restrict_err {
        return Err(err);
    }
    Ok(())
}

fn apply_seccomp_no_new_privs(filter: &[SockFilter]) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let prog = SockFprog {
        len: u16::try_from(filter.len()).map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?,
        filter: filter.as_ptr(),
    };
    if unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            0u32,
            std::ptr::from_ref(&prog),
        )
    } != 0
    {
        // Fallback for older kernels via `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog)`.
        if unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                std::ptr::from_ref(&prog),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[allow(clippy::too_many_lines)] // Architecture check, explicit deny set and branch offsets form one BPF program.
fn build_seccomp_bpf() -> Option<Vec<SockFilter>> {
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;

    const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
    const SECCOMP_RET_ERRNO_EPERM: u32 = 0x0005_0000 | (libc::EPERM as u32 & 0xFFFF);
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

    #[cfg(target_arch = "x86_64")]
    const EXPECTED_ARCH: u32 = 0xC000_003E;
    #[cfg(target_arch = "aarch64")]
    const EXPECTED_ARCH: u32 = 0xC000_00B7;

    let denied_syscalls: &[u32] = &[
        u32::try_from(libc::SYS_ptrace).ok()?,
        u32::try_from(libc::SYS_process_vm_readv).ok()?,
        u32::try_from(libc::SYS_process_vm_writev).ok()?,
        u32::try_from(libc::SYS_kexec_load).ok()?,
        u32::try_from(libc::SYS_mount).ok()?,
        u32::try_from(libc::SYS_umount2).ok()?,
        u32::try_from(libc::SYS_pivot_root).ok()?,
        425, // io_uring_setup
        426, // io_uring_enter
        427, // io_uring_register
        u32::try_from(libc::SYS_bpf).ok()?,
        u32::try_from(libc::SYS_socket).ok()?,
        u32::try_from(libc::SYS_socketpair).ok()?,
        u32::try_from(libc::SYS_connect).ok()?,
        u32::try_from(libc::SYS_bind).ok()?,
        u32::try_from(libc::SYS_listen).ok()?,
        u32::try_from(libc::SYS_setns).ok()?,
        u32::try_from(libc::SYS_unshare).ok()?,
        u32::try_from(libc::SYS_ioctl).ok()?,
        u32::try_from(libc::SYS_fchmod).ok()?,
        u32::try_from(libc::SYS_fchmodat).ok()?,
        u32::try_from(libc::SYS_fchown).ok()?,
        u32::try_from(libc::SYS_fchownat).ok()?,
        u32::try_from(libc::SYS_setxattr).ok()?,
        u32::try_from(libc::SYS_lsetxattr).ok()?,
        u32::try_from(libc::SYS_fsetxattr).ok()?,
        u32::try_from(libc::SYS_removexattr).ok()?,
        u32::try_from(libc::SYS_lremovexattr).ok()?,
        u32::try_from(libc::SYS_fremovexattr).ok()?,
        u32::try_from(libc::SYS_utimensat).ok()?,
        u32::try_from(libc::SYS_keyctl).ok()?,
        u32::try_from(libc::SYS_add_key).ok()?,
        u32::try_from(libc::SYS_request_key).ok()?,
        428, // open_tree
        429, // move_mount
        430, // fsopen
        431, // fsconfig
        432, // fsmount
        433, // fspick
        438, // pidfd_getfd
        442, // mount_setattr
        452, // fchmodat2
        #[cfg(target_arch = "x86_64")]
        u32::try_from(libc::SYS_chmod).ok()?,
        #[cfg(target_arch = "x86_64")]
        u32::try_from(libc::SYS_chown).ok()?,
        #[cfg(target_arch = "x86_64")]
        u32::try_from(libc::SYS_lchown).ok()?,
        #[cfg(target_arch = "x86_64")]
        u32::try_from(libc::SYS_utime).ok()?,
        #[cfg(target_arch = "x86_64")]
        u32::try_from(libc::SYS_utimes).ok()?,
    ];

    let mut prog = Vec::with_capacity(4 + denied_syscalls.len() + 2);
    // Validate `seccomp_data.arch` at offset 4.
    prog.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 4,
    });
    prog.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: EXPECTED_ARCH,
    });
    prog.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    // Load `seccomp_data.nr` at offset 0.
    prog.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0,
    });

    #[cfg(target_arch = "x86_64")]
    {
        // x32 uses the same audit architecture with a separate syscall number space.
        prog.push(SockFilter {
            code: BPF_JMP | 0x40 | BPF_K,
            jt: 0,
            jf: 1,
            k: 0x4000_0000,
        });
        prog.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        });
    }
    for (idx, nr) in denied_syscalls.iter().enumerate() {
        let remaining = u8::try_from(denied_syscalls.len() - idx - 1).ok()?;
        let jt = remaining.checked_add(1)?;
        prog.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt,
            jf: 0,
            k: *nr,
        });
    }
    prog.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    prog.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO_EPERM,
    });
    Some(prog)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn build_seccomp_bpf() -> Option<Vec<SockFilter>> {
    None
}

fn find_usable_bwrap() -> Option<PathBuf> {
    // Exercise the real fallback in its own test process with bubblewrap still installed.
    #[cfg(test)]
    if std::env::var("NEXORA_TEST_LINUX_SANDBOX_MECHANISM").as_deref()
        == Ok(MECHANISM_LANDLOCK_SECCOMP)
    {
        return None;
    }

    let candidate = Path::new("/usr/bin/bwrap");
    if candidate.is_file() && can_spawn_with_bwrap(candidate) {
        return Some(candidate.to_path_buf());
    }
    None
}

fn can_spawn_with_bwrap(bwrap: &Path) -> bool {
    // Availability only; mandatory boundary verification remains separate.
    use std::time::{Duration, Instant};
    let mut cmd = Command::new(bwrap);
    cmd.args([
        "--die-with-parent",
        "--unshare-all",
        "--ro-bind",
        "/",
        "/",
        "--",
        "/bin/true",
    ])
    .env_clear()
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let Ok((mut child, handle)) = PlatformHandle::spawn(&mut cmd) else {
        return false;
    };
    let start = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break false,
        }
    };
    let _ = handle.kill_tree();
    let _ = child.wait();
    success
}
