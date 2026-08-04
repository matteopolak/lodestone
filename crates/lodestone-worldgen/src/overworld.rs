//! Composed overworld chunk generation: the version-free driver that chains the
//! proven stages into a single "give me the blocks in chunk `(cx, cz)`" call.
//!
//! Everything below this module is a *stage* proven bit-for-bit against a JVM in
//! isolation (`region_parity`, `chunk_parity`, `surface_parity`, …). This module
//! is the glue that runs them in sequence so a caller — the integrated server, or
//! the shell's local world — gets real terrain instead of a stand-in. It holds
//! **no data**: the noise settings `Value` and every density function / noise it
//! references arrive through a [`Resolver`], exactly as the parity tests supply
//! them, so the engine stays version-free (plan §3).
//!
//! # Composed pipeline (and its honest scope)
//!
//! For each block it runs:
//! 1. **Shape** — [`NoiseChunkSampler`] evaluates the interpolated `final_density`
//!    field (the same code `chunk_parity` proves 98304/98304 block-for-block).
//!    `density > 0` ⇒ the settings' `default_block` (stone), else non-solid.
//! 2. **Fluid fill** — a non-solid block *below* `sea_level` becomes
//!    `default_fluid` (water), matching vanilla's default aquifer fluid picker
//!    (`y < seaLevel`, so the top water block sits at `sea_level - 1`). This is a **sea-level approximation of aquifers**:
//!    it reproduces oceans, beaches and the ocean floor (the surface-visible
//!    behaviour) but not underground water/lava pockets, which only matter once
//!    carvers cut caves. The real [`crate::aquifer`] is proven separately and is
//!    the drop-in replacement for this step.
//! 3. **Surface rules** — [`SurfaceSystem::build_surface`] rewrites the top of
//!    each column into grass/dirt/sand/gravel/etc. (the code `surface_parity`
//!    proves block-for-block), driven by the `surface_rule` data.
//!
//! **Not yet composed here:** carvers (no caves), the real aquifer, and features
//! (no ores/vegetation/trees). Those stages exist and are individually verified;
//! chaining and *whole-chunk* re-verifying them against a full-generation oracle
//! is the follow-up. What this module produces is therefore honest real terrain
//! *shape + surface*, not a block-for-block-complete vanilla chunk — and it says
//! so rather than looking finished.
//!
//! # Biome scope
//!
//! The multi-noise biome source is not built yet, so generation runs a single
//! fixed biome ([`OverworldGenerator::new`] takes it). Surface rules and the
//! noise router are biome-parameterised the moment that source lands; nothing
//! here has to change.

use serde_json::Value;

use crate::biome::{BiomeParameterPoint, ClimateSampler};
use crate::density::{Builder, Density, NoiseChunkSampler, Resolver};
use crate::surface::{SurfaceSystem, identity_canon};

/// Overworld cell dimensions (`NoiseSettings.getCellWidth()/getCellHeight()`).
const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;

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

/// A composed, reusable overworld generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
#[allow(missing_debug_implementations)]
pub struct OverworldGenerator {
    final_density: Density,
    slot_count: usize,
    surface: SurfaceSystem,
    min_y: i32,
    height: i32,
    sea_level: i32,
    default_block: String,
    default_fluid: String,
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
}

impl OverworldGenerator {
    /// Builds the generator for `seed` from a noise-settings `Value` and a
    /// [`Resolver`] that supplies the density functions and noises it
    /// references.
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
        let final_density = builder.build(&settings["noise_router"]["final_density"]);
        let slot_count = builder.slot_count();
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

        Self {
            final_density,
            slot_count,
            surface,
            min_y,
            height,
            sea_level,
            default_block,
            default_fluid,
            fallback_biome: biome.to_string(),
            fallback_cold_enough_to_snow: cold_enough_to_snow,
            dynamic_biome,
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

        let solid = self.shape_stage(base_x, base_z);
        let heights = self.fluid_heightmap_stage(&solid);
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let post = self.surface_stage(&solid, &heights, &biome_quarts, base_x, base_z);
        self.intern_stage(&solid, post, biome_quarts)
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
        let solid = self.shape_stage(base_x, base_z);
        let t1 = std::time::Instant::now();
        let heights = self.fluid_heightmap_stage(&solid);
        let biome_quarts = self.biome_stage(&heights, base_x, base_z);
        let t2 = std::time::Instant::now();
        let post = self.surface_stage(&solid, &heights, &biome_quarts, base_x, base_z);
        let t3 = std::time::Instant::now();
        let col = self.intern_stage(&solid, post, biome_quarts);
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

    /// Stage 1: shape. One fresh sampler per chunk mirrors vanilla's per-chunk
    /// `NoiseChunk` and bounds the interpolation-corner cache. Returns a
    /// `16×height×16` solid mask indexed by [`Self::idx`].
    ///
    /// Uses [`NoiseChunkSampler::new_bounded`] (a bounded, hash-free dense
    /// array in place of `slot_get`'s `HashMap`, `src/density/chunk.rs`'s
    /// `DenseShape`) rather than [`NoiseChunkSampler::new`], because every
    /// query this loop makes is known in advance to lie within exactly this
    /// chunk's `(base_x..=base_x+15, min_y..=min_y+height-1,
    /// base_z..=base_z+15)` — the bounded-sampler contract `new_bounded`
    /// documents. `docs/worldgen-surface-perf.md` has the profiling story and
    /// why this is a narrower, lower-risk fix than adopting vanilla's
    /// incremental cell-walk outright.
    fn shape_stage(&self, base_x: i32, base_z: i32) -> Vec<bool> {
        let height = self.height as usize;
        let sampler = NoiseChunkSampler::new_bounded(
            self.final_density.clone(),
            self.slot_count,
            CELL_WIDTH,
            CELL_HEIGHT,
            (base_x, base_x + 15),
            (self.min_y, self.min_y + self.height - 1),
            (base_z, base_z + 15),
        );
        let mut solid = vec![false; 16 * 16 * height];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let wy = self.min_y + ly;
                    let d = sampler.final_density(base_x + lx, wy, base_z + lz);
                    if d > 0.0 {
                        solid[Self::idx(lx, ly, lz, self.height)] = true;
                    }
                }
            }
        }
        solid
    }

    /// Stage 2: fluid fill (sea-level aquifer approximation) + WORLD_SURFACE_WG.
    /// `heights[lz*16+lx]` = highest non-air world Y (solid, or water up to sea
    /// level over submerged columns).
    fn fluid_heightmap_stage(&self, solid: &[bool]) -> [i32; 256] {
        let mut heights = [i32::MIN; 16 * 16];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let mut highest_solid = self.min_y - 1;
                for ly in (0..self.height).rev() {
                    if solid[Self::idx(lx, ly, lz, self.height)] {
                        highest_solid = self.min_y + ly;
                        break;
                    }
                }
                heights[(lz * 16 + lx) as usize] = highest_solid.max(self.sea_level - 1);
            }
        }
        heights
    }

    /// Stage "biome" (issue #405): one climate sample per horizontal quart
    /// `(qx, qz)` in `0..4`, row-major `qz * 4 + qx` — 16 per chunk, matching
    /// [`lodestone_world`](crate)'s own `ChunkSection::BIOME_EDGE` (4) so a
    /// future encoder can write this straight into a real biome container.
    /// Broadcast vertically: see `crate::biome`'s module doc for why one
    /// sample per quart *column*, not a full 3-D grid, is this phase's
    /// deliberate scope.
    ///
    /// Each quart samples at its own **already-generated surface height**
    /// (`heights[]`, this chunk's stage-2 output) rather than a fixed Y — the
    /// module doc's "y = 0 trap" section is why a constant height silently
    /// produces almost all cave/deep-ocean biomes instead of the terrain
    /// biome a player standing there would actually see.
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
            // at its own **corner** (`qx*4`), not its center. This matches
            // vanilla exactly: `Climate.Sampler.sample(quartX, quartY,
            // quartZ)` converts back to block space via
            // `QuartPos.toBlock(quartX) == quartX << 2`, i.e. the quart's
            // minimum corner — verified the hard way: an earlier version of
            // this code sampled at `qx*4 + 2` (the quart's center, matching
            // `ChunkSection::biome_at_block`'s `>> 2` *membership* test,
            // which is a different question from *where a quart's own
            // sample point sits*) and it JVM-parity-mismatched at a
            // dark_forest/river boundary the corner convention gets right —
            // `[1217,5285,-900,1346,-293,-495,0]` (center, wrong biome) vs.
            // the oracle's `[1223,5292,-882,1325,-118,-517]` at the
            // *corner*, which this now reproduces exactly.
            let lx = qx * 4;
            let lz = qz * 4;
            // Y needs the same quart-rounding as X/Z, and it is easy to miss
            // since it is not part of `biome_stage`'s own loop variables —
            // `heights[]` is a real terrain surface Y, essentially never a
            // multiple of 4. Vanilla's `Climate.Sampler.sample(quartX,
            // quartY, quartZ)` floors *every* axis to `QuartPos.toBlock`
            // before evaluating, and skipping this for Y alone reproduced
            // the exact bug the X/Z fix above already fixed once: the
            // `depth` channel came out 156 quantized units off
            // (`y_clamped_gradient`'s slope, `3.0 / 384` per block, times
            // the 2-block rounding error) — right table, right search, just
            // sampling 2 blocks away from where vanilla would.
            let y = (heights[(lz * 16 + lx) as usize] >> 2) << 2;
            let target = dynamic.climate.target(base_x + lx, y, base_z + lz);
            let name = crate::biome::nearest_biome(&dynamic.table, &target);
            let cold = crate::biome::cold_enough_to_snow(&dynamic.temperatures, name);
            (name.to_string(), cold)
        })
    }

    /// Stage 4: surface rules over the pre-surface (shape + fluid) column.
    /// Returns a **sparse diff** (see [`SurfaceSystem::build_surface`]): only
    /// the positions a surface rule actually rewrote. [`Self::intern_stage`]
    /// reconstructs the full column by seeding from `solid` (the same
    /// shape+fluid default this stage's own `pre` closure computes) and
    /// overlaying this diff, rather than this stage materialising all
    /// 16×16×`height` positions itself.
    fn surface_stage(
        &self,
        solid: &[bool],
        heights: &[i32; 256],
        biome_quarts: &[(String, bool); 16],
        base_x: i32,
        base_z: i32,
    ) -> std::collections::HashMap<(i32, i32, i32), String> {
        // Pre-surface block string at a local column position (aquifer-filled).
        let pre = |lx: i32, y: i32, lz: i32| -> String {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return "minecraft:air".to_string();
            }
            if solid[Self::idx(lx, ly, lz, self.height)] {
                self.default_block.clone()
            } else if y < self.sea_level {
                self.default_fluid.clone()
            } else {
                "minecraft:air".to_string()
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

    /// Stage 4: intern the surface-rewritten column into a dense
    /// palette-indexed grid.
    ///
    /// `post` is [`Self::surface_stage`]'s sparse diff, not a full column, so
    /// this seeds `blocks` from `solid` first — exactly the same
    /// solid/fluid/air default `surface_stage`'s own `pre` closure computes,
    /// just written straight into the dense grid instead of round-tripped
    /// through a `String`-keyed `HashMap` — and then overlays the (much
    /// smaller) set of positions the surface rules actually changed.
    fn intern_stage(
        &self,
        solid: &[bool],
        post: std::collections::HashMap<(i32, i32, i32), String>,
        biome_quarts: [(String, bool); 16],
    ) -> GeneratedColumn {
        let height = self.height as usize;
        let mut palette: Vec<String> = vec!["minecraft:air".to_string()];
        let mut blocks = vec![0u16; 16 * 16 * height];

        // Seed from shape + fluid fill (stage 1/2 output), matching
        // `surface_stage`'s `pre` closure: solid -> default_block, else
        // below sea level -> default_fluid, else air (already 0).
        let stone_id = palette.len() as u16;
        palette.push(self.default_block.clone());
        let fluid_id = palette.len() as u16;
        palette.push(self.default_fluid.clone());
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let idx = Self::idx(lx, ly, lz, self.height);
                    if solid[idx] {
                        blocks[idx] = stone_id;
                    } else if self.min_y + ly < self.sea_level {
                        blocks[idx] = fluid_id;
                    }
                }
            }
        }

        // Overlay the surface-rule diff.
        for ((lx, y, lz), state) in post {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                continue;
            }
            let id = match palette.iter().position(|p| p == &state) {
                Some(i) => i as u16,
                None => {
                    palette.push(state);
                    (palette.len() - 1) as u16
                }
            };
            blocks[Self::idx(lx, ly, lz, self.height)] = id;
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
/// Stage boundaries match the doc comment on [`OverworldGenerator`]: shape,
/// fluid fill + heightmap, surface rules, and palette interning. `shape` and
/// `surface` are the two stages named in `HANDOFF.md` §4's original (deleted)
/// split; `fluid_heightmap` and `intern` were folded into "surface"/"intern"
/// there but are broken out here since they are now separate functions.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct StageTimes {
    pub shape: std::time::Duration,
    /// Fluid fill + heightmap **and** biome sampling (issue #405's
    /// [`OverworldGenerator::biome_stage`] runs between them and is folded
    /// into this bucket rather than earning a fifth field every existing
    /// caller of this struct would need to learn about).
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
