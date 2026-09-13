//! Stages 5-7 of [`OverworldGenerator::column`]: the unified FEATURES
//! neighbourhood dispatcher and `TOP_LAYER_MODIFICATION`.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the parity history of the 3×3 drivers.
//!
//! # Unit 7: the two region stitches that used to feed these drivers are gone
//!
//! The FEATURES dispatcher needs to read *and write* across a 3×3 chunk neighbourhood, and
//! until Unit 7 of `docs/plans/worldgen-rewrite.md` the way that neighbourhood was
//! made addressable was to copy it: `stitch_region` materialised a
//! `48 × height × 48` `DenseBlockGrid` from the nine sources (884,736 cells),
//! `apply_ore_step_3x3_per_source` cloned it (884,736 more),
//! `stitch_veg_region` copied the nine terrain fields into a `VegGrid`'s
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
//! centre's own terrain grid (the store's copy is shared and must not be mutated)
//! and sparse transfers of what each decoration adapter actually wrote.
//!
//! **The trap, if you edit this file:** the fold-back order decides the served
//! palette, because a `DenseBlockGrid` appends to its local palette in first-write
//! order. The unified dispatcher preserves each adapter's established ordering
//! while transferring writes between them, and the byte-identity controls are
//! what notice any drift.

use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, sync::Arc};

use std::cell::RefCell;

use crate::feature::{
    PlacedOre, apply_ore_step_3x3_per_source_overworld,
};
use crate::rng::{WorldgenRandom, XoroshiroRandomSource};
use crate::stage_schedule::{
    SourceCompletion, OVERWORLD_FEATURES, OVERWORLD_SOURCES,
};

use super::OverworldGenerator;

/// Final source-tagged ore transition used only by the bounded parity
/// materializer. State text prevents generator-local interner ids escaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityOreSpill {
    pub source: (i32, i32),
    pub position: (i32, i32, i32),
    pub state: String,
}

/// Final source-tagged FEATURES transition used by the lifecycle parity
/// materializer.  Unlike [`ParityOreSpill`], this includes every driven
/// decoration step, including lakes, structures, springs, disks and vegetal
/// features.  State text is intentional at this public boundary: interner ids
/// belong to one generator and cannot safely cross into a resident column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityDecorationSpill {
    /// Chunk whose raw FEATURES entries produced this write.
    pub source: (i32, i32),
    /// Absolute block coordinate and final state after this source completed.
    pub position: (i32, i32, i32),
    pub state: String,
}

/// Complete result of one source's FEATURES pass.  The spill list is the
/// lifecycle materializer's block transition stream; block entities are kept
/// alongside it so a caller that persists entities does not need to replay the
/// feature body a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityDecorationResult {
    pub spills: Vec<ParityDecorationSpill>,
    pub block_entities: Vec<super::block_entities::GeneratedBlockEntity>,
}

/// Final source-local `TOP_LAYER_MODIFICATION` transition used by the
/// lifecycle parity materializer.  State text is intentional at this public
/// boundary: interner ids belong to one generator and cannot safely cross into
/// a resident column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityTopLayerSpill {
    /// Chunk whose source-local top-layer pass produced this write.
    pub source: (i32, i32),
    /// Absolute block coordinate and final state after the pass completed.
    pub position: (i32, i32, i32),
    pub state: String,
}

#[derive(Clone, Copy)]
enum MixedEntryWriter {
    Decoration,
    Ore,
}

#[derive(Default)]
struct MixedSync {
    projected: usize,
    retained_outside: usize,
}

/// Temporary state for one unified FEATURES dispatch.
///
/// These containers are private to one dispatch and never escape it. Keeping
/// them in a thread-local slot means the next chunk on the same generation
/// worker can reuse the already-sized hash tables and write logs without
/// sharing a lock or making the output depend on allocation order. The slot is
/// taken, rather than borrowed, so an unusual nested generation call gets a
/// fresh scratch value and remains correct.
#[derive(Default)]
struct MixedDispatchScratch {
    // This table is only probed by exact key; it is never iterated. A hash
    // table therefore both preserves the observable behavior and retains its
    // bucket allocation after `clear`, unlike a tree's per-entry nodes.
    seeded: HashMap<(i32, i32, i32), crate::interner::StateId>,
    ore_transferred: HashMap<(i32, i32, i32), crate::interner::StateId>,
    changed: Vec<(i32, i32, i32, crate::interner::StateId)>,
    final_cells: Vec<(i32, i32, i32, crate::interner::StateId)>,
}

thread_local! {
    static MIXED_DISPATCH_SCRATCH: RefCell<Vec<MixedDispatchScratch>> =
        const { RefCell::new(Vec::new()) };
}

fn take_mixed_dispatch_scratch() -> MixedDispatchScratch {
    MIXED_DISPATCH_SCRATCH.with(|slot| slot.borrow_mut().pop().unwrap_or_default())
}

fn return_mixed_dispatch_scratch(mut scratch: MixedDispatchScratch) {
    scratch.seeded.clear();
    scratch.ore_transferred.clear();
    scratch.changed.clear();
    scratch.final_cells.clear();
    MIXED_DISPATCH_SCRATCH.with(|slot| {
        // A nested call owns a separate value while this one is live. Keep a
        // small bound so an unusual re-entrant path cannot turn this cache into
        // an unbounded per-thread allocation sink.
        let mut slot = slot.borrow_mut();
        if slot.len() < 2 {
            slot.push(scratch);
        }
    });
}

/// Return the source window in the order declared by the Overworld schedule.
/// Keeping this small adapter in the dispatcher prevents the source window's
/// admission order from being re-spelled as nested coordinate ranges at each
/// preparation and execution seam.
fn overworld_source_offsets() -> &'static [(i32, i32)] {
    match OVERWORLD_SOURCES.completion() {
        SourceCompletion::Fixed(offsets) => offsets,
        SourceCompletion::AdmissionDependent => {
            unreachable!("Overworld feature sources require a fixed schedule")
        }
    }
}

/// Immutable read context for one centre chunk's unified FEATURES dispatch.
///
/// The terrain prefixes are already memoised by [`super::OverworldGenerator`]'s
/// staged store. This second layer keeps the derived 5×5 handles, stitched ore
/// height table, and source-local feature selections alive across the source
/// completions that share one centre. It contains no mutable world state and no
/// placement result: resident overrides and every source's RNG stream remain
/// owned by the caller and are evaluated in authenticated order.
#[derive(Debug)]
pub(super) struct MixedReplayContext {
    wide_pre: [Option<Arc<super::PreOreResult>>; crate::feature::region_view::WIDE_SLOTS],
    centre_biomes: Arc<super::biome_cells::BiomeCells>,
    ocean_floor_wg: crate::feature::RegionHeights,
    feature_biomes: Arc<HashMap<String, HashSet<String>>>,
    source_ores: BTreeMap<(i32, i32), Vec<PlacedOre>>,
    source_features:
        BTreeMap<(i32, i32), Vec<(i32, usize, crate::feature::vegetation::PlacedRef)>>,
}

/// Makes one completed entry visible through both FEATURES adapters.  Ore
/// placement writes into a centre-relative `RegionView`, while all other
/// configured feature bodies write through the absolute-coordinate `VegGrid`.
/// Keeping this boundary explicit is what lets the next raw entry observe the
/// resident state regardless of which adapter placed the previous entry.
#[cfg(test)]
fn synchronize_mixed_entry(
    writer: MixedEntryWriter,
    grid: &mut crate::feature::vegetation::VegGrid,
    ore_view: &mut crate::feature::region_view::RegionView<'_>,
    centre_x: i32,
    centre_z: i32,
    grid_cursor: &mut usize,
    ore_cursor: &mut usize,
    ore_transferred: &mut HashMap<(i32, i32, i32), crate::interner::StateId>,
) -> MixedSync {
    let mut final_cells = Vec::new();
    let mut changed = Vec::new();
    synchronize_mixed_entry_reusing(
        writer,
        grid,
        ore_view,
        centre_x,
        centre_z,
        grid_cursor,
        ore_cursor,
        ore_transferred,
        &mut final_cells,
        &mut changed,
    )
}

fn synchronize_mixed_entry_reusing(
    writer: MixedEntryWriter,
    grid: &mut crate::feature::vegetation::VegGrid,
    ore_view: &mut crate::feature::region_view::RegionView<'_>,
    centre_x: i32,
    centre_z: i32,
    grid_cursor: &mut usize,
    ore_cursor: &mut usize,
    ore_transferred: &mut HashMap<(i32, i32, i32), crate::interner::StateId>,
    final_cells: &mut Vec<(i32, i32, i32, crate::interner::StateId)>,
    changed: &mut Vec<(i32, i32, i32, crate::interner::StateId)>,
) -> MixedSync {
    match writer {
        MixedEntryWriter::Decoration => {
            let end = grid.dirty_len();
            final_cells.clear();
            final_cells.extend(grid.dirty_cell_ids().skip(*grid_cursor));
            // Keep the BTreeMap's observable ordering and last-write-wins
            // semantics without allocating one tree node per dirty cell on
            // every decoration entry. `sort_by_key` is stable, so replacing
            // an equal-coordinate entry while compacting retains the last
            // write just as `BTreeMap::insert` did.
            final_cells.sort_by_key(|&(x, y, z, _)| (x, y, z));
            let mut unique = 0usize;
            for read in 0..final_cells.len() {
                let cell = final_cells[read];
                if unique > 0 {
                    let previous = final_cells[unique - 1];
                    if previous.0 == cell.0 && previous.1 == cell.1 && previous.2 == cell.2 {
                        final_cells[unique - 1] = cell;
                        continue;
                    }
                }
                final_cells[unique] = cell;
                unique += 1;
            }
            final_cells.truncate(unique);
            *grid_cursor = end;
            let mut sync = MixedSync::default();
            for &(x, y, z, state) in final_cells.iter() {
                let lx = x - centre_x * 16;
                let lz = z - centre_z * 16;
                if !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lx)
                    || !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lz)
                {
                    sync.retained_outside += 1;
                    continue;
                }
                assert!(
                    ore_view.set_id(lx, y, lz, state),
                    "mixed decoration entry dropped an in-region write at ({x},{y},{z})",
                );
                ore_transferred.insert((x, y, z), state);
                sync.projected += 1;
            }
            sync
        }
        MixedEntryWriter::Ore => {
            changed.clear();
            let end = ore_view.write_log_len();
            ore_view.with_write_log_since_scan_order(*ore_cursor, |writes| {
                for &(lx, y, lz, state) in writes {
                    let x = centre_x * 16 + lx;
                    let z = centre_z * 16 + lz;
                    if ore_transferred.insert((x, y, z), state) != Some(state) {
                            changed.push((x, y, z, state));
                    }
                }
            });
            *ore_cursor = end;
            for (x, y, z, state) in changed.iter() {
                assert!(
                    grid.set_id_if_in_bounds(*x, *y, *z, *state),
                    "mixed ore entry wrote outside the decoration footprint at ({x},{y},{z})",
                );
            }
            *grid_cursor = grid.dirty_len();
            MixedSync {
                projected: changed.len(),
                retained_outside: 0,
            }
        }
    }
}

impl OverworldGenerator {
    /// Stage 5: the real `UNDERGROUND_ORES` 3×3 neighbourhood
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
    /// ore-capable feature exists in the global catalog — the same
    /// "no data supplied" convention every other resolver method
    /// follows, and the one every existing `Resolver` that predates this
    /// increment (most of this crate's own test fixtures) still gets.
    pub(super) fn ore_stage(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
    ) -> crate::dense_grid::DenseBlockGrid {
        self.ore_stage_with_source(cx, cz, center_world, center_heights, None, &[]).0
    }

    /// Runs one source chunk's complete FEATURES-stage dispatcher against the
    /// target's shaped prefix with the final states already applied by
    /// earlier source completions.  The ore and decoration adapters are both
    /// seeded from that resident view before any feature body runs, so
    /// replacement, height and neighbour probes observe the live field rather
    /// than a fresh shaped snapshot.
    #[must_use]
    pub fn parity_source_spills_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> Vec<ParityDecorationSpill> {
        self.parity_source_decoration_with_overrides(
            target_x,
            target_z,
            source_x,
            source_z,
            overrides,
        )
        .spills
    }

    /// Complete source-filtered FEATURES result, including generated block
    /// entities. Callers that persist generated entities should use this result
    /// directly so they do not run the same source body twice.
    #[must_use]
    pub fn parity_source_decoration_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> ParityDecorationResult {
        assert!(
            (target_x - source_x).abs() <= 1 && (target_z - source_z).abs() <= 1,
            "a decoration source must be inside the target's 3x3 dispatch window",
        );
        let pre = self.pre_ore_stage(target_x, target_z);
        if self.decoration_catalog.is_empty() {
            return ParityDecorationResult {
                spills: Vec::new(),
                block_entities: Vec::new(),
            };
        }
        let context = self.replay_context_for(target_x, target_z);
        self.mixed_features_stage_selected(
            target_x,
            target_z,
            (*pre.0).clone(),
            &context,
            Some((source_x, source_z)),
            overrides,
            true,
        )
        .1
    }

    /// Runs the one target FEATURES completion against its radius-one
    /// CARVERS region and returns the complete transition stream. The
    /// dispatcher may use several internal source views to route boundary
    /// reads and writes, but they are one lifecycle completion owned by the
    /// target chunk.
    #[must_use]
    pub fn parity_features_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> ParityDecorationResult {
        let pre = self.pre_ore_stage(target_x, target_z);
        if self.decoration_catalog.is_empty() {
            return ParityDecorationResult {
                spills: Vec::new(),
                block_entities: Vec::new(),
            };
        }
        let context = self.replay_context_for(target_x, target_z);
        self.mixed_features_stage_selected(
            target_x,
            target_z,
            (*pre.0).clone(),
            &context,
            None,
            overrides,
            true,
        )
        .1
    }

    /// Runs the source-local `TOP_LAYER_MODIFICATION` body over the source's
    /// shaped column and returns the exact net block writes.  `overrides` is
    /// the resident state already produced by earlier FEATURES/top-layer
    /// completions; those states are seeded before the shared production stage
    /// runs, so its height, replacement and read-after-write decisions use the
    /// live column rather than a fresh shaped snapshot.
    ///
    /// This stage has no neighbourhood spill: every returned position belongs
    /// to `(source_x, source_z)`.  Writes that leave a seeded state unchanged
    /// are omitted, matching the other replay seams' net-transition contract.
    #[must_use]
    pub fn parity_source_top_layer_spills_with_overrides(
        &self,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, String)],
    ) -> Vec<ParityTopLayerSpill> {
        let pre = self.pre_ore_stage(source_x, source_z);
        let mut world = (*pre.0).clone();
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        for &(x, y, z, ref state) in overrides {
            if !(min_x..min_x + size_x).contains(&x)
                || !(min_y..min_y + size_y).contains(&y)
                || !(min_z..min_z + size_z).contains(&z)
            {
                continue;
            }
            world.set(x, y, z, state);
        }
        let seeded = world.clone();
        let (world, _) = self.top_layer_stage(source_x, source_z, world, &pre.2);

        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        let mut changed = BTreeMap::new();
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    let state = world.get_id(x, y, z);
                    if state != seeded.get_id(x, y, z) {
                        changed.insert((x, y, z), state);
                    }
                }
            }
        }
        changed
            .into_iter()
            .map(|(position, state)| ParityTopLayerSpill {
                source: (source_x, source_z),
                position,
                state: self.interner.name_of(state).to_owned(),
            })
            .collect()
    }

    fn ore_stage_with_source(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, String)],
    ) -> (crate::dense_grid::DenseBlockGrid, Vec<ParityOreSpill>) {
        if self.ore_definitions.is_empty() {
            return (center_world, Vec::new());
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
        // The 24 read-context neighbours' pre-ore products are pulled out of the
        // staged store first and held for the whole lifetime of the view below,
        // because the view borrows into them. `Arc`s, so nothing is copied and
        // nothing is written — a neighbour's product is shared read-only with
        // every other in-flight column that has the same neighbour, which is what
        // keeps "one writer per chunk grid" true.
        let mut wide_pre: [Option<Arc<super::PreOreResult>>; crate::feature::region_view::WIDE_SLOTS] =
            std::array::from_fn(|_| None);
        for dx in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
            for dz in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
                if dx == 0 && dz == 0 {
                    continue;
                }
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)] =
                    Some(self.pre_ore_stage(cx + dx, cz + dz));
            }
        }
        let centre_biomes = Arc::clone(&self.pre_ore_stage(cx, cz).3);

        // The `OCEAN_FLOOR_WG` heightmap is still gathered into a small map,
        // deliberately: at most 80 × 80 = 6,400 entries across the whole read context,
        // three orders of magnitude below the block field this pass stopped
        // copying, and `OreInput` reads it by clamped region-local key rather than
        // by chunk. It was never the cost D2 named.
        let mut ocean_floor_wg = crate::feature::RegionHeights::unset();
        Self::stitch_heights(&mut ocean_floor_wg, 0, 0, center_heights);
        for dx in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
            for dz in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let neighbour = wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .expect("every non-centre offset was filled above");
                Self::stitch_heights(&mut ocean_floor_wg, dx * 16, dz * 16, &neighbour.1);
            }
        }

        // Ores use every section biome in the source's complete 3x3 feature
        // neighbourhood, just like the other decoration entries. The source
        // chunk's own container is only the centre of that neighbourhood; the
        // eight adjacent containers can make an ore entry eligible even when
        // the source surface biome does not list it. The global catalog keeps
        // the feature index stable across the selected biome set, which is the
        // index `set_feature_seed` consumes.
        let mut source_ores = BTreeMap::new();
        for &(dx, dz) in overworld_source_offsets() {
            let source_x = cx + dx;
            let source_z = cz + dz;
            let biomes = Self::source_biomes(
                source_x,
                source_z,
                cx,
                cz,
                &centre_biomes,
                &wide_pre,
            );
            source_ores.insert(
                (source_x, source_z),
                self.decoration_catalog.select_ores(
                    biomes.iter().copied(),
                    &self.ore_definitions,
                ),
            );
        }
        let ores_for_source = |source_x: i32, source_z: i32| -> &[PlacedOre] {
            source_ores
                .get(&(source_x, source_z))
                .map(Vec::as_slice)
                .unwrap_or(&[])
        };
        let in_tag = |block: &str, tag: &str| -> bool {
            self.ore_tag_map
                .get(tag)
                .is_some_and(|members| members.contains(block))
        };
        let feature_biomes = self.decoration_catalog.feature_biomes();
        let biome_zoom_seed = super::biome::biome_zoom_seed(self.seed);
        let biome_sources = |source_x: i32, source_z: i32| {
            let dx = source_x - cx;
            let dz = source_z - cz;
            if dx == 0 && dz == 0 {
                Some(&*centre_biomes)
            } else if (-crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS)
                .contains(&dx)
                && (-crate::feature::region_view::WIDE_RADIUS
                    ..=crate::feature::region_view::WIDE_RADIUS)
                    .contains(&dz)
            {
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .map(|pre| &*pre.3)
            } else {
                None
            }
        };
        let biome_allows = |pos: crate::feature::BlockPos, feature_id: &str| {
            let Some(biome) = super::biome::zoomed_biome(
                biome_zoom_seed,
                pos.x,
                pos.y,
                pos.z,
                biome_sources,
            ) else {
                return false;
            };
            let allowed = feature_biomes
                .get(feature_id)
                .is_some_and(|eligible| eligible.contains(biome));
            if std::env::var_os("LODESTONE_TRACE_DISK_CLAY").is_some()
                && feature_id == "minecraft:disk_clay"
                && pos.x.abs_diff(-98) <= 8
                && pos.z.abs_diff(-116) <= 8
            {
                eprintln!(
                    "RUST_DISK_BIOME pos=({},{},{}) biome={biome} allowed={allowed}",
                    pos.x, pos.y, pos.z
                );
            }
            allowed
        };

        // Borrowed once, outside the closure, so the view's lifetime is plainly
        // tied to two locals rather than to whatever the closure captured.
        let centre_source: &crate::dense_grid::DenseBlockGrid = &center_world;
        let wide_sources = &wide_pre;
        let mut view = crate::feature::region_view::RegionView::over_wide_sources(
            Arc::clone(&self.interner),
            cx,
            cz,
            self.min_y,
            self.height,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(centre_source)
                } else {
                    wide_sources[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        // `&*` because `PreOreResult`'s world is now an `Arc` — see
                        // that alias's own doc. Still a borrow, still no copy.
                        .map(|neighbour| &*neighbour.0)
                }
            },
        );

        // Earlier source completions belong to the same resident world, not
        // to a fresh pre-ore snapshot.  Seed their final states into the
        // dispatcher's own overlay so replacement predicates and blob probes
        // read after those writes.  Keep the seeded ids separately: a cell
        // that remains unchanged is context, not a new spill from this
        // source.
        let mut seeded = BTreeMap::new();
        for &(x, y, z, ref state) in overrides {
            let lx = x - cx * 16;
            let lz = z - cz * 16;
            let id = self.interner.id_of(state);
            if view.seed_read_id(lx, y, lz, id) {
                seeded.insert((lx, y, lz), id);
            }
        }

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        if let Some((source_x, source_z)) = selected_source {
            apply_ore_step_3x3_per_source_overworld(
                &mut random,
                self.seed,
                cx,
                cz,
                self.min_y,
                self.height,
                self.min_y,
                self.height,
                crate::feature::ORE_READ_MIN,
                crate::feature::ORE_READ_MAX,
                &ocean_floor_wg,
                &in_tag,
                Some(&biome_allows),
                crate::feature::STEP_UNDERGROUND_ORES,
                Some((source_x, source_z)),
                &mut view,
                &ores_for_source,
            );
        } else {
            apply_ore_step_3x3_per_source_overworld(
                &mut random,
                self.seed,
                cx,
                cz,
                self.min_y,
                self.height,
                self.min_y,
                self.height,
                crate::feature::ORE_READ_MIN,
                crate::feature::ORE_READ_MAX,
                &ocean_floor_wg,
                &in_tag,
                Some(&biome_allows),
                crate::feature::STEP_UNDERGROUND_ORES,
                None,
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
        let parity_spills = selected_source.map_or_else(Vec::new, |source| {
            let mut spills = Vec::new();
            view.with_writes_in_scan_order(|writes| {
                spills.extend(
                    writes
                        .iter()
                        .copied()
                        .filter(|&(lx, y, lz, state)| {
                            seeded.get(&(lx, y, lz)).copied() != Some(state)
                        })
                        .map(|(lx, y, lz, state)| ParityOreSpill {
                            source,
                            position: (cx * 16 + lx, y, cz * 16 + lz),
                            state: self.interner.name_of(state).to_owned(),
                        }),
                );
            });
            spills
        });
        let writes = view.centre_writes_in_scan_order();
        // Releases the view's borrow of `center_world` and of `wide_pre`.
        drop(view);
        let mut center_world = center_world;
        for (lx, y, lz, state) in writes {
            center_world.set_id(cx * 16 + lx, y, cz * 16 + lz, state);
        }
        (center_world, parity_spills)
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
    /// same thing.
    ///
    /// **U15 took the second half of the advice this doc used to end on** — "if it
    /// ever matters, the win is a dense `[i32; 48 * 48]` array rather than a
    /// `HashMap`, and the clamp has to move with it". It mattered: the driver
    /// probes this map once per cell of a box up to 27 x 27 for every emitted
    /// position of every ore of all nine sources, so the 2,304 entries were being
    /// SipHashed hundreds of thousands of times per column
    /// ([`crate::feature::RegionHeights`] carries the profile). The destination is
    /// now that dense array, and the clamp did move with it — it stays in
    /// [`crate::feature::OreInput::region_local`], and `RegionHeights`'s accessors
    /// document that they assume a pre-clamped key.
    fn stitch_heights(
        ocean_floor_wg: &mut crate::feature::RegionHeights,
        offset_x: i32,
        offset_z: i32,
        heights: &[i32; 256],
    ) {
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                ocean_floor_wg.set(
                    offset_x + lx,
                    offset_z + lz,
                    heights[(lz * 16 + lx) as usize],
                );
            }
        }
    }

    /// Returns the biomes visible to one source's own 3x3 feature neighbourhood
    /// without consulting the staged store.  The returned set borrows the
    /// already-built biome palettes; callers that need several catalog streams
    /// should retain it and pass it to all of them rather than rebuilding it.
    /// The centre is supplied by the caller because `column_timed` computes it
    /// locally; every other member of this neighbourhood is already present in
    /// the 5x5 `wide_pre` read rim.
    fn source_biomes<'a>(
        source_x: i32,
        source_z: i32,
        centre_x: i32,
        centre_z: i32,
        centre_biomes: &'a super::biome_cells::BiomeCells,
        wide_pre: &'a [Option<Arc<super::PreOreResult>>],
    ) -> BTreeSet<&'a str> {
        let mut biomes = BTreeSet::new();
        for dx in -1..=1 {
            for dz in -1..=1 {
                let x = source_x + dx;
                let z = source_z + dz;
                let offset_x = x - centre_x;
                let offset_z = z - centre_z;
                if offset_x == 0 && offset_z == 0 {
                    biomes.extend(centre_biomes.palette().builtin_names());
                } else {
                    let pre = wide_pre[crate::feature::region_view::wide_slot_of_offset(
                        offset_x,
                        offset_z,
                    )]
                    .as_ref()
                    .expect("every source biome lies inside the 5x5 read rim");
                    biomes.extend(pre.3.palette().builtin_names());
                }
            }
        }
        biomes
    }

    /// Prepare bounded slots for an authenticated lifecycle replay. The slots
    /// contain no generated data yet; each source completion fills its own
    /// immutable context lazily, after admissions have populated the staged
    /// pre-ore entries it reads.
    pub fn prepare_lifecycle_replay(&self, admissions: &[(i32, i32)]) {
        let mut slots = HashMap::with_capacity(admissions.len());
        let mut pre_ore = HashMap::with_capacity(admissions.len().saturating_mul(
            crate::feature::region_view::WIDE_SLOTS,
        ));
        for &chunk in admissions {
            assert!(
                slots
                    .insert(chunk, Arc::new(super::store::StageSlot::default()))
                    .is_none(),
                "duplicate lifecycle replay admission for {chunk:?}"
            );
            for dx in -crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS
            {
                for dz in -crate::feature::region_view::WIDE_RADIUS
                    ..=crate::feature::region_view::WIDE_RADIUS
                {
                    pre_ore
                        .entry((chunk.0 + dx, chunk.1 + dz))
                        .or_insert_with(|| Arc::new(std::sync::OnceLock::new()));
                }
            }
        }
        *self
            .replay_context_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(super::ReplayContextCache {
                slots,
                pre_ore: Arc::new(pre_ore),
            });
    }

    /// Number of prepared replay contexts that have been materialized so far.
    /// Diagnostics only; generation never branches on this value. A lifecycle
    /// control can use it to distinguish one lazy context build from a repeated
    /// rebuild without observing or mutating the generated output.
    #[must_use]
    pub fn lifecycle_replay_contexts_ready(&self) -> usize {
        self.replay_context_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|cache| cache.slots.values().filter(|slot| slot.peek().is_some()).count())
            .unwrap_or(0)
    }

    /// Returns the immutable dispatch context for one centre chunk. During a
    /// prepared lifecycle replay the exact admission set supplies a bounded,
    /// once-only slot; ordinary production generation builds the context
    /// directly and does not retain it for every explored chunk. The centre
    /// pre-ore result supplies the key's own heights and biomes; surrounding
    /// prefixes are borrowed as `Arc`s from exact-coordinate entries.
    fn replay_context_for(&self, cx: i32, cz: i32) -> Arc<MixedReplayContext> {
        let centre = self.pre_ore_stage(cx, cz);
        let slot = self
            .replay_context_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|cache| cache.slots.get(&(cx, cz)).cloned());
        match slot {
            Some(slot) => slot.get_or_compute(
                |_| {},
                || self.build_mixed_replay_context(
                    cx,
                    cz,
                    &centre.1,
                    Arc::clone(&centre.3),
                ),
            ),
            None => Arc::new(self.build_mixed_replay_context(
                cx,
                cz,
                &centre.1,
                Arc::clone(&centre.3),
            )),
        }
    }

    /// Builds the immutable portion of one unified FEATURES dispatch.
    ///
    /// `center_heights` and `center_biomes` are supplied explicitly so the
    /// timing path can still measure a locally computed centre prefix without
    /// accidentally replacing it with the staged-store value. The normal
    /// production and lifecycle paths pass the staged centre values and use
    /// [`Self::replay_context_for`] to retain the result.
    fn build_mixed_replay_context(
        &self,
        cx: i32,
        cz: i32,
        center_heights: &[i32; 256],
        center_biomes: Arc<super::biome_cells::BiomeCells>,
    ) -> MixedReplayContext {
        let mut wide_pre:
            [Option<Arc<super::PreOreResult>>; crate::feature::region_view::WIDE_SLOTS] =
            std::array::from_fn(|_| None);
        for dx in -crate::feature::region_view::WIDE_RADIUS
            ..=crate::feature::region_view::WIDE_RADIUS
        {
            for dz in -crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS
            {
                if dx == 0 && dz == 0 {
                    continue;
                }
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)] =
                    Some(self.pre_ore_stage(cx + dx, cz + dz));
            }
        }
        let centre_biomes = center_biomes;

        let mut ocean_floor_wg = crate::feature::RegionHeights::unset();
        Self::stitch_heights(&mut ocean_floor_wg, 0, 0, center_heights);
        for dx in -crate::feature::region_view::WIDE_RADIUS
            ..=crate::feature::region_view::WIDE_RADIUS
        {
            for dz in -crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS
            {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let neighbour = wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .expect("every non-centre wide source was filled above");
                Self::stitch_heights(
                    &mut ocean_floor_wg,
                    dx * 16,
                    dz * 16,
                    &neighbour.1,
                );
            }
        }

        let mut source_ores = BTreeMap::new();
        let mut source_features = BTreeMap::new();
        for &(dx, dz) in overworld_source_offsets() {
            let source_x = cx + dx;
            let source_z = cz + dz;
            // Ore membership follows the same complete 3x3 section-biome
            // union as the other decoration stream. The source's centre
            // container remains part of the union, but is not its boundary.
            let source_biomes = Self::source_biomes(
                source_x,
                source_z,
                cx,
                cz,
                &centre_biomes,
                &wide_pre,
            );
            let selected = self.decoration_catalog.select_all(
                source_biomes.iter().copied(),
                &self.ore_definitions,
            );
            let mut features = selected.features;
            features.extend(selected.step6_disks);
            features.extend(selected.step6_non_ore);
            // The production dispatcher merges all streams by global
            // step/index, so retaining this sort preserves the prior
            // ordering after the catalog scan was unified.
            features.sort_by_key(|(step, index, _)| (*step, *index));
            source_features.insert((source_x, source_z), features);
            source_ores.insert((source_x, source_z), selected.ores);
        }

        MixedReplayContext {
            wide_pre,
            centre_biomes,
            ocean_floor_wg,
            feature_biomes: self.decoration_catalog.feature_biomes(),
            source_ores,
            source_features,
        }
    }

    /// Runs the complete Overworld FEATURES stage for normal generation.  The
    /// parent orchestration module can hand this the shaped prefix directly;
    /// the returned dense grid is the centre result and the entity list is in
    /// feature write order.  Keeping this wrapper beside the source-filtered
    /// seam makes the lifecycle and production paths share one dispatcher.
    pub(super) fn features_stage(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        Vec<super::block_entities::GeneratedBlockEntity>,
    ) {
        if self.decoration_catalog.is_empty() {
            return (center_world, Vec::new());
        }
        let context = self.replay_context_for(cx, cz);
        self.features_stage_with_context(cx, cz, center_world, &context, None, &[], false)
    }

    /// Timing-only twin of [`Self::features_stage`]. It builds the immutable
    /// context from the caller's locally computed centre prefix, preserving
    /// `column_timed`'s cache-cold centre while sharing the exact dispatcher
    /// body and output filtering with production.
    pub(super) fn features_stage_uncached(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        center_heights: &[i32; 256],
        center_biomes: &super::biome_cells::BiomeCells,
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        Vec<super::block_entities::GeneratedBlockEntity>,
    ) {
        if self.decoration_catalog.is_empty() {
            return (center_world, Vec::new());
        }
        let context = self.build_mixed_replay_context(
            cx,
            cz,
            center_heights,
            Arc::new(center_biomes.clone()),
        );
        self.features_stage_with_context(cx, cz, center_world, &context, None, &[], false)
    }

    fn features_stage_with_context(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        context: &MixedReplayContext,
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, String)],
        capture_spills: bool,
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        Vec<super::block_entities::GeneratedBlockEntity>,
    ) {
        let (world, result) = self.mixed_features_stage_selected(
            cx,
            cz,
            center_world,
            context,
            selected_source,
            overrides,
            capture_spills,
        );
        let block_entities = result
            .block_entities
            .into_iter()
            .filter(|be| {
                let (x, _, z) = be.position();
                (x >> 4) == cx && (z >> 4) == cz
            })
            .collect();
        (world, block_entities)
    }

    /// Runs the complete Overworld FEATURES stream over one shared read/write
    /// neighbourhood. `selected_source` is a parity filter for source-local
    /// controls. The adapters are synchronized after every raw entry so later
    /// entries see the current resident state regardless of whether the
    /// previous body used ore or vegetation placement.
    #[allow(clippy::too_many_arguments)]
    fn mixed_features_stage_selected(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        context: &MixedReplayContext,
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, String)],
        capture_spills: bool,
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        ParityDecorationResult,
    ) {
        if self.decoration_catalog.is_empty() {
            return (
                center_world,
                ParityDecorationResult {
                    spills: Vec::new(),
                    block_entities: Vec::new(),
                },
            );
        }
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Vegetation);

        let wide_pre = &context.wide_pre;
        let centre_biomes = &context.centre_biomes;
        let ocean_floor_wg = &context.ocean_floor_wg;
        let source_ores = &context.source_ores;
        let source_features = &context.source_features;

        let in_tag = |block: &str, tag: &str| -> bool {
            self.ore_tag_map
                .get(tag)
                .is_some_and(|members| members.contains(block))
        };
        let feature_biomes = &context.feature_biomes;
        let biome_zoom_seed = super::biome::biome_zoom_seed(self.seed);
        let biome_sources = |source_x: i32, source_z: i32| {
            let dx = source_x - cx;
            let dz = source_z - cz;
            if dx == 0 && dz == 0 {
                Some(centre_biomes.as_ref())
            } else if (-crate::feature::region_view::WIDE_RADIUS
                ..=crate::feature::region_view::WIDE_RADIUS)
                .contains(&dx)
                && (-crate::feature::region_view::WIDE_RADIUS
                    ..=crate::feature::region_view::WIDE_RADIUS)
                    .contains(&dz)
            {
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                    .as_ref()
                    .map(|pre| &*pre.3)
            } else {
                None
            }
        };
        let biome_allows = |pos: crate::feature::BlockPos, feature_id: &str| {
            let Some(biome) = super::biome::zoomed_biome(
                biome_zoom_seed,
                pos.x,
                pos.y,
                pos.z,
                biome_sources,
            ) else {
                return false;
            };
            feature_biomes
                .get(feature_id)
                .is_some_and(|eligible| eligible.contains(biome))
        };

        let centre_source = &center_world;
        let wide_sources = &wide_pre;
        let mut ore_view = crate::feature::region_view::RegionView::over_wide_sources(
            Arc::clone(&self.interner),
            cx,
            cz,
            self.min_y,
            self.height,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(centre_source)
                } else {
                    wide_sources[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(|pre| &*pre.0)
                }
            },
        );

        // The decoration adapter and ore adapter share the same generator
        // interner.  The centre clone is the immutable source snapshot while
        // `center_world` remains the one dense grid this function may return.
        let centre_grid = Arc::new(center_world.clone());
        let grid_sources = &wide_pre;
        let grid_biomes = &wide_pre;
        let mut grid = crate::feature::vegetation::VegGrid::with_sources_and_biomes_shared_zoomed(
            Arc::clone(&self.interner),
            self.min_y,
            self.height,
            cx * 16,
            cz * 16,
            crate::feature::REGION_MIN - crate::feature::vegetation::GEODE_PADDING,
            crate::feature::REGION_MAX + crate::feature::vegetation::GEODE_PADDING,
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(Arc::clone(&centre_grid))
                } else {
                    grid_sources[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(|pre| Arc::clone(&pre.0))
                }
            },
            |dx, dz| {
                if dx == 0 && dz == 0 {
                    Some(Arc::clone(&centre_biomes))
                } else {
                    grid_biomes[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .map(|pre| Arc::clone(&pre.3))
                }
            },
            self.decoration_catalog.feature_biomes(),
            biome_zoom_seed,
        );
        self.veg_tags.bind(grid.interner());

        let mut dispatch_scratch = take_mixed_dispatch_scratch();
        let seeded = &mut dispatch_scratch.seeded;
        let ore_transferred = &mut dispatch_scratch.ore_transferred;
        let local_lo = crate::feature::REGION_MIN - crate::feature::vegetation::GEODE_PADDING;
        let local_hi = crate::feature::REGION_MAX + crate::feature::vegetation::GEODE_PADDING;
        for &(x, y, z, ref state) in overrides {
            let id = self.interner.id_of(state);
            let lx = x - cx * 16;
            let lz = z - cz * 16;
            if ore_view.seed_read_id(lx, y, lz, id) {
                ore_transferred.insert((x, y, z), id);
            }
            if (local_lo..local_hi).contains(&lx)
                && (local_lo..local_hi).contains(&lz)
                && (self.min_y..self.min_y + self.height).contains(&y)
            {
                grid.seed_id(x, y, z, id);
            }
            seeded.insert((x, y, z), id);
        }

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let mut grid_cursor = 0usize;
        let mut ore_cursor = 0usize;
        let changed_scratch = &mut dispatch_scratch.changed;
        let final_cells_scratch = &mut dispatch_scratch.final_cells;
        // A selected source is retained for the parity seam, which can inspect
        // one source in isolation. The lifecycle adapter exposes the complete
        // dispatch as one target-owned FEATURES transition; this loop's
        // internal source views are not lifecycle completion events.
        for &(dx, dz) in overworld_source_offsets() {
            let source_x = cx + dx;
            let source_z = cz + dz;
            if selected_source.is_some_and(|source| source != (source_x, source_z)) {
                continue;
            }
            let features = source_features
                .get(&(source_x, source_z))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let ores = source_ores
                .get(&(source_x, source_z))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let origin = crate::feature::BlockPos {
                x: source_x * 16,
                y: self.min_y,
                z: source_z * 16,
            };
            random.begin_decoration_source();
            let decoration_seed = random.set_decoration_seed(self.seed, origin.x, origin.z);

            for &step_kind in OVERWORLD_FEATURES.steps() {
                let step = step_kind.ordinal();
                let mut decoration_at = 0usize;
                let mut ore_at = 0usize;
                loop {
                        let next_decoration = features
                            .iter()
                            .enumerate()
                            .skip(decoration_at)
                            .find(|(_, (entry_step, _, _))| *entry_step == step);
                        let next_ore = (step == crate::feature::STEP_UNDERGROUND_ORES)
                            .then(|| ores.get(ore_at))
                            .flatten();
                        let next_index = match (next_decoration, next_ore) {
                            (Some((_, (_, index, _))), Some(ore)) => Some((*index).min(ore.index)),
                            (Some((_, (_, index, _))), None) => Some(*index),
                            (None, Some(ore)) => Some(ore.index),
                            (None, None) => None,
                        };
                        let Some(index) = next_index else {
                            break;
                        };
                        if let Some((entry_at, (_, found, placed))) = next_decoration
                            .filter(|(_, (_, found, _))| *found == index)
                        {
                            crate::feature::vegetation::apply_decoration_entry_at_world_seed(
                                &mut random,
                                self.seed,
                                decoration_seed,
                                origin,
                                step,
                                *found,
                                placed,
                                &mut grid,
                                &self.veg_tags,
                            );
                            synchronize_mixed_entry_reusing(
                                MixedEntryWriter::Decoration,
                                &mut grid,
                                &mut ore_view,
                                cx,
                                cz,
                                &mut grid_cursor,
                                &mut ore_cursor,
                                ore_transferred,
                                final_cells_scratch,
                                changed_scratch,
                            );
                            decoration_at = entry_at + 1;
                        } else if let Some(ore) = next_ore.filter(|ore| ore.index == index) {
                            let input = crate::feature::OreInput {
                                chunk_x: source_x,
                                chunk_z: source_z,
                                center_x: cx,
                                center_z: cz,
                                min_y: self.min_y,
                                height: self.height,
                                min_gen_y: self.min_y,
                                gen_depth: self.height,
                                read_min: crate::feature::ORE_READ_MIN,
                                read_max: crate::feature::ORE_READ_MAX,
                                ocean_floor_wg: &ocean_floor_wg,
                                in_tag: &in_tag,
                                biome_allows: Some(&biome_allows),
                            };
                            crate::feature::apply_ore_entry_at_seed(
                                &mut random,
                                decoration_seed,
                                &input,
                                step,
                                ore,
                                &mut ore_view,
                            );
                            synchronize_mixed_entry_reusing(
                                MixedEntryWriter::Ore,
                                &mut grid,
                                &mut ore_view,
                                cx,
                                cz,
                                &mut grid_cursor,
                                &mut ore_cursor,
                                ore_transferred,
                                final_cells_scratch,
                                changed_scratch,
                            );
                            ore_at += 1;
                        }
                }
            }
        }

        let mut spills = Vec::new();
        if capture_spills {
            let source = selected_source.unwrap_or((cx, cz));
            let mut final_cells = BTreeMap::new();
            for (x, y, z, state) in grid.dirty_cell_ids() {
                final_cells.insert((x, y, z), state);
            }
            spills = final_cells
                .into_iter()
                .filter(|(position, state)| seeded.get(position).copied() != Some(*state))
                .map(|(position, state)| ParityDecorationSpill {
                    source,
                    position,
                    state: self.interner.name_of(state).to_owned(),
                })
                .collect();
        }
        let block_entities = grid.take_block_entities();
        drop(ore_view);
        let mut world = center_world;
        for (x, y, z, state) in grid.dirty_cell_ids() {
            world.set_id(x, y, z, state);
        }
        return_mixed_dispatch_scratch(dispatch_scratch);
        (
            world,
            ParityDecorationResult {
                spills,
                block_entities,
            },
        )
    }

    /// Stage 7: the `TOP_LAYER_MODIFICATION` step —
    /// `freeze_top_layer`'s snow layers and surface ice, over the finished
    /// post-vegetation world.
    ///
    /// **No 3×3 driver, and that is vanilla's own behaviour, not a narrowing.**
    /// Vanilla's own snow-and-freeze feature's placement loops `dx`/`dz` over `0..16` from the chunk
    /// origin and writes only at `(x, y, z)` / `(x, y - 1, z)` of that same
    /// column, so it has no
    /// neighbour-write spill for the FEATURES dispatcher to model. A neighbour's own
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
        // Debug-only escape hatch: skip the step entirely so the A arm of a
        // timing comparison can be measured in the same process as the B arm.
        // Never used by `column()`'s normal path. Note a timing comparison must
        // still build a FRESH generator per arm — the staged store is
        // per-generator and would otherwise make the second arm measure
        // nothing (the trap `049c603` already had to fix in two determinism
        // gates).
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
// narrowing it. It copied one source chunk's whole terrain field into the
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{MixedEntryWriter, synchronize_mixed_entry};
    use crate::dense_grid::DenseBlockGrid;
    use crate::feature::OVERWORLD_SOURCE_OFFSETS;
    use crate::feature::region_view::RegionView;
    use crate::feature::vegetation::VegGrid;
    use crate::interner::StateInterner;

    #[test]
    fn overworld_source_offsets_match_external_feature_trace_order() {
        assert_eq!(
            OVERWORLD_SOURCE_OFFSETS,
            &[
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, -1),
                (0, 0),
                (0, 1),
                (1, -1),
                (1, 0),
                (1, 1),
            ],
        );
        assert_ne!(
            OVERWORLD_SOURCE_OFFSETS,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (0, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
        );
    }

    #[test]
    fn mixed_entry_sync_bridges_each_adapter_once() {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let backing = DenseBlockGrid::with_interner(
            Arc::clone(&interner),
            -16,
            0,
            -16,
            48,
            8,
            48,
            air,
        );
        let mut ore_view = RegionView::over_region_grid(&backing, 0, 8);
        let mut grid = VegGrid::with_footprint_interned(
            Arc::clone(&interner),
            0,
            8,
            0,
            0,
            -24,
            40,
        );
        let basalt = interner.id_of("minecraft:basalt");
        let blackstone = interner.id_of("minecraft:blackstone");
        assert!(grid.set_id_if_in_bounds(1, 1, 1, basalt));
        // Two writes in one entry must collapse to one sorted transfer, with
        // the final state matching the old BTreeMap's last-write-wins rule.
        assert!(grid.set_id_if_in_bounds(1, 1, 1, blackstone));

        let mut grid_cursor = 0usize;
        let mut ore_cursor = 0usize;
        let mut transferred = std::collections::HashMap::new();
        let sync = synchronize_mixed_entry(
            MixedEntryWriter::Decoration,
            &mut grid,
            &mut ore_view,
            0,
            0,
            &mut grid_cursor,
            &mut ore_cursor,
            &mut transferred,
        );
        assert_eq!(sync.projected, 1);
        assert_eq!(ore_view.get_id(1, 1, 1), blackstone);
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Decoration,
                &mut grid,
                &mut ore_view,
                0,
                0,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut transferred,
            )
            .projected,
            0,
        );

        assert!(ore_view.set_id(2, 1, 2, blackstone));
        let sync = synchronize_mixed_entry(
            MixedEntryWriter::Ore,
            &mut grid,
            &mut ore_view,
            0,
            0,
            &mut grid_cursor,
            &mut ore_cursor,
            &mut transferred,
        );
        assert_eq!(sync.projected, 1);
        assert_eq!(grid.get_id(2, 1, 2), blackstone);
        assert_eq!(
            synchronize_mixed_entry(
                MixedEntryWriter::Ore,
                &mut grid,
                &mut ore_view,
                0,
                0,
                &mut grid_cursor,
                &mut ore_cursor,
                &mut transferred,
            )
            .projected,
            0,
        );
    }
}
