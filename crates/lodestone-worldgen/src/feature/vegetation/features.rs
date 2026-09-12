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
//! | full upward face support | the generated six-direction face-occlusion table | local ids bind to canonical ids once when interned |
//! | `state.isSolid()` | same | same |
//! | `canSurvive` | the target's own family rule, or "support below is not air" | vegetation blocks use `#supports_vegetation`; support-free blocks such as potent sulfur opt out |
//! | `level.getSeaLevel()` | [`SEA_LEVEL`] | the overworld constant; a preset that moves it would need this parameterised |
//! | `scheduleTick` | dropped | there is no tick queue at generation time; the *block* still lands |
//! | block entities | dropped | the generator has no block-entity layer yet |
//!
//! The face lookup is exact for built-in states. A state outside the canonical
//! table remains visible, which is the conservative result for a shape whose
//! occlusion has not been measured.
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
use std::sync::Arc;

use crate::feature::{BlockPos, IntProvider};
use crate::interner::StateId;
use crate::rng::RandomSource;
use lodestone_data::block_states::StateId as CanonicalStateId;
use lodestone_data::block_survival;
use lodestone_data::collision_shapes;
use lodestone_data::face_occlusion::{self, Face};
use lodestone_worldgen_core::hash::FastSet;

use super::config::{
    BlockPredicate, BlockStateProvider, Decorator, PlacedRef, VegTags, blocks_motion, is_air, is_fluid,
};
use super::grid::VegGrid;
use super::ids::{Rewrite, Tag, tag_at};
use super::place::{place_attached_to_logs_decorator, place_trunk_vine_decorator};
use super::tree::valid_tree_pos;

/// A membership index paired with an explicit insertion log. Only vegetation
/// patches need this: their successful cells are traversed while consuming the
/// feature's random stream, so a set's native hash-table iteration is not an
/// acceptable ordering contract. `members` answers duplicate checks in
/// expected O(1); `entries` is the only production traversal path.
#[derive(Debug, Default)]
struct CompatBlockPosSet {
    members: FastSet<BlockPos>,
    /// Successful insertions in encounter order. Feature placement consumes
    /// random draws while iterating the set, so bucket order is an
    /// implementation detail and cannot determine the observable sequence.
    entries: Vec<BlockPos>,
}

impl CompatBlockPosSet {
    fn insert(&mut self, pos: BlockPos) -> bool {
        if !self.members.insert(pos) {
            return false;
        }
        self.entries.push(pos);
        true
    }

    #[cfg(test)]
    fn hash(pos: BlockPos) -> i32 {
        pos.y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x)
    }

    #[cfg(test)]
    fn spread(hash: i32) -> u32 {
        let hash = hash as u32;
        hash ^ (hash >> 16)
    }

    #[cfg(test)]
    fn bucket(hash: u32, capacity: usize) -> usize { hash as usize & (capacity - 1) }

    #[cfg(test)]
    fn bucket_capacity(&self) -> usize {
        let mut capacity = 16;
        while self.entries.len() > capacity * 3 / 4 {
            capacity *= 2;
        }
        capacity
    }

    fn order(&self) -> Vec<BlockPos> {
        self.entries.clone()
    }

    /// Reconstruct the bucket traversal that would be the wrong production
    /// order. This is test-only negative-control evidence: it must differ from
    /// the insertion log for a collision-heavy input.
    #[cfg(test)]
    fn bucket_order(&self) -> Vec<BlockPos> {
        let capacity = self.bucket_capacity();
        let mut buckets = vec![Vec::new(); capacity];
        for &pos in &self.entries {
            buckets[Self::bucket(Self::spread(Self::hash(pos)), capacity)].push(pos);
        }
        buckets.into_iter().flatten().collect()
    }
}

impl IntoIterator for CompatBlockPosSet {
    type Item = BlockPos;
    type IntoIter = std::vec::IntoIter<BlockPos>;

    fn into_iter(self) -> Self::IntoIter { self.entries.into_iter() }
}

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

/// Whether the support cell presents the face required by a vegetation patch.
///
/// The patch body asks for full upward support on a floor and center support
/// on the downward face of a ceiling. Those are distinct state predicates;
/// the broader motion-blocking approximation used by [`sturdy_at`] accepts
/// blocks whose collision occupies a cell without providing either face.
/// Unknown fixture or extension states fail closed because no exact support
/// fact has been captured for them.
fn vegetation_patch_supports(
    grid: &VegGrid,
    x: i32,
    y: i32,
    z: i32,
    surface: CaveSurface,
) -> bool {
    let state = grid.get_id(x, y, z);
    let Some(canonical) = grid.interner().canonical_id(state) else {
        return false;
    };
    match surface {
        CaveSurface::Floor => block_survival::sturdy_up(canonical),
        CaveSurface::Ceiling => block_survival::center_support_down(canonical),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimpleBlockSurvival {
    Vegetation,
    TagBelow(Tag),
    NetherFungus,
    NetherSprouts,
    NetherRoots,
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
    // These states are selected by nether_forest_vegetation as well as being
    // placeable blocks. Their support tag includes nylium (and fungi also
    // accept mycelium), while the ordinary vegetation tag intentionally does
    // not. Keep the family rule here so a weighted provider's selected state
    // participates in the same occupancy gate as the real block.
    let rule = match base {
        "minecraft:crimson_fungus" | "minecraft:warped_fungus" => SimpleBlockSurvival::NetherFungus,
        "minecraft:nether_sprouts" => SimpleBlockSurvival::NetherSprouts,
        // The warped-root support tag has the same closure as the sprouts
        // tag, but there is no dedicated bitset slot for it yet. Keep the
        // exact support family here rather than falling back to ordinary
        // overworld vegetation (which would reject roots on nylium).
        "minecraft:warped_roots" => SimpleBlockSurvival::NetherRoots,
        _ => simple_block_survival_rule(base),
    };
    match rule {
        SimpleBlockSurvival::Vegetation => tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::TagBelow(tag) => tag_at(grid, tags, tag, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::NetherFungus => nether_vegetation_can_survive(grid, tags, pos, true),
        SimpleBlockSurvival::NetherSprouts => nether_vegetation_can_survive(grid, tags, pos, false),
        SimpleBlockSurvival::NetherRoots => nether_vegetation_can_survive(grid, tags, pos, false),
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

/// Nether forest plants use support tags that extend the ordinary vegetation
/// floor with nylium. Fungi additionally accept mycelium. This is deliberately
/// a base-name check only for the Nether forest blocks whose support tags have
/// this shape; other vegetation must continue to use `supports_vegetation`.
fn nether_vegetation_can_survive(
    grid: &VegGrid,
    tags: &VegTags,
    pos: BlockPos,
    fungus: bool,
) -> bool {
    let below = base_at(grid, pos.x, pos.y - 1, pos.z);
    tags.supports_vegetation.contains(below)
        || below == "minecraft:crimson_nylium"
        || below == "minecraft:warped_nylium"
        || below == "minecraft:soul_soil"
        || (fungus && below == "minecraft:mycelium")
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

fn can_place_speleothem_pool(
    grid: &VegGrid,
    tags: &VegTags,
    cfg: &SpeleothemClusterCfg,
    floor: BlockPos,
) -> bool {
    let floor_state = base_at(grid, floor.x, floor.y, floor.z);
    if floor_state == "minecraft:water"
        || floor_state == cfg.base_block
        || floor_state == cfg.pointed_block
        || water_at(grid, floor.x, floor.y + 1, floor.z)
    {
        return false;
    }
    let supported = |x: i32, y: i32, z: i32| {
        tags.base_stone_overworld.contains(base_at(grid, x, y, z))
            || water_at(grid, x, y, z)
    };
    HORIZONTAL
        .iter()
        .all(|&(dx, dz)| supported(floor.x + dx, floor.y, floor.z + dz))
        && supported(floor.x, floor.y - 1, floor.z)
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
    tags: &VegTags,
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
            let Some(mut column) = scan_speleothem_column(grid, pos, cfg.floor_to_ceiling_search_range) else {
                continue;
            };
            if column.floor.is_none() && column.ceiling.is_none() {
                continue;
            }
            if random.next_float() < wetness {
                if let Some(floor) = column.floor {
                    let floor_pos = BlockPos { y: floor, ..pos };
                    if can_place_speleothem_pool(grid, tags, cfg, floor_pos) {
                        grid.set_if_in_bounds(pos.x, floor, pos.z, "minecraft:water".to_string());
                        column.floor = Some(floor - 1);
                    }
                }
            }
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
    pub state: CanonicalStateId,
    pub requires_block_below: bool,
    pub rock_count: i32,
    pub hole_count: i32,
    pub valid_blocks: HashSet<String>,
}

/// Configuration for magma patches on enclosed underwater floors.
#[derive(Clone, Debug)]
pub struct UnderwaterMagmaCfg {
    pub floor_search_range: i32,
    pub placement_probability_per_valid_position: f32,
    pub placement_radius_around_floor: i32,
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
    pub state: CanonicalStateId,
    pub radius: IntProvider,
}

/// `BlockBlobConfiguration`.
#[derive(Clone, Debug)]
pub struct BlockBlobCfg {
    pub state: CanonicalStateId,
    pub can_place_on: BlockPredicate,
}

/// Contents, rim state, and radii for a floor-held delta patch.
#[derive(Clone, Debug)]
pub struct DeltaCfg {
    pub contents: String,
    pub rim: String,
    pub rim_size: IntProvider,
    pub size: IntProvider,
}

/// Height and horizontal-reach providers for the two basalt-column records.
#[derive(Clone, Debug)]
pub struct BasaltColumnsCfg {
    pub height: IntProvider,
    pub reach: IntProvider,
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

/// The Nether fungus configuration. Unlike the overworld's cap-only mushroom,
/// this feature grows a variable stem, a probabilistic hat and (for wart hats)
/// hanging vines. `replaceable_blocks` is consulted only for plant states while
/// the ordinary replaceability check handles air and other non-solid cells.
#[derive(Clone, Debug)]
pub struct HugeFungusCfg {
    pub valid_base_block: String,
    pub stem_state: String,
    pub hat_state: String,
    pub decor_state: String,
    pub replaceable_blocks: BlockPredicate,
    pub planted: bool,
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

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

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
        let state = cfg.state.canonical_state();
        let state = grid.interner().id_of(&state);
        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
    }
}

fn underwater_floor_y(grid: &VegGrid, pos: BlockPos, search_range: i32) -> Option<i32> {
    if base_at(grid, pos.x, pos.y, pos.z) != "minecraft:water" {
        return None;
    }
    let mut y = pos.y;
    for _ in 0..search_range.saturating_sub(1) {
        y -= 1;
        if base_at(grid, pos.x, y, pos.z) != "minecraft:water" {
            return Some(y);
        }
    }
    None
}

/// Whether a neighboring block leaves the covered face visible from outside.
///
/// The local interner binds each state to its canonical global id when the
/// state is first seen. Unknown fixture or extension states are conservatively
/// treated as visible, because guessing that an unbound shape closes a face
/// would place magma in an enclosure the state table has not proved.
fn visible_from_outside(
    grid: &VegGrid,
    x: i32,
    y: i32,
    z: i32,
    covered_face: Face,
) -> bool {
    let state = grid.get_id(x, y, z);
    grid.interner()
        .canonical_id(state)
        .is_none_or(|canonical| !face_occlusion::occludes(canonical, covered_face))
}

fn valid_underwater_magma_position(grid: &VegGrid, pos: BlockPos) -> bool {
    let target = base_at(grid, pos.x, pos.y, pos.z);
    if is_air(target) || is_fluid(target) {
        return false;
    }
    if visible_from_outside(grid, pos.x, pos.y - 1, pos.z, Face::Up) {
        return false;
    }
    for (dx, dz) in HORIZONTAL {
        let covered_face = match (dx, dz) {
            (0, -1) => Face::South,
            (1, 0) => Face::West,
            (0, 1) => Face::North,
            (-1, 0) => Face::East,
            _ => unreachable!("HORIZONTAL contains only cardinal offsets"),
        };
        if visible_from_outside(
            grid,
            pos.x + dx,
            pos.y,
            pos.z + dz,
            covered_face,
        ) {
            return false;
        }
    }
    true
}

/// Places magma only on a floor reached through a water column, and only at
/// positions whose floor and four horizontal sides are enclosed by complete
/// unit faces from the generated canonical occlusion table.
pub(super) fn place_underwater_magma<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &UnderwaterMagmaCfg,
    grid: &mut VegGrid,
) {
    let Some(floor_y) = underwater_floor_y(grid, pos, cfg.floor_search_range) else {
        return;
    };
    let floor = BlockPos { y: floor_y, ..pos };
    let radius = cfg.placement_radius_around_floor.max(0);
    let magma = grid.interner().id_of("minecraft:magma_block");
    for dz in -radius..=radius {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let candidate = BlockPos {
                    x: floor.x + dx,
                    y: floor.y + dy,
                    z: floor.z + dz,
                };
                if random.next_float() < cfg.placement_probability_per_valid_position
                    && valid_underwater_magma_position(grid, candidate)
                {
                    grid.set_id_if_in_bounds(candidate.x, candidate.y, candidate.z, magma);
                }
            }
        }
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
                    if let Some(state) = cfg.provider.get_state_id(grid, tags, random, at) {
                        grid.set_id_if_in_bounds(x, y, z, state);
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
    if let Some(state) = provider.get_state_id(grid, tags, random, pos) {
        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
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
        let Some(state) = cfg.provider.get_state_id(grid, tags, random, target) else {
            continue;
        };
        if air_at(grid, target.x, target.y, target.z)
            && target.y > grid.min_y
            && simple_block_can_survive(grid, tags, state, target)
        {
            grid.set_id_if_in_bounds(target.x, target.y, target.z, state);
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
    let state = cfg.state.canonical_state();
    let state = grid.interner().id_of(&state);
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
                        grid.set_id_if_in_bounds(x, y, z, state);
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

fn within_manhattan_xz(mut visit: impl FnMut(i32, i32), reach_x: i32, reach_z: i32) {
    let max_depth = reach_x + reach_z;
    for depth in 0..=max_depth {
        let max_x = reach_x.min(depth);
        for x in -max_x..=max_x {
            let z = depth - x.abs();
            if z > reach_z {
                continue;
            }
            visit(x, z);
            if z != 0 {
                visit(x, -z);
            }
        }
    }
}

fn delta_clear(grid: &VegGrid, pos: BlockPos, contents: &str) -> bool {
    const CANNOT_REPLACE: &[&str] = &[
        "minecraft:bedrock",
        "minecraft:nether_bricks",
        "minecraft:nether_brick_fence",
        "minecraft:nether_brick_stairs",
        "minecraft:nether_wart",
        "minecraft:chest",
        "minecraft:spawner",
    ];
    let contents = super::base_id(contents);
    let state = base_at(grid, pos.x, pos.y, pos.z);
    if state == contents || CANNOT_REPLACE.contains(&state) {
        return false;
    }
    for (dx, dy, dz) in DIRECTIONS {
        let air = air_at(grid, pos.x + dx, pos.y + dy, pos.z + dz);
        if (air && dy != 1) || (!air && dy == 1) {
            return false;
        }
    }
    true
}

/// Places one basalt-deltas delta: a floor-held contents patch with an optional rim.
pub(super) fn place_delta<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &DeltaCfg,
    grid: &mut VegGrid,
) {
    let spawn_rim = random.next_double() < 0.9;
    let rim_x = if spawn_rim { cfg.rim_size.sample(random) } else { 0 };
    let rim_z = if spawn_rim { cfg.rim_size.sample(random) } else { 0 };
    let has_rim = spawn_rim && rim_x != 0 && rim_z != 0;
    let radius_x = cfg.size.sample(random);
    let radius_z = cfg.size.sample(random);
    let limit = radius_x.max(radius_z);
    within_manhattan_xz(
        |dx, dz| {
            if dx.abs() + dz.abs() > limit {
                return;
            }
            let pos = BlockPos { x: origin.x + dx, y: origin.y, z: origin.z + dz };
            if !delta_clear(grid, pos, &cfg.contents) {
                return;
            }
            if has_rim {
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.rim.clone());
            }
            let contents = BlockPos { x: pos.x + rim_x, y: pos.y, z: pos.z + rim_z };
            if delta_clear(grid, contents, &cfg.contents) {
                grid.set_if_in_bounds(contents.x, contents.y, contents.z, cfg.contents.clone());
            }
        },
        radius_x,
        radius_z,
    );
}

fn basalt_cannot_place_on(state: &str) -> bool {
    matches!(
        state,
        "minecraft:lava"
            | "minecraft:bedrock"
            | "minecraft:magma_block"
            | "minecraft:soul_sand"
            | "minecraft:nether_bricks"
            | "minecraft:nether_brick_fence"
            | "minecraft:nether_brick_stairs"
            | "minecraft:nether_wart"
            | "minecraft:chest"
            | "minecraft:spawner"
    )
}

fn air_or_lava_ocean(grid: &VegGrid, pos: BlockPos) -> bool {
    air_at(grid, pos.x, pos.y, pos.z)
        || (base_at(grid, pos.x, pos.y, pos.z) == "minecraft:lava" && pos.y <= 32)
}

fn basalt_can_place_at(grid: &VegGrid, pos: BlockPos) -> bool {
    air_or_lava_ocean(grid, pos)
        && !basalt_cannot_place_on(base_at(grid, pos.x, pos.y - 1, pos.z))
        && !air_at(grid, pos.x, pos.y - 1, pos.z)
}

fn basalt_find_surface(grid: &VegGrid, mut pos: BlockPos, mut limit: i32) -> Option<BlockPos> {
    while pos.y > grid.min_y + 1 && limit > 0 {
        limit -= 1;
        if basalt_can_place_at(grid, pos) {
            return Some(pos);
        }
        pos.y -= 1;
    }
    None
}

fn basalt_find_air(grid: &VegGrid, mut pos: BlockPos, mut limit: i32) -> Option<BlockPos> {
    while pos.y < grid.min_y + grid.height && limit > 0 {
        limit -= 1;
        let state = base_at(grid, pos.x, pos.y, pos.z);
        if basalt_cannot_place_on(state) {
            return None;
        }
        if is_air(state) {
            return Some(pos);
        }
        pos.y += 1;
    }
    None
}

fn place_basalt_column(grid: &mut VegGrid, origin: BlockPos, height: i32, reach: i32) {
    for x in origin.x - reach..=origin.x + reach {
        for z in origin.z - reach..=origin.z + reach {
            let distance = (x - origin.x).abs() + (z - origin.z).abs();
            let at = BlockPos { x, y: origin.y, z };
            let Some(mut cursor) = (if air_or_lava_ocean(grid, at) {
                basalt_find_surface(grid, at, distance)
            } else {
                basalt_find_air(grid, at, distance)
            }) else {
                continue;
            };
            let mut blocks = height - distance / 2;
            while blocks >= 0 {
                if air_or_lava_ocean(grid, cursor) {
                    grid.set_if_in_bounds(cursor.x, cursor.y, cursor.z, "minecraft:basalt[axis=y]".to_string());
                    cursor.y += 1;
                } else if base_at(grid, cursor.x, cursor.y, cursor.z) == "minecraft:basalt" {
                    cursor.y += 1;
                } else {
                    break;
                }
                blocks -= 1;
            }
        }
    }
}

fn visit_basalt_column_candidates<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    height: i32,
    clustered: bool,
    cfg: &BasaltColumnsCfg,
    mut visit: impl FnMut(BlockPos, i32, i32),
) {
    let spread = height.min(if clustered { 5 } else { 8 });
    let count = if clustered { 50 } else { 15 };
    for _ in 0..count {
        let x = origin.x - spread + random.next_int_bounded(spread * 2 + 1);
        // The sampled Y range has one value, but that draw is still part of
        // the feature's stream before the Z coordinate is chosen.
        let _ = random.next_int_bounded(1);
        let z = origin.z - spread + random.next_int_bounded(spread * 2 + 1);
        let pos = BlockPos { x, y: origin.y, z };
        let blocks = height - (pos.x - origin.x).abs() - (pos.z - origin.z).abs();
        if blocks >= 0 {
            visit(pos, blocks, cfg.reach.sample(random));
        }
    }
}

/// Places a basalt-deltas column cluster. These records occur only in the
/// Nether, whose lava level is 32.
pub(super) fn place_basalt_columns<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &BasaltColumnsCfg,
    grid: &mut VegGrid,
) {
    if !basalt_can_place_at(grid, origin) {
        return;
    }
    let height = cfg.height.sample(random);
    let clustered = random.next_float() < 0.9;
    visit_basalt_column_candidates(random, origin, height, clustered, cfg, |pos, blocks, reach| {
        place_basalt_column(grid, pos, blocks, reach);
    });
}

/// Manhattan-ball replacement of one target block.
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
    let state = cfg.state.canonical_state();
    let state = grid.interner().id_of(&state);
    for dx in -rx..=rx {
        for dy in -ry..=ry {
            for dz in -rz..=rz {
                if dx.abs() + dy.abs() + dz.abs() > max_r {
                    continue;
                }
                let (x, y, z) = (pos.x + dx, cy + dy, pos.z + dz);
                if base_at(grid, x, y, z) == cfg.target {
                    grid.set_id_if_in_bounds(x, y, z, state);
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
    place_growing_column_with_age(random, start, total, grid, upwards, 17, 25, None);
}

fn place_growing_column_with_age<R: RandomSource>(
    random: &mut R,
    start: BlockPos,
    total: i32,
    grid: &mut VegGrid,
    upwards: bool,
    min_age: i32,
    max_age: i32,
    owner: Option<BlockPos>,
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
                let age = next_int_between(random, min_age, max_age);
                if owner.is_none_or(|origin| in_source_chunk(origin, pos)) {
                    grid.set_if_in_bounds(pos.x, pos.y, pos.z, format!("{head}[age={age}]"));
                }
                return;
            }
            if owner.is_none_or(|origin| in_source_chunk(origin, pos)) {
                grid.set_if_in_bounds(pos.x, pos.y, pos.z, plant.to_string());
            }
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
    let interner = Arc::clone(grid.interner());
    let water_base = interner.base_of(interner.id_of("minecraft:water"));
    let lava_base = interner.base_of(interner.id_of("minecraft:lava"));
    let cave_air = interner.id_of("minecraft:cave_air");
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
    let Some(fluid) = cfg.fluid.get_state_id(grid, tags, random, origin) else {
        return;
    };
    let fluid_base = interner.base_of(fluid);
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
                let existing = grid.get_id(at.x, at.y, at.z);
                let existing_base = interner.base_of(existing);
                let existing_is_fluid = existing_base == water_base || existing_base == lava_base;
                if yy >= 4 && existing_is_fluid {
                    return;
                }
                if yy < 4 && !sturdy_at(grid, at.x, at.y, at.z) && fluid_base != existing_base {
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
                        cave_air
                    } else {
                        fluid
                    };
                    grid.set_id_if_in_bounds(at.x, at.y, at.z, state);
                }
            }
        }
    }
    let Some(barrier) = cfg.barrier.get_state_id(grid, tags, random, origin) else {
        return;
    };
    if grid.is_air_id(interner.base_of(barrier)) {
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
                    grid.set_id_if_in_bounds(at.x, at.y, at.z, barrier);
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

/// The feature's clearance rule admits only air and leaves. It is deliberately
/// separate from the write rule below: a leaf canopy may be displaced after a
/// valid mushroom site is found, while an arbitrary solid block cancels the
/// whole attempt before any partial cap or stem is written.
fn valid_mushroom_pos(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    tag_at(grid, tags, Tag::Air, x, y, z) || tag_at(grid, tags, Tag::Leaves, x, y, z)
}

/// Cap and stem blocks replace air and the dedicated mushroom-replaceable set.
/// This is wider than [`valid_mushroom_pos`] because the placement body is
/// allowed to overwrite leaves and other replaceable vegetation once the
/// preflight clearance scan has passed.
fn replaceable_mushroom_pos(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    tag_at(grid, tags, Tag::Air, x, y, z)
        || tag_at(grid, tags, Tag::ReplaceableByMushrooms, x, y, z)
}

fn mushroom_valid_radius(cfg: &HugeMushroomCfg, height: i32, y: i32) -> i32 {
    match cfg.kind {
        HugeMushroomKind::Brown => (y > 3).then_some(cfg.foliage_radius).unwrap_or(0),
        // Red uses the configured radius for each of its three lower cap rows
        // and for the top row; the stem-only rows below that stay radius zero.
        // This is the same clearance envelope as its cap geometry, including
        // cells that the lower cap later omits at the corners.
        HugeMushroomKind::Red => (y >= height - 3).then_some(cfg.foliage_radius).unwrap_or(0),
    }
}

/// Huge-mushroom placement shares one bounded `0..3` height draw plus four,
/// with a one-in-twelve second draw that doubles the height. Keeping the
/// deterministic body separate lets the layout tests exercise the bundled cap
/// records without coupling their assertions to a particular random-source seed.
pub(super) fn place_huge_mushroom<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &HugeMushroomCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let mut height = random.next_int_bounded(3) + 4;
    if random.next_int_bounded(12) == 0 {
        height *= 2;
    }
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
    // The height guard is relative to the source generator's depth, not the
    // widened receiving window used by Nether decoration.
    let max_y = grid.generation_top();
    if pos.y < grid.min_y + 1
        || pos.y + height + 1 > max_y
        || !cfg.can_place_on.test(
            grid,
            tags,
            BlockPos { x: pos.x, y: pos.y - 1, z: pos.z },
        )
    {
        return;
    }

    // The feature's validity scan is intentionally broader than either cap's
    // final cut-out shape: a blocked skipped corner still rejects the attempt
    // before any stem write occurs. Brown keeps its full radius for every
    // layer above the first four; red checks the three lower cap rows and top
    // row too.
    for y in 0..=height {
        let radius = mushroom_valid_radius(cfg, height, y);
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if !valid_mushroom_pos(grid, tags, pos.x + dx, pos.y + y, pos.z + dz) {
                    return;
                }
            }
        }
    }

    let mut place_cap = |at: BlockPos, west: bool, east: bool, north: bool, south: bool, up: bool| {
        if let Some(state) = cfg
            .cap_provider
            .get_state_for_mushroom_cap(grid, tags, random, pos)
        {
            if replaceable_mushroom_pos(grid, tags, at.x, at.y, at.z) {
                grid.set_if_in_bounds(
                    at.x,
                    at.y,
                    at.z,
                    mushroom_cap_state(state, west, east, north, south, up),
                );
            }
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
            let center = cfg.foliage_radius - 2;
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
                        dx < -center,
                        dx > center,
                        dz < -center,
                        dz > center,
                        y >= height - 1,
                    );
                }
            }
        }
    }

    // The cap is generated before the trunk. That ordering is observable for
    // weighted or noise-selected providers even though the bundled providers
    // are simple states.
    for y in 0..height {
        let at = BlockPos { x: pos.x, y: pos.y + y, z: pos.z };
        if let Some(state) = cfg.stem_provider.get_state_id(grid, tags, random, pos) {
            if replaceable_mushroom_pos(grid, tags, at.x, at.y, at.z) {
                grid.set_id_if_in_bounds(at.x, at.y, at.z, state);
            }
        }
    }
}

/// Places one Nether fungus. The height, rare doubled height, broad-stem roll,
/// per-row hat radius and per-block material rolls intentionally stay in the
/// reference order: later decoration entries share this random stream.
pub(super) fn place_huge_fungus<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &HugeFungusCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if base_at(grid, origin.x, origin.y - 1, origin.z) != cfg.valid_base_block {
        return;
    }

    let height_roll = random.next_int_bounded(10);
    let double_roll = random.next_int_bounded(12);
    let mut total_height = height_roll + 4;
    if double_roll == 0 {
        total_height *= 2;
    }
    // The source generator's depth, rather than the widened receiving window,
    // is the upper bound for natural Nether fungus placement.
    let max_y = grid.generation_top();
    if !cfg.planted && origin.y + total_height + 1 >= max_y {
        return;
    }

    // Planted fungi never roll the broad-stem variant. The conditional is
    // important for the shared stream: the planted configuration must not
    // consume a draw that its source feature does not make.
    let huge_roll = random.next_float();
    let huge = !cfg.planted && huge_roll < 0.06;
    grid.set_if_in_bounds(origin.x, origin.y, origin.z, "minecraft:air".to_string());

    let stem_radius: i32 = if huge { 1 } else { 0 };
    for dx in -stem_radius..=stem_radius {
        for dz in -stem_radius..=stem_radius {
            let corner = huge && dx.abs() == stem_radius && dz.abs() == stem_radius;
            for dy in 0..total_height {
                let at = BlockPos { x: origin.x + dx, y: origin.y + dy, z: origin.z + dz };
                if !huge_fungus_replaceable(grid, cfg, tags, at, true) {
                    continue;
                }
                if !cfg.planted && corner && random.next_float() >= 0.1 {
                    continue;
                }
                grid.set_if_in_bounds(at.x, at.y, at.z, cfg.stem_state.clone());
            }
        }
    }

    let hat_height = (random.next_int_bounded(1 + total_height / 3) + 5).min(total_height);
    let hat_start_y = total_height - hat_height;
    let place_vines = super::base_id(&cfg.hat_state) == "minecraft:nether_wart_block";
    for dy in hat_start_y..=total_height {
        let mut radius = if dy < total_height - random.next_int_bounded(3) { 2 } else { 1 };
        if hat_height > 8 && dy < hat_start_y + 4 {
            radius = 3;
        }
        if huge {
            radius += 1;
        }
        let is_hat_bottom = dy < hat_start_y + 3;
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let edge_x = dx == -radius || dx == radius;
                let edge_z = dz == -radius || dz == radius;
                let inside = !edge_x && !edge_z && dy != total_height;
                let corner = edge_x && edge_z;
                let at = BlockPos { x: origin.x + dx, y: origin.y + dy, z: origin.z + dz };
                if !huge_fungus_replaceable(grid, cfg, tags, at, false) {
                    continue;
                }
                if is_hat_bottom {
                    if !inside {
                        place_huge_fungus_drop(random, at, cfg, place_vines, grid);
                    }
                } else if inside {
                    place_huge_fungus_hat_block(random, at, cfg, 0.1, 0.2, if place_vines { 0.1 } else { 0.0 }, grid);
                } else if corner {
                    place_huge_fungus_hat_block(random, at, cfg, 0.01, 0.7, if place_vines { 0.083 } else { 0.0 }, grid);
                } else {
                    place_huge_fungus_hat_block(random, at, cfg, 0.0005, 0.98, if place_vines { 0.07 } else { 0.0 }, grid);
                }
            }
        }
    }
}

/// Huge-fungus placement reads the padded receiving view, but its body is
/// owned by the chunk containing the feature origin. Keep this check local to
/// the feature so ordinary border-spilling vegetation retains its own contract.
fn in_source_chunk(origin: BlockPos, pos: BlockPos) -> bool {
    origin.x.div_euclid(16) == pos.x.div_euclid(16)
        && origin.z.div_euclid(16) == pos.z.div_euclid(16)
}

fn huge_fungus_replaceable(
    grid: &VegGrid,
    cfg: &HugeFungusCfg,
    tags: &VegTags,
    pos: BlockPos,
    check_plants: bool,
) -> bool {
    let base = base_at(grid, pos.x, pos.y, pos.z);
    // The ordinary replaceability flag is narrower than `blocks_motion`:
    // non-colliding
    // blocks such as mushrooms and hanging vines are still protected during
    // the hat pass. Using the motion approximation here made a preceding
    // vegetation write disappear and, more importantly, skipped its random
    // draws, shifting every later cap and vine. Keep the ordinary replaceable
    // set separate; the configured predicate is the deliberate opt-in for
    // non-replaceable plants during the stem pass.
    can_be_replaced(base) || (check_plants && cfg.replaceable_blocks.test(grid, tags, pos))
}

/// The built-in block-state `canBeReplaced` flag for blocks that can occur in
/// a generated decoration region. This is intentionally not the same as
/// [`blocks_motion`]: several plant blocks have no collision but are not
/// generally replaceable, and the huge-fungus hat must preserve those blocks.
fn can_be_replaced(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air"
            | "minecraft:water"
            | "minecraft:lava"
            | "minecraft:short_grass"
            | "minecraft:fern"
            | "minecraft:dead_bush"
            | "minecraft:bush"
            | "minecraft:short_dry_grass"
            | "minecraft:tall_dry_grass"
            | "minecraft:seagrass"
            | "minecraft:tall_seagrass"
            | "minecraft:fire"
            | "minecraft:soul_fire"
            | "minecraft:snow"
            | "minecraft:vine"
            | "minecraft:glow_lichen"
            | "minecraft:resin_clump"
            | "minecraft:light"
            | "minecraft:tall_grass"
            | "minecraft:large_fern"
            | "minecraft:structure_void"
            | "minecraft:void_air"
            | "minecraft:cave_air"
            | "minecraft:bubble_column"
            | "minecraft:warped_roots"
            | "minecraft:nether_sprouts"
            | "minecraft:crimson_roots"
            | "minecraft:leaf_litter"
            | "minecraft:hanging_roots"
    )
}

fn place_huge_fungus_hat_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &HugeFungusCfg,
    decor_probability: f32,
    hat_probability: f32,
    vines_probability: f32,
    grid: &mut VegGrid,
) {
    if random.next_float() < decor_probability {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.decor_state.clone());
    } else if random.next_float() < hat_probability {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.hat_state.clone());
        if random.next_float() < vines_probability {
            try_place_huge_fungus_vines(random, pos, grid);
        }
    }
}

fn place_huge_fungus_drop<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &HugeFungusCfg,
    place_vines: bool,
    grid: &mut VegGrid,
) {
    if base_at(grid, pos.x, pos.y - 1, pos.z) == super::base_id(&cfg.hat_state) {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.hat_state.clone());
    } else if random.next_float() < 0.15 {
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, cfg.hat_state.clone());
        if place_vines && random.next_int_bounded(11) == 0 {
            try_place_huge_fungus_vines(random, pos, grid);
        }
    }
}

fn try_place_huge_fungus_vines<R: RandomSource>(random: &mut R, hat_pos: BlockPos, grid: &mut VegGrid) {
    let below = BlockPos { x: hat_pos.x, y: hat_pos.y - 1, z: hat_pos.z };
    if !air_at(grid, below.x, below.y, below.z) {
        return;
    }
    let mut goal = random.next_int_bounded(5) + 1;
    if random.next_int_bounded(7) == 0 {
        goal *= 2;
    }
    // Keep the temporary vine write visible to later placements in this source
    // pass; the mixed dispatcher filters source-crossing fungus writes when it
    // commits the source's final spills.
    place_growing_column_with_age(random, below, goal, grid, false, 23, 25, None);
}

fn west_or_east(value: i32, radius: i32) -> bool {
    value == -radius || value == radius
}

fn north_or_south(value: i32, radius: i32) -> bool {
    value == -radius || value == radius
}

/// Places a vegetation patch (and its waterlogged variant): a replaceable
/// floor/ceiling patch followed by the configured feature on the resulting
/// surface. The world seed is forwarded to that nested feature so a nested
/// geode uses the same seed as its enclosing decoration pass.
pub(super) fn place_vegetation_patch_with_seed<R: RandomSource>(
    random: &mut R,
    world_seed: i64,
    pos: BlockPos,
    cfg: &VegetationPatchCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let x_radius = cfg.xz_radius.sample(random) + 1;
    let z_radius = cfg.xz_radius.sample(random) + 1;
    let inwards = cfg.surface.dy();
    let outwards = -inwards;
    // Membership and table traversal are both observable: the latter assigns
    // each nested-placement random draw to a particular surface cell.
    let mut surface = CompatBlockPosSet::default();
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
            if !air_at(grid, cur.x, cur.y, cur.z)
                || !vegetation_patch_supports(grid, below.x, below.y, below.z, cfg.surface)
            {
                continue;
            }
            let mut depth = cfg.depth.sample(random);
            if cfg.extra_bottom_block_chance > 0.0
                && random.next_float() < cfg.extra_bottom_block_chance
            {
                depth += 1;
            }
            if place_patch_ground(random, below, cfg, depth, grid, tags) {
                surface.insert(below);
            }
        }
    }
    if cfg.waterlogged {
        // `WaterloggedVegetationPatchFeature`: only the non-exposed surface cells
        // survive. The new set's table order governs both water writes and the
        // subsequent nested-feature random draws.
        let mut kept = CompatBlockPosSet::default();
        for p in surface
            .into_iter()
            .filter(|p| !patch_exposed(grid, *p))
        {
            kept.insert(p);
        }
        for p in kept.order() {
            grid.set_if_in_bounds(p.x, p.y, p.z, "minecraft:water".to_string());
        }
        surface = kept;
    }
    for p in surface {
        if cfg.vegetation_chance > 0.0
            && random.next_float() < cfg.vegetation_chance
        {
            let target = BlockPos {
                x: p.x,
                y: p.y + outwards,
                z: p.z,
            };
            super::place_placed_feature_at_seed(
                random,
                world_seed,
                target,
                &cfg.vegetation_feature,
                grid,
                tags,
            );
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
        let Some(state) = cfg.ground_state.get_state_id(grid, tags, random, cur) else {
            return i != 0;
        };
        let below_state = grid.get_id(cur.x, cur.y, cur.z);
        let below = base_at(grid, cur.x, cur.y, cur.z);
        let same_base = grid.interner().base_of(state) == grid.interner().base_of(below_state);
        if same_base {
            continue;
        }
        if !cfg.replaceable.contains(below) {
            return i != 0;
        }
        grid.set_id_if_in_bounds(cur.x, cur.y, cur.z, state);
        cur.y += cfg.surface.dy();
    }
    true
}

const SCULK_MAX_CHARGE: i32 = 1000;
const SCULK_MAX_CURSOR_DISTANCE: i32 = 1024;
const SCULK_MAX_CURSORS: usize = 32;
const SCULK_DIRECTIONS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];
const SCULK_NON_CORNER_NEIGHBOURS: [(i32, i32, i32); 18] = [
    (0, -1, -1),
    (-1, 0, -1),
    (0, 0, -1),
    (1, 0, -1),
    (0, 1, -1),
    (-1, -1, 0),
    (0, -1, 0),
    (1, -1, 0),
    (-1, 0, 0),
    (1, 0, 0),
    (-1, 1, 0),
    (0, 1, 0),
    (1, 1, 0),
    (0, -1, 1),
    (-1, 0, 1),
    (0, 0, 1),
    (1, 0, 1),
    (0, 1, 1),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SculkBehaviourKind {
    Default,
    Sculk,
    Vein,
}

#[derive(Clone, Copy, Debug)]
struct SculkCursor {
    pos: BlockPos,
    charge: i32,
    update_delay: i32,
    decay_delay: i32,
    facings: Option<u8>,
}

fn sculk_behaviour(grid: &VegGrid, pos: BlockPos) -> SculkBehaviourKind {
    match base_at(grid, pos.x, pos.y, pos.z) {
        "minecraft:sculk" => SculkBehaviourKind::Sculk,
        "minecraft:sculk_vein" => SculkBehaviourKind::Vein,
        _ => SculkBehaviourKind::Default,
    }
}

fn sculk_is_water_source(state: &str) -> bool {
    let base = super::base_id(state);
    base == "minecraft:water" && (!state.contains('[') || state.contains("level=0"))
}

fn sculk_face_for_offset(offset: (i32, i32, i32)) -> Face {
    match offset {
        (0, -1, 0) => Face::Down,
        (0, 1, 0) => Face::Up,
        (0, 0, -1) => Face::North,
        (0, 0, 1) => Face::South,
        (-1, 0, 0) => Face::West,
        _ => Face::East,
    }
}

fn sculk_opposite(offset: (i32, i32, i32)) -> (i32, i32, i32) {
    (-offset.0, -offset.1, -offset.2)
}

fn sculk_block_face_sturdy(grid: &VegGrid, pos: BlockPos, face: Face) -> bool {
    if let Some(canonical) = grid.interner().canonical_id(grid.get_id(pos.x, pos.y, pos.z)) {
        face_occlusion::occludes(canonical, face)
    } else {
        sturdy_at(grid, pos.x, pos.y, pos.z)
    }
}

fn sculk_face_sturdy_at(grid: &VegGrid, pos: BlockPos, support: (i32, i32, i32)) -> bool {
    let support_pos = BlockPos {
        x: pos.x + support.0,
        y: pos.y + support.1,
        z: pos.z + support.2,
    };
    sculk_block_face_sturdy(grid, support_pos, sculk_face_for_offset(sculk_opposite(support)))
}

fn sculk_full_collision_at(grid: &VegGrid, pos: BlockPos) -> bool {
    let state = grid.get_id(pos.x, pos.y, pos.z);
    let Some(canonical) = grid.interner().canonical_id(state) else {
        return sturdy_at(grid, pos.x, pos.y, pos.z);
    };
    let boxes = collision_shapes::collision_boxes(canonical);
    boxes.len() == 1 && boxes[0].min == [0.0; 3] && boxes[0].max == [1.0; 3]
}

fn sculk_can_spread_from(grid: &VegGrid, origin: BlockPos) -> bool {
    if sculk_behaviour(grid, origin) != SculkBehaviourKind::Default {
        return true;
    }
    let state = grid.get(origin.x, origin.y, origin.z);
    if !(is_air(super::base_id(state)) || sculk_is_water_source(state)) {
        return false;
    }
    SCULK_DIRECTIONS.iter().any(|&(dx, dy, dz)| {
        sculk_full_collision_at(
            grid,
            BlockPos { x: origin.x + dx, y: origin.y + dy, z: origin.z + dz },
        )
    })
}

fn sculk_face_mask(state: &str) -> u8 {
    SCULK_DIRECTIONS.iter().enumerate().fold(0, |mask, (index, &offset)| {
        let face = face_property(offset.0, offset.1, offset.2);
        if state.contains(&format!("{face}=true")) {
            mask | (1 << index)
        } else {
            mask
        }
    })
}

fn sculk_has_face(mask: u8, offset: (i32, i32, i32)) -> bool {
    let index = SCULK_DIRECTIONS
        .iter()
        .position(|&candidate| candidate == offset)
        .expect("sculk direction table contains every face");
    mask & (1 << index) != 0
}

fn sculk_vein_state(mask: u8, waterlogged: bool) -> String {
    let enabled = |index: usize| mask & (1 << index) != 0;
    format!(
        "minecraft:sculk_vein[down={},east={},north={},south={},up={},waterlogged={},west={}]",
        enabled(0),
        enabled(5),
        enabled(2),
        enabled(3),
        enabled(1),
        waterlogged,
        enabled(4),
    )
}

fn sculk_vein_state_with_face(old: &str, face: (i32, i32, i32)) -> String {
    let mask = sculk_face_mask(old)
        | (1 << SCULK_DIRECTIONS.iter().position(|&candidate| candidate == face).unwrap());
    sculk_vein_state(mask, super::base_id(old) == "minecraft:water" || old.contains("waterlogged=true"))
}

fn sculk_can_replace_vein_target(grid: &VegGrid, target: BlockPos) -> bool {
    let state = grid.get(target.x, target.y, target.z);
    let base = super::base_id(state);
    (is_air(base) || base == "minecraft:sculk_vein" || sculk_is_water_source(state))
        && base != "minecraft:sculk"
}

fn sculk_spread_candidate_allowed(
    grid: &VegGrid,
    source: BlockPos,
    target: BlockPos,
    face: (i32, i32, i32),
) -> bool {
    let base = base_at(grid, target.x, target.y, target.z);
    // The support cell beyond the placement face is part of the spread
    // predicate too: a sculk block, catalyst, or piston there blocks a vein
    // face even when the target itself is replaceable. This matters for the
    // same-position pass after a cursor has just converted its substrate.
    let beyond = base_at(grid, target.x + face.0, target.y + face.1, target.z + face.2);
    if beyond == "minecraft:sculk"
        || beyond == "minecraft:sculk_catalyst"
        || beyond == "minecraft:moving_piston"
    {
        return false;
    }
    if base == "minecraft:sculk"
        || base == "minecraft:sculk_catalyst"
        || base == "minecraft:moving_piston"
        || base == "minecraft:fire"
        || (is_fluid(base) && base != "minecraft:water")
    {
        return false;
    }
    if !sculk_can_replace_vein_target(grid, target) {
        return false;
    }
    if !sculk_face_sturdy_at(grid, target, face) {
        return false;
    }
    if (target.x - source.x).abs() + (target.y - source.y).abs() + (target.z - source.z).abs() == 2 {
        let behind = BlockPos {
            x: source.x - face.0,
            y: source.y - face.1,
            z: source.z - face.2,
        };
        if sculk_block_face_sturdy(grid, behind, sculk_face_for_offset(face)) {
            return false;
        }
    }
    true
}

fn sculk_spread_all(
    grid: &mut VegGrid,
    source: BlockPos,
    source_state: &str,
    same_position_only: bool,
) -> bool {
    let kind = super::base_id(source_state);
    let other_source = kind != "minecraft:sculk_vein";
    let source_mask = sculk_face_mask(source_state);
    let mut placed = false;
    for &from_face in &SCULK_DIRECTIONS {
        if !other_source && !sculk_has_face(source_mask, from_face) {
            continue;
        }
        for &spread_direction in &SCULK_DIRECTIONS {
            if same_axis(from_face, spread_direction)
                || (!other_source && sculk_has_face(source_mask, spread_direction))
            {
                continue;
            }
            let candidates = if same_position_only {
                [
                    (source, spread_direction),
                    (source, spread_direction),
                    (source, spread_direction),
                ]
            } else {
                [
                    (source, spread_direction),
                    (
                        BlockPos {
                            x: source.x + spread_direction.0,
                            y: source.y + spread_direction.1,
                            z: source.z + spread_direction.2,
                        },
                        from_face,
                    ),
                    (
                        BlockPos {
                            x: source.x + spread_direction.0 + from_face.0,
                            y: source.y + spread_direction.1 + from_face.1,
                            z: source.z + spread_direction.2 + from_face.2,
                        },
                        sculk_opposite(spread_direction),
                    ),
                ]
            };
            for (target, face) in candidates {
                if !sculk_spread_candidate_allowed(grid, source, target, face) {
                    continue;
                }
                let old = grid.get(target.x, target.y, target.z).to_string();
                let next = sculk_vein_state_with_face(&old, face);
                if next == old || !grid.set_if_in_bounds(target.x, target.y, target.z, next) {
                    continue;
                }
                placed = true;
                break;
            }
        }
    }
    placed
}

fn sculk_regrow_vein(grid: &mut VegGrid, pos: BlockPos, source_state: &str, facings: u8) -> bool {
    let mut mask = 0;
    for (index, &face) in SCULK_DIRECTIONS.iter().enumerate() {
        let sturdy = sculk_face_sturdy_at(grid, pos, face);
        if facings & (1 << index) != 0 && sturdy {
            mask |= 1 << index;
        }
    }
    if mask == 0 {
        return false;
    }
    let waterlogged = super::base_id(source_state) == "minecraft:water"
        || source_state.contains("waterlogged=true");
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, sculk_vein_state(mask, waterlogged))
}

fn sculk_attempt_spread_vein(
    grid: &mut VegGrid,
    pos: BlockPos,
    source_state: &str,
    facings: Option<u8>,
) -> bool {
    match sculk_behaviour(grid, pos) {
        SculkBehaviourKind::Default => match facings {
            None => sculk_spread_all(grid, pos, source_state, true),
            Some(mask) if mask != 0 => sculk_regrow_vein(grid, pos, source_state, mask),
            Some(_) => false,
        },
        SculkBehaviourKind::Sculk | SculkBehaviourKind::Vein => {
            sculk_spread_all(grid, pos, source_state, false)
        }
    }
}

fn sculk_has_substrate_access(grid: &VegGrid, tags: &VegTags, pos: BlockPos, state: &str) -> bool {
    if super::base_id(state) != "minecraft:sculk_vein" {
        return false;
    }
    let mask = sculk_face_mask(state);
    SCULK_DIRECTIONS.iter().enumerate().any(|(index, &face)| {
        sculk_has_face(mask, face)
            && tags.sculk_replaceable.contains(base_at(
                grid,
                pos.x + face.0,
                pos.y + face.1,
                pos.z + face.2,
            ))
            && index < SCULK_DIRECTIONS.len()
    })
}

fn sculk_unobstructed_axis(grid: &VegGrid, from: BlockPos, axis: (i32, i32, i32)) -> bool {
    let test = BlockPos {
        x: from.x + axis.0,
        y: from.y + axis.1,
        z: from.z + axis.2,
    };
    sculk_block_face_sturdy(grid, test, sculk_face_for_offset(sculk_opposite(axis)))
        == false
}

fn sculk_movement_unobstructed(grid: &VegGrid, from: BlockPos, to: BlockPos) -> bool {
    let delta = (to.x - from.x, to.y - from.y, to.z - from.z);
    let manhattan = delta.0.abs() + delta.1.abs() + delta.2.abs();
    if manhattan == 1 {
        return true;
    }
    if delta.0 == 0 {
        sculk_unobstructed_axis(grid, from, (0, delta.1, 0))
            || sculk_unobstructed_axis(grid, from, (0, 0, delta.2))
    } else if delta.1 == 0 {
        sculk_unobstructed_axis(grid, from, (delta.0, 0, 0))
            || sculk_unobstructed_axis(grid, from, (0, 0, delta.2))
    } else {
        sculk_unobstructed_axis(grid, from, (delta.0, 0, 0))
            || sculk_unobstructed_axis(grid, from, (0, delta.1, 0))
    }
}

fn sculk_valid_movement_pos<R: RandomSource>(
    random: &mut R,
    grid: &VegGrid,
    tags: &VegTags,
    from: BlockPos,
) -> Option<BlockPos> {
    let mut fallback = None;
    for (dx, dy, dz) in shuffled_copy(random, &SCULK_NON_CORNER_NEIGHBOURS) {
        let target = BlockPos { x: from.x + dx, y: from.y + dy, z: from.z + dz };
        if sculk_behaviour(grid, target) == SculkBehaviourKind::Default
            || !sculk_movement_unobstructed(grid, from, target)
        {
            continue;
        }
        fallback = Some(target);
        if sculk_has_substrate_access(grid, tags, target, grid.get(target.x, target.y, target.z)) {
            break;
        }
    }
    fallback
}

fn sculk_on_discharged(grid: &mut VegGrid, pos: BlockPos, state: &str) {
    if super::base_id(state) != "minecraft:sculk_vein" {
        return;
    }
    let mut mask = sculk_face_mask(state);
    for (index, &face) in SCULK_DIRECTIONS.iter().enumerate() {
        if sculk_has_face(mask, face)
            && base_at(grid, pos.x + face.0, pos.y + face.1, pos.z + face.2) == "minecraft:sculk"
        {
            mask &= !(1 << index);
        }
    }
    let waterlogged = state.contains("waterlogged=true");
    let next = if mask == 0 {
        if waterlogged {
            "minecraft:water[level=0]".to_string()
        } else {
            "minecraft:air".to_string()
        }
    } else {
        sculk_vein_state(mask, waterlogged)
    };
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, next);
}

fn sculk_random_growth_state<R: RandomSource>(random: &mut R, waterlogged: bool) -> String {
    let growth_roll = random.next_int_bounded(11);
    let base = if growth_roll == 0 {
        "minecraft:sculk_shrieker"
    } else {
        "minecraft:sculk_sensor"
    };
    let mut state = CanonicalStateId::from_state_str(base)
        .map(CanonicalStateId::canonical_state)
        .unwrap_or_else(|| base.to_string());
    if base == "minecraft:sculk_shrieker" {
        state = state.replace("can_summon=false", "can_summon=true");
    }
    if waterlogged {
        state = state.replace("waterlogged=false", "waterlogged=true");
    }
    state
}

fn sculk_can_place_growth(grid: &VegGrid, pos: BlockPos) -> bool {
    let above = grid.get(pos.x, pos.y + 1, pos.z);
    if !(is_air(super::base_id(above)) || sculk_is_water_source(above)) {
        return false;
    }
    let mut growth_count = 0;
    for x in pos.x - 4..=pos.x + 4 {
        for y in pos.y..=pos.y + 2 {
            for z in pos.z - 4..=pos.z + 4 {
                let base = base_at(grid, x, y, z);
                if base == "minecraft:sculk_sensor" || base == "minecraft:sculk_shrieker" {
                    growth_count += 1;
                    if growth_count > 2 {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn sculk_attempt_place_sculk<R: RandomSource>(
    random: &mut R,
    grid: &mut VegGrid,
    tags: &VegTags,
    pos: BlockPos,
    state: &str,
) -> bool {
    let mask = sculk_face_mask(state);
    for support in shuffled_copy(random, &SCULK_DIRECTIONS) {
        if !sculk_has_face(mask, support) {
            continue;
        }
        let support_pos = BlockPos {
            x: pos.x + support.0,
            y: pos.y + support.1,
            z: pos.z + support.2,
        };
        if !tags
            .sculk_replaceable_world_gen
            .contains(base_at(grid, support_pos.x, support_pos.y, support_pos.z))
        {
            continue;
        }
        grid.set_if_in_bounds(support_pos.x, support_pos.y, support_pos.z, "minecraft:sculk".to_string());
        sculk_spread_all(
            grid,
            support_pos,
            "minecraft:sculk",
            false,
        );
        let skip = sculk_opposite(support);
        for &vein_direction in &SCULK_DIRECTIONS {
            if vein_direction == skip {
                continue;
            }
            let neighbour = BlockPos {
                x: support_pos.x + vein_direction.0,
                y: support_pos.y + vein_direction.1,
                z: support_pos.z + vein_direction.2,
            };
            let neighbour_state = grid.get(neighbour.x, neighbour.y, neighbour.z).to_string();
            if super::base_id(&neighbour_state) == "minecraft:sculk_vein" {
                sculk_on_discharged(grid, neighbour, &neighbour_state);
            }
        }
        return true;
    }
    false
}

fn sculk_attempt_use_charge<R: RandomSource>(
    random: &mut R,
    grid: &mut VegGrid,
    tags: &VegTags,
    cursor: &SculkCursor,
    origin: BlockPos,
    behaviour: SculkBehaviourKind,
    spread_veins: bool,
) -> i32 {
    match behaviour {
        SculkBehaviourKind::Default => {
            if cursor.decay_delay > 0 { cursor.charge } else { 0 }
        }
        SculkBehaviourKind::Vein => {
            let state = grid.get(cursor.pos.x, cursor.pos.y, cursor.pos.z).to_string();
            if spread_veins && sculk_attempt_place_sculk(random, grid, tags, cursor.pos, &state) {
                cursor.charge - 1
            } else if {
                let roll = random.next_int_bounded(5);
                roll == 0
            } {
                (cursor.charge as f32 * 0.5) as i32
            } else {
                cursor.charge
            }
        }
        SculkBehaviourKind::Sculk => {
            let use_roll = random.next_int_bounded(5);
            if cursor.charge == 0 || use_roll != 0 {
                return cursor.charge;
            }
            let dx = cursor.pos.x - origin.x;
            let dy = cursor.pos.y - origin.y;
            let dz = cursor.pos.z - origin.z;
            let close_to_origin = (dx * dx + dy * dy + dz * dz) < 1;
            if !close_to_origin && sculk_can_place_growth(grid, cursor.pos) {
                let growth_roll = random.next_int_bounded(50);
                if growth_roll < cursor.charge {
                    let above = grid.get(cursor.pos.x, cursor.pos.y + 1, cursor.pos.z);
                    let growth = sculk_random_growth_state(random, sculk_is_water_source(above));
                    grid.set_if_in_bounds(cursor.pos.x, cursor.pos.y + 1, cursor.pos.z, growth);
                }
                return (cursor.charge - 50).max(0);
            }
            let decay_roll = random.next_int_bounded(10);
            if decay_roll != 0 {
                return cursor.charge;
            }
            if close_to_origin {
                cursor.charge - 1
            } else {
                let distance = ((dx * dx + dy * dy + dz * dz) as f32).sqrt();
                let outer = (distance - 1.0_f32).max(0.0_f32).powi(2);
                let factor = (outer / (24.0_f32 - 1.0_f32).powi(2)).min(1.0_f32);
                ((cursor.charge as f32 * factor * 0.5) as i32).max(1)
            }
        }
    }
}

fn sculk_update_cursor<R: RandomSource>(
    random: &mut R,
    grid: &mut VegGrid,
    tags: &VegTags,
    cursor: &mut SculkCursor,
    origin: BlockPos,
    spread_veins: bool,
) {
    if cursor.charge <= 0 {
        return;
    }
    if cursor.update_delay > 0 {
        cursor.update_delay -= 1;
        return;
    }
    let mut state = grid.get(cursor.pos.x, cursor.pos.y, cursor.pos.z).to_string();
    let mut behaviour = sculk_behaviour(grid, cursor.pos);
    if spread_veins && sculk_attempt_spread_vein(grid, cursor.pos, &state, cursor.facings) {
        if behaviour != SculkBehaviourKind::Sculk {
            state = grid.get(cursor.pos.x, cursor.pos.y, cursor.pos.z).to_string();
            behaviour = sculk_behaviour(grid, cursor.pos);
        }
    }
    cursor.charge = sculk_attempt_use_charge(random, grid, tags, cursor, origin, behaviour, spread_veins);
    if cursor.charge <= 0 {
        sculk_on_discharged(grid, cursor.pos, &state);
        return;
    }
    if let Some(next) = sculk_valid_movement_pos(random, grid, tags, cursor.pos) {
        sculk_on_discharged(grid, cursor.pos, &state);
        cursor.pos = next;
        let dx = cursor.pos.x - origin.x;
        let dz = cursor.pos.z - origin.z;
        if dx * dx + dz * dz >= 15 * 15 {
            cursor.charge = 0;
            return;
        }
        state = grid.get(cursor.pos.x, cursor.pos.y, cursor.pos.z).to_string();
    }
    if sculk_behaviour(grid, cursor.pos) != SculkBehaviourKind::Default {
        cursor.facings = Some(sculk_face_mask(&state));
    }
    cursor.decay_delay = match behaviour {
        SculkBehaviourKind::Default => (cursor.decay_delay - 1).max(0),
        SculkBehaviourKind::Sculk | SculkBehaviourKind::Vein => 1,
    };
    cursor.update_delay = 1;
}

/// Applies the configured world-generation cursor spread, including substrate
/// replacement and multiface vein propagation. The cursor state is deliberately
/// local to this feature: world generation never persists or merges it.
pub(super) fn place_sculk_patch<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    cfg: &SculkPatchCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if !sculk_can_spread_from(grid, pos) {
        return;
    }
    let rounds = cfg.spread_rounds + cfg.growth_rounds;
    for round in 0..rounds {
        let mut cursors = Vec::new();
        if cfg.charge_count > 0 && cfg.amount_per_charge > 0 {
            for _ in 0..cfg.charge_count {
                let mut charge = cfg.amount_per_charge;
                while charge > 0 && cursors.len() < SCULK_MAX_CURSORS {
                    let current = charge.min(SCULK_MAX_CHARGE);
                    cursors.push(SculkCursor {
                        pos,
                        charge: current,
                        update_delay: 0,
                        decay_delay: 1,
                        facings: None,
                    });
                    charge -= current;
                }
            }
        }
        let spread_veins = round < cfg.spread_rounds;
        for _ in 0..cfg.spread_attempts.max(0) {
            let mut next = Vec::with_capacity(cursors.len());
            for mut cursor in cursors {
                let dx = cursor.pos.x - pos.x;
                let dy = cursor.pos.y - pos.y;
                let dz = cursor.pos.z - pos.z;
                if dx.abs().max(dy.abs()).max(dz.abs()) <= SCULK_MAX_CURSOR_DISTANCE {
                    sculk_update_cursor(random, grid, tags, &mut cursor, pos, spread_veins);
                    if cursor.charge > 0 {
                        next.push(cursor);
                    }
                }
            }
            cursors = next;
            if cursors.is_empty() {
                break;
            }
        }
    }
    let below = BlockPos { x: pos.x, y: pos.y - 1, z: pos.z };
    if random.next_float() <= cfg.catalyst_chance && sculk_full_collision_at(grid, below) {
        let state = CanonicalStateId::from_state_str("minecraft:sculk_catalyst")
            .map(CanonicalStateId::canonical_state)
            .unwrap_or_else(|| "minecraft:sculk_catalyst".to_string());
        grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
    }
    let extra = cfg.extra_rare_growths.sample(random);
    for _ in 0..extra {
        let candidate = BlockPos {
            x: pos.x + {
                let roll = random.next_int_bounded(5);
                roll
            } - 2,
            y: pos.y,
            z: pos.z + {
                let roll = random.next_int_bounded(5);
                roll
            } - 2,
        };
        if air_at(grid, candidate.x, candidate.y, candidate.z)
            && sculk_face_sturdy_at(grid, candidate, (0, -1, 0))
        {
            let shrieker = CanonicalStateId::from_state_str("minecraft:sculk_shrieker")
                .map(CanonicalStateId::canonical_state)
                .unwrap_or_else(|| {
                    "minecraft:sculk_shrieker[can_summon=false,shrieking=false,waterlogged=false]".to_string()
                })
                .replace("can_summon=false", "can_summon=true");
            grid.set_if_in_bounds(candidate.x, candidate.y, candidate.z, shrieker);
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
            Decorator::Beehive { .. }
            | Decorator::PlaceOnGround { .. }
            | Decorator::AlterGround { .. }
            | Decorator::Unsupported => {}
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
    use std::collections::{HashMap, HashSet, VecDeque};

    use super::*;
    use crate::feature::{HeightProvider, VerticalAnchor};
    use crate::feature::top_layer::StatePredicate;
    use crate::rng::{LegacyRandomSource, RandomSource, WorldgenRandom, XoroshiroPositionalFactory, XoroshiroRandomSource};

    fn pos_for_hash(hash: i32) -> BlockPos { BlockPos { x: hash, y: 0, z: 0 } }

    #[test]
    fn compatibility_set_preserves_insertion_order_and_rejects_bucket_order() {
        let mut set = CompatBlockPosSet::default();
        for hash in [17, 0, 1, 33, 16, 0] {
            set.insert(pos_for_hash(hash));
        }
        assert_eq!(
            set.order(),
            [17, 0, 1, 33, 16].map(pos_for_hash),
            "the feature stream consumes successful insertions in encounter order",
        );
        assert_ne!(set.order(), set.bucket_order(), "bucket order is only a membership detail");
    }

    #[test]
    fn compatibility_set_resize_splits_old_chains_without_reordering_them() {
        let mut set = CompatBlockPosSet::default();
        for hash in [0, 16, 32, 48, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
            assert!(set.insert(pos_for_hash(hash)));
        }
        assert_eq!(set.bucket_capacity(), 32, "the thirteenth entry crosses the 16-bucket threshold");
        assert_eq!(
            set.order(),
            [0, 16, 32, 48, 1, 2, 3, 4, 5, 6, 7, 8, 9].map(pos_for_hash),
            "resizing membership buckets does not reorder the feature stream",
        );
        assert_ne!(set.order(), set.bucket_order(), "the resized bucket order is a negative control");
    }

    #[test]
    fn compatibility_set_order_is_identical_across_cold_and_warm_construction() {
        let input = [97, -1, 31, 0, 16, 48, 2, 18, 34, 50, 3, 19, 35, 51];
        let build = || {
            let mut set = CompatBlockPosSet::default();
            for hash in input { set.insert(pos_for_hash(hash)); }
            set.order()
        };
        let expected = build();
        for _ in 0..32 {
            assert_eq!(build(), expected);
        }
    }

    #[test]
    fn sculk_movement_neighbour_order_matches_coordinate_iteration() {
        assert_eq!(
            SCULK_NON_CORNER_NEIGHBOURS,
            [
                (0, -1, -1),
                (-1, 0, -1),
                (0, 0, -1),
                (1, 0, -1),
                (0, 1, -1),
                (-1, -1, 0),
                (0, -1, 0),
                (1, -1, 0),
                (-1, 0, 0),
                (1, 0, 0),
                (-1, 1, 0),
                (0, 1, 0),
                (1, 1, 0),
                (0, -1, 1),
                (-1, 0, 1),
                (0, 0, 1),
                (1, 0, 1),
                (0, 1, 1),
            ],
        );
    }

    #[test]
    fn sculk_same_position_rejects_a_sculk_support_cell() {
        let origin = BlockPos { x: 8, y: 0, z: 8 };
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(origin.x, origin.y, origin.z, "minecraft:air".to_string());
        grid.seed(origin.x, origin.y - 1, origin.z, "minecraft:sculk".to_string());
        assert!(!sculk_spread_candidate_allowed(
            &grid,
            origin,
            origin,
            (0, -1, 0),
        ));

        grid.seed(origin.x, origin.y - 1, origin.z, "minecraft:deepslate".to_string());
        assert!(sculk_spread_candidate_allowed(
            &grid,
            origin,
            origin,
            (0, -1, 0),
        ));
    }

    struct DeltaScriptRandom {
        bounded: VecDeque<i32>,
    }

    impl DeltaScriptRandom {
        fn new(bounded: [i32; 4]) -> Self {
            Self { bounded: bounded.into() }
        }
    }

    struct DrawCountingRandom {
        floats: usize,
    }

    impl RandomSource for DrawCountingRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional { panic!("underwater magma does not fork") }
        fn set_seed(&mut self, _: i64) { panic!("underwater magma does not reseed") }
        fn next_bits(&mut self, _: u32) -> i32 { panic!("underwater magma only samples floats") }
        fn next_int(&mut self) -> i32 { panic!("underwater magma only samples floats") }
        fn next_int_bounded(&mut self, _: i32) -> i32 { panic!("underwater magma only samples floats") }
        fn next_long(&mut self) -> i64 { panic!("underwater magma only samples floats") }
        fn next_bool(&mut self) -> bool { panic!("underwater magma only samples floats") }
        fn next_float(&mut self) -> f32 {
            let value = (self.floats != 0) as u8 as f32;
            self.floats += 1;
            value
        }
        fn next_double(&mut self) -> f64 { panic!("underwater magma only samples floats") }
        fn next_gaussian(&mut self) -> f64 { panic!("underwater magma only samples floats") }
        fn consume_count(&mut self, _: u32) { panic!("underwater magma does not consume by count") }
    }

    fn enclosed_underwater_floor() -> VegGrid {
        let mut grid = VegGrid::new(-3, 8, 0, 0);
        for x in 3..=5 {
            for z in 3..=5 {
                for y in -2..=2 {
                    grid.seed(x, y, z, "minecraft:stone".to_string());
                }
                for y in 0..=2 {
                    grid.seed(x, y, z, "minecraft:water[level=0]".to_string());
                }
            }
        }
        grid
    }

    #[test]
    fn underwater_magma_requires_enclosed_floor_and_water_origin() {
        let cfg = UnderwaterMagmaCfg {
            floor_search_range: 5,
            placement_probability_per_valid_position: 1.0,
            placement_radius_around_floor: 0,
        };
        let mut enclosed = enclosed_underwater_floor();
        let mut random = LegacyRandomSource::new(7);
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 2, z: 4 },
            &cfg,
            &mut enclosed,
        );
        assert_eq!(enclosed.get(4, -1, 4), "minecraft:magma_block");
        assert_eq!(enclosed.dirty_len(), 1);

        let mut exposed = enclosed_underwater_floor();
        exposed.seed(5, -1, 4, "minecraft:air".to_string());
        let mut random = LegacyRandomSource::new(7);
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 2, z: 4 },
            &cfg,
            &mut exposed,
        );
        assert_eq!(exposed.get(4, -1, 4), "minecraft:stone");
        assert_eq!(exposed.dirty_len(), 0);

        let mut no_water = enclosed_underwater_floor();
        let mut random = LegacyRandomSource::new(7);
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 3, z: 4 },
            &cfg,
            &mut no_water,
        );
        assert_eq!(no_water.dirty_len(), 0);

        let mut too_narrow = enclosed_underwater_floor();
        let mut narrow_cfg = cfg.clone();
        narrow_cfg.floor_search_range = 1;
        let mut random = LegacyRandomSource::new(7);
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 2, z: 4 },
            &narrow_cfg,
            &mut too_narrow,
        );
        assert_eq!(too_narrow.dirty_len(), 0, "range one does not step below the water origin");

        let mut draw_control = enclosed_underwater_floor();
        for x in 3..=5 {
            for z in 3..=5 {
                for y in -2..=0 {
                    draw_control.seed(x, y, z, "minecraft:air".to_string());
                }
            }
        }
        draw_control.seed(4, -2, 4, "minecraft:stone".to_string());
        draw_control.seed(4, -1, 4, "minecraft:stone".to_string());
        draw_control.seed(4, 0, 4, "minecraft:water[level=0]".to_string());
        let draw_cfg = UnderwaterMagmaCfg {
            floor_search_range: 5,
            placement_probability_per_valid_position: 0.0,
            placement_radius_around_floor: 1,
        };
        let mut random = DrawCountingRandom { floats: 0 };
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 2, z: 4 },
            &draw_cfg,
            &mut draw_control,
        );
        assert_eq!(random.floats, 27, "each closed-box candidate draws before validity");
        assert_eq!(draw_control.dirty_len(), 0);

        let mut order_control = enclosed_underwater_floor();
        for (x, y, z) in [
            (3, -2, 3),
            (3, -3, 3),
            (2, -2, 3),
            (4, -2, 3),
            (3, -2, 2),
            (3, -2, 4),
        ] {
            order_control.seed(x, y, z, "minecraft:stone".to_string());
        }
        let order_cfg = UnderwaterMagmaCfg {
            floor_search_range: 5,
            placement_probability_per_valid_position: 0.5,
            placement_radius_around_floor: 1,
        };
        let mut random = DrawCountingRandom { floats: 0 };
        place_underwater_magma(
            &mut random,
            BlockPos { x: 4, y: 2, z: 4 },
            &order_cfg,
            &mut order_control,
        );
        assert_eq!(order_control.get(3, -2, 3), "minecraft:magma_block");
        assert_eq!(random.floats, 27);
    }

    #[test]
    fn disk_numeric_provider_keeps_write_order_for_builtin_and_extension_states() {
        let make_grid = || {
            let mut grid = VegGrid::new(-64, 384, 0, 0);
            grid.seed(8, 70, 8, "minecraft:air".to_string());
            grid
        };
        let cfg = |state: &str| DiskCfg {
            provider: BlockStateProvider::Simple(state.to_string()),
            target: BlockPredicate::True,
            radius: IntProvider::Constant(0),
            half_height: 0,
        };
        let pos = BlockPos { x: 8, y: 70, z: 8 };

        let mut builtin = make_grid();
        place_disk(
            &mut LegacyRandomSource::new(11),
            pos,
            &cfg("minecraft:sand"),
            &mut builtin,
            &VegTags::default(),
        );
        assert_eq!(builtin.dirty_cells().collect::<Vec<_>>(), vec![(8, 70, 8, "minecraft:sand")]);

        let mut extension = make_grid();
        place_disk(
            &mut LegacyRandomSource::new(11),
            pos,
            &cfg("example:plugin_block"),
            &mut extension,
            &VegTags::default(),
        );
        assert_eq!(
            extension.dirty_cells().collect::<Vec<_>>(),
            vec![(8, 70, 8, "example:plugin_block")]
        );
    }

    #[test]
    fn red_huge_mushroom_rejects_blocked_lower_cap_cell() {
        let cfg = HugeMushroomCfg {
            can_place_on: BlockPredicate::MatchingBlocks {
                blocks: ["minecraft:grass_block".to_string()].into_iter().collect(),
                offset: (0, 0, 0),
            },
            cap_provider: BlockStateProvider::Simple(
                "minecraft:red_mushroom_block[down=false,east=false,north=false,south=false,up=false,west=false]".to_string(),
            ),
            stem_provider: BlockStateProvider::Simple("minecraft:mushroom_stem".to_string()),
            foliage_radius: 2,
            kind: HugeMushroomKind::Red,
        };
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(8, 69, 8, "minecraft:grass_block".to_string());
        // This lower-cap corner is skipped by the final cap geometry, but the
        // feature's clearance scan still rejects it before writing anything.
        grid.seed(10, 72, 10, "minecraft:stone".to_string());
        let mut random = LegacyRandomSource::new(0);
        place_huge_mushroom_at_height(
            &mut random,
            BlockPos { x: 8, y: 70, z: 8 },
            &cfg,
            4,
            &mut grid,
            &VegTags::default(),
        );
        assert_eq!(grid.dirty_len(), 0);
    }

    #[test]
    fn red_huge_mushroom_accepts_a_leaf_canopy_across_its_cap_rows() {
        let cfg = HugeMushroomCfg {
            can_place_on: BlockPredicate::MatchingBlocks {
                blocks: ["minecraft:grass_block".to_string()].into_iter().collect(),
                offset: (0, 0, 0),
            },
            cap_provider: BlockStateProvider::Simple(
                "minecraft:red_mushroom_block[down=false,east=false,north=false,south=false,up=false,west=false]".to_string(),
            ),
            stem_provider: BlockStateProvider::Simple("minecraft:mushroom_stem".to_string()),
            foliage_radius: 2,
            kind: HugeMushroomKind::Red,
        };
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(8, 69, 8, "minecraft:grass_block".to_string());
        grid.seed(10, 72, 8, "minecraft:dark_oak_leaves[distance=1,persistent=false,waterlogged=false]".to_string());
        let mut tags = VegTags::default();
        tags.leaves.insert("minecraft:dark_oak_leaves".to_string());
        tags.replaceable_by_mushrooms.insert("minecraft:dark_oak_leaves".to_string());
        let mut random = LegacyRandomSource::new(0);

        place_huge_mushroom_at_height(
            &mut random,
            BlockPos { x: 8, y: 70, z: 8 },
            &cfg,
            4,
            &mut grid,
            &tags,
        );

        assert_eq!(base_at(&grid, 10, 72, 8), "minecraft:red_mushroom_block");
        assert_eq!(base_at(&grid, 8, 70, 8), "minecraft:mushroom_stem");
    }

    #[test]
    fn partial_neighbor_shapes_do_not_close_an_underwater_magma_face() {
        let cases = [
            (
                "minecraft:stone_slab[type=bottom,waterlogged=false]",
                (0, -1, 0),
                Face::Up,
            ),
            (
                "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]",
                (0, 0, -1),
                Face::South,
            ),
            (
                "minecraft:oak_trapdoor[facing=north,half=bottom,open=false,powered=false,waterlogged=false]",
                (1, 0, 0),
                Face::West,
            ),
        ];

        for (state, (dx, dy, dz), covered_face) in cases {
            let mut grid = enclosed_underwater_floor();
            let neighbour = BlockPos {
                x: 4 + dx,
                y: -1 + dy,
                z: 4 + dz,
            };
            grid.seed(neighbour.x, neighbour.y, neighbour.z, state.to_owned());
            let canonical = grid
                .interner()
                .canonical_id(grid.get_id(neighbour.x, neighbour.y, neighbour.z))
                .expect("partial-shape control belongs to the canonical state table");
            assert!(
                !face_occlusion::occludes(canonical, covered_face),
                "{state} must not completely occlude its {covered_face:?} face"
            );
            assert!(
                !valid_underwater_magma_position(&grid, BlockPos { x: 4, y: -1, z: 4 }),
                "{state} must expose the candidate's covered face"
            );
        }
    }

    #[test]
    fn vegetation_patch_support_tracks_floor_and_ceiling_faces() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        let support = "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]";
        grid.seed(8, 70, 8, support.to_string());

        // Motion blocking is intentionally coarser than a patch's support
        // face: this fence accepts the generic occupancy check, gives center
        // support below a ceiling, and does not give full support above a
        // floor.
        assert!(sturdy_at(&grid, 8, 70, 8));
        assert!(!vegetation_patch_supports(&grid, 8, 70, 8, CaveSurface::Floor));
        assert!(vegetation_patch_supports(&grid, 8, 70, 8, CaveSurface::Ceiling));

        grid.seed(9, 70, 8, "minecraft:stone".to_string());
        assert!(vegetation_patch_supports(&grid, 9, 70, 8, CaveSurface::Floor));
        assert!(vegetation_patch_supports(&grid, 9, 70, 8, CaveSurface::Ceiling));
    }

    impl RandomSource for DeltaScriptRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional { panic!("delta does not fork a positional source") }
        fn set_seed(&mut self, _: i64) { panic!("delta does not reseed") }
        fn next_bits(&mut self, _: u32) -> i32 { panic!("delta does not request raw bits") }
        fn next_int(&mut self) -> i32 { panic!("delta only samples bounded providers") }
        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let value = self.bounded.pop_front().expect("scripted delta draw");
            assert!((0..bound).contains(&value));
            value
        }
        fn next_long(&mut self) -> i64 { panic!("delta does not request longs") }
        fn next_bool(&mut self) -> bool { panic!("delta does not request booleans") }
        fn next_float(&mut self) -> f32 { panic!("delta does not request floats") }
        fn next_double(&mut self) -> f64 { 0.0 }
        fn next_gaussian(&mut self) -> f64 { panic!("delta does not request gaussians") }
        fn consume_count(&mut self, _: u32) { panic!("delta does not consume by count") }
    }

    #[test]
    fn captured_step4_delta_stream_preserves_patch_shape() {
        // Captured from the accepted 26.2 server jar with seed 123_456_789
        // through the feature wrapper: rim=true, rim radii=(2, 1), contents
        // radii=(5, 3).
        let cfg = DeltaCfg {
            contents: "minecraft:lava[level=0]".to_string(),
            rim: "minecraft:magma_block".to_string(),
            rim_size: IntProvider::Uniform { min: 0, max: 2 },
            size: IntProvider::Uniform { min: 3, max: 7 },
        };
        let mut grid = VegGrid::new(0, 16, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..=10 {
                    grid.seed(x, y, z, "minecraft:netherrack".to_string());
                }
            }
        }
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(123_456_789));
        place_delta(&mut random, BlockPos { x: 8, y: 10, z: 8 }, &cfg, &mut grid);

        assert_eq!(grid.get(3, 10, 8), "minecraft:magma_block");
        assert_eq!(grid.get(10, 10, 9), "minecraft:lava[level=0]");
        assert_eq!(grid.get(15, 10, 9), "minecraft:lava[level=0]");
        assert_eq!(grid.get(10, 10, 12), "minecraft:lava[level=0]");
        assert_eq!(grid.get(2, 10, 8), "minecraft:netherrack");
    }

    #[test]
    fn captured_delta_overlap_keeps_later_contents_over_its_rim() {
        let cfg = DeltaCfg {
            contents: "minecraft:lava[level=0]".to_string(),
            rim: "minecraft:magma_block".to_string(),
            rim_size: IntProvider::Uniform { min: 0, max: 2 },
            size: IntProvider::Uniform { min: 3, max: 7 },
        };
        let mut grid = VegGrid::with_footprint(0, 128, -4_000, -4_000, -24, 40);
        for x in -4_024..-3_960 {
            for z in -4_024..-3_960 {
                for y in 0..=57 {
                    grid.seed(x, y, z, "minecraft:netherrack".to_string());
                }
            }
        }
        // The scripted draws select a rim offset `(2, 1)` and `(5, 5)` extent
        // at the captured local origin. The second support position shifts its
        // contents write onto a prior rim position, so the final state proves
        // both the traversal order and the re-check against live state.
        let mut random = DeltaScriptRandom::new([2, 1, 2, 2]);
        place_delta(&mut random, BlockPos { x: -4_014, y: 57, z: -3_972 }, &cfg, &mut grid);

        assert_eq!(grid.get(-4_018, 57, -3_972), "minecraft:magma_block");
        assert_eq!(grid.get(-4_016, 57, -3_971), "minecraft:lava[level=0]");
        assert!(random.bounded.is_empty(), "the body must consume exactly four provider draws");

        let mut blocked = VegGrid::with_footprint(0, 128, -4_000, -4_000, -24, 40);
        for x in -4_024..-3_960 {
            for z in -4_024..-3_960 {
                for y in 0..=57 {
                    blocked.seed(x, y, z, "minecraft:netherrack".to_string());
                }
            }
        }
        blocked.seed(-4_018, 57, -3_972, "minecraft:lava[level=0]".to_string());
        let mut random = DeltaScriptRandom::new([2, 1, 2, 2]);
        place_delta(&mut random, BlockPos { x: -4_014, y: 57, z: -3_972 }, &cfg, &mut blocked);
        assert_eq!(blocked.get(-4_016, 57, -3_971), "minecraft:magma_block");
    }

    #[test]
    fn captured_step4_column_stream_keeps_unit_y_draw_and_conditional_reach_order() {
        // Captured from the accepted 26.2 server jar with seed 987_654_321
        // through the feature wrapper: height=9, clustered=false, then these
        // in-diamond attempts.
        let cfg = BasaltColumnsCfg {
            height: IntProvider::Uniform { min: 5, max: 10 },
            reach: IntProvider::Uniform { min: 2, max: 3 },
        };
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(987_654_321));
        assert_eq!(cfg.height.sample(&mut random), 9);
        assert!(!(random.next_float() < 0.9));
        let mut seen = Vec::new();
        visit_basalt_column_candidates(
            &mut random,
            BlockPos { x: 0, y: 10, z: 0 },
            9,
            false,
            &cfg,
            |pos, blocks, reach| seen.push(((pos.x, pos.y, pos.z), blocks, reach)),
        );
        assert_eq!(
            seen,
            vec![
                ((6, 10, 1), 2, 3),
                ((-3, 10, -1), 5, 3),
                ((4, 10, 2), 3, 2),
                ((-1, 10, -2), 6, 2),
                ((8, 10, -1), 0, 3),
                ((5, 10, -4), 0, 3),
                ((1, 10, -6), 2, 3),
                ((7, 10, -2), 0, 2),
                ((-4, 10, 3), 2, 3),
                ((1, 10, 8), 0, 3),
            ]
        );
    }

    #[test]
    fn captured_replace_blob_shape_keeps_search_and_radius_streams() {
        // Captured from the bundled server runtime with origin (0,20,0), a
        // netherrack field through y=18, and xoroshiro seed 987_654_321:
        // target y=18, radii=(7,3,7), then 300 basalt cells.
        let cfg = ReplaceBlobsCfg {
            target: "minecraft:netherrack".to_string(),
            state: CanonicalStateId::from_state_str("minecraft:basalt[axis=y]").unwrap(),
            radius: IntProvider::Uniform { min: 3, max: 7 },
        };
        let mut grid = VegGrid::with_footprint(0, 128, 0, 0, -8, 9);
        for x in -8..=8 {
            for y in 2..=18 {
                for z in -8..=8 {
                    grid.seed(x, y, z, "minecraft:netherrack".to_string());
                }
            }
        }
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(987_654_321));
        place_replace_blobs(&mut random, BlockPos { x: 0, y: 20, z: 0 }, &cfg, &mut grid);
        let mut written: Vec<_> = grid
            .dirty_cells()
            .filter(|(_, _, _, state)| *state == "minecraft:basalt[axis=y]")
            .map(|(x, y, z, _)| (x, y, z))
            .collect();
        written.sort_unstable_by_key(|&(x, y, z)| (y, z, x));
        assert_eq!(written.len(), 300);
        assert_eq!(written.first(), Some(&(0, 15, -4)));
        assert_eq!(written.last(), Some(&(0, 18, 7)));
        assert_eq!(random.next_int(), 906_570_412);

        let mut no_target = VegGrid::with_footprint(0, 128, 0, 0, -8, 9);
        let mut no_target_random = WorldgenRandom::new(XoroshiroRandomSource::new(987_654_321));
        place_replace_blobs(
            &mut no_target_random,
            BlockPos { x: 0, y: 20, z: 0 },
            &cfg,
            &mut no_target,
        );
        assert_eq!(no_target.dirty_len(), 0);
        assert_eq!(no_target_random.next_int(), -1_294_795_328, "control: no target consumes no radius draws");
    }

    /// The first two body-interleaved attempts from the seed-42 Nether
    /// packet fixture.  The field is deliberately all target material over
    /// this small bound, so each origin reaches the body and the recorded
    /// output distinguishes a lost radius draw from a modifier-only stream.
    #[test]
    fn captured_nether_index0_first_attempts_keep_body_interleaved_stream() {
        let cfg = ReplaceBlobsCfg {
            target: "minecraft:netherrack".to_string(),
            state: CanonicalStateId::from_state_str("minecraft:basalt[axis=y]").unwrap(),
            radius: IntProvider::Uniform { min: 3, max: 7 },
        };
        let mut grid = VegGrid::with_footprint(0, 128, -4_000, -4_000, -8, 18);
        for x in -4_008..-3_982 {
            for y in 0..128 {
                for z in -4_008..-3_982 {
                    grid.seed(x, y, z, "minecraft:netherrack".to_string());
                }
            }
        }
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let decoration_seed = random.set_decoration_seed(42, -4_000, -4_000);
        random.set_feature_seed(decoration_seed, 0, 7);
        let height = HeightProvider::Uniform {
            min: VerticalAnchor::AboveBottom(0),
            max: VerticalAnchor::BelowTop(0),
        };
        let first_x = -4_000 + random.next_int_bounded(16);
        let first_z = -4_000 + random.next_int_bounded(16);
        let first = BlockPos {
            x: first_x,
            y: height.sample(&mut random, 0, 128),
            z: first_z,
        };
        assert_eq!(first, BlockPos { x: -3_991, y: 118, z: -3_990 });
        place_replace_blobs(&mut random, first, &cfg, &mut grid);
        let first_writes: Vec<_> = grid.dirty_cells().collect();
        assert_eq!(first_writes.len(), 535);
        let bounds = first_writes.iter().fold(
            (i32::MAX, i32::MAX, i32::MAX, i32::MIN, i32::MIN, i32::MIN),
            |(min_x, min_y, min_z, max_x, max_y, max_z), &(x, y, z, _)| {
                (min_x.min(x), min_y.min(y), min_z.min(z), max_x.max(x), max_y.max(y), max_z.max(z))
            },
        );
        assert_eq!(bounds, (-3_997, 114, -3_997, -3_985, 122, -3_983));

        let second_x = -4_000 + random.next_int_bounded(16);
        let second_z = -4_000 + random.next_int_bounded(16);
        let second = BlockPos {
            x: second_x,
            y: height.sample(&mut random, 0, 128),
            z: second_z,
        };
        assert_eq!(second, BlockPos { x: -3_993, y: 91, z: -3_989 });
    }

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
    fn crimson_roots_use_their_support_tag_when_sturdiness_disagrees() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        let pos = BlockPos { x: 5, y: 70, z: 5 };
        grid.seed(pos.x, pos.y - 1, pos.z, "minecraft:water[level=0]".to_string());
        grid.seed(pos.x, pos.y, pos.z, "minecraft:crimson_roots".to_string());
        let mut tags = VegTags::default();
        tags.supports_crimson_roots.insert("minecraft:water".to_string());
        let state = grid.get_id(pos.x, pos.y, pos.z);

        assert!(simple_block_can_survive(&grid, &tags, state, pos));
        assert!(!sturdy_at(&grid, pos.x, pos.y - 1, pos.z));
    }

    #[test]
    fn nether_forest_vegetation_checks_the_selected_provider_state() {
        let pos = BlockPos { x: 5, y: 70, z: 5 };
        let cfg = |state: &str| NetherForestVegetationCfg {
            provider: BlockStateProvider::Simple(state.to_string()),
            spread_width: 1,
            spread_height: 1,
        };
        let mut tags = VegTags::default();
        tags.supports_crimson_roots.insert("minecraft:crimson_nylium".to_string());
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed(pos.x, pos.y - 1, pos.z, "minecraft:crimson_nylium".to_string());
        place_nether_forest_vegetation(
            &mut LegacyRandomSource::new(1),
            pos,
            &cfg("minecraft:crimson_roots"),
            &mut grid,
            &tags,
        );
        assert_eq!(grid.get(pos.x, pos.y, pos.z), "minecraft:crimson_roots");

        for state in [
            "minecraft:warped_roots",
            "minecraft:crimson_fungus",
            "minecraft:warped_fungus",
            "minecraft:nether_sprouts",
        ] {
            grid.seed(pos.x, pos.y, pos.z, "minecraft:air".to_string());
            place_nether_forest_vegetation(
                &mut LegacyRandomSource::new(1),
                pos,
                &cfg(state),
                &mut grid,
                &tags,
            );
            assert_eq!(grid.get(pos.x, pos.y, pos.z), state);
        }

        grid.seed(pos.x, pos.y - 1, pos.z, "minecraft:netherrack".to_string());
        grid.seed(pos.x, pos.y, pos.z, "minecraft:air".to_string());
        place_nether_forest_vegetation(
            &mut LegacyRandomSource::new(1),
            pos,
            &cfg("minecraft:crimson_fungus"),
            &mut grid,
            &tags,
        );
        assert_eq!(grid.get(pos.x, pos.y, pos.z), "minecraft:air");

        grid.seed(pos.x, pos.y - 1, pos.z, "minecraft:netherrack".to_string());
        grid.seed(pos.x, pos.y, pos.z, "minecraft:air".to_string());
        place_nether_forest_vegetation(
            &mut LegacyRandomSource::new(1),
            pos,
            &cfg("minecraft:warped_roots"),
            &mut grid,
            &tags,
        );
        assert_eq!(grid.get(pos.x, pos.y, pos.z), "minecraft:air");

        grid.seed(pos.x, pos.y - 1, pos.z, "minecraft:crimson_nylium".to_string());
        grid.seed(pos.x, pos.y, pos.z, "minecraft:air".to_string());
        place_nether_forest_vegetation(
            &mut LegacyRandomSource::new(1),
            pos,
            &cfg("minecraft:short_grass"),
            &mut grid,
            &tags,
        );
        assert_eq!(grid.get(pos.x, pos.y, pos.z), "minecraft:air");
    }

    #[test]
    fn huge_fungus_keeps_non_replaceable_non_colliding_plants_during_hat_pass() {
        assert!(can_be_replaced("minecraft:air"));
        assert!(can_be_replaced("minecraft:crimson_roots"));
        assert!(!can_be_replaced("minecraft:brown_mushroom"));
        assert!(!can_be_replaced("minecraft:crimson_fungus"));
        assert!(!can_be_replaced("minecraft:weeping_vines"));
        assert!(!can_be_replaced("minecraft:weeping_vines_plant"));
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
