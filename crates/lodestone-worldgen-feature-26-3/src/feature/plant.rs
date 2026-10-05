//! Single blocks and block columns: `simple_block` and `block_column`.

use lodestone_worldgen_core::rng::RandomSource as _;
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
    if matches!(env.survive.kind(block), Kind::DoubleVegetation | Kind::TallSeagrass | Kind::SmallDripleaf) {
        let above = level.get(origin.x, origin.y + 1, origin.z);
        if !env.blocks.is_air(above) && (env.blocks.fluid(above) != env.blocks.fluid(state) || !env.blocks.replaceable(above)) {
            return false;
        }
        let lower = env.blocks.with(state, "half", "lower").expect("double plant half");
        let upper = env.blocks.with(state, "half", "upper").expect("double plant half");
        let (lo, up) = (copy_waterlogged(level, lower, origin.x, origin.y, origin.z), copy_waterlogged(level, upper, origin.x, origin.y + 1, origin.z));
        level.set(origin.x, origin.y, origin.z, lo);
        level.set(origin.x, origin.y + 1, origin.z, up);
    } else if env.blocks.block_name(block).ends_with("pale_moss_carpet") {
        place_mossy_carpet(level, origin);
    } else {
        level.set(origin.x, origin.y, origin.z, state);
    }
    true
}

const WALL_SIDES: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

/// Recomputes the four wall sides of a pale moss carpet from what each side can attach to.
pub(super) fn carpet_state(level: &Level<'_>, state: State, pos: Pos, create_sides: bool) -> State {
    let blocks = &level.env.blocks;
    let carpet = blocks.block_by_name("pale_moss_carpet").expect("pale_moss_carpet");
    let base = blocks.get(state, "bottom") == Some("true");
    let create_sides = create_sides || base;
    let mut state = state;
    for d in WALL_SIDES {
        let face = d.name();
        let mut side = if super::misc::can_attach(level, pos, d) {
            if create_sides { "low" } else { blocks.get(state, face).expect("side") }
        } else {
            "none"
        };
        if side == "low" {
            let above = level.get(pos.x, pos.y + 1, pos.z);
            if blocks.block_of(above) == carpet && blocks.get(above, face) != Some("none") && blocks.get(above, "bottom") != Some("true") {
                side = "tall";
            }
            if !base {
                let below = level.get(pos.x, pos.y - 1, pos.z);
                if blocks.block_of(below) == carpet && blocks.get(below, face) == Some("none") {
                    side = "none";
                }
            }
        }
        state = blocks.with(state, face, side).expect("side");
    }
    state
}

pub(super) fn carpet_has_faces(level: &Level<'_>, state: State) -> bool {
    let blocks = &level.env.blocks;
    blocks.get(state, "bottom") == Some("true") || WALL_SIDES.iter().any(|d| blocks.get(state, d.name()) != Some("none"))
}

/// A carpet base, and sometimes a thinner carpet on top whose sides survive coin flips drawn
/// from the region's own random source.
fn place_mossy_carpet(level: &mut Level<'_>, pos: Pos) {
    let env = level.env;
    let blocks = &env.blocks;
    let carpet = blocks.block_by_name("pale_moss_carpet").expect("pale_moss_carpet");
    let air = blocks.default_state(blocks.block_by_name("air").expect("air"));
    let simple = blocks.default_state(carpet);
    let adjusted = carpet_state(level, simple, pos, true);
    level.set(pos.x, pos.y, pos.z, adjusted);
    let above = pos.above();
    let previous = level.get(above.x, above.y, above.z);
    let mossy = blocks.block_of(previous) == carpet;
    let topper = if (!mossy || blocks.get(previous, "bottom") != Some("true")) && (mossy || blocks.replaceable(previous)) {
        let no_base = blocks.with(simple, "bottom", "false").expect("bottom");
        let mut s = carpet_state(level, no_base, above, true);
        for d in WALL_SIDES {
            if blocks.get(s, d.name()) != Some("none") && !level.region_rng.next_bool() {
                s = blocks.with(s, d.name(), "none").expect("side");
            }
        }
        if carpet_has_faces(level, s) && s != previous { s } else { air }
    } else {
        air
    };
    if !blocks.is_air(topper) {
        level.set(above.x, above.y, above.z, topper);
        let again = carpet_state(level, adjusted, pos, true);
        level.set(pos.x, pos.y, pos.z, again);
    }
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
