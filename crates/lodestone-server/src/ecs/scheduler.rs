//! Tick-owned native plugin callbacks; no world lock or worker thread is exposed.

use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

/// Opaque cancellation handle, unique for the lifetime of one scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerTaskId(u64);

type Callback = Box<dyn FnMut(&mut World, ServerTaskId) + Send + Sync>;

struct Task {
    due: u64,
    period: Option<u64>,
    callback: Option<Callback>,
}

/// Delayed and repeating work on the primary server world's `GameTick`.
///
/// Delays count scheduler passes, excluding `ServerBoot`. Zero delay and zero
/// period normalize to one tick. Callbacks may schedule or cancel other work,
/// including their own repeat, using this resource through their world borrow.
/// Tasks are transient and disappear when the owning world shuts down.
#[derive(Resource, Default)]
pub struct ServerTaskScheduler {
    tick: u64,
    dispatching: bool,
    next_id: u64,
    tasks: BTreeMap<ServerTaskId, Task>,
    deadlines: BTreeSet<(u64, ServerTaskId)>,
}

impl std::fmt::Debug for ServerTaskScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerTaskScheduler")
            .field("tick", &self.tick)
            .field("pending", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl ServerTaskScheduler {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::{GameTick, ServerApp};

    #[derive(Resource, Default)]
    struct Calls(Vec<u64>);

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
}
