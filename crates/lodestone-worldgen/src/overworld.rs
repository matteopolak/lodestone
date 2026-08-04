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
//! **Still not composed:** vegetation/tree features (unbuilt anywhere in
//! this crate, epic #404 Phase 3) and structures (unbuilt anywhere in this
//! repo, `#136`). `docs/worldgen-parity.md` measures the composed subset
//! (shape + real aquifer + biome + surface + carvers + ores) against a real
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
//! `lodestone_server::worldgen_data::tests::column_is_byte_identical_across_two_independent_sequential_calls`
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
//! neighbour cache (safe to memoize — generation is pure/deterministic) is
//! the natural next step, not attempted this session:
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
//! # Badlands (issue #405's carried-over gap)
//!
//! `minecraft:badlands`/`eroded_badlands`/`wooded_badlands` remain excluded
//! from the searchable biome table (`crate::biome::usable_overworld_table`) —
//! their surface rule reaches an unported `SurfaceSystem.getBand` subsystem
//! that would panic. Composing carvers/ores here does not change that: the
//! same excluded table feeds both surface *and* the per-source-chunk carver
//! biome and the ore biome, so a column can never resolve to one of the three
//! excluded names in the first place.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::aquifer::{AquiferSystem, BlockKind, XoroshiroPositionalFactory};
use crate::biome::{BiomeParameterPoint, ClimateSampler};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::{Builder, Density, Resolver};
use crate::feature::{PlacedOre, apply_ore_step_3x3_per_source};
use crate::rng::{WorldgenRandom, XoroshiroRandomSource};
use crate::surface::{SurfaceSystem, identity_canon};

/// Real multi-noise biome assignment (issue #405), present on
/// [`OverworldGenerator`] whenever its [`Resolver`] supplies a non-empty
/// [`Resolver::biome_parameters`] table. See `crate::biome`'s module doc for
/// the resolution/height/excluded-biome decisions baked into this.
#[allow(missing_debug_implementations)]
struct DynamicBiome {
    climate: ClimateSampler,
    table: Vec<BiomeParameterPoint>,
    temperatures: std::collections::HashMap<String, f32>,
}

/// The real aquifer's eight router outputs plus its positional RNG factory,
/// pre-built once (issue #295) from the same shared [`Builder`] that builds
/// `final_density`/surface/climate, so every slot index they reference shares
/// one address space with [`OverworldGenerator::slot_count`] (captured *after*
/// every `builder.build()` call in [`OverworldGenerator::new`], which is
/// always a safe, if occasionally oversized, bound for any one tree's own
/// sampler — a sampler only ever indexes the slots its own tree references).
///
/// Stored so [`OverworldGenerator`] — built once per world seed and unable to
/// hold a borrowed [`Resolver`] for its own lifetime, since callers keep it
/// around far longer than any one `Resolver` borrow — can still build a fresh
/// per-chunk [`AquiferSystem`] (matching vanilla's own per-chunk `NoiseChunk`)
/// via [`AquiferSystem::from_parts`] instead of re-resolving JSON every chunk.
#[allow(missing_debug_implementations)]
struct AquiferTrees {
    final_density: Density,
    erosion: Density,
    depth: Density,
    barrier: Density,
    floodedness: Density,
    spread: Density,
    lava: Density,
    prelim: Density,
    positional: XoroshiroPositionalFactory,
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
        for name in &biome_names {
            carvers_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_carvers(resolver, name),
            );
            ores_by_biome.insert(name.clone(), crate::compose::build_biome_ores(resolver, name));
        }
        let all_ores: Vec<PlacedOre> = ores_by_biome.values().flatten().cloned().collect();
        let ore_tag_map = crate::compose::build_ore_tag_map(resolver, &all_ores);

        // Captured last, after every `builder.build()` call above (shape,
        // surface, climate, the eight aquifer trees) — see `AquiferTrees`'s
        // doc comment for why this is always a safe bound.
        let slot_count = builder.slot_count();

        Self {
            slot_count,
            surface,
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
        let (world, heights, biome_quarts) = self.pre_ore_stage(cx, cz);
        let world = self.ore_stage(cx, cz, world, &heights);
        self.intern_from_dense(world, biome_quarts)
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
    fn pre_ore_stage(&self, cx: i32, cz: i32) -> (crate::dense_grid::DenseBlockGrid, [i32; 256], [(String, bool); 16]) {
        let base_x = cx * 16;
        let base_z = cz * 16;

        let aquifer = self.build_aquifer(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z);
        let heights = self.heights_from_field(&field);
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);

        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);

        (world, heights, biome_quarts)
    }

    /// Stage 5 (issue #295): the real `UNDERGROUND_ORES` 3×3 neighbourhood
    /// driver (`crate::feature::apply_ore_step_3x3_per_source`). Builds the
    /// driven region (centre plus its 8 neighbours, each via
    /// [`Self::pre_ore_stage`]) and the `OCEAN_FLOOR_WG` heightmap over the
    /// same region, then runs all 9 source chunks' own ore decoration step —
    /// each source resolving its own biome the same way
    /// [`Self::biome_for_carver_source`] resolves carver biome — and returns
    /// `center_world` with the centre 16×16's own cells overwritten by
    /// whatever the driver placed there (from any of the 9 sources, matching
    /// vanilla's real spill).
    ///
    /// No-op (returns `center_world` unchanged) when the resolver supplied no
    /// biome carries any ore data (`ores_by_biome` all-empty) — the same
    /// "no data supplied" convention every other #295 resolver method
    /// follows, and the one every existing `Resolver` that predates this
    /// increment (most of this crate's own test fixtures) still gets.
    fn ore_stage(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
    ) -> crate::dense_grid::DenseBlockGrid {
        if self.ores_by_biome.values().all(Vec::is_empty) {
            return center_world;
        }

        let mut region: HashMap<(i32, i32, i32), String> = HashMap::new();
        let mut ocean_floor_wg: HashMap<(i32, i32), i32> = HashMap::new();

        Self::stitch_region(&mut region, &mut ocean_floor_wg, cx, cz, cx, cz, &center_world, center_heights);
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (world, heights, _biome_quarts) = self.pre_ore_stage(cx + dx, cz + dz);
                Self::stitch_region(&mut region, &mut ocean_floor_wg, cx + dx, cz + dz, cx, cz, &world, &heights);
            }
        }

        let ores_for_source = |source_x: i32, source_z: i32| -> &[PlacedOre] {
            let biome = self.biome_for_carver_source(source_x, source_z);
            self.ores_by_biome
                .get(biome)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        };
        let in_tag = |block: &str, tag: &str| -> bool {
            self.ore_tag_map
                .get(tag)
                .is_some_and(|members| members.contains(block))
        };

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        // Debug-only escape hatch (LODESTONE_ORE_SINGLE_SOURCE_DEBUG): run only
        // the centre's own decoration pass (matching `ComposedChunkOracle
        // .java`'s single-source-only `postfeatures` stage) while still
        // stitching the full 3x3 terrain/heightmap above, so `get_height`'s
        // probes never panic. Used once to isolate "is the centre pass itself
        // correct" from "does real 3x3 spill widen the gap against a
        // single-source oracle" — see docs/worldgen-parity.md. Not used by
        // `column()`'s normal path.
        let region = if std::env::var("LODESTONE_ORE_SINGLE_SOURCE_DEBUG").is_ok() {
            let ores = ores_for_source(cx, cz);
            let input = crate::feature::OreInput {
                chunk_x: cx,
                chunk_z: cz,
                center_x: cx,
                center_z: cz,
                min_y: self.min_y,
                height: self.height,
                min_gen_y: self.min_y,
                gen_depth: self.height,
                ocean_floor_wg: &ocean_floor_wg,
                in_tag: &in_tag,
            };
            crate::feature::apply_ore_step(&mut random, self.seed, &input, &region, ores)
        } else {
            let (region, _decoration_seed) = apply_ore_step_3x3_per_source(
                &mut random,
                self.seed,
                cx,
                cz,
                self.min_y,
                self.height,
                self.min_y,
                self.height,
                &ocean_floor_wg,
                &in_tag,
                &region,
                &ores_for_source,
            );
            region
        };

        let mut center_world = center_world;
        for y in self.min_y..self.min_y + self.height {
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    if let Some(state) = region.get(&(lx, y, lz)) {
                        center_world.set(cx * 16 + lx, y, cz * 16 + lz, state);
                    }
                }
            }
        }
        center_world
    }

    /// Inserts one source chunk's own post-carve world/heights into a shared
    /// 3×3 region grid, translating its absolute coordinates into
    /// centre-relative local coordinates (`crate::feature::REGION_MIN..
    /// REGION_MAX` on each axis, matching [`crate::feature::OreInput::region_local`]'s
    /// key space).
    ///
    /// `world` is read via [`crate::dense_grid::DenseBlockGrid::get`] (O(1)
    /// array access, issue #295's Job 2) rather than iterated as a
    /// `HashMap`; the destination `region`/`ocean_floor_wg` stay
    /// `HashMap`-keyed because `crate::feature::apply_ore_step_3x3_per_source`
    /// (proven against `feature_parity`'s fixture-driven `HashMap` shape)
    /// still expects that type — narrowing that engine's own signature to a
    /// dense grid too is future work, not attempted in this pass (see
    /// `docs/worldgen-parity.md`).
    fn stitch_region(
        region: &mut HashMap<(i32, i32, i32), String>,
        ocean_floor_wg: &mut HashMap<(i32, i32), i32>,
        source_cx: i32,
        source_cz: i32,
        center_cx: i32,
        center_cz: i32,
        world: &crate::dense_grid::DenseBlockGrid,
        heights: &[i32; 256],
    ) {
        let base_x = source_cx * 16;
        let base_z = source_cz * 16;
        let dx = (source_cx - center_cx) * 16;
        let dz = (source_cz - center_cz) * 16;
        for ly in 0..world.bounds().4 {
            let y = world.bounds().1 + ly;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let state = world.get(base_x + lx, y, base_z + lz);
                    let rx = base_x + lx - center_cx * 16;
                    let rz = base_z + lz - center_cz * 16;
                    region.insert((rx, y, rz), state.to_string());
                }
            }
        }
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                ocean_floor_wg.insert((dx + lx, dz + lz), heights[(lz * 16 + lx) as usize]);
            }
        }
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

        let t0 = std::time::Instant::now();
        let aquifer = self.build_aquifer(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z);
        let heights = self.heights_from_field(&field);
        let t1 = std::time::Instant::now();
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let t2 = std::time::Instant::now();
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        let t3 = std::time::Instant::now();
        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);
        let world = self.ore_stage(cx, cz, world, &heights);
        let col = self.intern_from_dense(world, biome_quarts);
        let t4 = std::time::Instant::now();

        (
            col,
            StageTimes {
                shape: t1 - t0,
                fluid_heightmap: t2 - t1,
                surface: t3 - t2,
                intern: t4 - t3,
            },
        )
    }

    /// Builds a fresh, chunk-bound [`AquiferSystem`] from this generator's
    /// pre-built [`AquiferTrees`] — matching vanilla's own per-chunk
    /// `NoiseChunk`, which the aquifer's internal grid-bound caches assume.
    fn build_aquifer(&self, cx: i32, cz: i32) -> AquiferSystem {
        let t = &self.aquifer_trees;
        AquiferSystem::from_parts(
            t.final_density.clone(),
            t.erosion.clone(),
            t.depth.clone(),
            t.barrier.clone(),
            t.floodedness.clone(),
            t.spread.clone(),
            t.lava.clone(),
            t.prelim.clone(),
            t.positional,
            self.sea_level,
            self.min_y,
            self.height,
            cx,
            cz,
            self.slot_count,
        )
    }

    /// Stage 1 (issue #295): `fillFromNoise` — shape + the **real** aquifer,
    /// replacing the sea-level approximation this generator used before.
    /// Returns a `16×height×16` dense field of [`BlockKind`] indexed by
    /// [`Self::idx`].
    fn fill_stage(&self, aquifer: &AquiferSystem, base_x: i32, base_z: i32) -> Vec<BlockKind> {
        let height = self.height as usize;
        let mut field = vec![BlockKind::Air; 16 * 16 * height];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let wy = self.min_y + ly;
                    field[Self::idx(lx, ly, lz, self.height)] =
                        aquifer.block_at(base_x + lx, wy, base_z + lz);
                }
            }
        }
        field
    }

    /// The heightmap [`Self::biome_stage`] and [`SurfaceSystem::build_surface`]
    /// consume: highest local `(lx, lz)` position whose block is *solid*
    /// (`BlockKind::Stone` — non-air, non-fluid), or `sea_level - 1` for a
    /// column with nothing solid. Matches
    /// `scripts/worldgen-oracle/ComposedChunkOracle.java`'s `solidTop`
    /// exactly (same definition, same fallback) — confirmed by name in that
    /// oracle's own doc comment, which calls this out as the reason biome
    /// sampling agrees between the two languages even though only the Rust
    /// side used to run an *approximated* aquifer.
    fn heights_from_field(&self, field: &[BlockKind]) -> [i32; 256] {
        let mut heights = [i32::MIN; 16 * 16];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let mut top = self.min_y - 1;
                for ly in (0..self.height).rev() {
                    if field[Self::idx(lx, ly, lz, self.height)] == BlockKind::Stone {
                        top = self.min_y + ly;
                        break;
                    }
                }
                heights[(lz * 16 + lx) as usize] = top.max(self.sea_level - 1);
            }
        }
        heights
    }

    /// Stage 2 (issue #405): one climate sample per horizontal quart
    /// `(qx, qz)` in `0..4`, row-major `qz * 4 + qx` — 16 per chunk, matching
    /// [`lodestone_world`](crate)'s own `ChunkSection::BIOME_EDGE` (4) so a
    /// future encoder can write this straight into a real biome container.
    /// Broadcast vertically: see `crate::biome`'s module doc for why one
    /// sample per quart *column*, not a full 3-D grid, is this phase's
    /// deliberate scope.
    ///
    /// Each quart samples at its own already-generated surface height
    /// (`heights[]`, [`Self::heights_from_field`]'s output) rather than a
    /// fixed Y — the module doc's "y = 0 trap" section is why a constant
    /// height silently produces almost all cave/deep-ocean biomes instead of
    /// the terrain biome a player standing there would actually see.
    fn biome_stage(&self, heights: &[i32; 256], base_x: i32, base_z: i32) -> [(String, bool); 16] {
        std::array::from_fn(|i| {
            let Some(dynamic) = &self.dynamic_biome else {
                return (
                    self.fallback_biome.clone(),
                    self.fallback_cold_enough_to_snow,
                );
            };
            let qx = (i % 4) as i32;
            let qz = (i / 4) as i32;
            // Quart cell (qx, qz) covers local x/z in [qx*4, qx*4+4); sample
            // at its own **corner** (`qx*4`), not its center — see
            // `crate::biome`'s module doc and `docs/worldgen-biomes.md` for
            // why this matched a real dark_forest/river boundary and the
            // center convention did not.
            let lx = qx * 4;
            let lz = qz * 4;
            // Y needs the same quart-rounding as X/Z (see the module doc).
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let target = dynamic.climate.target(base_x + lx, y, base_z + lz);
            let name = crate::biome::nearest_biome(&dynamic.table, &target);
            let cold = crate::biome::cold_enough_to_snow(&dynamic.temperatures, name);
            (name.to_string(), cold)
        })
    }

    /// Biome for one *source chunk* in the carve neighbourhood — vanilla's
    /// real `carverBiome` resolution (`NoiseBasedChunkGenerator.applyCarvers`):
    /// sampled at the source chunk's own quart corner (`QuartPos.fromBlock`
    /// of its min block X/Z, which is `source_cx * 16` / `source_cz * 16` —
    /// already quart-aligned since 16 is a multiple of 4, so no extra
    /// rounding is needed) and **`y = 0`**, not the source chunk's surface
    /// height. This is deliberately not [`Self::biome_stage`]'s question:
    /// carver *selection* and surface *material* sample the same climate
    /// fields at different heights and get different (correct) answers —
    /// see `docs/worldgen-parity.md`'s description of `ComposedChunkOracle
    /// .java`'s own `sourceBiome` resolution, which this reproduces exactly.
    fn biome_for_carver_source(&self, source_cx: i32, source_cz: i32) -> &str {
        match &self.dynamic_biome {
            None => self.fallback_biome.as_str(),
            Some(d) => {
                let target = d.climate.target(source_cx * 16, 0, source_cz * 16);
                crate::biome::nearest_biome(&d.table, &target)
            }
        }
    }

    /// Stage 3: surface rules over the pre-surface (post-fill) column.
    /// Returns a **sparse diff** (see [`SurfaceSystem::build_surface`]): only
    /// the positions a surface rule actually rewrote.
    fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        biome_quarts: &[(String, bool); 16],
        base_x: i32,
        base_z: i32,
    ) -> HashMap<(i32, i32, i32), String> {
        let pre = |lx: i32, y: i32, lz: i32| -> String {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return "minecraft:air".to_string();
            }
            match field[Self::idx(lx, ly, lz, self.height)] {
                BlockKind::Stone => self.default_block.clone(),
                BlockKind::Water => self.default_fluid.clone(),
                BlockKind::Lava => self.default_lava.clone(),
                BlockKind::Air => "minecraft:air".to_string(),
            }
        };
        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        let biome_at = |lx: i32, lz: i32| -> (String, bool) {
            let (name, cold) = &biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize];
            (name.clone(), *cold)
        };

        self.surface
            .build_surface(&pre, &heightmap, &biome_at, base_x, base_z)
    }

    /// Materialises the full `16×height×16` post-surface column into a
    /// dense, world-anchored grid (issue #295's Job 2 —
    /// [`crate::dense_grid::DenseBlockGrid`], not a `HashMap`) — the shape
    /// [`crate::carver::apply_carvers`] consumes via [`CarveGrid::from_dense`].
    /// Seeded from `field` (the same solid/fluid/air default
    /// [`Self::surface_stage`]'s own `pre` closure computes) and overlaid
    /// with the surface diff.
    fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: HashMap<(i32, i32, i32), String>,
        base_x: i32,
        base_z: i32,
    ) -> crate::dense_grid::DenseBlockGrid {
        let mut world = crate::dense_grid::DenseBlockGrid::new(
            base_x,
            self.min_y,
            base_z,
            16,
            self.height,
            16,
            "minecraft:air",
        );
        // `surface_diff` is consulted by **point lookup**, in the same fixed
        // `(lz, lx, ly)` order as the base fill below — never iterated
        // directly. This was a real bug (issue #295's Job 2, found by
        // `worldgen_data::tests::column_is_byte_identical_across_two_independent_sequential_calls`):
        // a `DenseBlockGrid`'s palette is built incrementally, in `.set()`
        // call order, unlike the old `HashMap<(i32,i32,i32), String>` `world`
        // this replaced, whose palette used to be assigned by a *separate*,
        // fixed-order final pass (`intern_from_world`) regardless of how
        // `world` itself was populated. `std::collections::HashMap`'s
        // iteration order is not guaranteed stable even across two
        // *separately constructed* maps with identical content (`RandomState`
        // reseeds per map) — so `for ((lx,y,lz), state) in surface_diff` here
        // assigned "which small integer means dirt" differently between two
        // independent `column()` calls for the *same* chunk: same blocks,
        // different palette order, so `GeneratedColumn::into_raw`'s
        // `blocks`/`palette` pair differed byte-for-byte while the actual
        // terrain did not. Confirmed by that control test failing with
        // exactly a palette permutation (`gravel`/`dirt`/`bedrock` reordered,
        // nothing added or removed) before this fix.
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let y = self.min_y + ly;
                    let base = match field[Self::idx(lx, ly, lz, self.height)] {
                        BlockKind::Stone => self.default_block.as_str(),
                        BlockKind::Water => self.default_fluid.as_str(),
                        BlockKind::Lava => self.default_lava.as_str(),
                        BlockKind::Air => "minecraft:air",
                    };
                    match surface_diff.get(&(lx, y, lz)) {
                        Some(state) => world.set(base_x + lx, y, base_z + lz, state),
                        None => world.set(base_x + lx, y, base_z + lz, base),
                    }
                }
            }
        }
        world
    }

    /// Stage 4 (issue #295): `applyCarvers` over the post-surface world grid.
    /// `heights`/`biome_quarts` feed the dirt-recap `top_material` callback
    /// (a carved grass block re-caps the dirt exposed beneath it with the
    /// *local* biome's surface material, looked up via `biome_quarts` since
    /// carving can expose ground anywhere within the centre chunk, which now
    /// carries real per-quart biome variety rather than one fixed biome).
    fn carve_stage(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        heights: &[i32; 256],
        biome_quarts: &[(String, bool); 16],
        base_x: i32,
        base_z: i32,
        world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        let heightmap_fn = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        let top_material = |x: i32, y: i32, z: i32, under_fluid: bool| -> Option<String> {
            let lx = x - base_x;
            let lz = z - base_z;
            if !(0..16).contains(&lx) || !(0..16).contains(&lz) {
                return None;
            }
            let (biome, cold) = &biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize];
            self.surface
                .top_material(x, y, z, under_fluid, &heightmap_fn, biome, *cold)
        };
        let carvers_for_source = |source_x: i32, source_z: i32| -> Vec<CarverConfig> {
            let biome = self.biome_for_carver_source(source_x, source_z);
            self.carvers_by_biome.get(biome).cloned().unwrap_or_default()
        };

        // Perf-measurement-only toggle (LODESTONE_CARVE_HASHMAP_DEBUG): forces
        // the pre-Job-2 HashMap<(i32,i32,i32), String> round trip through
        // `CarveGrid::new`/`into_blocks` instead of `from_dense`/`into_dense`,
        // so the dense-grid win can be measured end to end (144-chunk sweep)
        // rather than argued from Big-O alone. Not used by `column()`'s
        // normal path; safe to leave in as a one-line, well-documented
        // escape hatch (mirrors `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` above).
        let mut grid = if std::env::var("LODESTONE_CARVE_HASHMAP_DEBUG").is_ok() {
            CarveGrid::new(world.into_hashmap())
        } else {
            CarveGrid::from_dense(world)
        };
        let mut observer = NoObserver;
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
            &mut observer,
        );
        if std::env::var("LODESTONE_CARVE_HASHMAP_DEBUG").is_ok() {
            crate::dense_grid::DenseBlockGrid::from_hashmap(base_x, self.min_y, base_z, 16, self.height, 16, &grid.into_blocks())
        } else {
            grid.into_dense()
        }
    }

    /// Adopts the final (post-carve, post-ore) dense world grid straight into
    /// a [`GeneratedColumn`] — no re-intern pass (issue #295's Job 2): a
    /// centre-chunk-sized [`crate::dense_grid::DenseBlockGrid`]'s own
    /// `(palette, blocks)` layout is already identical to
    /// [`GeneratedColumn`]'s (`((ly * 16 + lz) * 16 + lx)`, verified by the
    /// `debug_assert!` below rather than merely asserted in a doc comment).
    fn intern_from_dense(
        &self,
        world: crate::dense_grid::DenseBlockGrid,
        biome_quarts: [(String, bool); 16],
    ) -> GeneratedColumn {
        debug_assert_eq!(world.bounds().3, 16, "centre chunk width must be 16");
        debug_assert_eq!(world.bounds().4, self.height, "centre chunk height must match the generator's");
        debug_assert_eq!(world.bounds().5, 16, "centre chunk depth must be 16");
        let (palette, blocks) = world.into_palette_and_blocks();

        GeneratedColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts: biome_quarts.map(|(name, _)| name),
        }
    }

    #[inline]
    fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        debug_assert!((0..16).contains(&lx) && (0..16).contains(&lz));
        debug_assert!((0..height).contains(&ly));
        ((ly * 16 + lz) * 16 + lx) as usize
    }
}

/// Per-stage wall-clock cost of one [`OverworldGenerator::column_timed`] call.
/// Stage boundaries match the doc comment on [`OverworldGenerator`]: `shape`
/// covers fill (shape + the real aquifer, issue #295); `fluid_heightmap`
/// covers the heightmap + biome sampling (issue #405's
/// [`OverworldGenerator::biome_stage`]); `surface` covers surface rules;
/// `intern` now also covers carve + ore-feature composition + palette
/// interning (issue #295) — folded into this bucket rather than earning new
/// fields every existing caller of this struct would need to learn about.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct StageTimes {
    pub shape: std::time::Duration,
    pub fluid_heightmap: std::time::Duration,
    pub surface: std::time::Duration,
    pub intern: std::time::Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl StageTimes {
    /// Total of the four stages (wall-clock, so approximately equal to but not
    /// exactly the same instant range as timing the whole `column()` call).
    #[must_use]
    pub fn total(&self) -> std::time::Duration {
        self.shape + self.fluid_heightmap + self.surface + self.intern
    }
}

/// A generated 16×`height`×16 block field, block-state strings interned into a
/// small per-column palette.
#[derive(Debug, Clone)]
pub struct GeneratedColumn {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` (issue #405) —
    /// see [`OverworldGenerator::biome_stage`]. Broadcast vertically: the
    /// whole column shares these 16 values regardless of `y`.
    biome_quarts: [String; 16],
}

impl GeneratedColumn {
    /// World Y of the lowest block row.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows.
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

    /// Highest world Y whose block is not air, or `min_y - 1` for an all-air
    /// column. Water counts as non-air (matching `WORLD_SURFACE_WG`).
    #[must_use]
    pub fn top_non_air_y(&self, lx: usize, lz: usize) -> i32 {
        for ly in (0..self.height).rev() {
            let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
            if self.blocks[idx] != 0 {
                return self.min_y + ly;
            }
        }
        self.min_y - 1
    }

    /// Number of non-air blocks (telemetry / anti-vacuity).
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        self.blocks.iter().filter(|b| **b != 0).count()
    }

    /// Biome id at local `(lx, lz)` in `0..16` (issue #405) — quart
    /// resolution, broadcast vertically (see [`OverworldGenerator::biome_stage`]),
    /// so the same answer comes back for every `y` at this `(lx, lz)`.
    ///
    /// # Panics
    /// Panics if `lx`/`lz` are not in `0..16`.
    #[must_use]
    pub fn biome_state(&self, lx: usize, lz: usize) -> &str {
        assert!(lx < 16 && lz < 16, "biome_state coordinates out of range");
        &self.biome_quarts[(lz >> 2) * 4 + (lx >> 2)]
    }

    /// Distinct biome count in this column (telemetry / anti-vacuity — a
    /// chunk straddling a biome boundary should report more than one).
    #[must_use]
    pub fn distinct_biome_count(&self) -> usize {
        let mut seen: Vec<&str> = Vec::with_capacity(16);
        for name in &self.biome_quarts {
            if !seen.contains(&name.as_str()) {
                seen.push(name.as_str());
            }
        }
        seen.len()
    }

    /// Consumes the column into its raw parts: `(min_y, height, palette,
    /// blocks, biome_quarts)`, where `blocks[(ly * 16 + lz) * 16 + lx]`
    /// indexes into `palette` (`palette[0] == "minecraft:air"`), `ly = y -
    /// min_y`, and `biome_quarts[qz * 4 + qx]` is this column's biome id for
    /// horizontal quart `(qx, qz)` (issue #405), constant across `y`.
    ///
    /// This is the zero-copy hand-off a downstream carrier (e.g. the integrated
    /// server's chunk column) uses to adopt the generated block field without
    /// re-interning every block. The index layout is stable and part of the
    /// contract.
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
