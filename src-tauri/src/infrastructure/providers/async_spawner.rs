//! Runtime-agnostic async-task spawning seam for the provider transport.
//!
//! The transport spawns each HTTP attempt so a blocking caller can poll its
//! cancellation token with short timeouts while the request is in flight.
//! Which runtime actually drives those futures is a shell concern: the
//! desktop installs `TauriSpawner`, while the Tauri-free runtime crate
//! supplies its own implementation. This module deliberately names no async
//! runtime, needs no `unsafe`, and never exposes a runtime handle: the only
//! thing that crosses the seam is an opaque [`AbortHandle`].

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

/// Test-only spawner used by the provider unit tests: the desktop Tauri
/// runtime. Kept here so every test call site passes the same seam type as
/// production instead of a private test-only executor.
#[cfg(test)]
pub(crate) fn test_spawner() -> Arc<dyn AsyncSpawner> {
    Arc::new(crate::async_spawner::TauriSpawner)
}
