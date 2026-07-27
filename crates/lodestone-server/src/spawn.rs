//! The task-spawn seam.
//!
//! [`IntegratedServer`](crate::IntegratedServer) spawns one background task per
//! server (and, natively, one more per accepted LAN connection). *How* a task is
//! spawned is the single thing that differs between a native host and a browser,
//! so it is isolated here rather than scattered as `cfg`s through
//! `integrated.rs`:
//!
//! * **Native** — [`tokio::spawn`], returning an abortable, awaitable
//!   [`tokio::task::JoinHandle`]. A shell running under `#[tokio::main]` (or any
//!   entered runtime) satisfies it.
//! * **wasm32** — [`wasm_bindgen_futures::spawn_local`]. A browser has no
//!   blocking tokio runtime to `tokio::spawn` into — doing so panics at runtime,
//!   the `Instant::now()` family of "compiles green, dies at runtime" — so the
//!   future is driven by the JS event loop instead. `spawn_local` yields no join
//!   handle, so shutdown is **cooperative**: `IntegratedServer` already ends its
//!   task by firing an `Arc<Notify>` the task `select!`s on, and that mechanism
//!   is identical on both targets. `abort`/`join` therefore degrade to that
//!   signal on wasm.
//!
//! Both `spawn` variants take a `'static` future; the native one additionally
//! requires `Send` (tokio's multi-thread runtime may move it across threads),
//! which the server task satisfies.

use std::future::Future;

/// A handle to a spawned server task, owned by
/// [`IntegratedServer`](crate::IntegratedServer).
///
/// Native: wraps a [`tokio::task::JoinHandle`].
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct Task(tokio::task::JoinHandle<()>);

#[cfg(not(target_arch = "wasm32"))]
impl Task {
    /// Aborts the task if it is still running.
    pub(crate) fn abort(&self) {
        self.0.abort();
    }

    /// Awaits the task to completion. Takes `&mut self` so the owning handle,
    /// which also implements `Drop`, need not be moved out of.
    pub(crate) async fn join(&mut self) {
        let _ = (&mut self.0).await;
    }
}

/// Spawns `fut` on the current tokio runtime.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn<F>(fut: F) -> Task
where
    F: Future<Output = ()> + Send + 'static,
{
    Task(tokio::spawn(fut))
}

/// A handle to a spawned server task, owned by
/// [`IntegratedServer`](crate::IntegratedServer).
///
/// wasm: `spawn_local` gives no join handle, so this is a marker; the server's
/// `Arc<Notify>` drives shutdown cooperatively instead.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub(crate) struct Task;

#[cfg(target_arch = "wasm32")]
impl Task {
    /// No-op: a `spawn_local` task cannot be aborted from outside. The server's
    /// `Arc<Notify>` shutdown signal ends the task's `select!` instead.
    #[allow(clippy::unused_self)]
    pub(crate) fn abort(&self) {}

    /// No-op: a `spawn_local` task is not joinable. Callers that must be sure the
    /// task wound down rely on the cooperative `Notify` shutdown.
    #[allow(clippy::unused_self, clippy::unused_async)]
    pub(crate) async fn join(&mut self) {}
}

/// Spawns `fut` on the browser event loop via `wasm-bindgen-futures`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn spawn<F>(fut: F) -> Task
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(fut);
    Task
}
