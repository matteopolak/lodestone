#[cfg(any(all(target_arch = "wasm32", feature = "wasm-threads"), all(test, not(target_arch = "wasm32"))))]
mod threaded {
    use std::sync::Arc;
    #[cfg(target_arch = "wasm32")]
    use std::sync::OnceLock;
    use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

    use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

    #[cfg(target_arch = "wasm32")]
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

    /// Why owned packet preparation produced no value.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum OwnedComputeError {
        /// The permit semaphore was closed.
        DispatcherClosed,
        /// The worker ended (or panicked) without sending its result.
        WorkerClosed,
    }

    impl std::fmt::Display for OwnedComputeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::DispatcherClosed => "owned compute dispatcher closed",
                Self::WorkerClosed => "owned compute worker closed without a result",
            })
        }
    }

    type TimerStart = fn(WorldgenTimingPhase, u32) -> Option<PhaseTimer>;

    struct TimedPermit {
        _timing: Option<PhaseTimer>,
        _permit: OwnedSemaphorePermit,
    }

    pub(crate) struct Completed<T> {
        value: T,
        return_timers: (Option<PhaseTimer>, Option<PhaseTimer>),
        permit: TimedPermit,
        items: u32,
        start_timer: TimerStart,
    }

    impl<T> Completed<T> {
        pub(crate) fn accept<R>(self, accept: impl FnOnce(T) -> R) -> R {
            let Self { value, return_timers, permit, items, start_timer } = self;
            drop(return_timers);
            let acceptance_timer = start_timer(WorldgenTimingPhase::PacketPreparationAcceptance, items);
            let result = accept(value);
            drop(acceptance_timer);
            drop(permit);
            result
        }
    }

    /// Runs one packet preparation on the browser worker's Rayon pool, one at a
    /// time, and hands the value back holding its permit until it is accepted.
    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn execute<F, T>(items: u32, work: F) -> Result<Completed<T>, OwnedComputeError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permits = Arc::clone(PERMITS.get_or_init(|| Arc::new(Semaphore::new(1))));
        execute_with(permits, items, work, |job| rayon::spawn(job)).await
    }

    async fn execute_with<F, T>(
        permits: Arc<Semaphore>,
        items: u32,
        work: F,
        submit: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) -> Result<Completed<T>, OwnedComputeError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        execute_with_timers(permits, items, work, submit, PhaseTimer::start).await
    }

    async fn execute_with_timers<F, T>(
        permits: Arc<Semaphore>,
        items: u32,
        work: F,
        submit: impl FnOnce(Box<dyn FnOnce() + Send>),
        start_timer: TimerStart,
    ) -> Result<Completed<T>, OwnedComputeError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        use WorldgenTimingPhase as Phase;
        let queue_timer = start_timer(Phase::ImmutableQueueWait, items);
        let permit_wait_timer = start_timer(Phase::PacketPreparationPermitWait, items);
        let permit = permits
            .acquire_owned()
            .await
            .map_err(|_| OwnedComputeError::DispatcherClosed)?;
        drop(permit_wait_timer);
        let permit = TimedPermit {
            _timing: start_timer(Phase::PacketPreparationPermitHold, items),
            _permit: permit,
        };
        let (sender, receiver) = oneshot::channel();
        let pool_wait_timer = start_timer(Phase::PacketPreparationPoolWait, items);
        submit(Box::new(move || {
            drop(pool_wait_timer);
            drop(queue_timer);
            if sender.is_closed() {
                return;
            }
            let compute_timers = (
                start_timer(Phase::ImmutableCompute, items),
                start_timer(Phase::PacketPreparationCompute, items),
            );
            let value = work();
            drop(compute_timers);
            let return_timers = (
                start_timer(Phase::ImmutableReturn, items),
                start_timer(Phase::PacketPreparationReturnWait, items),
            );
            let _ = sender.send(Completed { value, return_timers, permit, items, start_timer });
        }));
        receiver.await.map_err(|_| OwnedComputeError::WorkerClosed)
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        #[tokio::test(flavor = "current_thread")]
        async fn packet_preparation_services_timer_while_worker_is_gated() {
            let permits = Arc::new(Semaphore::new(1));
            let (entered, running) = oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let worker_finished = Arc::clone(&finished);
            let task = tokio::spawn(execute_with(permits, 1, move || {
                let _ = entered.send(());
                let _ = gate.recv();
                worker_finished.store(true, Ordering::Release);
                7
            }, |job| rayon::spawn(job)));
            tokio::time::timeout(Duration::from_secs(5), running).await.unwrap().unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(!finished.load(Ordering::Acquire));
            release.send(()).unwrap();
            assert_eq!(task.await.unwrap().unwrap().accept(|value| value), 7);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn packet_preparation_cancelled_receiver_keeps_running_permit() {
            let permits = Arc::new(Semaphore::new(1));
            let (entered, running) = oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let task = tokio::spawn(execute_with(Arc::clone(&permits), 1, move || {
                let _ = entered.send(());
                let _ = gate.recv();
            }, |job| rayon::spawn(job)));
            tokio::time::timeout(Duration::from_secs(5), running).await.unwrap().unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(permits.available_permits(), 0);
            release.send(()).unwrap();
            let permit = tokio::time::timeout(Duration::from_secs(5), permits.acquire())
                .await.unwrap().unwrap();
            drop(permit);
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn packet_preparation_completed_result_retains_permit_and_order() {
            use rayon::prelude::*;
            let permits = Arc::new(Semaphore::new(1));
            let completed = execute_with(Arc::clone(&permits), 3, || {
                vec![3, 1, 2].into_par_iter().map(|value| value * 10).collect::<Vec<_>>()
            }, |job| rayon::spawn(job)).await.unwrap();
            assert_eq!(permits.available_permits(), 0);
            completed.accept(|values| {
                assert_eq!(values, vec![30, 10, 20]);
                assert_eq!(permits.available_permits(), 0);
            });
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn packet_preparation_cancellation_before_start_skips_work() {
            let permits = Arc::new(Semaphore::new(1));
            let ran = Arc::new(AtomicBool::new(false));
            let worker_ran = Arc::clone(&ran);
            let (submitted, queued) = oneshot::channel();
            let task = tokio::spawn(execute_with(Arc::clone(&permits), 1, move || {
                worker_ran.store(true, Ordering::Release);
            }, move |job| { let _ = submitted.send(job); }));
            let job = queued.await.unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(permits.available_permits(), 0);
            job();
            assert!(!ran.load(Ordering::Acquire));
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn packet_preparation_closed_worker_is_an_error() {
            let permits = Arc::new(Semaphore::new(1));
            let result = execute_with(Arc::clone(&permits), 1, || 7, drop).await;
            assert!(matches!(result, Err(OwnedComputeError::WorkerClosed)));
            assert_eq!(permits.available_permits(), 1);
        }

        static TIMING_EVENTS: std::sync::Mutex<Vec<(u32, WorldgenTimingPhase, bool)>> =
            std::sync::Mutex::new(Vec::new());

        fn observed_timer(phase: WorldgenTimingPhase, items: u32) -> Option<PhaseTimer> {
            TIMING_EVENTS.lock().unwrap().push((items, phase, true));
            Some(PhaseTimer::with_sink(phase, items, |sample| {
                TIMING_EVENTS.lock().unwrap().push((sample.items, sample.phase, false));
            }))
        }

        fn timing_events(items: u32) -> Vec<(WorldgenTimingPhase, bool)> {
            TIMING_EVENTS.lock().unwrap().iter()
                .filter(|event| event.0 == items).map(|event| (event.1, event.2)).collect()
        }

        #[test]
        fn packet_preparation_timing_separates_permit_and_pool_wait() {
            use std::future::Future;
            use std::task::{Context, Poll, Waker};
            use WorldgenTimingPhase as Phase;

            let permits = Arc::new(Semaphore::new(1));
            let held = Arc::clone(&permits).try_acquire_owned().unwrap();
            let (submitted, queued) = std::sync::mpsc::channel();
            let mut future = std::pin::pin!(execute_with_timers(
                Arc::clone(&permits), 701, || 7, move |job| submitted.send(job).unwrap(), observed_timer,
            ));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::PacketPreparationPermitWait, true),
            ]);
            assert!(queued.try_recv().is_err());
            drop(held);
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::PacketPreparationPermitWait, true),
                (Phase::PacketPreparationPermitWait, false),
                (Phase::PacketPreparationPermitHold, true),
                (Phase::PacketPreparationPoolWait, true),
            ]);
            queued.try_recv().unwrap()();
            let Poll::Ready(Ok(completed)) = future.as_mut().poll(&mut context) else {
                panic!("authored worker did not return its result");
            };
            assert_eq!(permits.available_permits(), 0);
            assert_eq!(completed.accept(|value| value), 7);
            assert_eq!(permits.available_permits(), 1);
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::PacketPreparationPermitWait, true),
                (Phase::PacketPreparationPermitWait, false),
                (Phase::PacketPreparationPermitHold, true),
                (Phase::PacketPreparationPoolWait, true),
                (Phase::PacketPreparationPoolWait, false),
                (Phase::ImmutableQueueWait, false),
                (Phase::ImmutableCompute, true),
                (Phase::PacketPreparationCompute, true),
                (Phase::ImmutableCompute, false),
                (Phase::PacketPreparationCompute, false),
                (Phase::ImmutableReturn, true),
                (Phase::PacketPreparationReturnWait, true),
                (Phase::ImmutableReturn, false),
                (Phase::PacketPreparationReturnWait, false),
                (Phase::PacketPreparationAcceptance, true),
                (Phase::PacketPreparationAcceptance, false),
                (Phase::PacketPreparationPermitHold, false),
            ]);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_packet_timing_closes_on_acceptance_discard_and_panic() {
            use WorldgenTimingPhase as Phase;

            for (items, discard, panic_in_accept) in [
                (702, false, false), (703, true, false), (704, false, true),
            ] {
                let permits = Arc::new(Semaphore::new(1));
                let completed = execute_with_timers(
                    Arc::clone(&permits), items, || 11, |job| job(), observed_timer,
                ).await.unwrap();
                assert_eq!(permits.available_permits(), 0);
                assert_eq!(timing_events(items).last(), Some(&(Phase::PacketPreparationReturnWait, true)));
                if discard {
                    drop(completed);
                } else {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        completed.accept(|value| {
                            assert_eq!(value, 11);
                            assert_eq!(permits.available_permits(), 0);
                            let events = timing_events(items);
                            assert!(events.contains(&(Phase::PacketPreparationReturnWait, false)));
                            assert_eq!(events.last(), Some(&(Phase::PacketPreparationAcceptance, true)));
                            assert!(!events.contains(&(Phase::PacketPreparationPermitHold, false)));
                            assert!(!panic_in_accept, "authored acceptance panic");
                        });
                    }));
                    assert_eq!(result.is_err(), panic_in_accept);
                }
                assert_eq!(permits.available_permits(), 1);
                let events = timing_events(items);
                assert_eq!(events.last(), Some(&(Phase::PacketPreparationPermitHold, false)));
                assert_eq!(events.contains(&(Phase::PacketPreparationAcceptance, true)), !discard);
                for phase in [
                    Phase::ImmutableQueueWait, Phase::ImmutableCompute, Phase::ImmutableReturn,
                    Phase::PacketPreparationPermitWait, Phase::PacketPreparationPoolWait,
                    Phase::PacketPreparationCompute, Phase::PacketPreparationReturnWait,
                    Phase::PacketPreparationPermitHold,
                ] {
                    assert_eq!(events.iter().filter(|event| **event == (phase, true)).count(), 1);
                    assert_eq!(events.iter().filter(|event| **event == (phase, false)).count(), 1);
                }
                assert_eq!(events.contains(&(Phase::PacketPreparationAcceptance, false)), !discard);
            }

            let permits = Arc::new(Semaphore::new(1));
            let result = execute_with_timers(
                Arc::clone(&permits), 705,
                || -> i32 { panic!("authored worker panic") },
                |job| {
                    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err());
                },
                observed_timer,
            ).await;
            assert!(matches!(result, Err(OwnedComputeError::WorkerClosed)));
            assert_eq!(permits.available_permits(), 1);
            let events = timing_events(705);
            for phase in [
                Phase::ImmutableQueueWait, Phase::ImmutableCompute,
                Phase::PacketPreparationPermitWait, Phase::PacketPreparationPoolWait,
                Phase::PacketPreparationCompute, Phase::PacketPreparationPermitHold,
            ] {
                assert_eq!(events.iter().filter(|event| **event == (phase, true)).count(), 1);
                assert_eq!(events.iter().filter(|event| **event == (phase, false)).count(), 1);
            }
            assert!(!events.iter().any(|(phase, _)| matches!(
                phase, Phase::ImmutableReturn | Phase::PacketPreparationReturnWait |
                Phase::PacketPreparationAcceptance,
            )));
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub(crate) use threaded::execute;
