//! Single blocks and block columns: `simple_block` and `block_column`.

use serde_json::Value;

use crate::blocks::{Dir, FluidKind, State};
use crate::env::Env;
use crate::json::{Res, array, boolean, get, string};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::provider::IntProvider;
use crate::stateprovider::StateProvider;
use crate::survive::{Kind, can_survive};

#[derive(Clone, Debug)]
pub struct SimpleBlockConfig {
    pub to_place: StateProvider,
}

impl SimpleBlockConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let _ = boolean;
        Ok(Self { to_place: StateProvider::parse(env, get(v, "to_place", ctx)?, ctx)? })
    }
}

/// A waterlogged-capable state takes the waterlogged flag of the water present at the position.
fn copy_waterlogged(level: &Level<'_>, state: State, x: i32, y: i32, z: i32) -> State {
    let blocks = &level.env.blocks;
    if blocks.has_property(state, "waterlogged") {
        let water = blocks.fluid(level.get(x, y, z)) == FluidKind::Water;
        return blocks.with(state, "waterlogged", if water { "true" } else { "false" }).expect("waterlogged is boolean");
    }
    state
}

pub fn place_simple_block(cfg: &SimpleBlockConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let Some(state) = cfg.to_place.get_optional(level, rng, origin.x, origin.y, origin.z) else {
        return false;
    };
    if !can_survive(level, state, origin.x, origin.y, origin.z) {
        return false;
    }
    let block = env.blocks.block_of(state);
    if matches!(env.survive.kind(block), Kind::DoubleVegetation | Kind::TallSeagrass) {
        let above = level.get(origin.x, origin.y + 1, origin.z);
        if !env.blocks.is_air(above) && (env.blocks.fluid(above) != env.blocks.fluid(state) || !env.blocks.replaceable(above)) {
            return false;
        }
        let lower = env.blocks.with(state, "half", "lower").expect("double plant half");
        let upper = env.blocks.with(state, "half", "upper").expect("double plant half");
        let (lo, up) = (copy_waterlogged(level, lower, origin.x, origin.y, origin.z), copy_waterlogged(level, upper, origin.x, origin.y + 1, origin.z));
        level.set(origin.x, origin.y, origin.z, lo);
        level.set(origin.x, origin.y + 1, origin.z, up);
    } else {
        level.set(origin.x, origin.y, origin.z, state);
    }
    true
}

#[derive(Clone, Debug)]
pub struct BlockColumnConfig {
    pub layers: Vec<(IntProvider, StateProvider)>,
    pub direction: Dir,
    pub allowed_placement: BlockPred,
    pub prioritize_tip: bool,
}

impl BlockColumnConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let mut layers = Vec::new();
        for l in array(v, "layers", ctx)? {
            layers.push((IntProvider::parse(get(l, "height", ctx)?, ctx)?, StateProvider::parse(env, get(l, "provider", ctx)?, ctx)?));
        }
        Ok(Self {
            layers,
            direction: Dir::from_name(string(v, "direction", ctx)?).ok_or_else(|| format!("{ctx}: bad direction"))?,
            allowed_placement: BlockPred::parse(env, get(v, "allowed_placement", ctx)?, ctx)?,
            prioritize_tip: boolean(v, "prioritize_tip", false, ctx)?,
        })
    }
}

fn truncate(heights: &mut [i32], total: i32, new_height: i32, prioritize_tip: bool) {
    let mut remove = total - new_height;
    let order: Vec<usize> = if prioritize_tip { (0..heights.len()).collect() } else { (0..heights.len()).rev().collect() };
    for i in order {
        if remove <= 0 {
            break;
        }
        let take = heights[i].min(remove);
        remove -= take;
        heights[i] -= take;
    }
}

pub fn place_block_column(cfg: &BlockColumnConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let mut heights: Vec<i32> = Vec::with_capacity(cfg.layers.len());
    let mut total = 0;
    for (h, _) in &cfg.layers {
        let n = h.sample(rng);
        heights.push(n);
        total += n;
    }
    if total == 0 {
        return false;
    }
    let mut next = origin.relative(cfg.direction);
    for y in 0..total {
        if !cfg.allowed_placement.test(level, next.x, next.y, next.z) {
            truncate(&mut heights, total, y, cfg.prioritize_tip);
            break;
        }
        next = next.relative(cfg.direction);
    }
    let mut at = origin;
    for (i, (_, provider)) in cfg.layers.iter().enumerate() {
        for _ in 0..heights[i] {
            let s = provider.get(level, rng, at.x, at.y, at.z);
            level.set(at.x, at.y, at.z, s);
            at = at.relative(cfg.direction);
        }
    }
    true
}
