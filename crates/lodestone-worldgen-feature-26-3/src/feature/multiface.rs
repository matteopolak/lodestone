//! `multiface_growth`: glow lichen and sculk vein patches stuck to nearby surfaces.
//!
//! A growth is placed at the origin if it can attach there, otherwise along a search from the
//! origin in each shuffled direction; each placement may spread once to a neighbouring face.
//! Glow lichen uses the plain spreader; sculk vein refuses cells next to sculk, sturdy blocks
//! two steps away and non-water fluids, and accepts any block as a spread source.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::misc::can_attach;
use super::tree::trunk::shuffle;
use crate::blocks::{BlockId, Dir, FluidKind, State, Support};
use crate::env::Env;
use crate::json::{Res, array, boolean, float_or, get, int_or};
use crate::level::Level;
use crate::pos::{Pos, Rng};

#[derive(Clone, Debug)]
pub struct MultifaceConfig {
    pub block: BlockId,
    pub sculk: bool,
    pub search_range: i32,
    pub floor: bool,
    pub ceiling: bool,
    pub wall: bool,
    pub chance_of_spreading: f32,
    pub placed_on: Vec<BlockId>,
}

impl MultifaceConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let name = get(v, "block", ctx)?.as_str().ok_or_else(|| format!("{ctx}: block"))?;
        let block = env.blocks.block_by_name(name).ok_or_else(|| format!("{ctx}: unknown block {name}"))?;
        let mut placed_on = Vec::new();
        for n in array(v, "can_be_placed_on", ctx)? {
            let n = n.as_str().unwrap_or_default();
            placed_on.push(env.blocks.block_by_name(n).ok_or_else(|| format!("{ctx}: unknown block {n}"))?);
        }
        Ok(Self {
            block,
            sculk: name.ends_with("sculk_vein"),
            search_range: int_or(v, "search_range", 10, ctx)?,
            floor: boolean(v, "can_place_on_floor", false, ctx)?,
            ceiling: boolean(v, "can_place_on_ceiling", false, ctx)?,
            wall: boolean(v, "can_place_on_wall", false, ctx)?,
            chance_of_spreading: float_or(v, "chance_of_spreading", 0.5, ctx)?,
            placed_on,
        })
    }

    /// The sculk vein configuration the cursor-driven sculk spreader places with.
    pub(crate) fn sculk_vein(env: &Env) -> Self {
        Self {
            block: env.blocks.block_by_name("sculk_vein").expect("sculk_vein"),
            sculk: true,
            search_range: 0,
            floor: false,
            ceiling: false,
            wall: false,
            chance_of_spreading: 0.0,
            placed_on: Vec::new(),
        }
    }

    fn valid_directions(&self) -> Vec<Dir> {
        let mut v = Vec::new();
        if self.ceiling {
            v.push(Dir::Up);
        }
        if self.floor {
            v.push(Dir::Down);
        }
        if self.wall {
            v.extend(Dir::HORIZONTAL);
        }
        v
    }
}

fn air_or_water(env: &Env, s: State) -> bool {
    let b = &env.blocks;
    b.is_air(s) || b.block_of(s) == b.block_by_name("water").expect("water")
}

/// Where a spread lands relative to the source cell, tried in the order a spreader lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpreadType {
    SamePosition,
    SamePlane,
    WrapAround,
}

pub(crate) const DEFAULT_ORDER: [SpreadType; 3] = [SpreadType::SamePosition, SpreadType::SamePlane, SpreadType::WrapAround];

pub(crate) struct Ctx<'a> {
    pub(crate) cfg: &'a MultifaceConfig,
}

impl Ctx<'_> {
    pub(crate) fn has_face(&self, level: &Level<'_>, s: State, d: Dir) -> bool {
        let b = &level.env.blocks;
        b.block_of(s) == self.cfg.block && b.get(s, d.name()) == Some("true")
    }

    pub(crate) fn valid_for_placement(&self, level: &Level<'_>, old: State, pos: Pos, d: Dir) -> bool {
        (!self.is_this(level, old) || !self.has_face(level, old, d)) && can_attach(level, pos, d)
    }

    fn is_this(&self, level: &Level<'_>, s: State) -> bool {
        level.env.blocks.block_of(s) == self.cfg.block
    }

    pub(crate) fn state_for_placement(&self, level: &Level<'_>, old: State, pos: Pos, d: Dir) -> Option<State> {
        if !self.valid_for_placement(level, old, pos, d) {
            return None;
        }
        let b = &level.env.blocks;
        let base = if self.is_this(level, old) {
            old
        } else if b.fluid(old) == FluidKind::Water && b.fluid_is_source(old) {
            b.with(b.default_state(self.cfg.block), "waterlogged", "true").expect("waterlogged")
        } else {
            b.default_state(self.cfg.block)
        };
        b.with(base, d.name(), "true")
    }

    fn can_be_replaced(&self, level: &Level<'_>, source: Pos, at: Pos, face: Dir, existing: State) -> bool {
        let env = level.env;
        let b = &env.blocks;
        let plain = |existing: State| {
            b.is_air(existing)
                || self.is_this(level, existing)
                || (b.block_of(existing) == b.block_by_name("water").expect("water") && b.fluid_is_source(existing))
        };
        if !self.cfg.sculk {
            return plain(existing);
        }
        let (dx, dy, dz) = face.step();
        let against = level.get(at.x + dx, at.y + dy, at.z + dz);
        let name = b.block_name(b.block_of(against)).trim_start_matches("minecraft:");
        if matches!(name, "sculk" | "sculk_catalyst" | "moving_piston") {
            return false;
        }
        if (source.x - at.x).abs() + (source.y - at.y).abs() + (source.z - at.z).abs() == 2 {
            let n = source.relative(face.opposite());
            if b.face_sturdy(level.get(n.x, n.y, n.z), face, Support::Full) {
                return false;
            }
        }
        let fluid = b.fluid(existing);
        if fluid != FluidKind::Empty && fluid != FluidKind::Water {
            return false;
        }
        if env.tags.get("fire").expect("fire tag").contains(b.block_of(existing)) {
            return false;
        }
        b.replaceable(existing) || plain(existing)
    }

    fn can_spread_into(&self, level: &Level<'_>, source: Pos, at: Pos, face: Dir) -> bool {
        let existing = level.get(at.x, at.y, at.z);
        self.can_be_replaced(level, source, at, face, existing) && self.valid_for_placement(level, existing, at, face)
    }

    /// The spread cell reached from `pos` going `spread` from `from`, if it can be filled.
    fn spread_pos(&self, level: &Level<'_>, state: State, pos: Pos, from: Dir, spread: Dir, types: &[SpreadType]) -> Option<(Pos, Dir)> {
        if spread.axis_name() == from.axis_name() {
            return None;
        }
        let sculk_source = self.cfg.sculk && level.env.blocks.block_of(state) != self.cfg.block;
        if !(sculk_source || (self.has_face(level, state, from) && !self.has_face(level, state, spread))) {
            return None;
        }
        types
            .iter()
            .map(|t| match t {
                SpreadType::SamePosition => (pos, spread),
                SpreadType::SamePlane => (pos.relative(spread), from),
                SpreadType::WrapAround => (pos.relative(spread).relative(from), spread.opposite()),
            })
            .find(|&(p, f)| self.can_spread_into(level, pos, p, f))
    }

    /// Whether a spread may start from this face of `state`.
    fn can_spread_from(&self, level: &Level<'_>, state: State, face: Dir) -> bool {
        (self.cfg.sculk && level.env.blocks.block_of(state) != self.cfg.block) || self.has_face(level, state, face)
    }

    /// Spreads from every face of `state` toward every direction, returning how many cells were
    /// written (the cursor-driven spreaders' own measure of success).
    pub(crate) fn spread_all(&self, level: &mut Level<'_>, state: State, pos: Pos, types: &[SpreadType]) -> u32 {
        let mut count = 0;
        for face in Dir::ALL {
            if !self.can_spread_from(level, state, face) {
                continue;
            }
            for spread in Dir::ALL {
                let Some((p, f)) = self.spread_pos(level, state, pos, face, spread, types) else { continue };
                let old = level.get(p.x, p.y, p.z);
                if let Some(s) = self.state_for_placement(level, old, p, f)
                    && level.set(p.x, p.y, p.z, s)
                {
                    count += 1;
                }
            }
        }
        count
    }

    fn spread_from_face_toward_random(&self, level: &mut Level<'_>, rng: &mut Rng, state: State, pos: Pos, from: Dir) {
        let mut dirs = Dir::ALL;
        shuffle(&mut dirs, rng);
        for spread in dirs {
            let Some((p, f)) = self.spread_pos(level, state, pos, from, spread, &DEFAULT_ORDER) else { continue };
            let old = level.get(p.x, p.y, p.z);
            if let Some(s) = self.state_for_placement(level, old, p, f) {
                level.set(p.x, p.y, p.z, s);
                return;
            }
        }
    }

    fn place_growth(&self, level: &mut Level<'_>, rng: &mut Rng, pos: Pos, old: State, dirs: &[Dir]) -> bool {
        for d in dirs {
            let n = level.get(pos.x + d.step().0, pos.y + d.step().1, pos.z + d.step().2);
            if self.cfg.placed_on.contains(&level.env.blocks.block_of(n)) {
                let Some(new) = self.state_for_placement(level, old, pos, *d) else { return false };
                level.set(pos.x, pos.y, pos.z, new);
                if rng.next_float() < self.cfg.chance_of_spreading {
                    self.spread_from_face_toward_random(level, rng, new, pos, *d);
                }
                return true;
            }
        }
        false
    }
}

pub fn place(cfg: &MultifaceConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let at = level.get(origin.x, origin.y, origin.z);
    if !air_or_water(env, at) {
        return false;
    }
    let cx = Ctx { cfg };
    let mut dirs = cfg.valid_directions();
    shuffle(&mut dirs, rng);
    if cx.place_growth(level, rng, origin, at, &dirs) {
        return true;
    }
    for search in &dirs {
        let mut except: Vec<Dir> = cfg.valid_directions().into_iter().filter(|d| *d != search.opposite()).collect();
        shuffle(&mut except, rng);
        for _ in 0..cfg.search_range {
            let p = origin.relative(*search);
            let s = level.get(p.x, p.y, p.z);
            if !air_or_water(env, s) && env.blocks.block_of(s) != cfg.block {
                break;
            }
            if cx.place_growth(level, rng, p, s, &except) {
                return true;
            }
        }
    }
    false
}
