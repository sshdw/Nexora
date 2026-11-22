//! The CLI's *only* Tokio surface: an HTTP reactor behind `AsyncSpawner`.
//!
//! # Owner-approved scoped exception (#163 4b)
//!
//! `nexora-runtime` is runtime-agnostic: its provider transports spawn each
//! async HTTP attempt through [`AsyncSpawner`] and never name an async runtime.
//! The Desktop installs its own spawner; the CLI installs the one below. Tokio
//! is used **exclusively** to drive those HTTP futures — one lazily built,
//! process-wide runtime, started once, never exposed outside this module. The
//! agent loop, the run/forwarder threads and the CLI command layer stay on
//! plain `std` threads + `mpsc` + `Condvar`, and the TUI never sees Tokio at
//! all. No Tokio type crosses this module's boundary: the only value handed
//! back to the runtime is the opaque [`AbortHandle`].
//!
//! The declared feature set (`rt`, `net`, `time`) is deliberately minimal: a
//! current-thread runtime is enough for socket + timer driving, and a small
//! dedicated driver thread owns `block_on` so spawned futures make progress
//! while the synchronous run thread waits for agent frames. Tokio is MIT
//! licensed and already present in the workspace lockfile via the provider
//! transports, so this adds no copyleft dependency and no new license entry.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use nexora_runtime::{AbortHandle, AsyncSpawner};

/// Lazily built, process-wide HTTP reactor. Never dropped: provider tasks may
/// outlive a single `run` invocation's setup, and a runtime handle is cheap to
/// share.
static REACTOR: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();

fn reactor() -> &'static Arc<tokio::runtime::Runtime> {
    REACTOR.get_or_init(|| {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_name("nexora-http-reactor")
                .build()
                .expect("the scoped CLI HTTP reactor must start"),
        );
        // A current-thread runtime makes progress only inside `block_on`. One
        // owner thread drives it for the whole process; it never runs agent or
        // UI work, only provider HTTP futures.
        let driver = Arc::clone(&runtime);
        std::thread::Builder::new()
            .name("nexora-http-reactor-driver".to_string())
            .spawn(move || driver.block_on(std::future::pending::<()>()))
            .expect("the HTTP reactor driver thread must start");
        runtime
    })
}

/// The CLI's runtime seam implementation. Zero-sized: all state is the shared
/// reactor above, so an instance can be created on every run without cost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TokioSpawner;

impl AsyncSpawner for TokioSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle {
        let task = reactor().spawn(future);
        AbortHandle::new(move || task.abort())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam must spawn on the shared reactor without leaking a runtime
    /// handle and must hand back a callable (idempotent) abort closure.
    #[test]
    fn spawner_runs_a_future_and_aborts_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        let ran = Arc::new(AtomicUsize::new(0));
        let saw_ran = Arc::clone(&ran);
        let barrier = Arc::new(Barrier::new(2));
        let saw_barrier = Arc::clone(&barrier);
        let spawner = TokioSpawner;
        let handle = spawner.spawn(Box::pin(async move {
            saw_ran.fetch_add(1, Ordering::SeqCst);
            saw_barrier.wait();
            std::future::pending::<()>().await;
        }));
        // The worker reaches the barrier only if the reactor actually polls it.
        barrier.wait();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        handle.abort();
        handle.abort(); // idempotent
    }
}
