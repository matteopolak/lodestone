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
/// Native: wraps either a [`tokio::task::JoinHandle`] or the dedicated thread
/// that drives a world tick loop.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) enum Task {
    Tokio(tokio::task::JoinHandle<()>),
    Thread(Option<std::thread::JoinHandle<()>>),
}

#[cfg(not(target_arch = "wasm32"))]
impl Task {
    /// Aborts the task if it is still running.
    pub(crate) fn abort(&self) {
        if let Self::Tokio(task) = self {
            task.abort();
        }
    }

    /// Awaits the task to completion. Takes `&mut self` so the owning handle,
    /// which also implements `Drop`, need not be moved out of.
    pub(crate) async fn join(&mut self) {
        match self {
            Self::Tokio(task) => {
                let _ = task.await;
            }
            Self::Thread(thread) => {
                if let Some(thread) = thread.take() {
                    let _ = thread.join();
                }
            }
        }
    }
}

/// Spawns `fut` on the current tokio runtime.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn<F>(fut: F) -> Task
where
    F: Future<Output = ()> + Send + 'static,
{
    Task::Tokio(tokio::spawn(fut))
}

/// Runs a long-lived async task on its own OS thread and current-thread Tokio
/// runtime. This keeps CPU-heavy world work out of both the shell runtime and
/// its blocking pool: generation also uses that pool, so occupying a worker for
/// the lifetime of the world would prevent the join stream from producing
/// terrain.
pub(crate) fn spawn_isolated_runtime<F>(fut: F) -> Task
where
    F: Future<Output = ()> + Send + 'static,
{
    let thread = std::thread::Builder::new()
        .name("lodestone-world-tick".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("the integrated tick runtime must construct");
            runtime.block_on(fut);
        })
        .expect("the integrated tick thread must spawn");
    Task::Thread(Some(thread))
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
