//! NBT **structure templates** — the reader, the rotation/mirror transform, and
//! the write into a chunk's block grid.
//!
//! # What it is
//!
//! The structure-template type reads `.nbt` files under
//! `assets/structure/` (1212 of them, see
//! [`docs/worldgen-structure-corpus.md`](../../../../docs/worldgen-structure-corpus.md))
//! decoded into a palette plus a block list, and placed into a
//! [`DenseBlockGrid`] with a rotation, an optional mirror and a processor chain.
//! [`super::StructureKind`] builds the pieces; this module is what turns one into
//! blocks.
//!
//! # How it works
//!
//! ```text
//! parse:  gzip -> named NBT -> { size, palettes[], blocks[] }
//! place:  for each block:
//!             world = transform(rel, mirror, rotation, pivot) + position
//!             state = palette[block.state]                  <- unrotated
//!             state = processors.process(world, state)?      <- may drop it
//!             state = state.mirror(m).rotate(r)              <- placement order
//!             grid.set(world, state)                         <- clipped by the grid
//! ```
//!
//! Two properties make this work chunk-at-a-time with no cross-chunk state, which
//! is what our per-chunk memoised pipeline needs:
//!
//! * **Every random draw here is position-seeded.** Palette choice is
//!   a fresh default random source seeded from a position-derived hash of
//!   template position and
//!   the block-rotation processor's keep/drop roll is the same
//!   position-derived hash of each block position — so two
//!   chunks placing two halves of the same piece agree without communicating.
//! * **A write outside the grid's box is a no-op** ([`DenseBlockGrid::set`]), so
//!   "clip this piece to the chunk" needs no explicit box: the grid *is* the box.
//!   The grid therefore provides the placement bounding box by construction.
//!
//! # How to change it
//!
//! * **Rotation is per-property, not per-block** ([`BlockState::rotate`]):
//!   `facing`, `axis`, `rotation` and the four directional booleans. That covers
//!   every property in the 71 templates this unit places. A block whose rotation
//!   needs *block-class* knowledge (a stair's `shape` under a **mirror**, a rail's
//!   `shape`) is **not** handled — the three structures wired here all use
//!   no mirror, where `shape` is invariant, and the gap is named in
//!   [`super::StructureRegistry::unsupported`] rather than left to be discovered.
//! * **Multi-palette templates are normal, not exotic.** Every shipwreck template
//!   carries 8 palettes (the wood species), and the block list is shared across
//!   them — `blocks[i].state` indexes whichever palette
//!   [`StructureTemplate::palette_for`] picked. A reader that assumed a single
//!   `palette` key would silently place nothing for every shipwreck.
//! * Entities are parsed but not placed. Container blocks and their NBT are
//!   retained: the server's structure-loot consumer reads both data markers and
//!   a container's own loot fields to attach contents after
//!   the structure stage. The marker blocks themselves are dropped by the same
//!   block-ignore processor as ordinary template placement.
//! * **A block's `nbt` compound is retained** ([`TemplateBlock::nbt`]). It is not
//!   decoration: a jigsaw block's *entire*
//!   configuration — `name`, `target`, `pool`, `final_state`, `joint`,
//!   `placement_priority`, `selection_priority` — lives nowhere else, so assembly
//!   reads it through [`StructureTemplate::filter_blocks`]. Only a few
//!   blocks per template carry one, so it is an `Option<Arc<..>>` rather than a
//!   parallel dense array.
//!
//! # Dependencies
//!
//! `lodestone-core` for the NBT codec, `flate2` for the gzip wrapper around these
//! files, and [`crate::dense_grid`] for the write target.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::{Arc, OnceLock};

use lodestone_core::{Nbt, Reader};
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey, PropertyValue};
use lodestone_data::block_states::StateId as CanonicalStateId;
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

use super::BoundingBox;
use super::jigsaw::JigsawBlockInfo;
use super::processor::{ProcessCtx, Processor, ProcessedBlock};
use super::StructureMutationContext;
use crate::dense_grid::DenseBlockGrid;

/// One template block's `nbt` compound, as the flat field list the NBT reader
/// produces.
///
/// Kept unparsed because the two consumers want different keys — jigsaw
/// assembly reads seven of them, a data marker reads `metadata` — and because a
/// typed struct per consumer would have to be widened every time a new one
/// appears.
pub type BlockNbt = Vec<(String, Nbt)>;

/// One string field of a [`BlockNbt`].
#[must_use]
pub fn nbt_string<'a>(nbt: &'a BlockNbt, key: &str) -> Option<&'a str> {
    nbt.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        Nbt::String(s) => Some(s.as_str()),
        _ => None,
    })
}

/// One integer field of a [`BlockNbt`], accepting any of NBT's integral widths
/// `placement_priority` and `selection_priority` are normally `Int`, but
/// accepting `Byte` and `Short` keeps parsing tolerant of older files.
#[must_use]
pub fn nbt_int(nbt: &BlockNbt, key: &str) -> Option<i32> {
    nbt.iter().find(|(k, _)| k == key).and_then(|(_, v)| match *v {
        Nbt::Int(i) => Some(i),
        Nbt::Short(s) => Some(i32::from(s)),
        Nbt::Byte(b) => Some(i32::from(b)),
        _ => None,
    })
}

/// One of the four rotations, in the order used by the position-seeded picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// No rotation.
    #[default]
    None,
    /// A clockwise quarter turn.
    Cw90,
    /// A clockwise half turn.
    Cw180,
    /// A counter-clockwise quarter turn.
    Ccw90,
}

impl Rotation {
    /// Chooses a rotation using the supplied random source.
    pub fn random<R: RandomSource>(random: &mut R) -> Self {
        match random.next_int_bounded(4) {
            1 => Self::Cw90,
            2 => Self::Cw180,
            3 => Self::Ccw90,
            _ => Self::None,
        }
    }

    /// Clockwise quarter turns.
    #[must_use]
    pub fn turns(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Cw90 => 1,
            Self::Cw180 => 2,
            Self::Ccw90 => 3,
        }
    }
}

/// A horizontal mirror operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mirror {
    /// No mirror.
    #[default]
    None,
    /// Negates `z`, so north and south swap.
    LeftRight,
    /// Negates `x`, so east and west swap.
    FrontBack,
}

/// A validated built-in block state used while defining coded structures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockState {
    pub id: CanonicalStateId,
}

impl BlockState {
    /// Creates the default state for a typed built-in block.
    #[must_use]
    pub fn from_block(block: Block) -> Self {
        Self { id: block.default_state() }
    }

    /// A state with no properties.
    #[must_use]
    pub fn of(block: Block) -> Self {
        Self::from_block(block)
    }

    /// Parses `minecraft:oak_stairs[facing=north,half=bottom]`.
    #[must_use]
    pub fn parse(spec: &str) -> Self {
        let Some((name, rest)) = spec.split_once('[') else {
            return Self {
                id: parse_bound_state(spec.trim())
                    .unwrap_or_else(|| panic!("unknown structure block state {spec}")),
            };
        };
        let mut properties = Vec::new();
        for entry in rest.trim_end_matches(']').split(',') {
            if let Some((key, value)) = entry.split_once('=') {
                properties.push((key.trim(), value.trim()));
            }
        }
        let id = bind_state(name.trim(), &properties);
        Self { id }
    }

    /// Rotates supported directional properties (see the module documentation
    /// for the deliberate limits).
    #[must_use]
    pub fn rotate(&self, rotation: Rotation) -> Self {
        self.clone().into_rotate(rotation)
    }

    /// Applies a rotation while consuming the state.
    ///
    /// Placement already owns each processed state. Keeping the transform
    /// consuming lets that path mutate its one property map instead of cloning
    /// the map once for `mirror` and again for `rotate`.
    #[must_use]
    pub fn into_rotate(mut self, rotation: Rotation) -> Self {
        if rotation == Rotation::None {
            return self;
        }
        let mut properties = Properties::from_state_id(self.id);
        let turns = rotation.turns();
        if let Some(facing) = property_name(&properties, "facing") {
            if let Some(rotated) = rotate_direction(facing, turns) {
                set_property(&mut properties, "facing", rotated);
            }
        }
        if let Some(axis) = property_name(&properties, "axis") {
            if turns % 2 == 1 {
                let swapped = match axis {
                    "x" => Some("z"),
                    "z" => Some("x"),
                    _ => None,
                };
                if let Some(swapped) = swapped {
                    set_property(&mut properties, "axis", swapped);
                }
            }
        }
        if let Some(value) = property_name(&properties, "rotation").and_then(|value| value.parse::<u32>().ok()) {
            // Sixteen-step orientation values advance by four per quarter turn.
            let rotated = (value + 4 * turns) % 16;
            let rotated = rotated.to_string();
            set_property(&mut properties, "rotation", &rotated);
        }
        if let Some(orientation) = property_name(&properties, "orientation") {
            if let Some(rotated) = rotate_orientation(orientation, turns) {
                set_property(&mut properties, "orientation", &rotated);
            }
        }
        if let Some(shape) = property_name(&properties, "shape") {
            if let Some(rotated) = rotate_rail_shape(shape, turns) {
                set_property(&mut properties, "shape", rotated);
            }
        }
        rotate_directional_flags(&mut properties, turns);
        self.id = resolve_properties(self.id.block(), properties);
        self
    }

    /// Returns the two components of a jigsaw `orientation` property.
    ///
    /// Returns `None` for a state with no `orientation`, which is every block
    /// except a jigsaw.
    #[must_use]
    pub fn front_and_top(&self) -> Option<(&str, &str)> {
        property_name(&Properties::from_state_id(self.id), "orientation")?.split_once('_')
    }

    /// Mirrors supported directional properties.
    #[must_use]
    pub fn mirror(&self, mirror: Mirror) -> Self {
        self.clone().into_mirror(mirror)
    }

    /// Applies a mirror while consuming the state.
    #[must_use]
    pub fn into_mirror(mut self, mirror: Mirror) -> Self {
        if mirror == Mirror::None {
            return self;
        }
        let mut properties = Properties::from_state_id(self.id);
        let flip = |dir: &str| -> Option<&'static str> {
            match (mirror, dir) {
                (Mirror::LeftRight, "north") => Some("south"),
                (Mirror::LeftRight, "south") => Some("north"),
                (Mirror::FrontBack, "east") => Some("west"),
                (Mirror::FrontBack, "west") => Some("east"),
                _ => None,
            }
        };
        // Stairs require shape-specific handling when the mirror moves their
        // facing. The two mirror axes are intentionally asymmetric here.
        let is_stair = property_name(&properties, "shape").is_some()
            && property_name(&properties, "half").is_some()
            && property_name(&properties, "facing").is_some();
        if is_stair {
            let facing = property_name(&properties, "facing").unwrap_or_default();
            let z_axis = facing == "north" || facing == "south";
            let applies = match mirror {
                Mirror::LeftRight => z_axis,
                Mirror::FrontBack => !z_axis,
                Mirror::None => false,
            };
            if applies {
                if let Some(shape) = property_name(&properties, "shape") {
                    let swapped = match (mirror, shape) {
                        (Mirror::LeftRight, "outer_left") | (Mirror::FrontBack, "outer_left") => {
                            Some("outer_right")
                        }
                        (Mirror::LeftRight, "outer_right") | (Mirror::FrontBack, "outer_right") => {
                            Some("outer_left")
                        }
                        (Mirror::LeftRight, "inner_left") => Some("inner_right"),
                        (Mirror::LeftRight, "inner_right") => Some("inner_left"),
                        // This mirror leaves the inner pair unchanged.
                        _ => None,
                    };
                    if let Some(swapped) = swapped {
                        set_property(&mut properties, "shape", swapped);
                    }
                }
            }
        }
        if let Some(shape) = property_name(&properties, "shape") {
            if let Some(flipped) = mirror_rail_shape(shape, mirror) {
                set_property(&mut properties, "shape", flipped);
            }
        }
        if let Some(facing) = property_name(&properties, "facing") {
            if let Some(flipped) = flip(facing) {
                set_property(&mut properties, "facing", flipped);
            }
        }
        let mut flags: [Option<(usize, &'static str)>; 4] = std::array::from_fn(|_| None);
        for (source, dir) in ["north", "east", "south", "west"].into_iter().enumerate() {
            let Some(value) = property_name(&properties, dir) else {
                continue;
            };
            let target = flip(dir).unwrap_or(dir);
            let target = ["north", "east", "south", "west"]
                .iter()
                .position(|candidate| *candidate == target)
                .unwrap_or(source);
            flags[source] = Some((target, value));
        }
        for (target, value) in flags.into_iter().flatten() {
            set_property(&mut properties, ["north", "east", "south", "west"][target], value);
        }
        self.id = resolve_properties(self.id.block(), properties);
        self
    }

    /// True when this state carries `waterlogged=false`, i.e. the block *has* the
    /// property and is currently dry.
    #[must_use]
    pub fn is_waterloggable_and_dry(&self) -> bool {
        property_name(&Properties::from_state_id(self.id), "waterlogged") == Some("false")
    }
}

fn bind_state(name: &str, properties: &[(&str, &str)]) -> CanonicalStateId {
    let block = Block::from_name(name).unwrap_or_else(|| panic!("unsupported structure block: {name}"));
    let pairs = properties
        .iter()
        .map(|&(key, value)| {
            let key = PropertyKey::from_name(key)
                .unwrap_or_else(|| panic!("unsupported property {key} on structure block {name}"));
            let value = PropertyValue::from_name(value)
                .unwrap_or_else(|| panic!("unsupported property value {value} on structure block {name}"));
            (key, value)
        })
        .collect::<Vec<_>>();
    let typed = Properties::try_from_pairs(&pairs)
        .unwrap_or_else(|error| panic!("invalid properties for structure block {name}: {error}"));
    if let Some(state) = Properties::state_for_block(block, &typed) {
        return state;
    }
    let mut spec = name.to_owned();
    if !properties.is_empty() {
        spec.push('[');
        for (index, &(key, value)) in properties.iter().enumerate() {
            if index != 0 {
                spec.push(',');
            }
            spec.push_str(key);
            spec.push('=');
            spec.push_str(value);
        }
        spec.push(']');
    }
    let state = parse_bound_state(&spec)
        .unwrap_or_else(|| panic!("unsupported state for structure block {name}"));
    let generated = Properties::from_state_id(state);
    if typed.iter().all(|property| generated.get(property.key()) == Some(property.value())) {
        state
    } else {
        panic!("unsupported state for structure block {name}")
    }
}

fn parse_bound_state(spec: &str) -> Option<CanonicalStateId> {
    lodestone_data::block_states::state_id(spec).and_then(CanonicalStateId::new)
}

fn property_name(properties: &Properties, name: &str) -> Option<&'static str> {
    let key = PropertyKey::from_name(name)?;
    properties.get(key)?.name()
}

fn set_property(properties: &mut Properties, key: &str, value: &str) {
    let Some(key) = PropertyKey::from_name(key) else { return };
    let Some(value) = PropertyValue::from_name(value).and_then(PropertyValue::builtin_value) else {
        return;
    };
    if let Ok(updated) = (*properties).with_builtin(key, value) {
        *properties = updated;
    }
}

fn resolve_properties(block: Block, properties: Properties) -> CanonicalStateId {
    Properties::state_for_block(block, &properties)
        .unwrap_or_else(|| panic!("transformed structure state is not valid for {}", block.name()))
}

fn rotate_state(state: CanonicalStateId, rotation: Rotation) -> CanonicalStateId {
    state_with_transform(state, Mirror::None, rotation)
}

pub(crate) fn state_with_transform(
    state: CanonicalStateId,
    mirror: Mirror,
    rotation: Rotation,
) -> CanonicalStateId {
    if mirror == Mirror::None && rotation == Rotation::None {
        return state;
    }
    BlockState { id: state }
        .into_mirror(mirror)
        .into_rotate(rotation)
        .id
}

pub(crate) fn state_with(
    block: Block,
    values: &[(PropertyKey, BuiltinPropertyValue)],
) -> CanonicalStateId {
    let mut pairs = [(PropertyKey::Age, PropertyValue::builtin(BuiltinPropertyValue::Value0)); 7];
    for (index, &(key, value)) in values.iter().enumerate() {
        pairs[index] = (key, PropertyValue::builtin(value));
    }
    let properties = Properties::try_from_pairs(&pairs[..values.len()])
        .expect("structure state properties are valid");
    Properties::state_for_block(block, &properties).expect("structure state exists")
}

fn with_waterlogged(state: CanonicalStateId, waterlogged: bool) -> CanonicalStateId {
    let mut properties = Properties::from_state_id(state);
    set_property(&mut properties, "waterlogged", if waterlogged { "true" } else { "false" });
    resolve_properties(state.block(), properties)
}

/// Rotates the ten supported rail shape values.
/// Keyed on the `shape` **value** rather than on the presence of neighbouring
/// properties, because `shape` is spelled by two unrelated block families here: a
/// stair's is one of `straight`/`inner_*`/`outer_*` and a rail's is one of the ten
/// below, and the two sets are disjoint. Returning `None` for anything unrecognised
/// is therefore what leaves the stair branch — and every other `shape` — alone.
///
/// The explicit table keeps this mapping independent of canonicalisation.
fn rotate_rail_shape(shape: &str, turns: u32) -> Option<&'static str> {
    let table: [(&str, [&'static str; 3]); 10] = [
        // shape -> [cw90, cw180, ccw90]
        ("north_south", ["east_west", "north_south", "east_west"]),
        ("east_west", ["north_south", "east_west", "north_south"]),
        (
            "ascending_east",
            ["ascending_south", "ascending_west", "ascending_north"],
        ),
        (
            "ascending_west",
            ["ascending_north", "ascending_east", "ascending_south"],
        ),
        (
            "ascending_north",
            ["ascending_east", "ascending_south", "ascending_west"],
        ),
        (
            "ascending_south",
            ["ascending_west", "ascending_north", "ascending_east"],
        ),
        ("south_east", ["south_west", "north_west", "north_east"]),
        ("south_west", ["north_west", "north_east", "south_east"]),
        ("north_west", ["north_east", "south_east", "south_west"]),
        ("north_east", ["south_east", "south_west", "north_west"]),
    ];
    let row = table.iter().find(|(name, _)| *name == shape)?.1;
    match turns % 4 {
        1 => Some(row[0]),
        2 => Some(row[1]),
        3 => Some(row[2]),
        _ => None,
    }
}

/// Mirrors the supported rail shape values. The mapping is asymmetric:
/// `LeftRight` leaves `ascending_east` and
/// `ascending_west` alone and `FrontBack` leaves `ascending_north`/`ascending_south`
/// alone, while both swap the four diagonals — the same shape of asymmetry the stair
/// branch above carries, because the mirror axis must actually move
/// the shape's own axis).
fn mirror_rail_shape(shape: &str, mirror: Mirror) -> Option<&'static str> {
    match mirror {
        Mirror::LeftRight => match shape {
            "ascending_north" => Some("ascending_south"),
            "ascending_south" => Some("ascending_north"),
            "south_east" => Some("north_east"),
            "south_west" => Some("north_west"),
            "north_west" => Some("south_west"),
            "north_east" => Some("south_east"),
            _ => None,
        },
        Mirror::FrontBack => match shape {
            "ascending_east" => Some("ascending_west"),
            "ascending_west" => Some("ascending_east"),
            "south_east" => Some("south_west"),
            "south_west" => Some("south_east"),
            "north_west" => Some("north_east"),
            "north_east" => Some("north_west"),
            _ => None,
        },
        Mirror::None => None,
    }
}

fn rotate_direction(dir: &str, turns: u32) -> Option<&'static str> {
    const CW: [&str; 4] = ["north", "east", "south", "west"];
    let index = CW.iter().position(|d| *d == dir)?;
    Some(CW[(index + turns as usize) % 4])
}

/// Rotates both components of a jigsaw orientation around the Y axis.
/// A vertical component is invariant, which makes this a per-component rewrite
/// rather than a 12-entry table. **Load-bearing for jigsaw assembly**: the front
/// facing of a rotated jigsaw block is the direction the connection points in,
/// and getting it wrong makes every `canAttach` fail — a village that silently
/// consists of its town centre alone.
fn rotate_orientation(orientation: &str, turns: u32) -> Option<String> {
    let (front, top) = orientation.split_once('_')?;
    let front = rotate_direction(front, turns).unwrap_or(front);
    let top = rotate_direction(top, turns).unwrap_or(top);
    Some(format!("{front}_{top}"))
}

/// Returns the opposite cardinal or vertical direction.
#[must_use]
pub fn opposite_direction(dir: &str) -> &str {
    match dir {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        "down" => "up",
        other => other,
    }
}

/// Returns the unit offset of a named direction.
#[must_use]
pub fn direction_step(dir: &str) -> [i32; 3] {
    match dir {
        "north" => [0, 0, -1],
        "south" => [0, 0, 1],
        "west" => [-1, 0, 0],
        "east" => [1, 0, 0],
        "up" => [0, 1, 0],
        "down" => [0, -1, 0],
        _ => [0, 0, 0],
    }
}

/// Returns the cell that supports an attached-facing block. Template placement
/// is clipped to one served chunk, so the support may be outside `grid` and
/// therefore intentionally read as air. The structure writer performs this
/// shape update after all writes in a piece; keeping it here makes the same
/// ordering observable for ladders, wall banners, and the other wall-mounted
/// families rather than leaving them permanently attached to a clipped piece.
fn attached_support_position(pos: [i32; 3], state: CanonicalStateId) -> Option<[i32; 3]> {
    let block = state.block();
    let name = block.name();
    let attached = matches!(
        block,
        Block::Ladder
            | Block::Cocoa
            | Block::TripwireHook
            | Block::AmethystCluster
            | Block::SmallAmethystBud
            | Block::MediumAmethystBud
            | Block::LargeAmethystBud
    ) || name.ends_with("_wall_banner")
        || name.ends_with("_wall_hanging_sign")
        || name.ends_with("_wall_sign")
        || name.ends_with("_wall_torch")
        || name.ends_with("_wall_coral_fan");
    if !attached {
        return None;
    }
    let facing = property_name(&Properties::from_state_id(state), "facing")?;
    let step = direction_step(facing);
    Some([pos[0] - step[0], pos[1] - step[1], pos[2] - step[2]])
}

/// The four directional booleans of a fence/pane/vine, permuted by `turns`.
fn rotate_directional_flags(properties: &mut Properties, turns: u32) {
    const CW: [&str; 4] = ["north", "east", "south", "west"];
    let current = [
        property_name(properties, CW[0]),
        property_name(properties, CW[1]),
        property_name(properties, CW[2]),
        property_name(properties, CW[3]),
    ];
    if current.iter().all(Option::is_none) {
        return;
    }
    for (i, dir) in CW.iter().enumerate() {
        let source = (i + 4 - turns as usize % 4) % 4;
        if let Some(value) = current[source].clone() {
            set_property(properties, dir, value);
        }
    }
}

/// Applies mirror and rotation around `pivot` to a template-local position.
#[must_use]
pub fn transform(pos: [i32; 3], mirror: Mirror, rotation: Rotation, pivot: [i32; 3]) -> [i32; 3] {
    let [mut x, y, mut z] = pos;
    match mirror {
        Mirror::LeftRight => z = -z,
        Mirror::FrontBack => x = -x,
        Mirror::None => {}
    }
    let (px, pz) = (pivot[0], pivot[2]);
    match rotation {
        Rotation::Ccw90 => [px - pz + z, y, px + pz - x],
        Rotation::Cw90 => [px + pz - z, y, pz - px + x],
        Rotation::Cw180 => [px + px - x, y, pz + pz - z],
        Rotation::None => [x, y, z],
    }
}

/// The three origins needed when placing a template.
/// Grouped because each arrives from a different context and
/// is easy to get wrong on its own: `position` is the piece's, `reference` is the
/// whole *start*'s (see [`super::jigsaw::reference_position`]), and `seed` is the
/// **world** seed rather than any derived stream — [`Processor::Capped`] forks it
/// positionally, so a per-chunk or per-structure seed here would give one piece a
/// different set of suspicious blocks in each chunk it spans.
#[derive(Debug, Clone, Copy)]
pub struct PlaceOrigin {
    /// Where template-local `(0,0,0)` lands.
    pub position: [i32; 3],
    /// Reference position used by processors.
    pub reference: [i32; 3],
    /// World seed used by processors.
    pub seed: i64,
}

/// How one piece places its template.
const JIGSAW_BLOCK_NAME: &str = "minecraft:jigsaw";

#[derive(Debug, Clone)]
pub struct PlaceSettings {
    /// Template rotation.
    pub rotation: Rotation,
    /// Template mirror.
    pub mirror: Mirror,
    /// Rotation pivot.
    pub pivot: [i32; 3],
    /// The processor chain, in `addProcessor` order.
    pub processors: Vec<Processor>,
    /// Whether dry waterloggable blocks should be waterlogged.
    pub waterlogging: bool,
}

impl Default for PlaceSettings {
    fn default() -> Self {
        Self {
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [0, 0, 0],
            processors: Vec::new(),
            waterlogging: true,
        }
    }
}

/// One block of a template: its position relative to the template origin, its
/// index into whichever palette was selected, and its `nbt` compound if it has
/// one.
#[derive(Debug, Clone)]
pub struct TemplateBlock {
    pos: [i32; 3],
    state: u16,
    /// The block's own `nbt` compound — a jigsaw block's whole configuration, a
    /// data marker's `metadata`, or a container's loot fields. `None` for the
    /// overwhelming majority of blocks, and behind an `Arc` so cloning a block
    /// info is a refcount bump.
    pub nbt: Option<Arc<BlockNbt>>,
    /// Parsed once from jigsaw NBT so placement never resolves state text.
    jigsaw_final_state: Option<CanonicalStateId>,
}

/// One block of a template, resolved: absolute world position, rotated state and
/// retained NBT after resolving the selected palette and transform.
#[derive(Debug, Clone)]
pub struct TemplateBlockInfo {
    /// The world position, i.e. `calculateRelativePosition(...).offset(position)`.
    pub pos: [i32; 3],
    /// The **rotated** state (`blockInfo.state.rotate(rotation)`).
    pub state: CanonicalStateId,
    /// The template-local, unrotated position used by processors that apply a
    /// local displacement.
    pub local: [i32; 3],
    /// The block's `nbt`.
    pub nbt: Option<Arc<BlockNbt>>,
}

/// A parsed `.nbt` structure template.
#[derive(Debug, Clone)]
pub struct StructureTemplate {
    size: [i32; 3],
    /// One entry for a single-`palette` template, N for a `palettes` list. Every
    /// palette has the same length and the block list indexes all of them.
    palettes: Vec<Vec<CanonicalStateId>>,
    blocks: Vec<TemplateBlock>,
    /// Block-list order for the fixed jigsaw state, once per palette.
    jigsaw_indices: Vec<Vec<usize>>,
    jigsaw_blocks: Vec<[OnceLock<Vec<JigsawBlockInfo>>; 4]>,
}

impl StructureTemplate {
    fn new(size: [i32; 3], palettes: Vec<Vec<CanonicalStateId>>, blocks: Vec<TemplateBlock>) -> Self {
        let jigsaw_indices = palettes
            .iter()
            .map(|palette| {
                blocks
                    .iter()
                    .enumerate()
                    .filter_map(|(index, block)| {
                        (palette
                            .get(block.state as usize)
                            .is_some_and(|state| state.block() == Block::Jigsaw))
                        .then_some(index)
                    })
                    .collect()
            })
            .collect();
        let jigsaw_blocks = (0..palettes.len())
            .map(|_| std::array::from_fn(|_| OnceLock::new()))
            .collect();
        Self {
            size,
            palettes,
            blocks,
            jigsaw_indices,
            jigsaw_blocks,
        }
    }

    /// Decodes a template from gzip-wrapped or bare NBT bytes.
    ///
    /// # Errors
    ///
    /// Returns a message naming the failure when the gzip or NBT layer does not
    /// decode, or when the root compound has no `size`/`blocks`.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let decoded = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(|e| format!("gunzip: {e}"))?;
            out
        } else {
            bytes.to_vec()
        };
        let mut reader = Reader::new(&decoded);
        let (_, root) = lodestone_core::read_named_nbt(&mut reader).map_err(|e| format!("nbt: {e}"))?;
        let root = compound(&root).ok_or("template root is not a compound")?;

        let size = field(root, "size")
            .and_then(int_triple)
            .ok_or("template has no `size`")?;

        let mut palettes = Vec::new();
        if let Some(Nbt::List { elements, .. }) = field(root, "palettes") {
            for palette in elements {
                palettes.push(parse_palette(palette));
            }
        } else if let Some(palette) = field(root, "palette") {
            palettes.push(parse_palette(palette));
        }
        if palettes.is_empty() || palettes.iter().all(Vec::is_empty) {
            return Err("template has no palette".into());
        }

        let mut blocks = Vec::new();
        if let Some(Nbt::List { elements, .. }) = field(root, "blocks") {
            for entry in elements {
                let Some(entry) = compound(entry) else { continue };
                let Some(pos) = field(entry, "pos").and_then(int_triple) else {
                    continue;
                };
                let state = match field(entry, "state") {
                    Some(Nbt::Int(i)) => u16::try_from(*i).unwrap_or(0),
                    _ => 0,
                };
                // Jigsaw configuration is carried only by this block-local NBT.
                let nbt = match field(entry, "nbt") {
                    Some(Nbt::Compound(fields)) => Some(Arc::new(fields.clone())),
                    _ => None,
                };
                let jigsaw_final_state = nbt
                    .as_deref()
                    .and_then(|nbt| nbt_string(nbt, "final_state"))
                    .map(|spec| {
                        parse_bound_state(spec)
                            .ok_or_else(|| format!("unsupported jigsaw final state: {spec}"))
                    })
                    .transpose()?;
                blocks.push(TemplateBlock {
                    pos,
                    state,
                    nbt,
                    jigsaw_final_state,
                });
            }
        }
        if blocks.is_empty() {
            return Err("template has no `blocks`".into());
        }

        Ok(Self::new(size, palettes, blocks))
    }

    /// Returns an empty template for a missing or intentionally absent element.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(
            [0, 0, 0],
            // Keep one empty palette so placement paths need no special case.
            vec![Vec::new()],
            Vec::new(),
        )
    }

    /// Builds a template directly from a size, a single palette, and a list
    /// of `(position, palette index)` pairs — no NBT decode in the loop.
    ///
    /// For a plugin constructing a structure programmatically rather than
    /// shipping a `.nbt` file (the other origin for a template,
    /// alongside [`Self::parse`]), and for tests that want a template with a
    /// known, hand-written shape. Every block gets no attached `nbt`
    /// compound — a plugin that needs a data marker (a jigsaw block, a
    /// chest's loot table reference) should build one from `.nbt` bytes via
    /// [`Self::parse`] instead, since [`BlockNbt`] has no public constructor
    /// of its own.
    #[must_use]
    pub fn from_blocks(size: [i32; 3], palette: Vec<BlockState>, blocks: Vec<([i32; 3], u16)>) -> Self {
        Self::new(
            size,
            vec![palette.into_iter().map(|state| state.id).collect()],
            blocks
                .into_iter()
                .map(|(pos, state)| TemplateBlock {
                    pos,
                    state,
                    nbt: None,
                    jigsaw_final_state: None,
                })
                .collect(),
        )
    }

    /// Returns the template dimensions.
    #[must_use]
    pub fn size(&self) -> [i32; 3] {
        self.size
    }

    /// Returns the transformed bounding box at `position`.
    /// box, in world space.
    #[must_use]
    pub fn bounding_box(&self, position: [i32; 3], settings: &PlaceSettings) -> BoundingBox {
        let delta = [self.size[0] - 1, self.size[1] - 1, self.size[2] - 1];
        let a = transform([0, 0, 0], settings.mirror, settings.rotation, settings.pivot);
        let b = transform(delta, settings.mirror, settings.rotation, settings.pivot);
        BoundingBox {
            min: [
                a[0].min(b[0]) + position[0],
                a[1].min(b[1]) + position[1],
                a[2].min(b[2]) + position[2],
            ],
            max: [
                a[0].max(b[0]) + position[0],
                a[1].max(b[1]) + position[1],
                a[2].max(b[2]) + position[2],
            ],
        }
    }

    /// Returns the palette index for a placement at `position`, using a
    /// position-seeded random source.
    #[must_use]
    pub fn palette_for(&self, position: [i32; 3]) -> usize {
        if self.palettes.len() == 1 {
            return 0;
        }
        let mut random = LegacyRandomSource::new(get_seed(position[0], position[1], position[2]));
        let count = i32::try_from(self.palettes.len()).unwrap_or(1);
        random.next_int_bounded(count).max(0) as usize
    }

    /// Returns every block whose id is `name`, at its transformed world position
    /// with its state already rotated.
    ///
    /// Jigsaw assembly uses this traversal at arbitrary positions and rotations,
    /// while placement traverses the full block list. Palette choice depends on
    /// `position`, so the same element can legitimately use different palettes
    /// at different positions.
    #[must_use]
    pub fn filter_blocks(
        &self,
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
    ) -> Vec<TemplateBlockInfo> {
        let palette_index = self.palette_for(position).min(self.palettes.len() - 1);
        let palette = &self.palettes[palette_index];
        if name == JIGSAW_BLOCK_NAME {
            return self.filter_block_indices(
                palette,
                name,
                position,
                rotation,
                self.jigsaw_indices[palette_index].iter().copied(),
            );
        }
        self.filter_block_indices(palette, name, position, rotation, 0..self.blocks.len())
    }

    #[must_use]
    pub fn jigsaw_blocks(
        &self,
        position: [i32; 3],
        rotation: Rotation,
    ) -> Vec<JigsawBlockInfo> {
        let palette_index = self.palette_for(position).min(self.palettes.len() - 1);
        let blocks = self.jigsaw_blocks[palette_index][rotation.turns() as usize].get_or_init(|| {
            self.filter_block_indices(
                &self.palettes[palette_index],
                JIGSAW_BLOCK_NAME,
                [0, 0, 0],
                rotation,
                self.jigsaw_indices[palette_index].iter().copied(),
            )
            .into_iter()
            .map(JigsawBlockInfo::of)
            .collect()
        });
        blocks
            .iter()
            .cloned()
            .map(|mut block| {
                block.pos[0] += position[0];
                block.pos[1] += position[1];
                block.pos[2] += position[2];
                block
            })
            .collect()
    }

    fn filter_block_indices(
        &self,
        palette: &[CanonicalStateId],
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
        indices: impl IntoIterator<Item = usize>,
    ) -> Vec<TemplateBlockInfo> {
        let mut out = Vec::new();
        let Some(wanted) = Block::from_name(name) else {
            return out;
        };
        for index in indices {
            let block = &self.blocks[index];
            let Some(state) = palette.get(block.state as usize) else {
                continue;
            };
            if state.block() != wanted {
                continue;
            }
            let rel = transform(block.pos, Mirror::None, rotation, [0, 0, 0]);
            out.push(TemplateBlockInfo {
                pos: [
                    rel[0] + position[0],
                    rel[1] + position[1],
                    rel[2] + position[2],
                ],
                state: rotate_state(*state, rotation),
                local: block.pos,
                nbt: block.nbt.clone(),
            });
        }
        out
    }

    /// Places this template at `position` into `grid`, clipped to the grid's own
    /// box (a write outside it is a no-op, which is how per-chunk clipping
    /// happens — see the module doc).
    ///
    /// # Two passes, because processors can read the world
    ///
    /// The processor chain runs over the **whole** block list before any block is
    /// written, so a location predicate
    /// `location_predicate` (village streets test for water under a `dirt_path`, to
    /// build a bridge) sees the pre-structure world — never an earlier block of
    /// on the same template always observes the pre-structure world.
    ///
    /// Returns the number of blocks actually written inside the grid.
    pub fn place(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
    ) -> usize {
        self.place_impl(origin, settings, grid, &mut |_, _| {}, None)
    }

    pub fn place_with_mutations(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
        mutation: &mut StructureMutationContext<'_>,
    ) -> usize {
        self.place_impl(origin, settings, grid, &mut |_, _| {}, Some(mutation))
    }

    /// Place a template and report every state-owned block-entity creation
    /// event in write order. The callback observes the processed state before a
    /// later structure write can overwrite it, which is the history needed by
    /// packet-facing generation sidecars.
    pub fn place_with_block_entity_events(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
        mut on_block_entity: impl FnMut(
            [i32; 3],
            lodestone_data::block_entity_types::BlockEntityType,
        ),
    ) -> usize {
        self.place_impl(origin, settings, grid, &mut on_block_entity, None)
    }

    pub fn place_with_block_entity_events_and_mutations(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
        mut on_block_entity: impl FnMut(
            [i32; 3],
            lodestone_data::block_entity_types::BlockEntityType,
        ),
        mutation: &mut StructureMutationContext<'_>,
    ) -> usize {
        self.place_impl(origin, settings, grid, &mut on_block_entity, Some(mutation))
    }

    fn place_impl(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
        mut on_block_entity: impl FnMut(
            [i32; 3],
            lodestone_data::block_entity_types::BlockEntityType,
        ),
        mut mutation: Option<&mut StructureMutationContext<'_>>,
    ) -> usize {
        let position = origin.position;
        let palette = &self.palettes[self.palette_for(position).min(self.palettes.len() - 1)];
        let (min_x, min_y, min_z, size_x, size_y, size_z) = grid.bounds();
        let inside = |p: [i32; 3]| {
            p[0] >= min_x
                && p[0] < min_x + size_x
                && p[1] >= min_y
                && p[1] < min_y + size_y
                && p[2] >= min_z
                && p[2] < min_z + size_z
        };
        // `processOnlyInCurrentChunk`: false as soon as **any** processor
        // `evaluatesEntirePieceState()`. Only `capped` does, and for it the whole
        // piece must be processed even though only this chunk's share is written:
        // its shuffled walk indexes the processed list, so a list clipped to the
        // chunk would give the piece a different number of suspicious blocks on
        // each side of a border.
        let whole_piece = settings
            .processors
            .iter()
            .any(Processor::evaluates_entire_piece_state);
        let mut processed: Vec<ProcessedBlock> = Vec::new();
        // The `originalBlockInfoList` half — template-local position and `nbt` per
        // *surviving* block, kept index-parallel with `processed` because that is
        // exactly the invariant `CappedProcessor` checks before doing anything.
        let mut originals: Vec<(
            [i32; 3],
            Option<Arc<BlockNbt>>,
            Option<CanonicalStateId>,
        )> = Vec::new();
        for block in &self.blocks {
            let rel = transform(block.pos, settings.mirror, settings.rotation, settings.pivot);
            let world = [rel[0] + position[0], rel[1] + position[1], rel[2] + position[2]];
            // The grid clips writes, but a processor chain is not free — skip the
            // whole block when it cannot land here anyway, unless a whole-piece
            // processor forbids it.
            if !whole_piece && !inside(world) {
                continue;
            }
            let Some(state) = palette.get(block.state as usize) else {
                continue;
            };
            // Processors see the *unrotated* state at the absolute position.
            // A processor returning `None` drops the block, which
            // is how air, rot and jigsaw replacement work.
            let mut current = Some(ProcessedBlock {
                pos: world,
                state: *state,
            });
            for processor in &settings.processors {
                let Some(block_now) = current.take() else { break };
                let ctx = ProcessCtx {
                    local: block.pos,
                    reference: origin.reference,
                    nbt: block.nbt.as_deref(),
                    jigsaw_final_state: block.jigsaw_final_state,
                    world: grid,
                };
                current = processor.process(&ctx, block_now);
            }
            if let Some(block_now) = current {
                processed.push(block_now);
                originals.push((block.pos, block.nbt.clone(), block.jigsaw_final_state));
            }
        }
        // Finalize each processor in chain order.
        for processor in &settings.processors {
            processor.finalize(
                position,
                origin.reference,
                origin.seed,
                &originals,
                &mut processed,
                grid,
            );
        }
        let mut written = 0;
        let mut written_states = Vec::new();
        for block in processed {
            // A processor may move a block, so re-test the clip.
            if !inside(block.pos) {
                continue;
            }
            let mut final_state = state_with_transform(block.state, settings.mirror, settings.rotation);
            if settings.waterlogging
                && property_name(&Properties::from_state_id(final_state), "waterlogged") == Some("false")
                && grid.get_id(block.pos[0], block.pos[1], block.pos[2]).block() == Block::Water
            {
                final_state = with_waterlogged(final_state, true);
            }
            if let Some(type_id) = lodestone_data::block_entity_types::block_entity_type(final_state) {
                on_block_entity(block.pos, type_id);
            }
            if let Some(mutation) = mutation.as_deref_mut() {
                mutation.write(grid, block.pos[0], block.pos[1], block.pos[2], final_state);
            } else {
                grid.set_id(block.pos[0], block.pos[1], block.pos[2], final_state);
            }
            written_states.push((block.pos, final_state));
            written += 1;
        }
        // The structure writer updates shapes after the piece's writes have
        // been clipped to the current chunk. Re-evaluate until stable so a
        // ladder chain cannot keep a higher rung alive after its lower rung
        // loses the support that was outside this grid.
        loop {
            let mut changed = false;
            for &(position, written_state) in &written_states {
                if grid.get_id(position[0], position[1], position[2]) != written_state {
                    continue;
                }
                let Some(support) = attached_support_position(position, written_state) else {
                    continue;
                };
                if matches!(
                    grid.get_id(support[0], support[1], support[2]).block(),
                    Block::Air
                        | Block::CaveAir
                        | Block::VoidAir
                        | Block::Water
                        | Block::Lava
                        | Block::BubbleColumn
                )
                {
                    if let Some(mutation) = mutation.as_deref_mut() {
                        mutation.write(
                            grid,
                            position[0],
                            position[1],
                            position[2],
                            CanonicalStateId::AIR,
                        );
                    } else {
                        grid.set_id(position[0], position[1], position[2], CanonicalStateId::AIR);
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        written
    }
}

fn compound(value: &Nbt) -> Option<&Vec<(String, Nbt)>> {
    match value {
        Nbt::Compound(fields) => Some(fields),
        _ => None,
    }
}

fn field<'a>(fields: &'a [(String, Nbt)], name: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(key, _)| key == name).map(|(_, value)| value)
}

fn int_triple(value: &Nbt) -> Option<[i32; 3]> {
    match value {
        Nbt::List { elements, .. } if elements.len() >= 3 => {
            let mut out = [0i32; 3];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = match elements[i] {
                    Nbt::Int(v) => v,
                    Nbt::Short(v) => i32::from(v),
                    Nbt::Byte(v) => i32::from(v),
                    _ => return None,
                };
            }
            Some(out)
        }
        Nbt::IntArray(values) if values.len() >= 3 => Some([values[0], values[1], values[2]]),
        _ => None,
    }
}

fn parse_palette(value: &Nbt) -> Vec<CanonicalStateId> {
    let Nbt::List { elements, .. } = value else {
        return Vec::new();
    };
    elements
        .iter()
        .map(|entry| {
            let Some(entry) = compound(entry) else {
                return CanonicalStateId::AIR;
            };
            let name = match field(entry, "Name") {
                Some(Nbt::String(name)) => name.clone(),
                _ => "minecraft:air".to_string(),
            };
            let mut properties = BTreeMap::new();
            if let Some(Nbt::Compound(fields)) = field(entry, "Properties") {
                for (key, value) in fields {
                    if let Nbt::String(value) = value {
                        properties.insert(key.clone(), value.clone());
                    }
                }
            }
            let pairs = properties
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>();
            bind_state(&name, &pairs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_matches_corner_arithmetic() {
        // A 4x1x2 template's far corner (3, 0, 1) about pivot ZERO.
        assert_eq!(transform([3, 0, 1], Mirror::None, Rotation::None, [0, 0, 0]), [3, 0, 1]);
        assert_eq!(transform([3, 0, 1], Mirror::None, Rotation::Cw90, [0, 0, 0]), [-1, 0, 3]);
        assert_eq!(transform([3, 0, 1], Mirror::None, Rotation::Cw180, [0, 0, 0]), [-3, 0, -1]);
        assert_eq!(transform([3, 0, 1], Mirror::None, Rotation::Ccw90, [0, 0, 0]), [1, 0, -3]);
    }

    #[test]
    fn rotation_walks_facing_and_swaps_axis() {
        let stair = BlockState::parse("minecraft:oak_stairs[facing=north,half=bottom]");
        assert_eq!(
            stair.rotate(Rotation::Cw90).id.canonical_state(),
            "minecraft:oak_stairs[facing=east,half=bottom]"
        );
        let log = BlockState::parse("minecraft:oak_log[axis=x]");
        assert_eq!(log.rotate(Rotation::Cw90).id.canonical_state(), "minecraft:oak_log[axis=z]");
        assert_eq!(log.rotate(Rotation::Cw180).id.canonical_state(), "minecraft:oak_log[axis=x]");
        let sign = BlockState::parse("minecraft:oak_sign[rotation=2]");
        assert_eq!(sign.rotate(Rotation::Cw90).id.canonical_state(), "minecraft:oak_sign[rotation=6]");
    }

    #[test]
    fn directional_flags_permute() {
        let fence = BlockState::parse("minecraft:oak_fence[east=true,north=false,south=false,west=false]");
        assert_eq!(
            fence.rotate(Rotation::Cw90).id.canonical_state(),
            "minecraft:oak_fence[east=false,north=false,south=true,west=false]"
        );
    }

    #[test]
    fn consuming_transform_matches_borrowed_transform() {
        let states = [
            BlockState::parse("minecraft:oak_stairs[facing=north,half=bottom,shape=straight]"),
            BlockState::parse("minecraft:oak_fence[east=true,north=false,south=false,west=false]"),
            BlockState::parse("minecraft:oak_sign[rotation=2]"),
            BlockState::parse("minecraft:oak_log[axis=x]"),
        ];
        for state in states {
            for mirror in [Mirror::None, Mirror::LeftRight, Mirror::FrontBack] {
                for rotation in [Rotation::None, Rotation::Cw90, Rotation::Cw180, Rotation::Ccw90] {
                    let expected = state.mirror(mirror).rotate(rotation).id;
                    let actual = state.clone().into_mirror(mirror).into_rotate(rotation).id;
                    assert_eq!(actual, expected, "mirror={mirror:?} rotation={rotation:?}");
                }
            }
        }
    }

    #[test]
    fn bound_state_is_canonical_regardless_of_insertion_order() {
        let state = BlockState::parse("minecraft:oak_trapdoor[open=false,facing=north,half=top]");
        assert_eq!(
            state.id.canonical_state(),
            "minecraft:oak_trapdoor[facing=north,half=top,open=false]"
        );
    }

    #[test]
    fn clipped_attachment_drops_without_support_but_keeps_supported_control() {
        let ladder = BlockState::parse("minecraft:ladder[facing=north,waterlogged=false]");
        let stone = BlockState::parse("minecraft:stone");
        let template = StructureTemplate::from_blocks(
            [16, 16, 16],
            vec![ladder, stone],
            vec![([15, 1, 15], 0), ([1, 1, 1], 0), ([1, 1, 2], 1)],
        );
        let mut grid = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        template.place(
            PlaceOrigin {
                position: [0, 0, 0],
                reference: [0, 0, 0],
                seed: 42,
            },
            &PlaceSettings::default(),
            &mut grid,
        );
        assert_eq!(grid.get(15, 1, 15), "minecraft:air");
        assert_eq!(grid.get(1, 1, 1), "minecraft:ladder[facing=north,waterlogged=false]");
    }

    fn linear_filter_blocks(
        template: &StructureTemplate,
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
    ) -> Vec<TemplateBlockInfo> {
        let palette_index = template.palette_for(position).min(template.palettes.len() - 1);
        let palette = &template.palettes[palette_index];
        let mut out = Vec::new();
        let Some(wanted) = Block::from_name(name) else {
            return out;
        };
        for block in &template.blocks {
            let Some(&state) = palette.get(block.state as usize) else {
                continue;
            };
            if state.block() != wanted {
                continue;
            }
            let rel = transform(block.pos, Mirror::None, rotation, [0, 0, 0]);
            out.push(TemplateBlockInfo {
                pos: [
                    rel[0] + position[0],
                    rel[1] + position[1],
                    rel[2] + position[2],
                ],
                state: rotate_state(state, rotation),
                local: block.pos,
                nbt: block.nbt.clone(),
            });
        }
        out
    }

    fn assert_filter_matches_linear(
        template: &StructureTemplate,
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
    ) {
        let indexed = template.filter_blocks(name, position, rotation);
        let linear = linear_filter_blocks(template, name, position, rotation);
        assert_eq!(
            indexed.len(),
            linear.len(),
            "name={name} position={position:?} rotation={rotation:?}"
        );
        for (actual, expected) in indexed.iter().zip(linear.iter()) {
            assert_eq!(actual.pos, expected.pos);
            assert_eq!(actual.state, expected.state);
            assert_eq!(actual.local, expected.local);
            assert_eq!(actual.nbt, expected.nbt);
        }
    }

    #[test]
    fn indexed_jigsaw_filter_matches_linear_scan() {
        let nbt_a = Arc::new(vec![
            ("name".to_string(), Nbt::String("test:a".to_string())),
            ("selection_priority".to_string(), Nbt::Int(3)),
        ]);
        let nbt_b = Arc::new(vec![
            ("name".to_string(), Nbt::String("test:b".to_string())),
            ("placement_priority".to_string(), Nbt::Int(7)),
        ]);
        let template = StructureTemplate::new(
            [4, 1, 1],
            vec![
                vec![
                    BlockState::parse("minecraft:jigsaw[orientation=north_up]").id,
                    Block::Stone.default_state(),
                ],
                vec![
                    BlockState::parse("minecraft:jigsaw[orientation=south_up]").id,
                    Block::Dirt.default_state(),
                ],
            ],
            vec![
                TemplateBlock {
                    pos: [0, 0, 0],
                    state: 0,
                    nbt: Some(Arc::clone(&nbt_a)),
                    jigsaw_final_state: None,
                },
                TemplateBlock {
                    pos: [1, 0, 0],
                    state: 1,
                    nbt: None,
                    jigsaw_final_state: None,
                },
                TemplateBlock {
                    pos: [2, 0, 0],
                    state: 99,
                    nbt: Some(Arc::clone(&nbt_b)),
                    jigsaw_final_state: None,
                },
                TemplateBlock {
                    pos: [3, 0, 0],
                    state: 0,
                    nbt: Some(Arc::clone(&nbt_b)),
                    jigsaw_final_state: None,
                },
            ],
        );
        assert_eq!(template.jigsaw_indices, vec![vec![0, 3], vec![0, 3]]);

        let mut palette_positions = [None, None];
        'search: for x in -32..=32 {
            for y in -2..=2 {
                for z in -32..=32 {
                    let position = [x, y, z];
                    let palette = template.palette_for(position);
                    palette_positions[palette] = Some(position);
                    if palette_positions.iter().all(Option::is_some) {
                        break 'search;
                    }
                }
            }
        }
        let positions = [
            palette_positions[0].expect("palette 0 position"),
            palette_positions[1].expect("palette 1 position"),
            [-123, 45, 77],
        ];
        let names = [
            JIGSAW_BLOCK_NAME,
            "minecraft:stone",
            "minecraft:dirt",
            "minecraft:missing",
        ];
        for position in positions {
            for rotation in [
                Rotation::None,
                Rotation::Cw90,
                Rotation::Cw180,
                Rotation::Ccw90,
            ] {
                for name in names {
                    assert_filter_matches_linear(&template, name, position, rotation);
                }
            }
        }
    }

    #[test]
    fn constructors_populate_jigsaw_indices() {
        let template = StructureTemplate::from_blocks(
            [2, 1, 1],
            vec![BlockState::from_block(Block::Jigsaw)],
            vec![([0, 0, 0], 0), ([1, 0, 0], 1)],
        );
        assert_eq!(template.jigsaw_indices, vec![vec![0]]);
        assert_eq!(
            StructureTemplate::empty().jigsaw_indices,
            vec![Vec::<usize>::new()]
        );
    }
}
