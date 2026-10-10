//! Goal sets for the turtle, and the water goals of the drowned.
//!
//! # What it is
//!
//! The turtle's registrations plus the three drowned goals that move it between
//! water and land, steering the [`Amphibious`](crate::pathfinding::NavMode::Amphibious)
//! navigation and its per-species swim rule.
//!
//! # How it works
//!
//! A turtle on land looks for water two blocks below its feet, spiralling out
//! to 24 blocks every 200 to 399 goal ticks, and walks to the cell above it; in
//! water it travels toward a random point within 512 blocks horizontally and
//! 4 vertically, moving in legs of up to 16 blocks aimed within 18 degrees of
//! it. It wanders only on land, every 100 ticks on average.
//!
//! A drowned in daylight and out of water picks the first of ten random cells
//! within 10 blocks horizontally, from 5 below to 2 above, that holds water
//! and walks to it. At night, in water below two blocks under sea level, it
//! swims up in legs of up to 4 blocks sideways, flagging itself as heading for
//! land so its move rule climbs.
//!
//! # How to change it
//!
//! Not modelled: eggs, laying, breeding and the home block of the turtle; the
//! turtle's panic preferring water within 7 blocks; the drowned's beach goal.

use lodestone_model::Vec3;

use crate::ai::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use crate::ai::goals::{PanicGoal, RandomStrollGoal};
use crate::ai::mob::MobController;

use super::{Registration, Selector, SpeciesContext};

/// Resolves the species this module owns.
#[must_use]
pub fn lookup(species: &str) -> Option<&'static [Registration]> {
    match species {
        "turtle" => Some(TURTLE),
        _ => None,
    }
}

/// The turtle: panic, tempt, find water, travel through it, look, wander ashore.
pub static TURTLE: &[Registration] = &[
    Registration::goal(0, "Turtle.TurtlePanicGoal", panic),
    Registration::missing(Selector::Goal, 1, "Turtle.TurtleBreedGoal"),
    Registration::missing(Selector::Goal, 1, "Turtle.TurtleLayEggGoal"),
    Registration::goal(2, "TemptGoal(TURTLE_FOOD)", tempt),
    Registration::goal(3, "Turtle.TurtleGoToWaterGoal", go_to_water),
    Registration::missing(Selector::Goal, 4, "Turtle.TurtleGoHomeGoal"),
    Registration::goal(7, "Turtle.TurtleTravelGoal", travel),
    Registration::goal(8, "LookAtPlayerGoal(Player)", super::look_at_player_8),
    Registration::goal(9, "Turtle.TurtleRandomStrollGoal", land_stroll),
];

fn panic(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(PanicGoal::new(ctx.speed * 1.2))
}

fn tempt(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::goals::TemptGoal::new(ctx.speed * 1.1))
}

fn go_to_water(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MoveToWaterGoal::new(ctx.speed))
}

fn travel(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TravelGoal::new(ctx.speed))
}

fn land_stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LandStrollGoal(RandomStrollGoal::new(ctx.speed).with_interval(100)))
}

fn cell(at: Vec3) -> (i32, i32, i32) {
    (at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32)
}

fn bottom_center(x: i32, y: i32, z: i32) -> Vec3 {
    Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5)
}

/// A random point up to `h` blocks sideways and `v` blocks vertically from the
/// mob, aimed within `spread` radians of `toward`, that is not inside a solid
/// or open air above nothing. Tries ten times.
fn point_towards(mob: &mut dyn MobController, h: f64, v: i32, toward: Vec3, spread: f64) -> Option<Vec3> {
    let here = mob.position();
    let base = (toward.z - here.z).atan2(toward.x - here.x) - std::f64::consts::FRAC_PI_2;
    let (bx, by, bz) = cell(here);
    for _ in 0..10 {
        let angle = base + (2.0 * mob.next_f64() - 1.0) * spread;
        let distance = mob.next_f64().sqrt() * std::f64::consts::SQRT_2 * h;
        let dy = mob.next_i32(2 * v + 1) - v;
        let x = bx + (-distance * angle.sin()).floor() as i32;
        let z = bz + (distance * angle.cos()).floor() as i32;
        let candidate = bottom_center(x, by + dy, z);
        let open = mob.water_at(candidate) || mob.air_at(candidate);
        if open && !mob.air_at(Vec3::new(candidate.x, candidate.y - 1.0, candidate.z)) {
            return Some(candidate);
        }
    }
    None
}

/// Walks to the nearest water block found by a spiral search, then stops once
/// the mob is in it.
#[derive(Debug)]
struct MoveToWaterGoal {
    speed: f64,
    next_start: i32,
    block: (i32, i32, i32),
    try_ticks: i32,
}

impl MoveToWaterGoal {
    const RANGE: i32 = 24;
    const GIVE_UP_TICKS: i32 = 1200;

    fn new(speed: f64) -> Self {
        Self { speed, next_start: 0, block: (0, 0, 0), try_ticks: 0 }
    }

    /// The nearest water block in the one layer two below the feet, spiralling
    /// outward ring by ring.
    fn find(&self, mob: &dyn MobController) -> Option<(i32, i32, i32)> {
        let (mx, my, mz) = cell(mob.position());
        let step = |n: i32| if n > 0 { -n } else { 1 - n };
        for r in 0..Self::RANGE {
            let mut x = 0;
            while x <= r {
                let mut z = if x < r && x > -r { r } else { 0 };
                while z <= r {
                    let (px, py, pz) = (mx + x, my - 2, mz + z);
                    if mob.water_at(bottom_center(px, py, pz)) {
                        return Some((px, py, pz));
                    }
                    z = step(z);
                }
                x = step(x);
            }
        }
        None
    }

    fn destination(&self) -> Vec3 {
        bottom_center(self.block.0, self.block.1 + 1, self.block.2)
    }
}

impl Goal for MoveToWaterGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Jump])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if mob.in_water() {
            return false;
        }
        if self.next_start > 0 {
            self.next_start -= 1;
            return false;
        }
        self.next_start = reduced_tick_delay(200 + mob.next_i32(200));
        match self.find(mob) {
            Some(block) => {
                self.block = block;
                true
            }
            None => false,
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.in_water()
            && self.try_ticks <= Self::GIVE_UP_TICKS
            && mob.water_at(bottom_center(self.block.0, self.block.1, self.block.2))
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        let speed = if mob.is_baby() { 2.0 * self.speed } else { self.speed };
        mob.move_to(self.destination(), speed);
        self.try_ticks = 0;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let target = self.destination();
        let (dx, dy, dz) = (target.x - mob.position().x, target.y + 0.5 - mob.position().y, target.z - mob.position().z);
        if dx * dx + dy * dy + dz * dz >= 1.0 {
            self.try_ticks += 1;
            if self.try_ticks % 160 == 0 {
                let speed = if mob.is_baby() { 2.0 * self.speed } else { self.speed };
                mob.move_to(target, speed);
            }
        } else {
            self.try_ticks -= 1;
        }
    }
}

/// Swims in legs toward a far random point chosen when it starts.
#[derive(Debug)]
struct TravelGoal {
    speed: f64,
    destination: Option<Vec3>,
    stuck: bool,
}

impl TravelGoal {
    fn new(speed: f64) -> Self {
        Self { speed, destination: None, stuck: false }
    }
}

impl Goal for TravelGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.in_water()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.in_water() && !self.stuck
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        let here = mob.position();
        let dx = f64::from(mob.next_i32(1025) - 512);
        let mut dy = f64::from(mob.next_i32(9) - 4);
        let dz = f64::from(mob.next_i32(1025) - 512);
        if dy + here.y > f64::from(mob.sea_level() - 1) {
            dy = 0.0;
        }
        self.destination = Some(Vec3::new(here.x + dx, here.y + dy, here.z + dz));
        self.stuck = false;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(destination) = self.destination else {
            self.stuck = true;
            return;
        };
        if !mob.navigation_done() {
            return;
        }
        let next = point_towards(mob, 16.0, 3, destination, std::f64::consts::PI / 10.0)
            .or_else(|| point_towards(mob, 8.0, 7, destination, std::f64::consts::FRAC_PI_2));
        match next {
            Some(next) => {
                mob.move_to(next, self.speed);
            }
            None => self.stuck = true,
        }
    }
}

/// The stroll goal, only while out of the water.
#[derive(Debug)]
struct LandStrollGoal(RandomStrollGoal);

impl Goal for LandStrollGoal {
    fn flags(&self) -> FlagSet {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.in_water() && self.0.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.0.stop(mob);
    }
}

/// Builds the drowned's daylight walk to water at `speed`.
pub fn drowned_go_to_water(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DrownedGoToWaterGoal { speed: ctx.speed, wanted: None })
}

/// Builds the drowned's night swim toward the surface at `speed`.
pub fn drowned_swim_up(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DrownedSwimUpGoal { speed: ctx.speed, stuck: false })
}

/// In daylight and out of water, walks to the first of ten random cells that
/// holds water.
#[derive(Debug)]
struct DrownedGoToWaterGoal {
    speed: f64,
    wanted: Option<Vec3>,
}

impl Goal for DrownedGoToWaterGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !mob.bright_outside() || mob.in_water() {
            return false;
        }
        let (x, y, z) = cell(mob.position());
        for _ in 0..10 {
            let (dx, dy, dz) = (mob.next_i32(20) - 10, 2 - mob.next_i32(8), mob.next_i32(20) - 10);
            let spot = bottom_center(x + dx, y + dy, z + dz);
            if mob.water_at(spot) {
                self.wanted = Some(spot);
                return true;
            }
        }
        false
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(spot) = self.wanted {
            mob.move_to(spot, self.speed);
        }
    }
}

/// At night, in water more than two blocks under sea level, swims upward and
/// marks itself as heading for land.
#[derive(Debug)]
struct DrownedSwimUpGoal {
    speed: f64,
    stuck: bool,
}

impl DrownedSwimUpGoal {
    fn eligible(mob: &dyn MobController) -> bool {
        !mob.bright_outside() && mob.in_water() && mob.position().y < f64::from(mob.sea_level() - 2)
    }
}

impl Goal for DrownedSwimUpGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::default()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        Self::eligible(mob)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        Self::eligible(mob) && !self.stuck
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_searching_for_land(true);
        self.stuck = false;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_searching_for_land(false);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let here = mob.position();
        let sea_level = mob.sea_level();
        if here.y >= f64::from(sea_level - 1) || !mob.navigation_done() {
            return;
        }
        let surface = Vec3::new(here.x, f64::from(sea_level - 1), here.z);
        match point_towards(mob, 4.0, 8, surface, std::f64::consts::FRAC_PI_2) {
            Some(next) => {
                mob.move_to(next, self.speed);
            }
            None => self.stuck = true,
        }
    }
}
