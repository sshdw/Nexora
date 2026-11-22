//! Runtime-agnostic async-task spawning seam for the provider transport.
//!
//! The transport spawns each HTTP attempt so a blocking caller can poll its
//! cancellation token with short timeouts while the request is in flight.
//! Which runtime actually drives those futures is a shell concern: the
//! desktop installs `TauriSpawner`, while this Tauri-free crate supplies its
//! own implementation for its unit tests. This module deliberately names no
//! async runtime in its production API, needs no `unsafe`, and never exposes
//! a runtime handle: the only thing that crosses the seam is an opaque
//! [`AbortHandle`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Spawns one `'static`, `Send` future and returns a handle that can abort it.
pub trait AsyncSpawner: Send + Sync {
    /// Spawn `future` and return a handle whose [`AbortHandle::abort`] requests
    /// cancellation of the spawned task.
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle;
}

/// Owned abort handle for one spawned task.
///
/// Wraps a cloned closure instead of a runtime `JoinHandle` so no runtime type
/// leaks through the seam. Dropping the handle does not cancel the task; only
/// an explicit [`AbortHandle::abort`] does, preserving the transport's
/// "result racing cancel still counts as cancelled" contract.
#[derive(Clone)]
pub struct AbortHandle(Arc<dyn Fn() + Send + Sync>);

impl AbortHandle {
    /// Build a handle around the runtime-specific abort closure.
    pub fn new(abort: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(abort))
    }

    /// Request cancellation of the spawned task.
    pub fn abort(&self) {
        (self.0)();
    }
}

/// Test-only spawner for the provider unit tests.
///
/// `reqwest`'s async client needs a reactor to drive its sockets; a bare
/// `std::thread` + waker executor cannot supply one. `reqwest` already brings
/// Tokio into this crate's dependency graph transitively, so the tests use a
/// single lazily built multi-thread runtime behind the same [`AsyncSpawner`]
/// seam. This is a `[dev-dependencies]`-only facility: the shipped crate has
/// no async runtime dependency and no runtime preference of its own.
#[cfg(test)]
static TEST_RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> =
    std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build the test async runtime")
    });

/// Test-only spawner implementation backed by the lazily built test runtime.
#[cfg(test)]
struct TestSpawner;

#[cfg(test)]
impl AsyncSpawner for TestSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle {
        let handle = TEST_RUNTIME.spawn(future);
        AbortHandle::new(move || handle.abort())
    }
}

/// Return the shared test spawner (unit tests only).
#[cfg(test)]
pub(crate) fn test_spawner() -> Arc<dyn AsyncSpawner> {
    Arc::new(TestSpawner)
}
