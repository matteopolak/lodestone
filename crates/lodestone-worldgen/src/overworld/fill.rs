//! Stages 1-4 of [`OverworldGenerator::column`]: the per-chunk aquifer, the
//! `fillFromNoise` shape pass, the surface-rule diff, materialisation into a dense
//! grid, and carvers — plus the uncached body of `pre_ore_stage`.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the pipeline order and for every measurement behind these stages.

use std::sync::Arc;

use crate::aquifer::{AquiferSystem, BlockKind, AnyPositionalFactory};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::Density;
use crate::engine::Program;
use crate::surface::{PreState, SurfaceDiff};

use super::{OverworldGenerator, PreOreResult};

/// The real aquifer's eight router outputs plus its positional RNG factory,
/// pre-built once from the same shared [`Builder`] that builds
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
    pub(super) positional: AnyPositionalFactory,
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
        // Structure placement's S3. Built here rather than passed in because the *only*
        // consumer is the fill below, and it must be built from this chunk's own
        // refs — a beardifier from a neighbouring chunk has a different junction
        // window and a different affected box.
        let beard = self.beardifier_for(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z, &beard);
        let heights = self.heights_from_field(&field);
        // The 4x4x4 grid is now the primary biome product and the
        // 16-entry surface array is read out of it. Two separate sample passes
        // would be two chances to diverge; see `biome_stage`.
        let biome_cells = self.biome_cells_stage(base_x, base_z);
        let biome_quarts = self.biome_stage(&biome_cells, &heights);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);

        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, &heights, &biome_quarts, base_x, base_z, world);
        // Structure placement's S2. A no-op (and free) for a generator with no structure
        // data, which is every fixture resolver in this workspace.
        let world = self.structure_place_stage(cx, cz, world);

        // `Arc` because `PreOreResult` hands this world out to
        // `vegetation_stage`'s rim sources rather than only into a mutating
        // consumer — see that alias's own doc.
        (Arc::new(world), heights, biome_quarts, Arc::new(biome_cells))
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

    /// Stage 1: `fillFromNoise` — shape + the **real** aquifer,
    /// replacing the sea-level approximation this generator used before.
    /// Returns a `16×height×16` dense field of [`BlockKind`] indexed by
    /// [`Self::idx`].
    ///
    /// # The two loops, and why they are two
    ///
    /// `beard` is vanilla's structure-adaptation density term, added directly
    /// onto the final noise-router density before the solidity check. For an
    /// **empty** beardifier — every chunk with no
    /// adaptation-bearing structure within reach, which is the overwhelming
    /// majority of the world — this runs the *original* loop, calling
    /// [`AquiferSystem::block_at`] with no addition at all.
    ///
    /// That branch is a correctness property, not a micro-optimisation, and it is
    /// what makes S3's negative control hold **by construction** rather than by
    /// measurement: adding `0.0` is the identity for every finite `f64` *except*
    /// `-0.0`, where it flips the sign bit. Nothing downstream distinguishes
    /// `-0.0` from `0.0` today (`compute_substance` only asks `density > 0.0`), but
    /// "nothing downstream distinguishes it today" is a claim about the rest of
    /// the pipeline, and the branch means it never has to be made.
    pub(super) fn fill_stage(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &crate::structure::beardifier::Beardifier,
    ) -> Vec<BlockKind> {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Shape);
        let height = self.height as usize;
        let mut field = vec![BlockKind::Air; 16 * 16 * height];
        if beard.is_empty() {
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    for ly in 0..self.height {
                        let wy = self.min_y + ly;
                        field[Self::idx(lx, ly, lz, self.height)] =
                            aquifer.block_at(base_x + lx, wy, base_z + lz);
                    }
                }
            }
            return field;
        }
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let wy = self.min_y + ly;
                    let (wx, wz) = (base_x + lx, base_z + lz);
                    field[Self::idx(lx, ly, lz, self.height)] =
                        aquifer.block_at_beard(wx, wy, wz, beard.compute(wx, wy, wz));
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
    ///
    /// # This stage was 92% of the pipeline's allocations
    ///
    /// Measured over a 3×3 cold sweep at seed 42 with real `GlobalAlloc` calls
    /// binned by innermost stage (`tests/ore_alloc_attribution.rs`), the three
    /// closures below allocated **3,847,972** times — 97.3% of that scene's
    /// heap traffic, and 18× the entire ore path, which the same instrument had
    /// just been pointed at. The cause was entirely representational: `pre`
    /// returned `String` (77.08% of the stage), `try_apply` returned
    /// `Option<String>` (21.92%), and `biome_at` cloned a biome name per column
    /// (0.35%). Nothing about the *scan* changed here — see
    /// `docs/worldgen-surface-ids.md`.
    ///
    /// `build_surface` used to derive air/fluid/stone from each probe's *name*;
    /// it now reads the class off the [`PreState`] this function hands it. The
    /// classes are **not** written down here: they come from
    /// `crate::surface::class_of_name` applied to the settings' own strings,
    /// once, in [`OverworldGenerator::new`] — see the `default_*_pre` fields.
    /// A wrong class would change which rules fire and still produce a
    /// plausible column, so the pairing is re-derived and asserted on every
    /// entry below rather than reasoned about.
    pub(super) fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        biome_quarts: &[(String, bool); 16],
        base_x: i32,
        base_z: i32,
    ) -> SurfaceDiff {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Surface);

        // The lock-step check on every `PreState` this stage can hand over.
        // It **re-derives** each one from the settings string it is supposed to
        // have come from, so it is total: it catches a wrong id and a wrong
        // class equally, and it catches the copy-paste that pairs
        // `default_block_pre` with `default_fluid`'s string — none of which a
        // constant-vs-constant assertion could see. Four `from_name` calls per
        // stage *entry* (not per probe): a warm `id_of` lookup each, and 25
        // entries over the whole 3×3 attribution sweep.
        //
        // `PreState::AIR` is the one value in this function still written as a
        // literal, which is exactly why it is on this list.
        debug_assert_eq!(
            self.default_block_pre,
            PreState::from_name(&self.interner, &self.default_block),
            "default_block_pre must be default_block, interned and classified"
        );
        debug_assert_eq!(
            self.default_fluid_pre,
            PreState::from_name(&self.interner, &self.default_fluid),
            "default_fluid_pre must be default_fluid, interned and classified"
        );
        debug_assert_eq!(
            self.default_lava_pre,
            PreState::from_name(&self.interner, &self.default_lava),
            "default_lava_pre must be default_lava, interned and classified"
        );
        debug_assert_eq!(
            PreState::AIR,
            PreState::from_name(&self.interner, "minecraft:air"),
            "PreState::AIR must be what class_of_name says minecraft:air is"
        );

        let pre = |lx: i32, y: i32, lz: i32| -> PreState {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return PreState::AIR;
            }
            match field[Self::idx(lx, ly, lz, self.height)] {
                BlockKind::Stone => self.default_block_pre,
                BlockKind::Water => self.default_fluid_pre,
                BlockKind::Lava => self.default_lava_pre,
                BlockKind::Air => PreState::AIR,
            }
        };
        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        let biome_at = |lx: i32, lz: i32| -> (&str, bool) {
            let (name, cold) = &biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize];
            (name.as_str(), *cold)
        };

        self.surface
            .build_surface(&pre, &heightmap, &biome_at, base_x, base_z)
    }

    /// Materialises the full `16×height×16` post-surface column into a
    /// dense, world-anchored grid (this change's Job 2 —
    /// [`crate::dense_grid::DenseBlockGrid`], not a `HashMap`) — the shape
    /// [`crate::carver::apply_carvers`] consumes via [`CarveGrid::from_dense`].
    /// Seeded from `field` (the same solid/fluid/air default
    /// [`Self::surface_stage`]'s own `pre` closure computes) and overlaid
    /// with the surface diff.
    pub(super) fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: SurfaceDiff,
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
        // directly. This was a real bug (this change's Job 2, found by
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
        //
        // U21: `set_id` rather than `set`, and pre-interned `default_*` ids
        // rather than `&str`. `DenseBlockGrid::set` is `id_of` + `set_id`, so
        // this deletes 98,304 block-state *string* hashes per chunk and changes
        // nothing else — the palette is still appended in this loop's order,
        // which is the property the comment above is about.
        // The ore-vein sampler. Bound to this chunk once, outside the loop,
        // because `vein_toggle`/`vein_ridged` are `minecraft:interpolated` and the
        // sampler's cell caches are per-chunk — see `super::veins`.
        let veins = self
            .veins
            .as_ref()
            .map(|v| v.for_chunk(self.slot_count, base_x, base_z, self.min_y, self.height));
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let y = self.min_y + ly;
                    let base = match field[Self::idx(lx, ly, lz, self.height)] {
                        BlockKind::Stone => self.default_block_pre.state,
                        BlockKind::Water => self.default_fluid_pre.state,
                        BlockKind::Lava => self.default_lava_pre.state,
                        BlockKind::Air => crate::interner::StateId::AIR,
                    };
                    // Veins replace the *default block* only: vanilla's material
                    // rule chain reaches vein replacement after the aquifer
                    // and only where it returned that block, never over air or a
                    // fluid.
                    let vein_state = if base == self.default_block_pre.state {
                        veins
                            .as_ref()
                            .and_then(|v| v.state_at(base_x + lx, y, base_z + lz))
                    } else {
                        None
                    };
                    // **A vein wins over the surface diff, and that is not an
                    // ordering shortcut.** Vanilla runs surface building *after* the
                    // fill that placed the vein, but that pass opens by comparing
                    // the cell against the default block before applying any rule
                    // — a cell holding copper ore or
                    // granite is not the default block, so every surface rule skips
                    // it. Applying the diff unconditionally here was measured to
                    // erase **every** vein cell: the overworld surface rules write
                    // `deepslate` over the whole column below y ≈ 0, so a vein at
                    // y = -40 came back out as deepslate and the served chunk was
                    // byte-identical with veins on and off. That is what made this
                    // an island for one debugging session.
                    //
                    // Known second-order gap: `surface_diff` is computed from the
                    // pre-vein `field`, so vanilla's `stone_depth_above/below`
                    // counters see vein blocks and ours do not. Narrow (it can only
                    // matter where a vein reaches the surface band) and not measured.
                    let state = match vein_state {
                        Some(v) => v,
                        None => surface_diff.get(&(lx, y, lz)).copied().unwrap_or(base),
                    };
                    world.set_id(base_x + lx, y, base_z + lz, state);
                }
            }
        }
        world
    }

    /// Stage 4: `applyCarvers` over the post-surface world grid.
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

    /// `pub(super)` rather than private so `structures.rs`'s `shape_index` can
    /// forward to it — see there for why a caller must never restate this
    /// expression.
    #[inline]
    pub(super) fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        debug_assert!((0..16).contains(&lx) && (0..16).contains(&lz));
        debug_assert!((0..height).contains(&ly));
        ((ly * 16 + lz) * 16 + lx) as usize
    }
}
