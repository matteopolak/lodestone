//! Terrain source for the integrated server.
//!
//! A [`ChunkSource`] answers "what blocks are in column `(cx, cz)`?".
//!
//! The production implementation is [`Terrain263ChunkSource`]: the 26.3 noise
//! terrain, biomes, structures and decoration for one dimension. The flat and
//! debug presets have their own sources in [`crate::worldgen_data`], and the
//! saved-world store ([`crate::region_source::RegionChunkSource`]) wraps any of
//! them.
//!
//! # The column carries block states, not just solidity
//!
//! [`ChunkColumn`] stores a per-column palette of canonical [`StateId`] values plus a
//! dense index grid, so a
//! `ServerProtocol::encode_chunk` can emit a real chunk. The historical
//! solid/air API ([`ChunkColumn::set_solid`]/[`ChunkColumn::is_solid`]) is
//! preserved as a view over that field: a block is "solid" when it is neither air
//! nor a fluid, and `set_solid(true)` writes canonical stone.
//!
//! # Edits need somewhere to live
//!
//! [`ChunkSource::set_block`] mutates a block in place and [`ChunkSource::column`]
//! must go on reflecting that mutation afterward. A generating source keeps
//! edited columns and regenerates untouched ones on demand.

use std::sync::Arc;

use lodestone_model::BlockPos;
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;

use crate::block_entities::{BlockEntity, BlockEntityKind};
use crate::chunk_blocks::SectionedBlocks;

#[path = "chunk_stone_floor.rs"]
mod chunk_stone_floor;
mod terrain263;
pub use terrain263::Terrain263ChunkSource;
pub use chunk_stone_floor::StoneFloorSource;

// Counts calls to [`ChunkColumn::intern`] separately for each test thread.
// A counter records operation count rather than wall-clock time, so scheduling
// variance cannot change the measurement. Thread-local storage isolates each
// test's reset/read pair from calls made by other test threads.
#[cfg(test)]
thread_local! {
    static INTERN_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static HEIGHTMAP_REPAIRS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Resets [`INTERN_CALLS`] to zero. Call before the operation under
/// measurement.
#[cfg(test)]
pub(crate) fn reset_intern_calls() {
    INTERN_CALLS.with(|c| c.set(0));
}

/// Reads [`INTERN_CALLS`]. Call immediately after the operation under
/// measurement, before anything else on this thread can call
/// [`ChunkColumn::intern`].
#[cfg(test)]
pub(crate) fn intern_calls() -> u64 {
    INTERN_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_heightmap_repairs() {
    HEIGHTMAP_REPAIRS.with(|c| c.set(0));
}

#[cfg(test)]
fn heightmap_repairs() -> u64 {
    HEIGHTMAP_REPAIRS.with(std::cell::Cell::get)
}

#[inline]
pub(crate) fn air_state() -> StateId {
    Block::Air.default_state()
}

#[inline]
pub(crate) fn stone_state() -> StateId {
    Block::Stone.default_state()
}
/// Rows per implicit section. [`ChunkColumn`] has no per-section struct — a
/// "section" here is a 16-row window of the one flat grid, counted from
/// `min_y` — so this is the only place the window height is written down.
pub(crate) const SECTION_ROWS: usize = 16;
/// Fallback biome for a [`ChunkColumn`] built with no generator behind it
/// ([`ChunkColumn::new`]'s blank column, and [`StoneFloorSource`], which
/// only ever models solidity). A column
/// served by a real generator overwrites this with per-quart biome data.
pub(crate) const DEFAULT_BIOME: &str = "minecraft:plains";

/// A bounded, query-only surface sample for a distant-terrain client.
///
/// This is deliberately smaller than a [`ChunkColumn`]. A source may answer
/// it without generating, retaining, or mutating a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizonSample {
    /// Preliminary surface height in world coordinates.
    pub terrain_y: i32,
    /// Surface height of water, or `None` when the cell is dry.
    pub water_y: Option<i32>,
    /// Gamma-space RGB565 surface colour.
    pub surface_rgb565: u16,
    /// Material-class and future extension bits.
    pub flags: u16,
}

/// Vertical quart layers in a column of `height` block rows — the one place the
/// 3-D biome grid's Y extent is written down.
fn y_quarts_for(height: i32) -> usize {
    (height as usize).div_ceil(4).max(1)
}

const CLIENT_WORLD_SURFACE_HEIGHTMAP_TYPE_ID: u32 = 1;
pub(crate) const CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID: u32 = 4;
const CLIENT_MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID: u32 = 5;

#[cfg(test)]
fn client_heightmap_includes(type_id: u32, state: lodestone_data::block_states::StateId) -> bool {
    let motion = lodestone_data::block_solidity::blocks_motion(state)
        || lodestone_data::snow_support::has_fluid_state(state);
    match type_id {
        CLIENT_WORLD_SURFACE_HEIGHTMAP_TYPE_ID => {
            !matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
        }
        CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID => motion,
        CLIENT_MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID => {
            motion && !lodestone_data::tool::builtin_block_tag_contains("minecraft:leaves", state.block())
        }
        _ => false,
    }
}

/// Resolve all three client heightmaps for one XZ column in a single top-down
/// walk. Each map keeps its own first matching state, so the common walk does
/// not make the no-leaves map inherit the motion-blocking answer.
#[inline]
fn client_heightmap_values_at(
    min_y: i32,
    height: i32,
    mut state_at: impl FnMut(i32) -> StateId,
) -> [u32; 3] {
    let mut values = [0; 3];
    let mut remaining = 0b111u8;
    let mut scanned = 0;
    for y in (min_y..min_y + height).rev() {
        scanned += 1;
        let state = state_at(y);
        let stored = (y + 1 - min_y) as u32;
        if remaining & 1 != 0
            && !matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir)
        {
            values[0] = stored;
            remaining &= !1;
        }
        if remaining & 0b110 != 0 {
            let motion = lodestone_data::block_solidity::blocks_motion(state)
                || lodestone_data::snow_support::has_fluid_state(state);
            if remaining & 2 != 0 && motion {
                values[1] = stored;
                remaining &= !2;
            }
            if remaining & 4 != 0
                && motion
                && !lodestone_data::tool::builtin_block_tag_contains(
                    "minecraft:leaves",
                    state.block(),
                )
            {
                values[2] = stored;
                remaining &= !4;
            }
        }
        if remaining == 0 {
            break;
        }
    }
    lodestone_worldgen::counters::bump_heightmap_scan(scanned);
    values
}

fn derive_client_heightmaps(column: &ChunkColumn) -> lodestone_world::Heightmaps {
    let mut raw = [[0u16; 256]; 3];
    let scan_height = column.air_above_y().min(column.min_y + column.height) - column.min_y;
    for z in 0..16i32 {
        for x in 0..16i32 {
            let values = client_heightmap_values_at(column.min_y, scan_height, |y| {
                column.block_state_id(x, y, z)
            });
            let index = x as usize + z as usize * 16;
            for (map, value) in raw.iter_mut().zip(values) {
                map[index] = value as u16;
            }
        }
    }
    heightmaps_from_raw(column.height, raw)
}

#[cfg(test)]
fn derive_client_heightmaps_naive(column: &ChunkColumn) -> lodestone_world::Heightmaps {
    let mut maps = lodestone_world::Heightmaps::new();
    for type_id in [
        CLIENT_WORLD_SURFACE_HEIGHTMAP_TYPE_ID,
        CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID,
        CLIENT_MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID,
    ] {
        let mut map = lodestone_world::Heightmap::new(column.height as u32);
        for z in 0..16i32 {
            for x in 0..16i32 {
                let stored = (column.min_y..column.min_y + column.height)
                    .rev()
                    .find(|&y| client_heightmap_includes(type_id, column.block_state_id(x, y, z)))
                    .map_or(0, |y| (y + 1 - column.min_y) as u32);
                map.set(x as usize, z as usize, stored);
            }
        }
        maps.insert(type_id, map);
    }
    maps
}

fn heightmaps_from_raw(height: i32, raw: [[u16; 256]; 3]) -> lodestone_world::Heightmaps {
    let mut client_heightmaps = lodestone_world::Heightmaps::new();
    for (type_id, values) in [
        (CLIENT_WORLD_SURFACE_HEIGHTMAP_TYPE_ID, raw[0]),
        (CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, raw[1]),
        (CLIENT_MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID, raw[2]),
    ] {
        let mut map = lodestone_world::Heightmap::new(height as u32);
        for z in 0..16usize {
            for x in 0..16usize {
                map.set(x, z, u32::from(values[x + z * 16]));
            }
        }
        client_heightmaps.insert(type_id, map);
    }
    client_heightmaps
}

pub(crate) fn is_air_or_fluid_id(state: lodestone_data::block_states::StateId) -> bool {
    matches!(
        state.block(),
        Block::Air | Block::CaveAir | Block::VoidAir | Block::Water | Block::Lava
    )
}

fn state_metadata(state: StateId) -> (bool, crate::redstone_graph::ReactionClass) {
    (
        crate::random_tick::is_randomly_ticking_id(state),
        crate::redstone_graph::classify(state),
    )
}

/// The amount of world generation a streamed column requires.
///
/// This is deliberately a monotone two-value lattice: a full column is always
/// suitable where a shaped one was requested, but not conversely. Gameplay
/// callers use [`ChunkSource::column`], which is permanently `Full`; only the
/// view-streaming scheduler asks for `Shaped` outside its near band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChunkGenerationStage {
    /// Terrain through carving, without decoration or generation-time spawns.
    Shaped,
    /// The complete playable column, including decoration and spawn candidates.
    Full,
}

/// A decoded chunk column: the block state of every block in a 16×`height`×16
/// prism whose bottom is at `min_y`.
///
/// Blocks are stored as indices into a small per-column canonical state-id
/// palette, with entry zero equal to air, laid out
/// `blocks[(ly * 16 + z) * 16 + x]` with `ly = y - min_y`.
#[derive(Debug, Clone)]
pub struct ChunkColumn {
    /// World Y of the lowest block row.
    pub min_y: i32,
    /// Number of block rows (world height).
    pub height: i32,
    /// The highest generation tier incorporated in this value.
    generation_stage: ChunkGenerationStage,
    /// Canonical block-state palette; entry zero is air.
    palette: Vec<lodestone_data::block_states::StateId>,
    /// Palette indices for every cell, one bit-packed 16-row section at a time
    /// (`crate::chunk_blocks`). Logically the same
    /// `blocks[(y_local * 16 + z) * 16 + x]` logical grid; an all-air section
    /// allocates nothing and a populated one packs to the width its ids need
    /// instead of 16 bits.
    ///
    /// **This field was the server's whole render-distance memory bill** —
    /// 192 KiB of the 195.5 KiB `crate::chunk_store` measures per retained
    /// column, so 867 MiB at `render_distance` 32. See `chunk_blocks`'s module
    /// docs for the representation and for why it is not
    /// `lodestone_world::PalettedContainer`.
    blocks: SectionedBlocks,
    /// `palette_ticking[id]` is the cached random-tick classification for
    /// `palette[id]`,
    /// computed once per palette entry as that entry is appended.
    ///
    /// Sound because the palette is **append-only**: [`ChunkColumn::intern_state_id`]
    /// pushes and nothing in this crate ever removes, remaps or compacts an
    /// entry (`palette` is private, so that is compiler-enforced rather than
    /// conventional).
    palette_ticking: Vec<bool>,
    /// The palette itself is the validated global 26.2 state-id table. It is
    /// shared by cell reads and packet/persistence adapters, so no text form
    /// is retained alongside the canonical values.
    /// `palette_reaction[id] == crate::redstone_graph::classify(&palette[id])`
    /// — which family, if any, a neighbour notification landing on a cell
    /// holding `palette[id]` dispatches to. The third per-palette-entry
    /// derived table, sound for exactly the reason
    /// [`palette_ticking`](Self::palette_ticking) is, and maintained in the
    /// same two places.
    ///
    /// **This is what makes redstone dispatch cost an array index.**
    /// `crate::random_tick::react_to_notification` classifies the *palette*
    /// entry instead of cloning the cell's state string and running up to
    /// fifteen `base_name`-plus-`strcmp` family predicates for every
    /// notification. The classification makes that one
    /// index into this table. See `crate::redstone_graph`'s module doc for
    /// why a palette-derived table has no staleness class, and
    /// `docs/redstone-execution.md` for the measured split.
    palette_reaction: Vec<crate::redstone_graph::ReactionClass>,
    /// How many cells in each implicit 16-row window hold a randomly-ticking
    /// state — vanilla's own per-section ticking-block counter,
    /// one entry per section, `len =
    /// height.div_ceil(16)`.
    ///
    /// `u16` for the same reason vanilla uses `short`: a section holds at most
    /// 4096 cells. Maintained incrementally by [`ChunkColumn::set_block`] and
    /// initialized from the final section histogram when generated storage is adopted.
    ///
    /// **Derived state — never serialized.** `crate::chunk_nbt` does not write
    /// it and a column read back off disk rebuilds it from the predicate
    /// compiled into the running binary, so widening
    /// `crate::random_tick::is_randomly_ticking` later cannot strand a stale
    /// persisted count.
    section_ticking: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx`.
    ///
    /// **The surface answer, not the column's biome.** This is what a player
    /// standing on the column sees, and what surface material, carve and
    /// decorate consumed on the generator side. It is deliberately *not* what
    /// the wire or a region file's per-section biome container is built from —
    /// see [`biome_cells`](Self::biome_cells) and its per-section representation.
    biome_quarts: [String; 16],
    /// The distinct biome ids in this column, in first-use order.
    /// `biome_palette[0]` always exists.
    biome_palette: Vec<String>,
    /// Palette indices for the full 4×4×4-per-section biome grid,
    /// laid out `(qy * 4 + qz) * 4 + qx` with `qy` counting up from
    /// `min_y >> 2` — the biome container's own major-to-minor order, and the
    /// same order as `blocks` above.
    ///
    /// `len == biome_y_quarts() * 16`. Broadcasting [`biome_quarts`] vertically
    /// instead of carrying this is what made `lush_caves`/`dripstone_caves`/
    /// `deep_dark` unreachable, and what erased them from every re-saved
    /// vanilla world.
    biome_cells: Vec<u16>,
    /// Block entities living in this column, at **absolute**
    /// positions.
    ///
    /// Populated by the generating source (a generated bee nest and its
    /// occupants, structure containers) and by `crate::region_source`'s load path (so a
    /// chest read off disk reaches the client, not only the tick loop's
    /// registry). This is the list a `ServerProtocol::encode_chunk` writes into
    /// the chunk packet's block-entity array; the *save* path takes its own
    /// list from the live [`crate::block_entities::BlockEntityRegistry`]
    /// instead, because that one is newer.
    block_entities: Vec<(BlockPos, BlockEntity)>,
    /// Structure starts whose **origin** is this column, and this column's
    /// `structures.References`.
    ///
    /// Empty unless the generating chunk source filled them — they are answered
    /// per *chunk coordinate*, which a column does not carry, so the seam is the
    /// chunk source rather than a column constructor. `crate::chunk_nbt` is the only
    /// consumer: they go in a region file's `structures` compound and nowhere on
    /// the wire (there is no clientbound structure packet).
    structure_starts: Vec<std::sync::Arc<lodestone_worldgen::structure::StructureStart>>,
    /// Structure id → packed origin-chunk keys. See
    /// [`structure_starts`](Self::structure_starts).
    structure_references: std::collections::BTreeMap<String, Vec<i64>>,
    /// The generator's `MOTION_BLOCKING` heightmap in its
    /// **stored** form (`topY + 1`, `0` for an all-air column), indexed
    /// `lx + lz * 16`.
    ///
    /// `None` for a column that did not come from the real generator
    /// (a constructor or region-file load); `encode_chunk` then sends the zero-entry
    /// heightmap NBT it has always sent, which is well-framed and simply carries
    /// no map.
    ///
    /// **Not maintained by [`set_block`](Self::set_block).** It is the
    /// generator's snapshot, so a player edit does not move it; `chunk_nbt`
    /// deliberately omits heightmaps from the Anvil write; loading recomputes
    /// derived height data, so nothing
    /// persists a stale value either. Only the first send after generation
    /// carries it, which is exactly the send a client has no other way to
    /// derive one for.
    motion_blocking: Option<Box<[u16; 256]>>,
    /// All three client-visible maps retained for a completed generated column.
    /// An edit refreshes only its affected XZ cell; imported and placeholder
    /// columns leave this absent and use the protocol fallback scan.
    client_heightmaps: Option<lodestone_world::Heightmaps>,
    /// Population completion is shared by terrain snapshots and retained by
    /// the authoritative source before candidates are exposed to a world tick.
    generation_spawns: Option<Arc<crate::generation_population::GenerationSpawnBatch>>,
    /// The exact sky/block light state captured by the source's settlement
    /// transaction, when this column has one.
    ///
    /// A freshly generated column receives this snapshot through the source
    /// lifecycle; a persisted column restores it from its chunk record. Block
    /// edits clear it, and the server installs a replacement after recomputing
    /// the affected 3×3 footprint. Keeping the value beside the blocks makes
    /// an initial packet consume the same state that storage and later
    /// resident reads see.
    retained_light: Option<lodestone_world::ColumnLight>,
    /// Lifecycle stage of the retained light snapshot.
    ///
    /// A dependency may have allocated and populated light storage without
    /// ever completing its own centre admission. This metadata is therefore
    /// separate from the light values and survives explicit-zero snapshots.
    retained_light_status: Option<RetainedLightStatus>,
}

/// Lifecycle stage attached to a retained column-light snapshot.
///
/// `DependencyInitialized` means the light engine allocated this column while
/// settling a neighbouring centre; it must still run the centre admission when
/// this column is later requested as a centre. `CentreSettled` means the
/// column's own initial admission completed and is the only stage eligible for
/// the retained-light fast path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedLightStatus {
    /// Storage was initialized as part of another centre's footprint.
    DependencyInitialized,
    /// This column's own initial light admission completed.
    CentreSettled,
}

/// Logical allocation-capacity components of one retained [`ChunkColumn`].
///
/// This is intentionally a value report rather than a process RSS claim. The
/// `*_bytes` fields count direct buffers and their spare capacities; allocator
/// headers, hash-map/tree node padding, and nested block-entity/structure
/// payloads are separate concerns and are not silently presented as exact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkColumnMemory {
    /// Inline `ChunkColumn` value, useful when the cache owns it in a map node.
    pub inline_bytes: usize,
    /// Sectioned block-index spine and packed section payloads.
    pub blocks_bytes: usize,
    /// `Vec<StateId>` slot array for the block-state palette.
    pub block_palette_slots_bytes: usize,
    /// Reserved for serialized state text; canonical columns own no text.
    pub block_palette_text_bytes: usize,
    /// Per-palette derived arrays.
    pub block_derived_bytes: usize,
    /// Reserved for presentation adapters; canonical columns own no state text.
    pub custom_arc_payload_bytes: usize,
    /// Per-section random-ticking counters.
    pub section_ticking_bytes: usize,
    /// The 16 surface-biome string payloads.
    pub biome_surface_text_bytes: usize,
    /// `Vec<String>` slot array for the 3-D biome palette.
    pub biome_palette_slots_bytes: usize,
    /// 3-D biome palette string capacities.
    pub biome_palette_text_bytes: usize,
    /// Packed 3-D biome cell indices.
    pub biome_cells_bytes: usize,
    /// Outer block-entity record slots (nested values are owned by the entity).
    pub block_entities_slots_bytes: usize,
    /// Outer structure-start `Arc` handles (pointees are nested values).
    pub structure_slots_bytes: usize,
    /// Direct structure-reference key/vector capacities and entry values.
    pub structure_refs_bytes: usize,
    /// Optional motion-blocking map.
    pub motion_blocking_bytes: usize,
    /// Spawn-candidate vector and entity-name capacities.
    pub generation_spawns_bytes: usize,
    /// Retained light vectors and non-uniform arrays.
    pub retained_light_bytes: usize,
}

impl ChunkColumnMemory {
    /// Sum of all reported logical bytes, excluding the shared state cache and
    /// allocator metadata.
    #[must_use]
    pub const fn logical_total(self) -> usize {
        self.inline_bytes
            + self.blocks_bytes
            + self.block_palette_slots_bytes
            + self.block_palette_text_bytes
            + self.block_derived_bytes
            + self.custom_arc_payload_bytes
            + self.section_ticking_bytes
            + self.biome_surface_text_bytes
            + self.biome_palette_slots_bytes
            + self.biome_palette_text_bytes
            + self.biome_cells_bytes
            + self.block_entities_slots_bytes
            + self.structure_slots_bytes
            + self.structure_refs_bytes
            + self.motion_blocking_bytes
            + self.generation_spawns_bytes
            + self.retained_light_bytes
    }
}

impl ChunkColumn {
    /// Creates an all-air column of the given vertical extent, biome fixed
    /// to [`DEFAULT_BIOME`] everywhere (no generator behind this column to
    /// ask — see that constant's doc comment).
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        let (air_ticking, air_reaction) = state_metadata(air_state());
        Self {
            min_y,
            height,
            generation_stage: ChunkGenerationStage::Full,
            palette: vec![lodestone_data::block_states::air_state()],
            blocks: SectionedBlocks::new_air(height),
            palette_ticking: vec![air_ticking],
            palette_reaction: vec![air_reaction],
            section_ticking: vec![0u16; (height as usize).div_ceil(SECTION_ROWS)],
            biome_quarts: std::array::from_fn(|_| DEFAULT_BIOME.to_string()),
            biome_palette: vec![DEFAULT_BIOME.to_string()],
            biome_cells: vec![0u16; y_quarts_for(height) * 16],
            block_entities: Vec::new(),
            structure_starts: Vec::new(),
            structure_references: std::collections::BTreeMap::new(),
            motion_blocking: None,
            client_heightmaps: None,
            generation_spawns: None,
            retained_light: None,
            retained_light_status: None,
        }
    }

    /// The highest generation tier this column contains.
    #[must_use]
    pub fn generation_stage(&self) -> ChunkGenerationStage {
        self.generation_stage
    }

    #[cfg(test)]
    pub(crate) fn test_with_generation_stage(mut self, stage: ChunkGenerationStage) -> Self {
        self.generation_stage = stage;
        self
    }

    /// Adopts a flat
    /// `blocks[(ly * 16 + z) * 16 + x]` grid `generated_height` rows tall, adopted
    /// into a `window_height`-tall column whose remaining rows are air.
    ///
    /// The palette is **re-based so air is index 0**. A generator whose palette
    /// happens not to start with air (or
    /// which produced no air at all, in a fully solid column) would otherwise
    /// make index 0 mean netherrack — and since the padding rows are written as
    /// index 0, the sky above the Nether roof would come out solid.
    fn from_raw_window(
        min_y: i32,
        generated_height: i32,
        window_height: i32,
        palette: Vec<lodestone_data::block_states::StateId>,
        blocks: &[u16],
        biome_quarts: [String; 16],
        decoration_spills: &[(i32, i32, i32, lodestone_data::block_states::StateId)],
        derive_maps: bool,
    ) -> Self {
        assert!(window_height >= generated_height, "window cannot truncate the generated column");
        let mut column = Self::new(min_y, window_height);
        // `Self::new` seeded the palette with air at index 0; intern the rest in
        // the generator's own order so a remap is a single lookup table.
        let remap: Vec<u16> = palette
            .iter()
            .copied()
            .map(|state| column.intern_state_id(state))
            .collect();
        let mut spills = Vec::with_capacity(decoration_spills.len());
        for (order, &(x, y, z, state)) in decoration_spills.iter().enumerate() {
            if (min_y..min_y + window_height).contains(&y) {
                let id = column.intern_state_id(state);
                let index = (y - min_y) as usize * 256
                    + z.rem_euclid(16) as usize * 16 + x.rem_euclid(16) as usize;
                spills.push((index, id, order));
            }
        }
        spills.sort_unstable_by_key(|&(index, _, order)| (index, order));
        let (motion, no_leaves) = if derive_maps {
            column.palette.iter().copied().map(|state| {
                let motion = lodestone_data::block_solidity::blocks_motion(state)
                    || lodestone_data::snow_support::has_fluid_state(state);
                let no_leaves = motion && !lodestone_data::tool::builtin_block_tag_contains(
                    "minecraft:leaves", state.block(),
                );
                (motion, no_leaves)
            }).unzip::<_, _, Vec<_>, Vec<_>>()
        } else {
            (Vec::new(), Vec::new())
        };
        let extra_air = [Block::CaveAir.default_state(), Block::VoidAir.default_state()]
            .map(|state| column.palette.iter().position(|&entry| entry == state)
                .map_or(u16::MAX, |index| index as u16));
        let with_summaries = derive_maps || column.palette_ticking.iter().any(|&ticking| ticking);
        let generated_cells = generated_height.max(0) as usize * 256;
        lodestone_worldgen::counters::bump_raw_window(
            blocks.len().min(generated_cells) as u64,
            0,
            0,
        );
        let mut spill_cursor = 0;
        let (storage, summaries) = lodestone_worldgen::generated_storage::CompactBlockStorage::from_section_fn_with_predicates(
            min_y, window_height, column.palette.len(), with_summaries,
            derive_maps.then_some(motion.as_slice()),
            derive_maps.then_some(no_leaves.as_slice()),
            extra_air,
            |section, cells| {
                let start = section * SECTION_ROWS * 256;
                if start >= generated_cells.min(blocks.len())
                    && spills.get(spill_cursor).is_none_or(|&(index, _, _)| index >= start + cells.len())
                {
                    return Some(0);
                }
                cells.fill(0);
                let mut uniform_air = true;
                for (offset, id) in cells.iter_mut().enumerate().take(generated_cells.saturating_sub(start)) {
                    *id = blocks.get(start + offset)
                        .and_then(|&raw| remap.get(raw as usize)).copied().unwrap_or(0);
                    uniform_air &= *id == 0;
                }
                while let Some(&(index, id, _)) = spills.get(spill_cursor) {
                    if index >= start + cells.len() {
                        break;
                    }
                    cells[index - start] = id;
                    uniform_air &= id == 0;
                    spill_cursor += 1;
                }
                uniform_air.then_some(0)
            },
        );
        column.blocks = SectionedBlocks::from_compact(storage);
        if let Some(summaries) = summaries {
            column.section_ticking = summaries.section_state_counts().iter().map(|counts| {
                counts.iter().zip(&column.palette_ticking)
                    .filter(|(_, ticking)| **ticking).map(|(&count, _)| count).sum()
            }).collect();
            if derive_maps {
                column.client_heightmaps = Some(heightmaps_from_raw(window_height, [
                    *summaries.non_air_first_free(),
                    *summaries.motion_blocking_first_free().expect("motion predicate supplied"),
                    *summaries.motion_blocking_no_leaves_first_free().expect("no-leaves predicate supplied"),
                ]));
            }
        }
        column.biome_quarts = biome_quarts;
        // Broadcast the horizontal quarts through the whole window.
        column.biome_palette = Vec::new();
        column.biome_cells = Vec::with_capacity(column.biome_y_quarts() * 16);
        let quart_ids: Vec<u16> = (0..16)
            .map(|q| {
                let name = column.biome_quarts[q].clone();
                match column.biome_palette.iter().position(|entry| *entry == name) {
                    Some(index) => index as u16,
                    None => {
                        column.biome_palette.push(name);
                        (column.biome_palette.len() - 1) as u16
                    }
                }
            })
            .collect();
        for _ in 0..column.biome_y_quarts() {
            column.biome_cells.extend_from_slice(&quart_ids);
        }
        column
    }

    /// Biome id at local `(x, z)` in `0..16` — quart resolution, the column's
    /// **surface** answer, the same value for every `y`.
    ///
    /// **Wrong question for anything with a `y`** — underground tint, fog,
    /// spawn rules, a wire or region-file biome container. Use
    /// [`biome_state_at`](Self::biome_state_at) for those; use the y-aware accessor.
    #[must_use]
    pub fn biome_state(&self, x: i32, z: i32) -> &str {
        debug_assert!((0..16).contains(&x) && (0..16).contains(&z));
        &self.biome_quarts[((z >> 2) * 4 + (x >> 2)) as usize]
    }

    /// Number of vertical quart layers in the 3-D biome grid — `height / 4`,
    /// rounded up, and always at least one.
    #[must_use]
    pub fn biome_y_quarts(&self) -> usize {
        y_quarts_for(self.height)
    }

    /// The distinct biome ids in this column, first-use order — what
    /// [`biome_cell_index`](Self::biome_cell_index) indexes into. A section
    /// encoder resolves each of these to a registry id once and then indexes,
    /// rather than resolving per cell.
    #[must_use]
    pub fn biome_cell_palette(&self) -> &[String] {
        &self.biome_palette
    }

    /// Palette index at quart `(qx, qy, qz)`, `qy` counting up from the bottom
    /// of the column. Every coordinate is clamped into range.
    #[must_use]
    pub fn biome_cell_index(&self, qx: usize, qy: usize, qz: usize) -> u16 {
        let qx = qx.min(3);
        let qz = qz.min(3);
        let qy = qy.min(self.biome_y_quarts().saturating_sub(1));
        self.biome_cells[(qy * 4 + qz) * 4 + qx]
    }

    /// Biome id at quart `(qx, qy, qz)`.
    #[must_use]
    pub fn biome_cell(&self, qx: usize, qy: usize, qz: usize) -> &str {
        &self.biome_palette[self.biome_cell_index(qx, qy, qz) as usize]
    }

    /// Biome id at a block position — local `x`/`z` in `0..16`, world `y`
    /// at any `y`; out-of-column values clamp to the nearest layer, as every
    /// other accessor here does.
    #[must_use]
    pub fn biome_state_at(&self, x: i32, y: i32, z: i32) -> &str {
        let qy = ((y - self.min_y) >> 2).max(0) as usize;
        self.biome_cell((x >> 2) as usize, qy, (z >> 2) as usize)
    }

    /// Overwrites one biome quart cell, interning `name` into the cell palette.
    ///
    /// The counterpart of [`set_biome_quarts`](Self::set_biome_quarts) for the
    /// 3-D grid, and only `crate::chunk_nbt` calls it — restoring the
    /// per-section biome containers read off disk. Out-of-range coordinates are
    /// a silent no-op rather than a panic, because the caller's `qy` comes from
    /// a section index in a file we did not write.
    pub fn set_biome_cell(&mut self, qx: usize, qy: usize, qz: usize, name: &str) {
        if qx >= 4 || qz >= 4 || qy >= self.biome_y_quarts() {
            return;
        }
        let id = match self.biome_palette.iter().position(|p| p == name) {
            Some(i) => i as u16,
            None => {
                self.biome_palette.push(name.to_string());
                (self.biome_palette.len() - 1) as u16
            }
        };
        self.biome_cells[(qy * 4 + qz) * 4 + qx] = id;
    }

    /// Every block entity in this column, at its **absolute** position. Empty for
    /// the overwhelming majority of columns.
    #[must_use]
    pub fn block_entities(&self) -> &[(BlockPos, BlockEntity)] {
        &self.block_entities
    }

    /// Replaces this column's block-entity list.
    ///
    /// `crate::region_source` calls it after reading a chunk off disk, so the
    /// column a client is served carries the same entities the tick-loop
    /// registry just took. Nothing derives block state from this list, so it
    /// cannot desync the block grid.
    pub fn set_block_entities(&mut self, entities: Vec<(BlockPos, BlockEntity)>) {
        self.block_entities = entities;
    }

    /// Materializes the empty records owned by block states in a generated or
    /// edited column but not already represented by a richer sidecar.
    ///
    /// The external chunk lifecycle creates a block entity from the state
    /// itself, including state-only types such as `minecraft:potent_sulfur`.
    /// Generated columns arrive through a bulk palette adoption path, so they
    /// do not pass through the ordinary state-write hook. Keep this repair at
    /// the source boundary, where the chunk coordinates are available to turn
    /// local cells into absolute [`BlockPos`] values. `Nbt::End` is intentional:
    /// an empty update tag is encoded as the network end tag, while the record
    /// still carries the registry type and position.
    pub fn populate_missing_block_entity_states(&mut self, cx: i32, cz: i32) {
        let missing = self.missing_block_entity_states(cx, cz, &self.block_entities);
        if missing.is_empty() {
            return;
        }
        self.block_entities.extend(missing.into_iter().map(|(pos, id)| {
            let id = BlockEntityKind::from_registry_type(id);
            let entity = match id {
                BlockEntityKind::Comparator => BlockEntity::Comparator { output: 0 },
                BlockEntityKind::MobSpawner => {
                    BlockEntity::Spawner(crate::mob_spawner::SpawnerState::default())
                }
                id => BlockEntity::Opaque {
                    id,
                    nbt: lodestone_core::Nbt::End,
                },
            };
            (pos, entity)
        }));
    }

    /// Adds typed world-generation block entities, converted at the source
    /// boundary into the server's own block-entity records.
    pub fn add_generated_block_entities(
        &mut self,
        entities: &[lodestone_worldgen::block_entities::GeneratedBlockEntity],
    ) {
        self.block_entities.extend(
            entities
                .iter()
                .map(crate::chunk_nbt::generated_block_entity),
        );
    }

    /// Returns the exact light snapshot retained for this column, if one has
    /// been captured by a settlement transaction or restored from storage.
    #[must_use]
    pub fn retained_light(&self) -> Option<&lodestone_world::ColumnLight> {
        self.retained_light.as_ref()
    }

    /// Returns the lifecycle stage of the retained light snapshot, if any.
    #[must_use]
    pub fn retained_light_status(&self) -> Option<RetainedLightStatus> {
        self.retained_light_status
    }

    /// Returns the retained light only when this column completed its own
    /// initial admission. Dependency-initialized storage is deliberately not
    /// eligible for packet or final-save consumers: it still has to pass
    /// through the centre admission when this coordinate is requested.
    #[must_use]
    pub fn centre_settled_light(&self) -> Option<&lodestone_world::ColumnLight> {
        (self.retained_light_status == Some(RetainedLightStatus::CentreSettled))
            .then(|| self.retained_light.as_ref())
            .flatten()
    }

    /// Installs a complete light snapshot for this column.
    ///
    /// The snapshot spans the column's block sections plus the one-section
    /// boundary on either side. Callers that obtain light from a protocol
    /// encoder should validate that shape before storing it; the encoder also
    /// validates it before consuming a retained value.
    pub fn set_retained_light(&mut self, light: lodestone_world::ColumnLight) {
        self.set_retained_light_with_status(light, RetainedLightStatus::CentreSettled);
    }

    /// Installs a retained light snapshot with its explicit admission stage.
    pub fn set_retained_light_with_status(
        &mut self,
        light: lodestone_world::ColumnLight,
        status: RetainedLightStatus,
    ) {
        self.retained_light = Some(light);
        self.retained_light_status = Some(status);
    }

    /// Drops the retained light snapshot after a block mutation.
    ///
    /// The next initial send or light update must install a newly fenced
    /// snapshot instead of serving a value computed for the old block grid.
    pub fn clear_retained_light(&mut self) {
        self.retained_light = None;
        self.retained_light_status = None;
    }

    /// Explicit synchronous drain for legacy storage fixtures.
    #[cfg(test)]
    pub fn take_generation_spawns(&mut self) -> Vec<lodestone_worldgen::spawn_stage::GenerationSpawn> {
        self.generation_spawns.as_ref().map_or_else(Vec::new, |batch| batch.take_legacy())
    }

    #[must_use]
    pub fn generation_spawn_batch(&self) -> Option<&Arc<crate::generation_population::GenerationSpawnBatch>> {
        self.generation_spawns.as_ref()
    }

    pub(crate) fn reuse_generation_population(&mut self, previous: &Self) {
        if previous.generation_spawns.is_some() {
            self.generation_spawns = previous.generation_spawns.clone();
        }
    }

    /// Gives this column the one-shot creature candidates its generation proposed.
    pub(crate) fn set_generation_spawns(&mut self, spawns: Vec<lodestone_worldgen::spawn_stage::GenerationSpawn>) {
        self.generation_spawns = crate::generation_population::GenerationSpawnBatch::new(spawns);
    }

    /// Whether this freshly generated column still owns one-shot spawn candidates.
    ///
    /// A persistence adapter must not serialize the block grid and quietly drop
    /// these candidates: doing so changes the first-load population decision.
    /// The native record adapter uses this read-only check to decline such a
    /// column until its schema has a representation for the candidates.
    #[must_use]
    pub(crate) fn has_pending_generation_spawns(&self) -> bool {
        self.generation_spawns.as_ref().is_some_and(|batch| batch.is_pending())
    }

    /// This column's `MOTION_BLOCKING` heightmap in vanilla's stored form, or
    /// `None` if it did not come from the generator — see
    /// [`motion_blocking`](Self::motion_blocking) for the whole contract and
    /// `docs/motion-blocking-heightmap.md` for the `+1`.
    #[must_use]
    pub fn motion_blocking(&self) -> Option<&[u16; 256]> {
        self.motion_blocking.as_deref()
    }

    /// Retained client-visible heightmaps for a generated column.
    #[must_use]
    pub fn client_heightmaps(&self) -> Option<&lodestone_world::Heightmaps> {
        self.client_heightmaps.as_ref()
    }

    /// Return the retained client maps as authenticated raw cells in wire
    /// registry order (`WORLD_SURFACE`, `MOTION_BLOCKING`,
    /// `MOTION_BLOCKING_NO_LEAVES`). This boundary preserves map transitions
    /// captured independently of a later block-field scan.
    #[must_use]
    pub fn client_heightmaps_raw(&self) -> Option<[[u16; 256]; 3]> {
        let maps = self.client_heightmaps.as_ref()?;
        let mut raw = [[0u16; 256]; 3];
        for (map_index, type_id) in [1u32, 4, 5].into_iter().enumerate() {
            let map = maps.get(type_id)?;
            for z in 0..16usize {
                for x in 0..16usize {
                    raw[map_index][x + z * 16] = u16::try_from(map.get(x, z)).ok()?;
                }
            }
        }
        Some(raw)
    }

    /// Install an authenticated raw client-map snapshot and synchronize the
    /// generator's stored `MOTION_BLOCKING` view with the registry-id-4 map.
    pub fn install_client_heightmaps_raw(&mut self, raw: [[u16; 256]; 3]) {
        let mut maps = lodestone_world::Heightmaps::new();
        for (map_index, type_id) in [1u32, 4, 5].into_iter().enumerate() {
            let mut map = lodestone_world::Heightmap::new(self.height as u32);
            for z in 0..16usize {
                for x in 0..16usize {
                    map.set(x, z, u32::from(raw[map_index][x + z * 16]));
                }
            }
            maps.insert(type_id, map);
        }
        self.client_heightmaps = Some(maps);
        if let Some(motion) = self.motion_blocking.as_mut() {
            motion.copy_from_slice(&raw[1]);
        }
    }

    /// Prime the three client-visible heightmaps from the column's current
    /// resident block field.
    ///
    /// Generation lifecycle callers invoke this exactly when the column enters
    /// FEATURES. Subsequent [`Self::set_block`] calls then maintain only the
    /// affected XZ cell. This is deliberately separate from construction:
    /// neighbouring chunks can enter FEATURES and write here before this
    /// column enters the stage itself, while writes after entry must update the
    /// retained maps rather than being erased by a final-state rescan.
    pub fn prime_client_heightmaps(&mut self) {
        self.client_heightmaps = Some(derive_client_heightmaps(self));
        if let (Some(maps), Some(motion)) =
            (self.client_heightmaps.as_ref(), self.motion_blocking.as_mut())
        {
            if let Some(map) = maps.get(CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID) {
                for z in 0..16usize {
                    for x in 0..16usize {
                        motion[x + z * 16] = map.get(x, z) as u16;
                    }
                }
            }
        }
    }

    /// Derives the three client heightmaps and the retained `MOTION_BLOCKING`
    /// map from the column's current blocks, for a column loaded without
    /// them. Afterwards [`Self::set_block_id`] keeps both current cell by cell.
    pub fn derive_heightmaps(&mut self) {
        self.client_heightmaps = Some(derive_client_heightmaps(self));
        if let Some(raw) = self.client_heightmaps_raw() {
            self.set_motion_blocking(raw[1]);
        }
    }

    /// Restores the generator's stored `MOTION_BLOCKING` answer.
    ///
    /// The values use the persisted `top_y + 1` convention and are indexed by
    /// `local_x + local_z * 16`. This is intentionally a whole-map replacement:
    /// the heightmap is a derived snapshot, not an edit-time cache, so callers
    /// that load it must restore all 256 cells or leave the answer absent.
    pub fn set_motion_blocking(&mut self, heights: [u16; 256]) {
        self.motion_blocking = Some(Box::new(heights));
        if let Some(maps) = self.client_heightmaps.as_mut() {
            if let Some(map) = maps.get_mut(CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID) {
                for z in 0..16usize {
                    for x in 0..16usize {
                        map.set(x, z, u32::from(heights[x + z * 16]));
                    }
                }
            }
        }
    }

    fn refresh_client_heightmaps_at(&mut self, x: i32, z: i32) {
        if self.client_heightmaps.is_none() {
            return;
        }
        let scan_height = self.air_above_y().min(self.min_y + self.height) - self.min_y;
        self.refresh_client_heightmaps_at_bounded(x, z, scan_height);
    }

    fn refresh_client_heightmaps_at_bounded(&mut self, x: i32, z: i32, scan_height: i32) {
        if !(0..16).contains(&x) || !(0..16).contains(&z) {
            return;
        }
        let Some(mut maps) = self.client_heightmaps.take() else {
            return;
        };
        #[cfg(test)]
        HEIGHTMAP_REPAIRS.with(|c| c.set(c.get() + 1));
        let values = client_heightmap_values_at(self.min_y, scan_height, |y| {
            self.block_state_id(x, y, z)
        });
        for (type_id, stored) in [
            CLIENT_WORLD_SURFACE_HEIGHTMAP_TYPE_ID,
            CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID,
            CLIENT_MOTION_BLOCKING_NO_LEAVES_HEIGHTMAP_TYPE_ID,
        ]
        .into_iter()
        .zip(values)
        {
            if let Some(map) = maps.get_mut(type_id) {
                map.set(x as usize, z as usize, stored);
            }
            if type_id == CLIENT_MOTION_BLOCKING_HEIGHTMAP_TYPE_ID {
                if let Some(motion) = self.motion_blocking.as_mut() {
                    motion[x as usize + z as usize * 16] = stored as u16;
                }
            }
        }
        self.client_heightmaps = Some(maps);
    }

    /// Structure starts originating in this column.
    #[must_use]
    pub fn structure_starts(
        &self,
    ) -> &[std::sync::Arc<lodestone_worldgen::structure::StructureStart>] {
        &self.structure_starts
    }

    /// This column's `structures.References`: structure id → packed origin-chunk
    /// keys.
    #[must_use]
    pub fn structure_references(&self) -> &std::collections::BTreeMap<String, Vec<i64>> {
        &self.structure_references
    }

    /// Attaches the structure placement answer for this column's own chunk
    /// coordinates.
    ///
    /// Called by the generating path of [`Terrain263ChunkSource`], which is the only place that
    /// holds both the column and the `(cx, cz)` the generator needs. Purely
    /// additive: nothing derives a block from this, so a source that does not
    /// call it serves a column whose `structures` compound is empty — which is
    /// what every source other than the 26.3 generator does.
    pub fn set_structures(
        &mut self,
        starts: Vec<std::sync::Arc<lodestone_worldgen::structure::StructureStart>>,
        references: std::collections::BTreeMap<String, Vec<i64>>,
    ) {
        self.structure_starts = starts;
        self.structure_references = references;
    }

    /// Which 16-row window a `y - min_y` offset falls in. The windows are
    /// measured from `min_y`, not from world y = 0, which is the same
    /// arithmetic the section histogram and `crate::random_tick`'s walk use — change them together.
    #[inline]
    fn section_index(y_local: i32) -> usize {
        y_local as usize / SECTION_ROWS
    }

    /// Interns a validated canonical state into this column's palette.
    fn intern_state_id(&mut self, state: lodestone_data::block_states::StateId) -> u16 {
        if let Some(index) = self.palette.iter().position(|&candidate| candidate == state) {
            return index as u16;
        }
        #[cfg(test)]
        INTERN_CALLS.with(|c| c.set(c.get() + 1));
        self.palette.push(state);
        let (ticking, reaction) = state_metadata(state);
        self.palette_ticking.push(ticking);
        self.palette_reaction.push(reaction);
        debug_assert_eq!(self.palette.len(), self.palette_ticking.len());
        debug_assert_eq!(self.palette.len(), self.palette_reaction.len());
        (self.palette.len() - 1) as u16
    }

    /// Sets a block from its validated canonical state id.
    pub fn set_block_id(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) {
        self.clear_retained_light();
        let id = self.intern_state_id(state);
        self.write_block_id(x, y, z, id);
        self.refresh_client_heightmaps_at(x, z);
    }

    /// Applies ordered writes that already carry validated canonical state ids.
    /// Palette lookup happens once per distinct id, while cell writes remain a
    /// packed integer operation and heightmaps are repaired once per XZ cell.
    pub fn apply_ordered_block_id_batch(
        &mut self,
        writes: &[(i32, i32, i32, lodestone_data::block_states::StateId)],
    ) {
        if writes.is_empty() {
            return;
        }
        for &(x, y, z, _) in writes {
            assert!((0..16).contains(&x), "batch block x coordinate out of range: {x}");
            assert!((0..16).contains(&z), "batch block z coordinate out of range: {z}");
            assert!(self.contains_y(y), "batch block y coordinate out of range: {y}");
        }

        self.clear_retained_light();
        let mut interned = Vec::new();
        let mut dirty = Vec::with_capacity(writes.len().min(256));
        let mut seen = [false; 256];
        for &(x, y, z, state) in writes {
            let id = match interned.iter().find(|(candidate, _)| *candidate == state) {
                Some((_, id)) => *id,
                None => {
                    let id = self.intern_state_id(state);
                    interned.push((state, id));
                    id
                }
            };
            self.write_block_id(x, y, z, id);
            let index = x as usize + z as usize * 16;
            if !seen[index] {
                seen[index] = true;
                dirty.push(index as u16);
            }
        }
        dirty.sort_unstable();
        if self.client_heightmaps.is_none() {
            return;
        }
        let scan_height = self.air_above_y().min(self.min_y + self.height) - self.min_y;
        for index in dirty {
            self.refresh_client_heightmaps_at_bounded(
                (index as usize & 15) as i32,
                (index as usize >> 4) as i32,
                scan_height,
            );
        }
    }

    /// Whether `y` lies inside this column's stored vertical extent.
    ///
    /// Callers that mutate a retained column use this before [`Self::set_block`]
    /// so an out-of-height request is rejected rather than indexing a section
    /// that does not exist.
    #[must_use]
    pub fn contains_y(&self, y: i32) -> bool {
        (self.min_y..self.min_y.saturating_add(self.height)).contains(&y)
    }

    /// Writes an already-interned column-wide palette `id` at a local `(x, z)`
    /// in `0..16` and world `y`.
    fn write_block_id(&mut self, x: i32, y: i32, z: i32, id: u16) {
        let y_local = y - self.min_y;
        let old = self.blocks.get(x, y_local, z);
        self.blocks.set(x, y_local, z, id);

        // Vanilla's `LevelChunkSection.setBlockState` (`:58-102`) maintains
        // `tickingBlockCount` exactly here: decrement for the state leaving the
        // cell, increment for the one arriving. Both classifications are
        // already cached per palette id, so this is two array reads and at most
        // one `±1`. A same-state rewrite, and any ticking→ticking or
        // non-ticking→non-ticking replacement, is a no-op by construction —
        // `was == now` — with no special case needed.
        let was = self.palette_ticking[old as usize];
        let now = self.palette_ticking[id as usize];
        if was != now {
            let section = Self::section_index(y_local);
            if now {
                self.section_ticking[section] += 1;
            } else {
                // Plain `-=` behind a `debug_assert!`, deliberately **not**
                // `saturating_sub`. Saturation would silently absorb precisely
                // the maintenance bug this counter exists to prevent — a
                // mutation path that incremented on the way in but not on the
                // way out — converting a loud panic at the offending write into
                // a section that quietly stops random-ticking forever. Do not
                // "harden" this.
                debug_assert!(
                    self.section_ticking[section] > 0,
                    "section_ticking[{section}] underflowed writing {:?} at ({x}, {y}, {z}): a \
                     randomly-ticking state left a cell the counter did not know held one, so \
                     some mutation path reached `blocks` without `set_block` or \
                     final section histogram initialization",
                    self.palette[id as usize]
                );
                self.section_ticking[section] -= 1;
            }
        }
    }

    /// Interns a whole section's local state-id palette into the column-wide
    /// palette — `local.len()` calls to [`intern_state_id`](Self::intern_state_id), not one per cell —
    /// then writes every one of the section's cells from the resulting remap.
    ///
    /// The load-path mirror of [`palette`](Self::palette)/
    /// [`append_section_cells`](Self::append_section_cells):
    /// [`crate::chunk_nbt`]'s loader interns each section palette once, rather
    /// than scanning the whole column-wide palette for all 98,304 cells.
    ///
    /// `indices` is one entry per cell in vanilla's own `(y_in_section << 8) |
    /// (z << 4) | x` order (what `chunk_nbt::unpack_indices` already returns),
    /// indexing into `local` — **not** into the column-wide palette. Every
    /// entry must be `< local.len()`; callers validate that against the NBT
    /// before calling, so this indexes unchecked.
    pub fn set_section_from_local_palette(&mut self, y_base: i32, local: &[StateId], indices: &[u16]) {
        if indices.is_empty() {
            return;
        }
        self.clear_retained_light();
        let refresh_heightmaps = self.client_heightmaps.is_some();
        let mut dirty = [false; 256];
        let remap: Vec<u16> = local
            .iter()
            .copied()
            .map(|state| self.intern_state_id(state))
            .collect();
        for (cell, &local_index) in indices.iter().enumerate() {
            let id = remap[local_index as usize];
            let ly = (cell >> 8) as i32;
            let lz = ((cell >> 4) & 15) as i32;
            let lx = (cell & 15) as i32;
            self.write_block_id(lx, y_base + ly, lz, id);
            dirty[lx as usize + lz as usize * 16] = true;
        }
        if refresh_heightmaps {
            let scan_height = self.air_above_y().min(self.min_y + self.height) - self.min_y;
            for (index, &is_dirty) in dirty.iter().enumerate() {
                if is_dirty {
                    self.refresh_client_heightmaps_at_bounded(
                        (index & 15) as i32,
                        (index >> 4) as i32,
                        scan_height,
                    );
                }
            }
        }
    }

    /// Sets solidity at a local `(x, z)` in `0..16` and world `y`. `true` writes
    /// canonical stone, `false` writes air — the solid/air view preserved for
    /// callers that only reason about collidable terrain.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        self.set_block_id(
            x,
            y,
            z,
            if solid { stone_state() } else { air_state() },
        );
    }

    /// The **global 26.2 block-state id** at a local `(x, z)` in `0..16` and
    /// world `y`; this is what a protocol encoder should call. Out-of-range Y
    /// is air's id.
    ///
    /// Two array indexes and a range check: no string, no hash, no scan. The
    /// resolution happened once per palette entry — see
    /// [`palette`](Self::palette).
    #[must_use]
    pub fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return air_state();
        }
        self.palette[self.blocks.get(x, y_local, z) as usize]
    }

    /// First world Y at and above which block storage is all air.
    #[must_use]
    pub fn air_above_y(&self) -> i32 {
        self.min_y + (self.blocks.air_ceiling_section() * SECTION_ROWS) as i32
    }

    /// Which redstone family, if any, a neighbour notification landing at a
    /// local `(x, z)` in `0..16` and world `y` dispatches to. Out-of-range Y
    /// is air's class.
    ///
    /// Two array indexes and a range check: no string allocation, no
    /// `base_name` split, no `strcmp`. The classification happened once per
    /// palette entry — see
    /// [`palette_reaction`](Self::palette_reaction) and
    /// [`crate::redstone_graph`].
    #[must_use]
    pub(crate) fn reaction_class(&self, x: i32, y: i32, z: i32) -> crate::redstone_graph::ReactionClass {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return crate::redstone_graph::ReactionClass::Inert;
        }
        self.palette_reaction[self.blocks.get(x, y_local, z) as usize]
    }

    #[must_use]
    pub fn palette(&self) -> &[StateId] {
        &self.palette
    }

    /// Returns solidity at a local `(x, z)` in `0..16` and world `y`. A block is
    /// solid when it is neither air nor a fluid; blocks outside the vertical
    /// range are non-solid.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        !is_air_or_fluid_id(self.block_state_id(x, y, z))
    }

    /// Total number of solid (non-air, non-fluid) blocks.
    #[must_use]
    pub fn solid_count(&self) -> usize {
        // Classify the palette once, then count integer cells instead of
        // rechecking state properties for all 98,304 positions.
        let solid: Vec<bool> = self
            .palette
            .iter()
            .map(|&state| !is_air_or_fluid_id(state))
            .collect();
        let mut count = 0usize;
        for s in 0..self.blocks.section_count() {
            self.blocks.for_each_in_section(s, |_, id| {
                if solid[id as usize] {
                    count += 1;
                }
            });
        }
        count
    }

    /// Absolute positions and vanilla `minecraft:block_entity_type` registry
    /// keys for every cell in this column whose block state owns a block
    /// entity ([`lodestone_data::block_entity_types::block_entity_type`]) but
    /// is not among `existing`.
    ///
    /// Vanilla's `LevelChunk.setBlockState` creates a block entity from the
    /// *state* alone, for every block-entity type — not only the dozen this
    /// crate simulates real behaviour for
    /// ([`crate::block_entities::block_entity_for_item`]'s own scope note
    /// names them: furnace family, hopper, composter, brewing stand, the
    /// three containers, command block, spawner, sign, beacon, crafter). A
    /// skull, banner, jukebox, decorated pot, … placed before a registry
    /// entry existed for it (or one this crate has never modelled a
    /// placement-time entry for at all) can therefore reach a served column
    /// with a correct state and zero record. That is not cosmetic: a
    /// block-entity-rendered block draws nothing at all client-side without
    /// one, until an unrelated later block update lets the client
    /// synthesize an empty record for itself off the state — this is the
    /// gap behind that "invisible until interacted with" symptom.
    ///
    /// The palette is classified once — the same argument
    /// [`solid_count`](Self::solid_count) already makes for its own
    /// predicate — so a column with no block-entity-owning state in its
    /// palette (the overwhelming majority) costs one pass over the palette
    /// and no cell scan at all.
    #[must_use]
    pub fn missing_block_entity_states(
        &self,
        cx: i32,
        cz: i32,
        existing: &[(BlockPos, BlockEntity)],
    ) -> Vec<(
        BlockPos,
        lodestone_data::block_entity_types::BlockEntityType,
    )> {
        let types: Vec<Option<lodestone_data::block_entity_types::BlockEntityType>> = self
            .palette
            .iter()
            .map(|&id| lodestone_data::block_entity_types::block_entity_type(id))
            .collect();
        if types.iter().all(Option::is_none) {
            return Vec::new();
        }
        const ROW_CELLS: usize = 16 * 16;
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut out = Vec::new();
        for s in 0..self.blocks.section_count() {
            self.blocks.for_each_in_section(s, |cell, id| {
                let Some(type_id) = types[id as usize] else {
                    return;
                };
                let row_local = cell / ROW_CELLS;
                let rem = cell % ROW_CELLS;
                let local_z = (rem / 16) as i32;
                let local_x = (rem % 16) as i32;
                let y = self.min_y + (s * SECTION_ROWS + row_local) as i32;
                let pos = BlockPos::new(base_x + local_x, y, base_z + local_z);
                if !existing.iter().any(|(p, _)| *p == pos) {
                    out.push((pos, type_id));
                }
            });
        }
        out
    }

    /// 16-row sections in this column — `height / 16`, rounded up. The same
    /// windows [`section_ticking_counts`](Self::section_ticking_counts) indexes.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.blocks.section_count()
    }

    /// Returns the column-palette index when section `s` is uniform, or
    /// `None` when its packed cells carry more than one value. Read-only
    /// schedulers use this to skip uniform sections without materialising
    /// their repeated indices.
    #[inline]
    pub(crate) fn uniform_section_palette_index(&self, s: usize) -> Option<u16> {
        self.blocks.uniform_id(s)
    }

    /// Visits section `s`'s palette indices in block-storage order without
    /// allocating a temporary cell vector. Uniform sections are expanded by
    /// the storage layer only when the caller has decided their value matters.
    pub(crate) fn for_each_section_palette_index(
        &self,
        s: usize,
        f: impl FnMut(usize, u16),
    ) {
        self.blocks.for_each_in_section(s, f);
    }

    /// Appends section `s`'s palette indices to `out`, in vanilla's own
    /// `(y_in_section << 8) | (z << 4) | x` order — so `crate::chunk_nbt` builds a
    /// region file's per-section container straight from it.
    ///
    /// Persistence adapters walk the canonical palette and this index grid
    /// directly, resolving names only while encoding the external format.
    ///
    /// **This replaced a `raw_blocks() -> &[u16]` over the whole column**, which
    /// could not survive the sectioned representation (`crate::chunk_blocks`) —
    /// there is no longer one contiguous grid to borrow, and materialising one
    /// would reintroduce the 192 KiB the change exists to remove. Callers that
    /// walked the flat grid section-by-section (all of them did) want this;
    /// `out` is reused across sections so the whole save path is one allocation.
    pub fn append_section_cells(&self, s: usize, out: &mut Vec<u16>) {
        self.blocks.append_section_cells(s, out);
    }

    /// Heap bytes this column's block grid owns.
    ///
    /// A **count**, so a gate can assert the representation's cost without an RSS
    /// reading and without depending on machine load (a flat grid would be
    /// `16 × 16 × height × 2` bytes).
    #[must_use]
    pub fn blocks_heap_bytes(&self) -> usize {
        self.blocks.heap_bytes()
    }

    /// Returns the logical resident-memory census for this column.
    ///
    /// The values are allocation capacities, not an RSS reading: they include
    /// spare `Vec` capacity and direct string payloads, but deliberately exclude
    /// allocator metadata and shared built-in state text. Nested payloads owned
    /// by block entities and structure pieces are reported through their outer
    /// slots only; those are normally absent on a freshly generated terrain
    /// column and need their owning subsystem's census when present.
    #[must_use]
    pub fn memory_census(&self) -> ChunkColumnMemory {
        fn string_bytes(value: &str, capacity: usize) -> usize {
            capacity.max(value.len())
        }

        let block_palette_slots = self.palette.capacity()
            * size_of::<lodestone_data::block_states::StateId>();
        let block_palette_text = 0;
        let block_derived = self.palette_ticking.capacity() * size_of::<bool>()
            + self.palette_reaction.capacity() * size_of::<crate::redstone_graph::ReactionClass>();
        let custom_arc_payload = 0;

        let biome_surface_text = self
            .biome_quarts
            .iter()
            .map(|state| string_bytes(state, state.capacity()))
            .sum();
        let biome_palette_slots = self.biome_palette.capacity() * size_of::<String>();
        let biome_palette_text = self
            .biome_palette
            .iter()
            .map(|state| string_bytes(state, state.capacity()))
            .sum();
        let block_entities_slots =
            self.block_entities.capacity() * size_of::<(BlockPos, BlockEntity)>();
        let structure_slots = self.structure_starts.capacity()
            * size_of::<std::sync::Arc<lodestone_worldgen::structure::StructureStart>>();
        let structure_refs = self
            .structure_references
            .iter()
            .map(|(id, references)| {
                id.capacity()
                    + references.capacity() * size_of::<i64>()
                    + size_of::<(String, Vec<i64>)>()
            })
            .sum();
        let generation_spawns = self.generation_spawns.as_ref().map_or(0, |batch| batch.memory_bytes());

        ChunkColumnMemory {
            inline_bytes: size_of::<Self>(),
            blocks_bytes: self.blocks.heap_bytes(),
            block_palette_slots_bytes: block_palette_slots,
            block_palette_text_bytes: block_palette_text,
            block_derived_bytes: block_derived,
            custom_arc_payload_bytes: custom_arc_payload,
            section_ticking_bytes: self.section_ticking.capacity() * size_of::<u16>(),
            biome_surface_text_bytes: biome_surface_text,
            biome_palette_slots_bytes: biome_palette_slots,
            biome_palette_text_bytes: biome_palette_text,
            biome_cells_bytes: self.biome_cells.capacity() * size_of::<u16>(),
            block_entities_slots_bytes: block_entities_slots,
            structure_slots_bytes: structure_slots,
            structure_refs_bytes: structure_refs,
            motion_blocking_bytes: self
                .motion_blocking
                .as_ref()
                .map_or(0, |heights| size_of_val(heights.as_ref())),
            generation_spawns_bytes: generation_spawns,
            retained_light_bytes: self
                .retained_light
                .as_ref()
                .map_or(0, lodestone_world::ColumnLight::heap_bytes),
        }
    }

    /// Vanilla's own is-randomly-ticking boolean for the 16-row window
    /// whose lowest row is world `section_min_y` — the per-section ticking
    /// counter being greater than zero.
    ///
    /// **O(1): one integer compare.** The counters expose the answer directly;
    /// scanning up to 4096 cells per section, per column, per tick would make
    /// this query proportional to section size. A
    /// `section_min_y` outside this column is `false`.
    #[must_use]
    pub fn section_is_randomly_ticking(&self, section_min_y: i32) -> bool {
        let y_local = section_min_y - self.min_y;
        if y_local < 0 {
            return false;
        }
        self.section_ticking
            .get(Self::section_index(y_local))
            .is_some_and(|&count| count > 0)
    }

    /// `true` if any 16-row window in this column holds a randomly-ticking
    /// state — the whole-column early exit taken before any section is walked.
    /// At most `height / 16` integer compares (24 for a full overworld column).
    #[must_use]
    pub fn has_randomly_ticking_block(&self) -> bool {
        self.section_ticking.iter().any(|&count| count > 0)
    }

    /// The raw per-section ticking counts, indexed by 16-row window from
    /// `min_y`.
    ///
    /// Production code never reads the count itself, only whether it is
    /// positive ([`section_is_randomly_ticking`](Self::section_is_randomly_ticking)).
    /// This exists for the permanent parity gate
    /// (`tests/random_tick_section_counters.rs`), which compares every count
    /// against an independent recount walking
    /// [`append_section_cells`](Self::append_section_cells)
    /// — a *boolean*-only accessor would let a count drift by any amount
    /// without the gate noticing as long as it stayed on the same side of zero.
    #[must_use]
    pub fn section_ticking_counts(&self) -> &[u16] {
        &self.section_ticking
    }

    /// **Test hook. Deliberately corrupts the ticking counter for one section.**
    ///
    /// It exists for one purpose: to be the negative control of the parity gate
    /// in `tests/random_tick_section_counters.rs`. An assertion that two things
    /// agree is worth nothing without evidence the comparison can fail, and the
    /// only way to produce a genuine desync is to break the invariant on the
    /// **production** side — corrupting the gate's own recount instead would
    /// pass even if the gate were accidentally comparing the recount to itself.
    ///
    /// It is plain `pub` rather than `#[cfg(test)]` because an integration test
    /// is a separate crate and cannot see `#[cfg(test)]` items; the census test
    /// `no_production_code_corrupts_the_ticking_counter` in that same file
    /// keeps it out of `src/` permanently. **No production caller may exist.**
    /// After calling this the column's counters are wrong by `delta` and every
    /// random-tick decision derived from them is unsound.
    #[doc(hidden)]
    pub fn debug_corrupt_section_ticking_count(&mut self, section_index: usize, delta: i32) {
        let slot = &mut self.section_ticking[section_index];
        *slot = (i32::from(*slot) + delta) as u16;
    }

    /// The 16 per-quart biome ids, row-major `qz * 4 + qx`.
    #[must_use]
    pub fn biome_quarts(&self) -> &[String; 16] {
        &self.biome_quarts
    }

    /// Overwrites the per-quart biome ids from a slice of at least 16 entries;
    /// shorter slices leave the remaining quarts untouched.
    ///
    /// Only [`crate::chunk_nbt`] calls this, restoring biomes read off disk.
    /// It is not a gameplay mutation and has no `set_block`-style persistence
    /// path — a loaded column carries its biomes, a generated one gets them
    /// from the generator, and nothing else changes them.
    pub fn set_biome_quarts(&mut self, quarts: &[String]) {
        for (slot, value) in self.biome_quarts.iter_mut().zip(quarts) {
            slot.clone_from(value);
        }
    }
}

/// The result of trying to install a retained light snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnLightSettlementError {
    /// The protocol could not produce a light snapshot for the column.
    NoLight,
    /// A resident-only settlement could not assemble every contributing column.
    MissingFootprint,
    /// A write changed the column after the snapshot was captured.
    Conflict,
}

/// A deferred resident-light capture or rejected cooperative commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentLightTransactionError {
    /// A short source lock or an input coordinate is currently held.
    Busy,
    /// The complete input halo is not resident in the owning cache.
    MissingFootprint,
    /// An input changed after capture; none of the outputs were installed.
    Conflict,
    /// The requested footprint or returned output set is invalid.
    InvalidOutputs,
}

/// A stable terrain capture for a cooperative resident-light solve.
///
/// Capturing and committing never wait, generate terrain, or hydrate storage.
/// No source lock or coordinate gate remains held between these operations.
/// Implementations retain light only for the selected outputs and preserve
/// terrain. Cache-owned light is transient and need not reach durable storage.
pub trait ResidentLightTransaction: Send {
    /// Complete input halo in absolute column coordinates, stable until drop.
    fn columns(&self) -> &[(i32, i32, ChunkColumn)];

    /// Installs exactly the selected outputs if every input is still current.
    fn commit(
        self: Box<Self>,
        lights: Vec<((i32, i32), lodestone_world::ColumnLight)>,
    ) -> Result<(), ResidentLightTransactionError>;
}

/// An owner-held capture for detached initial-packet preparation.
/// No gate or source lock survives capture. `Busy` leaves the same prepared
/// result retryable; a changed or missing input requires a fresh capture.
pub trait InitialPacketTransaction: Send {
    /// Complete captured input in sorted absolute column coordinates.
    fn columns(&self) -> &[(i32, i32, ChunkColumn)];

    /// Validates every input and retains the optional initial-light product.
    /// `None` validates a packet that reused settled light or produced no light.
    fn try_commit(
        &mut self,
        settlement: Option<&ColumnLightSettlement>,
    ) -> Result<(), ResidentLightTransactionError>;
}

/// Light snapshots produced for one admitted chunk footprint.
///
/// The centre entry is always present. Additional entries use chunk-relative
/// offsets in the admitted 3x3 neighbourhood, so a version adapter can return
/// every snapshot it produced while the source still owns one revision-checked
/// transaction. The constructor rejects offsets outside that footprint and
/// duplicate coordinates instead of leaving a source to interpret an
/// untyped coordinate list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnLightSettlement {
    entries: Vec<((i32, i32), lodestone_world::ColumnLight)>,
}

impl ColumnLightSettlement {
    /// Creates a centre-only settlement, preserving the existing protocol
    /// behaviour for adapters that compute one column at a time.
    #[must_use]
    pub fn centre(light: lodestone_world::ColumnLight) -> Self {
        Self {
            entries: vec![((0, 0), light)],
        }
    }

    /// Creates a settlement with the centre and any additional 3x3 entries.
    ///
    /// `neighbours` contains `(dx, dz, light)` triples. `(0, 0)` is reserved
    /// for `centre`; offsets outside `-1..=1` or duplicate coordinates return
    /// `None`. A caller can therefore construct a batch from a version-specific
    /// solver without exposing an unchecked coordinate-to-light map to the
    /// source transaction.
    pub fn with_neighbours(
        centre: lodestone_world::ColumnLight,
        neighbours: impl IntoIterator<Item = (i32, i32, lodestone_world::ColumnLight)>,
    ) -> Option<Self> {
        let mut entries = vec![((0, 0), centre)];
        for (dx, dz, light) in neighbours {
            if !(-1..=1).contains(&dx) || !(-1..=1).contains(&dz) || (dx, dz) == (0, 0) {
                return None;
            }
            if entries.iter().any(|(offset, _)| *offset == (dx, dz)) {
                return None;
            }
            entries.push(((dx, dz), light));
        }
        Some(Self { entries })
    }

    /// The centre snapshot, which every valid settlement contains.
    #[must_use]
    pub fn centre_light(&self) -> &lodestone_world::ColumnLight {
        &self.entries[0].1
    }

    /// Iterates over `(dx, dz, light)` entries, with the centre first.
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = ((i32, i32), &lodestone_world::ColumnLight)> + '_ {
        self.entries.iter().map(|(offset, light)| (*offset, light))
    }

    /// Iterates over the dependency snapshots without yielding the centre.
    ///
    /// This read-only view is intentionally hidden from generated API docs:
    /// parity and diagnostic consumers need to inspect the exact settlement
    /// boundary, while normal callers should continue to use
    /// [`Self::centre_light`] and let the source own dependency persistence.
    #[doc(hidden)]
    pub fn dependency_lights(
        &self,
    ) -> impl Iterator<Item = ((i32, i32), &lodestone_world::ColumnLight)> + '_ {
        self.entries
            .iter()
            .skip(1)
            .map(|(offset, light)| (*offset, light))
    }
}

/// Supplies terrain columns to the integrated server.
pub trait ChunkSource: Send + Sync {
    /// Generates the column at chunk coordinates `(cx, cz)`.
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn;

    /// Retains population-bearing terrain on a generation worker, before
    /// publication. Completion must survive cache eviction.
    fn retain_generation_population(&self, _cx: i32, _cz: i32, _column: &mut ChunkColumn) -> bool {
        false
    }

    /// Bounded, nonblocking discovery of authoritative unclaimed population.
    fn pending_generation_spawn_batches(
        &self,
        _limit: usize,
    ) -> Vec<Arc<crate::generation_population::GenerationSpawnBatch>> {
        Vec::new()
    }

    /// The block position of the stronghold start nearest `from`, or `None` when
    /// this source places no strongholds (every non-overworld source, and any
    /// overworld without a structure registry).
    ///
    /// This is the target an eye of ender flies toward. The answer is the
    /// placement's own candidate list, not a scan of generated chunks, so it is
    /// valid before the stronghold's chunks exist.
    fn locate_stronghold(&self, _from: BlockPos) -> Option<BlockPos> {
        None
    }

    /// The block column a fresh world's spawn search starts from: the
    /// generator's climate-targeted position, or `None` when the source has no
    /// spawn targets (the search then starts at the origin).
    fn spawn_origin_block(&self) -> Option<(i32, i32)> {
        None
    }

    /// Answers a cheap distant-terrain surface query without materialising a
    /// chunk. `None` means this source does not expose a faithful surface
    /// estimate.
    fn horizon_sample(&self, _x: i32, _z: i32) -> Option<HorizonSample> {
        None
    }

    /// Generates several columns in the caller's exact coordinate order.
    /// Sources with shared dependency windows override this hook; the default
    /// preserves every existing source's scalar behavior.
    fn columns(&self, coords: &[(i32, i32)]) -> Vec<ChunkColumn> {
        coords
            .iter()
            .map(|&(cx, cz)| self.column(cx, cz))
            .collect()
    }

    /// Generates a column through at least `stage`.
    ///
    /// The default is intentionally conservative for sources that have no
    /// progressive generator: they return their normal, full column. Wrappers
    /// around a progressive source must forward this method explicitly; the
    /// production forwarding implementations below make that requirement
    /// testable without forcing every small test world to implement a second
    /// method.
    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        let _ = stage;
        self.column(cx, cz)
    }

    /// Returns the stage a source must reach before a partial column can be
    /// encoded into a packet. Most sources can encode their shaped product as
    /// returned; sources whose decoration reads and writes a neighbouring
    /// source must finish the target through their normal ordered full-column
    /// path first. The default keeps progressive streaming unchanged.
    fn packet_generation_stage(
        &self,
        _stage: ChunkGenerationStage,
    ) -> Option<ChunkGenerationStage> {
        None
    }

    /// Reads a single canonical state id at world coordinates
    /// `(x, y, z)`, through the same data [`column`](Self::column) would
    /// return — including any edit already applied via
    /// [`set_block`](Self::set_block).
    ///
    /// This is a required method so no implementor silently inherits a
    /// whole-column regeneration for a one-block read. An implementor with a cheaper path, one that
    /// reads a cell out of a column it already retains, must override this
    /// to avoid regenerating on every probe: the `ChunkStore` wrapper is the
    /// reference example. An implementor with no cheaper path implements it
    /// as `self.column(cx, cz).block_state_id(..)`, which is correct if
    /// column-sized; the point is that the choice is explicit at every
    /// implementor rather than silently inherited.
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId;

    /// Reads a retained block without loading or generating a column. `None`
    /// means unavailable, unsupported, or outside the retained column's height.
    /// Cache wrappers override this atomically; forwarding wrappers must forward.
    fn resident_block_state_id(&self, _x: i32, _y: i32, _z: i32) -> Option<lodestone_data::block_states::StateId> {
        None
    }

    /// Clones an already-resident column without generating a cache miss.
    ///
    /// This is intentionally narrower than [`column`](Self::column): a caller
    /// enriching an answer with loaded neighbours must preserve the unloaded
    /// result rather than turning one request into eight loads.
    fn resident_column(&self, _cx: i32, _cz: i32) -> Option<ChunkColumn> {
        None
    }

    /// Attempts a resident-only column snapshot through a source with an
    /// atomic admission boundary. `None` means the source keeps the legacy
    /// capability surface, so callers may preserve their existing fallback.
    /// `Some(Busy)` and `Some(Absent)` are distinct outcomes for a source such
    /// as [`crate::chunk_store::ChunkStore`] that must not let a race turn into
    /// blocking generation.
    fn try_resident_column(
        &self,
        _cx: i32,
        _cz: i32,
    ) -> Option<crate::chunk_store::TryResident<ChunkColumn>> {
        None
    }

    /// Captures a complete bounded halo for light work that may yield.
    /// `None` preserves the legacy source capability surface. A capable source
    /// returns a nonblocking transaction or an explicit deferral/rejection.
    fn try_begin_resident_light(
        &self,
        _outputs: &[(i32, i32)],
        _inputs: &[(i32, i32)],
    ) -> Option<Result<Box<dyn ResidentLightTransaction + '_>, ResidentLightTransactionError>> {
        None
    }

    /// Captures the centre and requested relative neighbours without waiting,
    /// generation, or disk hydration. Supported deferrals never imply a
    /// synchronous fallback.
    fn try_begin_initial_packet(
        &self,
        _cx: i32,
        _cz: i32,
        _neighbour_offsets: &[(i32, i32)],
    ) -> Option<Result<Box<dyn InitialPacketTransaction + '_>, ResidentLightTransactionError>> {
        None
    }

    /// Checks resident admission without copying the column. Sources with an
    /// atomic resident boundary override this; other sources retain the
    /// existing column-snapshot fallback.
    fn try_resident_column_presence(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<()>> {
        self.try_resident_column(cx, cz).map(|result| match result {
            crate::chunk_store::TryResident::Busy => crate::chunk_store::TryResident::Busy,
            crate::chunk_store::TryResident::Absent => crate::chunk_store::TryResident::Absent,
            crate::chunk_store::TryResident::Present(_) => {
                crate::chunk_store::TryResident::Present(())
            }
        })
    }

    /// Attempts a resident-only block snapshot through an atomic source
    /// boundary. See [`Self::try_resident_column`] for the meaning of the
    /// outer `Option` and the three resident outcomes.
    fn try_resident_block_state_id(
        &self,
        _x: i32,
        _y: i32,
        _z: i32,
    ) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
        None
    }

    /// Attempts a resident block mutation without waiting or starting
    /// generation. Sources without this explicit capability return `None`, so
    /// existing callers can retain their ordinary mutation path; a capable
    /// source returns `Some(Busy)`, `Some(Absent)`, `Some(Unsupported)`, or
    /// `Some(Applied)`.
    fn try_set_block(
        &self,
        _x: i32,
        _y: i32,
        _z: i32,
        _state: StateId,
    ) -> Option<crate::chunk_store::TryBlockMutation> {
        None
    }

    /// Retains a post-edit resident snapshot in a source's mutation-only edit
    /// ledger without generating or waiting on a condition variable. This is
    /// intentionally separate from [`Self::store_resident_column`]: light
    /// settlement may retain a complete snapshot for serving without turning
    /// every settled generated column into a permanent terrain edit. Returning
    /// `None` means this source has no nonblocking edit ledger; `Some(Busy)`
    /// means its short ledger lock was unavailable, and `Some(Applied)` means
    /// the exact snapshot was retained.
    fn try_store_resident_edit(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        None
    }

    /// Persists a complete column that an outer cache has already mutated.
    ///
    /// Returning `true` says the source retained this exact snapshot, so the
    /// caller must not also invoke [`set_block`](Self::set_block). Sources that
    /// cannot retain a full column keep the default and receive the ordinary
    /// coordinate-level mutation instead.
    fn store_resident_column(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> bool {
        false
    }

    /// Invalidates retained light snapshots whose dependency footprint includes
    /// the changed column `(cx, cz)`.
    ///
    /// A block mutation changes the light answer in its own column and in the
    /// adjacent columns used by the cross-column light encoder. A source that
    /// retains complete columns must clear those snapshots before the next
    /// admission fence; otherwise an initial packet can consume a value from a
    /// pre-mutation footprint. The default is correct for sources that do not
    /// retain light. Cache and persistence wrappers forward this hook so every
    /// retention layer applies the same lifecycle rule.
    fn invalidate_retained_light_neighbourhood(&self, _cx: i32, _cz: i32) {}

    /// Persists several complete columns as one source transaction.
    ///
    /// The default keeps small or legacy sources compatible by forwarding each
    /// column through [`store_resident_column`](Self::store_resident_column).
    /// Persistent wrappers that can hold their edit lock across the batch may
    /// override this to make every touched coordinate visible together.
    fn store_resident_columns(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        let mut stored = true;
        for &(cx, cz, ref column) in columns {
            if !self.store_resident_column(cx, cz, column) {
                stored = false;
            }
        }
        stored
    }

    /// Retains a settled light batch for columns this layer already retains.
    ///
    /// Light is derived state: a persistence layer refreshes the light of a
    /// column it already keeps as an edit, but never turns unedited generated
    /// terrain into one. Returns whether any column was retained here.
    fn store_resident_lights(&self, _columns: &[(i32, i32, ChunkColumn)]) -> bool {
        false
    }

    /// Retains an initial-light batch without waiting or partially publishing.
    /// The caller owns every input coordinate gate and has validated its
    /// captured revisions. Sources retaining complete serving snapshots must
    /// override this hook; `Ok(false)` means this layer retains no light.
    fn try_store_resident_lights(
        &self,
        _columns: &[(i32, i32, ChunkColumn)],
    ) -> Result<bool, ResidentLightTransactionError> {
        Ok(false)
    }

    /// Computes and installs a retained light snapshot from one stable column
    /// view. The callback runs without a source or cache lock held. A source
    /// with a versioned cache may reject the result when a concurrent block
    /// write changed the column between capture and commit; callers should
    /// refresh their input and retry that case.
    fn settle_resident_column_light(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        replace_existing: bool,
        compute: &mut dyn FnMut(&ChunkColumn) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        let mut compute_centre = |current: &ChunkColumn, _neighbours: &[(i32, i32, &ChunkColumn)]| {
            compute(current)
        };
        self.settle_resident_column_light_with_neighbours(
            cx,
            cz,
            fallback,
            &[],
            false,
            replace_existing,
            false,
            &mut compute_centre,
        )
    }

    /// Computes and installs every light snapshot returned by one admitted
    /// footprint. The default adapts the historical centre-only transaction,
    /// so existing sources and protocols keep their exact behaviour until they
    /// opt into the batch hook.
    fn settle_resident_column_lights_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        let mut compute_centre = |current: &ChunkColumn,
                                  neighbours: &[(i32, i32, &ChunkColumn)]| {
            compute(current, neighbours).map(|settlement| settlement.centre_light().clone())
        };
        self.settle_resident_column_light_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            &mut compute_centre,
        )
    }

    /// Captures the centre and every requested neighbour, computes light from
    /// that one footprint, and installs the resulting snapshot. The offsets
    /// are chunk-relative `(dx, dz)` pairs and omit `(0, 0)`; callers normally
    /// pass the eight entries in a 3×3 square.
    ///
    /// `resident_only` makes an absent centre or neighbour return
    /// [`ColumnLightSettlementError::MissingFootprint`] without generation or
    /// persistence. The callback runs after all columns have been captured and
    /// without a source/cache lock held. `exclusive` requests the source's
    /// guaranteed-progress path: a source with coordinate gates holds every
    /// requested gate through the callback and commit, while the ordinary path
    /// validates the captured revisions optimistically.
    fn settle_resident_column_light_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        _exclusive: bool,
        compute: &mut dyn FnMut(&ChunkColumn, &[(i32, i32, &ChunkColumn)]) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        let current = self
            .resident_column(cx, cz)
            .or_else(|| (!resident_only).then(|| fallback.clone()))
            .ok_or(ColumnLightSettlementError::MissingFootprint)?;
        if !replace_existing
            && current.retained_light_status() == Some(RetainedLightStatus::CentreSettled)
        {
            return Ok(current);
        }
        let mut neighbours = Vec::with_capacity(neighbour_offsets.len());
        for &(dx, dz) in neighbour_offsets {
            let column = if resident_only {
                self.resident_column(cx + dx, cz + dz)
            } else {
                Some(self.column(cx + dx, cz + dz))
            }
            .ok_or(ColumnLightSettlementError::MissingFootprint)?;
            neighbours.push((dx, dz, column));
        }
        let neighbour_refs = neighbours
            .iter()
            .map(|(dx, dz, column)| (*dx, *dz, column))
            .collect::<Vec<_>>();
        let Some(light) = compute(&current, &neighbour_refs) else {
            return Err(ColumnLightSettlementError::NoLight);
        };
        let mut settled = current;
        settled.set_retained_light(light);
        let _ = self.store_resident_column(cx, cz, &settled);
        Ok(settled)
    }

    /// Reads the biome id at world coordinates `(x, y, z)` — `/execute if
    /// biome`'s own read, through the same data
    /// [`column`](Self::column) would return.
    ///
    /// Required, not defaulted, for the same reason [`block_state_id`](Self::block_state_id)
    /// is: a defaulted trait method plus a wrapper impl is an island generator
    /// in this crate (measured — `is_column_resident`'s `true` default was
    /// once silently inherited by both `Arc<S>` and `DimensionalSource<S>`,
    /// making an entire fix a no-op in production while its own tests
    /// passed). An implementor with no cheaper path implements this as
    /// `self.column(x.div_euclid(16), z.div_euclid(16)).biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16)).to_string()`,
    /// which is correct if column-sized; an implementor with a retained
    /// column (`ChunkStore`) should read out of that instead.
    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String;

    /// Overwrites a single block's state at world coordinates `(x, y, z)`,
    /// persisting the change so a later [`column`](Self::column) call for
    /// its chunk reflects it.
    ///
    /// This is a required method so no implementor can silently drop a
    /// placement. Every implementor must decide explicitly how edits are
    /// stored. A source with no per-column
    /// retention must say so loudly — a `todo!()`, or an explicitly documented
    /// discard — rather than inherit silence.
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId);

    /// The block entity this source's data carries at `(x, y, z)`, if any —
    /// a *generated* one, such as a structure chest's rolled contents
    /// or a bee nest's occupants.
    ///
    /// This is not the live world's registry: [`crate::block_entities::BlockEntityRegistry`]
    /// holds every entity a player has placed or mutated, and is consulted first
    /// by every caller. This answers the narrower question "what did generation
    /// put here", which is what lets a chest that has never been opened be
    /// hydrated into the registry on the first click instead of arriving empty.
    ///
    /// Defaulted, because it regenerates a column and an implementor with a
    /// retained column should override it — unlike [`block_state_id`](Self::block_state_id),
    /// this is called at most once per container click, not every 50 ms, so the
    /// default is affordable rather than a trap.
    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        let pos = BlockPos::new(x, y, z);
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_entities()
            .iter()
            .find(|(at, _)| *at == pos)
            .map(|(_, entity)| entity.clone())
    }

    /// Whether `(cx, cz)` is already resident — answerable with **no**
    /// generation, unlike [`column`](Self::column) or
    /// [`block_state_id`](Self::block_state_id) on a miss.
    ///
    /// This exists for [`crate::block_entities::BlockEntityRegistry::tick_all_with_hopper_lock`]
    /// because only a block entity whose *chunk* is loaded should tick. Calling
    /// `block_state_id` to answer that question would generate a whole column for
    /// every 20 Hz probe that ultimately returns "not loaded".
    ///
    /// The default is `true` — "assume resident" — which is the honest
    /// answer for every implementor with no bounded cache to miss (an
    /// unbounded edit map, or a bare generator): there is no eviction to ask
    /// about, so refusing would be inventing an answer, not reporting one.
    /// [`crate::chunk_store::ChunkStore`] is the one implementor with a real
    /// capacity to check against, and it is the only override — production's
    /// `world` in `tick.rs` is `Arc<ChunkStore<..>>`, so that override is the
    /// one that matters.
    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        let _ = (cx, cz);
        true
    }

    /// Reconciles ticket-driven cache residency without generating or serving
    /// a column. Sources with a ticket-backed cache override this; lightweight
    /// generators have no residency to reconcile.
    fn reconcile_ticket_residency(&self) {}

    /// The ticket store owned by this source, if it has ticket-backed residency.
    /// Lightweight sources retain the connection's isolated compatibility store.
    fn ticket_store(&self) -> Option<crate::ticket::TicketStoreHandle> { None }

    /// Tells the source that the column at `(cx, cz)` is no longer resident in
    /// whatever cache sits above it, so a layer that retains state per column
    /// may release it.
    ///
    /// The default is a no-op, which is the correct behaviour for every source
    /// that owns no per-column state — and for [`Terrain263ChunkSource`], whose
    /// edit map *is* the world for a generator-only session and must therefore
    /// never shrink.
    ///
    /// # This is a hint, not an instruction
    ///
    /// The caller makes no promise the column will not be asked for again a
    /// moment later, so an implementor must stay correct if it is: releasing
    /// state here is only sound when that state can be *reconstructed*.
    /// [`crate::region_source::RegionChunkSource`] is the one implementor that
    /// acts on it, and it does so only for a column it has already written to
    /// disk — see its own doc for the invariant that makes that lossless.
    ///
    /// **Do no I/O here.** This is called from `ChunkStore`'s miss path, which
    /// is the tick thread as often as not; the whole reason region writes go
    /// through `spawn_blocking` is that a full-region write on that thread was
    /// the last large performance defect in this crate.
    fn unload(&self, cx: i32, cz: i32) {
        let _ = (cx, cz);
    }

    /// Tells the source that a connection's view radius is `view_radius`, so
    /// a layer that *retains* columns can resize its bound to match.
    ///
    /// The default is a no-op, correct for every source that retains nothing per
    /// view. [`crate::chunk_store::ChunkStore`] is the one implementor that acts
    /// on it.
    ///
    /// # Why this exists
    ///
    /// `ChunkStore`'s capacity is chosen from the connection's initial radius.
    /// A later increase can make the streamed view exceed the cache bound, and
    /// the LRU victim under a short capacity is the
    /// **innermost** ring, because `crate::server`'s `join_view_rings` streams
    /// outward and leaves ring 0 with the oldest stamp. Raising render distance
    /// therefore worked while quietly regenerating the ground under the player's
    /// feet at 909 ms a column. See `chunk_store`'s
    /// `integrated_capacity_for_view_radius` for the full argument.
    ///
    /// # This is a hint, not an instruction, and it is monotonic in practice
    ///
    /// Like [`unload`](Self::unload), an implementor must stay correct if it
    /// ignores this. It is called from `ViewTracker::set_view_radius` — after the
    /// clamp, so the value never exceeds what the connection may actually be
    /// served — on every radius change, lowering included. A store is free to
    /// treat a *lowering* as advisory rather than immediately evicting; see
    /// `ChunkStore::set_retention_radius` for what it does and why.
    ///
    /// **Do no I/O here**, for the same reason `unload` says so.
    fn set_retention_radius(&self, view_radius: i32) {
        let _ = view_radius;
    }

    /// The live-world registries this source persists, when it persists any.
    ///
    /// A source backed by a world directory owns the *one* block-entity registry
    /// and scheduled-tick queue its save path reads. A server constructor that
    /// builds its own `default()` pair instead ticks containers and repeater
    /// delays that no save can ever see — the island singleplayer exposed
    /// singleplayer, and the same one open-to-LAN had until this accessor
    /// existed: `open_to_lan` is generic over `S`, so it could not name
    /// `RegionChunkSource::block_entities` directly.
    ///
    /// `None` — the default — is the honest answer for an in-memory source: there
    /// is nothing on disk, so a private registry loses nothing. Wrappers forward
    /// through to whatever they wrap; only
    /// [`RegionChunkSource`](crate::region_source::RegionChunkSource) answers
    /// `Some`.
    fn world_registries(&self) -> Option<WorldRegistries> {
        None
    }

    /// Which dimension this source's terrain belongs to, when it knows.
    ///
    /// `None` — the default — means "unlabelled", which every source that is not
    /// wrapped in a [`DimensionalSource`](crate::dimension::DimensionalSource) is.
    /// Callers treat `None` as `Overworld`, since that is the only dimension a
    /// single-dimension world can be; the distinction is kept so a *labelled*
    /// source is never silently overridden.
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        None
    }

    /// Another dimension's terrain, for a source that is part of a multi-dimension
    /// world.
    ///
    /// **This is how a connection reaches the Nether.** It rides an accessor the
    /// join path already threads a source to, for exactly the reason
    /// [`WorldRegistries::player_data`] does: `serve_play` has forty parameters
    /// across eleven wrapper call sites in two target-gated definitions, and a new
    /// one for the dimension bundle would be eleven signature changes in this
    /// crate's most contended file to carry information the connection can ask the
    /// source it is already holding.
    ///
    /// `None` — the default, and the answer for the dimension the caller is
    /// already in — means "no such dimension here", which is the correct
    /// degradation: `crate::server`'s travel path simply does not travel, and a
    /// single-dimension world behaves exactly as it did before portals existed.
    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<std::sync::Arc<dyn ChunkSource>> {
        let _ = dimension;
        None
    }

    /// This world's shared index of lit nether portals, when it has one.
    ///
    /// Shared across *all* dimensions of one world, because a trip's destination
    /// search runs in the dimension the player is not yet in. See
    /// [`crate::portal::PortalIndex`] for why an index rather than a block scan.
    fn portal_index(&self) -> Option<&crate::portal::PortalIndex> {
        None
    }

    /// This dimension's own inbound tick-scheduling feed — the same
    /// [`crate::tick::BlockTickFeed`] its own background tick loop (when one
    /// runs) drains every tick to rebase a connection's delayed
    /// redstone/fluid request onto that loop's own scheduled-tick queue.
    ///
    /// `None` — the default — is correct for a source with no dimension-scoped
    /// tick loop of its own, and for the *primary* (join) dimension, whose feed
    /// a connection already holds directly as a `serve_play` parameter and has
    /// no reason to ask for a second time. Only
    /// [`DimensionalSource`](crate::dimension::DimensionalSource) built through
    /// [`crate::integrated`]'s sibling factory answers `Some` — see that type's
    /// own doc comment for why a *second* dimension's tick loop needs its own
    /// feed rather than sharing the primary's, and `crate::server`'s
    /// portal-travel handling for the one call site that asks.
    fn block_tick_feed(&self) -> Option<crate::tick::BlockTickFeed> {
        None
    }

    fn dragon_fight_started(&self) -> Option<bool> {
        None
    }

    /// Claims process-lifetime fight initialization. Sources without fight
    /// state retain the legacy unconditional claim.
    fn claim_dragon_fight_start(&self) -> bool {
        true
    }
}

/// Forwards every [`ChunkSource`] method through the `Arc`, the same shape
/// `crate::protocol`'s `impl<P: ServerProtocol + ?Sized> ServerProtocol for
/// Box<P>` already establishes for that trait — see its own doc comment for
/// why the forwarding has to be hand-written rather than derived.
///
/// This is what lets [`IntegratedServer::publish`](crate::IntegratedServer::publish)
/// hand every connection it accepts an `Arc<dyn ChunkSource>` — the
/// type-erased handle a running world's `HostCore` stores — through a
/// `serve_connection*` entry point whose `S: ChunkSource` bound is otherwise
/// only satisfiable by a concrete, `Sized` source. `Arc` is `#[fundamental]`,
/// so the impl is coherent here in the trait's own crate, same as `Box`'s.
///
/// **When you add a method to [`ChunkSource`], add its forward here too** — an
/// unforwarded defaulted method would silently answer the trait's own default
/// (`None`, a no-op, or a full regeneration) for every erased source instead
/// of asking the real one, which for `sibling`/`dimension` means a published
/// LAN player's portal travel would silently stop working while a directly-held
/// concrete source kept it.
impl<S: ChunkSource + ?Sized> ChunkSource for Arc<S> {
    fn retain_generation_population(&self, cx: i32, cz: i32, column: &mut ChunkColumn) -> bool {
        (**self).retain_generation_population(cx, cz, column)
    }

    fn pending_generation_spawn_batches(&self, limit: usize) -> Vec<Arc<crate::generation_population::GenerationSpawnBatch>> {
        (**self).pending_generation_spawn_batches(limit)
    }

    fn locate_stronghold(&self, from: BlockPos) -> Option<BlockPos> {
        (**self).locate_stronghold(from)
    }

    fn spawn_origin_block(&self) -> Option<(i32, i32)> {
        (**self).spawn_origin_block()
    }

    fn horizon_sample(&self, x: i32, z: i32) -> Option<HorizonSample> {
        (**self).horizon_sample(x, z)
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        (**self).resident_block_state_id(x, y, z)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        (**self).resident_column(cx, cz)
    }

    fn try_resident_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<ChunkColumn>> {
        (**self).try_resident_column(cx, cz)
    }

    fn try_begin_resident_light(
        &self,
        outputs: &[(i32, i32)],
        inputs: &[(i32, i32)],
    ) -> Option<Result<Box<dyn ResidentLightTransaction + '_>, ResidentLightTransactionError>> {
        (**self).try_begin_resident_light(outputs, inputs)
    }

    fn try_begin_initial_packet(
        &self,
        cx: i32,
        cz: i32,
        neighbour_offsets: &[(i32, i32)],
    ) -> Option<Result<Box<dyn InitialPacketTransaction + '_>, ResidentLightTransactionError>> {
        (**self).try_begin_initial_packet(cx, cz, neighbour_offsets)
    }

    fn try_resident_column_presence(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<()>> {
        (**self).try_resident_column_presence(cx, cz)
    }

    fn try_resident_block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
        (**self).try_resident_block_state_id(x, y, z)
    }

    fn try_set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
    ) -> Option<crate::chunk_store::TryBlockMutation> {
        (**self).try_set_block(x, y, z, state)
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        (**self).try_store_resident_edit(cx, cz, column)
    }

    fn store_resident_column(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        (**self).store_resident_column(cx, cz, column)
    }

    fn invalidate_retained_light_neighbourhood(&self, cx: i32, cz: i32) {
        (**self).invalidate_retained_light_neighbourhood(cx, cz);
    }

    fn store_resident_columns(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        (**self).store_resident_columns(columns)
    }

    fn store_resident_lights(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        (**self).store_resident_lights(columns)
    }

    fn try_store_resident_lights(
        &self,
        columns: &[(i32, i32, ChunkColumn)],
    ) -> Result<bool, ResidentLightTransactionError> {
        (**self).try_store_resident_lights(columns)
    }

    fn settle_resident_column_light(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        replace_existing: bool,
        compute: &mut dyn FnMut(&ChunkColumn) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_light(cx, cz, fallback, replace_existing, compute)
    }

    fn settle_resident_column_lights_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_lights_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            compute,
        )
    }

    fn settle_resident_column_light_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_light_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            compute,
        )
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        (**self).column(cx, cz)
    }

    fn columns(&self, coords: &[(i32, i32)]) -> Vec<ChunkColumn> {
        (**self).columns(coords)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        (**self).column_at(cx, cz, stage)
    }

    fn packet_generation_stage(
        &self,
        stage: ChunkGenerationStage,
    ) -> Option<ChunkGenerationStage> {
        (**self).packet_generation_stage(stage)
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        (**self).block_state_id(x, y, z)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        (**self).biome_state_at(x, y, z)
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        (**self).set_block(x, y, z, state);
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        (**self).block_entity(x, y, z)
    }

    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        (**self).is_column_resident(cx, cz)
    }

    fn reconcile_ticket_residency(&self) {
        (**self).reconcile_ticket_residency();
    }

    fn ticket_store(&self) -> Option<crate::ticket::TicketStoreHandle> {
        (**self).ticket_store()
    }

    fn unload(&self, cx: i32, cz: i32) {
        (**self).unload(cx, cz);
    }

    fn set_retention_radius(&self, view_radius: i32) {
        (**self).set_retention_radius(view_radius);
    }

    fn world_registries(&self) -> Option<WorldRegistries> {
        (**self).world_registries()
    }

    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        (**self).dimension()
    }

    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<std::sync::Arc<dyn ChunkSource>> {
        (**self).sibling(dimension)
    }

    fn portal_index(&self) -> Option<&crate::portal::PortalIndex> {
        (**self).portal_index()
    }

    fn block_tick_feed(&self) -> Option<crate::tick::BlockTickFeed> {
        (**self).block_tick_feed()
    }

    fn claim_dragon_fight_start(&self) -> bool {
        (**self).claim_dragon_fight_start()
    }

    fn dragon_fight_started(&self) -> Option<bool> {
        (**self).dragon_fight_started()
    }
}

/// A borrowed source, forwarding every method to the referent.
///
/// This exists so a caller holding `&S` for an `S` that may itself be
/// unsized (`S: ChunkSource + ?Sized`, the bound most of `crate::server`'s
/// packet handlers carry, so a type-erased `dyn ChunkSource` satisfies them)
/// can still produce a `&dyn ChunkSource` for an API that wants one: an
/// unsizing coercion needs a `Sized` source type, while `&S` is `Sized`
/// whatever `S` is. `&` is `#[fundamental]`, so the impl is coherent here in
/// the trait's own crate, same as `Arc`'s above.
///
/// **When you add a method to [`ChunkSource`], add its forward here too** —
/// see the `Arc` impl's own note for what an unforwarded defaulted method
/// silently costs.
impl<S: ChunkSource + ?Sized> ChunkSource for &S {
    fn retain_generation_population(&self, cx: i32, cz: i32, column: &mut ChunkColumn) -> bool {
        (**self).retain_generation_population(cx, cz, column)
    }

    fn pending_generation_spawn_batches(&self, limit: usize) -> Vec<Arc<crate::generation_population::GenerationSpawnBatch>> {
        (**self).pending_generation_spawn_batches(limit)
    }

    fn locate_stronghold(&self, from: BlockPos) -> Option<BlockPos> {
        (**self).locate_stronghold(from)
    }

    fn spawn_origin_block(&self) -> Option<(i32, i32)> {
        (**self).spawn_origin_block()
    }

    fn horizon_sample(&self, x: i32, z: i32) -> Option<HorizonSample> {
        (**self).horizon_sample(x, z)
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        (**self).resident_block_state_id(x, y, z)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        (**self).resident_column(cx, cz)
    }

    fn try_resident_column(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<ChunkColumn>> {
        (**self).try_resident_column(cx, cz)
    }

    fn try_begin_resident_light(
        &self,
        outputs: &[(i32, i32)],
        inputs: &[(i32, i32)],
    ) -> Option<Result<Box<dyn ResidentLightTransaction + '_>, ResidentLightTransactionError>> {
        (**self).try_begin_resident_light(outputs, inputs)
    }

    fn try_begin_initial_packet(
        &self,
        cx: i32,
        cz: i32,
        neighbour_offsets: &[(i32, i32)],
    ) -> Option<Result<Box<dyn InitialPacketTransaction + '_>, ResidentLightTransactionError>> {
        (**self).try_begin_initial_packet(cx, cz, neighbour_offsets)
    }

    fn try_resident_column_presence(
        &self,
        cx: i32,
        cz: i32,
    ) -> Option<crate::chunk_store::TryResident<()>> {
        (**self).try_resident_column_presence(cx, cz)
    }

    fn try_resident_block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<crate::chunk_store::TryResident<lodestone_data::block_states::StateId>> {
        (**self).try_resident_block_state_id(x, y, z)
    }

    fn try_set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
    ) -> Option<crate::chunk_store::TryBlockMutation> {
        (**self).try_set_block(x, y, z, state)
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        (**self).try_store_resident_edit(cx, cz, column)
    }

    fn store_resident_column(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        (**self).store_resident_column(cx, cz, column)
    }

    fn invalidate_retained_light_neighbourhood(&self, cx: i32, cz: i32) {
        (**self).invalidate_retained_light_neighbourhood(cx, cz);
    }

    fn store_resident_columns(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        (**self).store_resident_columns(columns)
    }

    fn store_resident_lights(&self, columns: &[(i32, i32, ChunkColumn)]) -> bool {
        (**self).store_resident_lights(columns)
    }

    fn try_store_resident_lights(
        &self,
        columns: &[(i32, i32, ChunkColumn)],
    ) -> Result<bool, ResidentLightTransactionError> {
        (**self).try_store_resident_lights(columns)
    }

    fn settle_resident_column_light(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        replace_existing: bool,
        compute: &mut dyn FnMut(&ChunkColumn) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_light(cx, cz, fallback, replace_existing, compute)
    }

    fn settle_resident_column_lights_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<ColumnLightSettlement>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_lights_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            compute,
        )
    }

    fn settle_resident_column_light_with_neighbours(
        &self,
        cx: i32,
        cz: i32,
        fallback: &ChunkColumn,
        neighbour_offsets: &[(i32, i32)],
        resident_only: bool,
        replace_existing: bool,
        exclusive: bool,
        compute: &mut dyn FnMut(
            &ChunkColumn,
            &[(i32, i32, &ChunkColumn)],
        ) -> Option<lodestone_world::ColumnLight>,
    ) -> Result<ChunkColumn, ColumnLightSettlementError> {
        (**self).settle_resident_column_light_with_neighbours(
            cx,
            cz,
            fallback,
            neighbour_offsets,
            resident_only,
            replace_existing,
            exclusive,
            compute,
        )
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        (**self).column(cx, cz)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        (**self).column_at(cx, cz, stage)
    }

    fn packet_generation_stage(
        &self,
        stage: ChunkGenerationStage,
    ) -> Option<ChunkGenerationStage> {
        (**self).packet_generation_stage(stage)
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        (**self).block_state_id(x, y, z)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        (**self).biome_state_at(x, y, z)
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        (**self).set_block(x, y, z, state);
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        (**self).block_entity(x, y, z)
    }

    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        (**self).is_column_resident(cx, cz)
    }

    fn reconcile_ticket_residency(&self) {
        (**self).reconcile_ticket_residency();
    }

    fn ticket_store(&self) -> Option<crate::ticket::TicketStoreHandle> {
        (**self).ticket_store()
    }

    fn unload(&self, cx: i32, cz: i32) {
        (**self).unload(cx, cz);
    }

    fn set_retention_radius(&self, view_radius: i32) {
        (**self).set_retention_radius(view_radius);
    }

    fn world_registries(&self) -> Option<WorldRegistries> {
        (**self).world_registries()
    }

    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        (**self).dimension()
    }

    fn sibling(
        &self,
        dimension: crate::dimension::Dimension,
    ) -> Option<std::sync::Arc<dyn ChunkSource>> {
        (**self).sibling(dimension)
    }

    fn portal_index(&self) -> Option<&crate::portal::PortalIndex> {
        (**self).portal_index()
    }

    fn block_tick_feed(&self) -> Option<crate::tick::BlockTickFeed> {
        (**self).block_tick_feed()
    }

    fn claim_dragon_fight_start(&self) -> bool {
        (**self).claim_dragon_fight_start()
    }

    fn dragon_fight_started(&self) -> Option<bool> {
        (**self).dragon_fight_started()
    }
}

/// The live registries a persistent [`ChunkSource`] owns, handed to a
/// server constructor so the tick loop and the save path share one instance.
///
/// All are cheap handles, so this is a clone of a few `Arc`s rather than a
/// borrow.
#[derive(Debug, Clone)]
pub struct WorldRegistries {
    /// Every container, sign and furnace a player has placed or mutated.
    pub block_entities: crate::block_entities::BlockEntityHandle,
    /// Pending scheduled and fluid ticks.
    ///
    /// Named through [`crate::scheduled_tick`], not through `region_source`,
    /// because this struct is **not** target-gated and `region_source` is: the
    /// handle is portable (two `Arc`s and an atomic), only the Anvil store
    /// behind it is native. `region_source` re-exports the same type.
    pub scheduled: crate::scheduled_tick::ScheduledTickHandle,
    /// Where per-player `.dat` files live for this world.
    ///
    /// **This is how the player store reaches a connection**, and the routing is
    /// deliberate: `crate::server`'s join path already threads a `ChunkSource`
    /// everywhere it needs one, and `serve_connection_inner`/`serve_play` are at
    /// 30-odd parameters between them across eleven wrapper call sites. Riding
    /// the accessor a persistent source already answers costs no new parameter
    /// and, more usefully, makes it *structurally* impossible for a persistent
    /// world to be served by a connection that cannot see its player files —
    /// the same island shape the block-entity registry had.
    ///
    /// Unlike its two siblings this is an `Option`, because a world can have a
    /// region directory and still fail to create `players/data`.
    ///
    /// # Native only, unlike the two fields above
    ///
    /// Gated rather than given a wasm stand-in, because the capability itself is
    /// native: [`crate::player_data::PlayerDataStore`] *is* a `std::fs` schema
    /// over gzipped NBT, and the only thing that ever answers `Some` here is
    /// [`RegionChunkSource`](crate::region_source::RegionChunkSource), which is
    /// native-only too. A browser world has no `players/data` to point a
    /// stand-in at, and every reader in `crate::server` (`player_store`,
    /// `persist_player`, and the join arm's `saved_player`) is already gated to
    /// match — so on wasm this is not a lost feature but a capability that has
    /// no backing store to lose.
    #[cfg(not(target_arch = "wasm32"))]
    pub player_data: Option<crate::player_data::PlayerDataStore>,
    /// The selected native typed-record backend, when this persistent source
    /// has one. The connection uses it only for the bounded player locator;
    /// complete player state remains in [`Self::player_data`].
    #[cfg(not(target_arch = "wasm32"))]
    pub native_storage: Option<std::sync::Arc<crate::world_storage::WorldStorage>>,
}

/// Generates every column in `coords` across the shared native Rayon pool over
/// `&source`,
/// returning them in the **same order as `coords`** regardless of which
/// thread finished which column first.
///
/// This is safe because `column()` is genuinely pure per chunk: every RNG a
/// generator touches is positionally seeded (`set_decoration_seed` /
/// `set_feature_seed` per source chunk, with `fork_positional`/`from_hash_of`)
/// and no shared RNG stream exists anywhere in
/// `lodestone-worldgen`, so results are order-independent by construction.
/// `ChunkSource: Send + Sync` (this trait's
/// own bound, above) is what makes `&S` shareable across the pool in the first
/// place. Reusing one pool is load-bearing: a fresh scoped thread set per
/// batch would multiply native workers when players cross chunk boundaries
/// together.
///
/// Callers that care about the wire being independent of thread scheduling
/// (i.e. every caller) must still encode/send the returned columns in the
/// fixed order they came in — this function only parallelises the
/// generation, not the ordering guarantee, which is why it hands back a
/// `Vec` aligned index-for-index with `coords` rather than an unordered
/// collection.
#[must_use]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn generate_columns_parallel<S: ChunkSource + ?Sized>(
    source: &S,
    coords: &[(i32, i32)],
) -> Vec<ChunkColumn> {
    source.columns(coords)
}

/// Run immutable world-generation jobs and collect them in submission order.
/// Native uses the persistent dispatcher; threaded wasm uses the initialized
/// global Rayon pool.
#[must_use]
#[cfg(not(target_arch = "wasm32"))]
pub fn run_worldgen_jobs<T, R, F>(jobs: Vec<T>, work: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Send + Sync,
{
    crate::worldgen_dispatch::run_ordered(jobs, work)
}

/// Serial browser fallback when `wasm-threads` is disabled.
#[must_use]
#[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
pub fn run_worldgen_jobs<T, R, F>(jobs: Vec<T>, work: F) -> Vec<R>
where
    F: Fn(T) -> R,
{
    jobs.into_iter().map(work).collect()
}

/// Rayon arm for the atomics-enabled browser worker's initialized pool.
#[must_use]
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub fn run_worldgen_jobs<T, R, F>(jobs: Vec<T>, work: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Send + Sync,
{
    use rayon::prelude::*;

    jobs.into_par_iter().map(work).collect()
}

#[must_use]
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub(crate) fn browser_worldgen_parallelism() -> usize {
    rayon::current_num_threads().clamp(1, 4)
}

/// Single-threaded, yielding map used by the wasm32
/// shape of the same idea, used because wasm32 has neither a scoped-thread fan-out
/// nor a blocking pool to offload to (see [`generate_columns_offloaded`]'s own
/// wasm32 note).
///
/// # Why per-column, not per-batch
///
/// `yield_between` is awaited **after every single column**, not after some
/// larger slice. `docs/world-open-latency.md` measures real per-column
/// generation cost at ~222 ms warm (contiguous, memo-assisted) and ~909 ms cold
/// (independent sources — closer to what a brand-new world's first join actually
/// is); a browser's own hang detector fires on a single unyielded task of only a
/// few seconds. One cold column alone can already spend a meaningful fraction of
/// that budget, so any slice bigger than one column reintroduces the exact
/// failure this function exists to remove — it would just take a slightly larger
/// batch to reproduce it. A slice of one is the only size that stays correct
/// regardless of how expensive a single column turns out to be.
///
/// # `FnMut` yield, not a fixed timer
///
/// `yield_between` is a caller-supplied future rather than (say) a hardcoded
/// `tokio::time::sleep`: `tokio::time` has no timer driver on wasm32 (see
/// `net.rs`'s own note — a wasm32 `tokio::time::timeout` hung a browser join on
/// its first poll), so the real yield has to come from the browser's own task
/// queue instead. Tests substitute a counting stub; production substitutes
/// [`yield_to_browser`].
async fn map_columns_yielding<S, T, F, Y, YFut>(
    source: &S,
    coords: &[(i32, i32)],
    f: F,
    mut yield_between: Y,
) -> Vec<T>
where
    S: ChunkSource + ?Sized,
    F: Fn((i32, i32), ChunkColumn) -> T,
    Y: FnMut() -> YFut,
    YFut: std::future::Future<Output = ()>,
{
    let mut out = Vec::with_capacity(coords.len());
    for &(cx, cz) in coords {
        out.push(f((cx, cz), source.column(cx, cz)));
        yield_between().await;
    }
    out
}

/// [`map_columns_yielding`] with the identity transform — the yielding twin of
/// [`generate_columns_parallel`], for the same reason `map_columns_parallel`
/// has a plain twin ([`generate_columns_parallel`] itself).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
async fn generate_columns_yielding<S, Y, YFut>(
    source: &S,
    coords: &[(i32, i32)],
    yield_between: Y,
) -> Vec<ChunkColumn>
where
    S: ChunkSource + ?Sized,
    Y: FnMut() -> YFut,
    YFut: std::future::Future<Output = ()>,
{
    map_columns_yielding(source, coords, |_, column| column, yield_between).await
}

/// Browser-safe counterpart for the borrowed [`ChunkSource`] dispatch arm.
///
/// The shared production arm already enters [`generate_columns_offloaded`],
/// whose wasm implementation uses this same yielding loop. Keeping this small
/// wrapper here prevents a borrowed/test-shaped caller from accidentally
/// reaching the native-only parallel function and trapping through
/// `std::thread` on wasm32.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn generate_columns_borrowed(
    source: &dyn ChunkSource,
    coords: &[(i32, i32)],
) -> Vec<ChunkColumn> {
    generate_columns_yielding(source, coords, yield_to_browser).await
}

/// Suspends generation at a host task boundary so timers and packets can run.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn yield_to_browser() {
    let _timing = crate::worldgen_progress::PhaseTimer::start(
        crate::worldgen_progress::WorldgenTimingPhase::BrowserYield, 1,
    );
    lodestone_time::browser_yield().await;
}

/// Generates columns off the async runtime's core thread.
/// This helper keeps the blocking generation work away from that thread.
///
/// # Why this exists when generation is already parallel
///
/// [`generate_columns_parallel`] improves *throughput*: the batch is fanned out
/// over the shared Rayon pool. It did nothing about *latency* when called
/// inline, because waiting for every worker still blocks the caller. Parallel
/// is not the same as non-blocking, and the
/// distinction is total rather than academic here: the shell builds the
/// server's runtime with `tokio::runtime::Builder::new_current_thread()`
/// (`crates/lodestone-shell/src/net.rs`), so the connection task and
/// [`crate::tick::run_tick_loop`] share **one** thread. Blocking it blocks
/// *every* task in the process — the world tick included — so an inline
/// chunk-boundary generation can drop one or more 50 ms ticks.
///
/// Generation requests run on bounded blocking workers. Their parallel
/// immutable work runs on the shared Rayon pool, so a request waiting for a
/// region lease cannot strand a Rayon worker needed by another request.
///
/// # Why `Arc<S>` rather than `&S`
///
/// The dispatched closure requires a `'static` lifetime, so the source cannot
/// be borrowed across it. Callers thread the shared handle they already hold
/// (`crate::integrated` builds `Arc::new(source)` for exactly this reason);
/// `crate::server::SourceRef` is the wrapper that lets a borrow-shaped
/// caller keep the old blocking path without duplicating any of
/// `serve_connection`'s body.
///
/// # wasm32
///
/// `wasm32-unknown-unknown` has no native Rayon pool and does **not** call
/// `generate_columns_parallel` straight through. `portal.rs`'s `create_portal`
/// gates its own call off on wasm32 for the same constraint. This helper therefore
/// wasm32 instead calls [`generate_columns_yielding`], which never enters
/// `map_columns_parallel`'s multi-column branch (it generates one column at a
/// time) and yields to the browser's own task queue between columns, avoiding
/// both the trap and the "page not responding" hang caused by synchronous
/// multi-column fan-out.
#[tracing::instrument(skip_all, fields(count = coords.len()))]
pub(crate) async fn generate_columns_offloaded<S: ChunkSource + 'static + ?Sized>(
    source: Arc<S>,
    coords: Vec<(i32, i32)>,
) -> Vec<ChunkColumn> {
    #[cfg(target_arch = "wasm32")]
    {
        generate_columns_yielding(&*source, &coords, yield_to_browser).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::worldgen_dispatch::spawn(move || {
            generate_columns_parallel(&*source, &coords)
        })
            .await
            .await
            .expect("worldgen worker panicked")
    }
}

/// [`generate_columns_offloaded`], with **protocol encode folded into the same
/// blocking closure** — so the caller receives finished frames and never touches
/// a column.
///
/// # Why the encode has to move too, and not only the generation
///
/// `generate_columns_offloaded` fixed *generation*; it left the encode where it
/// was, on the connection task, which is the task that owes the player a reply
/// to their block break. `crate::protocol::ChunkEncoder` carries the measurement
/// — 62 M instructions / ≈2.4 ms per column — and the path this exists for is
/// `crate::server`'s `ViewTracker::build_batch`: every chunk boundary the player
/// walks across produces a strip of `2r + 1` newly visible columns, 33 of them at
/// `view_radius = 16`, and encoding them inline is ≈80 ms of hitch **per
/// boundary**, repeating for as long as the player keeps walking. That is the
/// steady-state half of the owner's report; the join burst is the one-off half.
///
/// The wire is unaffected: the returned `Vec` is aligned index-for-index with
/// `coords`, exactly as `generate_columns_offloaded`'s is, so which function a
/// caller uses cannot change the emitted byte sequence.
///
/// Returns `None` when `encoder` is `None` — a protocol with no off-task encoder,
/// which is the default — so the caller falls back to
/// [`generate_columns_offloaded`] plus its own encode loop. Returning an `Option`
/// rather than taking a non-optional encoder keeps the fallback a property of
/// this function instead of a branch every call site repeats.
#[cfg_attr(not(target_arch = "wasm32"), tracing::instrument(skip_all, fields(count = coords.len())))]
pub(crate) async fn generate_and_encode_columns_offloaded<S: ChunkSource + 'static + ?Sized>(
    source: Arc<S>,
    coords: Vec<(i32, i32)>,
    encoder: Option<Arc<dyn crate::protocol::ChunkEncoder>>,
) -> Option<Result<Vec<crate::protocol::ServerDirective>, crate::protocol::ChunkEncodeError>> {
    let encoder = encoder?;
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    // wasm32: `map_columns_yielding`, one column generated-and-encoded at a
    // time with a real browser yield between each — see
    // `generate_columns_offloaded`'s wasm32 doc for why this is not merely a
    // latency nicety on this target. The native Rayon branch is not compiled
    // into the browser build, so this path cannot create native workers there.
    #[cfg(target_arch = "wasm32")]
    {
        let mut frames = Vec::with_capacity(coords.len());
        for (cx, cz) in coords {
            let column = source.column(cx, cz);
            frames.push(encoder.try_encode_chunk_in_dimension(cx, cz, &column, dimension));
            yield_to_browser().await;
        }
        Some(frames.into_iter().collect())
    }
    // Native: `map_columns_parallel`, not `generate_columns_parallel`
    // followed by an encode loop — the encode runs on the worker that
    // generated the column, so a 33-column strip's ≈80 ms of encode is
    // fanned out rather than paid serially after the join. See that
    // function's own doc for the two properties this buys.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let source_for_worker = Arc::clone(&source);
        let encode = move || {
            crate::worldgen_dispatch::run_ordered(coords, |(cx, cz)| {
                let column = source_for_worker.column(cx, cz);
                encoder.try_encode_chunk_in_dimension(cx, cz, &column, dimension)
            })
        };
        Some(
            crate::worldgen_dispatch::spawn(encode)
                .await
                .await
                .expect("worldgen Rayon worker panicked")
                .into_iter()
                .collect(),
        )
    }
}

struct VersionedAdmissionColumn {
    column: ChunkColumn,
    version: u64,
}

/// The start nearest `from` among concentric-ring `origins` (chunk coordinates),
/// as the block position a locate query reports.
///
/// Distance is measured to the chunk's centre column at `y = 32`, from `from`
/// including its height, and the winner's reported position is the chunk's
/// minimum corner at `y = 0`. Both halves are the placement's own locate rules:
/// the centre-and-height measure decides *which* start is nearest, the corner is
/// what a locator then steers at. Ties keep the earlier candidate.
pub(crate) fn nearest_ring_start(origins: &[(i32, i32)], from: BlockPos) -> Option<BlockPos> {
    let mut best: Option<(i64, (i32, i32))> = None;
    for &(cx, cz) in origins {
        let dx = i64::from(cx * 16 + 8) - i64::from(from.x);
        let dy = 32 - i64::from(from.y);
        let dz = i64::from(cz * 16 + 8) - i64::from(from.z);
        let distance = dx * dx + dy * dy + dz * dz;
        if best.is_none_or(|(held, _)| distance < held) {
            best = Some((distance, (cx, cz)));
        }
    }
    best.map(|(_, (cx, cz))| BlockPos::new(cx * 16, 0, cz * 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The patterned payload every End-city banner carries: a black top and
    /// bottom triangle.
    fn end_city_banner_nbt() -> lodestone_core::Nbt {
        use lodestone_core::Nbt;
    
        let pattern = |name: &str| {
            Nbt::Compound(vec![
                ("color".to_owned(), Nbt::String("black".to_owned())),
                ("pattern".to_owned(), Nbt::String(name.to_owned())),
            ])
        };
        Nbt::Compound(vec![
            ("components".to_owned(), Nbt::Compound(Vec::new())),
            (
                "patterns".to_owned(),
                Nbt::List {
                    element_type: lodestone_core::NbtTag::Compound,
                    elements: vec![pattern("minecraft:triangle_top"), pattern("minecraft:triangle_bottom")],
                },
            ),
        ])
    }
    fn sid(name: &str) -> StateId {
        StateId::from_state_str(name).expect("test state must be canonical")
    }

    /// The End source serves the *dimension's* 256-row window. A source that
    /// served the noise settings' 128-row generation height instead would report a column whose `height` disagrees with what
    /// `the_end`'s registry entry promises the client, which is a decode failure
    /// rather than a short world.
    #[test]
    fn end_chunk_source_pads_the_generators_128_rows_to_the_dimensions_256() {
        let source = crate::worldgen_data::end_chunk_source(-195_764_831);
        let column = source.column(0, 0);
        assert_eq!(column.height, 256, "the served column must be the dimension's own height");
        // Above the generator's own 128 rows, the padding must be air — checked
        // at y = 200, well clear of both the generator's ceiling and any
        // interpolation cell straddling it.
        for x in 0..16 {
            for z in 0..16 {
                assert_eq!(
                    column.block_state_id(x, 200, z),
                    sid("minecraft:air"),
                    "padding above the generator's own 128 rows must be air at ({x},200,{z})"
                );
            }
        }
        // And the generator's own terrain is not lost in the pad: some cell in
        // its native range is non-air, at the main island's centre chunk.
        let solid = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .any(|(x, z)| (0..128).any(|y| column.block_state_id(x, y, z) != sid("minecraft:air")));
        assert!(solid, "the generator's own 0..128 range must not be entirely air at the island's centre");
    }

    /// A served Nether column retains a MOTION_BLOCKING map. The bedrock roof's
    /// top layer is y = 127 in every column and nothing generated above it
    /// blocks motion, so every cell reads 128 in the stored `top_y + 1` form;
    /// a block placed above the roof then raises only its own cell.
    #[test]
    fn nether_columns_retain_a_motion_blocking_heightmap() {
        let mut column = crate::nether_chunk_source(42).column(3, -5);
        let heights = *column
            .motion_blocking()
            .expect("a Nether column retains its MOTION_BLOCKING map");
        assert_eq!(heights, [128; 256]);
        column.set_block_id(3, 200, 5, Block::Stone.default_state());
        let heights = column.motion_blocking().expect("the edit keeps the map");
        assert_eq!(heights[3 + 5 * 16], 201);
        assert!(
            heights.iter().enumerate().all(|(cell, &height)| cell == 3 + 5 * 16 || height == 128),
            "only the edited cell rises",
        );
    }

    /// Structure blocks, save metadata and the block-entity handoff must all
    /// survive the End source boundary. Checking only the generator would
    /// leave a source-level island: city blocks could reach the packet while
    /// the region writer still saw no start or reference.
    #[test]
    fn end_chunk_source_attaches_city_structures_and_references() {
        const SEED: i64 = -195_764_831;
        const CX: i32 = 45;
        const CZ: i32 = -115;
        let source = crate::worldgen_data::end_chunk_source(SEED);
        let column = source.column(CX, CZ);

        let starts = column.structure_starts();
        assert_eq!(starts.len(), 1, "the captured End city origin must persist one start");
        let city = &starts[0];
        assert_eq!(city.structure, "minecraft:end_city");
        assert_eq!((city.chunk_x, city.chunk_z), (CX, CZ));
        assert!(city.pieces_complete, "the persisted city start must carry its pieces");
        assert_eq!(city.pieces.len(), 9, "the captured city has nine template pieces");

        let packed_origin = (i64::from(CZ as u32) << 32) | i64::from(CX as u32);
        let references = column
            .structure_references()
            .get("minecraft:end_city")
            .expect("the served city chunk must retain an End-city reference");
        assert!(
            references.contains(&packed_origin),
            "the references sidecar must point at the city's origin chunk"
        );

        // This city contains no container-bearing template in the captured
        // piece sequence, so the attachment must not invent a structure
        // payload while preserving the entities the source generated.
        assert!(
            column
                .block_entities()
                .iter()
                .all(|(_, entity)| !matches!(entity, BlockEntity::Container { .. })),
            "the captured city must not gain a fabricated container sidecar"
        );
    }

    /// A banner entity's `patterns` list.
    fn banner_patterns(nbt: &lodestone_core::Nbt) -> Option<&lodestone_core::Nbt> {
        match nbt {
            lodestone_core::Nbt::Compound(fields) => fields.iter().find(|(key, _)| key == "patterns").map(|(_, value)| value),
            _ => None,
        }
    }

    /// End-city banners carry their patterned block-entity payload across the
    /// source boundary. The literal witness is the positive control for the
    /// state/event handoff, including the payload rather than only the block
    /// state.
    #[test]
    fn end_chunk_source_attaches_city_banners_with_pattern_sidecars() {
        let source = crate::worldgen_data::end_chunk_source(42);
        let column = source.column(283, 86);
        let expected = banner_patterns(&end_city_banner_nbt()).cloned();
        for position in [
            BlockPos::new(4536, 170, 1391),
            BlockPos::new(4538, 170, 1389),
            BlockPos::new(4542, 170, 1389),
        ] {
            assert!(
                column
                    .block_state_id(position.x.rem_euclid(16), position.y, position.z.rem_euclid(16))
                    .name()
                    .starts_with("minecraft:magenta_wall_banner"),
                "supported banner state must remain at {position:?}"
            );
            assert_eq!(
                column
                    .block_entities()
                    .iter()
                    .find(|(actual, _)| *actual == position)
                    .and_then(|(_, entity)| match entity {
                        BlockEntity::Opaque { nbt, .. } => banner_patterns(nbt).cloned(),
                        _ => None,
                    }),
                expected,
                "supported banner must retain its patterned sidecar at {position:?}"
            );
        }
    }

    /// The four banner records in the target End chunk survive attachment
    /// reconciliation and carry the same patterned payload as the external
    /// stream. The adjacent chunk is a negative control with three records.
    #[test]
    fn end_chunk_source_preserves_target_banner_sidecars_and_control_count() {
        let source = crate::worldgen_data::end_chunk_source(42);
        let column = source.column(283, 81);
        let expected = banner_patterns(&end_city_banner_nbt()).cloned();
        let target = [
            BlockPos::new(4535, 118, 1308),
            BlockPos::new(4537, 118, 1306),
            BlockPos::new(4541, 118, 1306),
            BlockPos::new(4543, 118, 1308),
        ];
        assert_eq!(
            column
                .block_entities()
                .iter()
                .filter(|(_, entity)| entity.type_id() == "minecraft:banner")
                .count(),
            target.len(),
            "target chunk must retain all four completed banner entities"
        );
        for position in target {
            assert!(
                column
                    .block_state_id(position.x.rem_euclid(16), position.y, position.z.rem_euclid(16))
                    .name()
                    .starts_with("minecraft:magenta_wall_banner"),
                "banner state at {position:?} must remain"
            );
            assert!(
                column
                    .block_entities()
                    .iter()
                    .any(|(actual, entity)| *actual == position
                        && matches!(entity, BlockEntity::Opaque { nbt, .. } if banner_patterns(nbt).cloned() == expected)),
                "completed banner at {position:?} must retain its patterned sidecar"
            );
        }

        // Negative control: the adjacent captured city chunk has three, not
        // four, banner records, proving the detector is location-sensitive.
        let control = source.column(283, 86);
        let surviving = control
            .block_entities()
            .iter()
            .filter(|(_, entity)| entity.type_id() == "minecraft:banner")
            .count();
        assert_eq!(surviving, 3, "control chunk must retain exactly three banner sidecars");
    }

    #[test]
    fn nether_default_columns_matches_scalar_columns_in_canonical_order() {
        let coords = [(-1, 0), (0, 0), (1, 0), (1, 1), (0, 1)];
        let scalar_source = crate::worldgen_data::nether_chunk_source(42);
        let scalar = coords
            .iter()
            .map(|&(cx, cz)| column_bytes(&scalar_source.column(cx, cz)))
            .collect::<Vec<_>>();
        let batch_source = crate::worldgen_data::nether_chunk_source(42);
        let batch = batch_source
            .columns(&coords)
            .iter()
            .map(column_bytes)
            .collect::<Vec<_>>();
        assert_eq!(batch, scalar);
    }

    #[test]
    fn out_of_range_is_air() {
        let src = StoneFloorSource::new(-64, 128, 0);
        let col = src.column(1, -3);
        assert!(!col.is_solid(0, 5000, 0));
        assert!(!col.is_solid(0, -5000, 0));
    }

    /// `claim_dragon_fight_start`'s whole reason to exist: exactly one caller
    /// among any number racing to claim the same fresh End sees `true`, so
    /// `crate::server::travel_through_end_portal` cannot spawn a second
    /// crystal ring and dragon for two connections that both reach the End on
    /// the same tick. A control proves the flag really gates rather than
    /// always answering `true` (which a stubbed-out no-op would do
    /// identically to a correct implementation on the *first* call alone).
    #[test]
    fn claim_dragon_fight_start_succeeds_exactly_once() {
        let source = crate::worldgen_data::end_chunk_source(4242);
        assert!(
            source.claim_dragon_fight_start(),
            "the first claim on a fresh source must succeed"
        );
        // Control: a second, third, and fourth claim against the *same*
        // instance must all fail — proves this is a one-shot gate, not a
        // pure function that always answers `true`.
        assert!(!source.claim_dragon_fight_start());
        assert!(!source.claim_dragon_fight_start());
        assert!(!source.claim_dragon_fight_start());

        // A second, independent source (a different End world, or the same
        // one after a restart — see `ChunkSource::claim_dragon_fight_start`'s
        // own doc for why this is a process-lifetime gate) starts unclaimed
        // again, proving the flag lives on the instance, not somewhere global.
        let other = crate::worldgen_data::end_chunk_source(4242);
        assert!(other.claim_dragon_fight_start());
    }

    /// Every source that does not serve the End answers the trait's
    /// default (`true`, "already claimed") unconditionally — the correct
    /// degradation for a source this is never meaningfully asked of. Checked
    /// against [`StoneFloorSource`] as a representative non-End source,
    /// and twice in a row to prove the default is not itself a one-shot gate
    /// that happens to start `true`.
    #[test]
    fn a_non_end_source_always_answers_the_default_claim() {
        let src = StoneFloorSource::new(-64, 128, 0);
        assert!(src.claim_dragon_fight_start());
        assert!(src.claim_dragon_fight_start());
    }

    #[test]
    fn set_block_round_trips_and_fluids_are_not_solid() {
        let mut col = ChunkColumn::new(0, 16);
        col.set_block_id(3, 5, 7, sid("minecraft:grass_block[snowy=false]"));
        col.set_block_id(3, 4, 7, sid("minecraft:water[level=0]"));
        assert_eq!(
            col.block_state_id(3, 5, 7),
            sid("minecraft:grass_block[snowy=false]")
        );
        // Grass is solid; water is a fluid and therefore not solid.
        assert!(col.is_solid(3, 5, 7));
        assert!(!col.is_solid(3, 4, 7));
        // Only the grass block counts toward solidity.
        assert_eq!(col.solid_count(), 1);
    }

    #[test]
    fn raw_window_bulk_preserves_palette_history_and_ordered_upper_spills() {
        let air = Block::Air.default_state();
        let dirt = Block::Dirt.default_state();
        let leaves = sid("minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]");
        let cave = Block::CaveAir.default_state();
        let void = Block::VoidAir.default_state();
        let gold = Block::GoldBlock.default_state();
        let emerald = Block::EmeraldBlock.default_state();
        let water = sid("minecraft:water[level=0]");
        let diamond = Block::DiamondBlock.default_state();
        let palette = vec![dirt, air, leaves, dirt, cave, void, gold];
        let mut source = vec![1; 17 * 256 - 3];
        source[0] = 0;
        source[256 + 7] = 2;
        source[2 * 256 + 9] = 4;
        source[3 * 256 + 9] = 5;
        source[17 * 256 - 4] = u16::MAX;
        let spills = [
            (-1, -3, -2, emerald),
            (0, -37, 0, air),
            (-1, -3, -2, water),
            (0, -2, 0, diamond),
            (0, -38, 0, diamond),
        ];
        let make = |spills: &[(i32, i32, i32, StateId)], maps| {
            ChunkColumn::from_raw_window(
                -37, 17, 35, palette.clone(), &source,
                std::array::from_fn(|_| "minecraft:nether_wastes".to_owned()),
                spills, maps,
            )
        };
        #[cfg(feature = "gen-counters")]
        lodestone_worldgen::counters::reset();
        let actual = make(&spills, true);
        #[cfg(feature = "gen-counters")]
        {
            let counts = lodestone_worldgen::counters::snapshot();
            assert_eq!(counts.raw_window_source_cells, 4349);
            assert_eq!(counts.raw_window_summary_cells, 4864);
            assert_eq!(counts.raw_window_bulk_sections, 3);
            assert_eq!(counts.heightmap_scan_cells, 0);
        }
        assert_eq!(actual.palette(), &[air, dirt, leaves, cave, void, gold, emerald, water]);
        assert_eq!(actual.uniform_section_palette_index(1), Some(0));
        assert_eq!(actual.uniform_section_palette_index(2), None);
        let mut expected = vec![0u16; 35 * 256];
        expected[256 + 7] = 2;
        expected[2 * 256 + 9] = 3;
        expected[3 * 256 + 9] = 4;
        expected[34 * 256 + 14 * 16 + 15] = 7;
        let mut indices = Vec::new();
        for section in 0..actual.section_count() {
            actual.append_section_cells(section, &mut indices);
        }
        assert_eq!(indices, expected);
        assert_eq!(actual.section_ticking_counts(), &[1, 0, 0]);
        let mut maps = [[0u16; 256]; 3];
        maps[0][7] = 2;
        maps[1][7] = 2;
        for map in &mut maps {
            map[14 * 16 + 15] = 35;
        }
        assert_eq!(actual.client_heightmaps_raw(), Some(maps));
        assert_eq!(actual.client_heightmaps(), Some(&derive_client_heightmaps_naive(&actual)));
        let without_spill = make(&[], true);
        assert_ne!(without_spill.client_heightmaps_raw(), Some(maps));
        let without_maps = make(&spills, false);
        assert_eq!(column_bytes(&without_maps), column_bytes(&actual));
        assert_eq!(without_maps.section_ticking_counts(), actual.section_ticking_counts());
        assert!(without_maps.client_heightmaps().is_none());
    }

    #[test]
    fn raw_window_omits_summaries_when_no_maps_or_ticking_states() {
        let air = Block::Air.default_state();
        let end_stone = Block::EndStone.default_state();
        let gold = Block::GoldBlock.default_state();
        let emerald = Block::EmeraldBlock.default_state();
        let mut source = vec![2; 17 * 256];
        source[0] = 0;
        let make = |maps| ChunkColumn::from_raw_window(
            -37, 17, 35, vec![end_stone, gold, air], &source,
            std::array::from_fn(|_| "minecraft:the_end".to_owned()),
            &[(15, -3, 14, emerald)], maps,
        );
        #[cfg(feature = "gen-counters")]
        lodestone_worldgen::counters::reset();
        let without_maps = make(false);
        #[cfg(feature = "gen-counters")]
        {
            let counts = lodestone_worldgen::counters::snapshot();
            assert_eq!(counts.raw_window_source_cells, 4352);
            assert_eq!(counts.raw_window_summary_cells, 0);
            assert_eq!(counts.raw_window_bulk_sections, 3);
        }
        assert_eq!(without_maps.palette(), &[air, end_stone, gold, emerald]);
        assert_eq!(without_maps.block_state_id(0, -37, 0), end_stone);
        assert_eq!(without_maps.block_state_id(15, -3, 14), emerald);
        assert_eq!(without_maps.section_ticking_counts(), &[0; 3]);
        assert!(without_maps.client_heightmaps().is_none());
        let with_maps = make(true);
        #[cfg(feature = "gen-counters")]
        assert_eq!(lodestone_worldgen::counters::snapshot().raw_window_summary_cells, 4864);
        assert_eq!(column_bytes(&without_maps), column_bytes(&with_maps));
        assert_eq!(without_maps.section_ticking_counts(), with_maps.section_ticking_counts());
        let maps = with_maps.client_heightmaps_raw().unwrap();
        assert_eq!(maps.map(|map| map[0]), [1; 3]);
        assert_eq!(maps.map(|map| map[15 + 14 * 16]), [35; 3]);
    }

    #[test]
    fn bounded_heightmaps_preserve_partial_window_and_live_removal() {
        let mut column = ChunkColumn::new(-37, 35);
        column.set_block_id(3, -32, 4, Block::Stone.default_state());
        column.set_block_id(3, -27, 4, sid("minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]"));
        column.set_block_id(3, -3, 4, sid("minecraft:water[level=0]"));
        column.prime_client_heightmaps();
        let index = 3 + 4 * 16;
        assert_eq!(column.client_heightmaps_raw().unwrap().map(|map| map[index]), [35, 35, 35]);
        assert_eq!(column.client_heightmaps(), Some(&derive_client_heightmaps_naive(&column)));
        reset_heightmap_repairs();
        column.apply_ordered_block_id_batch(&[
            (3, -3, 4, Block::Air.default_state()),
            (3, -3, 4, Block::Air.default_state()),
        ]);
        assert_eq!(heightmap_repairs(), 1);
        assert_eq!(column.client_heightmaps_raw().unwrap().map(|map| map[index]), [11, 11, 6]);
        assert_eq!(column.client_heightmaps(), Some(&derive_client_heightmaps_naive(&column)));
        let scan_height = column.air_above_y().min(column.min_y + column.height) - column.min_y;
        assert_eq!(scan_height, 16);
        let mut bounded_reads = 0;
        let bounded = client_heightmap_values_at(column.min_y, scan_height, |y| {
            bounded_reads += 1;
            column.block_state_id(0, y, 0)
        });
        let mut full_reads = 0;
        let full = client_heightmap_values_at(column.min_y, column.height, |y| {
            full_reads += 1;
            column.block_state_id(0, y, 0)
        });
        assert_eq!(bounded, [0; 3]);
        assert_eq!(bounded, full);
        assert_eq!((bounded_reads, full_reads), (16, 35));
        column.set_block_id(5, -3, 8, Block::CaveAir.default_state());
        assert_eq!(column.air_above_y(), 11);
        assert_eq!(derive_client_heightmaps(&column), derive_client_heightmaps_naive(&column));
        assert_eq!(column.client_heightmaps_raw().unwrap().map(|map| map[5 + 8 * 16]), [0; 3]);
        let wrong = client_heightmap_values_at(column.min_y, 17, |y| {
            if y == -3 { sid("minecraft:water[level=0]") } else { column.block_state_id(3, y, 4) }
        });
        assert_ne!(wrong, [35; 3], "a generated-height bound must miss the upper spill");
    }

    #[test]
    fn ordered_batch_matches_scalar_writes_and_repairs_unique_xz_cells() {
        let writes = [
            (3, 5, 7, sid("minecraft:grass_block[snowy=false]")),
            (3, 4, 7, sid("minecraft:water[level=0]")),
            (3, 5, 7, sid("minecraft:dirt")),
            (9, 2, 7, sid("minecraft:stone")),
        ];
        let mut scalar = ChunkColumn::new(0, 16);
        scalar.prime_client_heightmaps();
        for &(x, y, z, state) in &writes {
            scalar.set_block_id(x, y, z, state);
        }

        let mut batch = ChunkColumn::new(0, 16);
        batch.prime_client_heightmaps();
        reset_heightmap_repairs();
        batch.apply_ordered_block_id_batch(&writes);

        assert_eq!(column_bytes(&batch), column_bytes(&scalar));
        assert_eq!(batch.client_heightmaps_raw(), scalar.client_heightmaps_raw());
        assert_eq!(batch.section_ticking_counts(), scalar.section_ticking_counts());
        assert_eq!(heightmap_repairs(), 2, "one repair per dirty XZ cell");
    }

    #[test]
    fn shared_heightmap_refresh_matches_naive_for_negative_fluid_leaves_and_removal() {
        let mut column = ChunkColumn::new(-64, 16);
        let x = 4;
        let z = 9;
        let leaves = sid("minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]");
        let waterlogged = sid("minecraft:oak_slab[type=bottom,waterlogged=true]");
        let water = sid("minecraft:water[level=0]");

        column.set_block_id(x, -61, z, leaves);
        column.set_block_id(x, -62, z, waterlogged);
        column.set_block_id(x, -63, z, water);
        column.prime_client_heightmaps();

        let assert_matches_naive = |column: &ChunkColumn| {
            let expected = derive_client_heightmaps_naive(column);
            let actual = column.client_heightmaps_raw().unwrap();
            for (map_index, type_id) in [1u32, 4, 5].into_iter().enumerate() {
                assert_eq!(
                    actual[map_index][x as usize + z as usize * 16],
                    expected.get(type_id).unwrap().get(x as usize, z as usize) as u16,
                    "map {type_id} must retain its own first matching state"
                );
            }
        };

        assert_matches_naive(&column);
        assert_eq!(column.client_heightmaps_raw().unwrap()[2][x as usize + z as usize * 16], 3);

        column.set_block_id(x, -61, z, sid("minecraft:air"));
        assert_matches_naive(&column);
        assert_eq!(column.client_heightmaps_raw().unwrap()[0][x as usize + z as usize * 16], 3);

        column.set_block_id(x, -62, z, sid("minecraft:air"));
        assert_matches_naive(&column);
        assert_eq!(column.client_heightmaps_raw().unwrap()[1][x as usize + z as usize * 16], 2);
    }

    #[test]
    fn client_surface_heightmap_excludes_all_air_variants() {
        let mut column = ChunkColumn::new(-64, 16);
        column.set_block_id(3, -63, 5, sid("minecraft:stone"));
        column.set_block_id(3, -62, 5, sid("minecraft:cave_air"));
        column.set_block_id(3, -61, 5, sid("minecraft:void_air"));
        column.prime_client_heightmaps();
        let index = 3 + 5 * 16;
        assert_eq!(column.client_heightmaps_raw().unwrap()[0][index], 2);
        assert_ne!(column.client_heightmaps_raw().unwrap()[0][index], 4);

        column.set_block_id(3, -60, 5, sid("minecraft:dirt"));
        assert_eq!(column.client_heightmaps_raw().unwrap()[0][index], 5);
        column.set_block_id(3, -60, 5, sid("minecraft:cave_air"));
        assert_eq!(column.client_heightmaps_raw().unwrap()[0][index], 2);
    }

    #[test]
    fn ordered_batch_validates_before_mutating() {
        let mut column = ChunkColumn::new(0, 16);
        column.prime_client_heightmaps();
        let before = column_bytes(&column);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            column.apply_ordered_block_id_batch(&[
                (1, 1, 1, sid("minecraft:stone")),
                (16, 1, 1, sid("minecraft:dirt")),
            ]);
        }));
        assert!(result.is_err());
        assert_eq!(column_bytes(&column), before);
    }

    #[test]
    fn skipped_heightmap_repair_negative_control_is_detectable() {
        let mut column = ChunkColumn::new(0, 16);
        column.prime_client_heightmaps();
        let id = column.intern_state_id(sid("minecraft:stone"));
        column.write_block_id(0, 3, 0, id);
        let expected = derive_client_heightmaps(&column);
        let actual = column.client_heightmaps_raw().unwrap();
        assert_ne!(
            actual[0][0],
            expected.get(1).unwrap().get(0, 0) as u16,
            "the control must observe a stale map when repair is skipped"
        );
    }

    /// Canonical byte serialisation of a column's full content — `min_y`,
    /// `height`, the raw-id palette, the block-index grid, then the biome
    /// quarts (length-prefixed strings). Two columns
    /// with identical bytes here carry identical block/biome content; this
    /// is the "emitted byte sequence" the determinism control below
    /// compares, standing in for the real wire encoding (which lives behind
    /// `ServerProtocol` in the protocol crates, not reachable from here).
    fn column_bytes(col: &ChunkColumn) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&col.min_y.to_le_bytes());
        out.extend_from_slice(&col.height.to_le_bytes());
        out.extend_from_slice(&(col.palette.len() as u32).to_le_bytes());
        for state in &col.palette {
            out.extend_from_slice(&state.raw().to_le_bytes());
        }
        // Section by section, which is also the storage order — the bytes are
        // identical to the flat `Vec<u16>` walk this replaced, because
        // `append_section_cells` emits the same
        // `(y_local * 16 + z) * 16 + x` sequence.
        let mut cells = Vec::new();
        for s in 0..col.section_count() {
            cells.clear();
            col.append_section_cells(s, &mut cells);
            for &id in &cells {
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
        for s in &col.biome_quarts {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    /// **Determinism control.** Generates the same small patch of real,
    /// RNG-bearing overworld columns (surface + aquifer + ore/feature
    /// placement — the pipeline `crate::worldgen_data::overworld_chunk_source`
    /// serves to a real client) through [`generate_columns_parallel`]
    /// repeatedly, and asserts every repeat's emitted byte sequence
    /// ([`column_bytes`]) is identical to a plain serial baseline built by
    /// calling `source.column()` in a straight loop.
    ///
    /// This is the property the test checks: per-chunk RNG is positionally
    /// seeded (`set_decoration_seed`/`set_feature_seed`/
    /// `fork_positional`/`from_hash_of` —
    /// `lodestone-worldgen`'s own doc comments), so there is no shared RNG
    /// stream for thread scheduling to desync. A single passing repeat would
    /// prove nothing about a scheduling-dependent race, so this runs the
    /// parallel path many times against one fixed coordinate set, over a
    /// coordinate count that does not divide evenly across
    /// `available_parallelism` worker batches, to make an off-by-one batch
    /// boundary bug visible if one existed.
    ///
    /// The generator cache is per source instance, keyed by `(cx, cz)` and
    /// capped at 512 entries. The serial baseline and each of the eight
    /// parallel repeats use an independently constructed
    /// `overworld_chunk_source(42)`, so every run begins with a cold cache and
    /// exercises concurrent misses across `available_parallelism` threads.
    /// A byte match across all nine constructions verifies cross-construction
    /// determinism rather than a shared-cache replay.
    ///
    /// Deliberately small (2×3 = 6 columns) and a modest repeat count: this
    /// runs the real generator, which is not cheap, and this test executes
    /// in debug mode as part of the ordinary crate test suite on a shared,
    /// loaded machine.
    #[test]
    fn parallel_generation_is_deterministic_and_matches_serial() {
        let coords: Vec<(i32, i32)> = vec![(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (2, -1)];

        // Independent construction: its own generator, its own empty
        // pre-ore cache. Not reused below, so it cannot warm anything the
        // parallel repeats then hit.
        let serial_source = crate::overworld_chunk_source(42);
        let serial: Vec<Vec<u8>> = coords
            .iter()
            .map(|&(cx, cz)| column_bytes(&serial_source.column(cx, cz)))
            .collect();

        const REPEATS: usize = 8;
        for rep in 0..REPEATS {
            // Fresh, independently constructed source *every* repeat — a
            // cold cache each time, so every repeat is a real concurrent
            // miss across the parallel workers, not a hit against a cache
            // some earlier repeat (or the serial baseline) already filled.
            let parallel_source = crate::overworld_chunk_source(42);
            let parallel = generate_columns_parallel(&parallel_source, &coords);
            assert_eq!(
                parallel.len(),
                coords.len(),
                "repeat {rep}: chunk count changed under parallel generation"
            );
            let parallel_bytes: Vec<Vec<u8>> =
                parallel.iter().map(column_bytes).collect();
            assert_eq!(
                parallel_bytes, serial,
                "repeat {rep}: parallel generation from an independently constructed source \
                 diverged from the serial baseline's independently constructed source — a \
                 scheduling-dependent RNG desync or a cross-construction non-determinism bug \
                 would show up here"
            );
        }
    }

    /// A source whose every column costs a fixed amount of *blocking*
    /// wall-clock, which is the one property of real worldgen this fixture measures
    /// about. Deliberately hand-written rather than
    /// [`crate::overworld_chunk_source`]: the real generator carries a
    /// 512-entry memo cache that would absorb a second request for the same
    /// `(cx, cz)` and make any count- or duration-based gate vacuous.
    /// This source has no cache, so both arms below pay the same cost.
    struct SleepyChunkSource {
        per_column: std::time::Duration,
    }

    impl ChunkSource for SleepyChunkSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            std::thread::sleep(self.per_column);
            ChunkColumn::new(-64, 32)
        }

        fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
            // The gates only ever call `column()`, so this is the plain
            // column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state_id(lx, y, lz)
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            // The gates only ever call `column()`, so this is the plain
            // column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
        }

        // A wall-clock-only fixture: it exists to make `column()` take a fixed
        // amount of blocking time, and no gate here writes blocks. Deliberately
        // discards rather than inheriting a silent default — the point of
        // such a choice must be explicit per implementor.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// The world tick's period, scaled down so the gate runs in well under a
    /// second. `run_tick_loop` uses 50 ms (`crate::tick::TICK_PERIOD`); the
    /// shape that matters — a task parked on `sleep`/`sleep_until` — is
    /// identical.
    const GATE_TICK_PERIOD: std::time::Duration = std::time::Duration::from_millis(10);

    /// Chunk generation must not block the async runtime.
    ///
    /// # What this measures, and what it would miss
    ///
    /// `generate_columns_parallel` makes generation *parallel*,
    /// which is a throughput property. This gate is about *latency*: whether a
    /// task that is supposed to run every `GATE_TICK_PERIOD` still gets to run
    /// while a generation burst is in flight. A test that only checked the
    /// returned columns were correct could not see this at all — both arms
    /// below return byte-identical output.
    ///
    /// The stakes are not theoretical. `crates/lodestone-shell/src/net.rs`
    /// builds the server's runtime with
    /// `tokio::runtime::Builder::new_current_thread()`, so the connection task
    /// and `crate::tick::run_tick_loop` share **one** thread; blocking it
    /// stalls every task in the process, so an inline generation burst drops
    /// one or more 50 ms world ticks.
    ///
    /// # The negative control is the second arm, permanently
    ///
    /// `generate_columns_parallel` stays in the native tree (it is what
    /// `SourceRef::Borrowed` uses there), so the inline control is measurable
    /// alongside the offloaded path. The control must record **zero** ticks.
    /// The measured comparison is:
    /// offloaded 20 ticks over 214 ms, blocking 0 ticks over 209 ms.
    ///
    /// # Predicting the value, not just the sign
    ///
    /// Asserting merely "more ticks than the control" would be satisfied by a
    /// single tick, so the two competing hypotheses are computed from the
    /// measured wall-clock instead: if generation is genuinely offloaded the
    /// count is about `elapsed / GATE_TICK_PERIOD`; if it silently still
    /// blocks, it is 0. Those are far enough apart that a halved tolerance on
    /// the first cannot be met by the second.
    ///
    /// # Duration species
    ///
    /// The counter is created inside this test and read as an absolute over a
    /// bracketed operation, so nothing outlives the gate. `crate::tick::TickClock`
    /// accumulates MSPT/TPS/overrun over a whole server lifetime, so it cannot
    /// distinguish a stall during this bracket from a lifetime average.
    #[tokio::test]
    async fn offloaded_generation_lets_a_timer_task_keep_running() {
        // Load-bearing, not decoration. Under `flavor = "multi_thread"` a
        // second worker thread would poll the timer while the core thread
        // blocked, so the control arm would pass too and this gate would
        // measure nothing. Current-thread is also the production flavour.
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread,
            "this gate is only meaningful on a current-thread runtime — on a \
             multi-thread runtime the blocking control below passes too"
        );

        // 96 columns at 20 ms each: long enough that a correctly-offloaded
        // burst spans many tick periods at any plausible worker count, and
        // short enough to keep the test well under a second.
        let coords: Vec<(i32, i32)> = (0..96).map(|i| (i % 16, i / 16)).collect();
        let per_column = std::time::Duration::from_millis(20);

        // --- Arm 1: offloaded generation. ---
        let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ticker = {
            let ticks = Arc::clone(&ticks);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(GATE_TICK_PERIOD).await;
                    ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        };
        // Let the ticker reach its first await point before the clock starts,
        // so arm 1 and arm 2 begin from the same state.
        tokio::task::yield_now().await;
        let started = lodestone_time::Instant::now();
        let _offloaded = generate_columns_offloaded(
            Arc::new(SleepyChunkSource { per_column }),
            coords.clone(),
        )
        .await;
        let offloaded_elapsed = started.elapsed();
        // Read before any further await, so a catch-up burst of timer wakeups
        // cannot inflate the count after the operation ended.
        let offloaded_ticks = ticks.load(std::sync::atomic::Ordering::Relaxed);
        ticker.abort();

        // --- Arm 2: the permanent negative control, blocking. ---
        let control_ticks_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let control_ticker = {
            let ticks = Arc::clone(&control_ticks_counter);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(GATE_TICK_PERIOD).await;
                    ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        };
        tokio::task::yield_now().await;
        let control_started = lodestone_time::Instant::now();
        let _blocking = generate_columns_parallel(
            &SleepyChunkSource { per_column },
            &coords,
        );
        let control_elapsed = control_started.elapsed();
        let control_ticks = control_ticks_counter.load(std::sync::atomic::Ordering::Relaxed);
        control_ticker.abort();

        // Both arms must actually have taken long enough to be worth
        // measuring — otherwise the tick counts below are trivially satisfied
        // and this whole gate is a precondition-species vacuity. Failing
        // rather than skipping, deliberately.
        assert!(
            offloaded_elapsed >= GATE_TICK_PERIOD * 4,
            "offloaded burst finished in {offloaded_elapsed:?}, too fast to say anything \
             about stalling — raise `per_column` or the column count"
        );
        assert!(
            control_elapsed >= GATE_TICK_PERIOD * 4,
            "control burst finished in {control_elapsed:?}, too fast to be a control"
        );

        // The two competing hypotheses, derived from the measured wall-clock
        // rather than hardcoded: offloaded ⇒ ~elapsed/period, still-blocking
        // ⇒ 0. Halved to absorb scheduling jitter and the timer's own
        // coarseness; the wrong hypothesis is nowhere near it.
        let expected = (offloaded_elapsed.as_millis() / GATE_TICK_PERIOD.as_millis()) as u64;
        let floor = (expected / 2).max(3);
        assert!(
            offloaded_ticks >= floor,
            "the timer task ran {offloaded_ticks} times during a {offloaded_elapsed:?} \
             offloaded generation burst; expected at least {floor} (≈{expected} periods of \
             {GATE_TICK_PERIOD:?}). A count near 0 means generation is still blocking the \
             runtime — i.e. the Rayon handoff is not being reached"
        );

        // The control. If this is ever non-zero, `generate_columns_parallel`
        // has stopped being synchronous and the arm above is no longer
        // measuring a difference.
        assert_eq!(
            control_ticks, 0,
            "the blocking control let the timer task run {control_ticks} times over \
             {control_elapsed:?} — it is supposed to starve it completely, so this gate is \
             no longer distinguishing the two paths"
        );
    }

    /// The property the two arms above must **share**: offloading changes when
    /// generation runs, never what it produces. Without this, a
    /// `generate_columns_offloaded` that silently returned the wrong columns
    /// (or the right columns in the wrong order) would still pass the
    /// stall gate, since that one only counts timer wakeups.
    #[tokio::test]
    async fn offloading_does_not_change_the_columns_or_their_order() {
        let coords: Vec<(i32, i32)> = vec![(3, -7), (0, 0), (-2, 5), (11, 11), (-9, -9)];
        // A fresh, independent source per arm — same reasoning as
        // `SleepyChunkSource`'s doc comment and as the determinism test above.
        let serial: Vec<StateId> = coords
            .iter()
            .map(|&(cx, cz)| {
                let source = StoneFloorSource::new(-64, 128, 0);
                source.column(cx, cz).block_state_id(0, -1, 0)
            })
            .collect();

        let offloaded = generate_columns_offloaded(
            Arc::new(StoneFloorSource::new(-64, 128, 0)),
            coords.clone(),
        )
        .await;

        assert_eq!(
            offloaded.len(),
            coords.len(),
            "offloaded generation returned {} columns for {} coordinates",
            offloaded.len(),
            coords.len()
        );
        let offloaded_states: Vec<StateId> = offloaded
            .iter()
            .map(|column| column.block_state_id(0, -1, 0))
            .collect();
        assert_eq!(
            offloaded_states, serial,
            "offloaded generation must hand back columns aligned index-for-index with \
             `coords` — the wire order depends on it (see `generate_columns_parallel`)"
        );
    }

    /// Two production batch calls must share the Rayon pool rather than each
    /// creating a Tokio blocking task that fans out another `P` scoped threads.
    /// The source records both simultaneous occupancy and worker identities;
    /// either exceeding the pool size would expose nested/per-batch thread
    /// creation. The returned digest and order are checked at the same time so
    /// the regression cannot be reduced to a thread-count-only fixture.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_offloaded_batches_share_rayon_workers_and_content() {
        use std::collections::{HashSet, hash_map::DefaultHasher};
        use std::hash::{Hash, Hasher};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex;

        struct BatchProbe {
            active: AtomicUsize,
            max_active: AtomicUsize,
            workers: Mutex<HashSet<std::thread::ThreadId>>,
        }

        impl ChunkSource for BatchProbe {
            fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                self.workers
                    .lock()
                    .expect("batch worker log poisoned")
                    .insert(std::thread::current().id());
                std::thread::sleep(std::time::Duration::from_millis(3));
                let mut column = ChunkColumn::new(0, 16);
                let state = if (cx as i64 * 31 + cz as i64 * 17) & 1 == 0 {
                    "minecraft:stone"
                } else {
                    "minecraft:dirt"
                };
                column.set_block_id(0, 0, 0, sid(state));
                self.active.fetch_sub(1, Ordering::SeqCst);
                column
            }

            fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
                sid("minecraft:air")
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_string()
            }

            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
        }

        fn coords(offset: i32, count: usize) -> Vec<(i32, i32)> {
            (0..count)
                .map(|i| {
                    let x = offset + i as i32;
                    (x, -x - 1)
                })
                .collect()
        }

        fn digest(columns: &[ChunkColumn], coords: &[(i32, i32)]) -> u64 {
            let mut hasher = DefaultHasher::new();
            for (&(cx, cz), column) in coords.iter().zip(columns) {
                cx.hash(&mut hasher);
                cz.hash(&mut hasher);
                column.block_state_id(0, 0, 0).hash(&mut hasher);
            }
            hasher.finish()
        }

        let workers = rayon::current_num_threads().max(1);
        let first_coords = coords(0, workers * 3);
        let second_coords = coords(10_000, workers * 3);
        let source = Arc::new(BatchProbe {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            workers: Mutex::new(HashSet::new()),
        });
        let (first, second) = tokio::join!(
            generate_columns_offloaded(Arc::clone(&source), first_coords.clone()),
            generate_columns_offloaded(Arc::clone(&source), second_coords.clone()),
        );

        assert_eq!(first.len(), first_coords.len());
        assert_eq!(second.len(), second_coords.len());
        assert_eq!(
            digest(&first, &first_coords),
            digest_serial(&first_coords),
            "the first batch changed generated content or order"
        );
        assert_eq!(
            digest(&second, &second_coords),
            digest_serial(&second_coords),
            "the second batch changed generated content or order"
        );
        let max_active = source.max_active.load(Ordering::SeqCst);
        let unique_workers = source
            .workers
            .lock()
            .expect("batch worker log poisoned")
            .len();
        assert!(
            max_active <= workers,
            "concurrent batches exceeded the shared Rayon pool: {max_active} active, {workers} workers"
        );
        assert!(
            unique_workers <= workers,
            "batch dispatch created per-batch workers: observed {unique_workers}, pool has {workers}"
        );

        fn digest_serial(coords: &[(i32, i32)]) -> u64 {
            let mut hasher = DefaultHasher::new();
            for &(cx, cz) in coords {
                cx.hash(&mut hasher);
                cz.hash(&mut hasher);
                let state = if (cx as i64 * 31 + cz as i64 * 17) & 1 == 0 {
                    "minecraft:stone"
                } else {
                    "minecraft:dirt"
                };
                sid(state).hash(&mut hasher);
            }
            hasher.finish()
        }
    }

    /// [`generate_columns_yielding`]/[`map_columns_yielding`] are what
    /// `generate_columns_offloaded`/`generate_and_encode_columns_offloaded`'s
    /// wasm32 branches call instead of the native Rayon branch of
    /// `generate_columns_parallel`; native worker dispatch is not compiled
    /// into the browser target.
    /// wasm32-only code cannot be exercised by a native `cargo test`, so this
    /// gate proves the one property that is target-independent by
    /// construction: **the yield closure runs exactly once per column,
    /// strictly interleaved, never two columns before a yield.** Production
    /// substitutes [`yield_to_browser`] for the closure under test; nothing
    /// about *that* substitution can change the interleaving, only what a
    /// yield does once it happens.
    ///
    /// # Counter, not duration
    ///
    /// A wall-clock measurement of this loop would be exactly the duration
    /// species this repo's evidence rules warn about, and it could not see the
    /// property that matters anyway — *when* a yield lands relative to
    /// generation, not how long the whole call took. The log below is a
    /// counter: an ordered event sequence, checked against a value **predicted**
    /// from `coords.len()` rather than merely "at least one yield happened
    /// somewhere". Mismatches are collected rather than asserted one at a time
    /// inside the loop, so a broken gate reports every wrong position instead
    /// of only the first.
    #[tokio::test]
    async fn yielding_generation_yields_after_every_single_column() {
        use std::sync::Mutex;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Event {
            Column,
            Yield,
        }

        struct RecordingSource {
            log: Arc<Mutex<Vec<Event>>>,
        }

        impl ChunkSource for RecordingSource {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                self.log.lock().unwrap().push(Event::Column);
                ChunkColumn::new(-64, 32)
            }

            fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
                sid("minecraft:air")
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_string()
            }

            // Discarded by design (the explicit-choice rule) — this
            // fixture only ever reads.
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
        }

        /// Positional diff, collected rather than asserted per-element — a
        /// length mismatch is reported as its own entry rather than panicking
        /// the comparison outright, so one failing run names every wrong
        /// position at once.
        fn mismatches(expected: &[Event], observed: &[Event]) -> Vec<String> {
            let mut out: Vec<String> = expected
                .iter()
                .zip(observed.iter())
                .enumerate()
                .filter(|(_, (want, got))| want != got)
                .map(|(i, (want, got))| format!("position {i}: expected {want:?}, got {got:?}"))
                .collect();
            if expected.len() != observed.len() {
                out.push(format!(
                    "length mismatch: expected {} events, got {}",
                    expected.len(),
                    observed.len()
                ));
            }
            out
        }

        // 7 columns: enough that a "yield once per batch" or "yield every two
        // columns" bug cannot coincide with a "yield once per column" implementation by
        // accident (an even split would).
        let coords: Vec<(i32, i32)> = (0..7).map(|i| (i, -i)).collect();
        let expected: Vec<Event> = coords.iter().flat_map(|_| [Event::Column, Event::Yield]).collect();

        // --- Arm 1: per-column yielding. `generate_columns_yielding`, the exact function
        // `generate_columns_offloaded`'s wasm32 branch calls — with a yield
        // closure that logs into the same sequence `RecordingSource::column`
        // does, and *really* suspends (`tokio::task::yield_now`), so a closure
        // that logged without actually yielding could not pass by accident.
        let log = Arc::new(Mutex::new(Vec::new()));
        let source = RecordingSource { log: Arc::clone(&log) };
        let yield_log = Arc::clone(&log);
        let produced = generate_columns_yielding(&source, &coords, move || {
            let yield_log = Arc::clone(&yield_log);
            async move {
                yield_log.lock().unwrap().push(Event::Yield);
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(produced.len(), coords.len(), "must still generate every requested column");

        let observed = log.lock().unwrap().clone();
        let found = mismatches(&expected, &observed);
        assert!(
            found.is_empty(),
            "generate_columns_yielding must alternate Column, Yield, Column, Yield, … — one \
             yield strictly after every column, never two columns before a yield; got \
             {observed:?}, wanted {expected:?}; mismatches: {found:?}"
        );

        // --- Arm 2: the permanent negative control. A batched implementation that generates
        // every column first and yields only afterwards — what
        // `generate_columns_parallel`'s own `coords.len() <= 1` fast path looks
        // like when driven in a loop with no yields threaded through it at all,
        // and the shape a batched-instead-of-per-column implementation would produce
        // too. If this control ever stops failing the same check, the check
        // above has stopped distinguishing the two shapes.
        let control_log = Arc::new(Mutex::new(Vec::new()));
        let control_source = RecordingSource { log: Arc::clone(&control_log) };
        for &(cx, cz) in &coords {
            let _ = control_source.column(cx, cz);
        }
        let control_observed = control_log.lock().unwrap().clone();
        let control_found = mismatches(&expected, &control_observed);
        assert!(
            !control_found.is_empty(),
            "negative control: an unyielded batch must fail the alternation check, or this \
             gate is not distinguishing yielded generation from the pre-fix shape; got \
             {control_observed:?}"
        );
    }

    #[test]
    fn generated_state_block_entity_scan_emits_potent_sulfur_and_rejects_plain_sulfur() {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block_id(4, 14, 1, sid("minecraft:potent_sulfur"));
        column.populate_missing_block_entity_states(-25, -25);

        assert_eq!(column.block_entities().len(), 1);
        let (position, entity) = &column.block_entities()[0];
        assert_eq!(*position, BlockPos::new(-396, 14, -399));
        assert!(matches!(
            entity,
            BlockEntity::Opaque { id, nbt }
                if id.name() == "minecraft:potent_sulfur"
                    && matches!(nbt, lodestone_core::Nbt::End)
        ));
        assert_eq!(
            lodestone_data::block_entity_types::block_entity_type_id(entity.type_id())
                .map(|id| id.raw()),
            Some(48),
            "the state-owned potent-sulfur record uses registry slot 48"
        );

        // Negative control: ordinary sulfur is a terrain block, not a
        // state-owned block entity, so the table-driven scan must stay empty.
        let mut control = ChunkColumn::new(-64, 384);
        control.set_block_id(4, 14, 1, sid("minecraft:sulfur"));
        control.populate_missing_block_entity_states(-25, -25);
        assert!(
            control.block_entities().is_empty(),
            "plain sulfur must not create a block-entity sidecar"
        );
    }

    /// A spawner block with no saved data still becomes a spawner block entity,
    /// so it is ticked. An opaque record is never ticked.
    #[test]
    fn a_dataless_spawner_block_becomes_a_ticking_spawner() {
        let mut column = ChunkColumn::new(-64, 384);
        column.set_block_id(4, 14, 1, sid("minecraft:spawner"));
        column.populate_missing_block_entity_states(0, 0);
        assert_eq!(column.block_entities().len(), 1);
        assert!(matches!(
            &column.block_entities()[0].1,
            BlockEntity::Spawner(state) if *state == crate::mob_spawner::SpawnerState::default()
        ));
    }

    #[test]
    fn generated_block_entities_all_variants_cross_the_server_boundary() {
        let mut column = crate::ChunkSource::column_at(&crate::overworld_chunk_source(42), 0, 0, crate::ChunkGenerationStage::Shaped);
        let entities = [
            lodestone_worldgen::block_entities::GeneratedBlockEntity::Beehive {
                x: 1,
                y: 65,
                z: 2,
                bees: vec![lodestone_worldgen::block_entities::BeeOccupant {
                    ticks_in_hive: 7,
                    min_ticks_in_hive: 600,
                }],
            },
            lodestone_worldgen::block_entities::GeneratedBlockEntity::DungeonChest {
                x: 3,
                y: 20,
                z: 4,
                facing: "north".to_owned(),
                loot_table: "minecraft:chests/simple_dungeon".to_owned(),
                loot_table_seed: 99,
            },
            lodestone_worldgen::block_entities::GeneratedBlockEntity::DungeonSpawner {
                x: 5,
                y: 30,
                z: 6,
                entity_type: lodestone_data::entity_type::EntityType::Zombie.into(),
            },
        ];
        column.add_generated_block_entities(&entities);

        let converted = column.block_entities();
        assert_eq!(converted.len(), entities.len());
        assert!(matches!(
            &converted[0],
            (BlockPos { x: 1, y: 65, z: 2 }, BlockEntity::Opaque { id, nbt })
                if id.name() == "minecraft:beehive"
                    && matches!(nbt, lodestone_core::Nbt::Compound(fields)
                        if fields.iter().any(|(key, value)| key == "bees"
                            && matches!(value, lodestone_core::Nbt::List { elements, .. }
                                if elements.len() == 1)))
        ));
        assert!(matches!(
            &converted[1],
            (BlockPos { x: 3, y: 20, z: 4 }, BlockEntity::Opaque { id, nbt })
                if id.name() == "minecraft:chest"
                    && matches!(nbt, lodestone_core::Nbt::Compound(fields)
                        if fields.iter().any(|(key, value)| key == "LootTable"
                            && matches!(value, lodestone_core::Nbt::String(table)
                                if table == "minecraft:chests/simple_dungeon"))
                        && fields.iter().any(|(key, value)| key == "LootTableSeed"
                            && matches!(value, lodestone_core::Nbt::Long(seed) if *seed == 99)))
        ));
        assert!(matches!(
            &converted[2],
            (BlockPos { x: 5, y: 30, z: 6 }, BlockEntity::Spawner(state))
                if state == &crate::mob_spawner::SpawnerState::generated(
                    "minecraft:zombie".parse().expect("valid entity id")
                )
        ));
    }

    #[test]
    fn overworld_horizon_sample_is_deterministic() {
        let source = crate::overworld_chunk_source(42);
        let first = source
            .horizon_sample(-16_384, 16_384)
            .expect("the overworld source exposes a horizon estimate");
        assert_eq!(
            source.horizon_sample(-16_384, 16_384),
            Some(first),
            "the query must remain deterministic at one coordinate"
        );
    }
}
