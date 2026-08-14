//! Version-specific framing for the 1.8.9 (protocol 47) chunk packets
//! `minecraft:map_chunk` (id 33) and `minecraft:map_chunk_bulk` (id 38).
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! The 1.8 chunk format is not a variant of the modern one — it is a different
//! design, and crucially **`minecraft-data` does not model its internal
//! layout**. In `vendor/minecraft-data/data/pc/1.8/protocol.json` the payload is
//! a single opaque length-prefixed `buffer` (`chunkData`) for `map_chunk` and a
//! bare `restBuffer` for `map_chunk_bulk`; the real structure lives in
//! `prismarine-chunk`, a separate library. So there is no declarative schema to
//! derive from, and the byte geometry below is transcribed from the 1.8 wire
//! spec directly. That is a **judgement call worth flagging**: unlike the packet
//! *ids*, this layout has no authoritative community-data oracle.
//!
//! # The 1.8 layout (a full, "ground-up" column)
//!
//! After `int x`, `int z`, `bool groundUp`, `unsigned short primaryBitMask` and
//! a `varint` byte length, the blob is a set of **flat arrays grouped by type**
//! across the present sections (the bits set in `primaryBitMask`), in this
//! order:
//!
//! 1. **Block data** — for each present section, 4096 entries of a **16-bit
//!    little-endian** value `(blockId << 4) | meta`, and there is no palette on
//!    the wire at all. The little-endianness is a genuine 1.8 trap: Minecraft
//!    shorts are otherwise big-endian, and a big-endian misread scrambles every
//!    id while still consuming the right number of bytes — invisible to a
//!    length check, caught only by the known-block-at-known-Y assertion.
//!
//!    **That packed value is *not* a block-state id.** This module's header
//!    used to claim it "*is* the natural block-state id" and stored it raw in
//!    the [`PalettedContainer`]; that was wrong in a way nothing in this crate
//!    could see, because the container is version-free and accepts any `u32`.
//!    Its consumers — the mesher's atlas and collision — are built from the
//!    **canonical 26.2** `lodestone_data::block_states` space, in which `112`
//!    (1.8's bedrock, `7 << 4`) is a *pumpkin stem*, not bedrock. Every value
//!    is therefore translated through
//!    [`lodestone_canonical::canonical::resolve_or_air`] — the 1.13.2 jar's own
//!    `DataFixerUpper` flattening table plus the rename/property bridge — before
//!    it reaches a container. Unresolvable values become a **counted** air
//!    substitution ([`ChunkData::fallback`]), logged once per column rather than
//!    once per block or not at all. See canonicalisation unit U3.
//! 2. **Block light** — for each present section, a 2048-byte nibble array.
//! 3. **Sky light** — for each present section, a 2048-byte nibble array,
//!    present only in dimensions with sky light (the overworld). `map_chunk`
//!    itself does *not* carry a "sky light sent" flag, so the caller must supply
//!    it from the dimension (see [`ChunkShape::has_skylight`]); `map_chunk_bulk`
//!    carries it explicitly as `skyLightSent`.
//! 4. **Biomes** — 256 bytes (one per 16×16 XZ column), present only when
//!    `groundUp` is set. See the biome seam note below.
//!
//! Section indexing is `y << 8 | z << 4 | x` (YZX), identical to
//! [`lodestone_world`]'s [`PaletteKind::index`] and [`NibbleArray::index`], so
//! the flat arrays drop straight in with no transposition.
//!
//! # The biome seam ([`lodestone_world`] finding)
//!
//! 1.8 biomes are **2-D**: one byte per XZ column, constant over Y. The
//! version-free [`ChunkSection`] instead stores a **3-D** 4×4×4 (64-entry) biome
//! container per section, the modern shape. There is no column-level 2-D biome
//! store to decode into, so this crate must *fabricate* a 3-D container by
//! down-sampling the 16×16 map to 4×4 and replicating it over the four Y layers
//! of every section — discarding 15/16 of the horizontal resolution the server
//! actually sent, and inventing vertical structure the server never had. It
//! decodes and it is lossless enough for a flat test world, but it is an
//! impedance mismatch, and it is reported as the concrete `lodestone-world`
//! seam this task was meant to surface. The 256-byte footer is always fully
//! consumed regardless, because leaving it on the buffer would fail the
//! zero-trailing-bytes detector.
//!
//! # What does *not* leak in
//!
//! None of the modern paletted-wire machinery is touched. This crate never
//! calls [`PalettedContainer::decode`]; it builds containers with
//! [`PalettedContainer::from_values`], so the `LongArrayFraming` knob
//! (`Prefixed` vs `FixedSize`) — a modern packed-long concern — is simply never
//! consulted. 1.8 has no packed long arrays, no heightmaps on the wire, and no
//! separate light packet. The version-free storage types absorbed the legacy
//! format without a third framing case being needed. That is the headline
//! result: the seam held.

use lodestone_canonical::canonical::{self, FallbackTally};
use lodestone_core::Reader;
use lodestone_macros::Packet;
use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, LightData, NibbleArray, PaletteKind, PalettedContainer,
    Result,
};

/// Number of block sections in a 1.8 column (fixed world height 0..256).
const SECTION_COUNT: usize = 16;
/// Entries in one block-state section (16³).
const BLOCK_ENTRIES: usize = 4096;
/// Bytes for one section's block data (4096 little-endian shorts).
const BLOCK_BYTES: usize = BLOCK_ENTRIES * 2;
/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;
/// Bytes in the 2-D biome footer (16×16, one byte per column).
const BIOME_BYTES: usize = 256;

/// Dimension shape needed to decode a 1.8 chunk column.
///
/// 1.8 columns are always 16 sections tall with `min_y = 0`, so unlike the
/// modern [`ChunkShape`](../../../lodestone_v770/packets/chunk/struct.ChunkShape.html)
/// this carries no height parameters. The one thing `map_chunk` cannot tell us
/// from its own bytes is whether sky light is present — that depends on the
/// dimension the join packet announced — so it is supplied here.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// Whether the dimension carries sky light (true for the overworld, false
    /// for the nether/end). Determines whether the sky-light arrays are present
    /// in a `map_chunk`. For `map_chunk_bulk` the wire `skyLightSent` flag wins.
    pub has_skylight: bool,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the (fabricated) biome containers.
    pub biome_kind: PaletteKind,
    /// Block-state id treated as air — the **canonical 26.2**
    /// [`canonical::air_state_id`], not the legacy `(0, 0)` composite value.
    /// Every wire value has already passed through
    /// [`canonical::resolve_or_air`] by the time it reaches a container, so
    /// section-emptiness must be judged in the same space.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The 1.8 overworld: 16 sections, `min_y = 0`, sky light present.
    #[must_use]
    pub fn overworld() -> Self {
        Self {
            has_skylight: true,
            block_kind: PaletteKind::block_states(),
            biome_kind: PaletteKind::biomes(),
            air_id: canonical::air_state_id(),
            biome_id: 0,
        }
    }

    /// A dimension without sky light (nether/end), otherwise identical.
    #[must_use]
    pub fn no_skylight() -> Self {
        Self {
            has_skylight: false,
            ..Self::overworld()
        }
    }
}

/// A decoded 1.8 chunk column: block sections plus inline sky/block light.
///
/// 1.8 has no heightmaps or block-entity list in the chunk packet, so this
/// carries only what the format actually provides.
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
    /// Sky and block light.
    pub light: ColumnLight,
    /// Count of wire values that could not be resolved to a canonical 26.2
    /// state and were substituted with air, broken down by outcome. Zero for
    /// the overwhelming majority of columns; surfaced (rather than silently
    /// swallowed) so a table gap is visible. See
    /// [`canonical::resolve_or_air`].
    pub fallback: FallbackTally,
}

/// The `minecraft:map_chunk` packet (clientbound play, id 33).
///
/// This is a thin [`Packet`] marker: its id/name/state/bound come from the
/// derive, but decoding is hand-written via [`MapChunk::decode`] because the
/// payload layout is not expressible declaratively (see the module docs).
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk", state = Play, bound = Client)]
pub struct MapChunk;

/// The `minecraft:map_chunk_bulk` packet (clientbound play, id 38).
///
/// Bundles several full columns in one packet — a 1.8 construct with no modern
/// equivalent. Decoded via [`MapChunkBulk::decode`].
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk_bulk", state = Play, bound = Client)]
pub struct MapChunkBulk;

impl MapChunk {
    /// Decodes a `map_chunk` body into a [`ChunkData`] given the dimension
    /// [`ChunkShape`].
    ///
    /// The caller should invoke [`Reader::ensure_empty`] afterwards: zero
    /// trailing bytes across the whole packet is the single best detector of a
    /// subtly wrong layout, since a misparse almost always leaves the buffer
    /// misaligned.
    ///
    /// # Errors
    ///
    /// Returns an error on malformed input: a truncated buffer, a `chunkData`
    /// length that disagrees with the section geometry implied by the bitmask,
    /// or a light array of the wrong size. The data comes from a network
    /// socket, so every framing decision validates rather than trusting the
    /// sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        let ground_up = r.bool()?;
        let bitmask = r.u16()?;
        let blob_len =
            usize::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
        let mut blob = r.take_reader(blob_len)?;
        let data = decode_column(&mut blob, shape, x, z, ground_up, bitmask)?;
        // The declared blob length must exactly match the geometry; any slack is
        // a misparse (or a layout assumption that is wrong for this dimension).
        blob.ensure_empty()?;
        report_fallback(&data);
        Ok(data)
    }
}

impl MapChunkBulk {
    /// Decodes a `map_chunk_bulk` body into one [`ChunkData`] per bundled
    /// column.
    ///
    /// The `skyLightSent` flag on the wire overrides
    /// [`ChunkShape::has_skylight`]; every bundled column is a full "ground-up"
    /// column (bulk never carries partial updates). The remaining blob is the
    /// concatenation of all columns' data with no per-column length prefix, so
    /// it is read straight through in metadata order.
    ///
    /// # Errors
    ///
    /// Returns an error on truncation or on any column whose geometry does not
    /// line up with the shared data blob.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<Vec<ChunkData>> {
        let sky_light_sent = r.bool()?;
        let column_count =
            usize::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;

        let mut metas = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            let x = r.i32()?;
            let z = r.i32()?;
            let bitmask = r.u16()?;
            metas.push((x, z, bitmask));
        }

        let bulk_shape = ChunkShape {
            has_skylight: sky_light_sent,
            ..*shape
        };
        let mut columns = Vec::with_capacity(column_count);
        for (x, z, bitmask) in metas {
            // Bulk columns are always full (ground-up) and thus carry biomes.
            let data = decode_column(r, &bulk_shape, x, z, true, bitmask)?;
            report_fallback(&data);
            columns.push(data);
        }
        Ok(columns)
    }
}

/// Decodes one column's worth of flat 1.8 data from `blob` into the version-free
/// storage types.
///
/// `blob` is positioned at the start of this column's data. For `map_chunk` it
/// is a bounded sub-reader over exactly this column; for `map_chunk_bulk` it is
/// the shared blob and this consumes exactly one column's bytes.
fn decode_column(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    x: i32,
    z: i32,
    ground_up: bool,
    bitmask: u16,
) -> Result<ChunkData> {
    let present: Vec<usize> = (0..SECTION_COUNT)
        .filter(|i| bitmask & (1 << i) != 0)
        .collect();

    let fallback = &mut FallbackTally::default();

    let mut column = ChunkColumn::new(
        0,
        SECTION_COUNT,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );
    let mut light = ColumnLight::new(SECTION_COUNT);

    // 1. Block data: 4096 little-endian shorts per present section, each a
    //    legacy `(blockId << 4) | meta` composite — *not* a block-state id.
    //    Translate every one into the canonical 26.2 space the mesher and
    //    collision actually consume (see the module docs).
    //
    //    Unlike v340 there is no palette on the wire to translate once, so
    //    this resolves per cell — deliberately, and it is not a hot path
    //    problem: `resolve` is an index into a lazily-built 4096-entry array
    //    after the first call in the process. Per cell is also what makes the
    //    tally exact, counting *blocks* substituted rather than distinct
    //    values, which is what its log line claims.
    let mut section_blocks: Vec<(usize, PalettedContainer)> = Vec::with_capacity(present.len());
    for &index in &present {
        let raw = blob.bytes(BLOCK_BYTES)?;
        let mut values = vec![0u32; BLOCK_ENTRIES];
        for (i, value) in values.iter_mut().enumerate() {
            let lo = u32::from(raw[2 * i]);
            let hi = u32::from(raw[2 * i + 1]);
            // A 16-bit wire value can name a block id past 255, which no
            // vanilla 1.8 server sends; `resolve_composite_or_air` counts that
            // as a fallback rather than failing the packet, because a single
            // out-of-range cell must not cost the whole column.
            *value = canonical::resolve_composite_or_air(lo | (hi << 8), fallback);
        }
        section_blocks.push((
            index,
            PalettedContainer::from_values(shape.block_kind, &values),
        ));
    }

    // 2. Block light: one 2048-byte nibble array per present section.
    for &index in &present {
        let bytes = blob.bytes(LIGHT_BYTES)?;
        *light.block_mut(light_section(index)) = LightData::Values(NibbleArray::from_bytes(bytes)?);
    }

    // 3. Sky light: one nibble array per present section, overworld only.
    if shape.has_skylight {
        for &index in &present {
            let bytes = blob.bytes(LIGHT_BYTES)?;
            *light.sky_mut(light_section(index)) =
                LightData::Values(NibbleArray::from_bytes(bytes)?);
        }
    }

    // 4. Biome footer: 256 bytes (16×16) on full columns. Down-sampled into a
    //    fabricated per-section 3-D container (see the biome-seam module note).
    let biome_cells = if ground_up {
        Some(downsample_biomes(blob.bytes(BIOME_BYTES)?))
    } else {
        None
    };

    // Assemble sections now that all arrays are consumed. Air-only sections are
    // elided (left as None) so an empty section costs nothing.
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
        fallback: *fallback,
    })
}

/// Logs a column's canonicalisation fallbacks, if any.
///
/// The chosen substitution for every non-`Resolved` outcome is a **visible,
/// counted** air block rather than a silent one: once per column with the
/// breakdown, not once per block and not never. Silent for the overwhelming
/// majority of columns, which need no fallback at all. Mirrors v340's
/// treatment so the two pre-Flattening families report identically.
fn report_fallback(data: &ChunkData) {
    if data.fallback.is_empty() {
        return;
    }
    tracing::warn!(
        target: "v47::chunk",
        x = data.x,
        z = data.z,
        no_table_entry = data.fallback.no_table_entry,
        requires_additional_context = data.fallback.requires_additional_context,
        out_of_bounds = data.fallback.out_of_bounds,
        unmapped = data.fallback.unmapped,
        "substituted air for {} block(s) that could not be resolved to a canonical 26.2 state",
        data.fallback.no_table_entry
            + data.fallback.requires_additional_context
            + data.fallback.out_of_bounds
            + data.fallback.unmapped,
    );
}

/// Maps a block-section index (`0` lowest) to its light-section index.
///
/// [`ColumnLight`] carries `section_count + 2` light sections (one below and one
/// above the build range), with light section `i` covering world block-section
/// `i - 1`, so block section `s` lives at light section `s + 1`.
const fn light_section(block_section: usize) -> usize {
    block_section + 1
}

/// Down-samples the 16×16 2-D biome footer into a 64-entry (4×4×4) container's
/// worth of ids, replicating the single XZ layer across all four Y layers.
///
/// This is the lossy fabrication the biome seam forces: the returned slice is
/// indexed in the biome container's YZX order, but every Y layer is identical
/// because 1.8 biomes have no vertical dimension.
fn downsample_biomes(footer: &[u8]) -> Vec<u32> {
    // 4×4×4 biome grid: one biome per 4×4×4 block cell. Sample the footer at the
    // corner of each 4-wide cell (bx, bz in 0..4 → block 0,4,8,12).
    let mut cells = vec![0u32; 64];
    for y in 0..4 {
        for z in 0..4 {
            for x in 0..4 {
                let block_x = x * 4;
                let block_z = z * 4;
                let footer_index = block_z * 16 + block_x;
                let cell_index = (y << 4) | (z << 2) | x;
                cells[cell_index] = u32::from(footer[footer_index]);
            }
        }
    }
    cells
}
