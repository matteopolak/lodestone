//! The End: vanilla's own End biome source and [`EndGenerator`], the third dimension this
//! engine produces real terrain for.
//!
//! # What it is
//!
//! [`EndBiomeSource`] is the complete port of vanilla's own End biome source
//! (its whole biome-layout logic) — the End's whole biome layout, and it works
//! without the density interpreter, because the only thing it samples is
//! the router's `erosion` channel and for the End that channel is exactly
//! `cache_2d(end_islands)` (confirmed against the
//! bundled `noise_settings/end.json`, whose `erosion` is literally
//! `{"type": "minecraft:cache_2d", "argument": {"type": "minecraft:end_islands"}}`).
//! So it is built straight on [`crate::noise::EndIslandNoise`].
//!
//! [`EndGenerator`] is [`crate::nether::NetherGenerator`] with **four
//! substitutions**, every one of which is data rather than code:
//!
//! | | Nether | End |
//! |---|---|---|
//! | biome | 5-row multi-noise parameter table | [`EndBiomeSource`] — one erosion sample per *chunk* |
//! | `default_fluid` | `minecraft:lava[level=0]`, `sea_level 32` | **`minecraft:air`**, `sea_level 0` — the End has no fluid at all |
//! | cell geometry | `size_horizontal 1, size_vertical 2` → 4×8 | **`2, 1`** → **8×4** |
//! | carver | `nether_cave` | **none** — `configured_carver` has four entries and no End biome document names one |
//!
//! Everything else is shared: `legacy_random_source: true` through
//! [`crate::rng::Algorithm`], `aquifers_enabled: false` through
//! [`crate::aquifer::AquiferSystem::disabled`], and the fill / heightmap /
//! materialise stages through [`crate::compose`].
//!
//! # What is *not* here, and it is not terrain
//!
//! * **The central island's furniture — partially closed.** Obsidian pillars,
//!   the exit portal, the gateway and the dragon are structure/entity work,
//!   and three of the four are *not worldgen at all* despite looking like it:
//!   vanilla's own end-podium feature type is never registered in the game's
//!   feature registry, the pillars and
//!   the end platform each have a gameplay placer as well as a worldgen one,
//!   and only `end_gateway_return` (rarity 700 in `end_highlands`) is reached
//!   from a biome document. [`spikes::end_spikes_for_seed`] (the ten obsidian
//!   pillars' layout) and [`podium::end_podium`] (the exit-portal/dragon-egg
//!   podium's own block writes) are now ported here — both pure functions,
//!   neither touching a world — because `mobs/end_crystal.rs`'s and
//!   `mobs/dragon.rs`'s own module docs (in `lodestone-server`) disclosed
//!   them as the reason the dragon fight has no production entry point. The
//!   gateway and the dragon entity itself are still not here.
//! * **Decoration — partial, production-connected coverage.** The fixed
//!   `end_platform` entry, outer islands, chorus plants, and return gateways
//!   are applied during [`EndGenerator::column`]. A three-by-three region lets
//!   source chunks write over a served chunk boundary. Return gateways retain
//!   their exit data in [`EndColumn::gateways`], so a consumer can create a
//!   block entity instead of silently retaining only its blocks.
//! * **`end_city`** is a template-piece structure rather than a terrain feature.
//!   [`EndGenerator`] builds an End-filtered structure registry, samples its
//!   four-point start height against the End density field, and applies every
//!   intersecting complete city piece after materialization. It has no terrain
//!   adaptation or placement-time refinement.
//!
//! # How it works
//!
//! ```text
//! chunkX² + chunkZ² <= 4096  ->  the_end                       (radius 64, the main island)
//! erosion >  0.25            ->  end_highlands
//! erosion >= -0.0625         ->  end_midlands
//! erosion <  -0.21875        ->  small_end_islands
//! otherwise                  ->  end_barrens
//! ```
//!
//! Two details that are easy to get wrong and are load-bearing:
//!
//! * **The sample position is not the quart's own block position.** Vanilla
//!   computes the sample block x as `(chunk_x * 2 + 1) * 8`, i.e.
//!   `chunk_x * 16 + 8` — the *chunk centre*, so all 16 quarts of a chunk
//!   share one erosion sample; sampling at the quart
//!   would give a finer-grained and wrong biome map.
//! * **The 4096 gate is `i64`** and matches `end_islands`' own centre hole exactly,
//!   which is why `the_end` covers precisely the region that can never carry an
//!   island.
//!
//! The block y coordinate is passed to the erosion sample and never read (the channel is
//! `cache_2d`, i.e. xz-only), so End biomes are y-invariant just as the Nether's
//! are.
//!
//! # How to change it
//!
//! The five biome ids are **not** data: vanilla's own End biome source serialises to an empty
//! object and its five holders come from the registry,
//! so they are constants here too rather than a resolver lookup.
//!
//! `scripts/worldgen-oracle/EndChunkOracle.java` supplies an independent terrain
//! fixture by driving the bundled server classes. `EndPlatformOracle.java` and
//! `EndDecorationOracle.java` independently capture the platform and feature
//! shapes; `end_city_jvm.txt` captures a positive city start, piece list, and
//! placed-block controls. Each fixture has a narrowly scoped gate — no terrain
//! fixture is treated as evidence for a later writer.
//!
//! # Dependencies
//!
//! [`crate::aquifer`], [`crate::compose`], [`crate::surface`],
//! [`crate::dense_grid`], [`crate::interner`], [`crate::noise::EndIslandNoise`] and
//! `lodestone-worldgen-core`'s density interpreter. Nothing version-specific.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use lodestone_data::biomes::{BiomeRef, BuiltinBiome};
use lodestone_data::block_entity_types::BlockEntityType;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::dense_grid::DenseBlockGrid;
use crate::density::{Builder, Resolver};
use crate::engine::Program;
use crate::interner::StateInterner;
use crate::noise::EndIslandNoise;
use crate::surface::{PreState, SurfaceDiff, SurfaceSystem, identity_canon};
use lodestone_worldgen_core::hash::FastMap;

mod podium;
mod spikes;
mod decorate;

pub use podium::{PodiumBlock, end_podium};
pub use decorate::{EndDecorationResult, EndDecorationSpill, EndGateway};
pub use spikes::{EndSpike, SPIKE_COUNT, end_spike_blocks, end_spikes_for_seed};

/// `Biomes.THE_END`.
pub const THE_END: &str = "minecraft:the_end";
/// `Biomes.END_HIGHLANDS`.
pub const END_HIGHLANDS: &str = "minecraft:end_highlands";
/// `Biomes.END_MIDLANDS`.
pub const END_MIDLANDS: &str = "minecraft:end_midlands";
/// `Biomes.SMALL_END_ISLANDS`.
pub const SMALL_END_ISLANDS: &str = "minecraft:small_end_islands";
/// `Biomes.END_BARRENS`.
pub const END_BARRENS: &str = "minecraft:end_barrens";

/// The main island's chunk radius, squared — `chunkX² + chunkZ² <= 4096` is
/// radius 64 chunks, and it is the same constant `end_islands`' own centre hole
/// uses.
const MAIN_ISLAND_CHUNKS_SQUARED: i64 = 4096;

/// Vanilla's own End biome source.
#[derive(Debug, Clone)]
pub struct EndBiomeSource {
    islands: EndIslandNoise,
    zoom_seed: i64,
}

impl EndBiomeSource {
    /// Builds the source for `seed`. Constructs its own [`EndIslandNoise`] because
    /// vanilla's `erosion` channel is `cache2d(endIslands(seed))` and nothing else
    /// feeds it.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            islands: EndIslandNoise::new(seed),
            zoom_seed: {
                let digest = Sha256::digest(seed.to_le_bytes());
                i64::from_le_bytes(digest[..8].try_into().expect("SHA-256 digest prefix"))
            },
        }
    }

    /// The five biomes this source can return, in `collectPossibleBiomes` order.
    #[must_use]
    pub fn possible_biomes() -> [&'static str; 5] {
        [
            THE_END,
            END_HIGHLANDS,
            END_MIDLANDS,
            SMALL_END_ISLANDS,
            END_BARRENS,
        ]
    }

    /// `getNoiseBiome(quartX, quartY, quartZ, sampler)`.
    ///
    /// `quart_y` is accepted and unused, exactly as in vanilla: it reaches the
    /// erosion sample's context and the `cache_2d(end_islands)` channel never reads
    /// a `y`. Keeping the parameter means a caller writes the same call it would
    /// write for a multi-noise dimension.
    #[must_use]
    pub fn biome_at_quart(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> &'static str {
        self.biome_at_quart_typed(quart_x, quart_y, quart_z).name()
    }

    /// Typed counterpart to [`Self::biome_at_quart`]. Worldgen placement
    /// filters use this identity directly so they never compare resource-name
    /// strings in the feature hot path.
    #[must_use]
    pub fn biome_at_quart_typed(&self, quart_x: i32, _quart_y: i32, quart_z: i32) -> BuiltinBiome {
        let block_x = quart_x * 4;
        let block_z = quart_z * 4;
        let chunk_x = block_x >> 4;
        let chunk_z = block_z >> 4;
        if i64::from(chunk_x) * i64::from(chunk_x) + i64::from(chunk_z) * i64::from(chunk_z)
            <= MAIN_ISLAND_CHUNKS_SQUARED
        {
            return BuiltinBiome::TheEnd;
        }
        // `weirdBlockX` — the chunk *centre*, not the quart's own position, so all
        // 16 quarts of a chunk share one sample.
        let weird_block_x = (chunk_x * 2 + 1) * 8;
        let weird_block_z = (chunk_z * 2 + 1) * 8;
        let height = self.islands.compute(weird_block_x, weird_block_z);
        if height > 0.25 {
            BuiltinBiome::EndHighlands
        } else if height >= -0.0625 {
            BuiltinBiome::EndMidlands
        } else if height < -0.21875 {
            BuiltinBiome::SmallEndIslands
        } else {
            BuiltinBiome::EndBarrens
        }
    }

    /// The 16 horizontal quart biomes of chunk `(cx, cz)`.
    ///
    /// Each quart is resolved independently. The main-island gate and erosion
    /// sample ultimately reduce to a chunk-centre answer outside threshold
    /// boundaries, but the packet grid still asks all sixteen quart positions.
    #[must_use]
    pub fn chunk_quarts(&self, cx: i32, cz: i32) -> [&'static str; 16] {
        std::array::from_fn(|i| {
            self.biome_at_quart(cx * 4 + (i % 4) as i32, 0, cz * 4 + (i / 4) as i32)
        })
    }

    /// The same chunk answers in their generated enum representation.
    #[must_use]
    pub fn chunk_quarts_typed(&self, cx: i32, cz: i32) -> [BuiltinBiome; 16] {
        std::array::from_fn(|i| {
            self.biome_at_quart_typed(cx * 4 + (i % 4) as i32, 0, cz * 4 + (i / 4) as i32)
        })
    }

    /// The block-position biome used by placement modifiers.
    ///
    /// Packet quarts store the raw source answer, while a block lookup first
    /// chooses one of the eight nearby quart corners using the world's
    /// seed-derived positional zoom. The End source is vertically invariant,
    /// but the corner choice still includes `y`.
    #[must_use]
    pub fn biome_at_block(&self, x: i32, y: i32, z: i32) -> &'static str {
        self.biome_at_block_typed(x, y, z).name()
    }

    /// Typed block-position biome used by placement modifiers.
    #[must_use]
    pub fn biome_at_block_typed(&self, x: i32, y: i32, z: i32) -> BuiltinBiome {
        crate::overworld::zoomed_biome_flat(
            self.zoom_seed,
            x,
            y,
            z,
            |source_x, source_z, local_qx, local_qz| {
                let qx = source_x * 4 + local_qx as i32;
                let qz = source_z * 4 + local_qz as i32;
                Some(self.biome_at_quart_typed(qx, 0, qz))
            },
        )
        .expect("End biome source is defined for every quart")
    }
}

/// One generated End chunk: the block column plus its 16 horizontal biome quarts.
///
/// All 16 quarts are always equal — the erosion sample is at the chunk centre, which
/// is vanilla's behaviour and not a simplification — and the array shape is kept so a
/// caller building a biome container writes the same loop it writes for the Nether.
#[derive(Debug, Clone)]
pub struct EndColumn {
    min_y: i32,
    /// Noise-generated terrain rows. The retained block window is
    /// [`WORLD_HEIGHT`] rows so structures above the noise ceiling survive.
    height: i32,
    world_height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Typed biome identity per horizontal quart, resolved to a resource name
    /// only by the explicit string accessors below.
    biome_quarts: [BiomeRef; 16],
    /// Client heightmaps retained when the feature stage starts.  The packet
    /// path must use these snapshots rather than rescanning the final served
    /// 3x3 decoration result: a neighbouring feature can write terrain into
    /// this column after its own client maps were primed.
    client_heightmaps: [[u16; 256]; 3],
    gateways: Vec<decorate::EndGateway>,
    /// Block-entity creation events emitted while structure blocks were placed.
    /// These are retained even when a later structure or feature overwrites the
    /// block, because the packet lifecycle observes the creation sidecar before
    /// the final block field is assembled.
    block_entity_events: Vec<EndBlockEntityEvent>,
}

/// One state-owned block-entity creation event from End structure placement.
///
/// This is deliberately an event rather than a final-state census: a later
/// structure write may replace the state while the generated chunk still owns
/// the entity record created by the earlier placement. Ender chests are filtered
/// by the producer because their records are player-owned rather than generated
/// packet sidecars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndBlockEntityEvent {
    /// Absolute block position of the state write.
    pub position: [i32; 3],
    /// Validated registry id of the block-entity type created by the state
    /// write. Textual NBT conversion belongs at the server boundary.
    pub type_id: BlockEntityType,
}

impl EndColumn {
    /// World Y of the lowest block row (0 for the End).
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of noise-generated terrain rows (128 for the End).
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Canonical block-state string at local `(lx, lz)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, lx: usize, y: i32, lz: usize) -> &str {
        let ly = y - self.min_y;
        if !(0..self.world_height).contains(&ly) {
            return "minecraft:air";
        }
        let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
        &self.palette[self.blocks[idx] as usize]
    }

    /// Typed biome identity at horizontal quart `(qx, qz)`, both in `0..4`.
    #[must_use]
    pub fn biome_at_quart_typed(&self, qx: usize, qz: usize) -> BiomeRef {
        self.biome_quarts[qz * 4 + qx]
    }

    /// Built-in biome name at horizontal quart `(qx, qz)`, both in `0..4`.
    /// This is a display/packet-boundary adapter; worldgen identity work
    /// should use [`Self::biome_at_quart_typed`].
    #[must_use]
    pub fn biome_at_quart(&self, qx: usize, qz: usize) -> &'static str {
        self.biome_at_quart_typed(qx, qz)
            .builtin_or_none()
            .expect("End output biome is not a generated built-in")
            .name()
    }

    /// Typed biome identity covering local column `(lx, lz)`.
    #[must_use]
    pub fn biome_at_typed(&self, lx: usize, lz: usize) -> BiomeRef {
        self.biome_at_quart_typed(lx >> 2, lz >> 2)
    }

    /// Built-in biome name covering local column `(lx, lz)`.
    #[must_use]
    pub fn biome_at(&self, lx: usize, lz: usize) -> &'static str {
        self.biome_at_typed(lx, lz)
            .builtin_or_none()
            .expect("End output biome is not a generated built-in")
            .name()
    }

    /// The End's motion-blocking heightmap for the final served block field.
    ///
    /// Structure and decoration writes are included. The client-visible End
    /// heightmaps are derived from the completed chunk, so retaining a
    /// pre-structure snapshot would disagree whenever a later writer adds a
    /// taller block to the served column.
    #[must_use]
    pub fn motion_blocking_heightmap(&self) -> &[u16; 256] {
        &self.client_heightmaps[1]
    }

    /// The retained client maps in registry-id order `WORLD_SURFACE` (1),
    /// `MOTION_BLOCKING` (4), and `MOTION_BLOCKING_NO_LEAVES` (5).
    #[must_use]
    pub fn client_heightmaps(&self) -> &[[u16; 256]; 3] {
        &self.client_heightmaps
    }

    /// Return-gateway exits generated in this column.  The gateway block is in
    /// the palette; this sidecar is the block-entity data a server must retain.
    #[must_use]
    pub fn gateways(&self) -> &[decorate::EndGateway] {
        &self.gateways
    }

    /// State-owned block-entity creation events captured before later writes.
    #[must_use]
    pub fn block_entity_events(&self) -> &[EndBlockEntityEvent] {
        &self.block_entity_events
    }

    /// Count of non-air blocks — the cheapest "did this actually generate terrain"
    /// question, and the one an empty-column bug fails.
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        let air = self
            .palette
            .iter()
            .position(|s| s == "minecraft:air")
            .map(|i| i as u16);
        match air {
            Some(air) => self.blocks.iter().filter(|&&b| b != air).count(),
            None => self.blocks.len(),
        }
    }

    /// The raw parts, for a caller building a chunk packet or a region file.
    #[must_use]
    pub fn into_raw(self) -> (i32, i32, Vec<String>, Vec<u16>, [BiomeRef; 16]) {
        (
            self.min_y,
            self.world_height,
            self.palette,
            self.blocks,
            self.biome_quarts,
        )
    }
}

/// A composed, reusable End generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
///
/// **Demand-ordered and order-independent.** No terrain stage reads a neighbouring
/// chunk's product — there is no carver here, so not even the Nether's 17×17
/// re-derivation — and the bounded structure-start memo only caches pure
/// `(seed, cx, cz)` values. Columns may therefore be requested in any order, on
/// any thread, without changing a byte; eviction or a concurrent cache miss can
/// only repeat that same pure calculation.
#[allow(missing_debug_implementations)]
pub struct EndGenerator {
    seed: i64,
    slot_count: usize,
    interner: Arc<StateInterner>,
    surface: SurfaceSystem,
    /// `noise_router.final_density`, compiled once. Cloning it per chunk is an `Arc`
    /// bump.
    final_density: Program,
    biomes: EndBiomeSource,
    min_y: i32,
    height: i32,
    sea_level: i32,
    cell_width: i32,
    cell_height: i32,
    default_block: String,
    default_block_pre: PreState,
    /// The dimension's `default_fluid` as a [`BlockKind`]. **`Air` is a real answer
    /// here, not a missing one** — the End has no fluid, and `sea_level 0` against
    /// `min_y 0` makes every `FluidStatus::at` return air anyway. A generator that
    /// "helpfully" defaulted this to water would flood the End below y 0, which is
    /// nowhere, and then look correct.
    default_fluid: BlockKind,
    default_fluid_pre: PreState,
    decoration: decorate::EndDecoration,
    veg_tags: crate::feature::vegetation::VegTags,
    structures: Option<crate::structure::StructureRegistry>,
    /// `(cx, cz)` → that chunk's complete and incomplete starts. End-city
    /// placement walks a 33×33 neighbourhood for each of the nine source
    /// chunks in a served column; this memo makes the pure start calculation
    /// run once per origin instead of once per walk entry.
    starts: EndStartsMemo,
    /// Exact immutable source worlds shared by overlapping spatial batches.
    /// Each entry is produced by the scalar `base_world` path once; caching it
    /// changes no density operation or palette order and turns a moving view's
    /// overlap into Arc bumps instead of repeated noise evaluation.
    base_worlds: EndBaseMemo,
}

/// Immutable End terrain prepared for one source coordinate in a spatial
/// batch. Structure pieces are already present; feature decoration is not.
/// Output columns calculate their heightmap after the three-by-three
/// decoration pass so the sidecar describes final served content.
#[derive(Debug, Clone)]
pub struct EndBaseWorld {
    world: DenseBlockGrid,
    block_entity_events: Vec<EndBlockEntityEvent>,
}

/// Keep a long-lived End generator bounded while retaining the complete
/// locality window for ordinary chunk sweeps. Eviction can only repeat a pure
/// start calculation; it cannot alter generated bytes or ordering.
const STARTS_MEMO_CEILING: usize = 8192;
const STARTS_MEMO_SHARDS: usize = 32;
/// The End dimension exposes sixteen 16-row sections. Terrain noise fills the
/// lower half, but End-city template pieces can extend into the upper half.
const WORLD_HEIGHT: i32 = 256;
/// A 32-chunk render distance needs a 67x67 immutable dependency square.
/// Retain that ordinary moving-view footprint, but not an unbounded journey.
const BASE_MEMO_CEILING: usize = 67 * 67;

struct EndStartsShard {
    entries: Mutex<
        FastMap<
            (i32, i32),
            Arc<OnceLock<Arc<Vec<Arc<crate::structure::StructureStart>>>>>,
        >,
    >,
}

impl Default for EndStartsShard {
    fn default() -> Self {
        Self {
            entries: Mutex::new(FastMap::default()),
        }
    }
}

struct EndStartsMemo {
    shards: [EndStartsShard; STARTS_MEMO_SHARDS],
}

struct EndBaseShard {
    entries: Mutex<FastMap<(i32, i32), Arc<OnceLock<Arc<EndBaseWorld>>>>>,
}

impl Default for EndBaseShard {
    fn default() -> Self {
        Self { entries: Mutex::new(FastMap::default()) }
    }
}

struct EndBaseMemo {
    shards: [EndBaseShard; STARTS_MEMO_SHARDS],
}

impl EndBaseMemo {
    fn new() -> Self {
        Self { shards: std::array::from_fn(|_| EndBaseShard::default()) }
    }

    fn get_or_compute(
        &self,
        key: (i32, i32),
        compute: impl FnOnce() -> EndBaseWorld,
    ) -> Arc<EndBaseWorld> {
        let shard = &self.shards[EndStartsMemo::shard(key)];
        let slot = {
            let mut entries = shard.entries.lock().expect("end base memo poisoned");
            let per_shard = BASE_MEMO_CEILING.div_ceil(STARTS_MEMO_SHARDS);
            if entries.len() >= per_shard {
                entries.retain(|_, slot| Arc::strong_count(slot) > 1);
            }
            Arc::clone(entries.entry(key).or_insert_with(|| Arc::new(OnceLock::new())))
        };
        Arc::clone(slot.get_or_init(|| Arc::new(compute())))
    }

    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.entries.lock().expect("end base memo poisoned").len())
            .sum()
    }

    fn get(&self, key: (i32, i32)) -> Option<Arc<EndBaseWorld>> {
        let shard = &self.shards[EndStartsMemo::shard(key)];
        let entries = shard.entries.lock().expect("end base memo poisoned");
        entries.get(&key).and_then(|slot| slot.get()).map(Arc::clone)
    }

    fn insert(&self, key: (i32, i32), value: Arc<EndBaseWorld>) -> Arc<EndBaseWorld> {
        let shard = &self.shards[EndStartsMemo::shard(key)];
        let mut entries = shard.entries.lock().expect("end base memo poisoned");
        let per_shard = BASE_MEMO_CEILING.div_ceil(STARTS_MEMO_SHARDS);
        if entries.len() >= per_shard {
            entries.retain(|_, slot| Arc::strong_count(slot) > 1);
        }
        let slot = entries
            .entry(key)
            .or_insert_with(|| Arc::new(OnceLock::new()));
        if let Some(existing) = slot.get() {
            Arc::clone(existing)
        } else {
            let _ = slot.set(Arc::clone(&value));
            value
        }
    }
}


impl EndStartsMemo {
    fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| EndStartsShard::default()),
        }
    }

    fn shard((cx, cz): (i32, i32)) -> usize {
        let x = (cx as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let z = (cz as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        ((x ^ z.rotate_left(32)) >> 59) as usize
    }

    fn get_or_compute(
        &self,
        key: (i32, i32),
        compute: impl FnOnce() -> Vec<Arc<crate::structure::StructureStart>>,
    ) -> Arc<Vec<Arc<crate::structure::StructureStart>>> {
        let shard = &self.shards[Self::shard(key)];
        let slot = {
            let mut entries = shard.entries.lock().expect("end starts memo poisoned");
            let per_shard = STARTS_MEMO_CEILING.div_ceil(STARTS_MEMO_SHARDS);
            if entries.len() >= per_shard {
                entries.retain(|_, slot| Arc::strong_count(slot) > 1);
            }
            Arc::clone(
                entries
                    .entry(key)
                    .or_insert_with(|| Arc::new(OnceLock::new())),
            )
        };
        Arc::clone(slot.get_or_init(|| Arc::new(compute())))
    }
}


impl Default for EndStartsMemo {
    fn default() -> Self {
        Self::new()
    }
}

impl EndGenerator {
    /// The named pass order consumed by this generator and its parity tools.
    #[must_use]
    pub const fn stage_schedule() -> &'static crate::stage_schedule::StageSchedule {
        &crate::stage_schedule::END
    }

    /// Builds the generator for `seed` from `noise_settings/end.json` and a
    /// [`Resolver`] carrying the End's density functions and noises.
    ///
    /// Takes **no** biome parameter table, unlike the Nether: the End's biome layout
    /// is vanilla's own End biome source, which serialises to an empty object, so there is
    /// nothing for a resolver to supply and nothing that can be misconfigured. That
    /// is also why there is no equivalent of the Nether's empty-table panic.
    ///
    /// # Panics
    /// Panics if the settings do not set `legacy_random_source: true`. Every noise
    /// value in the dimension is wrong under xoroshiro, and it would look like
    /// terrain.
    #[must_use]
    pub fn new(seed: i64, settings: &Value, resolver: &dyn Resolver) -> Self {
        let builder =
            Builder::with_algorithm(seed, crate::rng::Algorithm::from_settings(settings), resolver);
        assert!(
            builder.algorithm().is_legacy(),
            "noise_settings for the End must set legacy_random_source: true; \
             with xoroshiro every noise value in the dimension is wrong"
        );

        let router = &settings["noise_router"];
        let interner = Arc::new(StateInterner::new());
        let canon = identity_canon(settings);
        let final_density = Program::compile(
            &builder.build(&router["final_density"]).expect("bundled final_density density-function document"),
        );
        let surface = SurfaceSystem::new(settings, &builder, &canon, &interner);

        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(0) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(128) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(0) as i32;
        let (cell_width, cell_height) = crate::aquifer::cell_geometry(settings);

        let default_block = settings["default_block"]["Name"]
            .as_str()
            .unwrap_or("minecraft:end_stone")
            .to_string();
        let default_fluid = crate::aquifer::fluid_from_settings(settings);
        let default_block_pre = PreState::from_name(&interner, &default_block);
        // Read through the same `BlockKind` the fill will produce, so the two cannot
        // disagree about what "the fluid" is in a dimension whose fluid is air.
        let default_fluid_pre = PreState::from_name(
            &interner,
            match default_fluid {
                BlockKind::Air => "minecraft:air",
                BlockKind::Water => "minecraft:water[level=0]",
                BlockKind::Lava => "minecraft:lava[level=0]",
                BlockKind::Stone => panic!("default_fluid is not a fluid: Stone"),
            },
        );

        let slot_count = builder.slot_count();

        let possible_biomes = [THE_END, END_HIGHLANDS, END_MIDLANDS, SMALL_END_ISLANDS, END_BARRENS]
            .into_iter()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let registry = crate::structure::StructureRegistry::new_for_biomes(seed, resolver, Some(&possible_biomes));
        let structures = (!registry.is_empty()).then_some(registry);

        Self {
            seed,
            slot_count,
            interner,
            surface,
            final_density,
            biomes: EndBiomeSource::new(seed),
            min_y,
            height,
            sea_level,
            cell_width,
            cell_height,
            default_block,
            default_block_pre,
            default_fluid,
            default_fluid_pre,
            decoration: decorate::EndDecoration::from_resolver(resolver),
            veg_tags: crate::feature::vegetation::build_veg_tags(resolver),
            structures,
            starts: EndStartsMemo::new(),
            base_worlds: EndBaseMemo::new(),
        }
    }

    /// World Y of the lowest generated block row.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows generated per column.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Block-position biome used by feature placement modifiers.
    #[must_use]
    pub fn biome_at_block(&self, x: i32, y: i32, z: i32) -> &'static str {
        self.biomes.biome_at_block(x, y, z)
    }

    /// The generated column for chunk `(cx, cz)`.
    ///
    /// The End's served order: fill, biome, surface, materialise, structures,
    /// then decoration. No End biome names a carver.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> EndColumn {
        self.column_inner(cx, cz, None)
    }

    /// Generate a column while recording the typed stage keys actually
    /// admitted by the End executor. The trace is diagnostic only; the same
    /// scalar path and the normal `column` path share all generation code.
    #[must_use]
    pub fn column_with_trace(
        &self,
        cx: i32,
        cz: i32,
        trace: &mut Vec<crate::stage_schedule::StageKey>,
    ) -> EndColumn {
        self.column_inner(cx, cz, Some(trace))
    }

    fn column_inner(
        &self,
        cx: i32,
        cz: i32,
        trace: Option<&mut Vec<crate::stage_schedule::StageKey>>,
    ) -> EndColumn {
        // The immutable source is already retained through the shaped prefix.
        // Advancing the same typed executor over that prefix makes the
        // resumable path explicit without re-running cached work.
        let mut schedule = match trace {
            Some(trace) => Self::stage_schedule().executor_with_trace(trace),
            None => Self::stage_schedule().executor(),
        };
        for &stage in Self::stage_schedule().stages_for(crate::stage_schedule::GenerationTarget::Shaped) {
            schedule.enter(stage);
        }
        let (world, client_heightmaps, gateways) = schedule.run(
            crate::stage_schedule::ColumnStage::Features,
            || self.decoration_region(cx, cz),
        );
        let block_entity_events = self
            .base_world_for_batch(cx, cz)
            .block_entity_events
            .clone();
        let column = schedule.run(crate::stage_schedule::ColumnStage::Output, || {
            self.finish_column(cx, cz, world, client_heightmaps, gateways, block_entity_events)
        });
        schedule.finish();
        column
    }

    fn finish_column(
        &self,
        cx: i32,
        cz: i32,
        world: DenseBlockGrid,
        client_heightmaps: [[u16; 256]; 3],
        gateways: Vec<decorate::EndGateway>,
        block_entity_events: Vec<EndBlockEntityEvent>,
    ) -> EndColumn {
        let biome_quarts = self
            .biomes
            .chunk_quarts_typed(cx, cz)
            .map(BiomeRef::builtin);
        let (palette, blocks) = world.into_palette_and_blocks_box(
            cx * 16,
            self.min_y,
            cz * 16,
            16,
            WORLD_HEIGHT,
            16,
            self.interner.id_of("minecraft:air"),
        );
        EndColumn {
            min_y: self.min_y,
            height: self.height,
            world_height: WORLD_HEIGHT,
            palette,
            blocks,
            biome_quarts,
            client_heightmaps,
            gateways,
            block_entity_events,
        }
    }

    /// Generate an adjacent output batch while computing every immutable
    /// source in the union of its three-by-three dependency windows once.
    /// `generate_dependencies` may execute jobs on any worker pool; this method
    /// consumes its indexed results and always decorates/emits in `coords`
    /// order, so scheduling cannot change palette or packet order.
    #[must_use]
    pub fn columns_spatial_batch<F>(
        &self,
        coords: &[(i32, i32)],
        generate_dependencies: F,
    ) -> Vec<EndColumn>
    where
        F: FnOnce(Vec<(i32, i32)>, &Self) -> Vec<((i32, i32), Arc<EndBaseWorld>)>,
    {
        self.columns_spatial_batch_observed(coords, generate_dependencies, |_, _| {})
    }

    /// Diagnostic twin of [`Self::columns_spatial_batch`] that reports the
    /// unique dependency count after generation and the completed output count.
    /// The observer runs outside both hot loops and cannot affect content.
    #[must_use]
    pub fn columns_spatial_batch_observed<F, O>(
        &self,
        coords: &[(i32, i32)],
        generate_dependencies: F,
        mut observe: O,
    ) -> Vec<EndColumn>
    where
        F: FnOnce(Vec<(i32, i32)>, &Self) -> Vec<((i32, i32), Arc<EndBaseWorld>)>,
        O: FnMut(usize, usize),
    {
        let mut dependencies = coords
            .iter()
            .flat_map(|&(cx, cz)| {
                (-1..=1).flat_map(move |dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
            })
            .collect::<Vec<_>>();
        dependencies.sort_unstable();
        dependencies.dedup();
        let generated = generate_dependencies(dependencies.clone(), self);
        assert_eq!(generated.len(), dependencies.len(), "End batch dependency count changed");
        let base_worlds = generated.into_iter().collect::<BTreeMap<_, _>>();
        observe(base_worlds.len(), 0);
        let columns = coords
            .iter()
            .map(|&(cx, cz)| {
                let mut region = DenseBlockGrid::with_interner(
                    self.interner.clone(),
                    (cx - 1) * 16,
                    self.min_y,
                    (cz - 1) * 16,
                    48,
                    WORLD_HEIGHT,
                    48,
                    self.interner.id_of("minecraft:air"),
                );
                for source_x in cx - 1..=cx + 1 {
                    for source_z in cz - 1..=cz + 1 {
                        let source = &base_worlds[&(source_x, source_z)].world;
                        region.copy_box_from(
                            source,
                            source_x * 16,
                            self.min_y,
                            source_z * 16,
                            source_x * 16,
                            self.min_y,
                            source_z * 16,
                            16,
                            WORLD_HEIGHT,
                            16,
                        );
                    }
                }
                let mut schedule = Self::stage_schedule().executor_at(
                    Self::stage_schedule().shaped_boundary_index(),
                );
                let gateways = schedule.run(crate::stage_schedule::ColumnStage::Features, || {
                    self.decoration.apply_region(
                        self.seed,
                        cx,
                        cz,
                        &mut region,
                        |source_x, source_z| {
                            self.biomes.biome_at_quart_typed(source_x * 4, 0, source_z * 4)
                        },
                    )
                });
                let client_heightmaps =
                    Self::end_client_heightmaps(&region, cx, cz, self.min_y, WORLD_HEIGHT);
                let block_entity_events = base_worlds[&(cx, cz)].block_entity_events.clone();
                let column = schedule.run(crate::stage_schedule::ColumnStage::Output, || {
                    self.finish_column(
                        cx,
                        cz,
                        region,
                        client_heightmaps,
                        gateways,
                        block_entity_events,
                    )
                });
                schedule.finish();
                column
            })
            .collect::<Vec<_>>();
        observe(base_worlds.len(), columns.len());
        columns
    }

    /// Serial spatial-batch entry point. Native server callers use the same
    /// spatial seam with a rectangle-aware dispatcher wrapper; this form keeps
    /// wasm and tests free of a Rayon dependency.
    #[must_use]
    pub fn columns_batch(&self, coords: &[(i32, i32)]) -> Vec<EndColumn> {
        self.columns_spatial_batch(coords, |dependencies, generator| {
            let cached = dependencies
                .iter()
                .map(|&chunk| generator.base_worlds.get(chunk).map(|base| (chunk, base)))
                .collect::<Option<Vec<_>>>();
            cached.unwrap_or_else(|| generator.base_world_rectangle(&dependencies))
        })
    }

    pub fn base_world_for_batch(&self, cx: i32, cz: i32) -> Arc<EndBaseWorld> {
        self.base_worlds.get_or_compute((cx, cz), || {
            let (world, block_entity_events) = self.base_world(cx, cz);
            EndBaseWorld {
                world,
                block_entity_events,
            }
        })
    }

    /// Materialize one immutable, undecorated End source column.
    ///
    /// Lifecycle replay uses this as the resident value before FEATURES. It is
    /// the same cached fill/surface/structure prefix consumed by ordinary End
    /// batches, so the parity path cannot silently substitute a second terrain
    /// implementation.
    #[must_use]
    pub fn column_shaped(&self, cx: i32, cz: i32) -> EndColumn {
        let base = self.base_world_for_batch(cx, cz);
        let maps = Self::end_client_heightmaps(&base.world, cx, cz, self.min_y, WORLD_HEIGHT);
        self.finish_column(
            cx,
            cz,
            base.world.clone(),
            maps,
            Vec::new(),
            base.block_entity_events.clone(),
        )
    }

    /// Run exactly one End FEATURES source against its immutable neighbourhood
    /// plus final writes from earlier source completions.
    #[must_use]
    pub fn parity_source_decoration_with_overrides(
        &self,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> EndDecorationResult {
        self.parity_source_decoration_for_target_with_overrides(
            source_x, source_z, source_x, source_z, overrides,
        )
    }

    /// Run one source body in the target-owned three-by-three read window.
    ///
    /// The target chooses which immutable columns and earlier overrides are
    /// visible. The completing source still chooses the decoration seed and
    /// the source-relative biome set used to select globally ordered features.
    #[must_use]
    pub fn parity_source_decoration_for_target_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> EndDecorationResult {
        let mut world = self.parity_decoration_grid_for_target(target_x, target_z);
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        for (x, y, z, state) in overrides {
            if (min_x..min_x + size_x).contains(x)
                && (min_y..min_y + size_y).contains(y)
                && (min_z..min_z + size_z).contains(z)
            {
                world.set(*x, *y, *z, state);
            }
        }
        let before = world.clone();
        let source_biomes = decorate::EndBiomeSet::around_source(source_x, source_z, |cx, cz| {
            self.biomes.biome_at_quart_typed(cx * 4, 0, cz * 4)
        });
        let gateways = self.decoration.apply_source(
            self.seed,
            source_x,
            source_z,
            &mut world,
            source_biomes,
        );
        let mut spills = Vec::new();
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    if world.get_id(x, y, z) != before.get_id(x, y, z) {
                        spills.push(EndDecorationSpill {
                            source: (source_x, source_z),
                            position: (x, y, z),
                            state: world.get(x, y, z).to_owned(),
                        });
                    }
                }
            }
        }
        EndDecorationResult { spills, gateways }
    }

    fn parity_decoration_grid_for_target(
        &self,
        target_x: i32,
        target_z: i32,
    ) -> DenseBlockGrid {
        let mut world = DenseBlockGrid::with_interner(
            Arc::clone(&self.interner),
            (target_x - 1) * 16,
            self.min_y,
            (target_z - 1) * 16,
            48,
            WORLD_HEIGHT,
            48,
            self.interner.id_of("minecraft:air"),
        );
        for cx in target_x - 1..=target_x + 1 {
            for cz in target_z - 1..=target_z + 1 {
                let base = self.base_world_for_batch(cx, cz);
                world.copy_box_from(
                    &base.world,
                    cx * 16,
                    self.min_y,
                    cz * 16,
                    cx * 16,
                    self.min_y,
                    cz * 16,
                    16,
                    WORLD_HEIGHT,
                    16,
                );
            }
        }
        world
    }

    /// Returns a previously prepared immutable source world without starting
    /// generation. Native batch callers use this to select the rectangular
    /// shared-density path only when every dependency is already present.
    #[must_use]
    pub fn base_world_cache_get(&self, chunk: (i32, i32)) -> Option<Arc<EndBaseWorld>> {
        self.base_worlds.get(chunk)
    }

    /// Generate exact immutable bases for a complete rectangular chunk set
    /// through one shared density sampler. Query order remains the scalar
    /// column order (`z`, `x`, then `y`) for each coordinate; only the sampler's
    /// corner/flat caches survive across chunk boundaries.
    #[must_use]
    pub fn base_world_rectangle(
        &self,
        chunks: &[(i32, i32)],
    ) -> Vec<((i32, i32), Arc<EndBaseWorld>)> {
        let Some(&(first_x, first_z)) = chunks.first() else {
            return Vec::new();
        };
        let (min_cx, max_cx, min_cz, max_cz) = chunks.iter().skip(1).fold(
            (first_x, first_x, first_z, first_z),
            |(min_x, max_x, min_z, max_z), &(cx, cz)| {
                (min_x.min(cx), max_x.max(cx), min_z.min(cz), max_z.max(cz))
            },
        );
        let expected = usize::try_from(i64::from(max_cx - min_cx + 1) * i64::from(max_cz - min_cz + 1))
            .expect("End batch rectangle size");
        assert_eq!(chunks.len(), expected, "End density batch must be a complete rectangle");
        let aquifer = AquiferSystem::disabled_bounded(
            self.final_density.clone(),
            self.slot_count,
            self.sea_level,
            self.default_fluid,
            self.min_y,
            WORLD_HEIGHT,
            (min_cx * 16, max_cx * 16 + 15),
            (min_cz * 16, max_cz * 16 + 15),
            self.cell_width,
            self.cell_height,
        );
        chunks
            .iter()
            .map(|&(cx, cz)| {
                let mut schedule = Self::stage_schedule().executor();
                schedule.enter(crate::stage_schedule::ColumnStage::Fill);
                let field = crate::compose::fill_column(
                    &aquifer,
                    cx * 16,
                    cz * 16,
                    self.min_y,
                    self.height,
                    &crate::structure::beardifier::Beardifier::empty(),
                );
                let heights = crate::compose::solid_top_heights(
                    &field,
                    self.min_y,
                    self.height,
                    self.sea_level,
                );
                schedule.enter(crate::stage_schedule::ColumnStage::Biomes);
                let biome_quarts = self.biomes.chunk_quarts(cx, cz);
                schedule.enter(crate::stage_schedule::ColumnStage::Surface);
                let surface_diff =
                    self.surface_stage(&field, &heights, &biome_quarts, cx * 16, cz * 16);
                schedule.enter(crate::stage_schedule::ColumnStage::Materialize);
                let world = crate::compose::materialize_column(
                    &self.interner,
                    &field,
                    &surface_diff,
                    cx * 16,
                    cz * 16,
                    self.min_y,
                    self.height,
                    self.default_block_pre.state,
                    self.default_fluid_pre.state,
                );
                schedule.enter(crate::stage_schedule::ColumnStage::StructureStarts);
                let (world, block_entity_events) =
                    self.structure_place_stage(cx, cz, self.widen_world(world));
                schedule.enter(crate::stage_schedule::ColumnStage::StructurePlacement);
                schedule.finish_prefix(
                    Self::stage_schedule().shaped_boundary_index(),
                );
                let base = Arc::new(EndBaseWorld {
                    world,
                    block_entity_events,
                });
                let base = self.base_worlds.insert((cx, cz), base);
                ((cx, cz), base)
            })
            .collect()
    }

    /// Number of immutable base worlds retained by spatial batching.
    /// Diagnostics only; generated content never branches on this value.
    #[must_use]
    pub fn base_world_cache_len(&self) -> usize {
        self.base_worlds.len()
    }


    /// Complete structure starts whose origin is `(cx, cz)`.
    ///
    /// City placement uses this same start calculation before looking through
    /// nearby starts for pieces that touch a served chunk.
    #[must_use]
    fn structure_starts_stage(
        &self,
        cx: i32,
        cz: i32,
    ) -> Arc<Vec<Arc<crate::structure::StructureStart>>> {
        self.starts.get_or_compute((cx, cz), || match &self.structures {
            None => Vec::new(),
            Some(registry) => {
                let sampler = EndStartSampler::new(self);
                registry
                    .starts_at(cx, cz, &sampler)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            }
        })
    }

    pub fn structure_starts(
        &self,
        cx: i32,
        cz: i32,
    ) -> Vec<Arc<crate::structure::StructureStart>> {
        self.structure_starts_stage(cx, cz)
            .iter()
            .filter(|start| start.pieces_complete)
            .map(Arc::clone)
            .collect()
    }

    /// Complete structure starts whose horizontal boxes intersect `(cx, cz)`.
    ///
    /// The returned map is the save-facing structure-reference view: each
    /// structure id owns packed origin-chunk coordinates. The scan is kept
    /// separate from [`Self::structure_starts`] because a chunk can reference
    /// a city that started in a neighbouring chunk, while only the origin
    /// chunk persists the start itself.
    #[must_use]
    pub fn structure_references(&self, cx: i32, cz: i32) -> BTreeMap<String, Vec<i64>> {
        const REFERENCE_RADIUS: i32 = 8;

        if self.structures.is_none() {
            return BTreeMap::new();
        }
        let (min_x, min_z) = (cx * 16, cz * 16);
        let mut references = BTreeMap::new();
        for (start_x, start_z) in self.structure_origin_candidates(cx, cz, REFERENCE_RADIUS) {
            for start in self.structure_starts_stage(start_x, start_z).iter() {
                if !start.pieces_complete
                    || !start.bounding_box.intersects_xz(min_x, min_z, min_x + 15, min_z + 15)
                {
                    continue;
                }
                let packed = (i64::from(start_z as u32) << 32) | i64::from(start_x as u32);
                let entries = references
                    .entry(start.structure.clone())
                    .or_insert_with(Vec::new);
                if !entries.contains(&packed) {
                    entries.push(packed);
                }
            }
        }
        for entries in references.values_mut() {
            entries.sort_unstable();
        }
        references
    }

    fn base_world(&self, cx: i32, cz: i32) -> (DenseBlockGrid, Vec<EndBlockEntityEvent>) {
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut schedule = Self::stage_schedule().executor();
        schedule.enter(crate::stage_schedule::ColumnStage::Fill);
        let aquifer = self.build_fill(cx, cz);
        // `Beardifier::empty()` rather than an `Option`: it takes
        // `fill_column`'s no-addition loop, so the End's density is the interpolated
        // `final_density` untouched — not `final_density + 0.0`.
        let field = crate::compose::fill_column(
            &aquifer,
            base_x,
            base_z,
            self.min_y,
            self.height,
            &crate::structure::beardifier::Beardifier::empty(),
        );
        let heights =
            crate::compose::solid_top_heights(&field, self.min_y, self.height, self.sea_level);
        schedule.enter(crate::stage_schedule::ColumnStage::Biomes);
        let biome_quarts = self.biomes.chunk_quarts(cx, cz);
        schedule.enter(crate::stage_schedule::ColumnStage::Surface);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        schedule.enter(crate::stage_schedule::ColumnStage::Materialize);
        let world = crate::compose::materialize_column(
            &self.interner,
            &field,
            &surface_diff,
            base_x,
            base_z,
            self.min_y,
            self.height,
            self.default_block_pre.state,
            self.default_fluid_pre.state,
        );
        schedule.enter(crate::stage_schedule::ColumnStage::StructureStarts);
        let placed = self.structure_place_stage(cx, cz, self.widen_world(world));
        schedule.enter(crate::stage_schedule::ColumnStage::StructurePlacement);
        schedule.finish_prefix(
            Self::stage_schedule().shaped_boundary_index(),
        );
        placed
    }

    /// Expand the noise-generated terrain into the full End dimension window
    /// before placing template structures. The upper half is air until a city
    /// piece writes there; keeping it in the same grid preserves clipping and
    /// processor reads across the full served column.
    fn widen_world(&self, terrain: DenseBlockGrid) -> DenseBlockGrid {
        let base_x = terrain.bounds().0;
        let base_y = terrain.bounds().1;
        let base_z = terrain.bounds().2;
        let mut world = DenseBlockGrid::with_interner(
            Arc::clone(&self.interner),
            base_x,
            base_y,
            base_z,
            16,
            WORLD_HEIGHT,
            16,
            self.interner.id_of("minecraft:air"),
        );
        world.copy_box_from(
            &terrain,
            base_x,
            base_y,
            base_z,
            base_x,
            base_y,
            base_z,
            16,
            self.height,
            16,
        );
        world
    }

    /// Resolve the registry-owned block-entity type for one canonical state.
    /// Structure placement records this before a later write can replace the
    /// state, so the packet seam does not have to infer history from the final
    /// palette.
    fn block_entity_type_for_state(state: &str) -> Option<BlockEntityType> {
        let state = lodestone_data::block_states::state_id(state)
            .and_then(lodestone_data::block_states::StateId::new)?;
        lodestone_data::block_entity_types::block_entity_type(state)
    }

    /// Places every complete End-city piece intersecting this chunk after
    /// materialization and before the palette is extracted. City pieces have no
    /// terrain adaptation or later refinement, so a bounded start scan and the
    /// grid's normal write clipping are sufficient.
    fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        mut world: DenseBlockGrid,
    ) -> (DenseBlockGrid, Vec<EndBlockEntityEvent>) {
        const START_SCAN_RADIUS: i32 = 16;
        let Some(registry) = &self.structures else {
            return (world, Vec::new());
        };
        let (min_x, min_z) = (cx * 16, cz * 16);
        let mut block_entity_events = Vec::new();
        let mut feature_randoms: HashMap<
            String,
            crate::rng::WorldgenRandom<crate::rng::LegacyRandomSource>,
        > = HashMap::new();
        for (start_x, start_z) in self.structure_origin_candidates(cx, cz, START_SCAN_RADIUS) {
            for start in self.structure_starts_stage(start_x, start_z).iter() {
                if !start.pieces_complete {
                    continue;
                }
                let reference = crate::structure::jigsaw::reference_position(&start.pieces);
                for piece in &start.pieces {
                    if !piece.bounding_box.intersects_xz(min_x, min_z, min_x + 15, min_z + 15) {
                        continue;
                    }
                    if let Some(blocks) = &piece.blocks {
                        for block in blocks.iter() {
                            if let Some(type_id) = Self::block_entity_type_for_state(&block.state) {
                                if type_id != BlockEntityType::ENDER_CHEST
                                    && (min_x..min_x + 16).contains(&block.pos[0])
                                    && (min_z..min_z + 16).contains(&block.pos[2])
                                    && world.bounds().1 <= block.pos[1]
                                    && block.pos[1] < world.bounds().1 + world.bounds().4
                                {
                                    block_entity_events.push(EndBlockEntityEvent {
                                        position: block.pos,
                                        type_id,
                                    });
                                }
                            }
                            world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                        }
                    }
                    if let Some(placement) = &piece.placement {
                        let mut record_event = |position: [i32; 3], type_id: BlockEntityType| {
                            if type_id != BlockEntityType::ENDER_CHEST {
                                if block_entity_events
                                    .iter()
                                    .any(|event| event.position == position && event.type_id == type_id)
                                {
                                    return;
                                }
                                block_entity_events.push(EndBlockEntityEvent { position, type_id });
                            }
                        };
                        let origin = crate::structure::template::PlaceOrigin {
                            position: placement.position,
                            reference,
                            seed: registry.seed(),
                        };
                        placement.template.place_with_block_entity_events(
                            origin,
                            &placement.settings,
                            &mut world,
                            &mut record_event,
                        );
                        for extra in &piece.extra_placements {
                            let origin = crate::structure::template::PlaceOrigin {
                                position: extra.position,
                                reference,
                                seed: registry.seed(),
                            };
                            extra.template.place_with_block_entity_events(
                                origin,
                                &extra.settings,
                                &mut world,
                                &mut record_event,
                            );
                        }
                    }
                    match piece.refine.as_ref() {
                        Some(crate::structure::PieceRefinement::FeaturePlacements { placements }) => {
                            let Some((step, index)) = registry.feature_placement_key(&start.structure) else {
                                continue;
                            };
                            let random = feature_randoms.entry(start.structure.clone()).or_insert_with(|| {
                                let mut random = crate::rng::WorldgenRandom::new(
                                    crate::rng::LegacyRandomSource::new(0),
                                );
                                let decoration_seed = random.set_decoration_seed(
                                    registry.seed(),
                                    min_x,
                                    min_z,
                                );
                                random.set_feature_seed(decoration_seed, index as i32, step);
                                random
                            });
                            crate::structure::feature_placement::place_feature_pool_elements(
                                random,
                                registry.seed(),
                                placements,
                                &mut world,
                                &self.veg_tags,
                            );
                        }
                        Some(crate::structure::PieceRefinement::StrongholdBlocks { writes }) => {
                            crate::structure::stronghold::place_post_surface_blocks(&mut world, writes);
                        }
                        Some(crate::structure::PieceRefinement::BuriedTreasureChest)
                        | Some(crate::structure::PieceRefinement::NetherFossilDriedGhast { .. })
                        | Some(crate::structure::PieceRefinement::RuinedPortalTerrain { .. })
                        | Some(crate::structure::PieceRefinement::FortressPlacement { .. })
                        | None => {}
                    }
                }
            }
        }
        (world, block_entity_events)
    }

    fn structure_origin_candidates(&self, cx: i32, cz: i32, radius: i32) -> Vec<(i32, i32)> {
        let min_x = cx - radius;
        let max_x = cx + radius;
        let min_z = cz - radius;
        let max_z = cz + radius;
        let Some(registry) = &self.structures else {
            return Vec::new();
        };
        registry
            .random_spread_origins_in(min_x, max_x, min_z, max_z)
            .unwrap_or_else(|| {
                (min_x..=max_x)
                    .flat_map(|start_x| (min_z..=max_z).map(move |start_z| (start_x, start_z)))
                    .collect()
            })
    }

    fn decoration_region(
        &self,
        cx: i32,
        cz: i32,
    ) -> (DenseBlockGrid, [[u16; 256]; 3], Vec<decorate::EndGateway>) {
        let mut region = DenseBlockGrid::with_interner(
            self.interner.clone(),
            (cx - 1) * 16,
            self.min_y,
            (cz - 1) * 16,
            48,
            WORLD_HEIGHT,
            48,
            self.interner.id_of("minecraft:air"),
        );
        for source_x in cx - 1..=cx + 1 {
            for source_z in cz - 1..=cz + 1 {
                // Keep immutable terrain in the generator-scoped memo so adjacent
                // served columns reuse their overlapping 3x3 dependency window.
                // The world is still cloned into this request's private region
                // before decoration, so feature writes cannot leak between columns.
                let source = self.base_world_for_batch(source_x, source_z);
                region.copy_box_from(
                    &source.world,
                    source_x * 16,
                    self.min_y,
                    source_z * 16,
                    source_x * 16,
                    self.min_y,
                    source_z * 16,
                    16,
                    WORLD_HEIGHT,
                    16,
                );
            }
        }
        let client_heightmaps =
            Self::end_client_heightmaps(&region, cx, cz, self.min_y, WORLD_HEIGHT);
        let gateways = self.decoration.apply_region(
            self.seed,
            cx,
            cz,
            &mut region,
            |source_x, source_z| self.biomes.biome_at_quart_typed(source_x * 4, 0, source_z * 4),
        );
        (region, client_heightmaps, gateways)
    }

    /// Capture the three client maps from the centre chunk before features run.
    /// Decoration still writes the complete three-by-three result into the
    /// served block field, but those writes do not retroactively change the
    /// centre chunk's already-primed snapshots.
    fn end_client_heightmaps(
        region: &DenseBlockGrid,
        cx: i32,
        cz: i32,
        min_y: i32,
        height: i32,
    ) -> [[u16; 256]; 3] {
        let mut maps = [[0; 256]; 3];
        for z in 0..16i32 {
            for x in 0..16i32 {
                let index = (z * 16 + x) as usize;
                let mut remaining = 3u8;
                for local_y in (0..height).rev() {
                    if remaining == 0 {
                        break;
                    }
                    let Some(state) = region
                        .interner()
                        .canonical_id(region.get_id(cx * 16 + x, min_y + local_y, cz * 16 + z))
                    else {
                        continue;
                    };
                    let stored = (local_y + 1) as u16;
                    if maps[0][index] == 0 && state != lodestone_data::block_states::air_state() {
                        maps[0][index] = stored;
                        remaining -= 1;
                    }
                    let motion = lodestone_data::block_solidity::blocks_motion(state)
                        || lodestone_data::snow_support::has_fluid_state(state);
                    if maps[1][index] == 0 && motion {
                        maps[1][index] = stored;
                        remaining -= 1;
                    }
                    if maps[2][index] == 0
                        && motion
                        && !lodestone_data::tool::builtin_block_tag_contains(
                            "minecraft:leaves",
                            state.block(),
                        )
                    {
                        maps[2][index] = stored;
                        remaining -= 1;
                    }
                }
            }
        }
        maps
    }

    /// `Aquifer.createDisabled` bound to this chunk — the End's whole fill decision,
    /// and with `default_fluid` air it reduces to "solid where the interpolated
    /// density is positive, air everywhere else".
    fn build_fill(&self, cx: i32, cz: i32) -> AquiferSystem {
        AquiferSystem::disabled(
            self.final_density.clone(),
            self.slot_count,
            self.sea_level,
            self.default_fluid,
            self.min_y,
            self.height,
            cx,
            cz,
            self.cell_width,
            self.cell_height,
        )
    }


    /// The pre-surface shape field for `(cx, cz)`, as
    /// [`BlockKind`]s — the seam a gate drives the End's density through without
    /// paying for the surface pass or the palette.
    ///
    /// Index it with [`crate::compose::column_index`] rather than restating the
    /// layout.
    #[must_use]
    pub fn shape_field(&self, cx: i32, cz: i32) -> Vec<BlockKind> {
        let aquifer = self.build_fill(cx, cz);
        crate::compose::fill_column(
            &aquifer,
            cx * 16,
            cz * 16,
            self.min_y,
            self.height,
            &crate::structure::beardifier::Beardifier::empty(),
        )
    }

    /// `buildSurface`.
    ///
    /// **The End's surface rule is `{"type": "minecraft:block", "result_state":
    /// end_stone}` — unconditional, and therefore a no-op**, because
    /// `default_block` is already `end_stone` and vanilla's own scan only rewrites a
    /// position holding the default block. It is composed anyway rather than skipped:
    /// the rule is data, a datapack may replace it, and a generator that special-cased
    /// "the End has no surface rules" would be encoding today's `end.json` as an
    /// assumption. There is no bedrock here either — no `vertical_gradient` anywhere
    /// in the End's rule — which is correct and is the one place the Nether's shape
    /// would have been actively wrong.
    fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        biome_quarts: &[&'static str; 16],
        base_x: i32,
        base_z: i32,
    ) -> SurfaceDiff {
        // Re-derived rather than reasoned about: a wrong `PreClass` changes which
        // surface rules fire and still produces a plausible column.
        debug_assert_eq!(
            self.default_block_pre,
            PreState::from_name(&self.interner, &self.default_block),
        );

        let pre = |lx: i32, y: i32, lz: i32| -> PreState {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return PreState::AIR;
            }
            match field[crate::compose::column_index(lx, ly, lz, self.height)] {
                BlockKind::Stone => self.default_block_pre,
                BlockKind::Water | BlockKind::Lava => self.default_fluid_pre,
                BlockKind::Air => PreState::AIR,
            }
        };
        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        // Every End biome declares `temperature: 0.5`, so `cold_enough_to_snow` is
        // false, and nothing in the End's rule tree reads it — there is no
        // temperature condition to read it with.
        let biome_at =
            |lx: i32, _y: i32, lz: i32| -> (&str, bool) { (biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize], false) };

        self.surface
            .build_surface(&pre, &heightmap, &biome_at, base_x, base_z)
    }
}

/// A start probe over the End's pre-surface density output.
///
/// The city start check samples four columns. Caching its disabled aquifers
/// avoids recomputing a sampled chunk when more than one of those columns lands
/// in it, while keeping starts independent of generated-column ordering.
struct EndStartSampler<'a> {
    generator: &'a EndGenerator,
    aquifers: RefCell<HashMap<(i32, i32), Arc<AquiferSystem>>>,
}

impl<'a> EndStartSampler<'a> {
    fn new(generator: &'a EndGenerator) -> Self {
        Self { generator, aquifers: RefCell::new(HashMap::new()) }
    }

    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        let built = Arc::new(self.generator.build_fill(cx, cz));
        self.aquifers.borrow_mut().insert((cx, cz), Arc::clone(&built));
        built
    }
}

impl crate::structure::StartContext for EndStartSampler<'_> {
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: crate::structure::HeightmapKind) -> i32 {
        let generator = self.generator;
        let aquifer = self.aquifer(x.div_euclid(16), z.div_euclid(16));
        for ly in (0..generator.height).rev() {
            let y = generator.min_y + ly;
            let kind = aquifer.block_at(x, y, z);
            let matches = match heightmap {
                crate::structure::HeightmapKind::WorldSurfaceWg => kind != BlockKind::Air,
                crate::structure::HeightmapKind::OceanFloorWg => kind == BlockKind::Stone,
            };
            if matches {
                return y;
            }
        }
        generator.min_y - 1
    }

    fn biome_at_quart(&self, qx: i32, _qy: i32, qz: i32) -> String {
        self.generator.biomes.biome_at_quart(qx, 0, qz).to_owned()
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level
    }

    fn min_y(&self) -> i32 {
        self.generator.min_y
    }

    fn dimension_height(&self) -> i32 {
        self.generator.height
    }

    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self.aquifer(x.div_euclid(16), z.div_euclid(16)).block_at(x, y, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The main island covers exactly the chunks the density function's own centre
    /// hole covers, and nothing outside it. The expectation is the geometric
    /// predicate itself, evaluated independently of the branch under test.
    #[test]
    fn the_main_island_is_exactly_chunk_radius_64() {
        let source = EndBiomeSource::new(-195_764_831);
        let mut inside = 0usize;
        let mut outside = 0usize;
        for cx in -70..=70 {
            for cz in -70..=70 {
                let want_end = i64::from(cx) * i64::from(cx) + i64::from(cz) * i64::from(cz) <= 4096;
                let got = source.biome_at_quart(cx * 4, 0, cz * 4);
                if want_end {
                    assert_eq!(got, THE_END, "chunk ({cx},{cz}) is inside radius 64");
                    inside += 1;
                } else {
                    assert_ne!(got, THE_END, "chunk ({cx},{cz}) is outside radius 64");
                    outside += 1;
                }
            }
        }
        // Both arms must be exercised, or the equality above proves nothing.
        assert!(inside > 1_000 && outside > 1_000, "{inside} / {outside}");
    }

    /// All 16 quarts of a chunk agree, because the sample is at the chunk centre.
    /// A port that used the quart's own block position would fail this.
    #[test]
    fn every_quart_of_a_chunk_shares_the_chunk_centre_sample() {
        let source = EndBiomeSource::new(-195_764_831);
        for (cx, cz) in [(100, 100), (-137, 244), (65, 0), (-2000, 1500)] {
            let quarts = source.chunk_quarts(cx, cz);
            let typed = source.chunk_quarts_typed(cx, cz);
            assert!(
                quarts.iter().all(|b| *b == quarts[0]),
                "chunk ({cx},{cz}) is not uniform: {quarts:?}"
            );
            assert!(typed.iter().all(|b| *b == typed[0]));
            assert!(typed.iter().zip(quarts).all(|(typed, name)| typed.name() == name));
        }
    }

    /// Outside the main island all four outer biomes must actually be reachable.
    /// Without this the threshold ladder could be collapsed to one arm and every
    /// other test here would still pass — the island species of vacuous test,
    /// applied to a `match`.
    #[test]
    fn all_five_biomes_are_reachable() {
        let source = EndBiomeSource::new(-195_764_831);
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        seen.insert(source.biome_at_quart(0, 0, 0));
        for cx in (-400..400).step_by(11) {
            for cz in (-400..400).step_by(13) {
                seen.insert(source.biome_at_quart(cx * 4, 0, cz * 4));
            }
        }
        let mut expected: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        expected.extend(EndBiomeSource::possible_biomes());
        assert_eq!(seen, expected, "not every End biome is reachable");
    }

    /// The thresholds are read off the erosion value, so the mapping must agree with
    /// the ladder re-derived from the density function directly — which is the
    /// independent construction, not a restatement: it reads
    /// [`EndIslandNoise::compute`] at the same position and applies the four
    /// constants transcribed from vanilla's own End biome source by hand.
    #[test]
    fn the_threshold_ladder_matches_the_erosion_value() {
        let source = EndBiomeSource::new(42);
        let islands = EndIslandNoise::new(42);
        let mut counts = std::collections::BTreeMap::new();
        for cx in (65..400).step_by(3) {
            for cz in (-400..400).step_by(7) {
                let h = islands.compute((cx * 2 + 1) * 8, (cz * 2 + 1) * 8);
                let want = if h > 0.25 {
                    END_HIGHLANDS
                } else if h >= -0.0625 {
                    END_MIDLANDS
                } else if h < -0.21875 {
                    SMALL_END_ISLANDS
                } else {
                    END_BARRENS
                };
                assert_eq!(source.biome_at_quart(cx * 4, 0, cz * 4), want, "({cx},{cz})");
                *counts.entry(want).or_insert(0usize) += 1;
            }
        }
        assert_eq!(counts.len(), 4, "only {counts:?} of the four outer arms fired");
    }

    /// End biomes do not depend on `y`, for the same structural reason the Nether's
    /// do not: the channel is `cache_2d`.
    #[test]
    fn end_biomes_do_not_vary_with_y() {
        let source = EndBiomeSource::new(-195_764_831);
        for (qx, qz) in [(0, 0), (400, -900), (-3000, 3000)] {
            let at_zero = source.biome_at_quart(qx, 0, qz);
            for qy in [-16, 1, 8, 31] {
                assert_eq!(source.biome_at_quart(qx, qy, qz), at_zero);
            }
        }
    }

}
