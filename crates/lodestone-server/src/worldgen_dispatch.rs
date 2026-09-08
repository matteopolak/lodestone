//! Bounded native dispatch for blocking world-generation work.
//!
//! Tokio's blocking pool is process-wide, but its admission limit is much
//! larger than the number of cores. A per-connection join window therefore
//! does not bound the total number of active generators when several players
//! join at once. This module supplies one reusable Rayon work-stealing pool for
//! every native world-generation dispatch. Async callers receive results over a
//! oneshot channel and never block the runtime thread.

#![cfg(not(target_arch = "wasm32"))]

use tokio::sync::oneshot;

/// Number of workers in the shared Rayon registry.
#[must_use]
pub(crate) fn parallelism() -> usize {
    rayon::current_num_threads().max(1)
}

/// Dispatch one native world-generation job to Rayon and return its async
/// result receiver. Rayon owns one reusable pool sized from available
/// parallelism, so all connections and batch helpers share the same worker
/// budget.
pub(crate) fn spawn<F, T>(f: F) -> oneshot::Receiver<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    rayon::spawn(move || {
        let _ = sender.send(f());
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dispatch_job_returns_through_the_async_channel() {
        let receiver = spawn(|| 7_u8);
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        assert_eq!(runtime.block_on(receiver).expect("Rayon worker returned"), 7);
    }
}
