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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::dense_grid::DenseBlockGrid;
use crate::density::{Builder, Resolver};
use crate::engine::Program;
use crate::interner::StateInterner;
use crate::noise::EndIslandNoise;
use crate::surface::{PreState, SurfaceDiff, SurfaceSystem, identity_canon};

mod podium;
mod spikes;
mod decorate;

pub use podium::{PodiumBlock, end_podium};
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
}

impl EndBiomeSource {
    /// Builds the source for `seed`. Constructs its own [`EndIslandNoise`] because
    /// vanilla's `erosion` channel is `cache2d(endIslands(seed))` and nothing else
    /// feeds it.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            islands: EndIslandNoise::new(seed),
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
    pub fn biome_at_quart(&self, quart_x: i32, _quart_y: i32, quart_z: i32) -> &'static str {
        let block_x = quart_x * 4;
        let block_z = quart_z * 4;
        let chunk_x = block_x >> 4;
        let chunk_z = block_z >> 4;
        if i64::from(chunk_x) * i64::from(chunk_x) + i64::from(chunk_z) * i64::from(chunk_z)
            <= MAIN_ISLAND_CHUNKS_SQUARED
        {
            return THE_END;
        }
        // `weirdBlockX` — the chunk *centre*, not the quart's own position, so all
        // 16 quarts of a chunk share one sample.
        let weird_block_x = (chunk_x * 2 + 1) * 8;
        let weird_block_z = (chunk_z * 2 + 1) * 8;
        let height = self.islands.compute(weird_block_x, weird_block_z);
        if height > 0.25 {
            END_HIGHLANDS
        } else if height >= -0.0625 {
            END_MIDLANDS
        } else if height < -0.21875 {
            SMALL_END_ISLANDS
        } else {
            END_BARRENS
        }
    }

    /// The 16 horizontal quart biomes of chunk `(cx, cz)`.
    ///
    /// All 16 are equal for any chunk, because the erosion sample is taken at the
    /// chunk centre — that is vanilla's behaviour, not a simplification, and the
    /// array shape is kept so a caller building a biome container writes the same
    /// loop it would for the Nether.
    #[must_use]
    pub fn chunk_quarts(&self, cx: i32, cz: i32) -> [&'static str; 16] {
        std::array::from_fn(|i| {
            self.biome_at_quart(cx * 4 + (i % 4) as i32, 0, cz * 4 + (i / 4) as i32)
        })
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
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    biome_quarts: [&'static str; 16],
    gateways: Vec<decorate::EndGateway>,
}

impl EndColumn {
    /// World Y of the lowest block row (0 for the End).
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows (128 for the End).
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
    pub fn biome_at_quart(&self, qx: usize, qz: usize) -> &'static str {
        self.biome_quarts[qz * 4 + qx]
    }

    /// The biome covering local column `(lx, lz)`.
    #[must_use]
    pub fn biome_at(&self, lx: usize, lz: usize) -> &'static str {
        self.biome_at_quart(lx >> 2, lz >> 2)
    }

    /// Return-gateway exits generated in this column.  The gateway block is in
    /// the palette; this sidecar is the block-entity data a server must retain.
    #[must_use]
    pub fn gateways(&self) -> &[decorate::EndGateway] {
        &self.gateways
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
    pub fn into_raw(self) -> (i32, i32, Vec<String>, Vec<u16>, [&'static str; 16]) {
        (
            self.min_y,
            self.height,
            self.palette,
            self.blocks,
            self.biome_quarts,
        )
    }
}

/// A composed, reusable End generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
///
/// **Demand-ordered and order-independent.** Nothing memoises across chunks and no
/// stage reads a neighbouring chunk's product — there is no carver here, so not even
/// the Nether's 17×17 re-derivation — so `column` is a pure function of
/// `(seed, cx, cz)` and columns may be requested in any order, on any thread,
/// without changing a byte.
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
    structures: Option<crate::structure::StructureRegistry>,
}

impl EndGenerator {
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
            structures,
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

    /// The generated column for chunk `(cx, cz)`.
    ///
    /// The End's served order: fill, biome, surface, materialise, structures,
    /// then decoration. No End biome names a carver.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> EndColumn {
        let (world, gateways) = self.decoration_region(cx, cz);
        let biome_quarts = self.biomes.chunk_quarts(cx, cz);
        let mut served = DenseBlockGrid::with_interner(
            self.interner.clone(),
            cx * 16,
            self.min_y,
            cz * 16,
            16,
            self.height,
            16,
            self.interner.id_of("minecraft:air"),
        );
        for y in self.min_y..self.min_y + self.height {
            for z in cz * 16..cz * 16 + 16 {
                for x in cx * 16..cx * 16 + 16 {
                    let state = world.get_id(x, y, z);
                    served.set_id(x, y, z, state);
                }
            }
        }
        let (palette, blocks) = served.into_palette_and_blocks();
        EndColumn { min_y: self.min_y, height: self.height, palette, blocks, biome_quarts, gateways }
    }

    /// Complete structure starts whose origin is `(cx, cz)`.
    ///
    /// City placement uses this same start calculation before looking through
    /// nearby starts for pieces that touch a served chunk.
    #[must_use]
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Vec<crate::structure::StructureStart> {
        let Some(registry) = &self.structures else {
            return Vec::new();
        };
        let sampler = EndStartSampler::new(self);
        registry
            .starts_at(cx, cz, &sampler)
            .into_iter()
            .filter(|start| start.pieces_complete)
            .collect()
    }

    fn base_world(&self, cx: i32, cz: i32) -> DenseBlockGrid {
        let base_x = cx * 16;
        let base_z = cz * 16;
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
        let biome_quarts = self.biomes.chunk_quarts(cx, cz);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
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
        self.structure_place_stage(cx, cz, world)
    }

    /// Places every complete End-city piece intersecting this chunk after
    /// materialization and before the palette is extracted. City pieces have no
    /// terrain adaptation or later refinement, so a bounded start scan and the
    /// grid's normal write clipping are sufficient.
    fn structure_place_stage(&self, cx: i32, cz: i32, mut world: DenseBlockGrid) -> DenseBlockGrid {
        const START_SCAN_RADIUS: i32 = 16;
        let Some(registry) = &self.structures else {
            return world;
        };
        let sampler = EndStartSampler::new(self);
        let (min_x, min_z) = (cx * 16, cz * 16);
        for start_x in cx - START_SCAN_RADIUS..=cx + START_SCAN_RADIUS {
            for start_z in cz - START_SCAN_RADIUS..=cz + START_SCAN_RADIUS {
                for start in registry.starts_at(start_x, start_z, &sampler) {
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
                                world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                            }
                        }
                        if let Some(placement) = &piece.placement {
                            let origin = crate::structure::template::PlaceOrigin {
                                position: placement.position,
                                reference,
                                seed: registry.seed(),
                            };
                            placement.template.place(origin, &placement.settings, &mut world);
                            for extra in &piece.extra_placements {
                                let origin = crate::structure::template::PlaceOrigin {
                                    position: extra.position,
                                    reference,
                                    seed: registry.seed(),
                                };
                                extra.template.place(origin, &extra.settings, &mut world);
                            }
                        }
                    }
                }
            }
        }
        world
    }

    fn decoration_region(&self, cx: i32, cz: i32) -> (DenseBlockGrid, Vec<decorate::EndGateway>) {
        let mut region = DenseBlockGrid::with_interner(
            self.interner.clone(),
            (cx - 1) * 16,
            self.min_y,
            (cz - 1) * 16,
            48,
            self.height,
            48,
            self.interner.id_of("minecraft:air"),
        );
        for source_x in cx - 1..=cx + 1 {
            for source_z in cz - 1..=cz + 1 {
                let source = self.base_world(source_x, source_z);
                for y in self.min_y..self.min_y + self.height {
                    for z in source_z * 16..source_z * 16 + 16 {
                        for x in source_x * 16..source_x * 16 + 16 {
                            region.set_id(x, y, z, source.get_id(x, y, z));
                        }
                    }
                }
            }
        }
        let gateways = self.decoration.apply_region(
            self.seed,
            cx,
            cz,
            &mut region,
            |source_x, source_z| self.biomes.biome_at_quart(source_x * 4, 0, source_z * 4),
        );
        (region, gateways)
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
            assert!(
                quarts.iter().all(|b| *b == quarts[0]),
                "chunk ({cx},{cz}) is not uniform: {quarts:?}"
            );
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
