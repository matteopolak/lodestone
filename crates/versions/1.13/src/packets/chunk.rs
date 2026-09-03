//! Version-specific framing for protocol 404's chunk packets
//! `minecraft:map_chunk` and `minecraft:unload_chunk`.
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! `minecraft-data` models only the **outer** framing of `map_chunk` and
//! leaves the section geometry opaque: in
//! `vendor/minecraft-data/data/pc/1.13.2/protocol.json` the payload is
//! `i32 x`, `i32 z`, `bool groundUp`, `varint bitMap`, a `varint`-length
//! `chunkData` **buffer**, then a `varint`-counted `blockEntities` array of
//! raw `nbt`. The bytes *inside* `chunkData` have no declarative schema, and
//! the schema does not mention light or biomes at all — so the layout below
//! is settled against real captured server bytes
//! (`tests/captures/join_1_13_2.txt`), not against the schema. The same
//! omission cost the 1.14 era a full 1,024 unread bytes when it was believed.
//!
//! # Where 1.13.2 sits between its two neighbours
//!
//! It is the only protocol in this repo that is **post-flattening and still
//! carries light inside the chunk packet**, which is exactly why neither
//! neighbouring era's decoder can serve it:
//!
//! * **Post-flattening, flat state ids — but not 26.2's flat state ids.** A
//!   palette entry is a single flat block-state id, not the pre-1.13
//!   `(blockId << 4) | meta` composite v1-8/v1-9 bridge through
//!   `lodestone_canonical`. It is still **1.13.2's own** global-palette
//!   numbering (8,599 states against 26.2's 32k), so every value decoded here
//!   is translated through [`CanonicalTable::resolve_or_air`] before it
//!   reaches [`PalettedContainer::from_values`] — see [`crate::canonical`].
//! * **Light is still inline.** Each present section ends with a 2,048-byte
//!   block-light nibble array and, in a sky-lit dimension, a 2,048-byte
//!   sky-light array. 1.14 moved both into a separate `update_light` packet,
//!   so a 1.14-era decoder run here treats the first light array as the next
//!   section's header.
//! * **No heightmap NBT, and no per-section block count.** 1.14 added both:
//!   an inline `MOTION_BLOCKING` compound before the sections, and a leading
//!   `i16` non-air count on every section. Neither exists at 404.
//! * **Straddling long packing.** A packed index may cross a 64-bit boundary,
//!   the pre-1.16 layout. [`PalettedContainer::decode`] implements only the
//!   1.16+ padded layout, so — exactly like v1-8 and v1-9 — this crate
//!   unpacks the longs itself and rebuilds a version-free container with
//!   [`PalettedContainer::from_values`].
//! * **2-D biomes, as `i32`s.** A full column ends with a 16×16 array of
//!   **big-endian `i32`** biome ids at the tail of the *buffer*. 1.13 is the
//!   release that widened them from bytes (v1-8/v1-9 read 256 bytes here);
//!   1.15 moved them out of the buffer entirely and made them 3-D. The
//!   version-free [`ChunkSection`] stores a 3-D 4×4×4 biome container, so
//!   this crate fabricates one by down-sampling 16×16→4×4 and replicating
//!   over Y — lossy horizontally and fictional vertically, the same
//!   `lodestone-world` seam v1-8 and v1-9 record.
//! * **Block entities** are full **named NBT** compounds, each carrying its
//!   own `x`/`y`/`z` and a string `id` rather than a wire header.
//!   [`block_entity_from_embedded_nbt`] reads the position back out of the
//!   compound and derives [`BlockEntity::type_id`] from the canonical block
//!   state already resolved there, rather than from the era's own string id.
//!   A sign's `Text1`..`Text4` are reshaped into the canonical
//!   `front_text`/`messages` compound so `SignText::parse` can read them
//!   (see [`legacy_sign_nbt`]); every other block-entity type's payload is
//!   kept exactly as decoded, unmapped.

use lodestone_core::{Nbt, NbtTag, Reader, read_named_nbt};
use lodestone_data::block_entity_types::block_entity_type;
use lodestone_data::block_states;
use lodestone_macros::Packet;
use lodestone_model::Text;
use lodestone_world::{
    BlockEntity, ChunkColumn, ChunkSection, ColumnLight, LightData, NibbleArray, PaletteKind,
    PalettedContainer, Result,
};

use crate::canonical::{CanonicalTable, FallbackTally};

/// Number of block sections in a 1.13.2 column (fixed world height 0..256).
const SECTION_COUNT: usize = 16;
/// Biome cells per section (4×4×4).
const BIOME_CELLS_PER_SECTION: usize = 64;
/// Block-state cells per section (16×16×16).
const BLOCK_ENTRIES: usize = 4096;
/// Entries in a full column's 2-D biome array (one per 16×16 column).
const TWO_D_BIOME_CELLS: usize = 256;
/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;

/// Dimension shape needed to decode a 1.13.2 chunk column.
///
/// As in 1.8 and 1.12.2, `map_chunk` cannot say from its own bytes whether
/// sky light is present — that follows from the dimension the join packet
/// announced — so it is supplied here. Columns are always 16 sections tall
/// with `min_y = 0`.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// The negotiated protocol. Carried so a decode can be attributed in a
    /// log and so the shape can never be built for a protocol this crate does
    /// not serve; never inferred from anything else here.
    pub protocol: i32,
    /// Whether the dimension carries sky light (true for the overworld). When
    /// set, each section carries a trailing 2048-byte sky-light array.
    pub has_skylight: bool,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the (fabricated) biome containers.
    pub biome_kind: PaletteKind,
    /// This protocol's wire-state -> canonical 26.2 table. Held here rather
    /// than looked up per call so a decode cannot reach a neighbouring
    /// protocol's numbering — see [`crate::canonical`]'s module docs.
    pub canonical: &'static CanonicalTable,
    /// Block-state id treated as air — the **canonical 26.2** air id from
    /// [`Self::canonical`], not the wire's own flat state 0. Every block this
    /// crate stores has already been translated by
    /// [`CanonicalTable::resolve_or_air`] by the time it reaches a
    /// [`PalettedContainer`], so this must match that id space, not the
    /// wire's.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The overworld shape for `protocol`: 16 sections, `min_y = 0`, sky
    /// light present, flat state ids.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`crate::PROTOCOLS`], via
    /// [`crate::canonical::table_for`].
    #[must_use]
    pub fn overworld(protocol: i32) -> Self {
        let canonical = crate::canonical::table_for(protocol);
        Self {
            protocol,
            has_skylight: true,
            block_kind: PaletteKind::block_states(),
            biome_kind: PaletteKind::biomes(),
            canonical,
            air_id: canonical.air_state_id(),
            biome_id: 0,
        }
    }

    /// A dimension without sky light (nether/end): no trailing sky-light
    /// arrays.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`crate::PROTOCOLS`].
    #[must_use]
    pub fn no_skylight(protocol: i32) -> Self {
        Self {
            has_skylight: false,
            ..Self::overworld(protocol)
        }
    }
}

/// A decoded chunk column: block sections plus inline sky/block light.
///
/// 1.13.2 carries no heightmaps in the chunk packet, so this holds only what
/// the world store can use directly.
#[derive(Debug, Clone)]
pub struct ChunkData {
    /// Chunk column x coordinate (in chunks).
    pub x: i32,
    /// Chunk column z coordinate (in chunks).
    pub z: i32,
    /// Whether this was a full "ground-up" column (carries biomes; absent
    /// sections are air) versus a partial update (absent sections unchanged).
    pub ground_up: bool,
    /// Block-state and biome sections.
    pub column: ChunkColumn,
    /// Sky and block light, read from inside the chunk packet.
    pub light: ColumnLight,
    /// Block entities within the column — see the module docs for how a
    /// legacy compound (own `x`/`y`/`z`/`id`, no wire header) becomes one of
    /// these.
    pub block_entities: Vec<BlockEntity>,
    /// How many blocks in this column had a wire state id outside 1.13.2's
    /// own state range while bridging to a canonical 26.2 state — see
    /// [`CanonicalTable::resolve_or_air`]. Zero for every real-world column;
    /// surfaced here (and logged, see [`MapChunk::decode`]) rather than
    /// silently absorbed so a wrong mapping stays traceable.
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
    /// Consumes the entire packet — outer framing, the length-prefixed
    /// `chunkData` buffer (validated to zero trailing bytes on its own), and
    /// the trailing block-entity NBT list. The caller should still invoke
    /// [`Reader::ensure_empty`] on the outer reader: zero trailing bytes
    /// across the whole packet is the single best detector of a subtly wrong
    /// layout.
    ///
    /// # Errors
    ///
    /// Returns an error on malformed input: a truncated buffer, a declared
    /// long count that disagrees with the straddling section geometry, a
    /// palette index that escapes its palette, a light array of the wrong
    /// size, or a negative length. The data comes from a network socket, so
    /// every framing decision validates rather than trusting the sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        let ground_up = r.bool()?;
        let bitmask =
            u32::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;

        // chunkData: a VarInt-length buffer holding the present sections and,
        // on a full column, the biome tail.
        let raw_len = r.var_i32()?;
        let blob_len =
            usize::try_from(raw_len).map_err(|_| lodestone_core::Error::NegativeLength(raw_len))?;
        let mut blob = r.take_reader(blob_len)?;
        let mut data = decode_column(&mut blob, shape, x, z, ground_up, bitmask)?;

        // The declared chunkData length must exactly match the section
        // geometry plus the biome tail; any slack is a misparse (or a wrong
        // dimension/skylight assumption).
        blob.ensure_empty()?;

        // Block entities trail the buffer as full named-NBT compounds, each
        // carrying its own `x`/`y`/`z` and a string `id` rather than the wire
        // header the modern compact record has.
        let block_entity_count = r.var_i32()?;
        if block_entity_count < 0 {
            return Err(lodestone_core::Error::NegativeLength(block_entity_count).into());
        }
        let mut block_entities = Vec::with_capacity(block_entity_count.clamp(0, 4096) as usize);
        for _ in 0..block_entity_count {
            let (_name, nbt) = read_named_nbt(r)?;
            if let Some(entity) = block_entity_from_embedded_nbt(nbt, &data.column) {
                block_entities.push(entity);
            }
        }
        data.block_entities = block_entities;

        if !data.fallback.is_empty() {
            tracing::warn!(
                target: "v1-13::chunk",
                x,
                z,
                protocol = shape.protocol,
                out_of_range = data.fallback.out_of_range,
                "substituted air for {} block(s) whose wire state id could not be \
                 resolved to a canonical 26.2 state",
                data.fallback.out_of_range,
            );
        }

        Ok(data)
    }
}

/// Decodes one column's worth of section data (and its biome tail) from the
/// `chunkData` buffer into the version-free storage types.
///
/// `blob` is the length-prefixed `chunkData` sub-reader and is consumed
/// exactly. Sections are **interleaved**: each is a full
/// `[blocks, blockLight, skyLight]` record before the next, so a decoder that
/// reads all the block data first (1.8's grouped layout) desynchronises at
/// the second present section rather than failing at the first.
fn decode_column(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    x: i32,
    z: i32,
    ground_up: bool,
    bitmask: u32,
) -> Result<ChunkData> {
    let present: Vec<usize> = (0..SECTION_COUNT)
        .filter(|i| bitmask & (1 << i) != 0)
        .collect();

    let mut column = ChunkColumn::new(
        0,
        SECTION_COUNT,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    let mut light = ColumnLight::new(SECTION_COUNT);
    let mut fallback = FallbackTally::default();

    let mut section_blocks: Vec<(usize, PalettedContainer)> = Vec::with_capacity(present.len());
    for &index in &present {
        let values = decode_section_blocks(blob, shape, &mut fallback)?;
        section_blocks.push((
            index,
            PalettedContainer::from_values(shape.block_kind, &values),
        ));

        let block_bytes = blob.bytes(LIGHT_BYTES)?;
        *light.block_mut(light_section(index)) =
            LightData::Values(NibbleArray::from_bytes(block_bytes)?);

        if shape.has_skylight {
            let sky_bytes = blob.bytes(LIGHT_BYTES)?;
            *light.sky_mut(light_section(index)) =
                LightData::Values(NibbleArray::from_bytes(sky_bytes)?);
        }
    }

    // Biome tail: 256 **big-endian `i32`s** (16×16) on full columns only, at
    // the end of the buffer rather than as a field of its own. 1.13 widened
    // these from the bytes v1-8/v1-9 read, so a pre-1.13 decoder here leaves
    // 768 bytes unread and a 1.15-era one looks for them before the sections.
    let biome_cells = if ground_up {
        Some(downsample_biomes(&read_flat_biomes(blob)?))
    } else {
        None
    };

    for (index, blocks) in section_blocks {
        let biomes = match &biome_cells {
            Some(cells) => PalettedContainer::from_values(shape.biome_kind, cells),
            None => PalettedContainer::new(shape.biome_kind, shape.biome_id),
        };
        let section = ChunkSection::from_containers(blocks, biomes, shape.air_id);
        if !section.is_empty(shape.biome_id) {
            column.set_section(index, Some(section));
        }
    }

    Ok(ChunkData {
        x,
        z,
        ground_up,
        column,
        light,
        block_entities: Vec::new(),
        fallback,
    })
}

/// Maps a world block-section index to its light-section index.
///
/// `ColumnLight` spans `section_count + 2` light sections, with light section
/// `0` sitting *below* the world, so the two indices differ by one. Getting
/// this wrong stores every section's light one section low, which nothing in
/// the chunk decode can detect — the arrays are the right size and the buffer
/// still ends exactly where it should.
const fn light_section(block_section: usize) -> usize {
    block_section + 1
}

/// Reads the 256 big-endian `i32` biome ids of a full column, with no length
/// prefix of any kind.
fn read_flat_biomes(blob: &mut Reader<'_>) -> Result<Vec<u32>> {
    let mut all = Vec::with_capacity(TWO_D_BIOME_CELLS);
    for _ in 0..TWO_D_BIOME_CELLS {
        all.push(u32::try_from(blob.i32()?).unwrap_or(0));
    }
    Ok(all)
}

/// Down-samples a 16×16 column biome array into the 64 cells of a 4×4×4
/// section container, replicating the same XZ cell up every Y layer.
///
/// The vertical dimension is fabricated: 1.13.2 has no per-Y biome data at
/// all (that arrived in 1.15), and the version-free container has nowhere to
/// record its absence. The XZ sample is the block at each 4×4 cell's corner,
/// which is what v1-8 and v1-9 do for the same seam.
fn downsample_biomes(all: &[u32]) -> Vec<u32> {
    let mut cells = vec![0u32; BIOME_CELLS_PER_SECTION];
    for cy_local in 0..4 {
        for cz in 0..4 {
            for cx in 0..4 {
                let wire = (cz * 4) * 16 + cx * 4;
                let local = (cy_local << 4) | (cz << 2) | cx;
                cells[local] = all.get(wire).copied().unwrap_or(0);
            }
        }
    }
    cells
}

/// Reads one section's paletted block data and returns the 4096 **canonical
/// 26.2** block-state ids, translated from 1.13.2's own flat wire state ids
/// via [`CanonicalTable::resolve_or_air`].
///
/// `[bitsPerBlock: u8][paletteLen: varint][palette: varint*][longCount:
/// varint][longs: i64*]`, with the indices packed so a value **may** cross a
/// 64-bit boundary. A `paletteLen` of zero means the direct/global palette:
/// the indices are wire state ids themselves. There is **no** leading `i16`
/// non-air block count — 1.14 added that, and reading one here consumes the
/// first two bytes of the palette.
///
/// The declared long count is checked against the straddling geometry rather
/// than trusted, which is what makes a 1.16-era (padded) column fed to this
/// decoder fail loudly for every width that is not a divisor of 64.
fn decode_section_blocks(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    fallback: &mut FallbackTally,
) -> Result<Vec<u32>> {
    let bits = u32::from(blob.u8()?);
    if bits == 0 || bits > 32 {
        return Err(lodestone_core::Error::Custom(format!("invalid bits-per-block {bits}")).into());
    }

    let palette_len =
        usize::try_from(blob.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
    if palette_len > BLOCK_ENTRIES {
        return Err(lodestone_core::Error::Custom(format!(
            "palette length {palette_len} exceeds {BLOCK_ENTRIES}"
        ))
        .into());
    }
    let mut palette = Vec::with_capacity(palette_len);
    for _ in 0..palette_len {
        palette.push(u32::try_from(blob.var_i32()?).unwrap_or(0));
    }
    // Translate the (typically small) palette once rather than each of the
    // 4096 output cells: `resolve_or_air` is a plain array index either way,
    // but a per-palette translation keeps the tally counting distinct wire
    // states rather than blocks. Sections large enough to use the direct
    // encoding translate each raw value instead, below.
    let translated_palette: Vec<u32> = palette
        .iter()
        .map(|&raw| shape.canonical.resolve_or_air(raw, fallback))
        .collect();

    let declared =
        usize::try_from(blob.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
    let expected = straddling_long_count(bits, BLOCK_ENTRIES);
    if declared != expected {
        return Err(lodestone_core::Error::Custom(format!(
            "long count {declared} disagrees with straddling geometry {expected} for {bits} bits"
        ))
        .into());
    }
    let mut longs = Vec::with_capacity(declared);
    for _ in 0..declared {
        longs.push(blob.i64()? as u64);
    }

    let indices = unpack_straddling(&longs, bits, BLOCK_ENTRIES);
    let mut values = vec![0u32; BLOCK_ENTRIES];
    for (out, &raw) in values.iter_mut().zip(indices.iter()) {
        *out = if translated_palette.is_empty() {
            shape.canonical.resolve_or_air(raw, fallback)
        } else {
            *translated_palette.get(raw as usize).ok_or_else(|| {
                lodestone_world::WorldError::from(lodestone_core::Error::Custom(format!(
                    "palette index {raw} escapes palette of {}",
                    translated_palette.len()
                )))
            })?
        };
    }
    Ok(values)
}

/// Number of 64-bit longs the **pre-1.16 (straddling)** packing uses for
/// `count` entries of `bits` width: values are packed with no per-long
/// padding, so a value may cross a boundary and the total is
/// `ceil(count * bits / 64)`.
const fn straddling_long_count(bits: u32, count: usize) -> usize {
    (count * bits as usize).div_ceil(64)
}

/// Unpacks `count` entries of `bits` width from `longs` using the pre-1.16
/// **straddling** layout, where an entry that crosses a 64-bit boundary is
/// reconstructed from the low bits of one long and the high bits of the next.
///
/// This is the crux of why [`PalettedContainer::decode`] cannot serve 404: it
/// implements only the 1.16+ padded layout where entries never straddle.
fn unpack_straddling(longs: &[u64], bits: u32, count: usize) -> Vec<u32> {
    let mask: u64 = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut out = vec![0u32; count];
    for (i, slot) in out.iter_mut().enumerate() {
        let bit_index = i * bits as usize;
        let start = bit_index / 64;
        let offset = (bit_index % 64) as u32;
        let value = if offset + bits <= 64 {
            (longs[start] >> offset) & mask
        } else {
            let low = longs[start] >> offset;
            let high = longs[start + 1] << (64 - offset);
            (low | high) & mask
        };
        *slot = value as u32;
    }
    out
}

/// Builds a canonical block entity from this era's compound, which carries
/// its own `x`/`y`/`z` position (and a string `id`) inline rather than in the
/// wire header the modern compact record has. Returns `None` when the
/// compound has no int `x`/`y`/`z` triplet — that shape gives a `BlockEntity`
/// nothing to key itself on, so this reports it as absent rather than
/// fabricating a position.
fn block_entity_from_embedded_nbt(nbt: Nbt, column: &ChunkColumn) -> Option<BlockEntity> {
    let Nbt::Compound(fields) = &nbt else {
        return None;
    };
    let int = |key: &str| {
        fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            Nbt::Int(i) => Some(*i),
            _ => None,
        })
    };
    let x = int("x")?;
    let y = int("y")?;
    let z = int("z")?;
    let rel_x = (x & 0xF) as u8;
    let rel_z = (z & 0xF) as u8;
    let state = column.get_block(rel_x as usize, y, rel_z as usize);
    Some(block_entity_from_state(rel_x, rel_z, y, state, nbt))
}

/// Finishes a block entity given the canonical state already resolved at its
/// position.
///
/// The wire's own type identifier — a string `id` embedded in the compound —
/// is not used to derive [`BlockEntity::type_id`]:
/// [`lodestone_data::block_entity_types::block_entity_type`] is the same
/// state-to-type derivation `World::sync_block_entity` uses for a block
/// entity created by a state write, so a block entity carried in the chunk
/// packet gets a type id from the same source as one created any other way —
/// nothing downstream reads [`BlockEntity::type_id`] to decide what a block
/// entity is; the block state does that (see `lodestone-shell`'s
/// `block_entities` module docs).
///
/// The NBT payload is reshaped to the canonical schema only where a mapping
/// is known — currently a sign's `Text1`..`Text4`/`Color`/`GlowingText`
/// fields (see [`legacy_sign_nbt`]). Every other block-entity type's payload
/// is passed through exactly as decoded: chest contents, spawner data and
/// banner patterns all use an item-stack or id-list shape that has changed
/// since, and reshaping those without an outside oracle for the target shape
/// would be an invented mapping.
fn block_entity_from_state(rel_x: u8, rel_z: u8, y: i32, state: u32, nbt: Nbt) -> BlockEntity {
    let type_id = block_entity_type(state).unwrap_or(0);
    let nbt = if is_sign_state(state) {
        legacy_sign_nbt(&nbt)
    } else {
        nbt
    };
    BlockEntity {
        rel_x,
        rel_z,
        y: y as i16,
        type_id,
        nbt,
    }
}

/// Whether the canonical block at `state` is one of the sign block types —
/// checked against the resolved 26.2 block name rather than this era's own
/// `id` string, which is spelled differently release to release (a bare
/// `"Sign"` pre-1.11, `"minecraft:sign"` after, with no wood-species split
/// until 1.14 added coloured signs).
fn is_sign_state(state: u32) -> bool {
    block_states::block_name(state).is_some_and(|name| name.ends_with("_sign"))
}

/// Reshapes a legacy sign's flat `Text1`..`Text4` (each a JSON chat
/// component — the same pre-1.20 wire form [`Text::from_json`] already
/// parses for chat and disconnect reasons) into the `front_text`/`messages`
/// compound `lodestone_world::SignText::parse` reads. Only the line content
/// survives: a legacy sign carries no per-run styling this reconstructs,
/// only a whole-side `Color`/`GlowingText`, which are carried straight
/// across.
fn legacy_sign_nbt(nbt: &Nbt) -> Nbt {
    let Nbt::Compound(fields) = nbt else {
        return nbt.clone();
    };
    let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let messages = ["Text1", "Text2", "Text3", "Text4"]
        .into_iter()
        .map(|key| {
            let json = match field(key) {
                Some(Nbt::String(s)) => s.as_str(),
                _ => "",
            };
            Nbt::String(Text::from_json(json).to_plain_string())
        })
        .collect();
    let color = match field("Color") {
        Some(Nbt::String(s)) => s.clone(),
        _ => "black".to_owned(),
    };
    let glowing = matches!(field("GlowingText"), Some(Nbt::Byte(b)) if *b != 0);
    Nbt::Compound(vec![(
        "front_text".to_owned(),
        Nbt::Compound(vec![
            (
                "messages".to_owned(),
                Nbt::List {
                    element_type: NbtTag::String,
                    elements: messages,
                },
            ),
            ("color".to_owned(), Nbt::String(color)),
            ("has_glowing_text".to_owned(), Nbt::Byte(i8::from(glowing))),
        ]),
    )])
}
