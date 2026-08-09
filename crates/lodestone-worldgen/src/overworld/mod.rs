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
//! the natural next step, and has since **LANDED** — first as two
//! `Mutex`-guarded FIFO caches (`6509a97`), and now as the sharded staged
//! [`store`] that replaced them in Unit 6 of the rewrite plan, because the
//! FIFO caches' own global mutexes became the next bottleneck (~5,000
//! concurrent lock attempts under a 289-column join burst, `4307b59`).
//! The rest of this paragraph is kept as the argument that
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
//!   orchestration and the two memoised stage entry points
//!   (`pre_ore_stage`/`post_ore_world`);
//! * [`store`] — the staged sharded per-chunk store those two memoise into,
//!   Unit 6's replacement for the two `Mutex`-guarded FIFO caches this file
//!   used to hold;
//! * [`fill`] — stages 1-4 (aquifer, shape, surface, materialise, carve);
//! * [`biome`] — the climate/biome resolution stages;
//! * [`decorate`] — stages 5-7 (ore, vegetation, top layer) and their stitches;
//! * [`output`] — [`GeneratedColumn`] and [`StageTimes`], the read-mostly result types.

mod biome;
pub mod biome_cells;
pub mod block_entities;
mod decorate;
mod fill;
mod output;
pub mod store;
pub mod structures;
mod veins;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::biome::ClimateSampler;
use crate::carver::CarverConfig;
use crate::density::{Builder, Resolver};
use crate::feature::PlacedOre;
use crate::surface::{SurfaceSystem, identity_canon};

use self::biome::DynamicBiome;
use self::fill::AquiferTrees;

pub use self::biome_cells::BiomeCells;
pub use self::block_entities::{BeeOccupant, GeneratedBlockEntity};
pub use self::output::{
    GeneratedColumn, HEIGHTMAP_COLUMNS, MOTION_BLOCKING_HEIGHTMAP_TYPE_ID,
};
#[cfg(not(target_arch = "wasm32"))]
pub use self::output::StageTimes;
pub use self::structures::{BEARD_REACH, REFS_RADIUS, StructureRefs};

/// The return shape of [`OverworldGenerator::pre_ore_stage`] — one chunk's
/// own post-carve world, heightmap and biome quarts (stages 1-4). Named so
/// [`OverworldGenerator::pre_ore_cache`]'s value type reads as "one chunk's
/// pre-ore result", not an anonymous 3-tuple.
///
/// The world is an `Arc` so it can be *handed out* rather than copied. Two callers
/// want it that way: [`OverworldGenerator::vegetation_stage`] supplies the 16 rim
/// chunks of its 5×5 read neighbourhood straight from here (see
/// [`crate::feature::region_view::WIDE_RADIUS`]), and it must not pay 16 dense-grid
/// memcpys per column to do so. Every pre-existing consumer that needs to *mutate*
/// it still clones out of the `Arc` — the same clone it already made, moved one
/// deref inward.
type PreOreResult = (
    Arc<crate::dense_grid::DenseBlockGrid>,
    [i32; 256],
    [(String, bool); 16],
    // Issue #512: the full 4x4x4 biome grid for this chunk. Behind an `Arc` for
    // the same reason the world is -- `vegetation_stage` pulls eight neighbours'
    // `PreOreResult`s out of the store and must not copy 1,536 cells per
    // neighbour to do it. The 16-entry surface array above is *derived from* this
    // one (`surface_quarts_from_cells`), kept alongside rather than recomputed
    // because every existing consumer -- surface, carve, decorate -- asks the
    // surface question specifically.
    Arc<biome_cells::BiomeCells>,
);

/// One chunk's memoised intermediate products — the payload of a
/// [`store::StagedStore`] entry, one [`store::StageSlot`] per stage of the
/// pipeline that other chunks read.
///
/// Unit 6 of `docs/plans/worldgen-rewrite.md` replaced two independent
/// `Mutex<HashMap + VecDeque>` FIFO caches with this: one entry per chunk
/// holding *both* stages, so the two products of one chunk share one shard
/// lookup instead of two global-mutex acquisitions, and each stage carries its
/// own once-only guard. Adding a stage (Unit 9's memoised per-source biome is
/// next) means adding a field here and nothing else — see [`store`]'s module
/// doc, including its reentrancy rule: a stage may only depend on stages
/// declared *below* it.
#[derive(Debug, Default)]
struct ChunkStages {
    /// Stage 0a (issue #514's S1) — this chunk's own structure starts. **The
    /// topmost stage**, and the only one whose computation reads no other stage:
    /// see [`structures`]'s module doc for why starts must precede noise, and
    /// [`store`]'s for why "above the stages it consumes" is the rule that keeps
    /// the once-guards deadlock-free.
    structure_starts: store::StageSlot<Vec<Arc<crate::structure::StructureStart>>>,
    /// Stage 0b — the 17×17 references walk over `structure_starts`. Consumed by
    /// `pre_ore` (as the beardifier context) and by the persistence path.
    structure_refs: store::StageSlot<structures::StructureRefs>,
    /// Stages 1–4 — see [`OverworldGenerator::pre_ore_stage`].
    pre_ore: store::StageSlot<PreOreResult>,
    /// Stages 1–5 — see [`OverworldGenerator::post_ore_world`].
    post_ore: store::StageSlot<crate::dense_grid::DenseBlockGrid>,
}

/// Chebyshev chunk radius one [`OverworldGenerator::column`] call closes over.
///
/// **Derived from the drivers, not chosen.** [`OverworldGenerator::vegetation_stage`]
/// reads the post-ore world of the 3×3 around its centre (radius 1), and each of
/// those post-ore worlds runs [`OverworldGenerator::ore_stage`], which reads the
/// pre-ore world of *its own* 3×3 — so one column's pre-ore closure is 5×5,
/// radius **2**. If a driver's neighbourhood ever widens, this widens with it or
/// the pin below stops covering the request that needs it.
///
/// **`vegetation_stage` now also reads that 5×5 rim directly**, rather than only
/// reaching it transitively through the eight neighbours' own `ore_stage` calls:
/// its read neighbourhood is [`crate::feature::region_view::WIDE_RADIUS`] = 2, and
/// the 16 rim chunks are supplied from `pre_ore_stage` (see that stage for why
/// pre-ore and not post-ore). That does **not** move this constant — the closure
/// was already exactly this set, which is precisely what makes the wider read free
/// — but it does mean two independent drivers now depend on this radius being 2.
/// Narrowing it would break vegetation's seam consistency as well as the pin.
const COLUMN_CLOSURE_RADIUS: i32 = 2;

/// Chebyshev chunk radius one [`OverworldGenerator::column`] call closes over
/// **including the structure stages** — the radius the store pin actually uses.
///
/// # Why this is not [`COLUMN_CLOSURE_RADIUS`], and what it cost to find out
///
/// Issue #514's S1 added one upstream edge: [`OverworldGenerator::pre_ore_stage`]
/// reads `structure_refs_stage` for its own chunk, and that stage walks
/// `structure_starts_stage` over [`structures::REFS_RADIUS`] = 8 chunks in every
/// direction (vanilla's `createReferences` range). Compose that with the 5×5
/// pre-ore closure and **one `column()` call touches 21×21 = 441 store entries,
/// not 25**.
///
/// The pin radius was left at 2, so 416 of those 441 entries were unpinned while
/// the request that needed them was still running, and [`STORE_RETENTION`] was
/// still sized for the 25-per-column world. Measured over the 12×12 C_ss sweep at
/// `ac5391d1`, against the exactly-once predictions in `benches/generation.rs`:
///
/// | counter | predicted | measured | factor |
/// |---|---|---|---|
/// | `pre_ore_computed` | 256 | **740** | 2.9× |
/// | `post_ore_computed` | 196 | **416** | 2.1× |
/// | `structure_starts_computed` | 1,024 | **7,569** | 7.4× |
///
/// Every one of those recomputations is a full terrain stage re-run, and the
/// counters that were supposed to report it were unreadable because a *different*
/// assertion in the same bench had gone red first (see that file's `block_at`
/// decomposition). Nothing was wrong with the eviction policy: the pin was
/// narrower than the closure, which is exactly the failure the pin exists to make
/// impossible.
///
/// **Widen this with any driver that widens.** The rule from
/// [`COLUMN_CLOSURE_RADIUS`] applies twice over here: this is `2 + 8` because two
/// separate constants say so, and if either moves this must be re-derived rather
/// than adjusted.
const STRUCTURE_CLOSURE_RADIUS: i32 = COLUMN_CLOSURE_RADIUS + structures::REFS_RADIUS;

/// Soft ceiling on entries retained by [`OverworldGenerator::store`].
///
/// **Derived from the scenario this unit exists to fix, not picked as a round
/// number.** `4307b59` — the revert that put `lodestone-server`'s per-ring
/// barrier back — names a **289-column join burst**. 289 columns are a 17×17
/// view, and a 17×17 view's closure is `17 + 2 ×`
/// [`STRUCTURE_CLOSURE_RADIUS`]` = 37` on a side, i.e. 37×37 = **1,369** chunks.
/// Retention therefore has to exceed 1,369, or the very burst this store is built
/// for could evict its own live working set; 2,048 is the next power of two above
/// it, and also comfortably covers the 12×12 parity sweep's 32×32 = 1,024-chunk
/// closure.
///
/// **This was 512 and 512 was the pre-#514 derivation** — the same sentence with
/// the pre-ore radius 2 in place of the structure radius 10, giving 441 and 256.
/// It is on the record in `DESIGN.md` §12.130 as a *measured* regression rather
/// than a theoretical one: at 512 the 12×12 sweep recomputed `pre_ore` 740 times
/// instead of 256, because the sweep's real working set was 1,024 entries and the
/// oldest unpinned ones were the neighbours it was about to read back.
///
/// **Entry count is not proportional to memory here, which is why raising it is
/// affordable.** Only entries whose `pre_ore`/`post_ore` slots were actually
/// computed hold a dense grid (~192 KiB each); the extra entries this ceiling
/// admits are structure-starts-only, which hold a `Vec` of starts and nothing
/// else. Per column of travel a session adds ~21 structure-only entries against
/// ~5 terrain-bearing ones, so the resident terrain set at 2,048 entries is on the
/// order of the 512-entry ceiling's — see `docs/worldgen-store-distance-leak.md`.
///
/// Two things keep this from being the capacity-FIFO guess it replaced. First,
/// in-flight neighbourhoods are **pinned** ([`store::StagedStore::open_view`]),
/// so exceeding the ceiling can never evict something a live request needs.
/// Second, [`store::StagedStore::evicted`] is observable, so "no eviction
/// happened" is a checkable control rather than an assumption — which is what
/// licenses reading the stage-computation counters as `chunks × stages`.
///
/// Memory is unchanged by the swap, deliberately: 512 entries × (one pre-ore
/// grid + one post-ore grid, ~192 KiB each) is the same worst case as the two
/// 512-entry caches it replaces, and the reason a ceiling exists at all is
/// still `lodestone_server`'s `OverworldChunkSource`, which holds one generator
/// for a whole world's lifetime — a session gradually exploring a large area
/// would otherwise grow this without bound, a real if slow leak on a machine
/// CLAUDE.md already flags memory as the binding limit on.
const STORE_RETENTION: usize = 2048;

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
    /// The three `default_*` strings above as [`PreState`]s — interned id plus
    /// air/fluid/stone class — resolved once here (issue #501, U21).
    ///
    /// [`Self::surface_stage`]'s `pre` closure and [`Self::materialize_world`]
    /// both need a block-state per position and used to `clone()` / re-hash one
    /// of the strings above per call; these make both a 4-byte copy.
    ///
    /// **Both halves come from [`PreState::from_name`]**, i.e. from
    /// `class_of_name` applied to the very string the settings supplied — so
    /// the class is *derived*, never hand-written at the use site. That is
    /// deliberate: a hand-written class would be a fully-connected wire
    /// carrying the wrong value (`CLAUDE.md`'s own phrase) — the scan would
    /// branch differently and still produce a plausible column. The `String`
    /// forms are kept as the definition `surface_stage` re-derives against on
    /// every entry under `debug_assertions`.
    /// Issue #496: `OreVeinifier`'s three router channels plus its positional RNG,
    /// or `None` when the settings do not enable veins. Consumed by
    /// [`Self::materialize_world`]. See [`veins`].
    veins: Option<veins::VeinPrograms>,
    default_block_pre: crate::surface::PreState,
    default_fluid_pre: crate::surface::PreState,
    default_lava_pre: crate::surface::PreState,
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
    /// The staged per-chunk store: this generator's memoisation of
    /// [`Self::pre_ore_stage`] and [`Self::post_ore_world`], and Unit 6's
    /// replacement for the two `Mutex`-guarded FIFO caches that preceded it.
    ///
    /// The memoisation itself is not new and its motivation is unchanged:
    /// [`Self::ore_stage`]'s real 3×3 driver needs the centre plus all 8
    /// neighbours' pre-ore pipelines on *every* [`column`](Self::column) call,
    /// and [`Self::vegetation_stage`]'s 3×3 driver needs 8 neighbours' full
    /// post-ore worlds (the expensive ore *RNG walk*, not just terrain), so
    /// without memoisation a sweep redoes each of those up to 9× — measured, a
    /// 144-chunk sweep went from ~68s to 700.57s in debug when ore composition
    /// landed, matching the predicted ~9×.
    ///
    /// What **is** new is that computing once is now structural rather than
    /// best-effort. The old caches took one global `Mutex` each and released it
    /// across the computation, so two threads racing the same key both ran the
    /// whole pipeline — `pre_ore_stage`'s own comment conceded "the work really
    /// was done twice". Under a 289-column join burst that produced ~5,000
    /// concurrent attempts on a single `Arc<Mutex>` and forced
    /// `lodestone-server`'s per-ring barrier back in (`4307b59`). Here the map
    /// is sharded [`store::SHARD_COUNT`] ways and each stage has its own
    /// once-only guard, so a racing thread *waits for the value* instead of
    /// computing a second copy. See [`store`]'s module doc for the full
    /// argument, the exact-key rule, and why eviction is view-scoped.
    store: store::StagedStore<ChunkStages>,
    /// Per-biome decoration list, resolved the same way and at the same time as
    /// `ores_by_biome` — see `crate::compose::build_biome_decoration`. Empty
    /// (whole map) when the resolver supplies no biome documents with any driven
    /// step, in which case [`Self::vegetation_stage`] is a no-op, matching every
    /// other #295/#406 resolver "no data supplied" convention.
    ///
    /// **Issue #513 widened this from `VEGETAL_DECORATION` alone to every step in
    /// `crate::compose::DRIVEN_STEPS`**, so entries now carry their own step index
    /// and the map is no longer one-step-per-biome. The name is unchanged because
    /// [`Self::vegetation_stage`] is still the one stage that consumes it.
    vegetation_by_biome:
        HashMap<String, Vec<(i32, usize, crate::feature::vegetation::PlacedRef)>>,
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
    /// Per-biome `MobSpawnSettings` — `spawners` and `spawn_costs` — read out of
    /// the same `Resolver::biome_document` walk as `biome_climates`, so it costs
    /// no extra JSON parse (issue #518, part 1). See
    /// [`crate::spawners`] for why only the parse landed and what the other three
    /// parts are blocked on, and [`Self::biome_spawners`] for the accessor.
    ///
    /// **No generation stage reads this.** It is data for a runtime spawner that
    /// does not exist yet.
    spawners_by_biome: HashMap<String, crate::spawners::BiomeSpawners>,
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
    /// The structure placement engine (issue #514's S1), or `None` when the
    /// resolver supplied no `structure_set_ids` — which is every fixture resolver
    /// in this workspace, and the reason this unit changes no parity fixture.
    ///
    /// `None` rather than an empty registry so the two structure stages can
    /// early-return without touching the registry at all: a generator with no
    /// structure data does zero placement draws per chunk, not twenty
    /// no-ops.
    structures: Option<crate::structure::StructureRegistry>,
}

/// Renders a noise-settings block-state object (`{"Name": ..., "Properties": {...}}`)
/// as this engine's canonical state string, `name[k=v,...]` with properties
/// **sorted by key**.
///
/// The properties are not optional decoration: vanilla's own
/// `noise_settings/overworld.json` carries
/// `"default_fluid": {"Name": "minecraft:water", "Properties": {"level": "0"}}`,
/// and reading only `["Name"]` produces `minecraft:water` — a *different string*
/// from the `minecraft:water[level=0]` that `crate::carver` writes for the same
/// block state. One column then holds two palette entries for one state, which
/// costs a palette slot each (and a bit of index width for the whole section once
/// the palette crosses 16), and makes every downstream `match` on the full state
/// string miss for the bare form.
fn canonical_state_from_settings(value: &Value, fallback: &str) -> String {
    let Some(name) = value["Name"].as_str() else {
        return fallback.to_string();
    };
    match value["Properties"].as_object() {
        Some(properties) if !properties.is_empty() => {
            let mut rendered: Vec<String> = properties
                .iter()
                .map(|(key, value)| {
                    let value = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
                    format!("{key}={value}")
                })
                .collect();
            rendered.sort();
            format!("{name}[{}]", rendered.join(","))
        }
        _ => name.to_string(),
    }
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
        // Built here rather than in the `Self { .. }` literal below because
        // `SurfaceSystem::new` interns its whole result-state set into it at
        // parse time (issue #501) — see that method's own note on why a set
        // walked out of the parsed data cannot drift the way a hand-maintained
        // pre-intern list would.
        let interner = Arc::new(crate::interner::StateInterner::new());
        let surface = SurfaceSystem::new(settings, &builder, &canon, &interner);

        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(384) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(63) as i32;
        let default_block = settings["default_block"]["Name"]
            .as_str()
            .unwrap_or("minecraft:stone")
            .to_string();
        let default_fluid =
            canonical_state_from_settings(&settings["default_fluid"], "minecraft:water[level=0]");
        let default_lava = "minecraft:lava[level=0]".to_string();
        // Issue #496. Built from the same `builder` every other router channel uses,
        // so `vein_toggle`'s slot indices share one address space with
        // `final_density`'s -- the property `slot_count` below depends on.
        let veins = veins::VeinPrograms::build(&builder, settings, &interner);
        let default_block_pre = crate::surface::PreState::from_name(&interner, &default_block);
        let default_fluid_pre = crate::surface::PreState::from_name(&interner, &default_fluid);
        let default_lava_pre = crate::surface::PreState::from_name(&interner, &default_lava);

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
            final_density: crate::engine::Program::compile(&final_density),
            erosion: crate::engine::Program::compile(&builder.build(&router["erosion"])),
            depth: crate::engine::Program::compile(&builder.build(&router["depth"])),
            barrier: std::sync::Arc::new(builder.build(&router["barrier"])),
            floodedness: std::sync::Arc::new(
                builder.build(&router["fluid_level_floodedness"]),
            ),
            spread: std::sync::Arc::new(builder.build(&router["fluid_level_spread"])),
            lava: std::sync::Arc::new(builder.build(&router["lava"])),
            prelim: std::sync::Arc::new(builder.build(&router["preliminary_surface_level"])),
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
        // Issue #518 part 1 rides the same walk, for the same reason.
        let mut spawners_by_biome = HashMap::new();
        for name in &biome_names {
            carvers_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_carvers(resolver, name),
            );
            ores_by_biome.insert(name.clone(), crate::compose::build_biome_ores(resolver, name));
            vegetation_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_decoration(resolver, name),
            );
            let document = resolver.biome_document(name);
            if let Some(climate) = crate::feature::top_layer::parse_biome_climate(&document) {
                biome_climates.insert(name.clone(), climate);
            }
            if crate::compose::biome_lists_freeze_top_layer(&document) {
                freeze_biomes.insert(name.clone());
            }
            let spawners = crate::spawners::parse_biome_spawners(&document);
            if !spawners.is_empty() {
                spawners_by_biome.insert(name.clone(), spawners);
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

        // Issue #514's S1. Built here, from the same `resolver` borrow every
        // other composition table above uses, because the registry parses ~54
        // JSON documents plus their biome-tag closures and must not do that per
        // chunk. An empty `structure_set_ids` (the `Resolver` default) yields
        // `None`, so nothing downstream distinguishes "no structure data" from
        // "this engine before structures existed".
        let structures = {
            let registry = crate::structure::StructureRegistry::new(seed, resolver);
            if registry.is_empty() { None } else { Some(registry) }
        };

        Self {
            slot_count,
            surface,
            // Fresh per generator, built just above so `SurfaceSystem::new`
            // could intern into it. Still deliberately *not* pre-populated
            // from a hand-written list of everything the resolver's data can
            // produce: the allocation budget is written against a steady-state
            // column, by which point every state the data can produce has been
            // interned by ordinary generation, and a hand-maintained
            // pre-intern list that drifted out of sync with the data would be
            // a stale claim of exactly the kind CLAUDE.md's rule 2 is about.
            // U21's additions are not that: the surface rule's result states,
            // the clay bands and the three `default_*` blocks are all walked
            // out of the parsed data itself, so they cannot drift from it.
            interner,
            min_y,
            height,
            sea_level,
            default_block,
            default_fluid,
            default_lava,
            veins,
            default_block_pre,
            default_fluid_pre,
            default_lava_pre,
            fallback_biome: biome.to_string(),
            fallback_cold_enough_to_snow: cold_enough_to_snow,
            dynamic_biome,
            seed,
            aquifer_trees,
            carver_replaceable,
            carvers_by_biome,
            ores_by_biome,
            ore_tag_map,
            store: store::StagedStore::new(STORE_RETENTION),
            vegetation_by_biome,
            veg_tags,
            biome_climates,
            spawners_by_biome,
            freeze_biomes,
            snow_support,
            climate_noise: crate::noise::ClimateNoise::new(),
            structures,
        }
    }

    /// Issue #518 part 1: one biome's parsed `MobSpawnSettings`, or `None` when
    /// the biome declares no spawner entry and no spawn cost (which includes every
    /// biome a fixture `Resolver` supplies, and any name this generator's biome
    /// table cannot produce).
    ///
    /// Resolved once at construction, so this is a map lookup rather than a JSON
    /// parse. **Nothing in the generation pipeline calls it** — see
    /// [`crate::spawners`]'s module doc for what the remaining three parts of #518
    /// are blocked on, and why a `SPAWN` stage built today would be measuring the
    /// wrong world.
    #[must_use]
    pub fn biome_spawners(&self, biome: &str) -> Option<&crate::spawners::BiomeSpawners> {
        self.spawners_by_biome.get(biome)
    }

    /// Every biome's parsed `MobSpawnSettings`, biome name to settings.
    ///
    /// The whole table, for a runtime spawner that needs it keyed by the biome
    /// name it reads off a served column rather than one lookup at a time — the
    /// consumer [`biome_spawners`](Self::biome_spawners)' doc used to say did not
    /// exist. Borrowed, so a caller that must own it (one holding no generator,
    /// e.g. a server tick loop) clones deliberately.
    #[must_use]
    pub fn all_biome_spawners(&self) -> &HashMap<String, crate::spawners::BiomeSpawners> {
        &self.spawners_by_biome
    }

    /// `MultiNoiseBiomeSource.getNoiseBiome(qx, qy, qz)` at an arbitrary quart
    /// cell, sampled fresh rather than read out of a generated chunk.
    ///
    /// Needed by the structure stages, which run before any chunk exists.
    /// [`Self::biome_cells_stage`] resolves the same question for a whole chunk's
    /// 4×4×4 grid; both go through the same `climate.target` +
    /// `table.nearest` pair, and the quart-**corner** convention is the same one
    /// that stage documents. Falls back to the fixed biome for a generator with no
    /// climate table.
    #[must_use]
    pub fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        match &self.dynamic_biome {
            None => self.fallback_biome.clone(),
            Some(dynamic) => {
                let target = dynamic.climate.target(qx * 4, qy * 4, qz * 4);
                dynamic.table.nearest(&target).to_string()
            }
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

    /// Distinct chunks currently held in the staged store. **Diagnostics and
    /// gates only** — nothing in generation may branch on it.
    ///
    /// Exposed because it is the one assertion about the store that works with
    /// `gen-counters` **off**: it bounds from above how many times a stage can
    /// have run. A sweep that ends with exactly as many entries as its
    /// neighbourhood closure, and [`Self::store_evictions`] at zero, has proved
    /// no chunk was entered twice and none was dropped — the counters-off half of
    /// Unit 6's acceptance criterion, and the reason the gate is not a
    /// counters-only test that silently measures nothing in a default build.
    #[must_use]
    pub fn store_len(&self) -> usize {
        self.store.len()
    }

    /// Store entries dropped by reclamation over this generator's life.
    /// **Diagnostics and gates only.**
    ///
    /// The control that separates "each stage computed exactly once" from "each
    /// stage computed once, plus however many times eviction silently made us
    /// redo it". A sweep or burst asserting this is zero has established that its
    /// stage-computation counts cannot have been inflated by the retention
    /// ceiling — see [`STORE_RETENTION`] for why zero is expected there.
    #[must_use]
    pub fn store_evictions(&self) -> usize {
        self.store.evicted()
    }

    /// Issue #515: whether `(cx, cz)` is a slime chunk for **this generator's**
    /// seed — `WorldgenRandom.seedSlimeChunk` plus its `nextInt(10) == 0`.
    ///
    /// A method on the generator rather than a bare function so a caller never has
    /// to know the world seed to ask; the derivation itself is
    /// [`crate::rng::is_slime_chunk`], beside the three other `WorldgenRandom` seed
    /// derivations, and is unit-tested element-wise against an independently
    /// transcribed lattice.
    ///
    /// **This has no gameplay consumer yet, and that is the honest state.** Slime
    /// spawning is blocked on there being a natural spawn cycle at all (#222) and a
    /// per-species spawn rule table (#221); nothing here can make slimes appear. It
    /// is a proven predicate waiting for whichever of those lands first, or for the
    /// F3 chunk-borders overlay (#197) to read it.
    #[must_use]
    pub fn is_slime_chunk(&self, cx: i32, cz: i32) -> bool {
        crate::rng::is_slime_chunk(cx, cz, self.seed)
    }

    /// Generates the block field for chunk `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> GeneratedColumn {
        // Pins this request's whole closure in the store for the duration of the
        // call, so nothing it computes can be evicted before it is read back —
        // the property that makes eviction view-scoped instead of a capacity
        // guess. Dropped at the end of the call.
        //
        // [`STRUCTURE_CLOSURE_RADIUS`], not [`COLUMN_CLOSURE_RADIUS`]: since
        // issue #514 the closure is 21×21, and pinning the inner 5×5 of it left
        // the request's own structure-start entries evictable *by the request
        // itself*. See that constant for the measured cost.
        let _view = self.store.open_view((cx, cz), STRUCTURE_CLOSURE_RADIUS);
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
        //
        // Unit 7 moved that clone *inside* `vegetation_stage`: the `Arc` is
        // handed over intact so the pre-vegetation content can double as the
        // centre source of the in-place region view, and the private mutable copy
        // is taken once at the end. Same one clone, one stage later.
        let (world, block_entities) = self.vegetation_stage(cx, cz, self.post_ore_world(cx, cz));
        // Issue #404's U2: `TOP_LAYER_MODIFICATION` is vanilla's LAST decoration
        // step (index 10) and must run after vegetation, because the
        // `MOTION_BLOCKING` height it reads includes leaves and logs — snow sits
        // on a spruce canopy. Running it before vegetation would put snow at the
        // pre-tree surface and then bury it.
        let (world, _) = self.top_layer_stage(cx, cz, world, &cached.2);
        self.intern_from_dense(world, cached.2.clone(), (*cached.3).clone(), block_entities)
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
    /// Memoised in [`Self::store`], keyed by the **exact** `(cx, cz)` passed in
    /// — never rounded, clamped, or otherwise merged with a neighbouring key.
    /// That distinction matters: an earlier version of this same idea in the JVM
    /// oracle this crate is proven against (`FeatureOracle.java`) *did* clamp
    /// reads to a bounded region, aliasing two distinct chunk coordinates onto
    /// one memoised value, and vanilla's own `BulkSectionAccess` then tried to
    /// lock the same `LevelChunkSection`'s non-reentrant semaphore twice within
    /// one placement and hung forever (see `docs/worldgen-parity.md`'s "Known
    /// gap" section on the 3×3 driver). This engine has no such semaphore, but
    /// the aliasing shape — two logically distinct chunks sharing one cached
    /// answer — is exactly what an exact-coordinate key rules out, and
    /// [`store::ChunkPos`] carries that rule as its own documentation.
    ///
    /// **Computed exactly once per chunk per generator, now by construction.**
    /// The shard lock is released before this returns and is never held across
    /// the pipeline below — but unlike the FIFO cache this replaced, a second
    /// thread arriving on the same miss no longer recomputes: it blocks on the
    /// slot's own once-guard and takes the first thread's value. The counter is
    /// bumped from *inside* that guard, so `pre_ore_computed` is the number of
    /// distinct chunks whose stages 1–4 really ran, and a sweep can assert it
    /// equals the size of the region it swept.
    fn pre_ore_stage(&self, cx: i32, cz: i32) -> Arc<PreOreResult> {
        let entry = self.store.entry((cx, cz));
        entry.pre_ore.get_or_compute(
            crate::counters::bump_pre_ore,
            || {
                // Issue #514's S1 added the upstream edge to `structure_refs`
                // here as a placeholder with a `debug_assert` guarding the gap;
                // **S3 closed it**, and the edge is now a real data dependency
                // consumed inside `pre_ore_stage_uncached` →
                // `beardifier_for` → `fill_stage`. Nothing to do at this level
                // any more, which is why the guard is gone rather than relaxed.
                self.pre_ore_stage_uncached(cx, cz)
            },
        )
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
    /// Memoised in [`Self::store`]'s `post_ore` slot — a *separate* stage from
    /// `pre_ore` on the same entry, because this one memoises the expensive ore
    /// placement **RNG walk**, not just the cheap-to-share pre-ore terrain
    /// feeding it: without it, a sweep over adjacent chunks would rerun the full
    /// [`Self::ore_stage`] once per `(neighbour, requester)` pair instead of once
    /// per neighbour, a second 9× on top of the one ore composition already
    /// costs.
    ///
    /// **The layering here is what keeps the store deadlock-free**, and it is a
    /// rule rather than an accident. This stage's computation calls
    /// [`Self::pre_ore_stage`] — a strictly *lower* stage — for its own chunk and,
    /// via [`Self::ore_stage`], for its 3×3; `pre_ore` calls nothing in the
    /// store. So the wait-for graph only ever points downward and its lowest
    /// layer never waits. A stage that re-entered its own slot, or that reached
    /// back up a layer, would deadlock on the once-guard: see [`store`]'s module
    /// doc before adding one.
    fn post_ore_world(&self, cx: i32, cz: i32) -> Arc<crate::dense_grid::DenseBlockGrid> {
        let entry = self.store.entry((cx, cz));
        entry.post_ore.get_or_compute(crate::counters::bump_post_ore, || {
            let pre = self.pre_ore_stage(cx, cz);
            self.ore_stage(cx, cz, (*pre.0).clone(), &pre.1)
        })
    }

    /// Runs **only** [`Self::ore_stage`] for `(cx, cz)` against a private copy of
    /// that chunk's already-memoised pre-ore world, and returns how many cells ore
    /// placement wrote into the centre.
    ///
    /// # Why this exists
    ///
    /// DESIGN.md §12.143 measured `ore` at 38.7% of a steady-state column with
    /// nothing ever having profiled it. A sampling profile of `column()` spends
    /// most of its samples in `shape`/`vegetation`; this isolates the stage so a
    /// `samply` profile of a sweep over it attributes ore's own cycles.
    /// `tests/ore_stage_profile.rs` is the driver, and its warm-up is the
    /// load-bearing part — on a cold store the `pre_ore_stage` call below runs a
    /// whole terrain pipeline and the profile stops being about ore.
    ///
    /// Deliberately **not** routed through [`Self::post_ore_world`]: that memoises,
    /// so a second call over the same chunk would measure a store hit. The one
    /// dense-grid clone it costs (98,304 `u16`s) is the same clone
    /// `post_ore_world`'s consumer pays and is ~0.2% of the stage.
    ///
    /// The return value is how many of the centre's own cells the stage changed,
    /// purely so a caller can assert the workload was not vacuous — a resolver with
    /// no ore data makes `ore_stage` an early return, which is exactly §12.143's
    /// "world" species of vacuous benchmark, and a profile of it would be a profile
    /// of the `if` at the top. The 98,304-cell diff walk that produces it is ~0.05%
    /// of the stage's own instruction count.
    #[must_use]
    pub fn ore_stage_for_profiling(&self, cx: i32, cz: i32) -> u64 {
        let _view = self.store.open_view((cx, cz), STRUCTURE_CLOSURE_RADIUS);
        let pre = self.pre_ore_stage(cx, cz);
        let world = self.ore_stage(cx, cz, (*pre.0).clone(), &pre.1);
        let (min_x, min_y, min_z, sx, sy, sz) = world.bounds();
        let mut changed = 0u64;
        for y in min_y..min_y + sy {
            for z in min_z..min_z + sz {
                for x in min_x..min_x + sx {
                    if world.get_id(x, y, z) != pre.0.get_id(x, y, z) {
                        changed += 1;
                    }
                }
            }
        }
        changed
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
        // Same pin as [`Self::column`] — this path drives the same 3×3/5×5
        // neighbourhood through `ore_stage`/`vegetation_stage`, so it needs the
        // same protection or a bench near the retention ceiling could measure an
        // eviction rather than the pipeline.
        let _view = self.store.open_view((cx, cz), STRUCTURE_CLOSURE_RADIUS);
        let base_x = cx * 16;
        let base_z = cz * 16;

        let t_aquifer_start = web_time::Instant::now();
        let aquifer = self.build_aquifer(cx, cz);
        // Issue #514's S3, inside the *aquifer* timing bucket rather than given one
        // of its own: for a chunk with no adaptation-bearing start in reach this is
        // a store read and an empty `Vec`, and the per-block cost it can add lands
        // in `shape` where it belongs.
        let beard = self.beardifier_for(cx, cz);
        let t_shape_start = web_time::Instant::now();
        let field = self.fill_stage(&aquifer, base_x, base_z, &beard);
        let heights = self.heights_from_field(&field);
        let t_biome_start = web_time::Instant::now();
        // Issue #512: same two-line shape as `pre_ore_stage_uncached` — the 4x4x4
        // grid is sampled and the 16 surface quarts are read out of it, so this
        // timing bucket now covers 96x the samples it used to. That is the point
        // of measuring it here.
        let biome_cells = self.biome_cells_stage(base_x, base_z);
        let biome_quarts = self.biome_stage(&biome_cells, &heights);
        let t_surface_start = web_time::Instant::now();
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        let t_materialize_start = web_time::Instant::now();
        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let t_carve_start = web_time::Instant::now();
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);
        // The same stage `pre_ore_stage_uncached` runs; timed inside the carve
        // bucket rather than given one of its own, because for a chunk with no
        // structure in reach it is a single early return.
        let world = self.structure_place_stage(cx, cz, world);
        let t_ore_start = web_time::Instant::now();
        let world = self.ore_stage(cx, cz, world, &heights);
        let t_vegetation_start = web_time::Instant::now();
        // `Arc::new` rather than a store lookup: this path builds its own world
        // locally (it is the per-stage timing split, not the memoised serve path),
        // so wrapping it is a pointer move, not a copy. `vegetation_stage` takes
        // the shared form because in `column` the centre's post-ore grid really is
        // shared — see there.
        let (world, block_entities) = self.vegetation_stage(cx, cz, Arc::new(world));
        let t_top_layer_start = web_time::Instant::now();
        // Issue #404's U2. This call is why `StageTimes` grew a field rather
        // than folding another stage into `intern`: `top_layer_stage` is the
        // first stage cheap enough that its cost had to be *measured* to be
        // believed, and `docs/plans/worldgen-parity.md` §6 predicts <5% for it.
        let (world, _) = self.top_layer_stage(cx, cz, world, &biome_quarts);
        let t_intern_start = web_time::Instant::now();
        let col = self.intern_from_dense(world, biome_quarts, biome_cells, block_entities);
        let t_end = web_time::Instant::now();

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
