//! Version-specific framing for the `minecraft:level_chunk_with_light` packet
//! (protocol 776 / 26.2).
//!
//! The bit-packing, palette selection, indexing and light/heightmap containers
//! all live in the version-free [`lodestone_world`] crate. This module owns
//! only the *framing* that is specific to this protocol family:
//!
//! * the top-level `int x`, `int z`, the heightmap map, the length-prefixed
//!   section blob, the block-entity list, and the trailing light payload;
//! * the per-section leading `short nonEmptyBlockCount` **and**
//!   `short fluidCount` (26.2 still writes both — a detail the synthetic tests
//!   could not have covered), followed by the block-state container then the
//!   biome container;
//! * [`LongArrayFraming::FixedSize`] and the typed-list heightmap form, which
//!   this family adopted at protocol 770 (1.21.5).
//!
//! # Structural context via the derive
//!
//! The chunk sub-structures need *structural* parameters that come from the
//! dimension type registry, not the protocol version:
//! [`PalettedContainer::decode`] needs a [`PaletteKind`],
//! [`Heightmaps::decode`] needs the world height, and
//! [`ColumnLight::decode`] needs the section count. Those describe the *shape*
//! of the world (min-Y, height), which the server sends separately in the
//! dimension registry, so they cannot be threaded through the plain
//! [`Decode`](lodestone_core::Decode) trait (its only runtime input is
//! [`Ctx`]`{ version }`).
//!
//! The macro crate closes this with `#[mc(decode_context = "ChunkShape")]`,
//! which generates an inherent `decode_with(r, ctx, &ChunkShape)` instead of a
//! `Decode` impl, and `#[mc(decode_with = "path")]` on each shape-dependent
//! field to route it through a custom decoder that receives the shape. The
//! [`Packet`] derive (id `45`, name/state/bound) is independent and coexists.
//!
//! See the `## Migration verdict` note at the bottom of this module for the
//! honest assessment of how well that mechanism fits *this* packet.

use lodestone_core::{Ctx, Reader};
use lodestone_macros::{Decode, Packet};
use lodestone_world::{
    BlockEntity, ChunkColumn, ChunkSection, ColumnLight, Heightmaps, LongArrayFraming, PaletteKind,
    PalettedContainer, Result,
};

/// Fixed decode context for this protocol family (776 / 26.2).
const CTX: Ctx = Ctx { version: 776 };

/// Dimension shape needed to decode a chunk column.
///
/// These values come from the dimension type registry (min build height,
/// total height in blocks) rather than the protocol version, which is why they
/// cannot be threaded through the `Decode` derive and must be supplied
/// explicitly. Construct one with [`ChunkShape::overworld_1_21`] for the 26.2
/// overworld, or build a custom shape for other dimensions.
#[derive(Debug, Clone)]
pub struct ChunkShape {
    /// Lowest world-`y` in the column (e.g. `-64` for the 1.18+ overworld).
    pub min_y: i32,
    /// Number of block-state sections in the column.
    pub section_count: usize,
    /// Total column height in blocks (`section_count * 16`), used to size the
    /// heightmap bit width.
    pub world_height: u32,
    /// Palette configuration for the block-state containers.
    pub block_kind: PaletteKind,
    /// Palette configuration for the biome containers.
    pub biome_kind: PaletteKind,
    /// Block-state id treated as air (registry id `0`).
    pub air_id: u32,
    /// Default biome id for empty sections (registry id `0`).
    pub biome_id: u32,
}

impl ChunkShape {
    /// The 26.2 overworld: `y = -64..320`, 24 sections, world height 384.
    ///
    /// Both containers use [`LongArrayFraming::FixedSize`] and the typed-list
    /// heightmap form, which this protocol family (770+, 1.21.5+) uses.
    #[must_use]
    pub fn overworld_1_21() -> Self {
        Self {
            min_y: -64,
            section_count: 24,
            world_height: 384,
            block_kind: PaletteKind::block_states().with_framing(LongArrayFraming::FixedSize),
            biome_kind: PaletteKind::biomes().with_framing(LongArrayFraming::FixedSize),
            air_id: 0,
            biome_id: 0,
        }
    }

    /// The 26.2 nether and end: `y = 0..256`, 16 sections, world height 256.
    ///
    /// Shares the 770-family palette framing with the overworld; only the
    /// build-height window differs. Both non-overworld vanilla dimensions use
    /// the same window, so one constructor covers both.
    #[must_use]
    pub fn nether_or_end_1_21() -> Self {
        Self {
            min_y: 0,
            section_count: 16,
            world_height: 256,
            ..Self::overworld_1_21()
        }
    }

    /// Selects the shape for a vanilla dimension by its identifier.
    ///
    /// Heights properly belong to the dimension-type registry (sent as registry
    /// data during configuration); until that registry is decoded, the three
    /// vanilla dimensions are mapped by name and anything else — including
    /// datapack-custom dimensions — falls back to the overworld window. That
    /// fallback is a documented limitation, not a silent guess: a custom
    /// dimension with a non-standard height would need the registry to decode
    /// correctly.
    #[must_use]
    pub fn for_dimension(name: &str) -> Self {
        match name {
            "minecraft:the_nether" | "minecraft:the_end" => Self::nether_or_end_1_21(),
            _ => Self::overworld_1_21(),
        }
    }
}

/// The `minecraft:level_chunk_with_light` packet (clientbound play, id `45`).
///
/// Carries a fully decoded chunk column: block-state and biome sections, sky
/// and block light, heightmaps, and block entities. Light arrives in **this**
/// packet, not a separate one (there is also a standalone `light_update`
/// packet, id `48`, for light-only updates).
///
/// Fields are declared in **wire order** because the [`Decode`] derive decodes
/// them top to bottom: `x`, `z`, heightmaps, the length-prefixed section blob,
/// block entities, then the trailing light payload.
#[derive(Debug, Clone, Decode, Packet)]
#[mc(name = "minecraft:level_chunk_with_light", state = Play, bound = Client)]
#[mc(decode_context = "ChunkShape")]
pub struct LevelChunkWithLight {
    /// Chunk column x coordinate (in chunks).
    pub x: i32,
    /// Chunk column z coordinate (in chunks).
    pub z: i32,
    /// Column heightmaps (typed-list / map form).
    #[mc(decode_with = "decode_heightmaps")]
    pub heightmaps: Heightmaps,
    /// Block-state and biome sections (length-prefixed section blob).
    #[mc(decode_with = "decode_column")]
    pub column: ChunkColumn,
    /// Block entities within the column.
    #[mc(decode_with = "decode_block_entities")]
    pub block_entities: Vec<BlockEntity>,
    /// Sky and block light.
    #[mc(decode_with = "decode_light")]
    pub light: ColumnLight,
}

impl LevelChunkWithLight {
    /// Decodes a chunk packet body given the dimension [`ChunkShape`].
    ///
    /// A thin wrapper over the derive-generated
    /// [`decode_with`](Self::decode_with) that pins the protocol [`Ctx`] and
    /// restores the [`WorldError`](lodestone_world::WorldError) result the rest
    /// of the world codec speaks.
    ///
    /// The caller is expected to invoke [`Reader::ensure_empty`] afterwards:
    /// zero trailing bytes across the whole packet is the single best detector
    /// of a subtly wrong layout, since a misparse almost always leaves the
    /// buffer misaligned.
    ///
    /// # Errors
    ///
    /// Returns a [`WorldError`](lodestone_world::WorldError) on malformed input:
    /// a bad bits-per-entry, a
    /// wrong packed-long count, an out-of-range palette index, a light array of
    /// the wrong length, or a truncated buffer. This data comes from a network
    /// socket, so every framing decision validates rather than trusting the
    /// sender.
    pub fn decode(r: &mut Reader<'_>, shape: &ChunkShape) -> Result<Self> {
        Ok(Self::decode_with(r, CTX, shape)?)
    }
}

/// Custom field decoder: the typed-list (map) heightmaps, sized by world height.
fn decode_heightmaps(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    shape: &ChunkShape,
) -> lodestone_core::Result<Heightmaps> {
    Ok(Heightmaps::decode(shape.world_height, r)?)
}

/// Custom field decoder: the length-prefixed section blob.
///
/// The blob is decoded inside a bounded sub-reader so a lie about the section
/// framing can only ever corrupt this chunk, not read past the declared length;
/// `ensure_empty` on the sub-reader is the strongest per-section alignment
/// check we have.
fn decode_column(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    shape: &ChunkShape,
) -> lodestone_core::Result<ChunkColumn> {
    let blob_len =
        usize::try_from(r.var_i32()?).map_err(|_| lodestone_core::Error::UnexpectedEof)?;
    let mut blob = r.take_reader(blob_len)?;
    let column = read_sections(&mut blob, shape)?;
    blob.ensure_empty()?;
    Ok(column)
}

/// Custom field decoder: the block-entity list (shape-independent, but routed
/// through a custom decoder because [`BlockEntity`] is decode-only and does not
/// implement [`Decode`]).
fn decode_block_entities(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    _shape: &ChunkShape,
) -> lodestone_core::Result<Vec<BlockEntity>> {
    Ok(BlockEntity::decode_list(r)?)
}

/// Custom field decoder: the trailing light payload, sized by section count.
fn decode_light(
    r: &mut Reader<'_>,
    _ctx: Ctx,
    shape: &ChunkShape,
) -> lodestone_core::Result<ColumnLight> {
    Ok(ColumnLight::decode(shape.section_count, r)?)
}

/// Reads the `section_count` sections that make up the section blob into a
/// [`ChunkColumn`], eliding empty sections. Returns the world crate's
/// [`Result`], which the calling field decoder lifts into the core `Result` via
/// `?` (the `From<WorldError>` bridge in `lodestone-world`).
fn read_sections(blob: &mut Reader<'_>, shape: &ChunkShape) -> Result<ChunkColumn> {
    let mut column = ChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    for index in 0..shape.section_count {
        // 26.2 writes both a non-air block count and a fluid count as shorts
        // before the containers. The fluid count is redundant for a client
        // that recomputes non-air from the block container, but it is on the
        // wire and must be consumed to stay aligned.
        let _non_air_count = blob.i16()?;
        let _fluid_count = blob.i16()?;

        // Block-state container first, then the biome container.
        let block_states = PalettedContainer::decode(shape.block_kind, blob)?;
        let biomes = PalettedContainer::decode(shape.biome_kind, blob)?;
        let section = ChunkSection::from_containers(block_states, biomes, shape.air_id);

        if !section.is_empty(shape.biome_id) {
            column.set_section(index, Some(section));
        }
    }

    Ok(column)
}

// ## Migration verdict (Task A)
//
// **adopt-with-changes**, leaning toward "keep hand-written for *this* packet".
//
// What the derive genuinely bought: field dispatch/ordering and the two plain
// `i32` reads (`x`, `z`). Everything structurally interesting — the
// length-prefixed sub-blob, the `section_count` loop over two *different*
// `PaletteKind` containers, the light bitset framing — still lives in
// hand-written functions. The macro has no native "`Vec<T>` whose element
// decode needs context": the repeated section structure is a manual loop inside
// `read_sections`, exactly as before. So the "interesting case" the design was
// meant to prove out is *not* expressed by the derive; it is expressed by a
// custom function the derive merely calls.
//
// Two concrete frictions this migration exposed, neither fatal:
//   1. The generated `decode_with` hardcodes `lodestone_core::Result`, so a
//      field codec that speaks `WorldError` must cross the two error types. The
//      *right* place to bridge that is `lodestone-world` itself: an
//      `impl From<WorldError> for lodestone_core::Error` is orphan-legal there
//      (the error type is local), so it now exists and the field codecs use
//      plain `?`. `WorldError::Core` unwraps back to its exact inner error
//      rather than being stringified, so a core error round-trips losslessly
//      (asserted by `error_bridge_tests` in `lodestone-world`). An earlier draft
//      wrongly claimed orphan rules forbade this by looking for the impl in
//      *this* crate; the impl belongs in the crate that owns `WorldError`.
//   2. A field that needs *no* context (`block_entities`) still cannot use the
//      plain path, because `BlockEntity` is decode-only (no `Decode` impl). It is
//      pulled through `decode_with` purely to bridge that gap, which slightly
//      overstates how "contextual" it is.
//
// Net readability is roughly a wash: the old single linear `decode` made wire
// order obvious in one place; the new form spreads it across field order plus
// four thin functions. It is not "hand-written decoding wearing an attribute" in
// a pejorative sense — the attributes do real dispatch — but the win is modest
// here and the honest recommendation is that the mechanism is worth adopting for
// packets with *many simple* context-dependent scalar/opaque fields, and not
// worth forcing onto codecs whose bulk is a bespoke loop. This packet is the
// latter; it is migrated here to prove the mechanism compiles and passes the
// live gate, and either form is defensible to keep. The mechanism stays because
// it is non-breaking and free; it is deliberately *not* extended with element-
// context support, since only ~2 packets per family would ever use it.
