//! Version-specific framing for this era's chunk packets
//! `minecraft:map_chunk`, `minecraft:update_light` and
//! `minecraft:unload_chunk`, across protocols 498, 578 and 754.
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! `minecraft-data` models only the **outer** framing of `map_chunk` and leaves
//! the section geometry opaque: in
//! `vendor/minecraft-data/data/pc/1.16.2/protocol.json` the payload is `i32 x`,
//! `i32 z`, `bool groundUp`, `varint bitMap`, `nbt heightmaps`, a biomes array
//! present only on full columns, a `varint`-length `chunkData` **buffer**, then a
//! `varint`-counted `blockEntities` array of raw `nbt`. The bytes *inside*
//! `chunkData` have no declarative schema, so the layouts below were settled
//! against real captured server bytes (`tests/captures/`), which is also the
//! only reason the 498 biome placement below is right: `minecraft-data`
//! models 1.14.4's `map_chunk` with **no biome field at all**, and a decoder
//! that believes it leaves 1,024 bytes of the buffer unread.
//!
//! # Where the three protocols differ
//!
//! Three framing differences, each of which desynchronises rather than
//! errors if taken from the wrong protocol:
//!
//! * **Where the biomes are.** At 498 a full column's biomes are a 2-D
//!   `16x16` array of big-endian `i32`s **inside** `chunkData`, after the last
//!   section. At 578 they left the buffer and became a bare 1,024-entry
//!   (4x4x4 over the column) `i32` array *before* it, with no count. At 754
//!   that array gained a VarInt length prefix and VarInt elements.
//! * **How the section indices are packed.** 498 and 578 use the pre-1.16
//!   *straddling* layout, where a value may cross a 64-bit boundary; 754 pads
//!   each long. The two disagree about the long count for every width that
//!   is not a divisor of 64, so this is caught rather than silently
//!   misdecoded — but only because the count is checked.
//! * **`update_light`'s leading `trustEdges` flag**, added at 754. One byte,
//!   before four VarInt masks.
//!
//! # How 1.16.5 differs from the pre-1.13 families (v1-8/v1-9)
//!
//! * **Post-flattening, flat state ids — but not *26.2*'s flat state ids.**
//!   A palette entry is a single flat block-state id, not the legacy
//!   `(blockId << 4) | meta`, but it is still **1.16.5's own** global-palette
//!   numbering: 26.2 has inserted thousands of blocks since, so the same
//!   numeric id now names a different block. Every value decoded by
//!   [`PalettedContainer::decode`] is translated through
//!   [`crate::canonical::resolve_or_air`] before it reaches
//!   [`PalettedContainer::from_values`] — see that module's docs for why and
//!   `tests/canonicalisation.rs` for the generated mapping's provenance.
//! * **Non-straddling (padded) long packing.** 1.16 packs each section's index
//!   array so a value **never** spans two 64-bit longs; unused high bits of each
//!   long are padding. This is exactly what [`PalettedContainer::decode`]
//!   implements, so — unlike v1-8/v1-9, which hand-unpack the old straddling
//!   layout — this crate calls it directly, selecting
//!   [`LongArrayFraming::Prefixed`] (the array is preceded by a VarInt long
//!   count, as every family ≤ 1.21.4 is).
//! * **Heightmaps as NBT.** A `MOTION_BLOCKING` (and, for full columns,
//!   `WORLD_SURFACE`) long-array heightmap travels as an inline NBT compound
//!   before the sections. It is consumed here (the world store recomputes
//!   heightmaps lazily) to keep the zero-trailing-bytes detector meaningful.
//! * **3-D biomes, from 1.15.** Full columns at 578 and 754 carry a flat
//!   array of **1024** biome ids (4×4×4 cells over the whole 256-tall column
//!   = 16 sections × 64 cells). These are real 3-D biomes, so no fabrication
//!   is needed. 498 is the pre-1.15 2-D case and *does* need the same
//!   down-sampling seam v1-8/v1-9 document: 256 values, one per column, each
//!   replicated up the whole height.
//! * **Light is gone.** 1.14 split light out of `map_chunk` into the separate
//!   `update_light` packet ([`UpdateLight`]), so a section here is just
//!   `[blockCount: i16, PalettedContainer]` with **no** inline block/sky light.
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
    BlockEntity, ChunkColumn, ChunkSection, ColumnLight, LightPatch, LongArrayFraming, NibbleArray,
    PaletteKind, PalettedContainer, Result,
};

use crate::canonical::{CanonicalTable, FallbackTally};

/// Number of block sections in a 1.16.5 column (fixed world height 0..256).
const SECTION_COUNT: usize = 16;
/// Biome cells per section (4×4×4).
const BIOME_CELLS_PER_SECTION: usize = 64;
/// Block-state cells per section (16×16×16).
const BLOCK_ENTRIES: usize = 4096;
/// Entries in a 1.15+ 3-D column biome array (4×4×4 cells over 16 sections).
const THREE_D_BIOME_CELLS: usize = 1024;
/// Entries in a 1.14 2-D column biome array (one per 16×16 column).
const TWO_D_BIOME_CELLS: usize = 256;
/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;

/// Dimension shape needed to decode one protocol's chunk column.
///
/// Columns in this era are always 16 sections tall with `min_y = 0`. Unlike
/// the pre-1.14 families, sky-light presence is **not** part of chunk
/// decoding — light travels in the separate `update_light` packet — so this
/// carries the palette configuration, the air/default ids, and the two
/// things that make the framing protocol-specific: the negotiated
/// [`protocol`](Self::protocol) and that protocol's own block-state table.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// The negotiated protocol. Selects the biome placement and the section
    /// index packing; never inferred from anything else here.
    pub protocol: i32,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the 3-D biome containers.
    pub biome_kind: PaletteKind,
    /// This protocol's wire-state -> canonical 26.2 table. Held here rather
    /// than looked up per call so a decode cannot reach a neighbouring
    /// protocol's numbering — see [`crate::canonical`]'s module docs for
    /// what that would look like (a lantern rendering as a bell).
    pub canonical: &'static CanonicalTable,
    /// Block-state id treated as air — the **canonical 26.2** air id from
    /// [`Self::canonical`], not the wire's own flat state 0. Every block this
    /// crate stores has already been translated by
    /// [`CanonicalTable::resolve_or_air`] by the time it reaches a
    /// [`PalettedContainer`] (see [`decode_sections`]), so this must match
    /// that id space, not the wire's.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The overworld shape for `protocol`: 16 sections, `min_y = 0`, flat
    /// state ids.
    ///
    /// The long-array framing is `Prefixed` for every protocol here (each
    /// section's index array is preceded by a VarInt long count, as every
    /// family <= 1.21.4 is). That is *not* the same axis as the straddling
    /// difference: 498 and 578 prefix a count **and** straddle, which is why
    /// [`decode_sections`] hand-unpacks them instead of calling
    /// [`PalettedContainer::decode`].
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
            block_kind: PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed),
            biome_kind: PaletteKind::biomes().with_framing(LongArrayFraming::Prefixed),
            canonical,
            air_id: canonical.air_state_id(),
            biome_id: 0,
        }
    }

    /// A dimension without sky light (nether/end). Since no protocol here
    /// carries light in `map_chunk`, this is identical to
    /// [`ChunkShape::overworld`]; the distinction is kept only so the
    /// adapter's dimension bookkeeping reads naturally.
    ///
    /// # Panics
    ///
    /// Panics for a protocol outside [`crate::PROTOCOLS`].
    #[must_use]
    pub fn no_skylight(protocol: i32) -> Self {
        Self::overworld(protocol)
    }

    /// Whether this protocol carries a full column's biomes as a separate
    /// field before `chunkData` (578, 754) rather than inside it (498).
    const fn biomes_precede_sections(self) -> bool {
        self.protocol >= crate::adapter::PROTOCOL_1_15_2
    }

    /// Whether this protocol packs section indices so a value never crosses a
    /// 64-bit boundary (754) rather than the pre-1.16 straddling layout
    /// (498, 578).
    const fn padded_long_packing(self) -> bool {
        self.protocol >= crate::adapter::PROTOCOL_1_16_5
    }
}

/// A decoded chunk column: block and biome sections.
///
/// No protocol here carries light in the chunk packet (it arrives via
/// `update_light`), so `light` is always the empty column light.
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
    /// Empty column light (light travels separately from 1.14 on).
    pub light: ColumnLight,
    /// Block entities within the column — see the module docs for how a
    /// legacy compound (own `x`/`y`/`z`/`id`, no wire header) becomes one of
    /// these.
    pub block_entities: Vec<BlockEntity>,
    /// How many blocks in this column had a wire state id outside the source
    /// protocol's own state range while bridging to a canonical 26.2 state —
    /// see [`CanonicalTable::resolve_or_air`]. Zero for every real-world column;
    /// surfaced here (and logged, see [`MapChunk::decode`]) rather than
    /// silently absorbed so a wrong mapping stays traceable per CLAUDE.md's
    /// evidence standards.
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
    /// Consumes the entire packet — outer framing, heightmap NBT, biome array,
    /// the length-prefixed `chunkData` buffer (validated to zero trailing bytes
    /// on its own), and the trailing block-entity NBT list. The caller should
    /// still invoke [`Reader::ensure_empty`] on the outer reader: zero trailing
    /// bytes across the whole packet is the single best detector of a subtly
    /// wrong layout.
    ///
    /// # Errors
    ///
    /// Returns an error on malformed input: a truncated buffer, a declared long
    /// count that disagrees with the (non-straddling) section geometry, a
    /// palette index that escapes its palette, or a negative length. The data
    /// comes from a network socket, so every framing decision validates rather
    /// than trusting the sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        let ground_up = r.bool()?;
        let bitmask =
            u32::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;

        // Heightmaps: an inline named NBT compound. Consumed — the world store
        // recomputes heightmaps lazily and there is no consumer for the raw tag.
        read_named_nbt(r)?;

        // Biomes, when this protocol puts them before `chunkData` (578, 754).
        // 578 sends a bare fixed 1024-entry array of big-endian `i32`s with
        // no count at all; 754 gave it a VarInt count and VarInt elements.
        // Partial updates carry no biomes in either.
        let mut biomes = if shape.biomes_precede_sections() && ground_up {
            Some(read_column_biomes(r, shape.protocol)?)
        } else {
            None
        };

        // chunkData: a VarInt-length buffer holding the present sections.
        let raw_len = r.var_i32()?;
        let blob_len =
            usize::try_from(raw_len).map_err(|_| lodestone_core::Error::NegativeLength(raw_len))?;
        let mut blob = r.take_reader(blob_len)?;
        let mut fallback = FallbackTally::default();
        let sections = read_section_values(&mut blob, shape, bitmask, &mut fallback)?;

        // At 498 the biomes are the *tail of the buffer*, not a field before
        // it: a full column ends with a 2-D 16x16 array of big-endian `i32`s,
        // one per column, which the 4x4x4 container fabricates a vertical
        // dimension for (the same seam v1-8 and v1-9 document). Reading them
        // anywhere else leaves exactly 1,024 bytes unconsumed, which is what
        // the `ensure_empty` below turns into an error instead of a chunk.
        if !shape.biomes_precede_sections() && ground_up {
            biomes = Some(read_flat_biomes(&mut blob, TWO_D_BIOME_CELLS)?);
        }

        // The declared chunkData length must exactly match the section
        // geometry (plus, at 498, the biome tail); any slack is a misparse.
        blob.ensure_empty()?;

        let column = build_column(shape, &sections, biomes.as_deref())?;

        // Block entities trail as full named-NBT compounds, each carrying
        // its own `x`/`y`/`z` and a string `id` rather than the wire header
        // the modern compact record has.
        let block_entity_count = r.var_i32()?;
        if block_entity_count < 0 {
            return Err(lodestone_core::Error::NegativeLength(block_entity_count).into());
        }
        let mut block_entities = Vec::with_capacity(block_entity_count.clamp(0, 4096) as usize);
        for _ in 0..block_entity_count {
            let (_name, nbt) = read_named_nbt(r)?;
            if let Some(entity) = block_entity_from_embedded_nbt(nbt, &column) {
                block_entities.push(entity);
            }
        }

        if !fallback.is_empty() {
            tracing::warn!(
                target: "v1-14::chunk",
                x,
                z,
                protocol = shape.protocol,
                out_of_range = fallback.out_of_range,
                "substituted air for {} block(s) whose wire state id could not be \
                 resolved to a canonical 26.2 state",
                fallback.out_of_range,
            );
        }

        Ok(ChunkData {
            x,
            z,
            ground_up,
            column,
            light: ColumnLight::new(SECTION_COUNT),
            block_entities,
            fallback,
        })
    }
}

/// Reads a full column's biome array from *before* `chunkData`, in whichever
/// of the two pre-`chunkData` forms `protocol` uses.
///
/// 578 writes a bare `[i32; 1024]` — no count, so a decoder that expects one
/// consumes the first biome as a length and runs off the end. 754 writes a
/// VarInt count followed by that many VarInts.
fn read_column_biomes(r: &mut Reader<'_>, protocol: i32) -> Result<Vec<u32>> {
    if protocol >= crate::adapter::PROTOCOL_1_16_5 {
        let count = r.var_i32()?;
        let count =
            usize::try_from(count).map_err(|_| lodestone_core::Error::NegativeLength(count))?;
        let mut all = Vec::with_capacity(count);
        for _ in 0..count {
            all.push(u32::try_from(r.var_i32()?).unwrap_or(0));
        }
        Ok(all)
    } else {
        read_flat_biomes(r, THREE_D_BIOME_CELLS)
    }
}

/// Reads `count` big-endian `i32` biome ids with no length prefix.
fn read_flat_biomes(r: &mut Reader<'_>, count: usize) -> Result<Vec<u32>> {
    let mut all = Vec::with_capacity(count);
    for _ in 0..count {
        all.push(u32::try_from(r.i32()?).unwrap_or(0));
    }
    Ok(all)
}

/// Decodes the present sections' block-state values out of the `chunkData`
/// buffer, already translated into the canonical 26.2 id space.
///
/// Returns one `(section index, 4096 canonical state ids)` pair per present
/// section, rather than a built column, so [`MapChunk::decode`] can consume
/// the 498 biome tail that follows them **inside the same buffer** before
/// assembling anything.
///
/// Each section is `[blockCount: i16, bitsPerBlock: u8, palette, longs]`.
/// `blockCount` is advisory (the container carries the authoritative
/// contents) but is present in every protocol here — 1.14 added it — and must
/// be consumed.
fn read_section_values(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    bitmask: u32,
    fallback: &mut FallbackTally,
) -> Result<Vec<(usize, Vec<u32>)>> {
    let mut out = Vec::new();
    for index in 0..SECTION_COUNT {
        if bitmask & (1 << index) == 0 {
            continue;
        }
        // Non-air block count: advisory, but consumed so the geometry lines up.
        let _block_count = blob.i16()?;
        let raw_blocks: Vec<u32> = if shape.padded_long_packing() {
            // 754's packing is exactly what `PalettedContainer::decode`
            // implements, header and all.
            PalettedContainer::decode(shape.block_kind, blob)?
                .iter()
                .collect()
        } else {
            decode_straddling_section(blob)?
        };
        // Translate every cell into the canonical 26.2 space before it
        // reaches version-free storage: per cell rather than per palette
        // entry, the same tradeoff `lodestone_v1_8`'s chunk decode documents
        // — `resolve_or_air` is a plain array index, not a hot-path problem,
        // and per-cell is what makes the tally count *blocks* substituted.
        let translated: Vec<u32> = raw_blocks
            .iter()
            .map(|&state_id| shape.canonical.resolve_or_air(state_id, fallback))
            .collect();
        out.push((index, translated));
    }
    Ok(out)
}

/// Decodes one pre-1.16 (498/578) section body into 4096 raw wire state ids.
///
/// `[bitsPerBlock: u8][paletteLen: varint][palette: varint*][longCount:
/// varint][longs: i64*]`, with the indices packed so a value **may** cross a
/// 64-bit boundary. A `paletteLen` of zero means the direct/global palette:
/// the indices are wire state ids themselves.
///
/// The declared long count is checked against the straddling geometry rather
/// than trusted. That check is the whole reason a 754 column fed to this
/// decoder fails loudly: for the four-bit width a flat world uses, padded
/// packing declares 256 longs and straddling geometry wants 256 as well — but
/// for the five-bit width the first non-flat column reaches, padded declares
/// 342 and straddling 320.
fn decode_straddling_section(blob: &mut Reader<'_>) -> Result<Vec<u32>> {
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
        *out = if palette.is_empty() {
            raw
        } else {
            *palette.get(raw as usize).ok_or_else(|| {
                lodestone_world::WorldError::from(lodestone_core::Error::Custom(format!(
                    "palette index {raw} escapes palette of {}",
                    palette.len()
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
/// This is the crux of why [`PalettedContainer::decode`] cannot serve 498 and
/// 578: it implements only the 1.16+ padded layout where entries never
/// straddle.
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

/// Assembles the decoded sections and the column's biome array into
/// version-free storage.
fn build_column(
    shape: &ChunkShape,
    sections: &[(usize, Vec<u32>)],
    biomes: Option<&[u32]>,
) -> Result<ChunkColumn> {
    let mut column = ChunkColumn::new(
        0,
        SECTION_COUNT,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    for (index, values) in sections {
        let blocks = PalettedContainer::from_values(shape.block_kind, values);
        let biome_container = match biomes {
            Some(all) => {
                PalettedContainer::from_values(shape.biome_kind, &section_biomes(all, *index))
            }
            None => PalettedContainer::new(shape.biome_kind, shape.biome_id),
        };
        let section = ChunkSection::from_containers(blocks, biome_container, shape.air_id);
        if !section.is_empty(shape.biome_id) {
            column.set_section(*index, Some(section));
        }
    }

    Ok(column)
}

/// Extracts one section's 64 biome cells from the column's biome array.
///
/// For a 3-D array (578, 754) the wire index for a cell at 4×4×4-cell
/// coordinates `(cx, cz, cy_global)` is `cy_global * 16 + cz * 4 + cx`; the
/// container's local index is `(cy_local << 4) | (cz << 2) | cx` with
/// `cy_global = section * 4 + cy_local`.
///
/// For the 2-D array 498 sends there is no vertical dimension at all, so
/// every Y layer of every section reads the same XZ cell — the lossy
/// fabrication the pre-1.15 biome seam forces, identical to v1-8's and
/// v1-9's. The two cases are told apart by the array's own length, which is
/// 1024 or 256 and nothing else.
fn section_biomes(all: &[u32], section: usize) -> Vec<u32> {
    let mut cells = vec![0u32; BIOME_CELLS_PER_SECTION];
    let two_d = all.len() == TWO_D_BIOME_CELLS;
    for cy_local in 0..4 {
        let cy_global = section * 4 + cy_local;
        for cz in 0..4 {
            for cx in 0..4 {
                let wire = if two_d {
                    // One value per column: the block at the cell's corner.
                    (cz * 4) * 16 + cx * 4
                } else {
                    cy_global * 16 + cz * 4 + cx
                };
                let local = (cy_local << 4) | (cz << 2) | cx;
                cells[local] = all.get(wire).copied().unwrap_or(0);
            }
        }
    }
    cells
}

/// The `minecraft:update_light` packet (clientbound play), added in 1.14 when
/// light left `map_chunk` — so it exists in every protocol of this era, and
/// this era is the one that introduced it.
///
/// A thin [`Packet`] marker: decoding is hand-written via [`UpdateLight::decode`]
/// because `minecraft-data` models the light arrays as an opaque `restBuffer`.
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
    /// # 1.16 wire shape
    ///
    /// `varint chunkX`, `varint chunkZ`, then — **754 only** — `bool
    /// trustEdges`, then four **VarInt** masks (sky, block, empty-sky,
    /// empty-block; every protocol here uses single-VarInt masks, not the
    /// 1.17 BitSet long arrays), followed by the present sky-light arrays
    /// (each a `varint`-length-prefixed 2048-byte nibble array, in ascending
    /// set-bit order) and then the present block-light arrays.
    ///
    /// The `trustEdges` flag is the only difference across this era, and it
    /// is the reason this takes a `protocol` rather than being version-free:
    /// reading a byte that is not there shifts all four masks, and a mask is
    /// what decides how many 2048-byte arrays follow.
    ///
    /// # Errors
    ///
    /// Returns an error on truncated input or a light array whose declared
    /// length is not 2048 bytes.
    pub fn decode(r: &mut Reader<'_>, protocol: i32) -> Result<LightUpdate> {
        let x = r.var_i32()?;
        let z = r.var_i32()?;
        if protocol >= crate::adapter::PROTOCOL_1_16_5 {
            let _trust_edges = r.bool()?;
        }
        let sky_mask = mask_bits(r.var_i32()?);
        let block_mask = mask_bits(r.var_i32()?);
        let empty_sky_mask = mask_bits(r.var_i32()?);
        let empty_block_mask = mask_bits(r.var_i32()?);

        let sky = read_light_arrays(r, sky_mask)?;
        let block = read_light_arrays(r, block_mask)?;

        let patch = LightPatch::from_light_masks(
            &[u64::from(sky_mask)],
            &[u64::from(empty_sky_mask)],
            sky,
            &[u64::from(block_mask)],
            &[u64::from(empty_block_mask)],
            block,
        );

        Ok(LightUpdate { x, z, patch })
    }
}

/// Reinterprets a VarInt-decoded mask (which may have its sign bit set for the
/// 18th section index) as an unsigned bitset.
fn mask_bits(mask: i32) -> u32 {
    mask as u32
}

/// Reads one nibble-array per set bit of `mask`, ascending. Each is a
/// `varint`-length-prefixed 2048-byte block.
fn read_light_arrays(r: &mut Reader<'_>, mask: u32) -> Result<Vec<NibbleArray>> {
    let mut out = Vec::new();
    for bit in 0..32 {
        if mask & (1 << bit) == 0 {
            continue;
        }
        let raw_len = r.var_i32()?;
        let len =
            usize::try_from(raw_len).map_err(|_| lodestone_core::Error::NegativeLength(raw_len))?;
        if len != LIGHT_BYTES {
            return Err(lodestone_core::Error::Custom(format!(
                "update_light array length {len} != {LIGHT_BYTES}"
            ))
            .into());
        }
        let bytes = r.bytes(LIGHT_BYTES)?;
        out.push(NibbleArray::from_bytes(bytes)?);
    }
    Ok(out)
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
