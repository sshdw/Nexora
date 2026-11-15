use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sandbox::{
    collect_sanitized_env, ProbeSpec, SandboxPolicy, SandboxSpawnConfig, SandboxVerificationStatus,
};
use crate::unix::PlatformHandle;

pub(crate) const MECHANISM_NAME: &str = "macos-seatbelt";
const SEATBELT_PATH: &str = "/usr/bin/sandbox-exec";

static PROFILE_SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) struct SeatbeltProfileGuard(PathBuf);

impl Drop for SeatbeltProfileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

pub(crate) fn probe_mechanism_available() -> Result<&'static str, SandboxVerificationStatus> {
    if !Path::new(SEATBELT_PATH).is_file() {
        return Err(SandboxVerificationStatus::Unavailable {
            reason: format!("macOS Seatbelt binary missing at {SEATBELT_PATH}"),
        });
    }
    Ok(MECHANISM_NAME)
}

pub(crate) fn spawn_sandboxed(
    command: &mut Command,
    config: &SandboxSpawnConfig,
) -> io::Result<(Child, PlatformHandle, SeatbeltProfileGuard)> {
    let policy = &config.policy;
    let profile_text = render_seatbelt_profile(policy)?;
    let profile_path = write_temp_profile(&profile_text)?;
    let guard = SeatbeltProfileGuard(profile_path.clone());

    let cwd = command
        .get_current_dir()
        .map_or_else(|| policy.workspace_root.clone(), Path::to_path_buf);
    let env_entries = collect_sanitized_env(command, policy.sanitize_env);

    let mut wrapped = Command::new(SEATBELT_PATH);
    wrapped
        .arg("-f")
        .arg(&profile_path)
        .arg(command.get_program())
        .args(command.get_args())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (k, v) in env_entries {
        wrapped.env(k, v);
    }

    let (child, handle) = PlatformHandle::spawn(&mut wrapped)?;
    Ok((child, handle, guard))
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

pub(crate) fn render_seatbelt_profile(policy: &SandboxPolicy) -> io::Result<String> {
    let mut sbpl = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read* (subpath \"/System\") (subpath \"/usr/lib\") (subpath \"/usr/bin\") (subpath \"/bin\") (subpath \"/sbin\") (subpath \"/Library/Apple\") (literal \"/dev/null\") (literal \"/dev/urandom\"))\n\
         (allow file-write-data (require-all (path \"/dev/null\") (vnode-type CHARACTER-DEVICE)))\n",
    );

    // Exact existing paths only: future private names never inherit read/write access.
    for entry in crate::sandbox::workspace_entries(policy)? {
        let escaped = escape_sbpl_string(&entry.path)?;
        let _ = writeln!(sbpl, "(allow file-read* (literal \"{escaped}\"))");
        if !entry.directory && entry.writable {
            let _ = writeln!(sbpl, "(allow file-write-data (literal \"{escaped}\"))");
        }
    }

    for deny_write in &policy.deny_write_paths {
        let canon = std::fs::canonicalize(deny_write).unwrap_or_else(|_| deny_write.clone());
        let escaped = escape_sbpl_string(&canon)?;
        let _ = writeln!(sbpl, "(deny file-write* (subpath \"{escaped}\"))");
    }

    for deny_read in &policy.deny_read_paths {
        let canon = std::fs::canonicalize(deny_read).unwrap_or_else(|_| deny_read.clone());
        let escaped = escape_sbpl_string(&canon)?;
        let _ = writeln!(sbpl, "(deny file-read* (subpath \"{escaped}\"))");
        if canon != *deny_read {
            let lex = escape_sbpl_string(deny_read)?;
            let _ = writeln!(sbpl, "(deny file-read* (subpath \"{lex}\"))");
        }
    }

    Ok(sbpl)
}

fn escape_sbpl_string(path: &Path) -> io::Result<String> {
    let raw = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 path for SBPL"))?;
    if raw.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "control characters are disallowed in SBPL paths",
        ));
    }
    Ok(raw.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_temp_profile(content: &str) -> io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let seq = PROFILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("nexora-sbpl-{}-{stamp}-{seq}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let path = dir.join("policy.sbpl");
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(content.as_bytes()));
    if let Err(error) = result {
        let _ = std::fs::remove_dir(&dir);
        return Err(error);
    }
    Ok(path)
}
