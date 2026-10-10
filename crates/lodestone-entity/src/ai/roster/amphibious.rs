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
//! A turtle remembers the beach it was born on and, once in about 700 goal
//! ticks while 64 or more blocks away, walks back in legs, doing nothing else
//! until within 7 blocks of it. Its panic flees to the nearest water within 7
//! blocks when there is any.
//!
//! Breeding makes the breeder carry an egg; `LayEggGoal` digs near the nest and
//! requests an egg block through the block-edit seam. The drowned's beach goal
//! is not modelled.

use lodestone_model::Vec3;

use crate::ai::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use crate::ai::block_seek::BlockSeek;
use crate::ai::goals::{MateGoal, FleeInPanicGoal, WanderGoal};
use crate::ai::turtle_egg;
use crate::ai::mob::MobController;

use super::{Registration, SpeciesContext};

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
    Registration::goal(0, "turtle.panic", panic),
    Registration::goal(1, "turtle.mate", breed),
    Registration::goal(1, "turtle.lay_egg", lay_egg),
    Registration::goal(2, "lure(turtle_food)", tempt),
    Registration::goal(3, "turtle.go_to_water", go_to_water),
    Registration::goal(4, "turtle.go_home", go_home),
    Registration::goal(7, "turtle.travel", travel),
    Registration::goal(8, "watch_player(player)", super::look_at_player_8),
    Registration::goal(9, "turtle.land_stroll", land_stroll),
];

fn panic(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(FleeInPanicGoal::new(ctx.speed * 1.2).seeking_water(7))
}

fn breed(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MateGoal::new(ctx.speed).unless_carrying_egg())
}

fn lay_egg(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LayEggGoal::new(ctx.speed))
}

fn tempt(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(crate::ai::goals::LureGoal::new(ctx.speed * 1.1))
}

fn go_to_water(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(MoveToWaterGoal::new(ctx.speed))
}

fn go_home(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(GoHomeGoal { speed: ctx.speed, stuck: false, close_ticks: 0 })
}

fn travel(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(TravelGoal::new(ctx.speed))
}

fn land_stroll(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(LandStrollGoal(WanderGoal::new(ctx.speed).with_interval(100)))
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
    point_towards_with(mob, h, v, toward, spread, false)
}

/// [`point_towards`], optionally settling each sample onto the highest
/// standing cell within `v` blocks of the mob's level instead of a random
/// depth, so a walker finds the ground on uneven terrain.
fn point_towards_with(mob: &mut dyn MobController, h: f64, v: i32, toward: Vec3, spread: f64, settle: bool) -> Option<Vec3> {
    let here = mob.position();
    let base = (toward.z - here.z).atan2(toward.x - here.x) - std::f64::consts::FRAC_PI_2;
    let (bx, by, bz) = cell(here);
    for _ in 0..10 {
        let angle = base + (2.0 * mob.next_f64() - 1.0) * spread;
        let distance = mob.next_f64().sqrt() * std::f64::consts::SQRT_2 * h;
        let dy = mob.next_i32(2 * v + 1) - v;
        let x = bx + (-distance * angle.sin()).floor() as i32;
        let z = bz + (distance * angle.cos()).floor() as i32;
        let mut candidate = bottom_center(x, by + dy, z);
        if settle {
            let ground = (by - v..=by + v).rev().map(|y| bottom_center(x, y, z)).find(|&at| {
                mob.air_at(at) && !mob.air_at(Vec3::new(at.x, at.y - 1.0, at.z)) && !mob.water_at(Vec3::new(at.x, at.y - 1.0, at.z))
            });
            let Some(ground) = ground else { continue };
            candidate = ground;
        }
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
        let baby_ashore = mob.is_baby() && !mob.in_water();
        if !baby_ashore && (mob.in_water() || mob.going_home() || mob.has_egg()) {
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

/// Walks back to the nesting beach, once in about 700 goal ticks, when 64 or
/// more blocks from it; it gives up within 7 blocks of it, after 600 ticks
/// within 16, or when no leg toward it can be found. Water legs are preferred
/// until it is within 16 blocks.
#[derive(Debug)]
struct GoHomeGoal {
    speed: f64,
    stuck: bool,
    close_ticks: i32,
}

fn within(mob: &dyn MobController, home: Vec3, distance: f64) -> bool {
    let centre = Vec3::new(home.x, home.y + 0.5, home.z);
    let here = mob.position();
    (centre.x - here.x).powi(2) + (centre.y - here.y).powi(2) + (centre.z - here.z).powi(2) < distance * distance
}

impl Goal for GoHomeGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(home) = mob.nest_position() else { return false };
        if mob.is_baby() {
            return false;
        }
        if mob.has_egg() {
            return true;
        }
        mob.next_i32(reduced_tick_delay(700)) == 0 && !within(mob, home, 64.0)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        let Some(home) = mob.nest_position() else { return false };
        !within(mob, home, 7.0) && !self.stuck && self.close_ticks <= reduced_tick_delay(600)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_going_home(true);
        self.stuck = false;
        self.close_ticks = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        mob.set_going_home(false);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(home) = mob.nest_position() else { return };
        let close = within(mob, home, 16.0);
        if close {
            self.close_ticks += 1;
        }
        if !mob.navigation_done() {
            return;
        }
        let towards = Vec3::new(home.x, home.y, home.z);
        let mut next = point_towards_with(mob, 16.0, 3, towards, std::f64::consts::PI / 10.0, true)
            .or_else(|| point_towards_with(mob, 8.0, 7, towards, std::f64::consts::FRAC_PI_2, true));
        if next.is_some_and(|n| !close && !mob.water_at(n)) {
            next = point_towards_with(mob, 16.0, 5, towards, std::f64::consts::FRAC_PI_2, true);
        }
        match next {
            Some(next) => {
                mob.move_to(next, self.speed);
            }
            None => self.stuck = true,
        }
    }
}

/// A turtle carrying an egg walks to sand within 16 blocks of its nest, digs
/// there for 200 ticks and leaves one to four eggs on the sand.
#[derive(Debug)]
struct LayEggGoal {
    seek: BlockSeek,
}

impl LayEggGoal {
    fn new(speed: f64) -> Self {
        Self { seek: BlockSeek::new(speed, 16, 1) }
    }

    fn near_nest(mob: &dyn MobController) -> bool {
        mob.nest_position().is_some_and(|home| within(mob, home, 9.0))
    }

    fn valid(mob: &dyn MobController, (x, y, z): (i32, i32, i32)) -> bool {
        mob.block_state_at((x, y + 1, z)).is_some_and(turtle_egg::is_air)
            && mob.block_state_at((x, y, z)).is_some_and(turtle_egg::is_sand)
    }
}

impl Goal for LayEggGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Jump])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !(mob.has_egg() && Self::near_nest(mob)) {
            return false;
        }
        let delay = BlockSeek::default_delay(mob);
        self.seek.can_use(mob, &Self::valid, delay)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.seek.can_continue(mob, &Self::valid) && mob.has_egg() && Self::near_nest(mob)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.seek.start(mob);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        self.seek.tick(mob, 1.0);
        if mob.in_water() || !self.seek.reached() {
            return;
        }
        let laying = mob.laying_egg_ticks();
        if laying < 1 {
            mob.set_laying_egg_ticks(1);
        } else if laying > reduced_tick_delay(200) {
            let (x, y, z) = self.seek.block();
            let eggs = 1 + mob.next_i32(4) as u32;
            if let Some(egg) = turtle_egg::state(eggs, 0) {
                mob.request_block_edit(crate::ai::BlockEdit {
                    cell: (x, y + 1, z),
                    expect: crate::ai::BlockExpect::Air,
                    set: Some(egg),
                });
            }
            mob.set_has_egg(false);
            mob.set_laying_egg_ticks(0);
            mob.fall_in_love();
        }
        if mob.laying_egg_ticks() >= 1 {
            mob.set_laying_egg_ticks(mob.laying_egg_ticks() + 1);
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
        mob.in_water() && !mob.going_home() && !mob.has_egg()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.in_water() && !self.stuck && !mob.going_home() && !mob.is_in_love() && !mob.has_egg()
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
struct LandStrollGoal(WanderGoal);

impl Goal for LandStrollGoal {
    fn flags(&self) -> FlagSet {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.in_water() && !mob.going_home() && !mob.has_egg() && self.0.can_use(mob)
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
    Box::new(DrownedSeekWaterGoal { speed: ctx.speed, wanted: None })
}

/// Builds the drowned's night walk from the water to a nearby beach at `speed`.
pub fn drowned_go_to_beach(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DrownedSeekShoreGoal { speed: ctx.speed, next_start: 0, block: (0, 0, 0), try_ticks: 0, stay_ticks: 0 })
}

/// At night, in water near the surface, walks to the nearest standable block
/// with two empty cells above it, found by the same ring search as the
/// turtle's, 8 blocks out and from one below to two above the feet.
#[derive(Debug)]
struct DrownedSeekShoreGoal {
    speed: f64,
    next_start: i32,
    block: (i32, i32, i32),
    try_ticks: i32,
    stay_ticks: i32,
}

impl DrownedSeekShoreGoal {
    fn valid(mob: &dyn MobController, (x, y, z): (i32, i32, i32)) -> bool {
        let at = |dy: i32| bottom_center(x, y + dy, z);
        let standable = !mob.air_at(at(0)) && !mob.water_at(at(0));
        standable && mob.air_at(at(1)) && mob.air_at(at(2))
    }

    fn find(mob: &dyn MobController) -> Option<(i32, i32, i32)> {
        let (mx, my, mz) = cell(mob.position());
        let step = |n: i32| if n > 0 { -n } else { 1 - n };
        let mut y = 0;
        while y <= 2 {
            for r in 0..8 {
                let mut x = 0;
                while x <= r {
                    let mut z = if x < r && x > -r { r } else { 0 };
                    while z <= r {
                        let pos = (mx + x, my + y - 1, mz + z);
                        if Self::valid(mob, pos) {
                            return Some(pos);
                        }
                        z = step(z);
                    }
                    x = step(x);
                }
            }
            y = step(y);
        }
        None
    }

    fn destination(&self) -> Vec3 {
        bottom_center(self.block.0, self.block.1 + 1, self.block.2)
    }
}

impl Goal for DrownedSeekShoreGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move, Flag::Jump])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if self.next_start > 0 {
            self.next_start -= 1;
            return false;
        }
        self.next_start = reduced_tick_delay(200 + mob.next_i32(200));
        if mob.bright_outside() || !mob.in_water() || mob.position().y < f64::from(mob.sea_level() - 3) {
            return false;
        }
        match Self::find(mob) {
            Some(block) => {
                self.block = block;
                true
            }
            None => false,
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.try_ticks >= -self.stay_ticks && self.try_ticks <= 1200 && Self::valid(mob, self.block)
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        mob.set_searching_for_land(false);
        mob.move_to(self.destination(), self.speed);
        self.try_ticks = 0;
        let inner = mob.next_i32(1200) + 1200;
        self.stay_ticks = mob.next_i32(inner) + 1200;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let target = self.destination();
        let here = mob.position();
        let d = (target.x - here.x).powi(2) + (target.y + 0.5 - here.y).powi(2) + (target.z - here.z).powi(2);
        if d >= 1.0 {
            self.try_ticks += 1;
            if self.try_ticks % 40 == 0 {
                mob.move_to(target, self.speed);
            }
        } else {
            self.try_ticks -= 1;
        }
    }
}

/// Builds the drowned's night swim toward the surface at `speed`.
pub fn drowned_swim_up(ctx: &SpeciesContext) -> Box<dyn Goal> {
    Box::new(DrownedSurfaceGoal { speed: ctx.speed, stuck: false })
}

/// In daylight and out of water, walks to the first of ten random cells that
/// holds water.
#[derive(Debug)]
struct DrownedSeekWaterGoal {
    speed: f64,
    wanted: Option<Vec3>,
}

impl Goal for DrownedSeekWaterGoal {
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
struct DrownedSurfaceGoal {
    speed: f64,
    stuck: bool,
}

impl DrownedSurfaceGoal {
    fn eligible(mob: &dyn MobController) -> bool {
        !mob.bright_outside() && mob.in_water() && mob.position().y < f64::from(mob.sea_level() - 2)
    }
}

impl Goal for DrownedSurfaceGoal {
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
