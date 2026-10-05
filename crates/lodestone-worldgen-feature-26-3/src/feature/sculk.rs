//! `sculk_patch`: sculk spreading outward from charged cursors, as the world-generation spreader
//! does it.
//!
//! Each round adds `charge_count` cursors of `amount_per_charge` at the origin and runs
//! `spread_attempts` update passes; the first `spread_rounds` rounds also grow veins and convert
//! neighbouring blocks, the remaining `growth_rounds` only move charge and place sensors and
//! shriekers. A cursor sits on a sculk block, a sculk vein or anything else (the "plain" case),
//! and each kind spends charge differently. Region writes run no neighbour updates, so every
//! placement here is a plain write.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::multiface::{Ctx, DEFAULT_ORDER, MultifaceConfig, SpreadType};
use super::tree::trunk::shuffle;
use crate::blocks::{BlockId, Dir, FluidKind, State, Support};
use crate::json::{Res, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::tags::BlockSet;

#[derive(Clone, Debug)]
pub struct SculkPatchConfig {
    charge_count: i32,
    amount_per_charge: i32,
    spread_attempts: i32,
    growth_rounds: i32,
    spread_rounds: i32,
}

impl SculkPatchConfig {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        Ok(Self {
            charge_count: int(v, "charge_count", ctx)?,
            amount_per_charge: int(v, "amount_per_charge", ctx)?,
            spread_attempts: int(v, "spread_attempts", ctx)?,
            growth_rounds: int(v, "growth_rounds", ctx)?,
            spread_rounds: int(v, "spread_rounds", ctx)?,
        })
    }
}

/// The world-generation spreader's constants.
const GROWTH_SPAWN_COST: i32 = 50;
const NO_GROWTH_RADIUS: i32 = 1;
const CHARGE_DECAY_RATE: i32 = 5;
const ADDITIONAL_DECAY_RATE: i32 = 10;
const MAX_CURSORS: usize = 32;
const MAX_CHARGE: i32 = 1000;
const MAX_GROWTH_RATE_RADIUS: i32 = 24;
/// A cursor moves at most this far (horizontally, squared) from the origin during generation.
const MAX_SPREAD_SQ: i32 = 144;

/// What a cursor's cell is, which decides how it spends charge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Sculk,
    Vein,
    Plain,
}

impl Behaviour {
    /// Whether a block on this cell can host a cursor's charge (sculk and its veins).
    fn is_sculk(self) -> bool {
        self != Self::Plain
    }

    fn can_change_state_on_spread(self) -> bool {
        self != Self::Sculk
    }

    fn update_decay_delay(self, age: i32) -> i32 {
        if self == Self::Plain { (age - 1).max(0) } else { 1 }
    }
}

struct Cursor {
    pos: Pos,
    charge: i32,
    update_delay: i32,
    decay_delay: i32,
    facings: Option<Vec<Dir>>,
}

struct Spreader<'a> {
    vein: Ctx<'a>,
    same_space: Ctx<'a>,
    sculk: BlockId,
    vein_block: BlockId,
    sensor: BlockId,
    shrieker: BlockId,
    water: BlockId,
    replaceable: &'a BlockSet,
    replaceable_world_gen: &'a BlockSet,
    inhibitors: &'a BlockSet,
    cursors: Vec<Cursor>,
}

/// The non-corner neighbour offsets (those with a zero component), in the order a z-then-y-then-x
/// walk of the 3x3x3 box lists them.
fn neighbour_offsets() -> Vec<(i32, i32, i32)> {
    let mut v = Vec::with_capacity(18);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                if (x == 0 || y == 0 || z == 0) && (x, y, z) != (0, 0, 0) {
                    v.push((x, y, z));
                }
            }
        }
    }
    v
}

fn dist_sq(a: Pos, b: Pos) -> f64 {
    let (dx, dy, dz) = (f64::from(a.x - b.x), f64::from(a.y - b.y), f64::from(a.z - b.z));
    dx * dx + dy * dy + dz * dz
}

impl<'a> Spreader<'a> {
    fn behaviour(&self, level: &Level<'_>, s: State) -> Behaviour {
        let b = level.env.blocks.block_of(s);
        if b == self.sculk {
            Behaviour::Sculk
        } else if b == self.vein_block {
            Behaviour::Vein
        } else {
            Behaviour::Plain
        }
    }

    fn add_cursors(&mut self, pos: Pos, mut charge: i32) {
        while charge > 0 {
            let current = charge.min(MAX_CHARGE);
            if self.cursors.len() < MAX_CURSORS {
                self.cursors.push(Cursor { pos, charge: current, update_delay: 0, decay_delay: 1, facings: None });
            }
            charge -= current;
        }
    }

    fn update_cursors(&mut self, level: &mut Level<'_>, origin: Pos, rng: &mut Rng, spread_veins: bool) {
        if self.cursors.is_empty() {
            return;
        }
        let mut cursors = std::mem::take(&mut self.cursors);
        let mut kept = Vec::with_capacity(cursors.len());
        for mut cursor in cursors.drain(..) {
            let chessboard = (cursor.pos.x - origin.x).abs().max((cursor.pos.y - origin.y).abs()).max((cursor.pos.z - origin.z).abs());
            if chessboard > 1024 {
                continue;
            }
            self.update(&mut cursor, level, origin, rng, spread_veins);
            if cursor.charge > 0 {
                kept.push(cursor);
            }
        }
        self.cursors = kept;
    }

    fn update(&self, c: &mut Cursor, level: &mut Level<'_>, origin: Pos, rng: &mut Rng, spread_veins: bool) {
        if c.charge <= 0 {
            return;
        }
        if c.update_delay > 0 {
            c.update_delay -= 1;
            return;
        }
        let mut state = level.get(c.pos.x, c.pos.y, c.pos.z);
        let mut behaviour = self.behaviour(level, state);
        if spread_veins && self.attempt_spread_vein(level, behaviour, c.pos, state, c.facings.as_deref()) && behaviour.can_change_state_on_spread() {
            state = level.get(c.pos.x, c.pos.y, c.pos.z);
            behaviour = self.behaviour(level, state);
        }
        c.charge = self.attempt_use_charge(behaviour, c, level, origin, rng, spread_veins);
        if c.charge <= 0 {
            self.on_discharged(behaviour, level, state, c.pos);
            return;
        }
        match self.valid_movement_pos(level, c.pos, rng, origin) {
            Some(to) => {
                self.on_discharged(behaviour, level, state, c.pos);
                c.pos = to;
                state = level.get(to.x, to.y, to.z);
            }
            None => {
                self.on_discharged(behaviour, level, state, c.pos);
                c.charge = 0;
                return;
            }
        }
        if self.behaviour(level, state).is_sculk() {
            c.facings = Some(self.available_faces(level, state));
        }
        c.decay_delay = behaviour.update_decay_delay(c.decay_delay);
        c.update_delay = 1;
    }

    /// The faces of a multiface state, in direction order (none for any other block).
    fn available_faces(&self, level: &Level<'_>, state: State) -> Vec<Dir> {
        Dir::ALL.into_iter().filter(|d| self.vein.has_face(level, state, *d)).collect()
    }

    fn attempt_spread_vein(&self, level: &mut Level<'_>, behaviour: Behaviour, pos: Pos, state: State, facings: Option<&[Dir]>) -> bool {
        if behaviour == Behaviour::Plain {
            match facings {
                None => {
                    let here = level.get(pos.x, pos.y, pos.z);
                    return self.same_space.spread_all(level, here, pos, &[SpreadType::SamePosition]) > 0;
                }
                Some(f) if !f.is_empty() => {
                    let b = &level.env.blocks;
                    if !b.is_air(state) && !(b.fluid(state) == FluidKind::Water && b.fluid_is_source(state)) {
                        return false;
                    }
                    return self.regrow(level, pos, state, f);
                }
                Some(_) => {}
            }
        }
        self.vein.spread_all(level, state, pos, &DEFAULT_ORDER) > 0
    }

    /// Re-places a vein on the faces of a cell that can still hold one.
    fn regrow(&self, level: &mut Level<'_>, pos: Pos, existing: State, faces: &[Dir]) -> bool {
        let b = &level.env.blocks;
        let mut new = b.default_state(self.vein_block);
        let mut any = false;
        for &face in faces {
            if super::misc::can_attach(level, pos, face) {
                new = b.with(new, face.name(), "true").expect("face property");
                any = true;
            }
        }
        if !any {
            return false;
        }
        if b.fluid(existing) != FluidKind::Empty {
            new = b.with(new, "waterlogged", "true").expect("waterlogged");
        }
        level.set(pos.x, pos.y, pos.z, new);
        true
    }

    fn attempt_use_charge(&self, behaviour: Behaviour, c: &Cursor, level: &mut Level<'_>, origin: Pos, rng: &mut Rng, spread_veins: bool) -> i32 {
        match behaviour {
            Behaviour::Plain => {
                if c.decay_delay > 0 {
                    c.charge
                } else {
                    0
                }
            }
            Behaviour::Sculk => self.sculk_use_charge(c, level, origin, rng),
            Behaviour::Vein => {
                if spread_veins && self.attempt_place_sculk(level, c.pos, rng) {
                    c.charge - 1
                } else if rng.next_int_bounded(CHARGE_DECAY_RATE) == 0 {
                    (c.charge as f32 * 0.5_f32).floor() as i32
                } else {
                    c.charge
                }
            }
        }
    }

    fn sculk_use_charge(&self, c: &Cursor, level: &mut Level<'_>, origin: Pos, rng: &mut Rng) -> i32 {
        let charge = c.charge;
        if charge == 0 || rng.next_int_bounded(CHARGE_DECAY_RATE) != 0 {
            return charge;
        }
        let pos = c.pos;
        let close = dist_sq(pos, origin) < f64::from(NO_GROWTH_RADIUS) * f64::from(NO_GROWTH_RADIUS);
        if !close && self.can_place_growth(level, pos) {
            if rng.next_int_bounded(GROWTH_SPAWN_COST) < charge {
                let at = pos.above();
                let b = &level.env.blocks;
                let mut state = if rng.next_int_bounded(11) == 0 {
                    b.with(b.default_state(self.shrieker), "can_summon", "true").expect("can_summon")
                } else {
                    b.default_state(self.sensor)
                };
                if b.fluid(level.get(at.x, at.y, at.z)) != FluidKind::Empty {
                    state = b.with(state, "waterlogged", "true").unwrap_or(state);
                }
                level.set(at.x, at.y, at.z, state);
            }
            return (charge - GROWTH_SPAWN_COST).max(0);
        }
        if rng.next_int_bounded(ADDITIONAL_DECAY_RATE) != 0 {
            return charge;
        }
        charge - if close { 1 } else { decay_penalty(pos, origin, charge) }
    }

    fn can_place_growth(&self, level: &Level<'_>, pos: Pos) -> bool {
        let b = &level.env.blocks;
        let above = level.get(pos.x, pos.y + 1, pos.z);
        let open = b.is_air(above) || (b.block_of(above) == self.water && b.fluid(above) == FluidKind::Water && b.fluid_is_source(above));
        if !open {
            return false;
        }
        let mut matched = 0;
        for y in 0..=2 {
            for z in -4..=4 {
                for x in -4..=4 {
                    if self.inhibitors.contains(b.block_of(level.get(pos.x + x, pos.y + y, pos.z + z))) {
                        matched += 1;
                        if matched > 2 {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// A vein turns one supported replaceable neighbour into sculk and veins around it.
    fn attempt_place_sculk(&self, level: &mut Level<'_>, pos: Pos, rng: &mut Rng) -> bool {
        let state = level.get(pos.x, pos.y, pos.z);
        let mut dirs = Dir::ALL;
        shuffle(&mut dirs, rng);
        for support in dirs {
            if !self.vein.has_face(level, state, support) {
                continue;
            }
            let at = pos.relative(support);
            let existing = level.get(at.x, at.y, at.z);
            if !self.replaceable_world_gen.contains(level.env.blocks.block_of(existing)) {
                continue;
            }
            let sculk = level.env.blocks.default_state(self.sculk);
            level.set(at.x, at.y, at.z, sculk);
            self.vein.spread_all(level, sculk, at, &DEFAULT_ORDER);
            let skip = support.opposite();
            for d in Dir::ALL {
                if d == skip {
                    continue;
                }
                let vp = at.relative(d);
                let s = level.get(vp.x, vp.y, vp.z);
                if self.behaviour(level, s) == Behaviour::Vein {
                    self.on_discharged(Behaviour::Vein, level, s, vp);
                }
            }
            return true;
        }
        false
    }

    /// A drained vein drops the faces that now touch sculk, and disappears with its last face.
    fn on_discharged(&self, behaviour: Behaviour, level: &mut Level<'_>, state: State, pos: Pos) {
        if behaviour != Behaviour::Vein || level.env.blocks.block_of(state) != self.vein_block {
            return;
        }
        let b = &level.env.blocks;
        let mut state = state;
        for d in Dir::ALL {
            let n = level.get(pos.x + d.step().0, pos.y + d.step().1, pos.z + d.step().2);
            if self.vein.has_face(level, state, d) && b.block_of(n) == self.sculk {
                state = b.with(state, d.name(), "false").expect("face property");
            }
        }
        if !Dir::ALL.into_iter().any(|d| self.vein.has_face(level, state, d)) {
            state = if b.fluid(level.get(pos.x, pos.y, pos.z)) == FluidKind::Empty {
                b.default_state(b.block_by_name("air").expect("air"))
            } else {
                b.default_state(self.water)
            };
        }
        level.set(pos.x, pos.y, pos.z, state);
    }

    fn valid_movement_pos(&self, level: &Level<'_>, pos: Pos, rng: &mut Rng, origin: Pos) -> Option<Pos> {
        let mut offsets = neighbour_offsets();
        shuffle(&mut offsets, rng);
        let mut best = pos;
        for (dx, dy, dz) in offsets {
            let n = pos.offset(dx, dy, dz);
            let (ox, oz) = (origin.x - n.x, origin.z - n.z);
            if ox * ox + oz * oz > MAX_SPREAD_SQ {
                continue;
            }
            let t = level.get(n.x, n.y, n.z);
            if self.behaviour(level, t).is_sculk() && movement_unobstructed(level, pos, n) {
                best = n;
                if self.has_substrate_access(level, t, n) {
                    break;
                }
            }
        }
        (best != pos).then_some(best)
    }

    fn has_substrate_access(&self, level: &Level<'_>, state: State, pos: Pos) -> bool {
        if level.env.blocks.block_of(state) != self.vein_block {
            return false;
        }
        Dir::ALL.into_iter().any(|d| {
            let n = pos.relative(d);
            self.vein.has_face(level, state, d) && self.replaceable.contains(level.env.blocks.block_of(level.get(n.x, n.y, n.z)))
        })
    }
}

/// The charge a far-from-origin cursor loses to decay: grows with distance past the no-growth radius.
fn decay_penalty(pos: Pos, origin: Pos, charge: i32) -> i32 {
    let outer = dist_sq(pos, origin).sqrt() as f32 - NO_GROWTH_RADIUS as f32;
    let outer_sq = outer * outer;
    let reach = MAX_GROWTH_RATE_RADIUS - NO_GROWTH_RADIUS;
    let max_reach_sq = (reach * reach) as f32;
    let factor = (outer_sq / max_reach_sq).min(1.0);
    ((charge as f32 * factor * 0.5_f32) as i32).max(1)
}

fn unobstructed(level: &Level<'_>, from: Pos, d: Dir) -> bool {
    let p = from.relative(d);
    !level.env.blocks.face_sturdy(level.get(p.x, p.y, p.z), d.opposite(), Support::Full)
}

/// Whether a diagonal step has an open side to pass through (orthogonal steps always do).
fn movement_unobstructed(level: &Level<'_>, from: Pos, to: Pos) -> bool {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    if dx.abs() + dy.abs() + dz.abs() == 1 {
        return true;
    }
    let dir_x = if dx < 0 { Dir::West } else { Dir::East };
    let dir_y = if dy < 0 { Dir::Down } else { Dir::Up };
    let dir_z = if dz < 0 { Dir::North } else { Dir::South };
    if dx == 0 {
        unobstructed(level, from, dir_y) || unobstructed(level, from, dir_z)
    } else if dy == 0 {
        unobstructed(level, from, dir_x) || unobstructed(level, from, dir_z)
    } else {
        unobstructed(level, from, dir_x) || unobstructed(level, from, dir_y)
    }
}

fn can_spread_from(level: &Level<'_>, sculk: BlockId, vein: BlockId, origin: Pos) -> bool {
    let b = &level.env.blocks;
    let start = level.get(origin.x, origin.y, origin.z);
    let block = b.block_of(start);
    if block == sculk || block == vein {
        return true;
    }
    let water_source = b.block_name(block) == "minecraft:water" && b.fluid_is_source(start);
    if !b.is_air(start) && !water_source {
        return false;
    }
    Dir::ALL.into_iter().any(|d| {
        let n = origin.relative(d);
        b.full_collision(level.get(n.x, n.y, n.z))
    })
}

pub fn place(cfg: &SculkPatchConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let vein_cfg = MultifaceConfig::sculk_vein(env);
    let same = MultifaceConfig::sculk_vein(env);
    let sculk = blocks.block_by_name("sculk").expect("sculk");
    if !can_spread_from(level, sculk, vein_cfg.block, origin) {
        return false;
    }
    let tag = |n: &str| env.tags.get(n).unwrap_or_else(|| panic!("unknown block tag {n}"));
    let mut spreader = Spreader {
        vein: Ctx { cfg: &vein_cfg },
        same_space: Ctx { cfg: &same },
        sculk,
        vein_block: vein_cfg.block,
        sensor: blocks.block_by_name("sculk_sensor").expect("sensor"),
        shrieker: blocks.block_by_name("sculk_shrieker").expect("shrieker"),
        water: blocks.block_by_name("water").expect("water"),
        replaceable: tag("sculk_replaceable"),
        replaceable_world_gen: tag("sculk_replaceable_world_gen"),
        inhibitors: tag("sculk_growth_inhibitors"),
        cursors: Vec::new(),
    };
    let rounds = cfg.spread_rounds + cfg.growth_rounds;
    for round in 0..rounds {
        for _ in 0..cfg.charge_count {
            spreader.add_cursors(origin, cfg.amount_per_charge);
        }
        let spread_veins = round < cfg.spread_rounds;
        for _ in 0..cfg.spread_attempts {
            spreader.update_cursors(level, origin, rng, spread_veins);
        }
        spreader.cursors.clear();
    }
    true
}
