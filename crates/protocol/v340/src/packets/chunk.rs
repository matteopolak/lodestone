//! Version-specific framing for the 1.12.2 (protocol 340) chunk packets
//! `minecraft:map_chunk` (id 32) and `minecraft:unload_chunk` (id 29).
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! 1.12.2 sits between the two formats already implemented and shares neither.
//! Like 1.8 (`v47`), `minecraft-data` models only the **outer** framing and
//! leaves the section geometry opaque: in
//! `vendor/minecraft-data/data/pc/1.12.2/protocol.json` the payload is
//! `i32 x`, `i32 z`, `bool groundUp`, `varint bitMap`, a `varint`-length
//! `chunkData` **buffer**, then a `varint`-counted `blockEntities` array of raw
//! `nbt`. The bytes *inside* `chunkData` have no declarative schema, so the
//! layout below is transcribed from the 1.9–1.12.2 wire spec directly. That is
//! the same judgement call flagged for v47: the packet *ids* have a community
//! oracle, the inner byte geometry does not.
//!
//! # How 1.12.2 differs from **both** neighbours
//!
//! * **Paletted, not raw.** Unlike 1.8's flat 16-bit `(blockId << 4) | meta`
//!   arrays, each section carries a per-section palette and packed indices. But
//!   unlike modern (v770) it is **pre-flattening**, so a palette entry is still
//!   the legacy `(blockId << 4) | meta` block-state id — the *same* version-free
//!   id space this project's world store uses, and the same one v47 produces.
//! * **Old (straddling) long packing.** The packed index array uses the
//!   pre-1.16 layout where a value **spans** two 64-bit longs when it crosses a
//!   boundary. [`lodestone_world`]'s [`PalettedContainer::decode`] /
//!   [`PackedArray`](lodestone_world) implement only the 1.16+ **non-straddling**
//!   (padded) layout — its `LongArrayFraming` knob toggles the length *prefix*,
//!   not the packing *style*. So, exactly like v47, this crate never calls
//!   `PalettedContainer::decode`; it unpacks the longs itself
//!   ([`unpack_straddling`]) and rebuilds a version-free container with
//!   [`PalettedContainer::from_values`]. The general storage absorbs the format;
//!   the version-specific *decoder* stays in the version crate, as isolation
//!   requires.
//! * **Interleaved, not grouped.** 1.8 groups arrays by type (all block data,
//!   then all block light…). 1.12.2 is **per-section**: each section is a full
//!   `[bitsPerBlock, palette, data, blockLight, skyLight]` record before the
//!   next. The biome footer (256 bytes, 2-D) trails the sections, present only
//!   when `groundUp`.
//! * **VarInt bitmask** (not the 1.8 `unsigned short`), and a dedicated
//!   `unload_chunk` packet rather than 1.8's empty-bitmask unload trick.
//! * **Block entities** are full **named NBT** compounds (the legacy form with a
//!   root name), not the modern compact `(packed_xz, y, varint type, nbt)`
//!   record [`lodestone_world::BlockEntity`] models. Their type id is sent as a
//!   *string* inside the NBT, with no numeric registry available here, so they
//!   are **consumed** (to keep the zero-trailing-bytes detector meaningful) but
//!   not retained. This is a reported seam, analogous to the biome down-sampling
//!   below — noted, not silently dropped.
//!
//! # The biome seam (shared with v47)
//!
//! 1.12.2 biomes are still **2-D**: one byte per XZ column. The version-free
//! [`ChunkSection`] stores a 3-D 4×4×4 biome container, so this crate fabricates
//! one by down-sampling 16×16→4×4 and replicating over Y — lossy horizontally
//! and fictional vertically. Harmless on a test world, wrong on a real one; the
//! same `lodestone-world` finding v47 surfaced.

use lodestone_core::{Reader, read_named_nbt};
use lodestone_macros::Packet;
use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, LightData, NibbleArray, PaletteKind, PalettedContainer,
    Result,
};

use crate::canonical::{self, FallbackTally};

/// Number of block sections in a 1.12.2 column (fixed world height 0..256).
const SECTION_COUNT: usize = 16;
/// Entries in one block-state section (16³).
const BLOCK_ENTRIES: usize = 4096;
/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;
/// Bytes in the 2-D biome footer (16×16, one byte per column).
const BIOME_BYTES: usize = 256;

/// Dimension shape needed to decode a 1.12.2 chunk column.
///
/// As in 1.8, `map_chunk` cannot say from its own bytes whether sky light is
/// present — that follows from the dimension the join packet announced — so it
/// is supplied here. 1.12.2 columns are always 16 sections tall with `min_y = 0`.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// Whether the dimension carries sky light (true for the overworld). When
    /// set, each section carries a trailing 2048-byte sky-light array.
    pub has_skylight: bool,
    /// Palette configuration for block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the (fabricated) biome containers.
    pub biome_kind: PaletteKind,
    /// Block-state id treated as air — the **canonical 26.2**
    /// [`canonical::air_state_id`], not the legacy `(0, 0)` composite id.
    /// Every block this crate stores has already been translated by
    /// [`canonical::resolve_or_air`] by the time it reaches a
    /// [`PalettedContainer`] (see [`decode_section_blocks`]), so this must
    /// match that id space, not the wire's.
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The 1.12.2 overworld: 16 sections, `min_y = 0`, sky light present.
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

    /// A dimension without sky light (nether/end): no trailing sky-light arrays.
    #[must_use]
    pub fn no_skylight() -> Self {
        Self {
            has_skylight: false,
            ..Self::overworld()
        }
    }
}

/// A decoded 1.12.2 chunk column: block sections plus inline sky/block light.
///
/// 1.12.2 carries no heightmaps in the chunk packet, and its block-entity list
/// is consumed but not retained (see the module docs), so this holds only what
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
    /// Sky and block light.
    pub light: ColumnLight,
    /// How many blocks in this column needed a fallback substitution while
    /// bridging legacy `id:meta` to a canonical 26.2 state — see
    /// [`canonical::resolve_or_air`]. Zero for the overwhelming majority of
    /// real-world columns; surfaced here (and logged, see
    /// [`MapChunk::decode`]) rather than silently absorbed so a wrong
    /// mapping stays traceable per CLAUDE.md's evidence standards.
    pub fallback: FallbackTally,
}

/// The `minecraft:map_chunk` packet (clientbound play, id 32).
///
/// A thin [`Packet`] marker: id/name/state/bound come from the derive, but
/// decoding is hand-written via [`MapChunk::decode`] (see the module docs).
#[derive(Debug, Clone, Packet)]
#[mc(name = "minecraft:map_chunk", state = Play, bound = Client)]
pub struct MapChunk;

/// The `minecraft:unload_chunk` packet (clientbound play, id 29).
///
/// 1.12.2 has a dedicated forget packet, unlike 1.8's empty-bitmask trick. This
/// one *is* a plain derived struct — its layout is two big-endian ints.
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
    /// Consumes the entire packet — the outer framing, the length-prefixed
    /// `chunkData` buffer (validated to zero trailing bytes on its own), and the
    /// trailing block-entity NBT list. The caller should still invoke
    /// [`Reader::ensure_empty`] on the outer reader: zero trailing bytes across
    /// the whole packet is the single best detector of a subtly wrong layout.
    ///
    /// # Errors
    ///
    /// Returns an error on malformed input: a truncated buffer, a declared long
    /// count that disagrees with the section geometry, a palette index that
    /// escapes its palette, or a light array of the wrong size. The data comes
    /// from a network socket, so every framing decision validates rather than
    /// trusting the sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkData> {
        let x = r.i32()?;
        let z = r.i32()?;
        let ground_up = r.bool()?;
        let bitmask =
            u32::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
        let blob_len =
            usize::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
        let mut blob = r.take_reader(blob_len)?;
        let data = decode_column(&mut blob, shape, x, z, ground_up, bitmask)?;
        // The declared chunkData length must exactly match the section geometry;
        // any slack is a misparse (or a wrong dimension/skylight assumption).
        blob.ensure_empty()?;

        // The adapter's chosen fallback for every non-Resolved canonical
        // outcome (see `crate::canonical`'s module docs) is a visible,
        // counted air substitution rather than a silent one: log it once per
        // column, with the breakdown, instead of once per block or not at
        // all. Silent for the overwhelming majority of columns, which need
        // no fallback at all.
        if !data.fallback.is_empty() {
            tracing::warn!(
                target: "v340::chunk",
                x, z,
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

        // Block entities trail the buffer as full named-NBT compounds. Their
        // string type id has no numeric registry here, so they are consumed to
        // keep the zero-trailing-bytes gate honest but are not retained.
        let block_entities = r.var_i32()?;
        if block_entities < 0 {
            return Err(lodestone_core::Error::NegativeLength(block_entities).into());
        }
        for _ in 0..block_entities {
            let _ = read_named_nbt(r)?;
        }
        Ok(data)
    }
}

/// Decodes one column's worth of 1.12.2 section data from the `chunkData` buffer
/// into the version-free storage types.
///
/// `blob` is the length-prefixed `chunkData` sub-reader and is consumed exactly.
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

    // Sections are interleaved: each is a full [blocks, blockLight, skyLight]
    // record before the next (unlike 1.8's grouped-by-type layout).
    let mut section_blocks: Vec<(usize, PalettedContainer)> = Vec::with_capacity(present.len());
    for &index in &present {
        let values = decode_section_blocks(blob, &mut fallback)?;
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

    // Biome footer: 256 bytes (16×16) on full columns only. Down-sampled into a
    // fabricated per-section 3-D container (see the biome-seam module note).
    let biome_cells = if ground_up {
        Some(downsample_biomes(blob.bytes(BIOME_BYTES)?))
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
        fallback,
    })
}

/// Reads one section's paletted block data and returns the 4096 **canonical
/// 26.2** block-state ids, translated from the wire's legacy
/// `(blockId << 4) | meta` composite ids via [`canonical::resolve_or_air`].
///
/// The translation happens on the palette (typically a few dozen distinct
/// entries) rather than per-cell where the section carries one — the 4096
/// output values are then just an index through the already-translated
/// palette, same as before. Sections large/varied enough to use the direct
/// (paletteless) encoding translate each of the 4096 raw values directly;
/// [`canonical::resolve_or_air`] is a lazily-built array lookup after the
/// first call in the process, so this stays cheap either way.
fn decode_section_blocks(blob: &mut Reader<'_>, fallback: &mut FallbackTally) -> Result<Vec<u32>> {
    let bits = u32::from(blob.u8()?);
    if bits == 0 || bits > 32 {
        return Err(lodestone_core::Error::Custom(format!("invalid bits-per-block {bits}")).into());
    }

    // Palette length is always present in 1.9–1.12.2; a length of 0 means the
    // direct/global palette (indices are block-state ids themselves).
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
    // Translate the (typically small) palette once, rather than each of the
    // 4096 output cells individually — see the function docs. Each entry is
    // still the wire's legacy `(blockId << 4) | meta` composite id at this
    // point; `canonical::resolve_or_air` is what turns it into a canonical
    // 26.2 state id (substituting air, and recording into `fallback`, for
    // any of the four outcomes `crate::canonical`'s docs enumerate that
    // aren't a clean `Resolved`).
    let mut translated_palette = Vec::with_capacity(palette.len());
    for &raw in &palette {
        let (old_block_id, meta) = legacy_id_meta(raw)?;
        translated_palette.push(canonical::resolve_or_air(old_block_id, meta, fallback));
    }

    // Data array: a VarInt long count then that many big-endian longs. The count
    // must match the old (straddling) packing geometry exactly.
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
            // Direct/global palette: `raw` *is* the legacy composite id
            // itself, one per cell — no shared palette to translate once.
            let (old_block_id, meta) = legacy_id_meta(raw)?;
            canonical::resolve_or_air(old_block_id, meta, fallback)
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

/// Splits a wire-format legacy composite block value into `(old_block_id,
/// meta)`, rejecting anything past `old_block_id in 0..=255, meta in 0..16`
/// (i.e. `raw > 0x0FFF`) as malformed rather than silently truncating —
/// every real 1.12.2 block/palette entry fits in 12 bits by construction (see
/// the module docs), so a larger value means desync or a hostile sender, not
/// a block this crate should render as *something*.
/// The 12-bit rule itself lives in [`canonical::split_composite`] — it is a
/// property of the pre-Flattening *era*, not of protocol 340, and `v47` reads
/// the same composites off a paletteless wire. This wrapper only chooses the
/// **policy**: here a bad value fails the packet, because it arrived in a
/// palette and a bad palette entry means the index stream is suspect too.
fn legacy_id_meta(raw: u32) -> Result<(u8, u8)> {
    canonical::split_composite(raw).ok_or_else(|| {
        lodestone_core::Error::Custom(format!(
            "legacy block value {raw} exceeds the (old_block_id << 4) | meta range"
        ))
        .into()
    })
}

/// Number of 64-bit longs the **old (straddling)** packing uses for `count`
/// entries of `bits` width: values are packed with no per-long padding, so a
/// value may cross a boundary and the total is `ceil(count * bits / 64)`.
const fn straddling_long_count(bits: u32, count: usize) -> usize {
    (count * bits as usize).div_ceil(64)
}

/// Unpacks `count` entries of `bits` width from `longs` using the pre-1.16
/// **straddling** layout, where an entry that crosses a 64-bit boundary is
/// reconstructed from the low bits of one long and the high bits of the next.
///
/// This is the crux of why [`PalettedContainer::decode`] cannot be used: it
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

/// Maps a block-section index (`0` lowest) to its light-section index.
///
/// [`ColumnLight`] carries `section_count + 2` light sections (one below and one
/// above the build range), so block section `s` lives at light section `s + 1`.
const fn light_section(block_section: usize) -> usize {
    block_section + 1
}

/// Down-samples the 16×16 2-D biome footer into a 64-entry (4×4×4) container's
/// worth of ids, replicating the single XZ layer across all four Y layers.
///
/// The lossy fabrication the biome seam forces (see v47's identical note): the
/// returned slice is indexed in the biome container's YZX order, but every Y
/// layer is identical because 1.12.2 biomes have no vertical dimension.
fn downsample_biomes(footer: &[u8]) -> Vec<u32> {
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
