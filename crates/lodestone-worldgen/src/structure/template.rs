//! NBT **structure templates** — the reader, the rotation/mirror transform, and
//! the write into a chunk's block grid.
//!
//! # What it is
//!
//! A port of vanilla's own structure-template type: the `.nbt` files under
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
//!             state = state.mirror(m).rotate(r)              <- vanilla's order
//!             grid.set(world, state)                         <- clipped by the grid
//! ```
//!
//! Two properties make this work chunk-at-a-time with no cross-chunk state, which
//! is what our per-chunk memoised pipeline needs and vanilla's shared mutable
//! `StructureStart` does not have:
//!
//! * **Every random draw here is position-seeded.** Palette choice is
//!   a fresh default random source seeded from a position-derived hash of
//!   `templatePosition` and
//!   vanilla's own block-rot processor's keep/drop roll is the same
//!   position-derived hash of `blockPos` — so two
//!   chunks placing two halves of the same piece agree without communicating.
//! * **A write outside the grid's box is a no-op** ([`DenseBlockGrid::set`]), so
//!   "clip this piece to the chunk" needs no explicit box: the grid *is* the box.
//!   This is vanilla's own place-settings bounding-box assignment by construction.
//!
//! # How to change it
//!
//! * **Rotation is per-property, not per-block** ([`BlockState::rotate`]):
//!   `facing`, `axis`, `rotation` and the four directional booleans. That covers
//!   every property in the 71 templates this unit places. A block whose rotation
//!   needs *block-class* knowledge (a stair's `shape` under a **mirror**, a rail's
//!   `shape`) is **not** handled — the three structures wired here all use
//!   `Mirror.NONE`, where `shape` is invariant, and the gap is named in
//!   [`super::StructureRegistry::unsupported`] rather than left to be discovered.
//! * **Multi-palette templates are normal, not exotic.** Every shipwreck template
//!   carries 8 palettes (the wood species), and the block list is shared across
//!   them — `blocks[i].state` indexes whichever palette
//!   [`StructureTemplate::palette_for`] picked. A reader that assumed a single
//!   `palette` key would silently place nothing for every shipwreck.
//! * Entities and block entities are parsed but **not** placed. Loot chests and
//!   the `structure_block` data markers that create them need block entities plus
//!   loot tables, which no worldgen stage has yet; the markers themselves are
//!   dropped by the same block-ignore processor vanilla drops them with.
//! * **A block's `nbt` compound is retained** ([`TemplateBlock::nbt`]), which S2
//!   deliberately dropped. It is not decoration: a jigsaw block's *entire*
//!   configuration — `name`, `target`, `pool`, `final_state`, `joint`,
//!   `placement_priority`, `selection_priority` — lives nowhere else, so S4's
//!   assembly reads it through [`StructureTemplate::filter_blocks`]. Only a few
//!   blocks per template carry one, so it is an `Option<Arc<..>>` rather than a
//!   parallel dense array.
//!
//! # Dependencies
//!
//! `lodestone-core` for the NBT codec, `flate2` for the gzip wrapper vanilla
//! writes these files with, and [`crate::dense_grid`] for the write target.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::Arc;

use lodestone_core::{Nbt, Reader};
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

use super::BoundingBox;
use super::processor::{ProcessCtx, Processor, ProcessedBlock};
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
/// (vanilla writes `placement_priority`/`selection_priority` as `Int`, but
/// vanilla's own "get int or" reads a `Byte` too).
#[must_use]
pub fn nbt_int(nbt: &BlockNbt, key: &str) -> Option<i32> {
    nbt.iter().find(|(k, _)| k == key).and_then(|(_, v)| match *v {
        Nbt::Int(i) => Some(i),
        Nbt::Short(s) => Some(i32::from(s)),
        Nbt::Byte(b) => Some(i32::from(b)),
        _ => None,
    })
}

/// One of vanilla's four rotations, in vanilla's own declaration order — the order
/// vanilla's own random-rotation pick indexes with a single `nextInt(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// `NONE`.
    #[default]
    None,
    /// `CLOCKWISE_90`.
    Cw90,
    /// `CLOCKWISE_180`.
    Cw180,
    /// `COUNTERCLOCKWISE_90`.
    Ccw90,
}

impl Rotation {
    /// Vanilla's own random-rotation pick at `(random)` — its own pick-a-random-list-element
    /// helper over all four values, one
    /// `nextInt(4)`.
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

/// Vanilla's `Mirror`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mirror {
    /// `NONE`.
    #[default]
    None,
    /// `LEFT_RIGHT` — negates `z`, so north and south swap.
    LeftRight,
    /// `FRONT_BACK` — negates `x`, so east and west swap.
    FrontBack,
}

/// A block state as a name plus its property map — the form template palettes
/// are written in.
///
/// The map is a `BTreeMap` because [`Self::canonical`] must emit properties in
/// alphabetical order: that is the spelling the rest of this engine's block field
/// holds (`lodestone_worldgen::feature::canon_state`,
/// `lodestone_server::worldgen_data::canonical_state`), and a differently-ordered
/// string is a *different palette entry* to [`crate::interner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockState {
    /// The block id, e.g. `minecraft:oak_stairs`.
    pub name: String,
    /// `key -> value`, alphabetical.
    pub properties: BTreeMap<String, String>,
}

impl BlockState {
    /// A state with no properties.
    #[must_use]
    pub fn of(name: &str) -> Self {
        Self {
            name: name.to_string(),
            properties: BTreeMap::new(),
        }
    }

    /// Parses `minecraft:oak_stairs[facing=north,half=bottom]`.
    #[must_use]
    pub fn parse(spec: &str) -> Self {
        let Some((name, rest)) = spec.split_once('[') else {
            return Self::of(spec.trim());
        };
        let mut properties = BTreeMap::new();
        for entry in rest.trim_end_matches(']').split(',') {
            if let Some((key, value)) = entry.split_once('=') {
                properties.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        Self {
            name: name.trim().to_string(),
            properties,
        }
    }

    /// The canonical `name[k=v,…]` string, properties alphabetical.
    #[must_use]
    pub fn canonical(&self) -> String {
        if self.properties.is_empty() {
            return self.name.clone();
        }
        let mut out = self.name.clone();
        out.push('[');
        for (i, (key, value)) in self.properties.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "{key}={value}");
        }
        out.push(']');
        out
    }

    /// Vanilla's own block-state rotate at `(rotation)`, per property (see the module doc for the
    /// deliberate limits).
    #[must_use]
    pub fn rotate(&self, rotation: Rotation) -> Self {
        if rotation == Rotation::None {
            return self.clone();
        }
        let mut out = self.clone();
        let turns = rotation.turns();
        if let Some(facing) = out.properties.get("facing") {
            if let Some(rotated) = rotate_direction(facing, turns) {
                out.properties.insert("facing".into(), rotated.into());
            }
        }
        if let Some(axis) = out.properties.get("axis") {
            if turns % 2 == 1 {
                let swapped = match axis.as_str() {
                    "x" => Some("z"),
                    "z" => Some("x"),
                    _ => None,
                };
                if let Some(swapped) = swapped {
                    out.properties.insert("axis".into(), swapped.into());
                }
            }
        }
        if let Some(value) = out.properties.get("rotation").and_then(|r| r.parse::<u32>().ok()) {
            // Vanilla's own rotation-index rotate over a span of 16: +4 per clockwise quarter turn.
            let rotated = (value + 4 * turns) % 16;
            out.properties.insert("rotation".into(), rotated.to_string());
        }
        if let Some(orientation) = out.properties.get("orientation") {
            if let Some(rotated) = rotate_orientation(orientation, turns) {
                out.properties.insert("orientation".into(), rotated);
            }
        }
        if let Some(shape) = out.properties.get("shape") {
            if let Some(rotated) = rotate_rail_shape(shape, turns) {
                out.properties.insert("shape".into(), rotated.into());
            }
        }
        rotate_directional_flags(&mut out, turns);
        out
    }

    /// Vanilla's own jigsaw-block front-facing / top-facing accessors — the two halves of the
    /// `orientation` property (`FrontAndTop`, serialised `"<front>_<top>"`).
    ///
    /// Returns `None` for a state with no `orientation`, which is every block
    /// except a jigsaw.
    #[must_use]
    pub fn front_and_top(&self) -> Option<(&str, &str)> {
        self.properties.get("orientation")?.split_once('_')
    }

    /// Vanilla's own block-state mirror at `(mirror)` for the same property set.
    #[must_use]
    pub fn mirror(&self, mirror: Mirror) -> Self {
        if mirror == Mirror::None {
            return self.clone();
        }
        let mut out = self.clone();
        let flip = |dir: &str| -> Option<&'static str> {
            match (mirror, dir) {
                (Mirror::LeftRight, "north") => Some("south"),
                (Mirror::LeftRight, "south") => Some("north"),
                (Mirror::FrontBack, "east") => Some("west"),
                (Mirror::FrontBack, "west") => Some("east"),
                _ => None,
            }
        };
        // Vanilla's own stair-block mirror — the one place a mirror needs *block-class*
        // knowledge, and now reachable: a coded piece with a SOUTH or WEST
        // orientation mirrors LEFT_RIGHT, and both the swamp hut and the pyramids
        // place stairs with an explicit `shape`. Vanilla applies the shape swap
        // **only** when the mirror actually moves the facing (LEFT_RIGHT with a Z
        // facing, FRONT_BACK with an X facing), and the two cases are *not*
        // symmetric: LEFT_RIGHT swaps all four inner/outer variants, FRONT_BACK
        // swaps only the outer pair.
        let is_stair = out.properties.contains_key("shape")
            && out.properties.contains_key("half")
            && out.properties.contains_key("facing");
        if is_stair {
            let facing = out.properties.get("facing").cloned().unwrap_or_default();
            let z_axis = facing == "north" || facing == "south";
            let applies = match mirror {
                Mirror::LeftRight => z_axis,
                Mirror::FrontBack => !z_axis,
                Mirror::None => false,
            };
            if applies {
                if let Some(shape) = out.properties.get("shape").cloned() {
                    let swapped = match (mirror, shape.as_str()) {
                        (Mirror::LeftRight, "outer_left") | (Mirror::FrontBack, "outer_left") => {
                            Some("outer_right")
                        }
                        (Mirror::LeftRight, "outer_right") | (Mirror::FrontBack, "outer_right") => {
                            Some("outer_left")
                        }
                        (Mirror::LeftRight, "inner_left") => Some("inner_right"),
                        (Mirror::LeftRight, "inner_right") => Some("inner_left"),
                        // FRONT_BACK leaves the inner pair alone — transcribed,
                        // not tidied.
                        _ => None,
                    };
                    if let Some(swapped) = swapped {
                        out.properties.insert("shape".into(), swapped.into());
                    }
                }
            }
        }
        if let Some(shape) = out.properties.get("shape") {
            if let Some(flipped) = mirror_rail_shape(shape, mirror) {
                out.properties.insert("shape".into(), flipped.into());
            }
        }
        if let Some(facing) = out.properties.get("facing") {
            if let Some(flipped) = flip(facing) {
                out.properties.insert("facing".into(), flipped.into());
            }
        }
        let flags: Vec<(String, String)> = ["north", "east", "south", "west"]
            .into_iter()
            .filter_map(|dir| {
                let value = out.properties.get(dir)?;
                let target = flip(dir).unwrap_or(dir);
                Some((target.to_string(), value.clone()))
            })
            .collect();
        for (dir, value) in flags {
            out.properties.insert(dir, value);
        }
        out
    }

    /// True when this state carries `waterlogged=false`, i.e. the block *has* the
    /// property and is currently dry.
    #[must_use]
    pub fn is_waterloggable_and_dry(&self) -> bool {
        self.properties.get("waterlogged").map(String::as_str) == Some("false")
    }
}

/// Vanilla's own base-rail-block rotate at `(RailShape, Rotation)`, transcribed.
///
/// Keyed on the `shape` **value** rather than on the presence of neighbouring
/// properties, because `shape` is spelled by two unrelated block families here: a
/// stair's is one of `straight`/`inner_*`/`outer_*` and a rail's is one of the ten
/// below, and the two sets are disjoint. Returning `None` for anything unrecognised
/// is therefore what leaves the stair branch — and every other `shape` — alone.
///
/// Written out rather than derived from "rotate the two connected directions and
/// re-canonicalise": the derivation needs a canonical-name pass of its own, and a
/// table lifted from the source cannot disagree with it.
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

/// Vanilla's own base-rail-block mirror at `(RailShape, Mirror)`, transcribed.
///
/// Note the asymmetry, which is vanilla's: `LEFT_RIGHT` leaves `ascending_east` and
/// `ascending_west` alone and `FRONT_BACK` leaves `ascending_north`/`ascending_south`
/// alone, while both swap the four diagonals — the same shape of asymmetry the stair
/// branch above carries, and for the same reason (the mirror axis must actually move
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

/// Vanilla's own octahedral-group rotate at `(FrontAndTop)` for a Y-axis rotation:
/// `fromFrontAndTop(rotate(front), rotate(top))`.
///
/// A vertical component is invariant (vanilla's own rotation-of-direction returns a
/// Y-axis direction unchanged), which is what makes this a per-component rewrite
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

/// Vanilla's own direction "get opposite".
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

/// Vanilla's own per-axis direction step accessors — the unit offset of a named direction.
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

/// The four directional booleans of a fence/pane/vine, permuted by `turns`.
fn rotate_directional_flags(state: &mut BlockState, turns: u32) {
    const CW: [&str; 4] = ["north", "east", "south", "west"];
    let current: Vec<Option<String>> = CW.iter().map(|d| state.properties.get(*d).cloned()).collect();
    if current.iter().all(Option::is_none) {
        return;
    }
    for (i, dir) in CW.iter().enumerate() {
        // The value that was `turns` quarter-turns counter-clockwise of `dir`
        // becomes `dir`'s.
        let source = (i + 4 - turns as usize % 4) % 4;
        if let Some(value) = current[source].clone() {
            state.properties.insert((*dir).to_string(), value);
        }
    }
}

/// Vanilla's own structure-template transform at `(pos, mirror, rotation, pivot)`.
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

/// `placeInWorld`'s three non-`StructurePlaceSettings` arguments.
///
/// Grouped because they arrive from three different places and every one of them
/// is easy to get wrong on its own: `position` is the piece's, `reference` is the
/// whole *start*'s (see [`super::jigsaw::reference_position`]), and `seed` is the
/// **world** seed rather than any derived stream — [`Processor::Capped`] forks it
/// positionally, so a per-chunk or per-structure seed here would give one piece a
/// different set of suspicious blocks in each chunk it spans.
#[derive(Debug, Clone, Copy)]
pub struct PlaceOrigin {
    /// `templatePosition` — where template-local `(0,0,0)` lands.
    pub position: [i32; 3],
    /// `referencePos`.
    pub reference: [i32; 3],
    /// `level.getSeed()`.
    pub seed: i64,
}

/// How one piece places its template: vanilla's `StructurePlaceSettings` plus the
/// template position, held together because a piece needs all of it and nothing
/// else.
#[derive(Debug, Clone)]
pub struct PlaceSettings {
    /// `getRotation`.
    pub rotation: Rotation,
    /// `getMirror`.
    pub mirror: Mirror,
    /// `getRotationPivot`.
    pub pivot: [i32; 3],
    /// The processor chain, in `addProcessor` order.
    pub processors: Vec<Processor>,
    /// `shouldApplyWaterlogging` — false for `LiquidSettings.IGNORE_WATERLOGGING`.
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
    /// data marker's `metadata`, a chest's `LootTable`. `None` for the
    /// overwhelming majority of blocks, and behind an `Arc` so cloning a block
    /// info is a refcount bump.
    pub nbt: Option<Arc<BlockNbt>>,
}

/// One block of a template, resolved: absolute world position, rotated state and
/// retained NBT — vanilla's `StructureTemplate.StructureBlockInfo` after
/// `filterBlocks(position, settings, block, absolute = true)`.
#[derive(Debug, Clone)]
pub struct TemplateBlockInfo {
    /// The world position, i.e. `calculateRelativePosition(...).offset(position)`.
    pub pos: [i32; 3],
    /// The **rotated** state (`blockInfo.state.rotate(rotation)`).
    pub state: BlockState,
    /// The template-local, unrotated position — what a `GravityProcessor` reads
    /// its `delta` from.
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
    palettes: Vec<Vec<BlockState>>,
    blocks: Vec<TemplateBlock>,
}

impl StructureTemplate {
    /// Decodes a template from its file bytes, gzip-wrapped (as Mojang ships
    /// them) or bare NBT.
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
                // Retained, unlike S2 and unlike vanilla's own placement loop:
                // this is the only place a jigsaw block's pool/target/joint
                // configuration exists. See the module doc.
                let nbt = match field(entry, "nbt") {
                    Some(Nbt::Compound(fields)) => Some(Arc::new(fields.clone())),
                    _ => None,
                };
                blocks.push(TemplateBlock { pos, state, nbt });
            }
        }
        if blocks.is_empty() {
            return Err("template has no `blocks`".into());
        }

        Ok(Self {
            size,
            palettes,
            blocks,
        })
    }

    /// The template vanilla's own structure-template-manager get-or-create invents when a
    /// referenced `.nbt` does not exist: **zero size, no palette, no blocks.**
    ///
    /// This is not a defensive stub; it is vanilla's own behaviour for a dangling
    /// reference, and vanilla's own data has one —
    /// vanilla's own ancient-city structure-pool data names
    /// `ancient_city/walls/intact_horizontal_wall_stairs_5`, of which only `_1`
    /// through `_4` ship. Its own get-or-create call logs, caches an empty template, and the
    /// element stays in its pool with a degenerate box and no jigsaw blocks, so it
    /// is offered by the pool's shuffle (consuming the draws) and never attaches.
    /// Refusing the pool instead would delete the whole structure.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            size: [0, 0, 0],
            // One empty palette rather than none, so `place`/`filter_blocks` need
            // no extra guard: the block list is empty, so neither iterates.
            palettes: vec![Vec::new()],
            blocks: Vec::new(),
        }
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
        Self {
            size,
            palettes: vec![palette],
            blocks: blocks
                .into_iter()
                .map(|(pos, state)| TemplateBlock { pos, state, nbt: None })
                .collect(),
        }
    }

    /// Vanilla's own size accessor.
    #[must_use]
    pub fn size(&self) -> [i32; 3] {
        self.size
    }

    /// Vanilla's own bounding-box accessor at `(settings, position)` — the transformed corner-to-corner
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

    /// Vanilla's own place-settings random-palette accessor at `(palettes,
    /// position)` — the
    /// palette index for a placement at `position`, from a fresh
    /// default random source seeded from a position-derived hash.
    #[must_use]
    pub fn palette_for(&self, position: [i32; 3]) -> usize {
        if self.palettes.len() == 1 {
            return 0;
        }
        let mut random = LegacyRandomSource::new(get_seed(position[0], position[1], position[2]));
        let count = i32::try_from(self.palettes.len()).unwrap_or(1);
        random.next_int_bounded(count).max(0) as usize
    }

    /// `filterBlocks(position, new StructurePlaceSettings().setRotation(r), block,
    /// absolute = true)` — every block of this template whose *id* is `name`, at
    /// its world position with its state already rotated.
    ///
    /// The reader jigsaw assembly is built on: it needs the jigsaw blocks of an
    /// element at an arbitrary position and rotation, which is a different
    /// traversal from placing the whole template. Note the palette is chosen from
    /// `position`, exactly as vanilla's `getJigsaws` does — so the same element
    /// scanned at its real position and at `BlockPos.ZERO` can legitimately see
    /// two different palettes, and `JigsawPlacement` scans it both ways.
    #[must_use]
    pub fn filter_blocks(
        &self,
        name: &str,
        position: [i32; 3],
        rotation: Rotation,
    ) -> Vec<TemplateBlockInfo> {
        let palette = &self.palettes[self.palette_for(position).min(self.palettes.len() - 1)];
        let mut out = Vec::new();
        for block in &self.blocks {
            let Some(state) = palette.get(block.state as usize) else {
                continue;
            };
            if state.name != name {
                continue;
            }
            let rel = transform(block.pos, Mirror::None, rotation, [0, 0, 0]);
            out.push(TemplateBlockInfo {
                pos: [
                    rel[0] + position[0],
                    rel[1] + position[1],
                    rel[2] + position[2],
                ],
                state: state.rotate(rotation),
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
    /// # Two passes, because a processor can read the world
    ///
    /// `processBlockInfos` runs the **whole** chain over the **whole** block list
    /// before `placeInWorld` writes a single block, so a `RuleProcessor`'s
    /// `location_predicate` (village streets test for water under a `dirt_path`, to
    /// build a bridge) sees the pre-structure world — never an earlier block of
    /// the same template. S2's single pass had no processor that read the world, so
    /// the distinction did not exist; with rule location predicates it is the
    /// difference between a plank bridge and a random one.
    ///
    /// Returns the number of blocks actually written inside the grid.
    pub fn place(
        &self,
        origin: PlaceOrigin,
        settings: &PlaceSettings,
        grid: &mut DenseBlockGrid,
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
        let mut originals: Vec<([i32; 3], Option<Arc<BlockNbt>>)> = Vec::new();
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
            // Processors see the *unrotated* state at the absolute position —
            // `processBlockInfos` runs before the `mirror().rotate()` in the
            // placement loop. A processor returning `None` drops the block, which
            // is how air, rot and jigsaw replacement work.
            let mut current = Some(ProcessedBlock {
                pos: world,
                state: state.clone(),
            });
            for processor in &settings.processors {
                let Some(block_now) = current.take() else { break };
                let ctx = ProcessCtx {
                    local: block.pos,
                    reference: origin.reference,
                    nbt: block.nbt.as_deref(),
                    world: grid,
                };
                current = processor.process(&ctx, block_now);
            }
            if let Some(block_now) = current {
                processed.push(block_now);
                originals.push((block.pos, block.nbt.clone()));
            }
        }
        // `for (StructureProcessor processor : settings.getProcessors())
        //      processedBlockInfoList = processor.finalizeProcessing(...)` — every
        // processor in chain order, and every one but `capped` is the identity.
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
        for block in processed {
            // A `GravityProcessor` moves a block, so re-test the clip.
            if !inside(block.pos) {
                continue;
            }
            let mut final_state = block.state.mirror(settings.mirror).rotate(settings.rotation);
            if settings.waterlogging
                && final_state.is_waterloggable_and_dry()
                && grid.get(block.pos[0], block.pos[1], block.pos[2]).starts_with("minecraft:water")
            {
                final_state.properties.insert("waterlogged".into(), "true".into());
            }
            grid.set(block.pos[0], block.pos[1], block.pos[2], &final_state.canonical());
            written += 1;
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

fn parse_palette(value: &Nbt) -> Vec<BlockState> {
    let Nbt::List { elements, .. } = value else {
        return Vec::new();
    };
    elements
        .iter()
        .map(|entry| {
            let Some(entry) = compound(entry) else {
                return BlockState::of("minecraft:air");
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
            BlockState { name, properties }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_matches_vanilla_corner_arithmetic() {
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
            stair.rotate(Rotation::Cw90).canonical(),
            "minecraft:oak_stairs[facing=east,half=bottom]"
        );
        let log = BlockState::parse("minecraft:oak_log[axis=x]");
        assert_eq!(log.rotate(Rotation::Cw90).canonical(), "minecraft:oak_log[axis=z]");
        assert_eq!(log.rotate(Rotation::Cw180).canonical(), "minecraft:oak_log[axis=x]");
        let sign = BlockState::parse("minecraft:oak_sign[rotation=2]");
        assert_eq!(sign.rotate(Rotation::Cw90).canonical(), "minecraft:oak_sign[rotation=6]");
    }

    #[test]
    fn directional_flags_permute() {
        let fence = BlockState::parse("minecraft:oak_fence[east=true,north=false,south=false,west=false]");
        assert_eq!(
            fence.rotate(Rotation::Cw90).canonical(),
            "minecraft:oak_fence[east=false,north=false,south=true,west=false]"
        );
    }

    #[test]
    fn canonical_is_alphabetical_regardless_of_insertion_order() {
        let state = BlockState::parse("minecraft:oak_trapdoor[open=false,facing=north,half=top]");
        assert_eq!(
            state.canonical(),
            "minecraft:oak_trapdoor[facing=north,half=top,open=false]"
        );
    }
}
