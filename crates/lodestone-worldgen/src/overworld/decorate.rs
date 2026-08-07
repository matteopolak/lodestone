//! Stages 5-7 of [`OverworldGenerator::column`]: the `UNDERGROUND_ORES` and
//! `VEGETAL_DECORATION` 3×3 neighbourhood drivers, `TOP_LAYER_MODIFICATION`, and the
//! two region stitches that feed them.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the parity history of the 3×3 drivers.

use std::collections::HashMap;
use std::sync::Arc;

use crate::feature::{PlacedOre, apply_ore_step_3x3_per_source};
use crate::rng::{WorldgenRandom, XoroshiroRandomSource};

use super::OverworldGenerator;

impl OverworldGenerator {
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
    pub(super) fn ore_stage(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
    ) -> crate::dense_grid::DenseBlockGrid {
        if self.ores_by_biome.values().all(Vec::is_empty) {
            return center_world;
        }
        // Entered AFTER the no-data early return, deliberately: `stage_entered`
        // must count stages that did real work, not stages that were called.
        // That is what makes it a detector for the "world" species of vacuous
        // benchmark — this file's own documented history is a resolver that
        // supplied no ore data, so `ore_stage` early-returned while a percentage
        // table went on looking plausible. A counter placed above this `if`
        // would have reported "ore ran once per chunk" for that exact run.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Ore);

        // Issue #106: a `DenseBlockGrid`, not a `HashMap<(i32,i32,i32),
        // String>` — see `crate::feature::RegionGrid`'s own doc for why.
        // `"minecraft:air"` as the default is a real behaviour match, not
        // just a placeholder: the old `HashMap`'s `.get(&key)` returned
        // `None` (folded to `"minecraft:air"` by every reader) for any cell
        // `stitch_region` hadn't visited yet, which never actually happens
        // in the covered region — every one of the 9 sources always stitches
        // its own full 16-wide column range before ore placement runs.
        let region_size = crate::feature::REGION_MAX - crate::feature::REGION_MIN;
        let mut region = crate::feature::RegionGrid::with_interner(
            Arc::clone(&self.interner),
            crate::feature::REGION_MIN,
            self.min_y,
            crate::feature::REGION_MIN,
            region_size,
            self.height,
            region_size,
            crate::interner::StateId::AIR,
        );
        let mut ocean_floor_wg: HashMap<(i32, i32), i32> = HashMap::new();

        Self::stitch_region(&mut region, &mut ocean_floor_wg, cx, cz, cx, cz, &center_world, center_heights);
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                // No clone: `stitch_region` only ever reads through a
                // reference, so a cache hit here (the common case in a
                // sweep — see `pre_ore_cache`'s doc comment) costs one
                // `HashMap` lookup and an `Arc` bump, not a recomputed
                // pipeline or a copied grid.
                let cached = self.pre_ore_stage(cx + dx, cz + dz);
                Self::stitch_region(&mut region, &mut ocean_floor_wg, cx + dx, cz + dz, cx, cz, &cached.0, &cached.1);
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
        // Unconditional, unlike the old `HashMap`'s `if let Some(state) =
        // region.get(&key)`: the centre's own 16×16×height range is always
        // fully stitched before ore placement runs (the very first
        // `stitch_region` call above), so every cell this loop reads was
        // always already written — a `DenseBlockGrid` has no separate
        // "touched" bit to check, and none is needed here.
        for y in self.min_y..self.min_y + self.height {
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let state = region.get(lx, y, lz);
                    center_world.set(cx * 16 + lx, y, cz * 16 + lz, state);
                }
            }
        }
        center_world
    }

    /// Copies one source chunk's own post-carve world/heights into a shared
    /// 3×3 region grid, translating its absolute coordinates into
    /// centre-relative local coordinates (`crate::feature::REGION_MIN..
    /// REGION_MAX` on each axis, matching [`crate::feature::OreInput::region_local`]'s
    /// key space).
    ///
    /// `world` is read via [`crate::dense_grid::DenseBlockGrid::get`] (O(1)
    /// array access, issue #295's Job 2) and written into `region` — also a
    /// [`crate::dense_grid::DenseBlockGrid`] since issue #106 — via
    /// [`crate::dense_grid::DenseBlockGrid::set`], which interns each
    /// distinct state string once in `region`'s own palette rather than
    /// heap-allocating a fresh `String` per cell the way a
    /// `HashMap<(i32,i32,i32), String>`'s `.insert` used to. This runs for
    /// all 9 source chunks on every `column()` call (this is the "9×
    /// String-map re-materialisation" issue #106 named — the per-source
    /// *pipeline* recompute this loop's caller avoids is a separate,
    /// already-fixed cost; see [`OverworldGenerator::store`]'s doc
    /// comment), so cutting its allocation count matters regardless of
    /// whether the pipeline itself was a cache hit. `ocean_floor_wg` stays
    /// a small `HashMap<(i32,i32), i32>` — at most 48×48 = 2304 entries
    /// total across the whole region, an order of magnitude below the
    /// `48×384×48` block field, so it was never the cost this pass targets.
    fn stitch_region(
        region: &mut crate::feature::RegionGrid,
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
        // Diagnostic D2, and U7's acceptance criterion (zero). Counted in bulk
        // from the loop bounds rather than once per cell: an atomic increment
        // 98,304 times per stitch would dominate the very cost being measured,
        // and the product is exact — the loop below has no `continue`.
        crate::counters::bump_stitch_cells(256 * world.bounds().4.max(0) as u64);
        for ly in 0..world.bounds().4 {
            let y = world.bounds().1 + ly;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let state = world.get(base_x + lx, y, base_z + lz);
                    let rx = base_x + lx - center_cx * 16;
                    let rz = base_z + lz - center_cz * 16;
                    region.set(rx, y, rz, state);
                }
            }
        }
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                ocean_floor_wg.insert((dx + lx, dz + lz), heights[(lz * 16 + lx) as usize]);
            }
        }
    }

    /// Stage 6 (issue #406, cross-chunk spill closed by issue #427):
    /// `VEGETAL_DECORATION`, over the real 3×3 `center ± 1` neighbourhood —
    /// [`crate::feature::vegetation::apply_vegetal_decoration_step_3x3_per_source`],
    /// the same [`Self::ore_stage`] shape applied to vegetal decoration
    /// instead of ores (see that module's own doc "Scope" section for why
    /// this is the same mechanism, not a second one).
    ///
    /// Builds one shared [`crate::feature::vegetation::VegGrid`] spanning
    /// [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`],
    /// stitched from all 9 chunks' own **post-ore** terrain (the centre's
    /// via the already-computed `world` parameter; each of the 8 neighbours'
    /// via [`Self::post_ore_world`], which recurses into that neighbour's
    /// *own* 3×3 ore composition — real vanilla parity, not an
    /// approximation, at the cost this module's own doc "Performance"
    /// section already names for `ore_stage` itself: no cache exists across
    /// this recursion, so a full sweep pays it 9× again on top of ore's own
    /// 9×). Biome (and therefore feature list) is resolved per-source from
    /// that source's own **surface-height** biome — [`Self::biome_stage`]'s
    /// per-quart map, quart 0 (the source's min-block corner) — **not**
    /// [`Self::biome_for_carver_source`]'s y=0 answer: issue #480, the
    /// `crate::biome` module doc's "y = 0 trap" (at y=0 the `depth` gradient
    /// is already ≈ +1.0, so surface dark_forest chunks resolved as lush_caves
    /// and decorated with that biome's all-silent feature list). Vegetation
    /// selects its list at the surface the player sees; carver *selection*
    /// and ore placement stay on the y=0 [`Self::biome_for_carver_source`]
    /// convention (their own deliberate, different question).
    ///
    /// No-op (returns `world` unchanged) when the resolver supplied no biome
    /// with a vegetation step, matching every other #295/#406/#427 resolver
    /// "no data supplied" convention.
    pub(super) fn vegetation_stage(
        &self,
        cx: i32,
        cz: i32,
        world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        if self.vegetation_by_biome.values().all(Vec::is_empty) {
            return world;
        }
        // After the early return — see `ore_stage`'s note on why.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Vegetation);

        let base_x = cx * 16;
        let base_z = cz * 16;
        // `VegGrid` takes the CENTRE chunk's own absolute block origin so
        // every one of the 9 sources' absolute-coordinate writes translates
        // correctly relative to it — see `VegGrid`'s own doc comment (the
        // island a chunk-local-only grid used to cause) and
        // `crate::feature::vegetation`'s module doc "Scope" section for why
        // the footprint is widened beyond `REGION_MIN..REGION_MAX` here: trees
        // (especially 2×2 trunks and their canopies) can spill past the tight
        // 48-block 3×3 neighbourhood, and `VegGrid::set_if_in_bounds` drops any
        // cell outside it — the chunk-border cut-off trees the player sees.
        let mut grid = crate::feature::vegetation::VegGrid::with_footprint_interned(
            Arc::clone(&self.interner),
            self.min_y,
            self.height,
            base_x,
            base_z,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
        );

        Self::stitch_veg_region(&mut grid, cx, cz, &world, self.min_y, self.height);
        // Debug-only escape hatch (LODESTONE_VEG_SINGLE_SOURCE_DEBUG),
        // mirroring `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` above: skip
        // stitching/decorating the 8 neighbours, matching
        // `VegetationOracle.java`'s SINGLE mode's own narrower scope for
        // direct comparison. Not used by `column()`'s normal path.
        let single_source_debug = std::env::var("LODESTONE_VEG_SINGLE_SOURCE_DEBUG").is_ok();
        if !single_source_debug {
            for dx in -1..=1i32 {
                for dz in -1..=1i32 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let neighbour_world = self.post_ore_world(cx + dx, cz + dz);
                    Self::stitch_veg_region(&mut grid, cx + dx, cz + dz, &neighbour_world, self.min_y, self.height);
                }
            }
        }

        let features_for_source = |source_x: i32, source_z: i32| -> &[(usize, crate::feature::vegetation::PlacedRef)] {
            // Issue #480: resolve the per-source feature list from the source
            // chunk's own SURFACE-HEIGHT biome — [`Self::biome_stage`]'s
            // per-quart map, quart 0 = the source's min-block corner sampled
            // at its own generated surface height — **not** the y=0
            // [`Self::biome_for_carver_source`] answer. At y=0 the `depth`
            // gradient is already ≈ +1.0 (`crate::biome`'s "y = 0 trap"), so
            // a surface dark_forest chunk resolved as lush_caves and decorated
            // with lush_caves' feature list (vines/vegetation_patch/
            // root_system — all silent no-ops), meaning dark_forest's own step
            // (including the 66.7%-weight dark oak branch, issue #428) never
            // ran. The source's `PreOreResult` is already in
            // [`Self::pre_ore_cache`] from the stitching loop above
            // (each neighbour's `post_ore_world` ran its own `pre_ore_stage`),
            // so this lookup is a cache hit, not a new pipeline pass.
            let pre = self.pre_ore_stage(source_x, source_z);
            let biome = &pre.2[0].0;
            self.vegetation_by_biome.get(biome).map(Vec::as_slice).unwrap_or(&[])
        };

        // Vegetal decoration draws from the SAME per-chunk `WorldgenRandom`
        // shape every #295/#427 decoration stage uses (`set_decoration_seed`
        // then per-feature `set_feature_seed`) — the fresh `XoroshiroRandomSource::new(0)`
        // seed here is a throwaway carrier state; only `set_decoration_seed`'s
        // own derivation (which mixes in `self.seed` and each source's own
        // origin) determines the actual RNG stream, matching `Self::ore_stage`'s
        // identical pattern.
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        if single_source_debug {
            let features = features_for_source(cx, cz);
            crate::feature::vegetation::apply_vegetal_decoration_step(
                &mut random,
                self.seed,
                cx,
                cz,
                &mut grid,
                &self.veg_tags,
                features,
            );
        } else {
            crate::feature::vegetation::apply_vegetal_decoration_step_3x3_per_source(
                &mut random,
                self.seed,
                cx,
                cz,
                &mut grid,
                &self.veg_tags,
                &features_for_source,
            );
        }

        let mut world = world;
        // `grid.dirty_cells()` yields absolute coordinates over the whole
        // driven `REGION_MIN..REGION_MAX` footprint (any of the 9 sources
        // may have written there), but `world` is sized to exactly the
        // centre chunk's own 16×16 box — `DenseBlockGrid::set` is a no-op
        // outside its own box, so this loop naturally keeps only the writes
        // that land in the chunk actually being served, discarding spill
        // into a neighbour with no extra filtering needed.
        // Ids, not strings: `dirty_cells` would resolve each write through the
        // interner (a read guard per cell) only for `world.set` to hash the
        // string and look the same id back up. Both grids share this
        // generator's interner, so the id moves straight across. Allocation-free
        // either way — this is lock and hash traffic, not heap traffic.
        debug_assert_eq!(
            grid.interner().instance_id(),
            world.interner().instance_id(),
            "folding vegetation writes back requires both grids on one interner",
        );
        for (x, y, z, state) in grid.dirty_cell_ids() {
            world.set_id(x, y, z, state);
        }
        world
    }

    /// Stage 7 (issue #404's U2): the `TOP_LAYER_MODIFICATION` step —
    /// `freeze_top_layer`'s snow layers and surface ice, over the finished
    /// post-vegetation world.
    ///
    /// **No 3×3 driver, and that is vanilla's own behaviour, not a narrowing.**
    /// `SnowAndFreezeFeature.place` loops `dx`/`dz` over `0..16` from the chunk
    /// origin and writes only at `(x, y, z)` / `(x, y - 1, z)` of that same
    /// column (`SnowAndFreezeFeature.java:26-45`), so it has no
    /// `blockStateWriteRadius(1)` spill for [`Self::ore_stage`]'s and
    /// [`Self::vegetation_stage`]'s neighbour drivers to model. A neighbour's own
    /// freeze pass cannot reach into this chunk, and this one cannot reach out.
    /// That also means this stage costs no neighbour recomputation at all — it is
    /// one 256-column scan over a grid that is already in hand.
    ///
    /// Returns the world plus the pass's [`FreezeCounts`](crate::feature::top_layer::FreezeCounts),
    /// which [`Self::column`] discards and gates use to assert a count without
    /// rescanning a chunk.
    ///
    /// No-op when the resolver supplied no `block_freeze_facts`, no biome with a
    /// `freeze_top_layer` entry, or no biome climates — the same "no data
    /// supplied" convention as every other stage, and the reason every existing
    /// fixture resolver in this crate still generates a snow-free world.
    pub(super) fn top_layer_stage(
        &self,
        cx: i32,
        cz: i32,
        world: crate::dense_grid::DenseBlockGrid,
        biome_quarts: &[(String, bool); 16],
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        crate::feature::top_layer::FreezeCounts,
    ) {
        let mut world = world;
        if self.snow_support.is_empty()
            || self.freeze_biomes.is_empty()
            || self.biome_climates.is_empty()
        {
            return (world, crate::feature::top_layer::FreezeCounts::default());
        }
        // After the early return — see `ore_stage`'s note on why. This is the
        // stage the fixture tree cannot exercise at all (no `block_freeze_facts`
        // document), so `stage_entered[top_layer] == 0` is precisely how a bench
        // discovers it is running against the fixture tree rather than the
        // embedded server data.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::TopLayer);
        // Debug-only escape hatch, mirroring `LODESTONE_ORE_SINGLE_SOURCE_DEBUG`
        // and `LODESTONE_VEG_SINGLE_SOURCE_DEBUG` above: skip the step entirely
        // so the A arm of a timing comparison can be measured in the same
        // process as the B arm. Never used by `column()`'s normal path. Note a
        // timing comparison must still build a FRESH generator per arm —
        // `pre_ore_cache`/`post_ore_cache` are per-generator and would otherwise
        // make the second arm measure nothing (the trap `049c603` already had to
        // fix in two determinism gates).
        if std::env::var("LODESTONE_FREEZE_DISABLE_DEBUG").is_ok() {
            return (world, crate::feature::top_layer::FreezeCounts::default());
        }
        // `level.getBiome(topPos)` resolves through the quart grid. `biome_stage`
        // samples each quart at its own corner, so a column's quart index is
        // `(lz >> 2) * 4 + (lx >> 2)` — the same rounding `Self::surface_stage`'s
        // own `biome_at` uses. See `crate::feature::top_layer`'s
        // "Approximations, named" for the 2-D-biome caveat this inherits.
        let biome_at = |lx: i32, lz: i32| -> &str {
            let quart = ((lz >> 2) * 4 + (lx >> 2)) as usize;
            let name = biome_quarts[quart].0.as_str();
            // A biome that does not list the step contributes no snow. Handing
            // back a name absent from `biome_climates` is how that is expressed,
            // since `apply_freeze_top_layer` skips an unknown biome.
            if self.freeze_biomes.contains(name) {
                name
            } else {
                ""
            }
        };
        let counts = crate::feature::top_layer::apply_freeze_top_layer(
            &mut world,
            cx,
            cz,
            self.min_y,
            self.height,
            self.sea_level,
            &biome_at,
            &self.biome_climates,
            &self.snow_support,
            &self.climate_noise,
        );
        (world, counts)
    }

    /// Copies one source chunk's own post-ore terrain into the shared
    /// [`crate::feature::vegetation::VegGrid`] `grid` — the vegetation
    /// analogue of [`Self::stitch_region`], but via [`VegGrid::seed`]
    /// (absolute-coordinate, per-cell) rather than a second dense-grid
    /// region, since [`VegGrid`] is this module's own established seam for
    /// vegetal decoration's read/write surface (see that type's doc
    /// comment) and [`Self::vegetation_stage`] already owns exactly one
    /// such grid per `column()` call.
    fn stitch_veg_region(
        grid: &mut crate::feature::vegetation::VegGrid,
        source_cx: i32,
        source_cz: i32,
        world: &crate::dense_grid::DenseBlockGrid,
        min_y: i32,
        height: i32,
    ) {
        let base_x = source_cx * 16;
        let base_z = source_cz * 16;
        // This loop *was* the single most damning number in the rewrite
        // diagnosis: one `String` allocation per cell, `256 * height` cells per
        // source chunk, nine sources per column — 884,736 of the 905,459
        // allocations a warm column performed, 97.7% of the serve path's entire
        // heap traffic from one `to_string()`.
        //
        // Unit 3 deleted it. Both grids now carry `crate::interner::StateId`
        // against the same interner, so a cell copy is a `u16` move and the
        // `bump_string_allocs` that used to accompany `bump_stitch_cells` is
        // gone with it. `stitch_cells` itself stays — the *copy* is still real,
        // and deleting it (not just its allocations) is Unit 7's job, measured
        // by this same counter.
        debug_assert_eq!(
            world.interner().instance_id(),
            grid.interner().instance_id(),
            "stitching between grids with different interners would copy meaningless ids",
        );
        let cells = 256 * height.max(0) as u64;
        crate::counters::bump_stitch_cells(cells);
        for ly in 0..height {
            let y = min_y + ly;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let state = world.get_id(base_x + lx, y, base_z + lz);
                    grid.seed_id(base_x + lx, y, base_z + lz, state);
                }
            }
        }
    }
}
