//! Version-specific framing for this era's chunk packets
//! `minecraft:map_chunk`, `minecraft:update_light` and
//! `minecraft:unload_chunk`, at protocol 762.
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! `minecraft-data` models only the **outer** framing of `map_chunk` and
//! leaves the section geometry opaque: the payload ends in a `varint`-length
//! `chunkData` **buffer** whose contents have no declarative schema at all.
//! The layout below was settled against real captured server bytes
//! (`tests/captures/`).
//!
//! # Where the vertical window comes from, and why it moved
//!
//! The column has not been sixteen sections tall since 1.17: the range is
//! **data**, a `min_y` (lowest block) and a `height` (blocks, a multiple of
//! 16). What changed at 762 is *where that data is*. Through 1.18 the join
//! packet carried the already-resolved dimension entry inline, so those two
//! keys could be read straight out of one NBT blob. At 762 the join packet
//! carries the whole **registry** plus the *name* of the dimension type in
//! use, so the window has to be looked up:
//!
//! `dimension_codec` → `minecraft:dimension_type` → `value` (a list) → the
//! entry whose `name` matches → `element` → `min_y` and `height`.
//!
//! [`ChunkShape::from_dimension_registry`] walks exactly that path and
//! nothing else infers the window. A wrong section count does not error — it
//! reads the wrong number of containers out of `chunkData` and then
//! mis-frames the trailing light — so it lands as a length mismatch only
//! because every read here is bounded by the declared `chunkData` length and
//! finished with a trailing-bytes check. Those two checks are load-bearing
//! rather than decorative.
//!
//! # The packet shape itself is the era below's newer half
//!
//! Measured from `minecraft-data`, `map_chunk` is **unchanged** from 1.18.2
//! to 1.19.4, so there is one decoder here where the 1.17 era needs two: no
//! section mask and no column biome array, every section present and carrying
//! its own biome [`PalettedContainer`] after its block container, block
//! entities as positioned records, and the light payload appended to the same
//! packet rather than waiting for `update_light`.
//!
//! # What is shared with the era below
//!
//! * **Post-flattening, flat state ids — but not *26.2*'s flat state ids.** A
//!   palette entry is a single flat block-state id in this protocol's own
//!   global numbering, which 26.2 has renumbered since. Every value decoded
//!   by [`PalettedContainer::decode`] is translated through
//!   [`CanonicalTable::resolve_or_air`] before it reaches
//!   [`PalettedContainer::from_values`] — see [`crate::canonical`].
//! * **Non-straddling (padded) long packing**, as 1.16 introduced: a value
//!   never spans two 64-bit longs. That is what [`PalettedContainer::decode`]
//!   implements, and the long count is still VarInt-prefixed
//!   ([`LongArrayFraming::Prefixed`]) in both protocols here.
//! * **Heightmaps as NBT**, consumed rather than retained (the world store
//!   recomputes them lazily), to keep the zero-trailing-bytes detector
//!   meaningful.
//!
//! # The single-valued palette
//!
//! 1.18 added a bits-per-entry of **zero**: one VarInt value for the whole
//! container, followed by a VarInt long count of `0`. `PalettedContainer::decode`
//! returns as soon as it reads a zero width and never consumes that trailing
//! count, because the family it was written for (1.21.5+) does not write one.
//! [`decode_container`] therefore peeks the width byte and handles the
//! zero-width case itself. Getting this wrong leaves one spare byte per
//! single-valued container — up to 48 per column.
//!
//! # The `chunkData` buffer is longer than the sections inside it
//!
//! Every other protocol in this repo can be decoded with the buffer's
//! declared length as an exact assertion: read the sections, then require zero
//! trailing bytes. **This one cannot**, and the reason is a property of the wire,
//! not of this decoder.
//!
//! Measured against a real 1.18.2 server, three columns from the same flat
//! world:
//!
//! | sections with a single-valued block palette | declared `chunkData` | consumed by the sections | left over |
//! |---|---|---|---|
//! | 23 | 2,268 | 2,245 | **23** |
//! | 21 | 6,369 | 6,348 | **21** |
//! | 19 | 10,471 | 10,452 | **19** |
//!
//! The leftover is always exactly one zero byte per section whose *block*
//! container is single-valued, and it always sits at the very end: every
//! section parses contiguously at its predecessor's end with a valid header,
//! and the light payload that follows the buffer parses to the packet's last
//! byte. The server is sizing the buffer from an estimate that over-counts a
//! zero-width container by one byte and sending the whole allocation, padding
//! and all.
//!
//! So the check here is the strongest one that is still true: read exactly
//! `section_count` sections, then require what remains to be **all zero and
//! no longer than the section count**. A misparse of any section leaves
//! either nonzero bytes or too many of them, so the detector survives; only
//! the exact-length half of it is given up, and only where the wire forces
//! it.

use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_macros::Packet;
use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, LightPatch, LongArrayFraming, PaletteKind,
    PalettedContainer, Result,
};

use crate::canonical::{CanonicalTable, FallbackTally};

/// Finds one dimension type's `element` compound inside a decoded dimension
/// registry, by its namespaced name.
///
/// Split out of [`ChunkShape::from_dimension_registry`] so the walk is one
/// readable chain of `?`s: registry compound, the `minecraft:dimension_type`
/// entry, its `value` list, the element whose `name` matches, that element's
/// `element` field.
fn dimension_element<'a>(root: &'a Nbt, world_type: &str) -> Option<&'a Nbt> {
    let field = |compound: &'a Nbt, key: &str| match compound {
        Nbt::Compound(fields) => fields
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value)),
        _ => None,
    };
    let registry = field(root, "minecraft:dimension_type")?;
    let Nbt::List { elements, .. } = field(registry, "value")? else {
        return None;
    };
    elements.iter().find_map(|entry| {
        match field(entry, "name") {
            Some(Nbt::String(name)) if name == world_type => field(entry, "element"),
            _ => None,
        }
    })
}

/// Blocks per section edge; a section is `SECTION_EDGE` cubed cells.
const SECTION_EDGE: usize = 16;
/// Block-state cells per section (16×16×16).
const BLOCK_ENTRIES: usize = 4096;

/// Dimension shape needed to decode one protocol's chunk column.
///
/// Unlike every era below, the vertical range here is **not** a constant: see
/// the module docs. [`Self::min_y`] and [`Self::section_count`] come from the
/// server's own dimension entry via [`Self::from_dimension_nbt`]; the
/// [`Self::overworld`] constructor is only the pre-join default, and the
/// adapter replaces it the moment a `login` or `respawn` arrives.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// The negotiated protocol. Selects the whole packet layout; never
    /// inferred from anything else here.
    pub protocol: i32,
    /// Lowest world-`y` in the column (`-64` for a vanilla 1.19 overworld).
    pub min_y: i32,
    /// Number of block-state sections in the column (`height / 16`).
    pub section_count: usize,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the biome containers.
    pub biome_kind: PaletteKind,
    /// This era's wire-state -> canonical 26.2 table. Held here rather than
    /// looked up per call so a decode cannot reach a neighbouring era's
    /// numbering — see [`crate::canonical`]'s module docs.
    pub canonical: &'static CanonicalTable,
    /// Block-state id treated as air — the **canonical 26.2** air id from
    /// [`Self::canonical`], not the wire's own flat state 0. Every block this
    /// crate stores has already been translated by
    /// [`CanonicalTable::resolve_or_air`] by the time it reaches a
    /// [`PalettedContainer`], so this must match that id space, not the wire's.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The vanilla overworld window, used until the server's own registry
    /// arrives.
    ///
    /// `y = -64..320` is the vanilla default from 1.18 on, which every
    /// protocol in this era is. A server can override it through a data pack,
    /// which is why [`Self::from_dimension_registry`] exists and this is only
    /// a fallback.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`crate::PROTOCOLS`], via
    /// [`crate::canonical::table_for`].
    #[must_use]
    pub fn overworld(protocol: i32) -> Self {
        let canonical = crate::canonical::table_for(protocol);
        let (min_y, section_count) = (-64, 24);
        Self {
            protocol,
            min_y,
            section_count,
            block_kind: PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed),
            biome_kind: PaletteKind::biomes().with_framing(LongArrayFraming::Prefixed),
            canonical,
            air_id: canonical.air_state_id(),
            biome_id: 0,
        }
    }

    /// Replaces this shape's vertical window with the one the server's own
    /// dimension registry declares for `world_type`.
    ///
    /// `codec` is the raw named-NBT registry blob the `login` packet carries.
    /// The walk is `minecraft:dimension_type` → `value` (a list of compounds,
    /// each `{name, id, element}`) → the entry whose `name` equals
    /// `world_type` → `element` → the `min_y` and `height` ints. Nothing else
    /// is read, and no other route to those two keys is attempted: a
    /// dimension type whose name does not appear must leave the shape alone
    /// rather than take the first entry in the list, which would silently be
    /// the overworld's window in the nether.
    ///
    /// Returns `None` — leaving the caller's shape untouched — when the blob
    /// is not a compound, when any step of that walk is missing or the wrong
    /// tag, or when `height` is not a positive multiple of 16. Guessing a
    /// height is the one thing that must not happen: a section count is a
    /// byte count, and a wrong one desynchronises the stream instead of
    /// erroring.
    #[must_use]
    pub fn from_dimension_registry(&self, codec: &[u8], world_type: &str) -> Option<Self> {
        let (_, root) = read_named_nbt(&mut Reader::new(codec)).ok()?;
        let element = dimension_element(&root, world_type)?;
        let Nbt::Compound(fields) = element else {
            return None;
        };
        let int = |key: &str| {
            fields.iter().find_map(|(name, value)| match value {
                Nbt::Int(v) if name == key => Some(*v),
                _ => None,
            })
        };
        let min_y = int("min_y")?;
        let height = int("height")?;
        if height <= 0 || height % (SECTION_EDGE as i32) != 0 {
            return None;
        }
        Some(Self {
            min_y,
            section_count: (height as usize) / SECTION_EDGE,
            ..*self
        })
    }

}

/// A decoded chunk column: block and biome sections, plus the light this
/// protocol carries inline.
#[derive(Debug, Clone)]
pub struct ChunkData {
    /// Chunk column x coordinate (in chunks).
    pub x: i32,
    /// Chunk column z coordinate (in chunks).
    pub z: i32,
    /// Block-state and biome sections.
    pub column: ChunkColumn,
    /// Column light, decoded from this packet's own tail. The separate
    /// `update_light` packet still exists and still arrives for light-only
    /// changes; this is the copy that rides along with the column.
    pub light: ColumnLight,
    /// How many blocks in this column had a wire state id outside this era's
    /// own state range while bridging to a canonical 26.2 state — see
    /// [`CanonicalTable::resolve_or_air`]. Zero for every real-world column;
    /// surfaced here (and logged, see [`MapChunk::decode`]) rather than
    /// silently absorbed.
    pub fallback: FallbackTally,
}

/// The `minecraft:map_chunk` packet (clientbound play).
///
/// A thin [`Packet`] marker: id/name/state/bound come from the derive, but
/// decoding is hand-written via [`MapChunk::decode`] (see the module docs).
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk", state = Play, bound = Client)]
pub struct MapChunk;

/// The `minecraft:unload_chunk` packet (clientbound play).
///
/// A plain derived struct — two big-endian ints.
#[derive(Debug, Clone, Packet, lodestone_macros::Decode, lodestone_macros::Encode)]
#[mc(name = "minecraft:unload_chunk", state = Play, bound = Client)]
pub struct UnloadChunk {
    /// Chunk column x coordinate (in chunks).
    pub chunk_x: i32,
    /// Chunk column z coordinate (in chunks).
    pub chunk_z: i32,
}

impl MapChunk {
    /// Decodes a `map_chunk` body into a [`ChunkData`] given the dimension
    /// [`ChunkShape`].
    ///
    /// Consumes the entire packet. The caller should still invoke
    /// [`Reader::ensure_empty`] on the outer reader: zero trailing bytes
    /// across the whole packet is the single best detector of a subtly wrong
    /// layout.
    ///
    /// # Errors
    ///
    /// Returns an error on malformed input: a truncated buffer, a declared
    /// long count that disagrees with the section geometry, a palette index
    /// that escapes its palette, a biome array whose length disagrees with the
    /// column height, or a negative length. The data comes from a network
    /// socket, so every framing decision validates rather than trusting the
    /// sender.
    /// The era's one shape: `x`, `z`, heightmap NBT, a section blob in which
    /// every section carries a block container **and** a biome container, the
    /// positioned block-entity list, then the light payload `update_light`
    /// otherwise carries.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        read_named_nbt(r)?;

        let mut fallback = FallbackTally::default();
        let blob_len = read_length(r)?;
        let mut blob = r.take_reader(blob_len)?;
        let mut column = new_column(shape);
        for index in 0..shape.section_count {
            let _block_count = blob.i16()?;
            let blocks = read_translated_blocks(&mut blob, shape, &mut fallback)?;
            let biome_container = decode_container(shape.biome_kind, &mut blob)?;
            put_section(&mut column, shape, index, blocks, biome_container);
        }
        // Not `ensure_empty`: see the module docs. The server pads this
        // buffer with one zero byte per single-valued block container, so the
        // check is that what remains is all zero and cannot be a section.
        let padding = blob.remaining_bytes();
        if padding.len() > shape.section_count || padding.iter().any(|&byte| byte != 0) {
            return Err(lodestone_core::Error::Custom(format!(
                "chunkData has {} bytes left after {} sections (at most {} zero \
                 padding bytes are expected)",
                padding.len(),
                shape.section_count,
                shape.section_count
            ))
            .into());
        }

        // Block entities became a positioned record rather than a bare NBT
        // compound: a packed `(x << 4) | z` nibble pair, a whole-world `y`,
        // the block-entity type, then the data. Consumed but not retained, to
        // keep the zero-trailing-bytes gate honest.
        let entries = read_length(r)?;
        for _ in 0..entries {
            let _packed_xz = r.u8()?;
            let _y = r.i16()?;
            let _kind = r.var_i32()?;
            let _ = read_named_nbt(r)?;
        }

        // The light payload, in exactly the shape `update_light` carries it.
        let _trust_edges = r.bool()?;
        let light = ColumnLight::decode(shape.section_count, r)?;

        report(x, z, shape, fallback);
        Ok(ChunkData {
            x,
            z,
            column,
            light,
            fallback,
        })
    }
}

/// Builds the empty column this shape's sections are written into.
fn new_column(shape: &ChunkShape) -> ChunkColumn {
    ChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    )
}

/// Reads one section's block container and translates every cell into the
/// canonical 26.2 id space.
///
/// Per cell rather than per palette entry, the same tradeoff the eras below
/// document: `resolve_or_air` is a plain array index, and per-cell is what
/// makes the tally count *blocks* substituted.
fn read_translated_blocks(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    fallback: &mut FallbackTally,
) -> Result<PalettedContainer> {
    let raw = decode_container(shape.block_kind, blob)?;
    let translated: Vec<u32> = (0..BLOCK_ENTRIES)
        .map(|i| shape.canonical.resolve_or_air(raw.get(i), fallback))
        .collect();
    Ok(PalettedContainer::from_values(shape.block_kind, &translated))
}

/// Stores one decoded section, eliding it when it is indistinguishable from
/// the column's own empty state.
fn put_section(
    column: &mut ChunkColumn,
    shape: &ChunkShape,
    index: usize,
    blocks: PalettedContainer,
    biomes: PalettedContainer,
) {
    let section = ChunkSection::from_containers(blocks, biomes, shape.air_id);
    if !section.is_empty(shape.biome_id) {
        column.set_section(index, Some(section));
    }
}

/// Decodes one [`PalettedContainer`], handling the zero-width (single-valued)
/// palette 1.18 introduced.
///
/// See the module docs: a zero-width container is `[0x00][varint value][varint
/// 0]`, and `PalettedContainer::decode` stops after the value because the
/// family it was written for writes no trailing long count. The width byte is
/// peeked off a copy of the reader — `Reader` is `Copy` — so the non-zero path
/// hands the untouched reader straight to the shared decoder.
fn decode_container(kind: PaletteKind, r: &mut Reader<'_>) -> Result<PalettedContainer> {
    let mut peek = *r;
    if peek.u8()? != 0 {
        return Ok(PalettedContainer::decode(kind, r)?);
    }
    let _bits = r.u8()?;
    let value = u32::try_from(r.var_i32()?).unwrap_or(0);
    let longs = r.var_i32()?;
    if longs != 0 {
        return Err(lodestone_core::Error::Custom(format!(
            "single-valued palette declares {longs} packed longs, expected 0"
        ))
        .into());
    }
    Ok(PalettedContainer::new(kind, value))
}

/// Reads a `varint`-counted `i64[]` bitset (LSB-first, little-endian word
/// order) — the form 1.17 replaced the single-VarInt section mask with.
fn read_long_bitset(r: &mut Reader<'_>) -> Result<Vec<u64>> {
    let count = read_length(r)?;
    let mut words = Vec::with_capacity(count);
    for _ in 0..count {
        words.push(r.i64()? as u64);
    }
    Ok(words)
}

/// Reads a VarInt length, rejecting a negative one rather than wrapping it.
fn read_length(r: &mut Reader<'_>) -> Result<usize> {
    let raw = r.var_i32()?;
    Ok(usize::try_from(raw).map_err(|_| lodestone_core::Error::NegativeLength(raw))?)
}

/// Logs any canonicalisation fallbacks for one column.
fn report(x: i32, z: i32, shape: &ChunkShape, fallback: FallbackTally) {
    if !fallback.is_empty() {
        tracing::warn!(
            target: "v1-19::chunk",
            x,
            z,
            protocol = shape.protocol,
            out_of_range = fallback.out_of_range,
            "substituted air for {} block(s) whose wire state id could not be \
             resolved to a canonical 26.2 state",
            fallback.out_of_range,
        );
    }
}

/// The `minecraft:update_light` packet (clientbound play).
///
/// A thin [`Packet`] marker: decoding is hand-written via
/// [`UpdateLight::decode`] because `minecraft-data` models the light arrays as
/// nested arrays whose section indexing lives in `lodestone-world`.
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:update_light", state = Play, bound = Client)]
pub struct UpdateLight;

/// A decoded `update_light` payload: the column position and a version-free
/// [`LightPatch`] ready to merge into the world store.
#[derive(Debug, Clone)]
pub struct LightUpdate {
    /// Chunk column x coordinate (in chunks).
    pub x: i32,
    /// Chunk column z coordinate (in chunks).
    pub z: i32,
    /// Sky/block light layers keyed by the wire masks.
    pub patch: LightPatch,
}

impl UpdateLight {
    /// Decodes an `update_light` body into a [`LightUpdate`].
    ///
    /// # Wire shape
    ///
    /// `varint chunkX`, `varint chunkZ`, `bool trustEdges`, then four
    /// `varint`-counted `i64[]` bitsets (sky, block, empty-sky, empty-block)
    /// and two `varint`-counted lists of `varint`-length 2048-byte nibble
    /// arrays. Identical in both protocols of this era — 1.17 is where the
    /// masks stopped being single VarInts, precisely because a column can now
    /// have more than 32 light sections.
    ///
    /// The masks index **light** sections, of which there are
    /// `section_count + 2`: one below the column and one above.
    ///
    /// # Errors
    ///
    /// Returns an error on truncated input or a light array whose declared
    /// length is not 2048 bytes.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<LightUpdate> {
        let x = r.var_i32()?;
        let z = r.var_i32()?;
        let _trust_edges = r.bool()?;
        let sky_mask = read_long_bitset(r)?;
        let block_mask = read_long_bitset(r)?;
        let empty_sky_mask = read_long_bitset(r)?;
        let empty_block_mask = read_long_bitset(r)?;

        let sky = read_light_arrays(r)?;
        let block = read_light_arrays(r)?;
        let _ = shape;

        let patch = LightPatch::from_light_masks(
            &sky_mask,
            &empty_sky_mask,
            sky,
            &block_mask,
            &empty_block_mask,
            block,
        );

        Ok(LightUpdate { x, z, patch })
    }
}

/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;

/// Reads a `varint`-counted list of `varint`-length 2048-byte nibble arrays.
///
/// 1.17 gave the list its own count; below this era the count was implied by
/// the mask's population, which is why this cannot be shared downward.
fn read_light_arrays(r: &mut Reader<'_>) -> Result<Vec<lodestone_world::NibbleArray>> {
    let count = read_length(r)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_length(r)?;
        if len != LIGHT_BYTES {
            return Err(lodestone_core::Error::Custom(format!(
                "update_light array length {len} != {LIGHT_BYTES}"
            ))
            .into());
        }
        out.push(lodestone_world::NibbleArray::from_bytes(r.bytes(LIGHT_BYTES)?)?);
    }
    Ok(out)
}
