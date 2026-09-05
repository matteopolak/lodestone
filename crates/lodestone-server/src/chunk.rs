//! Terrain source for the integrated server.
//!
//! A [`ChunkSource`] answers "what blocks are in column `(cx, cz)`?".
//!
//! Two implementations ship, and the distinction matters:
//!
//! * [`OverworldChunkSource`] is the **real** pipeline. It wraps
//!   [`lodestone_worldgen::overworld::OverworldGenerator`] — the composed,
//!   JVM-verified generator (interpolated `final_density` shape + sea-level
//!   aquifer + surface rules) — so its columns carry actual vanilla block-state
//!   strings (grass, dirt, stone, gravel, water, …), not a solid/air mask. This
//!   is the source a real client should be served, and the one the shell renders.
//! * [`WorldgenChunkSource`] is a **solidity-only** source kept for the
//!   transport/seam tests. It point-samples a bare [`Density`] node per block and
//!   maps `> 0` to stone — no cell interpolation, no surface, no fluid. It exists
//!   because the in-memory-transport tests only need *a* deterministic terrain to
//!   prove the wire round-trip, not a vanilla-accurate one. Do not reach for it
//!   as "the generator"; that is what [`OverworldChunkSource`] is.
//!
//! # The column carries block states, not just solidity
//!
//! [`ChunkColumn`] stores a per-column palette of block-state strings plus a
//! dense index grid (the same representation [`GeneratedColumn`] uses), so a
//! `ServerProtocol::encode_chunk` can emit a real chunk. The historical
//! solid/air API ([`ChunkColumn::set_solid`]/[`ChunkColumn::is_solid`]) is
//! preserved as a view over that field: a block is "solid" when it is neither air
//! nor a fluid, and `set_solid(true)` writes canonical stone.
//!
//! # Edits need somewhere to live
//!
//! [`ChunkSource::set_block`] mutates a block in place and [`ChunkSource::column`]
//! must go on reflecting that mutation afterward. [`OverworldChunkSource`]
//! retains edited columns, while untouched columns remain generator-backed;
//! see its own doc comment for the retention boundary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_model::BlockPos;
use lodestone_worldgen::density::{Context, Density};
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};

use crate::block_entities::BlockEntity;
use crate::chunk_blocks::SectionedBlocks;

/// Counts calls to [`ChunkColumn::intern`] separately for each test thread.
/// A counter records operation count rather than wall-clock time, so scheduling
/// variance cannot change the measurement. Thread-local storage isolates each
/// test's reset/read pair from calls made by other test threads.
#[cfg(test)]
thread_local! {
    static INTERN_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const STONE: &str = "minecraft:stone";

/// A shared, lazily-built `Arc<str>` for [`AIR`] — the out-of-column /
/// out-of-height answer every redstone lookup gives, and the one case with no
/// [`ChunkColumn`] palette to clone an entry out of. Cloning this bumps a
/// refcount; it never allocates after the first call on a process. See
/// [`ChunkColumn::block_state_arc`] and the `make_lookup` allocation
/// removal.
pub(crate) fn air_state_arc() -> std::sync::Arc<str> {
    static AIR_ARC: std::sync::LazyLock<std::sync::Arc<str>> = std::sync::LazyLock::new(|| std::sync::Arc::from(AIR));
    AIR_ARC.clone()
}
/// Rows per implicit section. [`ChunkColumn`] has no per-section struct — a
/// "section" here is a 16-row window of the one flat grid, counted from
/// `min_y` — so this is the only place the window height is written down.
pub(crate) const SECTION_ROWS: usize = 16;
/// Fallback biome for a [`ChunkColumn`] built with no generator behind it
/// ([`ChunkColumn::new`]'s blank column, and [`WorldgenChunkSource`], which
/// only ever models solidity — see that type's own doc comment). A column
/// adopted from the real generator via [`ChunkColumn::from_generated`] always
/// overwrites this with real per-quart biome data.
pub(crate) const DEFAULT_BIOME: &str = "minecraft:plains";

/// Vertical quart layers in a column of `height` block rows — the one place the
/// 3-D biome grid's Y extent is written down. Matches
/// [`lodestone_worldgen::overworld::BiomeCells`]'s own arithmetic exactly, which
/// is what lets [`ChunkColumn::from_generated`] adopt its indices verbatim.
fn y_quarts_for(height: i32) -> usize {
    (height as usize).div_ceil(4).max(1)
}

/// Returns `true` for blocks that do not count as collidable terrain: air
/// variants and fluids. `is_solid` is the negation of this over the block name.
///
/// Also doubles as this crate's "can a placement replace this cell" test
/// (`crate::server`'s `UseItemOn` handling) — the full game rule covers a
/// wider set (tall grass, snow layers, …), but the generator this
/// crate serves produces none of that vegetation yet (`worldgen_data`'s own
/// "no caves/ores/trees" scope note), so air-or-fluid is the whole set that
/// can actually appear here.
pub(crate) fn is_air_or_fluid(name: &str) -> bool {
    let base = name.split('[').next().unwrap_or(name);
    matches!(
        base,
        "minecraft:air"
            | "minecraft:cave_air"
            | "minecraft:void_air"
            | "minecraft:water"
            | "minecraft:lava"
    )
}

/// Returns `true` for water (any level state), the block `PlayerVitals`'
/// submersion test cares about (`LivingEntity.baseTick`'s
/// `this.isEyeInFluid(FluidTags.WATER)` — see `crate::vitals`'s module doc
/// comment for the full jar excerpt). Deliberately narrower than
/// [`is_air_or_fluid`]: lava does not drown a player (it burns, a mechanic
/// this crate does not model), so a drowning check must not treat the two
/// fluids as interchangeable the way "can this cell be replaced" does.
pub(crate) fn is_water(name: &str) -> bool {
    name.split('[').next().unwrap_or(name) == "minecraft:water"
}

/// One palette entry's canonical state string → its validated 26.2 state id,
/// with air for a block name the generated table does not carry.
///
/// The **single** definition of that fallback on this side of the seam, and
/// deliberately the same function `lodestone-v26-2`'s `resolve_state_id` is now a
/// one-line wrapper around — the reason a palette resolved here and a state
/// string resolved at the encoder cannot disagree about what a bare block name
/// means. Two test helpers had hand-duplicated an *older* version of that
/// fallback ("the lowest id sharing the name") and became silent callers when it
/// changed; one of them failed as a 30-second live timeout rather than a
/// mismatch. Do not copy this logic — call it.
pub(crate) fn resolve_palette_state_id(state: &str) -> lodestone_data::block_states::StateId {
    lodestone_data::block_states::StateId::from_state_str(state)
        .unwrap_or_else(lodestone_data::block_states::air_state)
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
/// Blocks are stored as indices into a small per-column `palette` of block-state
/// strings, with `palette[0] == "minecraft:air"`. The index layout matches
/// [`GeneratedColumn`] exactly (`blocks[(ly * 16 + z) * 16 + x]`, `ly = y -
/// min_y`) so [`ChunkColumn::from_generated`] is a zero-copy adoption.
#[derive(Debug, Clone)]
pub struct ChunkColumn {
    /// World Y of the lowest block row.
    pub min_y: i32,
    /// Number of block rows (world height).
    pub height: i32,
    /// The highest generation tier incorporated in this value.
    generation_stage: ChunkGenerationStage,
    /// Block-state palette; `palette[0]` is always `"minecraft:air"`.
    palette: Vec<String>,
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
    /// `palette_ticking[id] == crate::random_tick::is_randomly_ticking(&palette[id])`,
    /// computed once per palette entry as that entry is appended.
    ///
    /// Sound because the palette is **append-only**: [`ChunkColumn::intern`]
    /// pushes and nothing in this crate ever removes, remaps or compacts an
    /// entry (`palette` is private, so that is compiler-enforced rather than
    /// conventional). Palette entries are *full* state strings including
    /// `[...]` properties, and the predicate is a pure function of that string
    /// — property-sensitive families like `leaves_should_decay` included — so a
    /// per-entry classification is exact, not an approximation.
    palette_ticking: Vec<bool>,
    /// `palette_state_ids[id]` is `palette[id]`'s **validated 26.2 block-state
    /// id** (`lodestone_data::block_states::StateId::from_state_str`, or air
    /// for a name the table does not carry), computed once per palette entry as
    /// that entry is appended —
    /// sound for exactly the reason [`palette_ticking`](Self::palette_ticking)
    /// is, and maintained in the same two places.
    ///
    /// **This is the string→id boundary, and it exists so the protocol encoder
    /// never crosses it per block.** `V770ServerProtocol::encode_chunk` calls
    /// [`block_state`](Self::block_state) once per palette entry rather than
    /// probing
    /// each `&str` through a per-column `HashMap<&str, u32>` (SipHash), with each
    /// distinct entry then resolved by a 32,366-row scan doing a string compare
    /// per row — order 10⁶ string comparisons per served column, outside every
    /// worldgen instrument because generation cost excludes protocol encode by
    /// definition. Resolving the *palette* instead makes that a handful of
    /// lookups per column and turns the per-cell work into one array index. See
    /// `docs/chunk-column-encoding.md` and `DESIGN.md` §12.131.
    ///
    /// 26.2 is the one canonical internal version, and `lodestone-data` is
    /// deliberately outside the protocol-family feature seam, so holding a
    /// validated id here is not a version-seam crossing: no `lodestone-v26-2`
    /// dependency is implied, and `cargo check -p lodestone-shell
    /// --no-default-features` still passes.
    palette_state_ids: Vec<lodestone_data::block_states::StateId>,
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
    /// `palette_arc[id]` is an `Arc<str>` holding the same bytes as
    /// `palette[id]`, computed once per palette entry as that entry is
    /// appended — the fourth per-palette-entry derived table, sound for
    /// exactly the reason [`palette_ticking`](Self::palette_ticking) is, and
    /// maintained in the same two places.
    ///
    /// **This is what makes a redstone lookup closure cheap to call
    /// repeatedly.** Every `redstone::*`/`redstone_wire::*`/… signal query
    /// takes a `Fn(BlockPos) -> Arc<str>` "world" closure
    /// ([`crate::redstone::make_lookup`]); each call can use the cached string
    /// without allocating, on a path measured at 5899 reads in one active tick.
    /// Cloning an `Arc<str>` is one atomic
    /// increment, not a copy.
    palette_arc: Vec<std::sync::Arc<str>>,
    /// How many cells in each implicit 16-row window hold a randomly-ticking
    /// state — vanilla's own per-section ticking-block counter,
    /// one entry per section, `len =
    /// height.div_ceil(16)`.
    ///
    /// `u16` for the same reason vanilla uses `short`: a section holds at most
    /// 4096 cells. Maintained incrementally by [`ChunkColumn::set_block`] and
    /// recomputed wholesale by [`ChunkColumn::recalc_ticking_counts`] for the
    /// one constructor that adopts an already-populated grid.
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
    /// `min_y >> 2` — the same major-to-minor order as
    /// [`lodestone_worldgen::overworld::BiomeCells`], vanilla's own biome
    /// container order, and `blocks` above.
    ///
    /// `len == biome_y_quarts() * 16`. Broadcasting [`biome_quarts`] vertically
    /// instead of carrying this is what made `lush_caves`/`dripstone_caves`/
    /// `deep_dark` unreachable, and what erased them from every re-saved
    /// vanilla world.
    biome_cells: Vec<u16>,
    /// Block entities living in this column, at **absolute**
    /// positions.
    ///
    /// Populated by [`from_generated`](Self::from_generated) (a generated bee
    /// nest and its occupants) and by `crate::region_source`'s load path (so a
    /// chest read off disk reaches the client, not only the tick loop's
    /// registry). This is the list a `ServerProtocol::encode_chunk` writes into
    /// the chunk packet's block-entity array; the *save* path takes its own
    /// list from the live [`crate::block_entities::BlockEntityRegistry`]
    /// instead, because that one is newer.
    block_entities: Vec<(BlockPos, BlockEntity)>,
    /// Structure starts whose **origin** is this column, and this column's
    /// `structures.References`.
    ///
    /// Empty unless [`OverworldChunkSource::column`] filled them — they are not
    /// part of [`GeneratedColumn`] because they are answered per *chunk
    /// coordinate*, which a column does not carry, so the seam is the chunk
    /// source rather than `from_generated`. `crate::chunk_nbt` is the only
    /// consumer: they go in a region file's `structures` compound and nowhere on
    /// the wire (there is no clientbound structure packet).
    structure_starts: Vec<std::sync::Arc<lodestone_worldgen::structure::StructureStart>>,
    /// Structure id → packed origin-chunk keys. See
    /// [`structure_starts`](Self::structure_starts).
    structure_references: std::collections::BTreeMap<String, Vec<i64>>,
    /// The generator's `MOTION_BLOCKING` heightmap in its
    /// **stored** form (`topY + 1`, `0` for an all-air column), indexed
    /// `lx + lz * 16` — see
    /// [`lodestone_worldgen::overworld::GeneratedColumn::motion_blocking_heightmap`].
    ///
    /// `None` for a column that did not come from the real generator
    /// (a constructor or region-file load); `encode_chunk` then sends the zero-entry
    /// heightmap NBT it has always sent, which is well-framed and simply carries
    /// no map. It rides an accessor rather than `GeneratedColumn::into_raw`,
    /// whose own doc forbids widening that tuple — the same reason
    /// [`biome_cells`](Self::biome_cells) and
    /// [`block_entities`](Self::block_entities) are copied across.
    ///
    /// **Not maintained by [`set_block`](Self::set_block).** It is the
    /// generator's snapshot, so a player edit does not move it; `chunk_nbt`
    /// deliberately omits heightmaps from the Anvil write; loading recomputes
    /// derived height data, so nothing
    /// persists a stale value either. Only the first send after generation
    /// carries it, which is exactly the send a client has no other way to
    /// derive one for.
    motion_blocking: Option<Box<[u16; 256]>>,
    /// The generation stage's proposed creature placements —
    /// see [`lodestone_worldgen::spawn_stage`]'s module doc for what a
    /// candidate is (unconditioned on light/ground) and is not.
    ///
    /// Populated **only** by [`from_generated`](Self::from_generated), which
    /// only ever runs on a genuine disk-miss (`crate::region_source`'s
    /// `RegionChunkSource::column` calls the generator only when a saved
    /// region has no chunk yet) — so this is non-empty at most once in a
    /// chunk's whole lifetime, preserving one-shot-at-generation semantics. A
    /// column loaded from disk, or
    /// [`ChunkColumn::new`]'s placeholder, always starts with this empty.
    /// [`take_generation_spawns`](Self::take_generation_spawns) drains it, so
    /// even a cached `ChunkColumn` revisited later cannot hand out the same
    /// candidates twice. See `docs/worldgen-mob-generation-spawn.md`.
    generation_spawns: Vec<lodestone_worldgen::spawn_stage::GenerationSpawn>,
}

impl ChunkColumn {
    /// Creates an all-air column of the given vertical extent, biome fixed
    /// to [`DEFAULT_BIOME`] everywhere (no generator behind this column to
    /// ask — see that constant's doc comment).
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        Self {
            min_y,
            height,
            generation_stage: ChunkGenerationStage::Full,
            palette: vec![AIR.to_string()],
            blocks: SectionedBlocks::new_air(height),
            // All-air, so every section count is zero and every palette entry
            // is classified — correct by construction with no counting pass,
            // exactly like vanilla's own empty-section constructor,
            // which likewise does not run the block-count recalculation. The one classification is still routed
            // through the predicate rather than hardcoded `false`, so the
            // table cannot drift from the definition.
            palette_ticking: vec![crate::random_tick::is_randomly_ticking(AIR)],
            // Routed through the resolver rather than written as `0` for the
            // same reason the line above routes through the predicate: a
            // regenerated table that renumbered air must not silently desync
            // this from the real registry id.
            palette_state_ids: vec![resolve_palette_state_id(AIR)],
            // Third derived table, same append-time contract: air
            // reacts to nothing, but it is classified rather than
            // assumed so the one place a class is decided stays
            // `redstone_graph::classify`.
            palette_reaction: vec![crate::redstone_graph::classify(AIR)],
            // Fourth derived table, same append-time contract.
            palette_arc: vec![std::sync::Arc::from(AIR)],
            section_ticking: vec![0u16; (height as usize).div_ceil(SECTION_ROWS)],
            biome_quarts: std::array::from_fn(|_| DEFAULT_BIOME.to_string()),
            biome_palette: vec![DEFAULT_BIOME.to_string()],
            biome_cells: vec![0u16; y_quarts_for(height) * 16],
            block_entities: Vec::new(),
            structure_starts: Vec::new(),
            structure_references: std::collections::BTreeMap::new(),
            motion_blocking: None,
            generation_spawns: Vec::new(),
        }
    }

    /// Adopts a [`GeneratedColumn`] from the real worldgen pipeline: the palette
    /// moves as-is, and the flat block grid is *packed* into
    /// [`SectionedBlocks`] — one pass over the cells the caller has just written,
    /// which is also the pass that discards the ~160 KiB of it that is air (see
    /// `crate::chunk_blocks`). Real per-quart biome data comes across too.
    ///
    /// Packing uses one sequential pass over the cells. The dense representation
    /// costs 192 KiB per column; `chunk_store`'s
    /// 909 ms-per-column generation figure is the scale this pass is measured
    /// against.
    ///
    /// The 3-D biome grid and the block-entity list
    /// are *copied* rather than moved, because `GeneratedColumn::into_raw`
    /// deliberately does not carry them — see that method's doc comment. Both
    /// are small: a column's biome grid is `height / 4 * 16` `u16`s over a
    /// handful of palette entries (~3 KB), and nearly every column has zero
    /// block entities.
    #[must_use]
    pub fn from_generated(column: GeneratedColumn) -> Self {
        let generation_stage = match column.stage() {
            lodestone_worldgen::overworld::GenStage::Shaped => ChunkGenerationStage::Shaped,
            lodestone_worldgen::overworld::GenStage::Full => ChunkGenerationStage::Full,
        };
        let cells = column.biome_cells();
        let biome_palette = cells.palette().to_vec();
        let y_quarts = cells.y_quarts();
        let mut biome_cells = Vec::with_capacity(y_quarts * 16);
        for qy in 0..y_quarts {
            for qz in 0..4usize {
                for qx in 0..4usize {
                    biome_cells.push(cells.index_at_quart(qx, qy, qz));
                }
            }
        }
        let block_entities = column
            .block_entities()
            .iter()
            .map(crate::chunk_nbt::generated_block_entity)
            .collect();
        // Motion-blocking data is copied before `into_raw` consumes the column, for the same
        // reason the two above are.
        let motion_blocking = column.motion_blocking_heightmap().map(|map| Box::new(*map));
        // Generation-spawn candidates are copied before `into_raw` consumes the column, for
        // the same reason as the two above — see this struct's own field doc for
        // why "populated only here" is what makes generation-time spawning
        // one-shot rather than a duplication hazard.
        let generation_spawns = column.spawn_candidates().to_vec();

        let (min_y, height, palette, blocks, biome_quarts) = column.into_raw();
        debug_assert_eq!(
            palette.first().map(String::as_str),
            Some(AIR),
            "generated palette must start with air"
        );
        let blocks = SectionedBlocks::from_flat(height, &blocks);
        let mut column = Self {
            min_y,
            height,
            generation_stage,
            palette,
            blocks,
            // Placeholders: this constructor *adopts* an already-populated
            // grid, so the counters cannot be right by construction the way
            // `new`'s all-air ones are. `recalc_ticking_counts` below is the
            // one counting pass in the crate — vanilla's own block-count
            // recalculation, called from exactly the analogous
            // constructor.
            palette_ticking: Vec::new(),
            palette_state_ids: Vec::new(),
            palette_reaction: Vec::new(),
            palette_arc: Vec::new(),
            section_ticking: Vec::new(),
            biome_quarts,
            biome_palette,
            biome_cells,
            block_entities,
            structure_starts: Vec::new(),
            structure_references: std::collections::BTreeMap::new(),
            motion_blocking,
            generation_spawns,
        };
        column.recalc_ticking_counts();
        debug_assert_eq!(
            column.biome_cells.len(),
            column.biome_y_quarts() * 16,
            "generated biome grid must span the column's own height"
        );
        column
    }

    /// The highest generation tier this column contains.
    #[must_use]
    pub fn generation_stage(&self) -> ChunkGenerationStage {
        self.generation_stage
    }

    /// Adopts a [`lodestone_worldgen::nether::NetherColumn`], padded up to
    /// `window_height` rows of air.
    ///
    /// # Why there is a separate constructor, and why it pads
    ///
    /// [`from_generated`](Self::from_generated) takes the *overworld* generator's
    /// `GeneratedColumn`, which carries four products the Nether generator
    /// deliberately does not produce (a 4×4×4 biome grid, decoration block
    /// entities, a `MOTION_BLOCKING` heightmap, stage timings) — its own doc says
    /// a caller that wants to serve a Nether column "converts explicitly". This
    /// is that conversion.
    ///
    /// **The padding is the load-bearing part.** `NetherGenerator` produces 128
    /// rows (`noise_settings/nether.json`'s `noise.height`), while the Nether
    /// *dimension type* is `min_y 0, height 256, logical_height 128`
    /// (`DimensionTypes`' `BuiltinDimensionTypes.NETHER` registration). The wire
    /// frames a chunk against the **dimension**, not against whatever the
    /// generator felt like producing: a client that resolved `the_nether`'s
    /// registry entry reads exactly 16 sections, so serving an 8-section column
    /// is a decode failure, not a short world. The rows above 128 are genuinely
    /// air in vanilla — 127 is the bedrock roof and `logical_height` is what stops
    /// anything being built above it — so the padding is the truth rather than a
    /// stand-in.
    ///
    /// Biomes come across at the generator's own resolution: this dimension's
    /// climate is y-invariant (see `lodestone_worldgen::nether`'s module doc), so
    /// broadcasting the 16 horizontal quarts vertically is exact here, and is
    /// **not** a substitute for the dimension's full section window.
    #[must_use]
    pub fn from_nether(
        column: lodestone_worldgen::nether::NetherColumn,
        window_height: i32,
    ) -> Self {
        let (min_y, generated_height, palette, blocks, biome_quarts) = column.into_raw();
        Self::from_raw_window(
            min_y,
            generated_height,
            window_height,
            palette,
            &blocks,
            biome_quarts,
        )
    }

    /// The shared body behind [`from_nether`](Self::from_nether): a flat
    /// `blocks[(ly * 16 + z) * 16 + x]` grid `generated_height` rows tall, adopted
    /// into a `window_height`-tall column whose remaining rows are air.
    ///
    /// The palette is **re-based so air is index 0**, which every other
    /// constructor here gets for free from the overworld generator's own
    /// convention. A generator whose palette happens not to start with air (or
    /// which produced no air at all, in a fully solid column) would otherwise
    /// make index 0 mean netherrack — and since the padding rows are written as
    /// index 0, the sky above the Nether roof would come out solid.
    fn from_raw_window(
        min_y: i32,
        generated_height: i32,
        window_height: i32,
        palette: Vec<String>,
        blocks: &[u16],
        biome_quarts: [String; 16],
    ) -> Self {
        assert!(window_height >= generated_height, "window cannot truncate the generated column");
        let mut column = Self::new(min_y, window_height);
        // `Self::new` seeded the palette with air at index 0; intern the rest in
        // the generator's own order so a remap is a single lookup table.
        let remap: Vec<u16> = palette
            .iter()
            .map(|state| column.intern(state))
            .collect();
        let rows = generated_height.max(0) as usize;
        for ly in 0..rows {
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let index = (ly * 16 + lz) * 16 + lx;
                    let Some(&raw) = blocks.get(index) else { continue };
                    let id = remap.get(raw as usize).copied().unwrap_or(0);
                    if id == 0 {
                        continue;
                    }
                    column.blocks.set(lx as i32, ly as i32, lz as i32, id);
                }
            }
        }
        column.biome_quarts = biome_quarts;
        // Broadcast the horizontal quarts through the whole window — exact for
        // this dimension, see `from_nether`'s doc.
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
        column.recalc_ticking_counts();
        column
    }

    /// Adopts a [`lodestone_worldgen::end::EndColumn`], padded up to
    /// `window_height` rows of air — the End's counterpart to
    /// [`from_nether`](Self::from_nether), for exactly the same reason: the End's
    /// generator produces `noise_settings/end.json`'s `noise.height` (128) rows,
    /// while `the_end`'s *dimension type* is `min_y 0, height 256, logical_height
    /// 256` (`data/minecraft/dimension_type/the_end.json`). A client that resolved
    /// `the_end`'s registry entry reads 16 sections; serving an 8-section column
    /// is the same decode failure `from_nether`'s doc describes.
    ///
    /// Biomes broadcast across the padded window exactly as `from_nether`'s do,
    /// for the same reason: the End's biome layout is y-invariant (see
    /// `lodestone_worldgen::end`'s module doc — the erosion channel is
    /// `cache_2d`), so this is exact rather than an approximation.
    #[must_use]
    pub fn from_end(column: lodestone_worldgen::end::EndColumn, window_height: i32) -> Self {
        let (min_y, generated_height, palette, blocks, biome_quarts) = column.into_raw();
        Self::from_raw_window(
            min_y,
            generated_height,
            window_height,
            palette,
            &blocks,
            biome_quarts.map(str::to_string),
        )
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
    /// of the column. Every coordinate is clamped into range, matching
    /// [`lodestone_worldgen::overworld::BiomeCells::index_at_quart`].
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

    /// Takes this column's pending `SPAWN`-stage creature candidates, leaving
    /// it empty after the one generation-time consumer reads it.
    ///
    /// **Drain, not peek**, on purpose: a caller that observes a non-empty
    /// result is the one and only consumer for this column's whole lifetime —
    /// see [`generation_spawns`](Self::generation_spawns)'s field doc for why
    /// that is what keeps a fresh world's animals from duplicating. Empty for
    /// every column not fresh off [`from_generated`](Self::from_generated).
    pub fn take_generation_spawns(&mut self) -> Vec<lodestone_worldgen::spawn_stage::GenerationSpawn> {
        std::mem::take(&mut self.generation_spawns)
    }

    /// Whether this freshly generated column still owns one-shot spawn candidates.
    ///
    /// A persistence adapter must not serialize the block grid and quietly drop
    /// these candidates: doing so changes the first-load population decision.
    /// The native record adapter uses this read-only check to decline such a
    /// column until its schema has a representation for the candidates.
    #[must_use]
    pub(crate) fn has_pending_generation_spawns(&self) -> bool {
        !self.generation_spawns.is_empty()
    }

    /// This column's `MOTION_BLOCKING` heightmap in vanilla's stored form, or
    /// `None` if it did not come from the generator — see
    /// [`motion_blocking`](Self::motion_blocking) for the whole contract and
    /// `docs/motion-blocking-heightmap.md` for the `+1`.
    #[must_use]
    pub fn motion_blocking(&self) -> Option<&[u16; 256]> {
        self.motion_blocking.as_deref()
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
    /// Called by [`OverworldChunkSource::column`], which is the only place that
    /// holds both the column and the `(cx, cz)` the generator needs. Purely
    /// additive: nothing derives a block from this, so a source that does not
    /// call it serves a column whose `structures` compound is empty — which is
    /// what every source other than the real overworld generator does.
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
    /// arithmetic [`recalc_ticking_counts`](Self::recalc_ticking_counts) and
    /// `crate::random_tick`'s section walk use — change one and all three must
    /// change together.
    #[inline]
    fn section_index(y_local: i32) -> usize {
        y_local as usize / SECTION_ROWS
    }

    /// Recomputes both derived ticking tables from scratch — vanilla's own
    /// block-count recalculation,
    /// kept as a named production function for the same reason vanilla keeps
    /// it: exactly one constructor needs it (the one that *adopts* an
    /// already-populated grid), and naming it says so.
    ///
    /// Cost as a count rather than a duration, per this repo's evidence rule:
    /// exactly `palette.len()` predicate evaluations plus one read of every
    /// cell in `blocks` (98,304 for a full overworld column). The caller has
    /// just moved those same cells, so the pass adds less than one extra read
    /// of data already in cache, once per column construction — against the
    /// per-tick, per-column scan it removes.
    fn recalc_ticking_counts(&mut self) {
        self.palette_ticking = self
            .palette
            .iter()
            .map(|state| crate::random_tick::is_randomly_ticking(state))
            .collect();
        // The other per-palette-entry derived table, rebuilt here for the same
        // reason and by the same argument — this is the one constructor that
        // adopts an already-populated palette, so it is the one place the
        // append-time computation in `intern` cannot have run.
        self.palette_state_ids = self
            .palette
            .iter()
            .map(|state| resolve_palette_state_id(state))
            .collect();
        // And the third, for the same reason: this constructor adopts a
        // palette `intern` never saw, so the append-time classification in
        // `intern` cannot have run for any of its entries.
        self.palette_reaction = self
            .palette
            .iter()
            .map(|state| crate::redstone_graph::classify(state))
            .collect();
        // And the fourth, for the same reason: this constructor adopts a
        // palette `intern` never saw, so the append-time `Arc` build in
        // `intern` cannot have run for any of its entries.
        self.palette_arc = self.palette.iter().map(|state| std::sync::Arc::from(state.as_str())).collect();
        let sections = (self.height as usize).div_ceil(SECTION_ROWS);
        let mut counts = vec![0u16; sections];
        for s in 0..sections {
            let mut count = 0u16;
            // Per section rather than over one flat grid, because the sections
            // *are* the storage — and a uniform (usually all-air) section
            // reads without touching any cell memory at all.
            self.blocks.for_each_in_section(s, |_, id| {
                if self.palette_ticking[id as usize] {
                    count += 1;
                }
            });
            counts[s] = count;
        }
        self.section_ticking = counts;
    }

    /// Interns a block-state string into the palette, returning its index.
    fn intern(&mut self, name: &str) -> u16 {
        #[cfg(test)]
        INTERN_CALLS.with(|c| c.set(c.get() + 1));
        if let Some(i) = self.palette.iter().position(|p| p == name) {
            return i as u16;
        }
        self.palette.push(name.to_string());
        // Classify each palette entry exactly once, as it is appended — the
        // only place a new classification is ever needed, because the palette
        // is append-only. This is what lets `set_block` decide a counter delta
        // with **no** string predicate evaluation at all.
        self.palette_ticking
            .push(crate::random_tick::is_randomly_ticking(name));
        // Same argument, same place: resolve the string→id map once per entry so
        // the protocol encoder can index it 98,304 times without ever seeing a
        // string. See `palette_state_ids`.
        self.palette_state_ids.push(resolve_palette_state_id(name));
        // And the third: classify which redstone family (if any) a neighbour
        // notification landing on this state dispatches to, so the dispatch
        // itself never evaluates a string predicate. See
        // `crate::redstone_graph`.
        self.palette_reaction.push(crate::redstone_graph::classify(name));
        // And the fourth: build the `Arc<str>` once per entry so a redstone
        // lookup closure can hand one out on every call for the cost of an
        // atomic increment. See `palette_arc`.
        self.palette_arc.push(std::sync::Arc::from(name));
        debug_assert_eq!(
            self.palette.len(),
            self.palette_ticking.len(),
            "palette and its ticking classification must stay the same length"
        );
        debug_assert_eq!(
            self.palette.len(),
            self.palette_state_ids.len(),
            "palette and its resolved state ids must stay the same length"
        );
        debug_assert_eq!(
            self.palette.len(),
            self.palette_reaction.len(),
            "palette and its reaction classification must stay the same length"
        );
        debug_assert_eq!(
            self.palette.len(),
            self.palette_arc.len(),
            "palette and its Arc<str> mirror must stay the same length"
        );
        (self.palette.len() - 1) as u16
    }

    /// Sets the block state at a local `(x, z)` in `0..16` and world `y`.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let id = self.intern(name);
        self.write_block_id(x, y, z, id);
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

    /// [`set_block`](Self::set_block) minus the string→id resolution: writes an
    /// already-interned column-wide palette `id` at a local `(x, z)` in `0..16`
    /// and world `y`.
    ///
    /// Exists so a caller resolving many cells against a *known* palette — a
    /// section's worth, in [`set_section_from_local_palette`](Self::set_section_from_local_palette)
    /// — pays [`intern`](Self::intern)'s linear scan once per distinct state
    /// rather than once per cell.
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
                    "section_ticking[{section}] underflowed writing {} at ({x}, {y}, {z}): a \
                     randomly-ticking state left a cell the counter did not know held one, so \
                     some mutation path reached `blocks` without `set_block` or \
                     `recalc_ticking_counts`",
                    self.palette[id as usize]
                );
                self.section_ticking[section] -= 1;
            }
        }
    }

    /// Interns a whole section's *local* palette into the column-wide palette
    /// — `local.len()` calls to [`intern`](Self::intern), not one per cell —
    /// then writes every one of the section's cells from the resulting remap.
    ///
    /// The load-path mirror of [`raw_palette`](Self::raw_palette)/
    /// [`append_section_cells`](Self::append_section_cells):
    /// [`crate::chunk_nbt`]'s loader interns each section palette once, rather
    /// than scanning the whole column-wide palette for all 98,304 cells.
    ///
    /// `indices` is one entry per cell in vanilla's own `(y_in_section << 8) |
    /// (z << 4) | x` order (what `chunk_nbt::unpack_indices` already returns),
    /// indexing into `local` — **not** into the column-wide palette. Every
    /// entry must be `< local.len()`; callers validate that against the NBT
    /// before calling, so this indexes unchecked.
    pub fn set_section_from_local_palette(&mut self, y_base: i32, local: &[&str], indices: &[u16]) {
        let remap: Vec<u16> = local.iter().map(|name| self.intern(name)).collect();
        for (cell, &local_index) in indices.iter().enumerate() {
            let id = remap[local_index as usize];
            let ly = (cell >> 8) as i32;
            let lz = ((cell >> 4) & 15) as i32;
            let lx = (cell & 15) as i32;
            self.write_block_id(lx, y_base + ly, lz, id);
        }
    }

    /// Sets solidity at a local `(x, z)` in `0..16` and world `y`. `true` writes
    /// canonical stone, `false` writes air — the solid/air view preserved for
    /// callers that only reason about collidable terrain.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        self.set_block(x, y, z, if solid { STONE } else { AIR });
    }

    /// Canonical block-state string at a local `(x, z)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> &str {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return AIR;
        }
        &self.palette[self.blocks.get(x, y_local, z) as usize]
    }

    /// [`block_state`](Self::block_state), but cheap to call repeatedly on a
    /// hot read path: an `Arc<str>` clone (one atomic increment) rather than
    /// a fresh heap allocation and copy. Out-of-range Y clones the shared
    /// [`air_state_arc`] instead of allocating a new "minecraft:air".
    ///
    /// This is what [`crate::redstone::make_lookup`] and
    /// [`crate::random_tick::RedstoneColumns`]'s reads call — see
    /// [`palette_arc`](Self::palette_arc).
    #[must_use]
    pub fn block_state_arc(&self, x: i32, y: i32, z: i32) -> std::sync::Arc<str> {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return air_state_arc();
        }
        self.palette_arc[self.blocks.get(x, y_local, z) as usize].clone()
    }

    /// The **global 26.2 block-state id** at a local `(x, z)` in `0..16` and
    /// world `y` — the integer form of [`block_state`](Self::block_state), and
    /// what a protocol encoder should call. Out-of-range Y is air's id, exactly as
    /// `block_state` returns `"minecraft:air"`.
    ///
    /// Two array indexes and a range check: no string, no hash, no scan. The
    /// resolution happened once per palette entry — see
    /// [`palette_state_ids`](Self::palette_state_ids).
    #[must_use]
    pub fn block_state_id(&self, x: i32, y: i32, z: i32) -> u32 {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return lodestone_data::block_states::air_state().raw();
        }
        self.palette_state_ids[self.blocks.get(x, y_local, z) as usize].raw()
    }

    /// The validated global 26.2 state at a local coordinate.
    ///
    /// This is the in-process counterpart of [`block_state_id`](Self::block_state_id).
    /// The raw form remains for protocol encoders; callers that stay within the
    /// canonical registry should use this total lookup instead.
    #[must_use]
    pub fn resolved_block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> lodestone_data::block_states::StateId {
        let y_local = y - self.min_y;
        if !(0..self.height).contains(&y_local) {
            return lodestone_data::block_states::air_state();
        }
        self.palette_state_ids[self.blocks.get(x, y_local, z) as usize]
    }

    /// Which redstone family, if any, a neighbour notification landing at a
    /// local `(x, z)` in `0..16` and world `y` dispatches to. Out-of-range Y
    /// is air's class, exactly as [`block_state`](Self::block_state) returns
    /// `"minecraft:air"`.
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

    /// This column's palette resolved to global 26.2 block-state ids, parallel to
    /// [`raw_palette`](Self::raw_palette).
    ///
    /// Exists for an encoder that walks the index grid section-by-section
    /// (`append_section_cells`) rather than cell-by-cell, and as the observable a
    /// gate uses to check these ids against the jar-derived dump without
    /// re-deriving them.
    #[must_use]
    pub fn palette_state_ids(&self) -> &[lodestone_data::block_states::StateId] {
        &self.palette_state_ids
    }

    /// Returns solidity at a local `(x, z)` in `0..16` and world `y`. A block is
    /// solid when it is neither air nor a fluid; blocks outside the vertical
    /// range are non-solid.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        !is_air_or_fluid(self.block_state(x, y, z))
    }

    /// Total number of solid (non-air, non-fluid) blocks.
    #[must_use]
    pub fn solid_count(&self) -> usize {
        // Classify the palette once, then count integers — the same argument
        // `raw_palette` and `recalc_ticking_counts` already make. Previously this
        // ran the string predicate on all 98,304 cells.
        let solid: Vec<bool> = self
            .palette
            .iter()
            .map(|state| !is_air_or_fluid(state))
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

    /// The column-wide block-state palette, borrowed.
    ///
    /// Exists for [`crate::chunk_nbt`], which has to walk the palette and the
    /// index grid together to build vanilla's *per-section* palettes. Going
    /// through [`block_state`](Self::block_state) instead would mean 98,304
    /// string lookups and a fresh `String` per block for every column saved.
    #[must_use]
    pub fn raw_palette(&self) -> &[String] {
        &self.palette
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
    ) -> Vec<(BlockPos, &'static str)> {
        let types: Vec<Option<&'static str>> = self
            .palette_state_ids
            .iter()
            .map(|&id| {
                lodestone_data::block_entity_types::block_entity_type(id)
                    .map(lodestone_data::block_entity_types::block_entity_type_name)
            })
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
                let Some(name) = types[id as usize] else {
                    return;
                };
                let row_local = cell / ROW_CELLS;
                let rem = cell % ROW_CELLS;
                let local_z = (rem / 16) as i32;
                let local_x = (rem % 16) as i32;
                let y = self.min_y + (s * SECTION_ROWS + row_local) as i32;
                let pos = BlockPos::new(base_x + local_x, y, base_z + local_z);
                if !existing.iter().any(|(p, _)| *p == pos) {
                    out.push((pos, name));
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

    /// Appends section `s`'s palette indices to `out`, in vanilla's own
    /// `(y_in_section << 8) | (z << 4) | x` order — so `crate::chunk_nbt` builds a
    /// region file's per-section container straight from it.
    ///
    /// Same rationale as [`raw_palette`](Self::raw_palette): going through
    /// [`block_state`](Self::block_state) instead would mean 98,304 string lookups
    /// and a fresh `String` per block for every column saved.
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
    /// reading and without depending on machine load: the flat `Vec<u16>` this
    /// replaced was unconditionally `16 × 16 × height × 2`, and
    /// `tests/chunk_memory.rs` predicts both numbers from outside constants.
    #[must_use]
    pub fn blocks_heap_bytes(&self) -> usize {
        self.blocks.heap_bytes()
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

/// Supplies terrain columns to the integrated server.
pub trait ChunkSource: Send + Sync {
    /// Generates the column at chunk coordinates `(cx, cz)`.
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn;

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

    /// Reads a single block's canonical state string at world coordinates
    /// `(x, y, z)`, through the same data [`column`](Self::column) would
    /// return — including any edit already applied via
    /// [`set_block`](Self::set_block).
    ///
    /// This is a required method so no implementor silently inherits a
    /// whole-column regeneration for a one-block read. An implementor with a cheaper path, one that
    /// reads a cell out of a column it already retains, must override this
    /// to avoid regenerating on every probe: the `ChunkStore` wrapper is the
    /// reference example. An implementor with no cheaper path implements it
    /// as `self.column(cx, cz).block_state(..)`, which is correct if
    /// column-sized; the point is that the choice is explicit at every
    /// implementor rather than silently inherited.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String;

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

    /// Reads the biome id at world coordinates `(x, y, z)` — `/execute if
    /// biome`'s own read, through the same data
    /// [`column`](Self::column) would return.
    ///
    /// Required, not defaulted, for the same reason [`block_state`](Self::block_state)
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
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str);

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
    /// retained column should override it — unlike [`block_state`](Self::block_state),
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
    /// [`block_state`](Self::block_state) on a miss.
    ///
    /// This exists for [`crate::block_entities::BlockEntityRegistry::tick_all_with_hopper_lock`]
    /// because only a block entity whose *chunk* is loaded should tick. Calling
    /// `block_state` to answer that question would generate a whole column for
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

    /// Tells the source that the column at `(cx, cz)` is no longer resident in
    /// whatever cache sits above it, so a layer that retains state per column
    /// may release it.
    ///
    /// The default is a no-op, which is the correct behaviour for every source
    /// that owns no per-column state — and for [`OverworldChunkSource`], whose
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

    /// Atomically claims "the end-dragon fight for this source has now
    /// started", answering `true` only for the one call that actually flips
    /// it from unset — so two connections reaching a fresh End around the
    /// same tick cannot both spawn a second crystal ring and dragon.
    ///
    /// `true` unconditionally — the default — is the correct degradation for
    /// every source with no such flag of its own: this is only ever called
    /// against the End's own sibling source, immediately before deciding
    /// whether to call `MobSim::init_end_dragon_fight`, and answering "already
    /// claimed" for a source this can never meaningfully be asked of just
    /// means that (unreachable) call site does nothing, rather than assuming
    /// the trait's absent flag means "unclaimed" and re-initialising a fight
    /// every time. [`crate::chunk::EndChunkSource`] is the one real override.
    ///
    /// This is a **process-lifetime** gate, not a persisted one: this crate
    /// has no `EnderDragonFight`-equivalent world state yet (see
    /// `docs/dragon-fight.md`), so a server restart re-arms it. That is a
    /// disclosed, documented gap, not a silent one.
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
    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        (**self).resident_block_state_id(x, y, z)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        (**self).resident_column(cx, cz)
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        (**self).column(cx, cz)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        (**self).column_at(cx, cz, stage)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        (**self).block_state(x, y, z)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        (**self).biome_state_at(x, y, z)
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        (**self).set_block(x, y, z, name);
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        (**self).block_entity(x, y, z)
    }

    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        (**self).is_column_resident(cx, cz)
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
    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<lodestone_data::block_states::StateId> {
        (**self).resident_block_state_id(x, y, z)
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        (**self).resident_column(cx, cz)
    }

    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        (**self).column(cx, cz)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        (**self).column_at(cx, cz, stage)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        (**self).block_state(x, y, z)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        (**self).biome_state_at(x, y, z)
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        (**self).set_block(x, y, z, name);
    }

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<crate::block_entities::BlockEntity> {
        (**self).block_entity(x, y, z)
    }

    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        (**self).is_column_resident(cx, cz)
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
}

/// Generates every column in `coords` across scoped OS threads over `&source`,
/// returning them in the **same order as `coords`** regardless of which
/// thread finished which column first.
///
/// This is safe because `column()` is genuinely pure per chunk: every RNG a
/// generator touches is positionally seeded (`set_decoration_seed` /
/// `set_feature_seed` per source chunk, with `fork_positional`/`from_hash_of`)
/// and no shared RNG stream exists anywhere in
/// `lodestone-worldgen`, so results are order-independent by construction —
/// see `OverworldGenerator::column`'s own doc comment and
/// `examples/bench_worldgen.rs`, which already shares a generator across
/// `std::thread::scope` workers the same way. `ChunkSource: Send + Sync`
/// (this trait's own bound, above) is what makes `&S` shareable across the
/// scope in the first place.
///
/// Callers that care about the wire being independent of thread scheduling
/// (i.e. every caller) must still encode/send the returned columns in the
/// fixed order they came in — this function only parallelises the
/// generation, not the ordering guarantee, which is why it hands back a
/// `Vec` aligned index-for-index with `coords` rather than an unordered
/// collection.
#[must_use]
pub(crate) fn generate_columns_parallel<S: ChunkSource + ?Sized>(
    source: &S,
    coords: &[(i32, i32)],
) -> Vec<ChunkColumn> {
    map_columns_parallel(source, coords, |_, column| column)
}

/// [`generate_columns_parallel`] with a per-column transform applied **inside the
/// worker that generated it**, so whatever `f` costs is parallelised on the same
/// fan-out and the intermediate [`ChunkColumn`] is dropped without ever reaching
/// the caller.
///
/// This exists because folding the protocol encode into the same blocking closure
/// as the generation (`generate_and_encode_columns_offloaded`) and the encode
/// both run inside the blocking fan-out. Each worker calls `encode_chunk` for
/// its generated column, avoiding a serial pass. At the ≈2.4 ms per column
/// `crate::protocol::ChunkEncoder` carries, a 33-column strip therefore paid ≈80 ms
/// of *unavoidably* single-threaded work no matter how many cores generated it —
/// which is the whole cost the offload was supposed to remove, still present, just
/// relocated off the connection task.
///
/// Two consequences beyond the wall clock, both properties of doing it here rather
/// than after the join:
///
/// * peak memory is one column per worker instead of `coords.len()` columns. A
///   composed column is not small, and the old shape held the entire strip live at
///   once purely to iterate it afterwards.
/// * `f` runs on the worker thread, so it must be `Sync` and its output `Send`.
///   `ChunkEncoder` already requires both (`Send + Sync + 'static`), which is why
///   no call site has to change shape.
///
/// Order is still **`coords` order**, not completion order — the same guarantee
/// [`generate_columns_parallel`] documents, and for the same reason: the wire byte
/// sequence must not depend on which thread finished first.
#[must_use]
fn map_columns_parallel<S, T, F>(source: &S, coords: &[(i32, i32)], f: F) -> Vec<T>
where
    S: ChunkSource + ?Sized,
    T: Send,
    F: Fn((i32, i32), ChunkColumn) -> T + Sync,
{
    if coords.len() <= 1 {
        return coords
            .iter()
            .map(|&(cx, cz)| f((cx, cz), source.column(cx, cz)))
            .collect();
    }

    let workers = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(4)
        .max(1);
    let batch = coords.len().div_ceil(workers).max(1);

    std::thread::scope(|scope| {
        let handles: Vec<_> = coords
            .chunks(batch)
            .map(|slice| {
                let f = &f;
                scope.spawn(move || {
                    slice
                        .iter()
                        .map(|&(cx, cz)| f((cx, cz), source.column(cx, cz)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worldgen worker thread panicked"))
            .collect()
    })
}

/// [`map_columns_parallel`]'s single-threaded, **yielding** twin — the wasm32
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

/// The production `yield_between` for [`generate_columns_yielding`]/
/// [`map_columns_yielding`] on wasm32: a real browser **macrotask**, not a
/// microtask.
///
/// `js_sys::Promise::new` resolved by `window.setTimeout(_, 0)` is load-bearing
/// in that choice. A microtask (e.g. `Promise::resolve().then(...)`) drains
/// entirely within the *current* JS task — the browser does not get a chance to
/// paint or service input between microtasks, only between tasks — so a future
/// built on one would satisfy "the Rust code has an `.await` point" while doing
/// nothing to stop the tab from hanging. `setTimeout` queues a genuine new
/// macrotask, which is the granularity Chrome's own unresponsive-page detector
/// (and rendering) actually yields at.
#[cfg(target_arch = "wasm32")]
async fn yield_to_browser() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window()
            .expect("no global `window`: this crate's wasm32 build only runs inside a browser tab");
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0)
            .expect("window.setTimeout");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Generates columns off the async runtime's core thread.
/// This helper keeps the blocking generation work away from that thread.
///
/// # Why this exists when generation is already parallel
///
/// [`generate_columns_parallel`] improves *throughput*: the batch is
/// fanned out over scoped OS threads. It did nothing about *latency*, because
/// its final `std::thread::scope` join blocks the calling thread until every
/// worker finishes. Parallel is not the same as non-blocking, and the
/// distinction is total rather than academic here: the shell builds the
/// server's runtime with `tokio::runtime::Builder::new_current_thread()`
/// (`crates/lodestone-shell/src/net.rs`), so the connection task and
/// [`crate::tick::run_tick_loop`] share **one** thread. Blocking it blocks
/// *every* task in the process — the world tick included — so an inline
/// chunk-boundary generation can drop one or more 50 ms ticks.
///
/// # Why `spawn_blocking` and not `block_in_place`
///
/// [`tokio::task::block_in_place`] needs no signature change and is the
/// obvious-looking fix. It **panics** on a current-thread runtime —
/// `can call blocking only when running on the multi-threaded runtime` —
/// which is exactly the runtime production builds, so it would panic in
/// singleplayer rather than merely fail a test. Measured, on a
/// `new_current_thread` runtime:
///
/// | call | result |
/// |---|---|
/// | `block_in_place` | panics |
/// | `spawn_blocking` | `Ok` |
/// | 10 ms timer ticks during a 300 ms `spawn_blocking` | **25** |
/// | 10 ms timer ticks during a 300 ms inline block | **0** |
///
/// `spawn_blocking` is correct on a current-thread runtime because the
/// blocking pool is a separate set of threads from the core thread, and it
/// stays correct on a multi-thread runtime as well, so the behavior is
/// independent of the runtime's thread count.
///
/// # Why `Arc<S>` rather than `&S`
///
/// `spawn_blocking` requires a `'static` closure, so the source cannot be
/// borrowed across it. Callers thread the shared handle they already hold
/// (`crate::integrated` builds `Arc::new(source)` for exactly this reason);
/// `crate::server::SourceRef` is the wrapper that lets a borrow-shaped
/// caller keep the old blocking path without duplicating any of
/// `serve_connection`'s body.
///
/// # wasm32
///
/// `wasm32-unknown-unknown` has no blocking pool and does **not** call
/// `generate_columns_parallel` straight through: that function's
/// `coords.len() > 1` arm fans out
/// over `std::thread::scope`, and a `Scope::spawn` on this target reaches
/// `Builder::spawn`'s `Err` through an internal `.expect()` — measured,
/// executed in a wasm VM: `unreachable`, i.e. it TRAPS, and with this crate's
/// `panic = "abort"` release profile that is unrecoverable. `portal.rs`'s
/// `create_portal` gates its own `generate_columns_parallel` call off on wasm32
/// for the same constraint. This helper therefore
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
        tokio::task::spawn_blocking(move || generate_columns_parallel(&*source, &coords))
            .await
            .expect("worldgen blocking task panicked")
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
    // wasm32: `map_columns_yielding`, one column generated-and-encoded at a
    // time with a real browser yield between each — see
    // `generate_columns_offloaded`'s wasm32 doc for why this is not merely a
    // latency nicety on this target. `map_columns_parallel`'s
    // `coords.len() > 1` branch (native's own past shape here) TRAPS on
    // wasm32 via `std::thread::scope`, so this is also what keeps the browser
    // build from crashing on any batch bigger than one column.
    #[cfg(target_arch = "wasm32")]
    {
        Some(
            map_columns_yielding(
                &*source,
                &coords,
                |(cx, cz), column| encoder.try_encode_chunk(cx, cz, &column),
                yield_to_browser,
            )
            .await
            .into_iter()
            .collect(),
        )
    }
    // Native: `map_columns_parallel`, not `generate_columns_parallel`
    // followed by an encode loop — the encode runs on the worker that
    // generated the column, so a 33-column strip's ≈80 ms of encode is
    // fanned out rather than paid serially after the join. See that
    // function's own doc for the two properties this buys.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let encode = move || {
            map_columns_parallel(&*source, &coords, |(cx, cz), column| {
                encoder.try_encode_chunk(cx, cz, &column)
            })
        };
        Some(
            tokio::task::spawn_blocking(encode)
                .await
                .expect("worldgen blocking task panicked")
                .into_iter()
                .collect(),
        )
    }
}

/// The real terrain source: the composed, JVM-verified overworld generator.
///
/// This is what a client connecting to the integrated server should be served —
/// its columns carry real vanilla block states (shape + sea-level aquifer +
/// surface rules), the same output the shell renders directly. Build one per
/// world (via [`crate::overworld_chunk_source`]) and share it across the view.
///
/// # Retention: the design question a served, editable world raises
///
/// Before block-edit support, `column()` called straight through to
/// `self.generator.column(cx, cz)` on **every** request — nothing was ever
/// retained. That was fine for read-only terrain (the generator is
/// deterministic, so "regenerate on every request" and "cache forever" are
/// observationally identical), but it means there was nowhere for an edit to
/// live: a `set_block` with no cache behind it would be overwritten by the
/// next `column()` call the moment the edited chunk left a client's view and
/// came back (`ViewTracker::recenter`'s forget/resend cycle in
/// `crate::server`).
///
/// `edits` is that missing retention, added deliberately narrow: it is
/// populated **only** by [`set_block`](Self::set_block), not by every
/// `column()` read. An unedited column is still regenerated fresh on every
/// request exactly as before (unchanged cost, unchanged behaviour — see
/// `worldgen_data`'s `chunk_source_serves_generator_block_for_block` test,
/// which still passes unmodified because it never edits anything). Only a
/// column that has actually been touched by a player pays for a permanent
/// `ChunkColumn` in memory, for the life of this source. Caching *every*
/// generated column (edited or not) was the other option; it was rejected
/// because it would make memory cost scale with how much of the world a
/// session has merely looked at, not with how much it has changed — the
/// wrong invariant for a server that is otherwise happy to regenerate
/// deterministic terrain on demand.
pub struct OverworldChunkSource {
    generator: OverworldGenerator,
    /// Columns a `set_block` call has touched, keyed by chunk coordinates.
    /// Absent from this map means "not yet edited"; `column()` falls through
    /// to the generator in that case. See the struct doc comment above.
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
}

impl OverworldChunkSource {
    /// Wraps a pre-built [`OverworldGenerator`].
    #[must_use]
    pub fn new(generator: OverworldGenerator) -> Self {
        Self {
            generator,
            edits: Mutex::new(HashMap::new()),
        }
    }

    /// The lowest world `y` this source's columns contain.
    ///
    /// Exposed for [`crate::region_source::RegionChunkSource::new`]'s
    /// `min_y`/`height` arguments, which **must** match the world the columns
    /// came from — that module's own gotcha, because vanilla writes light-only
    /// sections past both ends and a mismatch silently mis-slices every saved
    /// column. Reading them off the generator makes the pair impossible to get
    /// wrong; hardcoding `(-64, 384)` at each call site is a guess that drifts
    /// the moment the overworld's shape changes. Free — no column is generated.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.generator.min_y()
    }

    /// How many `y` levels this source's columns contain. See [`Self::min_y`].
    #[must_use]
    pub fn height(&self) -> i32 {
        self.generator.height()
    }

    /// The wrapped generator, for the store observables a gate has to read.
    ///
    /// `store_len`/`store_evictions` live on [`OverworldGenerator`] and are what
    /// license reading Unit 6's stage counters as one-per-chunk ("nothing was
    /// evicted, so nothing was recomputed"). Without this accessor a gate
    /// measuring the join scheduler would have to drive a hand-rolled
    /// [`ChunkSource`] over a bare generator instead of the source production
    /// actually serves through — the `world` species of vacuous test
    /// (`DESIGN.md` §12.43), where the flaw is which implementation the test
    /// resolves to rather than anything in the test's source. Read-only, and no
    /// column is generated.
    #[must_use]
    pub fn generator(&self) -> &OverworldGenerator {
        &self.generator
    }

    /// Copies the generator's structure placement answer for `(cx, cz)` onto a
    /// freshly built column.
    ///
    /// Both calls are memoised store reads on the two stages that already ran
    /// above terrain (`structure_starts` / `structure_refs`), so this adds no
    /// generation work — it only moves an answer the generator already computed
    /// somewhere the NBT writer can see it. Without it the placement engine is an
    /// island: fully built, oracle-verified, and reaching zero chunks.
    fn attach_structures(&self, column: &mut ChunkColumn, cx: i32, cz: i32) {
        let starts = self.generator.structure_starts(cx, cz);
        let references = self.generator.structure_references(cx, cz);
        self.fill_structure_chests(column, cx, cz, &references);
        column.set_structures(starts, references);
    }

    /// Attaches the filled chests every structure piece reaching this chunk asks
    /// for. [`crate::structure_loot`] supplies the marker pass.
    ///
    /// **The starts come from `references`, not from `structure_starts(cx, cz)`.**
    /// The latter is the starts whose *origin* is this column, and a shipwreck's
    /// chest is routinely in a neighbouring chunk; `references` is vanilla's own
    /// "which structures reach here" answer, already narrowed to the chunk box.
    /// Using the origin list instead loses every chest that crosses a border,
    /// which is most of them and which no count-the-chests test would notice.
    fn fill_structure_chests(
        &self,
        column: &mut ChunkColumn,
        cx: i32,
        cz: i32,
        references: &std::collections::BTreeMap<String, Vec<i64>>,
    ) {
        if references.is_empty() {
            return;
        }
        let mut origins: Vec<(i32, i32)> = references
            .values()
            .flatten()
            .map(|packed| (*packed as u32 as i32, (*packed >> 32) as u32 as i32))
            .collect();
        origins.sort_unstable();
        origins.dedup();

        let mut starts = Vec::new();
        for (ox, oz) in origins {
            starts.extend(self.generator.structure_starts(ox, oz));
        }
        let chests = crate::structure_loot::chests_for_chunk(
            &starts,
            cx,
            cz,
            crate::block_drops::bundled_tables(),
        );
        if chests.is_empty() {
            return;
        }
        let mut entities = column.block_entities().to_vec();
        for chest in chests {
            if let Some(block) = chest.block {
                column.set_block(
                    chest.pos.x.rem_euclid(16),
                    chest.pos.y,
                    chest.pos.z.rem_euclid(16),
                    block,
                );
            }
            entities.push((chest.pos, chest.entity));
        }
        column.set_block_entities(entities);
    }
}

impl std::fmt::Debug for OverworldChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverworldChunkSource")
            .finish_non_exhaustive()
    }
}

impl ChunkSource for OverworldChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_at(cx, cz, ChunkGenerationStage::Full)
    }

    fn column_at(&self, cx: i32, cz: i32, stage: ChunkGenerationStage) -> ChunkColumn {
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        let generated = match stage {
            ChunkGenerationStage::Shaped => self.generator.column_shaped(cx, cz),
            ChunkGenerationStage::Full => self.generator.column(cx, cz),
        };
        let mut column = ChunkColumn::from_generated(generated);
        self.attach_structures(&mut column, cx, cz);
        column
    }

    // There is no cheaper single-block path here: the generator only answers
    // whole columns, so this goes through `column()`, which already consults
    // `edits` first — so the answer reflects a `set_block` edit exactly as a
    // `column()` read would. A source with no edits to consult could skip
    // this and reuse the column-regenerating form; this one keeps it explicit.
    // (`crate::chunk_store::ChunkStore`, which wraps this source, overrides
    // `block_state` with the one-cell read that avoids the regeneration.)
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits.entry((cx, cz)).or_insert_with(|| {
            let mut column = ChunkColumn::from_generated(self.generator.column(cx, cz));
            // Same attachment as `column()`, because an edited column is the one
            // that gets *saved* — dropping the structures here would delete a
            // village's `starts` from the first chunk a player breaks a block in,
            // and dropping the chests would empty a shipwreck the moment someone
            // mined a block in its chunk.
            self.attach_structures(&mut column, cx, cz);
            column
        });
        column.set_block(lx, y, lz, name);
    }
}

/// The Nether's terrain source — [`OverworldChunkSource`]'s counterpart for the
/// second dimension this server hosts.
///
/// Same retention rule as its sibling and for the same reason: `edits` is
/// populated **only** by [`set_block`](Self::set_block), so an untouched column is
/// regenerated on demand and only a column a player (or a portal) has actually
/// changed costs memory. That matters more here than in the overworld, because
/// *every* Nether portal trip writes blocks — the destination portal the travel
/// path builds when it finds none is a `set_block` fan-out, and it has to still be
/// there when the player walks back into it.
///
/// # The window height is 256, not the generator's 128
///
/// [`WINDOW_HEIGHT`](Self::WINDOW_HEIGHT) is the *dimension type*'s height, and
/// [`ChunkColumn::from_nether`] pads to it. See that constructor's doc for why
/// serving the generator's own 128 rows is a client-side decode failure rather
/// than a short world.
pub struct NetherChunkSource {
    generator: lodestone_worldgen::nether::NetherGenerator,
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
}

impl NetherChunkSource {
    /// The Nether dimension type's `min_y` (`DimensionTypes`'
    /// `BuiltinDimensionTypes.NETHER`).
    pub const MIN_Y: i32 = 0;
    /// The Nether dimension type's `height` — **not** its `logical_height` of
    /// 128, and not the generator's 128 either. See the struct doc.
    pub const WINDOW_HEIGHT: i32 = 256;

    /// Wraps a pre-built Nether generator.
    #[must_use]
    pub fn new(generator: lodestone_worldgen::nether::NetherGenerator) -> Self {
        Self {
            generator,
            edits: Mutex::new(HashMap::new()),
        }
    }

    /// The lowest world `y` this source's columns contain.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        Self::MIN_Y
    }

    /// How many `y` levels this source's columns contain — the dimension's, not
    /// the generator's. See the struct doc.
    #[must_use]
    pub fn height(&self) -> i32 {
        Self::WINDOW_HEIGHT
    }

    fn generate(&self, cx: i32, cz: i32) -> ChunkColumn {
        ChunkColumn::from_nether(self.generator.column(cx, cz), Self::WINDOW_HEIGHT)
    }
}

impl std::fmt::Debug for NetherChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetherChunkSource").finish_non_exhaustive()
    }
}

impl ChunkSource for NetherChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        self.generate(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block(lx, y, lz, name);
    }
}

/// The End's terrain source — [`NetherChunkSource`]'s counterpart for the third
/// dimension this engine generates real terrain for.
///
/// Same retention rule as its siblings, for the same reason: `edits` is
/// populated only by [`set_block`](Self::set_block), so an untouched column is
/// regenerated on demand.
///
/// **Constructed, but not reachable by a player.** `crate::integrated`'s
/// `with_nether` sibling factory has an `End` arm that builds one of these
/// (mirroring the `Nether` arm), so `DimensionalSource::sibling(Dimension::End)`
/// answers `Some`. A trip still requires an end-portal-frame ignition and a
/// end-portal-frame ignition and no step-into-`end_portal` teleport. See
/// `crate::dimension`'s module doc and `docs/nether-portals.md`'s "How to change
/// it" for the exact remaining hops.
///
/// # The window height is 256, not the generator's 128
///
/// Same shape as [`NetherChunkSource`]'s own gotcha: [`WINDOW_HEIGHT`](Self::WINDOW_HEIGHT)
/// is the End dimension type's `height`, and [`ChunkColumn::from_end`] pads to
/// it. See that constructor's doc.
pub struct EndChunkSource {
    generator: lodestone_worldgen::end::EndGenerator,
    edits: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    /// Set the first time [`ChunkSource::claim_dragon_fight_start`] succeeds
    /// against this instance — see that method's own doc comment for why this
    /// is a process-lifetime gate rather than a persisted one.
    dragon_fight_started: std::sync::atomic::AtomicBool,
}

impl EndChunkSource {
    /// The End dimension type's `height` — **not** its `logical_height` (which, unlike
    /// the Nether's, is the same 256), and not the generator's 128 either. See the
    /// struct doc.
    pub const WINDOW_HEIGHT: i32 = 256;

    /// Wraps a pre-built End generator.
    #[must_use]
    pub fn new(generator: lodestone_worldgen::end::EndGenerator) -> Self {
        Self {
            generator,
            edits: Mutex::new(HashMap::new()),
            dragon_fight_started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn generate(&self, cx: i32, cz: i32) -> ChunkColumn {
        ChunkColumn::from_end(self.generator.column(cx, cz), Self::WINDOW_HEIGHT)
    }
}

impl std::fmt::Debug for EndChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndChunkSource").finish_non_exhaustive()
    }
}

impl ChunkSource for EndChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        self.generate(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block(lx, y, lz, name);
    }

    /// The one real override — a compare-exchange on `dragon_fight_started`,
    /// so exactly one caller among any concurrent claimants sees `true`.
    fn claim_dragon_fight_start(&self) -> bool {
        self.dragon_fight_started
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }
}

/// A solidity-only [`ChunkSource`] backed by a bare density node.
///
/// **Not the real generator** — see the module docs. It point-samples
/// `final_density` per block and maps `> 0` to stone, with no cell
/// interpolation, surface, or fluid. Kept for the in-memory-transport tests,
/// which need a deterministic terrain to prove the wire round-trip, not a
/// vanilla-accurate one. For real terrain use [`OverworldChunkSource`].
#[derive(Debug, Clone)]
pub struct WorldgenChunkSource {
    final_density: Density,
    min_y: i32,
    height: i32,
}

impl WorldgenChunkSource {
    /// Wraps a pre-built `final_density` node with the world's vertical extent.
    #[must_use]
    pub fn new(final_density: Density, min_y: i32, height: i32) -> Self {
        Self {
            final_density,
            min_y,
            height,
        }
    }
}

impl ChunkSource for WorldgenChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(self.min_y, self.height);
        let base_x = cx * 16;
        let base_z = cz * 16;
        for lx in 0..16 {
            for lz in 0..16 {
                let wx = base_x + lx;
                let wz = base_z + lz;
                for ly in 0..self.height {
                    let wy = self.min_y + ly;
                    let d = self.final_density.compute(Context::new(wx, wy, wz));
                    if d > 0.0 {
                        col.set_solid(lx, wy, lz, true);
                    }
                }
            }
        }
        col
    }

    // This source is solidity-only: one block is stone iff its density
    // sample is positive, mirroring `column()`'s `set_solid` rule exactly
    // (including air for any y outside the vertical extent). Point-sampling
    // the density node is cheaper than `column()` and gives the same answer,
    // so unlike the column-regenerating form this is the efficient read.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if !(self.min_y..self.min_y + self.height).contains(&y) {
            return AIR.to_string();
        }
        if self.final_density.compute(Context::new(x, y, z)) > 0.0 {
            STONE.to_string()
        } else {
            AIR.to_string()
        }
    }

    /// This source stamps no biome data of its own (a solidity-only
    /// transport-test source — see [`block_state`](Self::block_state)'s own
    /// doc), so every cell reads [`DEFAULT_BIOME`] via
    /// [`ChunkColumn::new`]'s own default, through the one path that column
    /// actually exists on: `column()`, not a point-sampled shortcut like
    /// `block_state`'s (there is no density-shaped biome field to sample).
    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    /// This source has no per-column retention — every `column()` call
    /// regenerates fresh from the density node — so there is nowhere for an
    /// edit to live. That is a deliberate property of a solidity-only
    /// transport-test source, not a gap: reach for
    /// [`OverworldChunkSource`] (or [`crate::region_source::RegionChunkSource`])
    /// when edits must persist. Panics loudly rather than silently discarding
    /// the placement.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let _ = (x, y, z, name);
        todo!("WorldgenChunkSource is a solidity-only, non-retaining source; it cannot accept a set_block edit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `y_clamped_gradient` that is positive below y=0 and negative above acts
    /// as a flat solid floor, letting us verify the sign-field logic with no
    /// external data.
    fn floor_density() -> Density {
        Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        }
    }

    #[test]
    fn worldgen_source_maps_positive_density_to_solid() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
        let col = src.column(0, 0);
        // Deep down (y = -64) density is +1 → solid; high up (y = 63) it is
        // near -1 → air. The crossover is y = 0.
        assert!(col.is_solid(0, -64, 0));
        assert!(col.is_solid(5, -1, 9));
        assert!(!col.is_solid(0, 0, 0));
        assert!(!col.is_solid(5, 40, 9));
        // Every one of the 16×16 columns is solid for exactly y in [-64, -1].
        assert_eq!(col.solid_count(), 16 * 16 * 64);
    }

    /// [`EndChunkSource`] serves the *dimension's* 256-row window, not the
    /// generator's own 128 — the same padding [`NetherChunkSource`] needs and for
    /// the same reason (see [`ChunkColumn::from_end`]'s doc). A source that
    /// forgot the pad would report a column whose `height` disagrees with what
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
                    column.block_state(x, 200, z),
                    "minecraft:air",
                    "padding above the generator's own 128 rows must be air at ({x},200,{z})"
                );
            }
        }
        // And the generator's own terrain is not lost in the pad: some cell in
        // its native range is non-air, at the main island's centre chunk.
        let solid = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .any(|(x, z)| (0..128).any(|y| column.block_state(x, y, z) != "minecraft:air"));
        assert!(solid, "the generator's own 0..128 range must not be entirely air at the island's centre");
    }

    #[test]
    fn out_of_range_is_air() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
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

    /// Every non-`EndChunkSource` [`ChunkSource`] answers the trait's
    /// default (`true`, "already claimed") unconditionally — the correct
    /// degradation for a source this is never meaningfully asked of. Checked
    /// against [`WorldgenChunkSource`] as a representative non-End source,
    /// and twice in a row to prove the default is not itself a one-shot gate
    /// that happens to start `true`.
    #[test]
    fn a_non_end_source_always_answers_the_default_claim() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
        assert!(src.claim_dragon_fight_start());
        assert!(src.claim_dragon_fight_start());
    }

    #[test]
    fn set_block_round_trips_and_fluids_are_not_solid() {
        let mut col = ChunkColumn::new(0, 16);
        col.set_block(3, 5, 7, "minecraft:grass_block[snowy=false]");
        col.set_block(3, 4, 7, "minecraft:water[level=0]");
        assert_eq!(
            col.block_state(3, 5, 7),
            "minecraft:grass_block[snowy=false]"
        );
        // Grass is solid; water is a fluid and therefore not solid.
        assert!(col.is_solid(3, 5, 7));
        assert!(!col.is_solid(3, 4, 7));
        // Only the grass block counts toward solidity.
        assert_eq!(col.solid_count(), 1);
    }

    /// Canonical byte serialisation of a column's full content — `min_y`,
    /// `height`, the palette (length-prefixed strings), the block-index
    /// grid, then the biome quarts (length-prefixed strings). Two columns
    /// with identical bytes here carry identical block/biome content; this
    /// is the "emitted byte sequence" the determinism control below
    /// compares, standing in for the real wire encoding (which lives behind
    /// `ServerProtocol` in the protocol crates, not reachable from here).
    fn column_bytes(col: &ChunkColumn) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&col.min_y.to_le_bytes());
        out.extend_from_slice(&col.height.to_le_bytes());
        for s in &col.palette {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
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

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            // The gates only ever call `column()`, so this is the plain
            // column-regenerating form, kept for completeness.
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16);
            let lz = z.rem_euclid(16);
            self.column(cx, cz).block_state(lx, y, lz).to_string()
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
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
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
    /// `generate_columns_parallel` stays in the tree (it is what
    /// `SourceRef::Borrowed` still uses), so the inline control is measurable
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
        let offloaded = generate_columns_offloaded(
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
        let blocking = generate_columns_parallel(
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
             runtime — i.e. `spawn_blocking` is not being reached"
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
        let serial: Vec<String> = coords
            .iter()
            .map(|&(cx, cz)| {
                let source = WorldgenChunkSource::new(floor_density(), -64, 128);
                source.column(cx, cz).block_state(0, -1, 0).to_string()
            })
            .collect();

        let offloaded = generate_columns_offloaded(
            Arc::new(WorldgenChunkSource::new(floor_density(), -64, 128)),
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
        let offloaded_states: Vec<String> = offloaded
            .iter()
            .map(|column| column.block_state(0, -1, 0).to_string())
            .collect();
        assert_eq!(
            offloaded_states, serial,
            "offloaded generation must hand back columns aligned index-for-index with \
             `coords` — the wire order depends on it (see `generate_columns_parallel`)"
        );
    }

    /// [`generate_columns_yielding`]/[`map_columns_yielding`] are what
    /// `generate_columns_offloaded`/`generate_and_encode_columns_offloaded`'s
    /// wasm32 branches call instead of `generate_columns_parallel` — whose
    /// `coords.len() > 1` branch TRAPS on wasm32 via `std::thread::scope` (see
    /// their doc comments; measured executing the equivalent
    /// `std::thread::scope(|s| s.spawn(...))` in a wasm VM: `unreachable`).
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

            fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
                "minecraft:air".to_string()
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_string()
            }

            // Discarded by design (the explicit-choice rule) — this
            // fixture only ever reads.
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
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
}
