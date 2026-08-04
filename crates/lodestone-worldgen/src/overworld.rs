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
//! **Ore features are deliberately still not composed here**, even though the
//! engine (`crate::feature::apply_ore_step_3x3`) and the per-biome resolution
//! glue (`crate::compose::build_biome_ores`) both exist and the ORACLE this
//! gap depended on has since been fixed. An architecture review of this exact
//! composition originally found that `FeatureOracle.java` — the oracle
//! `feature_parity` validates the ore engine against — shared the
//! simplification it was supposed to be checking: its own header used to say
//! it "deliberately does NOT model ore spill from the 8 neighbouring chunks
//! into the centre," and `OreInput::get_height`/`in_center` (`crate::feature`)
//! used to wrap/drop edge probes to match that oracle rather than vanilla's
//! real `blockStateWriteRadius(1)` 3×3 driver (`ChunkPyramid.java:32-35`).
//! **That part is now fixed**: `FeatureOracle.java` drives a real 3×3
//! neighbourhood (memoised per-chunk generation, clamped beyond it — see its
//! own doc comment for the measured residual), `OreInput::region_local`
//! replaces the old wrap/drop, and `ComposedChunkOracle.java` has a
//! `postfeatures` stage (single-source only there — see that stage's own doc
//! comment in `ComposedChunkOracle.java` for why it is narrower than
//! `FeatureOracle.java`'s fixture). The parity numbers this produced (measured
//! against the *fixed* oracle, not the one that shared its own bug) are in
//! `docs/worldgen-parity.md`.
//!
//! What is **still** the next increment: wiring `apply_ore_step_3x3` into
//! this module's own `column()` (real per-quart biome instead of a fixed
//! biome, and the choke-point-file discipline that composing over
//! `CarveGrid`-shaped HashMaps was too slow for at carver scale — see this
//! module's own carver step above and `CLAUDE.md`'s "do not compose ores over
//! the String-keyed HashMap grids" note, which applies here identically).
//! Landing the oracle fix first and the composition second (rather than
//! together) is deliberate: composing against a wrong oracle would have baked
//! a wrong ~13-block edge-case band into every chunk with no gate able to see
//! it, per the same review.
//!
//! **Still not composed:** ore features (see above), vegetation/tree
//! features (unbuilt anywhere in this crate, epic #404 Phase 3), and
//! structures (unbuilt anywhere in this repo, `#136`). `docs/worldgen-parity.md`
//! measures the composed subset (shape + real aquifer + biome + surface +
//! carvers) against a real vanilla JVM.
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
    /// can produce — see `crate::compose::build_biome_carvers`. Ore features
    /// are not composed here yet (see the module doc's "ore features are
    /// deliberately not composed" section), so there is no `ores_by_biome`/
    /// `ore_tag_map` sibling — `crate::compose::build_biome_ores`/
    /// `build_ore_tag_map` exist and are unit-tested, ready for the follow-up
    /// that wires them in once the harness can verify the result.
    carvers_by_biome: HashMap<String, Vec<CarverConfig>>,
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
        for name in &biome_names {
            carvers_by_biome.insert(
                name.clone(),
                crate::compose::build_biome_carvers(resolver, name),
            );
        }

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
        let base_x = cx * 16;
        let base_z = cz * 16;

        let aquifer = self.build_aquifer(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z);
        let heights = self.heights_from_field(&field);
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);

        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);

        self.intern_from_world(&world, base_x, base_z, biome_quarts)
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
        let col = self.intern_from_world(&world, base_x, base_z, biome_quarts);
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
    /// world-coordinate-keyed grid — the shape [`crate::carver::apply_carvers`]
    /// consumes. Seeded from `field` (the same solid/fluid/air default
    /// [`Self::surface_stage`]'s own `pre` closure computes) and overlaid
    /// with the surface diff.
    fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: HashMap<(i32, i32, i32), String>,
        base_x: i32,
        base_z: i32,
    ) -> HashMap<(i32, i32, i32), String> {
        let mut world = HashMap::with_capacity(16 * 16 * self.height as usize);
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
                    world.insert((base_x + lx, y, base_z + lz), base.to_string());
                }
            }
        }
        for ((lx, y, lz), state) in surface_diff {
            world.insert((base_x + lx, y, base_z + lz), state);
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
        world: HashMap<(i32, i32, i32), String>,
    ) -> HashMap<(i32, i32, i32), String> {
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

        let mut grid = CarveGrid::new(world);
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
        grid.into_blocks()
    }

    /// Interns the final (post-carve) world grid into a dense palette-indexed
    /// [`GeneratedColumn`]. Ore-feature composition (issue #295's next
    /// increment — see the module doc) will insert its own stage between
    /// [`Self::carve_stage`] and this one once the harness can verify it.
    fn intern_from_world(
        &self,
        world: &HashMap<(i32, i32, i32), String>,
        base_x: i32,
        base_z: i32,
        biome_quarts: [(String, bool); 16],
    ) -> GeneratedColumn {
        let height = self.height as usize;
        let mut palette: Vec<String> = vec!["minecraft:air".to_string()];
        let mut index_of: HashMap<String, u16> = HashMap::new();
        index_of.insert("minecraft:air".to_string(), 0);
        let mut blocks = vec![0u16; 16 * 16 * height];

        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let y = self.min_y + ly;
                    let state = world
                        .get(&(base_x + lx, y, base_z + lz))
                        .map(String::as_str)
                        .unwrap_or("minecraft:air");
                    let id = if let Some(&id) = index_of.get(state) {
                        id
                    } else {
                        let id = palette.len() as u16;
                        palette.push(state.to_string());
                        index_of.insert(state.to_string(), id);
                        id
                    };
                    blocks[Self::idx(lx, ly, lz, self.height)] = id;
                }
            }
        }

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
