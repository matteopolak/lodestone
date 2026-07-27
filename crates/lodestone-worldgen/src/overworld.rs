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

use crate::density::{Builder, Density, NoiseChunkSampler, Resolver};
use crate::surface::{SurfaceSystem, identity_canon};

/// Overworld cell dimensions (`NoiseSettings.getCellWidth()/getCellHeight()`).
const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;

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
}

impl OverworldGenerator {
    /// Builds the generator for `seed` from a noise-settings `Value` and a
    /// [`Resolver`] that supplies the density functions and noises it references.
    ///
    /// `biome` is the fixed biome id (e.g. `"minecraft:plains"`) generation runs
    /// under until the multi-noise biome source exists; `cold_enough_to_snow` is
    /// that biome's answer (only the `temperature` surface condition consults it).
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
        let surface = SurfaceSystem::new(settings, &builder, biome, &canon, cold_enough_to_snow);

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

        Self {
            final_density,
            slot_count,
            surface,
            min_y,
            height,
            sea_level,
            default_block,
            default_fluid,
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
        let height = self.height as usize;

        // Stage 1: shape. One fresh sampler per chunk mirrors vanilla's per-chunk
        // `NoiseChunk` and bounds the interpolation-corner cache.
        let sampler = NoiseChunkSampler::new(
            self.final_density.clone(),
            self.slot_count,
            CELL_WIDTH,
            CELL_HEIGHT,
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

        // Stage 2: fluid fill (sea-level aquifer approximation) + WORLD_SURFACE_WG.
        // `heights[lz*16+lx]` = highest non-air world Y (solid, or water up to sea
        // level over submerged columns).
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

        // Stage 3: surface rules over the pre-surface column.
        let post = self.surface.build_surface(&pre, &heightmap, base_x, base_z);

        // Intern into a dense palette-indexed grid.
        let mut palette: Vec<String> = vec!["minecraft:air".to_string()];
        let mut blocks = vec![0u16; 16 * 16 * height];
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
        }
    }

    #[inline]
    fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        debug_assert!((0..16).contains(&lx) && (0..16).contains(&lz));
        debug_assert!((0..height).contains(&ly));
        ((ly * 16 + lz) * 16 + lx) as usize
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
}
