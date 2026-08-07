//! Stages 1-4 of [`OverworldGenerator::column`]: the per-chunk aquifer, the
//! `fillFromNoise` shape pass, the surface-rule diff, materialisation into a dense
//! grid, and carvers — plus the uncached body of `pre_ore_stage`.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the pipeline order and for every measurement behind these stages.

use std::collections::HashMap;
use std::sync::Arc;

use crate::aquifer::{AquiferSystem, BlockKind, XoroshiroPositionalFactory};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::Density;
use crate::engine::Program;

use super::{OverworldGenerator, PreOreResult};

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
pub(super) struct AquiferTrees {
    /// The three routes that become [`NoiseChunkSampler`]s, held as compiled
    /// [`Program`]s: cloning one is an `Arc` bump plus a `u32` copy.
    pub(super) final_density: Program,
    pub(super) erosion: Program,
    pub(super) depth: Program,
    /// The five point-evaluated routes, behind `Arc` for the same reason.
    pub(super) barrier: Arc<Density>,
    pub(super) floodedness: Arc<Density>,
    pub(super) spread: Arc<Density>,
    pub(super) lava: Arc<Density>,
    pub(super) prelim: Arc<Density>,
    pub(super) positional: XoroshiroPositionalFactory,
}

impl OverworldGenerator {
    /// The actual stages 1-4 computation [`Self::pre_ore_stage`] memoises.
    /// Never call this directly outside that wrapper — doing so bypasses the
    /// cache and reintroduces the exact 9× redundancy this cache exists to
    /// remove.
    pub(super) fn pre_ore_stage_uncached(&self, cx: i32, cz: i32) -> PreOreResult {
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

    /// Builds a fresh, chunk-bound [`AquiferSystem`] from this generator's
    /// pre-built [`AquiferTrees`] — matching vanilla's own per-chunk
    /// `NoiseChunk`, which the aquifer's internal grid-bound caches assume.
    /// Every `clone()` below is a **refcount bump**, not a tree copy. Before U4
    /// these were eight recursive deep copies of `Box`-linked `Density` trees
    /// (232 bytes per node) on every chunk — diagnostic D3. That is why the
    /// field types are `Program` and `Arc<Density>`: nothing else in this
    /// function changed.
    pub(super) fn build_aquifer(&self, cx: i32, cz: i32) -> AquiferSystem {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Aquifer);
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
    pub(super) fn fill_stage(&self, aquifer: &AquiferSystem, base_x: i32, base_z: i32) -> Vec<BlockKind> {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Shape);
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
    pub(super) fn heights_from_field(&self, field: &[BlockKind]) -> [i32; 256] {
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

    /// Stage 3: surface rules over the pre-surface (post-fill) column.
    /// Returns a **sparse diff** (see [`SurfaceSystem::build_surface`]): only
    /// the positions a surface rule actually rewrote.
    pub(super) fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        biome_quarts: &[(String, bool); 16],
        base_x: i32,
        base_z: i32,
    ) -> HashMap<(i32, i32, i32), String> {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Surface);
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
    pub(super) fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: HashMap<(i32, i32, i32), String>,
        base_x: i32,
        base_z: i32,
    ) -> crate::dense_grid::DenseBlockGrid {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Materialize);
        let mut world = crate::dense_grid::DenseBlockGrid::with_interner(
            Arc::clone(&self.interner),
            base_x,
            self.min_y,
            base_z,
            16,
            self.height,
            16,
            crate::interner::StateId::AIR,
        );
        // `surface_diff` is consulted by **point lookup**, in the same fixed
        // `(lz, lx, ly)` order as the base fill below — never iterated
        // directly. This was a real bug (issue #295's Job 2, found by
        // `worldgen_data::tests::column_is_byte_identical_across_two_independently_constructed_generators`):
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
    pub(super) fn carve_stage(
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
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Carve);
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
            crate::dense_grid::DenseBlockGrid::from_hashmap_with_interner(
                Arc::clone(&self.interner),
                base_x,
                self.min_y,
                base_z,
                16,
                self.height,
                16,
                &grid.into_blocks(),
            )
        } else {
            grid.into_dense()
        }
    }

    #[inline]
    fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        debug_assert!((0..16).contains(&lx) && (0..16).contains(&lz));
        debug_assert!((0..height).contains(&ly));
        ((ly * 16 + lz) * 16 + lx) as usize
    }
}
