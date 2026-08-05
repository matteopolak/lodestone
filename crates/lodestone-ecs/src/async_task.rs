//! Issue #114 — async task hand-back: run blocking work off-tick and rejoin the
//! `World` safely.
//!
//! # What it is
//!
//! [`AsyncTaskPool`] is a small `Resource`-owned thread pool for plugin work
//! that must not block the tick: a database query, an HTTP call, or (this
//! codebase's own worked example) a pathfinding search. Results come back
//! either by polling a [`PendingTask`] from a system, or by a **hand-back
//! closure** that the pool runs with `&mut World` at a tick boundary — Bukkit's
//! `runTaskAsynchronously` followed by `runTask`.
//!
//! # The issue as written cannot be done soundly, and this is the sound variant
//!
//! The issue asks for "`spawn_async(future) -> impl Future<Output = T>` with a
//! documented 'here is how you get `T` back into a system safely'". Read
//! literally — a future a plugin `.await`s wherever it likes, which then touches
//! the `World` — that is **unsound here**, and not for an API-shape reason:
//!
//! - `docs/world-unification.md`'s Lock Discipline rule 2 forbids awaiting while
//!   holding a `World` guard. A plugin-authored future that both awaits *and*
//!   reaches the `World` is a construction whose only defence is review.
//! - `parking_lot::RwLock` is neither reentrant nor upgradable, and
//!   [`crate::handle::check`]'s own table shows three of the four
//!   held/requested combinations cannot make progress **at all**. A second
//!   thread taking a guard is legitimate (the net thread does it); a thread
//!   that is *inside* plugin work and takes a guard is how you get a hang with
//!   no panic, no error and no log line — this repo's worst failure mode.
//! - The server-ECS decision was explicitly that the server's
//!   `bevy_ecs::World` is tick-thread-owned with **no lock at all**, so there is
//!   not even a guard to take on that side. Any API premised on off-thread
//!   `World` access could not be honoured there even in principle.
//!
//! So the shape here is the one that *is* sound, and the soundness is a property
//! of the **types**, not of the documentation:
//!
//! | half | signature | can it reach the `World`? |
//! |---|---|---|
//! | off-tick work | `FnOnce() -> T + Send` | **no argument to reach it with** |
//! | hand-back | `FnOnce(T, &mut World) + Send` | yes — and it runs on the tick thread |
//!
//! The off-tick closure takes **no parameters**. There is no `&World`, no
//! `&mut World`, no [`crate::EcsHandle`] handed in — so the ordinary way to
//! violate rule 2 is not merely discouraged, it is unrepresentable. The
//! hand-back closure does get `&mut World`, and it runs inside
//! [`drain_completed_tasks`], an exclusive system on the tick thread, where the
//! borrow it receives is the driver's own guard passed one frame deeper (same
//! argument as [`crate::scheduler`]'s).
//!
//! # The one hole the types cannot close, and the runtime guard that does
//!
//! A plugin can still *capture* an [`crate::EcsHandle`] clone in the off-tick
//! closure and call [`crate::hold_write`] on it from the worker thread. No
//! signature can prevent that: `EcsHandle` is `Arc<RwLock<World>>`, which is
//! `Send` because it has to be — the net thread holds one.
//!
//! The issue asks for "a compile-time lint if feasible". It is not feasible
//! (Rust has no negative trait bound to say "this closure captures no
//! `EcsHandle`"), so this module does the next best thing and makes it a
//! **loud runtime failure instead of a hang**: every pool worker thread marks
//! itself with [`IN_WORKER`] for its whole lifetime, and
//! [`crate::handle::Ledger::enter`] — the ledger `hold_read`/`hold_write`
//! already run for rule 1 — refuses on a marked thread with a message naming
//! rule 2 and both call sites.
//!
//! **What this is blind to, named rather than left to be discovered:**
//!
//! - `EcsHandle` is a type *alias* for `Arc<RwLock<World>>`, so `handle.read()`
//!   and `handle.write()` are `parking_lot`'s own inherent methods. They cannot
//!   be intercepted, so a worker that calls them directly still hangs. That is
//!   the same gap issue #20 ("route the ~12 direct `ecs.read()` calls through
//!   `hold_read`") exists to close, and closing it makes this guard total. Until
//!   then, the guard covers the *sanctioned* path and nothing more.
//! - Threads a plugin spawns itself with `std::thread::spawn` are not marked.
//!   The guard is a property of *this pool's* workers, which is what the pool
//!   can honestly speak for.
//!
//! # Determinism, and why the drain is at a tick boundary
//!
//! Completions are drained in [`crate::TickSet::Input`], before
//! [`crate::scheduler::run_due_tasks`] — so a hand-back's effect is visible to
//! any scheduled task the same tick, and the ordering is a declared fact rather
//! than whatever the topological sort happened to pick.
//!
//! Which *tick* a given job's hand-back lands on is inherently
//! nondeterministic — it depends on when the worker finished — so nothing here
//! pretends otherwise. What is deterministic is that the hand-back runs on the
//! tick thread, at a schedule point, never concurrently with a tick.
//!
//! # wasm32
//!
//! There are no threads (`docs/bevy-migration.md` §3.1; `bevy_ecs`'s
//! `multi_threaded` does not even compile there). On that target
//! [`AsyncTaskPool::spawn`] runs the closure **inline, on the caller**, and
//! queues its result exactly as a worker would. The API and the observable
//! completion path are identical; the "off-tick" property is the one thing that
//! is not, and it is not silently absent — see
//! [`AsyncTaskPool::runs_work_inline`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Component, IntoScheduleConfigs, Resource};
use bevy_ecs::world::World;
use parking_lot::Mutex;

use crate::schedules::GameTick;
use crate::sets::TickSet;

thread_local! {
    /// Set for the whole lifetime of an [`AsyncTaskPool`] worker thread. Read by
    /// [`assert_not_in_async_worker`], which `handle.rs`'s guard ledger calls.
    ///
    /// A plain `Cell<bool>` rather than a registry of thread ids: the question is
    /// "am *I* a worker", which is exactly what a thread-local answers, and it
    /// costs one TLS read per guard — negligible against acquiring an `RwLock`,
    /// and it is always on rather than `debug_assertions`-only, because a check
    /// that silently vanishes in release is `CLAUDE.md`'s *precondition* species
    /// of vacuous test.
    static IN_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current thread as a pool worker for as long as it lives. `Drop`
/// rather than a matching clear, so an unwinding job leaves the flag correct.
struct WorkerMark;

impl WorkerMark {
    fn set() -> Self {
        IN_WORKER.set(true);
        Self
    }
}

impl Drop for WorkerMark {
    fn drop(&mut self) {
        IN_WORKER.set(false);
    }
}

/// Whether the calling thread is an [`AsyncTaskPool`] worker.
///
/// Public because a plugin writing its own off-tick helper may reasonably want
/// to assert the same thing, and because it makes the guard below testable from
/// outside this module.
#[must_use]
pub fn in_async_worker() -> bool {
    IN_WORKER.get()
}

/// Panics if called from an [`AsyncTaskPool`] worker thread. Called by
/// `handle.rs`'s guard ledger, so it fires on every [`crate::hold_read`] /
/// [`crate::hold_write`] — see the module doc for what it is blind to.
///
/// # Panics
///
/// On a worker thread, with a message naming Lock Discipline rule 2 and the
/// sound alternative.
#[track_caller]
pub(crate) fn assert_not_in_async_worker(write: bool) {
    if !IN_WORKER.get() {
        return;
    }
    let kind = if write { "write" } else { "read" };
    panic!(
        "World guard taken from an AsyncTaskPool worker thread: a {kind} guard was \
         requested at {at} while running off-tick plugin work.\n\
         \n\
         This is docs/world-unification.md's Lock Discipline rule 2. The tick thread \
         holds this same lock for the whole of NetIngest/GameTick/Extract, and \
         parking_lot's RwLock is not reentrant, so this either blocks the worker for \
         a whole frame or deadlocks outright — with no panic and no log line, which \
         is why it is turned into this message instead.\n\
         \n\
         The off-tick half of AsyncTaskPool::spawn takes NO arguments precisely so \
         it cannot reach the World; reaching it anyway means an EcsHandle was \
         captured. Do the World work in the hand-back instead: \
         `pool.spawn_with_handback(|| expensive(), |result, world| {{ ... }})` runs the \
         second closure on the tick thread, at a schedule point, with the borrow the \
         driver already holds.",
        at = std::panic::Location::caller(),
    );
}

/// A hand-back: the tick-thread half of one finished job, with its result
/// already moved in. Type-erased at the point where `T` is still known (inside
/// the worker), which is why the pool needs no type parameter.
type HandBack = Box<dyn FnOnce(&mut World) + Send + 'static>;

/// A queued unit of off-tick work.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The result slot a [`PendingTask`] polls. Separate from the pool so a handle
/// outlives the job and stays valid after the pool is dropped.
struct Slot<T> {
    value: Mutex<Option<T>>,
    done: AtomicBool,
}

/// A poll-able handle to one off-tick job's result — the issue's
/// "`PendingTask<T>` component/resource pattern for polling completion inside a
/// system".
///
/// Derives `Component`, so the common shape works: insert it on an entity, and
/// a `Query<(Entity, &PendingTask<T>)>` system takes the result out on whatever
/// tick it lands. It is equally usable inside a plugin's own `Resource`.
///
/// `T: Send + Sync + 'static` because a `Component` is; the pool itself only
/// needs `T: Send`, which is why [`AsyncTaskPool::spawn_with_handback`] has the
/// looser bound.
#[derive(Component)]
pub struct PendingTask<T: Send + Sync + 'static> {
    slot: Arc<Slot<T>>,
}

impl<T: Send + Sync + 'static> std::fmt::Debug for PendingTask<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingTask")
            .field("finished", &self.is_finished())
            .finish()
    }
}

impl<T: Send + Sync + 'static> PendingTask<T> {
    /// Whether the work has finished. `true` does not mean the value is still
    /// here — [`Self::try_take`] moves it out — so a poller should branch on
    /// `try_take` rather than on this. Exposed because "finished but already
    /// taken" is a real state and a caller that cannot distinguish it will
    /// double-handle.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.slot.done.load(Ordering::Acquire)
    }

    /// Take the result if it is ready, leaving the slot empty. Returns `None`
    /// while the work is still running *and* after the value has been taken.
    #[must_use]
    pub fn try_take(&self) -> Option<T> {
        if !self.is_finished() {
            return None;
        }
        self.slot.value.lock().take()
    }
}

struct Shared {
    /// Work not yet picked up. `Mutex<Vec<_>>` + a condvar rather than a channel
    /// so `Drop` can wake every worker without needing one sender per thread.
    queue: Mutex<Vec<Job>>,
    ready: parking_lot::Condvar,
    /// Finished hand-backs waiting for a tick boundary.
    completions: Mutex<Vec<HandBack>>,
    shutdown: AtomicBool,
    spawned: AtomicU64,
    completed: AtomicU64,
    handed_back: AtomicU64,
}

impl Shared {
    fn push_job(&self, job: Job) {
        self.queue.lock().push(job);
        self.ready.notify_one();
    }
}

/// Counters for one [`AsyncTaskPool`]. A **count**, deliberately, not a
/// duration: `CLAUDE.md` is explicit that a timing taken while other work is
/// live gets attributed to the wrong cause, and every question worth asking here
/// ("did anything actually run off-tick", "did every job hand back") is a
/// counting question.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Jobs handed to the pool.
    pub spawned: u64,
    /// Jobs whose off-tick closure returned.
    pub completed: u64,
    /// Hand-backs run on the tick thread by [`drain_completed_tasks`].
    pub handed_back: u64,
    /// Hand-backs finished but not yet drained.
    pub pending_hand_backs: usize,
}

/// The plugin-facing thread pool. `init_resource`'d by
/// [`AsyncTaskPoolPlugin`].
///
/// Cloning is cheap and shares one pool (`Arc` inside), so a plugin may keep its
/// own clone in its own `Resource` rather than reaching for
/// `Res<AsyncTaskPool>` — useful because [`AsyncTaskPool::spawn`] needs only
/// `&self`.
#[derive(Resource, Clone)]
pub struct AsyncTaskPool {
    shared: Arc<Shared>,
    #[cfg(not(target_arch = "wasm32"))]
    workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl std::fmt::Debug for AsyncTaskPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncTaskPool")
            .field("stats", &self.stats())
            .field("runs_work_inline", &Self::runs_work_inline())
            .finish()
    }
}

impl Default for AsyncTaskPool {
    /// Two worker threads. Small on purpose: plugin off-tick work is
    /// latency-bound (a query, a request, a search), not throughput-bound, and
    /// `CLAUDE.md`'s machine notes are emphatic about unbounded thread and
    /// memory growth on this box. Use [`AsyncTaskPool::with_threads`] for more.
    fn default() -> Self {
        Self::with_threads(2)
    }
}

impl AsyncTaskPool {
    /// Whether this build runs off-tick work **inline on the caller** instead of
    /// on a worker thread. `true` only on wasm32, which has no threads.
    ///
    /// A plugin that must not block should branch on this rather than assume;
    /// the alternative is a silently-blocking tick on one target, which is the
    /// kind of gap `CLAUDE.md` asks to be named rather than discovered.
    #[must_use]
    pub const fn runs_work_inline() -> bool {
        cfg!(target_arch = "wasm32")
    }

    /// Build a pool with `threads` workers (clamped to at least 1). On wasm32
    /// the count is recorded and ignored — see [`Self::runs_work_inline`].
    #[must_use]
    pub fn with_threads(threads: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Vec::new()),
            ready: parking_lot::Condvar::new(),
            completions: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            spawned: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            handed_back: AtomicU64::new(0),
        });

        #[cfg(not(target_arch = "wasm32"))]
        let workers = {
            let mut handles = Vec::new();
            for n in 0..threads.max(1) {
                let shared = Arc::clone(&shared);
                let handle = std::thread::Builder::new()
                    .name(format!("lodestone-plugin-async-{n}"))
                    .spawn(move || worker_loop(&shared))
                    .expect("spawning an AsyncTaskPool worker thread");
                handles.push(handle);
            }
            Arc::new(Mutex::new(handles))
        };
        #[cfg(target_arch = "wasm32")]
        let _ = threads;

        Self {
            shared,
            #[cfg(not(target_arch = "wasm32"))]
            workers,
        }
    }

    /// Run `work` off-tick and hand its result back to a closure that runs on
    /// the **tick thread**, with `&mut World`, inside
    /// [`drain_completed_tasks`].
    ///
    /// This is the issue's `runTaskAsynchronously` + `runTask` pair, and it is
    /// the recommended shape: `work` takes no arguments, so it cannot reach the
    /// `World`; `hand_back` gets the `World` but never runs concurrently with a
    /// tick.
    ///
    /// Which tick the hand-back lands on depends on when the worker finished and
    /// is not deterministic. That it runs on the tick thread, at a schedule
    /// point, is.
    pub fn spawn_with_handback<T, W, H>(&self, work: W, hand_back: H)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        H: FnOnce(T, &mut World) + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        self.shared.spawned.fetch_add(1, Ordering::Relaxed);
        self.run(Box::new(move || {
            let value = work();
            shared.completed.fetch_add(1, Ordering::Relaxed);
            // Erase `T` here, where it is still known.
            let boxed: HandBack = Box::new(move |world: &mut World| hand_back(value, world));
            shared.completions.lock().push(boxed);
        }));
    }

    /// Run `work` off-tick and return a [`PendingTask`] to poll from a system.
    ///
    /// Use this when the result belongs to an entity (insert the handle as a
    /// component) or when the plugin wants to decide *when* to consume it.
    /// [`Self::spawn_with_handback`] is the better default otherwise — nothing
    /// has to remember to poll.
    #[must_use]
    pub fn spawn<T, W>(&self, work: W) -> PendingTask<T>
    where
        T: Send + Sync + 'static,
        W: FnOnce() -> T + Send + 'static,
    {
        let slot = Arc::new(Slot {
            value: Mutex::new(None),
            done: AtomicBool::new(false),
        });
        let job_slot = Arc::clone(&slot);
        let shared = Arc::clone(&self.shared);
        self.shared.spawned.fetch_add(1, Ordering::Relaxed);
        self.run(Box::new(move || {
            let value = work();
            *job_slot.value.lock() = Some(value);
            // `Release` pairs with `is_finished`'s `Acquire`, so a poller that
            // observes `done` also observes the value being in the slot.
            job_slot.done.store(true, Ordering::Release);
            shared.completed.fetch_add(1, Ordering::Relaxed);
        }));
        PendingTask { slot }
    }

    /// Counters. See [`PoolStats`] on why this is a count and not a duration.
    #[must_use]
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            spawned: self.shared.spawned.load(Ordering::Relaxed),
            completed: self.shared.completed.load(Ordering::Relaxed),
            handed_back: self.shared.handed_back.load(Ordering::Relaxed),
            pending_hand_backs: self.shared.completions.lock().len(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run(&self, job: Job) {
        self.shared.push_job(job);
    }

    /// wasm32 has no threads, so the job runs here. Marked as a worker for its
    /// duration anyway, so the rule-2 guard behaves identically on both
    /// targets — a plugin that captures an `EcsHandle` gets the same panic, not
    /// a target-specific one.
    #[cfg(target_arch = "wasm32")]
    fn run(&self, job: Job) {
        let _mark = WorkerMark::set();
        job();
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for AsyncTaskPool {
    fn drop(&mut self) {
        // Only the last clone tears the pool down: `workers` is behind the same
        // `Arc` every clone shares, so a plugin holding its own clone does not
        // kill the pool when its resource is replaced.
        if Arc::strong_count(&self.workers) > 1 {
            return;
        }
        // **Take the queue lock before setting the flag.** Without this there is
        // a lost-wakeup race that hangs `join` below forever, and it is not
        // theoretical — it hung `cargo test -p lodestone-ecs` on this crate's own
        // `a_pending_task_survives_the_pool_being_dropped` under load:
        //
        //   worker: locks queue, sees shutdown == false, has not yet called wait
        //   drop:   sets shutdown = true, calls notify_all -> NO waiters, no-op
        //   worker: calls wait(), releasing the lock, and sleeps forever
        //   drop:   join() blocks forever
        //
        // `wait` releases the lock atomically as it sleeps, so acquiring it here
        // means the worker is either already waiting (and `notify_all` reaches
        // it) or has not yet checked the flag (and will see it set). The lock
        // must be released before `notify_all`, or the woken worker immediately
        // blocks re-acquiring it.
        {
            let _queue = self.shared.queue.lock();
            self.shared.shutdown.store(true, Ordering::Release);
        }
        self.shared.ready.notify_all();
        let handles: Vec<_> = std::mem::take(&mut *self.workers.lock());
        for handle in handles {
            // A worker that panicked (the rule-2 guard fires inside one, by
            // design, in this crate's own tests) must not turn into a panic
            // while unwinding here.
            let _ = handle.join();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worker_loop(shared: &Arc<Shared>) {
    // Marked for the thread's whole life, not per job, so the guard fires even
    // for work a job spawns onto its own stack.
    let _mark = WorkerMark::set();
    loop {
        let job = {
            let mut queue = shared.queue.lock();
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(job) = queue.pop() {
                    break Some(job);
                }
                shared.ready.wait(&mut queue);
            }
        };
        let Some(job) = job else { return };
        // A panicking plugin job must not take the worker down with it — issue
        // #168's concern, handled locally here rather than left to chance,
        // because a dead worker silently stops draining the queue.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        if outcome.is_err() {
            // `catch_unwind` already printed the panic through the hook.
            shared.completed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// [`crate::TickSet::Input`], exclusive: run every hand-back whose off-tick work
/// has finished, on the tick thread.
///
/// The completions list is drained under a short lock that is released *before*
/// any hand-back runs, so a hand-back may itself call
/// [`AsyncTaskPool::spawn_with_handback`] without deadlocking on the pool's own
/// mutex — the same reentrancy shape [`crate::scheduler::run_due_tasks`] solves
/// for tasks, and just as easy to get wrong.
pub fn drain_completed_tasks(world: &mut World) {
    let Some(pool) = world.get_resource::<AsyncTaskPool>() else {
        return;
    };
    let shared = Arc::clone(&pool.shared);
    let ready: Vec<HandBack> = std::mem::take(&mut *shared.completions.lock());
    for hand_back in ready {
        hand_back(world);
        shared.handed_back.fetch_add(1, Ordering::Relaxed);
    }
}

/// Installs [`AsyncTaskPool`] and [`drain_completed_tasks`].
///
/// Adds [`crate::scheduler::SchedulerPlugin`] if absent, and orders the drain
/// **before** [`crate::scheduler::run_due_tasks`]. Both are exclusive systems in
/// the same set, so without an explicit edge their relative order is whatever
/// the topological sort picks — a hand-back landing before or after a scheduled
/// task depending on nothing observable. The coupling is one-directional (the
/// scheduler knows nothing about the pool) and the two are one feature in
/// Bukkit's own API, so this buys determinism cheaply.
#[derive(Debug, Default)]
pub struct AsyncTaskPoolPlugin;

impl Plugin for AsyncTaskPoolPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::scheduler::SchedulerPlugin>() {
            app.add_plugins(crate::scheduler::SchedulerPlugin);
        }
        app.init_resource::<AsyncTaskPool>();
        app.add_systems(
            GameTick,
            drain_completed_tasks
                .in_set(TickSet::Input)
                .before(crate::scheduler::run_due_tasks),
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {
    //! As in [`crate::scheduler`], every gate drives the **registry**: it adds
    //! [`AsyncTaskPoolPlugin`] the way a third-party plugin would and runs
    //! `GameTick` through `run_schedule`, never calling
    //! [`drain_completed_tasks`] by hand.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use bevy_app::App;
    use bevy_ecs::resource::Resource;
    use parking_lot::Mutex;

    use super::{AsyncTaskPool, AsyncTaskPoolPlugin, PendingTask};
    use crate::schedules::GameTick;

    /// Generous, and a *bound* rather than a measurement: nothing here asserts
    /// on elapsed time (`CLAUDE.md`: a timing taken while other agents run gets
    /// attributed to the wrong cause), it only refuses to hang forever.
    const DEADLINE: Duration = Duration::from_secs(10);

    fn app_with_pool() -> App {
        let mut app = App::new();
        app.add_plugins(AsyncTaskPoolPlugin);
        app
    }

    /// Runs `GameTick` until `done` returns true or [`DEADLINE`] expires,
    /// returning how many ticks it took. Uses the driver's own entry point.
    fn tick_until(app: &mut App, mut done: impl FnMut(&App) -> bool) -> u32 {
        let start = Instant::now();
        let mut ticks = 0;
        while !done(app) {
            assert!(
                start.elapsed() < DEADLINE,
                "off-tick work did not complete within {DEADLINE:?} after {ticks} ticks"
            );
            app.world_mut().run_schedule(GameTick);
            ticks += 1;
            std::thread::yield_now();
        }
        ticks
    }

    #[test]
    fn the_plugin_installs_the_pool() {
        let app = app_with_pool();
        assert!(app.world().get_resource::<AsyncTaskPool>().is_some());
    }

    /// The hand-back really runs, really gets the live `World`, and the counters
    /// agree: one spawn, one completion, one hand-back, nothing left pending.
    #[test]
    fn a_hand_back_runs_on_the_tick_thread_and_mutates_the_real_world() {
        #[derive(Resource, Default)]
        struct Answer(u32);

        let mut app = app_with_pool();
        app.init_resource::<Answer>();
        let pool = app.world().resource::<AsyncTaskPool>().clone();
        pool.spawn_with_handback(
            || 6 * 7,
            |value: u32, world: &mut bevy_ecs::world::World| {
                world.resource_mut::<Answer>().0 = value;
            },
        );

        tick_until(&mut app, |app| app.world().resource::<Answer>().0 != 0);
        assert_eq!(app.world().resource::<Answer>().0, 42);

        let stats = app.world().resource::<AsyncTaskPool>().stats();
        assert_eq!(
            (stats.spawned, stats.completed, stats.handed_back, stats.pending_hand_backs),
            (1, 1, 1, 0),
            "every job must be spawned, completed, handed back exactly once and leave nothing queued"
        );
    }

    /// **The control for the hand-back gate.** With no registered drain system,
    /// the job still completes off-tick (`completed == 1`) but the hand-back
    /// never runs and stays queued forever — so the assertion above is
    /// measuring the *drain*, not merely that a thread ran.
    ///
    /// Observed failing as designed: swapping `CorePlugin` for
    /// `AsyncTaskPoolPlugin` here makes the final `handed_back` read 1, not 0,
    /// and `Answer` becomes 42.
    #[test]
    fn with_no_pool_plugin_the_work_runs_off_tick_but_no_hand_back_ever_lands() {
        #[derive(Resource, Default)]
        struct Answer(u32);

        let mut app = App::new();
        app.add_plugins(crate::CorePlugin);
        app.init_resource::<Answer>();
        // Insert the pool by hand, so the *only* missing thing is the system.
        app.world_mut().insert_resource(AsyncTaskPool::default());
        let pool = app.world().resource::<AsyncTaskPool>().clone();
        pool.spawn_with_handback(
            || 6 * 7,
            |value: u32, world: &mut bevy_ecs::world::World| {
                world.resource_mut::<Answer>().0 = value;
            },
        );

        // Wait for the *worker* to finish, which does not need the schedule.
        let start = Instant::now();
        while pool.stats().completed == 0 {
            assert!(start.elapsed() < DEADLINE, "the worker never ran");
            std::thread::yield_now();
        }
        for _ in 0..20 {
            app.world_mut().run_schedule(GameTick);
        }
        let stats = pool.stats();
        assert_eq!(stats.completed, 1, "the off-tick half must still have run");
        assert_eq!(stats.handed_back, 0, "no drain system is registered");
        assert_eq!(stats.pending_hand_backs, 1, "so the hand-back is still queued");
        assert_eq!(app.world().resource::<Answer>().0, 0);
    }

    /// The `PendingTask` polling shape, consumed from a real system through a
    /// `Query` — the component half the issue asks for, not just the method.
    #[test]
    fn a_pending_task_component_is_polled_by_a_system_and_yields_its_value() {
        use bevy_ecs::prelude::{Commands, Entity, Query};

        #[derive(Resource, Default)]
        struct Collected(Vec<u32>);

        fn collect(
            mut commands: Commands,
            tasks: Query<(Entity, &PendingTask<u32>)>,
            mut out: bevy_ecs::system::ResMut<Collected>,
        ) {
            for (entity, task) in &tasks {
                if let Some(value) = task.try_take() {
                    out.0.push(value);
                    commands.entity(entity).despawn();
                }
            }
        }

        let mut app = app_with_pool();
        app.init_resource::<Collected>();
        app.add_systems(GameTick, collect);

        let pool = app.world().resource::<AsyncTaskPool>().clone();
        for n in 1..=3u32 {
            let task = pool.spawn(move || n * 100);
            app.world_mut().spawn(task);
        }

        tick_until(&mut app, |app| app.world().resource::<Collected>().0.len() == 3);
        let mut got = app.world().resource::<Collected>().0.clone();
        got.sort_unstable();
        assert_eq!(got, vec![100, 200, 300]);
    }

    /// `try_take` moves the value out: a second call returns `None` even though
    /// `is_finished` stays true. The distinction exists so a poller cannot
    /// double-handle, and this is what would catch it collapsing.
    #[test]
    fn try_take_yields_the_value_once_and_is_finished_stays_true() {
        let pool = AsyncTaskPool::with_threads(1);
        let task = pool.spawn(|| 7u32);
        let start = Instant::now();
        while !task.is_finished() {
            assert!(start.elapsed() < DEADLINE, "the worker never ran");
            std::thread::yield_now();
        }
        assert_eq!(task.try_take(), Some(7));
        assert_eq!(task.try_take(), None, "the value must not be handed out twice");
        assert!(task.is_finished(), "is_finished reports the work, not the slot");
    }

    /// A `PendingTask` outlives its pool: the result slot is a separate `Arc`,
    /// so dropping the pool (a plugin swapping resources, a `World` teardown)
    /// does not invalidate a handle a system is still holding.
    #[test]
    fn a_pending_task_survives_the_pool_being_dropped() {
        let task = {
            let pool = AsyncTaskPool::with_threads(1);
            let task = pool.spawn(|| 5u32);
            let start = Instant::now();
            while !task.is_finished() {
                assert!(start.elapsed() < DEADLINE, "the worker never ran");
                std::thread::yield_now();
            }
            task
        };
        assert_eq!(task.try_take(), Some(5));
    }

    // -----------------------------------------------------------------------
    // Rule 2, enforced rather than reviewed
    // -----------------------------------------------------------------------

    /// The soundness gate. A worker that captures an [`crate::EcsHandle`] and
    /// calls [`crate::hold_write`] gets a **panic naming rule 2**, not a
    /// deadlock. The panic is caught inside the job and shipped back, so the
    /// test observes the message rather than the process hanging.
    #[test]
    fn a_worker_that_takes_a_world_guard_panics_naming_rule_two() {
        let pool = AsyncTaskPool::with_threads(1);
        let handle = crate::new_handle();
        let message: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicU32::new(0));

        {
            let handle = handle.clone();
            let message = Arc::clone(&message);
            let signal = Arc::clone(&done);
            // The panic hook would otherwise print this expected panic and make
            // the test output look like a failure.
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            pool.spawn_with_handback(
                move || {
                    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::hold_write(&handle, |_world| {});
                    }));
                    if let Err(payload) = caught {
                        let text = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                            .unwrap_or_default();
                        *message.lock() = Some(text);
                    }
                    signal.fetch_add(1, Ordering::Release);
                },
                |(), _world| {},
            );
            let start = Instant::now();
            while done.load(Ordering::Acquire) == 0 {
                assert!(start.elapsed() < DEADLINE, "the worker never ran");
                std::thread::yield_now();
            }
            std::panic::set_hook(previous);
        }

        let text = message.lock().clone().unwrap_or_default();
        assert!(
            text.contains("AsyncTaskPool worker thread"),
            "the panic must name where it came from; got: {text}"
        );
        assert!(
            text.contains("rule 2"),
            "the panic must name Lock Discipline rule 2; got: {text}"
        );
        assert!(
            text.contains("spawn_with_handback"),
            "the panic must name the sound alternative; got: {text}"
        );
    }

    /// **The control for the gate above**, and the one that matters most: the
    /// *identical* `hold_write` call on the tick thread must **succeed**. Without
    /// this, the guard could be panicking unconditionally — refusing every
    /// `World` guard everywhere — and the test above would still pass while the
    /// whole client was broken.
    #[test]
    fn the_identical_guard_on_the_tick_thread_succeeds() {
        let handle = crate::new_handle();
        assert!(!super::in_async_worker(), "the test thread is not a worker");
        let ran = crate::hold_write(&handle, |_world| 1u32);
        assert_eq!(ran, 1, "hold_write must be unaffected off a worker thread");
    }

    /// The marker is a property of the *worker* thread, not of the process — so
    /// a pool existing does not poison the tick thread. The pair of reads is
    /// what makes the flag discriminating rather than a constant.
    #[test]
    fn the_worker_marker_is_set_on_workers_and_clear_on_the_tick_thread() {
        let pool = AsyncTaskPool::with_threads(1);
        let seen = Arc::new(AtomicU32::new(u32::MAX));
        {
            let seen = Arc::clone(&seen);
            let _marked = pool.spawn(move || {
                seen.store(u32::from(super::in_async_worker()), Ordering::Release);
            });
        }
        let start = Instant::now();
        while seen.load(Ordering::Acquire) == u32::MAX {
            assert!(start.elapsed() < DEADLINE, "the worker never ran");
            std::thread::yield_now();
        }
        assert_eq!(seen.load(Ordering::Acquire), 1, "a worker must be marked");
        assert!(!super::in_async_worker(), "the tick thread must not be");
    }

    /// A panicking plugin job must not kill the worker: the pool keeps draining.
    /// Issue #168's concern, bounded locally, because a dead worker silently
    /// stops all later work.
    #[test]
    fn a_panicking_job_does_not_kill_the_worker() {
        let pool = AsyncTaskPool::with_threads(1);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let _doomed = pool.spawn(|| panic!("plugin bug"));
        let survivor = pool.spawn(|| 11u32);
        let start = Instant::now();
        while !survivor.is_finished() {
            assert!(
                start.elapsed() < DEADLINE,
                "the worker died with the panicking job"
            );
            std::thread::yield_now();
        }
        std::panic::set_hook(previous);
        assert_eq!(survivor.try_take(), Some(11));
    }

    /// A hand-back that spawns more work must not deadlock on the pool's own
    /// completions mutex — the reason [`super::drain_completed_tasks`] takes the
    /// list out before running anything.
    #[test]
    fn a_hand_back_can_spawn_more_work() {
        #[derive(Resource, Default)]
        struct Stage(u32);

        let mut app = app_with_pool();
        app.init_resource::<Stage>();
        let pool = app.world().resource::<AsyncTaskPool>().clone();
        pool.spawn_with_handback(
            || 1u32,
            |first: u32, world: &mut bevy_ecs::world::World| {
                world.resource_mut::<Stage>().0 = first;
                let pool = world.resource::<AsyncTaskPool>().clone();
                pool.spawn_with_handback(
                    move || first + 1,
                    |second: u32, world: &mut bevy_ecs::world::World| {
                        world.resource_mut::<Stage>().0 = second;
                    },
                );
            },
        );
        tick_until(&mut app, |app| app.world().resource::<Stage>().0 == 2);
        assert_eq!(app.world().resource::<Stage>().0, 2);
    }

    /// This build genuinely runs work off-tick — the `runs_work_inline` claim is
    /// checked rather than asserted in prose. On a native target the job must be
    /// observed on a *different* thread from the caller.
    #[test]
    fn native_builds_run_work_on_another_thread() {
        assert!(!AsyncTaskPool::runs_work_inline());
        let pool = AsyncTaskPool::with_threads(1);
        let here = std::thread::current().id();
        let task = pool.spawn(move || std::thread::current().id() != here);
        let start = Instant::now();
        while !task.is_finished() {
            assert!(start.elapsed() < DEADLINE, "the worker never ran");
            std::thread::yield_now();
        }
        assert_eq!(task.try_take(), Some(true), "work must not run on the caller");
    }

    /// The drain is ordered `.before(run_due_tasks)`, and the graph accepts the
    /// edge — declared *and* built, since an ordering edge that bevy rejects is
    /// a startup panic for whoever installs both.
    ///
    /// [`AsyncTaskPoolPlugin`] pulls the scheduler in itself, so this also
    /// checks that transitive add really happened rather than assuming it.
    #[test]
    fn the_pool_plugin_pulls_in_the_scheduler_and_builds_one_clean_schedule() {
        let mut app = app_with_pool();
        assert!(
            app.world().get_resource::<crate::TaskScheduler>().is_some(),
            "AsyncTaskPoolPlugin must add SchedulerPlugin transitively"
        );
        app.world_mut().schedule_scope(GameTick, |world, schedule| {
            schedule
                .initialize(world)
                .expect("the drain must order cleanly against run_due_tasks");
        });
    }

    /// The reverse registration order. This is the one that could break: with
    /// the scheduler added *first*, `AsyncTaskPoolPlugin`'s `is_plugin_added`
    /// check is what stops bevy's "plugin was already added in application"
    /// panic — measured, because that panic is exactly what an earlier draft of
    /// this test hit.
    #[test]
    fn adding_the_scheduler_before_the_pool_also_builds_cleanly() {
        let mut app = App::new();
        app.add_plugins(crate::scheduler::SchedulerPlugin);
        app.add_plugins(AsyncTaskPoolPlugin);
        assert!(app.world().get_resource::<AsyncTaskPool>().is_some());
        assert!(app.world().get_resource::<crate::TaskScheduler>().is_some());
        app.world_mut().schedule_scope(GameTick, |world, schedule| {
            schedule
                .initialize(world)
                .expect("the ordering edge must hold in either registration order");
        });
    }

    /// **The regression gate for the lost-wakeup race in `Drop`.** Creating and
    /// dropping many pools, each with several idle workers, is what makes the
    /// check-then-wait window get hit: the failure mode is `join` blocking
    /// forever, so a hang here *is* the failure.
    ///
    /// Observed failing before the fix — `cargo test -p lodestone-ecs` stalled on
    /// `a_pending_task_survives_the_pool_being_dropped` with
    /// "has been running for over 60 seconds" and never finished. A test that can
    /// only fail by hanging is unusual, so the loop count is high enough to make
    /// the race overwhelmingly likely rather than relying on one attempt.
    #[test]
    fn dropping_many_idle_pools_never_hangs() {
        for _ in 0..200 {
            let pool = AsyncTaskPool::with_threads(4);
            // Idle workers, i.e. all of them parked in `wait` or about to be —
            // exactly the state the race needs.
            drop(pool);
        }
    }

    /// The same race with work in flight, so a worker may be running a job rather
    /// than parked when shutdown lands. Both arms of the wait loop matter.
    #[test]
    fn dropping_a_busy_pool_never_hangs() {
        for _ in 0..100 {
            let pool = AsyncTaskPool::with_threads(2);
            for _ in 0..8 {
                let _task = pool.spawn(|| 1u32);
            }
            drop(pool);
        }
    }

    /// `drain_completed_tasks` on a `World` with no pool returns quietly.
    #[test]
    fn the_drain_is_a_no_op_with_no_pool_resource() {
        let mut world = bevy_ecs::world::World::new();
        super::drain_completed_tasks(&mut world);
    }
}
