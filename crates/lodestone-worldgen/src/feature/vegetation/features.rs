//! The block-pile and sculk-patch feature bodies, plus the survival rules a
//! simple-block placement checks.
//!
//! Each body reproduces the reference draw order exactly, because the
//! structure-placement stream it consumes is shared with every later feature
//! element of the same structure. Where a check needs world state generation
//! does not carry, it is narrowed rather than dropped:
//!
//! * full upward face support and solidity come from the generated
//!   six-direction face-occlusion table;
//! * survival is the target's own family rule, or "support below is not air";
//!   vegetation uses `#supports_vegetation`;
//! * scheduled block ticks are dropped: there is no tick queue during
//!   generation, but the block itself still lands.
//!
//! A state outside the canonical table stays visible, the conservative result
//! for a shape whose occlusion has not been measured.

use std::cell::RefCell;
use crate::feature::{BlockPos, IntProvider};
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, PropertyKey};
use lodestone_data::block_states::StateId;
use crate::rng::RandomSource;
use lodestone_data::collision_shapes;
use lodestone_data::face_occlusion::{self, Face};

use super::config::{BlockStateProvider, VegTags};
use super::grid::VegGrid;
use super::ids::{Tag, tag_at};

fn builtin_state(_grid: &VegGrid, block: Block) -> StateId {
    block.default_state()
}

fn builtin_default(grid: &VegGrid, block: Block) -> StateId {
    builtin_state(grid, block)
}

fn variant_state(_grid: &VegGrid, base: StateId, edits: &[(lodestone_data::block_properties::PropertyKey, lodestone_data::block_properties::BuiltinPropertyValue)]) -> StateId {
    let mut properties = lodestone_data::block_properties::Properties::from_state_id(base);
    for &(key, value) in edits {
        properties = properties.with_builtin(key, value).unwrap_or(properties);
    }
    lodestone_data::block_properties::Properties::state_for_block(base.block(), &properties)
        .unwrap_or(base)
}

fn base_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> Block {
    block_at(grid, x, y, z)
}

fn is_fluid(base: Block) -> bool {
    matches!(base, Block::Water | Block::Lava)
}

fn block_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> Block {
    grid.get_id(x, y, z).block()
}

fn canonical_base_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> StateId {
    grid.get_id(x, y, z).block().default_state()
}

fn air_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    matches!(block_at(grid, x, y, z), Block::Air | Block::CaveAir | Block::VoidAir)
}

/// Solidity and sturdy-face support, narrowed to "occupied by something that is
/// not a fluid and does block motion". See this module's doc.
fn sturdy_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    let state = canonical_base_at(grid, x, y, z);
    !matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir | Block::Water | Block::Lava)
        && lodestone_data::block_solidity::blocks_motion(state)
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
const SIMPLE_BLOCK_SURVIVAL: &[(Block, SimpleBlockSurvival)] = &[
    (Block::DeadBush, SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    (Block::ShortDryGrass, SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    (Block::TallDryGrass, SimpleBlockSurvival::TagBelow(Tag::SupportsDryVegetation)),
    (Block::Azalea, SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
    (Block::FloweringAzalea, SimpleBlockSurvival::TagBelow(Tag::SupportsAzalea)),
    (Block::CrimsonRoots, SimpleBlockSurvival::TagBelow(Tag::SupportsCrimsonRoots)),
    (Block::BrownMushroom, SimpleBlockSurvival::Mushroom),
    (Block::RedMushroom, SimpleBlockSurvival::Mushroom),
    (Block::SmallDripleaf, SimpleBlockSurvival::SmallDripleaf),
    (Block::LilyPad, SimpleBlockSurvival::LilyPad),
    (Block::SporeBlossom, SimpleBlockSurvival::CeilingCenter),
    (Block::LeafLitter, SimpleBlockSurvival::SturdyBelow),
    (Block::MossCarpet, SimpleBlockSurvival::NonAirBelow),
    (Block::PaleMossCarpet, SimpleBlockSurvival::NonAirBelow),
    (Block::Fire, SimpleBlockSurvival::Fire),
    (Block::SoulFire, SimpleBlockSurvival::TagBelow(Tag::SoulFireBaseBlocks)),
    (Block::Melon, SimpleBlockSurvival::Always),
    (Block::Pumpkin, SimpleBlockSurvival::Always),
    (Block::Tuff, SimpleBlockSurvival::Always),
    (Block::PotentSulfur, SimpleBlockSurvival::Always),
];

fn simple_block_survival_rule(base: Block) -> SimpleBlockSurvival {
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
    let base = state.block();
    // These states are selected by nether_forest_vegetation as well as being
    // placeable blocks. Their support tag includes nylium (and fungi also
    // accept mycelium), while the ordinary vegetation tag intentionally does
    // not. Keep the family rule here so a weighted provider's selected state
    // participates in the same occupancy gate as the real block.
    let rule = match base {
        Block::CrimsonFungus | Block::WarpedFungus => SimpleBlockSurvival::NetherFungus,
        Block::NetherSprouts => SimpleBlockSurvival::NetherSprouts,
        // The warped-root support tag has the same closure as the sprouts
        // tag, but there is no dedicated bitset slot for it yet. Keep the
        // exact support family here rather than falling back to ordinary
        // overworld vegetation (which would reject roots on nylium).
        Block::WarpedRoots => SimpleBlockSurvival::NetherRoots,
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
                || tags.simple_block_support.solid_render.test_id(grid.get_id(pos.x, pos.y - 1, pos.z))
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
                && tags.simple_block_support.center_support_down.test_id(grid.get_id(pos.x, pos.y + 1, pos.z))
        }
        SimpleBlockSurvival::SturdyBelow => tags.simple_block_support.sturdy_up.test_id(grid.get_id(pos.x, pos.y - 1, pos.z)),
        SimpleBlockSurvival::NonAirBelow => !air_at(grid, pos.x, pos.y - 1, pos.z),
        SimpleBlockSurvival::Fire => {
            tags.simple_block_support.sturdy_up.test_id(grid.get_id(pos.x, pos.y - 1, pos.z))
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
    let below = block_at(grid, pos.x, pos.y - 1, pos.z);
    tags.supports_vegetation.contains(&below)
        || below == Block::CrimsonNylium
        || below == Block::WarpedNylium
        || below == Block::SoulSoil
        || (fungus && below == Block::Mycelium)
}

/// Exact per-state fire capability supplied by the version boundary.
fn fire_fuel_at(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    tags.simple_block_support.fire_flammable.test_id(grid.get_id(x, y, z))
}

fn water_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> bool {
    base_at(grid, x, y, z) == Block::Water
}

// ---------------------------------------------------------------------------
// Configs
// ---------------------------------------------------------------------------

/// `SculkPatchConfiguration`.
#[derive(Clone, Debug)]
pub(crate) struct SculkPatchCfg {
    pub(crate) charge_count: i32,
    pub(crate) amount_per_charge: i32,
    pub(crate) spread_attempts: i32,
    pub(crate) growth_rounds: i32,
    pub(crate) spread_rounds: i32,
    pub(crate) extra_rare_growths: IntProvider,
    pub(crate) catalyst_chance: f32,
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

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
    let ok = if below == Block::DirtPath {
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

/// `MultifaceGrowthConfiguration`'s `validDirections`, in its own build order:
/// ceiling (UP), floor (DOWN), then `Plane.HORIZONTAL` (N, E, S, W). The order is
/// the shuffle's input, so it decides the output.
#[derive(Clone, Copy)]
struct DirectionList {
    values: [(i32, i32, i32); 6],
    len: usize,
}

impl DirectionList {

    fn as_slice(&self) -> &[(i32, i32, i32)] {
        &self.values[..self.len]
    }
}

impl<'a> IntoIterator for &'a DirectionList {
    type Item = &'a (i32, i32, i32);
    type IntoIter = std::slice::Iter<'a, (i32, i32, i32)>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl IntoIterator for DirectionList {
    type Item = (i32, i32, i32);
    type IntoIter = std::iter::Take<std::array::IntoIter<(i32, i32, i32), 6>>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter().take(self.len)
    }
}

fn same_axis(a: (i32, i32, i32), b: (i32, i32, i32)) -> bool {
    (a.0 != 0 && b.0 != 0) || (a.1 != 0 && b.1 != 0) || (a.2 != 0 && b.2 != 0)
}

fn has_face(grid: &VegGrid, state: StateId, face: lodestone_data::block_properties::PropertyKey) -> bool {
    let _ = grid;
    lodestone_data::block_properties::Properties::from_state_id(state)
        .get(face)
        .and_then(|value| value.builtin_value())
        == Some(lodestone_data::block_properties::BuiltinPropertyValue::True)
}

/// The growth's own face property for a support at the given offset.
fn face_property(dx: i32, dy: i32, dz: i32) -> lodestone_data::block_properties::PropertyKey {
    match (dx, dy, dz) {
        (0, -1, 0) => lodestone_data::block_properties::PropertyKey::Down,
        (0, 1, 0) => lodestone_data::block_properties::PropertyKey::Up,
        (0, 0, -1) => lodestone_data::block_properties::PropertyKey::North,
        (0, 0, 1) => lodestone_data::block_properties::PropertyKey::South,
        (-1, 0, 0) => lodestone_data::block_properties::PropertyKey::West,
        _ => lodestone_data::block_properties::PropertyKey::East,
    }
}

fn shuffled_array<R: RandomSource, const N: usize>(
    random: &mut R,
    input: &[(i32, i32, i32); N],
) -> [(i32, i32, i32); N] {
    let mut out = *input;
    shuffle_offsets(random, &mut out);
    out
}

fn shuffle_offsets<R: RandomSource>(random: &mut R, values: &mut [(i32, i32, i32)]) {
    let mut i = values.len();
    while i > 1 {
        let j = random.next_int_bounded(i as i32) as usize;
        values.swap(i - 1, j);
        i -= 1;
    }
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

thread_local! {
    static SCULK_CURSORS: RefCell<Vec<SculkCursor>> = const { RefCell::new(Vec::new()) };
    static SCULK_NEXT_CURSORS: RefCell<Vec<SculkCursor>> = const { RefCell::new(Vec::new()) };
}

fn return_sculk_cursors(mut cursors: Vec<SculkCursor>, mut next: Vec<SculkCursor>) {
    cursors.clear();
    next.clear();
    SCULK_CURSORS.with(|slot| *slot.borrow_mut() = cursors);
    SCULK_NEXT_CURSORS.with(|slot| *slot.borrow_mut() = next);
}

fn sculk_behaviour(grid: &VegGrid, pos: BlockPos) -> SculkBehaviourKind {
    match base_at(grid, pos.x, pos.y, pos.z) {
        Block::Sculk => SculkBehaviourKind::Sculk,
        Block::SculkVein => SculkBehaviourKind::Vein,
        _ => SculkBehaviourKind::Default,
    }
}

fn sculk_is_water_source(grid: &VegGrid, id: StateId) -> bool {
    let _ = grid;
    id.block() == Block::Water
        && lodestone_data::block_properties::Properties::from_state_id(id)
            .get(lodestone_data::block_properties::PropertyKey::Level)
            .and_then(|value| value.builtin_value())
            .is_none_or(|value| value == lodestone_data::block_properties::BuiltinPropertyValue::Value0)
}

fn sculk_is_air(grid: &VegGrid, id: StateId) -> bool {
    let _ = grid;
    matches!(id.block(), Block::Air | Block::CaveAir | Block::VoidAir)
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
    face_occlusion::occludes(grid.get_id(pos.x, pos.y, pos.z), face)
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
    let boxes = collision_shapes::collision_boxes(state);
    boxes.len() == 1 && boxes[0].min == [0.0; 3] && boxes[0].max == [1.0; 3]
}

fn sculk_can_spread_from(grid: &VegGrid, origin: BlockPos) -> bool {
    if sculk_behaviour(grid, origin) != SculkBehaviourKind::Default {
        return true;
    }
    let state = grid.get_id(origin.x, origin.y, origin.z);
    if !(sculk_is_air(grid, state) || sculk_is_water_source(grid, state)) {
        return false;
    }
    SCULK_DIRECTIONS.iter().any(|&(dx, dy, dz)| {
        sculk_full_collision_at(
            grid,
            BlockPos { x: origin.x + dx, y: origin.y + dy, z: origin.z + dz },
        )
    })
}

#[derive(Clone, Copy)]
struct SculkVeinState(u8);

impl SculkVeinState {
    fn from_state(state: StateId) -> Option<Self> {
        (state.block() == Block::SculkVein)
            .then(|| Self((Block::SculkVein.default_state().raw() - state.raw()) as u8))
    }

    fn from_mask(mask: u8, waterlogged: bool) -> Self {
        Self(
            ((mask & 1) << 6)
                | ((mask & 2) << 4)
                | ((mask & 4) << 2)
                | (mask & 8)
                | ((mask & 16) >> 2)
                | ((mask & 32) >> 5)
                | (u8::from(waterlogged) << 1),
        )
    }

    fn state_id(self) -> StateId {
        StateId::new(Block::SculkVein.default_state().raw() - u32::from(self.0))
            .expect("sculk vein has all 128 boolean states")
    }

    fn face_mask(self) -> u8 {
        ((self.0 & 64) >> 6)
            | ((self.0 & 4) >> 1)
            | ((self.0 & 16) >> 2)
            | (self.0 & 8)
            | ((self.0 & 1) << 4)
            | (self.0 & 32)
    }

    fn waterlogged(self) -> bool {
        self.0 & 2 != 0
    }
}

fn sculk_face_mask(grid: &VegGrid, state: StateId) -> u8 {
    if let Some(vein) = SculkVeinState::from_state(state) {
        return vein.face_mask();
    }
    SCULK_DIRECTIONS.iter().enumerate().fold(0, |mask, (index, &offset)| {
        let face = face_property(offset.0, offset.1, offset.2);
        if has_face(grid, state, face) {
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

fn sculk_vein_state_id(_grid: &VegGrid, mask: u8, waterlogged: bool) -> StateId {
    SculkVeinState::from_mask(mask, waterlogged).state_id()
}

fn sculk_waterlogged(state: StateId) -> bool {
    state.block() == Block::Water
        || SculkVeinState::from_state(state).map_or_else(
            || lodestone_data::block_properties::Properties::from_state_id(state)
                .get(PropertyKey::Waterlogged)
                .and_then(|value| value.builtin_value())
                == Some(BuiltinPropertyValue::True),
            SculkVeinState::waterlogged,
        )
}

fn sculk_vein_state_with_face(grid: &VegGrid, old: StateId, face: (i32, i32, i32)) -> StateId {
    let mask = sculk_face_mask(grid, old)
        | (1 << SCULK_DIRECTIONS.iter().position(|&candidate| candidate == face).unwrap());
    sculk_vein_state_id(
        grid,
        mask,
        sculk_waterlogged(old),
    )
}

fn sculk_can_replace_vein_target(grid: &VegGrid, target: BlockPos) -> bool {
    let state = grid.get_id(target.x, target.y, target.z);
    let base = state.block();
    (sculk_is_air(grid, state)
        || base == lodestone_data::block::Block::SculkVein
        || sculk_is_water_source(grid, state))
        && base != lodestone_data::block::Block::Sculk
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
    if beyond == Block::Sculk
        || beyond == Block::SculkCatalyst
        || beyond == Block::MovingPiston
    {
        return false;
    }
    if base == Block::Sculk
        || base == Block::SculkCatalyst
        || base == Block::MovingPiston
        || base == Block::Fire
        || (is_fluid(base) && base != Block::Water)
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
    source_state: StateId,
    same_position_only: bool,
) -> bool {
    let other_source = source_state.block() != Block::SculkVein;
    let source_mask = sculk_face_mask(grid, source_state);
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
                let old_id = grid.get_id(target.x, target.y, target.z);
                let next = sculk_vein_state_with_face(grid, old_id, face);
                if next == old_id || !grid.set_id_if_in_bounds(target.x, target.y, target.z, next) {
                    continue;
                }
                placed = true;
                break;
            }
        }
    }
    placed
}

fn sculk_regrow_vein(grid: &mut VegGrid, pos: BlockPos, source_state: StateId, facings: u8) -> bool {
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
    let waterlogged = sculk_waterlogged(source_state);
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, sculk_vein_state_id(grid, mask, waterlogged))
}

fn sculk_attempt_spread_vein(grid: &mut VegGrid, pos: BlockPos, source_state: StateId, facings: Option<u8>) -> bool {
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

fn sculk_has_substrate_access(grid: &VegGrid, tags: &VegTags, pos: BlockPos, state: StateId) -> bool {
    if state.block() != Block::SculkVein {
        return false;
    }
    let mask = sculk_face_mask(grid, state);
    SCULK_DIRECTIONS.iter().enumerate().any(|(index, &face)| {
        sculk_has_face(mask, face)
            && tags.sculk_replaceable.contains(&block_at(
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
    for (dx, dy, dz) in shuffled_array(random, &SCULK_NON_CORNER_NEIGHBOURS) {
        let target = BlockPos { x: from.x + dx, y: from.y + dy, z: from.z + dz };
        if sculk_behaviour(grid, target) == SculkBehaviourKind::Default
            || !sculk_movement_unobstructed(grid, from, target)
        {
            continue;
        }
        fallback = Some(target);
        if sculk_has_substrate_access(grid, tags, target, grid.get_id(target.x, target.y, target.z)) {
            break;
        }
    }
    fallback
}

fn sculk_on_discharged(grid: &mut VegGrid, pos: BlockPos, state: StateId) {
    if state.block() != Block::SculkVein {
        return;
    }
    let mut mask = sculk_face_mask(grid, state);
    for (index, &face) in SCULK_DIRECTIONS.iter().enumerate() {
        if sculk_has_face(mask, face)
            && base_at(grid, pos.x + face.0, pos.y + face.1, pos.z + face.2) == Block::Sculk
        {
            mask &= !(1 << index);
        }
    }
    let waterlogged = sculk_waterlogged(state);
    let next = if mask == 0 {
        if waterlogged {
            builtin_default(grid, Block::Water)
        } else {
            builtin_default(grid, Block::Air)
        }
    } else {
        sculk_vein_state_id(grid, mask, waterlogged)
    };
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, next);
}

fn sculk_random_growth_state<R: RandomSource>(
    random: &mut R,
    grid: &VegGrid,
    waterlogged: bool,
) -> StateId {
    let growth_roll = random.next_int_bounded(11);
    if growth_roll == 0 {
        variant_state(
            grid,
            builtin_default(grid, Block::SculkShrieker),
            &[
                (lodestone_data::block_properties::PropertyKey::CanSummon, lodestone_data::block_properties::BuiltinPropertyValue::True),
                (lodestone_data::block_properties::PropertyKey::Shrieking, lodestone_data::block_properties::BuiltinPropertyValue::False),
                (lodestone_data::block_properties::PropertyKey::Waterlogged, if waterlogged { lodestone_data::block_properties::BuiltinPropertyValue::True } else { lodestone_data::block_properties::BuiltinPropertyValue::False }),
            ],
        )
    } else {
        variant_state(
            grid,
            builtin_default(grid, Block::SculkSensor),
            &[
                (lodestone_data::block_properties::PropertyKey::Facing, lodestone_data::block_properties::BuiltinPropertyValue::North),
                (lodestone_data::block_properties::PropertyKey::Power, lodestone_data::block_properties::BuiltinPropertyValue::Value0),
                (lodestone_data::block_properties::PropertyKey::SculkSensorPhase, lodestone_data::block_properties::BuiltinPropertyValue::Inactive),
                (lodestone_data::block_properties::PropertyKey::Waterlogged, if waterlogged { lodestone_data::block_properties::BuiltinPropertyValue::True } else { lodestone_data::block_properties::BuiltinPropertyValue::False }),
            ],
        )
    }
}

fn sculk_can_place_growth(grid: &VegGrid, pos: BlockPos) -> bool {
    let above = grid.get_id(pos.x, pos.y + 1, pos.z);
    if !(sculk_is_air(grid, above) || sculk_is_water_source(grid, above)) {
        return false;
    }
    let mut growth_count = 0;
    for x in pos.x - 4..=pos.x + 4 {
        for y in pos.y..=pos.y + 2 {
            for z in pos.z - 4..=pos.z + 4 {
                let base = base_at(grid, x, y, z);
                if base == Block::SculkSensor || base == Block::SculkShrieker {
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
    state_mask: u8,
) -> bool {
    for support in shuffled_array(random, &SCULK_DIRECTIONS) {
        if !sculk_has_face(state_mask, support) {
            continue;
        }
        let support_pos = BlockPos {
            x: pos.x + support.0,
            y: pos.y + support.1,
            z: pos.z + support.2,
        };
        if !tags
            .sculk_replaceable_world_gen
            .contains(&block_at(grid, support_pos.x, support_pos.y, support_pos.z))
        {
            continue;
        }
        let sculk = builtin_default(grid, Block::Sculk);
        grid.set_id_if_in_bounds(support_pos.x, support_pos.y, support_pos.z, sculk);
        sculk_spread_all(
            grid,
            support_pos,
            sculk,
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
            let neighbour_state = grid.get_id(neighbour.x, neighbour.y, neighbour.z);
            if neighbour_state.block() == Block::SculkVein
            {
                sculk_on_discharged(grid, neighbour, neighbour_state);
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
            let state = grid.get_id(cursor.pos.x, cursor.pos.y, cursor.pos.z);
            let state_mask = sculk_face_mask(grid, state);
            if spread_veins && sculk_attempt_place_sculk(random, grid, tags, cursor.pos, state_mask) {
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
                    let above = grid.get_id(cursor.pos.x, cursor.pos.y + 1, cursor.pos.z);
                    let growth = sculk_random_growth_state(random, grid, sculk_is_water_source(grid, above));
                    grid.set_id_if_in_bounds(cursor.pos.x, cursor.pos.y + 1, cursor.pos.z, growth);
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
    let mut state = grid.get_id(cursor.pos.x, cursor.pos.y, cursor.pos.z);
    let mut behaviour = sculk_behaviour(grid, cursor.pos);
    if spread_veins && sculk_attempt_spread_vein(grid, cursor.pos, state, cursor.facings) {
        if behaviour != SculkBehaviourKind::Sculk {
            state = grid.get_id(cursor.pos.x, cursor.pos.y, cursor.pos.z);
            behaviour = sculk_behaviour(grid, cursor.pos);
        }
    }
    cursor.charge = sculk_attempt_use_charge(random, grid, tags, cursor, origin, behaviour, spread_veins);
    if cursor.charge <= 0 {
        sculk_on_discharged(grid, cursor.pos, state);
        return;
    }
    if let Some(next) = sculk_valid_movement_pos(random, grid, tags, cursor.pos) {
        sculk_on_discharged(grid, cursor.pos, state);
        cursor.pos = next;
        let dx = cursor.pos.x - origin.x;
        let dz = cursor.pos.z - origin.z;
        if dx * dx + dz * dz >= 15 * 15 {
            cursor.charge = 0;
            return;
        }
        state = grid.get_id(cursor.pos.x, cursor.pos.y, cursor.pos.z);
    }
    if sculk_behaviour(grid, cursor.pos) != SculkBehaviourKind::Default {
        cursor.facings = Some(sculk_face_mask(grid, state));
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
    let mut cursors = SCULK_CURSORS.take();
    let mut next = SCULK_NEXT_CURSORS.take();
    cursors.clear();
    next.clear();
    let rounds = cfg.spread_rounds + cfg.growth_rounds;
    for round in 0..rounds {
        cursors.clear();
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
            next.clear();
            for mut cursor in cursors.drain(..) {
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
            std::mem::swap(&mut cursors, &mut next);
            if cursors.is_empty() {
                break;
            }
        }
    }
    let below = BlockPos { x: pos.x, y: pos.y - 1, z: pos.z };
    if random.next_float() <= cfg.catalyst_chance && sculk_full_collision_at(grid, below) {
        let state = builtin_default(grid, Block::SculkCatalyst);
        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
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
            let shrieker = variant_state(
                grid,
                builtin_default(grid, Block::SculkShrieker),
                &[
                    (PropertyKey::CanSummon, BuiltinPropertyValue::True),
                    (PropertyKey::Shrieking, BuiltinPropertyValue::False),
                    (PropertyKey::Waterlogged, BuiltinPropertyValue::False),
                ],
            );
            grid.set_id_if_in_bounds(candidate.x, candidate.y, candidate.z, shrieker);
        }
    }
    return_sculk_cursors(cursors, next);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::feature::state_predicate::StatePredicate;
    use crate::rng::{LegacyRandomSource, RandomSource};

    fn fixture_state(spec: &str) -> StateId {
        StateId::from_state_str(spec).expect("test state is in the generated table")
    }

    fn fixture_block(name: &str) -> Block {
        Block::from_name(name).expect("test block is in the generated registry")
    }

    #[test]
    fn sculk_numeric_states_match_registry_properties() {
        let grid = VegGrid::new(-64, 384, 0, 0);
        assert_eq!(Block::SculkVein.default_state().raw(), 27771);
        for raw in 27644..27772 {
            let state = StateId::new(raw).unwrap();
            let properties = state.properties();
            let enabled = |key: &str| properties.iter().any(|(name, value)| *name == key && *value == "true");
            let mask_for = |keys: [&str; 6]| {
                keys.iter().enumerate().fold(0, |mask, (index, key)| {
                    mask | (u8::from(enabled(key)) << index)
                })
            };
            let read_mask = mask_for(["down", "up", "north", "south", "west", "east"]);
            let write_mask = mask_for(["down", "east", "north", "south", "up", "west"]);
            assert_eq!(sculk_face_mask(&grid, state), read_mask, "state {raw}");
            assert_eq!(sculk_waterlogged(state), enabled("waterlogged"), "state {raw}");
            assert_eq!(
                sculk_vein_state_id(&grid, write_mask, enabled("waterlogged")),
                state,
                "state {raw}",
            );
        }
        for raw in [27643, 27772] {
            assert!(SculkVeinState::from_state(StateId::new(raw).unwrap()).is_none());
        }
    }

    #[test]
    fn sculk_numeric_states_preserve_asymmetric_mask_orders() {
        let grid = VegGrid::new(-64, 384, 0, 0);
        let expected = fixture_state(
            "minecraft:sculk_vein[down=false,east=true,north=false,south=false,up=true,waterlogged=true,west=false]",
        );
        assert_eq!(expected.raw(), 27733);
        assert_eq!(sculk_vein_state_id(&grid, 0b010010, true), expected);
        assert_eq!(sculk_face_mask(&grid, expected), 0b100010);
        assert_ne!(sculk_face_mask(&grid, expected), 0b010010);
        let dry = sculk_vein_state_id(&grid, 0b010010, false);
        assert_eq!(dry.raw(), 27735);
        assert_eq!(sculk_face_mask(&grid, dry), 0b100010);
        assert!(!sculk_waterlogged(dry));
    }

    #[test]
    fn sculk_numeric_states_keep_non_vein_water_and_face_properties() {
        let grid = VegGrid::new(-64, 384, 0, 0);
        let lichen = fixture_state(
            "minecraft:glow_lichen[down=false,east=true,north=false,south=false,up=false,waterlogged=true,west=false]",
        );
        assert!(SculkVeinState::from_state(lichen).is_none());
        assert_eq!(sculk_face_mask(&grid, lichen), 0b100000);
        assert!(sculk_waterlogged(lichen));
        assert!(sculk_waterlogged(Block::Water.default_state()));
        assert!(!sculk_waterlogged(Block::Stone.default_state()));
        assert_eq!(sculk_face_mask(&grid, Block::Stone.default_state()), 0);
        assert_eq!(
            sculk_vein_state_with_face(&grid, Block::Water.default_state(), (0, -1, 0)).raw(),
            27705,
        );
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
    fn sculk_neighbour_shuffle_supports_all_eighteen_offsets() {
        let mut random = LegacyRandomSource::new(0x5c01_5eed);
        let shuffled = shuffled_array(&mut random, &SCULK_NON_CORNER_NEIGHBOURS);
        let next_after_shuffle = random.next_int();

        assert_eq!(
            shuffled.into_iter().collect::<HashSet<_>>(),
            SCULK_NON_CORNER_NEIGHBOURS.into_iter().collect(),
        );

        let mut draw_control = LegacyRandomSource::new(0x5c01_5eed);
        for bound in (2..=SCULK_NON_CORNER_NEIGHBOURS.len()).rev() {
            let _ = draw_control.next_int_bounded(bound as i32);
        }
        assert_eq!(next_after_shuffle, draw_control.next_int());
    }

    #[test]
    fn sculk_same_position_rejects_a_sculk_support_cell() {
        let origin = BlockPos { x: 8, y: 0, z: 8 };
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        grid.seed_id(origin.x, origin.y, origin.z, fixture_state("minecraft:air"));
        grid.seed_id(origin.x, origin.y - 1, origin.z, fixture_state("minecraft:sculk"));
        assert!(!sculk_spread_candidate_allowed(
            &grid,
            origin,
            origin,
            (0, -1, 0),
        ));

        grid.seed_id(origin.x, origin.y - 1, origin.z, fixture_state("minecraft:deepslate"));
        assert!(sculk_spread_candidate_allowed(
            &grid,
            origin,
            origin,
            (0, -1, 0),
        ));
    }

    #[test]
    fn simple_block_survival_uses_exact_state_capabilities() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in 0..16 { for z in 0..16 { for y in 60..80 { grid.seed_id(x, y, z, fixture_state("minecraft:air")); } } }
        let mut tags = VegTags::default();
        tags.simple_block_support = super::super::config::SimpleBlockSupport {
            solid_render: StatePredicate::new(["minecraft:stone".to_string()].into_iter().collect(), HashMap::new()),
            sturdy_up: StatePredicate::new(["minecraft:stone".to_string()].into_iter().collect(), HashMap::new()),
            center_support_down: StatePredicate::new(HashSet::new(), [("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]".to_string(), true)].into_iter().collect()),
            fire_flammable: StatePredicate::new(["minecraft:oak_planks".to_string()].into_iter().collect(), HashMap::new()),
        };
        let pos = BlockPos { x: 5, y: 70, z: 5 };
        let survives = |grid: &mut VegGrid, state: &str| {
            grid.seed_id(pos.x, pos.y, pos.z, fixture_state(state));
            simple_block_can_survive(grid, &tags, grid.get_id(pos.x, pos.y, pos.z), pos)
        };

        grid.seed_id(5, 69, 5, fixture_state("minecraft:stone"));
        assert!(survives(&mut grid, "minecraft:brown_mushroom"), "solid-render stone supports a mushroom during unlit decoration");
        assert!(survives(&mut grid, "minecraft:leaf_litter[segment_amount=1,facing=north]"), "the exact full-up state supports leaf litter");
        grid.seed_id(5, 71, 5, fixture_state("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]"));
        assert!(survives(&mut grid, "minecraft:spore_blossom"), "center-down support is independent of full-up support");
        grid.seed_id(5, 69, 5, fixture_state("minecraft:air"));
        grid.seed_id(6, 70, 5, fixture_state("minecraft:oak_planks"));
        assert!(survives(&mut grid, "minecraft:fire[age=0,east=false,north=false,south=false,up=false,west=false]"), "a flammable side neighbour supports fire without a sturdy floor");
    }

    #[test]
    fn crimson_roots_use_their_support_tag_when_sturdiness_disagrees() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        let pos = BlockPos { x: 5, y: 70, z: 5 };
        grid.seed_id(pos.x, pos.y - 1, pos.z, fixture_state("minecraft:water[level=0]"));
        grid.seed_id(pos.x, pos.y, pos.z, fixture_state("minecraft:crimson_roots"));
        let mut tags = VegTags::default();
        tags.supports_crimson_roots.insert(Block::Water);
        tags.bind();
        let state = grid.get_id(pos.x, pos.y, pos.z);

        assert!(simple_block_can_survive(&grid, &tags, state, pos));
        assert!(!sturdy_at(&grid, pos.x, pos.y - 1, pos.z));
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
            let actual = simple_block_survival_rule(fixture_block(output));
            let intended = expected.get(output.as_str()).copied().unwrap_or(SimpleBlockSurvival::Vegetation);
            assert_eq!(actual, intended, "{output} must use its audited survival family");
        }
        assert_eq!(SIMPLE_BLOCK_SURVIVAL.len(), expected.len(), "no exceptional state may silently take the vegetation fallback");
    }
}
