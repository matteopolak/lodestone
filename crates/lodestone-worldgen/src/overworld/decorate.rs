//! Stages 5-7 of [`OverworldGenerator::column`]: the `UNDERGROUND_ORES` and
//! `VEGETAL_DECORATION` 3×3 neighbourhood drivers and `TOP_LAYER_MODIFICATION`.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the parity history of the 3×3 drivers.
//!
//! # Unit 7: the two region stitches that used to feed these drivers are gone
//!
//! Both drivers need to read *and write* across a 3×3 chunk neighbourhood, and
//! until Unit 7 of `docs/plans/worldgen-rewrite.md` the way that neighbourhood was
//! made addressable was to copy it: `stitch_region` materialised a
//! `48 × height × 48` `DenseBlockGrid` from the nine sources (884,736 cells),
//! `apply_ore_step_3x3_per_source` cloned it (884,736 more),
//! `stitch_veg_region` copied the nine post-ore fields into a `VegGrid`'s
//! `HashMap` (884,736 again), and each driver's output was folded back over the
//! centre's full 98,304 cells. ~2.85M cell copies per served column, **every one of
//! them warm** — the neighbours were already computed and memoised in
//! [`super::store`]; the copies existed only to give them one coordinate space.
//!
//! [`crate::feature::region_view::RegionView`] and
//! [`crate::feature::vegetation::VegGrid::with_sources`] route reads to whichever
//! source chunk owns the column instead, holding writes in a sparse overlay, so
//! `crate::counters::Counters::stitch_cells` reads **zero** for a served column —
//! this unit's acceptance criterion. What is left is one `Vec<u16>` clone of the
//! centre's own post-ore grid (the store's copy is shared and must not be mutated)
//! and two sparse fold-backs of what decoration actually wrote.
//!
//! **The trap, if you edit this file:** the fold-back order decides the served
//! palette, because a `DenseBlockGrid` appends to its local palette in first-write
//! order. `ore_stage` folds in `(y, lz, lx)` scan order because the full-box walk
//! it replaced did; `vegetation_stage` folds in *write* order because the `dirty`
//! `Vec` it replays always did. Neither is interchangeable with the other, and
//! `column_is_byte_identical_across_two_independently_constructed_generators` is
//! what notices.

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

        // Unit 7: **no region grid is materialised.** What used to happen here was
        // a fresh 48 × height × 48 `DenseBlockGrid` plus `stitch_region` copying
        // all nine already-computed source chunks into it — 884,736 cells, every
        // one warm, on every `column()` call — which
        // `apply_ore_step_3x3_per_source` then `clone()`d for another 884,736.
        // Both are gone: the nine grids are *borrowed* and reads are routed to
        // whichever chunk owns the column, with the ore writes held in the view's
        // own sparse overlay. See `crate::feature::region_view`.
        //
        // The eight neighbours' pre-ore products are pulled out of the staged
        // store first and held in `pre` for the whole lifetime of the view below,
        // because the view borrows into them. `Arc`s, so nothing is copied and
        // nothing is written — a neighbour's product is shared read-only with
        // every other in-flight column that has the same neighbour, which is what
        // keeps "one writer per chunk grid" true.
        let mut pre: [Option<Arc<super::PreOreResult>>; 9] = std::array::from_fn(|_| None);
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                pre[Self::region_slot(dx, dz)] = Some(self.pre_ore_stage(cx + dx, cz + dz));
            }
        }

        // The `OCEAN_FLOOR_WG` heightmap is still gathered into a small map,
        // deliberately: at most 48 × 48 = 2,304 entries across the whole region,
        // three orders of magnitude below the block field this pass stopped
        // copying, and `OreInput` reads it by clamped region-local key rather than
        // by chunk. It was never the cost D2 named.
        let mut ocean_floor_wg: HashMap<(i32, i32), i32> = HashMap::new();
        Self::stitch_heights(&mut ocean_floor_wg, 0, 0, center_heights);
        for dx in -1..=1i32 {
            for dz in -1..=1i32 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let neighbour = pre[Self::region_slot(dx, dz)]
                    .as_ref()
                    .expect("every non-centre offset was filled above");
                Self::stitch_heights(&mut ocean_floor_wg, dx * 16, dz * 16, &neighbour.1);
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

        // Borrowed once, outside the closure, so the view's lifetime is plainly
        // tied to two locals rather than to whatever the closure captured.
        let centre_source: &crate::dense_grid::DenseBlockGrid = &center_world;
        let pre_sources = &pre;
        let mut view = crate::feature::region_view::RegionView::over_sources(
            Arc::clone(&self.interner),
            cx,
            cz,
            self.min_y,
            self.height,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(centre_source)
                } else {
                    pre_sources[Self::region_slot(dx, dz)]
                        .as_ref()
                        .map(|neighbour| &neighbour.0)
                }
            },
        );

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        // Debug-only escape hatch (LODESTONE_ORE_SINGLE_SOURCE_DEBUG): run only
        // the centre's own decoration pass (matching `ComposedChunkOracle
        // .java`'s single-source-only `postfeatures` stage) while still exposing
        // the full 3x3 terrain/heightmap through the view, so `get_height`'s
        // probes never panic. Used once to isolate "is the centre pass itself
        // correct" from "does real 3x3 spill widen the gap against a
        // single-source oracle" — see docs/worldgen-parity.md. Not used by
        // `column()`'s normal path.
        if std::env::var("LODESTONE_ORE_SINGLE_SOURCE_DEBUG").is_ok() {
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
            crate::feature::apply_ore_step(&mut random, self.seed, &input, &mut view, ores);
        } else {
            apply_ore_step_3x3_per_source(
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
                &mut view,
                &ores_for_source,
            );
        }

        // Only the cells ore actually wrote, in the `(y, lz, lx)` order the
        // deleted full-box walk visited them in.
        //
        // **That order is the byte-identity argument, not a tidiness one.** The
        // old fold-back called `set_id` on all 98,304 centre cells; a
        // `DenseBlockGrid` appends to its local palette in first-write order, and
        // that palette is what reaches the wire. Skipping the unchanged cells is
        // safe because an unchanged cell's state came out of `center_world` itself
        // and so is already in its palette — therefore every state that is *new*
        // to the palette sits at a written cell, and the new states' first-write
        // sequence is unchanged as long as the written cells are visited in the
        // same order. See `RegionView::centre_writes_in_scan_order`.
        let writes = view.centre_writes_in_scan_order();
        // Releases the view's borrow of `center_world` and of `pre`.
        drop(view);
        let mut center_world = center_world;
        for (lx, y, lz, state) in writes {
            center_world.set_id(cx * 16 + lx, y, cz * 16 + lz, state);
        }
        center_world
    }

    /// The [`crate::feature::region_view`] slot index for chunk offset
    /// `(dx, dz)` ∈ `[-1, 1]²`.
    ///
    /// Derived by asking `source_slot` about that offset's **own origin column**,
    /// so this file and the view agree on the slot convention by construction
    /// rather than by two copies of the same arithmetic staying in step. That is
    /// the class of mistake `region_view`'s module doc records.
    fn region_slot(dx: i32, dz: i32) -> usize {
        crate::feature::region_view::source_slot(dx * 16, dz * 16)
            .expect("a 3x3 chunk offset's own origin column is inside the driven region")
    }

    /// Copies one source chunk's own `OCEAN_FLOOR_WG` heightmap into the shared
    /// region-local map the ore driver probes, at centre-relative offset
    /// `(offset_x, offset_z)` = `(source_cx - center_cx) * 16` (matching
    /// [`crate::feature::OreInput::region_local`]'s key space).
    ///
    /// **This is all that is left of `stitch_region`.** Until Unit 7 this function
    /// also copied the source's entire `16 × height × 16` block field into a
    /// materialised region grid — 98,304 cells per source, nine sources, on every
    /// `column()` call, warm, which is the half of diagnostic D2 that survived
    /// Unit 3's interning and Unit 6's id-keying. `RegionView` routes those reads
    /// to the source grid instead, so the block loop is gone and with it the
    /// `crate::counters::bump_stitch_cells` call that measured it: the counter now
    /// reads **zero** for a served column, which is this unit's acceptance
    /// criterion.
    ///
    /// The heights stayed a copy on purpose. 256 `i32`s per source is 2,304
    /// entries for the whole region against the 884,736-cell field that went away,
    /// and the driver reads them by *clamped* region-local key
    /// ([`crate::feature::OreInput::region_local`]) rather than by chunk, so a view
    /// over nine `[i32; 256]`s would have to reproduce that clamp to answer the
    /// same thing. Not worth the risk for 0.26% of the volume; if it ever matters,
    /// the win is a dense `[i32; 48 * 48]` array rather than a `HashMap`, and the
    /// clamp has to move with it.
    fn stitch_heights(
        ocean_floor_wg: &mut HashMap<(i32, i32), i32>,
        offset_x: i32,
        offset_z: i32,
        heights: &[i32; 256],
    ) {
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                ocean_floor_wg.insert(
                    (offset_x + lx, offset_z + lz),
                    heights[(lz * 16 + lx) as usize],
                );
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
        world: Arc<crate::dense_grid::DenseBlockGrid>,
    ) -> crate::dense_grid::DenseBlockGrid {
        if self.vegetation_by_biome.values().all(Vec::is_empty) {
            return Arc::try_unwrap(world).unwrap_or_else(|shared| (*shared).clone());
        }
        // After the early return — see `ore_stage`'s note on why.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Vegetation);

        let base_x = cx * 16;
        let base_z = cz * 16;
        // Debug-only escape hatch (LODESTONE_VEG_SINGLE_SOURCE_DEBUG),
        // mirroring `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` above: give the grid no
        // neighbour sources and decorate the centre only, matching
        // `VegetationOracle.java`'s SINGLE mode's own narrower scope for
        // direct comparison. Not used by `column()`'s normal path.
        let single_source_debug = std::env::var("LODESTONE_VEG_SINGLE_SOURCE_DEBUG").is_ok();
        // `VegGrid` takes the CENTRE chunk's own absolute block origin so
        // every one of the 9 sources' absolute-coordinate writes translates
        // correctly relative to it — see `VegGrid`'s own doc comment (the
        // island a chunk-local-only grid used to cause) and
        // `crate::feature::vegetation`'s module doc "Scope" section for why
        // the footprint is widened beyond `REGION_MIN..REGION_MAX` here: trees
        // (especially 2×2 trunks and their canopies) can spill past the tight
        // 48-block 3×3 neighbourhood, and `VegGrid::set_if_in_bounds` drops any
        // cell outside it — the chunk-border cut-off trees the player sees.
        //
        // Unit 7: `with_sources`, not `with_footprint_interned` + a seeding loop.
        // `stitch_veg_region` used to copy all nine post-ore chunks into this
        // grid's `HashMap` — 884,736 inserts per column, leaving 884,736 live
        // entries — purely to make the neighbourhood addressable. The grids are
        // now borrowed as read-only `Arc` snapshots straight out of the staged
        // store and the map holds only what decoration writes. Nothing about the
        // placement engine's view of the world changed: a read still answers the
        // source terrain, a write still shadows it for later reads in the same
        // step, and the padding ring still answers air because no source covers
        // it.
        let mut grid = crate::feature::vegetation::VegGrid::with_sources(
            Arc::clone(&self.interner),
            self.min_y,
            self.height,
            base_x,
            base_z,
            crate::feature::REGION_MIN - crate::feature::VEG_PADDING,
            crate::feature::REGION_MAX + crate::feature::VEG_PADDING,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(Arc::clone(&world))
                } else if single_source_debug {
                    None
                } else {
                    // Recurses into that neighbour's own 3×3 ore composition,
                    // memoised in the store — the same call the seeding loop made,
                    // in the same `dx` outer / `dz` inner order, just without the
                    // copy that followed it.
                    Some(self.post_ore_world(cx + dx, cz + dz))
                }
            },
        );

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

        // The one grid this call is allowed to mutate, and the one copy this stage
        // still makes: the store owns the canonical post-ore product and shares it
        // with every other column that has this chunk as a neighbour, so the
        // served chunk has to be a private copy of it. That is one 98,304-cell
        // `Vec<u16>` memcpy — the same clone `column()` used to make before
        // handing the grid in, moved here so the pre-vegetation content can also
        // serve as the view's centre source. It is not a stitch: nothing is
        // re-palettised and no cell is read individually.
        let mut world = (*world).clone();
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
        //
        // Write order, not scan order, and unchanged by Unit 7: `dirty` is a `Vec`
        // in insertion order, so the states vegetation introduces are appended to
        // `world`'s palette in exactly the sequence they were placed, as before.
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

}

// `stitch_veg_region` used to live here, and Unit 7 deleted it rather than
// narrowing it. It copied one source chunk's whole post-ore field into the
// vegetation grid, absolute cell by absolute cell, and it was the single most
// damning number in `docs/plans/worldgen-rewrite.md`'s diagnosis: at one
// `String` allocation per cell it accounted for 884,736 of the 905,459 heap
// allocations a warm column performed — 97.7% of the serve path's entire heap
// traffic from one `to_string()`.
//
// Unit 3 took the allocations (both grids carry `StateId` against one interner,
// so a cell copy became a `u16` move) and Unit 6 took the interner lock traffic,
// but the *copy* survived both, and so did the 884,736-entry `HashMap` it filled.
// `VegGrid::with_sources` deletes it outright: the nine grids are borrowed, and
// `crate::counters::Counters::stitch_cells` — the counter that measured exactly
// this loop and `stitch_region`'s twin — now reads zero for a served column.
