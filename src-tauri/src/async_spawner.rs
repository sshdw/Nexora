//! Desktop-only async spawner: the Tauri async runtime behind the
//! [`AsyncSpawner`](crate::infrastructure::providers::async_spawner::AsyncSpawner)
//! seam used by the provider transport.
//!
//! This is the ONLY place in the desktop shell that names the Tauri runtime
//! for provider HTTP attempts. Keeping it in the composition-root crate means
//! the whole `application`/`infrastructure` tree stays Tauri-free and can be
//! extracted into the shared runtime crate without a behavior delta.

use std::future::Future;
use std::pin::Pin;

use crate::infrastructure::providers::async_spawner::{AbortHandle, AsyncSpawner};

/// Spawns provider attempt futures on the Tauri async runtime.
pub(crate) struct TauriSpawner;

impl AsyncSpawner for TauriSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle {
        let handle = tauri::async_runtime::spawn(future);
        AbortHandle::new(move || handle.abort())
    }
}
