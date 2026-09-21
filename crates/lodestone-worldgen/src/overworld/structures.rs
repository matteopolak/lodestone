//! Stages 0a/0b of [`OverworldGenerator::column`] — `structure_starts` and
//! `structure_refs`, the two stages that run *before* terrain.
//!
//! # Why these are the topmost stages, not the last ones
//!
//! Vanilla's `ChunkStatus` order is `STRUCTURE_STARTS → STRUCTURE_REFERENCES →
//! BIOMES → NOISE → …`, and the reason is the beardifier: `NoiseChunk`'s fill
//! consults the structure bounds intersecting the chunk to flatten terrain
//! underneath them, so the bounds have to exist before a single density sample is
//! taken. This inverts the intuition that structures are placed *on* terrain,
//! and getting it backwards is not a small error — it is the difference between
//! a village on flat ground and a village draped over a hillside.
//!
//! In this engine the fill lives inside
//! [`OverworldGenerator::pre_ore_stage`], so the two stages here sit *above*
//! `pre_ore` in [`super::ChunkStages`] and `pre_ore` gains exactly one upstream
//! edge: it reads its own chunk's [`StructureRefs`]. The store's stage rule ("add
//! a stage above the ones it consumes") is therefore satisfied, and the
//! reentrancy trap is avoided because neither stage here reads any terrain
//! product — [`StartSampler`] samples a *fresh* noise column, exactly as
//! vanilla's `getBaseColumn` does, which is what makes the layering acyclic
//! rather than merely conventional.
//!
//! # What it costs
//!
//! `structure_starts` is ~20 structure-set placement predicates (two to four
//! legacy RNG draws each) and, only on the rare chunk where a placement fires, a
//! handful of column samples. `structure_refs` is at most 289 store probes over
//! the 17×17 neighbourhood (`ChunkStatus`'s STRUCTURE_REFERENCES radius 8), each
//! an `Arc` clone. Neither touches a block, and for a chunk with no
//! adaptation-bearing start in reach `refs` produces an empty list and
//! `pre_ore`'s beardifier context stays the constant-zero leaf it is today —
//! which is why this unit is bit-identical on output.
//!
//! # How to change it
//!
//! * The beardifier itself is **not** here (S3). What is here is the *seam*:
//!   [`StructureRefs`] is the product S3's evaluator consumes, and it already
//!   filters the way vanilla's `Beardifier.forStructuresInChunk` filters
//!   (adaptation `!= NONE`, within 12 blocks of the chunk). Widen that filter and
//!   you widen the halo the join scheduler has to lead by.
//! * [`StartSampler`]'s height scan builds a whole [`AquiferSystem`] per
//!   candidate chunk. That is deliberate — it is the cheapest thing that is
//!   *exactly* vanilla's column — but it means a structure kind that samples many
//!   scattered columns (mineshaft's mesa arm, ruined portals' corner heights)
//!   wants the sampler to cache per chunk, which it does.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[cfg(feature = "gen-counters")]
use std::sync::atomic::{AtomicU64, Ordering};

use lodestone_worldgen_core::rng::{
    RandomSource, WorldgenRandom, XoroshiroRandomSource,
};
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
use lodestone_data::block_states::StateId as CanonicalStateId;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::carver::{mark_touched_column, TouchedMask};
use crate::structure::{
    CodedBlock, HeightmapKind, PieceRefinement, RingProbeCache, StartContext,
    StructureKind, StructureMutationContext, StructureMutationSink, StructureStart,
    VerticalPlacement,
};

use super::OverworldGenerator;

/// One chunk's structure references: every start whose *adjusted* bounding box
/// comes within 12 blocks of this chunk, paired with the chunk that owns it.
///
/// This is both halves of vanilla's `structures.References` (which chunks' starts
/// this chunk participates in) and the beardifier's input
/// (`Beardifier.forStructuresInChunk`). Kept as one product because they are the
/// same walk over the same 17×17 neighbourhood, and computing them separately
/// would be two chances to disagree about the reach.
#[derive(Debug, Default)]
pub struct StructureRefs {
    /// `(owning chunk x, owning chunk z, start)`, in neighbourhood scan order.
    pub entries: Vec<(i32, i32, Arc<StructureStart>)>,
}

impl StructureRefs {
    /// The `References` NBT view: structure id → the packed chunk keys of the
    /// chunks whose starts this chunk references, deduplicated and sorted.
    ///
    /// Vanilla writes a `long[]` per structure id; `ChunkPos.pack` is
    /// `(z as u32 as i64) << 32 | (x as u32 as i64)`.
    #[must_use]
    pub fn packed_by_structure(&self) -> std::collections::BTreeMap<String, Vec<i64>> {
        let mut out: std::collections::BTreeMap<String, Vec<i64>> =
            std::collections::BTreeMap::new();
        for (cx, cz, start) in &self.entries {
            let packed =
                (i64::from(*cz as u32) << 32) | i64::from(*cx as u32);
            let list = out.entry(start.structure.clone()).or_default();
            if !list.contains(&packed) {
                list.push(packed);
            }
        }
        for list in out.values_mut() {
            list.sort_unstable();
        }
        out
    }

    /// The starts the beardifier evaluates: adaptation-bearing, piece-complete,
    /// and in reach.
    ///
    /// **Which structures reach this** is the honest measure of how much of S3 is
    /// observable in a generated world: only the seven adaptation-bearing kinds
    /// with a landed piece generator do. See `docs/worldgen-beardifier.md` for the
    /// current list — while every adaptation-bearing kind is still jigsaw (S4) or
    /// coded (S5), this iterator is empty for every chunk and the fill stage takes
    /// its no-beard branch, which is exactly what the negative control asserts.
    #[must_use]
    pub fn adaptation_bearing(&self) -> impl Iterator<Item = &Arc<StructureStart>> {
        self.entries
            .iter()
            .map(|(_, _, start)| start)
            .filter(|start| {
                start.pieces_complete
                    && start.terrain_adaptation != crate::structure::TerrainAdjustment::None
            })
    }
}

/// A bounded source-window index. Each origin stores the bundled structure-set
/// memberships in one mask, so reference gathering does not retain a tuple for
/// every `(source, set)` candidate or rescan an origin's set list.
#[derive(Debug)]
pub(super) struct StructureOriginIndex {
    min_x: i32,
    min_z: i32,
    width: usize,
    masks: Vec<u32>,
}

impl StructureOriginIndex {
    fn from_candidates(
        min_x: i32,
        max_x: i32,
        min_z: i32,
        max_z: i32,
        candidates: Vec<((i32, i32), usize)>,
    ) -> Result<Self, Vec<((i32, i32), usize)>> {
        if candidates
            .iter()
            .any(|(_, set_index)| *set_index >= u32::BITS as usize)
        {
            return Err(candidates);
        }
        let width = usize::try_from(max_x - min_x + 1).map_err(|_| Vec::new())?;
        let depth = usize::try_from(max_z - min_z + 1).map_err(|_| Vec::new())?;
        let Some(mut masks) = width.checked_mul(depth).map(|size| vec![0; size]) else {
            return Err(Vec::new());
        };
        for ((x, z), set_index) in candidates {
            let Ok(local_x) = usize::try_from(x - min_x) else {
                return Err(Vec::new());
            };
            let Ok(local_z) = usize::try_from(z - min_z) else {
                return Err(Vec::new());
            };
            let Some(slot) = local_z
                .checked_mul(width)
                .and_then(|slot| slot.checked_add(local_x))
            else {
                return Err(Vec::new());
            };
            masks[slot] |= 1u32 << set_index;
        }
        Ok(Self {
            min_x,
            min_z,
            width,
            masks,
        })
    }

    #[inline]
    fn mask_at(&self, x: i32, z: i32) -> u32 {
        let local_x = usize::try_from(x - self.min_x).expect("origin index X escaped window");
        let local_z = usize::try_from(z - self.min_z).expect("origin index Z escaped window");
        self.masks[local_z * self.width + local_x]
    }

    fn for_each_source(
        &self,
        min_x: i32,
        max_x: i32,
        min_z: i32,
        max_z: i32,
        mut visit: impl FnMut(i32, i32, &[usize]),
    ) {
        let mut set_indices = [0usize; u32::BITS as usize];
        for x in min_x..=max_x {
            for z in min_z..=max_z {
                let mut mask = self.mask_at(x, z);
                let mut count = 0;
                while mask != 0 {
                    let set_index = mask.trailing_zeros() as usize;
                    set_indices[count] = set_index;
                    count += 1;
                    mask &= mask - 1;
                }
                if count != 0 {
                    visit(x, z, &set_indices[..count]);
                }
            }
        }
    }
}

/// Orders target-chunk structure starts the way the decoration lifecycle
/// consumes them: generation step first, then the complete structure-registry
/// position within that step. The reference walk itself is source-chunk-first,
/// which is the right order for building the persisted reference sets but not
/// for placing overlapping structures. A stable sort keeps the source walk's
/// order among starts of one structure, matching the ordered reference set
/// iterator used by the placement pass.
fn order_structure_entries<'a>(
    registry: &crate::structure::StructureRegistry,
    entries: &mut Vec<&'a (i32, i32, Arc<StructureStart>)>,
) {
    entries.sort_by_key(|(_, _, start)| {
        registry
            .feature_placement_key(&start.structure)
            .unwrap_or((i32::MAX, usize::MAX))
    });
}

struct TouchedColumnSink<'a> {
    mask: &'a mut TouchedMask,
    chunk_x: i32,
    chunk_z: i32,
}

struct NoopStructureMutationSink;

impl StructureMutationSink for NoopStructureMutationSink {
    fn record_structure_mutation(
        &mut self,
        _source: (i32, i32),
        _step: i32,
        _position: [i32; 3],
        _state: CanonicalStateId,
    ) {
    }
}

impl StructureMutationSink for TouchedColumnSink<'_> {
    fn record_structure_mutation(
        &mut self,
        _source: (i32, i32),
        _step: i32,
        position: [i32; 3],
        _state: CanonicalStateId,
    ) {
        mark_touched_column(self.mask, self.chunk_x, self.chunk_z, position[0], position[2]);
    }
}

/// Writes a coded piece's ordered block list into the receiving chunk.
///
/// Chest facing is the one state that depends on the receiving grid rather than
/// on the eager start-time list. Reorienting immediately before the chest write
/// preserves the coded walk's last-write-wins order and leaves its loot vector —
/// including every already-spent roll seed — untouched.
#[cfg(test)]
fn place_coded_blocks(
    world: &mut crate::dense_grid::DenseBlockGrid,
    blocks: &[CodedBlock],
    solid_render: &dyn Fn(CanonicalStateId) -> bool,
) {
    place_coded_blocks_with_sink(world, blocks, solid_render, None);
}

fn place_coded_blocks_with_sink(
    world: &mut crate::dense_grid::DenseBlockGrid,
    blocks: &[CodedBlock],
    solid_render: &dyn Fn(CanonicalStateId) -> bool,
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    for block in blocks {
        if block.state.block() == Block::Chest {
            let state = crate::structure::fortress::chest_state(world, block.pos, solid_render);
            if let Some(mutation) = mutation.as_deref_mut() {
                mutation.write(world, block.pos[0], block.pos[1], block.pos[2], state);
            } else {
                world.set_id(block.pos[0], block.pos[1], block.pos[2], state);
            }
        } else {
            if let Some(mutation) = mutation.as_deref_mut() {
                mutation.write(world, block.pos[0], block.pos[1], block.pos[2], block.state);
            } else {
                world.set_id(block.pos[0], block.pos[1], block.pos[2], block.state);
            }
        }
    }
}

/// Chebyshev chunk radius `structure_refs` reads `structure_starts` over —
/// vanilla's `ChunkGenerator.createReferences`' hardcoded `int range = 8`, i.e.
/// a 17×17 neighbourhood.
pub const REFS_RADIUS: i32 = 8;

/// The reach `Beardifier.forStructuresInChunk` keeps a start at
/// (`isCloseToChunk(chunkPos, 12)`), and the amount
/// `Structure.adjustBoundingBox` inflates an adaptation-bearing box by. Same
/// number twice in vanilla, and it is the same number for the same reason.
pub const BEARD_REACH: i32 = 12;

/// How far a ruined portal's post-template terrain pass can write beyond its
/// frame. Structure references normally need only the piece box (plus the
/// beardifier halo), but this pass must also reach neighbouring grids so each
/// can regenerate and clip its portion of the skirt.
const PORTAL_TERRAIN_REACH: i32 = 14;

/// [`StartContext`] over freshly sampled noise columns.
///
/// Holds no terrain product and reads no store slot, which is what keeps
/// `structure_starts` above `pre_ore` instead of circular. The per-chunk
/// [`AquiferSystem`] cache exists because building one is the expensive part and
/// a structure predicate asks about several columns of the same chunk.
pub(super) struct StartSampler<'a> {
    generator: &'a OverworldGenerator,
    /// A bounded request-local aquifer cache. Region-prefix source walks keep
    /// one sampler across all targets; scalar calls still own one sampler per
    /// reference computation. `RefCell` because
    /// [`StartContext`] takes `&self` (it is called from a `&dyn` behind the
    /// registry) and this is single-threaded per stage invocation. Structure
    /// starts normally touch one or two chunks; eviction is an exact rebuild,
    /// so a fixed array avoids a heap table on every cold start sampler.
    aquifers: RefCell<AquiferCache>,
    /// The immediately repeated pre-surface predicate result. A one-entry
    /// cache covers adjacent checks without retaining a terrain region.
    block_kind: Cell<Option<((i32, i32, i32), BlockKind)>>,
    /// Compact request-local height probes. Each entry remembers both `_WG`
    /// heightmaps and the point at which its downward walk stopped, so asking
    /// for the other map resumes the same walk instead of rereading the upper
    /// half of the column. A bounded array keeps this cold path from creating
    /// one heap entry per generation-point query; eviction only repeats an
    /// exact probe and cannot change its answer.
    heights: RefCell<HeightProbeCache>,
    /// The biome tree's last-result candidate for the current structure
    /// placement lifecycle. Ring relocation probes thousands of adjacent
    /// quart cells; retaining the candidate matches the reference search
    /// lifecycle and avoids restarting the tree from its root for every probe.
    biome_cursor: RefCell<Option<crate::biome::BiomeSearchCursor>>,
}

const HEIGHT_PROBE_CACHE_CAPACITY: usize = 256;
const AQUIFER_CACHE_CAPACITY: usize = 512;

#[cfg(feature = "gen-counters")]
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureCacheStats {
    pub sampler_constructions: u64,
    pub height_lookups: u64,
    pub height_hits: u64,
    pub height_misses: u64,
    pub height_evictions: u64,
    pub aquifer_lookups: u64,
    pub aquifer_hits: u64,
    pub aquifer_misses: u64,
    pub aquifer_evictions: u64,
    pub aquifer_rebuilds: u64,
}

#[cfg(feature = "gen-counters")]
struct StructureCacheCounters {
    sampler_constructions: AtomicU64,
    height_lookups: AtomicU64,
    height_hits: AtomicU64,
    height_misses: AtomicU64,
    height_evictions: AtomicU64,
    aquifer_lookups: AtomicU64,
    aquifer_hits: AtomicU64,
    aquifer_misses: AtomicU64,
    aquifer_evictions: AtomicU64,
    aquifer_rebuilds: AtomicU64,
}

#[cfg(feature = "gen-counters")]
static STRUCTURE_CACHE_COUNTERS: StructureCacheCounters = StructureCacheCounters {
    sampler_constructions: AtomicU64::new(0),
    height_lookups: AtomicU64::new(0),
    height_hits: AtomicU64::new(0),
    height_misses: AtomicU64::new(0),
    height_evictions: AtomicU64::new(0),
    aquifer_lookups: AtomicU64::new(0),
    aquifer_hits: AtomicU64::new(0),
    aquifer_misses: AtomicU64::new(0),
    aquifer_evictions: AtomicU64::new(0),
    aquifer_rebuilds: AtomicU64::new(0),
};

#[cfg(feature = "gen-counters")]
pub fn reset_structure_cache_stats() {
    let counters = &STRUCTURE_CACHE_COUNTERS;
    for counter in [
        &counters.sampler_constructions,
        &counters.height_lookups,
        &counters.height_hits,
        &counters.height_misses,
        &counters.height_evictions,
        &counters.aquifer_lookups,
        &counters.aquifer_hits,
        &counters.aquifer_misses,
        &counters.aquifer_evictions,
        &counters.aquifer_rebuilds,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

#[cfg(feature = "gen-counters")]
#[must_use]
pub fn structure_cache_stats() -> StructureCacheStats {
    let counters = &STRUCTURE_CACHE_COUNTERS;
    StructureCacheStats {
        sampler_constructions: counters.sampler_constructions.load(Ordering::Relaxed),
        height_lookups: counters.height_lookups.load(Ordering::Relaxed),
        height_hits: counters.height_hits.load(Ordering::Relaxed),
        height_misses: counters.height_misses.load(Ordering::Relaxed),
        height_evictions: counters.height_evictions.load(Ordering::Relaxed),
        aquifer_lookups: counters.aquifer_lookups.load(Ordering::Relaxed),
        aquifer_hits: counters.aquifer_hits.load(Ordering::Relaxed),
        aquifer_misses: counters.aquifer_misses.load(Ordering::Relaxed),
        aquifer_evictions: counters.aquifer_evictions.load(Ordering::Relaxed),
        aquifer_rebuilds: counters.aquifer_rebuilds.load(Ordering::Relaxed),
    }
}

struct AquiferCache {
    entries: [Option<(i32, i32, Arc<AquiferSystem>)>; AQUIFER_CACHE_CAPACITY],
    replacement: usize,
}

impl Default for AquiferCache {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
            replacement: 0,
        }
    }
}

impl AquiferCache {
    #[inline]
    fn hash(cx: i32, cz: i32) -> usize {
        let x = (cx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let z = (cz as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        (x ^ z.rotate_left(32)) as usize & (AQUIFER_CACHE_CAPACITY - 1)
    }

    fn get(&self, cx: i32, cz: i32) -> Option<Arc<AquiferSystem>> {
        #[cfg(feature = "gen-counters")]
        STRUCTURE_CACHE_COUNTERS
            .aquifer_lookups
            .fetch_add(1, Ordering::Relaxed);
        let mut index = Self::hash(cx, cz);
        for _ in 0..AQUIFER_CACHE_CAPACITY {
            let Some((entry_x, entry_z, aquifer)) = self.entries[index].as_ref() else {
                return None;
            };
            if *entry_x == cx && *entry_z == cz {
                #[cfg(feature = "gen-counters")]
                STRUCTURE_CACHE_COUNTERS
                    .aquifer_hits
                    .fetch_add(1, Ordering::Relaxed);
                return Some(Arc::clone(aquifer));
            }
            index = (index + 1) & (AQUIFER_CACHE_CAPACITY - 1);
        }
        None
    }

    fn insert(&mut self, cx: i32, cz: i32, aquifer: Arc<AquiferSystem>) {
        let mut index = Self::hash(cx, cz);
        for _ in 0..AQUIFER_CACHE_CAPACITY {
            if self.entries[index].is_none() {
                self.entries[index] = Some((cx, cz, aquifer));
                return;
            }
            index = (index + 1) & (AQUIFER_CACHE_CAPACITY - 1);
        }
        index = self.replacement;
        self.replacement = (index + 1) & (AQUIFER_CACHE_CAPACITY - 1);
        #[cfg(feature = "gen-counters")]
        STRUCTURE_CACHE_COUNTERS
            .aquifer_evictions
            .fetch_add(1, Ordering::Relaxed);
        self.entries[index] = Some((cx, cz, aquifer));
    }
}

/// No valid overworld Y can equal this value, so the probe's cursor doubles as
/// its occupancy bit. Keeping the two answers as sentinelled `i32`s makes the
/// entry five words instead of two `Option<i32>` values plus padding; this table
/// is present in every request-local sampler and its size is part of the bounded
/// memory budget.
const EMPTY_HEIGHT_PROBE_Y: i32 = i32::MIN;

#[derive(Clone, Copy, Debug)]
struct HeightProbeEntry {
    x: i32,
    z: i32,
    next_y: i32,
    world_surface: i32,
    ocean_floor: i32,
}

impl Default for HeightProbeEntry {
    fn default() -> Self {
        Self {
            x: 0,
            z: 0,
            next_y: EMPTY_HEIGHT_PROBE_Y,
            world_surface: EMPTY_HEIGHT_PROBE_Y,
            ocean_floor: EMPTY_HEIGHT_PROBE_Y,
        }
    }
}

impl HeightProbeEntry {
    #[inline]
    fn occupied(self) -> bool {
        self.next_y != EMPTY_HEIGHT_PROBE_Y
    }
}

#[derive(Debug)]
struct HeightProbeCache {
    entries: [HeightProbeEntry; HEIGHT_PROBE_CACHE_CAPACITY],
    replacement: usize,
}

impl Default for HeightProbeCache {
    fn default() -> Self {
        Self {
            entries: [HeightProbeEntry::default(); HEIGHT_PROBE_CACHE_CAPACITY],
            replacement: 0,
        }
    }
}

impl HeightProbeCache {
    #[inline]
    fn hash(x: i32, z: i32) -> usize {
        let x = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let z = (z as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        (x ^ z.rotate_left(32)) as usize & (HEIGHT_PROBE_CACHE_CAPACITY - 1)
    }

    fn find(&self, x: i32, z: i32) -> Option<usize> {
        let mut index = Self::hash(x, z);
        for _ in 0..HEIGHT_PROBE_CACHE_CAPACITY {
            let entry = self.entries[index];
            if !entry.occupied() {
                return None;
            }
            if entry.x == x && entry.z == z {
                return Some(index);
            }
            index = (index + 1) & (HEIGHT_PROBE_CACHE_CAPACITY - 1);
        }
        None
    }

    fn entry_mut(&mut self, x: i32, z: i32, initial_y: i32) -> &mut HeightProbeEntry {
        let index = self.find(x, z).unwrap_or_else(|| {
            let mut index = Self::hash(x, z);
            for _ in 0..HEIGHT_PROBE_CACHE_CAPACITY {
                if !self.entries[index].occupied() {
                    self.entries[index] = HeightProbeEntry {
                        x,
                        z,
                        next_y: initial_y,
                        world_surface: EMPTY_HEIGHT_PROBE_Y,
                        ocean_floor: EMPTY_HEIGHT_PROBE_Y,
                    };
                    return index;
                }
                index = (index + 1) & (HEIGHT_PROBE_CACHE_CAPACITY - 1);
            }
            let index = self.replacement;
            self.replacement = (index + 1) & (HEIGHT_PROBE_CACHE_CAPACITY - 1);
            #[cfg(feature = "gen-counters")]
            if self.entries[index].occupied() {
                STRUCTURE_CACHE_COUNTERS
                    .height_evictions
                    .fetch_add(1, Ordering::Relaxed);
            }
            self.entries[index] = HeightProbeEntry {
                x,
                z,
                next_y: initial_y,
                world_surface: EMPTY_HEIGHT_PROBE_Y,
                ocean_floor: EMPTY_HEIGHT_PROBE_Y,
            };
            index
        });
        &mut self.entries[index]
    }
}

impl StartSampler<'_> {
    pub(super) fn new(generator: &OverworldGenerator) -> StartSampler<'_> {
        #[cfg(feature = "gen-counters")]
        STRUCTURE_CACHE_COUNTERS
            .sampler_constructions
            .fetch_add(1, Ordering::Relaxed);
        StartSampler {
            generator,
            aquifers: RefCell::new(AquiferCache::default()),
            block_kind: Cell::new(None),
            heights: RefCell::new(HeightProbeCache::default()),
            biome_cursor: RefCell::new(
                generator.biome_search_cursor(),
            ),
        }
    }

    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(cx, cz) {
            return existing;
        }
        #[cfg(feature = "gen-counters")]
        {
            STRUCTURE_CACHE_COUNTERS
                .aquifer_misses
                .fetch_add(1, Ordering::Relaxed);
            STRUCTURE_CACHE_COUNTERS
                .aquifer_rebuilds
                .fetch_add(1, Ordering::Relaxed);
        }
        // Counted separately from the fill path's aquifers: `build_aquifer` bumps
        // `stage_entered[Aquifer]`, which the calibration bench predicts as the
        // pre-ore closure size, and this one is not part of that closure.
        crate::counters::bump_structure_aquifer();
        let built = Arc::new(self.generator.build_aquifer(cx, cz));
        self.aquifers.borrow_mut().insert(cx, cz, Arc::clone(&built));
        built
    }

    fn cached_block_kind_at(&self, x: i32, y: i32, z: i32) -> (BlockKind, bool) {
        let position = (x, y, z);
        if let Some((cached_position, kind)) = self.block_kind.get()
            && cached_position == position
        {
            return (kind, true);
        }
        let kind = self.aquifer(x >> 4, z >> 4).block_at(x, y, z);
        self.block_kind.set(Some((position, kind)));
        (kind, false)
    }
}

impl StartContext for StartSampler<'_> {
    /// `NoiseBasedChunkGenerator.getFirstOccupiedHeight` — `getBaseHeight - 1`,
    /// i.e. the Y of the topmost block satisfying the heightmap predicate.
    ///
    /// Vanilla scans a 1-cell `NoiseChunk` from the top down and returns
    /// `posY + 1` for the first match, `minY` for none; the `-1` in
    /// `getFirstOccupiedHeight` cancels the `+1`, so the answer is the matching
    /// block's own Y (and `minY - 1` when the column never matches).
    ///
    /// Interpolation cells are 4 blocks wide and globally aligned, so reading
    /// this out of the *chunk*-wide aquifer gives the same value as vanilla's
    /// cell-wide one. The heightmap predicates come from `Heightmap.Types`:
    /// `WORLD_SURFACE_WG` is `NOT_AIR`, `OCEAN_FLOOR_WG` is
    /// `blocksMotion` — which for the fill's four-way [`BlockKind`] means
    /// "stone", water and lava explicitly excluded.
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        let generator = self.generator;
        let aquifer = self.aquifer(x >> 4, z >> 4);
        let min_y = generator.min_y();
        let initial_y = min_y + generator.height() - 1;
        let mut heights = self.heights.borrow_mut();
        let entry = heights.entry_mut(x, z, initial_y);
        #[cfg(feature = "gen-counters")]
        STRUCTURE_CACHE_COUNTERS
            .height_lookups
            .fetch_add(1, Ordering::Relaxed);
        let cached = match heightmap {
            HeightmapKind::WorldSurfaceWg => entry.world_surface,
            HeightmapKind::OceanFloorWg => entry.ocean_floor,
        };
        if cached != EMPTY_HEIGHT_PROBE_Y {
            #[cfg(feature = "gen-counters")]
            STRUCTURE_CACHE_COUNTERS
                .height_hits
                .fetch_add(1, Ordering::Relaxed);
            return cached;
        }
        #[cfg(feature = "gen-counters")]
        STRUCTURE_CACHE_COUNTERS
            .height_misses
            .fetch_add(1, Ordering::Relaxed);

        // Keep the walk resumable. The first map requested may terminate at a
        // shallow surface, while the other map still needs the lower part of the
        // same column. Recording the cursor and both answers avoids rescanning
        // the already-consumed cells without retaining their block values.
        let mut queries = 0u64;
        while entry.next_y >= min_y {
            let y = entry.next_y;
            entry.next_y -= 1;
            queries += 1;
            let kind = aquifer.block_at(x, y, z);
            if entry.world_surface == EMPTY_HEIGHT_PROBE_Y && kind != BlockKind::Air {
                entry.world_surface = y;
            }
            if entry.ocean_floor == EMPTY_HEIGHT_PROBE_Y && kind == BlockKind::Stone {
                entry.ocean_floor = y;
            }
            let answer = match heightmap {
                HeightmapKind::WorldSurfaceWg => entry.world_surface,
                HeightmapKind::OceanFloorWg => entry.ocean_floor,
            };
            if answer != EMPTY_HEIGHT_PROBE_Y {
                crate::counters::bump_structure_height_probe(queries);
                return answer;
            }
        }
        crate::counters::bump_structure_height_probe(queries);
        let answer = min_y - 1;
        match heightmap {
            HeightmapKind::WorldSurfaceWg => entry.world_surface = answer,
            HeightmapKind::OceanFloorWg => entry.ocean_floor = answer,
        }
        answer
    }

    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        self.generator.biome_at_quart(qx, qy, qz)
    }

    fn biome_in_set_at_quart(
        &self,
        qx: i32,
        qy: i32,
        qz: i32,
        allowed: &HashSet<String>,
    ) -> bool {
        self.generator.biome_in_set_at_quart(
            qx,
            qy,
            qz,
            allowed,
            &mut self.biome_cursor.borrow_mut(),
        )
    }

    fn biome_in_set_at_quart_cached(
        &self,
        qx: i32,
        qy: i32,
        qz: i32,
        allowed: &HashSet<String>,
        cache: &mut RingProbeCache,
    ) -> bool {
        self.generator.biome_in_set_at_quart_cached(
            qx,
            qy,
            qz,
            allowed,
            cache,
            &mut self.biome_cursor.borrow_mut(),
        )
    }

    fn ring_probe_targets(&self, quart_cells: &[(i32, i32, i32)]) -> Option<Vec<[i64; 7]>> {
        self.generator.ring_probe_targets(quart_cells)
    }

    fn supports_ring_probe_batch(&self) -> bool {
        self.generator.has_dynamic_biome()
    }

    fn ring_positions_cache_key(&self) -> Option<u64> {
        Some(self.generator.ring_positions_cache_key)
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level()
    }

    /// The real dimension bounds, so a jigsaw structure's `above_bottom` /
    /// `below_top` start height and its `dimension_padding` resolve against this
    /// world rather than against the trait's overworld default.
    fn min_y(&self) -> i32 {
        self.generator.min_y()
    }

    fn dimension_height(&self) -> i32 {
        self.generator.height()
    }

    /// `isReplaceableByStructures`: air or fluid. Read out of the same per-chunk
    /// [`AquiferSystem`] the height probe uses, so a coded piece's foundation walk
    /// costs no extra aquifer build.
    fn is_replaceable_at(&self, x: i32, y: i32, z: i32) -> bool {
        let (kind, cached) = self.cached_block_kind_at(x, y, z);
        if !cached {
            crate::counters::bump_structure_context_replaceable_block_at();
        }
        kind != BlockKind::Stone
    }

    /// The four-way fill kind itself, for the predicates that must separate water
    /// from lava from air. Same cached aquifer, so a mineshaft's liquid survey adds
    /// [`AquiferSystem::block_at`] calls but no aquifer builds beyond the chunks it
    /// already spans.
    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        let (kind, cached) = self.cached_block_kind_at(x, y, z);
        if !cached {
            crate::counters::bump_structure_context_kind_block_at();
        }
        kind
    }
}

/// `BuriedTreasurePieces.BuriedTreasurePiece.postProcess` — walk a cursor down
/// from the ocean-floor height at `(origin.x, origin.z)` until the block
/// *below* it is one of the five stone-family materials, fill the walk
/// position's six air/liquid neighbours (stone-family straight down, the
/// pre-walk block or sand everywhere else) and place an empty chest.
///
/// Runs against the **real** per-chunk grid at placement time (see
/// [`crate::structure::PieceRefinement::BuriedTreasureChest`]'s own doc for
/// why this cannot be an eager, start-time list like every other coded piece).
/// Draws no random: vanilla's own `random` argument is spent only inside
/// `createChest` on the loot-table roll seed, which is out of scope here the
/// same way every other structure's container loot is (see the
/// `template:block_entity_nbt`/`coded:chests` ledger rows) — the **block** is
/// what this places.
#[cfg(test)]
fn place_buried_treasure_chest(world: &mut crate::dense_grid::DenseBlockGrid, origin: [i32; 3]) {
    place_buried_treasure_chest_with_sink(world, origin, None);
}

fn place_buried_treasure_chest_with_sink(
    world: &mut crate::dense_grid::DenseBlockGrid,
    origin: [i32; 3],
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    let (min_x, min_y, _min_z, size_x, size_y, _size_z) = world.bounds();
    let (x, z) = (origin[0], origin[2]);
    if x < min_x || x >= min_x + size_x {
        // The piece's own column is always inside its origin chunk
        // (`chunkBlockX(9)`), so this never fires in practice — a defensive
        // bound rather than a reachable one.
        return;
    }
    let top = min_y + size_y - 1;
    // `level.getHeight(OCEAN_FLOOR_WG, x, z)`: one above the topmost block that
    // is neither air nor a fluid, scanned against the *real* grid — sand,
    // sandstone and every surface-rule product are visible here, unlike at
    // structure-start time.
    let Some(ground) = (min_y..=top).rev().find(|&y| {
        !is_air_or_liquid_id(world.get_id(x, y, z))
    }) else {
        return;
    };
    let mut y = ground + 1;
    while y > min_y {
        let below = world.get_id(x, y - 1, z);
        if is_stone_family_id(below) {
            let current = world.get_id(x, y, z);
            let soft = if !is_air_or_liquid_id(current) {
                current
            } else {
                Block::Sand.default_state()
            };
            const NEIGHBOURS: [[i32; 3]; 6] = [
                [0, -1, 0],
                [0, 1, 0],
                [0, 0, -1],
                [0, 0, 1],
                [-1, 0, 0],
                [1, 0, 0],
            ];
            for delta in NEIGHBOURS {
                let rel = [x + delta[0], y + delta[1], z + delta[2]];
                if !is_air_or_liquid_id(world.get_id(rel[0], rel[1], rel[2])) {
                    continue;
                }
                let below_rel = world.get_id(rel[0], rel[1] - 1, rel[2]);
                let is_up = delta == [0, 1, 0];
                if is_air_or_liquid_id(below_rel) && !is_up {
                    if let Some(mutation) = mutation.as_deref_mut() {
                        mutation.write(world, rel[0], rel[1], rel[2], below);
                    } else {
                        world.set_id(rel[0], rel[1], rel[2], below);
                    }
                } else {
                    if let Some(mutation) = mutation.as_deref_mut() {
                        mutation.write(world, rel[0], rel[1], rel[2], soft);
                    } else {
                        world.set_id(rel[0], rel[1], rel[2], soft);
                    }
                }
            }
            // The four neighbours are now solid by construction (each was either
            // already solid or just filled), so the receiving-grid reorientation
            // fallback lands on north here.
            if let Some(mutation) = mutation.as_deref_mut() {
                mutation.write(
                    world,
                    x,
                    y,
                    z,
                    chest_north_state(),
                );
            } else {
                world.set_id(x, y, z, chest_north_state());
            }
            return;
        }
        y -= 1;
    }
}

#[cfg(test)]
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn state_with_properties(
    block: Block,
    entries: &[(&str, &str)],
) -> CanonicalStateId {
    let properties = entries.iter().fold(
        Properties::from_state_id(block.default_state()),
        |properties, &(key, value)| {
            properties
                .with_builtin(
                    PropertyKey::from_name(key).expect("generated property key"),
                    BuiltinPropertyValue::from_name(value).expect("generated property value"),
                )
                .expect("generated block-state property is valid")
        },
    );
    Properties::state_for_block(block, &properties).expect("generated block-state properties")
}

#[inline]
fn chest_north_state() -> CanonicalStateId {
    state_with_properties(
        Block::Chest,
        &[("facing", "north"), ("type", "single"), ("waterlogged", "false")],
    )
}

#[cfg(test)]
fn is_air_or_liquid(state: &str) -> bool {
    let name = base_name(state);
    name == "minecraft:air" || name == "minecraft:water" || name == "minecraft:lava"
}

fn is_air_or_liquid_id(state: CanonicalStateId) -> bool {
    matches!(state.block(), Block::Air | Block::Water | Block::Lava)
}

/// `belowState.is(SANDSTONE) || .is(STONE) || .is(ANDESITE) || .is(GRANITE) ||
/// .is(DIORITE)`.
#[cfg(test)]
fn is_stone_family(name: &str) -> bool {
    matches!(
        name,
        "minecraft:sandstone"
            | "minecraft:stone"
            | "minecraft:andesite"
            | "minecraft:granite"
            | "minecraft:diorite"
    )
}

fn is_stone_family_id(state: CanonicalStateId) -> bool {
    matches!(
        state.block(),
        Block::Sandstone | Block::Stone | Block::Andesite | Block::Granite | Block::Diorite
    )
}

/// The ruined-portal post-template pass: terrain growth, downward columns and
/// optional overgrowth. It runs against a fully surfaced chunk, after the
/// template itself wrote its frame.
///
/// The reference receives the decorating chunk's mutable `surface_structures`
/// stream. The caller resets that stream once for each portal registry entry and
/// shares it across the starts of that entry, so this pass consumes the same
/// sequence as the surrounding structure lifecycle while the grid still clips
/// writes to the receiving chunk.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn place_ruined_portal_terrain<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    box_: crate::structure::BoundingBox,
    random: &mut R,
    placement: VerticalPlacement,
    cold: bool,
    overgrown: bool,
    vines: bool,
    features_cannot_replace: &std::collections::HashSet<String>,
) {
    place_ruined_portal_terrain_with_sink(
        world,
        box_,
        random,
        placement,
        cold,
        overgrown,
        vines,
        features_cannot_replace,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn place_ruined_portal_terrain_with_sink<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    box_: crate::structure::BoundingBox,
    random: &mut R,
    placement: VerticalPlacement,
    cold: bool,
    overgrown: bool,
    vines: bool,
    features_cannot_replace: &std::collections::HashSet<String>,
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    let centre = [
        box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2,
        box_.min[1] + (box_.max[1] - box_.min[1] + 1) / 2,
        box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2,
    ];
    let average_width = (box_.max[0] - box_.min[0] + 1 + box_.max[2] - box_.min[2] + 1) / 2;
    let distance_adjustment = random.next_int_bounded((8 - average_width / 2).max(1));
    const CHANCE_BY_DISTANCE: [f32; 14] = [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
    ];
    let follows_surface = matches!(placement, VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor);
    for x in (centre[0] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[0] + CHANCE_BY_DISTANCE.len() as i32) {
        for z in (centre[2] - CHANCE_BY_DISTANCE.len() as i32)..=(centre[2] + CHANCE_BY_DISTANCE.len() as i32) {
            let distance = (x - centre[0]).abs() + (z - centre[2]).abs();
            let adjusted = (distance + distance_adjustment).max(0) as usize;
            let Some(&chance) = CHANCE_BY_DISTANCE.get(adjusted) else {
                continue;
            };
            if random.next_double() >= f64::from(chance) {
                continue;
            }
            let Some(surface) = portal_surface_y(world, x, z, placement) else {
                continue;
            };
            let y = if follows_surface { surface } else { box_.min[1].min(surface) };
            if (y - box_.min[1]).abs() > 3
                || !portal_replaceable_id(
                    world.get_id(x, y, z),
                    placement,
                    features_cannot_replace,
                )
            {
                continue;
            }
            place_portal_netherrack_or_magma(world, random, [x, y, z], cold, mutation.as_deref_mut());
            if overgrown {
                maybe_add_portal_leaves(world, random, [x, y, z], mutation.as_deref_mut());
            }
            add_portal_drip_column(world, random, [x, y - 1, z], cold, mutation.as_deref_mut());
        }
    }
    for x in (box_.min[0] + 1)..box_.max[0] {
        for z in (box_.min[2] + 1)..box_.max[2] {
            if world.get_base_id(x, box_.min[1], z).block() == Block::Netherrack {
                add_portal_drip_column(
                    world,
                    random,
                    [x, box_.min[1] - 1, z],
                    cold,
                    mutation.as_deref_mut(),
                );
            }
        }
    }
    if vines || overgrown {
        for x in box_.min[0]..=box_.max[0] {
            for y in box_.min[1]..=box_.max[1] {
                for z in box_.min[2]..=box_.max[2] {
                    if vines {
                        maybe_add_portal_vine(world, random, [x, y, z], mutation.as_deref_mut());
                    }
                    if overgrown {
                        maybe_add_portal_leaves(world, random, [x, y, z], mutation.as_deref_mut());
                    }
                }
            }
        }
    }
}

fn portal_surface_y(
    world: &crate::dense_grid::DenseBlockGrid,
    x: i32,
    z: i32,
    placement: VerticalPlacement,
) -> Option<i32> {
    let (_, min_y, _, _, size_y, _) = world.bounds();
    let ocean_floor = placement == VerticalPlacement::OnOceanFloor;
    (min_y..(min_y + size_y)).rev().find(|&y| {
        if ocean_floor {
            !is_air_or_liquid_id(world.get_id(x, y, z))
        } else {
            world.get_base_id(x, y, z).block() != Block::Air
        }
    })
}

fn portal_replaceable_id(
    state: CanonicalStateId,
    placement: VerticalPlacement,
    features_cannot_replace: &std::collections::HashSet<String>,
) -> bool {
    let block = state.block();
    block != Block::Air
        && block != Block::Obsidian
        && !features_cannot_replace.contains(block.name())
        && (placement == VerticalPlacement::InNether || block != Block::Lava)
}

fn place_portal_netherrack_or_magma<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    random: &mut R,
    pos: [i32; 3],
    cold: bool,
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    let state = if !cold && random.next_float() < 0.07 {
        Block::MagmaBlock.default_state()
    } else {
        Block::Netherrack.default_state()
    };
    if let Some(mutation) = mutation.as_deref_mut() {
        mutation.write(world, pos[0], pos[1], pos[2], state);
    } else {
        world.set_id(pos[0], pos[1], pos[2], state);
    }
}

fn add_portal_drip_column<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    random: &mut R,
    mut pos: [i32; 3],
    cold: bool,
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    place_portal_netherrack_or_magma(world, random, pos, cold, mutation.as_deref_mut());
    for _ in 0..8 {
        if random.next_float() >= 0.5 {
            break;
        }
        pos[1] -= 1;
        place_portal_netherrack_or_magma(world, random, pos, cold, mutation.as_deref_mut());
    }
}

fn maybe_add_portal_leaves<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    random: &mut R,
    pos: [i32; 3],
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    if random.next_float() < 0.5
        && world.get_base_id(pos[0], pos[1], pos[2]).block() == Block::Netherrack
        && world.get_base_id(pos[0], pos[1] + 1, pos[2]).block() == Block::Air
    {
        let state = state_with_properties(
            Block::JungleLeaves,
            &[("distance", "7"), ("persistent", "true"), ("waterlogged", "false")],
        );
        if let Some(mutation) = mutation.as_deref_mut() {
            mutation.write(world, pos[0], pos[1] + 1, pos[2], state);
        } else {
            world.set_id(pos[0], pos[1] + 1, pos[2], state);
        }
    }
}

fn maybe_add_portal_vine<R: RandomSource>(
    world: &mut crate::dense_grid::DenseBlockGrid,
    random: &mut R,
    pos: [i32; 3],
    mut mutation: Option<&mut StructureMutationContext<'_>>,
) {
    let state = world.get_base_id(pos[0], pos[1], pos[2]).block();
    if matches!(state, Block::Air | Block::Water | Block::Lava | Block::Vine) {
        return;
    }
    let (dx, dz, vine) = match random.next_int_bounded(4) {
        0 => (
            0,
            -1,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "false"), ("south", "true"), ("up", "false"), ("west", "false")],
            ),
        ),
        1 => (
            1,
            0,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "false"), ("south", "false"), ("up", "false"), ("west", "true")],
            ),
        ),
        2 => (
            0,
            1,
            state_with_properties(
                Block::Vine,
                &[("east", "false"), ("north", "true"), ("south", "false"), ("up", "false"), ("west", "false")],
            ),
        ),
        _ => (
            -1,
            0,
            state_with_properties(
                Block::Vine,
                &[("east", "true"), ("north", "false"), ("south", "false"), ("up", "false"), ("west", "false")],
            ),
        ),
    };
    if world.get_base_id(pos[0] + dx, pos[1], pos[2] + dz).block() == Block::Air {
        if let Some(mutation) = mutation.as_deref_mut() {
            mutation.write(world, pos[0] + dx, pos[1], pos[2] + dz, vine);
        } else {
            world.set_id(pos[0] + dx, pos[1], pos[2] + dz, vine);
        }
    }
}

impl OverworldGenerator {
    pub(super) fn has_structure_registry(&self) -> bool {
        self.structures.is_some()
    }

    fn compute_structure_starts(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        crate::counters::bump_structure_start();
        let Some(registry) = &self.structures else {
            return Vec::new();
        };
        let sampler = StartSampler::new(self);
        registry
            .starts_at(cx, cz, &sampler)
            .into_iter()
            .map(Arc::new)
            .collect()
    }

    /// Stage 0a: this chunk's structure starts, memoised.
    ///
    /// Empty (and allocation-free after the `Vec`'s own zero-capacity
    /// construction) for a generator whose resolver supplied no structure sets,
    /// which is every fixture resolver in this workspace.
    pub(super) fn structure_starts_stage(&self, cx: i32, cz: i32) -> Arc<Vec<Arc<StructureStart>>> {
        self.store
            .entry((cx, cz))
            .structure_starts
            .get_or_compute(drop, || {
                // Inside the once-guard, so this counts chunks whose starts
                // really ran — a cache hit adds nothing. Tagged `Structure` so
                // the bench's allocation binning attributes this work to
                // something narrower than `Other` (which also holds generator
                // construction).
                let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Structure);
                self.compute_structure_starts(cx, cz)
            })
    }

    fn structure_starts_stage_for_sets_with_sampler(
        &self,
        cx: i32,
        cz: i32,
        set_indices: &[usize],
        sampler: &StartSampler<'_>,
    ) -> Arc<Vec<Arc<StructureStart>>> {
        // Candidate provenance lets this cold computation skip every set whose
        // placement cannot own this source. The slot still memoises the complete
        // answer for the source, so later full-start callers see no subset.
        self.store
            .entry((cx, cz))
            .structure_starts
            .get_or_compute(drop, || {
                crate::counters::bump_structure_start();
                let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Structure);
                let Some(registry) = &self.structures else {
                    return Vec::new();
                };
                registry
                    .starts_at_sets(set_indices, cx, cz, sampler)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            })
    }

    /// Stage 0b: this chunk's structure references — `createReferences`' 17×17
    /// walk, keeping the starts whose adjusted box comes within
    /// [`BEARD_REACH`] blocks of this chunk.
    /// Candidate origins retain their structure-set index through the walk, so
    /// each source runs only its candidate sets while the source stage remains
    /// the single memoised owner of the resulting starts.
    ///
    /// Vanilla's `createReferences` uses a plain 16×16 chunk-box intersection;
    /// the extra 12 blocks are the beardifier's own reach
    /// (`Beardifier.forStructuresInChunk`'s `isCloseToChunk(chunkPos, 12)`), and
    /// keeping one product for both consumers is why this is the wider of the
    /// two. [`StructureRefs::packed_by_structure`] re-narrows to the exact
    /// chunk-box test for the persistence view, so the NBT is vanilla's and the
    /// beardifier's input is the beardifier's.
    pub(super) fn structure_refs_stage(&self, cx: i32, cz: i32) -> Arc<StructureRefs> {
        self.structure_refs_stage_with_index(cx, cz, None)
    }

    /// Computes references using a prebuilt source-window index when one is
    /// available. The stage slot remains the owner of each target's product;
    /// the index only replaces repeated candidate-cell enumeration.
    pub(super) fn structure_refs_stage_with_index(
        &self,
        cx: i32,
        cz: i32,
        origin_index: Option<&StructureOriginIndex>,
    ) -> Arc<StructureRefs> {
        if self.structures.is_none() {
            return self
                .store
                .entry((cx, cz))
                .structure_refs
                .get_or_compute(drop, || StructureRefs::default());
        }
        let sampler = StartSampler::new(self);
        self.structure_refs_stage_with_index_and_sampler(cx, cz, origin_index, &sampler)
    }

    pub(super) fn structure_refs_stage_with_index_and_sampler(
        &self,
        cx: i32,
        cz: i32,
        origin_index: Option<&StructureOriginIndex>,
        sampler: &StartSampler<'_>,
    ) -> Arc<StructureRefs> {
        self.store.entry((cx, cz)).structure_refs.get_or_compute(drop, || {
            let Some(registry) = &self.structures else {
                return StructureRefs::default();
            };
            crate::counters::bump_structure_reference_computation();
            let mut entries = Vec::new();
            let min_x = cx - REFS_RADIUS;
            let max_x = cx + REFS_RADIUS;
            let min_z = cz - REFS_RADIUS;
            let max_z = cz + REFS_RADIUS;
            let mut append_source = |sx: i32, sz: i32, set_indices: &[usize]| {
                for start in self
                    .structure_starts_stage_for_sets_with_sampler(sx, sz, set_indices, sampler)
                    .iter()
                {
                    if start.adjusted_bounding_box().is_close_to_chunk(cx, cz, BEARD_REACH)
                        || (start.structure.contains("ruined_portal")
                            && start.pieces.iter().any(|piece| {
                                matches!(piece.refine.as_ref(), Some(PieceRefinement::RuinedPortalTerrain { .. }))
                                    && piece.bounding_box.is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH)
                            }))
                    {
                        entries.push((sx, sz, Arc::clone(start)));
                    }
                }
            };
            if let Some(index) = origin_index {
                index.for_each_source(min_x, max_x, min_z, max_z, append_source);
            } else {
                let candidates = registry.origin_candidates_by_set_in(
                    min_x,
                    max_x,
                    min_z,
                    max_z,
                    sampler,
                );
                match StructureOriginIndex::from_candidates(min_x, max_x, min_z, max_z, candidates) {
                    Ok(index) => {
                        index.for_each_source(min_x, max_x, min_z, max_z, append_source);
                    }
                    Err(mut candidates) => {
                        // The bundled registry has fewer than 32 sets. Datapacks
                        // may exceed the mask width; retain the old sorted walk.
                        candidates
                            .sort_unstable_by_key(|((sx, sz), set_index)| (*sx, *sz, *set_index));
                        let mut offset = 0;
                        while offset < candidates.len() {
                            let ((sx, sz), _) = candidates[offset];
                            let end = candidates[offset..]
                                .iter()
                                .position(|((x, z), _)| (*x, *z) != (sx, sz))
                                .map_or(candidates.len(), |relative| offset + relative);
                            let mut set_indices = Vec::with_capacity(end - offset);
                            for &((_, _), set_index) in &candidates[offset..end] {
                                if set_indices.last().copied() != Some(set_index) {
                                    set_indices.push(set_index);
                                }
                            }
                            append_source(sx, sz, &set_indices);
                            offset = end;
                        }
                    }
                }
            }
            StructureRefs { entries }
        })
    }

    pub(super) fn structure_origin_index_for_bounds_with_sampler(
        &self,
        min_x: i32,
        max_x: i32,
        min_z: i32,
        max_z: i32,
        sampler: &StartSampler<'_>,
    ) -> Option<StructureOriginIndex> {
        let registry = self.structures.as_ref()?;
        let candidates = registry.origin_candidates_by_set_in(
            min_x,
            max_x,
            min_z,
            max_z,
            sampler,
        );
        StructureOriginIndex::from_candidates(min_x, max_x, min_z, max_z, candidates).ok()
    }

    /// The starts whose origin is `(cx, cz)` and whose piece lists are complete —
    /// the set a save file may legitimately carry.
    ///
    /// A start with an incomplete piece list is deliberately **not** returned:
    /// vanilla reloads a start with no `Children` as `INVALID`, so persisting one
    /// would be worse than persisting nothing. Use
    /// [`Self::structure_starts_including_incomplete`] to see the placement
    /// answer, which is complete today even where the pieces are not.
    #[must_use]
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        // Radius 0: computing a chunk's own starts reads no other store slot (that
        // is what keeps stage 0a the topmost stage), so there is nothing wider to
        // pin. `column()`'s pin is the wide one — see `STRUCTURE_CLOSURE_RADIUS`.
        let _view = self.store.open_view((cx, cz), 0);
        self.structure_starts_stage(cx, cz)
            .iter()
            .filter(|start| start.pieces_complete)
            .map(Arc::clone)
            .collect()
    }

    /// Every start whose origin is `(cx, cz)`, including those this engine can
    /// place but not yet build (see
    /// [`StructureStart::pieces_complete`](crate::structure::StructureStart::pieces_complete)
    /// and [`StructureRegistry::unsupported`](crate::structure::StructureRegistry::unsupported)).
    ///
    /// This is the placement answer, and it is the one to compare against a
    /// vanilla save's `structures.starts` keys.
    #[must_use]
    pub fn structure_starts_including_incomplete(
        &self,
        cx: i32,
        cz: i32,
    ) -> Vec<Arc<StructureStart>> {
        let _view = self.store.open_view((cx, cz), 0);
        self.structure_starts_stage(cx, cz).to_vec()
    }

    /// Every start whose pieces this chunk's placement stage will write, in the
    /// order it writes them — the input `structure_place_stage` itself reads.
    ///
    /// Exists so a gate can predict what a chunk must contain **without
    /// re-deriving the 17×17 reach or the `is_close_to_chunk` filter**. A mineshaft
    /// is 160 blocks wide and the oracle world has two starts three chunks apart, so
    /// "the pieces of the start at this chunk" is not the set that lands here, and a
    /// test that assumed it was measured 59 blocks against a true 97.
    #[must_use]
    pub fn structure_starts_placed_in(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        self.structure_refs_stage(cx, cz)
            .entries
            .iter()
            .filter(|(_, _, start)| start.pieces_complete)
            .map(|(_, _, start)| Arc::clone(start))
            .collect()
    }

    /// This chunk's `structures.References`, ready for the NBT writer: structure
    /// id → packed origin-chunk keys, narrowed to vanilla's own 16×16
    /// intersection test.
    #[must_use]
    pub fn structure_references(&self, cx: i32, cz: i32) -> std::collections::BTreeMap<String, Vec<i64>> {
        // `REFS_RADIUS`, not the wider column closure: this reads `structure_refs`
        // for one chunk, whose own walk is exactly the 17×17.
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        let refs = self.structure_refs_stage(cx, cz);
        let (bx, bz) = (cx * 16, cz * 16);
        let mut narrowed = StructureRefs::default();
        for (sx, sz, start) in &refs.entries {
            if start.pieces_complete
                && start
                    .bounding_box
                    .intersects_xz(bx, bz, bx + 15, bz + 15)
            {
                narrowed
                    .entries
                    .push((*sx, *sz, Arc::clone(start)));
            }
        }
        narrowed.packed_by_structure()
    }

    /// Stage 4b: writes every template-driven piece that
    /// touches this chunk into `world`.
    ///
    /// # Where this sits, and why
    ///
    /// Vanilla places structures inside `applyBiomeDecoration`, per generation
    /// step, *before* that step's features — and the three kinds wired today are
    /// all `surface_structures` (step 4), which precedes `underground_ores`
    /// (step 6) and `vegetal_decoration` (step 9). So this runs at the end of
    /// [`OverworldGenerator::pre_ore_stage`](super::OverworldGenerator): ore and
    /// vegetation then see the structure's blocks, exactly as they do in vanilla,
    /// and the whole thing is memoised once per chunk with the rest of the pre-ore
    /// product.
    ///
    /// # Clipping is the grid, not a box
    ///
    /// The working grid spans this chunk's 16×16 columns only and
    /// [`DenseBlockGrid::set`](crate::dense_grid::DenseBlockGrid::set) ignores a
    /// write outside it, so a piece that straddles a border writes its own half
    /// here and the other half when the neighbour generates — vanilla's
    /// `placeSettings.setBoundingBox(chunkBB)` for free. Template processor
    /// draws remain position-seeded; the ruined-portal refinement instead uses
    /// its target chunk's shared `surface_structures` stream, reset per portal
    /// registry entry and consumed in the same start order.
    pub(super) fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        self.structure_place_stage_with_touched(cx, cz, world, None)
    }

    pub(super) fn structure_place_stage_with_touched(
        &self,
        cx: i32,
        cz: i32,
        world: crate::dense_grid::DenseBlockGrid,
        touched: Option<&mut TouchedMask>,
    ) -> crate::dense_grid::DenseBlockGrid {
        self.structure_place_stage_with_touched_and_observer(cx, cz, world, touched, None)
    }

    pub(super) fn structure_place_stage_with_touched_and_observer(
        &self,
        cx: i32,
        cz: i32,
        mut world: crate::dense_grid::DenseBlockGrid,
        mut touched: Option<&mut TouchedMask>,
        mutation_observer: Option<super::BlockMutationObserverHandle>,
    ) -> crate::dense_grid::DenseBlockGrid {
        let Some(registry) = &self.structures else {
            return world;
        };
        // Below the early return, per this file's own rule about stage guards:
        // a guard above it would count a fixture-tree no-op as a run.
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Structure);
        let seed = registry.seed();
        let (bx, bz) = (cx * 16, cz * 16);
        // Structure identifiers are owned by the cached starts and live for the
        // whole placement pass. Borrow them as map keys so each chunk does not
        // clone the identifier merely to seed its local random stream.
        let mut feature_randoms: HashMap<
            &str,
            crate::rng::WorldgenRandom<crate::rng::XoroshiroRandomSource>,
        > = HashMap::new();
        let mineshaft_sampler = StartSampler::new(self);
        let solid_render = |state: CanonicalStateId| {
            self.veg_tags
                .simple_block_support
                .solid_render
                .test_id(state)
        };
        let structure_refs = self.structure_refs_stage(cx, cz);
        let mut structure_entries = structure_refs.entries.iter().collect::<Vec<_>>();
        order_structure_entries(registry, &mut structure_entries);
        // Each structure's target-chunk stream starts at its runtime-registry
        // index within the decoration step. Every start of that structure then
        // shares it, while each start reconstructs its own retained tree.
        let mut mineshaft_randoms: HashMap<
            &str,
            WorldgenRandom<XoroshiroRandomSource>,
        > = HashMap::new();
        let mut portal_randoms: HashMap<&str, WorldgenRandom<XoroshiroRandomSource>> = HashMap::new();
        let mut no_op_sink = NoopStructureMutationSink;
        for (_, _, start) in structure_entries {
            if !start.pieces_complete {
                continue;
            }
            let step = registry
                .feature_placement_key(&start.structure)
                .map_or(0, |(step, _)| step);
            let mut touched_sink = touched.as_deref_mut().map(|mask| TouchedColumnSink {
                mask,
                chunk_x: cx,
                chunk_z: cz,
            });
            let mut mutation = if let Some(sink) = touched_sink.as_mut() {
                Some(StructureMutationContext::new_with_observer(
                    sink,
                    (start.chunk_x, start.chunk_z),
                    step,
                    mutation_observer.clone(),
                ))
            } else if mutation_observer.is_some() {
                Some(StructureMutationContext::new_with_observer(
                    &mut no_op_sink,
                    (start.chunk_x, start.chunk_z),
                    step,
                    mutation_observer.clone(),
                ))
            } else {
                None
            };
            let is_mineshaft = registry
                .structure(&start.structure)
                .is_some_and(|definition| matches!(definition.kind, StructureKind::Mineshaft { .. }));
            if is_mineshaft && start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) {
                let mineshaft_random = mineshaft_randoms.entry(start.structure.as_str()).or_insert_with(|| {
                    let (step, index) = registry
                        .runtime_decoration_key(&start.structure)
                        .expect("mineshaft structure has a decoration key");
                    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
                    let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                    random.set_feature_seed(decoration_seed, index as i32, step);
                    random
                });
                if let Some(blocks) = registry.mineshaft_blocks_for_chunk(
                    start,
                    cx,
                    cz,
                    &mineshaft_sampler,
                    &world,
                    mineshaft_random,
                ) {
                    for block in blocks {
                        if let Some(mutation) = mutation.as_mut() {
                            mutation.write(
                                &mut world,
                                block.pos[0],
                                block.pos[1],
                                block.pos[2],
                                block.state,
                            );
                        } else {
                            world.set_id(block.pos[0], block.pos[1], block.pos[2], block.state);
                        }
                    }
                    continue;
                }
            }
            if start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) {
                let fortress_placed = if let Some(mutation) = mutation.as_mut() {
                    if let Some((fortress_step, fortress_index)) =
                        registry.runtime_decoration_key(&start.structure)
                    {
                        let mut fortress_random =
                            WorldgenRandom::new(XoroshiroRandomSource::new(0));
                        let decoration_seed = fortress_random.set_decoration_seed(seed, bx, bz);
                        fortress_random.set_feature_seed(
                            decoration_seed,
                            fortress_index as i32,
                            fortress_step,
                        );
                        registry
                            .place_fortress_for_chunk_with_sink(
                                start,
                                cx,
                                cz,
                                &mut world,
                                &mut fortress_random,
                                &solid_render,
                                Some(mutation),
                            )
                            .is_some()
                    } else {
                        false
                    }
                } else {
                    registry.place_fortress_for_chunk(start, cx, cz, &mut world)
                };
                if fortress_placed {
                    continue;
                }
            }
            // `StructureStart.placeInChunk` derives one `referencePos` for the whole
            // start, from its **first** piece's box, before the per-piece loop. It
            // is not a per-piece value and it is not the chunk — an
            // `axis_aligned_linear_pos` rule measures from here.
            let reference = crate::structure::jigsaw::reference_position(&start.pieces);
            for piece in &start.pieces {
                let portal_terrain_reaches = matches!(
                    piece.refine.as_ref(),
                    Some(PieceRefinement::RuinedPortalTerrain { .. })
                ) && piece
                    .bounding_box
                    .is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH);
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) && !portal_terrain_reaches {
                    continue;
                }
                // A coded piece writes a pre-resolved block list; a template piece
                // writes its template. Both are clipped by the grid.
                if let Some(blocks) = &piece.blocks {
                    place_coded_blocks_with_sink(
                        &mut world,
                        blocks,
                        &solid_render,
                        mutation.as_mut(),
                    );
                }
                if let Some(placement) = &piece.placement {
                    let origin = crate::structure::template::PlaceOrigin {
                        position: placement.position,
                        reference,
                        seed,
                    };
                    if let Some(mutation) = mutation.as_mut() {
                        placement.template.place_with_mutations(
                            origin,
                            &placement.settings,
                            &mut world,
                            mutation,
                        );
                    } else {
                        placement
                            .template
                            .place(origin, &placement.settings, &mut world);
                    }
                    // A `list_pool_element` writes several templates at one position,
                    // in document order — `ListPoolElement.place`'s own loop.
                    for extra in &piece.extra_placements {
                        let origin = crate::structure::template::PlaceOrigin {
                            position: extra.position,
                            reference,
                            seed,
                        };
                        if let Some(mutation) = mutation.as_mut() {
                            extra.template.place_with_mutations(
                                origin,
                                &extra.settings,
                                &mut world,
                                mutation,
                            );
                        } else {
                            extra.template.place(origin, &extra.settings, &mut world);
                        }
                    }
                }
                // Refinements read and write the real post-surface, post-carve grid.
                // Portal terrain runs after the frame, while buried treasure has no
                // template and simply takes this same post-placement hook.
                match piece.refine.as_ref() {
                    Some(PieceRefinement::FeaturePlacements { placements }) => {
                        let Some((step, index)) = registry.feature_placement_key(&start.structure) else {
                            continue;
                        };
                        let random = feature_randoms.entry(start.structure.as_str()).or_insert_with(|| {
                            let mut random = crate::rng::WorldgenRandom::new(
                                crate::rng::XoroshiroRandomSource::new(0),
                            );
                            let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                            random.set_feature_seed(decoration_seed, index as i32, step);
                            random
                        });
                        crate::structure::feature_placement::place_feature_pool_elements_with_sink(
                            random,
                            seed,
                            placements,
                            &mut world,
                            &self.veg_tags,
                            mutation.as_mut(),
                        );
                    }
                    Some(PieceRefinement::StrongholdBlocks { writes }) => {
                        crate::structure::stronghold::place_post_surface_blocks_with_sink(
                            &mut world,
                            writes,
                            mutation.as_mut(),
                        );
                    }
                    Some(PieceRefinement::BuriedTreasureChest) => {
                        place_buried_treasure_chest_with_sink(
                            &mut world,
                            piece.bounding_box.min,
                            mutation.as_mut(),
                        );
                    }
                    Some(PieceRefinement::RuinedPortalTerrain {
                        placement,
                        cold,
                        overgrown,
                        vines,
                        features_cannot_replace,
                    }) => {
                        let Some((step, index)) = registry.feature_placement_key(&start.structure) else {
                            continue;
                        };
                        let random = portal_randoms.entry(start.structure.as_str()).or_insert_with(|| {
                            let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
                            let decoration_seed = random.set_decoration_seed(seed, bx, bz);
                            random.set_feature_seed(decoration_seed, index as i32, step);
                            random
                        });
                        place_ruined_portal_terrain_with_sink(
                            &mut world,
                            piece.bounding_box,
                            random,
                            *placement,
                            *cold,
                            *overgrown,
                            *vines,
                            features_cannot_replace,
                            mutation.as_mut(),
                        );
                    }
                    Some(PieceRefinement::FortressPlacement { .. }) | None => {}
                    Some(PieceRefinement::NetherFossilDriedGhast { .. }) => {}
                }
            }
        }
        world
    }

    /// Stage 0c: this chunk's beard term.
    ///
    /// Cheap in the case that matters. A generator with no structure data returns
    /// [`Beardifier::empty`] without touching the store; a generator *with*
    /// structure data reads its already-memoised
    /// [`StructureRefs`] and, for the overwhelming majority of chunks, finds no
    /// adaptation-bearing start and returns empty too. Only a chunk genuinely
    /// within reach of one builds a rigid list, and only then does
    /// [`fill_stage`](OverworldGenerator::fill_stage) take its per-block branch.
    ///
    /// Not memoised in the store, deliberately: it is a pure function of
    /// `structure_refs` (which *is* memoised) and it is consumed exactly once per
    /// chunk, by the fill. A slot for it would be a third stage carrying no work.
    pub(super) fn beardifier_for(&self, cx: i32, cz: i32) -> crate::structure::beardifier::Beardifier {
        self.beardifier_for_with_index(cx, cz, None)
    }

    pub(super) fn beardifier_for_with_index(
        &self,
        cx: i32,
        cz: i32,
        origin_index: Option<&StructureOriginIndex>,
    ) -> crate::structure::beardifier::Beardifier {
        if self.structures.is_none() {
            return crate::structure::beardifier::Beardifier::empty();
        }
        let sampler = StartSampler::new(self);
        self.beardifier_for_with_index_and_sampler(cx, cz, origin_index, &sampler)
    }

    pub(super) fn beardifier_for_with_index_and_sampler(
        &self,
        cx: i32,
        cz: i32,
        origin_index: Option<&StructureOriginIndex>,
        sampler: &StartSampler<'_>,
    ) -> crate::structure::beardifier::Beardifier {
        use crate::structure::beardifier::Beardifier;
        if self.structures.is_none() {
            return Beardifier::empty();
        }
        let refs = self.structure_refs_stage_with_index_and_sampler(cx, cz, origin_index, sampler);
        let starts = refs
            .entries
            .iter()
            .map(|(_, _, start)| start)
            .filter(|start| {
                start.pieces_complete
                    && start.terrain_adaptation != crate::structure::TerrainAdjustment::None
                    && start.adjusted_bounding_box().is_close_to_chunk(cx, cz, BEARD_REACH)
            });
        Beardifier::for_chunk(cx, cz, starts.map(std::convert::AsRef::as_ref))
    }

    /// This chunk's pre-surface shape field (`fillFromNoise`'s output, stage 1)
    /// with an **explicit** beard term — the seam a gate or a JVM comparison
    /// drives the beardifier through.
    ///
    /// Public because S3's evidence needs it and there is no other way in. The
    /// production path derives its beard from [`Self::beardifier_for`], which can
    /// only ever produce the beard the *real* starts imply; every
    /// adaptation-bearing structure in 26.2 is jigsaw (S4) or coded (S5), so until
    /// one of those lands, a real generated chunk cannot exercise a non-empty
    /// beard at all. Passing one in is what lets the terrain change be measured
    /// now rather than asserted later, and it is also the comparison point for a
    /// `Beardifier`-bearing JVM dump.
    ///
    /// Returns `16 × height × 16` [`BlockKind`](crate::aquifer::BlockKind)s;
    /// index it with [`Self::shape_index`] rather than restating the layout.
    /// Calling it does **not** touch the store's `pre_ore` slot: it builds a fresh
    /// aquifer, so it is not a way to poison the memoised pipeline with a
    /// synthetic beard.
    #[must_use]
    pub fn shape_field_with_beard(
        &self,
        cx: i32,
        cz: i32,
        beard: &crate::structure::beardifier::Beardifier,
    ) -> Vec<crate::aquifer::BlockKind> {
        let aquifer = self.build_aquifer(cx, cz);
        self.fill_stage(&aquifer, cx * 16, cz * 16, beard).0
    }

    /// Where chunk-local `(lx, ly, lz)` lands in
    /// [`Self::shape_field_with_beard`]'s return.
    ///
    /// Forwards to the *same* private `idx` the fill writes through, rather than
    /// restating `((ly * 16 + lz) * 16 + lx)`: a caller that restated it would
    /// read a transposed field and report a plausible-looking wrong answer, and
    /// the two spellings could then drift apart independently.
    #[must_use]
    pub fn shape_index(&self, lx: i32, ly: i32, lz: i32) -> usize {
        Self::idx(lx, ly, lz, self.height())
    }

    /// The beard term this generator's real starts imply for `(cx, cz)` — the
    /// exact value the production fill uses.
    ///
    /// Public so a gate can assert *which* branch the fill took, rather than
    /// inferring it from the output. See [`Self::shape_field_with_beard`].
    #[must_use]
    pub fn beardifier(&self, cx: i32, cz: i32) -> crate::structure::beardifier::Beardifier {
        let _view = self.store.open_view((cx, cz), REFS_RADIUS);
        self.beardifier_for(cx, cz)
    }

    /// The registry's unsupported ledger, or an empty map for a generator with no
    /// structure data. See
    /// [`StructureRegistry::unsupported`](crate::structure::StructureRegistry::unsupported).
    #[must_use]
    pub fn structure_ledger(&self) -> std::collections::BTreeMap<String, String> {
        self.structures
            .as_ref()
            .map(|r| r.unsupported().clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    use lodestone_worldgen_core::rng::{get_seed, LegacyRandomSource};

    #[test]
    fn height_probe_cursor_shares_the_downward_walk_between_maps() {
        let mut cache = HeightProbeCache::default();
        let entry = cache.entry_mut(3, -2, 10);

        // A world-surface query stops at the first non-air block and leaves the
        // cursor below it; the ocean-floor query then resumes at that cursor.
        for y in (0..=entry.next_y).rev() {
            entry.next_y = y - 1;
            let kind = match y {
                8 => BlockKind::Water,
                6 => BlockKind::Stone,
                _ => BlockKind::Air,
            };
            if entry.world_surface == EMPTY_HEIGHT_PROBE_Y && kind != BlockKind::Air {
                entry.world_surface = y;
                break;
            }
        }
        assert_eq!(entry.world_surface, 8);
        assert_eq!(entry.next_y, 7);

        while entry.next_y >= 0 && entry.ocean_floor == EMPTY_HEIGHT_PROBE_Y {
            let y = entry.next_y;
            entry.next_y -= 1;
            if y == 6 {
                entry.ocean_floor = y;
            }
        }
        assert_eq!(entry.ocean_floor, 6);
        assert_eq!(entry.next_y, 5, "the second map must not rescan the first hit");
    }

    #[test]
    fn height_probe_cache_replacement_is_bounded_and_exact() {
        let mut cache = HeightProbeCache::default();
        for index in 0..(HEIGHT_PROBE_CACHE_CAPACITY + 3) {
            let entry = cache.entry_mut(index as i32, 0, 20);
            entry.world_surface = index as i32;
        }
        assert_eq!(cache.entries.len(), HEIGHT_PROBE_CACHE_CAPACITY);
        assert!(cache.entries.iter().all(|entry| entry.occupied()));
        for index in 0..3 {
            assert!(
                cache.entries.iter().any(|entry| {
                    entry.x == (HEIGHT_PROBE_CACHE_CAPACITY + index) as i32
                        && entry.world_surface == (HEIGHT_PROBE_CACHE_CAPACITY + index) as i32
                }),
                "replacement must retain the newest probe {index}"
            );
        }
    }

    #[test]
    fn height_probe_cache_reuses_a_key_without_replacement() {
        let mut cache = HeightProbeCache::default();
        cache.entry_mut(11, -7, 20).world_surface = 13;
        cache.entry_mut(11, -7, 20).ocean_floor = 9;

        assert_eq!(cache.replacement, 0);
        let entry = cache.entry_mut(11, -7, 20);
        assert_eq!(entry.world_surface, 13);
        assert_eq!(entry.ocean_floor, 9);
    }

    #[test]
    fn structure_cache_entries_have_the_bounded_layout() {
        assert_eq!(
            std::mem::size_of::<HeightProbeEntry>(),
            5 * std::mem::size_of::<i32>(),
            "height probes must stay five-word entries; Option padding would grow every sampler"
        );
        assert_eq!(
            std::mem::size_of::<HeightProbeCache>(),
            HEIGHT_PROBE_CACHE_CAPACITY * std::mem::size_of::<HeightProbeEntry>()
                + std::mem::size_of::<usize>(),
            "height cache must remain a fixed array plus one replacement cursor"
        );
    }

    #[test]
    fn origin_mask_preserves_source_and_set_order_with_negative_coordinates() {
        let candidates = vec![
            ((-2, -1), 3),
            ((-2, -1), 1),
            ((-2, -1), 1),
            ((-2, 0), 0),
            ((0, -2), 2),
        ];
        let index = StructureOriginIndex::from_candidates(-2, 0, -2, 0, candidates.clone())
            .expect("bundled set indices fit the compact mask");
        let mut indexed = Vec::new();
        for x in -2..=0 {
            for z in -2..=0 {
                let mut mask = index.mask_at(x, z);
                while mask != 0 {
                    let set_index = mask.trailing_zeros() as usize;
                    indexed.push(((x, z), set_index));
                    mask &= mask - 1;
                }
            }
        }
        let mut sorted = candidates;
        sorted.sort_unstable();
        let mut fallback = Vec::new();
        let mut offset = 0;
        while offset < sorted.len() {
            let ((x, z), _) = sorted[offset];
            let end = sorted[offset..]
                .iter()
                .position(|((other_x, other_z), _)| (*other_x, *other_z) != (x, z))
                .map_or(sorted.len(), |relative| offset + relative);
            for &((_, _), set_index) in &sorted[offset..end] {
                if fallback.last().copied() != Some(((x, z), set_index)) {
                    fallback.push(((x, z), set_index));
                }
            }
            offset = end;
        }
        assert_eq!(indexed, fallback);
    }

    #[test]
    fn origin_mask_falls_back_when_a_datapack_exceeds_u32_set_width() {
        let candidates = vec![((-1, -1), u32::BITS as usize)];
        assert!(StructureOriginIndex::from_candidates(-1, -1, -1, -1, candidates).is_err());
    }

    #[test]
    fn structure_touch_sink_marks_same_state_writes_but_not_clipped_columns() {
        let mut world = crate::dense_grid::DenseBlockGrid::new(0, 0, 0, 2, 2, 2, "minecraft:air");
        let mut mask = TouchedMask::default();
        let mut sink = TouchedColumnSink {
            mask: &mut mask,
            chunk_x: 0,
            chunk_z: 0,
        };
        let mut mutation = StructureMutationContext::new(&mut sink, (0, 0), 4);

        mutation.write(
            &mut world,
            1,
            0,
            1,
            CanonicalStateId::from_state_str("minecraft:air").unwrap(),
        );
        mutation.write(
            &mut world,
            2,
            0,
            0,
            CanonicalStateId::from_state_str("minecraft:stone").unwrap(),
        );

        assert!(crate::carver::touched_column(&mask, 1, 1));
        assert!(!crate::carver::touched_column(&mask, 0, 0));
    }

    struct SourceIndexResolver;

    impl crate::density::Resolver for SourceIndexResolver {
        fn density_function(&self, _id: &str) -> serde_json::Value {
            serde_json::Value::Null
        }

        fn noise(&self, _id: &str) -> crate::density::NoiseParams {
            crate::density::NoiseParams {
                first_octave: 0,
                amplitudes: Vec::new(),
            }
        }

        fn structure_set_ids(&self) -> Vec<String> {
            vec!["test:sparse".to_owned(), "test:dense".to_owned()]
        }

        fn structure_set(&self, id: &str) -> serde_json::Value {
            let (spacing, salt) = match id {
                "test:sparse" => (2, 17),
                "test:dense" => (1, 31),
                _ => return serde_json::Value::Null,
            };
            serde_json::json!({
                "placement": {
                    "type": "minecraft:random_spread",
                    "spacing": spacing,
                    "separation": 0,
                    "salt": salt
                },
                "structures": [{"structure": "minecraft:buried_treasure", "weight": 1}]
            })
        }

        fn structure(&self, id: &str) -> serde_json::Value {
            if id != "minecraft:buried_treasure" {
                return serde_json::Value::Null;
            }
            serde_json::json!({
                "type": "minecraft:buried_treasure",
                "biomes": ["test:biome"],
                "step": "surface_structures",
                "terrain_adaptation": "none"
            })
        }
    }

    struct SourceIndexWorld;

    impl crate::structure::StartContext for SourceIndexWorld {
        fn first_occupied_height(
            &self,
            _x: i32,
            _z: i32,
            _heightmap: HeightmapKind,
        ) -> i32 {
            63
        }

        fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
            "test:biome".to_owned()
        }

        fn sea_level(&self) -> i32 {
            63
        }
    }

    fn start_signatures(starts: &[crate::structure::StructureStart]) -> Vec<String> {
        starts.iter().map(|start| format!("{start:?}")).collect()
    }

    #[test]
    fn source_set_index_matches_scalar_starts_with_a_discriminating_control() {
        let context = SourceIndexWorld;
        for seed in [-195_764_831, 0, 42] {
            let registry = crate::structure::StructureRegistry::new(seed, &SourceIndexResolver);
            let discriminating_source = (-8..=8)
                .flat_map(|cx| (-8..=8).map(move |cz| (cx, cz)))
                .find(|&(cx, cz)| {
                    registry
                        .origin_candidates_by_set_in(cx, cx, cz, cz, &context)
                        .len()
                        == 2
                })
                .expect("the fixture must expose a source with two set candidates");
            for (cx, cz) in [(0, 0), (-1, 2), (3, -4), (7, 5), discriminating_source] {
                let mut set_indices = registry
                    .origin_candidates_by_set_in(cx, cx, cz, cz, &context)
                    .into_iter()
                    .map(|(_, set_index)| set_index)
                    .collect::<Vec<_>>();
                set_indices.sort_unstable();
                set_indices.dedup();
                let scalar = registry.starts_at(cx, cz, &context);
                let indexed = registry.starts_at_sets(&set_indices, cx, cz, &context);
                assert_eq!(
                    start_signatures(&indexed),
                    start_signatures(&scalar),
                    "source/set index changed starts at seed {seed}, chunk ({cx}, {cz})"
                );

                if (cx, cz) == discriminating_source {
                    let mut missing_set = set_indices.clone();
                    missing_set.pop();
                    assert_ne!(
                        start_signatures(&registry.starts_at_sets(&missing_set, cx, cz, &context)),
                        start_signatures(&scalar),
                        "negative control must detect an omitted source set"
                    );
                }
            }
        }
    }

    fn portal_fixture_world() -> crate::dense_grid::DenseBlockGrid {
        let mut map = HashMap::new();
        for x in 0..64 {
            for y in 0..=59 {
                for z in 0..64 {
                    map.insert((x, y, z), "minecraft:stone".to_string());
                }
            }
        }
        map.insert((30, 60, 30), "minecraft:netherrack".to_string());
        crate::dense_grid::DenseBlockGrid::from_hashmap(0, 0, 0, 64, 65, 64, &map)
    }

    fn portal_fixture_hash(world: &crate::dense_grid::DenseBlockGrid) -> String {
        use sha2::{Digest as _, Sha256};

        let mut digest = Sha256::new();
        for y in 0..=64 {
            for x in 0..64 {
                for z in 0..64 {
                    if world.get(x, y, z) == "minecraft:netherrack" {
                        digest.update(
                            format!("{x},{y},{z}=minecraft:netherrack\n").as_bytes(),
                        );
                    }
                }
            }
        }
        format!("{:x}", digest.finalize())
    }

    fn portal_stream(
        world_seed: i64,
        chunk_x: i32,
        chunk_z: i32,
    ) -> WorldgenRandom<XoroshiroRandomSource> {
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let decoration_seed = random.set_decoration_seed(world_seed, chunk_x * 16, chunk_z * 16);
        random.set_feature_seed(decoration_seed, 8, 4);
        random
    }

    #[test]
    fn ruined_portal_terrain_stream_matches_external_fixture_and_rejects_positional_control() {
        const EXTERNAL: &str =
            include_str!("../../tests/support/coded_ruined_portal_terrain_external.txt");
        let mut cases = Vec::new();
        for line in EXTERNAL.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 6, "portal fixture fields: {line}");
            assert_eq!(fields[0], "case", "portal fixture row: {line}");
            let world_seed = fields[1]
                .parse::<i64>()
                .unwrap_or_else(|error| panic!("invalid portal fixture seed {}: {error}", fields[1]));
            let chunk_x = fields[2]
                .parse::<i32>()
                .unwrap_or_else(|error| panic!("invalid portal fixture chunk {}: {error}", fields[2]));
            let chunk_z = fields[3]
                .parse::<i32>()
                .unwrap_or_else(|error| panic!("invalid portal fixture chunk {}: {error}", fields[3]));
            let distance = fields[4]
                .parse::<i32>()
                .unwrap_or_else(|error| panic!("invalid portal fixture distance {}: {error}", fields[4]));
            cases.push((world_seed, chunk_x, chunk_z, distance, fields[5].to_string()));
        }
        assert_eq!(cases.len(), 4, "external portal fixture case count");

        let box_ = crate::structure::BoundingBox {
            min: [29, 60, 29],
            max: [33, 63, 33],
        };
        let mut hashes = Vec::new();
        for (world_seed, chunk_x, chunk_z, expected_distance, expected_hash) in &cases {
            let mut probe = portal_stream(*world_seed, *chunk_x, *chunk_z);
            assert_eq!(
                probe.next_int_bounded(6),
                *expected_distance,
                "external distance draw for ({chunk_x},{chunk_z})"
            );

            let mut world = portal_fixture_world();
            let mut random = portal_stream(*world_seed, *chunk_x, *chunk_z);
            place_ruined_portal_terrain(
                &mut world,
                box_,
                &mut random,
                VerticalPlacement::OnLandSurface,
                true,
                false,
                false,
                &HashSet::new(),
            );
            let actual_hash = portal_fixture_hash(&world);
            assert_eq!(actual_hash, *expected_hash, "external portal case {world_seed} ({chunk_x},{chunk_z})");
            hashes.push(actual_hash);
        }

        assert_ne!(hashes[0], hashes[1], "same portal geometry must follow target chunk stream");
        assert_ne!(hashes[0], hashes[2], "chunk-Z stream change must be observable");

        let centre = [31, 62, 31];
        let old_distance = LegacyRandomSource::new(42 ^ get_seed(centre[0], centre[1], centre[2]))
            .next_int_bounded(6);
        assert_ne!(old_distance, cases[1].3, "the positional control must reject the target-chunk fixture");
    }

    /// A column with `depth` blocks of sand over stone, air above — the shape
    /// that makes the walk actually walk (a beach/ocean-floor surface rule
    /// stacking sand over the real stone), rather than terminating on its
    /// first iteration.
    fn sandy_column(depth: i32) -> crate::dense_grid::DenseBlockGrid {
        let mut map = HashMap::new();
        // Stone from -64 up to (but not including) the sand layer.
        let stone_top = 60 - depth;
        for y in -64..=stone_top {
            map.insert((8, y, 8), "minecraft:stone".to_string());
        }
        for y in (stone_top + 1)..=60 {
            map.insert((8, y, 8), "minecraft:sand".to_string());
        }
        crate::dense_grid::DenseBlockGrid::from_hashmap(0, -64, 0, 16, 384, 16, &map)
    }

    /// The expected facing values come from the independent four-neighbour
    /// direction table in the fixture, not from the production helper. The
    /// final row repeats the asymmetric east-neighbour case with solid cells
    /// above and below; those cells must not affect a horizontal reorientation.
    #[test]
    fn coded_chest_reorientation_matches_external_asymmetric_fixture() {
        const EXTERNAL: &str =
            include_str!("../../tests/support/coded_chest_reorient_external.txt");
        let mut east_face = None;
        let mut east_vertical_control = None;
        for line in EXTERNAL.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 8, "fixture fields: {line}");
            let pos = [1, 65, 1];
            let horizontal = [
                (0, -1, fields[1]),
                (1, 0, fields[2]),
                (0, 1, fields[3]),
                (-1, 0, fields[4]),
            ];
            let mut world = crate::dense_grid::DenseBlockGrid::new(
                0,
                64,
                0,
                3,
                3,
                3,
                "minecraft:air",
            );
            for (dx, dz, state) in horizontal {
                world.set(pos[0] + dx, pos[1], pos[2] + dz, state);
            }
            world.set(pos[0], pos[1] + 1, pos[2], fields[5]);
            world.set(pos[0], pos[1] - 1, pos[2], fields[6]);
            let solid_render = |state: CanonicalStateId| state.block() == Block::Stone;
            place_coded_blocks(
                &mut world,
                &[CodedBlock {
                    pos,
                    state: CanonicalStateId::from_state_str(
                        "minecraft:chest[facing=north,type=single,waterlogged=false]",
                    )
                    .unwrap(),
                }],
                &solid_render,
            );
            let actual = world.get(pos[0], pos[1], pos[2]).to_string();
            let expected = format!(
                "minecraft:chest[facing={},type=single,waterlogged=false]",
                fields[7]
            );
            assert_eq!(actual, expected, "fixture case {}", fields[0]);
            match fields[0] {
                "single_east" => east_face = Some(actual),
                "single_east_vertical_control" => east_vertical_control = Some(actual),
                _ => {}
            }
        }
        assert_eq!(
            east_face.as_deref(),
            Some("minecraft:chest[facing=west,type=single,waterlogged=false]")
        );
        assert_eq!(east_face, east_vertical_control);
    }

    /// The chest lands exactly one block above the first **stone-family**
    /// block, not the first solid block — a beach column with sand on top must
    /// be walked *through*, matching vanilla's own multi-layer descent.
    #[test]
    fn the_chest_lands_on_stone_under_a_sand_beach() {
        let mut world = sandy_column(3);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Stone top is at 60 - 3 = 57, so the chest sits at 58.
        assert_eq!(world.get(8, 58, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
        // Nothing was placed at the sand layer or below the stone surface.
        assert_ne!(world.get(8, 60, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// A column with **no** sand at all (stone straight to the surface) places
    /// the chest one above bare stone — the degenerate case of the same walk.
    #[test]
    fn the_chest_lands_directly_on_bare_stone() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        assert_eq!(world.get(8, 61, 8), "minecraft:chest[facing=north,type=single,waterlogged=false]");
    }

    /// Every air/liquid neighbour of the chest is filled — straight down with
    /// the stone-family block the walk found, everywhere else with the
    /// pre-existing block (here, air, so it falls back to sand).
    #[test]
    fn every_air_neighbour_of_the_chest_is_filled() {
        let mut world = sandy_column(0);
        place_buried_treasure_chest(&mut world, [8, 90, 8]);
        // Chest at (8, 61, 8), stone at (8, 60, 8) and below.
        assert_eq!(world.get(8, 60, 8), "minecraft:stone", "the ground itself is untouched");
        // The four horizontal neighbours and the one above were air; each
        // must now be something solid (sand, since there was nothing else to
        // reuse) rather than air.
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0)] {
            let state = world.get(8 + dx, 61 + dy, 8 + dz);
            assert_ne!(state, "minecraft:air", "neighbour ({dx},{dy},{dz}) was left air");
        }
    }

    /// `base_name` strips a bracketed property list; `is_air_or_liquid` and
    /// `is_stone_family` read the five- and three-member sets vanilla's own
    /// `postProcess` names, and nothing else.
    #[test]
    fn the_material_predicates_match_exactly_vanillas_named_sets() {
        assert_eq!(base_name("minecraft:water[level=0]"), "minecraft:water");
        assert!(is_air_or_liquid("minecraft:air"));
        assert!(is_air_or_liquid("minecraft:water[level=3]"));
        assert!(is_air_or_liquid("minecraft:lava"));
        assert!(!is_air_or_liquid("minecraft:stone"));
        for name in [
            "minecraft:sandstone",
            "minecraft:stone",
            "minecraft:andesite",
            "minecraft:granite",
            "minecraft:diorite",
        ] {
            assert!(is_stone_family(name), "{name} should be stone-family");
        }
        for name in ["minecraft:dirt", "minecraft:gravel", "minecraft:sand", "minecraft:deepslate"] {
            assert!(!is_stone_family(name), "{name} should not be stone-family");
        }
    }

    /// The portal refinement grows a real skirt beyond the frame, creates a
    /// downward column from the frame's netherrack, and leaves protected blocks
    /// alone. A bare template-placement test cannot observe any of those three
    /// post-template effects.
    #[test]
    fn ruined_portal_refinement_grows_skirt_and_preserves_protected_blocks() {
        let mut map = HashMap::new();
        for x in 0..16 {
            for z in 0..16 {
                for y in -4..=59 {
                    map.insert((x, y, z), "minecraft:stone".to_string());
                }
            }
        }
        let box_ = crate::structure::BoundingBox {
            min: [5, 60, 5],
            max: [8, 63, 8],
        };
        map.insert((6, 60, 6), "minecraft:netherrack".to_string());
        map.insert((5, 59, 5), "minecraft:obsidian".to_string());
        let mut world = crate::dense_grid::DenseBlockGrid::from_hashmap(0, -4, 0, 16, 96, 16, &map);
        let protected = std::collections::HashSet::new();
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(41));
        place_ruined_portal_terrain(
            &mut world,
            box_,
            &mut random,
            VerticalPlacement::OnLandSurface,
            true,
            false,
            false,
            &protected,
        );
        let skirt_cells = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, z)))
            .filter(|&(x, z)| x < box_.min[0] || x > box_.max[0] || z < box_.min[2] || z > box_.max[2])
            .filter(|&(x, z)| base_name(&world.get(x, 59, z)) == "minecraft:netherrack")
            .count();
        assert!(skirt_cells > 0, "the portal produced no netherrack outside its frame");
        assert_eq!(
            base_name(&world.get(6, 59, 6)),
            "minecraft:netherrack",
            "the frame's netherrack did not grow a drip column"
        );
        assert_eq!(
            base_name(&world.get(5, 59, 5)),
            "minecraft:obsidian",
            "terrain growth replaced a protected block"
        );
    }

    /// A reference walk is source-chunk-first, while the decoration lifecycle
    /// groups starts by generation step and then by the complete registry's
    /// resource order. Keeping this deliberately reversed control catches a
    /// direct `StructureRefs.entries` walk: the two orders differ both across
    /// steps and within the same step.
    #[test]
    fn target_structure_replay_groups_by_step_and_registry_order() {
        use crate::density::{NoiseParams, Resolver};
        use serde_json::Value;

        struct ResolverForOrder;

        impl Resolver for ResolverForOrder {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }

            fn noise(&self, _id: &str) -> NoiseParams {
                NoiseParams {
                    first_octave: 0,
                    amplitudes: Vec::new(),
                }
            }

            fn structure_set_ids(&self) -> Vec<String> {
                [
                    "minecraft:order_mansion",
                    "minecraft:order_mineshaft",
                    "minecraft:order_fortress",
                    "minecraft:order_ancient_city",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect()
            }

            fn structure_set(&self, id: &str) -> Value {
                let structure = match id {
                    "minecraft:order_mansion" => "minecraft:mansion",
                    "minecraft:order_mineshaft" => "minecraft:mineshaft",
                    "minecraft:order_fortress" => "minecraft:fortress",
                    "minecraft:order_ancient_city" => "minecraft:ancient_city",
                    _ => return Value::Null,
                };
                serde_json::json!({
                    "placement": {
                        "type": "minecraft:random_spread",
                        "spacing": 1,
                        "separation": 0,
                        "salt": 0
                    },
                    "structures": [{"structure": structure, "weight": 1}]
                })
            }

            fn structure(&self, id: &str) -> Value {
                let step = match id {
                    "minecraft:mansion" | "minecraft:fortress" | "minecraft:ancient_city" => {
                        if id == "minecraft:ancient_city" || id == "minecraft:fortress" {
                            "underground_decoration"
                        } else {
                            "surface_structures"
                        }
                    }
                    "minecraft:mineshaft" => "underground_structures",
                    _ => return Value::Null,
                };
                serde_json::json!({
                    "type": "minecraft:test_unsupported",
                    "biomes": [],
                    "step": step,
                    "terrain_adaptation": "none"
                })
            }
        }

        fn start(id: &str) -> Arc<StructureStart> {
            Arc::new(StructureStart {
                structure: id.to_string(),
                chunk_x: 0,
                chunk_z: 0,
                references: 0,
                bounding_box: crate::structure::BoundingBox {
                    min: [0, 0, 0],
                    max: [0, 0, 0],
                },
                pieces: Vec::new(),
                terrain_adaptation: crate::structure::TerrainAdjustment::None,
                pieces_complete: true,
                mineshaft_tree: None,
            })
        }

        let registry = crate::structure::StructureRegistry::new(0, &ResolverForOrder);
        let starts = [
            start("minecraft:mansion"),
            start("minecraft:mineshaft"),
            start("minecraft:fortress"),
            start("minecraft:ancient_city"),
        ];
        let structure_refs = StructureRefs {
            entries: starts
                .into_iter()
                .enumerate()
                .map(|(source_order, start)| (source_order as i32, 0, start))
                .collect(),
        };
        let mut entries = structure_refs.entries.iter().collect::<Vec<_>>();
        let source_order = entries
            .iter()
            .map(|(_, _, start)| start.structure.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            source_order,
            [
                "minecraft:mansion",
                "minecraft:mineshaft",
                "minecraft:fortress",
                "minecraft:ancient_city"
            ]
        );

        order_structure_entries(&registry, &mut entries);
        let replay_order = entries
            .iter()
            .map(|(_, _, start)| start.structure.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            replay_order,
            [
                "minecraft:mineshaft",
                "minecraft:mansion",
                "minecraft:ancient_city",
                "minecraft:fortress"
            ]
        );
        assert_ne!(source_order, replay_order);
    }
}
