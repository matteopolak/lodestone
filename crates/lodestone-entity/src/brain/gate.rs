//! Gate behaviours: run-one / try-all groups with ordered or shuffled children.
//!
//! Faithful to vanilla's `GateBehavior`. A gate is itself a
//! [`BehaviorControl`], so it slots into the brain like any leaf, but internally
//! it runs a group of child behaviours under a policy:
//!
//! * [`OrderPolicy`] decides whether children are tried in registration order or
//!   a weighted shuffle each time the gate starts.
//! * [`RunningPolicy`] decides whether to start **one** child (the first that
//!   accepts) or **all** eligible children.
//!
//! `RUN_ONE` + `SHUFFLED` is the workhorse: it is how a brain's `IDLE` activity
//! picks *one* of "look at a player" / "stroll randomly" / "look around" each
//! time, which is the Brain-system analogue of the goal system's flag
//! arbitration.

use super::behavior::{BehaviorControl, Status};
use super::memory::{Memories, MemoryModuleType, MemoryStatus};
use super::mob::BrainMob;

/// The order in which a gate considers its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderPolicy {
    /// Registration order, unchanged.
    Ordered,
    /// A fresh weighted shuffle each time the gate starts.
    Shuffled,
}

/// How many children a gate starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningPolicy {
    /// Start the first child that accepts, then stop trying.
    RunOne,
    /// Start every child that accepts.
    TryAll,
}

/// A composite behaviour running a group of children under a policy.
pub struct GateBehavior {
    name: &'static str,
    entry_condition: Vec<(MemoryModuleType, MemoryStatus)>,
    exit_erased_memories: Vec<MemoryModuleType>,
    order_policy: OrderPolicy,
    running_policy: RunningPolicy,
    children: Vec<Box<dyn BehaviorControl>>,
    weights: Vec<f32>,
    order: Vec<usize>,
    status: Status,
}

impl GateBehavior {
    /// Builds a gate. `children` pairs each behaviour with an integer weight
    /// (used only by [`OrderPolicy::Shuffled`]; higher weights sort earlier more
    /// often).
    #[must_use]
    pub fn new(
        name: &'static str,
        entry_condition: Vec<(MemoryModuleType, MemoryStatus)>,
        exit_erased_memories: Vec<MemoryModuleType>,
        order_policy: OrderPolicy,
        running_policy: RunningPolicy,
        children: Vec<(Box<dyn BehaviorControl>, i32)>,
    ) -> Self {
        let mut ctrls = Vec::with_capacity(children.len());
        let mut weights = Vec::with_capacity(children.len());
        for (ctrl, weight) in children {
            ctrls.push(ctrl);
            weights.push(weight.max(1) as f32);
        }
        let order = (0..ctrls.len()).collect();
        Self {
            name,
            entry_condition,
            exit_erased_memories,
            order_policy,
            running_policy,
            children: ctrls,
            weights,
            order,
            status: Status::Stopped,
        }
    }

    /// A `RUN_ONE` gate with a stable registration order and no entry/exit
    /// memory conditions — the most common `IDLE` shape.
    #[must_use]
    pub fn run_one(name: &'static str, children: Vec<Box<dyn BehaviorControl>>) -> Self {
        Self::new(
            name,
            Vec::new(),
            Vec::new(),
            OrderPolicy::Ordered,
            RunningPolicy::RunOne,
            children.into_iter().map(|c| (c, 1)).collect(),
        )
    }

    fn has_required_memories(&self, mem: &Memories) -> bool {
        self.entry_condition
            .iter()
            .all(|&(ty, status)| mem.check(ty, status))
    }

    /// Weighted shuffle matching vanilla `ShufflingList.shuffle`: sort by
    /// `-nextFloat()^(1/weight)` ascending. Higher weight biases toward the
    /// front.
    fn apply_order(&mut self, mob: &mut dyn BrainMob) {
        if self.order_policy == OrderPolicy::Shuffled {
            let mut keyed: Vec<(f32, usize)> = (0..self.children.len())
                .map(|i| {
                    let r = mob.next_f32();
                    let key = -r.powf(1.0 / self.weights[i]);
                    (key, i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            self.order = keyed.into_iter().map(|(_, i)| i).collect();
        }
    }

    fn any_running(&self) -> bool {
        self.children.iter().any(|c| c.status() == Status::Running)
    }
}

impl std::fmt::Debug for GateBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let children: Vec<&'static str> = self.children.iter().map(|c| c.name()).collect();
        f.debug_struct("GateBehavior")
            .field("name", &self.name)
            .field("order_policy", &self.order_policy)
            .field("running_policy", &self.running_policy)
            .field("children", &children)
            .field("status", &self.status)
            .finish()
    }
}

impl BehaviorControl for GateBehavior {
    fn status(&self) -> Status {
        self.status
    }

    fn required_memories(&self) -> Vec<MemoryModuleType> {
        let mut out: Vec<MemoryModuleType> =
            self.entry_condition.iter().map(|&(ty, _)| ty).collect();
        for child in &self.children {
            out.extend(child.required_memories());
        }
        out
    }

    fn try_start(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) -> bool {
        if !self.has_required_memories(mem) {
            return false;
        }
        self.status = Status::Running;
        self.apply_order(mob);
        match self.running_policy {
            RunningPolicy::RunOne => {
                for i in 0..self.order.len() {
                    let idx = self.order[i];
                    if self.children[idx].status() == Status::Stopped
                        && self.children[idx].try_start(mem, mob, time)
                    {
                        break;
                    }
                }
            }
            RunningPolicy::TryAll => {
                for i in 0..self.order.len() {
                    let idx = self.order[i];
                    if self.children[idx].status() == Status::Stopped {
                        self.children[idx].try_start(mem, mob, time);
                    }
                }
            }
        }
        true
    }

    fn tick_or_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) {
        for i in 0..self.children.len() {
            if self.children[i].status() == Status::Running {
                self.children[i].tick_or_stop(mem, mob, time);
            }
        }
        if !self.any_running() {
            self.do_stop(mem, mob, time);
        }
    }

    fn do_stop(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob, time: i64) {
        self.status = Status::Stopped;
        for i in 0..self.children.len() {
            if self.children[i].status() == Status::Running {
                self.children[i].do_stop(mem, mob, time);
            }
        }
        for &ty in &self.exit_erased_memories {
            mem.erase(ty);
        }
    }

    fn name(&self) -> &'static str {
        self.name
    }
}
