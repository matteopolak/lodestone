//! Doors on a mob's path: opening them as the mob walks through, or breaking
//! them down.

use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
use lodestone_data::block_states::StateId;

use super::block_edit::{BlockEdit, BlockExpect};
use super::goal::{FlagSet, Goal};
use super::mob::MobController;

/// Ticks of attack on a closed door before it breaks.
const BREAK_TICKS: i32 = 240;
/// Ticks an opened door is held before it closes behind the mob.
const HOLD_OPEN_TICKS: i32 = 20;
/// Squared distance within which a door on the path is the mob's to deal with.
const REACH_SQR: f64 = 2.25;

/// Whether `state` is a wooden door.
#[must_use]
pub fn is_wooden_door(state: StateId) -> bool {
    lodestone_data::tool::block_tag_contains("minecraft:wooden_doors", state.block())
}

/// A door's `open` property, `None` for a state without one.
#[must_use]
pub fn is_open(state: StateId) -> Option<bool> {
    match Properties::from_state_id(state).get(PropertyKey::Open)?.builtin_value()? {
        BuiltinPropertyValue::True => Some(true),
        BuiltinPropertyValue::False => Some(false),
        _ => None,
    }
}

/// `state` with its `open` property set, every other property kept.
#[must_use]
pub fn with_open(state: StateId, open: bool) -> Option<StateId> {
    let value = if open { BuiltinPropertyValue::True } else { BuiltinPropertyValue::False };
    let properties = Properties::from_state_id(state).with_builtin(PropertyKey::Open, value).ok()?;
    Properties::state_for_block(state.block(), &properties)
}

/// The other half of the door at `cell`: above a lower half, below an upper one.
#[must_use]
pub fn other_half(cell: (i32, i32, i32), state: StateId) -> (i32, i32, i32) {
    let upper = matches!(
        Properties::from_state_id(state).get(PropertyKey::Half).and_then(|v| v.builtin_value()),
        Some(BuiltinPropertyValue::Upper)
    );
    if upper { (cell.0, cell.1 - 1, cell.2) } else { (cell.0, cell.1 + 1, cell.2) }
}

/// Finds the door a mob is about to walk through and tracks whether it has
/// gone past it.
#[derive(Debug, Default)]
struct DoorTracker {
    cell: Option<(i32, i32, i32)>,
    dir_x: f64,
    dir_z: f64,
    passed: bool,
}

impl DoorTracker {
    /// Looks along the path walked so far and the next two waypoints for a
    /// wooden door within reach, then at the mob's own cell.
    fn find(&mut self, mob: &mut dyn MobController) -> bool {
        self.cell = None;
        if !mob.can_open_doors() {
            return false;
        }
        let Some(cells) = mob.path_cells_near() else { return false };
        let here = mob.position();
        let is_door = |mob: &dyn MobController, cell| mob.block_state_at(cell).is_some_and(is_wooden_door);
        for (x, y, z) in cells {
            let door = (x, y + 1, z);
            let (dx, dz) = (f64::from(door.0) - here.x, f64::from(door.2) - here.z);
            if dx * dx + dz * dz <= REACH_SQR && is_door(mob, door) {
                self.cell = Some(door);
                return true;
            }
        }
        let own = (here.x.floor() as i32, here.y.floor() as i32 + 1, here.z.floor() as i32);
        if is_door(mob, own) {
            self.cell = Some(own);
            return true;
        }
        false
    }

    fn begin(&mut self, mob: &dyn MobController) {
        self.passed = false;
        if let Some((x, _, z)) = self.cell {
            let here = mob.position();
            self.dir_x = f64::from(x) + 0.5 - here.x;
            self.dir_z = f64::from(z) + 0.5 - here.z;
        }
    }

    /// The mob has passed the door once it is on the far side of the line it
    /// started on.
    fn update(&mut self, mob: &dyn MobController) {
        if let Some((x, _, z)) = self.cell {
            let here = mob.position();
            let dot = self.dir_x * (f64::from(x) + 0.5 - here.x) + self.dir_z * (f64::from(z) + 0.5 - here.z);
            if dot < 0.0 {
                self.passed = true;
            }
        }
    }

    fn open_now(&self, mob: &dyn MobController) -> bool {
        self.cell.and_then(|c| mob.block_state_at(c)).and_then(is_open).unwrap_or(false)
    }

    /// Swings both halves of the door open or shut.
    fn set_open(&self, mob: &mut dyn MobController, open: bool) {
        let Some(cell) = self.cell else { return };
        let Some(state) = mob.block_state_at(cell).filter(|s| is_wooden_door(*s)) else { return };
        for half in [cell, other_half(cell, state)] {
            let Some(current) = mob.block_state_at(half).filter(|s| is_wooden_door(*s)) else { continue };
            if is_open(current) == Some(open) {
                continue;
            }
            if let Some(next) = with_open(current, open) {
                mob.request_block_edit(BlockEdit::new(half, BlockExpect::State(current), Some(next)).quietly());
            }
        }
    }

    /// Removes both halves of the door.
    fn remove(&self, mob: &mut dyn MobController) {
        let Some(cell) = self.cell else { return };
        let Some(state) = mob.block_state_at(cell).filter(|s| is_wooden_door(*s)) else { return };
        for half in [cell, other_half(cell, state)] {
            if let Some(current) = mob.block_state_at(half).filter(|s| is_wooden_door(*s)) {
                mob.request_block_edit(BlockEdit::new(half, BlockExpect::State(current), None));
            }
        }
    }
}

/// Opens the door on the path as the mob reaches it and, when `close_after`,
/// closes it again after the mob has passed or 20 ticks have gone by.
#[derive(Debug)]
pub struct OpenDoorGoal {
    tracker: DoorTracker,
    close_after: bool,
    hold_ticks: i32,
    allowed: Option<fn(&dyn MobController) -> bool>,
}

impl OpenDoorGoal {
    /// A door-opening goal.
    #[must_use]
    pub fn new(close_after: bool) -> Self {
        Self { tracker: DoorTracker::default(), close_after, hold_ticks: 0, allowed: None }
    }

    /// Opens only while `allowed` holds for the mob.
    #[must_use]
    pub fn only_when(mut self, allowed: fn(&dyn MobController) -> bool) -> Self {
        self.allowed = Some(allowed);
        self
    }
}

impl Goal for OpenDoorGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.allowed.is_none_or(|allowed| allowed(mob)) && self.tracker.find(mob)
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        self.close_after && self.hold_ticks > 0 && !self.tracker.passed
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.tracker.begin(mob);
        self.hold_ticks = HOLD_OPEN_TICKS;
        self.tracker.set_open(mob, true);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        if self.close_after {
            self.tracker.set_open(mob, false);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.hold_ticks -= 1;
        self.tracker.update(mob);
    }
}

/// Batters a closed door on the path for 240 ticks and then removes it, when
/// `allowed` says the mob may.
#[derive(Debug)]
pub struct BreakDoorGoal {
    tracker: DoorTracker,
    allowed: fn(&dyn MobController) -> bool,
    broken_for: i32,
}

impl BreakDoorGoal {
    /// A door-breaking goal gated by `allowed`.
    #[must_use]
    pub fn new(allowed: fn(&dyn MobController) -> bool) -> Self {
        Self { tracker: DoorTracker::default(), allowed, broken_for: 0 }
    }
}

impl Goal for BreakDoorGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.mob_griefing() && (self.allowed)(mob) && self.tracker.find(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some((x, y, z)) = self.tracker.cell else { return false };
        let here = mob.position();
        let (dx, dy, dz) = (f64::from(x) + 0.5 - here.x, f64::from(y) + 0.5 - here.y, f64::from(z) + 0.5 - here.z);
        self.broken_for <= BREAK_TICKS
            && !self.tracker.passed
            && !self.tracker.open_now(mob)
            && dx * dx + dy * dy + dz * dz < 4.0
            && (self.allowed)(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.tracker.begin(mob);
        self.broken_for = 0;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.tracker.update(mob);
        self.broken_for += 1;
        if self.broken_for == BREAK_TICKS && (self.allowed)(mob) {
            self.tracker.remove(mob);
        }
    }
}
