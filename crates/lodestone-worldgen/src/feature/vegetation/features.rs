//! The decoration feature types beyond the original seven.
//!
//! ## What it is
//!
//! One placement body per `Feature<…>` subclass this engine models, dispatched from
//! [`super::place_configured_feature`] exactly like `simple_block`/`tree`/`block_column`
//! already are. Everything here writes through [`VegGrid`], so a feature reached from
//! any decoration step (not just `VEGETAL_DECORATION`) shares one region, one overlay
//! and one RNG stream with the rest of the pass.
//!
//! ## How it works
//!
//! Each `place_*` reproduces its vanilla `place()` body's **RNG draw order**, because
//! that is what decides every *later* feature in the same step
//! (`set_feature_seed(seed, index, step)` isolates features from each other but not
//! placements within one). Where a vanilla check needs world state this engine does not
//! carry, the check is narrowed in a **named** way rather than dropped:
//!
//! | vanilla concept | here | why |
//! |---|---|---|
//! | `isFaceSturdy(UP)` | "not air, not a fluid, and [`blocks_motion`]" | no per-state occlusion table in this crate |
//! | `state.isSolid()` | same | same |
//! | `canSurvive` | the target's own family rule, or "support below is not air" | vegetation blocks use `#supports_vegetation`; support-free blocks such as potent sulfur opt out |
//! | `level.getSeaLevel()` | [`SEA_LEVEL`] | the overworld constant; a preset that moves it would need this parameterised |
//! | `scheduleTick` | dropped | there is no tick queue at generation time; the *block* still lands |
//! | block entities | dropped | the generator has no block-entity layer yet |
//!
//! The narrowings make some features place slightly more than vanilla would (a
//! non-full block reads as sturdy). That direction is deliberate: the alternative
//! reading, "sturdy only if in a hardcoded list", silently produced *nothing* for
//! whole biomes, which is the failure mode this issue exists to close.
//!
//! ## How to change it
//!
//! Adding a type is three edits: a [`super::config::ConfiguredFeature`] variant, a
//! `parse_configured_feature_doc` arm, and a body here. **Do not** delete a variant to
//! "simplify" — an entry removed from a biome's step list shifts every later
//! `set_feature_seed` index; [`super::config::ConfiguredFeature::Unsupported`] exists
//! precisely so an unmodelled type stays in the list and stays inert.
//!
//! The `_ =>` in `parse_configured_feature_doc` is the island factory here: a variant
//! added without an arm parses to `Unsupported` and is silently never reached.

use std::collections::HashSet;

use crate::feature::{BlockPos, IntProvider};
use crate::interner::StateId;
use crate::rng::RandomSource;

use super::config::{
    BlockPredicate, BlockStateProvider, Decorator, PlacedRef, VegTags, blocks_motion, is_air, is_fluid,
};
use super::grid::VegGrid;
use super::ids::{Rewrite, Tag, tag_at};
use super::place::{place_attached_to_logs_decorator, place_trunk_vine_decorator};
use super::tree::valid_tree_pos;

/// `level.getSeaLevel()` for the overworld. Only [`place_blue_ice`] reads it.
pub const SEA_LEVEL: i32 = 63;

/// Base id of the state at `pos`, without its property list.
fn base_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> &str {
    super::base_id(grid.get(x, y, z))
}

fn air_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    is_air(base_at(grid, x, y, z))
}

/// `state.isSolid()` / `isFaceSturdy` — narrowed to "occupied by something that is
/// not a fluid and does block motion". See this module's doc table.
///
/// The motion test is the same fix as [`VegGrid::height_ocean_floor`]'s and matters
/// for the same reason: without it an already-placed seagrass reads as a sturdy
/// support, so [`place_seagrass`]'s `canSurvive` stand-in would let a second plant
/// stack on the first even once the heightmap stopped pointing there.
fn sturdy_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    let base = base_at(grid, x, y, z);
    !is_air(base) && !is_fluid(base) && blocks_motion(base)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimpleBlockSurvival {
    Vegetation,
    TagBelow(Tag),
    Mushroom,
    SmallDripleaf,
    LilyPad,
    CeilingCenter,
    SturdyBelow,
    NonAirBelow,
    Fire,
    Always,
}

/// The exceptional states from the bundled `simple_block` records. Every other
/// bundled output is an ordinary vegetation block, so the fallback remains
/// explicit instead of turning an unrecognised new state into an unconditional
/// placement.
const SIMPLE_BLOCK_SURVIVAL: &[(&str, SimpleBlockSurvival)] = &[
    ("minecraft:dead_bush", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    ("minecraft:short_dry_grass", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    ("minecraft:tall_dry_grass", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    ("minecraft:azalea", SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
    ("minecraft:flowering_azalea", SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
    ("minecraft:crimson_roots", SimpleBlockSurvival::TagBelow(Tag::SupportsCrimsonRoots)),
    ("minecraft:brown_mushroom", SimpleBlockSurvival::Mushroom),
    ("minecraft:red_mushroom", SimpleBlockSurvival::Mushroom),
    ("minecraft:small_dripleaf", SimpleBlockSurvival::SmallDripleaf),
    ("minecraft:lily_pad", SimpleBlockSurvival::LilyPad),
    ("minecraft:spore_blossom", SimpleBlockSurvival::CeilingCenter),
    ("minecraft:leaf_litter", SimpleBlockSurvival::SturdyBelow),
    ("minecraft:moss_carpet", SimpleBlockSurvival::NonAirBelow),
    ("minecraft:pale_moss_carpet", SimpleBlockSurvival::NonAirBelow),
    ("minecraft:fire", SimpleBlockSurvival::Fire),
    ("minecraft:soul_fire", SimpleBlockSurvival::TagBelow(Tag::SoulFireBaseBlocks)),
    ("minecraft:melon", SimpleBlockSurvival::Always),
    ("minecraft:pumpkin", SimpleBlockSurvival::Always),
    ("minecraft:tuff", SimpleBlockSurvival::Always),
    ("minecraft:potent_sulfur", SimpleBlockSurvival::Always),
];

fn simple_block_survival_rule(base: &str) -> SimpleBlockSurvival {
    SIMPLE_BLOCK_SURVIVAL
        .iter()
        .find_map(|&(name, rule)| (name == base).then_some(rule))
        .unwrap_or(SimpleBlockSurvival::Vegetation)
}

/// The simple-block feature asks the placed state's own survival rule. During
/// decoration the reference raw-brightness query is zero (lighting has not yet
/// been initialised), which makes the mushroom light arm unconditional and
/// leaves only its floor rule here.
pub(super) fn simple_block_can_survive(
    grid: &VegGrid,
    tags: &super::config::VegTags,
    state: StateId,
    pos: BlockPos,
) -> bool {
    let base = super::base_id(grid.interner().name_of(state));
    let rule = simple_block_survival_rule(base);
    match rule {
        SimpleBlockSurvival::Vegetation => tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::TagBelow(tag) => tag_at(grid, tags, tag, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::Mushroom => {
            tag_at(grid, tags, Tag::OverridesMushroomLightRequirement, pos.x, pos.y - 1, pos.z)
                || tags.simple_block_support.solid_render.test(grid.interner().name_of(grid.get_id(pos.x, pos.y - 1, pos.z)))
        }
        SimpleBlockSurvival::SmallDripleaf => {
            tag_at(grid, tags, Tag::SupportsSmallDripleaf, pos.x, pos.y - 1, pos.z)
                || (water_at(grid, pos.x, pos.y, pos.z)
                    && tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z))
        }
        SimpleBlockSurvival::LilyPad => {
            !is_fluid(base_at(grid, pos.x, pos.y, pos.z))
                && (water_at(grid, pos.x, pos.y - 1, pos.z)
                    || tag_at(grid, tags, Tag::SupportsLilyPad, pos.x, pos.y - 1, pos.z))
        }
        SimpleBlockSurvival::CeilingCenter => {
            !water_at(grid, pos.x, pos.y, pos.z)
                && tags.simple_block_support.center_support_down.test(grid.interner().name_of(grid.get_id(pos.x, pos.y + 1, pos.z)))
        }
        SimpleBlockSurvival::SturdyBelow => tags.simple_block_support.sturdy_up.test(grid.interner().name_of(grid.get_id(pos.x, pos.y - 1, pos.z))),
        SimpleBlockSurvival::NonAirBelow => !air_at(grid, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::Fire => {
            tags.simple_block_support.sturdy_up.test(grid.interner().name_of(grid.get_id(pos.x, pos.y - 1, pos.z)))
                || [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)]
                    .iter()
                    .any(|&(dx, dy, dz)| fire_fuel_at(grid, tags, pos.x + dx, pos.y + dy, pos.z + dz))
        }
        SimpleBlockSurvival::Always => true,
    }
}

/// Exact per-state fire capability supplied by the version boundary.
fn fire_fuel_at(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    tags.simple_block_support.fire_flammable.test(grid.interner().name_of(grid.get_id(x, y, z)))
}

fn water_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    base_at(grid, x, y, z) == "minecraft:water"
}

fn speleothem_base_at(
    grid: &VegGrid,
    cfg: &SpeleothemCfg,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    let state = base_at(grid, x, y, z);
    state == cfg.base_block || cfg.replaceable_blocks.contains(state)
}

fn place_speleothem_base_if_possible(
    grid: &mut VegGrid,
    cfg: &SpeleothemCfg,
    x: i32,
    y: i32,
    z: i32,
) {
    if cfg.replaceable_blocks.contains(base_at(grid, x, y, z)) {
        grid.set_if_in_bounds(x, y, z, cfg.base_block.clone());
    }
}

fn pointed_speleothem_state(
    cfg: &SpeleothemCfg,
    dy: i32,
    thickness: &str,
    waterlogged: bool,
) -> String {
    let direction = if dy > 0 { "up" } else { "down" };
    format!(
        "{}[thickness={thickness},vertical_direction={direction},waterlogged={waterlogged}]",
        cfg.pointed_block
    )
}

/// Places one configured speleothem and its small patch of anchor material.
///
/// The branch and draw ordering deliberately follows the configured feature:
/// choose a direction only when both anchors qualify, expand every horizontal
/// arm independently, then decide whether the point has a second segment.
/// The radius-two and radius-three direction draws occur even when the target
/// cannot be replaced; they belong to placement order, not to successful
/// writes.
pub fn place_speleothem<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &SpeleothemCfg,
    grid: &mut VegGrid,
) {
    let above = speleothem_base_at(grid, cfg, pos.x, pos.y + 1, pos.z);
    let below = speleothem_base_at(grid, cfg, pos.x, pos.y - 1, pos.z);
    let Some(dy) = (match (above, below) {
        (true, true) => Some(if random.next_bool() { -1 } else { 1 }),
        (true, false) => Some(-1),
        (false, true) => Some(1),
        (false, false) => None,
    }) else {
        return;
    };

    let root = BlockPos {
        x: pos.x,
        y: pos.y - dy,
        z: pos.z,
    };
    place_speleothem_base_if_possible(grid, cfg, root.x, root.y, root.z);
    for (dx, dz) in HORIZONTAL {
        if random.next_float() > cfg.chance_of_directional_spread {
            continue;
        }
        let first = BlockPos {
            x: root.x + dx,
            y: root.y,
            z: root.z + dz,
        };
        place_speleothem_base_if_possible(grid, cfg, first.x, first.y, first.z);
        if random.next_float() > cfg.chance_of_spread_radius2 {
            continue;
        }
        let (dx2, dy2, dz2) =
            DIRECTIONS[random.next_int_bounded(DIRECTIONS.len() as i32) as usize];
        let second = BlockPos {
            x: first.x + dx2,
            y: first.y + dy2,
            z: first.z + dz2,
        };
        place_speleothem_base_if_possible(grid, cfg, second.x, second.y, second.z);
        if random.next_float() > cfg.chance_of_spread_radius3 {
            continue;
        }
        let (dx3, dy3, dz3) =
            DIRECTIONS[random.next_int_bounded(DIRECTIONS.len() as i32) as usize];
        place_speleothem_base_if_possible(
            grid,
            cfg,
            second.x + dx3,
            second.y + dy3,
            second.z + dz3,
        );
    }

    let height = if random.next_float() < cfg.chance_of_taller_generation
        && (air_at(grid, pos.x, pos.y + dy, pos.z) || water_at(grid, pos.x, pos.y + dy, pos.z))
    {
        2
    } else {
        1
    };
    // The patch may have changed the root from a replaceable state into the
    // base state. Keep the anchor test after that write, as the feature does.
    if !speleothem_base_at(grid, cfg, root.x, root.y, root.z) {
        return;
    }
    for segment in 0..height {
        let thickness = match (height, segment) {
            (1, _) => "tip",
            (2, 0) => "frustum",
            (_, 0) => "base",
            (_, segment) if segment == height - 1 => "tip",
            _ => "middle",
        };
        let at = BlockPos {
            x: pos.x,
            y: pos.y + dy * segment,
            z: pos.z,
        };
        grid.set_if_in_bounds(
            at.x,
            at.y,
            at.z,
            pointed_speleothem_state(cfg, dy, thickness, water_at(grid, at.x, at.y, at.z)),
        );
    }
}

#[derive(Clone, Debug)]
pub enum FloatProvider {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: f32,
        max: f32,
    },
}

impl FloatProvider {
    pub(super) fn try_parse(v: &serde_json::Value) -> Option<Self> {
        match v {
            serde_json::Value::Number(n) => Some(Self::Constant(n.as_f64()? as f32)),
            serde_json::Value::Object(_) => match v["type"]
                .as_str()?
                .strip_prefix("minecraft:")
                .unwrap_or(v["type"].as_str()?)
            {
                "constant" => Some(Self::Constant(v["value"].as_f64()? as f32)),
                "uniform" => Some(Self::Uniform {
                    min: v["min_inclusive"].as_f64()? as f32,
                    max: v["max_exclusive"].as_f64()? as f32,
                }),
                "clamped_normal" => Some(Self::ClampedNormal {
                    mean: v["mean"].as_f64()? as f32,
                    deviation: v["deviation"].as_f64()? as f32,
                    min: v["min"].as_f64()? as f32,
                    max: v["max"].as_f64()? as f32,
                }),
                _ => None,
            },
            _ => None,
        }
    }

    fn sample<R: RandomSource>(&self, random: &mut R) -> f32 {
        match self {
            Self::Constant(value) => *value,
            Self::Uniform { min, max } => random.next_float() * (max - min) + min,
            Self::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => (*mean + random.next_gaussian() as f32 * *deviation).clamp(*min, *max),
        }
    }
}

/// Configuration for the broad cave cluster form of a speleothem.
#[derive(Clone, Debug)]
pub struct SpeleothemClusterCfg {
    pub base_block: String,
    pub pointed_block: String,
    pub replaceable_blocks: HashSet<String>,
    pub floor_to_ceiling_search_range: i32,
    pub height: IntProvider,
    pub radius: IntProvider,
    pub max_stalagmite_stalactite_height_diff: i32,
    pub height_deviation: i32,
    pub speleothem_block_layer_thickness: IntProvider,
    pub density: FloatProvider,
    pub wetness: FloatProvider,
    pub chance_of_speleothem_at_max_distance_from_center: f32,
    pub max_distance_from_edge_affecting_chance_of_speleothem: i32,
    pub max_distance_from_center_affecting_height_bias: i32,
}

#[derive(Clone, Copy)]
struct SpeleothemColumn {
    floor: Option<i32>,
    ceiling: Option<i32>,
}

impl SpeleothemColumn {
    fn height(self) -> Option<i32> {
        Some(self.ceiling? - self.floor? - 1)
    }
}

fn empty_or_water_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    air_at(grid, x, y, z) || water_at(grid, x, y, z)
}

fn scan_speleothem_column(
    grid: &VegGrid,
    pos: BlockPos,
    search_range: i32,
) -> Option<SpeleothemColumn> {
    if !empty_or_water_at(grid, pos.x, pos.y, pos.z) {
        return None;
    }
    let scan = |dy: i32| {
        let mut y = pos.y;
        for _ in 1..search_range {
            if !empty_or_water_at(grid, pos.x, y, pos.z) {
                break;
            }
            y += dy;
        }
        (!empty_or_water_at(grid, pos.x, y, pos.z)).then_some(y)
    };
    Some(SpeleothemColumn {
        ceiling: scan(1),
        floor: scan(-1),
    })
}

fn replace_speleothem_base_layer(
    grid: &mut VegGrid,
    cfg: &SpeleothemClusterCfg,
    pos: BlockPos,
    count: i32,
    dy: i32,
) {
    for offset in 0..count {
        let y = pos.y + dy * offset;
        if !cfg.replaceable_blocks.contains(base_at(grid, pos.x, y, pos.z)) {
            return;
        }
        grid.set_if_in_bounds(pos.x, y, pos.z, cfg.base_block.clone());
    }
}

fn grow_cluster_speleothem(
    grid: &mut VegGrid,
    cfg: &SpeleothemClusterCfg,
    start: BlockPos,
    dy: i32,
    height: i32,
    merge_tips: bool,
) {
    let root_y = start.y - dy;
    let root = base_at(grid, start.x, root_y, start.z);
    if root != cfg.base_block && !cfg.replaceable_blocks.contains(root) {
        return;
    }
    for segment in 0..height {
        let thickness = match segment {
            value if value == height - 1 && merge_tips => "tip_merge",
            value if value == height - 1 => "tip",
            value if value == height - 2 => "frustum",
            0 => "base",
            _ => "middle",
        };
        let y = start.y + dy * segment;
        grid.set_if_in_bounds(
            start.x,
            y,
            start.z,
            pointed_speleothem_state_for(&cfg.pointed_block, dy, thickness, water_at(grid, start.x, y, start.z)),
        );
    }
}

fn pointed_speleothem_state_for(
    pointed_block: &str,
    dy: i32,
    thickness: &str,
    waterlogged: bool,
) -> String {
    let direction = if dy > 0 { "up" } else { "down" };
    format!("{pointed_block}[thickness={thickness},vertical_direction={direction},waterlogged={waterlogged}]")
}

fn cluster_height<R: RandomSource>(
    random: &mut R,
    dx: i32,
    dz: i32,
    density: f32,
    max_height: i32,
    cfg: &SpeleothemClusterCfg,
) -> i32 {
    if random.next_float() > density {
        return 0;
    }
    let distance = (dx.abs() + dz.abs()) as f32;
    let limit = cfg.max_distance_from_center_affecting_height_bias as f32;
    let t = (distance / limit).clamp(0.0, 1.0);
    let mean = max_height as f32 / 2.0 * (1.0 - t);
    (random.next_gaussian() as f32 * cfg.height_deviation as f32 + mean)
        .clamp(0.0, max_height as f32) as i32
}

/// Places a cave-wide cluster. The per-column scan and every random branch run
/// in rectangular x/z order; successful writes must not decide which later
/// columns consume random values.
pub fn place_speleothem_cluster<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &SpeleothemClusterCfg,
    grid: &mut VegGrid,
) {
    if !empty_or_water_at(grid, origin.x, origin.y, origin.z) {
        return;
    }
    let cluster_max_height = cfg.height.sample(random);
    let wetness = cfg.wetness.sample(random);
    let density = cfg.density.sample(random);
    let x_radius = cfg.radius.sample(random);
    let z_radius = cfg.radius.sample(random);
    for dx in -x_radius..=x_radius {
        for dz in -z_radius..=z_radius {
            let edge_distance = (x_radius - dx.abs()).min(z_radius - dz.abs()) as f32;
            let chance = cfg.chance_of_speleothem_at_max_distance_from_center
                + (1.0 - cfg.chance_of_speleothem_at_max_distance_from_center)
                    * (edge_distance / cfg.max_distance_from_edge_affecting_chance_of_speleothem as f32)
                        .clamp(0.0, 1.0);
            let pos = BlockPos { x: origin.x + dx, y: origin.y, z: origin.z + dz };
            let Some(column) = scan_speleothem_column(grid, pos, cfg.floor_to_ceiling_search_range) else {
                continue;
            };
            if column.floor.is_none() && column.ceiling.is_none() {
                continue;
            }
            // Sulfur has zero wetness, but the float draw still belongs to each
            // accepted column. The pool branch is intentionally absent until a
            // full base-stone tag predicate is available; it cannot run here.
            let _want_pool = random.next_float() < wetness;
            let want_stalactite = random.next_double() < chance as f64;
            let stalactite_height = if let Some(ceiling) = column.ceiling {
                if want_stalactite && base_at(grid, pos.x, ceiling, pos.z) != "minecraft:lava" {
                    let thickness = cfg.speleothem_block_layer_thickness.sample(random);
                    replace_speleothem_base_layer(grid, cfg, BlockPos { y: ceiling, ..pos }, thickness, 1);
                    let max = column.floor.map_or(cluster_max_height, |floor| cluster_max_height.min(ceiling - floor));
                    cluster_height(random, dx, dz, density, max, cfg)
                } else { 0 }
            } else { 0 };
            let want_stalagmite = random.next_double() < chance as f64;
            let stalagmite_height = if let Some(floor) = column.floor {
                if want_stalagmite && base_at(grid, pos.x, floor, pos.z) != "minecraft:lava" {
                    let thickness = cfg.speleothem_block_layer_thickness.sample(random);
                    replace_speleothem_base_layer(grid, cfg, BlockPos { y: floor, ..pos }, thickness, -1);
                    if column.ceiling.is_some() {
                        (stalactite_height + random.next_int_bounded(cfg.max_stalagmite_stalactite_height_diff * 2 + 1)
                            - cfg.max_stalagmite_stalactite_height_diff).max(0)
                    } else {
                        cluster_height(random, dx, dz, density, cluster_max_height, cfg)
                    }
                } else { 0 }
            } else { 0 };
            let (actual_stalactite, actual_stalagmite) = match (column.ceiling, column.floor) {
                (Some(ceiling), Some(floor)) if ceiling - stalactite_height <= floor + stalagmite_height => {
                    let low = (ceiling - stalactite_height).max(floor + 1);
                    let high = (floor + stalagmite_height).min(ceiling - 1) + 1;
                    let bottom = low + random.next_int_bounded(high - low + 1);
                    (ceiling - bottom, bottom - 1 - floor)
                }
                _ => (stalactite_height, stalagmite_height),
            };
            let merge_tips = random.next_bool()
                && actual_stalactite > 0
                && actual_stalagmite > 0
                && column.height().is_some_and(|height| actual_stalactite + actual_stalagmite == height);
            if let Some(ceiling) = column.ceiling {
                grow_cluster_speleothem(grid, cfg, BlockPos { y: ceiling - 1, ..pos }, -1, actual_stalactite, merge_tips);
            }
            if let Some(floor) = column.floor {
                grow_cluster_speleothem(grid, cfg, BlockPos { y: floor + 1, ..pos }, 1, actual_stalagmite, merge_tips);
            }
        }
    }
}

/// The six direction offsets, in vanilla's own declaration order
/// (DOWN, UP, NORTH, SOUTH, WEST, EAST) — several features below iterate
/// vanilla's own all-directions order and stop at the first hit, so the order is not cosmetic.
const DIRECTIONS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

/// `Direction.Plane.HORIZONTAL`, in vanilla order (NORTH, EAST, SOUTH, WEST).
const HORIZONTAL: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

// ---------------------------------------------------------------------------
// Configs
// ---------------------------------------------------------------------------

/// `SpringConfiguration`.
#[derive(Clone, Debug)]
pub struct SpringCfg {
    pub state: String,
    pub requires_block_below: bool,
    pub rock_count: i32,
    pub hole_count: i32,
    pub valid_blocks: HashSet<String>,
}

/// `DiskConfiguration`.
#[derive(Clone, Debug)]
pub struct DiskCfg {
    pub provider: BlockStateProvider,
    pub target: BlockPredicate,
    pub radius: IntProvider,
    pub half_height: i32,
}

/// `ReplaceSphereConfiguration` (`netherrack_replace_blobs`).
#[derive(Clone, Debug)]
pub struct ReplaceBlobsCfg {
    pub target: String,
    pub state: String,
    pub radius: IntProvider,
}

/// `BlockBlobConfiguration`.
#[derive(Clone, Debug)]
pub struct BlockBlobCfg {
    pub state: String,
    pub can_place_on: BlockPredicate,
}

/// `NetherForestVegetationConfig`, and `BlockPileConfiguration` when the two
/// spread fields are absent.
#[derive(Clone, Debug)]
pub struct NetherForestVegetationCfg {
    pub provider: BlockStateProvider,
    pub spread_width: i32,
    pub spread_height: i32,
}

/// `TwistingVinesConfig`.
#[derive(Clone, Copy, Debug)]
pub struct TwistingVinesCfg {
    pub spread_width: i32,
    pub spread_height: i32,
    pub max_height: i32,
}

/// `MultifaceGrowthConfiguration` (glow lichen, sculk vein).
#[derive(Clone, Debug)]
pub struct MultifaceGrowthCfg {
    pub block: String,
    pub search_range: i32,
    pub can_place_on_floor: bool,
    pub can_place_on_ceiling: bool,
    pub can_place_on_wall: bool,
    pub chance_of_spreading: f32,
    pub can_be_placed_on: HashSet<String>,
}

/// Configuration for a single floor- or ceiling-anchored speleothem.
///
/// The base and pointed states are kept as base ids because the placement body
/// derives the pointed block's direction, thickness, and waterlogged
/// properties for every segment. `replaceable_blocks` is the resolved block
/// tag from the configured record; it is also accepted as an existing anchor.
#[derive(Clone, Debug)]
pub struct SpeleothemCfg {
    pub base_block: String,
    pub pointed_block: String,
    pub replaceable_blocks: HashSet<String>,
    pub chance_of_taller_generation: f32,
    pub chance_of_directional_spread: f32,
    pub chance_of_spread_radius2: f32,
    pub chance_of_spread_radius3: f32,
}

/// `LakeFeature.Configuration`.
#[derive(Clone, Debug)]
pub struct LakeCfg {
    pub fluid: BlockStateProvider,
    pub barrier: BlockStateProvider,
    pub can_place_feature: BlockPredicate,
    pub can_replace_with_air_or_fluid: BlockPredicate,
    pub can_replace_with_barrier: BlockPredicate,
}

/// The shared huge-mushroom configuration. The two configured feature types
/// differ only in their cap geometry; both use the same height, clearance,
/// ground and stem rules.
#[derive(Clone, Debug)]
pub struct HugeMushroomCfg {
    pub can_place_on: BlockPredicate,
    pub cap_provider: BlockStateProvider,
    pub stem_provider: BlockStateProvider,
    pub foliage_radius: i32,
    pub kind: HugeMushroomKind,
}

/// The cap shape selected by the configured-feature type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HugeMushroomKind {
    Brown,
    Red,
}

/// `CaveSurface` — which way `vegetation_patch` grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaveSurface {
    Floor,
    Ceiling,
}

impl CaveSurface {
    /// `getDirection()` — the *inward* direction (towards the surface's own solid).
    fn dy(self) -> i32 {
        match self {
            // FLOOR's direction is DOWN, CEILING's is UP.
            CaveSurface::Floor => -1,
            CaveSurface::Ceiling => 1,
        }
    }
}

/// `VegetationPatchConfiguration`. `waterlogged` distinguishes
/// `WaterloggedVegetationPatchFeature` from its base class.
#[derive(Clone, Debug)]
pub struct VegetationPatchCfg {
    pub replaceable: HashSet<String>,
    pub ground_state: BlockStateProvider,
    pub vegetation_feature: PlacedRef,
    pub surface: CaveSurface,
    pub depth: IntProvider,
    pub extra_bottom_block_chance: f32,
    pub vertical_range: i32,
    pub vegetation_chance: f32,
    pub xz_radius: IntProvider,
    pub extra_edge_column_chance: f32,
    pub waterlogged: bool,
}

/// `SculkPatchConfiguration`.
#[derive(Clone, Debug)]
pub struct SculkPatchCfg {
    pub charge_count: i32,
    pub amount_per_charge: i32,
    pub spread_attempts: i32,
    pub growth_rounds: i32,
    pub spread_rounds: i32,
    pub extra_rare_growths: IntProvider,
    pub catalyst_chance: f32,
}

/// Vanilla's own fallen-tree-configuration — its own fallen-tree feature's own
/// config. Distinct from [`super::config::TreeConfig`]: no trunk/foliage
/// placer, no `minimum_size`. `stump_decorators`/`log_decorators` reuse
/// [`super::config::Decorator`] — the same tree-decorator hierarchy
/// `TreeConfig.decorators` already parses through, since vanilla's own
/// trunk-vine decorator / attached-to-logs decorator are shared, not specific
/// to either feature type.
#[derive(Clone, Debug)]
pub struct FallenTreeCfg {
    pub trunk_provider: BlockStateProvider,
    pub log_length: IntProvider,
    pub stump_decorators: Vec<Decorator>,
    pub log_decorators: Vec<Decorator>,
}

/// The cave-root column feature.  The nested placed feature is intentionally
/// retained here: a successful column consumes the nested feature's complete
/// placement body on the same random stream before either kind of root is
/// scattered around it.
#[derive(Clone, Debug)]
pub struct RootSystemCfg {
    pub feature: PlacedRef,
    pub required_vertical_space_for_tree: i32,
    pub level_test_distance: i32,
    pub max_level_deviation: i32,
    pub root_radius: i32,
    pub root_replaceable: HashSet<String>,
    pub root_state_provider: BlockStateProvider,
    pub root_placement_attempts: i32,
    pub root_column_max_height: i32,
    pub hanging_root_radius: i32,
    pub hanging_roots_vertical_span: i32,
    pub hanging_root_state_provider: BlockStateProvider,
    pub hanging_root_placement_attempts: i32,
    pub allowed_vertical_water_for_tree: i32,
    pub allowed_tree_position: BlockPredicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoralKind { Tree, Claw, Mushroom }

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// Places the cave-root column, then its two deliberately separate scatter
/// passes.  A failed nested feature does not leave either kind of root behind.
pub(super) fn place_root_system<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &RootSystemCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if !air_at(grid, origin.x, origin.y, origin.z) {
        return;
    }
    let mut target = None;
    for dy in 1..=cfg.root_column_max_height {
        let at = BlockPos { x: origin.x, y: origin.y + dy, z: origin.z };
        if grid.height_world_surface(at.x, at.z) < at.y {
            return;
        }
        if !cfg.allowed_tree_position.test(grid, tags, at) || !root_tree_space(at, cfg, grid) {
            continue;
        }
        let below = base_at(grid, at.x, at.y - 1, at.z);
        if below == "minecraft:lava" || !sturdy_at(grid, at.x, at.y - 1, at.z) {
            return;
        }
        // The nested feature owns its own placement pipeline.  It is the
        // success signal for the entire column: roots are only scattered once
        // a tree/spring actually landed at the selected height.
        let before = grid.dirty_cells().count();
        super::place_placed_feature(random, at, &cfg.feature, grid, tags);
        if grid.dirty_cells().count() > before {
            // The scan index names the cell ABOVE the final root-column row:
            // the replacement pass ends one row below the nested feature's
            // candidate, not on the candidate's supporting block.
            target = Some(at.y - 1);
        }
        break;
    }
    let Some(target_y) = target else { return; };
    for y in origin.y..target_y {
        for _ in 0..cfg.root_placement_attempts {
            let x = origin.x + random.next_int_bounded(cfg.root_radius) - random.next_int_bounded(cfg.root_radius);
            let z = origin.z + random.next_int_bounded(cfg.root_radius) - random.next_int_bounded(cfg.root_radius);
            if cfg.root_replaceable.contains(base_at(grid, x, y, z)) {
                if let Some(state) = cfg.root_state_provider.get_state(grid, tags, random, BlockPos { x, y, z }) {
                    grid.set_if_in_bounds(x, y, z, state.to_string());
                }
            }
        }
    }
    for _ in 0..cfg.hanging_root_placement_attempts {
        let x = origin.x + random.next_int_bounded(cfg.hanging_root_radius) - random.next_int_bounded(cfg.hanging_root_radius);
        let y = origin.y + random.next_int_bounded(cfg.hanging_roots_vertical_span) - random.next_int_bounded(cfg.hanging_roots_vertical_span);
        let z = origin.z + random.next_int_bounded(cfg.hanging_root_radius) - random.next_int_bounded(cfg.hanging_root_radius);
        if !air_at(grid, x, y, z) || !sturdy_at(grid, x, y + 1, z) {
            continue;
        }
        if let Some(state) = cfg.hanging_root_state_provider.get_state(grid, tags, random, BlockPos { x, y, z }) {
            grid.set_if_in_bounds(x, y, z, state.to_string());
        }
    }
}

fn root_tree_space(at: BlockPos, cfg: &RootSystemCfg, grid: &VegGrid) -> bool {
    for i in 1..=cfg.required_vertical_space_for_tree {
        let state = base_at(grid, at.x, at.y + i, at.z);
        if !is_air(state) && !(i + 1 <= cfg.allowed_vertical_water_for_tree && state == "minecraft:water") {
            return false;
        }
    }
    if cfg.level_test_distance == 0 { return true; }
    for (dx, dz) in HORIZONTAL {
        let x = at.x + dx * cfg.level_test_distance;
        let z = at.z + dz * cfg.level_test_distance;
        if air_at(grid, x, at.y - cfg.max_level_deviation, z)
            || !air_at(grid, x, at.y + cfg.max_level_deviation, z) {
            return false;
        }
    }
    true
}

const CORAL_BLOCKS: [&str; 5] = [
    "minecraft:tube_coral_block", "minecraft:brain_coral_block", "minecraft:bubble_coral_block",
    "minecraft:fire_coral_block", "minecraft:horn_coral_block",
];
const CORALS: [&str; 10] = [
    "minecraft:tube_coral", "minecraft:brain_coral", "minecraft:bubble_coral", "minecraft:fire_coral", "minecraft:horn_coral",
    "minecraft:tube_coral_fan[waterlogged=true]", "minecraft:brain_coral_fan[waterlogged=true]", "minecraft:bubble_coral_fan[waterlogged=true]", "minecraft:fire_coral_fan[waterlogged=true]", "minecraft:horn_coral_fan[waterlogged=true]",
];
const WALL_CORALS: [&str; 5] = [
    "minecraft:tube_coral_wall_fan", "minecraft:brain_coral_wall_fan", "minecraft:bubble_coral_wall_fan",
    "minecraft:fire_coral_wall_fan", "minecraft:horn_coral_wall_fan",
];

/// The three underwater geometries share the tagged coral-block choice and
/// survival/decorating write, but intentionally not their shape loops.
pub(super) fn place_coral<R: RandomSource>(random: &mut R, origin: BlockPos, kind: CoralKind, grid: &mut VegGrid) {
    let state = CORAL_BLOCKS[random.next_int_bounded(CORAL_BLOCKS.len() as i32) as usize];
    match kind {
        CoralKind::Tree => place_coral_tree(random, origin, state, grid),
        CoralKind::Claw => place_coral_claw(random, origin, state, grid),
        CoralKind::Mushroom => place_coral_mushroom(random, origin, state, grid),
    }
}

fn place_coral_block<R: RandomSource>(random: &mut R, at: BlockPos, state: &str, grid: &mut VegGrid) -> bool {
    let here = base_at(grid, at.x, at.y, at.z);
    if !(here == "minecraft:water" || CORALS.iter().any(|state| super::base_id(state) == here)) || !water_at(grid, at.x, at.y + 1, at.z) { return false; }
    grid.set_if_in_bounds(at.x, at.y, at.z, state.to_string());
    if random.next_float() < 0.25 {
        let above = coral_default_state(CORALS[random.next_int_bounded(CORALS.len() as i32) as usize]);
        grid.set_if_in_bounds(at.x, at.y + 1, at.z, above.to_string());
    } else if random.next_float() < 0.05 {
        let pickles = random.next_int_bounded(4) + 1;
        grid.set_if_in_bounds(at.x, at.y + 1, at.z, format!("minecraft:sea_pickle[pickles={pickles},waterlogged=true]"));
    }
    for (i, (dx, dz)) in HORIZONTAL.iter().enumerate() {
        if random.next_float() < 0.2 && water_at(grid, at.x + dx, at.y, at.z + dz) {
            let fan = WALL_CORALS[random.next_int_bounded(WALL_CORALS.len() as i32) as usize];
            let facing = ["north", "east", "south", "west"][i];
            grid.set_if_in_bounds(at.x + dx, at.y, at.z + dz, format!("{fan}[facing={facing},waterlogged=true]"));
        }
    }
    true
}

fn coral_default_state(state: &str) -> String {
    if state.ends_with("_coral") { format!("{state}[waterlogged=true]") } else { state.to_string() }
}

fn shuffled<R: RandomSource>(random: &mut R, dirs: &mut [(i32, i32)]) {
    for i in (1..dirs.len()).rev() { dirs.swap(i, random.next_int_bounded((i + 1) as i32) as usize); }
}

fn place_coral_tree<R: RandomSource>(random: &mut R, origin: BlockPos, state: &str, grid: &mut VegGrid) {
    let height = random.next_int_bounded(3) + 1;
    for y in 0..height { if !place_coral_block(random, BlockPos { x: origin.x, y: origin.y + y, z: origin.z }, state, grid) { return; } }
    let top = origin.y + height;
    let count = random.next_int_bounded(3) + 2;
    let mut dirs = HORIZONTAL;
    shuffled(random, &mut dirs);
    for (dx, dz) in dirs.into_iter().take(count as usize) {
        let mut x = origin.x + dx; let mut y = top; let mut z = origin.z + dz;
        let branch_height = random.next_int_bounded(5) + 2; let mut segment = 0;
        for j in 0..branch_height {
            if !place_coral_block(random, BlockPos { x, y, z }, state, grid) { break; }
            segment += 1; y += 1;
            if j == 0 || (segment >= 2 && random.next_float() < 0.25) { x += dx; z += dz; segment = 0; }
        }
    }
}

fn place_coral_claw<R: RandomSource>(random: &mut R, origin: BlockPos, state: &str, grid: &mut VegGrid) {
    if !place_coral_block(random, origin, state, grid) { return; }
    let claw_i = random.next_int_bounded(4) as usize;
    let claw = HORIZONTAL[claw_i];
    let count = random.next_int_bounded(2) + 2;
    let mut dirs = [claw, HORIZONTAL[(claw_i + 1) % 4], HORIZONTAL[(claw_i + 3) % 4]];
    shuffled(random, &mut dirs);
    for (dx, dz) in dirs.into_iter().take(count as usize) {
        let side = random.next_int_bounded(2) + 1;
        let (seg_dx, seg_dz, mut x, mut y, mut z, inward) = if (dx, dz) == claw {
            (dx, dz, origin.x + dx, origin.y, origin.z + dz, random.next_int_bounded(3) + 2)
        } else {
            let vertical = random.next_int_bounded(2) == 1;
            (if vertical { 0 } else { dx }, if vertical { 0 } else { dz }, origin.x + dx, origin.y + 1, origin.z + dz, random.next_int_bounded(3) + 3)
        };
        for _ in 0..side { if !place_coral_block(random, BlockPos { x, y, z }, state, grid) { break; } x += seg_dx; y += if seg_dx == 0 && seg_dz == 0 { 1 } else { 0 }; z += seg_dz; }
        x -= seg_dx; y -= if seg_dx == 0 && seg_dz == 0 { 1 } else { 0 }; z -= seg_dz; y += 1;
        for _ in 0..inward { x += claw.0; z += claw.1; if !place_coral_block(random, BlockPos { x, y, z }, state, grid) { break; } if random.next_float() < 0.25 { y += 1; } }
    }
}

fn place_coral_mushroom<R: RandomSource>(random: &mut R, origin: BlockPos, state: &str, grid: &mut VegGrid) {
    let h = random.next_int_bounded(3) + 3; let w = random.next_int_bounded(3) + 3; let l = random.next_int_bounded(3) + 3; let sink = random.next_int_bounded(3) + 1;
    for x in 0..=w { for y in 0..=h { for z in 0..=l {
        let inner_x = x != 0 && x != w; let inner_y = y != 0 && y != h; let inner_z = z != 0 && z != l;
        let shell = x == 0 || x == w || y == 0 || y == h || z == 0 || z == l;
        if shell && (inner_x || inner_y) && (inner_z || inner_y) && (inner_x || inner_z) && !(random.next_float() < 0.1) {
            let _ = place_coral_block(random, BlockPos { x: origin.x + x, y: origin.y + y - sink, z: origin.z + z }, state, grid);
        }
    }}}
}

/// Vanilla's own spring feature's place — the single most common absentee in the bundle (6
/// configured features, 112 step-8 entries across the 66 biomes).
///
/// `scheduleTick` is dropped: the fluid block lands, its flow does not run at
/// generation time here. See this module's doc table.
pub(super) fn place_spring(pos: BlockPos, cfg: &SpringCfg, grid: &mut VegGrid) {
    let valid = |x: i32, y: i32, z: i32| cfg.valid_blocks.contains(base_at(grid, x, y, z));
    if !valid(pos.x, pos.y + 1, pos.z) {
        return;
    }
    if cfg.requires_block_below && !valid(pos.x, pos.y - 1, pos.z) {
        return;
    }
    let here = base_at(grid, pos.x, pos.y, pos.z);
    if !is_air(here) && !cfg.valid_blocks.contains(here) {
        return;
    }
    // Vanilla's five neighbours, in its own order: W, E, N, S, DOWN.
    let neighbours = [
        (pos.x - 1, pos.y, pos.z),
        (pos.x + 1, pos.y, pos.z),
        (pos.x, pos.y, pos.z - 1),
        (pos.x, pos.y, pos.z + 1),
        (pos.x, pos.y - 1, pos.z),
    ];
    let mut rock_count = 0;
    let mut hole_count = 0;
    for (x, y, z) in neighbours {
        if valid(x, y, z) {
            rock_count += 1;
        }
        if air_at(grid, x, y, z) {
            hole_count += 1;
        }
    }
    if rock_count == cfg.rock_count && hole_count == cfg.hole_count {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.state.clone());
    }
}

/// Vanilla's own disk feature's place — one `radius` draw, then a column walk per in-circle cell.
pub(super) fn place_disk<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &DiskCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let top = pos.y + cfg.half_height;
    let bottom = pos.y - cfg.half_height - 1;
    let r = cfg.radius.sample(random);
    if r < 0 {
        return;
    }
    for dx in -r..=r {
        for dz in -r..=r {
            if dx * dx + dz * dz > r * r {
                continue;
            }
            let (x, z) = (pos.x + dx, pos.z + dz);
            let mut y = top;
            while y > bottom {
                let at = BlockPos { x, y, z };
                if cfg.target.test(grid, tags, at) {
                    if let Some(state) = cfg.provider.get_state(grid, tags, random, at) {
                        let state = state.to_string();
                        grid.set_if_in_bounds(x, y, z, state);
                    }
                }
                y -= 1;
            }
        }
    }
}

/// Vanilla's own block-pile feature's place. The per-cell `nextFloat()` pair is drawn for every
/// cell in the box, in vanilla's own inclusive-range walk (x outer, y, z inner) order.
pub(super) fn place_block_pile<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if pos.y < grid.min_y + 5 {
        return;
    }
    let xr = 2 + random.next_int_bounded(2);
    let zr = 2 + random.next_int_bounded(2);
    for x in (pos.x - xr)..=(pos.x + xr) {
        for y in pos.y..=(pos.y + 1) {
            for z in (pos.z - zr)..=(pos.z + zr) {
                let xd = pos.x - x;
                let zd = pos.z - z;
                let threshold = random.next_float() * 10.0 - random.next_float() * 6.0;
                let inside = ((xd * xd + zd * zd) as f32) <= threshold;
                let extra = !inside && random.next_float() < 0.031;
                if inside || extra {
                    try_place_pile_block(random, BlockPos { x, y, z }, provider, grid, tags);
                }
            }
        }
    }
}

fn try_place_pile_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if !air_at(grid, pos.x, pos.y, pos.z) {
        return;
    }
    // `mayPlaceOn`: dirt_path is a coin flip, anything else must be face-sturdy.
    // The draw happens on the dirt_path branch only, exactly as vanilla.
    let below = base_at(grid, pos.x, pos.y - 1, pos.z);
    let ok = if below == "minecraft:dirt_path" {
        random.next_bool()
    } else {
        sturdy_at(grid, pos.x, pos.y - 1, pos.z)
    };
    if !ok {
        return;
    }
    if let Some(state) = provider.get_state(grid, tags, random, pos) {
        let state = state.to_string();
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
    }
}

/// Vanilla's own nether-forest-vegetation feature's place. Nylium membership is matched by base
/// id — the bundled `#minecraft:nylium` tag has exactly two members.
pub(super) fn place_nether_forest_vegetation<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &NetherForestVegetationCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let below = base_at(grid, pos.x, pos.y - 1, pos.z);
    if below != "minecraft:crimson_nylium" && below != "minecraft:warped_nylium" {
        return;
    }
    if pos.y < grid.min_y + 1 || pos.y + 1 > grid.min_y + grid.height - 1 {
        return;
    }
    let w = cfg.spread_width.max(1);
    let h = cfg.spread_height.max(1);
    for _ in 0..(w * w) {
        let target = BlockPos {
            x: pos.x + random.next_int_bounded(w) - random.next_int_bounded(w),
            y: pos.y + random.next_int_bounded(h) - random.next_int_bounded(h),
            z: pos.z + random.next_int_bounded(w) - random.next_int_bounded(w),
        };
        let Some(state) = cfg.provider.get_state(grid, tags, random, target) else {
            continue;
        };
        let state = state.to_string();
        if air_at(grid, target.x, target.y, target.z)
            && target.y > grid.min_y
            && sturdy_at(grid, target.x, target.y - 1, target.z)
        {
            grid.set_if_in_bounds(target.x, target.y, target.z, state);
        }
    }
}

/// Vanilla's own block-blob feature's place — three overlapping ellipsoids, each walking down to
/// find its own base.
pub(super) fn place_block_blob<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &BlockBlobCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let mut origin = pos;
    while origin.y > grid.min_y + 3
        && !cfg.can_place_on.test(
            grid,
            tags,
            BlockPos {
                x: origin.x,
                y: origin.y - 1,
                z: origin.z,
            },
        )
    {
        origin.y -= 1;
    }
    if origin.y <= grid.min_y + 3 {
        return;
    }
    for _ in 0..3 {
        let xr = random.next_int_bounded(2);
        let yr = random.next_int_bounded(2);
        let zr = random.next_int_bounded(2);
        let tr = (xr + yr + zr) as f32 * 0.333 + 0.5;
        for x in (origin.x - xr)..=(origin.x + xr) {
            for y in (origin.y - yr)..=(origin.y + yr) {
                for z in (origin.z - zr)..=(origin.z + zr) {
                    let d = (x - origin.x).pow(2) + (y - origin.y).pow(2) + (z - origin.z).pow(2);
                    if (d as f32) <= tr * tr {
                        grid.set_if_in_bounds(x, y, z, cfg.state.clone());
                    }
                }
            }
        }
        origin = BlockPos {
            x: origin.x - 1 + random.next_int_bounded(2),
            y: origin.y - random.next_int_bounded(2),
            z: origin.z - 1 + random.next_int_bounded(2),
        };
    }
}

/// Vanilla's own replace-blobs feature's place — Manhattan-ball replacement of one target block.
pub(super) fn place_replace_blobs<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &ReplaceBlobsCfg,
    grid: &mut VegGrid,
) {
    let mut cursor = pos.y.clamp(grid.min_y + 1, grid.min_y + grid.height - 1);
    let mut found = None;
    while cursor > grid.min_y + 1 {
        if base_at(grid, pos.x, cursor, pos.z) == cfg.target {
            found = Some(cursor);
            break;
        }
        cursor -= 1;
    }
    let Some(cy) = found else { return };
    let rx = cfg.radius.sample(random);
    let ry = cfg.radius.sample(random);
    let rz = cfg.radius.sample(random);
    let max_r = rx.max(ry).max(rz);
    for dx in -rx..=rx {
        for dy in -ry..=ry {
            for dz in -rz..=rz {
                if dx.abs() + dy.abs() + dz.abs() > max_r {
                    continue;
                }
                let (x, y, z) = (pos.x + dx, cy + dy, pos.z + dz);
                if base_at(grid, x, y, z) == cfg.target {
                    grid.set_if_in_bounds(x, y, z, cfg.state.clone());
                }
            }
        }
    }
}

/// Vanilla's own glowstone feature's place — 1500 attempts, each requiring exactly one glowstone
/// neighbour.
pub(super) fn place_glowstone_blob<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
) {
    const GLOWSTONE: &str = "minecraft:glowstone";
    if !air_at(grid, pos.x, pos.y, pos.z) {
        return;
    }
    let above = base_at(grid, pos.x, pos.y + 1, pos.z);
    if above != "minecraft:netherrack" && above != "minecraft:basalt" && above != "minecraft:blackstone" {
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, GLOWSTONE.to_string());
    for _ in 0..1500 {
        let x = pos.x + random.next_int_bounded(8) - random.next_int_bounded(8);
        let y = pos.y - random.next_int_bounded(12);
        let z = pos.z + random.next_int_bounded(8) - random.next_int_bounded(8);
        if !air_at(grid, x, y, z) {
            continue;
        }
        let mut neighbours = 0;
        for (dx, dy, dz) in DIRECTIONS {
            if base_at(grid, x + dx, y + dy, z + dz) == GLOWSTONE {
                neighbours += 1;
            }
            if neighbours > 1 {
                break;
            }
        }
        if neighbours == 1 {
            grid.set_if_in_bounds(x, y, z, GLOWSTONE.to_string());
        }
    }
}

/// Vanilla's own basalt-pillar feature's place.
pub(super) fn place_basalt_pillar<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
) {
    const BASALT: &str = "minecraft:basalt";
    if !air_at(grid, pos.x, pos.y, pos.z) || air_at(grid, pos.x, pos.y + 1, pos.z) {
        return;
    }
    let mut y = pos.y;
    let mut hang = [true; 4];
    let min = grid.min_y;
    let max = grid.min_y + grid.height - 1;
    while air_at(grid, pos.x, y, pos.z) {
        if y < min || y > max {
            return;
        }
        grid.set_if_in_bounds(pos.x, y, pos.z, BASALT.to_string());
        for (i, (dx, dz)) in [(0, -1), (0, 1), (-1, 0), (1, 0)].into_iter().enumerate() {
            // N, S, W, E — vanilla's own order for the four hang-off flags.
            if hang[i] {
                hang[i] = if random.next_int_bounded(10) != 0 {
                    grid.set_if_in_bounds(pos.x + dx, y, pos.z + dz, BASALT.to_string());
                    true
                } else {
                    false
                };
            }
        }
        y -= 1;
    }
    y += 1;
    for (dx, dz) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
        if random.next_bool() {
            grid.set_if_in_bounds(pos.x + dx, y, pos.z + dz, BASALT.to_string());
        }
    }
    y -= 1;
    for dx in -3..4i32 {
        for dz in -3..4i32 {
            let probability = dx.abs() * dz.abs();
            if random.next_int_bounded(10) < 10 - probability {
                let (bx, bz) = (pos.x + dx, pos.z + dz);
                let mut by = y;
                let mut max_drop = 3;
                while air_at(grid, bx, by - 1, bz) {
                    by -= 1;
                    max_drop -= 1;
                    if max_drop <= 0 {
                        break;
                    }
                }
                if !air_at(grid, bx, by - 1, bz) {
                    grid.set_if_in_bounds(bx, by, bz, BASALT.to_string());
                }
            }
        }
    }
}

/// Vanilla's own desert-well feature. The suspicious-sand block entity's
/// loot table is
/// dropped — the blocks themselves are placed, and the two
/// vanilla pick-a-random-list-element draws stay so the stream matches.
pub(super) fn place_desert_well<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
) {
    const SAND: &str = "minecraft:sand";
    const SLAB: &str = "minecraft:sandstone_slab";
    const SANDSTONE: &str = "minecraft:sandstone";
    const WATER: &str = "minecraft:water";
    let mut origin = BlockPos {
        x: pos.x,
        y: pos.y + 1,
        z: pos.z,
    };
    while air_at(grid, origin.x, origin.y, origin.z) && origin.y > grid.min_y + 2 {
        origin.y -= 1;
    }
    if base_at(grid, origin.x, origin.y, origin.z) != SAND {
        return;
    }
    for ox in -2..=2 {
        for oz in -2..=2 {
            if air_at(grid, origin.x + ox, origin.y - 1, origin.z + oz)
                && air_at(grid, origin.x + ox, origin.y - 2, origin.z + oz)
            {
                return;
            }
        }
    }
    let set = |grid: &mut VegGrid, dx: i32, dy: i32, dz: i32, s: &str| {
        grid.set_if_in_bounds(origin.x + dx, origin.y + dy, origin.z + dz, s.to_string());
    };
    for oy in -2..=0 {
        for ox in -2..=2 {
            for oz in -2..=2 {
                set(grid, ox, oy, oz, SANDSTONE);
            }
        }
    }
    set(grid, 0, 0, 0, WATER);
    for (dx, dz) in HORIZONTAL {
        set(grid, dx, 0, dz, WATER);
    }
    set(grid, 0, -1, 0, SAND);
    for (dx, dz) in HORIZONTAL {
        set(grid, dx, -1, dz, SAND);
    }
    for ox in -2..=2 {
        for oz in -2..=2 {
            if ox == -2 || ox == 2 || oz == -2 || oz == 2 {
                set(grid, ox, 1, oz, SANDSTONE);
            }
        }
    }
    for (dx, dz) in [(2, 0), (-2, 0), (0, 2), (0, -2)] {
        set(grid, dx, 1, dz, SLAB);
    }
    for ox in -1..=1 {
        for oz in -1..=1 {
            set(grid, ox, 4, oz, if ox == 0 && oz == 0 { SANDSTONE } else { SLAB });
        }
    }
    for oy in 1..=3 {
        for (dx, dz) in [(-1, -1), (-1, 1), (1, -1), (1, 1)] {
            set(grid, dx, oy, dz, SANDSTONE);
        }
    }
    // Vanilla's own pick-a-random-list-element helper is one `nextInt(5)` per call, twice.
    let picks = [(0, 0), (1, 0), (0, 1), (-1, 0), (0, -1)];
    for depth in 1..=2 {
        let (dx, dz) = picks[random.next_int_bounded(5) as usize];
        set(grid, dx, -depth, dz, "minecraft:suspicious_sand");
    }
}

/// Vanilla's own blue-ice feature's place.
pub(super) fn place_blue_ice<R: RandomSource>(random: &mut R, pos: BlockPos, grid: &mut VegGrid) {
    const BLUE_ICE: &str = "minecraft:blue_ice";
    if pos.y > SEA_LEVEL - 1 {
        return;
    }
    if !water_at(grid, pos.x, pos.y, pos.z) && !water_at(grid, pos.x, pos.y - 1, pos.z) {
        return;
    }
    let mut found = false;
    for (dx, dy, dz) in DIRECTIONS {
        if (dx, dy, dz) == (0, -1, 0) {
            continue;
        }
        if base_at(grid, pos.x + dx, pos.y + dy, pos.z + dz) == "minecraft:packed_ice" {
            found = true;
            break;
        }
    }
    if !found {
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, BLUE_ICE.to_string());
    for _ in 0..200 {
        let y_off = random.next_int_bounded(5) - random.next_int_bounded(6);
        let mut xz_diff = 3;
        if y_off < 2 {
            xz_diff += y_off / 2;
        }
        if xz_diff < 1 {
            continue;
        }
        let x = pos.x + random.next_int_bounded(xz_diff) - random.next_int_bounded(xz_diff);
        let y = pos.y + y_off;
        let z = pos.z + random.next_int_bounded(xz_diff) - random.next_int_bounded(xz_diff);
        let here = base_at(grid, x, y, z);
        let replaceable = is_air(here)
            || here == "minecraft:water"
            || here == "minecraft:packed_ice"
            || here == "minecraft:ice";
        if !replaceable {
            continue;
        }
        for (dx, dy, dz) in DIRECTIONS {
            if base_at(grid, x + dx, y + dy, z + dz) == BLUE_ICE {
                grid.set_if_in_bounds(x, y, z, BLUE_ICE.to_string());
                break;
            }
        }
    }
}

/// Vanilla's own kelp feature's place.
pub(super) fn place_kelp<R: RandomSource>(random: &mut R, pos: BlockPos, grid: &mut VegGrid) {
    let y = grid.height_ocean_floor(pos.x, pos.z);
    let (x, z) = (pos.x, pos.z);
    if !water_at(grid, x, y, z) {
        return;
    }
    let height = 1 + random.next_int_bounded(10);
    let mut cy = y;
    for h in 0..=height {
        if water_at(grid, x, cy, z) && water_at(grid, x, cy + 1, z) {
            if h == height {
                let age = random.next_int_bounded(4) + 20;
                grid.set_if_in_bounds(x, cy, z, format!("minecraft:kelp[age={age}]"));
            } else {
                grid.set_if_in_bounds(x, cy, z, "minecraft:kelp_plant".to_string());
            }
        } else if h > 0 {
            let below = cy - 1;
            if base_at(grid, x, below - 1, z) != "minecraft:kelp" {
                let age = random.next_int_bounded(4) + 20;
                grid.set_if_in_bounds(x, below, z, format!("minecraft:kelp[age={age}]"));
            }
            break;
        }
        cy += 1;
    }
}

/// Vanilla's own sea-pickle feature's place.
pub(super) fn place_sea_pickle<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    count: &IntProvider,
    grid: &mut VegGrid,
) {
    let n = count.sample(random);
    for _ in 0..n {
        let dx = random.next_int_bounded(8) - random.next_int_bounded(8);
        let dz = random.next_int_bounded(8) - random.next_int_bounded(8);
        let (x, z) = (pos.x + dx, pos.z + dz);
        let y = grid.height_ocean_floor(x, z);
        let pickles = random.next_int_bounded(4) + 1;
        if water_at(grid, x, y, z) && sturdy_at(grid, x, y - 1, z) {
            grid.set_if_in_bounds(
                x,
                y,
                z,
                format!("minecraft:sea_pickle[pickles={pickles},waterlogged=true]"),
            );
        }
    }
}

/// Vanilla's own seagrass feature's place.
pub(super) fn place_seagrass<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    probability: f64,
    grid: &mut VegGrid,
) {
    let dx = random.next_int_bounded(8) - random.next_int_bounded(8);
    let dz = random.next_int_bounded(8) - random.next_int_bounded(8);
    let (x, z) = (pos.x + dx, pos.z + dz);
    let y = grid.height_ocean_floor(x, z);
    if !water_at(grid, x, y, z) {
        return;
    }
    let is_tall = random.next_double() < probability;
    if !sturdy_at(grid, x, y - 1, z) {
        return;
    }
    if is_tall {
        if water_at(grid, x, y + 1, z) {
            grid.set_if_in_bounds(x, y, z, "minecraft:tall_seagrass[half=lower]".to_string());
            grid.set_if_in_bounds(x, y + 1, z, "minecraft:tall_seagrass[half=upper]".to_string());
        }
    } else {
        grid.set_if_in_bounds(x, y, z, "minecraft:seagrass".to_string());
    }
}

/// Vanilla's own vines feature's place — one vine face against the first acceptable neighbour.
pub(super) fn place_vines(pos: BlockPos, grid: &mut VegGrid) {
    if !air_at(grid, pos.x, pos.y, pos.z) {
        return;
    }
    // Vanilla's own vine-block "is acceptable neighbour" is a full-face-sturdy test; narrowed here.
    // Property names are vanilla's own: north/east/south/west/up.
    for ((dx, dy, dz), prop) in DIRECTIONS.into_iter().zip([
        "down", "up", "north", "south", "west", "east",
    ]) {
        if prop == "down" {
            continue;
        }
        if sturdy_at(grid, pos.x + dx, pos.y + dy, pos.z + dz) {
            grid.set_if_in_bounds(pos.x, pos.y, pos.z, format!("minecraft:vine[{prop}=true]"));
            return;
        }
    }
}

/// Vanilla's own twisting-vines feature's place.
pub(super) fn place_twisting_vines<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: TwistingVinesCfg,
    grid: &mut VegGrid,
) {
    if invalid_twisting_location(grid, pos) {
        return;
    }
    let (w, h, max_h) = (cfg.spread_width.max(1), cfg.spread_height.max(1), cfg.max_height.max(1));
    for _ in 0..(w * w) {
        let mut p = BlockPos {
            x: pos.x + next_int_between(random, -w, w),
            y: pos.y + next_int_between(random, -h, h),
            z: pos.z + next_int_between(random, -w, w),
        };
        if !find_first_air_above_ground(grid, &mut p) || invalid_twisting_location(grid, p) {
            continue;
        }
        let mut height = next_int_between(random, 1, max_h);
        if random.next_int_bounded(6) == 0 {
            height *= 2;
        }
        if random.next_int_bounded(5) == 0 {
            height = 1;
        }
        place_growing_column(random, p, height, grid, true);
    }
}

fn invalid_twisting_location(grid: &VegGrid, pos: BlockPos) -> bool {
    if !air_at(grid, pos.x, pos.y, pos.z) {
        return true;
    }
    let below = base_at(grid, pos.x, pos.y - 1, pos.z);
    below != "minecraft:netherrack"
        && below != "minecraft:warped_nylium"
        && below != "minecraft:warped_wart_block"
}

fn find_first_air_above_ground(grid: &VegGrid, pos: &mut BlockPos) -> bool {
    loop {
        pos.y -= 1;
        if pos.y < grid.min_y || pos.y >= grid.min_y + grid.height {
            return false;
        }
        if !air_at(grid, pos.x, pos.y, pos.z) {
            break;
        }
    }
    pos.y += 1;
    true
}

/// The shared "place weeping-vines column" step used by both vine features. `upwards`
/// selects twisting (grows up, `h` starts at 1) from weeping (grows down, `h`
/// starts at 0) — the two really do differ in their loop bounds.
fn place_growing_column<R: RandomSource>(
    random: &mut R,
    start: BlockPos,
    total: i32,
    grid: &mut VegGrid,
    upwards: bool,
) {
    let (head, plant, step, first) = if upwards {
        ("minecraft:twisting_vines", "minecraft:twisting_vines_plant", 1, 1)
    } else {
        ("minecraft:weeping_vines", "minecraft:weeping_vines_plant", -1, 0)
    };
    let mut pos = start;
    let mut h = first;
    while h <= total {
        if air_at(grid, pos.x, pos.y, pos.z) {
            let blocked = !air_at(grid, pos.x, pos.y + step, pos.z);
            if h == total || blocked {
                let age = next_int_between(random, 17, 25);
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, format!("{head}[age={age}]"));
                return;
            }
            grid.set_if_in_bounds(pos.x, pos.y, pos.z, plant.to_string());
        }
        pos.y += step;
        h += 1;
    }
}

/// Vanilla's own weeping-vines feature's place — nether wart roof blob plus hanging vines.
pub(super) fn place_weeping_vines<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
) {
    const WART: &str = "minecraft:nether_wart_block";
    if !air_at(grid, pos.x, pos.y, pos.z) {
        return;
    }
    let above = base_at(grid, pos.x, pos.y + 1, pos.z);
    if above != "minecraft:netherrack" && above != WART {
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, WART.to_string());
    for _ in 0..200 {
        let x = pos.x + random.next_int_bounded(6) - random.next_int_bounded(6);
        let y = pos.y + random.next_int_bounded(2) - random.next_int_bounded(5);
        let z = pos.z + random.next_int_bounded(6) - random.next_int_bounded(6);
        if !air_at(grid, x, y, z) {
            continue;
        }
        let mut neighbours = 0;
        for (dx, dy, dz) in DIRECTIONS {
            let b = base_at(grid, x + dx, y + dy, z + dz);
            if b == "minecraft:netherrack" || b == WART {
                neighbours += 1;
            }
            if neighbours > 1 {
                break;
            }
        }
        if neighbours == 1 {
            grid.set_if_in_bounds(x, y, z, WART.to_string());
        }
    }
    for _ in 0..100 {
        let x = pos.x + random.next_int_bounded(8) - random.next_int_bounded(8);
        let y = pos.y + random.next_int_bounded(2) - random.next_int_bounded(7);
        let z = pos.z + random.next_int_bounded(8) - random.next_int_bounded(8);
        if !air_at(grid, x, y, z) {
            continue;
        }
        let up = base_at(grid, x, y + 1, z);
        if up != "minecraft:netherrack" && up != WART {
            continue;
        }
        let mut height = next_int_between(random, 1, 8);
        if random.next_int_bounded(6) == 0 {
            height *= 2;
        }
        if random.next_int_bounded(5) == 0 {
            height = 1;
        }
        place_growing_column(random, BlockPos { x, y, z }, height, grid, false);
    }
}

/// Vanilla's own multiface-growth feature's place — glow lichen and sculk vein.
///
/// Faithful including the search loop, support gate, and the one outward spread
/// attempt made after placement. The spread's all-directions shuffle consumes
/// five draws, so its order affects both the extra block and later attempts.
///
/// The search loop reproduces vanilla's actual code, including that
/// `pos.setWithOffset(origin, searchDirection)` re-derives from `origin` every
/// iteration rather than advancing — so `search_range` really does re-test the
/// same adjacent cell. That is vanilla's behaviour, not a transcription slip;
/// "fixing" it would place lichen where vanilla places none.
pub(super) fn place_multiface_growth<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &MultifaceGrowthCfg,
    grid: &mut VegGrid,
) {
    if !air_or_water_at(grid, pos) {
        return;
    }
    let valid = valid_directions(cfg);
    if valid.is_empty() {
        return;
    }
    let search_order = shuffled_copy(random, &valid);
    if place_growth_if_possible(random, pos, cfg, &search_order, grid) {
        return;
    }
    for &search_dir in &search_order {
        let opposite = (-search_dir.0, -search_dir.1, -search_dir.2);
        let placement: Vec<(i32, i32, i32)> =
            valid.iter().copied().filter(|d| *d != opposite).collect();
        let placement = shuffled_copy(random, &placement);
        for _ in 0..cfg.search_range {
            let at = BlockPos {
                x: pos.x + search_dir.0,
                y: pos.y + search_dir.1,
                z: pos.z + search_dir.2,
            };
            if !air_or_water_at(grid, at) && base_at(grid, at.x, at.y, at.z) != cfg.block {
                break;
            }
            if place_growth_if_possible(random, at, cfg, &placement, grid) {
                return;
            }
        }
    }
}

/// Places one bamboo stalk and, when configured, its seeded podzol disk.
pub(super) fn place_bamboo<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    podzol_probability: f64,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if !air_at(grid, origin.x, origin.y, origin.z)
        || !tags
            .supports_bamboo
            .contains(base_at(grid, origin.x, origin.y - 1, origin.z))
    {
        return;
    }
    let height = random.next_int_bounded(12) + 5;
    if random.next_float() < podzol_probability as f32 {
        let radius = random.next_int_bounded(4) + 1;
        for x in origin.x - radius..=origin.x + radius {
            for z in origin.z - radius..=origin.z + radius {
                let dx = x - origin.x;
                let dz = z - origin.z;
                if dx * dx + dz * dz > radius * radius {
                    continue;
                }
                let y = grid.height_world_surface(x, z) - 1;
                if tags
                    .beneath_bamboo_podzol_replaceable
                    .contains(base_at(grid, x, y, z))
                {
                    grid.set_if_in_bounds(x, y, z, "minecraft:podzol[snowy=false]".to_string());
                }
            }
        }
    }
    let mut top = origin.y;
    for _ in 0..height {
        if !air_at(grid, origin.x, top, origin.z) {
            break;
        }
        grid.set_if_in_bounds(
            origin.x,
            top,
            origin.z,
            "minecraft:bamboo[age=1,leaves=none,stage=0]".to_string(),
        );
        top += 1;
    }
    if top - origin.y >= 3 {
        grid.set_if_in_bounds(
            origin.x,
            top,
            origin.z,
            "minecraft:bamboo[age=1,leaves=large,stage=1]".to_string(),
        );
        grid.set_if_in_bounds(
            origin.x,
            top - 1,
            origin.z,
            "minecraft:bamboo[age=1,leaves=large,stage=0]".to_string(),
        );
        grid.set_if_in_bounds(
            origin.x,
            top - 2,
            origin.z,
            "minecraft:bamboo[age=1,leaves=small,stage=0]".to_string(),
        );
    }
}

fn air_or_water_at(grid: &VegGrid, pos: BlockPos) -> bool {
    let base = base_at(grid, pos.x, pos.y, pos.z);
    is_air(base) || base == "minecraft:water"
}

/// `MultifaceGrowthConfiguration`'s `validDirections`, in its own build order:
/// ceiling (UP), floor (DOWN), then `Plane.HORIZONTAL` (N, E, S, W). The order is
/// the shuffle's input, so it decides the output.
fn valid_directions(cfg: &MultifaceGrowthCfg) -> Vec<(i32, i32, i32)> {
    let mut out = Vec::with_capacity(6);
    if cfg.can_place_on_ceiling {
        out.push((0, 1, 0));
    }
    if cfg.can_place_on_floor {
        out.push((0, -1, 0));
    }
    if cfg.can_place_on_wall {
        for (dx, dz) in HORIZONTAL {
            out.push((dx, 0, dz));
        }
    }
    out
}

/// Vanilla's own multiface-growth feature's "place growth if possible" — the first direction whose
/// neighbour is in `can_be_placed_on` wins, and a `null` state there aborts the
/// whole call rather than trying the next direction.
fn place_growth_if_possible<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &MultifaceGrowthCfg,
    directions: &[(i32, i32, i32)],
    grid: &mut VegGrid,
) -> bool {
    for &(dx, dy, dz) in directions {
        if !can_attach_to(grid, pos, (dx, dy, dz), cfg) {
            continue;
        }
        let existing = grid.get(pos.x, pos.y, pos.z).to_string();
        let face = face_property(dx, dy, dz);
        // Vanilla's own "get state for placement" returns null when the growth is already there
        // with that face set, and vanilla then gives up entirely.
        if super::base_id(&existing) == cfg.block && existing.contains(&format!("{face}=true")) {
            return false;
        }
        let new_state = multiface_state(&cfg.block, &existing, face);
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, new_state.clone());
        if random.next_float() < cfg.chance_of_spreading {
            spread_multiface(random, pos, (dx, dy, dz), &new_state, cfg, grid);
        }
        return true;
    }
    false
}

fn can_attach_to(
    grid: &VegGrid,
    pos: BlockPos,
    face: (i32, i32, i32),
    cfg: &MultifaceGrowthCfg,
) -> bool {
    let support_x = pos.x + face.0;
    let support_y = pos.y + face.1;
    let support_z = pos.z + face.2;
    cfg.can_be_placed_on.contains(base_at(grid, support_x, support_y, support_z))
        && sturdy_at(grid, support_x, support_y, support_z)
}

/// Tries one shuffled outward direction and commits the first valid spread
/// position. The three candidate forms cover adding a face in place, moving
/// along the same support plane, and wrapping around an edge.
fn spread_multiface<R: RandomSource>(
    random: &mut R,
    source: BlockPos,
    starting_face: (i32, i32, i32),
    source_state: &str,
    cfg: &MultifaceGrowthCfg,
    grid: &mut VegGrid,
) {
    for direction in shuffled_copy(random, &DIRECTIONS) {
        if same_axis(direction, starting_face)
            || !source_state.contains(&format!("{}=true", face_property(starting_face.0, starting_face.1, starting_face.2)))
            || source_state.contains(&format!("{}=true", face_property(direction.0, direction.1, direction.2)))
        {
            continue;
        }
        let candidates = [
            (source, direction),
            (
                BlockPos { x: source.x + direction.0, y: source.y + direction.1, z: source.z + direction.2 },
                starting_face,
            ),
            (
                BlockPos {
                    x: source.x + direction.0 + starting_face.0,
                    y: source.y + direction.1 + starting_face.1,
                    z: source.z + direction.2 + starting_face.2,
                },
                (-direction.0, -direction.1, -direction.2),
            ),
        ];
        for (target, face) in candidates {
            let old = grid.get(target.x, target.y, target.z).to_string();
            if (air_or_water_at(grid, target) || super::base_id(&old) == cfg.block)
                && can_attach_to(grid, target, face, cfg)
                && !(super::base_id(&old) == cfg.block
                    && old.contains(&format!("{}=true", face_property(face.0, face.1, face.2))))
            {
                grid.set_if_in_bounds(
                    target.x,
                    target.y,
                    target.z,
                    multiface_state(&cfg.block, &old, face_property(face.0, face.1, face.2)),
                );
                return;
            }
        }
    }
}

fn same_axis(a: (i32, i32, i32), b: (i32, i32, i32)) -> bool {
    (a.0 != 0 && b.0 != 0) || (a.1 != 0 && b.1 != 0) || (a.2 != 0 && b.2 != 0)
}

fn multiface_state(block: &str, old: &str, enabled_face: &str) -> String {
    let value = |face: &str| face == enabled_face || old.contains(&format!("{face}=true"));
    let waterlogged = super::base_id(old) == "minecraft:water" || old.contains("waterlogged=true");
    format!(
        "{block}[down={},east={},north={},south={},up={},waterlogged={waterlogged},west={}]",
        value("down"),
        value("east"),
        value("north"),
        value("south"),
        value("up"),
        value("west"),
    )
}

/// The growth's own face property for a support at the given offset.
fn face_property(dx: i32, dy: i32, dz: i32) -> &'static str {
    match (dx, dy, dz) {
        (0, -1, 0) => "down",
        (0, 1, 0) => "up",
        (0, 0, -1) => "north",
        (0, 0, 1) => "south",
        (-1, 0, 0) => "west",
        _ => "east",
    }
}

/// Vanilla's own shuffled-copy / shuffle — Fisher-Yates `for (i = size; i > 1; i--)
/// swap(i - 1, nextInt(i))`, so exactly `size - 1` draws. The count is what
/// matters most here; see [`place_multiface_growth`]'s doc.
fn shuffled_copy<R: RandomSource>(random: &mut R, input: &[(i32, i32, i32)]) -> Vec<(i32, i32, i32)> {
    let mut out = input.to_vec();
    let mut i = out.len();
    while i > 1 {
        let j = random.next_int_bounded(i as i32) as usize;
        out.swap(i - 1, j);
        i -= 1;
    }
    out
}

/// Vanilla's own math-helper next-int at `(random, min, max)` — inclusive both ends, one draw.
fn next_int_between<R: RandomSource>(random: &mut R, min: i32, max: i32) -> i32 {
    if min >= max {
        return min;
    }
    random.next_int_bounded(max - min + 1) + min
}

/// Vanilla's own lake feature's place. The 8×16×16 boolean mould, its full validity scan, the
/// fluid/air fill and the barrier shell — all of it, because the scan is what
/// stops a lake opening into an existing cave.
///
/// The final "freeze the surface if the biome would" pass is dropped: biome
/// membership is not available to a placement body here, and `freeze_top_layer`
/// (step 10) already ices exposed water.
pub(super) fn place_lake<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &LakeCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if pos.y <= grid.min_y + 4 {
        return;
    }
    let origin = BlockPos {
        x: pos.x - 8,
        y: pos.y - 4,
        z: pos.z - 8,
    };
    let mut mould = [false; 2048];
    let idx = |x: usize, z: usize, y: usize| (x * 16 + z) * 8 + y;
    let spots = random.next_int_bounded(4) + 4;
    for _ in 0..spots {
        let xr = random.next_double() * 6.0 + 3.0;
        let yr = random.next_double() * 4.0 + 2.0;
        let zr = random.next_double() * 6.0 + 3.0;
        let xp = random.next_double() * (16.0 - xr - 2.0) + 1.0 + xr / 2.0;
        let yp = random.next_double() * (8.0 - yr - 4.0) + 2.0 + yr / 2.0;
        let zp = random.next_double() * (16.0 - zr - 2.0) + 1.0 + zr / 2.0;
        for xx in 1..15usize {
            for zz in 1..15usize {
                for yy in 1..7usize {
                    let xd = (xx as f64 - xp) / (xr / 2.0);
                    let yd = (yy as f64 - yp) / (yr / 2.0);
                    let zd = (zz as f64 - zp) / (zr / 2.0);
                    if xd * xd + yd * yd + zd * zd < 1.0 {
                        mould[idx(xx, zz, yy)] = true;
                    }
                }
            }
        }
    }
    let Some(fluid) = cfg.fluid.get_state(grid, tags, random, origin).map(str::to_string) else {
        return;
    };
    // Shell test: an unset cell adjacent to a set one.
    let shell = |mould: &[bool; 2048], xx: usize, zz: usize, yy: usize| -> bool {
        if mould[idx(xx, zz, yy)] {
            return false;
        }
        (xx < 15 && mould[idx(xx + 1, zz, yy)])
            || (xx > 0 && mould[idx(xx - 1, zz, yy)])
            || (zz < 15 && mould[idx(xx, zz + 1, yy)])
            || (zz > 0 && mould[idx(xx, zz - 1, yy)])
            || (yy < 7 && mould[idx(xx, zz, yy + 1)])
            || (yy > 0 && mould[idx(xx, zz, yy - 1)])
    };
    for xx in 0..16usize {
        for zz in 0..16usize {
            for yy in 0..8usize {
                if !shell(&mould, xx, zz, yy) {
                    continue;
                }
                let at = BlockPos {
                    x: origin.x + xx as i32,
                    y: origin.y + yy as i32,
                    z: origin.z + zz as i32,
                };
                let base = base_at(grid, at.x, at.y, at.z);
                if yy >= 4 && is_fluid(base) {
                    return;
                }
                if yy < 4 && !sturdy_at(grid, at.x, at.y, at.z) && super::base_id(&fluid) != base {
                    return;
                }
                if !cfg.can_place_feature.test(grid, tags, at) {
                    return;
                }
            }
        }
    }
    for xx in 0..16usize {
        for zz in 0..16usize {
            for yy in 0..8usize {
                if !mould[idx(xx, zz, yy)] {
                    continue;
                }
                let at = BlockPos {
                    x: origin.x + xx as i32,
                    y: origin.y + yy as i32,
                    z: origin.z + zz as i32,
                };
                if cfg.can_replace_with_air_or_fluid.test(grid, tags, at) {
                    let state = if yy >= 4 {
                        "minecraft:cave_air".to_string()
                    } else {
                        fluid.clone()
                    };
                    grid.set_if_in_bounds(at.x, at.y, at.z, state);
                }
            }
        }
    }
    let Some(barrier) = cfg.barrier.get_state(grid, tags, random, origin).map(str::to_string) else {
        return;
    };
    if is_air(super::base_id(&barrier)) {
        return;
    }
    for xx in 0..16usize {
        for zz in 0..16usize {
            for yy in 0..8usize {
                if !shell(&mould, xx, zz, yy) {
                    continue;
                }
                if yy >= 4 && random.next_int_bounded(2) == 0 {
                    continue;
                }
                let at = BlockPos {
                    x: origin.x + xx as i32,
                    y: origin.y + yy as i32,
                    z: origin.z + zz as i32,
                };
                if sturdy_at(grid, at.x, at.y, at.z)
                    && cfg.can_replace_with_barrier.test(grid, tags, at)
                {
                    grid.set_if_in_bounds(at.x, at.y, at.z, barrier.clone());
                }
            }
        }
    }
}

/// Turns the configured all-connected cap state into the state for one cap
/// position. A `true` face is exposed at the cap's edge; an interior face is
/// `false`. The bundled providers carry all six properties, and retaining the
/// provider's spelling for the other properties keeps arbitrary valid inputs
/// intact.
fn mushroom_cap_state(state: &str, west: bool, east: bool, north: bool, south: bool, up: bool) -> String {
    let mut out = state.to_string();
    for (name, exposed) in [
        ("west", west),
        ("east", east),
        ("north", north),
        ("south", south),
        ("up", up),
    ] {
        let value = if exposed { "true" } else { "false" };
        out = out
            .replacen(&format!("{name}=true"), &format!("{name}={value}"), 1)
            .replacen(&format!("{name}=false"), &format!("{name}={value}"), 1);
    }
    out
}

/// Huge-mushroom placement shares one bounded `0..3` height draw plus four
/// across the brown and red variants. Keeping the deterministic body
/// separate lets the layout tests exercise the bundled cap records without
/// coupling their assertions to a particular random-source seed.
pub(super) fn place_huge_mushroom<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &HugeMushroomCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let height = random.next_int_bounded(3) + 4;
    place_huge_mushroom_at_height(random, pos, cfg, height, grid, tags);
}

pub(super) fn place_huge_mushroom_at_height<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &HugeMushroomCfg,
    height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let max_y = grid.min_y + grid.height;
    if pos.y < grid.min_y + 1
        || pos.y + height + 1 >= max_y
        || !cfg.can_place_on.test(
            grid,
            tags,
            BlockPos { x: pos.x, y: pos.y - 1, z: pos.z },
        )
    {
        return;
    }

    // The feature's validity scan reserves only the stem column until the
    // cap layer, then its complete configured radius. This is intentionally
    // broader than either cap's final cut-out shape: a blocked skipped corner
    // still rejects the attempt before any stem write occurs.
    for y in 0..=height {
        let radius = if y == height { cfg.foliage_radius } else { 0 };
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if !valid_tree_pos(grid, tags, pos.x + dx, pos.y + y, pos.z + dz) {
                    return;
                }
            }
        }
    }

    for y in 0..height {
        let at = BlockPos { x: pos.x, y: pos.y + y, z: pos.z };
        if !sturdy_at(grid, at.x, at.y, at.z) {
            if let Some(state) = cfg.stem_provider.get_state(grid, tags, random, at) {
                grid.set_if_in_bounds(at.x, at.y, at.z, state.to_string());
            }
        }
    }

    let mut place_cap = |at: BlockPos, west: bool, east: bool, north: bool, south: bool, up: bool| {
        if sturdy_at(grid, at.x, at.y, at.z) {
            return;
        }
        if let Some(state) = cfg.cap_provider.get_state(grid, tags, random, at) {
            grid.set_if_in_bounds(
                at.x,
                at.y,
                at.z,
                mushroom_cap_state(state, west, east, north, south, up),
            );
        }
    };

    match cfg.kind {
        HugeMushroomKind::Brown => {
            let radius = cfg.foliage_radius;
            let positions: Vec<_> = (-radius..=radius)
                .flat_map(|dx| {
                    (-radius..=radius).filter_map(move |dz| {
                        // The four corners are deliberately absent from the brown cap.
                        ((dx != -radius || dz != -radius)
                            && (dx != -radius || dz != radius)
                            && (dx != radius || dz != -radius)
                            && (dx != radius || dz != radius))
                            .then_some((dx, dz))
                    })
                })
                .collect();
            let occupied: HashSet<_> = positions.iter().copied().collect();
            for (dx, dz) in positions {
                place_cap(
                    BlockPos { x: pos.x + dx, y: pos.y + height, z: pos.z + dz },
                    !occupied.contains(&(dx - 1, dz)),
                    !occupied.contains(&(dx + 1, dz)),
                    !occupied.contains(&(dx, dz - 1)),
                    !occupied.contains(&(dx, dz + 1)),
                    true,
                );
            }
        }
        HugeMushroomKind::Red => {
            for y in (height - 3)..=height {
                let radius = if y < height { cfg.foliage_radius } else { cfg.foliage_radius - 1 };
                let positions: Vec<_> = (-radius..=radius)
                    .flat_map(|dx| {
                        (-radius..=radius).filter_map(move |dz| {
                            // The three lower layers are a plus-shaped rim; the
                            // top layer is a filled smaller square.
                            (y == height || (west_or_east(dx, radius) != north_or_south(dz, radius)))
                                .then_some((dx, dz))
                        })
                    })
                    .collect();
                for (dx, dz) in positions {
                    place_cap(
                        BlockPos { x: pos.x + dx, y: pos.y + y, z: pos.z + dz },
                        dx < 0,
                        dx > 0,
                        dz < 0,
                        dz > 0,
                        y >= height - 1,
                    );
                }
            }
        }
    }
}

fn west_or_east(value: i32, radius: i32) -> bool {
    value == -radius || value == radius
}

fn north_or_south(value: i32, radius: i32) -> bool {
    value == -radius || value == radius
}

/// Places a vegetation patch (and its waterlogged variant): a replaceable
/// floor/ceiling patch followed by the configured feature on the resulting
/// surface.
pub(super) fn place_vegetation_patch<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &VegetationPatchCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let x_radius = cfg.xz_radius.sample(random) + 1;
    let z_radius = cfg.xz_radius.sample(random) + 1;
    let inwards = cfg.surface.dy();
    let outwards = -inwards;
    let mut surface: Vec<BlockPos> = Vec::new();
    for dx in -x_radius..=x_radius {
        let x_edge = dx == -x_radius || dx == x_radius;
        for dz in -z_radius..=z_radius {
            let z_edge = dz == -z_radius || dz == z_radius;
            let edge = x_edge || z_edge;
            let corner = x_edge && z_edge;
            let edge_not_corner = edge && !corner;
            if corner {
                continue;
            }
            if edge_not_corner
                && !(cfg.extra_edge_column_chance != 0.0
                    && random.next_float() <= cfg.extra_edge_column_chance)
            {
                continue;
            }
            let mut cur = BlockPos {
                x: pos.x + dx,
                y: pos.y,
                z: pos.z + dz,
            };
            let mut steps = 0;
            while air_at(grid, cur.x, cur.y, cur.z) && steps < cfg.vertical_range {
                cur.y += inwards;
                steps += 1;
            }
            let mut steps = 0;
            while !air_at(grid, cur.x, cur.y, cur.z) && steps < cfg.vertical_range {
                cur.y += outwards;
                steps += 1;
            }
            let below = BlockPos {
                x: cur.x,
                y: cur.y + inwards,
                z: cur.z,
            };
            if !air_at(grid, cur.x, cur.y, cur.z) || !sturdy_at(grid, below.x, below.y, below.z) {
                continue;
            }
            let mut depth = cfg.depth.sample(random);
            if cfg.extra_bottom_block_chance > 0.0
                && random.next_float() < cfg.extra_bottom_block_chance
            {
                depth += 1;
            }
            if place_patch_ground(random, below, cfg, depth, grid, tags) {
                surface.push(below);
            }
        }
    }
    if cfg.waterlogged {
        // `WaterloggedVegetationPatchFeature`: only the non-exposed surface cells
        // survive, and each becomes water.
        let kept: Vec<BlockPos> = surface
            .iter()
            .copied()
            .filter(|p| !patch_exposed(grid, *p))
            .collect();
        for p in &kept {
            grid.set_if_in_bounds(p.x, p.y, p.z, "minecraft:water".to_string());
        }
        surface = kept;
    }
    for p in surface {
        if cfg.vegetation_chance > 0.0 && random.next_float() < cfg.vegetation_chance {
            let target = BlockPos {
                x: p.x,
                y: p.y + outwards,
                z: p.z,
            };
            super::place_placed_feature_at(random, target, &cfg.vegetation_feature, grid, tags);
        }
    }
}

fn patch_exposed(grid: &VegGrid, pos: BlockPos) -> bool {
    // NORTH, EAST, SOUTH, WEST, DOWN — the five vanilla checks.
    for (dx, dz) in HORIZONTAL {
        if !sturdy_at(grid, pos.x + dx, pos.y, pos.z + dz) {
            return true;
        }
    }
    !sturdy_at(grid, pos.x, pos.y - 1, pos.z)
}

fn place_patch_ground<R: RandomSource>(
    random: &mut R,
    start: BlockPos,
    cfg: &VegetationPatchCfg,
    depth: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
) -> bool {
    let mut cur = start;
    for i in 0..depth {
        let Some(state) = cfg.ground_state.get_state(grid, tags, random, cur).map(str::to_string)
        else {
            return i != 0;
        };
        let below = base_at(grid, cur.x, cur.y, cur.z);
        if super::base_id(&state) == below {
            continue;
        }
        if !cfg.replaceable.contains(below) {
            return i != 0;
        }
        grid.set_if_in_bounds(cur.x, cur.y, cur.z, state);
        cur.y += cfg.surface.dy();
    }
    true
}

/// Vanilla's own sculk-patch feature's place.
///
/// **Narrowed, named:** vanilla's own sculk-spreader is a full charge-propagation simulation
/// over a live level and is not modelled. What lands instead is a sculk skin over
/// the sturdy cells within a radius derived from the config's own charge budget,
/// plus the catalyst and the rare shriekers, whose draws are the ones later
/// features depend on. Deep dark reads as sculk-floored rather than as vanilla's
/// exact patch outline.
pub(super) fn place_sculk_patch<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &SculkPatchCfg,
    grid: &mut VegGrid,
) {
    if !sturdy_at(grid, pos.x, pos.y - 1, pos.z) || !air_at(grid, pos.x, pos.y, pos.z) {
        return;
    }
    let budget = (cfg.charge_count * cfg.amount_per_charge).max(1);
    let radius = ((budget as f64).sqrt() / 2.0).round().clamp(1.0, 8.0) as i32;
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            if dx * dx + dz * dz > radius * radius {
                continue;
            }
            let (x, z) = (pos.x + dx, pos.z + dz);
            if sturdy_at(grid, x, pos.y - 1, z) && air_at(grid, x, pos.y, z) {
                grid.set_if_in_bounds(x, pos.y - 1, z, "minecraft:sculk".to_string());
            }
        }
    }
    if random.next_float() <= cfg.catalyst_chance && sturdy_at(grid, pos.x, pos.y - 1, pos.z) {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, "minecraft:sculk_catalyst".to_string());
    }
    let extra = cfg.extra_rare_growths.sample(random);
    for _ in 0..extra {
        let x = pos.x + random.next_int_bounded(5) - 2;
        let z = pos.z + random.next_int_bounded(5) - 2;
        if air_at(grid, x, pos.y, z) && sturdy_at(grid, x, pos.y - 1, z) {
            grid.set_if_in_bounds(
                x,
                pos.y,
                z,
                "minecraft:sculk_shrieker[can_summon=true,shrieking=false,waterlogged=false]"
                    .to_string(),
            );
        }
    }
}

/// Vanilla's own fallen-tree feature's "is over solid ground" (`isFaceSturdy(UP)` on the block
/// below) — reuses [`sturdy_at`], this file's own established approximation
/// for exactly that vanilla concept (see the module doc's table). Affects
/// only whether the ground check passes, never the RNG stream:
/// `canPlaceEntireFallenLog` (below) draws no RNG regardless of its verdict.
fn is_over_solid_ground(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    sturdy_at(grid, x, y - 1, z)
}

/// Applies one tree-decorator list (`stump_decorators`/`log_decorators`)
/// against `logs`, dispatching the two kinds [`super::place`] implements.
/// `Beehive` cannot occur here — no shipped `fallen_*_tree` config carries
/// one, and vanilla's own registry never attaches a beehive to a fallen
/// tree — so it degrades the same as `Unsupported` rather than getting its
/// own (unreachable) arm.
fn apply_fallen_tree_decorators<R: RandomSource>(
    random: &mut R,
    logs: &[BlockPos],
    decorators: &[Decorator],
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    for decorator in decorators {
        match decorator {
            Decorator::TrunkVine => place_trunk_vine_decorator(random, logs, grid, tags),
            Decorator::AttachedToLogs { probability, block_provider, directions } => {
                place_attached_to_logs_decorator(
                    random,
                    logs,
                    *probability,
                    block_provider,
                    directions,
                    grid,
                    tags,
                );
            }
            Decorator::Beehive { .. } | Decorator::PlaceOnGround { .. } | Decorator::Unsupported => {}
        }
    }
}

/// Vanilla's own fallen-tree feature's place-fallen-tree — a vertical stump plus
/// a horizontal fallen log, reachable from many biomes' `fallen_*_tree`
/// `RandomSelector` branches at a small (~1-1.25%) chance each. A real,
/// distinct feature type: `placeLogBlock` places UNCONDITIONALLY (no
/// `validTreePos` gate the way every trunk placer's own `placeLog` has one)
/// — `canPlaceEntireFallenLog`'s own pre-check is what decides whether
/// placement happens at all, and it draws no RNG of its own, so this
/// function's RNG stream is fixed regardless of that check's outcome.
///
/// RNG order, ported from vanilla's own place-fallen-tree exactly: the stump (one
/// trunk-provider draw, plus its own `stump_decorators`), then ONE
/// `Direction.Plane.HORIZONTAL` draw, ONE `log_length` sample, ONE
/// `nextInt(2)` for the start-position offset — all real draws even when
/// the walk that follows finds no room at all — then, only if the whole
/// log's path checks out, the log itself (one trunk-provider draw per
/// position, unconditional) and its own `log_decorators`.
pub(super) fn place_fallen_tree<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &FallenTreeCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    // Vanilla's own "place stump": its own place-log-block at `origin`, identity axis modifier
    // (leaves the configured — vertical — axis unchanged).
    let Some(stump_state) = cfg.trunk_provider.get_state_id(grid, tags, random, origin) else {
        return;
    };
    grid.set_id_if_in_bounds(origin.x, origin.y, origin.z, stump_state);
    apply_fallen_tree_decorators(random, &[origin], &cfg.stump_decorators, grid, tags);

    // Vanilla's own horizontal-plane random-direction pick — the same NORTH,
    // EAST, SOUTH, WEST index table every horizontal trunk placer in this
    // module already uses.
    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    let direction = STEP[random.next_int_bounded(4) as usize];
    let log_length = cfg.log_length.sample(random) - 2;
    let step_count = 2 + random.next_int_bounded(2);
    let mut log_start = BlockPos {
        x: origin.x + direction.0 * step_count,
        y: origin.y,
        z: origin.z + direction.1 * step_count,
    };

    // `setGroundHeightForFallenLogStartPos`: move up one, then walk down up
    // to 6 times looking for a valid, solid-ground position. No RNG.
    log_start.y += 1;
    for _ in 0..6 {
        if valid_tree_pos(grid, tags, log_start.x, log_start.y, log_start.z)
            && is_over_solid_ground(grid, log_start.x, log_start.y, log_start.z)
        {
            break;
        }
        log_start.y -= 1;
    }

    // `canPlaceEntireFallenLog`: a pure check over the same walk
    // `placeFallenLog` below repeats — no RNG draw either way.
    if log_length > 0 {
        let mut gap = 0;
        let mut ok = true;
        for i in 0..log_length {
            let pos = BlockPos {
                x: log_start.x + direction.0 * i,
                y: log_start.y,
                z: log_start.z + direction.1 * i,
            };
            if !valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
                ok = false;
                break;
            }
            if is_over_solid_ground(grid, pos.x, pos.y, pos.z) {
                gap = 0;
            } else {
                gap += 1;
                if gap > 2 {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            return;
        }
    }

    // `placeFallenLog`: unconditional placement, sideways axis from
    // `direction`'s own axis (`RotatedPillarBlock.AXIS`).
    let axis = if direction.0 != 0 { "x" } else { "z" };
    let mut fallen_log = Vec::with_capacity(log_length.max(0) as usize);
    for i in 0..log_length.max(0) {
        let pos = BlockPos {
            x: log_start.x + direction.0 * i,
            y: log_start.y,
            z: log_start.z + direction.1 * i,
        };
        if let Some(state) = cfg.trunk_provider.get_state_id(grid, tags, random, pos) {
            let state = tags.rewrite(grid.interner(), state, Rewrite::Axis(axis)).unwrap_or(state);
            grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
            fallen_log.push(pos);
        }
    }
    apply_fallen_tree_decorators(random, &fallen_log, &cfg.log_decorators, grid, tags);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::feature::top_layer::StatePredicate;
    use crate::rng::LegacyRandomSource;

    #[test]
    fn potent_sulfur_simple_block_uses_state_survival_not_vegetation_support() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(5, 69, 5, "minecraft:deepslate[axis=y]".to_string());
        grid.seed(5, 70, 5, "minecraft:sulfur".to_string());
        grid.seed(5, 71, 5, "minecraft:water[level=0]".to_string());
        grid.seed(6, 69, 6, "minecraft:deepslate[axis=y]".to_string());
        grid.seed(6, 70, 6, "minecraft:air".to_string());
        grid.seed(6, 71, 6, "minecraft:water[level=0]".to_string());
        let mut tags = VegTags::default();
        tags.supports_vegetation.insert("minecraft:grass_block".to_string());
        let potent = BlockStateProvider::Simple(
            "minecraft:potent_sulfur[potent_sulfur_state=wet]".to_string(),
        );
        let grass = BlockStateProvider::Simple("minecraft:short_grass".to_string());
        let mut random = LegacyRandomSource::new(1);

        super::super::place::place_simple_block(
            &mut random,
            BlockPos { x: 5, y: 70, z: 5 },
            &potent,
            &mut grid,
            &tags,
        );
        assert_eq!(
            grid.get(5, 70, 5),
            "minecraft:potent_sulfur[potent_sulfur_state=wet]",
            "potent sulfur has its own support-free survival rule"
        );

        super::super::place::place_simple_block(
            &mut random,
            BlockPos { x: 6, y: 70, z: 6 },
            &grass,
            &mut grid,
            &tags,
        );
        assert_eq!(
            grid.get(6, 70, 6),
            "minecraft:air",
            "vegetation still requires a supports_vegetation block below"
        );
    }

    #[test]
    fn simple_block_survival_uses_exact_state_capabilities() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in 0..16 { for z in 0..16 { for y in 60..80 { grid.seed(x, y, z, "minecraft:air".to_string()); } } }
        let mut tags = VegTags::default();
        tags.simple_block_support = super::super::config::SimpleBlockSupport {
            solid_render: StatePredicate::new(["minecraft:stone".to_string()].into_iter().collect(), HashMap::new()),
            sturdy_up: StatePredicate::new(["minecraft:stone".to_string()].into_iter().collect(), HashMap::new()),
            center_support_down: StatePredicate::new(HashSet::new(), [("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]".to_string(), true)].into_iter().collect()),
            fire_flammable: StatePredicate::new(["minecraft:oak_planks".to_string()].into_iter().collect(), HashMap::new()),
        };
        let pos = BlockPos { x: 5, y: 70, z: 5 };
        let survives = |grid: &mut VegGrid, state: &str| {
            grid.seed(pos.x, pos.y, pos.z, state.to_string());
            simple_block_can_survive(grid, &tags, grid.get_id(pos.x, pos.y, pos.z), pos)
        };

        grid.seed(5, 69, 5, "minecraft:stone".to_string());
        assert!(survives(&mut grid, "minecraft:brown_mushroom"), "solid-render stone supports a mushroom during unlit decoration");
        assert!(survives(&mut grid, "minecraft:leaf_litter[segment_amount=1,facing=north]"), "the exact full-up state supports leaf litter");
        grid.seed(5, 71, 5, "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]".to_string());
        assert!(survives(&mut grid, "minecraft:spore_blossom"), "center-down support is independent of full-up support");
        grid.seed(5, 69, 5, "minecraft:air".to_string());
        grid.seed(6, 70, 5, "minecraft:oak_planks".to_string());
        assert!(survives(&mut grid, "minecraft:fire[age=0,east=false,north=false,south=false,up=false,west=false]"), "a flammable side neighbour supports fire without a sturdy floor");
    }

    #[test]
    fn bundled_simple_block_outputs_have_a_complete_survival_family_audit() {
        use std::path::Path;

        fn collect_provider_states(value: &serde_json::Value, out: &mut HashSet<String>) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(name) = object.get("Name").and_then(serde_json::Value::as_str) {
                        out.insert(name.to_owned());
                    }
                    for child in object.values() { collect_provider_states(child, out); }
                }
                serde_json::Value::Array(values) => for child in values { collect_provider_states(child, out); },
                _ => {}
            }
        }
        fn visit(value: &serde_json::Value, out: &mut HashSet<String>) -> bool {
            match value {
                serde_json::Value::Object(object) => {
                    let mut found = false;
                    if object.get("type").and_then(serde_json::Value::as_str) == Some("minecraft:simple_block") {
                        found = true;
                        // A selector can nest this record beneath another feature;
                        // walk the matched record itself so no provider level is
                        // accidentally skipped (forest flowers exercises this).
                        collect_provider_states(value, out);
                    }
                    for child in object.values() { found |= visit(child, out); }
                    found
                }
                serde_json::Value::Array(values) => {
                    let mut found = false;
                    for child in values {
                        found |= visit(child, out);
                    }
                    found
                }
                _ => false,
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen/configured_feature");
        let mut records = 0;
        let mut outputs = HashSet::new();
        for entry in std::fs::read_dir(root).expect("bundled configured-feature directory") {
            let path = entry.expect("directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
            let text = std::fs::read_to_string(path).expect("bundled configured-feature JSON");
            if visit(&serde_json::from_str(&text).expect("valid configured-feature JSON"), &mut outputs) { records += 1; }
        }
        assert_eq!(records, 36, "the audit must traverse every bundled simple_block record");
        assert_eq!(outputs.len(), 46, "the audit must retain every distinct output state base: {outputs:?}");

        let expected: HashMap<&str, SimpleBlockSurvival> = [
            ("minecraft:dead_bush", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
            ("minecraft:short_dry_grass", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
            ("minecraft:tall_dry_grass", SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
            ("minecraft:azalea", SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
            ("minecraft:flowering_azalea", SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
            ("minecraft:crimson_roots", SimpleBlockSurvival::TagBelow(Tag::SupportsCrimsonRoots)),
            ("minecraft:brown_mushroom", SimpleBlockSurvival::Mushroom), ("minecraft:red_mushroom", SimpleBlockSurvival::Mushroom),
            ("minecraft:small_dripleaf", SimpleBlockSurvival::SmallDripleaf), ("minecraft:lily_pad", SimpleBlockSurvival::LilyPad),
            ("minecraft:spore_blossom", SimpleBlockSurvival::CeilingCenter), ("minecraft:leaf_litter", SimpleBlockSurvival::SturdyBelow),
            ("minecraft:moss_carpet", SimpleBlockSurvival::NonAirBelow), ("minecraft:pale_moss_carpet", SimpleBlockSurvival::NonAirBelow),
            ("minecraft:fire", SimpleBlockSurvival::Fire), ("minecraft:soul_fire", SimpleBlockSurvival::TagBelow(Tag::SoulFireBaseBlocks)),
            ("minecraft:melon", SimpleBlockSurvival::Always), ("minecraft:pumpkin", SimpleBlockSurvival::Always),
            ("minecraft:tuff", SimpleBlockSurvival::Always), ("minecraft:potent_sulfur", SimpleBlockSurvival::Always),
        ].into_iter().collect();
        for output in &outputs {
            let actual = simple_block_survival_rule(output);
            let intended = expected.get(output.as_str()).copied().unwrap_or(SimpleBlockSurvival::Vegetation);
            assert_eq!(actual, intended, "{output} must use its audited survival family");
        }
        assert_eq!(SIMPLE_BLOCK_SURVIVAL.len(), expected.len(), "no exceptional state may silently take the vegetation fallback");
    }
}
