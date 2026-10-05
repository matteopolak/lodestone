//! `vegetation_patch` and `waterlogged_vegetation_patch`: a disc of ground blocks on a cave
//! floor or ceiling, then a nested feature planted on some of the surface cells.
//!
//! The surface cells live in a [`JavaSet`] because the reference iterates a hash set when it
//! plants vegetation, so the iteration order decides which draws go where.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{BlockId, Dir, Support};
use crate::env::Env;
use crate::javaset::JavaSet;
use crate::json::{Res, float, get, int, string};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::provider::IntProvider;
use crate::registry::{Loader, PlacedFeature};
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct PatchConfig {
    pub waterlogged: bool,
    pub replaceable: Vec<BlockId>,
    pub ground: StateProvider,
    pub vegetation: Arc<PlacedFeature>,
    /// The direction from the open space toward the ground (down for a floor patch).
    pub toward_ground: Dir,
    pub depth: IntProvider,
    pub extra_bottom: f32,
    pub vertical_range: i32,
    pub vegetation_chance: f32,
    pub xz_radius: IntProvider,
    pub extra_edge: f32,
}

impl PatchConfig {
    pub fn parse(env: &Env, loader: &mut Loader<'_>, v: &Value, waterlogged: bool, ctx: &str) -> Res<Self> {
        let tag = string(v, "replaceable", ctx)?;
        let tag = tag.strip_prefix('#').ok_or_else(|| format!("{ctx}: replaceable must be a tag"))?;
        let tag = tag.strip_prefix("minecraft:").unwrap_or(tag);
        let surface = string(v, "surface", ctx)?;
        Ok(Self {
            waterlogged,
            replaceable: env.tags.ordered(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.to_vec(),
            ground: StateProvider::parse(env, get(v, "ground_state", ctx)?, ctx)?,
            vegetation: loader.placed_ref(env, get(v, "vegetation_feature", ctx)?, ctx)?,
            toward_ground: if surface == "floor" { Dir::Down } else { Dir::Up },
            depth: IntProvider::parse(get(v, "depth", ctx)?, ctx)?,
            extra_bottom: float(v, "extra_bottom_block_chance", ctx)?,
            vertical_range: int(v, "vertical_range", ctx)?,
            vegetation_chance: float(v, "vegetation_chance", ctx)?,
            xz_radius: IntProvider::parse(get(v, "xz_radius", ctx)?, ctx)?,
            extra_edge: float(v, "extra_edge_column_chance", ctx)?,
        })
    }

    fn place_ground(&self, level: &mut Level<'_>, rng: &mut Rng, below: &mut Pos, depth: i32) -> bool {
        let env = level.env;
        for i in 0..depth {
            let to_place = self.ground.get(level, rng, below.x, below.y, below.z);
            let here = level.get(below.x, below.y, below.z);
            if env.blocks.block_of(to_place) != env.blocks.block_of(here) {
                if !self.replaceable.contains(&env.blocks.block_of(here)) {
                    return i != 0;
                }
                level.set(below.x, below.y, below.z, to_place);
                *below = below.relative(self.toward_ground);
            }
        }
        true
    }

    fn ground_patch(&self, level: &mut Level<'_>, rng: &mut Rng, origin: Pos, xr: i32, zr: i32) -> JavaSet {
        let env = level.env;
        let blocks = &env.blocks;
        let inwards = self.toward_ground;
        let outwards = inwards.opposite();
        let mut surface = JavaSet::new();
        for dx in -xr..=xr {
            let x_edge = dx == -xr || dx == xr;
            for dz in -zr..=zr {
                let z_edge = dz == -zr || dz == zr;
                let edge = x_edge || z_edge;
                let corner = x_edge && z_edge;
                let edge_only = edge && !corner;
                if !corner && (!edge_only || (self.extra_edge != 0.0 && !(rng.next_float() > self.extra_edge))) {
                    let mut pos = origin.offset(dx, 0, dz);
                    let mut offset = 0;
                    while blocks.is_air(level.get(pos.x, pos.y, pos.z)) && offset < self.vertical_range {
                        pos = pos.relative(inwards);
                        offset += 1;
                    }
                    let mut k = 0;
                    while !blocks.is_air(level.get(pos.x, pos.y, pos.z)) && k < self.vertical_range {
                        pos = pos.relative(outwards);
                        k += 1;
                    }
                    let mut below = pos.relative(self.toward_ground);
                    let below_state = level.get(below.x, below.y, below.z);
                    if blocks.is_air(level.get(pos.x, pos.y, pos.z)) && blocks.face_sturdy(below_state, outwards, Support::Full) {
                        let depth = self.depth.sample(rng)
                            + i32::from(self.extra_bottom > 0.0 && rng.next_float() < self.extra_bottom);
                        let ground_pos = below;
                        if self.place_ground(level, rng, &mut below, depth) {
                            surface.insert(ground_pos);
                        }
                    }
                }
            }
        }
        if self.waterlogged {
            return self.flood(level, surface);
        }
        surface
    }

    /// The cells of the patch with no open side or open bottom become water.
    fn flood(&self, level: &mut Level<'_>, surface: JavaSet) -> JavaSet {
        let env = level.env;
        let blocks = &env.blocks;
        let mut water_surface = JavaSet::new();
        for p in surface.iter() {
            let exposed = [Dir::North, Dir::East, Dir::South, Dir::West, Dir::Down].iter().any(|d| {
                let n = p.relative(*d);
                !blocks.face_sturdy(level.get(n.x, n.y, n.z), d.opposite(), Support::Full)
            });
            if !exposed {
                water_surface.insert(p);
            }
        }
        let water = blocks.default_state(blocks.block_by_name("water").expect("water"));
        for p in water_surface.iter() {
            level.set(p.x, p.y, p.z, water);
        }
        water_surface
    }

    fn plant(&self, level: &mut Level<'_>, rng: &mut Rng, at: Pos) -> bool {
        let outwards = self.toward_ground.opposite();
        if !self.waterlogged {
            return self.vegetation.place_nested(level, rng, at.relative(outwards));
        }
        if !self.vegetation.place_nested(level, rng, at.below().relative(outwards)) {
            return false;
        }
        let blocks = &level.env.blocks;
        let placed = level.get(at.x, at.y, at.z);
        if blocks.has_property(placed, "waterlogged") && blocks.get(placed, "waterlogged") == Some("false") {
            let wet = blocks.with(placed, "waterlogged", "true").expect("waterlogged");
            level.set(at.x, at.y, at.z, wet);
        }
        true
    }
}

pub fn place(cfg: &PatchConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let xr = cfg.xz_radius.sample(rng) + 1;
    let zr = cfg.xz_radius.sample(rng) + 1;
    let surface = cfg.ground_patch(level, rng, origin, xr, zr);
    let cells: Vec<Pos> = surface.iter().collect();
    for p in &cells {
        if cfg.vegetation_chance > 0.0 && rng.next_float() < cfg.vegetation_chance {
            cfg.plant(level, rng, *p);
        }
    }
    !surface.is_empty()
}
