//! `geode`: concentric shells (filling, inner, middle, outer) around a few random points, cut by
//! a crack, with crystals budding on the inner layer.
//!
//! A cell's shell is decided by a sum of inverse distances to the points, perturbed by a
//! seeded noise; the noise is rebuilt from the world seed on every placement. The sums are
//! `f64` and the noise returns `f32`, as in the reference; the sampling order over the cuboid
//! (x fastest, then y, then z) decides the draw order.

use lodestone_worldgen_core::engine::release26_3::noise::{NormalNoise, parity_params};
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};
use serde_json::Value;

use super::tree::block_list;
use crate::blocks::{BlockId, Dir, State};
use crate::env::Env;
use crate::json::{Res, array, boolean, get, int_or, obj};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::provider::{IntProvider, double_or};
use crate::stateprovider::StateProvider;

#[derive(Clone, Debug)]
pub struct GeodeConfig {
    filling: StateProvider,
    inner: StateProvider,
    alternate: StateProvider,
    middle: StateProvider,
    outer: StateProvider,
    placements: Vec<State>,
    cannot_replace: Vec<BlockId>,
    invalid: Vec<BlockId>,
    filling_size: f64,
    inner_size: f64,
    middle_size: f64,
    outer_size: f64,
    crack_chance: f64,
    crack_base: f64,
    crack_point_offset: i32,
    potential_chance: f64,
    alternate_chance: f64,
    require_alternate: bool,
    outer_wall: IntProvider,
    points: IntProvider,
    point_offset: IntProvider,
    min_offset: i32,
    max_offset: i32,
    noise_multiplier: f64,
    invalid_threshold: i32,
}

impl GeodeConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let blocks = get(v, "blocks", ctx)?;
        let layers = get(v, "layers", ctx)?;
        let crack = get(v, "crack", ctx)?;
        let provider = |key: &str| StateProvider::parse(env, get(blocks, key, ctx)?, ctx);
        let placements = array(blocks, "inner_placements", ctx)?
            .iter()
            .map(|p| env.blocks.parse_state(p).map_err(|e| format!("{ctx}: {e}")))
            .collect::<Res<Vec<_>>>()?;
        let int_provider = |key: &str, lo: i32, hi: i32| match v.get(key) {
            Some(p) => IntProvider::parse(p, ctx),
            None => Ok(IntProvider::Uniform(lo, hi)),
        };
        obj(layers, ctx)?;
        Ok(Self {
            filling: provider("filling_provider")?,
            inner: provider("inner_layer_provider")?,
            alternate: provider("alternate_inner_layer_provider")?,
            middle: provider("middle_layer_provider")?,
            outer: provider("outer_layer_provider")?,
            placements,
            cannot_replace: block_list(env, get(blocks, "cannot_replace", ctx)?, ctx)?,
            invalid: block_list(env, get(blocks, "invalid_blocks", ctx)?, ctx)?,
            filling_size: double_or(layers, "filling", 1.7, ctx)?,
            inner_size: double_or(layers, "inner_layer", 2.2, ctx)?,
            middle_size: double_or(layers, "middle_layer", 3.2, ctx)?,
            outer_size: double_or(layers, "outer_layer", 4.2, ctx)?,
            crack_chance: double_or(crack, "generate_crack_chance", 1.0, ctx)?,
            crack_base: double_or(crack, "base_crack_size", 2.0, ctx)?,
            crack_point_offset: int_or(crack, "crack_point_offset", 2, ctx)?,
            potential_chance: double_or(v, "use_potential_placements_chance", 0.35, ctx)?,
            alternate_chance: double_or(v, "use_alternate_layer0_chance", 0.0, ctx)?,
            require_alternate: boolean(v, "placements_require_layer0_alternate", true, ctx)?,
            outer_wall: int_provider("outer_wall_distance", 4, 5)?,
            points: int_provider("distribution_points", 3, 4)?,
            point_offset: int_provider("point_offset", 1, 2)?,
            min_offset: int_or(v, "min_gen_offset", -16, ctx)?,
            max_offset: int_or(v, "max_gen_offset", 16, ctx)?,
            noise_multiplier: double_or(v, "noise_multiplier", 0.05, ctx)?,
            invalid_threshold: crate::json::int(v, "invalid_blocks_threshold", ctx)?,
        })
    }

    /// Every state the feature can write (for survival-rule gap detection).
    pub fn states(&self, out: &mut Vec<State>) {
        for p in [&self.filling, &self.inner, &self.alternate, &self.middle, &self.outer] {
            p.states(out);
        }
        out.extend(&self.placements);
    }
}

fn safe_set(level: &mut Level<'_>, cfg: &GeodeConfig, p: Pos, s: State) {
    let b = level.env.blocks.block_of(level.get(p.x, p.y, p.z));
    if !cfg.cannot_replace.contains(&b) {
        level.set(p.x, p.y, p.z, s);
    }
}

fn can_cluster_grow(level: &Level<'_>, s: State) -> bool {
    let blocks = &level.env.blocks;
    blocks.is_air(s) || (blocks.block_by_name("water") == Some(blocks.block_of(s)) && blocks.fluid_amount(s) == 8)
}

fn inv_sqrt(x: f64) -> f64 {
    1.0 / x.sqrt()
}

pub fn place(cfg: &GeodeConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let num_points = cfg.points.sample(rng);
    let noise = {
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(level.seed));
        NormalNoise::new(parity_params(-4, &[1.0])).create(&mut random)
    };
    let adjustment = f64::from(num_points) / f64::from(cfg.outer_wall.max_inclusive());
    let inner_air = 1.0 / cfg.filling_size.sqrt();
    let innermost = 1.0 / (cfg.inner_size + adjustment).sqrt();
    let inner_crust = 1.0 / (cfg.middle_size + adjustment).sqrt();
    let outer_crust = 1.0 / (cfg.outer_size + adjustment).sqrt();
    let crack_size = 1.0 / (cfg.crack_base + rng.next_double() / 2.0 + if num_points > 3 { adjustment } else { 0.0 }).sqrt();
    let generate_crack = f64::from(rng.next_float()) < cfg.crack_chance;
    let mut invalid_points = 0;
    let mut points: Vec<(Pos, i32)> = Vec::new();
    for _ in 0..num_points {
        let x = cfg.outer_wall.sample(rng);
        let y = cfg.outer_wall.sample(rng);
        let z = cfg.outer_wall.sample(rng);
        let p = origin.offset(x, y, z);
        let s = level.get(p.x, p.y, p.z);
        if level.env.blocks.is_air(s) || cfg.invalid.contains(&level.env.blocks.block_of(s)) {
            invalid_points += 1;
            if invalid_points > cfg.invalid_threshold {
                return false;
            }
        }
        points.push((p, cfg.point_offset.sample(rng)));
    }
    let mut cracks: Vec<Pos> = Vec::new();
    if generate_crack {
        let index = rng.next_int_bounded(4);
        let c = num_points * 2 + 1;
        let (dx, dz) = match index {
            0 => (c, 0),
            1 => (0, c),
            2 => (c, c),
            _ => (0, 0),
        };
        for dy in [7, 5, 1] {
            cracks.push(origin.offset(dx, dy, dz));
        }
    }
    let air = {
        let b = &level.env.blocks;
        b.default_state(b.block_by_name("air").expect("air"))
    };
    let mut potential: Vec<Pos> = Vec::new();
    let (lo, hi) = (cfg.min_offset, cfg.max_offset);
    let width = hi - lo + 1;
    for index in 0..width * width * width {
        let p = origin.offset(lo + index % width, lo + index / width % width, lo + index / (width * width));
        let offset = f64::from(noise.get(f64::from(p.x), f64::from(p.y), f64::from(p.z))) * cfg.noise_multiplier;
        let mut shell = 0.0f64;
        for (q, o) in &points {
            let d = dist_sq(p, *q);
            shell += inv_sqrt(d + f64::from(*o)) + offset;
        }
        if shell < outer_crust {
            continue;
        }
        if shell >= inner_air {
            let s = cfg.filling.get(level, rng, p.x, p.y, p.z);
            safe_set(level, cfg, p, s);
            continue;
        }
        let mut crack_sum = 0.0f64;
        for q in &cracks {
            crack_sum += inv_sqrt(dist_sq(p, *q) + f64::from(cfg.crack_point_offset)) + offset;
        }
        if generate_crack && crack_sum >= crack_size {
            safe_set(level, cfg, p, air);
        } else if shell >= innermost {
            let use_alternate = f64::from(rng.next_float()) < cfg.alternate_chance;
            let provider = if use_alternate { &cfg.alternate } else { &cfg.inner };
            let s = provider.get(level, rng, p.x, p.y, p.z);
            safe_set(level, cfg, p, s);
            if (!cfg.require_alternate || use_alternate) && f64::from(rng.next_float()) < cfg.potential_chance {
                potential.push(p);
            }
        } else if shell >= inner_crust {
            let s = cfg.middle.get(level, rng, p.x, p.y, p.z);
            safe_set(level, cfg, p, s);
        } else if shell >= outer_crust {
            let s = cfg.outer.get(level, rng, p.x, p.y, p.z);
            safe_set(level, cfg, p, s);
        }
    }
    for crystal in potential {
        let mut state = cfg.placements[rng.next_int_bounded(cfg.placements.len() as i32) as usize];
        for d in Dir::ALL {
            let blocks = &level.env.blocks;
            if blocks.has_property(state, "facing") {
                state = blocks.with(state, "facing", d.name()).expect("facing");
            }
            let at = crystal.relative(d);
            let here = level.get(at.x, at.y, at.z);
            let blocks = &level.env.blocks;
            if blocks.has_property(state, "waterlogged") {
                let source = blocks.fluid(here) == crate::blocks::FluidKind::Water && blocks.fluid_is_source(here);
                state = blocks.with(state, "waterlogged", if source { "true" } else { "false" }).expect("waterlogged");
            }
            if can_cluster_grow(level, here) {
                safe_set(level, cfg, at, state);
                break;
            }
        }
    }
    true
}

fn dist_sq(a: Pos, b: Pos) -> f64 {
    let (dx, dy, dz) = (f64::from(a.x - b.x), f64::from(a.y - b.y), f64::from(a.z - b.z));
    dx * dx + dy * dy + dz * dz
}
