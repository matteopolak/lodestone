//! Composed overworld chunk generation: the version-free driver that chains the
//! proven stages into a single "give me the blocks in chunk `(cx, cz)`" call.
//!
//! Everything below this module is a *stage* proven bit-for-bit against a JVM in
//! isolation (`region_parity`, `chunk_parity`, `surface_parity`, `carver_parity`,
//! `feature_parity`, `aquifer_parity`). This module is the glue that runs them in
//! sequence so a caller — the integrated server, or the shell's local world —
//! gets real terrain instead of a stand-in. It holds **no data**: the noise
//! settings `Value` and every density function / noise / carver / feature it
//! references arrive through a [`Resolver`], exactly as the parity tests supply
//! them, so the engine stays version-free (plan §3).
//!
//! # Composed pipeline (issue #295), and vanilla's own order
//!
//! `NoiseBasedChunkGenerator`'s real order is `fillFromNoise` (shape + the real
//! aquifer, the aquifer participating *inside* fill rather than after it) ->
//! per-quart biome resolution -> `buildSurface` -> `applyCarvers` -> feature
//! decoration. [`column`](Self::column) reproduces that order exactly:
//!
//! 1. **Fill** — [`AquiferSystem::block_at`] evaluates the interpolated
//!    `final_density` field *and* the real aquifer's barrier/floodedness/
//!    spread/lava routing together (`computeSubstance`), the same code
//!    `aquifer_parity` proves block-for-block against the JVM. This replaces
//!    the sea-level-only fluid approximation this generator used before #295:
//!    underground water/lava pockets now come from the real aquifer, not just
//!    "below sea level ⇒ water."
//! 2. **Biome** — one climate sample per quart, unchanged from #405 (real
//!    multi-noise biome variety), now sampling the fill stage's real
//!    solid-top heightmap.
//! 3. **Surface** — [`SurfaceSystem::build_surface`], unchanged from #405,
//!    now consuming the real aquifer's fill instead of the approximation.
//! 4. **Carve** — [`crate::carver::apply_carvers`] over a materialised
//!    world-keyed block grid, replicating vanilla's real per-source-chunk
//!    `carverBiome` resolution (each of the 17×17 source chunks in the carve
//!    neighbourhood gets its own biome — and therefore its own carver list —
//!    sampled at that source chunk's quart corner and `y = 0`, **not** its
//!    surface height; carver selection is a different question from surface
//!    material). See [`crate::carver::apply_carvers`]'s doc comment.
//!
//! 5. **Ore features** — [`Self::ore_stage`] runs
//!    [`crate::feature::apply_ore_step_3x3_per_source`], vanilla's real 3×3
//!    neighbourhood `UNDERGROUND_ORES` driver: each of the 9 chunks in
//!    `center ± 1` gets its own full pre-ore pipeline (stages 1-4 above,
//!    via [`Self::pre_ore_stage`]) and its own biome-resolved ore list (the
//!    same per-source-chunk convention [`Self::biome_for_carver_source`]
//!    already uses for carvers), and every one of the 9 passes writes into
//!    one shared region grid before the centre 16×16 is folded back in —
//!    matching vanilla's real `blockStateWriteRadius(1)` spill, not an
//!    approximation of it.
//!
//! This landed after an architecture review found that `FeatureOracle.java`
//! — the oracle `feature_parity` validates the ore *engine* against —
//! originally shared the very simplification it was supposed to be
//! checking (it used to not model neighbour spill at all); that oracle bug
//! was fixed first (`7f97ca1`), and this module's own composition second,
//! deliberately in that order — composing against a wrong oracle would have
//! baked a wrong edge band into every chunk with no gate able to see it.
//!
//! **What composing the real 3×3 driver actually measured, and why the gap
//! against `postfeatures` did not go to (near) zero the way carve's gap
//! against `postcarve` did.** `ComposedChunkOracle.java`'s `postfeatures`
//! stage is *single-source only* (it never extends to a real 3×3 with real
//! per-quart biome variety — that would need 8 more fully-generated real
//! chunks per fixture dump, not attempted; see that file's own doc comment).
//! A debug-only toggle (`LODESTONE_ORE_SINGLE_SOURCE_DEBUG=1`, in
//! [`Self::ore_stage`]) reproduces that oracle's own narrower scope and
//! measured a much smaller residual against it (563/98304 at chunk (0,0),
//! down from the pre-composition 4113) — evidence the *engine* is correct
//! and that most of the *full* 3×3 gap against `postfeatures` (2237/98304 at
//! the same chunk) is real vanilla ore spill this oracle stage cannot model,
//! not a defect. See `docs/worldgen-parity.md` for the full per-chunk
//! numbers, including the one fixture chunk ((-120,-120)) where the gap
//! against `postfeatures` genuinely *worsened*: that chunk's real biome is
//! badlands (see "Badlands" below), so composing ores there places the
//! *wrong* biome's ore list, not merely an incomplete one — confirmed
//! directly by a whole missing ore type (`badlands.json`'s
//! `UNDERGROUND_ORES` step names `minecraft:ore_gold_extra`, badlands' bonus
//! gold vein, which no substitute biome's list contains).
//!
//! **Still not composed:** structures (unbuilt anywhere in this repo,
//! `#136`). Vegetation/tree features WERE still-not-composed when this line
//! was first written, but issue #406 built and composed them
//! (`Self::vegetation_stage`) and issue #427 gave that stage the real 3×3
//! `blockStateWriteRadius(1)` driver every other decoration stage already
//! had — this sentence just never got updated, which is itself the thing
//! CLAUDE.md's §2 keeps warning stale claims in this file are prone to.
//! `docs/worldgen-parity.md` measures the composed subset
//! (shape + real aquifer + biome + surface + carvers + ores + vegetation)
//! against a real
//! vanilla JVM.
//!
//! # Performance (issue #295's Job 2), and an honest miss
//!
//! **A correctness bug this refactor introduced, found and fixed before
//! landing.** A [`crate::dense_grid::DenseBlockGrid`]'s palette is built
//! incrementally, in `.set()` call order — unlike the `HashMap`-keyed
//! `world` it replaced, whose palette used to be assigned by a *separate*,
//! fixed-order final pass regardless of how `world` itself was populated.
//! [`Self::materialize_world`] originally applied `surface_diff` (a
//! `HashMap<(i32,i32,i32), String>`, fresh per chunk) by iterating it
//! directly — and `std::collections::HashMap` iteration order is not
//! guaranteed stable even across two *separately constructed* maps with
//! identical content (`RandomState` reseeds per map). Two independent
//! `column()` calls for the *same* chunk therefore produced the same blocks
//! at the same positions but a **different palette order** — same terrain,
//! different bytes. Caught by
//! `lodestone_server::worldgen_data::tests::column_is_byte_identical_across_two_independently_constructed_generators`
//! (added as a permanent regression control, no threading involved) after
//! it was first surfaced by `lodestone-server`'s own
//! `chunk::tests::parallel_generation_is_deterministic_and_matches_serial`
//! (issue #414, a different agent's concurrently-landed feature — confirmed
//! via an isolated `git worktree` at the commit *before* this crate's ore
//! composition that the failure did not exist there, ruling out a
//! threading bug in that test's own new code before spending time on it).
//! Fixed by consulting `surface_diff` with a point lookup inside the same
//! fixed `(lz, lx, ly)` loop the base fill already uses, never iterating it.
//!
//! The working grid every stage above writes into is
//! [`crate::dense_grid::DenseBlockGrid`] — a flat, palette-indexed array —
//! not a `HashMap<(i32,i32,i32), String>`. `materialize_world` builds the
//! dense grid directly; [`crate::carver::CarveGrid`] wraps it with no copy
//! (`from_dense`/`into_dense`); [`Self::intern_from_dense`] adopts the
//! finished grid's own palette/blocks straight into [`GeneratedColumn`] with
//! no second interning pass. A debug-only toggle
//! (`LODESTONE_CARVE_HASHMAP_DEBUG=1`, in [`Self::carve_stage`]) forces the
//! old `HashMap` round trip for direct comparison, measured (debug,
//! single-threaded, radius-1/9-chunk patch): **4782us → 4173us mean/chunk,
//! ~12.7% faster**; parallel wall/chunk (10 threads) 892us → 799us, ~11.6%
//! faster. Real, and the right shape of fix — but **not** what closes the
//! gap to the historical "144-chunk sweep: sub-second → ~68s in debug"
//! regression that motivated this section, because that regression was
//! carve-only, pre-ore-composition. Composing the real 3×3 ore driver
//! (stage 5 above) adds its own ~9× multiplier on top — 9 full pre-ore
//! pipeline recomputations per `column()` call (1 centre + 8 neighbours,
//! each needing its own real post-carve terrain/heightmap for correctness,
//! not an approximation) — which dominates over the `HashMap`-vs-array
//! delta. Measured directly on the actual 144-chunk sweep this section's
//! history refers to
//! (`lodestone_server::worldgen_data::tests::served_columns_never_carry_an_unported_badlands_variant`,
//! a 12×12 chunk loop, `crates/lodestone-server/src/worldgen_data.rs`) —
//! `cargo test -p lodestone-server --lib` (debug, whose total wall time is
//! dominated by this one test among 129) measured **700.57s**, versus the
//! documented pre-ore-composition ~68s: **~10× worse**, close to the
//! predicted ~9× (1 centre + 8 neighbours) rather than an unexplained
//! blow-up. **This is not fully solved.** The dense grid is real and worth keeping; the
//! dominant remaining cost is structural — `ore_stage` has no cache across
//! adjacent chunks in a sweep (exactly the access pattern a real server/
//! shell has), so neighbour work that's shared between two adjacent
//! `column()` calls is redone from scratch every time. A per-generator
//! neighbour cache (safe to memoize — generation is pure/deterministic) was
//! the natural next step, and has since **LANDED** — see [`PreOreCache`]/
//! [`PostOreCache`] and [`Self::pre_ore_stage`] a few hundred lines below
//! (`6509a97`). The rest of this paragraph is kept as the argument that
//! produced it, because it named the real design constraint correctly:
//! [`OverworldGenerator`] is used from multiple threads
//! (`chunk::tests::parallel_generation_is_deterministic_and_matches_serial`
//! in `lodestone-server` exercises this directly), so a correct cache needs
//! real interior-mutability design rather than something bolted on under
//! time pressure. The one remaining `HashMap<(i32,i32,i32), String>` in the
//! hot path regardless is the ore region grid
//! `crate::feature::apply_ore_step_3x3_per_source` itself expects (proven
//! against `feature_parity`'s fixture-driven `HashMap` shape) — narrowing
//! that engine's own signature to a dense grid too is further work, also
//! not attempted in this pass.
//!
//! # Badlands (issue #405's carried-over gap, now closed)
//!
//! `minecraft:badlands`/`eroded_badlands`/`wooded_badlands` used to be
//! excluded from the searchable biome table
//! (`crate::biome::usable_overworld_table`) because their surface rule
//! reached an unported `SurfaceSystem.getBand` subsystem that would panic.
//! `getBand` is now ported (`crate::surface::Rule::Bandlands`) and the
//! exclusion is removed (`usable_overworld_table` is a pass-through), so a
//! column can resolve to any of the three real names again — which means
//! the per-source-chunk carver biome and ore biome (both driven through the
//! same table [`Self::biome_for_carver_source`] resolves) now see them too,
//! closing the specific gap `docs/worldgen-parity.md` measured: chunk
//! `(-120,-120)`'s real vanilla biome is badlands, and the substitute biome
//! this exclusion used to force could never carry badlands' bonus
//! `ore_gold_extra` gold vein (51 blocks at that chunk, measured zero
//! before).

//! # File layout (U16 Phase A)
//!
//! This module was one 1,873-line file until the decomposition unit split it along
//! the stage seams `column`/`column_timed` already called. Nothing moved but text:
//!
//! * this file — the generator struct, `new`, the `column`/`column_timed`
//!   orchestration and the two memo caches;
//! * [`fill`] — stages 1-4 (aquifer, shape, surface, materialise, carve);
//! * [`biome`] — the climate/biome resolution stages;
//! * [`decorate`] — stages 5-7 (ore, vegetation, top layer) and their stitches;
//! * [`output`] — [`GeneratedColumn`] and [`StageTimes`], the read-mostly result types.

mod biome;
mod decorate;
mod fill;
mod output;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::biome::ClimateSampler;
use crate::carver::CarverConfig;
use crate::density::{Builder, Resolver};
use crate::feature::PlacedOre;
use crate::surface::{SurfaceSystem, identity_canon};

use self::biome::DynamicBiome;
use self::fill::AquiferTrees;

pub use self::output::GeneratedColumn;
#[cfg(not(target_arch = "wasm32"))]
pub use self::output::StageTimes;

/// The return shape of [`OverworldGenerator::pre_ore_stage`] — one chunk's
/// own post-carve world, heightmap and biome quarts (stages 1-4). Named so
/// [`OverworldGenerator::pre_ore_cache`]'s value type reads as "one chunk's
/// pre-ore result", not an anonymous 3-tuple.
type PreOreResult = (crate::dense_grid::DenseBlockGrid, [i32; 256], [(String, bool); 16]);

/// Bound on [`PreOreCache`]'s size. `OverworldGenerator` is not only used by
/// one-shot sweeps (this cache's original motivation): `lodestone_server`'s
/// `OverworldChunkSource` holds one generator for a whole world's lifetime
/// ("share it across the view", its own doc comment), so a session that
/// gradually explores a large area would otherwise grow this cache without
/// bound — each entry holds a full `16×height×16` `DenseBlockGrid`, on the
/// order of 200 KiB, so unbounded growth here is a real, if slow, memory
/// leak on a machine `CLAUDE.md` already flags memory as the binding limit
/// on. 512 entries (~100 MiB worst case) comfortably covers the 14×14 = 196
/// unique chunks the 144-chunk sweep this cache was built for actually needs
/// (its 12×12 centres plus their shared 1-chunk halo), with headroom to
/// spare — and an eviction only ever costs a recompute on the next request
/// for that key, never a wrong answer (see [`OverworldGenerator::pre_ore_stage`]'s
/// own doc comment on why the *value* per key never changes).
const PRE_ORE_CACHE_CAPACITY: usize = 512;

/// [`OverworldGenerator::pre_ore_cache`]'s storage: the memoisation map plus
/// a FIFO insertion-order queue so eviction can drop the oldest key once the
/// map exceeds [`PRE_ORE_CACHE_CAPACITY`]. Plain FIFO rather than true LRU
/// (which would need to reorder on *read*, not just insert) because the
/// access pattern this exists for — a centre's own [`Self::column`] call plus
/// [`Self::ore_stage`]'s 8-neighbour sweep, repeated across a scan that
/// advances roughly in coordinate order — makes insertion order and recency
/// order coincide closely enough that the extra bookkeeping of true LRU
/// would not measurably change the hit rate.
#[derive(Default)]
struct PreOreCache {
    map: HashMap<(i32, i32), Arc<PreOreResult>>,
    order: std::collections::VecDeque<(i32, i32)>,
}

/// [`OverworldGenerator::post_ore_cache`]'s storage — same FIFO shape as
/// [`PreOreCache`], for the same reason: [`Self::vegetation_stage`]'s 3×3
/// driver (issue #427) calls [`Self::post_ore_world`] for 8 neighbours on
/// every `column()` call, and a sweep over adjacent chunks asks for the
/// *same* neighbour's post-ore world repeatedly (centre `(cx,cz)`'s own
/// vegetation pass computes `(cx+1,cz)`'s post-ore world; `(cx+1,cz)`'s own
/// `column()` call needs that exact same value for itself). Without this
/// cache, [`Self::ore_stage`]'s own real 3×3 `UNDERGROUND_ORES` RNG-driver
/// (not just the memoised [`Self::pre_ore_stage`] terrain feeding it) would
/// be recomputed from scratch for every `(neighbour, requester)` pair in a
/// sweep — a further, uncached 9× on top of the 9× `docs/worldgen-parity.md`
/// already measured for ore composition alone (700.57s for a 12×12 debug
/// sweep), which this cache exists specifically to avoid multiplying again.
#[derive(Default)]
struct PostOreCache {
    map: HashMap<(i32, i32), Arc<crate::dense_grid::DenseBlockGrid>>,
    order: std::collections::VecDeque<(i32, i32)>,
}

/// A composed, reusable overworld generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
#[allow(missing_debug_implementations)]
pub struct OverworldGenerator {
    /// Shared slot-index upper bound for every `Density` tree this generator
    /// built (final_density, surface, climate, aquifer) — see
    /// [`AquiferTrees`]'s doc comment.
    slot_count: usize,
    surface: SurfaceSystem,
    /// Block-state string to [`crate::interner::StateId`] table, shared by
    /// **every** grid this generator builds — that sharing is what lets a cell
    /// move between two grids as a `u16` instead of a fresh `String`, which is
    /// where Unit 3's 884,736-allocations-per-column saving comes from
    /// (`docs/plans/worldgen-rewrite.md` D2).
    ///
    /// Owned per generator rather than globally, and it outlives every column,
    /// so interning is a warmup cost and steady-state serving allocates nothing
    /// here. See [`crate::interner`]'s module doc for why id-assignment order
    /// cannot reach the wire.
    interner: Arc<crate::interner::StateInterner>,
    min_y: i32,
    height: i32,
    sea_level: i32,
    default_block: String,
    default_fluid: String,
    /// Vanilla hardcodes lava as the aquifer's second fluid regardless of the
    /// dimension's configured `default_fluid` (`Aquifer.FluidStatus` built
    /// from `Blocks.LAVA.defaultBlockState()`, not from `NoiseGeneratorSettings`)
    /// — not a simplification, this is vanilla's own behaviour.
    default_lava: String,
    /// The biome (and its `coldEnoughToSnow` answer) used for every column
    /// when [`Self::dynamic_biome`] is `None` — i.e. exactly the whole-world
    /// behaviour this generator had before issue #405, kept as the fallback
    /// a [`Resolver`] with no biome data still gets.
    fallback_biome: String,
    fallback_cold_enough_to_snow: bool,
    /// `None` unless `resolver.biome_parameters()` returned a non-empty
    /// table, in which case every column samples real climate instead of
    /// using the fallback above.
    dynamic_biome: Option<DynamicBiome>,
    seed: i64,
    aquifer_trees: AquiferTrees,
    /// `#overworld_carver_replaceables` tag closure (issue #295) — which
    /// blocks a carver is allowed to overwrite. Empty when the [`Resolver`]
    /// supplies no tag data (`Resolver::block_tag`'s default), in which case
    /// `carver::apply_carvers`'s own `can_replace` is always false and
    /// carving becomes a harmless no-op rather than a panic — matching the
    /// "no data supplied" convention every #295 resolver method establishes.
    carver_replaceable: HashSet<String>,
    /// Per-biome carver list, resolved once at construction for every biome
    /// name the [`Resolver`]'s biome-parameter table (or the fallback biome)
    /// can produce — see `crate::compose::build_biome_carvers`.
    carvers_by_biome: HashMap<String, Vec<CarverConfig>>,
    /// Per-biome `UNDERGROUND_ORES` list (issue #295), resolved the same way
    /// and at the same time as `carvers_by_biome` — see
    /// `crate::compose::build_biome_ores`. Empty (whole map) when the
    /// resolver supplies no biome documents with an ore step, in which case
    /// [`Self::ore_stage`] is a no-op (matches every other #295 resolver
    /// "no data supplied" convention).
    ores_by_biome: HashMap<String, Vec<PlacedOre>>,
    /// Block-tag closures for every tag referenced by any biome's ore
    /// targets, resolved once — see `crate::compose::build_ore_tag_map`.
    ore_tag_map: HashMap<String, HashSet<String>>,
    /// Per-generator memoisation of [`Self::pre_ore_stage`] (issue #295's Job
    /// 1 performance follow-up, see this module's doc "Performance"
    /// section). [`Self::ore_stage`]'s real 3×3 driver calls
    /// `pre_ore_stage` for the centre plus all 8 neighbours on *every*
    /// [`column`](Self::column) call, with no memoisation across calls — so a
    /// 144-chunk sweep, where every interior chunk is somebody else's
    /// neighbour up to 8 times, redid the same full pre-ore pipeline up to
    /// 9× (measured: a 144-chunk sweep went from ~68s to 700.57s in debug
    /// after ore composition landed, matching the predicted ~9×). This cache
    /// makes each chunk's pre-ore result pay for its own computation exactly
    /// once per generator, regardless of whether it is first reached as a
    /// centre or as a neighbour.
    ///
    /// Keyed by the **exact** chunk coordinate, never clamped or rounded —
    /// see [`Self::pre_ore_stage`]'s own doc comment for why a clamped
    /// equivalent is a known failure shape (it aliased two distinct chunks
    /// onto one cached value in a JVM oracle and hung the process). `Arc`
    /// so a hit hands back a cheap pointer clone rather than a clone of the
    /// `DenseBlockGrid`'s own `Vec`s; `Mutex`-protected because
    /// [`OverworldGenerator`] is shared across threads by
    /// `lodestone_server::chunk::generate_columns_parallel` — the value
    /// behind each key is immutable-after-insert (a pure function of
    /// `(cx, cz)` and this generator's own fixed state), so the lock only
    /// ever guards *insertion*, never a mutation raced against a reader.
    /// Bounded — see [`PreOreCache`] and [`PRE_ORE_CACHE_CAPACITY`].
    pre_ore_cache: Mutex<PreOreCache>,
    /// Memoises [`Self::post_ore_world`] (issue #427's vegetation 3×3
    /// driver) — same shape, same bound, same rationale as `pre_ore_cache`
    /// one level up the pipeline; see [`PostOreCache`]'s own doc comment for
    /// why this one specifically matters (it caches the expensive *ore
    /// placement RNG walk*, not just the cheap-to-clone pre-ore terrain
    /// `pre_ore_cache` already covers).
    post_ore_cache: Mutex<PostOreCache>,
    /// Per-biome `VEGETAL_DECORATION` list (issue #406), resolved the same
    /// way and at the same time as `ores_by_biome` — see
    /// `crate::compose::build_biome_vegetation`. Empty (whole map) when the
    /// resolver supplies no biome documents with a vegetation step, in
    /// which case [`Self::vegetation_stage`] is a no-op, matching every
    /// other #295/#406 resolver "no data supplied" convention.
    vegetation_by_biome: HashMap<String, Vec<(usize, crate::feature::vegetation::PlacedRef)>>,
    /// Block-tag closures [`crate::feature::vegetation`]'s own predicates/
    /// checks need (`supports_vegetation`, `replaceable_by_trees`, `logs`,
    /// `cannot_replace_below_tree_trunk`) — resolved once, analogous to
    /// `ore_tag_map` but via `crate::feature::vegetation::build_veg_tags`
    /// rather than a per-ore-target walk (this module's own tag set is
    /// fixed, not data-dependent — see that function's doc comment).
    veg_tags: crate::feature::vegetation::VegTags,
    /// Per-biome `ClimateSettings` (`has_precipitation`, `temperature`,
    /// `temperature_modifier`), read straight out of each biome's own
    /// `Resolver::biome_document` — see
    /// [`crate::feature::top_layer::parse_biome_climate`] for why no new
    /// resolver method was needed. Only populated for biomes whose document
    /// carries a `temperature` field; a biome absent here does not freeze.
    biome_climates: HashMap<String, crate::feature::top_layer::BiomeClimate>,
    /// Which biomes list `minecraft:freeze_top_layer` in their
    /// `TOP_LAYER_MODIFICATION` step. In vanilla 26.2 that is **every** biome
    /// (`BiomeDefaultFeatures.java:413`), so this is not really a filter — it is
    /// there so a trimmed or modified datapack that omits the step gets a
    /// snow-free world rather than snow this engine invented.
    freeze_biomes: HashSet<String>,
    /// The five per-block-state predicates plus two tags
    /// [`Self::top_layer_stage`] needs. Empty (making the stage a no-op) when
    /// the resolver supplies no `block_freeze_facts` — the same "no data
    /// supplied" convention as every field above.
    snow_support: crate::feature::top_layer::SnowSupport,
    /// `Biome`'s three climate noise fields, built once per generator rather
    /// than per column — see [`crate::noise::ClimateNoise`]. Only read by
    /// [`Self::top_layer_stage`], and cheap enough (~780 draws) to build
    /// unconditionally.
    climate_noise: crate::noise::ClimateNoise,
}

impl OverworldGenerator {
    /// Builds the generator for `seed` from a noise-settings `Value` and a
    /// [`Resolver`] that supplies the density functions, noises, carvers,
    /// features and tags it references.
    ///
    /// `biome` is the fallback biome id (e.g. `"minecraft:plains"`) used for
    /// every column when `resolver` supplies no real biome-parameter table
    /// (`resolver.biome_parameters()` empty, the default — see
    /// [`Resolver::biome_parameters`]); `cold_enough_to_snow` is that
    /// biome's answer. A resolver that overrides `biome_parameters`/
    /// `biome_temperatures` (the bundled singleplayer generator does) gets
    /// real per-column biome variety instead, and these two arguments are
    /// then unused except as a documentation of "what this used to always
    /// be."
    #[must_use]
    pub fn new(
        seed: i64,
        settings: &Value,
        resolver: &dyn Resolver,
        biome: &str,
        cold_enough_to_snow: bool,
    ) -> Self {
        let builder = Builder::new(seed, resolver);
        let router = &settings["noise_router"];
        let final_density = builder.build(&router["final_density"]);
        let canon = identity_canon(settings);
        let surface = SurfaceSystem::new(settings, &builder, &canon);

        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(63) as i32;
        let default_block = settings["default_block"]["Name"]
            .as_str()
            .unwrap_or("minecraft:stone")
            .to_string();
        let default_fluid = settings["default_fluid"]["Name"]
            .as_str()
            .unwrap_or("minecraft:water")
            .to_string();
        let default_lava = "minecraft:lava".to_string();

        let raw_table = crate::biome::parse_table(&resolver.biome_parameters());
        let dynamic_biome = if raw_table.is_empty() {
            None
        } else {
            let table = crate::biome::usable_overworld_table(raw_table);
            let temperatures = crate::biome::parse_temperatures(&resolver.biome_temperatures());
            let climate = ClimateSampler::new(settings, &builder);
            Some(DynamicBiome {
                climate,
                table,
                temperatures,
            })
        };

        // Aquifer support trees (issue #295) — built via the same shared
        // `builder` as final_density/surface/climate above; see
        // `AquiferTrees`'s doc comment for why `slot_count` is captured only
        // after every one of these `builder.build()` calls.
        let aquifer_trees = AquiferTrees {
            final_density,
            erosion: builder.build(&router["erosion"]),
            depth: builder.build(&router["depth"]),
            barrier: builder.build(&router["barrier"]),
            floodedness: builder.build(&router["fluid_level_floodedness"]),
            spread: builder.build(&router["fluid_level_spread"]),
            lava: builder.build(&router["lava"]),
            prelim: builder.build(&router["preliminary_surface_level"]),
            positional: {
                use crate::rng::{PositionalRandomFactory, RandomSource};
                let mut src = builder
                    .positional_factory()
                    .from_hash_of("minecraft:aquifer");
                src.fork_positional()
            },
        };

        // Carver-replaceable tag closure (issue #295): without this
        // populated, every carve write is rejected (`can_replace` always
        // false) — the same trap `CarverOracle.java`'s own header warns
        // about for the isolated oracle.
        let mut carver_replaceable = HashSet::new();
        {
            let mut seen = HashSet::new();
            crate::compose::resolve_block_tag(
                resolver,
                "minecraft:overworld_carver_replaceables",
                &mut carver_replaceable,
                &mut seen,
            );
        }

        // Per-biome carver composition data (issue #295): resolved once for
        // every biome name that can appear (every distinct name in the usable
        // biome table, plus the fallback biome) — a handful of JSON parses at
        // construction time, not one per chunk or per source-chunk. Ore
        // features are deliberately not resolved here yet — see the module
        // doc.
        let mut biome_names: std::collections::BTreeSet<String> = dynamic_biome
            .as_ref()
            .map(|d| d.table.iter().map(|p| p.biome.clone()).collect())
            .unwrap_or_default();
        biome_names.insert(biome.to_string());

        let mut carvers_by_biome = HashMap::new();
        let mut ores_by_biome = HashMap::new();
        let mut vegetation_by_biome = HashMap::new();
        // Issue #404's U2: the same per-biome document walk also yields each
        // biome's `ClimateSettings` and whether it lists `freeze_top_layer`, so
        // `TOP_LAYER_MODIFICATION` composition costs no extra JSON parses.
        let mut biome_climates = HashMap::new();
        let mut freeze_biomes = HashSet::new();
        for name in &biome_names {
            carvers_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_carvers(resolver, name),
            );
            ores_by_biome.insert(name.clone(), crate::compose::build_biome_ores(resolver, name));
            vegetation_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_vegetation(resolver, name),
            );
            let document = resolver.biome_document(name);
            if let Some(climate) = crate::feature::top_layer::parse_biome_climate(&document) {
                biome_climates.insert(name.clone(), climate);
            }
            if crate::compose::biome_lists_freeze_top_layer(&document) {
                freeze_biomes.insert(name.clone());
            }
        }
        let all_ores: Vec<PlacedOre> = ores_by_biome.values().flatten().cloned().collect();
        let ore_tag_map = crate::compose::build_ore_tag_map(resolver, &all_ores);
        let veg_tags = crate::feature::vegetation::build_veg_tags(resolver);
        let snow_support = crate::feature::top_layer::build_snow_support(resolver);

        // Captured last, after every `builder.build()` call above (shape,
        // surface, climate, the eight aquifer trees) — see `AquiferTrees`'s
        // doc comment for why this is always a safe bound.
        let slot_count = builder.slot_count();

        Self {
            slot_count,
            surface,
            // Fresh per generator. Deliberately *not* pre-populated from the
            // resolver's data: the allocation budget is written against a
            // steady-state column, by which point every state the data can
            // produce has been interned by ordinary generation. Pre-interning
            // would only move cold-start work around, and a pre-intern list
            // that drifted out of sync with the data would be a stale claim of
            // exactly the kind CLAUDE.md's rule 2 is about.
            interner: Arc::new(crate::interner::StateInterner::new()),
            min_y,
            height,
            sea_level,
            default_block,
            default_fluid,
            default_lava,
            fallback_biome: biome.to_string(),
            fallback_cold_enough_to_snow: cold_enough_to_snow,
            dynamic_biome,
            seed,
            aquifer_trees,
            carver_replaceable,
            carvers_by_biome,
            ores_by_biome,
            ore_tag_map,
            pre_ore_cache: Mutex::new(PreOreCache::default()),
            post_ore_cache: Mutex::new(PostOreCache::default()),
            vegetation_by_biome,
            veg_tags,
            biome_climates,
            freeze_biomes,
            snow_support,
            climate_noise: crate::noise::ClimateNoise::new(),
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

    /// Sea level (fluid fill height).
    #[must_use]
    pub fn sea_level(&self) -> i32 {
        self.sea_level
    }

    /// Generates the block field for chunk `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        let cached = self.pre_ore_stage(cx, cz);
        // Issue #427: routed through `post_ore_world` (which wraps
        // `ore_stage` in `Self::post_ore_cache`) rather than calling
        // `ore_stage` directly, so this chunk's post-ore result is
        // available with no recomputation to any OTHER chunk's
        // `vegetation_stage` that later needs it as one of its 8
        // neighbours — see `PostOreCache`'s own doc comment for why that
        // sharing matters (without it, every chunk that appears as both a
        // sweep's own centre and some other chunk's neighbour would pay the
        // real ore-placement RNG walk twice). This costs one
        // `DenseBlockGrid` clone (unwrapping the cached `Arc`) in place of
        // the clone `ore_stage` already required directly — same order of
        // cost as before, not a new one.
        let world = (*self.post_ore_world(cx, cz)).clone();
        let world = self.vegetation_stage(cx, cz, world);
        // Issue #404's U2: `TOP_LAYER_MODIFICATION` is vanilla's LAST decoration
        // step (index 10) and must run after vegetation, because the
        // `MOTION_BLOCKING` height it reads includes leaves and logs — snow sits
        // on a spruce canopy. Running it before vegetation would put snow at the
        // pre-tree surface and then bury it.
        let (world, _) = self.top_layer_stage(cx, cz, world, &cached.2);
        self.intern_from_dense(world, cached.2.clone())
    }

    /// Stages 1-4 (fill/aquifer, biome, surface, carve) for chunk `(cx, cz)` —
    /// any chunk, not only the one being composed. Returns that chunk's own
    /// post-carve world (absolute-coordinate keyed, populated only for its
    /// own 16×16 columns), its heightmap and its biome quarts.
    ///
    /// Factored out of [`Self::column`] so [`Self::ore_stage`] (issue #295)
    /// can call it again for each of the 8 neighbour chunks in the ore
    /// driver's 3×3 neighbourhood: vanilla's real `blockStateWriteRadius(1)`
    /// ore spill (`FeatureOracle.java`'s own doc comment,
    /// `docs/worldgen-parity.md`'s "known gap" section) depends on each
    /// neighbour's own real post-carve terrain and heightmap, not an
    /// approximation — a neighbour in a different biome to the centre also
    /// carves (and later decorates) differently, so there is no shortcut
    /// that reuses the centre's own field.
    ///
    /// Memoised in [`Self::pre_ore_cache`], keyed by the **exact** `(cx, cz)`
    /// passed in — never rounded, clamped, or otherwise merged with a
    /// neighbouring key. That distinction matters: an earlier version of
    /// this same idea in the JVM oracle this crate is proven against
    /// (`FeatureOracle.java`) *did* clamp reads to a bounded region, aliasing
    /// two distinct chunk coordinates onto one memoised value, and vanilla's
    /// own `BulkSectionAccess` then tried to lock the same
    /// `LevelChunkSection`'s non-reentrant semaphore twice within one
    /// placement and hung forever (see `docs/worldgen-parity.md`'s "Known
    /// gap" section on the 3×3 driver). This engine has no such semaphore,
    /// but the aliasing shape — two logically distinct chunks sharing one
    /// cached answer — is exactly what an exact-coordinate key rules out.
    fn pre_ore_stage(&self, cx: i32, cz: i32) -> Arc<PreOreResult> {
        {
            let cache = self.pre_ore_cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hit) = cache.map.get(&(cx, cz)) {
                crate::counters::bump_pre_ore(false);
                return Arc::clone(hit);
            }
        }
        // Counted before the computation, not after, so a panic mid-pipeline
        // still leaves the counter describing what was attempted. Note the
        // racing-miss path below can make `pre_ore_computed` exceed the number
        // of *distinct* chunks computed under a parallel sweep — which is the
        // honest reading (the work really was done twice), and is exactly the
        // waste U6's store is meant to make impossible.
        crate::counters::bump_pre_ore(true);
        // Computed with the lock released: this is a pure function of
        // `(cx, cz)` and this generator's own fixed state, so two threads
        // racing on the same miss both landing here just means the same
        // value gets computed twice, never a wrong one — see
        // `pre_ore_cache`'s doc comment. The alternative (holding the lock
        // across the whole pipeline) would serialise every worker thread in
        // `generate_columns_parallel` on one mutex for the most expensive
        // part of generation, defeating the parallelism it relies on.
        let computed = Arc::new(self.pre_ore_stage_uncached(cx, cz));
        let mut cache = self.pre_ore_cache.lock().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have inserted the same key while this one was
        // computing (a racing miss, not an error — see above); keep
        // whichever is already there instead of double-inserting into
        // `order`, which would let one key occupy two eviction slots.
        if let Some(existing) = cache.map.get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        cache.map.insert((cx, cz), Arc::clone(&computed));
        cache.order.push_back((cx, cz));
        if cache.order.len() > PRE_ORE_CACHE_CAPACITY {
            if let Some(oldest) = cache.order.pop_front() {
                cache.map.remove(&oldest);
            }
        }
        computed
    }

    /// One chunk's own post-carve-and-ore world (stages 1-5), for an
    /// arbitrary `(cx, cz)` — not necessarily the chunk [`Self::column`] was
    /// asked to generate. Used by [`Self::vegetation_stage`]'s 3×3 driver to
    /// obtain each of the 8 neighbours' own post-ore terrain, exactly the
    /// input vegetal decoration reads/writes against for that neighbour in
    /// real vanilla. Recurses into that neighbour's own 3×3 ore composition
    /// via [`Self::ore_stage`]/[`Self::pre_ore_stage`] (the latter memoised,
    /// per [`Self::pre_ore_cache`]'s doc comment) — real parity, not an
    /// approximation.
    ///
    /// Memoised in [`Self::post_ore_cache`] — see that field's and
    /// [`PostOreCache`]'s own doc comments for why this specific result
    /// (not just the pre-ore terrain feeding it) needs its own cache: without
    /// it, a sweep over adjacent chunks would rerun the full `ore_stage` RNG
    /// walk once per `(neighbour, requester)` pair instead of once per
    /// neighbour. Same lock-released-during-compute shape as
    /// [`Self::pre_ore_stage`], for the same reason (never serialise
    /// `generate_columns_parallel`'s worker threads on this mutex).
    fn post_ore_world(&self, cx: i32, cz: i32) -> Arc<crate::dense_grid::DenseBlockGrid> {
        {
            let cache = self.post_ore_cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hit) = cache.map.get(&(cx, cz)) {
                crate::counters::bump_post_ore(false);
                return Arc::clone(hit);
            }
        }
        crate::counters::bump_post_ore(true);
        let pre = self.pre_ore_stage(cx, cz);
        let computed = Arc::new(self.ore_stage(cx, cz, pre.0.clone(), &pre.1));
        let mut cache = self.post_ore_cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = cache.map.get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        cache.map.insert((cx, cz), Arc::clone(&computed));
        cache.order.push_back((cx, cz));
        if cache.order.len() > PRE_ORE_CACHE_CAPACITY {
            if let Some(oldest) = cache.order.pop_front() {
                cache.map.remove(&oldest);
            }
        }
        computed
    }

    /// Identical to [`column`](Self::column), timed per stage. Exists so the
    /// per-stage cost split can be re-measured without maintaining a second,
    /// hand-duplicated copy of the pipeline: this calls the exact same private
    /// stage functions `column` does, just wrapped in `Instant::now()` at each
    /// boundary. Native-only (wall-clock timing has no meaning under wasm, and
    /// `Instant::now()` panics on bare `wasm32-unknown-unknown`).
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn column_timed(&self, cx: i32, cz: i32) -> (GeneratedColumn, StageTimes) {
        let base_x = cx * 16;
        let base_z = cz * 16;

        let t_aquifer_start = std::time::Instant::now();
        let aquifer = self.build_aquifer(cx, cz);
        let t_shape_start = std::time::Instant::now();
        let field = self.fill_stage(&aquifer, base_x, base_z);
        let heights = self.heights_from_field(&field);
        let t_biome_start = std::time::Instant::now();
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let t_surface_start = std::time::Instant::now();
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        let t_materialize_start = std::time::Instant::now();
        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let t_carve_start = std::time::Instant::now();
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);
        let t_ore_start = std::time::Instant::now();
        let world = self.ore_stage(cx, cz, world, &heights);
        let t_vegetation_start = std::time::Instant::now();
        let world = self.vegetation_stage(cx, cz, world);
        let t_top_layer_start = std::time::Instant::now();
        // Issue #404's U2. This call is why `StageTimes` grew a field rather
        // than folding another stage into `intern`: `top_layer_stage` is the
        // first stage cheap enough that its cost had to be *measured* to be
        // believed, and `docs/plans/worldgen-parity.md` §6 predicts <5% for it.
        let (world, _) = self.top_layer_stage(cx, cz, world, &biome_quarts);
        let t_intern_start = std::time::Instant::now();
        let col = self.intern_from_dense(world, biome_quarts);
        let t_end = std::time::Instant::now();

        (
            col,
            StageTimes {
                aquifer: t_shape_start - t_aquifer_start,
                shape: t_biome_start - t_shape_start,
                biome: t_surface_start - t_biome_start,
                surface: t_materialize_start - t_surface_start,
                materialize: t_carve_start - t_materialize_start,
                carve: t_ore_start - t_carve_start,
                ore: t_vegetation_start - t_ore_start,
                vegetation: t_top_layer_start - t_vegetation_start,
                top_layer: t_intern_start - t_top_layer_start,
                intern: t_end - t_intern_start,
            },
        )
    }
}
