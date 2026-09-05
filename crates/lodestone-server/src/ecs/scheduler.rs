//! Tick-owned native plugin callbacks; no world lock or worker thread is exposed.

use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

/// Opaque cancellation handle, unique for the lifetime of one scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerTaskId(u64);

/// Opaque cancellation handle for one off-tick task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerAsyncTaskId(u64);

/// Why [`ServerTaskScheduler::spawn_with_handback`] could not accept work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAsyncTaskError {
    /// Every hand-back reservation is occupied by running or completed work.
    Full,
    /// [`ServerTaskScheduler::shutdown_async_tasks`] has begun.
    Shutdown,
}

type Callback = Box<dyn FnMut(&mut World, ServerTaskId) + Send + Sync>;
type HandBack = Box<dyn FnOnce(&mut World) + Send>;

/// The default upper bound for running work plus completed, undrained results.
///
/// A result retains its reservation until the tick owner has either run or
/// discarded its hand-back. That makes this one bound cover both worker count
/// and queued world callbacks without asking a worker to wait for the tick.
pub const DEFAULT_ASYNC_HAND_BACK_CAPACITY: usize = 64;

struct Task {
    due: u64,
    period: Option<u64>,
    callback: Option<Callback>,
}

struct AsyncState {
    accepting: AtomicBool,
    reserved: AtomicUsize,
}

impl AsyncState {
    fn try_reserve(&self, capacity: usize) -> bool {
        let mut reserved = self.reserved.load(Ordering::Acquire);
        loop {
            if reserved >= capacity {
                return false;
            }
            match self.reserved.compare_exchange_weak(
                reserved,
                reserved + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => reserved = actual,
            }
        }
    }
}

struct AsyncReservation(Arc<AsyncState>);

impl Drop for AsyncReservation {
    fn drop(&mut self) {
        let previous = self.0.reserved.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "server async hand-back reservation underflow");
    }
}

struct CompletedAsyncTask {
    id: ServerAsyncTaskId,
    cancelled: Arc<AtomicBool>,
    hand_back: Option<HandBack>,
    // Dropping this only after the tick owner handles the completion is what
    // makes a bounded completion channel sufficient without blocking workers.
    _reservation: AsyncReservation,
}

/// Delayed and repeating work on the primary server world's `GameTick`.
///
/// Delays count scheduler passes, excluding `ServerBoot`. Zero delay and zero
/// period normalize to one tick. Callbacks may schedule or cancel other work,
/// including their own repeat, using this resource through their world borrow.
/// Tasks are transient and disappear when the owning world shuts down.
#[derive(Resource)]
pub struct ServerTaskScheduler {
    tick: u64,
    dispatching: bool,
    next_id: u64,
    tasks: BTreeMap<ServerTaskId, Task>,
    deadlines: BTreeSet<(u64, ServerTaskId)>,
    async_capacity: usize,
    async_state: Arc<AsyncState>,
    async_sender: SyncSender<CompletedAsyncTask>,
    async_receiver: Mutex<Receiver<CompletedAsyncTask>>,
    next_async_id: u64,
    async_tasks: BTreeMap<ServerAsyncTaskId, Arc<AtomicBool>>,
}

impl std::fmt::Debug for ServerTaskScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerTaskScheduler")
            .field("tick", &self.tick)
            .field("pending", &self.tasks.len())
            .field("pending_async", &self.async_tasks.len())
            .finish_non_exhaustive()
    }
}

impl ServerTaskScheduler {
    /// Build a scheduler whose off-tick work has one shared, finite bound.
    ///
    /// The bound covers both currently-running work and completed results that
    /// have not reached the tick owner. A zero capacity is nonsensical because
    /// it could never admit a hand-back.
    #[must_use]
    pub fn with_async_hand_back_capacity(async_capacity: usize) -> Self {
        assert!(
            async_capacity > 0,
            "server async hand-back capacity must be nonzero"
        );
        let (async_sender, async_receiver) = mpsc::sync_channel(async_capacity);
        Self {
            tick: 0,
            dispatching: false,
            next_id: 0,
            tasks: BTreeMap::new(),
            deadlines: BTreeSet::new(),
            async_capacity,
            async_state: Arc::new(AsyncState {
                accepting: AtomicBool::new(true),
                reserved: AtomicUsize::new(0),
            }),
            async_sender,
            async_receiver: Mutex::new(async_receiver),
            next_async_id: 0,
            async_tasks: BTreeMap::new(),
        }
    }

    /// Run once after `delay` scheduler passes (at least one).
    /// Panics if the deadline or handle space exceeds `u64`.
    pub fn schedule_once(
        &mut self,
        delay: u64,
        callback: impl FnMut(&mut World, ServerTaskId) + Send + Sync + 'static,
    ) -> ServerTaskId {
        self.schedule(delay, None, Box::new(callback))
    }

    /// Run after `delay` passes and every `period` passes thereafter.
    /// Both zero arguments normalize to one; there is no wall-clock catch-up.
    /// Panics if the deadline or handle space exceeds `u64`.
    pub fn schedule_repeating(
        &mut self,
        delay: u64,
        period: u64,
        callback: impl FnMut(&mut World, ServerTaskId) + Send + Sync + 'static,
    ) -> ServerTaskId {
        self.schedule(delay, Some(period.max(1)), Box::new(callback))
    }

    fn schedule(&mut self, delay: u64, period: Option<u64>, callback: Callback) -> ServerTaskId {
        let due = self.tick.checked_add(delay.max(1)).expect("server task deadline exhausted");
        let id = ServerTaskId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("server task handles exhausted");
        self.tasks.insert(id, Task { due, period, callback: Some(callback) });
        self.deadlines.insert((due, id));
        id
    }

    /// Cancel pending work or prevent a running callback's next repetition.
    /// Returns false for an unknown or already completed handle.
    pub fn cancel(&mut self, id: ServerTaskId) -> bool {
        let Some(task) = self.tasks.remove(&id) else { return false; };
        self.deadlines.remove(&(task.due, id));
        true
    }

    /// Run `work` away from the tick owner and queue its result for a later
    /// [`run_server_tasks`] pass.
    ///
    /// `work` has no `World` parameter, so it cannot borrow the tick-owned
    /// world through this API. `hand_back` is the only world-facing closure and
    /// runs on the primary tick task, after message maintenance and before due
    /// synchronous scheduler callbacks. Native targets use a fresh named worker
    /// thread; wasm32 runs `work` inline because it has no worker threads.
    ///
    /// Admission is explicitly backpressured: at most the configured capacity
    /// of running work plus completed, undrained hand-backs exist at once. A
    /// caller receiving [`ServerAsyncTaskError::Full`] must defer or discard its
    /// own request; no unbounded worker or callback queue is created.
    pub fn spawn_with_handback<T, W, H>(
        &mut self,
        work: W,
        hand_back: H,
    ) -> Result<ServerAsyncTaskId, ServerAsyncTaskError>
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        H: FnOnce(T, &mut World) + Send + 'static,
    {
        if !self.async_state.accepting.load(Ordering::Acquire) {
            return Err(ServerAsyncTaskError::Shutdown);
        }
        if !self.async_state.try_reserve(self.async_capacity) {
            return Err(ServerAsyncTaskError::Full);
        }
        if !self.async_state.accepting.load(Ordering::Acquire) {
            drop(AsyncReservation(Arc::clone(&self.async_state)));
            return Err(ServerAsyncTaskError::Shutdown);
        }

        let id = ServerAsyncTaskId(self.next_async_id);
        self.next_async_id = self
            .next_async_id
            .checked_add(1)
            .expect("server async task handles exhausted");
        let cancelled = Arc::new(AtomicBool::new(false));
        self.async_tasks.insert(id, Arc::clone(&cancelled));
        let completion = AsyncCompletion {
            id,
            cancelled,
            state: Arc::clone(&self.async_state),
            sender: self.async_sender.clone(),
            reservation: AsyncReservation(Arc::clone(&self.async_state)),
        };

        #[cfg(not(target_arch = "wasm32"))]
        std::thread::Builder::new()
            .name("lodestone-server-async".into())
            .spawn(move || completion.run(work, hand_back))
            .expect("spawning a server async hand-back worker");
        #[cfg(target_arch = "wasm32")]
        completion.run(work, hand_back);

        Ok(id)
    }

    /// Prevent a queued result from mutating the world.
    ///
    /// Cancellation cannot stop a closure already executing on a native worker,
    /// but it guarantees that closure's result will be discarded. A successful
    /// cancellation releases its hand-back reservation when that work returns;
    /// cancelling an already delivered, unknown, or previously cancelled task
    /// returns `false`.
    pub fn cancel_async(&mut self, id: ServerAsyncTaskId) -> bool {
        let Some(cancelled) = self.async_tasks.remove(&id) else { return false; };
        cancelled.store(true, Ordering::Release);
        true
    }

    /// Stop accepting off-tick work and discard every result that has not run.
    ///
    /// Running native work is not forcibly interrupted. It finishes without a
    /// world hand-back, then releases its reservation. Dropping the scheduler
    /// invokes the same shutdown, so a world can disappear without a worker
    /// retaining a path back into it.
    pub fn shutdown_async_tasks(&mut self) {
        self.async_state.accepting.store(false, Ordering::Release);
        for cancelled in self.async_tasks.values() {
            cancelled.store(true, Ordering::Release);
        }
        self.async_tasks.clear();
        let receiver = self
            .async_receiver
            .get_mut()
            .expect("server async hand-back receiver poisoned");
        while receiver.try_recv().is_ok() {}
    }
}

impl Default for ServerTaskScheduler {
    fn default() -> Self {
        Self::with_async_hand_back_capacity(DEFAULT_ASYNC_HAND_BACK_CAPACITY)
    }
}

impl Drop for ServerTaskScheduler {
    fn drop(&mut self) {
        self.shutdown_async_tasks();
    }
}

struct AsyncCompletion {
    id: ServerAsyncTaskId,
    cancelled: Arc<AtomicBool>,
    state: Arc<AsyncState>,
    sender: SyncSender<CompletedAsyncTask>,
    reservation: AsyncReservation,
}

impl AsyncCompletion {
    fn run<T, W, H>(self, work: W, hand_back: H)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        H: FnOnce(T, &mut World) + Send + 'static,
    {
        let hand_back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .ok()
            .map(|value| Box::new(move |world: &mut World| hand_back(value, world)) as HandBack);
        if self.cancelled.load(Ordering::Acquire)
            || !self.state.accepting.load(Ordering::Acquire)
        {
            return;
        }
        let completed = CompletedAsyncTask {
            id: self.id,
            cancelled: self.cancelled,
            hand_back,
            _reservation: self.reservation,
        };
        match self.sender.try_send(completed) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                panic!("server async hand-back capacity exceeded despite reservation");
            }
        }
    }
}

/// Exclusive `TickSet::Drain` system installed by `ServerCorePlugin`.
/// Order other systems explicitly before/after this function when they share
/// resources with callbacks. Native callbacks have the same trust as systems:
/// they must not block or remove the scheduler resource. Recursive dispatch
/// fails immediately instead of advancing the clock inside a callback.
pub fn run_server_tasks(world: &mut World) {
    let due = {
        let mut tasks = world.resource_mut::<ServerTaskScheduler>();
        assert!(!tasks.dispatching, "server scheduler cannot run recursively");
        tasks.dispatching = true;
        tasks.tick = tasks.tick.checked_add(1).expect("server scheduler clock exhausted");
        let mut due = Vec::new();
        while let Some(&(deadline, id)) = tasks.deadlines.first() {
            if deadline > tasks.tick { break; }
            tasks.deadlines.pop_first();
            due.push(id);
        }
        due
    };
    drain_async_hand_backs(world);
    for id in due {
        let callback = world.resource_mut::<ServerTaskScheduler>()
            .tasks.get_mut(&id).and_then(|task| task.callback.take());
        let Some(mut callback) = callback else { continue; };
        callback(world, id);
        let mut tasks = world.resource_mut::<ServerTaskScheduler>();
        let tick = tasks.tick;
        let Some(task) = tasks.tasks.get_mut(&id) else { continue; };
        if let Some(period) = task.period {
            task.due = tick.checked_add(period).expect("server task deadline exhausted");
            task.callback = Some(callback);
            let deadline = task.due;
            tasks.deadlines.insert((deadline, id));
        } else {
            tasks.tasks.remove(&id);
        }
    }
    world.resource_mut::<ServerTaskScheduler>().dispatching = false;
}

/// Run completed off-tick results on the tick owner.
///
/// This is deliberately private to [`run_server_tasks`]: a second system would
/// make the scheduler's hand-back order depend on application registration.
fn drain_async_hand_backs(world: &mut World) {
    loop {
        let completed = {
            let scheduler = world.resource::<ServerTaskScheduler>();
            match scheduler
                .async_receiver
                .lock()
                .expect("server async hand-back receiver poisoned")
                .try_recv()
            {
                Ok(completed) => completed,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        };
        let hand_back = {
            let mut scheduler = world.resource_mut::<ServerTaskScheduler>();
            scheduler.async_tasks.remove(&completed.id);
            if completed.cancelled.load(Ordering::Acquire)
                || !scheduler.async_state.accepting.load(Ordering::Acquire)
            {
                None
            } else {
                completed.hand_back
            }
        };
        if let Some(hand_back) = hand_back {
            hand_back(world);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::{GameTick, ServerApp};
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Resource, Default)]
    struct Calls(Vec<u64>);

    #[derive(Resource, Default)]
    struct AsyncCalls(Vec<std::thread::ThreadId>);

    fn world() -> World {
        let mut world = ServerApp::bootstrap().into_world();
        world.init_resource::<Calls>();
        world
    }

    #[test]
    fn deadlines_are_game_ticks_and_equal_deadlines_keep_registration_order() {
        let mut world = world();
        let mut tasks = world.resource_mut::<ServerTaskScheduler>();
        for (delay, value) in [(3, 30), (0, 10), (1, 11), (3, 31)] {
            tasks.schedule_once(delay, move |world, _| {
                world.resource_mut::<Calls>().0.push(value);
            });
        }
        assert!(world.resource::<Calls>().0.is_empty());
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [10, 11]);
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [10, 11]);
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [10, 11, 30, 31]);
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [10, 11, 30, 31]);
    }

    #[test]
    fn repeating_callback_can_cancel_itself_and_defer_nested_work() {
        let mut world = world();
        world.resource_mut::<ServerTaskScheduler>().schedule_repeating(2, 3, |world, id| {
            let mut calls = world.resource_mut::<Calls>();
            calls.0.push(7);
            if calls.0.len() == 2 {
                let mut tasks = world.resource_mut::<ServerTaskScheduler>();
                assert!(tasks.cancel(id));
                tasks.schedule_once(0, |world, _| world.resource_mut::<Calls>().0.push(9));
            }
        });
        for expected in [vec![], vec![7], vec![7], vec![7], vec![7, 7], vec![7, 7, 9], vec![7, 7, 9], vec![7, 7, 9]] {
            world.run_schedule(GameTick);
            assert_eq!(world.resource::<Calls>().0, expected);
        }
    }

    #[test]
    fn a_callback_can_cancel_another_due_callback() {
        let mut world = world();
        #[derive(Resource)]
        struct Cancel(ServerTaskId);
        world.resource_mut::<ServerTaskScheduler>().schedule_once(1, |world, _| {
            let id = world.resource::<Cancel>().0;
            assert!(world.resource_mut::<ServerTaskScheduler>().cancel(id));
            world.resource_mut::<Calls>().0.push(1);
        });
        let id = world.resource_mut::<ServerTaskScheduler>().schedule_once(1, |world, _| {
            world.resource_mut::<Calls>().0.push(2);
        });
        world.insert_resource(Cancel(id));
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [1]);
        assert!(!world.resource_mut::<ServerTaskScheduler>().cancel(id));
    }

    #[test]
    fn zero_period_runs_at_most_once_per_tick() {
        let mut world = world();
        let id = world.resource_mut::<ServerTaskScheduler>().schedule_repeating(0, 0, |world, _| {
            world.resource_mut::<Calls>().0.push(1);
        });
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [1]);
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [1, 1]);
        assert!(world.resource_mut::<ServerTaskScheduler>().cancel(id));
        world.run_schedule(GameTick);
        assert_eq!(world.resource::<Calls>().0, [1, 1]);
    }

    #[test]
    #[should_panic(expected = "server scheduler cannot run recursively")]
    fn recursive_dispatch_is_rejected() {
        let mut world = world();
        world.resource_mut::<ServerTaskScheduler>().schedule_once(1, |world, _| {
            run_server_tasks(world);
        });
        world.run_schedule(GameTick);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn completed_off_tick_work_hands_back_on_the_tick_owner_before_due_callbacks() {
        let mut world = world();
        world.init_resource::<AsyncCalls>();
        let tick_thread = std::thread::current().id();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let events = Arc::new(Mutex::new(Vec::new()));
        let work_events = Arc::clone(&events);
        world
            .resource_mut::<ServerTaskScheduler>()
            .spawn_with_handback(
                move || {
                    started_tx.send(()).expect("worker start receiver alive");
                    release_rx.recv().expect("worker release sender alive");
                    std::thread::current().id()
                },
                move |worker_thread, world| {
                    world.resource_mut::<AsyncCalls>().0.push(worker_thread);
                    work_events.lock().expect("event log poisoned").push("async");
                },
            )
            .expect("first async task fits the default capacity");
        let due_events = Arc::clone(&events);
        world.resource_mut::<ServerTaskScheduler>().schedule_repeating(1, 1, move |_, _| {
            due_events.lock().expect("event log poisoned").push("due");
        });

        started_rx.recv_timeout(Duration::from_secs(1)).expect("worker starts off tick");
        world.run_schedule(GameTick);
        assert!(world.resource::<AsyncCalls>().0.is_empty());
        release_tx.send(()).expect("worker is waiting for release");

        for _ in 0..1000 {
            world.run_schedule(GameTick);
            if !world.resource::<AsyncCalls>().0.is_empty() {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(world.resource::<AsyncCalls>().0.len(), 1);
        assert_ne!(world.resource::<AsyncCalls>().0[0], tick_thread);
        let events = events.lock().expect("event log poisoned");
        let async_index = events.iter().position(|event| *event == "async").expect("hand-back ran");
        assert_eq!(events[async_index + 1], "due");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn async_capacity_backpressures_until_the_tick_owner_consumes_the_hand_back() {
        let mut world = world();
        world.insert_resource(ServerTaskScheduler::with_async_hand_back_capacity(1));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        world.resource_mut::<ServerTaskScheduler>()
            .spawn_with_handback(
                move || {
                    started_tx.send(()).expect("worker start receiver alive");
                    release_rx.recv().expect("worker release sender alive");
                    1_u8
                },
                |value, world| world.resource_mut::<Calls>().0.push(u64::from(value)),
            )
            .expect("first task fits");
        started_rx.recv_timeout(Duration::from_secs(1)).expect("worker starts");
        assert_eq!(
            world
                .resource_mut::<ServerTaskScheduler>()
                .spawn_with_handback(|| 2_u8, |_, _| {}),
            Err(ServerAsyncTaskError::Full)
        );
        release_tx.send(()).expect("worker is waiting for release");
        assert_eq!(
            world
                .resource_mut::<ServerTaskScheduler>()
                .spawn_with_handback(|| 2_u8, |_, _| {}),
            Err(ServerAsyncTaskError::Full),
            "a returning worker retains capacity until the tick owner drains it"
        );

        for _ in 0..1000 {
            world.run_schedule(GameTick);
            if world.resource::<Calls>().0 == [1] {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(world.resource::<Calls>().0, [1]);
        world
            .resource_mut::<ServerTaskScheduler>()
            .spawn_with_handback(|| 3_u8, |value, world| {
                world.resource_mut::<Calls>().0.push(u64::from(value));
            })
            .expect("draining the first hand-back releases capacity");
        for _ in 0..1000 {
            world.run_schedule(GameTick);
            if world.resource::<Calls>().0 == [1, 3] {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(world.resource::<Calls>().0, [1, 3]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cancellation_and_shutdown_discard_results_without_stopping_running_work() {
        let mut world = world();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let id = world
            .resource_mut::<ServerTaskScheduler>()
            .spawn_with_handback(
                move || {
                    started_tx.send(()).expect("worker start receiver alive");
                    release_rx.recv().expect("worker release sender alive");
                    finished_tx.send(()).expect("worker finish receiver alive");
                    7_u8
                },
                |value, world| world.resource_mut::<Calls>().0.push(u64::from(value)),
            )
            .expect("first task fits");
        started_rx.recv_timeout(Duration::from_secs(1)).expect("worker starts");
        assert!(world.resource_mut::<ServerTaskScheduler>().cancel_async(id));
        assert!(!world.resource_mut::<ServerTaskScheduler>().cancel_async(id));
        world.resource_mut::<ServerTaskScheduler>().shutdown_async_tasks();
        assert_eq!(
            world
                .resource_mut::<ServerTaskScheduler>()
                .spawn_with_handback(|| 8_u8, |_, _| {}),
            Err(ServerAsyncTaskError::Shutdown)
        );
        release_tx.send(()).expect("worker is waiting for release");
        finished_rx.recv_timeout(Duration::from_secs(1)).expect("cancellation does not stop work");
        for _ in 0..8 {
            world.run_schedule(GameTick);
        }
        assert!(world.resource::<Calls>().0.is_empty());
    }
}
