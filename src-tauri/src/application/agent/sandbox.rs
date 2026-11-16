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
    invalidated: bool,
}
fn entries() -> &'static RwLock<HashMap<PathBuf, Entry>> {
    static ENTRIES: OnceLock<RwLock<HashMap<PathBuf, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(RwLock::default)
}
fn probe_lock() -> &'static Mutex<()> {
    static PROBE_LOCK: Mutex<()> = Mutex::new(());
    &PROBE_LOCK
}
#[cfg(test)]
fn test_probes() -> &'static RwLock<HashMap<PathBuf, SandboxVerificationStatus>> {
    static TEST_PROBES: OnceLock<RwLock<HashMap<PathBuf, SandboxVerificationStatus>>> =
        OnceLock::new();
    TEST_PROBES.get_or_init(RwLock::default)
}
fn root(path: &Path) -> PathBuf {
    crate::application::workspace::strip_verbatim(
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    )
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
fn is_ttl_only_expired(path: &Path) -> bool {
    let fresh = fingerprint(path);
    entries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .is_some_and(|entry| {
            !entry.invalidated
                && entry.fingerprint == fresh
                && entry.checked.elapsed() >= Duration::from_secs(30)
        })
}
/// Re-verifies a cached entry when only its 30s TTL elapsed so parked consents
/// stay valid unless posture, policy, or environment actually changed.
pub(crate) fn refresh_if_expired(path: &Path) {
    let path = root(path);
    if !is_ttl_only_expired(&path) {
        return;
    }
    let _probe = probe_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if is_ttl_only_expired(&path) {
        refresh_locked(&path, false);
    }
}
/// Returns `(posture, identity)` atomically after refreshing any TTL-expired entry.
pub(crate) fn posture_and_identity(path: &Path) -> (&'static str, String) {
    let path = root(path);
    #[cfg(test)]
    let mut expired_read_observer = None;
    let mut retry_expired = true;
    let result = loop {
        refresh_if_expired(&path);
        #[cfg(test)]
        if expired_read_observer.is_none() {
            expired_read_observer = expire_identity_read_if_requested(&path);
        }
        let fresh = fingerprint(&path);
        let map = entries()
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (posture, revision) = match map.get(&path) {
            Some(entry)
                if entry.fingerprint == fresh
                    && entry.checked.elapsed() < Duration::from_secs(30) =>
            {
                (entry.status.posture, entry.revision)
            }
            Some(entry) if retry_expired && !entry.invalidated && entry.fingerprint == fresh => {
                // TTL can elapse after refresh_if_expired's check. Re-probe rather
                // than emitting a transient identity change or trusting stale Verified.
                // Retry once; another expiry or any real identity change fails closed.
                retry_expired = false;
                drop(map);
                continue;
            }
            Some(entry) => ("PerCommandConsent", entry.revision),
            None => ("PerCommandConsent", 0),
        };
        break (posture, format!("{fresh}:{posture}:{revision}"));
    };
    #[cfg(test)]
    if let Some(observer) = expired_read_observer {
        let _ = observer.send(());
    }
    result
}
fn verify_probe(path: &Path) -> SandboxVerificationStatus {
    #[cfg(test)]
    if let Some(status) = test_probes()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .cloned()
    {
        return status;
    }
    nexora_os_exec::verify_platform_sandbox(path)
}
fn refresh_locked(path: &Path, allow_clearing_invalidated: bool) -> SandboxStatus {
    let before = fingerprint(path);
    let verification = verify_probe(path);
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
            ..fallback(path, "native sandbox unavailable")
        },
        SandboxVerificationStatus::Failed { reason } => SandboxStatus {
            state: "failed",
            reason: Some(reason),
            ..fallback(path, "native boundary verification failed")
        },
    };
    let after = fingerprint(path);
    if before != after {
        status = fallback(path, "sandbox policy changed during verification");
    }
    let mut entries = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !allow_clearing_invalidated {
        if let Some(old) = entries.get(path).filter(|old| old.invalidated) {
            return old.status.clone();
        }
    }
    let revision = entries.get(path).map_or(1, |old| {
        old.revision
            + u64::from(old.invalidated || old.fingerprint != after || old.status != status)
    });
    entries.insert(
        path.to_path_buf(),
        Entry {
            status: status.clone(),
            fingerprint: after,
            revision,
            checked: Instant::now(),
            invalidated: false,
        },
    );
    status
}
/// Startup, workspace changes, dispatch and final authorization actively verify the same policy.
pub(crate) fn refresh(path: &Path) -> SandboxStatus {
    let _probe = probe_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = root(path);
    refresh_locked(&path, true)
}
pub(crate) fn invalidate(path: &Path) {
    let path = root(path);
    let mut map = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = map.get_mut(&path) {
        entry.status = fallback(
            &path,
            "sandbox launch failed; command consent must be renewed",
        );
        entry.revision += 1;
        entry.checked = Instant::now();
        entry.invalidated = true;
    } else {
        map.insert(
            path.clone(),
            Entry {
                status: fallback(
                    &path,
                    "sandbox launch failed; command consent must be renewed",
                ),
                fingerprint: fingerprint(&path),
                revision: 1,
                checked: Instant::now(),
                invalidated: true,
            },
        );
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
#[cfg(test)]
thread_local! {
    static FORCE_SANDBOX_SPAWN_FAIL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static IDENTITY_READ_EXPIRY: RefCell<Option<TestIdentityReadExpiry>> = const { RefCell::new(None) };
}
#[cfg(test)]
struct TestIdentityReadExpiry {
    reads_to_skip: usize,
    observer: Option<std::sync::mpsc::Sender<()>>,
}
/// Cross the TTL boundary after the refresh check, without scheduler-dependent sleeps.
#[cfg(test)]
pub(crate) fn expire_identity_read_for_test(
    reads_to_skip: usize,
    observer: Option<std::sync::mpsc::Sender<()>>,
) {
    IDENTITY_READ_EXPIRY.with(|slot| {
        *slot.borrow_mut() = Some(TestIdentityReadExpiry {
            reads_to_skip,
            observer,
        });
    });
}
#[cfg(test)]
fn expire_identity_read_if_requested(path: &Path) -> Option<std::sync::mpsc::Sender<()>> {
    let expiry = IDENTITY_READ_EXPIRY.with(|slot| {
        let mut slot = slot.borrow_mut();
        let expiry = slot.as_mut()?;
        if expiry.reads_to_skip > 0 {
            expiry.reads_to_skip -= 1;
            return None;
        }
        slot.take()
    });
    if let Some(expiry) = expiry {
        age_test_status(path, Duration::from_secs(31));
        expiry.observer
    } else {
        None
    }
}
#[cfg(test)]
pub(crate) struct TestSandboxGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}
#[cfg(test)]
impl Drop for TestSandboxGuard {
    fn drop(&mut self) {
        FORCE_SANDBOX_SPAWN_FAIL.with(|flag| flag.set(false));
        IDENTITY_READ_EXPIRY.with(|slot| *slot.borrow_mut() = None);
        PERMIT.with(|slot| *slot.borrow_mut() = None);
        entries()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        test_probes()
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}
/// Serializes tests that mutate global `#[cfg(test)]` sandbox overrides and clears them on drop.
#[cfg(test)]
#[must_use]
pub(crate) fn test_lock() -> TestSandboxGuard {
    static TEST_SANDBOX_LOCK: Mutex<()> = Mutex::new(());
    let guard = TEST_SANDBOX_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    FORCE_SANDBOX_SPAWN_FAIL.with(|flag| flag.set(false));
    IDENTITY_READ_EXPIRY.with(|slot| *slot.borrow_mut() = None);
    PERMIT.with(|slot| *slot.borrow_mut() = None);
    entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    test_probes()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    TestSandboxGuard { _guard: guard }
}
#[cfg(test)]
pub(crate) fn has_permit() -> bool {
    PERMIT.with(|slot| slot.borrow().is_some())
}
#[cfg(test)]
pub(crate) fn fail_next_sandboxed_spawn() {
    FORCE_SANDBOX_SPAWN_FAIL.with(|flag| flag.set(true));
}
#[cfg(test)]
pub(crate) fn set_test_probe(path: &Path, verification: Option<SandboxVerificationStatus>) {
    let key = root(path);
    let mut probes = test_probes()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(status) = verification {
        probes.insert(key, status);
    } else {
        probes.remove(&key);
    }
}
#[cfg(test)]
pub(crate) fn age_test_status(path: &Path, age: Duration) {
    let key = root(path);
    let checked = Instant::now()
        .checked_sub(age)
        .expect("system uptime exceeds synthetic test age");
    if let Some(entry) = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&key)
    {
        entry.checked = checked;
    }
}
#[cfg(test)]
pub(crate) fn set_test_status(path: &Path, verified: bool, age: Duration) -> PathBuf {
    set_test_status_with_fingerprint(path, verified, None, age)
}
#[cfg(test)]
pub(crate) fn set_test_status_with_fingerprint(
    path: &Path,
    verified: bool,
    fingerprint_override: Option<&str>,
    age: Duration,
) -> PathBuf {
    let key = root(path);
    let status = if verified {
        SandboxStatus {
            workspace_root: key.to_string_lossy().into_owned(),
            state: "verified",
            posture: "Verified",
            mechanism: Some("test-sandbox".into()),
            reason: None,
        }
    } else {
        fallback(&key, "test-unverified")
    };
    let fp = fingerprint_override.map_or_else(|| fingerprint(&key), str::to_owned);
    let checked = Instant::now()
        .checked_sub(age)
        .expect("system uptime exceeds synthetic test age");
    let mut map = entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let revision = map.get(&key).map_or(1, |old| old.revision + 1);
    map.insert(
        key.clone(),
        Entry {
            status,
            fingerprint: fp,
            revision,
            checked,
            invalidated: false,
        },
    );
    key
}
#[cfg(test)]
pub(crate) fn clear_test_status(path: &Path) {
    let key = root(path);
    entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&key);
    test_probes()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&key);
}
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
        #[cfg(test)]
        if FORCE_SANDBOX_SPAWN_FAIL.with(|flag| flag.replace(false)) {
            Err(std::io::Error::other("injected sandbox launch failure"))
        } else {
            nexora_os_exec::spawn_sandboxed_in_process_tree(
                command,
                &SandboxSpawnConfig::for_workspace(path),
            )
        }
        #[cfg(not(test))]
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
        let _sandbox_lock = test_lock();
        let path = std::env::temp_dir().join("nexora-sandbox-identity-only");
        let key = root(&path);
        entries().write().unwrap().insert(
            key.clone(),
            Entry {
                status: fallback(&key, "test"),
                fingerprint: fingerprint(&key),
                revision: 1,
                checked: Instant::now(),
                invalidated: false,
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
    #[test]
    fn root_strips_windows_verbatim_prefix_to_match_workspace_root() {
        let ws = super::super::tools::test_support::temp_workspace();
        let expected = crate::application::workspace::strip_verbatim(
            std::fs::canonicalize(&ws).expect("canonicalize temp workspace"),
        );
        let resolved = root(&ws);
        assert_eq!(resolved, expected);
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "sandbox root must strip verbatim prefix: {resolved:?}"
        );
        #[cfg(windows)]
        {
            let verbatim = PathBuf::from(format!(r"\\?\{}", expected.display()));
            assert_eq!(root(&verbatim), expected);
            assert_eq!(
                status(&verbatim).workspace_root,
                expected.to_string_lossy().as_ref()
            );
        }
        std::fs::remove_dir_all(&ws).unwrap();
    }
    #[test]
    fn ttl_expiry_and_fingerprint_mismatch_drop_verified_to_per_command_consent() {
        let _sandbox_lock = test_lock();
        let ws = super::super::tools::test_support::temp_workspace();
        set_test_status(&ws, true, Duration::ZERO);
        let fresh = status(&ws);
        assert_eq!(fresh.state, "verified");
        assert_eq!(fresh.posture, "Verified");

        set_test_status(&ws, true, Duration::from_secs(31));
        let expired = status(&ws);
        assert_eq!(expired.state, "stale");
        assert_eq!(expired.posture, "PerCommandConsent");

        set_test_status_with_fingerprint(&ws, true, Some("stale-fingerprint"), Duration::ZERO);
        let mismatched = status(&ws);
        assert_eq!(mismatched.state, "stale");
        assert_eq!(mismatched.posture, "PerCommandConsent");

        clear_test_status(&ws);
        std::fs::remove_dir_all(&ws).unwrap();
    }
    #[test]
    fn failed_sandboxed_launch_invalidates_verified_posture_and_fails_closed() {
        let _sandbox_lock = test_lock();
        let ws = super::super::tools::test_support::temp_workspace();
        set_test_status(&ws, true, Duration::ZERO);
        assert_eq!(status(&ws).posture, "Verified");

        fail_next_sandboxed_spawn();
        let outcome = authorized(&ws, true, || {
            spawn_authorized(&mut Command::new("never-executed"), &ws)
        });
        assert!(
            matches!(outcome, Err(super::super::tools::ToolError::Io(_))),
            "failed sandboxed launch must reject fail-closed: {outcome:?}"
        );
        assert_eq!(
            status(&ws).posture,
            "PerCommandConsent",
            "failed sandboxed launch must invalidate Verified posture"
        );

        set_test_status(&ws, true, Duration::ZERO);
        let invalidated = authorized(&ws, true, || {
            invalidate(&ws);
            spawn_authorized(&mut Command::new("never-executed"), &ws)
        });
        assert!(matches!(
            invalidated,
            Err(super::super::tools::ToolError::ConsentInvalidated)
        ));

        clear_test_status(&ws);
        std::fs::remove_dir_all(&ws).unwrap();
    }
    #[test]
    fn refresh_preserves_revision_across_ttl_expiry_when_probe_and_fingerprint_match() {
        let _sandbox_lock = test_lock();
        let ws = super::super::tools::test_support::temp_workspace();
        set_test_probe(
            &ws,
            Some(SandboxVerificationStatus::Verified {
                mechanism: "test-sandbox".into(),
            }),
        );
        assert_eq!(refresh(&ws).posture, "Verified");
        let id_verified = identity(&ws);

        // Pure TTL expiry drops read-only status() to PerCommandConsent,
        // while refresh() / posture_and_identity() re-verify without bumping revision.
        age_test_status(&ws, Duration::from_secs(31));
        assert_eq!(status(&ws).posture, "PerCommandConsent");
        assert_eq!(posture_and_identity(&ws), ("Verified", id_verified.clone()));
        age_test_status(&ws, Duration::from_secs(31));
        assert_eq!(refresh(&ws).posture, "Verified");
        assert_eq!(identity(&ws), id_verified);

        // Real posture/status/reason change must bump revision.
        set_test_probe(
            &ws,
            Some(SandboxVerificationStatus::Unavailable {
                reason: "reason-a".into(),
            }),
        );
        assert_eq!(refresh(&ws).posture, "PerCommandConsent");
        let id_unavail_a = identity(&ws);
        assert_ne!(id_unavail_a, id_verified);

        // TTL expiry under unchanged Unavailable keeps the same revision.
        age_test_status(&ws, Duration::from_secs(31));
        assert_eq!(refresh(&ws).posture, "PerCommandConsent");
        assert_eq!(identity(&ws), id_unavail_a);

        // Changed reason bumps revision.
        set_test_probe(
            &ws,
            Some(SandboxVerificationStatus::Unavailable {
                reason: "reason-b".into(),
            }),
        );
        assert_eq!(refresh(&ws).posture, "PerCommandConsent");
        let id_unavail_b = identity(&ws);
        assert_ne!(id_unavail_b, id_unavail_a);

        // Explicit invalidate() stays invalidated across refresh_if_expired and bumps revision on refresh().
        invalidate(&ws);
        let id_invalidated = identity(&ws);
        assert_ne!(id_invalidated, id_unavail_b);
        age_test_status(&ws, Duration::from_secs(31));
        refresh_if_expired(&ws);
        assert_eq!(identity(&ws), id_invalidated);
        refresh(&ws);
        assert_ne!(identity(&ws), id_unavail_b);
        assert_ne!(identity(&ws), id_invalidated);

        clear_test_status(&ws);
        std::fs::remove_dir_all(&ws).unwrap();
    }

    #[test]
    fn identity_read_rechecks_ttl_expiring_after_refresh_check() {
        let _sandbox_lock = test_lock();
        let ws = super::super::tools::test_support::temp_workspace();
        set_test_probe(
            &ws,
            Some(SandboxVerificationStatus::Verified {
                mechanism: "test-sandbox".into(),
            }),
        );
        assert_eq!(refresh(&ws).posture, "Verified");
        let verified = posture_and_identity(&ws);

        expire_identity_read_for_test(0, None);
        assert_eq!(posture_and_identity(&ws), verified);
        assert_eq!(status(&ws).posture, "Verified");

        // Re-verification must actually run; returning the expired Verified entry
        // would incorrectly preserve identity when the boundary probe degrades.
        set_test_probe(
            &ws,
            Some(SandboxVerificationStatus::Unavailable {
                reason: "degraded at the TTL boundary".into(),
            }),
        );
        expire_identity_read_for_test(0, None);
        let degraded = posture_and_identity(&ws);
        assert_eq!(degraded.0, "PerCommandConsent");
        assert_ne!(degraded.1, verified.1);
        assert_eq!(status(&ws).state, "unavailable");

        clear_test_status(&ws);
        std::fs::remove_dir_all(&ws).unwrap();
    }
}
