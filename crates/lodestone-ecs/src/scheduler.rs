//! Issue #113 — the sync scheduler: `runTaskLater`/`runTaskTimer` for plugins.
//!
//! # What it is
//!
//! [`TaskScheduler`] is a `Resource` holding closures to run on a future
//! [`crate::GameTick`], once ([`TaskScheduler::schedule_once`]) or on a period
//! ([`TaskScheduler::schedule_repeating`]). It is Bukkit's
//! `BukkitScheduler.runTaskLater` / `runTaskTimer` — "the single most-used
//! non-event API surface in the Java ecosystem" per the issue — and it exists
//! to delete the counter-component-plus-per-tick-system boilerplate a plugin
//! author otherwise hand-rolls for every cooldown, countdown and particle loop.
//!
//! # The closure signature, and why it is `&mut World` rather than `Commands`
//!
//! The issue names the signature as "the main design question", flagging that
//! `FnMut(&mut World)` "re-opens the reentrancy question if the closure is
//! invoked while a guard is already held elsewhere". It does not, and the
//! reason is the thing worth writing down:
//!
//! [`run_due_tasks`] is an **exclusive** system. By the time bevy calls it, the
//! driver has already taken the `EcsHandle` write guard and handed the
//! resulting `&mut World` down through `World::run_schedule(GameTick)` — so the
//! `&mut World` a task closure receives *is that same borrow*, passed further
//! down the stack. Nothing re-locks anything. A task closure therefore cannot
//! deadlock against the guard the driver holds, because it never acquires a
//! guard at all; it is handed the one that already exists.
//!
//! What *would* deadlock is a closure that captured an [`crate::EcsHandle`]
//! clone and called `hold_write` on it — the reentrancy hazard is real, it is
//! simply not created by this signature. That is
//! [`crate::async_task`]'s territory (issue #114), and the runtime guard there
//! catches it for worker threads.
//!
//! `Commands` was the alternative. It is strictly weaker here: a task that
//! wants to *read* state to decide what to do (the overwhelmingly common case —
//! "is the player still holding the item?") cannot, because `Commands` only
//! queues. `&mut World` subsumes `Commands` (`world.commands()`), so nothing is
//! lost by picking the capable one.
//!
//! # Reentrancy *within* the scheduler: why due tasks are moved out, not the whole list
//!
//! The obvious implementation is `World::resource_scope::<TaskScheduler, _>`,
//! which takes the resource out of the `World`, runs the closures, and puts it
//! back. That is wrong here in a way that only shows up for a plugin doing
//! something completely reasonable: **a task that schedules another task.**
//! `resource_scope` removes the resource for the duration of the callback, so
//! `world.resource_mut::<TaskScheduler>()` inside a task closure panics with
//! "requested resource does not exist".
//!
//! So [`run_due_tasks`] moves out only the tasks that are *due* this tick,
//! leaving [`TaskScheduler`] itself present and mutable in the `World` for the
//! whole drain. A task closure can schedule, cancel, or inspect freely.
//! Anything it schedules lands in the live list and is considered from the
//! *next* tick — never re-entered this one, so a task that schedules itself
//! with `delay 0` cannot spin forever inside one tick. See
//! `tests::a_task_that_schedules_another_task_does_not_panic_and_defers_it_one_tick`.
//!
//! # Tick semantics, stated exactly rather than approximately
//!
//! Off-by-one is the entire difficulty of a scheduler, and a gate that asserts
//! "it fired at least once" is `CLAUDE.md`'s *magnitude* species of vacuous
//! test — every wrong implementation also fires at least once. So the contract
//! is stated as a firing **schedule**, and the gates assert the exact tick
//! indices, not a count:
//!
//! - `schedule_once(n, f)` runs `f` on the **n-th** [`GameTick`] from now,
//!   counting the next tick as 1. `n == 0` and `n == 1` both mean "next tick",
//!   matching Bukkit's `runTaskLater(plugin, task, 0)`.
//! - `schedule_repeating(delay, period, f)` runs `f` on tick `delay`, then
//!   every `period` ticks after that: `delay`, `delay + period`,
//!   `delay + 2 * period`, … A `period` of 0 is clamped to 1 (every tick)
//!   rather than being an infinite loop or a silent no-op.
//!
//! Over `N` ticks a repeating task therefore fires
//! `1 + (N - delay) / period` times (integer division, for `N >= delay`), and
//! `tests::a_repeating_task_fires_on_exactly_the_predicted_ticks` computes both
//! that and the off-by-one hypothesis from outside constants and requires the
//! measurement to land on one of them.
//!
//! # Where it runs, and the one thing that is deliberately deferred
//!
//! [`run_due_tasks`] is anchored in [`crate::TickSet::Input`] — the earliest
//! anchor in [`GameTick`], and empty until now (see [`crate::TickSet::Input`]'s
//! own doc). That is Bukkit's own placement: scheduled tasks run at the *start*
//! of a tick, observing the previous tick's finished state, which is why a task
//! can usefully write [`crate::MovementIntent`] and have
//! [`crate::TickSet::Intent`] and [`crate::TickSet::Physics`] act on it the
//! same tick.
//!
//! An **end-of-tick** pool (a task that wants to observe what this tick just
//! did) is a named follow-up rather than a silent gap: it needs a second anchor,
//! and adding an ordering-anchor variant is an ABI change that goes through
//! `docs/plugin-api.md`'s ordering-anchor changelog (issue #170). A task
//! needing end-of-tick semantics today schedules with `delay 1` and reads the
//! previous tick's result, which is the same information one tick later.
//!
//! # Not installed by default
//!
//! [`SchedulerPlugin`] is opt-in, exactly like [`crate::GameEventBusPlugin`]:
//! a client nobody wrote a plugin for should not pay for an exclusive system
//! that iterates an always-empty list. A plugin that wants the scheduler adds
//! the plugin (idempotent — `add_plugins` on an already-added plugin is a
//! no-op in bevy, and the `is_plugin_added` check below makes the
//! [`crate::CorePlugin`] dependency explicit).

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{IntoScheduleConfigs, Resource};
use bevy_ecs::world::World;

use crate::schedules::GameTick;
use crate::sets::TickSet;

/// Handle to a scheduled task, returned by [`TaskScheduler::schedule_once`] /
/// [`TaskScheduler::schedule_repeating`] and accepted by
/// [`TaskScheduler::cancel`].
///
/// Opaque and `Copy`: a plugin stores it in its own `Resource` to cancel later
/// (a countdown the player interrupted, a particle loop whose entity died).
/// Ids are never reused within one [`TaskScheduler`], so cancelling a task that
/// already finished is a defined no-op rather than a collision with whatever
/// was scheduled next — see
/// `tests::cancelling_an_already_finished_task_is_a_no_op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// The raw id, for a plugin that wants to log or key a map on it.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A task closure. `Send + Sync` because [`TaskScheduler`] is a bevy
/// `Resource`, which is `Send + Sync + 'static` — so a task may not capture an
/// `Rc`. This is a real constraint on plugin code and is called out in
/// `docs/plugin-scheduler.md` rather than discovered from a trait error.
type TaskFn = Box<dyn FnMut(&mut World) + Send + Sync + 'static>;

/// How a task repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeat {
    /// Run once, then drop.
    Once,
    /// Re-arm with this many ticks until the next run. Never 0 (clamped at
    /// construction), so a repeating task cannot busy-loop within one tick.
    Every(u32),
}

struct Task {
    id: TaskId,
    /// Ticks remaining *before* this task's next run: 0 means "run on the next
    /// pass of [`run_due_tasks`]". See the module doc for why this is
    /// `delay - 1` rather than `delay`.
    remaining: u32,
    repeat: Repeat,
    run: TaskFn,
}

/// The plugin-facing scheduler. `init_resource`'d by [`SchedulerPlugin`].
///
/// A plugin reaches it as `ResMut<TaskScheduler>` from any system, in any
/// schedule — scheduling is a plain `&mut self` push with no `World` access, so
/// it composes with anything.
#[derive(Resource, Default)]
pub struct TaskScheduler {
    next_id: u64,
    tasks: Vec<Task>,
    /// Ids cancelled since the last drain. Checked both when a task comes up
    /// due and when a repeating task is re-armed, so
    /// [`TaskScheduler::cancel`] works on a task that is *currently running*
    /// (a task cancelling itself) as well as on a queued one.
    cancelled: Vec<u64>,
}

impl std::fmt::Debug for TaskScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `TaskFn` is not `Debug`; report the shape rather than dropping the
        // derive and leaving the resource undebuggable in a panic message.
        f.debug_struct("TaskScheduler")
            .field("pending", &self.tasks.len())
            .field("cancelled", &self.cancelled.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl TaskScheduler {
    /// Run `f` on the `delay_ticks`-th [`GameTick`] from now, once.
    ///
    /// `delay_ticks` of 0 or 1 both mean "next tick" (Bukkit's
    /// `runTaskLater(plugin, task, 0)`). See the module doc for the exact
    /// firing schedule.
    pub fn schedule_once(
        &mut self,
        delay_ticks: u32,
        f: impl FnMut(&mut World) + Send + Sync + 'static,
    ) -> TaskId {
        self.push(delay_ticks, Repeat::Once, Box::new(f))
    }

    /// Run `f` on the `delay_ticks`-th [`GameTick`] from now, and every
    /// `period_ticks` after that, until [`TaskScheduler::cancel`].
    ///
    /// `period_ticks` of 0 is clamped to 1 (run every tick). A zero period
    /// cannot mean "run repeatedly within one tick" — the drain considers each
    /// task at most once per pass by construction — so clamping is the only
    /// sane reading, and it is louder than silently dropping the task.
    pub fn schedule_repeating(
        &mut self,
        delay_ticks: u32,
        period_ticks: u32,
        f: impl FnMut(&mut World) + Send + Sync + 'static,
    ) -> TaskId {
        self.push(
            delay_ticks,
            Repeat::Every(period_ticks.max(1)),
            Box::new(f),
        )
    }

    fn push(&mut self, delay_ticks: u32, repeat: Repeat, run: TaskFn) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        self.tasks.push(Task {
            id,
            // `delay_ticks` counts the next tick as 1, and `remaining` counts
            // ticks *before* the run, so they differ by one. `saturating_sub`
            // makes 0 and 1 both mean "next tick" rather than making 0 wrap.
            remaining: delay_ticks.saturating_sub(1),
            repeat,
            run,
        });
        id
    }

    /// Cancel `id`. A no-op if it already ran to completion, was already
    /// cancelled, or never existed.
    ///
    /// Safe to call from inside a task closure, including on the task's own id:
    /// a repeating task that cancels itself is not re-armed. See
    /// `tests::a_repeating_task_can_cancel_itself_from_inside_its_own_closure`.
    pub fn cancel(&mut self, id: TaskId) {
        self.tasks.retain(|t| t.id != id);
        if !self.cancelled.contains(&id.0) {
            self.cancelled.push(id.0);
        }
    }

    /// How many tasks are queued. Exposed because "the once-task was removed
    /// after it fired" is otherwise unobservable, and an accumulating task list
    /// is exactly the leak `CLAUDE.md` warns a gate should check for rather
    /// than assume.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.tasks.len()
    }

    /// Whether `id` is still queued to run again.
    #[must_use]
    pub fn is_scheduled(&self, id: TaskId) -> bool {
        self.tasks.iter().any(|t| t.id == id)
    }
}

/// [`crate::TickSet::Input`], exclusive: decrement every queued task's
/// countdown and run whatever came due, in schedule order.
///
/// Exclusive (`&mut World`) because that is the whole point — see the module
/// doc on the closure signature. The due tasks are moved out of
/// [`TaskScheduler`] before any of them runs, so the resource stays present and
/// a task closure can schedule or cancel.
pub fn run_due_tasks(world: &mut World) {
    // Phase 1: decide, under a short `&mut TaskScheduler` borrow that ends
    // before any closure runs.
    let mut due: Vec<Task> = Vec::new();
    {
        let Some(mut sched) = world.get_resource_mut::<TaskScheduler>() else {
            return;
        };
        let sched = &mut *sched;
        // Cancellations from previous ticks are already applied by `cancel`
        // itself; the list only needs to survive long enough to suppress a
        // re-arm, so clear it at the top of each pass.
        sched.cancelled.clear();

        let mut i = 0;
        while i < sched.tasks.len() {
            if sched.tasks[i].remaining == 0 {
                due.push(sched.tasks.swap_remove(i));
                // `swap_remove` moved a different task into slot `i`; do not
                // advance, or that task silently skips this tick.
            } else {
                sched.tasks[i].remaining -= 1;
                i += 1;
            }
        }
    }
    if due.is_empty() {
        return;
    }
    // `swap_remove` above scrambles order; restore schedule order so "order is
    // send order" holds for tasks as it does for `ActionQueue`.
    due.sort_by_key(|t| t.id);

    // Phase 2: run, with the resource present and mutable in the `World`.
    for mut task in due {
        (task.run)(world);
        if let Repeat::Every(period) = task.repeat {
            let Some(mut sched) = world.get_resource_mut::<TaskScheduler>() else {
                // A task closure removed the scheduler itself. Nothing to
                // re-arm into; drop the task rather than panicking.
                return;
            };
            if sched.cancelled.contains(&task.id.0) {
                continue;
            }
            task.remaining = period - 1;
            sched.tasks.push(task);
        }
    }
}

/// Installs [`TaskScheduler`] and [`run_due_tasks`]. Opt-in — see the module
/// doc on why this is not folded into [`crate::CorePlugin`].
#[derive(Debug, Default)]
pub struct SchedulerPlugin;

impl Plugin for SchedulerPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<TaskScheduler>();
        app.add_systems(GameTick, run_due_tasks.in_set(TickSet::Input));
    }
}

#[cfg(test)]
mod tests {
    //! Every gate here drives the **registry**, not the type: it builds an
    //! `App`, adds [`SchedulerPlugin`] exactly as a third-party plugin would,
    //! schedules through `ResMut<TaskScheduler>` from a real system, and then
    //! runs [`GameTick`] the way `crate::Runner::run_headless` does
    //! (`world.run_schedule(GameTick)`). None of them calls
    //! [`run_due_tasks`] directly — a test that did would pass even if
    //! [`SchedulerPlugin`] never registered the system, which is the island
    //! `CLAUDE.md` rule 1 names.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use bevy_app::App;
    use bevy_ecs::resource::Resource;
    use parking_lot::Mutex;

    use super::{SchedulerPlugin, TaskScheduler};
    use crate::schedules::GameTick;

    /// Which tick index each firing happened on. `1` is the first
    /// `run_schedule(GameTick)` after scheduling, matching the module doc's
    /// "counting the next tick as 1".
    type FiringLog = Arc<Mutex<Vec<u32>>>;

    #[derive(Resource, Default)]
    struct TickIndex(u32);

    /// Runs `GameTick` `n` times, maintaining [`TickIndex`] so a task closure
    /// can record *which* tick it fired on. Uses the same entry point the real
    /// driver uses (`crate::Runner::run_headless` → `run_schedule(GameTick)`),
    /// not a hand-rolled call to the scheduler system.
    fn run_ticks(app: &mut App, n: u32) {
        for _ in 0..n {
            app.world_mut().resource_mut::<TickIndex>().0 += 1;
            app.world_mut().run_schedule(GameTick);
        }
    }

    fn app_with_scheduler() -> App {
        let mut app = App::new();
        app.add_plugins(SchedulerPlugin);
        app.init_resource::<TickIndex>();
        app
    }

    /// The registry really installed the resource — the precondition every
    /// other gate here rests on, checked as a fact rather than assumed.
    #[test]
    fn the_scheduler_plugin_installs_the_resource() {
        let app = app_with_scheduler();
        assert!(app.world().get_resource::<TaskScheduler>().is_some());
    }

    /// **The control for the whole file.** On an `App` that never added
    /// [`SchedulerPlugin`], the resource is absent and [`run_due_tasks`] —
    /// which the registry would have installed — is not in `GameTick`, so a
    /// task scheduled by hand can never fire. Without this, every positive
    /// gate below could be passing against a `GameTick` that runs the system
    /// for some unrelated reason.
    ///
    /// Observed failing as designed: with `SchedulerPlugin` added here instead,
    /// the final assertion reports `fired = 1`, not 0.
    #[test]
    fn with_no_scheduler_plugin_a_hand_inserted_task_never_fires() {
        let mut app = App::new();
        app.add_plugins(crate::CorePlugin);
        app.init_resource::<TickIndex>();
        // Insert the resource by hand, so the *only* thing missing is the
        // registered system. This isolates "the runner drives it" from "the
        // resource exists".
        app.world_mut().insert_resource(TaskScheduler::default());
        let fired = Arc::new(AtomicU32::new(0));
        {
            let fired = Arc::clone(&fired);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(1, move |_| {
                    fired.fetch_add(1, Ordering::Relaxed);
                });
        }
        run_ticks(&mut app, 10);
        assert_eq!(
            fired.load(Ordering::Relaxed),
            0,
            "no system is registered in GameTick, so nothing can run the task"
        );
    }

    /// **The control's twin.** Byte-for-byte the scenario above with
    /// [`SchedulerPlugin`] added, so the *only* difference between the two
    /// tests is the registration. This is what makes the control above
    /// evidence rather than a claim: the detector demonstrably fires (1) when
    /// the system is registered and does not (0) when it is not, so a `0` in
    /// the control cannot be some unrelated reason the task never ran.
    /// `CLAUDE.md`'s "a control's premise can be false before the feature
    /// existed" — here the premise is checked directly instead of described.
    #[test]
    fn with_the_scheduler_plugin_the_same_hand_inserted_task_does_fire() {
        let mut app = App::new();
        app.add_plugins(SchedulerPlugin);
        app.init_resource::<TickIndex>();
        app.world_mut().insert_resource(TaskScheduler::default());
        let fired = Arc::new(AtomicU32::new(0));
        {
            let fired = Arc::clone(&fired);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(1, move |_: &mut bevy_ecs::world::World| {
                    fired.fetch_add(1, Ordering::Relaxed);
                });
        }
        run_ticks(&mut app, 10);
        assert_eq!(
            fired.load(Ordering::Relaxed),
            1,
            "the registered system must run the task exactly once"
        );
    }

    /// `schedule_once(n)` fires on tick `n` and on no other tick — the exact
    /// index, not "at least once". `n = 5` is chosen so an off-by-one in either
    /// direction (4 or 6) fails.
    #[test]
    fn a_once_task_fires_on_exactly_the_nth_tick() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(5, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(world.resource::<TickIndex>().0);
                });
        }
        run_ticks(&mut app, 12);
        assert_eq!(&*log.lock(), &[5], "schedule_once(5) must fire on tick 5 only");
    }

    /// Bukkit's `runTaskLater(plugin, task, 0)` runs next tick; so does ours,
    /// and so does `delay = 1`. Both stated in the module doc, both checked,
    /// because "0 means immediately, inside the scheduling tick" is the other
    /// plausible reading and it would be observable.
    #[test]
    fn delay_zero_and_delay_one_both_mean_the_next_tick() {
        for delay in [0u32, 1] {
            let mut app = app_with_scheduler();
            let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
            {
                let log = Arc::clone(&log);
                app.world_mut()
                    .resource_mut::<TaskScheduler>()
                    .schedule_once(delay, move |world: &mut bevy_ecs::world::World| {
                        log.lock().push(world.resource::<TickIndex>().0);
                    });
            }
            run_ticks(&mut app, 3);
            assert_eq!(&*log.lock(), &[1], "delay {delay} must fire on tick 1");
        }
    }

    /// The *magnitude* gate `CLAUDE.md` asks for: predict the value from
    /// outside constants, compute the suspected-wrong hypothesis too, and
    /// require the measurement to land on one. Asserting the exact tick
    /// indices rather than the count is what discriminates a phase error — a
    /// delay off by one produces the same *number* of firings.
    #[test]
    fn a_repeating_task_fires_on_exactly_the_predicted_ticks() {
        const DELAY: u32 = 3;
        const PERIOD: u32 = 5;
        const TICKS: u32 = 20;

        // Correct hypothesis: DELAY, DELAY+PERIOD, DELAY+2*PERIOD, …
        let expected: Vec<u32> = (0..)
            .map(|k| DELAY + k * PERIOD)
            .take_while(|t| *t <= TICKS)
            .collect();
        // The off-by-one hypothesis, computed rather than described.
        let off_by_one: Vec<u32> = expected.iter().map(|t| t + 1).collect();
        assert_ne!(
            expected, off_by_one,
            "the two hypotheses must be distinguishable for this gate to mean anything"
        );
        assert_eq!(
            expected.len(),
            1 + ((TICKS - DELAY) / PERIOD) as usize,
            "the closed form in the module doc must agree with the enumeration"
        );

        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_repeating(DELAY, PERIOD, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(world.resource::<TickIndex>().0);
                });
        }
        run_ticks(&mut app, TICKS);

        let fired = log.lock().clone();
        assert_eq!(fired, expected, "expected {expected:?}, off-by-one would be {off_by_one:?}");
    }

    /// A `period` of 0 is clamped to "every tick" rather than looping forever
    /// or vanishing. 6 ticks, 6 firings, on ticks 1..=6.
    #[test]
    fn a_zero_period_is_clamped_to_every_tick() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_repeating(1, 0, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(world.resource::<TickIndex>().0);
                });
        }
        run_ticks(&mut app, 6);
        assert_eq!(&*log.lock(), &[1, 2, 3, 4, 5, 6]);
    }

    /// The accumulation check `CLAUDE.md` asks for: does any counter outlive
    /// the gate? A once-task must be *gone* after it fires, so a plugin
    /// scheduling one task per tick for an hour does not grow the list.
    #[test]
    fn a_finished_once_task_leaves_no_entry_behind() {
        let mut app = app_with_scheduler();
        app.world_mut()
            .resource_mut::<TaskScheduler>()
            .schedule_once(2, |_| {});
        assert_eq!(app.world().resource::<TaskScheduler>().pending(), 1);
        run_ticks(&mut app, 5);
        assert_eq!(
            app.world().resource::<TaskScheduler>().pending(),
            0,
            "a once-task must not accumulate after firing"
        );
    }

    /// A repeating task, by contrast, *must* stay queued — the negative
    /// control for the assertion above, proving `pending()` is measuring
    /// something rather than always returning 0 after a drain.
    #[test]
    fn a_repeating_task_stays_queued_after_firing() {
        let mut app = app_with_scheduler();
        app.world_mut()
            .resource_mut::<TaskScheduler>()
            .schedule_repeating(1, 2, |_| {});
        run_ticks(&mut app, 5);
        assert_eq!(app.world().resource::<TaskScheduler>().pending(), 1);
    }

    /// The reentrancy case `resource_scope` would have broken: a task that
    /// schedules another task. Must not panic, and the new task must be
    /// deferred to a later tick rather than re-entered this one — otherwise a
    /// task that schedules itself with `delay 0` spins forever inside one tick.
    #[test]
    fn a_task_that_schedules_another_task_does_not_panic_and_defers_it_one_tick() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(1, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(100 + world.resource::<TickIndex>().0);
                    let log = Arc::clone(&log);
                    // Reaching the resource from inside a running task is the
                    // exact operation `resource_scope` would panic on.
                    world
                        .resource_mut::<TaskScheduler>()
                        .schedule_once(0, move |world: &mut bevy_ecs::world::World| {
                            log.lock().push(200 + world.resource::<TickIndex>().0);
                        });
                });
        }
        run_ticks(&mut app, 4);
        assert_eq!(
            &*log.lock(),
            &[101, 202],
            "the outer task fires on tick 1; the task it schedules with delay 0 \
             must fire on tick 2, not be re-entered on tick 1"
        );
    }

    /// A repeating task cancelling itself from inside its own closure stops,
    /// rather than being re-armed by the drain loop that is holding it.
    #[test]
    fn a_repeating_task_can_cancel_itself_from_inside_its_own_closure() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        let id = {
            let log = Arc::clone(&log);
            let slot: Arc<Mutex<Option<super::TaskId>>> = Arc::new(Mutex::new(None));
            let slot_inner = Arc::clone(&slot);
            let id = app
                .world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_repeating(1, 1, move |world: &mut bevy_ecs::world::World| {
                    let tick = world.resource::<TickIndex>().0;
                    log.lock().push(tick);
                    if tick == 3 {
                        let id = slot_inner.lock().expect("id is set before any tick runs");
                        world.resource_mut::<TaskScheduler>().cancel(id);
                    }
                });
            *slot.lock() = Some(id);
            id
        };
        run_ticks(&mut app, 8);
        assert_eq!(&*log.lock(), &[1, 2, 3], "self-cancel on tick 3 must stop the re-arm");
        assert!(!app.world().resource::<TaskScheduler>().is_scheduled(id));
        assert_eq!(app.world().resource::<TaskScheduler>().pending(), 0);
    }

    /// Cancelling from *outside* (another system, a later tick) also stops it.
    #[test]
    fn cancelling_a_repeating_task_from_outside_stops_it() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        let id = {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_repeating(1, 1, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(world.resource::<TickIndex>().0);
                })
        };
        run_ticks(&mut app, 3);
        app.world_mut().resource_mut::<TaskScheduler>().cancel(id);
        run_ticks(&mut app, 5);
        assert_eq!(&*log.lock(), &[1, 2, 3]);
    }

    /// Cancelling an id that already ran to completion is a defined no-op, not
    /// a panic and not a collision with a later task that happens to reuse the
    /// slot.
    #[test]
    fn cancelling_an_already_finished_task_is_a_no_op() {
        let mut app = app_with_scheduler();
        let id = app
            .world_mut()
            .resource_mut::<TaskScheduler>()
            .schedule_once(1, |_| {});
        run_ticks(&mut app, 2);
        assert!(!app.world().resource::<TaskScheduler>().is_scheduled(id));

        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        let second = {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(1, move |_| log.lock().push(1))
        };
        assert_ne!(id, second, "ids must never be reused");
        app.world_mut().resource_mut::<TaskScheduler>().cancel(id);
        run_ticks(&mut app, 2);
        assert_eq!(&*log.lock(), &[1], "cancelling a dead id must not kill a live task");
    }

    /// Several tasks due on the same tick all run, in schedule order — the
    /// `swap_remove` in [`run_due_tasks`] scrambles the list, so the re-sort is
    /// load-bearing and this is what would catch its removal.
    #[test]
    fn tasks_due_on_the_same_tick_run_in_schedule_order() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        for n in 1..=5u32 {
            let log = Arc::clone(&log);
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(2, move |_| log.lock().push(n));
        }
        run_ticks(&mut app, 3);
        assert_eq!(&*log.lock(), &[1, 2, 3, 4, 5]);
    }

    /// The `swap_remove`-skips-a-task bug specifically: interleave due and
    /// not-due tasks so that removing a due one moves a *not*-due one into the
    /// vacated slot. Every task must still fire on its own tick.
    #[test]
    fn removing_a_due_task_does_not_make_a_later_task_skip_a_tick() {
        let mut app = app_with_scheduler();
        let log: FiringLog = Arc::new(Mutex::new(Vec::new()));
        // Delays 1,2,1,2,1,2 — three due on tick 1, three on tick 2.
        for n in 0..6u32 {
            let log = Arc::clone(&log);
            let delay = if n % 2 == 0 { 1 } else { 2 };
            app.world_mut()
                .resource_mut::<TaskScheduler>()
                .schedule_once(delay, move |world: &mut bevy_ecs::world::World| {
                    log.lock().push(n * 10 + world.resource::<TickIndex>().0);
                });
        }
        run_ticks(&mut app, 4);
        assert_eq!(
            &*log.lock(),
            &[1, 21, 41, 12, 32, 52],
            "even-numbered tasks fire on tick 1, odd on tick 2, none skipped"
        );
    }

    /// A task really does get the live `World`: it writes a resource and a
    /// later assertion reads it back. `&mut World` would be worthless if the
    /// closure received a scratch world, which is exactly the *world* species
    /// of vacuous test this rules out.
    #[test]
    fn a_task_mutates_the_real_world() {
        #[derive(Resource, Default)]
        struct Marker(u32);

        let mut app = app_with_scheduler();
        app.init_resource::<Marker>();
        app.world_mut()
            .resource_mut::<TaskScheduler>()
            .schedule_once(1, |world: &mut bevy_ecs::world::World| {
                world.resource_mut::<Marker>().0 = 99;
            });
        assert_eq!(app.world().resource::<Marker>().0, 0);
        run_ticks(&mut app, 2);
        assert_eq!(app.world().resource::<Marker>().0, 99);
    }

    /// `run_due_tasks` on a `World` with no [`TaskScheduler`] returns quietly
    /// rather than panicking — so adding the system without the resource (a
    /// half-built `App`) is not a crash.
    #[test]
    fn the_drain_system_is_a_no_op_with_no_scheduler_resource() {
        let mut world = bevy_ecs::world::World::new();
        super::run_due_tasks(&mut world);
    }
}
