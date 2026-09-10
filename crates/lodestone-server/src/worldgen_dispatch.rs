//! Native world-generation dispatch shared by joins and batches.
//!
//! Tokio's blocking pool admits far more threads than a typical machine has
//! cores. A per-connection window therefore does not bound total generation
//! when multiple players join together. Native world-generation work enters
//! one dedicated, persistent Rayon work-stealing pool instead. Admission is a
//! synchronous try-operation, so an authoritative tick never waits for a free
//! worker; async callers receive results through oneshot channels.

#![cfg(not(target_arch = "wasm32"))]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};

/// Leave one hardware thread available to the Tokio runtime and the
/// authoritative world tick. The floor keeps a single-core host functional;
/// the scheduler's own window floor still preserves its ordering contract.
const TICK_RESERVE: usize = 1;

fn worker_count_for(available: usize) -> usize {
    available.saturating_sub(TICK_RESERVE).max(1)
}

struct Dispatcher {
    pool: rayon::ThreadPool,
    permits: Arc<Semaphore>,
    capacity: Arc<Notify>,
    workers: usize,
}

/// Run independent world-generation jobs on the process-wide dispatcher and
/// return their results in submission order. Workers may finish in any order;
/// the indexed collection is the canonical hand-off boundary for callers that
/// must commit mutable lifecycle state or emit packets deterministically.
///
/// This is the synchronous compatibility seam used by generator-owned
/// dependency batches. It reserves the complete dispatcher budget before
/// entering Rayon. If another async producer already owns any permit, it keeps
/// the result ordered but runs serially instead of placing a second batch in
/// Rayon’s queue. The async [`try_spawn`] path remains the preferred admission
/// mechanism for connection and tick-facing callers.
pub(crate) fn run_ordered<T, R, F>(jobs: Vec<T>, work: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Send + Sync,
{
    if jobs.len() <= 1 {
        return jobs.into_iter().map(work).collect();
    }
    // An offloaded source call already runs inside this dispatcher's Rayon
    // pool while holding its one admission permit. Requiring that worker to
    // acquire the whole semaphore can never succeed, and serialising here
    // strands every otherwise-idle worker. Rayon supports nested joins on the
    // same work-stealing pool, so reuse it directly; indexed collection keeps
    // the caller's deterministic result order.
    if rayon::current_thread_index().is_some() {
        use rayon::prelude::*;

        return jobs.into_par_iter().map(work).collect();
    }
    let workers = dispatcher().workers;
    let permits = u32::try_from(workers).expect("worldgen worker count fits semaphore permits");
    let permit = match Arc::clone(&dispatcher().permits).try_acquire_many_owned(permits) {
        Ok(permit) => permit,
        Err(tokio::sync::TryAcquireError::NoPermits) => {
            // This synchronous API is used by generator-owned dependency
            // batches. If an async producer already owns the dispatcher,
            // running another parallel batch would only queue more Rayon work
            // behind it. Keep the ordered result live without adding queue
            // pressure; the caller's deterministic order is unchanged.
            return jobs.into_iter().map(work).collect();
        }
        Err(tokio::sync::TryAcquireError::Closed) => {
            unreachable!("worldgen dispatcher semaphore cannot close")
        }
    };
    let _permit = PermitGuard {
        permit: Some(permit),
        capacity: Arc::clone(&dispatcher().capacity),
    };
    dispatcher().pool.install(|| {
        use rayon::prelude::*;

        jobs.into_par_iter().map(work).collect()
    })
}

/// Releases a dispatch permit before waking one async submitter.
///
/// The order is important: waking before releasing can let a waiter consume
/// the notification, observe no permit, and then sleep forever after the
/// permit is dropped. Keeping the two operations in one guard also covers a
/// worker that panics or notices cancellation before entering the job.
struct PermitGuard {
    permit: Option<OwnedSemaphorePermit>,
    capacity: Arc<Notify>,
}

impl Drop for PermitGuard {
    fn drop(&mut self) {
        drop(self.permit.take());
        // `notify_one` retains one notification when no waiter is currently
        // registered. That matters because a caller can observe backpressure,
        // then release a permit before it reaches `wait_for_capacity`.
        self.capacity.notify_one();
    }
}

fn dispatcher() -> &'static Dispatcher {
    static DISPATCHER: OnceLock<Dispatcher> = OnceLock::new();
    DISPATCHER.get_or_init(|| {
        let available = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        let workers = worker_count_for(available);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("lodestone-worldgen-{index}"))
            .build()
            .expect("worldgen dispatcher pool must initialize");
        Dispatcher {
            pool,
            permits: Arc::new(Semaphore::new(workers)),
            capacity: Arc::new(Notify::new()),
            workers,
        }
    })
}

/// The number of persistent world-generation workers.
#[must_use]
pub(crate) fn worker_count() -> usize {
    dispatcher().workers
}

/// The worker count of the persistent world-generation pool.
#[cfg(test)]
#[must_use]
pub(crate) fn parallelism() -> usize {
    worker_count()
}

/// A result handle for one admitted world-generation operation.
///
/// Dropping a handle marks work that has not started as cancelled. A worker
/// already inside the source call cannot be interrupted safely, but it still
/// releases its permit and suppresses delivery after cancellation. Keeping the
/// cancellation bit with the result rather than with the caller's future is
/// what lets a `select!` temporarily drop a wait without losing its ordered
/// result: the pipeline retains the handle and awaits it again later.
pub(crate) struct DispatchHandle<T> {
    receiver: oneshot::Receiver<T>,
    cancelled: Arc<AtomicBool>,
}

impl<T> DispatchHandle<T> {
    /// Request cancellation of work that has not started yet.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl<T> Future for DispatchHandle<T> {
    type Output = Result<T, oneshot::error::RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().receiver).poll(cx)
    }
}

impl<T> Drop for DispatchHandle<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Try to submit a native world-generation operation to the bounded pool.
///
/// This function never waits. `Err(f)` is backpressure: all worker permits are
/// already held by running or queued jobs, so the caller retains its closure
/// and retries from an async service point. The permit is acquired before the
/// Rayon job is queued, which bounds both active work and the pool's internal
/// queue across all connections.
pub(crate) fn try_spawn<F, T>(f: F) -> Result<DispatchHandle<T>, F>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = match Arc::clone(&dispatcher().permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(tokio::sync::TryAcquireError::NoPermits) => return Err(f),
        Err(tokio::sync::TryAcquireError::Closed) => {
            unreachable!("worldgen dispatcher semaphore cannot close")
        }
    };
    let (sender, receiver) = oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let capacity = Arc::clone(&dispatcher().capacity);
    dispatcher().pool.spawn(move || {
        let _permit = PermitGuard {
            permit: Some(permit),
            capacity,
        };
        if worker_cancelled.load(Ordering::Acquire) {
            return;
        }
        let value = f();
        if !worker_cancelled.load(Ordering::Acquire) {
            let _ = sender.send(value);
        }
    });
    Ok(DispatchHandle {
        receiver,
        cancelled,
    })
}

/// Wait for a permit to become available after [`try_spawn`] reports
/// backpressure.
///
/// This is an async notification, not an admission waiter held by the
/// dispatcher. Callers still retain the returned closure/coordinate and must
/// retry [`try_spawn`] after this future resolves.
pub(crate) async fn wait_for_capacity() {
    dispatcher().capacity.notified().await;
}

/// Submit a native world-generation operation, waiting asynchronously while
/// the bounded dispatcher is saturated.
///
/// This compatibility wrapper is for async callers that explicitly choose to
/// wait for admission. It does not hold a semaphore waiter or block a runtime
/// thread: every saturated attempt returns immediately from [`try_spawn`], and
/// the retry waits on a capacity notification. Authoritative tick code should
/// use `try_spawn` and defer its own pending work instead of awaiting this
/// wrapper.
pub(crate) async fn spawn<F, T>(mut f: F) -> DispatchHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    loop {
        match try_spawn(f) {
            Ok(handle) => return handle,
            Err(returned) => {
                f = returned;
                wait_for_capacity().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Barrier, Mutex, MutexGuard};

    use super::*;

    // The dispatcher and its semaphore are deliberately process-global in
    // production. Keep tests that intentionally occupy every permit from
    // racing the tests that submit ordinary jobs through that same singleton.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn a_dispatch_job_returns_through_the_async_channel() {
        let _test_guard = test_guard();
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        assert_eq!(
            runtime
                .block_on(async { spawn(|| 7_u8).await.await })
                .expect("Rayon worker returned"),
            7
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn try_submission_reports_backpressure_without_waiting() {
        let _test_guard = test_guard();
        let permits = worker_count();
        let mut held = Vec::with_capacity(permits);
        for _ in 0..permits {
            held.push(
                Arc::clone(&dispatcher().permits)
                    .acquire_owned()
                    .await
                    .expect("worldgen dispatcher semaphore open"),
            );
        }

        let returned = match try_spawn(|| 7_u8) {
            Err(job) => job,
            Ok(_handle) => panic!("a saturated dispatcher admitted work"),
        };

        drop(held);
        let handle = spawn(returned).await;
        assert_eq!(handle.await.expect("Rayon worker returned"), 7);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_suppresses_a_started_job_result() {
        let _test_guard = test_guard();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let handle = spawn(move || {
            worker_started.wait();
            worker_release.wait();
            7_u8
        })
        .await;

        started.wait();
        handle.cancel();
        release.wait();
        assert!(handle.await.is_err(), "cancelled result must not be delivered");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordered_batch_falls_back_serial_when_dispatcher_is_saturated() {
        let _test_guard = test_guard();
        let permits = worker_count();
        let mut held = Vec::with_capacity(permits);
        for _ in 0..permits {
            held.push(
                Arc::clone(&dispatcher().permits)
                    .acquire_owned()
                    .await
                    .expect("worldgen dispatcher semaphore open"),
            );
        }

        let caller = std::thread::current().id();
        let results = run_ordered(vec![3_u8, 1, 2], |value| {
            (value, std::thread::current().id())
        });
        assert_eq!(
            results.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
            vec![3, 1, 2],
            "backpressure must not change ordered batch results"
        );
        assert!(
            results.iter().all(|(_, thread)| *thread == caller),
            "a saturated ordered batch must not queue Rayon work"
        );
        drop(held);
    }

    #[test]
    fn ordered_batch_nested_in_dispatch_worker_reuses_idle_workers() {
        let _test_guard = test_guard();
        if worker_count() < 2 {
            return;
        }
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let results = runtime
            .block_on(async {
                spawn(|| {
                    run_ordered((0_u8..32).collect(), |value| {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        (value, std::thread::current().id())
                    })
                })
                .await
                .await
            })
            .expect("Rayon worker returned");
        assert_eq!(
            results.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
            (0_u8..32).collect::<Vec<_>>(),
            "nested parallel work must preserve indexed result order"
        );
        let worker_ids = results
            .iter()
            .map(|(_, worker)| *worker)
            .collect::<HashSet<_>>();
        assert!(
            worker_ids.len() > 1,
            "nested dispatch used only its permit-owning worker"
        );
    }

    #[test]
    fn the_dispatch_pool_reserves_a_runtime_thread_when_available() {
        let _test_guard = test_guard();
        assert_eq!(worker_count_for(0), 1);
        assert_eq!(worker_count_for(1), 1);
        assert_eq!(worker_count_for(2), 1);
        assert_eq!(worker_count_for(8), 7);
    }
}
