//! NBT **structure templates** — the reader, the rotation/mirror transform, and
//! the write into a chunk's block grid (issue #514's S2).
//!
//! # What it is
//!
//! A port of vanilla's `StructureTemplate`: the `.nbt` files under
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
//!   `RandomSource.create(Mth.getSeed(templatePosition))` and
//!   `BlockRotProcessor`'s keep/drop roll is `Mth.getSeed(blockPos)` — so two
//!   chunks placing two halves of the same piece agree without communicating.
//! * **A write outside the grid's box is a no-op** ([`DenseBlockGrid::set`]), so
//!   "clip this piece to the chunk" needs no explicit box: the grid *is* the box.
//!   This is vanilla's `placeSettings.setBoundingBox(chunkBB)` by construction.
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
//!   dropped by the same `BlockIgnoreProcessor` vanilla drops them with.
//!
//! # Dependencies
//!
//! `lodestone-core` for the NBT codec, `flate2` for the gzip wrapper vanilla
//! writes these files with, and [`crate::dense_grid`] for the write target.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;

use lodestone_core::{Nbt, Reader};
use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

use super::BoundingBox;
use super::processor::Processor;
use crate::dense_grid::DenseBlockGrid;

/// One of vanilla's four `Rotation`s, in `Rotation.values()` order — the order
/// `Rotation.getRandom` indexes with a single `nextInt(4)`.
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
    /// `Rotation.getRandom(random)` — `Util.getRandom(values(), random)`, one
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

    /// `BlockState.rotate(rotation)`, per property (see the module doc for the
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
            // `Rotation.rotate(i, 16)`: +4 per clockwise quarter turn.
            let rotated = (value + 4 * turns) % 16;
            out.properties.insert("rotation".into(), rotated.to_string());
        }
        rotate_directional_flags(&mut out, turns);
        out
    }

    /// `BlockState.mirror(mirror)` for the same property set.
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

fn rotate_direction(dir: &str, turns: u32) -> Option<&'static str> {
    const CW: [&str; 4] = ["north", "east", "south", "west"];
    let index = CW.iter().position(|d| *d == dir)?;
    Some(CW[(index + turns as usize) % 4])
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

/// `StructureTemplate.transform(pos, mirror, rotation, pivot)`.
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

/// One block of a template: its position relative to the template origin and its
/// index into whichever palette was selected.
#[derive(Debug, Clone, Copy)]
struct TemplateBlock {
    pos: [i32; 3],
    state: u16,
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
                blocks.push(TemplateBlock { pos, state });
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

    /// `getSize()`.
    #[must_use]
    pub fn size(&self) -> [i32; 3] {
        self.size
    }

    /// `getBoundingBox(settings, position)` — the transformed corner-to-corner
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

    /// `StructurePlaceSettings.getRandomPalette(palettes, position)` — the
    /// palette index for a placement at `position`, from a fresh
    /// `RandomSource.create(Mth.getSeed(pos))`.
    #[must_use]
    pub fn palette_for(&self, position: [i32; 3]) -> usize {
        if self.palettes.len() == 1 {
            return 0;
        }
        let mut random = LegacyRandomSource::new(get_seed(position[0], position[1], position[2]));
        let count = i32::try_from(self.palettes.len()).unwrap_or(1);
        random.next_int_bounded(count).max(0) as usize
    }

    /// Places this template at `position` into `grid`, clipped to the grid's own
    /// box (a write outside it is a no-op, which is how per-chunk clipping
    /// happens — see the module doc).
    ///
    /// Returns the number of blocks actually written inside the grid.
    pub fn place(&self, position: [i32; 3], settings: &PlaceSettings, grid: &mut DenseBlockGrid) -> usize {
        let palette = &self.palettes[self.palette_for(position).min(self.palettes.len() - 1)];
        let (min_x, min_y, min_z, size_x, size_y, size_z) = grid.bounds();
        let mut written = 0;
        for block in &self.blocks {
            let rel = transform(block.pos, settings.mirror, settings.rotation, settings.pivot);
            let world = [rel[0] + position[0], rel[1] + position[1], rel[2] + position[2]];
            // The grid clips writes, but a processor chain is not free — skip the
            // whole block when it cannot land here anyway.
            if world[0] < min_x
                || world[0] >= min_x + size_x
                || world[1] < min_y
                || world[1] >= min_y + size_y
                || world[2] < min_z
                || world[2] >= min_z + size_z
            {
                continue;
            }
            let Some(state) = palette.get(block.state as usize) else {
                continue;
            };
            // Processors see the *unrotated* state at the absolute position —
            // `StructureTemplate.processBlockInfos` runs before the
            // `mirror().rotate()` in the placement loop. A processor returning
            // `None` drops the block, which is how air and rot work.
            let mut processed = Some(state.clone());
            for processor in &settings.processors {
                let Some(current) = processed.take() else { break };
                processed = processor.process(world, current);
            }
            let Some(processed) = processed else { continue };
            let mut final_state = processed.mirror(settings.mirror).rotate(settings.rotation);
            if settings.waterlogging
                && final_state.is_waterloggable_and_dry()
                && grid.get(world[0], world[1], world[2]).starts_with("minecraft:water")
            {
                final_state.properties.insert("waterlogged".into(), "true".into());
            }
            grid.set(world[0], world[1], world[2], &final_state.canonical());
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
