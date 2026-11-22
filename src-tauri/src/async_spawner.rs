//! Desktop-only async spawner: the Tauri async runtime behind the
//! [`AsyncSpawner`](nexora_runtime::AsyncSpawner) seam used by the provider
//! transport.
//!
//! This is the ONLY place in the desktop shell that names the Tauri runtime
//! for provider HTTP attempts. Keeping it in the composition-root crate means
//! the whole extracted `nexora-runtime` tree stays Tauri-free and the desktop
//! keeps byte-identical runtime behavior.

use std::future::Future;
use std::pin::Pin;

use nexora_runtime::{AbortHandle, AsyncSpawner};

/// Spawns provider attempt futures on the Tauri async runtime.
pub(crate) struct TauriSpawner;

impl AsyncSpawner for TauriSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> AbortHandle {
        let handle = tauri::async_runtime::spawn(future);
        AbortHandle::new(move || handle.abort())
    }
}
