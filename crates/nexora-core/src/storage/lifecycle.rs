//! Cross-process lifecycle rules: instance lease + orphan-sweep safety.
//!
//! The desktop sweeps `running` agent runs/tasks to an error state at startup
//! (DP-8, NEX-TASK-001). That assumption — "anything still running belongs to
//! a dead process" — is only true when this process is the only live instance.
//! With two windows (or one window plus a CLI writer) sharing one database,
//! the second startup would otherwise mark the first one's in-flight work as
//! interrupted.
//!
//! The rule here is an OS advisory lease beside the database:
//!
//! - every running process holds a **shared** lock on `<db>.instance.lock`
//!   for its lifetime (the OS releases it on crash, so there is no stale
//!   lease to reap);
//! - the first process to start tries an **exclusive** lock. Success means
//!   "no other instance is alive" and only then may it sweep. It downgrades
//!   to shared after the sweep so later starts can share the lease;
//! - a later process fails the exclusive attempt (`WouldBlock`) and skips the
//!   sweep, returning [`OrphanSweep::skipped_live_instance`] instead of
//!   touching rows it does not own.
//!
//! The migration-leadership lock in [`super::database::open`] is a separate
//! file: sweeping and migrating are different critical sections.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use super::database::{Database, DatabaseError};
use super::repository::agent_runs::AgentRunRepository;
use super::repository::agent_tasks::AgentTaskRepository;

/// Fixed reason stored on a run swept by startup (the exact string the UI
/// already shows; do not reword without a migration plan).
pub const ORPHAN_RUN_MESSAGE: &str = "run interrupted by application shutdown";

/// Fixed reason stored on a task swept by startup.
pub const ORPHAN_TASK_MESSAGE: &str = "task interrupted by application shutdown";

/// Result of a startup orphan sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrphanSweep {
    /// Runs transitioned `running` → `error`.
    pub runs: usize,
    /// Tasks transitioned `running` → `failed`.
    pub tasks: usize,
    /// True when another live instance exists, so the sweep was skipped —
    /// no row was touched.
    pub skipped_live_instance: bool,
}

/// The per-process instance lease, held until the process exits.
#[derive(Debug)]
pub struct InstanceLease {
    file: File,
    first: bool,
}

fn instance_lock_path(db_path: &Path) -> PathBuf {
    let name = db_path.file_name().map_or_else(
        || "nexora.db".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    db_path.with_file_name(format!("{name}.instance.lock"))
}

impl InstanceLease {
    /// Take the instance lease beside `db_path`.
    ///
    /// The first live process holds an exclusive advisory lock until
    /// [`Self::become_shared`] is called; later processes hold a shared lock
    /// for their lifetime. A crash releases the OS lock automatically.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError::Io`] when the lock file cannot be created or
    /// locked.
    pub fn acquire(db_path: &Path) -> Result<Self, DatabaseError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(instance_lock_path(db_path))
            .map_err(DatabaseError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, first: true }),
            Err(std::fs::TryLockError::WouldBlock) => {
                // Another live process owns a lock. Share the lease without
                // ever claiming the right to sweep.
                file.lock_shared().map_err(DatabaseError::Io)?;
                Ok(Self { file, first: false })
            }
            Err(std::fs::TryLockError::Error(err)) => Err(DatabaseError::Io(err)),
        }
    }

    /// True when this process is the only live instance (exclusive lock held).
    #[must_use]
    pub fn is_first(&self) -> bool {
        self.first
    }

    /// Downgrade the first instance's exclusive lock to shared for the rest of
    /// the process lifetime. No-op for later instances.
    ///
    /// # Errors
    ///
    /// Returns [`DatabaseError::Io`] if the OS lock cannot be converted.
    pub fn become_shared(&mut self) -> Result<(), DatabaseError> {
        if self.first {
            self.file.unlock().map_err(DatabaseError::Io)?;
            self.file.lock_shared().map_err(DatabaseError::Io)?;
            self.first = false;
        }
        Ok(())
    }
}

/// Sweep orphaned runs/tasks only when no other instance is alive.
///
/// A later live instance returns `skipped_live_instance = true` and touches
/// nothing, regardless of row age: a live process may legitimately hold a
/// `running` row for hours.
///
/// # Errors
///
/// Propagates [`DatabaseError`] from the two repository updates. Callers that
/// prefer to continue starting up (the desktop does) should log and proceed.
pub fn sweep_orphans(db: &Database, lease: &InstanceLease) -> Result<OrphanSweep, DatabaseError> {
    if !lease.is_first() {
        return Ok(OrphanSweep {
            runs: 0,
            tasks: 0,
            skipped_live_instance: true,
        });
    }
    let runs = AgentRunRepository::new(db).fail_orphaned_running_runs(ORPHAN_RUN_MESSAGE)?;
    let tasks = AgentTaskRepository::new(db).fail_orphaned_running_tasks(ORPHAN_TASK_MESSAGE)?;
    Ok(OrphanSweep {
        runs,
        tasks,
        skipped_live_instance: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::{configure, migrate};
    use crate::storage::repository::agent_runs::AgentRunRepository;
    use crate::storage::repository::agent_tasks::AgentTaskRepository;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "nexora-lifecycle-{name}-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let path = dir.join("nexora.db");
            let mut conn = Connection::open(&path).expect("open");
            configure(&conn).expect("configure");
            migrate(&mut conn).expect("migrate");
            Self { dir, path }
        }

        fn db(&self) -> Database {
            let conn = Connection::open(&self.path).expect("reopen");
            Database::new(conn)
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn seed_running_run(db: &Database) -> i64 {
        let runs = AgentRunRepository::new(db);
        runs.create_run(None, "m", "semi_autonomous").expect("run")
    }

    fn run_status(db: &Database, id: i64) -> String {
        AgentRunRepository::new(db)
            .read_run(id)
            .expect("read")
            .expect("exists")
            .status
    }

    #[test]
    fn first_instance_sweeps_and_downgrades() {
        let temp = TempDb::new("first");
        let db = temp.db();
        let run_id = seed_running_run(&db);
        let mut lease = InstanceLease::acquire(&temp.path).expect("lease");
        assert!(lease.is_first());
        let sweep = sweep_orphans(&db, &lease).expect("sweep");
        assert_eq!(sweep.runs, 1, "the only live instance may sweep");
        assert!(!sweep.skipped_live_instance);
        assert_eq!(run_status(&db, run_id), "error");
        lease.become_shared().expect("downgrade");
        assert!(!lease.is_first());
    }

    #[test]
    fn sweep_is_skipped_while_another_instance_is_alive() {
        let temp = TempDb::new("live-peer");
        let db_a = temp.db();
        let db_b = temp.db();
        let run_id = seed_running_run(&db_a);

        let mut first = InstanceLease::acquire(&temp.path).expect("first lease");
        first.become_shared().expect("first downgrades");
        let second = InstanceLease::acquire(&temp.path).expect("second lease");
        assert!(!second.is_first());

        let sweep = sweep_orphans(&db_b, &second).expect("skip sweep");
        assert!(sweep.skipped_live_instance);
        assert_eq!(sweep.runs, 0);
        assert_eq!(
            run_status(&db_b, run_id),
            "running",
            "a live peer's run must not be marked interrupted"
        );
    }

    #[test]
    fn sweep_runs_after_all_peers_are_gone() {
        let temp = TempDb::new("peer-gone");
        let db = temp.db();
        let run_id = seed_running_run(&db);

        {
            let mut first = InstanceLease::acquire(&temp.path).expect("first");
            first.become_shared().expect("downgrade");
            let second = InstanceLease::acquire(&temp.path).expect("second");
            assert!(!second.is_first());
            drop(second);
            drop(first);
        }

        let mut fresh = InstanceLease::acquire(&temp.path).expect("fresh");
        assert!(fresh.is_first(), "no live lock holder remains");
        let sweep = sweep_orphans(&db, &fresh).expect("sweep");
        assert_eq!(sweep.runs, 1);
        assert_eq!(run_status(&db, run_id), "error");
        fresh.become_shared().expect("downgrade");
    }

    #[test]
    fn task_sweep_obeys_the_same_gate() {
        let temp = TempDb::new("task-gate");
        let db = temp.db();
        let tasks = AgentTaskRepository::new(&db);
        let id = tasks
            .create_task("t", None, None, None, None, 25)
            .expect("task");
        tasks.mark_task_running(id, 1).expect("running");

        let mut first = InstanceLease::acquire(&temp.path).expect("first");
        first.become_shared().expect("downgrade");
        let second = InstanceLease::acquire(&temp.path).expect("second");
        let skipped = sweep_orphans(&db, &second).expect("skip");
        assert!(skipped.skipped_live_instance);
        assert_eq!(
            tasks.read_task(id).expect("read").expect("exists").status,
            "running"
        );
        drop(second);
        drop(first);

        let fresh = InstanceLease::acquire(&temp.path).expect("fresh");
        let swept = sweep_orphans(&db, &fresh).expect("sweep");
        assert_eq!(swept.tasks, 1);
        assert_eq!(
            tasks.read_task(id).expect("read").expect("exists").status,
            "failed"
        );
    }
}
