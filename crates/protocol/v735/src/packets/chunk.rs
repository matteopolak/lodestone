//! Version-specific framing for the 1.16.5 (protocol 754) chunk packets
//! `minecraft:map_chunk` and `minecraft:unload_chunk`.
//!
//! # Why this is hand-written and *not* a derived `Decode`
//!
//! `minecraft-data` models only the **outer** framing of `map_chunk` and leaves
//! the section geometry opaque: in
//! `vendor/minecraft-data/data/pc/1.16.2/protocol.json` the payload is `i32 x`,
//! `i32 z`, `bool groundUp`, `varint bitMap`, `nbt heightmaps`, a biomes array
//! present only on full columns, a `varint`-length `chunkData` **buffer**, then a
//! `varint`-counted `blockEntities` array of raw `nbt`. The bytes *inside*
//! `chunkData` have no declarative schema, so the layout below is transcribed
//! from the 1.16 wire spec directly.
//!
//! # How 1.16.5 differs from the pre-1.13 families (v47/v340)
//!
//! * **Post-flattening, flat state ids.** A palette entry is a single flat
//!   block-state id, *not* the legacy `(blockId << 4) | meta`. That flat id is
//!   the version-free id space this crate feeds the world store (the same space
//!   the modern v770 crate uses).
//! * **Non-straddling (padded) long packing.** 1.16 packs each section's index
//!   array so a value **never** spans two 64-bit longs; unused high bits of each
//!   long are padding. This is exactly what [`PalettedContainer::decode`]
//!   implements, so — unlike v47/v340, which hand-unpack the old straddling
//!   layout — this crate calls it directly, selecting
//!   [`LongArrayFraming::Prefixed`] (the array is preceded by a VarInt long
//!   count, as every family ≤ 1.21.4 is).
//! * **Heightmaps as NBT.** A `MOTION_BLOCKING` (and, for full columns,
//!   `WORLD_SURFACE`) long-array heightmap travels as an inline NBT compound
//!   before the sections. It is consumed here (the world store recomputes
//!   heightmaps lazily) to keep the zero-trailing-bytes detector meaningful.
//! * **3-D biomes.** Full columns carry a flat array of **1024** biome ids
//!   (4×4×4 cells over the whole 256-tall column = 16 sections × 64 cells), a
//!   VarInt each, *before* the section blob — not the pre-1.15 256-byte 2-D
//!   footer. These are real 3-D biomes, so no fabrication is needed (contrast
//!   the v47/v340 down-sampling seam).
//! * **Light is gone.** 1.14 split light out of `map_chunk` into the separate
//!   `update_light` packet ([`UpdateLight`]), so a section here is just
//!   `[blockCount: i16, PalettedContainer]` with **no** inline block/sky light.
//! * **Block entities** are full **named NBT** compounds; their type id is a
//!   string with no numeric registry here, so they are **consumed** (to keep the
//!   zero-trailing-bytes detector meaningful) but not retained — a reported seam.

use lodestone_core::{Reader, read_named_nbt};
use lodestone_macros::Packet;
use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, LightPatch, LongArrayFraming, NibbleArray, PaletteKind,
    PalettedContainer, Result,
};

/// Number of block sections in a 1.16.5 column (fixed world height 0..256).
const SECTION_COUNT: usize = 16;
/// Biome cells per section (4×4×4).
const BIOME_CELLS_PER_SECTION: usize = 64;
/// Bytes for one nibble light array (4096 nibbles).
const LIGHT_BYTES: usize = 2048;

/// Dimension shape needed to decode a 1.16.5 chunk column.
///
/// 1.16 columns are always 16 sections tall with `min_y = 0`. Unlike the
/// pre-1.14 families, sky-light presence is **not** part of chunk decoding —
/// light travels in the separate `update_light` packet — so this carries only
/// the palette configuration and the air/default ids.
#[derive(Debug, Clone, Copy)]
pub struct ChunkShape {
    /// Palette configuration for block-state containers (prefixed long arrays).
    pub block_kind: PaletteKind,
    /// Palette configuration for the 3-D biome containers.
    pub biome_kind: PaletteKind,
    /// Block-state id treated as air (flat state id 0).
    pub air_id: u32,
    /// Default biome id for sections/columns without biome data.
    pub biome_id: u32,
}

impl ChunkShape {
    /// The 1.16.5 overworld shape: 16 sections, `min_y = 0`, flat state ids in
    /// prefixed-long paletted containers.
    #[must_use]
    pub fn overworld() -> Self {
        Self {
            block_kind: PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed),
            biome_kind: PaletteKind::biomes().with_framing(LongArrayFraming::Prefixed),
            air_id: 0,
            biome_id: 0,
        }
    }

    /// A dimension without sky light (nether/end). Since 1.16 does not carry
    /// light in `map_chunk`, this is identical to [`ChunkShape::overworld`]; the
    /// distinction is kept only so the adapter's dimension bookkeeping reads
    /// naturally.
    #[must_use]
    pub fn no_skylight() -> Self {
        Self::overworld()
    }
}

/// A decoded 1.16.5 chunk column: block and biome sections.
///
/// 1.16 carries no light in the chunk packet (it arrives via `update_light`), so
/// `light` is always the empty column light here; the block-entity list is
/// consumed but not retained (see the module docs).
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
    /// Empty column light (1.16 light travels separately).
    pub light: ColumnLight,
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

        // Biomes: full columns carry a VarInt-length-prefixed array of biome
        // ids (1024 = 4×4×4 over the whole column). The length prefix is a
        // 1.16.2 change — 1.15/1.16.1 sent a bare fixed 1024-int array with no
        // count. Partial updates carry no biomes at all.
        let biomes = if ground_up {
            let count = r.var_i32()?;
            let count =
                usize::try_from(count).map_err(|_| lodestone_core::Error::NegativeLength(count))?;
            let mut all = Vec::with_capacity(count);
            for _ in 0..count {
                all.push(u32::try_from(r.var_i32()?).unwrap_or(0));
            }
            Some(all)
        } else {
            None
        };

        // chunkData: a VarInt-length buffer holding the present sections.
        let raw_len = r.var_i32()?;
        let blob_len =
            usize::try_from(raw_len).map_err(|_| lodestone_core::Error::NegativeLength(raw_len))?;
        let mut blob = r.take_reader(blob_len)?;
        let column = decode_sections(&mut blob, shape, biomes.as_deref(), bitmask)?;
        // The declared chunkData length must exactly match the section geometry;
        // any slack is a misparse.
        blob.ensure_empty()?;

        // Block entities trail as full named-NBT compounds; consumed but not
        // retained to keep the zero-trailing-bytes gate honest.
        let block_entities = r.var_i32()?;
        if block_entities < 0 {
            return Err(lodestone_core::Error::NegativeLength(block_entities).into());
        }
        for _ in 0..block_entities {
            let _ = read_named_nbt(r)?;
        }

        Ok(ChunkData {
            x,
            z,
            ground_up,
            column,
            light: ColumnLight::new(SECTION_COUNT),
        })
    }
}

/// Decodes the present sections from the `chunkData` buffer into version-free
/// storage. Each present section is `[blockCount: i16, PalettedContainer]`; the
/// per-column biome ids (when present) are sliced into per-section 4×4×4
/// containers.
fn decode_sections(
    blob: &mut Reader<'_>,
    shape: &ChunkShape,
    biomes: Option<&[u32]>,
    bitmask: u32,
) -> Result<ChunkColumn> {
    let mut column = ChunkColumn::new(
        0,
        SECTION_COUNT,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    for index in 0..SECTION_COUNT {
        if bitmask & (1 << index) == 0 {
            continue;
        }
        // Non-air block count: advisory, validated non-negative but otherwise
        // unused (the container carries the authoritative contents).
        let _block_count = blob.i16()?;
        let blocks = PalettedContainer::decode(shape.block_kind, blob)?;

        let biome_container = match biomes {
            Some(all) => {
                PalettedContainer::from_values(shape.biome_kind, &section_biomes(all, index))
            }
            None => PalettedContainer::new(shape.biome_kind, shape.biome_id),
        };

        let section = ChunkSection::from_containers(blocks, biome_container, shape.air_id);
        if !section.is_empty(shape.biome_id) {
            column.set_section(index, Some(section));
        }
    }

    Ok(column)
}

/// Extracts one section's 64 biome cells from the column's 1024-entry biome
/// array, mapping the wire's whole-column YZX index into the biome container's
/// section-local YZX index.
///
/// Wire index for a cell at 4×4×4-cell coordinates `(cx, cz, cy_global)` is
/// `cy_global * 16 + cz * 4 + cx`; the container's local index is
/// `(cy_local << 4) | (cz << 2) | cx` with `cy_global = section * 4 + cy_local`.
fn section_biomes(all: &[u32], section: usize) -> Vec<u32> {
    let mut cells = vec![0u32; BIOME_CELLS_PER_SECTION];
    for cy_local in 0..4 {
        let cy_global = section * 4 + cy_local;
        for cz in 0..4 {
            for cx in 0..4 {
                let wire = cy_global * 16 + cz * 4 + cx;
                let local = (cy_local << 4) | (cz << 2) | cx;
                cells[local] = all.get(wire).copied().unwrap_or(0);
            }
        }
    }
    cells
}

/// The `minecraft:update_light` packet (clientbound play), added in 1.14 when
/// light left `map_chunk`.
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
    /// `varint chunkX`, `varint chunkZ`, `bool trustEdges`, then four **VarInt**
    /// masks (sky, block, empty-sky, empty-block — 1.16 uses single-VarInt masks,
    /// not the 1.17 BitSet long arrays), followed by the present sky-light
    /// arrays (each a `varint`-length-prefixed 2048-byte nibble array, in
    /// ascending set-bit order) and then the present block-light arrays.
    ///
    /// # Errors
    ///
    /// Returns an error on truncated input or a light array whose declared
    /// length is not 2048 bytes.
    pub fn decode(r: &mut Reader<'_>) -> Result<LightUpdate> {
        let x = r.var_i32()?;
        let z = r.var_i32()?;
        let _trust_edges = r.bool()?;
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
