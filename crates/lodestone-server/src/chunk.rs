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
//! must go on reflecting that mutation afterward — that only works if *something*
//! retains the edited column, and before this existed, nothing did:
//! `OverworldChunkSource::column` called straight through to the generator on
//! every request. See [`OverworldChunkSource`]'s own doc comment for the
//! retention this module now adds and why it is scoped to edited columns only,
//! not every column ever requested.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_model::BlockPos;
use lodestone_worldgen::density::{Context, Density};
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};

use crate::block_entities::BlockEntity;
use crate::chunk_blocks::SectionedBlocks;

pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const STONE: &str = "minecraft:stone";
/// Rows per implicit section. [`ChunkColumn`] has no per-section struct — a
/// "section" here is a 16-row window of the one flat grid, counted from
/// `min_y` — so this is the only place the window height is written down.
pub(crate) const SECTION_ROWS: usize = 16;
/// Fallback biome for a [`ChunkColumn`] built with no generator behind it
/// ([`ChunkColumn::new`]'s blank column, and [`WorldgenChunkSource`], which
/// only ever models solidity — see that type's own doc comment). A column
/// adopted from the real generator via [`ChunkColumn::from_generated`] always
/// overwrites this with real per-quart biome data (issue #405).
pub(crate) const DEFAULT_BIOME: &str = "minecraft:plains";

/// Vertical quart layers in a column of `height` block rows — the one place the
/// 3-D biome grid's Y extent is written down (issue #512). Matches
/// [`lodestone_worldgen::overworld::BiomeCells`]'s own arithmetic exactly, which
/// is what lets [`ChunkColumn::from_generated`] adopt its indices verbatim.
fn y_quarts_for(height: i32) -> usize {
    (height as usize).div_ceil(4).max(1)
}

/// Returns `true` for blocks that do not count as collidable terrain: air
/// variants and fluids. `is_solid` is the negation of this over the block name.
///
/// Also doubles as this crate's "can a placement replace this cell" test
/// (`crate::server`'s `UseItemOn` handling) — vanilla's real `canBeReplaced`
/// covers a wider set (tall grass, snow layers, …), but the generator this
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
    /// Block-state palette; `palette[0]` is always `"minecraft:air"`.
    palette: Vec<String>,
    /// Palette indices for every cell, one bit-packed 16-row section at a time
    /// (`crate::chunk_blocks`). Logically the same
    /// `blocks[(y_local * 16 + z) * 16 + x]` grid this used to be a flat
    /// `Vec<u16>` of; an all-air section now allocates nothing and a populated
    /// one packs to the width its ids need instead of 16 bits.
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
    /// How many cells in each implicit 16-row window hold a randomly-ticking
    /// state — vanilla's `LevelChunkSection.tickingBlockCount`
    /// (`LevelChunkSection.java:16`), one entry per section, `len =
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
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` (issue #405).
    ///
    /// **The surface answer, not the column's biome.** This is what a player
    /// standing on the column sees, and what surface material, carve and
    /// decorate consumed on the generator side. It is deliberately *not* what
    /// the wire or a region file's per-section biome container is built from —
    /// see [`biome_cells`](Self::biome_cells) and issue #512.
    biome_quarts: [String; 16],
    /// Issue #512: the distinct biome ids in this column, first-use order.
    /// `biome_palette[0]` always exists.
    biome_palette: Vec<String>,
    /// Issue #512: palette indices for the full 4×4×4-per-section biome grid,
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
    /// Issue #520: block entities living in this column, at **absolute**
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
    /// Issue #514's S1: the structure starts whose **origin** is this column, and
    /// this column's `structures.References`.
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
    /// Issue #516: the generator's `MOTION_BLOCKING` heightmap in vanilla's
    /// **stored** form (`topY + 1`, `0` for an all-air column), indexed
    /// `lx + lz * 16` — see
    /// [`lodestone_worldgen::overworld::GeneratedColumn::motion_blocking_heightmap`].
    ///
    /// `None` for a column that came from anywhere but the real generator
    /// (`new`, a region-file load); `encode_chunk` then sends the zero-entry
    /// heightmap NBT it has always sent, which is well-framed and simply carries
    /// no map. It rides an accessor rather than `GeneratedColumn::into_raw`,
    /// whose own doc forbids widening that tuple — the same reason
    /// [`biome_cells`](Self::biome_cells) and
    /// [`block_entities`](Self::block_entities) are copied across.
    ///
    /// **Not maintained by [`set_block`](Self::set_block).** It is the
    /// generator's snapshot, so a player edit does not move it; `chunk_nbt`
    /// deliberately omits heightmaps from the Anvil write and relies on
    /// vanilla's `Heightmap.primeHeightmaps` to re-derive on load, so nothing
    /// persists a stale value either. Only the first send after generation
    /// carries it, which is exactly the send a client has no other way to
    /// derive one for.
    motion_blocking: Option<Box<[u16; 256]>>,
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
            palette: vec![AIR.to_string()],
            blocks: SectionedBlocks::new_air(height),
            // All-air, so every section count is zero and every palette entry
            // is classified — correct by construction with no counting pass,
            // exactly like vanilla's empty-section constructor
            // (`LevelChunkSection.java:36-39`), which likewise does not call
            // `recalcBlockCounts`. The one classification is still routed
            // through the predicate rather than hardcoded `false`, so the
            // table cannot drift from the definition.
            palette_ticking: vec![crate::random_tick::is_randomly_ticking(AIR)],
            section_ticking: vec![0u16; (height as usize).div_ceil(SECTION_ROWS)],
            biome_quarts: std::array::from_fn(|_| DEFAULT_BIOME.to_string()),
            biome_palette: vec![DEFAULT_BIOME.to_string()],
            biome_cells: vec![0u16; y_quarts_for(height) * 16],
            block_entities: Vec::new(),
            structure_starts: Vec::new(),
            structure_references: std::collections::BTreeMap::new(),
            motion_blocking: None,
        }
    }

    /// Adopts a [`GeneratedColumn`] from the real worldgen pipeline: the palette
    /// moves as-is, and the flat block grid is *packed* into
    /// [`SectionedBlocks`] — one pass over the cells the caller has just written,
    /// which is also the pass that discards the ~160 KiB of it that is air (see
    /// `crate::chunk_blocks`). Real per-quart biome data comes across too (issue
    /// #405).
    ///
    /// The grid used to move by value rather than be repacked. The move was
    /// cheaper per column and is what made the *retained* cost 192 KiB each;
    /// `chunk_store`'s 909 ms-per-column generation figure is the scale this one
    /// extra sequential pass is measured against.
    ///
    /// The 3-D biome grid (issue #512) and the block-entity list (issue #520)
    /// are *copied* rather than moved, because `GeneratedColumn::into_raw`
    /// deliberately does not carry them — see that method's doc comment. Both
    /// are small: a column's biome grid is `height / 4 * 16` `u16`s over a
    /// handful of palette entries (~3 KB), and nearly every column has zero
    /// block entities.
    #[must_use]
    pub fn from_generated(column: GeneratedColumn) -> Self {
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
        // Issue #516. Copied before `into_raw` consumes the column, for the same
        // reason the two above are.
        let motion_blocking = column.motion_blocking_heightmap().map(|map| Box::new(*map));

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
            palette,
            blocks,
            // Placeholders: this constructor *adopts* an already-populated
            // grid, so the counters cannot be right by construction the way
            // `new`'s all-air ones are. `recalc_ticking_counts` below is the
            // one counting pass in the crate — vanilla's
            // `recalcBlockCounts`, called from exactly the analogous
            // constructor (`LevelChunkSection.java:33`).
            palette_ticking: Vec::new(),
            section_ticking: Vec::new(),
            biome_quarts,
            biome_palette,
            biome_cells,
            block_entities,
            structure_starts: Vec::new(),
            structure_references: std::collections::BTreeMap::new(),
            motion_blocking,
        };
        column.recalc_ticking_counts();
        debug_assert_eq!(
            column.biome_cells.len(),
            column.biome_y_quarts() * 16,
            "generated biome grid must span the column's own height"
        );
        column
    }

    /// Biome id at local `(x, z)` in `0..16` — quart resolution, the column's
    /// **surface** answer, the same value for every `y` (issue #405).
    ///
    /// **Wrong question for anything with a `y`** — underground tint, fog,
    /// spawn rules, a wire or region-file biome container. Use
    /// [`biome_state_at`](Self::biome_state_at) for those; see issue #512.
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

    /// Biome id at quart `(qx, qy, qz)` (issue #512).
    #[must_use]
    pub fn biome_cell(&self, qx: usize, qy: usize, qz: usize) -> &str {
        &self.biome_palette[self.biome_cell_index(qx, qy, qz) as usize]
    }

    /// Biome id at a block position — local `x`/`z` in `0..16`, world `y`
    /// (issue #512). Out-of-column `y` clamps to the nearest layer, as every
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

    /// Every block entity in this column, at its **absolute** position (issue
    /// #520). Empty for the overwhelming majority of columns.
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

    /// This column's `MOTION_BLOCKING` heightmap in vanilla's stored form, or
    /// `None` if it did not come from the generator — see
    /// [`motion_blocking`](Self::motion_blocking) for the whole contract and
    /// `docs/motion-blocking-heightmap.md` for the `+1`.
    #[must_use]
    pub fn motion_blocking(&self) -> Option<&[u16; 256]> {
        self.motion_blocking.as_deref()
    }

    /// The structure starts originating in this column (issue #514's S1).
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

    /// Recomputes both derived ticking tables from scratch — vanilla's
    /// `LevelChunkSection.recalcBlockCounts` (`LevelChunkSection.java:122-153`),
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
        let sections = (self.height as usize).div_ceil(SECTION_ROWS);
        let mut counts = vec![0u16; sections];
        for s in 0..sections {
            let mut count = 0u16;
            // Per section rather than over one flat grid, because the sections
            // *are* the storage now — and a uniform (usually all-air) section
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
        debug_assert_eq!(
            self.palette.len(),
            self.palette_ticking.len(),
            "palette and its ticking classification must stay the same length"
        );
        (self.palette.len() - 1) as u16
    }

    /// Sets the block state at a local `(x, z)` in `0..16` and world `y`.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let id = self.intern(name);
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
                    "section_ticking[{section}] underflowed writing {name} at ({x}, {y}, {z}): \
                     a randomly-ticking state left a cell the counter did not know held one, so \
                     some mutation path reached `blocks` without `set_block` or \
                     `recalc_ticking_counts`"
                );
                self.section_ticking[section] -= 1;
            }
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

    /// `LevelChunkSection::isRandomlyTicking`'s boolean for the 16-row window
    /// whose lowest row is world `section_min_y` — `tickingBlockCount > 0`
    /// (`LevelChunkSection.java:110-112`).
    ///
    /// **O(1): one integer compare.** This is the whole point of the counters;
    /// `crate::random_tick` used to reach the identical boolean by scanning up
    /// to 4096 cells per section, per column, per tick (issue #507). A
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

    /// Reads a single block's canonical state string at world coordinates
    /// `(x, y, z)`, through the same data [`column`](Self::column) would
    /// return — including any edit already applied via
    /// [`set_block`](Self::set_block).
    ///
    /// This is a required method so no implementor silently inherits a
    /// whole-column regeneration for a one-block read (the historical
    /// default — issue #440). An implementor with a cheaper path, one that
    /// reads a cell out of a column it already retains, must override this
    /// to avoid regenerating on every probe: the `ChunkStore` wrapper is the
    /// reference example. An implementor with no cheaper path implements it
    /// as `self.column(cx, cz).block_state(..)`, which is correct if
    /// column-sized; the point is that the choice is explicit at every
    /// implementor rather than silently inherited.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String;

    /// Overwrites a single block's state at world coordinates `(x, y, z)`,
    /// persisting the change so a later [`column`](Self::column) call for
    /// its chunk reflects it.
    ///
    /// This is a required method so no implementor can silently drop a
    /// placement: the historical default was a no-op, which made "block
    /// placement fails with no error" the default experience for any new
    /// source that forgot to override it (issue #440). Every implementor must
    /// now decide explicitly how edits are stored. A source with no per-column
    /// retention must say so loudly — a `todo!()`, or an explicitly documented
    /// discard — rather than inherit silence.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str);

    /// The block entity this source's data carries at `(x, y, z)`, if any —
    /// a *generated* one, such as a structure chest's rolled contents
    /// (issue #337) or a bee nest's occupants.
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

    /// Tells the source that a connection's view radius is now `view_radius`, so
    /// a layer that *retains* columns can resize its bound to match.
    ///
    /// The default is a no-op, correct for every source that retains nothing per
    /// view. [`crate::chunk_store::ChunkStore`] is the one implementor that acts
    /// on it.
    ///
    /// # Why this exists (issue #551)
    ///
    /// `ChunkStore`'s capacity was fixed at construction from the radius the
    /// connection *joined* with. Since `0c09f576` a client can raise its render
    /// distance mid-session and the server honours it — so the streamed view then
    /// exceeds the cache bound, and the LRU victim under a short capacity is the
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
}

/// Generates every column in `coords` across scoped OS threads over `&source`,
/// returning them in the **same order as `coords`** regardless of which
/// thread finished which column first.
///
/// This is safe because `column()` is genuinely pure per chunk: every RNG a
/// generator touches is positionally seeded (`set_decoration_seed` /
/// `set_feature_seed` / `setLargeFeatureSeed` per source chunk,
/// `fork_positional`/`from_hash_of`) with no shared RNG stream anywhere in
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
pub(crate) fn generate_columns_parallel<S: ChunkSource>(
    source: &S,
    coords: &[(i32, i32)],
) -> Vec<ChunkColumn> {
    if coords.len() <= 1 {
        return coords.iter().map(|&(cx, cz)| source.column(cx, cz)).collect();
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
                scope.spawn(move || {
                    slice
                        .iter()
                        .map(|&(cx, cz)| source.column(cx, cz))
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

/// [`generate_columns_parallel`], moved off the async runtime's core thread
/// (issue #293).
///
/// # Why this exists when generation is already parallel
///
/// [`generate_columns_parallel`] (issue #414) fixed *throughput*: the batch is
/// fanned out over scoped OS threads. It did nothing about *latency*, because
/// its final `std::thread::scope` join blocks the calling thread until every
/// worker finishes. Parallel is not the same as non-blocking, and the
/// distinction is total rather than academic here: the shell builds the
/// server's runtime with `tokio::runtime::Builder::new_current_thread()`
/// (`crates/lodestone-shell/src/net.rs`), so the connection task and
/// [`crate::tick::run_tick_loop`] share **one** thread. Blocking it blocks
/// *every* task in the process — the world tick included — so before this
/// function every chunk-boundary crossing in singleplayer dropped one or more
/// 50 ms ticks.
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
/// stays correct on a multi-thread runtime — so nothing here has to be
/// revisited if issue #281's thread split ever lands.
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
/// `wasm32-unknown-unknown` has no blocking pool (and no OS threads for
/// `generate_columns_parallel`'s scope either), so there it calls straight
/// through — unchanged behaviour on a target that never had a second thread
/// to protect.
#[tracing::instrument(skip_all, fields(count = coords.len()))]
pub(crate) async fn generate_columns_offloaded<S: ChunkSource + 'static>(
    source: Arc<S>,
    coords: Vec<(i32, i32)>,
) -> Vec<ChunkColumn> {
    #[cfg(target_arch = "wasm32")]
    {
        generate_columns_parallel(&*source, &coords)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::task::spawn_blocking(move || generate_columns_parallel(&*source, &coords))
            .await
            .expect("worldgen blocking task panicked")
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
    /// freshly built column (issue #514's S1).
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
    /// for (issue #337) — see [`crate::structure_loot`] for the marker pass.
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
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        let mut column = ChunkColumn::from_generated(self.generator.column(cx, cz));
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

    #[test]
    fn out_of_range_is_air() {
        let src = WorldgenChunkSource::new(floor_density(), -64, 128);
        let col = src.column(1, -3);
        assert!(!col.is_solid(0, 5000, 0));
        assert!(!col.is_solid(0, -5000, 0));
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
    /// This is the property the task exists to protect: per-chunk RNG is
    /// positionally seeded (`set_decoration_seed`/`set_feature_seed`/
    /// `setLargeFeatureSeed`, `fork_positional`/`from_hash_of` —
    /// `lodestone-worldgen`'s own doc comments), so there is no shared RNG
    /// stream for thread scheduling to desync. A single passing repeat would
    /// prove nothing about a scheduling-dependent race, so this runs the
    /// parallel path many times against one fixed coordinate set, over a
    /// coordinate count that does not divide evenly across
    /// `available_parallelism` worker batches, to make an off-by-one batch
    /// boundary bug visible if one existed.
    ///
    /// **Made vacuous by `6509a97`'s pre-ore memoisation cache, now fixed.**
    /// The cache lives on `OverworldGenerator` (per-instance, keyed by exact
    /// `(cx, cz)`, capped at 512 entries, never evicted below that). This
    /// test used to build **one** `source` and reuse it for the serial
    /// baseline *and* all 8 parallel repeats — so the serial pass warmed
    /// every coordinate's cache entry, and every parallel repeat after it
    /// was a pure cache hit, never touching the real generation path at all.
    /// It still proved ordering (the `Vec` comes back aligned to `coords`)
    /// and it still proved the ore stage itself is deterministic (the
    /// cached pre-ore result feeds a fresh `ore_stage` call each time), but
    /// it stopped proving **recomputation** determinism — the exact thing a
    /// server restart, or a cache eviction under load, actually needs — and
    /// it never exercised a concurrent cache *miss* despite spawning
    /// multiple threads over the same coordinates repeatedly.
    ///
    /// Fixed by building a fresh, **independently constructed**
    /// `overworld_chunk_source(42)` for the serial baseline and for *every*
    /// one of the 8 parallel repeats — each starts from a cold cache, so
    /// each repeat's `generate_columns_parallel` call is a genuine
    /// concurrent-miss race across `available_parallelism` threads writing
    /// into a fresh `Mutex`-protected cache, not a replay of one already
    /// populated. A byte match across all 9 independent constructions is
    /// real cross-construction determinism, not a shared cache artifact.
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
    /// wall-clock, which is the one property of real worldgen issue #293 is
    /// about. Deliberately hand-written rather than
    /// [`crate::overworld_chunk_source`]: the real generator carries a
    /// 512-entry memo cache that would absorb a second request for the same
    /// `(cx, cz)` and make any count- or duration-based gate vacuous — the
    /// exact trap already found and fixed in
    /// `parallel_generation_is_deterministic_and_matches_serial` just above.
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

        // A wall-clock-only fixture: it exists to make `column()` take a fixed
        // amount of blocking time, and no gate here writes blocks. Deliberately
        // discards rather than inheriting a silent default — the point of
        // issue #440 is that such a choice must be explicit per implementor.
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            // No storage; edits are discarded by design for this fixture.
        }
    }

    /// The world tick's period, scaled down so the gate runs in well under a
    /// second. `run_tick_loop` uses 50 ms (`crate::tick::TICK_PERIOD`); the
    /// shape that matters — a task parked on `sleep`/`sleep_until` — is
    /// identical.
    const GATE_TICK_PERIOD: std::time::Duration = std::time::Duration::from_millis(10);

    /// Issue #293: chunk generation must not block the async runtime.
    ///
    /// # What this measures, and what it would miss
    ///
    /// `generate_columns_parallel` (issue #414) made generation *parallel*,
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
    /// stalls every task in the process. Before this, every chunk-boundary
    /// crossing in singleplayer dropped one or more 50 ms world ticks.
    ///
    /// # The negative control is the second arm, permanently
    ///
    /// `generate_columns_parallel` stays in the tree (it is what
    /// `SourceRef::Borrowed` still uses), so the pre-fix behaviour is
    /// measurable here forever rather than only during a temporary neuter. The
    /// control must record **zero** ticks. Measured when this landed:
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
    /// would have been the wrong instrument for exactly that reason: it
    /// accumulates MSPT/TPS/overrun over a whole server lifetime, so it cannot
    /// distinguish "no stall now" from "the stall already averaged away."
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

        // --- Arm 1: offloaded (the fix). ---
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
        let started = std::time::Instant::now();
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
        let control_started = std::time::Instant::now();
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
}
