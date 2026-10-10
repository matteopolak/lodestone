//! Goal sets for the bat and the phantom.
//!
//! # What it is
//!
//! The bat registers no goals: its flight, roosting and wake rules are the
//! [`Flutter`](crate::pathfinding::NavMode::Flutter) locomotion of
//! `NavigatingMob`, so its table is empty and exists to stop the bat falling
//! through to the ground-stroll fallback. The phantom registers a target scan,
//! an attack-phase timer, a circling goal and a swooping goal, which steer the
//! [`Swoop`](crate::pathfinding::NavMode::Swoop) move control by writing its
//! [`SwoopState`](crate::ai::SwoopState).
//!
//! # How it works
//!
//! Circling sets a move target on a ring around an anchor block, 5 to 15 blocks
//! out and up to four below to five above it. A target makes the strategy goal
//! lift the anchor 20 to 39 blocks above the target (never under sea level);
//! after 10 ticks of circling it starts a swoop for 8 to 11 seconds, during
//! which the move target is the target's body. A hit, a wall or a hurt ends the
//! swoop.
//!
//! # How to change it
//!
//! The target scan takes the highest player inside a box of 16 blocks
//! horizontally and 64 vertically that the phantom can see (the host feeds
//! [`players_by_height`](MobController::players_by_height)). Every 20 ticks a
//! cat within 16 blocks ends a swoop. The cat's hiss is not modelled.

use lodestone_model::Vec3;

use crate::ai::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use crate::ai::mob::MobController;

use super::{Registration, SpeciesContext};

/// Resolves the species this module owns.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "bat" => Some(BAT),
        "phantom" => Some(PHANTOM),
        _ => None,
    }
}

/// The bat: no goals.
pub static BAT: &[Registration] = &[];

/// The phantom: strategy, sweep and circle goals, and the player target scan.
pub static PHANTOM: &[Registration] = &[
    Registration::goal(1, "Phantom.PhantomAttackStrategyGoal", attack_strategy),
    Registration::goal(2, "Phantom.PhantomSweepAttackGoal", sweep_attack),
    Registration::goal(3, "Phantom.PhantomCircleAroundAnchorGoal", circle_around_anchor),
    Registration::target(1, "Phantom.PhantomAttackPlayerTargetGoal", attack_player_target),
];

fn attack_strategy(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AttackStrategyGoal::default())
}

fn sweep_attack(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(SweepAttackGoal)
}

fn circle_around_anchor(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(CircleAroundAnchorGoal::default())
}

fn attack_player_target(_ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(AttackPlayerTargetGoal::new())
}

/// How far away the target scan sees a player.
const SCAN_RANGE: f64 = 64.0;

/// Scans for a player on a timer: after 20 ticks, then every 60.
#[derive(Debug)]
struct AttackPlayerTargetGoal {
    next_scan: i32,
    found: Option<Vec3>,
}

impl AttackPlayerTargetGoal {
    fn new() -> Self {
        Self { next_scan: reduced_tick_delay(20), found: None }
    }
}

impl Goal for AttackPlayerTargetGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Target])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.next_scan > 0 {
            self.next_scan -= 1;
            return false;
        }
        self.next_scan = reduced_tick_delay(60);
        let here = mob.position();
        self.found = mob.players_by_height().iter().copied().find(|&p| {
            (p.x - here.x).powi(2) + (p.y - here.y).powi(2) + (p.z - here.z).powi(2) <= SCAN_RANGE * SCAN_RANGE
                && mob.has_line_of_sight(p)
        });
        self.found.is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(self.found);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(None);
    }
}

/// Lifts the anchor 20 to 39 blocks above the target, but never under sea level.
fn anchor_above_target(mob: &mut dyn MobController) {
    let Some(target) = mob.attack_target() else { return };
    let lift = mob.next_i32(20);
    let (x, z) = (target.x.floor() as i32, target.z.floor() as i32);
    let y = (target.y.floor() as i32 + 20 + lift).max(mob.sea_level() + 1);
    if let Some(swoop) = mob.swoop() {
        swoop.anchor = (x, y, z);
    }
}

/// While there is a target, times the switch from circling to swooping.
#[derive(Debug, Default)]
struct AttackStrategyGoal {
    next_sweep: i32,
}

impl Goal for AttackStrategyGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.next_sweep = reduced_tick_delay(10);
        if let Some(swoop) = mob.swoop() {
            swoop.swooping = false;
        }
        anchor_above_target(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        let Some((x, y, z)) = mob.swoop().map(|s| s.anchor) else { return };
        let ground = mob.motion_blocking_height(x, y, z);
        let lift = 10 + mob.next_i32(20);
        if let Some(swoop) = mob.swoop() {
            swoop.anchor = (x, ground + lift, z);
        }
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if mob.swoop().is_none_or(|s| s.swooping) {
            return;
        }
        self.next_sweep -= 1;
        if self.next_sweep <= 0 {
            if let Some(swoop) = mob.swoop() {
                swoop.swooping = true;
            }
            anchor_above_target(mob);
            self.next_sweep = reduced_tick_delay((8 + mob.next_i32(4)) * 20);
        }
    }
}

/// Dives at the target's body until it connects, hits a wall or is hurt.
#[derive(Debug)]
struct SweepAttackGoal;

impl Goal for SweepAttackGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_some() && mob.swoop().is_some_and(|s| s.swooping)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob) && !(mob.tick_count().is_multiple_of(20) && mob.cat_near())
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_attack_target(None);
        if let Some(swoop) = mob.swoop() {
            swoop.swooping = false;
        }
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(target) = mob.attack_target() else { return };
        let here = mob.position();
        let hurt = mob.last_hurt_by().is_some();
        let Some(swoop) = mob.swoop() else { return };
        swoop.move_target = Vec3::new(target.x, target.y + 0.9, target.z);
        let touched = swoop.touches(here, target);
        if touched || swoop.blocked || hurt {
            swoop.swooping = false;
        }
        if touched {
            mob.attack(target);
        }
    }
}

/// Flies a ring around the anchor, changing radius, height and direction now
/// and then.
#[derive(Debug, Default)]
struct CircleAroundAnchorGoal {
    angle: f32,
    distance: f32,
    height: f32,
    clockwise: f32,
}

impl CircleAroundAnchorGoal {
    fn select_next(&mut self, mob: &mut dyn MobController) {
        self.angle += self.clockwise * 15.0_f32.to_radians();
        let Some(swoop) = mob.swoop() else { return };
        let (x, y, z) = swoop.anchor;
        swoop.move_target = Vec3::new(
            f64::from(x) + f64::from(self.distance * self.angle.cos()),
            f64::from(y) + f64::from(-4.0 + self.height),
            f64::from(z) + f64::from(self.distance * self.angle.sin()),
        );
    }
}

impl Goal for CircleAroundAnchorGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.attack_target().is_none() || mob.swoop().is_some_and(|s| !s.swooping)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.distance = 5.0 + mob.next_f32() * 10.0;
        self.height = -4.0 + mob.next_f32() * 9.0;
        self.clockwise = if mob.next_i32(2) == 0 { 1.0 } else { -1.0 };
        self.select_next(mob);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if mob.next_i32(reduced_tick_delay(350)) == 0 {
            self.height = -4.0 + mob.next_f32() * 9.0;
        }
        if mob.next_i32(reduced_tick_delay(250)) == 0 {
            self.distance += 1.0;
            if self.distance > 15.0 {
                self.distance = 5.0;
                self.clockwise = -self.clockwise;
            }
        }
        if mob.next_i32(reduced_tick_delay(450)) == 0 {
            self.angle = mob.next_f32() * 2.0 * std::f32::consts::PI;
            self.select_next(mob);
        }
        let here = mob.position();
        let Some(target) = mob.swoop().map(|s| s.move_target) else { return };
        if (target.x - here.x).powi(2) + (target.y - here.y).powi(2) + (target.z - here.z).powi(2) < 4.0 {
            self.select_next(mob);
        }
        let below = Vec3::new(here.x, here.y - 1.0, here.z);
        let above = Vec3::new(here.x, here.y + 1.0, here.z);
        let target = mob.swoop().map_or(target, |s| s.move_target);
        if target.y < here.y && !mob.air_at(below) {
            self.height = self.height.max(1.0);
            self.select_next(mob);
        }
        let target = mob.swoop().map_or(target, |s| s.move_target);
        if target.y > here.y && !mob.air_at(above) {
            self.height = self.height.min(-1.0);
            self.select_next(mob);
        }
    }
}
