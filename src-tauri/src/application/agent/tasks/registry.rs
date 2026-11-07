//! Active-task registry: handles for in-flight autonomous loops.
//!
//! Split from `tasks.rs` (AC-207): [`TaskRegistry`] and its per-task
//! `ActiveTaskRun` handles live here; the CRUD service, the autonomous
//! loop, and prompt composition are siblings.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Registry (active autonomous loops: cancel flag + current run id)
// ---------------------------------------------------------------------------

/// One active autonomous loop's handles: the cooperative cancel flag the
/// loop polls, and the current step's agent `run_id` (so `stop` aborts the
/// in-flight run through [`AgentRunRegistry::cancel`], the same
/// cancellation-token path as `cancel_agent_run`).
#[derive(Debug, Default)]
pub(crate) struct ActiveTaskRun {
    cancel: AtomicBool,
    run_id: Mutex<Option<i64>>,
}

/// Active-task registry: managed Tauri state mapping `task_id` to the
/// handles of the in-flight autonomous loop. Mirrors the agent
/// [`AgentRunRegistry`] contract: at most one active loop per task.
///
/// NEX-TASK-002: the registry additionally retains the loop thread
/// [`JoinHandle`](std::thread::JoinHandle) so orderly shutdown can cancel
/// every active loop and join its thread with a bounded deadline. Like the
/// run registry, the handle is *not* removed by [`Self::release`] (the loop
/// thread calls it on its own exit path); finished handles are reaped by
/// [`Self::reap_finished`], called on every [`Self::track_thread`] and by
/// the shutdown join.
#[derive(Debug, Default)]
pub(crate) struct TaskRegistry {
    runs: Mutex<HashMap<i64, Arc<ActiveTaskRun>>>,
    threads: Mutex<HashMap<i64, std::thread::JoinHandle<()>>>,
}

impl TaskRegistry {
    /// Claim a task slot. Returns `None` when a loop is already active.
    pub(crate) fn claim(&self, task_id: i64) -> Option<Arc<ActiveTaskRun>> {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        if runs.contains_key(&task_id) {
            return None;
        }
        let entry = Arc::new(ActiveTaskRun::default());
        runs.insert(task_id, Arc::clone(&entry));
        Some(entry)
    }

    /// Release a terminated loop (every exit path of the loop thread).
    ///
    /// NEX-TASK-002: the retained thread handle is deliberately *not*
    /// removed here (see the struct docs); [`Self::reap_finished`] and
    /// [`Self::join_threads_until`] own it.
    pub(crate) fn release(&self, task_id: i64) {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&task_id);
    }

    /// Whether a loop is currently registered for `task_id`.
    #[must_use]
    pub(crate) fn is_active(&self, task_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&task_id)
    }

    /// Request cancellation: sets the flag the loop polls and reports the
    /// current step's agent `run_id` (if any) so the caller can abort it.
    /// The `active` flag is false when no loop is registered.
    pub(crate) fn request_cancel(&self, task_id: i64) -> (bool, Option<i64>) {
        let runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = runs.get(&task_id) else {
            return (false, None);
        };
        entry.cancel.store(true, Ordering::SeqCst);
        let run_id = *entry.run_id.lock().unwrap_or_else(PoisonError::into_inner);
        (true, run_id)
    }

    /// Snapshot the ids of all currently active loops (orderly-shutdown
    /// seam; see [`AgentRunRegistry::active_run_ids`](super::registry::AgentRunRegistry::active_run_ids)).
    #[must_use]
    pub(crate) fn active_task_ids(&self) -> Vec<i64> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    /// Request shutdown cancellation of every active loop (NEX-TASK-002):
    /// sets each loop's flag and snapshots its in-flight agent `run_id`
    /// (if any) so the caller can shutdown-cancel it through the agent
    /// registry's honest shutdown path. Returns one `(task_id, run_id)`
    /// pair per loop that was active.
    pub(crate) fn request_shutdown_cancel_all(&self) -> Vec<(i64, Option<i64>)> {
        self.active_task_ids()
            .iter()
            .filter_map(|task_id| {
                let (active, run_id) = self.request_cancel(*task_id);
                active.then_some((*task_id, run_id))
            })
            .collect()
    }

    /// Retain the spawned loop-thread handle for `task_id` (NEX-TASK-002).
    /// Finished handles of *other* tasks are reaped first so the map stays
    /// bounded by the live set.
    pub(crate) fn track_thread(&self, task_id: i64, handle: std::thread::JoinHandle<()>) {
        self.reap_finished();
        self.threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(task_id, handle);
    }

    /// Join and remove every already-finished retained handle (non-blocking).
    /// Returns how many handles are still live.
    pub(crate) fn reap_finished(&self) -> usize {
        let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        let finished: Vec<i64> = threads
            .iter()
            .filter_map(|(task_id, handle)| handle.is_finished().then_some(*task_id))
            .collect();
        for task_id in finished {
            if let Some(handle) = threads.remove(&task_id) {
                if handle.join().is_err() {
                    // A panicked loop thread: its registry entry was never
                    // released and its row stays `running` — the startup
                    // sweep (crash fallback) reconciles it. Never panic the
                    // reaper for a dead thread.
                    log::warn!("task {task_id}: retained thread panicked");
                }
            }
        }
        threads.len()
    }

    /// Join retained loop-thread handles until none remain or `deadline`
    /// passes (NEX-TASK-002 bounded quit: never hangs the shutdown path).
    /// Returns how many handles are still unjoined past the deadline.
    pub(crate) fn join_threads_until(&self, deadline: Instant) -> usize {
        loop {
            let live = self.reap_finished();
            if live == 0 || Instant::now() >= deadline {
                return live;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// How many loop-thread handles are currently retained (test seam).
    #[must_use]
    pub(crate) fn tracked_thread_count(&self) -> usize {
        self.threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether cancellation was requested for `task_id` (false when no loop
    /// is active).
    #[must_use]
    pub(crate) fn is_cancelled(&self, task_id: i64) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&task_id)
            .is_some_and(|entry| entry.cancel.load(Ordering::SeqCst))
    }

    /// Record the current step's agent `run_id` (cleared with `None` when
    /// the step terminates).
    pub(crate) fn set_run_id(&self, task_id: i64, run_id: Option<i64>) {
        if let Some(entry) = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&task_id)
        {
            *entry.run_id.lock().unwrap_or_else(PoisonError::into_inner) = run_id;
        }
    }
}
