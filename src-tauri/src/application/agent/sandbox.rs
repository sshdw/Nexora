//! Trusted workspace-bound sandbox posture and exact launch permits.
use nexora_os_exec::{
    ProcessTreeChild, SandboxPolicy, SandboxSpawnConfig, SandboxVerificationStatus,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SandboxStatus {
    pub workspace_root: String,
    pub state: &'static str,
    pub posture: &'static str,
    pub mechanism: Option<String>,
    pub reason: Option<String>,
}
#[derive(Clone)]
struct Entry {
    status: SandboxStatus,
    fingerprint: String,
    revision: u64,
    checked: Instant,
}
fn entries() -> &'static RwLock<HashMap<PathBuf, Entry>> {
    static ENTRIES: OnceLock<RwLock<HashMap<PathBuf, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(RwLock::default)
}
fn root(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
fn fingerprint(path: &Path) -> String {
    let policy = SandboxPolicy::for_workspace(path);
    let mut hash = Sha256::new();
    hash.update(format!("native-policy-v1:{policy:?}"));
    hash.update(super::access::environment_fingerprint());
    format!("{:x}", hash.finalize())
}
fn fallback(path: &Path, reason: &str) -> SandboxStatus {
    SandboxStatus {
        workspace_root: path.to_string_lossy().into_owned(),
        state: "stale",
        posture: "PerCommandConsent",
        mechanism: None,
        reason: Some(reason.into()),
    }
}
/// Read-only. A changed environment/policy or expired verification is never returned as Verified.
pub(crate) fn status(path: &Path) -> SandboxStatus {
    let path = root(path);
    let fresh = fingerprint(&path);
    let entries = entries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match entries.get(&path) {
        Some(entry)
            if entry.fingerprint == fresh && entry.checked.elapsed() < Duration::from_secs(30) =>
        {
            entry.status.clone()
        }
        _ => fallback(&path, "sandbox verification is missing or stale"),
    }
}
pub(crate) fn identity(path: &Path) -> String {
    let path = root(path);
    let status = status(&path);
    let revision = entries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&path)
        .map_or(0, |entry| entry.revision);
    format!("{}:{}:{revision}", fingerprint(&path), status.posture)
}
/// Startup, workspace changes, dispatch and final authorization actively verify the same policy.
pub(crate) fn refresh(path: &Path) -> SandboxStatus {
    static PROBE_LOCK: Mutex<()> = Mutex::new(());
    let _probe = PROBE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = root(path);
    let before = fingerprint(&path);
    let verification = nexora_os_exec::verify_platform_sandbox(&path);
    let mut status = match verification {
        SandboxVerificationStatus::Verified { mechanism } => SandboxStatus {
            workspace_root: path.to_string_lossy().into_owned(),
            state: "verified",
            posture: "Verified",
            mechanism: Some(mechanism),
            reason: None,
        },
        SandboxVerificationStatus::Unavailable { reason } => SandboxStatus {
            state: "unavailable",
            reason: Some(reason),
            ..fallback(&path, "native sandbox unavailable")
        },
        SandboxVerificationStatus::Failed { reason } => SandboxStatus {
            state: "failed",
            reason: Some(reason),
            ..fallback(&path, "native boundary verification failed")
        },
    };
    let after = fingerprint(&path);
    if before != after {
        status = fallback(&path, "sandbox policy changed during verification");
    }
    let mut entries = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let revision = entries.get(&path).map_or(1, |old| {
        old.revision
            + u64::from(
                old.fingerprint != after
                    || old.status != status
                    || old.checked.elapsed() >= Duration::from_secs(30),
            )
    });
    entries.insert(
        path,
        Entry {
            status: status.clone(),
            fingerprint: after,
            revision,
            checked: Instant::now(),
        },
    );
    status
}
pub(crate) fn invalidate(path: &Path) {
    let path = root(path);
    if let Some(entry) = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&path)
    {
        entry.status = fallback(
            &path,
            "sandbox launch failed; command consent must be renewed",
        );
        entry.revision += 1;
    }
}
#[derive(Clone)]
struct Permit {
    workspace: PathBuf,
    fingerprint: String,
    identity: String,
    sandboxed: bool,
}
thread_local! { static PERMIT: RefCell<Option<Permit>> = const { RefCell::new(None) }; }
/// Only the authorization gate supplies this execution capability, after exact consent validation.
pub(crate) fn authorized<T>(path: &Path, sandboxed: bool, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<Permit>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PERMIT.with(|permit| *permit.borrow_mut() = self.0.take());
        }
    }
    let permit = Permit {
        workspace: root(path),
        fingerprint: fingerprint(path),
        identity: identity(path),
        sandboxed,
    };
    let _restore = Restore(PERMIT.with(|slot| slot.replace(Some(permit))));
    action()
}
pub(crate) fn spawn_authorized(
    command: &mut Command,
    path: &Path,
) -> Result<ProcessTreeChild, super::tools::ToolError> {
    let permit = PERMIT
        .with(|slot| slot.borrow().clone())
        .ok_or(super::tools::ToolError::ConsentInvalidated)?;
    if permit.workspace != root(path)
        || permit.fingerprint != fingerprint(path)
        || permit.identity != identity(path)
    {
        return Err(super::tools::ToolError::ConsentInvalidated);
    }
    // Pin posture through the actual OS spawn; refresh/invalidation cannot silently switch this launch.
    let entries = entries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let posture = status_unlocked(&entries, &permit.workspace, &permit.fingerprint);
    let revision = entries
        .get(&permit.workspace)
        .map_or(0, |entry| entry.revision);
    if permit.identity != format!("{}:{posture}:{revision}", permit.fingerprint) {
        return Err(super::tools::ToolError::ConsentInvalidated);
    }
    let result = if permit.sandboxed {
        if status_unlocked(&entries, &permit.workspace, &permit.fingerprint) != "Verified" {
            return Err(super::tools::ToolError::ConsentInvalidated);
        }
        nexora_os_exec::spawn_sandboxed_in_process_tree(
            command,
            &SandboxSpawnConfig::for_workspace(path),
        )
    } else {
        nexora_os_exec::spawn_in_process_tree(command)
    };
    drop(entries);
    result.map_err(|_| {
        invalidate(path);
        super::tools::ToolError::Io("command launch failed; sandbox consent invalidated".into())
    })
}
fn status_unlocked(
    entries: &HashMap<PathBuf, Entry>,
    path: &Path,
    fingerprint: &str,
) -> &'static str {
    entries
        .get(path)
        .filter(|e| e.fingerprint == fingerprint && e.checked.elapsed() < Duration::from_secs(30))
        .map_or("PerCommandConsent", |e| e.status.posture)
}
/// The terminal is an explicit user command; its permission never authorizes agent automation.
pub(crate) fn spawn_terminal(
    command: &mut Command,
    path: &Path,
) -> Result<ProcessTreeChild, super::tools::ToolError> {
    let verified = refresh(path).posture == "Verified";
    authorized(path, verified, || spawn_authorized(command, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_workspace_and_missing_permit_fail_closed() {
        let path = std::env::temp_dir().join("nexora-no-such-sandbox-workspace");
        assert_eq!(status(&path).posture, "PerCommandConsent");
        assert!(matches!(
            spawn_authorized(&mut Command::new("never-executed"), &path),
            Err(super::super::tools::ToolError::ConsentInvalidated)
        ));
    }
    #[test]
    fn changed_posture_invalidates_trusted_permit() {
        let path = std::env::temp_dir().join("nexora-sandbox-identity-only");
        let key = root(&path);
        entries().write().unwrap().insert(
            key.clone(),
            Entry {
                status: fallback(&key, "test"),
                fingerprint: fingerprint(&key),
                revision: 1,
                checked: Instant::now(),
            },
        );
        authorized(&path, false, || {
            invalidate(&path);
            assert!(matches!(
                spawn_authorized(&mut Command::new("never-executed"), &path),
                Err(super::super::tools::ToolError::ConsentInvalidated)
            ));
        });
        entries().write().unwrap().remove(&key);
    }
}
