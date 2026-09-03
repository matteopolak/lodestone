//! Version-specific framing for this era's chunk packets
//! `minecraft:map_chunk`, `minecraft:update_light` and
//! `minecraft:unload_chunk`, at protocol 766.
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! `minecraft-data` models only the **outer** framing of `map_chunk` and
//! leaves the section geometry opaque: the payload ends in a `varint`-length
//! `chunkData` **buffer** whose contents have no declarative schema at all.
//! The layout below was settled against real captured server bytes
//! (`tests/captures/`).
//!
//! # Where the vertical window comes from, and why it moved again
//!
//! The column has not been sixteen sections tall since 1.17: the range is
//! **data**, a `min_y` (lowest block) and a `height` (blocks, a multiple of
//! 16). What changed at this protocol is *where that data is* — for the
//! second time in three eras. Through 1.18 the join packet carried the
//! resolved dimension entry inline. At 762 it carried the whole registry
//! plus the *name* of the type in use. Here the registry does not travel in
//! the join packet at all: it arrives during the **configuration** phase, one
//! packet per registry, and the join packet identifies the dimension by its
//! **index into `minecraft:dimension_type`** — a bare varint with no name
//! anywhere in the packet.
//!
//! So the shape is resolved in two steps that are separated in time:
//! [`DimensionRegistry`] retains the ordered entries as they arrive during
//! configuration, and [`ChunkShape::from_dimension_index`] reads `min_y` and
//! `height` off the entry at the index the join or respawn packet names.
//! A wrong section count does not error — it reads the wrong number of
//! containers out of `chunkData` and then mis-frames the trailing light — so
//! it lands as a length mismatch only because every read here is bounded by
//! the declared `chunkData` length and finished with a trailing-bytes check.
//! Those two checks are load-bearing rather than decorative.
//!
//! An index the registry does not have must leave the shape alone rather than
//! fall back to entry zero: entry zero is the overworld on a vanilla server,
//! and silently using its 384-block window in a 256-block nether is exactly
//! the failure this two-step exists to prevent.
//!
//! # What changed in the packet itself
//!
//! Measured against 762 from `minecraft-data` with named types inlined, and
//! confirmed against real bytes: two changes, both invisible to a round-trip
//! against our own encoder.
//!
//! * **The heightmap NBT lost its root name.** It is anonymous
//!   (network-form) NBT here — a tag byte then the payload. A decoder that
//!   reads the named form consumes two bytes of the payload as a name length
//!   and desynchronises immediately.
//! * **`trust_edges` is gone**, from `map_chunk` and from `update_light`
//!   alike. The bool the 1.17 and 1.19 eras read between the block entities
//!   and the light masks is not on the wire, so reading it steals the first
//!   byte of the sky-light mask count.
//!
//! # The `chunkData` buffer is exact again
//!
//! The 1.17 era measured a buffer *longer* than the sections inside it at 758
//! (one zero byte per single-valued block palette) and the 1.19 era measured
//! the same at 762. That is **not** true here, and it is re-measured rather
//! than inherited: the committed capture's column declares a `chunkData` of
//! 2,245 bytes across a 24-section window and the 24 sections consume all
//! 2,245. So this decoder asserts the buffer is exactly empty after the
//! sections — the stronger check, restored because the wire allows it again.

use lodestone_core::{Nbt, Reader, read_network_nbt};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_macros::Packet;
use lodestone_world::{
    BlockEntity, ChunkColumn, ChunkSection, ColumnLight, LightPatch, LongArrayFraming, PaletteKind,
    PalettedContainer, Result,
};

use crate::canonical::{CanonicalTable, FallbackTally};

/// Blocks per section edge; a section is `SECTION_EDGE` cubed cells.
const SECTION_EDGE: usize = 16;
/// Block-state cells per section (16×16×16).
const BLOCK_ENTRIES: usize = 4096;

/// The ordered `minecraft:dimension_type` entries the configuration phase
/// delivered, in the order they arrived — which is the order the join and
/// respawn packets index into.
///
/// Only the two keys that decide a column's geometry are kept. Everything
/// else in a dimension entry (ambient light, the infiniburn tag, monster
/// spawn rules) has no consumer in this crate, and keeping the whole blob
/// would mean holding every registry the server sends for the life of the
/// connection.
#[derive(Debug, Clone, Default)]
pub struct DimensionRegistry {
    /// One entry per dimension type, indexed exactly as the wire indexes it.
    entries: Vec<DimensionEntry>,
}

/// One dimension type's identity and vertical window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionEntry {
    /// The entry's resource id, e.g. `minecraft:overworld`. Retained for
    /// diagnostics and for the tests; nothing in the decode path keys off it,
    /// because the wire identifies the entry by index.
    pub id: String,
    /// Lowest world-`y` in a column of this dimension, when the entry
    /// carried a payload.
    pub min_y: Option<i32>,
    /// Column height in blocks, when the entry carried a payload.
    pub height: Option<i32>,
}

impl DimensionRegistry {
    /// Records the `minecraft:dimension_type` registry from one
    /// `registry_data` packet, replacing whatever was held before.
    ///
    /// An entry the server elided (because the client claimed the data pack
    /// it came from) keeps its id and reports no window; see
    /// [`crate::packets::configuration`] for why this client asks for none of
    /// them to be elided.
    pub fn adopt(&mut self, entries: &[crate::packets::configuration::PackedRegistryEntry]) {
        self.entries = entries
            .iter()
            .map(|entry| {
                let (min_y, height) = entry
                    .data
                    .as_ref()
                    .map_or((None, None), |value| (nbt_int(value, "min_y"), nbt_int(value, "height")));
                DimensionEntry {
                    id: entry.id.clone(),
                    min_y,
                    height,
                }
            })
            .collect();
    }

    /// The entry at a wire index, if the registry has one.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&DimensionEntry> {
        self.entries.get(index)
    }

    /// Number of entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no registry has arrived yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reads one `TAG_Int` out of an NBT compound by key.
fn nbt_int(value: &Nbt, key: &str) -> Option<i32> {
    let Nbt::Compound(fields) = value else {
        return None;
    };
    fields.iter().find_map(|(name, field)| match field {
        Nbt::Int(int) if name == key => Some(*int),
        _ => None,
    })
}

/// Dimension shape needed to decode one protocol's chunk column.
///
/// The vertical range is **data**, not a constant: [`Self::min_y`] and
/// [`Self::section_count`] come from the server's own dimension registry via
/// [`Self::from_dimension_index`]; the [`Self::overworld`] constructor is
/// only the pre-join default.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// The negotiated protocol. Selects the whole packet layout; never
    /// inferred from anything else here.
    pub protocol: i32,
    /// Lowest world-`y` in the column (`-64` for a vanilla overworld here).
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
    /// [`Self::canonical`], not the wire's own flat state 0.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The vanilla overworld window, used until the server's own registry
    /// arrives.
    ///
    /// `y = -64..320` is what this era's own jar declares for
    /// `minecraft:overworld` (`min_y` -64, `height` 384), and the committed
    /// capture's columns carry 24 sections, which is that height. A data pack
    /// can override it, which is why [`Self::from_dimension_index`] exists and
    /// this is only a fallback.
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
    /// dimension registry declares at `index`.
    ///
    /// Returns `None` — leaving the caller's shape untouched — when the
    /// registry has no such index, when the entry arrived without a payload,
    /// or when `height` is not a positive multiple of 16. Guessing a height
    /// is the one thing that must not happen: a section count is a byte
    /// count, and a wrong one desynchronises the stream instead of erroring.
    #[must_use]
    pub fn from_dimension_index(&self, registry: &DimensionRegistry, index: usize) -> Option<Self> {
        let entry = registry.get(index)?;
        let min_y = entry.min_y?;
        let height = entry.height?;
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

/// A decoded chunk column: block and biome sections, the block entities it
/// carries, and the light this protocol appends to the same packet.
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
    /// The column's block entities, positioned and typed.
    pub block_entities: Vec<BlockEntity>,
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
#[mc(name = "minecraft:map_chunk", state = Play, bound = Client, protocols = "766..=766")]
pub struct MapChunk;

/// The `minecraft:unload_chunk` packet (clientbound play).
///
/// # The field order is the wire's, not the obvious one
///
/// Both coordinates are plain big-endian ints, but **z comes first**: the
/// packet carries a single column position value whose serialization writes
/// the z coordinate ahead of the x coordinate, where every era below this one
/// wrote two separate x-then-z fields. `minecraft-data` models it that way at
/// this protocol, and `tests/capture_join.rs`'s unload gate checks it against
/// a real server's bytes — a swapped pair is invisible in a square view
/// distance and only shows up as columns unloading in the wrong place.
#[derive(Debug, Clone, Packet, lodestone_macros::Decode, lodestone_macros::Encode)]
#[mc(name = "minecraft:unload_chunk", state = Play, bound = Client, protocols = "766..=766")]
pub struct UnloadChunk {
    /// Chunk column z coordinate (in chunks) — first on the wire.
    pub chunk_z: i32,
    /// Chunk column x coordinate (in chunks).
    pub chunk_x: i32,
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
    /// that escapes its palette, a `chunkData` buffer that is not exactly
    /// consumed, or a negative length. The data comes from a network socket,
    /// so every framing decision validates rather than trusting the sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        // Anonymous, not named: see the module docs.
        read_network_nbt(r)?;

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
        blob.ensure_empty()?;

        // A positioned record per block entity: a packed `(x << 4) | z`
        // nibble pair, a whole-world `y`, this version's own block-entity
        // type id, then the payload as anonymous NBT that may be a bare
        // TAG_End. The wire's type id is read and discarded —
        // [`block_entity_from_state`] re-derives the canonical type from the
        // block state already resolved at the same position, so a block
        // entity carried in a chunk packet is typed from the same source as
        // one created by a state write.
        let entries = read_length(r)?;
        let mut block_entities = Vec::with_capacity(entries.min(4096));
        for _ in 0..entries {
            let packed_xz = r.u8()?;
            let y = r.i16()?;
            let _kind = r.var_i32()?;
            let nbt = read_network_nbt(r)?;
            let rel_x = packed_xz >> 4;
            let rel_z = packed_xz & 0x0F;
            let state = column.get_block(rel_x as usize, i32::from(y), rel_z as usize);
            block_entities.push(block_entity_from_state(rel_x, rel_z, y, state, nbt));
        }

        // The light payload, in exactly the shape `update_light` carries it —
        // and with no `trust_edges` byte ahead of it at this protocol.
        let light = ColumnLight::decode(shape.section_count, r)?;

        report(x, z, shape, fallback);
        Ok(ChunkData {
            x,
            z,
            column,
            light,
            block_entities,
            fallback,
        })
    }
}

/// Finishes a block entity given the canonical state already resolved at its
/// position.
///
/// The wire's own type id is not used: this era numbers block-entity types in
/// its own registry, which is not canonical space.
/// [`lodestone_data::block_entity_types::block_entity_type`] is the same
/// state-to-type derivation `World::sync_block_entity` uses.
///
/// The NBT payload is passed through exactly as decoded. Unlike the eras
/// below, no reshaping is needed for signs: two-sided signs landed in 1.20,
/// so a sign already arrives as the `front_text`/`back_text` compound
/// `lodestone_world::SignText::parse` reads, rather than the flat
/// `Text1`..`Text4` fields the pre-1.20 eras have to reshape.
fn block_entity_from_state(rel_x: u8, rel_z: u8, y: i16, state: u32, nbt: Nbt) -> BlockEntity {
    BlockEntity {
        rel_x,
        rel_z,
        y,
        type_id: block_entity_type(state).unwrap_or(0),
        nbt,
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
/// palette.
///
/// A zero-width container is `[0x00][varint value][varint 0]`, and
/// `PalettedContainer::decode` stops after the value because the family it
/// was written for writes no trailing long count. The width byte is peeked
/// off a copy of the reader — `Reader` is `Copy` — so the non-zero path hands
/// the untouched reader straight to the shared decoder. Getting this wrong
/// leaves one spare byte per single-valued container, up to 48 per column.
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
/// order) — the light-section mask form from 1.17 on.
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
            target: "v1-20-6::chunk",
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
#[mc(name = "minecraft:update_light", state = Play, bound = Client, protocols = "766..=766")]
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
    /// `varint chunkX`, `varint chunkZ`, then four `varint`-counted `i64[]`
    /// bitsets (sky, block, empty-sky, empty-block) and two `varint`-counted
    /// lists of `varint`-length 2048-byte nibble arrays. The `trust_edges`
    /// bool the eras below read after the coordinates is **not** on the wire
    /// here.
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
