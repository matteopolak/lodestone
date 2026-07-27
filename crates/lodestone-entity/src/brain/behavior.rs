//! Behaviour lifecycle: the running units a brain schedules.
//!
//! Faithful to vanilla's `Behavior` / `BehaviorControl`. Every behaviour is a
//! small state machine with two states ([`Status`]) and a randomised timeout:
//!
//! * [`try_start`](BehaviorControl::try_start) runs only if the behaviour's
//!   entry memories are satisfied and its extra start conditions pass; on
//!   success it rolls a duration in `[min, max]` and calls `start`.
//! * [`tick_or_stop`](BehaviorControl::tick_or_stop) ticks the behaviour unless
//!   it has timed out or `can_still_use` returns `false`, in which case it stops.
//! * The default `can_still_use` is `false`, so a behaviour that overrides
//!   nothing runs for exactly one tick — the common "one-shot" case.
//!
//! Rust can't reproduce Java's `final` template methods directly, so the fixed
//! lifecycle lives in the [`Leaf`] wrapper and the customisation points live in
//! the [`Behavior`] trait it wraps.

use super::memory::{Memories, MemoryModuleType, MemoryStatus};
use super::mob::BrainMob;

/// Whether a behaviour is currently running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Not running.
    Stopped,
    /// Running.
    Running,
}

/// The default behaviour timeout in ticks (vanilla's `DEFAULT_DURATION`).
pub const DEFAULT_DURATION: i32 = 60;

/// The uniform interface the brain schedules against.
///
/// Both leaf behaviours (via [`Leaf`]) and composite gates
/// ([`GateBehavior`](super::gate::GateBehavior)) implement this.
pub trait BehaviorControl {
    /// The current run state.
    fn status(&self) -> Status;

    /// Every memory this behaviour (or its descendants) needs registered.
    fn required_memories(&self) -> Vec<MemoryModuleType>;

    /// Attempts to start. Returns whether it is now running.
    fn try_start(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) -> bool;

    /// Ticks if still usable, otherwise stops.
    fn tick_or_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64);

    /// Forces the behaviour to stop, running its `stop` hook.
    fn do_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64);

    /// A short name for debugging.
    fn name(&self) -> &'static str;
}

/// The customisation points of a leaf behaviour.
///
/// A concrete behaviour implements this; [`Leaf`] wraps it with the fixed
/// lifecycle. Every hook may mutate both the memory map and the mob.
pub trait Behavior {
    /// The `(memory, status)` requirements that must all hold to start (and, by
    /// registration, the memories this behaviour depends on).
    fn entry_condition(&self) -> &[(MemoryModuleType, MemoryStatus)];

    /// Minimum run duration in ticks.
    fn min_duration(&self) -> i32 {
        DEFAULT_DURATION
    }

    /// Maximum run duration in ticks.
    fn max_duration(&self) -> i32 {
        DEFAULT_DURATION
    }

    /// Extra gate beyond the entry memories (default: always pass). May mutate,
    /// matching vanilla behaviours that decrement cooldowns here.
    fn check_extra_start_conditions(
        &mut self,
        _mem: &mut Memories,
        _mob: &mut dyn BrainMob,
    ) -> bool {
        true
    }

    /// Called once when the behaviour starts.
    fn start(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {}

    /// Whether the behaviour may keep running (default: `false`, i.e. one tick).
    fn can_still_use(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) -> bool {
        false
    }

    /// Called each tick while running.
    fn tick(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {}

    /// Called once when the behaviour stops.
    fn stop(&mut self, _mem: &mut Memories, _mob: &mut dyn BrainMob, _time: i64) {}

    /// A short name for debugging.
    fn name(&self) -> &'static str;
}

/// Wraps a [`Behavior`] with vanilla's fixed start/tick/stop lifecycle.
#[derive(Debug)]
pub struct Leaf<B: Behavior> {
    behavior: B,
    status: Status,
    end_timestamp: i64,
}

impl<B: Behavior> Leaf<B> {
    /// Wraps `behavior`, initially stopped.
    pub fn new(behavior: B) -> Self {
        Self {
            behavior,
            status: Status::Stopped,
            end_timestamp: 0,
        }
    }

    fn timed_out(&self, time: i64) -> bool {
        time > self.end_timestamp
    }

    fn has_required_memories(&self, mem: &Memories) -> bool {
        self.behavior
            .entry_condition()
            .iter()
            .all(|&(ty, status)| mem.check(ty, status))
    }
}

impl<B: Behavior> BehaviorControl for Leaf<B> {
    fn status(&self) -> Status {
        self.status
    }

    fn required_memories(&self) -> Vec<MemoryModuleType> {
        self.behavior
            .entry_condition()
            .iter()
            .map(|&(ty, _)| ty)
            .collect()
    }

    fn try_start(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) -> bool {
        if self.has_required_memories(mem) && self.behavior.check_extra_start_conditions(mem, mob) {
            self.status = Status::Running;
            let min = self.behavior.min_duration();
            let max = self.behavior.max_duration();
            let span = max + 1 - min;
            let duration = if span > 0 {
                min + mob.next_i32(span)
            } else {
                min
            };
            self.end_timestamp = time + i64::from(duration);
            self.behavior.start(mem, mob, time);
            true
        } else {
            false
        }
    }

    fn tick_or_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) {
        if !self.timed_out(time) && self.behavior.can_still_use(mem, mob, time) {
            self.behavior.tick(mem, mob, time);
        } else {
            self.do_stop(mem, mob, time);
        }
    }

    fn do_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) {
        self.status = Status::Stopped;
        self.behavior.stop(mem, mob, time);
    }

    fn name(&self) -> &'static str {
        self.behavior.name()
    }
}
