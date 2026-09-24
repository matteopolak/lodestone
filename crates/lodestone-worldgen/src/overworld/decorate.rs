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
//! this unit's acceptance criterion. The returned parity result also reports
//! zero `bridge_sync_events` and `bridge_sync_cells`: no per-entry projection
//! remains. What is left is one `Vec<u16>` clone of the
//! centre's own terrain grid (the store's copy is shared and must not be mutated)
//! and sparse transfers of what each decoration adapter actually wrote.
//!
//! **The trap, if you edit this file:** the fold-back order decides the served
//! palette, because a `DenseBlockGrid` appends to its local palette in first-write
//! order. The unified dispatcher preserves each adapter's established ordering
//! while transferring writes between them, and the byte-identity controls are
//! what notice any drift.

use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, sync::Arc};

use std::cell::{Cell, RefCell};

use lodestone_data::block_states::StateId as CanonicalStateId;
use lodestone_data::biomes::BiomeRef;

use crate::feature::{
    FeatureMembershipId, OreWorldAccess, PlacedOre,
    apply_ore_entry_at_seed_with_membership,
    apply_ore_step_3x3_per_source_overworld_with_membership,
};
use crate::compose::{BiomeMask, FeatureBiomePlan};
use crate::feature::region_view::Overlay;
use crate::rng::{WorldgenRandom, XoroshiroRandomSource};
use crate::stage_schedule::{
    SourceCompletion, OVERWORLD_SOURCES,
};

use super::OverworldGenerator;

/// Final source-tagged ore transition used only by the bounded parity
/// materializer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityOreSpill {
    pub source: (i32, i32),
    pub position: (i32, i32, i32),
    pub state: CanonicalStateId,
}

/// Final source-tagged FEATURES transition used by the lifecycle parity
/// materializer.  Unlike [`ParityOreSpill`], this includes every driven
/// decoration step, including lakes, structures, springs, disks and vegetal
/// features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityDecorationSpill {
    /// Chunk whose raw FEATURES entries produced this write.
    pub source: (i32, i32),
    /// Absolute block coordinate and final state after this source completed.
    pub position: (i32, i32, i32),
    pub state: CanonicalStateId,
}

/// Complete result of one source's FEATURES pass.  The spill list is the
/// lifecycle materializer's block transition stream; block entities are kept
/// alongside it so a caller that persists entities does not need to replay the
/// feature body a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityDecorationResult {
    pub spills: Vec<ParityDecorationSpill>,
    pub block_entities: Vec<super::block_entities::GeneratedBlockEntity>,
    /// Target-local final FEATURES writes retained by direct target output.
    /// This is separate from [`Self::spills`] because the target column is
    /// already complete when the lifecycle materializer adopts it.
    pub local_features: Vec<ParityDecorationSpill>,
    /// Number of per-entry adapter bridge passes. The mixed production path
    /// shares one overlay, so this remains zero and is a regression counter.
    pub bridge_sync_events: u64,
    /// Cells copied by those adapter bridge passes. The mixed production path
    /// shares one overlay, so this remains zero and is a regression counter.
    pub bridge_sync_cells: u64,
}

/// The target-owned production result keeps the finished target column in its
/// dense representation and separates its local read state from writes that
/// leave that column.
#[derive(Debug)]
pub struct DirectDecorationResult {
    pub column: super::GeneratedColumn,
    pub spills: Vec<ParityDecorationSpill>,
    /// Final target-local FEATURES and top-layer writes. The caller seeds these
    /// into the later target read view without replaying them into this
    /// already-finished dense column.
    pub local_features: Vec<ParityDecorationSpill>,
    /// Number of per-entry adapter bridge passes; zero with the shared overlay.
    pub bridge_sync_events: u64,
    /// Cells copied by per-entry adapter bridges; zero with the shared overlay.
    pub bridge_sync_cells: u64,
}

const ORE_BIOME_CACHE_SLOTS: usize = 256;

struct OreBiomeMembershipGrid {
    source_min_x: i32,
    source_min_z: i32,
    source_width: usize,
    source_depth: usize,
    source_indices: Vec<i32>,
    product_min_x: i32,
    product_min_z: i32,
    product_width: usize,
    product_depth: usize,
    product_biomes: Vec<Arc<super::biome_cells::BiomeCells>>,
    min_y: i32,
    height: i32,
    cache_keys: Vec<Vec<Cell<u32>>>,
    cache_corners: Vec<Vec<Cell<u8>>>,
    lattice: RefCell<super::biome::ZoomFiddleLattice>,
    feature_biomes: Arc<FeatureBiomePlan>,
    #[cfg(test)]
    queries: Cell<u64>,
    #[cfg(test)]
    cache_misses: Cell<u64>,
}

impl OreBiomeMembershipGrid {
    fn new(
        sources: &[(i32, i32)],
        product_min_x: i32,
        product_min_z: i32,
        product_width: usize,
        product_depth: usize,
        products: &[Arc<super::PreOreResult>],
        min_y: i32,
        height: i32,
        zoom_seed: i64,
        feature_biomes: Arc<FeatureBiomePlan>,
    ) -> Self {
        let source_min_x = sources.iter().map(|&(x, _)| x).min().expect("ore sources");
        let source_max_x = sources.iter().map(|&(x, _)| x).max().expect("ore sources");
        let source_min_z = sources.iter().map(|&(_, z)| z).min().expect("ore sources");
        let source_max_z = sources.iter().map(|&(_, z)| z).max().expect("ore sources");
        let source_width = usize::try_from(source_max_x - source_min_x + 1)
            .expect("ore source width");
        let source_depth = usize::try_from(source_max_z - source_min_z + 1)
            .expect("ore source depth");
        let mut source_indices = vec![-1_i32; source_width * source_depth];
        for (index, &(x, z)) in sources.iter().enumerate() {
            let local_x = usize::try_from(x - source_min_x).expect("ore source x");
            let local_z = usize::try_from(z - source_min_z).expect("ore source z");
            source_indices[local_z * source_width + local_x] =
                i32::try_from(index).expect("ore source count");
        }
        let product_biomes = products
            .iter()
            .map(|product| Arc::clone(&product.3))
            .collect::<Vec<_>>();
        let lattice = super::biome::ZoomFiddleLattice::for_block_bounds(
            zoom_seed,
            source_min_x * 16,
            source_max_x * 16 + 15,
            min_y,
            min_y + height - 1,
            source_min_z * 16,
            source_max_z * 16 + 15,
        );
        let cache_keys = (0..sources.len())
            .map(|_| {
                (0..ORE_BIOME_CACHE_SLOTS)
                    .map(|_| Cell::new(u32::MAX))
                    .collect::<Vec<_>>()
            })
            .collect();
        let cache_corners = (0..sources.len())
            .map(|_| {
                (0..ORE_BIOME_CACHE_SLOTS)
                    .map(|_| Cell::new(0))
                    .collect::<Vec<_>>()
            })
            .collect();
        Self {
            source_min_x,
            source_min_z,
            source_width,
            source_depth,
            source_indices,
            product_min_x,
            product_min_z,
            product_width,
            product_depth,
            product_biomes,
            min_y,
            height,
            cache_keys,
            cache_corners,
            lattice: RefCell::new(lattice),
            feature_biomes,
            #[cfg(test)]
            queries: Cell::new(0),
            #[cfg(test)]
            cache_misses: Cell::new(0),
        }
    }

    #[inline]
    fn source_index(&self, chunk_x: i32, chunk_z: i32) -> Option<usize> {
        let x = usize::try_from(chunk_x - self.source_min_x).ok()?;
        let z = usize::try_from(chunk_z - self.source_min_z).ok()?;
        if x >= self.source_width || z >= self.source_depth {
            return None;
        }
        let index = self.source_indices[z * self.source_width + x];
        (index >= 0).then_some(index as usize)
    }

    fn biome_at(&self, pos: crate::feature::BlockPos) -> Option<BiomeRef> {
        crate::feature::ore_probe::bump_biome_cache_query(1);
        #[cfg(test)]
        self.queries.set(self.queries.get() + 1);
        let source_index = self.source_index(pos.x.div_euclid(16), pos.z.div_euclid(16));
        let cache_key = source_index.and_then(|source_index| {
            let y = usize::try_from(pos.y - self.min_y).ok()?;
            if y >= usize::try_from(self.height).expect("ore biome height") {
                return None;
            }
            let x = pos.x.rem_euclid(16) as usize;
            let z = pos.z.rem_euclid(16) as usize;
            let key = u32::try_from((y * 16 + z) * 16 + x).expect("ore biome coordinate");
            let mixed = key ^ (key >> 8) ^ (key >> 16);
            let slot = mixed.wrapping_mul(0x9e37_79b9) as usize
                & (ORE_BIOME_CACHE_SLOTS - 1);
            Some((source_index, key, slot))
        });
        if let Some((source_index, key, slot)) = cache_key {
            let corner = if self.cache_keys[source_index][slot].get() == key {
                crate::feature::ore_probe::bump_biome_cache_hit(1);
                self.cache_corners[source_index][slot].get()
            } else {
                crate::feature::ore_probe::bump_biome_cache_miss(1);
                #[cfg(test)]
                self.cache_misses.set(self.cache_misses.get() + 1);
                let corner = self.compute_corner(pos);
                self.cache_keys[source_index][slot].set(key);
                self.cache_corners[source_index][slot].set(corner);
                corner
            };
            return self.biome_from_corner(corner, pos);
        }
        crate::feature::ore_probe::bump_biome_cache_miss(1);
        self.compute_biome(pos)
    }

    fn compute_corner(&self, pos: crate::feature::BlockPos) -> u8 {
        let mut lattice = self.lattice.borrow_mut();
        lattice.selected_corner(pos.x, pos.y, pos.z)
    }

    fn biome_from_corner(&self, corner: u8, pos: crate::feature::BlockPos) -> Option<BiomeRef> {
        let product_biomes = &self.product_biomes;
        let product_min_x = self.product_min_x;
        let product_min_z = self.product_min_z;
        let product_width = self.product_width;
        let product_depth = self.product_depth;
        super::biome::zoomed_biome_ref_from_corner(
            corner,
            pos.x,
            pos.y,
            pos.z,
            |chunk_x, chunk_z| {
                let x = usize::try_from(chunk_x - product_min_x).ok()?;
                let z = usize::try_from(chunk_z - product_min_z).ok()?;
                if x >= product_width || z >= product_depth {
                    return None;
                }
                Some(product_biomes[z * product_width + x].as_ref())
            },
        )
    }

    fn compute_biome(&self, pos: crate::feature::BlockPos) -> Option<BiomeRef> {
        let mut lattice = self.lattice.borrow_mut();
        let product_biomes = &self.product_biomes;
        let product_min_x = self.product_min_x;
        let product_min_z = self.product_min_z;
        let product_width = self.product_width;
        let product_depth = self.product_depth;
        super::biome::zoomed_biome_ref_with_lattice(
            &mut lattice,
            pos.x,
            pos.y,
            pos.z,
            |chunk_x, chunk_z| {
                let x = usize::try_from(chunk_x - product_min_x).ok()?;
                let z = usize::try_from(chunk_z - product_min_z).ok()?;
                if x >= product_width || z >= product_depth {
                    return None;
                }
                Some(product_biomes[z * product_width + x].as_ref())
            },
        )
    }

    #[inline]
    fn allows(&self, pos: crate::feature::BlockPos, membership: FeatureMembershipId) -> bool {
        self.biome_at(pos)
            .is_some_and(|biome| self.feature_biomes.allows(membership, biome))
    }

    #[cfg(test)]
    fn lookup_counts(&self) -> (u64, u64) {
        (self.queries.get(), self.cache_misses.get())
    }
}

/// Sparse target-owned decoration result for a settlement padding writer.
#[derive(Debug)]
pub struct SparseDirectDecorationResult {
    pub spills: Vec<ParityDecorationSpill>,
    pub local_features: Vec<ParityDecorationSpill>,
    pub block_entities: Vec<super::block_entities::GeneratedBlockEntity>,
    /// Number of per-entry adapter bridge passes; zero with the shared overlay.
    pub bridge_sync_events: u64,
    /// Cells copied by per-entry adapter bridges; zero with the shared overlay.
    pub bridge_sync_cells: u64,
}

/// Final source-local `TOP_LAYER_MODIFICATION` transition used by the
/// lifecycle parity materializer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityTopLayerSpill {
    /// Chunk whose source-local top-layer pass produced this write.
    pub source: (i32, i32),
    /// Absolute block coordinate and final state after the pass completed.
    pub position: (i32, i32, i32),
    pub state: CanonicalStateId,
}

#[derive(Clone, Copy)]
enum SpillCapture {
    None,
    All,
    CrossColumn,
    Epoch,
}

struct VegGridOreWindow<'a> {
    grid: &'a mut crate::feature::vegetation::VegGrid,
    center_x: i32,
    center_z: i32,
}

impl OreWorldAccess for VegGridOreWindow<'_> {
    #[inline]
    fn ore_get_id(&self, lx: i32, y: i32, lz: i32) -> CanonicalStateId {
        self.grid
            .get_id(self.center_x * 16 + lx, y, self.center_z * 16 + lz)
    }

    #[inline]
    fn ore_set_id(
        &mut self,
        lx: i32,
        y: i32,
        lz: i32,
        state: CanonicalStateId,
    ) -> bool {
        if !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lx)
            || !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lz)
        {
            return false;
        }
        self.grid
            .set_id_if_in_bounds(self.center_x * 16 + lx, y, self.center_z * 16 + lz, state)
    }

    #[inline]
    fn ore_entry_begin(&mut self) {
        OreWorldAccess::ore_entry_begin(self.grid);
    }

    fn ore_entry_end(&mut self) {
        OreWorldAccess::ore_entry_end(self.grid);
    }
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
    seeded: HashMap<(i32, i32, i32), CanonicalStateId>,
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
/// staged store. This second layer points into one request-owned bounded
/// product window and keeps fixed source-plan indices and a compact height view
/// alive across the source completion. Production target contexts are
/// radius-one; broader diagnostic batches may request a wider immutable window.
/// It contains no mutable world state and no placement result: resident overrides and every source's
/// RNG stream remain owned by the caller and are evaluated in authenticated order.
#[derive(Debug)]
pub struct MixedReplayContext {
    window: Arc<MixedReplayWindow>,
    wide_pre: [u16; crate::feature::region_view::WIDE_SLOTS],
    centre_pre: Option<u16>,
    centre_biomes: Arc<super::biome_cells::BiomeCells>,
    ocean_floor_wg: crate::feature::RegionHeights,
    source_plans: [u16; 9],
}

impl MixedReplayContext {
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.window.retained_bytes()
    }

    pub(crate) fn centre_pre_ore(&self) -> Option<&Arc<super::PreOreResult>> {
        self.centre_pre.map(|index| {
            self.window.products[index as usize]
                .pre
                .as_ref()
                .expect("a centre product index must point at a pre-ore product")
        })
    }

    #[inline]
    fn pre(&self, index: u16) -> Option<&super::PreOreResult> {
        (index != u16::MAX)
            .then(|| self.window.products.get(index as usize))
            .flatten()
            .and_then(|product| product.pre.as_deref())
    }

    #[inline]
    fn source_plan(&self, source_x: i32, source_z: i32, cx: i32, cz: i32) -> Option<&MixedReplaySourcePlan> {
        let dx = source_x - cx;
        let dz = source_z - cz;
        let radius = super::TARGET_DECORATION_RADIUS;
        if !(-radius..=radius).contains(&dx) || !(-radius..=radius).contains(&dz) {
            return None;
        }
        Some(&self.window.source_plans[self.source_plans[((dx + 1) * 3 + dz + 1) as usize] as usize])
    }
}

#[derive(Clone, Debug)]
enum MixedReplayEntry {
    Decoration {
        step: i32,
        index: usize,
        placed: Arc<crate::feature::vegetation::PlacedRef>,
    },
    Ore {
        step: i32,
        index: usize,
        ore: usize,
    },
}

fn merge_replay_entries(
    features: &[(i32, usize, Arc<crate::feature::vegetation::PlacedRef>)],
    ores: &[PlacedOre],
) -> Vec<MixedReplayEntry> {
    let mut entries = Vec::with_capacity(features.len() + ores.len());
    entries.extend(features.iter().map(|(step, index, placed)| {
        MixedReplayEntry::Decoration {
            step: *step,
            index: *index,
            placed: Arc::clone(placed),
        }
    }));
    entries.extend(ores.iter().enumerate().map(|(ore, placed)| MixedReplayEntry::Ore {
        step: crate::feature::STEP_UNDERGROUND_ORES,
        index: placed.index,
        ore,
    }));
    entries.sort_by(|left, right| {
        let key = |entry: &MixedReplayEntry| match entry {
            MixedReplayEntry::Decoration { step, index, .. } => (*step, *index, 0_u8),
            MixedReplayEntry::Ore { step, index, .. } => (*step, *index, 1_u8),
        };
        key(left).cmp(&key(right))
    });
    entries
}

/// Immutable source selection shared by every target whose 3×3 window contains
/// this source. Selection depends on the source's biome union, not on the
/// target-relative coordinates, so adjacent targets can retain one plan.
#[derive(Debug)]
pub(crate) struct MixedReplaySourcePlan {
    source: (i32, i32),
    ores: Arc<Vec<PlacedOre>>,
    entries: Arc<Vec<MixedReplayEntry>>,
}

struct MixedReplayProduct {
    coordinate: (i32, i32),
    pre: Option<Arc<super::PreOreResult>>,
    heights: [i32; 256],
}

struct MixedReplayWindow {
    products: Vec<MixedReplayProduct>,
    source_plans: Vec<Arc<MixedReplaySourcePlan>>,
    heights: Arc<crate::feature::RegionHeightStorage>,
    feature_biomes: Arc<crate::compose::FeatureBiomePlan>,
}

impl std::fmt::Debug for MixedReplayWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixedReplayWindow")
            .field("products", &self.products.len())
            .field("source_plans", &self.source_plans.len())
            .finish()
    }
}

impl MixedReplayWindow {
    fn product_index(&self, coordinate: (i32, i32)) -> u16 {
        self.products
            .binary_search_by_key(&coordinate, |product| product.coordinate)
            .ok()
            .unwrap_or_else(|| panic!("replay product missing for {coordinate:?}")) as u16
    }

    fn source_plan_index(&self, source: (i32, i32)) -> u16 {
        self.source_plans
            .binary_search_by_key(&source, |plan| plan.source)
            .ok()
            .unwrap_or_else(|| panic!("replay source plan missing for {source:?}")) as u16
    }

    fn retained_bytes(&self) -> usize {
        let mut seen_features = HashSet::new();
        let feature_bytes = self
            .source_plans
            .iter()
            .map(|plan| {
                std::mem::size_of::<MixedReplaySourcePlan>()
                    + plan.ores.capacity() * std::mem::size_of::<PlacedOre>()
                    + plan.ores.iter().map(placed_ore_retained_bytes).sum::<usize>()
                    + plan.entries.capacity() * std::mem::size_of::<MixedReplayEntry>()
                    + plan.entries.iter().filter_map(|entry| {
                        let MixedReplayEntry::Decoration { placed, .. } = entry else {
                            return None;
                        };
                        seen_features.insert(Arc::as_ptr(placed)).then(|| placed_ref_retained_bytes(placed))
                    }).sum::<usize>()
            })
            .sum::<usize>();
        std::mem::size_of::<Self>()
            + self.products.capacity() * std::mem::size_of::<MixedReplayProduct>()
            + self.heights.retained_bytes()
            + feature_bytes
    }
}

fn make_replay_window(
    products: Vec<MixedReplayProduct>,
    source_plans: Vec<Arc<MixedReplaySourcePlan>>,
    feature_biomes: Arc<crate::compose::FeatureBiomePlan>,
) -> Arc<MixedReplayWindow> {
    assert!(products.len() <= u16::MAX as usize, "replay product window exceeds compact index space");
    assert!(source_plans.len() <= u16::MAX as usize, "replay source-plan window exceeds compact index space");
    let heights = crate::feature::RegionHeightStorage::from_columns(
        products.iter().map(|product| product.heights).collect(),
    );
    Arc::new(MixedReplayWindow {
        products,
        source_plans,
        heights,
        feature_biomes,
    })
}

fn context_from_window(
    window: Arc<MixedReplayWindow>,
    cx: i32,
    cz: i32,
    read_radius: i32,
    centre_pre: Option<u16>,
    centre_biomes: Arc<super::biome_cells::BiomeCells>,
) -> MixedReplayContext {
    let mut wide_pre = [u16::MAX; crate::feature::region_view::WIDE_SLOTS];
    for dx in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
        for dz in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
            if dx.abs() <= read_radius && dz.abs() <= read_radius {
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)] =
                    window.product_index((cx + dx, cz + dz));
            }
        }
    }
    let mut source_plans = [u16::MAX; 9];
    for &(dx, dz) in overworld_source_offsets() {
        source_plans[((dx + 1) * 3 + dz + 1) as usize] =
            window.source_plan_index((cx + dx, cz + dz));
    }
    MixedReplayContext {
        ocean_floor_wg: crate::feature::RegionHeights::from_shared(
            Arc::clone(&window.heights),
            wide_pre,
        ),
        window,
        wide_pre,
        centre_pre,
        centre_biomes,
        source_plans,
    }
}

/// Request-owned immutable preparation for a set of adjacent Overworld targets.
/// The generator's staged store remains the only cross-request memo; dropping
/// this value releases the batch's terrain handles, height tables and plans.
#[derive(Debug)]
pub struct MixedReplayBatch {
    order: Vec<(i32, i32)>,
    contexts: Vec<((i32, i32), Arc<MixedReplayContext>)>,
    window: Arc<MixedReplayWindow>,
}

#[derive(Debug)]
pub struct RegionFeatureEpoch {
    grid: crate::feature::vegetation::VegGrid,
    min_chunk_x: i32,
    min_chunk_z: i32,
    width: usize,
    depth: usize,
    column_writes: Vec<Vec<usize>>,
    writes: Vec<ParityDecorationSpill>,
    /// Direct-address membership for resident override positions. The
    /// materializer feeds revisions into this once; it is separate from the
    /// decoration overlay so an override can be applied in the canonical
    /// ordered pass without walking the server's BTreeMap again.
    override_positions: Overlay,
    override_column_positions: Vec<Vec<(i32, i32, i32)>>,
    override_cursor: usize,
    override_entries_applied: usize,
    override_target_count: usize,
    last_target: Option<(i32, i32)>,
}

enum EpochOverrideInput<'a> {
    None,
    Map(&'a BTreeMap<(i32, i32, i32), CanonicalStateId>),
    Events(&'a [((i32, i32, i32), CanonicalStateId)]),
}

impl RegionFeatureEpoch {
    #[must_use]
    pub fn writes(&self) -> &[ParityDecorationSpill] {
        &self.writes
    }

    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.grid.retained_bytes()
            + self.column_writes.capacity() * std::mem::size_of::<Vec<usize>>()
            + self
                .column_writes
                .iter()
                .map(|writes| writes.capacity() * std::mem::size_of::<usize>())
                .sum::<usize>()
            + self.writes.capacity() * std::mem::size_of::<ParityDecorationSpill>()
            + self.override_positions.retained_bytes()
            + self
                .override_column_positions
                .capacity()
                * std::mem::size_of::<Vec<(i32, i32, i32)>>()
            + self
                .override_column_positions
                .iter()
                .map(|positions| {
                    positions.capacity() * std::mem::size_of::<(i32, i32, i32)>()
                })
                .sum::<usize>()
    }

    #[inline]
    fn column_slot(&self, chunk_x: i32, chunk_z: i32) -> Option<usize> {
        let x = usize::try_from(chunk_x - self.min_chunk_x).ok()?;
        let z = usize::try_from(chunk_z - self.min_chunk_z).ok()?;
        (x < self.width && z < self.depth).then_some(z * self.width + x)
    }

    fn apply_prior_writes(
        &self,
        target: (i32, i32),
        world: &mut crate::dense_grid::DenseBlockGrid,
    ) {
        let Some(slot) = self.column_slot(target.0, target.1) else {
            return;
        };
        for &index in &self.column_writes[slot] {
            let write = &self.writes[index];
            world.set_id(write.position.0, write.position.1, write.position.2, write.state);
        }
    }

    fn consume_override_events(
        &mut self,
        events: &[((i32, i32, i32), CanonicalStateId)],
    ) {
        assert!(
            self.override_cursor <= events.len(),
            "override revision cursor cannot move backwards"
        );
        let newly_consumed = events.len() - self.override_cursor;
        for &((x, y, z), state) in &events[self.override_cursor..] {
            let position = (x, y, z);
            if self
                .override_positions
                .insert_if_in_bounds_with_new(position, state)
                == Some(true)
            {
                if let Some(slot) = self.column_slot(x.div_euclid(16), z.div_euclid(16)) {
                    self.override_column_positions[slot].push(position);
                }
            }
            let _ = self.grid.seed_epoch_absolute_id(x, y, z, state);
        }
        self.override_cursor = events.len();
        self.override_entries_applied += newly_consumed;
    }

    #[must_use]
    pub fn override_application_counts(&self) -> (usize, usize) {
        (self.override_entries_applied, self.override_target_count)
    }

    fn seed_map_overrides(
        &mut self,
        target: (i32, i32),
        overrides: &BTreeMap<(i32, i32, i32), CanonicalStateId>,
    ) {
        let local_lo = crate::feature::REGION_MIN - crate::feature::vegetation::GEODE_PADDING;
        let local_hi = crate::feature::REGION_MAX + crate::feature::vegetation::GEODE_PADDING;
        for (&position, &state) in overrides {
            if self
                .override_positions
                .insert_if_in_bounds_with_new(position, state)
                == Some(true)
            {
                if let Some(slot) = self.column_slot(
                    position.0.div_euclid(16),
                    position.2.div_euclid(16),
                ) {
                    self.override_column_positions[slot].push(position);
                }
            }
            let local_x = position.0 - target.0 * 16;
            let local_z = position.2 - target.1 * 16;
            if (local_lo..local_hi).contains(&local_x)
                && (local_lo..local_hi).contains(&local_z)
            {
                self.grid.seed_id(position.0, position.1, position.2, state);
            }
        }
    }

    fn apply_override_writes(
        &mut self,
        target: (i32, i32),
        world: &mut crate::dense_grid::DenseBlockGrid,
    ) {
        let Some(slot) = self.column_slot(target.0, target.1) else {
            return;
        };
        let positions = &mut self.override_column_positions[slot];
        positions.sort_unstable();
        for &(x, y, z) in positions.iter() {
            let state = self
                .override_positions
                .get_if_in_bounds(&(x, y, z))
                .expect("override position has a direct-address value");
            world.set_id(x, y, z, state);
        }
    }

    fn record_target_writes(
        &mut self,
        target: (i32, i32),
        sparse_padding: bool,
    ) -> (Vec<ParityDecorationSpill>, Vec<ParityDecorationSpill>) {
        let mut seen = HashSet::new();
        let mut local = Vec::new();
        let mut spills = Vec::new();
        let mut raw_entries = 0;
        let mut unique_positions = 0;
        for (x, y, z, state) in self.grid.dirty_cells() {
            raw_entries += 1;
            let position = (x, y, z);
            if !seen.insert(position) {
                continue;
            }
            unique_positions += 1;
            let local_write = (x.div_euclid(16), z.div_euclid(16)) == target;
            crate::counters::bump_epoch_dirty_write(local_write);
            let mutation = ParityDecorationSpill {
                source: target,
                position,
                state,
            };
            let index = self.writes.len();
            self.writes.push(mutation.clone());
            if let Some(slot) = self.column_slot(x.div_euclid(16), z.div_euclid(16)) {
                self.column_writes[slot].push(index);
            }
            if local_write {
                local.push(mutation);
            } else {
                spills.push(mutation);
            }
        }
        crate::counters::bump_epoch_dirty_dedup(
            sparse_padding,
            raw_entries,
            unique_positions,
        );
        (local, spills)
    }
}

impl MixedReplayBatch {
    /// Targets in the caller's first-occurrence order.
    #[must_use]
    pub fn targets(&self) -> &[(i32, i32)] {
        &self.order
    }

    /// Returns the immutable context for one admitted target.
    #[must_use]
    pub fn context(&self, target: (i32, i32)) -> Option<&MixedReplayContext> {
        self.contexts
            .iter()
            .find(|(coordinate, _)| *coordinate == target)
            .map(|(_, context)| context.as_ref())
    }

    /// Clone the target context handle for a lifecycle owner. The batch keeps
    /// the product alive while the returned context is in use.
    #[must_use]
    pub fn context_arc(&self, target: (i32, i32)) -> Option<Arc<MixedReplayContext>> {
        self.contexts
            .iter()
            .find(|(coordinate, _)| *coordinate == target)
            .map(|(_, context)| Arc::clone(context))
    }

    /// Writes each context in the request's stable target order. The closure
    /// is the production seam: callers can run source bodies and persist their
    /// results while this request-owned product remains live.
    pub fn write_contexts<F>(&self, mut writer: F)
    where
        F: FnMut((i32, i32), &MixedReplayContext),
    {
        for &target in &self.order {
            writer(target, self.context(target).expect("batch target context missing"));
        }
    }

    /// Number of unique pre-ore products admitted by this request.
    #[must_use]
    pub fn pre_ore_product_count(&self) -> usize {
        self.window.products.iter().filter(|product| product.pre.is_some()).count()
    }

    /// Number of source-local selection plans retained by this request.
    #[must_use]
    pub fn source_plan_count(&self) -> usize {
        self.window.source_plans.len()
    }

    pub(crate) fn source_plan(&self, source: (i32, i32)) -> &MixedReplaySourcePlan {
        self.window
            .source_plans
            .binary_search_by_key(&source, |plan| plan.source)
            .ok()
            .map(|index| self.window.source_plans[index].as_ref())
            .expect("batch source plan missing")
    }

    /// Conservative accounting for storage retained by this request product.
    /// The dense source fields are bounded by the admitted dependency union;
    /// no generator-global decoration map is involved.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        let world_bytes = self.window.products.iter().filter_map(|product| product.pre.as_ref()).map(|pre| {
                let (_, _, _, sx, sy, sz) = pre.0.bounds();
                (sx as usize)
                    .saturating_mul(sy as usize)
                    .saturating_mul(sz as usize)
                    .saturating_mul(std::mem::size_of::<CanonicalStateId>())
            }).sum::<usize>();
        std::mem::size_of::<Self>()
            + self.order.capacity() * std::mem::size_of::<(i32, i32)>()
            + world_bytes
            + self.window.retained_bytes()
    }
}

fn unique_target_order(targets: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut order = Vec::with_capacity(targets.len());
    let mut seen = BTreeSet::new();
    for &target in targets {
        if seen.insert(target) {
            order.push(target);
        }
    }
    order
}

#[cfg(test)]
fn replay_dependency_union(targets: &[(i32, i32)]) -> BTreeSet<(i32, i32)> {
    replay_dependency_union_with_radius(targets, crate::feature::region_view::WIDE_RADIUS)
}

fn replay_dependency_union_with_radius(
    targets: &[(i32, i32)],
    radius: i32,
) -> BTreeSet<(i32, i32)> {
    let mut dependencies = BTreeSet::new();
    for &(cx, cz) in targets {
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                dependencies.insert((cx + dx, cz + dz));
            }
        }
    }
    dependencies
}

fn placed_ore_retained_bytes(value: &PlacedOre) -> usize {
    value
        .registry_id
        .as_ref()
        .map(String::capacity)
        .unwrap_or(0)
        + value.placements.capacity() * std::mem::size_of::<crate::feature::Placement>()
        + value.config.targets.capacity() * std::mem::size_of::<crate::feature::OreTarget>()
        + value
            .config
            .targets
            .iter()
            .map(|target| {
                std::mem::size_of::<CanonicalStateId>()
                    + match &target.target {
                        crate::feature::RuleTest::TagMatch(value)
                        | crate::feature::RuleTest::BlockMatch(value) => value.capacity(),
                        crate::feature::RuleTest::TagMatchCompiled { .. }
                        | crate::feature::RuleTest::BlockMatchCompiled(_) => 0,
                    }
            })
            .sum::<usize>()
}

fn placed_ref_retained_bytes(value: &crate::feature::vegetation::PlacedRef) -> usize {
    value
        .registry_id
        .as_ref()
        .map(String::capacity)
        .unwrap_or(0)
        + value.placements.capacity()
            * std::mem::size_of::<crate::feature::vegetation::VegPlacement>()
        + std::mem::size_of::<crate::feature::vegetation::ConfiguredFeature>()
}

#[inline]
fn product_index_for(
    chunk_x: i32,
    chunk_z: i32,
    min_x: i32,
    min_z: i32,
    width: usize,
    depth: usize,
) -> usize {
    let x = usize::try_from(chunk_x - min_x).expect("source product x");
    let z = usize::try_from(chunk_z - min_z).expect("source product z");
    assert!(x < width && z < depth, "source product outside rectangle");
    z * width + x
}

impl OverworldGenerator {
    /// Stage 5: the real `UNDERGROUND_ORES` 3×3 neighbourhood
    /// driver (`crate::feature::apply_ore_step_3x3_per_source`). Builds the
    /// driven region (centre plus its 8 neighbours, each via
    /// [`Self::pre_ore_stage`]) and the `OCEAN_FLOOR_WG` heightmap over the
    /// same region, then runs all 9 source chunks' own ore decoration step —
    /// each source resolving its own biome the same way
    /// [`Self::carver_biome_for_source`] resolves carver biome — and returns
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
        overrides: &[(i32, i32, i32, CanonicalStateId)],
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
        overrides: &[(i32, i32, i32, CanonicalStateId)],
    ) -> ParityDecorationResult {
        assert!(
            (target_x - source_x).abs() <= 1 && (target_z - source_z).abs() <= 1,
            "a decoration source must be inside the target's 3x3 dispatch window",
        );
        if self.decoration_catalog.is_empty() {
            return ParityDecorationResult {
                spills: Vec::new(),
                block_entities: Vec::new(),
                local_features: Vec::new(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        let context = self.replay_context_for(target_x, target_z, (source_x, source_z));
        self.parity_source_decoration_with_context(
            target_x,
            target_z,
            source_x,
            source_z,
            overrides,
            &context,
        )
    }

    /// Complete one source's FEATURES body using a context retained by the
    /// request that owns the source wavefront.
    #[must_use]
    pub fn parity_source_decoration_with_context(
        &self,
        target_x: i32,
        target_z: i32,
        source_x: i32,
        source_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
    ) -> ParityDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target_x, target_z));
        if self.decoration_catalog.is_empty() {
            return ParityDecorationResult {
                spills: Vec::new(),
                block_entities: Vec::new(),
                local_features: Vec::new(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        self.mixed_features_stage_selected(
            target_x,
            target_z,
            Some((*pre.0).clone()),
            context,
            Some((source_x, source_z)),
            overrides,
            SpillCapture::All,
            None,
        )
        .1
    }

    /// Runs the one target FEATURES completion against its radius-one
    /// CARVERS region and returns the complete transition stream. The target
    /// origin owns this invocation; neighbouring columns are read/write
    /// context, not additional source bodies.
    #[must_use]
    pub fn parity_features_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
    ) -> ParityDecorationResult {
        let pre = self.pre_ore_stage(target_x, target_z);
        if self.decoration_catalog.is_empty() {
            return ParityDecorationResult {
                spills: Vec::new(),
                block_entities: Vec::new(),
                local_features: Vec::new(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        let context = self.replay_context_for(target_x, target_z, (target_x, target_z));
        self.mixed_features_stage_selected(
            target_x,
            target_z,
            Some((*pre.0).clone()),
            &context,
            Some((target_x, target_z)),
            overrides,
            SpillCapture::All,
            None,
        )
        .1
    }

    /// Complete the target-owned Overworld decoration and top-layer stages in
    /// one pass. The dense target result is adopted directly; only writes into
    /// other columns cross the lifecycle boundary.
    #[must_use]
    pub fn direct_decoration_with_overrides(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
    ) -> DirectDecorationResult {
        let pre = self.pre_ore_stage(target_x, target_z);
        if self.decoration_catalog.is_empty() {
            let mut local_states = BTreeMap::new();
            let (world, _) =
                self.top_layer_stage_with_observer(
                    target_x,
                    target_z,
                    (*pre.0).clone(),
                    &pre.2,
                    &mut |x, y, z, state| {
                        local_states.insert((x, y, z), state);
                    },
                );
            let column = self.intern_from_dense_typed(
                target_x,
                target_z,
                super::output::GenStage::Full,
                world,
                pre.2.clone(),
                (*pre.3).clone(),
                Vec::new(),
            );
            return DirectDecorationResult {
                column,
                spills: Vec::new(),
                local_features: local_states
                    .into_iter()
                    .map(|(position, state)| ParityDecorationSpill {
                        source: (target_x, target_z),
                        position,
                        state,
                    })
                    .collect(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        let context = self.replay_context_for(target_x, target_z, (target_x, target_z));
        self.direct_decoration_with_context_and_pre(
            target_x,
            target_z,
            overrides,
            &context,
            &pre,
            Some((target_x, target_z)),
        )
    }

    /// Complete target-owned decoration using a context prepared by the
    /// request batch. This is the direct-output counterpart to source replay's
    /// `parity_source_decoration_with_context` seam.
    #[must_use]
    pub fn direct_decoration_with_context(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
    ) -> DirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target_x, target_z));
        self.direct_decoration_with_context_and_pre(
            target_x,
            target_z,
            overrides,
            context,
            &pre,
            Some((target_x, target_z)),
        )
    }

    /// Complete only the target's own source body against a retained context.
    /// Neighbor source bodies remain separate lifecycle completions.
    #[must_use]
    pub fn direct_source_decoration_with_context(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
    ) -> DirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target_x, target_z));
        self.direct_decoration_with_context_and_pre(
            target_x,
            target_z,
            overrides,
            context,
            &pre,
            Some((target_x, target_z)),
        )
    }

    /// Run FEATURES and TOP_LAYER in production order while retaining only
    /// sparse writes and generated entities.
    #[must_use]
    pub fn sparse_direct_decoration_with_context(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
    ) -> SparseDirectDecorationResult {
        self.sparse_decoration_with_context_selected(
            target_x,
            target_z,
            overrides,
            context,
            Some((target_x, target_z)),
        )
    }

    /// Complete only the target's own source body while retaining sparse
    /// local writes, outward spills, and generated entities for settlement.
    #[must_use]
    pub fn sparse_source_decoration_with_context(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
    ) -> SparseDirectDecorationResult {
        self.sparse_decoration_with_context_selected(
            target_x,
            target_z,
            overrides,
            context,
            Some((target_x, target_z)),
        )
    }

    fn sparse_decoration_with_context_selected(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
        selected_source: Option<(i32, i32)>,
    ) -> SparseDirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target_x, target_z));
        if self.decoration_catalog.is_empty() {
            let mut world = (*pre.0).clone();
            let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
            for &(x, y, z, state) in overrides {
                if (min_x..min_x + size_x).contains(&x)
                    && (min_y..min_y + size_y).contains(&y)
                    && (min_z..min_z + size_z).contains(&z)
                {
                    world.set_id(x, y, z, state);
                }
            }
            let mut local_states = BTreeMap::new();
            let (world, _) = self.top_layer_stage_with_observer(
                target_x,
                target_z,
                world,
                &pre.2,
                &mut |x, y, z, state| {
                    local_states.insert((x, y, z), state);
                },
            );
            drop(world);
            return SparseDirectDecorationResult {
                spills: Vec::new(),
                local_features: local_states
                    .into_iter()
                    .map(|(position, state)| ParityDecorationSpill {
                        source: (target_x, target_z),
                        position,
                        state,
                    })
                    .collect(),
                block_entities: Vec::new(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        let (world, result) = self.mixed_features_stage_selected(
            target_x,
            target_z,
            Some((*pre.0).clone()),
            context,
            selected_source,
            overrides,
            SpillCapture::CrossColumn,
            None,
        );
        let mut local_states = result
            .local_features
            .into_iter()
            .map(|spill| (spill.position, spill.state))
            .collect::<BTreeMap<_, _>>();
        let (_, _) = self.top_layer_stage_with_observer(
            target_x,
            target_z,
            world.expect("sparse scalar decoration retains its dense world"),
            &pre.2,
            &mut |x, y, z, state| {
                local_states.insert((x, y, z), state);
            },
        );
        SparseDirectDecorationResult {
            spills: result.spills,
            local_features: local_states
                .into_iter()
                .map(|(position, state)| ParityDecorationSpill {
                    source: (target_x, target_z),
                    position,
                    state,
                })
                .collect(),
            block_entities: result.block_entities,
            bridge_sync_events: result.bridge_sync_events,
            bridge_sync_cells: result.bridge_sync_cells,
        }
    }

    fn direct_decoration_with_context_and_pre(
        &self,
        target_x: i32,
        target_z: i32,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        context: &MixedReplayContext,
        pre: &Arc<super::PreOreResult>,
        selected_source: Option<(i32, i32)>,
    ) -> DirectDecorationResult {
        if self.decoration_catalog.is_empty() {
            let mut local_states = BTreeMap::new();
            let (world, _) =
                self.top_layer_stage_with_observer(
                    target_x,
                    target_z,
                    (*pre.0).clone(),
                    &pre.2,
                    &mut |x, y, z, state| {
                        local_states.insert((x, y, z), state);
                    },
                );
            let column = self.intern_from_dense_typed(
                target_x,
                target_z,
                super::output::GenStage::Full,
                world,
                pre.2.clone(),
                (*pre.3).clone(),
                Vec::new(),
            );
            return DirectDecorationResult {
                column,
                spills: Vec::new(),
                local_features: local_states
                    .into_iter()
                    .map(|(position, state)| ParityDecorationSpill {
                        source: (target_x, target_z),
                        position,
                        state,
                    })
                    .collect(),
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
            };
        }
        let (world, result) = self.mixed_features_stage_selected(
            target_x,
            target_z,
            Some((*pre.0).clone()),
            &context,
            selected_source,
            overrides,
            SpillCapture::CrossColumn,
            None,
        );
        let mut local_states = result
            .local_features
            .into_iter()
            .map(|spill| (spill.position, spill.state))
            .collect::<BTreeMap<_, _>>();
        let (world, _) = self.top_layer_stage_with_observer(
            target_x,
            target_z,
            world.expect("direct decoration retains its dense world"),
            &pre.2,
            &mut |x, y, z, state| {
                local_states.insert((x, y, z), state);
            },
        );
        let local_features = local_states
            .into_iter()
            .map(|(position, state)| ParityDecorationSpill {
                source: (target_x, target_z),
                position,
                state,
            })
            .collect();
        let block_entities = result
            .block_entities
            .into_iter()
            .filter(|entity| {
                let (x, _, z) = entity.position();
                (x >> 4) == target_x && (z >> 4) == target_z
            })
            .collect();
        let column = self.intern_from_dense_typed(
            target_x,
            target_z,
            super::output::GenStage::Full,
            world,
            pre.2.clone(),
            (*pre.3).clone(),
            block_entities,
        );
        DirectDecorationResult {
            column,
            spills: result.spills,
            local_features,
            bridge_sync_events: result.bridge_sync_events,
            bridge_sync_cells: result.bridge_sync_cells,
        }
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
        overrides: &[(i32, i32, i32, CanonicalStateId)],
    ) -> Vec<ParityTopLayerSpill> {
        let pre = self.pre_ore_stage(source_x, source_z);
        let mut world = (*pre.0).clone();
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        for &(x, y, z, state) in overrides {
            if !(min_x..min_x + size_x).contains(&x)
                || !(min_y..min_y + size_y).contains(&y)
                || !(min_z..min_z + size_z).contains(&z)
            {
                continue;
            }
            world.set_id(x, y, z, state);
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
                state,
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
        overrides: &[(i32, i32, i32, CanonicalStateId)],
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

        let mut columns = Vec::with_capacity(crate::feature::region_view::WIDE_SLOTS);
        for dx in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
            for dz in -crate::feature::region_view::WIDE_RADIUS..=crate::feature::region_view::WIDE_RADIUS {
                columns.push(if dx == 0 && dz == 0 {
                    *center_heights
                } else {
                    wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)]
                        .as_ref()
                        .expect("every non-centre offset was filled above")
                        .1
                });
            }
        }
        let ocean_floor_wg = crate::feature::RegionHeights::from_shared(
            crate::feature::RegionHeightStorage::from_columns(columns),
            std::array::from_fn(|index| index as u16),
        );

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
                self.decoration_catalog.select_ores_mask(biomes),
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
        let biome_allows_membership = |pos: crate::feature::BlockPos, membership: FeatureMembershipId| {
            let Some(biome) = super::biome::zoomed_biome_ref(
                biome_zoom_seed,
                pos.x,
                pos.y,
                pos.z,
                biome_sources,
            ) else { return false };
            feature_biomes.allows(membership, biome)
        };

        // Borrowed once, outside the closure, so the view's lifetime is plainly
        // tied to two locals rather than to whatever the closure captured.
        let centre_source: &crate::dense_grid::DenseBlockGrid = &center_world;
        let wide_sources = &wide_pre;
        let mut view = crate::feature::region_view::RegionView::over_wide_sources(
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
        for &(x, y, z, state) in overrides {
            let lx = x - cx * 16;
            let lz = z - cz * 16;
            let id = state;
            if view.seed_read_id(lx, y, lz, id) {
                seeded.insert((lx, y, lz), id);
            }
        }

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        if let Some((source_x, source_z)) = selected_source {
            apply_ore_step_3x3_per_source_overworld_with_membership(
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
                Some(&biome_allows_membership),
                crate::feature::STEP_UNDERGROUND_ORES,
                Some((source_x, source_z)),
                &mut view,
                &ores_for_source,
            );
        } else {
            apply_ore_step_3x3_per_source_overworld_with_membership(
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
                Some(&biome_allows_membership),
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
                            state,
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
    /// Returns the built-in biomes visible to one source's own 3x3 feature
    /// neighbourhood without consulting the staged store.
    /// The centre is supplied by the caller because `column_timed` computes it
    /// locally; every other member of this neighbourhood is already present in
    /// the 5x5 `wide_pre` read rim.
    fn source_biomes(
        source_x: i32,
        source_z: i32,
        centre_x: i32,
        centre_z: i32,
        centre_biomes: &super::biome_cells::BiomeCells,
        wide_pre: &[Option<Arc<super::PreOreResult>>],
    ) -> BiomeMask {
        let mut biomes = BiomeMask::default();
        let radius = super::TARGET_DECORATION_RADIUS;
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let x = source_x + dx;
                let z = source_z + dz;
                let offset_x = x - centre_x;
                let offset_z = z - centre_z;
                if offset_x == 0 && offset_z == 0 {
                    for &biome in centre_biomes.palette().entries() {
                        biomes.insert_ref(biome);
                    }
                } else {
                    let pre = wide_pre[crate::feature::region_view::wide_slot_of_offset(
                        offset_x,
                        offset_z,
                    )]
                    .as_ref()
                    .expect("every source biome lies inside the 5x5 read rim");
                    for &biome in pre.3.palette().entries() {
                        biomes.insert_ref(biome);
                    }
                }
            }
        }
        biomes
    }

    fn source_biomes_from_products(
        source: (i32, i32),
        products: &[MixedReplayProduct],
    ) -> Option<BiomeMask> {
        let mut biomes = BiomeMask::default();
        let radius = super::TARGET_DECORATION_RADIUS;
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let coordinate = (source.0 + dx, source.1 + dz);
                let Some(pre) = products
                    .binary_search_by_key(&coordinate, |product| product.coordinate)
                    .ok()
                    .and_then(|index| products[index].pre.as_ref()) else {
                    return None;
                };
                for &biome in pre.3.palette().entries() {
                    biomes.insert_ref(biome);
                }
            }
        }
        Some(biomes)
    }

    /// Build the immutable target-owned dispatch product. Only the target
    /// source plan is selected because the radius-one context is C for this
    /// target; neighbouring source plans belong to broader diagnostics.
    pub fn lifecycle_replay_context(&self, cx: i32, cz: i32) -> Arc<MixedReplayContext> {
        let centre = self.pre_ore_stage(cx, cz);
        Arc::new(self.build_mixed_replay_context(
            cx,
            cz,
            &centre.1,
            Arc::clone(&centre.3),
            Some(Arc::clone(&centre)),
            Some((cx, cz)),
            super::TARGET_DECORATION_RADIUS,
        ))
    }

    #[must_use]
    pub fn begin_region_feature_epoch(
        &self,
        batch: &MixedReplayBatch,
        targets: &[(i32, i32)],
    ) -> RegionFeatureEpoch {
        self.begin_region_feature_epoch_with_window(Arc::clone(&batch.window), targets)
    }

    /// Start an epoch from a context whose batch window is already retained by
    /// the lifecycle request. This keeps target setup from rebuilding any
    /// immutable terrain or source plans.
    pub fn begin_region_feature_epoch_from_context(
        &self,
        context: &MixedReplayContext,
        targets: &[(i32, i32)],
    ) -> RegionFeatureEpoch {
        self.begin_region_feature_epoch_with_window(Arc::clone(&context.window), targets)
    }

    fn begin_region_feature_epoch_with_window(
        &self,
        window: Arc<MixedReplayWindow>,
        targets: &[(i32, i32)],
    ) -> RegionFeatureEpoch {
        assert!(!targets.is_empty(), "feature epoch requires a target");
        let min_target_x = targets.iter().map(|&(x, _)| x).min().expect("target x");
        let max_target_x = targets.iter().map(|&(x, _)| x).max().expect("target x");
        let min_target_z = targets.iter().map(|&(_, z)| z).min().expect("target z");
        let max_target_z = targets.iter().map(|&(_, z)| z).max().expect("target z");
        let min_chunk_x = min_target_x - 2;
        let max_chunk_x = max_target_x + 2;
        let min_chunk_z = min_target_z - 2;
        let max_chunk_z = max_target_z + 2;
        let width = usize::try_from(max_chunk_x - min_chunk_x + 1).expect("epoch width");
        let depth = usize::try_from(max_chunk_z - min_chunk_z + 1).expect("epoch depth");
        let product_window = Arc::clone(&window);
        let product_at = move |chunk_x: i32, chunk_z: i32| {
            product_window
                .products
                .binary_search_by_key(&(chunk_x, chunk_z), |product| product.coordinate)
                .ok()
                .and_then(|index| product_window.products[index].pre.as_ref())
                .map(|pre| Arc::clone(&pre.0))
        };
        let biome_at = {
            let window = Arc::clone(&window);
            move |chunk_x: i32, chunk_z: i32| {
                window
                    .products
                    .binary_search_by_key(&(chunk_x, chunk_z), |product| product.coordinate)
                    .ok()
                    .and_then(|index| window.products[index].pre.as_ref())
                    .map(|pre| Arc::clone(&pre.3))
            }
        };
        let mut grid = crate::feature::vegetation::VegGrid::with_dynamic_sources_and_biomes_shared_zoomed(
            self.min_y,
            self.height,
            0,
            0,
            crate::feature::REGION_MIN - crate::feature::vegetation::GEODE_PADDING,
            crate::feature::REGION_MAX + crate::feature::vegetation::GEODE_PADDING,
            min_chunk_x,
            min_chunk_z,
            width,
            depth,
            product_at,
            biome_at,
            Arc::clone(&window.feature_biomes),
            super::biome::biome_zoom_seed(self.seed),
        );
        let overlay_min_x = min_target_x * 16
            + crate::feature::REGION_MIN
            - crate::feature::vegetation::GEODE_PADDING;
        let overlay_max_x = max_target_x * 16
            + crate::feature::REGION_MAX
            + crate::feature::vegetation::GEODE_PADDING;
        let overlay_min_z = min_target_z * 16
            + crate::feature::REGION_MIN
            - crate::feature::vegetation::GEODE_PADDING;
        let overlay_max_z = max_target_z * 16
            + crate::feature::REGION_MAX
            + crate::feature::vegetation::GEODE_PADDING;
        grid.install_epoch_overlay(Overlay::with_bounds_xyz(
            overlay_min_x,
            overlay_max_x,
            self.min_y,
            self.height,
            overlay_min_z,
            overlay_max_z,
        ));
        RegionFeatureEpoch {
            grid,
            min_chunk_x,
            min_chunk_z,
            width,
            depth,
            column_writes: (0..width * depth).map(|_| Vec::new()).collect(),
            writes: Vec::new(),
            override_positions: Overlay::with_bounds_xyz(
                overlay_min_x,
                overlay_max_x,
                self.min_y,
                self.height,
                overlay_min_z,
                overlay_max_z,
            ),
            override_column_positions: (0..width * depth).map(|_| Vec::new()).collect(),
            override_cursor: 0,
            override_entries_applied: 0,
            override_target_count: 0,
            last_target: None,
        }
    }

    pub fn complete_region_feature_epoch_target(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        center_world: crate::dense_grid::DenseBlockGrid,
        context: &MixedReplayContext,
    ) -> DirectDecorationResult {
        self.complete_region_feature_epoch_target_with_overrides(
            epoch,
            target,
            center_world,
            context,
            EpochOverrideInput::None,
        )
    }

    fn complete_region_feature_epoch_target_with_overrides(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        center_world: crate::dense_grid::DenseBlockGrid,
        context: &MixedReplayContext,
        overrides: EpochOverrideInput<'_>,
    ) -> DirectDecorationResult {
        if let Some(last) = epoch.last_target {
            assert!((target.1, target.0) > (last.1, last.0), "feature epoch targets must be canonical");
        }
        epoch.override_target_count += 1;
        epoch.last_target = Some(target);
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target.0, target.1));
        let mut world = center_world;
        epoch.grid.begin_epoch_target(target.0, target.1);
        epoch.apply_prior_writes(target, &mut world);
        match overrides {
            EpochOverrideInput::None => {}
            EpochOverrideInput::Map(overrides) => {
                epoch.seed_map_overrides(target, overrides);
            }
            EpochOverrideInput::Events(events) => {
                epoch.consume_override_events(events);
            }
        }
        epoch.apply_override_writes(target, &mut world);
        let (world, result) = self.mixed_features_stage_selected(
            target.0,
            target.1,
            Some(world),
            context,
            Some(target),
            &[],
            SpillCapture::Epoch,
            Some(&mut epoch.grid),
        );
        let mut world = world.expect("full epoch target retains its dense output");
        let (world_after_top, _) = self.top_layer_stage_with_observer(
            target.0,
            target.1,
            world,
            &pre.2,
            &mut |x, y, z, state| {
                epoch.grid.set_id_if_in_bounds(x, y, z, state);
            },
        );
        world = world_after_top;
        let (local_features, spills) = epoch.record_target_writes(target, false);
        let block_entities = result
            .block_entities
            .into_iter()
            .filter(|entity| {
                let (x, _, z) = entity.position();
                (x.div_euclid(16), z.div_euclid(16)) == target
            })
            .collect();
        let column = self.intern_from_dense_typed(
            target.0,
            target.1,
            super::output::GenStage::Full,
            world,
            pre.2.clone(),
            (*pre.3).clone(),
            block_entities,
        );
        DirectDecorationResult {
            column,
            spills,
            local_features,
            bridge_sync_events: result.bridge_sync_events,
            bridge_sync_cells: result.bridge_sync_cells,
        }
    }

    pub fn complete_region_feature_epoch_target_from_context(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
    ) -> DirectDecorationResult {
        self.complete_region_feature_epoch_target_from_context_with_overrides(
            epoch,
            target,
            context,
            &BTreeMap::new(),
        )
    }

    pub fn complete_region_feature_epoch_target_from_context_with_overrides(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
        overrides: &BTreeMap<(i32, i32, i32), CanonicalStateId>,
    ) -> DirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target.0, target.1));
        self.complete_region_feature_epoch_target_with_overrides(
            epoch,
            target,
            (*pre.0).clone(),
            context,
            EpochOverrideInput::Map(overrides),
        )
    }

    pub fn complete_region_feature_epoch_target_from_context_with_override_events(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
        overrides: &[((i32, i32, i32), CanonicalStateId)],
    ) -> DirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target.0, target.1));
        self.complete_region_feature_epoch_target_with_overrides(
            epoch,
            target,
            (*pre.0).clone(),
            context,
            EpochOverrideInput::Events(overrides),
        )
    }

    /// Complete one padding target against the shared epoch while retaining
    /// only its local and outward writes. The dense working field is discarded
    /// after the dispatcher and is never compacted into a server column.
    pub fn complete_region_feature_epoch_target_sparse_from_context(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
    ) -> SparseDirectDecorationResult {
        self.complete_region_feature_epoch_target_sparse_from_context_with_overrides(
            epoch,
            target,
            context,
            &BTreeMap::new(),
        )
    }

    pub fn complete_region_feature_epoch_target_sparse_from_context_with_overrides(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
        overrides: &BTreeMap<(i32, i32, i32), CanonicalStateId>,
    ) -> SparseDirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target.0, target.1));
        epoch.override_target_count += 1;
        epoch.grid.begin_epoch_target(target.0, target.1);
        epoch.seed_map_overrides(target, overrides);
        let (_, result) = self.mixed_features_stage_selected(
            target.0,
            target.1,
            None,
            context,
            Some(target),
            &[],
            SpillCapture::Epoch,
            Some(&mut epoch.grid),
        );
        let mut discard_write = |_x: i32, _y: i32, _z: i32, _state: CanonicalStateId| {};
        let _ = self.apply_top_layer_to_grid(
            target.0,
            target.1,
            &pre.2,
            &mut epoch.grid,
            &mut discard_write,
        );
        let (local_features, spills) = epoch.record_target_writes(target, true);
        SparseDirectDecorationResult {
            spills,
            local_features,
            block_entities: result.block_entities,
            bridge_sync_events: result.bridge_sync_events,
            bridge_sync_cells: result.bridge_sync_cells,
        }
    }

    pub fn complete_region_feature_epoch_target_sparse_from_context_with_override_events(
        &self,
        epoch: &mut RegionFeatureEpoch,
        target: (i32, i32),
        context: &MixedReplayContext,
        overrides: &[((i32, i32, i32), CanonicalStateId)],
    ) -> SparseDirectDecorationResult {
        let pre = context
            .centre_pre_ore()
            .cloned()
            .unwrap_or_else(|| self.pre_ore_stage(target.0, target.1));
        epoch.override_target_count += 1;
        epoch.grid.begin_epoch_target(target.0, target.1);
        epoch.consume_override_events(overrides);
        let (_, result) = self.mixed_features_stage_selected(
            target.0,
            target.1,
            None,
            context,
            Some(target),
            &[],
            SpillCapture::Epoch,
            Some(&mut epoch.grid),
        );
        let mut discard_write = |_x: i32, _y: i32, _z: i32, _state: CanonicalStateId| {};
        let _ = self.apply_top_layer_to_grid(
            target.0,
            target.1,
            &pre.2,
            &mut epoch.grid,
            &mut discard_write,
        );
        let (local_features, spills) = epoch.record_target_writes(target, true);
        SparseDirectDecorationResult {
            spills,
            local_features,
            block_entities: result.block_entities,
            bridge_sync_events: result.bridge_sync_events,
            bridge_sync_cells: result.bridge_sync_cells,
        }
    }

    /// Prepares one request-owned mixed replay product for `targets`. Every
    /// terrain prefix in the union of the target radius-two windows is touched
    /// once, and every source selection shared by those windows is built once.
    /// The returned product is intentionally not stored on the generator.
    #[must_use]
    pub fn mixed_replay_batch(&self, targets: &[(i32, i32)]) -> MixedReplayBatch {
        self.mixed_replay_batch_with_radius(targets, crate::feature::region_view::WIDE_RADIUS)
    }

    /// Builds a replay batch with an explicit immutable read radius. Production
    /// lifecycle calls pass [`super::TARGET_DECORATION_RADIUS`]; the wider
    /// default remains available for diagnostics and benchmark comparisons.
    pub fn mixed_replay_batch_with_radius(
        &self,
        targets: &[(i32, i32)],
        read_radius: i32,
    ) -> MixedReplayBatch {
        self.mixed_replay_batch_with_radius_inner(targets, read_radius, true)
    }

    /// Builds a replay batch from prefixes prepared by the caller's admitted
    /// generation region. Production uses this after the shaped admission
    /// already covered the complete replay dependency union.
    #[must_use]
    pub fn mixed_replay_batch_with_radius_prepared(
        &self,
        targets: &[(i32, i32)],
        read_radius: i32,
    ) -> MixedReplayBatch {
        self.mixed_replay_batch_with_radius_inner(targets, read_radius, false)
    }

    fn mixed_replay_batch_with_radius_inner(
        &self,
        targets: &[(i32, i32)],
        read_radius: i32,
        prepare_prefixes: bool,
    ) -> MixedReplayBatch {
        assert!((0..=3).contains(&read_radius));
        let order = unique_target_order(targets);
        let positions = replay_dependency_union_with_radius(&order, read_radius)
            .into_iter()
            .collect::<Vec<_>>();
        if prepare_prefixes {
            self.prepare_pre_ore_position_union(&positions);
        }
        let products = positions
            .into_iter()
            .map(|position| {
                let pre = self.pre_ore_stage(position.0, position.1);
                MixedReplayProduct {
                    coordinate: position,
                    heights: pre.1,
                    pre: Some(pre),
                }
            })
            .collect::<Vec<_>>();

        let mut source_positions = BTreeSet::new();
        for &(cx, cz) in &order {
            for &(dx, dz) in overworld_source_offsets() {
                source_positions.insert((cx + dx, cz + dz));
            }
        }
        let source_plans = source_positions
            .into_iter()
            .map(|source| {
                // A radius-one target batch contains complete biome input for
                // each target origin, but not for the eight neighbouring source
                // origins retained only for scalar diagnostics. Those plans are
                // intentionally empty; target-owned dispatch never executes
                // them, and widening the batch would violate C = W expanded by
                // the authoritative radius.
                let biomes = Self::source_biomes_from_products(source, &products)
                    .unwrap_or_default();
                let selected = self.decoration_catalog.select_all_mask(biomes);
                let mut features = selected.features;
                features.extend(selected.step6_disks);
                features.extend(selected.step6_non_ore);
                features.sort_by_key(|(step, index, _)| (*step, *index));
                let entries = merge_replay_entries(&features, &selected.ores);
                Arc::new(MixedReplaySourcePlan {
                    source,
                    ores: Arc::new(selected.ores),
                    entries: Arc::new(entries),
                })
            })
            .collect::<Vec<_>>();

        let window = make_replay_window(
            products,
            source_plans,
            self.decoration_catalog.feature_biomes(),
        );

        let mut contexts = Vec::with_capacity(order.len());
        for &(cx, cz) in &order {
            let centre = window.product_index((cx, cz));
            let centre_biomes = window.products[centre as usize]
                .pre
                .as_ref()
                .expect("batch target must have a pre-ore product")
                .3
                .clone();
            let context = context_from_window(
                Arc::clone(&window),
                cx,
                cz,
                read_radius.min(crate::feature::region_view::WIDE_RADIUS),
                Some(centre),
                centre_biomes,
            );
            contexts.push(((cx, cz), Arc::new(context)));
        }

        MixedReplayBatch {
            order,
            contexts,
            window,
        }
    }

    /// Runs a writer over one request-owned mixed replay product and drops the
    /// product as soon as the writer returns.
    pub fn with_mixed_replay_batch<R>(
        &self,
        targets: &[(i32, i32)],
        writer: impl FnOnce(&MixedReplayBatch) -> R,
    ) -> R {
        let batch = self.mixed_replay_batch(targets);
        writer(&batch)
    }

    pub(crate) fn source_once_features_region(
        &self,
        targets: &[(i32, i32)],
        batch: &MixedReplayBatch,
        capture_provenance: bool,
    ) -> crate::overworld::SourceOnceBatchResult {
        use crate::overworld::fused_features::{
            SourceExecutionCount, SourceOnceExecution, SourceOnceTargetResult,
        };
        use crate::overworld::fused_features::SourceOnceMutation;

        assert!(!targets.is_empty(), "source-once request must contain a target");
        let mut sources = Vec::with_capacity(targets.len() * 9);
        let mut seen_sources = BTreeSet::new();
        for &(target_x, target_z) in targets {
            for &(dx, dz) in overworld_source_offsets() {
                let source = (target_x + dx, target_z + dz);
                if seen_sources.insert(source) {
                    sources.push(source);
                }
            }
        }

        let min_target_x = targets.iter().map(|&(x, _)| x).min().expect("target x");
        let max_target_x = targets.iter().map(|&(x, _)| x).max().expect("target x");
        let min_target_z = targets.iter().map(|&(_, z)| z).min().expect("target z");
        let max_target_z = targets.iter().map(|&(_, z)| z).max().expect("target z");
        let source_min_x = min_target_x - 3;
        let source_max_x = max_target_x + 3;
        let source_min_z = min_target_z - 3;
        let source_max_z = max_target_z + 3;
        let source_width = usize::try_from(source_max_x - source_min_x + 1)
            .expect("source layout width");
        let source_depth = usize::try_from(source_max_z - source_min_z + 1)
            .expect("source layout depth");

        let height_min_x = source_min_x;
        let height_min_z = source_min_z;
        let height_width = source_width;
        let height_depth = source_depth;
        let product_count = height_width * height_depth;
        let mut product_table = vec![None; product_count];
        for product in &batch.window.products {
            let (chunk_x, chunk_z) = product.coordinate;
            if let (Ok(x), Ok(z)) = (
                usize::try_from(chunk_x - height_min_x),
                usize::try_from(chunk_z - height_min_z),
            ) {
                if x < height_width && z < height_depth {
                    product_table[z * height_width + x] = product.pre.as_ref().map(Arc::clone);
                }
            }
        }
        for chunk_z in height_min_z..height_min_z + i32::try_from(height_depth).expect("height depth") {
            for chunk_x in height_min_x..height_min_x + i32::try_from(height_width).expect("height width") {
                let index = product_index_for(
                    chunk_x,
                    chunk_z,
                    height_min_x,
                    height_min_z,
                    height_width,
                    height_depth,
                );
                if product_table[index].is_none() {
                    product_table[index] = Some(self.pre_ore_stage(chunk_x, chunk_z));
                }
            }
        }
        let product_table = product_table
            .into_iter()
            .map(|product| product.expect("source product table entry"))
            .collect::<Vec<_>>();
        let product_index = |(chunk_x, chunk_z): (i32, i32)| {
            product_index_for(
                chunk_x,
                chunk_z,
                height_min_x,
                height_min_z,
                height_width,
                height_depth,
            )
        };
        let product_at = |coordinate: (i32, i32)| {
            Arc::clone(&product_table[product_index(coordinate)])
        };

        let mut height_columns = Vec::with_capacity(height_width * height_depth);
        for chunk_z in height_min_z..height_min_z + i32::try_from(height_depth).expect("height depth") {
            for chunk_x in height_min_x..height_min_x + i32::try_from(height_width).expect("height width") {
                height_columns.push(product_at((chunk_x, chunk_z)).1);
            }
        }
        let height_storage = crate::feature::RegionHeightStorage::from_columns(height_columns);
        let slots_for = |center_x: i32, center_z: i32| {
            let mut slots = [0u16; crate::feature::region_view::WIDE_SLOTS];
            for dx in -2..=2 {
                for dz in -2..=2 {
                    let chunk_x = center_x + dx;
                    let chunk_z = center_z + dz;
                    let index = (chunk_z - height_min_z) as usize * height_width
                        + (chunk_x - height_min_x) as usize;
                    slots[((dx + 2) * 5 + dz + 2) as usize] =
                        u16::try_from(index).expect("height product index");
                }
            }
            slots
        };
        // Slot translation is source-relative but immutable for this batch.
        // Build it once alongside the source plan order, rather than rebuilding
        // the same 5x5 table at every source's first ore entry.
        let source_slots = sources
            .iter()
            .copied()
            .map(|(source_x, source_z)| slots_for(source_x, source_z))
            .collect::<Vec<_>>();

        let feature_biomes = Arc::clone(&batch.window.feature_biomes);
        let biome_zoom_seed = super::biome::biome_zoom_seed(self.seed);
        let source_biome_coords = sources
            .iter()
            .copied()
            .filter(|&source| {
                batch
                    .source_plan(source)
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, MixedReplayEntry::Ore { .. }))
            })
            .collect::<Vec<_>>();
        let source_biome_grid = (!source_biome_coords.is_empty()).then(|| {
            OreBiomeMembershipGrid::new(
                &source_biome_coords,
                height_min_x,
                height_min_z,
                height_width,
                height_depth,
                &product_table,
                self.min_y,
                self.height,
                biome_zoom_seed,
                Arc::clone(&feature_biomes),
            )
        });
        let biome_allows_membership = |pos: crate::feature::BlockPos, membership: FeatureMembershipId| {
            source_biome_grid
                .as_ref()
                .is_some_and(|grid| grid.allows(pos, membership))
        };
        let in_tag = |block: &str, tag: &str| -> bool {
            self.ore_tag_map
                .get(tag)
                .is_some_and(|members| members.contains(block))
        };

        let mut grid = crate::feature::vegetation::VegGrid::with_dynamic_sources_and_biomes_shared_zoomed(
            self.min_y,
            self.height,
            source_min_x * 16,
            source_min_z * 16,
            0,
            i32::try_from(source_width * 16).expect("source layout width in blocks"),
            source_min_x,
            source_min_z,
            source_width,
            source_depth,
            |chunk_x, chunk_z| Some(Arc::clone(&product_at((chunk_x, chunk_z)).0)),
            |chunk_x, chunk_z| Some(Arc::clone(&product_at((chunk_x, chunk_z)).3)),
            Arc::clone(&batch.window.feature_biomes),
            biome_zoom_seed,
        );
        self.veg_tags.bind();

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let mut executions = Vec::with_capacity(sources.len());
        let mut source_ranges = capture_provenance.then(|| Vec::with_capacity(sources.len()));
        let mut mutable_writes = 0usize;
        for (source, slots) in sources.into_iter().zip(source_slots) {
            let plan = batch.source_plan(source);
            let before = grid.dirty_len();
            let origin = crate::feature::BlockPos {
                x: source.0 * 16,
                y: self.min_y,
                z: source.1 * 16,
            };
            random.begin_decoration_source();
            let decoration_seed = random.set_decoration_seed(self.seed, origin.x, origin.z);
            let ocean_floor_wg = crate::feature::RegionHeights::from_shared(
                Arc::clone(&height_storage),
                slots,
            );
            for entry in plan.entries.iter() {
                match entry {
                    MixedReplayEntry::Decoration {
                        step,
                        index,
                        placed,
                    } => crate::feature::vegetation::apply_decoration_entry_at_world_seed(
                        &mut random,
                        self.seed,
                        decoration_seed,
                        origin,
                        *step,
                        *index,
                        placed,
                        &mut grid,
                        &self.veg_tags,
                    ),
                    MixedReplayEntry::Ore {
                        step,
                        index: _,
                        ore,
                    } => {
                        let input = crate::feature::OreInput {
                            chunk_x: source.0,
                            chunk_z: source.1,
                            center_x: source.0,
                            center_z: source.1,
                            min_y: self.min_y,
                            height: self.height,
                            min_gen_y: self.min_y,
                            gen_depth: self.height,
                            read_min: crate::feature::ORE_READ_MIN,
                            read_max: crate::feature::ORE_READ_MAX,
                            ocean_floor_wg: &ocean_floor_wg,
                            in_tag: &in_tag,
                            biome_allows: None,
                        };
                        let mut window = VegGridOreWindow {
                            grid: &mut grid,
                            center_x: source.0,
                            center_z: source.1,
                        };
                        apply_ore_entry_at_seed_with_membership(
                            &mut random,
                            decoration_seed,
                            &input,
                            *step,
                            &plan.ores[*ore],
                            &mut window,
                            Some(&biome_allows_membership),
                        );
                    }
                }
            }
            let after = grid.dirty_len();
            mutable_writes += after - before;
            if let Some(ranges) = &mut source_ranges {
                ranges.push((source, before, after));
            }
            executions.push(SourceOnceExecution {
                source,
                owner_target: targets
                    .iter()
                    .copied()
                    .find(|&(x, z)| (source.0 - x).abs() <= 1 && (source.1 - z).abs() <= 1)
                    .expect("source has an owning target"),
                mutations: Vec::new(),
            });
        }

        let mut global_overrides = Vec::new();
        if let Some(ranges) = source_ranges {
            let mut seen = HashSet::with_capacity(grid.overlay_len());
            let mut canonical_by_local = Vec::<Option<CanonicalStateId>>::new();
            let mut source_index = ranges.len();
            let mut dirty_index = grid.dirty_len();
            for (x, y, z, _) in grid.dirty_cell_ids_reverse() {
                dirty_index -= 1;
                while source_index > 0 && dirty_index < ranges[source_index - 1].1 {
                    source_index -= 1;
                }
                let Some(current_source) = source_index.checked_sub(1) else {
                    break;
                };
                let (source, start, end) = ranges[current_source];
                if dirty_index < start || dirty_index >= end {
                    continue;
                }
                let position = (x, y, z);
                if !seen.insert(position) {
                    continue;
                }
                let local_state = grid
                    .overlay_id(x, y, z)
                    .expect("a dirty position must remain in the overlay");
                let local_index = local_state.index();
                if local_index >= canonical_by_local.len() {
                    canonical_by_local.resize(local_index + 1, None);
                }
                let state = *canonical_by_local[local_index].get_or_insert_with(|| {
                    local_state
                });
                executions[current_source].mutations.push(SourceOnceMutation {
                    source,
                    position,
                    state,
                });
            }
            for execution in &mut executions {
                execution.mutations.reverse();
                global_overrides.extend(execution.mutations.iter().copied());
            }
        }

        let mut entities_by_target = (0..targets.len()).map(|_| Vec::new()).collect::<Vec<_>>();
        for entity in grid.take_block_entities() {
            let (x, _, z) = entity.position();
            if let Some(index) = targets.iter().position(|&(cx, cz)| {
                x.div_euclid(16) == cx && z.div_euclid(16) == cz
            }) {
                entities_by_target[index].push(entity);
            }
        }
        let requested_writes = grid.writes_for_chunks_in_scan_order(targets);
        let requested = targets
            .iter()
            .copied()
            .enumerate()
            .map(|(index, target)| {
                let pre = product_at(target);
                let mut world = (*pre.0).clone();
                for (x, y, z, state) in &requested_writes[index] {
                    world.set_id(*x, *y, *z, *state);
                }
                let (world, _) = self.top_layer_stage(target.0, target.1, world, &pre.2);
                let column = self.intern_from_dense_typed(
                    target.0,
                    target.1,
                    super::GenStage::Full,
                    world,
                    pre.2.clone(),
                    (*pre.3).clone(),
                    std::mem::take(&mut entities_by_target[index]),
                );
                SourceOnceTargetResult { target, column }
            })
            .collect();
        let padding_mutations = if capture_provenance {
            let requested_set = targets.iter().copied().collect::<BTreeSet<_>>();
            global_overrides
                .iter()
                .filter(|mutation| {
                    !requested_set.contains(&(
                        mutation.position.0.div_euclid(16),
                        mutation.position.2.div_euclid(16),
                    ))
                })
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let region_retained_bytes = grid.retained_bytes();
        let execution_counts = executions
            .iter()
            .map(|execution| SourceExecutionCount {
                source: execution.source,
                count: 1,
            })
            .collect();
        crate::overworld::SourceOnceBatchResult::from_region_run(
            requested,
            executions,
            global_overrides,
            padding_mutations,
            execution_counts,
            mutable_writes,
            region_retained_bytes,
        )
    }

    fn replay_context_for(
        &self,
        cx: i32,
        cz: i32,
        source: (i32, i32),
    ) -> Arc<MixedReplayContext> {
        let centre = self.pre_ore_stage(cx, cz);
        Arc::new(self.build_mixed_replay_context(
            cx,
            cz,
            &centre.1,
            Arc::clone(&centre.3),
            Some(Arc::clone(&centre)),
            Some(source),
            if source == (cx, cz) {
                super::TARGET_DECORATION_RADIUS
            } else {
                crate::feature::region_view::WIDE_RADIUS
            },
        ))
    }

    /// Builds the immutable portion of one unified FEATURES dispatch.
    ///
    /// `center_heights` and `center_biomes` are supplied explicitly so the
    /// timing path can still measure a locally computed centre prefix without
    /// accidentally replacing it with the staged-store value. The normal
    /// production and lifecycle paths pass the staged centre values.
    fn build_mixed_replay_context(
        &self,
        cx: i32,
        cz: i32,
        center_heights: &[i32; 256],
        center_biomes: Arc<super::biome_cells::BiomeCells>,
        center_product: Option<Arc<super::PreOreResult>>,
        selected_source: Option<(i32, i32)>,
        read_radius: i32,
    ) -> MixedReplayContext {
        assert!(
            (0..=crate::feature::region_view::WIDE_RADIUS).contains(&read_radius),
            "mixed replay read radius exceeds its slot table"
        );
        let mut wide_pre:
            [Option<Arc<super::PreOreResult>>; crate::feature::region_view::WIDE_SLOTS] =
            std::array::from_fn(|_| None);
        for dx in -read_radius..=read_radius
        {
            for dz in -read_radius..=read_radius
            {
                if dx == 0 && dz == 0 {
                    continue;
                }
                wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)] =
                    Some(self.pre_ore_stage(cx + dx, cz + dz));
            }
        }
        let centre_biomes = center_biomes;

        let mut source_plans = Vec::with_capacity(9);
        for &(dx, dz) in overworld_source_offsets() {
            let source_x = cx + dx;
            let source_z = cz + dz;
            let source_biomes = if selected_source
                .is_none_or(|source| source == (source_x, source_z))
            {
                Self::source_biomes(
                    source_x,
                    source_z,
                    cx,
                    cz,
                    &centre_biomes,
                    &wide_pre,
                )
            } else {
                BiomeMask::default()
            };
            let selected = self.decoration_catalog.select_all_mask(source_biomes);
            let mut features = selected.features;
            features.extend(selected.step6_disks);
            features.extend(selected.step6_non_ore);
            // The production dispatcher merges all streams by global
            // step/index, so retaining this sort preserves the prior
            // ordering after the catalog scan was unified.
            features.sort_by_key(|(step, index, _)| (*step, *index));
            let entries = merge_replay_entries(&features, &selected.ores);
            source_plans.push(Arc::new(MixedReplaySourcePlan {
                source: (source_x, source_z),
                ores: Arc::new(selected.ores),
                entries: Arc::new(entries),
            }));
        }

        let mut products = Vec::new();
        for dx in -read_radius..=read_radius {
            for dz in -read_radius..=read_radius {
                let coordinate = (cx + dx, cz + dz);
                let pre = if dx == 0 && dz == 0 {
                    center_product.clone()
                } else {
                    wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)].clone()
                };
                let heights = if dx == 0 && dz == 0 {
                    *center_heights
                } else {
                    pre.as_ref()
                        .expect("every non-centre replay product was filled above")
                        .1
                };
                products.push(MixedReplayProduct {
                    coordinate,
                    pre,
                    heights,
                });
            }
        }
        let window = make_replay_window(
            products,
            source_plans,
            self.decoration_catalog.feature_biomes(),
        );
        let centre_index = window.product_index((cx, cz));
        context_from_window(
            window,
            cx,
            cz,
            read_radius,
            center_product.map(|_| centre_index),
            centre_biomes,
        )
    }

    /// Runs the complete Overworld FEATURES stage for normal generation.  The
    /// parent orchestration module can hand this the shaped prefix directly;
    /// the returned dense grid is the centre result and the entity list is in
    /// feature write order. The scalar path folds its nine admitted sources
    /// into the target while lifecycle generation settles one source at a time.
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
        let centre = self.pre_ore_stage(cx, cz);
        let context = self.build_mixed_replay_context(
            cx,
            cz,
            &centre.1,
            Arc::clone(&centre.3),
            Some(Arc::clone(&centre)),
            None,
            crate::feature::region_view::WIDE_RADIUS,
        );
        self.features_stage_with_context(
            cx,
            cz,
            center_world,
            &context,
            None,
            &[],
            false,
        )
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
            None,
            None,
            crate::feature::region_view::WIDE_RADIUS,
        );
        self.features_stage_with_context(
            cx,
            cz,
            center_world,
            &context,
            None,
            &[],
            false,
        )
    }

    fn features_stage_with_context(
        &self,
        cx: i32,
        cz: i32,
        center_world: crate::dense_grid::DenseBlockGrid,
        context: &MixedReplayContext,
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        capture_spills: bool,
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        Vec<super::block_entities::GeneratedBlockEntity>,
    ) {
        let (world, result) = self.mixed_features_stage_selected(
            cx,
            cz,
            Some(center_world),
            context,
            selected_source,
            overrides,
            if capture_spills {
                SpillCapture::All
            } else {
                SpillCapture::None
            },
            None,
        );
        let block_entities = result
            .block_entities
            .into_iter()
            .filter(|be| {
                let (x, _, z) = be.position();
                (x >> 4) == cx && (z >> 4) == cz
            })
            .collect();
        (
            world.expect("source feature stage retains its dense output"),
            block_entities,
        )
    }

    /// Runs the complete Overworld FEATURES dispatch over its shared
    /// read/write neighbourhood. `Some` executes one target/source origin;
    /// `None` retains the nine-origin diagnostic path. Ore and vegetation use
    /// the same `VegGrid` overlay, so later entries observe earlier writes
    /// directly.
    #[allow(clippy::too_many_arguments)]
    fn mixed_features_stage_selected(
        &self,
        cx: i32,
        cz: i32,
        mut center_world: Option<crate::dense_grid::DenseBlockGrid>,
        context: &MixedReplayContext,
        selected_source: Option<(i32, i32)>,
        overrides: &[(i32, i32, i32, CanonicalStateId)],
        spill_capture: SpillCapture,
        epoch_grid: Option<&mut crate::feature::vegetation::VegGrid>,
    ) -> (
        Option<crate::dense_grid::DenseBlockGrid>,
        ParityDecorationResult,
    ) {
        if self.decoration_catalog.is_empty() {
            return (
                center_world,
                ParityDecorationResult {
                    spills: Vec::new(),
                    block_entities: Vec::new(),
                    local_features: Vec::new(),
                    bridge_sync_events: 0,
                    bridge_sync_cells: 0,
                },
            );
        }
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Vegetation);

        if matches!(spill_capture, SpillCapture::CrossColumn) {
            let world = center_world
                .as_mut()
                .expect("cross-column capture needs a dense output fold");
            let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
            for &(x, y, z, state) in overrides {
                if (min_x..min_x + size_x).contains(&x)
                    && (min_y..min_y + size_y).contains(&y)
                    && (min_z..min_z + size_z).contains(&z)
                {
                    world.set_id(x, y, z, state);
                }
            }
        }

        let wide_pre = &context.wide_pre;
        let centre_biomes = &context.centre_biomes;
        let ocean_floor_wg = &context.ocean_floor_wg;

        let in_tag = |block: &str, tag: &str| -> bool {
            self.ore_tag_map
                .get(tag)
                .is_some_and(|members| members.contains(block))
        };
        let feature_biomes = &context.window.feature_biomes;
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
                context
                    .pre(wide_pre[crate::feature::region_view::wide_slot_of_offset(dx, dz)])
                    .map(|pre| &*pre.3)
            } else {
                None
            }
        };
        let biome_allows_membership = |pos: crate::feature::BlockPos, membership: FeatureMembershipId| {
            let Some(biome) = super::biome::zoomed_biome_ref(
                biome_zoom_seed,
                pos.x,
                pos.y,
                pos.z,
                biome_sources,
            ) else { return false };
            feature_biomes.allows(membership, biome)
        };

        let grid_sources = &wide_pre;
        let grid_biomes = &wide_pre;
        let mut owned_grid = if epoch_grid.is_none() {
            // The centre clone is the immutable source snapshot while the
            // supplied `center_world` remains the dense grid this function may return.
            let centre_grid = Arc::new(
                center_world
                    .as_ref()
                    .expect("owned feature grid needs a dense source snapshot")
                    .clone(),
            );
            Some(
                crate::feature::vegetation::VegGrid::with_sources_and_biomes_shared_zoomed(
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
                            context
                                .pre(grid_sources[crate::feature::region_view::wide_slot_of_offset(dx, dz)])
                                .map(|pre| Arc::clone(&pre.0))
                        }
                    },
                    |dx, dz| {
                        if dx == 0 && dz == 0 {
                            Some(Arc::clone(&centre_biomes))
                        } else {
                            context
                                .pre(grid_biomes[crate::feature::region_view::wide_slot_of_offset(dx, dz)])
                                .map(|pre| Arc::clone(&pre.3))
                        }
                    },
                    self.decoration_catalog.feature_biomes(),
                    biome_zoom_seed,
                ),
            )
        } else {
            None
        };
        let grid = if let Some(grid) = epoch_grid {
            grid.begin_epoch_target_if_needed(cx, cz);
            grid
        } else {
            owned_grid.as_mut().expect("owned feature grid")
        };
        self.veg_tags.bind();

        let mut dispatch_scratch = take_mixed_dispatch_scratch();
        let seeded = &mut dispatch_scratch.seeded;
        let local_lo = crate::feature::REGION_MIN - crate::feature::vegetation::GEODE_PADDING;
        let local_hi = crate::feature::REGION_MAX + crate::feature::vegetation::GEODE_PADDING;
        for &(x, y, z, state) in overrides {
            let id = state;
            let lx = x - cx * 16;
            let lz = z - cz * 16;
            if (local_lo..local_hi).contains(&lx)
                && (local_lo..local_hi).contains(&lz)
                && (self.min_y..self.min_y + self.height).contains(&y)
            {
                grid.seed_id(x, y, z, id);
            }
            seeded.insert((x, y, z), id);
        }

        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        // A target-owned body has C = W expanded by one chunk. Source-ordered
        // diagnostics retain the wider ore probe window because they may run
        // a neighbour origin against a different centre.
        let (read_min, read_max) = if selected_source == Some((cx, cz)) {
            (crate::feature::REGION_MIN, crate::feature::REGION_MAX)
        } else {
            (crate::feature::ORE_READ_MIN, crate::feature::ORE_READ_MAX)
        };
        for &(dx, dz) in overworld_source_offsets() {
            let source_x = cx + dx;
            let source_z = cz + dz;
            if selected_source.is_some_and(|source| source != (source_x, source_z)) {
                continue;
            }
            let plan = context
                .source_plan(source_x, source_z, cx, cz)
                .expect("every dispatched source has a source plan");
            let origin = crate::feature::BlockPos {
                x: source_x * 16,
                y: self.min_y,
                z: source_z * 16,
            };
            random.begin_decoration_source();
            let decoration_seed = random.set_decoration_seed(self.seed, origin.x, origin.z);

            for entry in plan.entries.iter() {
                match entry {
                    MixedReplayEntry::Decoration {
                        step,
                        index,
                        placed,
                    } => crate::feature::vegetation::apply_decoration_entry_at_world_seed(
                        &mut random,
                        self.seed,
                        decoration_seed,
                        origin,
                        *step,
                        *index,
                        placed,
                        grid,
                        &self.veg_tags,
                    ),
                    MixedReplayEntry::Ore {
                        step,
                        index: _,
                        ore,
                    } => {
                        let input = crate::feature::OreInput {
                            chunk_x: source_x,
                            chunk_z: source_z,
                            center_x: cx,
                            center_z: cz,
                            min_y: self.min_y,
                            height: self.height,
                            min_gen_y: self.min_y,
                            gen_depth: self.height,
                            read_min,
                            read_max,
                            ocean_floor_wg: &ocean_floor_wg,
                            in_tag: &in_tag,
                            biome_allows: None,
                        };
                        crate::feature::apply_ore_entry_at_seed_with_membership(
                            &mut random,
                            decoration_seed,
                            &input,
                            *step,
                            &plan.ores[*ore],
                            grid,
                            Some(&biome_allows_membership),
                        );
                    }
                }
            }
        }

        let mut spills = Vec::new();
        let mut local_features = Vec::new();
        if !matches!(spill_capture, SpillCapture::None | SpillCapture::Epoch) {
            let source = selected_source.unwrap_or((cx, cz));
            let mut final_cells = BTreeMap::new();
            for (x, y, z, state) in grid.dirty_cells() {
                final_cells.insert((x, y, z), state);
            }
            for (position, state) in final_cells {
                let transition = ParityDecorationSpill {
                    source,
                    position,
                    state,
                };
                if matches!(spill_capture, SpillCapture::CrossColumn)
                    && (position.0.div_euclid(16), position.2.div_euclid(16)) == (cx, cz)
                {
                    // Direct target output already contains these writes. Keep
                    // the sparse final stream for later target read state, but
                    // never send it through the outward spill ledger.
                    local_features.push(transition);
                } else if seeded.get(&position).copied() != Some(state) {
                    spills.push(transition);
                }
            }
        }
        let block_entities = grid.take_block_entities();
        let world = center_world.map(|mut world| {
            for (x, y, z, state) in grid.dirty_cells() {
                world.set_id(x, y, z, state);
            }
            world
        });
        return_mixed_dispatch_scratch(dispatch_scratch);
        (
            world,
            ParityDecorationResult {
                spills,
                block_entities,
                local_features,
                bridge_sync_events: 0,
                bridge_sync_cells: 0,
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
        biome_quarts: &[(BiomeRef, bool); 16],
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        crate::feature::top_layer::FreezeCounts,
    ) {
        let mut discard_write = |_x: i32, _y: i32, _z: i32, _state: CanonicalStateId| {};
        self.top_layer_stage_with_observer(cx, cz, world, biome_quarts, &mut discard_write)
    }

    fn top_layer_stage_with_observer(
        &self,
        cx: i32,
        cz: i32,
        world: crate::dense_grid::DenseBlockGrid,
        biome_quarts: &[(BiomeRef, bool); 16],
        observer: &mut dyn FnMut(i32, i32, i32, CanonicalStateId),
    ) -> (
        crate::dense_grid::DenseBlockGrid,
        crate::feature::top_layer::FreezeCounts,
    ) {
        let mut world = world;
        let counts = self.apply_top_layer_to_grid(
            cx,
            cz,
            biome_quarts,
            &mut world,
            observer,
        );
        (world, counts)
    }

    fn apply_top_layer_to_grid<G: crate::feature::top_layer::TopLayerGrid>(
        &self,
        cx: i32,
        cz: i32,
        biome_quarts: &[(BiomeRef, bool); 16],
        grid: &mut G,
        observer: &mut dyn FnMut(i32, i32, i32, CanonicalStateId),
    ) -> crate::feature::top_layer::FreezeCounts {
        if self.snow_support.is_empty()
            || self.freeze_biomes.is_empty()
            || self.biome_climates.is_empty()
        {
            return crate::feature::top_layer::FreezeCounts::default();
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
            return crate::feature::top_layer::FreezeCounts::default();
        }
        // `level.getBiome(topPos)` resolves through the quart grid. `biome_stage`
        // samples each quart at its own corner, so a column's quart index is
        // `(lz >> 2) * 4 + (lx >> 2)`.
        let mut climates = self.biome_climates_typed;
        for (climate, &enabled) in climates.iter_mut().zip(self.freeze_biomes_typed.iter()) {
            if !enabled {
                *climate = None;
            }
        }
        let biome_at = |lx: i32, lz: i32| -> lodestone_data::biomes::BuiltinBiome {
            let quart = ((lz >> 2) * 4 + (lx >> 2)) as usize;
            let biome = biome_quarts[quart]
                .0
                .builtin_or_none()
                .expect("extension biome requires its owning registry at the name boundary");
            biome
        };
        crate::feature::top_layer::apply_freeze_top_layer_typed_with_observer_on(
            grid,
            cx,
            cz,
            self.min_y,
            self.height,
            self.sea_level,
            &biome_at,
            &climates,
            &self.snow_support,
            &self.climate_noise,
            observer,
        )
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
// Unit 3 took the allocations (both grids carry canonical `StateId`,
// so a cell copy became a `u16` move) and Unit 6 took the lock traffic,
// but the *copy* survived both, and so did the 884,736-entry `HashMap` it filled.
// `VegGrid::with_sources` deletes it outright: the nine grids are borrowed, and
// `crate::counters::Counters::stitch_cells` — the counter that measured exactly
// this loop and `stitch_region`'s twin — now reads zero for a served column.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        MixedReplayProduct, MixedReplaySourcePlan, context_from_window, make_replay_window,
        replay_dependency_union, unique_target_order,
    };
    use crate::feature::OVERWORLD_SOURCE_OFFSETS;

    fn synthetic_window(targets: &[(i32, i32)]) -> super::MixedReplayWindow {
        let coordinates = replay_dependency_union(targets);
        let products = coordinates
            .into_iter()
            .map(|coordinate| MixedReplayProduct {
                coordinate,
                pre: None,
                heights: [coordinate.0 * 1000 + coordinate.1; 256],
            })
            .collect();
        let sources = targets
            .iter()
            .flat_map(|&(cx, cz)| {
                super::overworld_source_offsets()
                    .iter()
                    .map(move |&(dx, dz)| (cx + dx, cz + dz))
            })
            .collect::<std::collections::BTreeSet<_>>();
        let plans = sources
            .into_iter()
            .map(|source| {
                Arc::new(MixedReplaySourcePlan {
                    source,
                    ores: Arc::new(Vec::new()),
                    entries: Arc::new(Vec::new()),
                })
            })
            .collect();
        Arc::try_unwrap(make_replay_window(
            products,
            plans,
            Arc::new(crate::compose::FeatureBiomePlan::default()),
        ))
        .ok()
        .expect("synthetic window has one owner")
    }

    #[test]
    fn adjacent_contexts_share_one_window_and_fixed_height_products() {
        let window = Arc::new(synthetic_window(&[(0, 0), (1, 0)]));
        let biomes = || Arc::new(crate::overworld::biome_cells::BiomeCells::uniform("minecraft:plains", -64, 384));
        let first = context_from_window(Arc::clone(&window), 0, 0, 2, None, biomes());
        let second = context_from_window(Arc::clone(&window), 1, 0, 2, None, biomes());
        assert!(Arc::ptr_eq(&first.window, &second.window));
        assert_eq!(window.products.len(), 30);
        assert_eq!(window.source_plans.len(), 12);
        assert_eq!(first.ocean_floor_wg.get(16, 0), 1000);
        assert_eq!(second.ocean_floor_wg.get(0, 0), 1000);
    }

    #[test]
    fn far_apart_targets_keep_the_window_sparse_and_bounded() {
        let window = synthetic_window(&[(0, 0), (100, 100)]);
        assert_eq!(window.products.len(), 50);
        assert_eq!(window.source_plans.len(), 18);
        assert!(window.products.len() < 1000);
    }

    #[test]
    fn replay_batch_deduplicates_adjacent_and_negative_targets() {
        let targets = [(-1, 0), (0, 0), (-1, 0)];
        assert_eq!(unique_target_order(&targets), [(-1, 0), (0, 0)]);
        let dependencies = replay_dependency_union(&unique_target_order(&targets));
        assert_eq!(dependencies.len(), 30);
        assert!(dependencies.contains(&(-3, -2)));
        assert!(dependencies.contains(&(2, 2)));
        assert!(!dependencies.contains(&(-4, -2)));
    }

    #[test]
    fn replay_batch_empty_request_retains_no_dependency_products() {
        assert!(replay_dependency_union(&[]).is_empty());
        assert!(unique_target_order(&[]).is_empty());
    }

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
    fn ore_biome_grid_reuses_exact_candidate_lookups() {
        let products = (0..3)
            .flat_map(|z| (0..3).map(move |x| (x, z)))
            .map(|_| {
                Arc::new((
                    Arc::new(crate::dense_grid::DenseBlockGrid::new(
                        0, -64, 0, 16, 384, 16, "minecraft:air",
                    )),
                    [0; 256],
                    std::array::from_fn(|_| {
                        (
                            lodestone_data::biomes::BiomeRef::builtin(
                                lodestone_data::biomes::BuiltinBiome::Plains,
                            ),
                            false,
                        )
                    }),
                    Arc::new(crate::overworld::biome_cells::BiomeCells::uniform(
                        "minecraft:plains",
                        -64,
                        384,
                    )),
                ))
            })
            .collect::<Vec<_>>();
        let grid = super::OreBiomeMembershipGrid::new(
            &[(1, 1)],
            0,
            0,
            3,
            3,
            &products,
            -64,
            384,
            0,
            Arc::new(crate::compose::FeatureBiomePlan::default()),
        );
        let pos = crate::feature::BlockPos { x: 16, y: -64, z: 16 };
        assert!(grid.biome_at(pos).is_some());
        assert_eq!(grid.lookup_counts(), (1, 1));
        assert!(grid.biome_at(pos).is_some());
        assert_eq!(grid.lookup_counts(), (2, 1));
    }

}
