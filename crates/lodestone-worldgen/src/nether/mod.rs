//! Composed Nether chunk generation — the second dimension this engine can
//! actually produce terrain for.
//!
//! # What it is
//!
//! The Nether counterpart of [`crate::overworld::OverworldGenerator`]: build one
//! per world seed from `noise_settings/nether.json` plus a [`Resolver`] carrying
//! the Nether's documents, then call [`NetherGenerator::column`] per chunk. It
//! runs vanilla's own stage order — fill, per-quart biome, surface rules,
//! carvers, mixed underground decoration and vegetal decoration — and holds no
//! version data.
//!
//! It is a **separate type rather than a generalised `OverworldGenerator`**
//! because three of the Overworld generator's stages have no Nether counterpart at
//! all (ore veins, `freeze_top_layer`, and the staged neighbour store that exists
//! to serve those drivers), and
//! four of its stages behave differently rather than merely being configured
//! differently. Sharing the type would have meant `if nether` inside the one file
//! this repo's own notes name as a choke point.
//!
//! # How it works, and what differs from the Overworld
//!
//! | | Overworld | Nether |
//! |---|---|---|
//! | terrain RNG family | xoroshiro | **legacy** (`legacy_random_source: true`) |
//! | decoration RNG family | xoroshiro | xoroshiro |
//! | climate channels | 6 real | temperature + vegetation; the other four are `0.0` constants in the router |
//! | biome noises | positional factory forked from a hash of the noise id | **`LegacyRandomSource(seed+0)` / `(seed+1)`**, legacy-init `NormalNoise` |
//! | aquifer | vanilla's own noise-based aquifer | **disabled** — global fluid picker only |
//! | fluid | water at y<63 | **lava at y<32** |
//! | vertical extent | `min_y -64`, height 384 | `min_y 0`, height 128 |
//! | carver | `cave`, `canyon` | **`nether_cave`** |
//! | bedrock | flag-gated | hardcoded floor y 0–4 *and* roof y 123–127, in the surface rules |
//!
//! Every one of those is data-driven except the carver type, and all of them are
//! reached through machinery that already existed — [`crate::rng::Algorithm`],
//! [`crate::aquifer::AquiferSystem::disabled`], [`crate::carver::CaveConfig::nether`].
//!
//! ## Biomes are two-dimensional here, and that is derived rather than assumed
//!
//! `noise_settings/nether.json`'s `temperature` and `vegetation` are
//! `shifted_noise` with **`y_scale: 0.0`** and `shift_y: 0.0`, so the `y` argument
//! to the underlying noise is the constant `0.0` at every position; the router's
//! other four climate channels are literal `0.0`. A Nether biome is therefore a
//! pure function of `(quartX, quartZ)`, and this generator samples it once per
//! horizontal quart instead of once per 4×4×4 cell. `nether_biomes_do_not_vary_with_y`
//! in `tests/nether_gen.rs` is the gate on that, and it is the reason
//! [`NetherColumn`] carries 16 biomes rather than 128 — **do not copy this shape
//! into a dimension whose climate has a real depth channel**: broadcasting a
//! biome vertically only stays correct because the Nether's does not have one.
//!
//! ## Structures are here, and they were the island this generator shipped with
//!
//! The first version of this file composed no structure stage at all — no starts,
//! no references, no beardifier, no place step. Every counter read healthy,
//! `bastion_remnant`'s pools loaded, its jigsaw assembly was gated, and it placed
//! **zero blocks anywhere in the game**: its biome tag is Nether-only, so the
//! Overworld's stage could never accept it, and this dimension had no stage to
//! accept it with. Four structures (`bastion_remnant`, `fortress`,
//! `nether_fossil`, `ruined_portal_nether`) sat in that position and the
//! unsupported ledger was silent about all four.
//!
//! What is composed now is the *dimension's* stage sequence, not new structure
//! machinery: [`crate::overworld::structures`]'s
//! [`StructureRefs`](crate::overworld::structures::StructureRefs) product,
//! [`crate::structure::beardifier`] and [`crate::structure::StructureRegistry`]
//! are all dimension-agnostic already, and this file supplies the four things they
//! need that *are* dimension-shaped:
//!
//! | need | the Nether's answer |
//! |---|---|
//! | which structure sets exist here | [`StructureRegistry::new_for_biomes`] over the parameter table's own biome names — vanilla's own "has biomes for structure set" check, so a Nether registry loads `bastion`'s pools and no village's |
//! | the height probe | a *disabled* aquifer over the Nether's `final_density`, `min_y 0`, height 128 — not an Overworld-shaped column, and never `sea_level 63` |
//! | the biome the filter reads | [`Self::biome_quarts`]' own sampler at `y = 0`, because this dimension's climate is y-invariant |
//! | memoisation of the 17×17 starts walk | a bounded pure-function memo on this generator, not the Overworld's staged store (which is keyed by that generator's own stage set) |
//!
//! **The bedrock roof and floor are surface-rule products and the height probe
//! does not see them.** `first_occupied_height` reads the *pre-surface* fill,
//! exactly as vanilla's own base-height lookup does, so a structure sited near y 127 is
//! sited against noise rather than against the roof it will be buried under. That
//! is vanilla's behaviour and not a gap; it is written down because the opposite
//! assumption is the natural one.
//!
//! ## Decoration uses the Nether's mixed step
//!
//! The bundled Nether biome documents carry a mixed feature list at step 7:
//! springs, fire, glowstone and mushrooms share raw indices with ore entries.
//! This module separates those two bodies while preserving every raw index and
//! seeding both with step 7; deleting the non-ore entries would shift every ore
//! stream. Step 9 is driven through the existing vegetation interpreter. The
//! terrain setting selects the legacy family for terrain construction, but the
//! decoration scheduler derives each feature stream through
//! `WorldgenRandom<XoroshiroRandomSource>` in every dimension. Do not reuse the
//! terrain carrier here: its two decoration-seed scale draws relocate every
//! feature even when the later placement body is otherwise identical.
//!
//! # How to change it
//!
//! * **The fill/surface/carve order is vanilla's and is load-bearing.** Carvers
//!   run over the *post-surface* column, so a carver that exposes netherrack sees
//!   the surface rules' output, not the raw fill. Structures are written *after*
//!   carving, which is where `surface_structures` (step 4) sits.
//! * **`min_gen_y + 31` in vanilla's own nether-world-carver carve-block step is not `sea_level`.**
//!   It is hardcoded, and at `min_y 0` it means "lava at y ≤ 31" — one below the
//!   `sea_level 32` the fill uses. Do not unify them.
//! * A biome name this generator can produce must have its carver list resolved
//!   at construction ([`Self::new`]'s `carvers_by_biome` walk), or its columns
//!   silently never carve.
//!
//! # Dependencies
//!
//! [`crate::aquifer`], [`crate::biome`], [`crate::carver`], [`crate::compose`],
//! [`crate::feature`], [`crate::surface`], [`crate::dense_grid`],
//! [`crate::interner`], and `lodestone-worldgen-core`'s density interpreter.
//! Nothing version-specific.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::Instant;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::biome::{BiomeTable, ClimateSampler};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::{Builder, Resolver};
use crate::engine::Program;
use crate::feature::{PlacedOre, PlacedScatteredOre, RuleTest};
use crate::interner::{StateId, StateInterner};
use crate::overworld::structures::{BEARD_REACH, REFS_RADIUS, StructureRefs};
use crate::structure::beardifier::Beardifier;
use crate::structure::{
    CodedLoot, HeightmapKind, PieceRefinement, StartContext, StructureRegistry, StructureStart,
};
use crate::surface::{PreState, SurfaceDiff, SurfaceSystem, identity_canon};

/// One generated Nether chunk: the block column plus its 16 horizontal biome
/// quarts.
///
/// Deliberately *not* [`crate::overworld::GeneratedColumn`]: that type carries
/// four products this dimension does not produce (a 4×4×4 biome grid, decoration
/// block entities, a `MOTION_BLOCKING` heightmap, `StageTimes`), and three of
/// them would have to be filled with plausible-looking stand-ins. A caller that
/// needs to serve this over the wire converts explicitly.
#[derive(Debug, Clone)]
pub struct NetherColumn {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` — the whole answer
    /// for this dimension, see the module doc's 2-D section.
    biome_quarts: [String; 16],
    placement_loot: Vec<CodedLoot>,
    /// Decoration writes in the dimension's upper 128 rows. The noise carrier
    /// remains 128 rows tall, but vegetation runs against the full 256-row
    /// resident window and may spill into the served chunk above that carrier.
    decoration_spills: Vec<(i32, i32, i32, String)>,
}

type PreDecorationResult = (
    Arc<crate::dense_grid::DenseBlockGrid>,
    [i32; 256],
    [String; 16],
    Vec<CodedLoot>,
);

type DecorationFeatures = Vec<(i32, usize, crate::feature::vegetation::PlacedRef)>;

const MEMO_SHARD_COUNT: usize = 32;

#[derive(Debug, Clone, Copy, Default)]
pub struct NetherCacheStats {
    pub lock_attempts: u64,
    pub lock_wait_nanos: u64,
    pub lock_hold_nanos: u64,
    pub slot_waits: u64,
    pub slot_wait_nanos: u64,
    pub computes: u64,
    pub compute_nanos: u64,
    pub evictions: u64,
}

#[derive(Debug, Default)]
struct MemoStats {
    lock_attempts: AtomicU64,
    lock_wait_nanos: AtomicU64,
    lock_hold_nanos: AtomicU64,
    slot_waits: AtomicU64,
    slot_wait_nanos: AtomicU64,
    computes: AtomicU64,
    compute_nanos: AtomicU64,
    evictions: AtomicU64,
}

impl MemoStats {
    fn read(&self) -> NetherCacheStats {
        NetherCacheStats {
            lock_attempts: self.lock_attempts.load(Ordering::Relaxed),
            lock_wait_nanos: self.lock_wait_nanos.load(Ordering::Relaxed),
            lock_hold_nanos: self.lock_hold_nanos.load(Ordering::Relaxed),
            slot_waits: self.slot_waits.load(Ordering::Relaxed),
            slot_wait_nanos: self.slot_wait_nanos.load(Ordering::Relaxed),
            computes: self.computes.load(Ordering::Relaxed),
            compute_nanos: self.compute_nanos.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    fn reset(&self) {
        for value in [
            &self.lock_attempts,
            &self.lock_wait_nanos,
            &self.lock_hold_nanos,
            &self.slot_waits,
            &self.slot_wait_nanos,
            &self.computes,
            &self.compute_nanos,
            &self.evictions,
        ] {
            value.store(0, Ordering::Relaxed);
        }
    }
}

struct MemoShard<T> {
    entries: Mutex<HashMap<(i32, i32), Arc<OnceLock<Arc<T>>>>>,
}

impl<T> Default for MemoShard<T> {
    fn default() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }
}

struct ShardedMemo<T> {
    shards: [MemoShard<T>; MEMO_SHARD_COUNT],
    capacity: AtomicUsize,
    stats: MemoStats,
}

impl<T> ShardedMemo<T> {
    fn new(capacity: usize) -> Self {
        Self {
            shards: std::array::from_fn(|_| MemoShard::default()),
            capacity: AtomicUsize::new(capacity),
            stats: MemoStats::default(),
        }
    }

    fn shard(key: (i32, i32)) -> usize {
        let x = (key.0 as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let z = (key.1 as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        ((x ^ z.rotate_left(32)) >> 59) as usize
    }

    fn slot(&self, key: (i32, i32)) -> Arc<OnceLock<Arc<T>>> {
        let profile = profile_enabled();
        let lock_started = profile.then(Instant::now);
        let mut entries = self.shards[Self::shard(key)]
            .entries
            .lock()
            .expect("nether memo shard poisoned");
        if let Some(started) = lock_started {
            self.stats.lock_attempts.fetch_add(1, Ordering::Relaxed);
            self.stats.lock_wait_nanos.fetch_add(
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
        }
        let hold_started = profile.then(Instant::now);
        if let Some(slot) = entries.get(&key) {
            let slot = Arc::clone(slot);
            if let Some(started) = hold_started {
                self.stats.lock_hold_nanos.fetch_add(
                    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    Ordering::Relaxed,
                );
            }
            return slot;
        }
        let per_shard = self
            .capacity
            .load(Ordering::Relaxed)
            .div_ceil(MEMO_SHARD_COUNT)
            .max(1);
        if entries.len() >= per_shard {
            let before = entries.len();
            entries.retain(|_, slot| Arc::strong_count(slot) > 1);
            if entries.len() != before {
                self.stats.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        let slot = Arc::clone(entries.entry(key).or_insert_with(|| Arc::new(OnceLock::new())));
        if let Some(started) = hold_started {
            self.stats.lock_hold_nanos.fetch_add(
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
        }
        slot
    }

    fn get_or_compute(&self, key: (i32, i32), compute: impl FnOnce() -> T) -> Arc<T> {
        let slot = self.slot(key);
        if let Some(value) = slot.get() {
            return Arc::clone(value);
        }
        let started = profile_enabled().then(Instant::now);
        let mut computed = false;
        let value = slot.get_or_init(|| {
            computed = true;
            let value = compute();
            if let Some(started) = started {
                self.stats.compute_nanos.fetch_add(
                    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    Ordering::Relaxed,
                );
            }
            self.stats.computes.fetch_add(1, Ordering::Relaxed);
            Arc::new(value)
        });
        if computed {
            return Arc::clone(value);
        }
        if let Some(started) = started {
            self.stats.slot_waits.fetch_add(1, Ordering::Relaxed);
            self.stats.slot_wait_nanos.fetch_add(
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
        }
        Arc::clone(value)
    }

    fn set_capacity(&self, capacity: usize) {
        self.capacity.store(capacity, Ordering::Relaxed);
        self.stats.reset();
    }

    fn clear(&self) {
        for shard in &self.shards {
            shard.entries.lock().expect("nether memo shard poisoned").clear();
        }
        self.stats.reset();
    }

    fn stats(&self) -> NetherCacheStats {
        self.stats.read()
    }
}

fn profile_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LODESTONE_NETHER_PROFILE").is_some())
}

/// A step-7 ore entry retains the body selected by its configured feature.
/// Standard and scattered entries share the raw index and target parser, but
/// intentionally dispatch to different placement bodies.
#[derive(Clone, Debug)]
enum NetherOre {
    Standard(PlacedOre),
    Scattered(PlacedScatteredOre),
}

impl NetherOre {
    fn index(&self) -> usize {
        match self {
            Self::Standard(ore) => ore.index,
            Self::Scattered(ore) => ore.index,
        }
    }

    fn registry_id(&self) -> Option<&str> {
        match self {
            Self::Standard(ore) => ore.registry_id.as_deref(),
            Self::Scattered(ore) => ore.registry_id.as_deref(),
        }
    }

    fn with_index(&self, index: usize) -> Self {
        match self {
            Self::Standard(ore) => {
                let mut ore = ore.clone();
                ore.index = index;
                Self::Standard(ore)
            }
            Self::Scattered(ore) => {
                let mut ore = ore.clone();
                ore.index = index;
                Self::Scattered(ore)
            }
        }
    }

    fn config(&self) -> &crate::feature::OreConfig {
        match self {
            Self::Standard(ore) => &ore.config,
            Self::Scattered(ore) => &ore.config,
        }
    }
}

/// The final block state one completed decoration source wrote in a target's
/// decoration region. The bounded parity materializer consumes these
/// transitions in its captured completion order; normal chunk generation
/// folds only the centre slice back into its returned [`NetherColumn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityDecorationSpill {
    /// Chunk whose raw decoration entries produced this write.
    pub source: (i32, i32),
    /// Absolute block coordinate and canonical final state after that source
    /// completed. Text is intentional at this public boundary: `StateId` is
    /// local to this generator's interner and cannot be transferred into the
    /// packet/source layer by its raw number.
    pub position: (i32, i32, i32),
    pub state: String,
}

impl NetherColumn {
    /// World Y of the lowest block row (0 for the Nether).
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows (128 for the Nether).
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Canonical block-state string at local `(lx, lz)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, lx: usize, y: i32, lz: usize) -> &str {
        let ly = y - self.min_y;
        if !(0..self.height).contains(&ly) {
            return "minecraft:air";
        }
        let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
        &self.palette[self.blocks[idx] as usize]
    }

    /// The biome at horizontal quart `(qx, qz)`, both in `0..4`.
    #[must_use]
    pub fn biome_at_quart(&self, qx: usize, qz: usize) -> &str {
        &self.biome_quarts[qz * 4 + qx]
    }

    /// Coded containers created by the receiving chunk's structure-placement
    /// pass, after neighbor-facing resolution and placement-stream seeding.
    #[must_use]
    pub fn placement_loot(&self) -> &[CodedLoot] {
        &self.placement_loot
    }

    /// Decoration writes in the served chunk's upper resident rows. Positions
    /// are absolute world coordinates; the server boundary filters them to its
    /// local column before applying them to the padded window.
    #[must_use]
    pub fn decoration_spills(&self) -> &[(i32, i32, i32, String)] {
        &self.decoration_spills
    }

    /// The biome covering local column `(lx, lz)`.
    #[must_use]
    pub fn biome_at(&self, lx: usize, lz: usize) -> &str {
        self.biome_at_quart(lx >> 2, lz >> 2)
    }

    /// Count of non-air blocks — the cheapest "did this actually generate
    /// terrain" question, and the one an empty-column bug fails.
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
    pub fn into_raw(self) -> (i32, i32, Vec<String>, Vec<u16>, [String; 16]) {
        (
            self.min_y,
            self.height,
            self.palette,
            self.blocks,
            self.biome_quarts,
        )
    }
}

/// A composed, reusable Nether generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
///
/// **Demand-ordered and order-independent.** Nothing here memoises across chunks
/// and no stage reads a neighbouring chunk's product except `applyCarvers`, which
/// re-derives its 17×17 neighbourhood from the seed alone — so `column` is a pure
/// function of `(seed, cx, cz)` and columns may be requested in any order, on any
/// thread, without changing a byte.
#[allow(missing_debug_implementations)]
pub struct NetherGenerator {
    seed: i64,
    slot_count: usize,
    interner: Arc<StateInterner>,
    surface: SurfaceSystem,
    /// `noise_router.final_density`, compiled once. Cloning it per chunk is an
    /// `Arc` bump.
    final_density: Program,
    climate: ClimateSampler,
    table: BiomeTable,
    min_y: i32,
    height: i32,
    sea_level: i32,
    cell_width: i32,
    cell_height: i32,
    default_block: String,
    default_fluid: String,
    default_block_pre: PreState,
    default_fluid_pre: PreState,
    /// `#minecraft:nether_carver_replaceables`. Empty when the resolver supplies
    /// no tag data, in which case carving is a harmless no-op — the same
    /// no-data-supplied convention every other stage here follows.
    carver_replaceable: HashSet<String>,
    carvers_by_biome: HashMap<String, Vec<CarverConfig>>,
    /// The globally ordered decoration catalog. Feature seeds use the index in
    /// this per-step order, not a biome document's local position; the source
    /// pass selects the union of its 3x3 section biomes from this catalog.
    decoration_catalog: crate::compose::DecorationCatalog,
    /// Ore bodies keyed by their placed-feature identity so the mixed
    /// dispatcher can recover standard and scattered entries from the global
    /// catalog while retaining the correct body-specific parser.
    ore_definitions: HashMap<String, NetherOre>,
    /// Biome membership for each placed feature's biome modifier.  Nether
    /// climate is y-invariant, so the mixed dispatcher can resolve an exact
    /// candidate position from the resident chunk's horizontal quart data.
    feature_biomes: Arc<HashMap<String, HashSet<String>>>,
    ore_tag_map: HashMap<String, HashSet<String>>,
    veg_tags: crate::feature::vegetation::VegTags,
    /// The Nether's structure engine, or `None` for a resolver that supplies no
    /// structure sets (every shape/surface fixture in this workspace). `None` makes
    /// every structure stage below an early return, so nothing distinguishes "no
    /// structure data" from "this generator before structures existed".
    structures: Option<StructureRegistry>,
    /// `(cx, cz)` → that chunk's starts. A memo of a **pure function** of
    /// `(seed, cx, cz)`, which is the only reason it may exist at all here: one
    /// `column` call walks the 17×17 [`REFS_RADIUS`] neighbourhood, and without
    /// this a bastion within reach is reassembled once per column that can see it.
    ///
    /// Deliberately *not* the Overworld's [`crate::overworld::store`]: that store's
    /// entry type is the Overworld's own stage set, and its retention ceiling is
    /// sized against a 37×37 pinned closure this dimension does not have.
    ///
    /// **Eviction cannot change a byte of output**, only cost — see
    /// [`STARTS_MEMO_CEILING`] for why this pure memo can use bounded sharded
    /// retention where the Overworld needed a view-pinned one.
    starts: ShardedMemo<Vec<Arc<StructureStart>>>,
    pre_decoration: ShardedMemo<PreDecorationResult>,
    /// Capacity of the pure pre-decoration memo. Production keeps the bounded
    /// sharded floor; an explicit lifecycle or packet replay raises retention
    /// only for the requested closure, so a bounded replay cannot trigger the
    /// same prefix repeatedly while its authenticated source order is replayed.
    /// Number of cache-miss computations, exposed for parity diagnostics.
    pre_decoration_computations: AtomicUsize,
}

/// Entries [`NetherGenerator::starts`] holds before it is cleared wholesale.
///
/// A 17×17 walk touches 289 chunks, so this is ~28 whole neighbourhoods — enough
/// that a sweep in any locality never evicts a chunk it is about to re-read, and
/// small enough that a long random-access session cannot grow without bound.
///
/// **Clearing the whole map is sound here and would not be in the Overworld.**
/// The Overworld's store holds *stage products* that later stages consume within
/// one `column` call, so dropping one mid-call would silently recompute a
/// neighbour's terrain (which is why that store is view-pinned and counts its
/// evictions). This map holds only starts, each a pure function of
/// `(seed, cx, cz)`: a miss costs a recomputation and returns the identical value,
/// so the worst an eviction can do is make a column slower.
const STARTS_MEMO_CEILING: usize = 8192;
/// The ruined-portal terrain pass can grow fourteen blocks beyond the frame,
/// so references used for placement must reach farther than the beardifier.
const PORTAL_TERRAIN_REACH: i32 = 14;
/// A thousand full pre/post fields cover the ordinary decorated join convoy's
/// 5×5 neighbour closure without demand-order thrashing. The memo is sharded
/// and each entry is once-initialized, so this larger ceiling removes repeated
/// work without putting a global lock across generation. Lifecycle replay uses
/// a separate capacity derived from its admitted closure.
const DECORATION_MEMO_CEILING: usize = 1024;
/// The Nether dimension exposes two 128-row halves. Noise fills the lower
/// half; vegetation placement still runs against the full 256-row dimension
/// window, where `MOTION_BLOCKING` can return the first air row above the roof.
const DECORATION_WINDOW_HEIGHT: i32 = 256;

fn pre_decoration_capacity(admissions: &[(i32, i32)], radius: i32) -> usize {
    let Some(&(first_x, first_z)) = admissions.first() else {
        return DECORATION_MEMO_CEILING;
    };
    let (min_x, max_x, min_z, max_z) = admissions.iter().skip(1).fold(
        (first_x, first_x, first_z, first_z),
        |(min_x, max_x, min_z, max_z), &(x, z)| {
            (min_x.min(x), max_x.max(x), min_z.min(z), max_z.max(z))
        },
    );
    let width: usize = (i64::from(max_x) - i64::from(min_x) + 1 + i64::from(radius) * 2)
        .try_into()
        .expect("immutable-stage x closure must fit usize");
    let height: usize = (i64::from(max_z) - i64::from(min_z) + 1 + i64::from(radius) * 2)
        .try_into()
        .expect("immutable-stage z closure must fit usize");
    width
        .checked_mul(height)
        .expect("immutable-stage closure must fit usize")
}

#[cfg(test)]
fn lifecycle_pre_decoration_capacity(admissions: &[(i32, i32)]) -> usize {
    pre_decoration_capacity(admissions, crate::feature::region_view::WIDE_RADIUS)
}

/// Re-express a generated-column height anchor for the wider resident window.
/// The placement context has two independent vertical extents: the noise
/// generator contributes 128 rows, while the receiving Nether dimension has
/// 256 rows.  A `BelowTop` anchor is relative to the former, so moving the
/// placement grid to the latter requires adding the extra rows to its offset;
/// absolute and `AboveBottom` anchors do not change.
fn nether_window_anchor(
    anchor: crate::feature::VerticalAnchor,
    generated_height: i32,
) -> crate::feature::VerticalAnchor {
    let extra_rows = DECORATION_WINDOW_HEIGHT.saturating_sub(generated_height);
    match anchor {
        crate::feature::VerticalAnchor::BelowTop(offset) => {
            crate::feature::VerticalAnchor::BelowTop(offset.saturating_add(extra_rows))
        }
        other => other,
    }
}

fn nether_window_height_provider(
    provider: crate::feature::HeightProvider,
    generated_height: i32,
) -> crate::feature::HeightProvider {
    match provider {
        crate::feature::HeightProvider::Uniform { min, max } => {
            crate::feature::HeightProvider::Uniform {
                min: nether_window_anchor(min, generated_height),
                max: nether_window_anchor(max, generated_height),
            }
        }
        crate::feature::HeightProvider::Trapezoid { min, max, plateau } => {
            crate::feature::HeightProvider::Trapezoid {
                min: nether_window_anchor(min, generated_height),
                max: nether_window_anchor(max, generated_height),
                plateau,
            }
        }
        crate::feature::HeightProvider::VeryBiasedToBottom { min, max, inner } => {
            crate::feature::HeightProvider::VeryBiasedToBottom {
                min: nether_window_anchor(min, generated_height),
                max: nether_window_anchor(max, generated_height),
                inner,
            }
        }
    }
}

/// Clones one parsed placed-feature tree with its explicit height ranges bound
/// to the noise-generated depth, while leaving the same feature ids, modifier
/// order and random stream visible to the dispatcher.  Selectors and the two
/// configured-feature bodies that carry nested placed refs are walked as well;
/// otherwise a branch selected after the window conversion could silently use
/// a different top anchor than its parent.
fn nether_window_placed_ref(
    placed: &crate::feature::vegetation::PlacedRef,
    generated_height: i32,
) -> crate::feature::vegetation::PlacedRef {
    let mut placed = placed.clone();
    for modifier in &mut placed.placements {
        if let crate::feature::vegetation::VegPlacement::HeightRange(provider) = modifier {
            *provider = nether_window_height_provider(*provider, generated_height);
        }
    }
    nether_window_configured_feature(&mut placed.feature, generated_height);
    placed
}

fn nether_window_configured_feature(
    feature: &mut crate::feature::vegetation::ConfiguredFeature,
    generated_height: i32,
) {
    use crate::feature::vegetation::ConfiguredFeature;

    match feature {
        ConfiguredFeature::RootSystem(config) => {
            config.feature = nether_window_placed_ref(&config.feature, generated_height);
        }
        ConfiguredFeature::RandomSelector { default, options } => {
            **default = nether_window_placed_ref(default, generated_height);
            for (_, option) in options {
                *option = nether_window_placed_ref(option, generated_height);
            }
        }
        ConfiguredFeature::SimpleRandomSelector(options)
        | ConfiguredFeature::Sequence(options) => {
            for option in options {
                *option = nether_window_placed_ref(option, generated_height);
            }
        }
        ConfiguredFeature::VegetationPatch(config) => {
            config.vegetation_feature =
                nether_window_placed_ref(&config.vegetation_feature, generated_height);
        }
        ConfiguredFeature::RandomBooleanSelector { yes, no } => {
            **yes = nether_window_placed_ref(yes, generated_height);
            **no = nether_window_placed_ref(no, generated_height);
        }
        ConfiguredFeature::WeightedRandomSelector(options) => {
            for (_, option) in options {
                *option = nether_window_placed_ref(option, generated_height);
            }
        }
        _ => {}
    }
}

/// The feature scheduler's carrier source. Terrain construction in this
/// dimension uses the legacy family, but per-chunk decoration seeds are derived
/// through xoroshiro.
fn decoration_random() -> crate::rng::WorldgenRandom<crate::rng::XoroshiroRandomSource> {
    crate::rng::WorldgenRandom::new(crate::rng::XoroshiroRandomSource::new(0))
}

/// Seed and distance helpers for the block-biome lookup used by the placement
/// biome modifier.  Nether biome values do not vary with Y, but the zoom choice
/// still uses the full block position, including its Y corner coordinate.
fn nether_zoom_seed(seed: i64) -> i64 {
    let digest = Sha256::digest(seed.to_le_bytes());
    i64::from_le_bytes(digest[..8].try_into().expect("SHA-256 digest prefix"))
}

fn next_nether_zoom_random(value: i64, addend: i64) -> i64 {
    value
        .wrapping_mul(
            value
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407),
        )
        .wrapping_add(addend)
}

fn nether_zoom_fiddle(value: i64) -> f64 {
    let uniform = (value >> 24).rem_euclid(1024) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}

fn nether_fiddled_distance(
    seed: i64,
    x: i32,
    y: i32,
    z: i32,
    dx: f64,
    dy: f64,
    dz: f64,
) -> f64 {
    let mut value = seed;
    for coordinate in [x, y, z, x, y, z] {
        value = next_nether_zoom_random(value, i64::from(coordinate));
    }
    let fx = nether_zoom_fiddle(value);
    value = next_nether_zoom_random(value, seed);
    let fy = nether_zoom_fiddle(value);
    value = next_nether_zoom_random(value, seed);
    let fz = nether_zoom_fiddle(value);
    let x = dx + fx;
    let y = dy + fy;
    let z = dz + fz;
    x * x + y * y + z * z
}

/// Expands one Nether chunk's horizontal biome answer into the 3-D cell shape
/// the vegetation placement gate consumes.  The Nether climate is invariant in
/// Y, including the widened resident decoration window, so every vertical
/// quart repeats its source chunk's horizontal value.
fn nether_biome_cells(
    biome_quarts: &[String; 16],
    min_y: i32,
    height: i32,
) -> crate::overworld::BiomeCells {
    crate::overworld::BiomeCells::from_fn(min_y, height, |qx, _, qz| {
        biome_quarts[qz * 4 + qx].clone()
    })
}

/// Which representation produced the entry that just completed.
#[derive(Clone, Copy)]
enum MixedEntryWriter {
    Decoration,
    Ore,
}

#[derive(Default)]
struct MixedSync {
    projected: usize,
    retained_outside: usize,
}

/// Makes the entry that just finished visible through the other placement
/// adapter before another raw feature entry begins. Ore placement reads a
/// bounded `RegionView`, while the decoration interpreter keeps a padded
/// sparse grid for feature-local height and neighbour queries; neither is an
/// authoritative overlay on its own. This is the sole bridge between them.
///
/// The bridge transfers each entry's final value once per cell. Intermediate
/// writes inside an entry are not observable until that entry returns, and
/// replaying them would falsely make duplicate writes part of the next entry's
/// state. The padded decoration footprint deliberately retains spill beyond
/// the ore reader's 3×3 window; only the representable intersection is
/// projected, and every coordinate in that intersection must land.
#[cfg(test)]
fn synchronize_mixed_entry(
    writer: MixedEntryWriter,
    grid: &mut crate::feature::vegetation::VegGrid,
    ore_view: &mut crate::feature::region_view::RegionView<'_>,
    centre_x: i32,
    centre_z: i32,
    terrain_min_y: i32,
    terrain_height: i32,
    grid_cursor: &mut usize,
    ore_cursor: &mut usize,
    ore_transferred: &mut HashMap<(i32, i32, i32), StateId>,
) -> MixedSync {
    let mut changed = Vec::new();
    synchronize_mixed_entry_reusing(
        writer,
        grid,
        ore_view,
        centre_x,
        centre_z,
        terrain_min_y,
        terrain_height,
        grid_cursor,
        ore_cursor,
        ore_transferred,
        &mut changed,
    )
}

fn synchronize_mixed_entry_reusing(
    writer: MixedEntryWriter,
    grid: &mut crate::feature::vegetation::VegGrid,
    ore_view: &mut crate::feature::region_view::RegionView<'_>,
    centre_x: i32,
    centre_z: i32,
    terrain_min_y: i32,
    terrain_height: i32,
    grid_cursor: &mut usize,
    ore_cursor: &mut usize,
    ore_transferred: &mut HashMap<(i32, i32, i32), StateId>,
    changed: &mut Vec<(i32, i32, i32, StateId)>,
) -> MixedSync {
    match writer {
        MixedEntryWriter::Decoration => {
            let end = grid.dirty_len();
            let mut final_cells = BTreeMap::new();
            for (x, y, z, state) in grid.dirty_cell_ids().skip(*grid_cursor) {
                final_cells.insert((x, y, z), state);
            }
            *grid_cursor = end;
            let mut sync = MixedSync::default();
            for ((x, y, z), state) in &final_cells {
                let lx = x - centre_x * 16;
                let lz = z - centre_z * 16;
                if !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lx)
                    || !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lz)
                {
                    sync.retained_outside += 1;
                    continue;
                }
                if ore_view.set_id(lx, *y, lz, *state) {
                    ore_transferred.insert((*x, *y, *z), *state);
                    sync.projected += 1;
                } else {
                    assert!(
                        !(terrain_min_y..terrain_min_y + terrain_height).contains(y),
                        "mixed decoration entry dropped an in-region write at ({x},{y},{z})"
                    );
                    // The decoration grid covers the receiving dimension's
                    // full vertical window, while the ore adapter is only the
                    // canonical noise-generated terrain height.  Keep an
                    // upper-half write in the decoration spill stream; it
                    // cannot participate in an ore read at this step.
                    sync.retained_outside += 1;
                }
            }
            sync
        }
        MixedEntryWriter::Ore => {
            changed.clear();
            let end = ore_view.write_log_len();
            ore_view.with_write_log_since_scan_order(*ore_cursor, |writes| {
                for &(lx, y, lz, state) in writes {
                    let x = centre_x * 16 + lx;
                    let z = centre_z * 16 + lz;
                    if ore_transferred.insert((x, y, z), state) != Some(state) {
                        changed.push((x, y, z, state));
                    }
                }
            });
            *ore_cursor = end;
            for (x, y, z, state) in changed.iter() {
                assert!(
                    grid.set_id_if_in_bounds(*x, *y, *z, *state),
                    "mixed ore entry wrote outside the decoration footprint at ({x},{y},{z})"
                );
            }
            *grid_cursor = grid.dirty_len();
            MixedSync {
                projected: changed.len(),
                retained_outside: 0,
            }
        }
    }
}

/// Splits the Nether's deliberately mixed step 7 without changing the raw
/// `(step, index)` identity either engine seeds from. Step 9 contains the
/// usual vegetal pass. This is dimension-specific data interpretation: the
/// Overworld's step-6 ore helper must not be taught that every data pack has
/// the Nether's mixed layout.
fn build_nether_feature_lists(
    resolver: &dyn Resolver,
    biome: &str,
) -> (Vec<NetherOre>, DecorationFeatures) {
    let document = resolver.biome_document(biome);
    let Some(steps) = document.get("features").and_then(Value::as_array) else {
        return (Vec::new(), Vec::new());
    };
    let mut ores = Vec::new();
    let mut decoration = Vec::new();
    for &step in &[4_i32, 7, crate::feature::STEP_VEGETAL_DECORATION] {
        let Some(entries) = steps.get(step as usize).and_then(Value::as_array) else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let Some(placed_id) = entry.as_str() else {
                continue;
            };
            let placed = resolver.placed_feature(placed_id);
            if placed.is_null() {
                continue;
            }
            let configured = placed
                .get("feature")
                .and_then(Value::as_str)
                .map(|id| resolver.configured_feature(id));
            let configured_type = configured
                .as_ref()
                .and_then(|feature| feature.get("type"))
                .and_then(Value::as_str);
            if step == 7
                && matches!(configured_type, Some("minecraft:ore" | "minecraft:scattered_ore"))
            {
                let configured = configured.as_ref().expect("checked above");
                let config = crate::feature::parse_ore_config(&configured["config"]);
                let placements = crate::feature::parse_placements(&placed);
                if configured_type == Some("minecraft:scattered_ore") {
                    ores.push(NetherOre::Scattered(PlacedScatteredOre {
                        registry_id: Some(placed_id.to_owned()),
                        index,
                        placements,
                        config,
                    }));
                } else {
                    ores.push(NetherOre::Standard(PlacedOre {
                        registry_id: Some(placed_id.to_owned()),
                        index,
                        placements,
                        config,
                    }));
                }
            } else {
                decoration.push((
                    step,
                    index,
                    crate::feature::vegetation::resolve_placed_feature_ref(resolver, entry),
                ));
            }
        }
    }
    (ores, decoration)
}

/// Resolves every target tag used by either step-7 ore body. The shared
/// Overworld helper accepts only standard entries, while Nether scattered ore
/// has its own target tag and must be included in the same membership closure.
fn build_nether_ore_tag_map(
    resolver: &dyn Resolver,
    ores: &[NetherOre],
) -> HashMap<String, HashSet<String>> {
    let mut map = HashMap::new();
    for ore in ores {
        for target in &ore.config().targets {
            if let RuleTest::TagMatch(tag) = &target.target {
                map.entry(tag.clone()).or_insert_with(|| {
                    let mut members = HashSet::new();
                    let mut seen = HashSet::new();
                    crate::compose::resolve_block_tag(resolver, tag, &mut members, &mut seen);
                    members
                });
            }
        }
    }
    map
}

impl NetherGenerator {
    /// Builds the generator for `seed` from `noise_settings/nether.json` and a
    /// [`Resolver`] carrying the Nether's density functions, noises, biome
    /// parameter table, biome documents and configured carvers.
    ///
    /// # Panics
    /// Panics if the resolver's `biome_parameters()` is empty. Unlike the
    /// Overworld generator there is **no fixed-biome fallback**: temperature and
    /// vegetation are the entire Nether biome layout, so a Nether without its
    /// 5-row parameter table is not a degraded world, it is a misconfigured one,
    /// and falling back would produce a uniform `nether_wastes` that looks
    /// plausible in a screenshot.
    #[must_use]
    pub fn new(seed: i64, settings: &Value, resolver: &dyn Resolver) -> Self {
        // The whole point of this phase: the family comes from the document.
        let builder =
            Builder::with_algorithm(seed, crate::rng::Algorithm::from_settings(settings), resolver);
        assert!(
            builder.algorithm().is_legacy(),
            "noise_settings for the Nether must set legacy_random_source: true; \
             with xoroshiro every noise value in the dimension is wrong"
        );

        let router = &settings["noise_router"];
        let interner = Arc::new(StateInterner::new());
        let canon = identity_canon(settings);
        let final_density = Program::compile(
            &builder.build(&router["final_density"]).expect("bundled final_density density-function document"),
        );
        let surface = SurfaceSystem::new(settings, &builder, &canon, &interner);
        let climate = ClimateSampler::new(settings, &builder);

        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(0) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(128) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(32) as i32;
        let (cell_width, cell_height) = crate::aquifer::cell_geometry(settings);

        let default_block = settings["default_block"]["Name"]
            .as_str()
            .unwrap_or("minecraft:netherrack")
            .to_string();
        // The Nether's `default_fluid` carries `{"level": "0"}`, and reading only
        // `Name` would produce `minecraft:lava` where the carver writes
        // `minecraft:lava[level=0]` — two palette entries for one state, and every
        // downstream match on the full string missing for the bare form.
        let default_fluid =
            canonical_state_from_settings(&settings["default_fluid"], "minecraft:lava[level=0]");
        let default_block_pre = PreState::from_name(&interner, &default_block);
        let default_fluid_pre = PreState::from_name(&interner, &default_fluid);

        let raw_table = crate::biome::parse_table(&resolver.biome_parameters());
        assert!(
            !raw_table.is_empty(),
            "the Nether needs its multi-noise parameter table (biome_parameters/nether)"
        );
        let table = BiomeTable::new(raw_table);

        let mut carver_replaceable = HashSet::new();
        {
            let mut seen = HashSet::new();
            crate::compose::resolve_block_tag(
                resolver,
                "minecraft:nether_carver_replaceables",
                &mut carver_replaceable,
                &mut seen,
            );
        }

        let mut carvers_by_biome = HashMap::new();
        let mut ores_by_biome = HashMap::new();
        // Vanilla's own multi-noise biome source's "possible biomes" for this dimension, derived from
        // the parameter table rather than written down: a hardcoded list of the
        // Nether's five would be a second copy of the data, and a datapack that
        // added a sixth would silently lose its structures.
        let mut possible_biomes: HashSet<String> = HashSet::new();
        for point in table.iter() {
            possible_biomes.insert(point.biome.clone());
            carvers_by_biome
                .entry(point.biome.clone())
                .or_insert_with(|| crate::compose::build_biome_carvers(resolver, &point.biome));
            let (ores, _) = build_nether_feature_lists(resolver, &point.biome);
            ores_by_biome.entry(point.biome.clone()).or_insert(ores);
        }
        let mut biome_source_order = Vec::new();
        for point in table.iter() {
            if !biome_source_order.contains(&point.biome) {
                biome_source_order.push(point.biome.clone());
            }
        }
        let decoration_catalog =
            crate::compose::build_decoration_catalog(resolver, &biome_source_order);
        let feature_biomes = decoration_catalog.feature_biomes();
        let mut ore_definitions = HashMap::new();
        for ore in ores_by_biome.values().flatten() {
            if let Some(id) = ore.registry_id() {
                ore_definitions.entry(id.to_owned()).or_insert_with(|| ore.clone());
            }
        }
        let all_ores: Vec<NetherOre> = ores_by_biome.values().flatten().cloned().collect();
        let ore_tag_map = build_nether_ore_tag_map(resolver, &all_ores);
        let veg_tags = crate::feature::vegetation::build_veg_tags(resolver);

        // Structure placement's dimension half. Filtered by `possible_biomes`
        // because vanilla filters the same way when building per-dimension
        // structure state, which
        // here means the registry parses `nether_complexes`, `nether_fossils` and
        // `ruined_portals` and loads only `bastion`'s pool graph — not every
        // village's. `is_empty()` → `None`, so a fixture resolver is unaffected.
        let structures = {
            let registry =
                StructureRegistry::new_for_biomes(seed, resolver, Some(&possible_biomes));
            if registry.is_empty() { None } else { Some(registry) }
        };

        // Captured after every `builder.build()` above, which is always a safe
        // bound for any one tree's own sampler.
        let slot_count = builder.slot_count();

        Self {
            seed,
            slot_count,
            interner,
            surface,
            final_density,
            climate,
            table,
            min_y,
            height,
            sea_level,
            cell_width,
            cell_height,
            default_block,
            default_fluid,
            default_block_pre,
            default_fluid_pre,
            carver_replaceable,
            carvers_by_biome,
            decoration_catalog,
            ore_definitions,
            feature_biomes,
            ore_tag_map,
            veg_tags,
            structures,
            starts: ShardedMemo::new(STARTS_MEMO_CEILING),
            pre_decoration: ShardedMemo::new(DECORATION_MEMO_CEILING),
            pre_decoration_computations: AtomicUsize::new(0),
        }
    }

    /// Raise the pre-decoration memo for one lifecycle replay without changing
    /// the production demand-ordered default. Every source completion reads a
    /// 5×5 context, so the exact replay closure is the admitted rectangle
    /// expanded by [`crate::feature::region_view::WIDE_RADIUS`] in both axes.
    /// The returned capacity is useful to diagnostics and tests; an empty
    /// admission list leaves the production bound unchanged.
    pub fn prepare_lifecycle_replay(&self, admissions: &[(i32, i32)]) -> usize {
        self.prepare_immutable_stage_cache(
            admissions,
            crate::feature::region_view::WIDE_RADIUS,
        )
    }

    /// Raises the pure pre-decoration memo for packet replay over `targets`.
    ///
    /// A packet needs the target plus its eight light neighbours, and each
    /// generated column reads the wider 5×5 immutable prefix context. The
    /// capacity is therefore derived from the target coordinates expanded by
    /// one packet-neighbour radius and then by the generator's wide radius.
    /// The production sharded floor remains unchanged for callers that do not
    /// explicitly prepare a replay.
    pub fn prepare_packet_replay(&self, targets: &[(i32, i32)]) -> usize {
        let mut packet_targets = Vec::with_capacity(targets.len().saturating_mul(9));
        for &(cx, cz) in targets {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    packet_targets.push((cx + dx, cz + dz));
                }
            }
        }
        self.prepare_immutable_stage_cache(
            &packet_targets,
            crate::feature::region_view::WIDE_RADIUS,
        )
    }

    /// Releases the immutable pre-decoration state retained by a packet
    /// replay and restores the ordinary sharded cache floor.
    ///
    /// A large raw-packet sweep calls this after each bounded spatial window;
    /// clearing the memo is part of the replay boundary, not an eviction that
    /// changes generation order or bytes.
    pub fn reset_packet_replay(&self) {
        self.pre_decoration.clear();
        self.pre_decoration.set_capacity(DECORATION_MEMO_CEILING);
        self.pre_decoration_computations.store(0, Ordering::Relaxed);
    }

    fn prepare_immutable_stage_cache(&self, admissions: &[(i32, i32)], radius: i32) -> usize {
        let capacity = pre_decoration_capacity(admissions, radius);
        self.pre_decoration
            .set_capacity(capacity.max(DECORATION_MEMO_CEILING));
        self.pre_decoration_computations.store(0, Ordering::Relaxed);
        capacity
    }

    /// Number of pre-decoration fields actually computed by this generator.
    /// Diagnostics only; generation never branches on this count.
    #[must_use]
    pub fn pre_decoration_computations(&self) -> usize {
        self.pre_decoration_computations.load(Ordering::Relaxed)
    }

    /// Number of pre-decoration shard evictions since the last explicit replay
    /// preparation (or since generator construction before the first
    /// preparation).
    #[must_use]
    pub fn pre_decoration_evictions(&self) -> usize {
        self.pre_decoration.stats().evictions as usize
    }

    /// Lock and once-cell timings for the Nether's two shared generator caches.
    /// Timings are collected only when `LODESTONE_NETHER_PROFILE` is set.
    #[must_use]
    pub fn cache_stats(&self) -> (NetherCacheStats, NetherCacheStats) {
        (self.starts.stats(), self.pre_decoration.stats())
    }

    /// The generated column for chunk `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> NetherColumn {
        let pre = self.pre_decoration_stage(cx, cz);
        let (world, decoration_spills) = self.mixed_step7_stage_with_spills(
            cx,
            cz,
            (*pre.0).clone(),
            &pre.1,
        );

        let (palette, blocks) = world.into_palette_and_blocks();
        NetherColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts: pre.2.clone(),
            placement_loot: pre.3.clone(),
            decoration_spills,
        }
    }

    /// Generates the complete pre-decoration field for `(cx, cz)`.
    ///
    /// The returned column contains terrain, biome, carving and structure
    /// placement products, but no mixed decoration entries.  A lifecycle
    /// materializer uses this prefix as the resident value before applying
    /// source completion spill in the observed order; ordinary callers should
    /// use [`Self::column`] for a packet-ready result.
    #[must_use]
    pub fn column_shaped(&self, cx: i32, cz: i32) -> NetherColumn {
        let pre = self.pre_decoration_stage(cx, cz);
        let (palette, blocks) = (*pre.0).clone().into_palette_and_blocks();
        NetherColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts: pre.2.clone(),
            placement_loot: pre.3.clone(),
            decoration_spills: Vec::new(),
        }
    }

    /// Runs one source chunk's existing mixed decoration dispatcher against a
    /// target's pre-decoration field with the final states already applied by
    /// earlier source completions.  Both production placement adapters receive
    /// those states: the ore view sees the same bounded overlay used by ore
    /// replacement checks, while the vegetation grid sees the same sparse
    /// resident field used by height and neighbour probes.  Only net writes
    /// beyond the seeded context are returned.
    #[must_use]
    pub fn parity_source_spills_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> Vec<ParityDecorationSpill> {
        assert!(
            (target_x - source_x).abs() <= 1 && (target_z - source_z).abs() <= 1,
            "a decoration source must be inside the target's 3x3 dispatch window",
        );
        let pre = self.pre_decoration_stage(target_x, target_z);
        self.mixed_step7_stage_selected(
            target_x,
            target_z,
            (*pre.0).clone(),
            &pre.1,
            Some((source_x, source_z)),
            overrides,
        )
        .1
    }

    fn mixed_step7_stage_with_spills(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
    ) -> (crate::dense_grid::DenseBlockGrid, Vec<(i32, i32, i32, String)>) {
        let (world, _, spills) = self.mixed_step7_stage_selected(
            cx,
            cz,
            center_world,
            center_heights,
            None,
            &[],
        );
        (world, spills)
    }

    /// [`Self::mixed_step7_stage_with_spills`] with an optional source filter for the
    /// parity materializer. The single raw dispatcher below remains the only
    /// placement/RNG implementation for both paths.
    fn mixed_step7_stage_selected(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, String)],
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        Vec<ParityDecorationSpill>,
        Vec<(i32, i32, i32, String)>,
    ) {
        let mut nearby: [Option<Arc<PreDecorationResult>>; 25] =
            std::array::from_fn(|_| None);
        let mut nearby_biomes: [Option<Arc<crate::overworld::BiomeCells>>; 25] =
            std::array::from_fn(|_| None);
        for dx in -crate::feature::region_view::WIDE_RADIUS
            ..=crate::feature::region_view::WIDE_RADIUS
        {
            for dz in -crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS
            {
                if dx != 0 || dz != 0 {
                    let pre = self.pre_decoration_stage(cx + dx, cz + dz);
                    let slot = crate::feature::region_view::wide_slot_of_offset(dx, dz);
                    nearby[slot] = Some(Arc::clone(&pre));
                    nearby_biomes[slot] = Some(Arc::new(nether_biome_cells(
                        &pre.2,
                        self.min_y,
                        DECORATION_WINDOW_HEIGHT,
                    )));
                }
            }
        }
        let centre_biomes = self.pre_decoration_stage(cx, cz).2.clone();
        let centre_biome_cells = Arc::new(nether_biome_cells(
            &centre_biomes,
            self.min_y,
            DECORATION_WINDOW_HEIGHT,
        ));
        let feature_biomes = &self.feature_biomes;
        let zoom_seed = nether_zoom_seed(self.seed);
        let biome_quart_at = |block_x: i32, block_z: i32| {
            let source_x = block_x.div_euclid(16);
            let source_z = block_z.div_euclid(16);
            let source_biomes = if source_x == cx && source_z == cz {
                Some(&centre_biomes)
            } else {
                let dx = source_x - cx;
                let dz = source_z - cz;
                if (-crate::feature::region_view::WIDE_RADIUS
                    ..=crate::feature::region_view::WIDE_RADIUS)
                    .contains(&dx)
                    && (-crate::feature::region_view::WIDE_RADIUS
                        ..=crate::feature::region_view::WIDE_RADIUS)
                        .contains(&dz)
                {
                    nearby[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(|pre| &pre.2)
                } else {
                    None
                }
            }?;
            let qx = block_x.rem_euclid(16).div_euclid(4) as usize;
            let qz = block_z.rem_euclid(16).div_euclid(4) as usize;
            Some(source_biomes[qz * 4 + qx].as_str())
        };
        let biome_at = |pos: crate::feature::BlockPos| {
            let shifted_x = pos.x - 2;
            let shifted_y = pos.y - 2;
            let shifted_z = pos.z - 2;
            let parent_x = shifted_x >> 2;
            let parent_y = shifted_y >> 2;
            let parent_z = shifted_z >> 2;
            let fract_x = f64::from(shifted_x.rem_euclid(4)) / 4.0;
            let fract_y = f64::from(shifted_y.rem_euclid(4)) / 4.0;
            let fract_z = f64::from(shifted_z.rem_euclid(4)) / 4.0;
            let mut selected = 0;
            let mut best = f64::INFINITY;
            for corner in 0..8 {
                let x_low = corner & 4 == 0;
                let y_low = corner & 2 == 0;
                let z_low = corner & 1 == 0;
                let qx = if x_low { parent_x } else { parent_x + 1 };
                let qy = if y_low { parent_y } else { parent_y + 1 };
                let qz = if z_low { parent_z } else { parent_z + 1 };
                let dx = if x_low { fract_x } else { fract_x - 1.0 };
                let dy = if y_low { fract_y } else { fract_y - 1.0 };
                let dz = if z_low { fract_z } else { fract_z - 1.0 };
                let distance = nether_fiddled_distance(zoom_seed, qx, qy, qz, dx, dy, dz);
                if best > distance {
                    selected = corner;
                    best = distance;
                }
            }
            let qx = if selected & 4 == 0 { parent_x } else { parent_x + 1 };
            let qz = if selected & 1 == 0 { parent_z } else { parent_z + 1 };
            biome_quart_at(qx * 4, qz * 4)
        };
        let biome_allows = |pos: crate::feature::BlockPos, feature_id: &str| {
            biome_at(pos).is_some_and(|biome| {
                feature_biomes
                    .get(feature_id)
                    .is_some_and(|eligible| eligible.contains(biome))
            })
        };
        let mut heights = crate::feature::RegionHeights::unset();
        Self::stitch_heights(&mut heights, 0, 0, center_heights);
        for dx in -crate::feature::region_view::WIDE_RADIUS
            ..=crate::feature::region_view::WIDE_RADIUS
        {
            for dz in -crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS
            {
            if dx != 0 || dz != 0 {
                let source = nearby[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .expect("all wide sources present");
                Self::stitch_heights(&mut heights, dx * 16, dz * 16, &source.1);
            }
            }
        }
        let in_tag = |block: &str, tag: &str| self.ore_tag_map.get(tag).is_some_and(|members| members.contains(block));
        let centre_source = &center_world;
        let mut ore_view = crate::feature::region_view::RegionView::over_wide_sources(
            Arc::clone(&self.interner), cx, cz, self.min_y, self.height,
            |dx, dz| if dx == 0 && dz == 0 { Some(centre_source) } else {
                nearby[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .map(|source| &*source.0)
            },
        );
        let centre_grid = Arc::new(center_world.clone());
        let grid_sources = &nearby;
        let grid_biomes = &nearby_biomes;
        let grid_feature_biomes = self.feature_biomes.clone();
        let mut grid = crate::feature::vegetation::VegGrid::with_sources_and_biomes_shared(
            Arc::clone(&self.interner), self.min_y, DECORATION_WINDOW_HEIGHT, cx * 16, cz * 16,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(Arc::clone(&centre_grid))
                } else {
                    grid_sources[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(|source| Arc::clone(&source.0))
                }
            },
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(Arc::clone(&centre_biome_cells))
                } else {
                    grid_biomes[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(Arc::clone)
                }
            },
            grid_feature_biomes,
        );
        self.veg_tags.bind(grid.interner());
        let mut ore_transferred = HashMap::new();
        let mut seeded = BTreeMap::new();
        for &(x, y, z, ref state) in overrides {
            let id = self.interner.id_of(state);
            let lx = x - cx * 16;
            let lz = z - cz * 16;
            if ore_view.seed_read_id(lx, y, lz, id) {
                ore_transferred.insert((x, y, z), id);
            }
            if (crate::feature::REGION_MIN - crate::feature::VEG_PADDING
                ..crate::feature::REGION_MAX + crate::feature::VEG_PADDING)
                .contains(&lx)
                && (crate::feature::REGION_MIN - crate::feature::VEG_PADDING
                    ..crate::feature::REGION_MAX + crate::feature::VEG_PADDING)
                    .contains(&lz)
                && (self.min_y..self.min_y + DECORATION_WINDOW_HEIGHT).contains(&y)
            {
                grid.seed_id(x, y, z, id);
                seeded.insert((x, y, z), id);
            }
        }
        let mut source_features = BTreeMap::new();
        for source_x in cx - 1..=cx + 1 {
            for source_z in cz - 1..=cz + 1 {
                source_features.insert(
                    (source_x, source_z),
                    self.source_mixed_features(source_x, source_z),
                );
            }
        }
        let mut decoration_rng = decoration_random();
        let mut ore_random = decoration_random();
        let mut grid_cursor = 0usize;
        let mut ore_cursor = 0usize;
        let mut changed_scratch = Vec::new();
        for dx in -1..=1_i32 { for dz in -1..=1_i32 {
            let source_x = cx + dx;
            let source_z = cz + dz;
            if selected_source.is_some_and(|source| source != (source_x, source_z)) {
                continue;
            }
            let origin = crate::feature::BlockPos { x: source_x * 16, y: self.min_y, z: source_z * 16 };
            decoration_rng.begin_decoration_source();
            ore_random.begin_decoration_source();
            let decoration_seed = decoration_rng.set_decoration_seed(self.seed, origin.x, origin.z);
            let ore_seed = ore_random.set_decoration_seed(self.seed, origin.x, origin.z);
            let (ores, decorations) = source_features
                .get(&(source_x, source_z))
                .map(|(ores, decorations)| (ores.as_slice(), decorations.as_slice()))
                .unwrap_or((&[], &[]));
            for step in [4_i32, 7, crate::feature::STEP_VEGETAL_DECORATION] {
                let mut decoration_at = 0usize;
                let mut ore_at = 0usize;
                loop {
                    let next_decoration = decorations.iter().enumerate()
                        .skip(decoration_at)
                        .find(|(_, (s, _, _))| *s == step);
                    let next_ore = if step == 7 { ores.get(ore_at) } else { None };
                    let next_index = match (next_decoration, next_ore) {
                        (Some((_, (_, index, _))), Some(ore)) => Some((*index).min(ore.index())),
                        (Some((_, (_, index, _))), None) => Some(*index),
                        (None, Some(ore)) => Some(ore.index()),
                        (None, None) => None,
                    };
                    let Some(index) = next_index else { break };
                    if let Some((entry_at, (_, found, placed))) = next_decoration
                        .filter(|(_, (_, found, _))| *found == index)
                    {
                        crate::feature::vegetation::apply_decoration_entry_at_world_seed(&mut decoration_rng, self.seed, decoration_seed, origin, step, *found, placed, &mut grid, &self.veg_tags);
                        synchronize_mixed_entry_reusing(
                            MixedEntryWriter::Decoration,
                            &mut grid,
                            &mut ore_view,
                            cx,
                            cz,
                            self.min_y,
                            self.height,
                            &mut grid_cursor,
                            &mut ore_cursor,
                            &mut ore_transferred,
                            &mut changed_scratch,
                        );
                        decoration_at = entry_at + 1;
                    } else if let Some(ore) = next_ore.filter(|ore| ore.index() == index) {
                        let input = crate::feature::OreInput { chunk_x: source_x, chunk_z: source_z, center_x: cx, center_z: cz, min_y: self.min_y, height: self.height, min_gen_y: self.min_y, gen_depth: self.height, read_min: crate::feature::ORE_READ_MIN, read_max: crate::feature::ORE_READ_MAX, ocean_floor_wg: &heights, in_tag: &in_tag, biome_allows: Some(&biome_allows) };
                        match ore {
                            NetherOre::Standard(ore) => crate::feature::apply_ore_entry_at_seed(
                                &mut ore_random,
                                ore_seed,
                                &input,
                                7,
                                ore,
                                &mut ore_view,
                            ),
                            NetherOre::Scattered(ore) => {
                                crate::feature::apply_scattered_ore_entry_at_seed(
                                    &mut ore_random,
                                    ore_seed,
                                    &input,
                                    7,
                                    ore,
                                    &mut ore_view,
                                );
                            }
                        }
                        synchronize_mixed_entry_reusing(
                            MixedEntryWriter::Ore,
                            &mut grid,
                            &mut ore_view,
                            cx,
                            cz,
                            self.min_y,
                            self.height,
                            &mut grid_cursor,
                            &mut ore_cursor,
                            &mut ore_transferred,
                            &mut changed_scratch,
                        );
                        ore_at += 1;
                    }
                }
            }
        }}
        let mut world = center_world;
        let mut decoration_spills = Vec::new();
        for (x, y, z, state) in grid.dirty_cell_ids() {
            if (self.min_y..self.min_y + self.height).contains(&y) {
                world.set_id(x, y, z, state);
            } else if selected_source.is_none()
                && (cx * 16..cx * 16 + 16).contains(&x)
                && (cz * 16..cz * 16 + 16).contains(&z)
                && (self.min_y..self.min_y + DECORATION_WINDOW_HEIGHT).contains(&y)
            {
                decoration_spills.push((
                    x,
                    y,
                    z,
                    self.interner.name_of(state).to_owned(),
                ));
            }
        }
        let mut final_spills = BTreeMap::new();
        if let Some(source) = selected_source {
            for (x, y, z, state) in grid.dirty_cell_ids() {
                if seeded.get(&(x, y, z)).copied() != Some(state) {
                    final_spills.insert((x, y, z), ParityDecorationSpill {
                        source,
                        position: (x, y, z),
                        state: self.interner.name_of(state).to_owned(),
                    });
                }
            }
        }
        (world, final_spills.into_values().collect(), decoration_spills)
    }

    /// The complete prefix decorations read: terrain through structure pieces.
    /// It is cached by exact chunk coordinate because both 3×3 drivers need
    /// neighbours' real terrain, not a copy of the centre field.
    fn pre_decoration_stage(&self, cx: i32, cz: i32) -> Arc<PreDecorationResult> {
        self.pre_decoration.get_or_compute((cx, cz), || {
            self.pre_decoration_computations
                .fetch_add(1, Ordering::Relaxed);
            let base_x = cx * 16;
            let base_z = cz * 16;
            let refs = self.structure_refs(cx, cz);
            let beard = self.beardifier_for(cx, cz, &refs);
            let aquifer = self.build_fill(cx, cz);
            let field = self.fill_stage(&aquifer, base_x, base_z, &beard);
            let heights = self.heights_from_field(&field);
            let biome_quarts = self.biome_quarts(cx, cz);
            let surface_diff = self.surface_stage(&field, &heights, base_x, base_z);
            let world = self.materialize_world(&field, surface_diff, base_x, base_z);
            let world = self.carve_stage(cx, cz, &aquifer, world);
            let (world, placement_loot) = self.structure_place_stage(cx, cz, &refs, world);
            (Arc::new(world), heights, biome_quarts, placement_loot)
        })
    }

    /// Selects the mixed source list from the same globally ordered catalog
    /// used by the feature scheduler.  A source sees the union of all biomes
    /// stored by its own 3x3 chunk neighbourhood; each selected feature keeps
    /// its global per-step index, including the entries handled by the ore
    /// adapter below.
    fn source_mixed_features(
        &self,
        source_x: i32,
        source_z: i32,
    ) -> (Vec<NetherOre>, DecorationFeatures) {
        let mut biomes = std::collections::BTreeSet::new();
        for dx in -1..=1 {
            for dz in -1..=1 {
                let pre = self.pre_decoration_stage(source_x + dx, source_z + dz);
                biomes.extend(pre.2.iter().cloned());
            }
        }
        let mut ores = Vec::new();
        let mut decorations = Vec::new();
        for (step, index, placed) in self
            .decoration_catalog
            .select(biomes.iter().map(String::as_str))
        {
            if !matches!(step, 4 | 7 | crate::feature::STEP_VEGETAL_DECORATION) {
                continue;
            }
            if step == 7 {
                if let Some(id) = placed.registry_id.as_deref() {
                    if let Some(ore) = self.ore_definitions.get(id) {
                        ores.push(ore.with_index(index));
                        continue;
                    }
                }
            }
            decorations.push((
                step,
                index,
                nether_window_placed_ref(&placed, self.height),
            ));
        }
        (ores, decorations)
    }

    fn stitch_heights(
        heights: &mut crate::feature::RegionHeights,
        offset_x: i32,
        offset_z: i32,
        source: &[i32; 256],
    ) {
        for lz in 0..16_i32 {
            for lx in 0..16_i32 {
                heights.set(offset_x + lx, offset_z + lz, source[(lz * 16 + lx) as usize]);
            }
        }
    }

    /// Vanilla's own disabled-aquifer constructor bound to this chunk — the Nether's whole fill
    /// decision. See [`AquiferSystem::disabled`].
    fn build_fill(&self, cx: i32, cz: i32) -> AquiferSystem {
        AquiferSystem::disabled(
            self.final_density.clone(),
            self.slot_count,
            self.sea_level,
            BlockKind::Lava,
            self.min_y,
            self.height,
            cx,
            cz,
            self.cell_width,
            self.cell_height,
        )
    }

    fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        crate::compose::column_index(lx, ly, lz, height)
    }

    /// `fillFromNoise`, i.e. `add(finalDensity, BeardifierMarker)` —
    /// [`crate::compose::fill_column`], which carries the two-loop rule and the
    /// `-0.0` argument for it.
    ///
    /// The property that matters *here* is what an empty beard means for this
    /// dimension: `nether_fossil` is the Nether's only adaptation-bearing structure
    /// and has no piece generator, so every column takes the no-addition loop and is
    /// bit-identical to the pre-structure generator.
    fn fill_stage(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &Beardifier,
    ) -> Vec<BlockKind> {
        crate::compose::fill_column(aquifer, base_x, base_z, self.min_y, self.height, beard)
    }

    /// Highest solid Y per column, floored at `sea_level - 1` — the same
    /// `solidTop` definition the Overworld path and `ComposedChunkOracle` use.
    fn heights_from_field(&self, field: &[BlockKind]) -> [i32; 256] {
        crate::compose::solid_top_heights(field, self.min_y, self.height, self.sea_level)
    }

    /// One climate sample per horizontal quart, vanilla's own climate sampler at
    /// `(quartX, quartY, quartZ)` → its own quart-to-block conversion → the parameter list's nearest row.
    ///
    /// `quartY` is passed as 0 because the Nether's climate is y-invariant (module
    /// doc); the gate that keeps this honest is
    /// `nether_biomes_do_not_vary_with_y`.
    ///
    /// **Public because it is the cheap half of `column`** and the parity gate
    /// against the vanilla oracle world's 1,116 stored Nether chunks runs it
    /// alone: 16 climate samples per chunk instead of a whole 32,768-block fill,
    /// which is what makes an exhaustive comparison affordable. It is the same
    /// code path `column` uses, not a reimplementation for the test.
    #[must_use]
    pub fn biome_quarts(&self, cx: i32, cz: i32) -> [String; 16] {
        std::array::from_fn(|i| {
            let qx = cx * 4 + (i % 4) as i32;
            let qz = cz * 4 + (i / 4) as i32;
            let target = self.climate.target(qx * 4, 0, qz * 4);
            self.table.nearest(&target).to_string()
        })
    }

    /// Resolves the biome at a block position through the dimension's zoomed
    /// quart lookup.  The packet biome container stores the climate answer at
    /// quart corners, but surface rules ask the biome manager at each block
    /// position; the latter chooses among eight corners after the
    /// seed-derived fiddle.  Nether climate is y-invariant, while the zoom
    /// choice still uses the supplied block Y exactly as the dimension's
    /// biome accessor does.
    fn biome_at_block(&self, x: i32, y: i32, z: i32) -> &str {
        let shifted_x = x - 2;
        let shifted_y = y - 2;
        let shifted_z = z - 2;
        let parent_x = shifted_x >> 2;
        let parent_y = shifted_y >> 2;
        let parent_z = shifted_z >> 2;
        let fract_x = f64::from(shifted_x.rem_euclid(4)) / 4.0;
        let fract_y = f64::from(shifted_y.rem_euclid(4)) / 4.0;
        let fract_z = f64::from(shifted_z.rem_euclid(4)) / 4.0;
        let zoom_seed = nether_zoom_seed(self.seed);
        let mut selected = 0;
        let mut best = f64::INFINITY;
        for corner in 0..8 {
            let x_low = corner & 4 == 0;
            let y_low = corner & 2 == 0;
            let z_low = corner & 1 == 0;
            let qx = if x_low { parent_x } else { parent_x + 1 };
            let qy = if y_low { parent_y } else { parent_y + 1 };
            let qz = if z_low { parent_z } else { parent_z + 1 };
            let dx = if x_low { fract_x } else { fract_x - 1.0 };
            let dy = if y_low { fract_y } else { fract_y - 1.0 };
            let dz = if z_low { fract_z } else { fract_z - 1.0 };
            let distance = nether_fiddled_distance(zoom_seed, qx, qy, qz, dx, dy, dz);
            if best > distance {
                selected = corner;
                best = distance;
            }
        }
        let qx = if selected & 4 == 0 { parent_x } else { parent_x + 1 };
        let qy = if selected & 2 == 0 { parent_y } else { parent_y + 1 };
        let qz = if selected & 1 == 0 { parent_z } else { parent_z + 1 };
        self.table.nearest(&self.climate.target(qx * 4, qy * 4, qz * 4))
    }

    /// The biome one *source chunk* of the carve neighbourhood resolves to —
    /// vanilla's `carverBiome`, sampled at that chunk's own quart corner and
    /// `y = 0`.
    fn biome_for_carver_source(&self, source_x: i32, source_z: i32) -> &str {
        let target = self.climate.target(source_x * 16, 0, source_z * 16);
        self.table.nearest(&target)
    }

    fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        base_x: i32,
        base_z: i32,
    ) -> SurfaceDiff {
        // Re-derived rather than reasoned about, for the reason `overworld::fill`
        // gives: a wrong `PreClass` changes which surface rules fire and still
        // produces a plausible column.
        debug_assert_eq!(
            self.default_block_pre,
            PreState::from_name(&self.interner, &self.default_block),
        );
        debug_assert_eq!(
            self.default_fluid_pre,
            PreState::from_name(&self.interner, &self.default_fluid),
        );

        let pre = |lx: i32, y: i32, lz: i32| -> PreState {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return PreState::AIR;
            }
            match field[Self::idx(lx, ly, lz, self.height)] {
                BlockKind::Stone => self.default_block_pre,
                // The Nether's `default_fluid` *is* lava, so both fluid arms are
                // the same state here; keeping them separate keeps the match
                // exhaustive over `BlockKind` rather than over this dimension.
                BlockKind::Water | BlockKind::Lava => self.default_fluid_pre,
                BlockKind::Air => PreState::AIR,
            }
        };
        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        // `cold_enough_to_snow` is false for every Nether biome (they all declare
        // `temperature: 2.0`), and nothing in vanilla's own bundled Nether surface-rule data reads it
        // — there is no `temperature` condition in the Nether rule tree.
        let biome_at = |lx: i32, y: i32, lz: i32| -> (&str, bool) {
            (self.biome_at_block(base_x + lx, y, base_z + lz), false)
        };

        self.surface
            .build_surface(&pre, &heightmap, &biome_at, base_x, base_z)
    }

    fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: SurfaceDiff,
        base_x: i32,
        base_z: i32,
    ) -> crate::dense_grid::DenseBlockGrid {
        // Point lookups into `surface_diff` in a fixed order, never iteration — see
        // [`crate::compose::materialize_column`] for the palette-order rule and the
        // bug that established it.
        crate::compose::materialize_column(
            &self.interner,
            field,
            &surface_diff,
            base_x,
            base_z,
            self.min_y,
            self.height,
            self.default_block_pre.state,
            self.default_fluid_pre.state,
        )
    }

    /// Vanilla's own carve-application step over the post-surface column.
    ///
    /// `top_material` is a constant `None`: vanilla's own nether-world-carver
    /// carve-block step never
    /// calls it (nor the aquifer, nor the grass tracking) — see
    /// [`crate::carver::CaveConfig::nether`].
    fn carve_stage(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        let mut grid = CarveGrid::from_dense(world);
        let carvers_for_source =
            |sx: i32, sz: i32| -> Vec<CarverConfig> {
                let biome = self.biome_for_carver_source(sx, sz);
                self.carvers_by_biome.get(biome).cloned().unwrap_or_default()
            };
        let top_material = |_: i32, _: i32, _: i32, _: bool| -> Option<String> { None };
        crate::carver::apply_carvers(
            self.seed,
            cx,
            cz,
            self.min_y,
            self.height,
            &carvers_for_source,
            &mut grid,
            aquifer,
            &self.carver_replaceable,
            &top_material,
            &mut NoObserver,
        );
        grid.into_dense()
    }
}

/// [`StartContext`] over freshly sampled *Nether* noise columns.
///
/// The Overworld's equivalent is `overworld::structures::StartSampler`, and the
/// difference between them is the whole reason this type exists rather than a
/// shared one: every column it samples comes from
/// [`AquiferSystem::disabled`] over the Nether's `final_density` at `min_y 0`,
/// height 128, so a height probe answers against *this* dimension's terrain. A
/// sampler that resolved to an Overworld-shaped column would site a bastion
/// plausibly and wrongly, and nothing in a screenshot would say so.
struct NetherStartSampler<'a> {
    generator: &'a NetherGenerator,
    /// `(cx, cz)` → that chunk's disabled aquifer. `RefCell` because
    /// [`StartContext`] takes `&self` (it is called through a `&dyn` from the
    /// registry) and this is single-threaded per stage invocation. Building one is
    /// the expensive part and a start predicate asks about several columns of the
    /// same chunk.
    aquifers: RefCell<HashMap<(i32, i32), Arc<AquiferSystem>>>,
}

impl NetherStartSampler<'_> {
    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        let built = Arc::new(self.generator.build_fill(cx, cz));
        self.aquifers
            .borrow_mut()
            .insert((cx, cz), Arc::clone(&built));
        built
    }
}

impl StartContext for NetherStartSampler<'_> {
    /// Vanilla's own first-occupied-height lookup — the Y of the topmost
    /// block satisfying the heightmap predicate, or `minY - 1` for a column that
    /// never matches.
    ///
    /// **This reads the pre-surface fill, so it never sees the bedrock roof.** The
    /// Nether's `y 123..=127` bedrock is a `vertical_gradient` surface rule, and
    /// vanilla's own base-height lookup runs against a fresh noise-chunk sampler too — so a
    /// `WORLD_SURFACE_WG` probe here answers "topmost non-air *noise*", which is
    /// exactly the number vanilla's structure placement uses. The Nether's noise is
    /// solid near the roof over most of the dimension, so this is usually a large
    /// number; a structure kind that treats it as "the walkable surface" would be
    /// wrong about this dimension, and vanilla's Nether kinds do not (bastion is
    /// `start_height: {absolute: 33}` and probes nothing).
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        let generator = self.generator;
        let aquifer = self.aquifer(x >> 4, z >> 4);
        for ly in (0..generator.height).rev() {
            let y = generator.min_y + ly;
            let kind = aquifer.block_at(x, y, z);
            let matched = match heightmap {
                HeightmapKind::WorldSurfaceWg => kind != BlockKind::Air,
                // `blocksMotion`: stone only — lava explicitly excluded, which in
                // this dimension is the difference between the netherrack shelf and
                // the lava sea's surface.
                HeightmapKind::OceanFloorWg => kind == BlockKind::Stone,
            };
            if matched {
                return y;
            }
        }
        generator.min_y - 1
    }

    /// Vanilla's own multi-noise biome lookup at `(qx, qy, qz)`, written with the real
    /// `qy` rather than the constant 0 [`NetherGenerator::biome_quarts`] uses.
    ///
    /// The two agree by the y-invariance `nether_biomes_do_not_vary_with_y` pins
    /// (`temperature`/`vegetation` are `shifted_noise` with `y_scale: 0.0`), and
    /// spelling it the vanilla way here means the structure biome filter does not
    /// inherit an assumption from a neighbouring optimisation.
    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        let target = self.generator.climate.target(qx * 4, qy * 4, qz * 4);
        self.generator.table.nearest(&target).to_string()
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level
    }

    /// The real dimension bounds, so a jigsaw structure's `above_bottom` /
    /// `below_top` start height and its `dimension_padding` resolve against a
    /// 0..128 world rather than against the trait's Overworld default. Getting this
    /// from the default would put every `below_top` bastion piece 256 blocks above
    /// the Nether roof.
    fn min_y(&self) -> i32 {
        self.generator.min_y
    }

    fn dimension_height(&self) -> i32 {
        self.generator.height
    }

    /// `isReplaceableByStructures`: air or fluid. Read out of the same per-chunk
    /// aquifer the height probe uses.
    fn is_replaceable_at(&self, x: i32, y: i32, z: i32) -> bool {
        let aquifer = self.aquifer(x >> 4, z >> 4);
        !matches!(aquifer.block_at(x, y, z), BlockKind::Stone)
    }

    /// The four-way fill kind. Here the *default fluid is lava*, so a predicate that
    /// treats "fluid" as water is wrong in this dimension and the distinction is not
    /// academic — it is `ruined_portal_nether`'s whole obsidian/lava test.
    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self.aquifer(x >> 4, z >> 4).block_at(x, y, z)
    }
}

/// The Nether's structure stages — the composition that closes the
/// `dimension:nether_structures` island. See this module's own doc for what is
/// dimension-shaped about them and what is shared.
impl NetherGenerator {
    /// Stage 0a: this chunk's structure starts, memoised in [`Self::starts`].
    ///
    /// Empty and allocation-cheap for a generator with no structure data.
    fn structure_starts_stage(&self, cx: i32, cz: i32) -> Arc<Vec<Arc<StructureStart>>> {
        self.starts.get_or_compute((cx, cz), || {
            // `starts_at` samples columns and can assemble a whole jigsaw, so the
            // sharded map lock is released before this OnceLock computes. A second
            // worker racing the same key waits for the published value instead of
            // repeating the registry work.
            match &self.structures {
                None => Vec::new(),
                Some(registry) => {
                    let sampler = NetherStartSampler {
                        generator: self,
                        aquifers: RefCell::new(HashMap::new()),
                    };
                    registry
                        .starts_at(cx, cz, &sampler)
                        .into_iter()
                        .map(Arc::new)
                        .collect()
                }
            }
        })
    }

    /// Stage 0b: vanilla's own structure-reference-gathering 17×17 walk, keeping the starts whose adjusted
    /// box comes within [`BEARD_REACH`] blocks of this chunk.
    ///
    /// Identical in shape to the Overworld's — the widened reach is the
    /// beardifier's own ("beardifier for structures in chunk"'s
    /// own "close to chunk" test at distance 12), and
    /// [`StructureRefs::packed_by_structure`] re-narrows to vanilla's exact
    /// chunk-box test for the persistence view.
    ///
    /// Not memoised: it is a walk over an already-memoised product and is consumed
    /// exactly once per column, so a second map would carry no work.
    fn structure_refs(&self, cx: i32, cz: i32) -> StructureRefs {
        if self.structures.is_none() {
            return StructureRefs::default();
        }
        let mut entries = Vec::new();
        for sx in (cx - REFS_RADIUS)..=(cx + REFS_RADIUS) {
            for sz in (cz - REFS_RADIUS)..=(cz + REFS_RADIUS) {
                for start in self.structure_starts_stage(sx, sz).iter() {
                    let beard_reaches = start
                        .adjusted_bounding_box()
                        .is_close_to_chunk(cx, cz, BEARD_REACH);
                    let portal_terrain_reaches = start.pieces.iter().any(|piece| {
                        matches!(
                            piece.refine.as_ref(),
                            Some(PieceRefinement::RuinedPortalTerrain { .. })
                        ) && piece
                            .bounding_box
                            .is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH)
                    });
                    if beard_reaches || portal_terrain_reaches {
                        entries.push((sx, sz, Arc::clone(start)));
                    }
                }
            }
        }
        StructureRefs { entries }
    }

    /// Stage 0c: this chunk's beard term.
    ///
    /// `nether_fossil` (`terrain_adaptation: beard_thin`) is the dimension's *only*
    /// adaptation-bearing structure and has no piece generator, so today this is
    /// empty for every chunk and [`Self::fill_stage`] takes its no-beard branch —
    /// which is the negative control for the whole change: the Nether's biome and
    /// bedrock parity against the vanilla oracle world is unchanged, by
    /// construction rather than by measurement.
    fn beardifier_for(&self, cx: i32, cz: i32, refs: &StructureRefs) -> Beardifier {
        if self.structures.is_none() {
            return Beardifier::empty();
        }
        Beardifier::for_chunk(cx, cz, refs.adaptation_bearing().map(AsRef::as_ref))
    }

    /// Stage 4b: writes every piece that touches this chunk into `world`.
    ///
    /// Clipping is the grid, not a box: [`crate::dense_grid::DenseBlockGrid::set`]
    /// ignores a write outside this chunk's 16×16 columns, so a piece that straddles
    /// a border writes its own half here and the other half when the neighbour
    /// generates. Template processor draws remain position-seeded; a ruined
    /// portal's terrain refinement consumes its target chunk's shared
    /// `surface_structures` stream, reset per portal registry entry.
    fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        refs: &StructureRefs,
        mut world: crate::dense_grid::DenseBlockGrid,
    ) -> (crate::dense_grid::DenseBlockGrid, Vec<CodedLoot>) {
        let Some(registry) = &self.structures else {
            return (world, Vec::new());
        };
        let seed = registry.seed();
        let (bx, bz) = (cx * 16, cz * 16);
        let mut feature_randoms: HashMap<
            String,
            crate::rng::WorldgenRandom<crate::rng::LegacyRandomSource>,
        > = HashMap::new();
        let mut structure_randoms: HashMap<
            String,
            crate::rng::WorldgenRandom<crate::rng::XoroshiroRandomSource>,
        > = HashMap::new();
        let mut portal_randoms: HashMap<
            String,
            crate::rng::WorldgenRandom<crate::rng::XoroshiroRandomSource>,
        > = HashMap::new();
        let mut placement_loot = Vec::new();
        for (_, _, start) in &refs.entries {
            if !start.pieces_complete {
                continue;
            }
            if start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) {
                if let Some((step, index)) = registry.runtime_decoration_key(&start.structure) {
                    let structure_random = structure_randoms.entry(start.structure.clone()).or_insert_with(|| {
                        let mut random = crate::rng::WorldgenRandom::new(
                            crate::rng::XoroshiroRandomSource::new(0),
                        );
                        let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                        random.set_feature_seed(decoration_seed, index as i32, step);
                        random
                    });
                    let solid_render = |state: &str| {
                        self.veg_tags.simple_block_support.solid_render.test(state)
                    };
                    if let Some(mut loot) = registry.place_fortress_for_chunk_with(
                        start,
                        cx,
                        cz,
                        &mut world,
                        structure_random,
                        &solid_render,
                    ) {
                        placement_loot.append(&mut loot);
                        continue;
                    }
                }
            }
            // One `referencePos` per start, from its **first** piece's box, before
            // the per-piece loop — vanilla's own structure-start place-in-chunk step's own derivation. It
            // is not a per-piece value and it is not the chunk.
            let reference = crate::structure::jigsaw::reference_position(&start.pieces);
            for piece in &start.pieces {
                let portal_terrain_reaches = matches!(
                    piece.refine.as_ref(),
                    Some(PieceRefinement::RuinedPortalTerrain { .. })
                ) && piece
                    .bounding_box
                    .is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH);
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15)
                    && !portal_terrain_reaches
                {
                    continue;
                }
                if let Some(blocks) = &piece.blocks {
                    for block in blocks.iter() {
                        world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                    }
                }
                if let Some(placement) = &piece.placement {
                    let origin = crate::structure::template::PlaceOrigin {
                        position: placement.position,
                        reference,
                        seed,
                    };
                    placement
                        .template
                        .place(origin, &placement.settings, &mut world);
                    for extra in &piece.extra_placements {
                        let origin = crate::structure::template::PlaceOrigin {
                            position: extra.position,
                            reference,
                            seed,
                        };
                        extra.template.place(origin, &extra.settings, &mut world);
                    }
                }
                match piece.refine.as_ref() {
                    Some(PieceRefinement::FeaturePlacements { placements }) => {
                        let Some((step, index)) = registry.feature_placement_key(&start.structure) else {
                            continue;
                        };
                        let random = feature_randoms.entry(start.structure.clone()).or_insert_with(|| {
                            let mut random = crate::rng::WorldgenRandom::new(
                                crate::rng::LegacyRandomSource::new(0),
                            );
                            let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                            random.set_feature_seed(decoration_seed, index as i32, step);
                            random
                        });
                        crate::structure::feature_placement::place_feature_pool_elements(
                            random,
                            seed,
                            placements,
                            &mut world,
                            &self.veg_tags,
                        );
                    }
                    Some(PieceRefinement::RuinedPortalTerrain {
                        placement,
                        cold,
                        overgrown,
                        vines,
                        features_cannot_replace,
                    }) => {
                        let Some((step, index)) = registry.feature_placement_key(&start.structure) else {
                            continue;
                        };
                        let random = portal_randoms.entry(start.structure.clone()).or_insert_with(|| {
                            let mut random = crate::rng::WorldgenRandom::new(
                                crate::rng::XoroshiroRandomSource::new(0),
                            );
                            let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                            random.set_feature_seed(decoration_seed, index as i32, step);
                            random
                        });
                        crate::overworld::structures::place_ruined_portal_terrain(
                            &mut world,
                            piece.bounding_box,
                            random,
                            *placement,
                            *cold,
                            *overgrown,
                            *vines,
                            features_cannot_replace,
                        );
                    }
                    Some(PieceRefinement::StrongholdBlocks { writes }) => {
                        crate::structure::stronghold::place_post_surface_blocks(&mut world, writes);
                    }
                    Some(PieceRefinement::BuriedTreasureChest) | None => {}
                }
            }
        }
        (world, placement_loot)
    }

    /// Every start whose origin is `(cx, cz)` and whose piece list is complete —
    /// the set a save file may legitimately carry, and the one a gate asserts on.
    #[must_use]
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        self.structure_starts_stage(cx, cz)
            .iter()
            .filter(|start| start.pieces_complete)
            .map(Arc::clone)
            .collect()
    }

    /// Every start whose origin is `(cx, cz)`, including the advisory ones this
    /// engine can place but not build (`fortress`, `nether_fossil`,
    /// `ruined_portal_nether`). This is the *placement* answer — the one to compare
    /// against a vanilla save's `structures.starts` keys.
    #[must_use]
    pub fn structure_starts_including_incomplete(
        &self,
        cx: i32,
        cz: i32,
    ) -> Vec<Arc<StructureStart>> {
        self.structure_starts_stage(cx, cz).to_vec()
    }

    /// This chunk's `structures.References`, narrowed to vanilla's own 16×16
    /// intersection test — the NBT view rather than the beardifier's input.
    #[must_use]
    pub fn structure_references(
        &self,
        cx: i32,
        cz: i32,
    ) -> std::collections::BTreeMap<String, Vec<i64>> {
        let refs = self.structure_refs(cx, cz);
        let (bx, bz) = (cx * 16, cz * 16);
        let mut narrowed = StructureRefs::default();
        for (sx, sz, start) in &refs.entries {
            if start.pieces_complete
                && start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15)
            {
                narrowed.entries.push((*sx, *sz, Arc::clone(start)));
            }
        }
        narrowed.packed_by_structure()
    }

    /// The beard term this generator's real starts imply for `(cx, cz)` — the exact
    /// value the production fill uses, so a gate can assert *which* branch the fill
    /// took rather than inferring it from the output.
    #[must_use]
    pub fn beardifier(&self, cx: i32, cz: i32) -> Beardifier {
        let refs = self.structure_refs(cx, cz);
        self.beardifier_for(cx, cz, &refs)
    }

    /// The registry's unsupported ledger, or an empty map for a generator with no
    /// structure data.
    #[must_use]
    pub fn structure_ledger(&self) -> std::collections::BTreeMap<String, String> {
        self.structures
            .as_ref()
            .map(|r| r.unsupported().clone())
            .unwrap_or_default()
    }
}

/// Renders a noise-settings block-state object (`{"Name": …, "Properties": {…}}`)
/// as this engine's canonical `name[k=v,…]` string, properties **sorted by key**.
///
/// The properties are not decoration: `noise_settings/nether.json` carries
/// `"default_fluid": {"Name": "minecraft:lava", "Properties": {"level": "0"}}`,
/// and reading only `Name` yields `minecraft:lava` — a *different string* from the
/// `minecraft:lava[level=0]` [`crate::carver`] writes for the same state. One
/// column would then hold two palette entries for one block and every downstream
/// full-state match would miss for the bare form.
fn canonical_state_from_settings(value: &Value, fallback: &str) -> String {
    let Some(name) = value["Name"].as_str() else {
        return fallback.to_string();
    };
    match value["Properties"].as_object() {
        Some(properties) if !properties.is_empty() => {
            let mut rendered: Vec<String> = properties
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    format!("{key}={value}")
                })
                .collect();
            rendered.sort();
            format!("{name}[{}]", rendered.join(","))
        }
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use sha2::{Digest as _, Sha256};

    use super::{
        MixedEntryWriter, NetherGenerator, build_nether_feature_lists, decoration_random,
        lifecycle_pre_decoration_capacity, pre_decoration_capacity, synchronize_mixed_entry,
        ShardedMemo,
    };
    use crate::dense_grid::DenseBlockGrid;
    use crate::density::{NoiseParams, Resolver};
    use crate::feature::region_view::RegionView;
    use crate::feature::vegetation::VegGrid;
    use crate::interner::StateInterner;
    use crate::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};
    use serde_json::Value;

    #[test]
    fn lifecycle_cache_capacity_covers_the_admitted_18_by_18_closure() {
        let admissions = (-9..=8)
            .flat_map(|z| (-9..=8).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        assert_eq!(lifecycle_pre_decoration_capacity(&admissions), 484);
        assert_eq!(lifecycle_pre_decoration_capacity(&[(0, 0)]), 25);
        assert_eq!(lifecycle_pre_decoration_capacity(&[]), 1024);

        let computes = |capacity: usize| {
            let mut memo = HashSet::new();
            let mut computes = 0;
            let mut read = |chunk| {
                if memo.insert(chunk) {
                    if memo.len() > capacity {
                        memo.clear();
                        memo.insert(chunk);
                    }
                    computes += 1;
                }
            };
            for &chunk in &admissions {
                read(chunk);
            }
            for &(cx, cz) in &admissions {
                read((cx, cz));
                for dx in -2..=2 {
                    for dz in -2..=2 {
                        if dx != 0 || dz != 0 {
                            read((cx + dx, cz + dz));
                        }
                    }
                }
                read((cx, cz));
                for source_x in cx - 1..=cx + 1 {
                    for source_z in cz - 1..=cz + 1 {
                        for dx in -1..=1 {
                            for dz in -1..=1 {
                                read((source_x + dx, source_z + dz));
                            }
                        }
                    }
                }
            }
            computes
        };
        assert_eq!(computes(32), 5_534);
        assert_eq!(computes(484), 484);
    }

    #[test]
    fn packet_cache_capacity_covers_target_neighbour_prefix_closure() {
        let radius = crate::feature::region_view::WIDE_RADIUS;
        assert_eq!(pre_decoration_capacity(&[(-1, -1), (1, 1)], radius), 49);
        assert_eq!(
            pre_decoration_capacity(&[(-26, -26), (26, 26)], radius),
            3_249,
        );
    }

    #[test]
    fn sharded_memo_computes_one_value_for_a_concurrent_miss() {
        let memo = Arc::new(ShardedMemo::<usize>::new(1024));
        let computes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let memo = Arc::clone(&memo);
            let computes = Arc::clone(&computes);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                assert_eq!(
                    *memo.get_or_compute((17, -23), || {
                        computes.fetch_add(1, Ordering::Relaxed);
                        776
                    }),
                    776
                );
            }));
        }
        for worker in workers {
            worker.join().expect("memo worker must not panic");
        }
        assert_eq!(computes.load(Ordering::Relaxed), 1);
    }

    struct NetherAssets {
        root: PathBuf,
    }

    impl NetherAssets {
        fn read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
        }

        fn try_read(&self, kind: &str, id: &str) -> Value {
            let name = id.strip_prefix("minecraft:").unwrap_or(id);
            let path = self.root.join(kind).join(format!("{name}.json"));
            std::fs::read_to_string(&path).map_or(Value::Null, |text| {
                serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
            })
        }
    }

    impl Resolver for NetherAssets {
        fn density_function(&self, id: &str) -> Value { self.read("density_function", id) }
        fn noise(&self, id: &str) -> NoiseParams {
            let value = self.read("noise", id);
            NoiseParams {
                first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
                amplitudes: value["amplitudes"].as_array().expect("amplitudes")
                    .iter().map(|value| value.as_f64().expect("amplitude")).collect(),
            }
        }
        fn biome_parameters(&self) -> Value { self.read("biome_parameters", "nether") }
        fn biome_document(&self, id: &str) -> Value { self.try_read("biome", id) }
        fn configured_carver(&self, id: &str) -> Value { self.try_read("configured_carver", id) }
        fn configured_feature(&self, id: &str) -> Value { self.try_read("configured_feature", id) }
        fn placed_feature(&self, id: &str) -> Value { self.try_read("placed_feature", id) }
        fn block_tag(&self, id: &str) -> Value { self.try_read("tags/block", id) }
    }

    fn nether_assets_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")
    }

    fn grid_hash(
        grid: &VegGrid,
        center_x: i32,
        center_z: i32,
        local_lo: i32,
        local_hi: i32,
        min_y: i32,
        height: i32,
    ) -> String {
        let mut digest = Sha256::new();
        for lx in local_lo..local_hi {
            for lz in local_lo..local_hi {
                for y in min_y..min_y + height {
                    digest.update(format!("{lx},{y},{lz}={}\n", grid.get(center_x * 16 + lx, y, center_z * 16 + lz)).as_bytes());
                }
            }
        }
        format!("{:x}", digest.finalize())
    }

    /// The external trace drives all 3×3 step-4 entries before the centre
    /// source's step-7 index zero.  Hashing that identical bounded field keeps
    /// a terrain/step-4 mismatch from being blamed on its first blob modifier.
    #[test]
    fn captured_nether_pre_index0_field_matches_step4_output() {
        let assets = NetherAssets { root: nether_assets_root() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(assets.root.join("noise_settings/nether.json"))
                .expect("reading Nether settings"),
        ).expect("parsing Nether settings");
        let generator = NetherGenerator::new(42, &settings, &assets);
        let center_x = -250;
        let center_z = -250;
        let mut sources = HashMap::new();
        for dx in -2..=2 {
            for dz in -2..=2 {
                sources.insert((dx, dz), generator.pre_decoration_stage(center_x + dx, center_z + dz));
            }
        }
        let mut grid = VegGrid::with_sources(
            Arc::clone(&generator.interner), generator.min_y, generator.height,
            center_x * 16, center_z * 16,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |dx, dz| sources.get(&(dx, dz)).map(|source| Arc::clone(&source.0)),
        );
        generator.veg_tags.bind(grid.interner());
        for dx in -1..=1 {
            for dz in -1..=1 {
                let source_x = center_x + dx;
                let source_z = center_z + dz;
                let origin = crate::feature::BlockPos { x: source_x * 16, y: generator.min_y, z: source_z * 16 };
                let mut random = decoration_random();
                random.begin_decoration_source();
                let decoration_seed = random.set_decoration_seed(generator.seed, origin.x, origin.z);
                let biome = generator.biome_for_carver_source(source_x, source_z);
                let (_, decorations) = build_nether_feature_lists(&assets, biome);
                for (_, index, placed) in decorations.iter().filter(|(step, _, _)| *step == 4) {
                    if (source_x, source_z, *index) == (-251, -249, 0) {
                        assert_eq!(
                            grid_hash(
                                &grid, center_x, center_z,
                                crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
                                crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
                                generator.min_y, generator.height,
                            ),
                            "c057c17bc26d8d95038fce12a550f9d58468778cf9854b71465bda9e8eb23a07",
                            "the reference delta's padded input must be identical before local write order can be compared",
                        );
                        assert_eq!(grid.get(-4_018, 57, -3_972), "minecraft:netherrack");
                    }
                    crate::feature::vegetation::apply_decoration_entry_at_seed(
                        &mut random, decoration_seed, origin, 4, *index, placed, &mut grid, &generator.veg_tags,
                    );
                }
            }
        }
        let actual = grid_hash(
            &grid, center_x, center_z,
            crate::feature::REGION_MIN, crate::feature::REGION_MAX,
            generator.min_y, generator.height,
        );
        assert_eq!(actual, "56dc13107cdd7aafee3b0a299cc222bc0d8748e89200ff3cec2ef133fb501ef3");
    }

    #[test]
    fn decoration_rng_uses_xoroshiro_at_first_large_parity_chunk() {
        let mut decoration = decoration_random();
        let seed = decoration.set_decoration_seed(42, -4_000, -4_000);
        assert_eq!(seed, -2_931_187_344_497_052_822);
        decoration.set_feature_seed(seed, 0, 7);
        assert_eq!(decoration.next_int(), -1_735_331_230);

        let mut legacy = WorldgenRandom::new(LegacyRandomSource::new(0));
        let legacy_seed = legacy.set_decoration_seed(42, -4_000, -4_000);
        legacy.set_feature_seed(legacy_seed, 0, 7);
        assert_ne!(legacy_seed, seed, "control: legacy derives a different decoration stream");
        assert_ne!(legacy.next_int(), -1_735_331_230);
    }

    #[test]
    fn mixed_step7_ore_entry_uses_the_captured_xoroshiro_stream() {
        let mut ore = decoration_random();
        let seed = ore.set_decoration_seed(42, -4_000, -4_000);
        ore.set_feature_seed(seed, 9, 7);
        assert_eq!(ore.next_int(), 510_353_045);

        let mut legacy = WorldgenRandom::new(LegacyRandomSource::new(0));
        let legacy_seed = legacy.set_decoration_seed(42, -4_000, -4_000);
        legacy.set_feature_seed(legacy_seed, 9, 7);
        assert_ne!(legacy.next_int(), 510_353_045, "control: the terrain carrier is not a feature stream");
    }

    #[test]
    fn nether_surface_biome_zoom_distinguishes_containing_quart() {
        let assets = NetherAssets { root: nether_assets_root() };
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(assets.root.join("noise_settings/nether.json"))
                .expect("reading Nether settings"),
        ).expect("parsing Nether settings");
        let generator = NetherGenerator::new(42, &settings, &assets);
        let quarts = generator.biome_quarts(-2, -8);
        let containing = quarts[(11 / 4) * 4 + (13 / 4)].as_str();

        assert_eq!(containing, "minecraft:crimson_forest");
        assert_eq!(
            generator.biome_at_block(-2 * 16 + 13, 97, -8 * 16 + 11),
            "minecraft:nether_wastes",
        );
        assert_eq!(
            generator.biome_at_block(-2 * 16 + 12, 98, -8 * 16 + 11),
            "minecraft:nether_wastes",
        );
        assert_eq!(
            generator.biome_at_block(-2 * 16 + 14, 97, -8 * 16 + 12),
            "minecraft:crimson_forest",
        );
        assert_ne!(
            generator.biome_at_block(-2 * 16 + 13, 97, -8 * 16 + 11),
            containing,
            "control: containing-quart lookup cannot distinguish the accepted netherrack cell",
        );
    }

    #[test]
    fn nether_vegetation_biome_gate_has_positive_and_negative_candidate_controls() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let source = Arc::new(DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            0,
            0,
            0,
            16,
            128,
            16,
            air,
        ));
        let biome_quarts = std::array::from_fn(|index| {
            if index == 0 {
                "minecraft:crimson_forest".to_owned()
            } else {
                "minecraft:nether_wastes".to_owned()
            }
        });
        let cells = Arc::new(super::nether_biome_cells(
            &biome_quarts,
            0,
            super::DECORATION_WINDOW_HEIGHT,
        ));
        let feature_biomes = [(
            "minecraft:crimson_fungi".to_owned(),
            ["minecraft:crimson_forest".to_owned()].into_iter().collect(),
        )]
        .into_iter()
        .collect();
        let grid = VegGrid::with_sources_and_biomes(
            Arc::clone(&interner),
            0,
            super::DECORATION_WINDOW_HEIGHT,
            0,
            0,
            0,
            16,
            |_, _| Some(Arc::clone(&source)),
            |dx, dz| (dx == 0 && dz == 0).then(|| Arc::clone(&cells)),
            feature_biomes,
        );

        assert!(grid.biome_allows_placed_feature(
            Some("minecraft:crimson_fungi"),
            1,
            200,
            1,
        ));
        assert!(!grid.biome_allows_placed_feature(
            Some("minecraft:crimson_fungi"),
            5,
            200,
            1,
        ));
        assert!(!grid.biome_allows_placed_feature(
            Some("minecraft:warped_fungi"),
            1,
            200,
            1,
        ));
        assert!(!grid.biome_allows_placed_feature(None, 1, 200, 1));
    }

    fn mushroom_fixture_source() -> Arc<DenseBlockGrid> {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let netherrack = interner.id_of("minecraft:netherrack");
        let mut source = DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            -32,
            0,
            -32,
            80,
            128,
            80,
            air,
        );
        for z in -32..48 {
            for x in -32..48 {
                source.set_id(x, 127, z, netherrack);
            }
        }
        Arc::new(source)
    }

    fn mushroom_fixture_grid(source: &Arc<DenseBlockGrid>, height: i32) -> VegGrid {
        let interner = Arc::clone(source.interner());
        VegGrid::with_sources(
            Arc::clone(&interner),
            0,
            height,
            0,
            0,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |_, _| Some(Arc::clone(source)),
        )
    }

    fn mushroom_fixture_tags() -> crate::feature::vegetation::VegTags {
        let mut tags = crate::feature::vegetation::VegTags::default();
        tags.simple_block_support.solid_render =
            crate::feature::top_layer::StatePredicate::new(
                ["minecraft:netherrack".to_string()].into_iter().collect(),
                HashMap::new(),
            );
        tags
    }

    fn run_mushroom_fixture(
        source: &Arc<DenseBlockGrid>,
        height: i32,
        seed: i64,
        index: usize,
        placed: &crate::feature::vegetation::PlacedRef,
    ) -> Vec<(i32, i32, i32, String)> {
        let mut grid = mushroom_fixture_grid(source, height);
        let tags = mushroom_fixture_tags();
        tags.bind(grid.interner());
        let mut random = decoration_random();
        random.begin_decoration_source();
        let decoration_seed = random.set_decoration_seed(seed, 0, 0);
        crate::feature::vegetation::apply_decoration_entry_at_seed(
            &mut random,
            decoration_seed,
            crate::feature::BlockPos { x: 0, y: 0, z: 0 },
            crate::feature::STEP_VEGETAL_DECORATION,
            index,
            placed,
            &mut grid,
            &tags,
        );
        grid.dirty_cells()
            .map(|(x, y, z, state)| (x, y, z, state.to_owned()))
            .collect()
    }

    fn assert_normal_mushroom_contract(
        placed: &crate::feature::vegetation::PlacedRef,
        expected_state: &str,
    ) {
        let state = match placed.feature.as_ref() {
            crate::feature::vegetation::ConfiguredFeature::SimpleBlock(
                crate::feature::vegetation::BlockStateProvider::Simple(state),
            ) => state.as_str(),
            other => panic!("normal mushroom must be a simple block, got {other:?}"),
        };
        assert_eq!(state, expected_state);
        assert!(placed.placements.iter().any(|modifier| {
            matches!(
                modifier,
                crate::feature::vegetation::VegPlacement::Heightmap(
                    crate::feature::vegetation::HeightmapKind::MotionBlocking
                )
            )
        }));
        assert!(placed.placements.iter().any(|modifier| {
            matches!(
                modifier,
                crate::feature::vegetation::VegPlacement::Count(
                    crate::feature::IntProvider::Constant(96)
                )
            )
        }));
        assert!(placed.placements.iter().any(|modifier| {
            matches!(
                modifier,
                crate::feature::vegetation::VegPlacement::RandomOffset { .. }
            )
        }));
    }

    #[test]
    fn nether_mushroom_window_preserves_generator_depth_and_upper_spill() {
        let assets = NetherAssets { root: nether_assets_root() };
        let brown = crate::feature::vegetation::resolve_placed_feature_ref(
            &assets,
            &Value::String("minecraft:brown_mushroom_normal".to_owned()),
        );
        let red = crate::feature::vegetation::resolve_placed_feature_ref(
            &assets,
            &Value::String("minecraft:red_mushroom_normal".to_owned()),
        );
        assert_normal_mushroom_contract(&brown, "minecraft:brown_mushroom");
        assert_normal_mushroom_contract(&red, "minecraft:red_mushroom");

        let source = mushroom_fixture_source();
        let mut brown_case = None;
        for seed in 0..4096_i64 {
            let full = run_mushroom_fixture(
                &source,
                super::DECORATION_WINDOW_HEIGHT,
                seed,
                1,
                &brown,
            );
            if full
                .iter()
                .any(|(_, y, _, state)| *y >= 128 && state == "minecraft:brown_mushroom")
            {
                let narrow = run_mushroom_fixture(&source, 128, seed, 1, &brown);
                brown_case = Some((seed, full, narrow));
                break;
            }
        }
        let Some((seed, full, narrow)) = brown_case else {
            panic!("the normal brown mushroom fixture never found an upper placement");
        };
        assert!(
            full.iter().any(|(_, y, _, state)| {
                *y >= 128 && state == "minecraft:brown_mushroom"
            }),
            "the full resident window must retain the upper brown mushroom",
        );
        assert!(
            narrow.iter().all(|(_, y, _, _)| *y < 128),
            "control: the canonical 128-row terrain grid rejects the same upper write",
        );
        assert!(
            full.iter().all(|(_, _, _, state)| state == "minecraft:brown_mushroom"),
            "brown source must not be replaced by the red mushroom body",
        );

        let red_writes = run_mushroom_fixture(
            &source,
            super::DECORATION_WINDOW_HEIGHT,
            seed,
            2,
            &red,
        );
        assert!(
            red_writes
                .iter()
                .all(|(_, _, _, state)| state == "minecraft:red_mushroom"),
            "negative control: red's global index/body must never write brown state",
        );
    }

    #[test]
    fn mixed_entry_bridge_transfers_each_final_cell_once_in_both_directions() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let basalt = interner.id_of("minecraft:basalt[axis=y]");
        let blackstone = interner.id_of("minecraft:blackstone");
        let source = Arc::new(DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            -16,
            0,
            -16,
            48,
            8,
            48,
            air,
        ));
        let mut ore_view = RegionView::over_region_grid(&source, 0, 8);
        let mut grid = VegGrid::with_sources(
            Arc::clone(&interner),
            0,
            8,
            0,
            0,
            crate::feature::REGION_MIN,
            crate::feature::REGION_MAX,
            |_, _| Some(Arc::clone(&source)),
        );
        let mut grid_cursor = 0;
        let mut ore_cursor = 0;
        let mut ore_transferred = HashMap::new();

        assert!(grid.set_id_if_in_bounds(1, 1, 1, basalt));
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Decoration,
                &mut grid,
                &mut ore_view,
                0,
                0,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            1
        );
        assert_eq!(ore_view.get_id(1, 1, 1), basalt);
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Decoration,
                &mut grid,
                &mut ore_view,
                0,
                0,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            0,
            "control: a completed decoration entry is not replayed"
        );

        assert!(ore_view.set_id(2, 1, 2, blackstone));
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Ore,
                &mut grid,
                &mut ore_view,
                0,
                0,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            1,
            "only the new ore cell crosses back into the decoration grid"
        );
        assert_eq!(grid.get_id(2, 1, 2), blackstone);
        // A cell can be touched several times inside a later entry. The
        // bridge must inspect the final overlay value, not replay an
        // intermediate state that the complete overlay scan never exposed.
        assert!(ore_view.set_id(2, 1, 2, basalt));
        assert!(ore_view.set_id(2, 1, 2, blackstone));
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Ore,
                &mut grid,
                &mut ore_view,
                0,
                0,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            0,
            "control: unchanged ore overlay cells are not replayed"
        );
        assert!(ore_view.set_id(2, 1, 2, basalt));
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Ore,
                &mut grid,
                &mut ore_view,
                0,
                0,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            1,
            "a final overwrite with a new state crosses the bridge once"
        );
        assert_eq!(grid.get_id(2, 1, 2), basalt);
    }

    #[test]
    fn mixed_entry_bridge_converts_negative_region_view_coordinates_to_world_space() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let blackstone = interner.id_of("minecraft:blackstone");
        let centre_x = -250;
        let centre_z = -250;
        let source = Arc::new(DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            centre_x * 16 - 16,
            0,
            centre_z * 16 - 16,
            48,
            8,
            48,
            air,
        ));
        let mut ore_view = RegionView::over_sources(
            Arc::clone(&interner), centre_x, centre_z, 0, 8, |_, _| Some(&*source),
        );
        let mut grid = VegGrid::with_sources(
            Arc::clone(&interner),
            0,
            8,
            centre_x * 16,
            centre_z * 16,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |_, _| Some(Arc::clone(&source)),
        );
        let mut grid_cursor = 0;
        let mut ore_cursor = 0;
        let mut ore_transferred = HashMap::new();
        assert!(ore_view.set_id(-16, 4, -16, blackstone));

        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Ore,
                &mut grid,
                &mut ore_view,
                centre_x,
                centre_z,
                0,
                8,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut ore_transferred,
            ).projected,
            1,
        );
        assert_eq!(grid.get_id(-4_016, 4, -4_016), blackstone);
        assert_eq!(
            ore_transferred.get(&(-4_016, 4, -4_016)),
            Some(&blackstone),
            "the bridge's ownership map stores absolute coordinates too",
        );
    }

    #[test]
    fn mixed_entry_bridge_retains_step9_spill_outside_the_ore_window() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let basalt = interner.id_of("minecraft:basalt[axis=y]");
        let source = Arc::new(DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            -16,
            0,
            -16,
            48,
            8,
            48,
            air,
        ));
        let mut ore_view = RegionView::over_region_grid(&source, 0, 8);
        let mut grid = VegGrid::with_sources(
            Arc::clone(&interner),
            0,
            8,
            0,
            0,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |_, _| Some(Arc::clone(&source)),
        );
        let mut grid_cursor = 0;
        let mut ore_cursor = 0;
        let mut ore_transferred = HashMap::new();
        assert!(grid.set_id_if_in_bounds(-17, 1, 0, basalt));

        let sync = synchronize_mixed_entry(
            MixedEntryWriter::Decoration,
            &mut grid,
            &mut ore_view,
            0,
            0,
            0,
            8,
            &mut grid_cursor,
            &mut ore_cursor,
            &mut ore_transferred,
        );
        assert_eq!(sync.projected, 0);
        assert_eq!(sync.retained_outside, 1);
        assert_eq!(grid.get_id(-17, 1, 0), basalt, "padded spill remains for final decoration output");
        assert_eq!(ore_view.get_id(-17, 1, 0), air, "the bounded ore reader does not fabricate a fourth source column");
    }

    #[test]
    fn mixed_entry_bridge_retains_upper_half_spill_outside_canonical_terrain() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let brown = interner.id_of("minecraft:brown_mushroom");
        let source = Arc::new(DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            -16,
            0,
            -16,
            48,
            128,
            48,
            air,
        ));
        let mut ore_view = RegionView::over_region_grid(&source, 0, 128);
        let mut grid = VegGrid::with_sources(
            Arc::clone(&interner),
            0,
            super::DECORATION_WINDOW_HEIGHT,
            0,
            0,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |_, _| Some(Arc::clone(&source)),
        );
        let mut grid_cursor = 0;
        let mut ore_cursor = 0;
        let mut ore_transferred = HashMap::new();
        assert!(grid.set_id_if_in_bounds(1, 128, 1, brown));

        let sync = synchronize_mixed_entry(
            MixedEntryWriter::Decoration,
            &mut grid,
            &mut ore_view,
            0,
            0,
            0,
            128,
            &mut grid_cursor,
            &mut ore_cursor,
            &mut ore_transferred,
        );
        assert_eq!(sync.projected, 0);
        assert_eq!(sync.retained_outside, 1);
        assert_eq!(grid.get_id(1, 128, 1), brown);
        assert_eq!(ore_view.get_id(1, 128, 1), air);
    }
}
