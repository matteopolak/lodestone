//! Chunk-column framing for the protocol 5 packets `minecraft:map_chunk`
//! (id 33) and `minecraft:map_chunk_bulk` (id 38).
//!
//! # This is the module where the era is genuinely different
//!
//! Two things here have no counterpart in any later protocol, and neither is
//! a variation on the 1.8 layout.
//!
//! ## 1. The payload is a zlib stream inside the packet body
//!
//! Whole-connection compression does not exist at protocol 5 — the login
//! state carries no compression-threshold packet at all — so the chunk
//! packets compress themselves. `map_chunk` carries an `i32` byte count
//! followed by that many bytes of zlib-deflate; `map_chunk_bulk` carries one
//! stream covering *all* of its bundled columns. Nothing else on this wire is
//! compressed.
//!
//! Measured on a real 1.7.10 server: a five-column bulk packet's stream began
//! `78 9c` (zlib, default compression) and inflated from 167 bytes to 52,480.
//!
//! ## 2. Block ids and block metadata arrive in separate arrays
//!
//! Protocol 47 packs one 16-bit `(id << 4) | meta` value per block. Protocol
//! 5 splits that across up to three arrays per column, grouped by *type*
//! across all present sections, in this order:
//!
//! 1. **Block types** — one byte per block, 4096 per present section.
//! 2. **Block metadata** — a 2048-byte nibble array per present section.
//! 3. **Block light** — a 2048-byte nibble array per present section.
//! 4. **Sky light** — a 2048-byte nibble array per present section, present
//!    only in dimensions that have sky light. `map_chunk` cannot tell from its
//!    own bytes whether they are there, so the caller supplies it from the
//!    dimension ([`ChunkShape::has_skylight`]); `map_chunk_bulk` carries a
//!    `skyLightSent` flag that wins.
//! 5. **Add** — a 2048-byte nibble array per section whose bit is set in the
//!    *second* bitmask, supplying bits 8..12 of the block id. This is how the
//!    era addresses block ids above 255, and it has no equivalent in any
//!    later format because the Flattening removed numeric ids entirely.
//!    Vanilla never sets it, but a modded server does, so it is decoded
//!    rather than assumed absent.
//! 6. **Biomes** — 256 bytes, one per 16×16 XZ column, on full columns only.
//!
//! The composite this module assembles is `(((add << 8) | type) << 4) | meta`,
//! which is the same pre-Flattening space [`lodestone_canonical`] already
//! translates for the 1.8 and 1.9 eras. That is the one substantial piece of
//! work this era did **not** have to repeat.
//!
//! ### Two layout facts that were measured rather than assumed
//!
//! Both had a plausible alternative that a length check cannot distinguish,
//! so each was pinned against a real server with an input on which the two
//! hypotheses disagree.
//!
//! - **Arrays are grouped per column, not per array type across columns.** A
//!   bulk packet's stream is column-after-column, each column complete with
//!   its own biome footer. Both groupings produce a byte-identical total
//!   length, so the discriminator was the biome footer's *position*: at the
//!   per-column offset it reads as 256 bytes of biome id 1 (plains), which
//!   under the other grouping is light data.
//! - **Even block indices occupy the low nibble.** Four wool blocks were
//!   placed at adjacent x with metadata 14, 1, 5 and 11 — chosen so that no
//!   value equals its byte-partner and the two orderings give different
//!   answers — and read back out of a fresh chunk. Even index → low nibble,
//!   odd → high nibble; the reverse hypothesis was ruled out on all four.
//!   This is the convention [`NibbleArray::get`] already implements, so the
//!   nibble arrays are read through it rather than by hand.
//!
//! # The biome seam, unchanged from the 1.8 era
//!
//! Biomes here are 2-D: one byte per XZ column, constant over y. The
//! version-free [`ChunkSection`] stores a 3-D 4×4×4 biome container, so this
//! module down-samples the 16×16 map to 4×4 and replicates it over every y
//! layer — discarding 15/16 of the horizontal resolution the server sent and
//! inventing vertical structure it never had. The footer is always fully
//! consumed regardless, because leaving it would fail the zero-trailing-bytes
//! check that is this module's best detector of a wrong layout.
//!
//! # Chunk unload, and the only thing `map_chunk` is actually used for
//!
//! There is no separate unload packet. A `map_chunk` whose primary bitmask is
//! zero, with `groundUp` set, is the unload signal. It is **not** an empty
//! payload: the 12-byte zlib stream a real server sends inflates to 256 bytes
//! of biome footer, because `groundUp` implies a footer whether or not any
//! section is present. [`ChunkData::is_unload`] answers for the condition so
//! the adapter does not have to re-derive it.
//!
//! Measured alongside it: a vanilla server on a flat overworld sends
//! single-column `map_chunk` packets for **nothing else**. Walking 320 blocks
//! produced 20 unloads and not one data-bearing `map_chunk`; every column that
//! was loaded, on join and while travelling, arrived in a `map_chunk_bulk`.
//! The single-column loading path is still decoded — a non-vanilla server may
//! use it, and a partial update (`groundUp` false) has no other framing — but
//! no vanilla capture will exercise it, which is why `tests/chunk.rs` builds
//! that case by hand and pins the unload case against recorded bytes.

use flate2::bufread::ZlibDecoder;
use lodestone_canonical::canonical::{self, FallbackTally};
use lodestone_core::{Error, Reader};
use lodestone_macros::Packet;
use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, LightData, NibbleArray, PaletteKind, PalettedContainer,
    Result,
};
use std::io::Read as _;

/// Sections in a protocol 5 column: world height is a fixed `0..256`.
const SECTION_COUNT: usize = 16;
/// Entries in one section (16³).
const BLOCK_ENTRIES: usize = 4096;
/// Bytes for one section's block-type array (one byte per block).
const TYPE_BYTES: usize = BLOCK_ENTRIES;
/// Bytes for one nibble array (4096 nibbles, two per byte).
const NIBBLE_BYTES: usize = 2048;
/// Bytes in the 2-D biome footer (16×16, one byte per column).
const BIOME_BYTES: usize = 256;
/// Ceiling on one inflated column blob, as a guard against a hostile stream.
///
/// A full 16-section column with sky light and a complete add array is
/// `16 * (4096 + 2048 + 2048 + 2048 + 2048) + 256` = 199,936 bytes. The cap is
/// that, times the largest column count a bulk packet's `i16` can express,
/// rounded up.
const MAX_INFLATED: usize = 199_936 * 64;

/// Dimension shape needed to decode a protocol 5 column.
///
/// Columns are always 16 sections tall starting at y 0, so unlike a modern
/// chunk shape this carries no height parameters. The one thing `map_chunk`
/// cannot tell us from its own bytes is whether sky light is present.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// Whether the dimension carries sky light (true for the overworld).
    pub has_skylight: bool,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the fabricated biome containers.
    pub biome_kind: PaletteKind,
    /// Canonical block-state id treated as air. Every wire value has already
    /// been translated by the time it reaches a container, so emptiness is
    /// judged in canonical space, never in the legacy composite space.
    pub air_id: u32,
    /// Default biome id for columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The overworld: 16 sections from y 0, sky light present.
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

    /// A dimension without sky light (nether or end), otherwise identical.
    #[must_use]
    pub fn no_skylight() -> Self {
        Self {
            has_skylight: false,
            ..Self::overworld()
        }
    }
}

/// A decoded protocol 5 chunk column.
///
/// There are no heightmaps and no block-entity list in this era's chunk
/// packets, so this carries only what the format actually provides.
#[derive(Debug, Clone)]
pub struct ChunkData {
    /// Column x, in chunks.
    pub x: i32,
    /// Column z, in chunks.
    pub z: i32,
    /// Whether this was a full column (carries biomes; absent sections are
    /// air) rather than a partial update.
    pub ground_up: bool,
    /// Whether any section was present at all.
    pub had_sections: bool,
    /// Block-state and biome sections.
    pub column: ChunkColumn,
    /// Sky and block light.
    pub light: ColumnLight,
    /// How many blocks used the add array's high id bits. Zero against
    /// vanilla; non-zero says the column came from a server with block ids
    /// above 255, and that the canonical translation below is on much thinner
    /// ice than usual.
    pub extended_ids: usize,
    /// Wire values that could not be resolved to a canonical block state and
    /// became a counted air substitution.
    pub fallback: FallbackTally,
}

impl ChunkData {
    /// Whether this column is the era's chunk-unload signal: a full column
    /// with no sections present.
    #[must_use]
    pub const fn is_unload(&self) -> bool {
        self.ground_up && !self.had_sections
    }
}

/// The `minecraft:map_chunk` packet (clientbound play, id 33).
///
/// A [`Packet`] marker only: the id, name, state and bound come from the
/// derive, and decoding is hand-written because the payload is a compressed
/// blob whose internal layout no schema describes.
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk", state = Play, bound = Client)]
pub struct MapChunk;

/// The `minecraft:map_chunk_bulk` packet (clientbound play, id 38).
///
/// Bundles several full columns behind one zlib stream.
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk_bulk", state = Play, bound = Client)]
pub struct MapChunkBulk;

impl MapChunk {
    /// Decodes a `map_chunk` body into one [`ChunkData`].
    ///
    /// # Errors
    ///
    /// Returns an error on a truncated buffer, an inflate failure, or an
    /// inflated length that disagrees with the geometry the two bitmasks
    /// imply. The bytes come off a socket, so every framing decision is
    /// validated rather than trusted.
    pub fn decode(reader: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = reader.i32()?;
        let z = reader.i32()?;
        let ground_up = reader.bool()?;
        let primary = reader.u16()?;
        let add = reader.u16()?;
        let compressed_len = reader.i32()?;
        let compressed_len = usize::try_from(compressed_len)
            .map_err(|_| Error::NegativeLength(compressed_len))?;
        let compressed = reader.bytes(compressed_len)?;

        let expected = column_bytes(shape.has_skylight, primary, add, ground_up);
        let inflated = inflate(compressed, expected)?;
        let mut blob = Reader::new(&inflated);
        let data = decode_column(&mut blob, shape, x, z, ground_up, primary, add)?;
        // Zero trailing bytes across the whole inflated blob is the single
        // best detector of a subtly wrong layout: a misparse almost always
        // leaves the buffer misaligned even when every individual read
        // succeeded.
        blob.ensure_empty()?;
        report_column(&data);
        Ok(data)
    }
}

impl MapChunkBulk {
    /// Decodes a `map_chunk_bulk` body into one [`ChunkData`] per column.
    ///
    /// # The field order is not the 1.8 one
    ///
    /// Protocol 5 puts the column count and the compressed length *before*
    /// the payload and the per-column metadata *after* it; protocol 47 moved
    /// the metadata in front of the payload. Both orders parse a
    /// single-column packet without erroring, so this order is the one read
    /// off a real server rather than carried over from the neighbour.
    ///
    /// Every bundled column is a full column, so all of them carry biomes.
    ///
    /// # Errors
    ///
    /// Returns an error on truncation, an inflate failure, or any column
    /// whose geometry does not line up with the shared blob.
    pub fn decode(reader: &mut Reader<'_>, shape: &ChunkShape) -> Result<Vec<ChunkData>> {
        let column_count = reader.i16()?;
        let column_count =
            usize::try_from(column_count).map_err(|_| Error::NegativeLength(i32::from(column_count)))?;
        let compressed_len = reader.i32()?;
        let compressed_len = usize::try_from(compressed_len)
            .map_err(|_| Error::NegativeLength(compressed_len))?;
        let sky_light_sent = reader.bool()?;
        let compressed = reader.bytes(compressed_len)?;

        let mut metas = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            let x = reader.i32()?;
            let z = reader.i32()?;
            let primary = reader.u16()?;
            let add = reader.u16()?;
            metas.push((x, z, primary, add));
        }

        let expected: usize = metas
            .iter()
            .map(|&(_, _, primary, add)| column_bytes(sky_light_sent, primary, add, true))
            .sum();
        let inflated = inflate(compressed, expected)?;
        let mut blob = Reader::new(&inflated);

        let bulk_shape = ChunkShape {
            has_skylight: sky_light_sent,
            ..*shape
        };
        let mut columns = Vec::with_capacity(column_count);
        for (x, z, primary, add) in metas {
            let data = decode_column(&mut blob, &bulk_shape, x, z, true, primary, add)?;
            report_column(&data);
            columns.push(data);
        }
        blob.ensure_empty()?;
        Ok(columns)
    }
}

/// Bytes one column occupies in the inflated blob, from its two bitmasks.
///
/// This is the prediction the inflate is checked against. It was derived
/// arithmetically and then confirmed against a real five-column bulk packet,
/// which inflated to exactly the predicted 52,480 bytes.
fn column_bytes(has_skylight: bool, primary: u16, add: u16, ground_up: bool) -> usize {
    let sections = primary.count_ones() as usize;
    let add_sections = add.count_ones() as usize;
    sections * (TYPE_BYTES + NIBBLE_BYTES + NIBBLE_BYTES)
        + if has_skylight {
            sections * NIBBLE_BYTES
        } else {
            0
        }
        + add_sections * NIBBLE_BYTES
        + if ground_up { BIOME_BYTES } else { 0 }
}

/// Inflates one zlib stream, refusing anything that does not produce exactly
/// the predicted number of bytes.
///
/// The exact-length check is what makes the geometry falsifiable: an inflate
/// that succeeds but yields a different length means the bitmask arithmetic
/// or the array order is wrong, and that is worth failing on rather than
/// decoding past.
fn inflate(compressed: &[u8], expected: usize) -> Result<Vec<u8>> {
    if expected > MAX_INFLATED {
        return Err(lodestone_world::WorldError::Core(Error::LimitExceeded {
            limit: MAX_INFLATED,
            actual: expected,
        }));
    }
    let mut out = Vec::with_capacity(expected);
    ZlibDecoder::new(compressed)
        .take(expected as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|err| {
            lodestone_world::WorldError::Core(Error::Custom(format!(
                "chunk payload is not a valid zlib stream: {err}"
            )))
        })?;
    if out.len() != expected {
        return Err(lodestone_world::WorldError::Core(Error::Custom(format!(
            "chunk payload inflated to {} bytes, but its bitmasks describe {expected}",
            out.len()
        ))));
    }
    Ok(out)
}

/// Decodes one column out of the inflated blob.
///
/// `blob` is positioned at the start of this column and exactly one column's
/// bytes are consumed, which is what lets a bulk packet read straight through
/// in metadata order with no per-column length prefix.
fn decode_column(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    x: i32,
    z: i32,
    ground_up: bool,
    primary: u16,
    add: u16,
) -> Result<ChunkData> {
    let present: Vec<usize> = (0..SECTION_COUNT)
        .filter(|index| primary & (1 << index) != 0)
        .collect();
    let add_present: Vec<usize> = (0..SECTION_COUNT)
        .filter(|index| add & (1 << index) != 0)
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

    // The arrays are grouped by type across this column's sections, so each
    // loop below drains one whole array before the next begins. Reading them
    // interleaved per section would consume the same total and produce
    // garbage.
    let mut types = Vec::with_capacity(present.len());
    for _ in &present {
        types.push(blob.bytes(TYPE_BYTES)?);
    }
    let mut metadata = Vec::with_capacity(present.len());
    for _ in &present {
        metadata.push(NibbleArray::from_bytes(blob.bytes(NIBBLE_BYTES)?)?);
    }
    for &index in &present {
        let bytes = blob.bytes(NIBBLE_BYTES)?;
        *light.block_mut(light_section(index)) = LightData::Values(NibbleArray::from_bytes(bytes)?);
    }
    if shape.has_skylight {
        for &index in &present {
            let bytes = blob.bytes(NIBBLE_BYTES)?;
            *light.sky_mut(light_section(index)) =
                LightData::Values(NibbleArray::from_bytes(bytes)?);
        }
    }
    // One add array per section named by the *second* bitmask, which is not
    // required to be a subset of the first.
    let mut add_arrays: Vec<(usize, NibbleArray)> = Vec::with_capacity(add_present.len());
    for &index in &add_present {
        add_arrays.push((index, NibbleArray::from_bytes(blob.bytes(NIBBLE_BYTES)?)?));
    }
    let biome_cells = if ground_up {
        Some(downsample_biomes(blob.bytes(BIOME_BYTES)?))
    } else {
        None
    };

    let fallback = &mut FallbackTally::default();
    let mut extended_ids = 0usize;
    for (slot, &index) in present.iter().enumerate() {
        let type_bytes = types[slot];
        let meta = &metadata[slot];
        let high = add_arrays
            .iter()
            .find(|&&(add_index, _)| add_index == index)
            .map(|(_, array)| array);
        let mut values = vec![0u32; BLOCK_ENTRIES];
        for (entry, value) in values.iter_mut().enumerate() {
            let mut id = u32::from(type_bytes[entry]);
            if let Some(array) = high {
                let bits = u32::from(array.get(entry));
                if bits != 0 {
                    id |= bits << 8;
                    extended_ids += 1;
                }
            }
            let composite = (id << 4) | u32::from(meta.get(entry));
            *value = canonical::resolve_composite_or_air(composite, fallback);
        }
        let blocks = PalettedContainer::from_values(shape.block_kind, &values);
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
        had_sections: !present.is_empty(),
        column,
        light,
        extended_ids,
        fallback: *fallback,
    })
}

/// Logs a column's canonicalisation fallbacks and extended-id usage, if any.
///
/// Once per column with a breakdown, not once per block and not never. Silent
/// for the overwhelming majority of columns, which need no fallback at all.
fn report_column(data: &ChunkData) {
    if data.extended_ids != 0 {
        tracing::warn!(
            target: "v1-7::chunk",
            x = data.x,
            z = data.z,
            blocks = data.extended_ids,
            "column used the add array's high id bits; no vanilla server of this era does, so \
             the canonical translation of those blocks is unverified"
        );
    }
    if data.fallback.is_empty() {
        return;
    }
    tracing::warn!(
        target: "v1-7::chunk",
        x = data.x,
        z = data.z,
        no_table_entry = data.fallback.no_table_entry,
        requires_additional_context = data.fallback.requires_additional_context,
        out_of_bounds = data.fallback.out_of_bounds,
        unmapped = data.fallback.unmapped,
        "substituted air for {} block(s) with no canonical block state",
        data.fallback.no_table_entry
            + data.fallback.requires_additional_context
            + data.fallback.out_of_bounds
            + data.fallback.unmapped,
    );
}

/// Maps a block-section index to its light-section index.
///
/// [`ColumnLight`] carries `section_count + 2` light sections, one below and
/// one above the build range, so block section `s` lives at light section
/// `s + 1`.
const fn light_section(block_section: usize) -> usize {
    block_section + 1
}

/// Down-samples the 16×16 2-D biome footer into a 4×4×4 container's ids,
/// replicating the single XZ layer across all four y layers.
///
/// The returned slice is in the biome container's YZX order, and every y
/// layer is identical because biomes in this era have no vertical dimension.
fn downsample_biomes(footer: &[u8]) -> Vec<u32> {
    let mut cells = vec![0u32; 64];
    for y in 0..4 {
        for z in 0..4 {
            for x in 0..4 {
                // Sample the footer at the corner of each 4-wide cell.
                let biome = u32::from(footer[(z * 4) * 16 + (x * 4)]);
                cells[y * 16 + z * 4 + x] = biome;
            }
        }
    }
    cells
}
