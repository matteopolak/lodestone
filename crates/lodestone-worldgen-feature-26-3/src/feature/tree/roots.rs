//! Root placers: blocks laid under and around the trunk before it grows (mangrove roots).
//!
//! The placer first walks the column between the origin and the trunk base; every cell must
//! accept a root. Each horizontal side then simulates a random descending walk, and only when
//! all four sides succeed are any roots written. The walk's draw order is the contract with the
//! oracle.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::{Run, block_list, int_provider};
use crate::blocks::{BlockId, Dir, FluidKind, State};
use crate::env::Env;
use crate::json::{Res, float, get, int, type_of};
use crate::pos::Pos;
use crate::provider::IntProvider;
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct RootPlacer {
    trunk_offset_y: IntProvider,
    root: StateProvider,
    /// A second block above a root, with its chance.
    above: Option<(StateProvider, f32)>,
    can_grow_through: Vec<BlockId>,
    muddy_in: Vec<BlockId>,
    muddy: StateProvider,
    max_width: i32,
    max_length: i32,
    skew_chance: f32,
}

impl RootPlacer {
    pub fn parse(env: &Env, v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
        let kind = type_of(v, ctx)?;
        if kind != "mangrove_root_placer" {
            unsupported.push(format!("root_placer {kind}"));
        }
        let m = get(v, "mangrove_root_placement", ctx)?;
        let above = match v.get("above_root_placement") {
            Some(a) => Some((StateProvider::parse(env, get(a, "above_root_provider", ctx)?, ctx)?, float(a, "above_root_placement_chance", ctx)?)),
            None => None,
        };
        Ok(Self {
            trunk_offset_y: int_provider(v, "trunk_offset_y", ctx)?,
            root: StateProvider::parse(env, get(v, "root_provider", ctx)?, ctx)?,
            above,
            can_grow_through: block_list(env, get(m, "can_grow_through", ctx)?, ctx)?,
            muddy_in: block_list(env, get(m, "muddy_roots_in", ctx)?, ctx)?,
            muddy: StateProvider::parse(env, get(m, "muddy_roots_provider", ctx)?, ctx)?,
            max_width: int(m, "max_root_width", ctx)?,
            max_length: int(m, "max_root_length", ctx)?,
            skew_chance: float(m, "random_skew_chance", ctx)?,
        })
    }

    /// The trunk's base: the origin raised by a sampled offset.
    pub fn trunk_origin(&self, run: &mut Run<'_, '_>, origin: Pos) -> Pos {
        origin.offset(0, self.trunk_offset_y.sample(run.rng), 0)
    }

    fn can_place(&self, run: &Run<'_, '_>, p: Pos) -> bool {
        run.valid_tree_pos(p) || self.can_grow_through.contains(&run.env.blocks.block_of(run.get(p)))
    }

    /// Whether the roots fit; writes them when they do.
    pub fn place_roots(&self, run: &mut Run<'_, '_>, origin: Pos, trunk_origin: Pos) -> bool {
        let mut roots = Vec::new();
        let mut column = origin;
        while column.y < trunk_origin.y {
            if !self.can_place(run, column) {
                return false;
            }
            column = column.above();
        }
        roots.push(trunk_origin.below());
        for dir in Dir::HORIZONTAL {
            let start = trunk_origin.relative(dir);
            let mut found = Vec::new();
            if !self.simulate(run, start, dir, trunk_origin, &mut found, 0) {
                return false;
            }
            roots.extend(found);
            roots.push(trunk_origin.relative(dir));
        }
        for p in roots {
            self.place_root(run, p);
        }
        true
    }

    fn simulate(&self, run: &mut Run<'_, '_>, at: Pos, dir: Dir, origin: Pos, found: &mut Vec<Pos>, layer: i32) -> bool {
        if layer == self.max_length || found.len() as i32 > self.max_length {
            return false;
        }
        for p in self.candidates(run, at, dir, origin) {
            if self.can_place(run, p) {
                found.push(p);
                if !self.simulate(run, p, dir, origin, found, layer + 1) {
                    return false;
                }
            }
        }
        true
    }

    fn candidates(&self, run: &mut Run<'_, '_>, at: Pos, dir: Dir, origin: Pos) -> Vec<Pos> {
        let below = at.below();
        let next_to = at.relative(dir);
        let width = (at.x - origin.x).abs() + (at.y - origin.y).abs() + (at.z - origin.z).abs();
        if width > self.max_width - 3 && width <= self.max_width {
            if run.rng.next_float() < self.skew_chance { vec![below, next_to.below()] } else { vec![below] }
        } else if width > self.max_width {
            vec![below]
        } else if run.rng.next_float() < self.skew_chance {
            vec![below]
        } else if run.rng.next_bool() {
            vec![next_to]
        } else {
            vec![below]
        }
    }

    fn waterlogged(&self, run: &Run<'_, '_>, p: Pos, s: State) -> State {
        let blocks = &run.env.blocks;
        if blocks.has_property(s, "waterlogged") {
            let water = blocks.fluid(run.get(p)) == FluidKind::Water;
            blocks.with(s, "waterlogged", if water { "true" } else { "false" }).expect("waterlogged is boolean")
        } else {
            s
        }
    }

    fn place_root(&self, run: &mut Run<'_, '_>, p: Pos) {
        if self.muddy_in.contains(&run.env.blocks.block_of(run.get(p))) {
            let s = self.muddy.get(run.level, run.rng, p.x, p.y, p.z);
            let s = self.waterlogged(run, p, s);
            run.set_root(p, s);
        } else if run.valid_tree_pos(p) || self.can_grow_through.contains(&run.env.blocks.block_of(run.get(p))) {
            let s = self.root.get(run.level, run.rng, p.x, p.y, p.z);
            let s = self.waterlogged(run, p, s);
            run.set_root(p, s);
            if let Some((provider, chance)) = &self.above {
                let above = p.above();
                if run.rng.next_float() < *chance && run.is_air(above) {
                    let s = provider.get(run.level, run.rng, above.x, above.y, above.z);
                    let s = self.waterlogged(run, above, s);
                    run.set_root(above, s);
                }
            }
        }
    }
}
