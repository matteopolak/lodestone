//! Task-spawn seam.
//!
//! The driver runs as a detached task, but *how* it is spawned is a genuine
//! design fork between targets — not a renamed function:
//!
//! * **Native.** The task is handed to [`tokio::spawn`] on the multi-threaded
//!   runtime, which requires it be [`Send`]. This is the primary target and is
//!   left exactly as it was: [`DriverTask`] is a thin newtype over
//!   [`tokio::task::JoinHandle`], so `join`/`is_finished` semantics and
//!   performance are unchanged.
//! * **Browser (`wasm32`).** There is no tokio runtime; the executor is
//!   `wasm_bindgen_futures::spawn_local`, which is single-threaded and does
//!   **not** require `Send`. That difference is forced, not incidental:
//!   `lodestone-net`'s `ws-web` transport wraps the browser `WebSocket` and is
//!   `!Send`, so the whole driver future is `!Send` on wasm and *cannot* be
//!   given to a `Send`-bounded spawner. The wasm [`DriverTask`] therefore
//!   carries its own completion signal (a shared flag) and delivers the final
//!   [`SessionOutcome`](crate::error::SessionOutcome) over a `oneshot`, since
//!   `spawn_local` yields no join handle.
//!
//! The two spawners differ only in the `Send` bound on the spawned future — the
//! honest crux of the seam — and both expose the same [`DriverTask`] surface.

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{DriverTask, spawn_driver};
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::{DriverTask, spawn_driver};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::future::Future;

    use tokio::task::JoinHandle;

    use crate::error::{ClientError, SessionOutcome};

    /// Handle to the spawned driver task (a `tokio::task::JoinHandle` newtype).
    #[derive(Debug)]
    pub(crate) struct DriverTask(JoinHandle<SessionOutcome>);

    impl DriverTask {
        /// Returns `true` once the driver task has finished.
        pub(crate) fn is_finished(&self) -> bool {
            self.0.is_finished()
        }

        /// Waits for the driver task to finish and returns its outcome.
        pub(crate) async fn join(self) -> SessionOutcome {
            match self.0.await {
                Ok(outcome) => outcome,
                Err(_) => SessionOutcome::Failed(ClientError::DriverPanicked),
            }
        }
    }

    /// Spawns the driver onto the tokio runtime. Requires `Send`.
    pub(crate) fn spawn_driver<F>(future: F) -> DriverTask
    where
        F: Future<Output = SessionOutcome> + Send + 'static,
    {
        DriverTask(tokio::spawn(future))
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::Cell;
    use std::future::Future;
    use std::rc::Rc;

    use tokio::sync::oneshot;

    use crate::error::{ClientError, SessionOutcome};

    /// Handle to the spawned driver task.
    ///
    /// `spawn_local` returns no join handle, so completion is tracked with a
    /// shared flag and the outcome is delivered over a `oneshot`.
    #[derive(Debug)]
    pub(crate) struct DriverTask {
        outcome: oneshot::Receiver<SessionOutcome>,
        finished: Rc<Cell<bool>>,
    }

    impl DriverTask {
        /// Returns `true` once the driver task has finished.
        pub(crate) fn is_finished(&self) -> bool {
            self.finished.get()
        }

        /// Waits for the driver task to finish and returns its outcome.
        pub(crate) async fn join(self) -> SessionOutcome {
            self.outcome
                .await
                .unwrap_or(SessionOutcome::Failed(ClientError::DriverPanicked))
        }
    }

    /// Spawns the driver onto the browser event loop. Does **not** require
    /// `Send`, which is what makes a `!Send` `ws-web` transport usable.
    pub(crate) fn spawn_driver<F>(future: F) -> DriverTask
    where
        F: Future<Output = SessionOutcome> + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let finished = Rc::new(Cell::new(false));
        let flag = Rc::clone(&finished);
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = future.await;
            flag.set(true);
            let _ = tx.send(outcome);
        });
        DriverTask {
            outcome: rx,
            finished,
        }
    }
}
