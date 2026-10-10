//! Bee behaviour: hive, flower and pollination goals, and the state they share.
//!
//! # What it is
//!
//! A bee leaves its hive to find a bloom, hovers over it until it carries
//! nectar, then flies home and enters the hive; a nectar-carrying bee also tends
//! crops it passes over. [`BeeState`] holds what one bee remembers, the goals in
//! this module read and change it, and the host drains what leaves the entity:
//! [`BeeState::entering`] (the bee went into a hive) and [`BeeState::grown`]
//! (crop states to write).
//!
//! # How it works
//!
//! * Every tick [`BeeState::tick`] runs the cooldowns and the nectar clock.
//!   Every 20th tick a hive that no longer exists is forgotten.
//! * A bee wants into its hive when it carries nectar, has looked for nectar for
//!   more than 3600 ticks, or the host says bees stay in (night, rain), unless it
//!   is stung, hunting, mid-pollination, in its enter cooldown, or the hive is
//!   burning.
//! * Without a bloom of its own, a bee scans the cells within Manhattan distance
//!   5, nearest first, for one it can path to. Cells it cannot reach are
//!   remembered for 600 ticks.
//! * While pollinating, the bee steers straight at a point 0.6 above the bloom
//!   (not along a path) and hops between nearby hover points; after 400
//!   successful ticks each tick has a 1 in 5 chance of ending, and ending then
//!   sets nectar.
//! * Locating a hive takes the nearest hive within 20 blocks that has room and is
//!   not blacklisted; a bee that gives up on a hive blacklists it, remembering
//!   the last three.
//!
//! # How to change it
//!
//! The pure block rules ([`attracts_bees`], [`grown_state`]) are the only block
//! knowledge here; the world answers hive questions through
//! [`PathWorld`](crate::pathfinding::PathWorld). The host feeds
//! [`BeeState::stays_in_hive`], [`BeeState::raining`] and [`BeeState::has_stung`].
//!
//! Not modelled: the hive's smoke sedation, and the roll animation.

use std::collections::BTreeMap;

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_model::Vec3;

use super::goal::{Flag, FlagSet, Goal, reduced_tick_delay};
use super::mob::MobController;

/// A block cell as (x, y, z).
pub type Cell = (i32, i32, i32);

/// Ticks a bee must search without nectar before it heads home regardless.
const TIRED_AFTER: i32 = 3600;
/// A hive or bloom farther than this many blocks is forgotten.
const TOO_FAR: f64 = 48.0;
/// Ticks between hive searches after a failed or abandoned one.
const HIVE_SEARCH_COOLDOWN: i32 = 200;
/// Ticks between bloom searches after pollinating or giving up on a bloom.
const FLOWER_RETRY_COOLDOWN: i32 = 200;

/// What one bee remembers.
#[derive(Debug, Clone, Default)]
pub struct BeeState {
    /// Whether this mob is a bee at all.
    pub active: bool,
    /// The hive it calls home.
    pub hive: Option<Cell>,
    /// The bloom it last found or was told about.
    pub flower: Option<Cell>,
    /// Whether it carries nectar.
    pub has_nectar: bool,
    /// Host-fed: it has stung and is dying.
    pub has_stung: bool,
    /// Ticks spent without nectar since it last left a hive.
    pub ticks_without_nectar: i32,
    /// Ticks it must stay out of hives.
    pub stay_out_ticks: i32,
    /// Crops grown since it last collected nectar.
    pub crops_grown: i32,
    /// Ticks until it may look for a new hive.
    pub hive_cooldown: i32,
    /// Ticks until it may look for a new bloom.
    pub flower_cooldown: i32,
    /// Whether it is hovering over a bloom.
    pub pollinating: bool,
    /// Hives it gave up on, most recent last (at most three).
    pub blacklist: Vec<Cell>,
    /// Set when it enters a hive; the host drains it and removes the bee.
    pub entering: Option<Cell>,
    /// Block writes for crops it tended; the host drains them.
    pub grown: Vec<(Cell, StateId)>,
    /// Host-fed: night or rain keeps bees in their hives.
    pub stays_in_hive: bool,
    /// Host-fed: rain falls hard enough to stop pollination.
    pub raining: bool,
    /// Blooms it could not path to, with the game tick they are forgotten at.
    unreachable: Vec<(Cell, u64)>,
}

impl BeeState {
    /// A fresh bee: it waits 20 to 60 ticks before its first bloom search.
    #[must_use]
    pub fn new(first_flower_cooldown: i32) -> Self {
        Self { active: true, flower_cooldown: first_flower_cooldown, ..Self::default() }
    }

    /// Runs the per-tick clocks: cooldowns down, nectar clock up.
    pub fn tick(&mut self) {
        self.stay_out_ticks = (self.stay_out_ticks - 1).max(0);
        self.hive_cooldown = (self.hive_cooldown - 1).max(0);
        self.flower_cooldown = (self.flower_cooldown - 1).max(0);
        if !self.has_nectar {
            self.ticks_without_nectar += 1;
        }
    }

    /// Takes nectar, which restarts the search clock.
    pub fn set_has_nectar(&mut self, has_nectar: bool) {
        if has_nectar {
            self.ticks_without_nectar = 0;
        }
        self.has_nectar = has_nectar;
    }

    /// Hands the nectar over to the hive and forgets the crops it grew.
    pub fn drop_off_nectar(&mut self) {
        self.has_nectar = false;
        self.crops_grown = 0;
    }

    /// Forgets the hive and waits before choosing another.
    pub fn drop_hive(&mut self) {
        self.hive = None;
        self.hive_cooldown = HIVE_SEARCH_COOLDOWN;
    }

    /// Forgets the bloom and waits 20 to 60 ticks before choosing another.
    pub fn drop_flower(&mut self, cooldown: i32) {
        self.flower = None;
        self.flower_cooldown = cooldown;
    }

    fn tired_of_looking_for_nectar(&self) -> bool {
        self.ticks_without_nectar > TIRED_AFTER
    }

    /// Whether the bee is willing to go home right now, ignoring fire at the
    /// hive (the world answers that).
    #[must_use]
    pub fn wants_hive(&self, has_target: bool) -> bool {
        self.stay_out_ticks <= 0
            && !self.pollinating
            && !self.has_stung
            && !has_target
            && (self.has_nectar || self.tired_of_looking_for_nectar() || self.stays_in_hive)
    }

    fn unreachable_until(&self, cell: Cell) -> Option<u64> {
        self.unreachable.iter().find(|&&(c, _)| c == cell).map(|&(_, until)| until)
    }

    fn blacklist_hive(&mut self, cell: Cell) {
        self.blacklist.push(cell);
        while self.blacklist.len() > 3 {
            self.blacklist.remove(0);
        }
    }
}

/// Whether a block state is a bloom a bee pollinates: in the attractive tag, not
/// waterlogged, and for a sunflower only its upper half.
#[must_use]
pub fn attracts_bees(state: StateId) -> bool {
    if !lodestone_data::tool::block_tag_contains("minecraft:bee_attractive", state.block()) {
        return false;
    }
    let property = |key: &str| state.properties().iter().find(|&&(k, _)| k == key).map(|&(_, v)| v);
    if property("waterlogged") == Some("true") {
        return false;
    }
    state.block() != Block::Sunflower || property("half") == Some("upper")
}

/// The state a bee-growable block advances to: one more age for crops, stems and
/// berry bushes (while a higher age exists), berries on a cave vine without them.
#[must_use]
pub fn grown_state(state: StateId) -> Option<StateId> {
    if !lodestone_data::tool::block_tag_contains("minecraft:bee_growables", state.block()) {
        return None;
    }
    let mut parts: BTreeMap<String, String> =
        state.properties().iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect();
    let name = state.block().name();
    if matches!(state.block(), Block::CaveVines | Block::CaveVinesPlant) {
        if parts.get("berries").map(String::as_str) != Some("false") {
            return None;
        }
        parts.insert("berries".to_owned(), "true".to_owned());
    } else {
        let age: u32 = parts.get("age")?.parse().ok()?;
        parts.insert("age".to_owned(), (age + 1).to_string());
    }
    StateId::from_exact_parts(name, &parts)
}

fn block_of(mob: &dyn MobController) -> Cell {
    let at = mob.position();
    (at.x.floor() as i32, at.y.floor() as i32, at.z.floor() as i32)
}

fn closer_than(mob: &dyn MobController, cell: Cell, distance: i32) -> bool {
    let here = block_of(mob);
    let (dx, dy, dz) = (cell.0 - here.0, cell.1 - here.1, cell.2 - here.2);
    f64::from(dx * dx + dy * dy + dz * dz) < f64::from(distance * distance)
}

fn too_far(mob: &dyn MobController, cell: Cell) -> bool {
    !closer_than(mob, cell, TOO_FAR as i32)
}

fn at_bottom_centre(cell: Cell) -> Vec3 {
    Vec3::new(f64::from(cell.0) + 0.5, f64::from(cell.1), f64::from(cell.2) + 0.5)
}

/// The hive the bee remembers, if it still exists and is within range.
fn valid_hive(mob: &mut dyn MobController) -> Option<Cell> {
    let hive = mob.bee()?.hive?;
    (!too_far(mob, hive) && mob.hive_view(hive).is_some()).then_some(hive)
}

/// Whether the bee will go home now: its own wishes plus fire at the hive.
fn wants_to_enter_hive(mob: &mut dyn MobController) -> bool {
    let has_target = mob.attack_target().is_some();
    let Some(bee) = mob.bee() else { return false };
    if !bee.wants_hive(has_target) {
        return false;
    }
    match valid_hive(mob) {
        Some(hive) => !mob.hive_view(hive).is_some_and(|view| view.fire_nearby),
        // Without a hive nothing burns; the locate goal still wants to run.
        None => true,
    }
}

/// A bee only acts on these goals when calm.
fn calm(mob: &dyn MobController) -> bool {
    mob.angry_target().is_none()
}

/// Gets nearer the target by pathing to a random point toward it.
fn path_randomly_towards(mob: &mut dyn MobController, target: Cell, speed: f64) {
    let target_vec = at_bottom_centre(target);
    let here = block_of(mob);
    let dy = target_vec.y as i32 - here.1;
    let adjust = if dy > 2 {
        4
    } else if dy < -2 {
        -4
    } else {
        0
    };
    let distance = (target.0 - here.0).abs() + (target.1 - here.1).abs() + (target.2 - here.2).abs();
    let (horizontal, vertical) = if distance < 15 { (distance / 2, distance / 2) } else { (6, 8) };
    if let Some(next) = mob.air_point_towards(horizontal, vertical, adjust, target_vec, std::f64::consts::PI / 10.0) {
        mob.set_path_effort(0.5);
        mob.move_to(next, speed);
    }
}

/// Goes into the hive it is standing at.
#[derive(Debug, Default)]
pub struct EnterHiveGoal;

impl Goal for EnterHiveGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(hive) = mob.bee().and_then(|bee| bee.hive) else { return false };
        if !wants_to_enter_hive(mob) {
            return false;
        }
        let at = mob.position();
        let centre = (f64::from(hive.0) + 0.5, f64::from(hive.1) + 0.5, f64::from(hive.2) + 0.5);
        if (centre.0 - at.x).powi(2) + (centre.1 - at.y).powi(2) + (centre.2 - at.z).powi(2) >= 4.0 {
            return false;
        }
        match valid_hive(mob).and_then(|h| mob.hive_view(h)) {
            Some(view) if !view.is_full() => true,
            Some(_) => {
                if let Some(bee) = mob.bee() {
                    bee.hive = None;
                }
                false
            }
            None => false,
        }
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(hive) = valid_hive(mob)
            && let Some(bee) = mob.bee()
        {
            bee.entering = Some(hive);
        }
    }
}

/// Forgets a hive that has gone, checked every 20 to 40 ticks.
#[derive(Debug, Default)]
pub struct ValidateHiveGoal {
    cooldown: Option<i32>,
    last: i64,
}

impl Goal for ValidateHiveGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let cooldown = *self.cooldown.get_or_insert_with(|| 20 + mob.next_i32(21));
        calm(mob) && mob.tick_count() as i64 > self.last + i64::from(cooldown)
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(hive) = mob.bee().and_then(|bee| bee.hive)
            && mob.is_cell_loaded(hive)
            && valid_hive(mob).is_none()
            && let Some(bee) = mob.bee()
        {
            bee.drop_hive();
        }
        self.last = mob.tick_count() as i64;
    }
}

/// Forgets a bloom that has gone, checked every 20 to 40 ticks.
#[derive(Debug, Default)]
pub struct ValidateFlowerGoal {
    cooldown: Option<i32>,
    last: i64,
}

impl Goal for ValidateFlowerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        let cooldown = *self.cooldown.get_or_insert_with(|| 20 + mob.next_i32(21));
        calm(mob) && mob.tick_count() as i64 > self.last + i64::from(cooldown)
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(flower) = mob.bee().and_then(|bee| bee.flower)
            && mob.is_cell_loaded(flower)
            && !mob.attracts_bees_at(flower)
        {
            let cooldown = 20 + mob.next_i32(41);
            if let Some(bee) = mob.bee() {
                bee.drop_flower(cooldown);
            }
        }
        self.last = mob.tick_count() as i64;
    }
}

/// Hovers over a bloom until it carries nectar.
#[derive(Debug)]
pub struct PollinateGoal {
    speed: f64,
    successful_ticks: i32,
    last_sound: i32,
    ticks: i32,
    hover: Option<Vec3>,
}

impl PollinateGoal {
    /// Ticks of hovering before the bee may finish.
    const MIN_POLLINATION: i32 = 400;
    /// Ticks after which the bee gives up on a bloom.
    const MAX_POLLINATING: i32 = 600;
    /// Blocks within which the bee looks for a bloom.
    const SEARCH_RADIUS: i32 = 5;

    /// A pollinate goal for a bee with base movement `speed`.
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self { speed, successful_ticks: 0, last_sound: 0, ticks: 0, hover: None }
    }

    fn long_enough(&self) -> bool {
        self.successful_ticks > Self::MIN_POLLINATION
    }

    /// The nearest bloom within the search radius the bee can path to.
    fn find_flower(mob: &mut dyn MobController) -> Option<Cell> {
        let now = mob.tick_count();
        let origin = block_of(mob);
        let r = Self::SEARCH_RADIUS;
        let mut tried_unreachable: Vec<(Cell, u64)> = Vec::new();
        let mut found = None;
        'depth: for depth in 0..=(3 * r) {
            let max_x = r.min(depth);
            for x in -max_x..=max_x {
                let max_y = r.min(depth - x.abs());
                for y in -max_y..=max_y {
                    let z = depth - x.abs() - y.abs();
                    if z > r {
                        continue;
                    }
                    for z in if z == 0 { vec![0] } else { vec![z, -z] } {
                        let cell = (origin.0 + x, origin.1 + y, origin.2 + z);
                        if !mob.attracts_bees_at(cell) {
                            continue;
                        }
                        if let Some(until) = mob.bee().and_then(|bee| bee.unreachable_until(cell))
                            && now < until
                        {
                            tried_unreachable.push((cell, until));
                            continue;
                        }
                        if mob.path_reaches_block(cell, 1) {
                            found = Some(cell);
                            break 'depth;
                        }
                        tried_unreachable.push((cell, now + 600));
                    }
                }
            }
        }
        if let Some(bee) = mob.bee() {
            bee.unreachable = tried_unreachable;
        }
        found
    }

    fn steer(&self, mob: &mut dyn MobController) {
        if let Some(hover) = self.hover {
            mob.hover_to(hover, self.speed * 0.35);
        }
    }

    fn offset(mob: &mut dyn MobController) -> f64 {
        (f64::from(mob.next_f32()) * 2.0 - 1.0) * f64::from(0.333_333_34_f32)
    }
}

impl Goal for PollinateGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(bee) = mob.bee() else { return false };
        if bee.flower_cooldown > 0 || bee.has_nectar || bee.raining {
            return false;
        }
        match Self::find_flower(mob) {
            Some(flower) => {
                if let Some(bee) = mob.bee() {
                    bee.flower = Some(flower);
                }
                mob.move_to(Vec3::new(f64::from(flower.0) + 0.5, f64::from(flower.1) + 0.5, f64::from(flower.2) + 0.5), self.speed * 1.2);
                true
            }
            None => {
                let cooldown = 20 + mob.next_i32(41);
                if let Some(bee) = mob.bee() {
                    bee.flower_cooldown = cooldown;
                }
                false
            }
        }
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(bee) = mob.bee() else { return false };
        if !bee.pollinating || bee.flower.is_none() || bee.raining {
            return false;
        }
        if self.long_enough() { mob.next_f32() < 0.2 } else { true }
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        self.successful_ticks = 0;
        self.ticks = 0;
        self.last_sound = 0;
        if let Some(bee) = mob.bee() {
            bee.pollinating = true;
            bee.ticks_without_nectar = 0;
        }
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        let done = self.long_enough();
        if let Some(bee) = mob.bee() {
            if done {
                bee.set_has_nectar(true);
            }
            bee.pollinating = false;
            bee.flower_cooldown = FLOWER_RETRY_COOLDOWN;
        }
        mob.stop_navigation();
        self.hover = None;
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(flower) = mob.bee().and_then(|bee| bee.flower) else { return };
        self.ticks += 1;
        if self.ticks > Self::MAX_POLLINATING {
            if let Some(bee) = mob.bee() {
                bee.drop_flower(FLOWER_RETRY_COOLDOWN);
                bee.pollinating = false;
            }
            return;
        }
        let target = Vec3::new(f64::from(flower.0) + 0.5, f64::from(flower.1) + f64::from(0.6_f32), f64::from(flower.2) + 0.5);
        let here = mob.position();
        let away = ((target.x - here.x).powi(2) + (target.y - here.y).powi(2) + (target.z - here.z).powi(2)).sqrt();
        if away > 1.0 {
            self.hover = Some(target);
            self.steer(mob);
            return;
        }
        let hover = *self.hover.get_or_insert(target);
        let arrived = ((here.x - hover.x).powi(2) + (here.y - hover.y).powi(2) + (here.z - hover.z).powi(2)).sqrt() <= 0.1;
        let mut set_wanted = true;
        if arrived {
            if mob.next_i32(25) == 0 {
                let dx = Self::offset(mob);
                let dz = Self::offset(mob);
                self.hover = Some(Vec3::new(target.x + dx, target.y, target.z + dz));
                mob.stop_navigation();
            } else {
                set_wanted = false;
            }
            mob.look_at(target);
        }
        if set_wanted {
            self.steer(mob);
        }
        self.successful_ticks += 1;
        if mob.next_f32() < 0.05 && self.successful_ticks > self.last_sound + 60 {
            self.last_sound = self.successful_ticks;
        }
    }
}

/// Picks the nearest hive with room within 20 blocks.
#[derive(Debug, Default)]
pub struct LocateHiveGoal;

impl Goal for LocateHiveGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(bee) = mob.bee() else { return false };
        if bee.hive_cooldown != 0 || bee.hive.is_some() {
            return false;
        }
        wants_to_enter_hive(mob)
    }

    fn can_continue_to_use(&mut self, _mob: &mut dyn MobController) -> bool {
        false
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        if let Some(bee) = mob.bee() {
            bee.hive_cooldown = HIVE_SEARCH_COOLDOWN;
        }
        let here = block_of(mob);
        let mut hives: Vec<Cell> = mob
            .hives_near(20)
            .into_iter()
            .filter(|&cell| mob.hive_view(cell).is_some_and(|view| !view.is_full()))
            .collect();
        hives.sort_by_key(|&(x, y, z)| (x - here.0).pow(2) + (y - here.1).pow(2) + (z - here.2).pow(2));
        let Some(&first) = hives.first() else { return };
        let Some(bee) = mob.bee() else { return };
        let free = hives.iter().copied().find(|cell| !bee.blacklist.contains(cell));
        match free {
            Some(cell) => bee.hive = Some(cell),
            None => {
                bee.blacklist.clear();
                bee.hive = Some(first);
            }
        }
    }
}

/// Flies to the remembered hive.
#[derive(Debug)]
pub struct GoToHiveGoal {
    speed: f64,
    travelling: i32,
    stuck: i32,
    last_path: Option<u64>,
}

impl GoToHiveGoal {
    /// Ticks of travel before the bee drops the hive.
    const MAX_TRAVELLING: i32 = 2400;
    /// Ticks on an unchanged path before the bee drops the hive.
    const MAX_STUCK: i32 = 60;

    /// A go-to-hive goal for a bee with base movement `speed`.
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self { speed, travelling: 0, stuck: 0, last_path: None }
    }

    fn reached(mob: &mut dyn MobController, hive: Cell) -> bool {
        closer_than(mob, hive, 2) || mob.path_ended_at(hive)
    }

    fn drop_and_blacklist(mob: &mut dyn MobController) {
        if let Some(bee) = mob.bee() {
            if let Some(hive) = bee.hive {
                bee.blacklist_hive(hive);
            }
            bee.drop_hive();
        }
    }
}

impl Goal for GoToHiveGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(hive) = mob.bee().and_then(|bee| bee.hive) else { return false };
        !too_far(mob, hive)
            && wants_to_enter_hive(mob)
            && !Self::reached(mob, hive)
            && mob.hive_view(hive).is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.travelling = 0;
        self.stuck = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.travelling = 0;
        self.stuck = 0;
        mob.stop_navigation();
        mob.set_path_effort(1.0);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(hive) = mob.bee().and_then(|bee| bee.hive) else { return };
        self.travelling += 1;
        if self.travelling > reduced_tick_delay(Self::MAX_TRAVELLING) {
            Self::drop_and_blacklist(mob);
            return;
        }
        if !mob.navigation_done() {
            return;
        }
        if !closer_than(mob, hive, 16) {
            if too_far(mob, hive) {
                if let Some(bee) = mob.bee() {
                    bee.drop_hive();
                }
            } else {
                path_randomly_towards(mob, hive, self.speed);
            }
            return;
        }
        let close_enough = if closer_than(mob, hive, 3) { 1 } else { 2 };
        mob.set_path_effort(10.0);
        let target = Vec3::new(f64::from(hive.0), f64::from(hive.1), f64::from(hive.2));
        mob.move_to_within(target, self.speed, close_enough);
        if !mob.path_reaches_target() {
            Self::drop_and_blacklist(mob);
        } else if self.last_path.is_some() && mob.path_signature() == self.last_path {
            self.stuck += 1;
            if self.stuck > Self::MAX_STUCK {
                if let Some(bee) = mob.bee() {
                    bee.drop_hive();
                }
                self.stuck = 0;
            }
        } else {
            self.last_path = mob.path_signature();
        }
    }
}

/// Flies to the remembered bloom once it has gone without nectar for a while.
#[derive(Debug)]
pub struct GoToKnownFlowerGoal {
    speed: f64,
    travelling: i32,
}

impl GoToKnownFlowerGoal {
    /// Ticks of travel before the bee drops the bloom.
    const MAX_TRAVELLING: i32 = 2400;

    /// A go-to-bloom goal for a bee with base movement `speed`.
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self { speed, travelling: 0 }
    }
}

impl Goal for GoToKnownFlowerGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        let Some(flower) = mob.bee().and_then(|bee| bee.flower) else { return false };
        mob.bee().is_some_and(|bee| bee.ticks_without_nectar > 600) && !closer_than(mob, flower, 2)
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    fn start(&mut self, _mob: &mut dyn MobController) {
        self.travelling = 0;
    }

    fn stop(&mut self, mob: &mut dyn MobController) {
        self.travelling = 0;
        mob.stop_navigation();
        mob.set_path_effort(1.0);
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        let Some(flower) = mob.bee().and_then(|bee| bee.flower) else { return };
        self.travelling += 1;
        if self.travelling > reduced_tick_delay(Self::MAX_TRAVELLING) {
            let cooldown = 20 + mob.next_i32(41);
            if let Some(bee) = mob.bee() {
                bee.drop_flower(cooldown);
            }
        } else if mob.navigation_done() {
            if too_far(mob, flower) {
                let cooldown = 20 + mob.next_i32(41);
                if let Some(bee) = mob.bee() {
                    bee.drop_flower(cooldown);
                }
            } else {
                path_randomly_towards(mob, flower, self.speed);
            }
        }
    }
}

/// Tends the crops under a nectar-carrying bee near its hive.
#[derive(Debug, Default)]
pub struct GrowCropGoal;

impl Goal for GrowCropGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::none()
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        if !calm(mob) {
            return false;
        }
        if mob.bee().is_none_or(|bee| bee.crops_grown >= 10) {
            return false;
        }
        if mob.next_f32() < 0.3 {
            return false;
        }
        mob.bee().is_some_and(|bee| bee.has_nectar) && valid_hive(mob).is_some()
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        self.can_use(mob)
    }

    fn tick(&mut self, mob: &mut dyn MobController) {
        if mob.next_i32(reduced_tick_delay(30)) != 0 {
            return;
        }
        let here = block_of(mob);
        for depth in 1..=2 {
            let cell = (here.0, here.1 - depth, here.2);
            if let Some(grown) = mob.bee_growth_at(cell)
                && let Some(bee) = mob.bee()
            {
                bee.grown.push((cell, grown));
                bee.crops_grown += 1;
            }
        }
    }
}

/// Wanders at random, drifting back toward a hive it has strayed from.
#[derive(Debug)]
pub struct BeeWanderGoal {
    speed: f64,
}

impl BeeWanderGoal {
    /// A wander goal for a bee with base movement `speed`.
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self { speed }
    }

    fn threshold(mob: &mut dyn MobController) -> i32 {
        let attached = mob.bee().is_some_and(|bee| bee.hive.is_some() || bee.flower.is_some());
        48 - if attached { 24 } else { 16 }
    }
}

impl Goal for BeeWanderGoal {
    fn flags(&self) -> FlagSet {
        FlagSet::of(&[Flag::Move])
    }

    fn can_use(&mut self, mob: &mut dyn MobController) -> bool {
        mob.navigation_done() && mob.next_i32(10) == 0
    }

    fn can_continue_to_use(&mut self, mob: &mut dyn MobController) -> bool {
        !mob.navigation_done()
    }

    fn start(&mut self, mob: &mut dyn MobController) {
        let threshold = Self::threshold(mob);
        let toward = match valid_hive(mob) {
            Some(hive) if !closer_than(mob, hive, threshold) => {
                let here = mob.position();
                Some(Vec3::new(f64::from(hive.0) + 0.5 - here.x, f64::from(hive.1) + 0.5 - here.y, f64::from(hive.2) + 0.5 - here.z))
            }
            _ => None,
        };
        if let Some(target) = mob.air_wander_position(toward) {
            mob.move_to(target, self.speed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str) -> StateId {
        StateId::from_state_str(text).expect("state")
    }

    /// The groups below are read straight from the generated tag members, so the
    /// expectation is the tag, not a restatement of the code.
    #[test]
    fn a_dandelion_attracts_bees_but_grass_and_waterlogged_blooms_do_not() {
        assert!(attracts_bees(state("minecraft:dandelion")));
        assert!(!attracts_bees(state("minecraft:short_grass")));
        assert!(!attracts_bees(state("minecraft:stone")));
        assert!(attracts_bees(state("minecraft:flowering_azalea_leaves[distance=7,persistent=false,waterlogged=false]")));
        assert!(!attracts_bees(state("minecraft:flowering_azalea_leaves[distance=7,persistent=false,waterlogged=true]")));
    }

    #[test]
    fn only_the_upper_half_of_a_sunflower_attracts_bees() {
        assert!(attracts_bees(state("minecraft:sunflower[half=upper]")));
        assert!(!attracts_bees(state("minecraft:sunflower[half=lower]")));
    }

    #[test]
    fn crops_grow_one_age_until_the_last() {
        assert_eq!(grown_state(state("minecraft:wheat[age=3]")), Some(state("minecraft:wheat[age=4]")));
        assert_eq!(grown_state(state("minecraft:wheat[age=7]")), None);
        assert_eq!(grown_state(state("minecraft:beetroots[age=2]")), Some(state("minecraft:beetroots[age=3]")));
        assert_eq!(grown_state(state("minecraft:beetroots[age=3]")), None);
        assert_eq!(grown_state(state("minecraft:sweet_berry_bush[age=2]")), Some(state("minecraft:sweet_berry_bush[age=3]")));
        assert_eq!(grown_state(state("minecraft:sweet_berry_bush[age=3]")), None);
        assert_eq!(grown_state(state("minecraft:melon_stem[age=6]")), Some(state("minecraft:melon_stem[age=7]")));
        assert_eq!(grown_state(state("minecraft:melon_stem[age=7]")), None);
    }

    #[test]
    fn an_attached_stem_and_plain_stone_do_not_grow() {
        assert_eq!(grown_state(state("minecraft:attached_melon_stem[facing=north]")), None);
        assert_eq!(grown_state(state("minecraft:stone")), None);
    }

    #[test]
    fn a_cave_vine_gains_berries_once() {
        let bare = state("minecraft:cave_vines_plant[berries=false]");
        let grown = grown_state(bare).expect("a bare vine grows berries");
        assert_eq!(grown, state("minecraft:cave_vines_plant[berries=true]"));
        assert_eq!(grown_state(grown), None);
    }

    #[test]
    fn a_bee_wants_home_for_nectar_tiredness_or_the_hour_but_not_when_stung_or_hunting() {
        let mut bee = BeeState::new(0);
        assert!(!bee.wants_hive(false));
        bee.has_nectar = true;
        assert!(bee.wants_hive(false));
        assert!(!bee.wants_hive(true), "a hunting bee stays out");
        bee.has_stung = true;
        assert!(!bee.wants_hive(false), "a stung bee stays out");
        let mut tired = BeeState::new(0);
        tired.ticks_without_nectar = 3601;
        assert!(tired.wants_hive(false));
        tired.ticks_without_nectar = 3600;
        assert!(!tired.wants_hive(false), "3600 is not yet more than 3600");
        let mut night = BeeState::new(0);
        night.stays_in_hive = true;
        assert!(night.wants_hive(false));
        night.stay_out_ticks = 1;
        assert!(!night.wants_hive(false), "the cooldown outranks the hour");
    }

    #[test]
    fn the_blacklist_keeps_the_last_three_hives() {
        let mut bee = BeeState::new(0);
        for x in 0..5 {
            bee.blacklist_hive((x, 0, 0));
        }
        assert_eq!(bee.blacklist, vec![(2, 0, 0), (3, 0, 0), (4, 0, 0)]);
    }

    #[test]
    fn nectar_resets_the_search_clock_and_dropping_it_off_resets_the_crop_count() {
        let mut bee = BeeState::new(0);
        bee.ticks_without_nectar = 500;
        bee.crops_grown = 4;
        bee.set_has_nectar(true);
        assert_eq!(bee.ticks_without_nectar, 0);
        bee.drop_off_nectar();
        assert!(!bee.has_nectar);
        assert_eq!(bee.crops_grown, 0);
    }

    #[test]
    fn the_clocks_count_down_and_the_nectar_clock_runs_only_without_nectar() {
        let mut bee = BeeState::new(5);
        bee.stay_out_ticks = 2;
        bee.hive_cooldown = 1;
        bee.tick();
        assert_eq!((bee.flower_cooldown, bee.stay_out_ticks, bee.hive_cooldown, bee.ticks_without_nectar), (4, 1, 0, 1));
        bee.set_has_nectar(true);
        bee.tick();
        assert_eq!(bee.ticks_without_nectar, 0);
    }
}
