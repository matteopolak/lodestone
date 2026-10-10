//! Goals run on every second tick; only the goals that ask for it run on all.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lodestone_entity::ai::{Flag, FlagSet, Goal, GoalSelector, MobController, NavigatingMob};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

struct Void;

impl PathWorld for Void {
    fn min_y(&self) -> i32 {
        -8
    }
    fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
        if y <= -1 { PathType::Blocked } else { PathType::Open }
    }
    fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
        if y <= -1 { 1.0 } else { 0.0 }
    }
    fn collides(&self, aabb: Aabb) -> bool {
        aabb.min_y < 0.0
    }
    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

struct Counter {
    ticks: Arc<AtomicU32>,
    every_tick: bool,
}

impl Goal for Counter {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Look])
    }
    fn can_use(&mut self, _mob: &mut dyn MobController) -> bool {
        true
    }
    fn tick(&mut self, _mob: &mut dyn MobController) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }
    fn requires_update_every_tick(&self) -> bool {
        self.every_tick
    }
}

fn ticks_over(phase: u64, every_tick: bool, run: usize) -> u32 {
    let counted = Arc::new(AtomicU32::new(0));
    let mut mob = NavigatingMob::new(&Void, MobShape::land(0.6, 1.95), Vec3::new(0.5, 0.0, 0.5), 0.2, 64, 1);
    mob.set_ai_phase(phase);
    let mut ai = GoalSelector::new();
    ai.add(1, Box::new(Counter { ticks: counted.clone(), every_tick }));
    for _ in 0..run {
        mob.tick(&mut ai);
    }
    counted.load(Ordering::Relaxed)
}

/// Over 20 ticks an every-tick goal ticks 20 times. Any other goal ticks on
/// the first tick and then every second one: ticks 1, 2, 4, ..., 20 is 11 at
/// phase 0, and 1, 3, ..., 19 is 10 at phase 1, so neighbours alternate.
#[test]
fn goals_tick_every_second_tick_unless_they_ask_for_every_tick() {
    assert_eq!(ticks_over(0, true, 20), 20);
    assert_eq!(ticks_over(1, true, 20), 20);
    assert_eq!(ticks_over(0, false, 20), 11);
    assert_eq!(ticks_over(1, false, 20), 10);
}
