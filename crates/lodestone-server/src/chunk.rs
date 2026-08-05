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

use lodestone_worldgen::density::{Context, Density};
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};

pub(crate) const AIR: &str = "minecraft:air";
pub(crate) const STONE: &str = "minecraft:stone";
/// Fallback biome for a [`ChunkColumn`] built with no generator behind it
/// ([`ChunkColumn::new`]'s blank column, and [`WorldgenChunkSource`], which
/// only ever models solidity — see that type's own doc comment). A column
/// adopted from the real generator via [`ChunkColumn::from_generated`] always
/// overwrites this with real per-quart biome data (issue #405).
pub(crate) const DEFAULT_BIOME: &str = "minecraft:plains";

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
    /// `blocks[(y_local * 16 + z) * 16 + x]` indexes into `palette`.
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` (issue #405),
    /// constant across `y` — see [`GeneratedColumn::biome_state`]'s doc for
    /// why this port broadcasts one climate sample per quart column instead
    /// of a full 3-D grid.
    biome_quarts: [String; 16],
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
            blocks: vec![0u16; 16 * 16 * height as usize],
            biome_quarts: std::array::from_fn(|_| DEFAULT_BIOME.to_string()),
        }
    }

    /// Adopts a [`GeneratedColumn`] from the real worldgen pipeline. Zero-copy:
    /// the palette and block grid are moved as-is (their index layout is the
    /// same), including the real per-quart biome data (issue #405).
    #[must_use]
    pub fn from_generated(column: GeneratedColumn) -> Self {
        let (min_y, height, palette, blocks, biome_quarts) = column.into_raw();
        debug_assert_eq!(
            palette.first().map(String::as_str),
            Some(AIR),
            "generated palette must start with air"
        );
        Self {
            min_y,
            height,
            palette,
            blocks,
            biome_quarts,
        }
    }

    /// Biome id at local `(x, z)` in `0..16` — quart resolution, same value
    /// for every `y` at this `(x, z)` (issue #405).
    #[must_use]
    pub fn biome_state(&self, x: i32, z: i32) -> &str {
        debug_assert!((0..16).contains(&x) && (0..16).contains(&z));
        &self.biome_quarts[((z >> 2) * 4 + (x >> 2)) as usize]
    }

    #[inline]
    fn index(&self, x: i32, y_local: i32, z: i32) -> usize {
        debug_assert!((0..16).contains(&x));
        debug_assert!((0..16).contains(&z));
        debug_assert!((0..self.height).contains(&y_local));
        ((y_local * 16 + z) * 16 + x) as usize
    }

    /// Interns a block-state string into the palette, returning its index.
    fn intern(&mut self, name: &str) -> u16 {
        if let Some(i) = self.palette.iter().position(|p| p == name) {
            return i as u16;
        }
        self.palette.push(name.to_string());
        (self.palette.len() - 1) as u16
    }

    /// Sets the block state at a local `(x, z)` in `0..16` and world `y`.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let id = self.intern(name);
        let y_local = y - self.min_y;
        let i = self.index(x, y_local, z);
        self.blocks[i] = id;
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
        &self.palette[self.blocks[self.index(x, y_local, z)] as usize]
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
        self.blocks
            .iter()
            .filter(|&&id| !is_air_or_fluid(&self.palette[id as usize]))
            .count()
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

    /// The raw palette-index grid, `blocks[(y_local * 16 + z) * 16 + x]`.
    ///
    /// Same rationale as [`raw_palette`](Self::raw_palette). The layout is
    /// deliberately identical to a vanilla section's `(y << 8) | (z << 4) | x`
    /// order restricted to a 16-row window, so `chunk_nbt` slices it directly.
    #[must_use]
    pub fn raw_blocks(&self) -> &[u16] {
        &self.blocks
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
    /// The default recomputes the owning column and reads one cell out of
    /// it, so an implementor whose `column()` already consults an edit cache
    /// (see [`OverworldChunkSource`]) gets a correct answer for free without
    /// overriding this.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    /// Overwrites a single block's state at world coordinates `(x, y, z)`,
    /// persisting the change so a later [`column`](Self::column) call for
    /// its chunk reflects it.
    ///
    /// The default is a no-op: a source with no persistence (e.g.
    /// [`WorldgenChunkSource`], kept only for the solidity-only transport
    /// tests — see this module's own doc comment) silently discards edits
    /// rather than needing its own override. [`OverworldChunkSource`] is the
    /// one implementor that actually persists a `set_block` call; see its
    /// doc comment for why that retention did not already exist.
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let _ = (x, y, z, name);
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
        ChunkColumn::from_generated(self.generator.column(cx, cz))
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::from_generated(self.generator.column(cx, cz)));
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
        for &id in &col.blocks {
            out.extend_from_slice(&id.to_le_bytes());
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
