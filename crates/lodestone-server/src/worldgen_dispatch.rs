//! Native world-generation dispatch shared by joins and batches.
//!
//! Tokio's blocking pool admits far more threads than a typical machine has
//! cores. A per-connection window therefore does not bound total generation
//! when multiple players join together. Native world-generation work enters
//! one persistent Rayon work-stealing pool instead; async callers receive
//! results through oneshot channels and never block the runtime thread.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, OnceLock};

use tokio::sync::{oneshot, Semaphore};

fn cpu_budget() -> &'static Arc<Semaphore> {
    static CPU_BUDGET: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CPU_BUDGET.get_or_init(|| Arc::new(Semaphore::new(rayon::current_num_threads().max(1))))
}

/// The worker count of the process-wide Rayon registry.
#[cfg(test)]
#[must_use]
pub(crate) fn parallelism() -> usize {
    rayon::current_num_threads().max(1)
}

/// Submit a native world-generation operation to the shared pool.
///
/// Acquiring the permit is asynchronous, so a burst of connections applies
/// backpressure at the dispatcher boundary instead of growing an unbounded
/// native queue. The permit is held by the worker until its result is sent;
/// dropping a caller's receiver therefore still lets the job finish and then
/// returns its capacity to the next waiting submission.
pub(crate) async fn spawn<F, T>(f: F) -> oneshot::Receiver<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = Arc::clone(cpu_budget())
        .acquire_owned()
        .await
        .expect("worldgen dispatcher semaphore closed");
    let (sender, receiver) = oneshot::channel();
    rayon::spawn(move || {
        let _permit = permit;
        let _ = sender.send(f());
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dispatch_job_returns_through_the_async_channel() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        assert_eq!(
            runtime
                .block_on(async { spawn(|| 7_u8).await.await })
                .expect("Rayon worker returned"),
            7
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn submission_backpressure_precedes_worker_result() {
        let permits = parallelism();
        let mut held = Vec::with_capacity(permits);
        for _ in 0..permits {
            held.push(
                Arc::clone(cpu_budget())
                    .acquire_owned()
                    .await
                    .expect("worldgen dispatcher semaphore open"),
            );
        }

        let mut submission = Box::pin(spawn(|| 7_u8));
        tokio::select! {
            biased;
            _ = &mut submission => panic!("a saturated dispatcher admitted work"),
            _ = tokio::task::yield_now() => {}
        }

        drop(held);
        let receiver = submission.await;
        assert_eq!(receiver.await.expect("Rayon worker returned"), 7);
    }
}
