//! Stages 1-4 of [`OverworldGenerator::column`]: the per-chunk aquifer, the
//! `fillFromNoise` shape pass, the surface-rule diff, materialisation into a dense
//! grid, and carvers — plus the uncached body of `pre_ore_stage`.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A; see [`super`]'s own module
//! doc for the pipeline order and for every measurement behind these stages.

use std::cell::RefCell;
use std::sync::Arc;

use lodestone_worldgen_core::hash::FastMap;

use crate::aquifer::{
    AquiferRegionCache, AquiferSystem, AnyPositionalFactory, BlockKind,
    CompiledAquiferPointRoutes, VerticalRunState,
};
use crate::biome::BiomeSearchCursor;
use crate::carver::{CarveGrid, CarveObserver, CarverConfig, TouchedMask};
use crate::density::{Density, NoiseChunkRegionSampler};
use crate::engine::Bounds;
use crate::engine::{PointProgram, Program, XzProductLattice};
use crate::dense_grid::BaseStateFacts;
use lodestone_data::block_states::StateId;
use lodestone_data::biomes::BiomeRef;
use crate::rng::RandomSource;
use crate::surface::{PreClass, PreState, SurfaceDiff};

use super::{OverworldGenerator, PreOreResult};

/// Packed pre-surface fill output. The carrier uses the same `u16` width as
/// the dense grid's final block indices, so materialisation can rewrite it in
/// place instead of allocating a second full-column buffer.
#[derive(Debug)]
pub(super) struct PackedShapeField {
    pub(super) blocks: Vec<u16>,
    pub(super) height: i32,
}

pub(crate) struct PackedStateCarrier {
    base_x: i32,
    base_z: i32,
    min_y: i32,
    height: i32,
    blocks: Vec<u16>,
    classes: Vec<u8>,
    vein_batch: Option<super::veins::VeinBatch>,
}

impl PackedStateCarrier {
    pub(super) fn from_field(
        generator: &OverworldGenerator,
        field: PackedShapeField,
        base_x: i32,
        base_z: i32,
    ) -> Self {
        let PackedShapeField { mut blocks, height } = field;
        let vein_batch = generator.veins.as_ref().map(|programs| {
            programs
                .for_chunk(
                    generator.slot_count,
                    base_x,
                    base_z,
                    generator.min_y,
                    generator.height,
                )
                .prepare_batch_packed(
                    &blocks,
                    base_x,
                    base_z,
                    generator.min_y,
                    generator.height,
                )
        });
        let mut classes = Vec::with_capacity(blocks.len());
        for block in &mut blocks {
            let (state, class) = match *block {
                0 => (StateId::AIR, 0),
                1 => (generator.default_block_pre.state, 2),
                2 => (generator.default_fluid_pre.state, 1),
                3 => (generator.default_lava_pre.state, 1),
                other => panic!("invalid packed fill block kind: {other}"),
            };
            *block = u16::try_from(state.raw()).expect("generated state id fits packed carrier");
            classes.push(class);
        }
        Self {
            base_x,
            base_z,
            min_y: generator.min_y,
            height,
            blocks,
            classes,
            vein_batch,
        }
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let lx = x - self.base_x;
        let ly = y - self.min_y;
        let lz = z - self.base_z;
        if (0..16).contains(&lx) && (0..self.height).contains(&ly) && (0..16).contains(&lz) {
            Some(((ly * 16 + lz) * 16 + lx) as usize)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn base_x(&self) -> i32 { self.base_x }

    #[inline]
    pub(crate) fn base_z(&self) -> i32 { self.base_z }

    #[inline]
    pub(crate) fn pre_state(&self, x: i32, y: i32, z: i32) -> PreState {
        let Some(index) = self.index(x, y, z) else { return PreState::AIR };
        PreState {
            state: StateId::from_raw(self.blocks[index]),
            class: match self.classes[index] {
                0 => PreClass::Air,
                1 => PreClass::Fluid,
                2 => PreClass::Stone,
                other => panic!("invalid packed state class: {other}"),
            },
        }
    }

    #[inline]
    pub(crate) fn pre_code(&self, x: i32, y: i32, z: i32) -> u8 {
        let Some(index) = self.index(x, y, z) else { return 0 };
        match self.classes[index] {
            0 => 0,
            1 => 2,
            2 => 1,
            other => panic!("invalid packed state class: {other}"),
        }
    }

    #[inline]
    pub(crate) fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let Some(index) = self.index(x, y, z) else { return };
        self.blocks[index] = u16::try_from(state.raw()).expect("generated state id fits packed carrier");
    }

    fn into_world(
        self,
    ) -> (crate::dense_grid::DenseBlockGrid, OceanFloorState) {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Materialize);
        crate::counters::bump_full_column_conversion((16 * 16 * self.height) as u64);
        let Self {
            base_x,
            base_z,
            min_y,
            height,
            blocks,
            mut vein_batch,
            ..
        } = self;
        let mut ocean_floor = OceanFloorState::new(min_y);
        ocean_floor.configure(base_x, base_z, height);
        let world = crate::dense_grid::DenseBlockGrid::from_ordered_packed_state_fn(
            base_x,
            min_y,
            base_z,
            16,
            height,
            16,
            StateId::AIR,
            blocks,
            |x, y, z, index, raw| {
                let vein_state = vein_batch
                    .as_mut()
                    .and_then(|batch| batch.state_at_index(index));
                let state = vein_state.unwrap_or_else(|| StateId::from_raw(raw));
                let facts = base_facts(state);
                ocean_floor.observe(x - base_x, y, z - base_z, facts);
                state
            },
        );
        assert!(
            vein_batch
                .as_ref()
                .is_none_or(super::veins::VeinBatch::is_consumed),
            "ordered materialisation must consume every vein candidate"
        );
        (world, ocean_floor)
    }
}

#[inline]
fn base_facts(state: StateId) -> BaseStateFacts {
    let block = state.block();
    BaseStateFacts::Builtin {
        is_air: matches!(
            block,
            lodestone_data::block::Block::Air
                | lodestone_data::block::Block::CaveAir
                | lodestone_data::block::Block::VoidAir
        ),
        is_fluid: matches!(
            block,
            lodestone_data::block::Block::Water | lodestone_data::block::Block::Lava
        ),
        blocks_motion: lodestone_data::block_solidity::blocks_motion(state),
    }
}

/// Baseline ocean-floor heights produced as the packed materialisation walk
/// visits final states. Later stages only replace entries for touched columns.
const OCEAN_FLOOR_WORDS_PER_COLUMN: usize = 6;
const OCEAN_FLOOR_OCCUPANCY_WORDS: usize = 256 * OCEAN_FLOOR_WORDS_PER_COLUMN;

#[derive(Debug, Clone)]
struct OceanFloorState {
    heights: [i32; 256],
    touched: TouchedMask,
    min_y: i32,
    base_x: i32,
    base_z: i32,
    height: i32,
    tracked: bool,
    occupancy: [u64; OCEAN_FLOOR_OCCUPANCY_WORDS],
}

impl OceanFloorState {
    fn new(min_y: i32) -> Self {
        Self {
            heights: [min_y; 256],
            touched: [0; 4],
            min_y,
            base_x: 0,
            base_z: 0,
            height: 0,
            tracked: false,
            occupancy: [0; OCEAN_FLOOR_OCCUPANCY_WORDS],
        }
    }

    fn configure(&mut self, base_x: i32, base_z: i32, height: i32) {
        debug_assert!(height > 0 && height <= (OCEAN_FLOOR_WORDS_PER_COLUMN * 64) as i32);
        self.base_x = base_x;
        self.base_z = base_z;
        self.height = height;
        self.tracked = true;
    }

    #[inline]
    fn observe(&mut self, lx: i32, y: i32, lz: i32, facts: BaseStateFacts) {
        if !facts.is_ocean_floor() {
            return;
        }
        let column = (lz * 16 + lx) as usize;
        self.heights[column] = y + 1;
        if self.tracked {
            let local_y = y - self.min_y;
            debug_assert!((0..self.height).contains(&local_y));
            let word = column * OCEAN_FLOOR_WORDS_PER_COLUMN + local_y as usize / 64;
            self.occupancy[word] |= 1u64 << (local_y as usize % 64);
        }
    }

    #[inline]
    fn observe_mutation(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        old: BaseStateFacts,
        new: StateId,
    ) {
        if !self.tracked {
            return;
        }
        let lx = x - self.base_x;
        let lz = z - self.base_z;
        let local_y = y - self.min_y;
        if !(0..16).contains(&lx)
            || !(0..16).contains(&lz)
            || !(0..self.height).contains(&local_y)
        {
            return;
        }
        let new_facts = base_facts(new);
        if old.is_ocean_floor() == new_facts.is_ocean_floor() {
            return;
        }
        let column = (lz * 16 + lx) as usize;
        let word = column * OCEAN_FLOOR_WORDS_PER_COLUMN + local_y as usize / 64;
        let bit = 1u64 << (local_y as usize % 64);
        self.touched[column / 64] |= 1u64 << (column % 64);
        if new_facts.is_ocean_floor() {
            self.occupancy[word] |= bit;
        } else {
            self.occupancy[word] &= !bit;
        }
    }

    #[inline]
    fn tracked_height(&self, column: usize) -> i32 {
        debug_assert!(column < 256);
        let first = column * OCEAN_FLOOR_WORDS_PER_COLUMN;
        for offset in (0..OCEAN_FLOOR_WORDS_PER_COLUMN).rev() {
            let word = self.occupancy[first + offset];
            if word != 0 {
                let bit = (u64::BITS - 1 - word.leading_zeros()) as i32;
                let local_y = (offset * 64) as i32 + bit;
                if local_y < self.height {
                    return self.min_y + local_y + 1;
                }
            }
        }
        self.min_y
    }

}

#[inline]
fn pack_block_kind(block: BlockKind) -> u16 {
    match block {
        BlockKind::Air => 0,
        BlockKind::Stone => 1,
        BlockKind::Water => 2,
        BlockKind::Lava => 3,
    }
}

#[inline]
fn unpack_block_kind(block: u16) -> BlockKind {
    match block {
        0 => BlockKind::Air,
        1 => BlockKind::Stone,
        2 => BlockKind::Water,
        3 => BlockKind::Lava,
        other => panic!("invalid packed fill block kind: {other}"),
    }
}

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
/// The cell geometry is captured alongside the trees because it is part of the
/// settings contract, not a universal 4×8 default.
#[allow(missing_debug_implementations)]
pub(super) struct AquiferTrees {
    /// The three routes that become [`NoiseChunkSampler`]s, held as compiled
    /// [`Program`]s: cloning one is an `Arc` bump plus a `u32` copy.
    pub(super) final_density: Program,
    pub(super) erosion: Program,
    pub(super) depth: Program,
    /// The four point-evaluated aquifer routes, behind `Arc` for the same
    /// reason. Compound routes share one compiled bundle for every chunk.
    pub(super) barrier: Arc<Density>,
    pub(super) floodedness: Arc<Density>,
    pub(super) spread: Arc<Density>,
    pub(super) lava: Arc<Density>,
    pub(super) point_programs: CompiledAquiferPointRoutes,
    pub(super) prelim: Arc<Density>,
    /// Compiled point path for `prelim`, shared by every chunk-bound aquifer.
    /// Its evaluator scratch remains inside each [`AquiferSystem`].
    pub(super) prelim_program: Arc<PointProgram>,
    /// Fingerprint shared by the generator's final and preliminary programs
    /// when their factor/offset routes admitted the pure-X/Z product plan.
    /// `None` means the route remains on the ordinary exact evaluator.
    pub(super) xz_product_fingerprint: Option<u64>,
    pub(super) positional: AnyPositionalFactory,
    pub(super) cell_width: i32,
    pub(super) cell_height: i32,
}

impl OverworldGenerator {
    /// Maximum chunk side for one request-scoped density region. Larger unions
    /// are split into bounded eight-by-eight chunk tiles so the dense sampler remains
    /// local without giving up sharing for normal decoration windows.
    const PRE_ORE_REGION_MAX_SIDE: i32 = 8;
    const PRE_ORE_REGION_TILE_SIDE: i32 = 8;

    /// Prepares the terrain-prefix closure used by one production target.
    pub fn prepare_pre_ore_batch(&self, cx: i32, cz: i32) -> usize {
        self.prepare_pre_ore_targets(&[(cx, cz)])
    }

    /// Prepares nearby production targets while keeping each density sampler
    /// within the cache-efficient five-by-five request closure.
    pub fn prepare_pre_ore_targets(&self, targets: &[(i32, i32)]) -> usize {
        self.prepare_pre_ore_targets_with_radius(targets, super::COLUMN_CLOSURE_RADIUS)
    }

    /// Prepares the terrain halo required by a target-owned decoration pass.
    pub fn prepare_pre_ore_targets_with_radius(
        &self,
        targets: &[(i32, i32)],
        radius: i32,
    ) -> usize {
        assert!(radius >= 0, "pre-ore radius must be non-negative");
        let preliminary = self.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        let positions = pre_ore_target_union(targets, radius);
        self.prepare_pre_ore_position_union_with_lease(&positions, None, &preliminary)
    }

    /// Prepares target-local terrain while an enclosing production batch lease
    /// is live. Each target keeps its own bounded density sampler and exact
    /// traversal order; only the store's repeated pin/unpin work is removed.
    pub(super) fn prepare_pre_ore_targets_with_lease(
        &self,
        targets: &[(i32, i32)],
        radius: i32,
        lease: &super::OverworldBatchLease<'_>,
    ) -> usize {
        assert!(radius >= 0, "pre-ore radius must be non-negative");
        let preliminary = Arc::clone(&lease.preliminary);
        let positions = pre_ore_target_union(targets, radius);
        self.prepare_pre_ore_position_union_with_lease(&positions, Some(&lease.view), &preliminary)
    }

    /// Prepares exactly the supplied chunk-position union before callers fetch
    /// its memoised `pre_ore_stage` products. Positions are deduplicated in
    /// first-seen order and large unions are split into deterministic bounded
    /// regions.
    pub(super) fn prepare_pre_ore_position_union(&self, positions: &[(i32, i32)]) -> usize {
        let preliminary = self.preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
        self.prepare_pre_ore_position_union_with_lease(positions, None, &preliminary)
    }

    fn prepare_pre_ore_position_union_with_lease(
        &self,
        positions: &[(i32, i32)],
        existing_lease: Option<&crate::overworld::store::ViewScope<'_, super::ChunkStages>>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> usize {
        let positions = canonical_pre_ore_positions(positions.iter().copied());
        if positions.is_empty() {
            return 0;
        }
        split_pre_ore_regions(
            positions,
            Self::PRE_ORE_REGION_MAX_SIDE,
            Self::PRE_ORE_REGION_TILE_SIDE,
        )
        .into_iter()
        .map(|region| self.prepare_pre_ore_region(region, existing_lease, preliminary))
        .sum()
    }

    fn prepare_pre_ore_region(
        &self,
        positions: Vec<(i32, i32)>,
        existing_lease: Option<&crate::overworld::store::ViewScope<'_, super::ChunkStages>>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> usize {
        let Some(&(min_x, min_z)) = positions.first() else {
            return 0;
        };
        let (mut max_x, mut max_z) = (min_x, min_z);
        let (mut low_x, mut low_z) = (min_x, min_z);
        for &(x, z) in &positions[1..] {
            low_x = low_x.min(x);
            low_z = low_z.min(z);
            max_x = max_x.max(x);
            max_z = max_z.max(z);
        }
        let centre = ((low_x + max_x).div_euclid(2), (low_z + max_z).div_euclid(2));
        let radius = positions.iter().fold(0, |radius, &(x, z)| {
            radius.max((x - centre.0).abs()).max((z - centre.1).abs())
        });
        let local_lease;
        let lease = match existing_lease {
            Some(lease) => lease,
            None => {
                local_lease = self.store.open_view(centre, radius);
                &local_lease
            }
        };
        let region_positions = positions.clone();
        let missing_positions = positions
            .iter()
            .copied()
            .filter(|position| self.store.entry(*position).pre_ore.peek().is_none())
            .collect::<Vec<_>>();
        let mut region = None;
        let compute = |position| {
            region
                .get_or_insert_with(|| {
                    super::region_prefix::RegionPrefixBatch::execute(
                        self,
                        &region_positions,
                        &missing_positions,
                        preliminary,
                    )
                })
                .result(position)
                .as_ref()
                .clone()
        };
        let prepared = self.store.compute_stage_batch_in_view(
            lease,
            positions,
            |entry| &entry.pre_ore,
            crate::counters::bump_pre_ore,
            compute,
        );
        prepared.len()
    }

    pub(super) fn pre_ore_stage_uncached_with_preliminary_cache(
        &self,
        cx: i32,
        cz: i32,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> PreOreResult {
        let sampler = NoiseChunkRegionSampler::from_program(
            self.aquifer_trees.final_density.clone(),
            self.slot_count,
            self.aquifer_trees.cell_width,
            self.aquifer_trees.cell_height,
            Bounds {
                x: (cx * 16, cx * 16 + 15),
                y: (self.min_y, self.min_y + self.height - 1),
                z: (cz * 16, cz * 16 + 15),
            },
        );
        self.pre_ore_stage_uncached_with_sampler(cx, cz, Some(&sampler), preliminary)
    }

    fn pre_ore_stage_uncached_with_sampler(
        &self,
        cx: i32,
        cz: i32,
        region_sampler: Option<&NoiseChunkRegionSampler>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> PreOreResult {
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut schedule = crate::stage_schedule::OVERWORLD.cursor();

        let aquifer = self.build_aquifer_with_preliminary_cache(cx, cz, preliminary);
        // Structure placement's S3. Built here rather than passed in because the *only*
        // consumer is the fill below, and it must be built from this chunk's own
        // refs — a beardifier from a neighbouring chunk has a different junction
        // window and a different affected box.
        schedule.enter(crate::stage_schedule::ColumnStage::StructureStarts);
        schedule.enter(crate::stage_schedule::ColumnStage::StructureReferences);
        let beard = self.beardifier_for(cx, cz);
        schedule.enter(crate::stage_schedule::ColumnStage::StructureInfluence);
        schedule.enter(crate::stage_schedule::ColumnStage::Fill);
        let (field, heights) = self.fill_stage_packed_with_sampler(
            &aquifer,
            base_x,
            base_z,
            &beard,
            region_sampler,
        );
        let mut biome_cursor = self
            .dynamic_biome
            .as_ref()
            .map(|dynamic| dynamic.table.search_cursor());
        let climate_grid = self.prepare_climate_grid(base_x, base_z);
        // The 4x4x4 grid is now the primary biome product and the
        // 16-entry surface array is read out of it. Two separate sample passes
        // would be two chances to diverge; see `biome_stage`.
        schedule.enter(crate::stage_schedule::ColumnStage::Biomes);
        let biome_cells = self.biome_cells_stage_with_prepared(
            base_x,
            base_z,
            biome_cursor.as_mut().map(|cursor| cursor),
            climate_grid.as_deref(),
        );
        let biome_quarts = self.biome_stage(&biome_cells, &heights);
        schedule.enter(crate::stage_schedule::ColumnStage::Surface);
        let surface_diff = self.surface_stage_packed_with_preliminary_cache(
            &field,
            &heights,
            base_x,
            base_z,
            biome_cursor.as_mut().map(|cursor| cursor),
            climate_grid,
            preliminary,
        );

        schedule.enter(crate::stage_schedule::ColumnStage::Materialize);
        let (world, ocean_floor) = self.materialize_world_packed(field, surface_diff, base_x, base_z);
        let ocean_floor = std::rc::Rc::new(std::cell::RefCell::new(ocean_floor));
        let mutation_observer: super::BlockMutationObserverHandle = std::rc::Rc::new({
            let ocean_floor = std::rc::Rc::clone(&ocean_floor);
            move |x, y, z, old, new| ocean_floor.borrow_mut().observe_mutation(x, y, z, old, new)
        });
        schedule.enter(crate::stage_schedule::ColumnStage::Carvers);
        let world = {
            let mutation_observer = std::rc::Rc::clone(&mutation_observer);
            self.carve_stage_with_touched_and_observer(
                cx,
                cz,
                &aquifer,
                &heights,
                &biome_quarts,
                base_x,
                base_z,
                world,
                biome_cursor.as_mut().map(|cursor| cursor),
                None,
                Some(mutation_observer),
            )
        };
        // Structure placement's S2. A no-op (and free) for a generator with no structure
        // data, which is every fixture resolver in this workspace.
        schedule.enter(crate::stage_schedule::ColumnStage::StructurePlacement);
        let world = {
            let mutation_observer = std::rc::Rc::clone(&mutation_observer);
            self.structure_place_stage_with_touched_and_observer(
                cx,
                cz,
                world,
                None,
                Some(mutation_observer),
            )
        };
        // Ores consult the live `OCEAN_FLOOR_WG` map while deciding whether a
        // blob is buried. Unlike the surface and biome heights above, this map
        // must see the completed pre-ore terrain: a carver can lower a column
        // enough to cull a feature before it draws its blob radii.
        drop(mutation_observer);
        let ocean_floor = match std::rc::Rc::try_unwrap(ocean_floor) {
            Ok(state) => state.into_inner(),
            Err(_) => panic!("ocean-floor observer remained live after pre-ore stages"),
        };
        let ore_heights = self.ore_heights_from_ocean_floor_state(&world, ocean_floor);

        // `Arc` because `PreOreResult` hands this world out to
        // the unified FEATURES stage's rim sources rather than only into a mutating
        // consumer — see that alias's own doc.
        schedule.finish_prefix(
            crate::stage_schedule::OVERWORLD.shaped_boundary_index(),
        );
        (Arc::new(world), ore_heights, biome_quarts, Arc::new(biome_cells))
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
        self.build_aquifer_with_preliminary_cache(cx, cz, &self.preliminary_region)
    }

    pub(super) fn build_aquifer_with_preliminary_cache(
        &self,
        cx: i32,
        cz: i32,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> AquiferSystem {
        self.build_aquifer_with_preliminary_cache_and_products(cx, cz, preliminary, None)
    }

    /// Builds a chunk-bound aquifer while sharing the request's pure-X/Z
    /// products with both its final-density and preliminary routes. The
    /// product fingerprint is checked by the compiled evaluators; `None`
    /// retains the ordinary exact path for scalar callers.
    pub(super) fn build_aquifer_with_preliminary_cache_and_products(
        &self,
        cx: i32,
        cz: i32,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
        xz_products: Option<Arc<XzProductLattice>>,
    ) -> AquiferSystem {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Aquifer);
        let t = &self.aquifer_trees;
        AquiferSystem::from_parts_with_preliminary_cache_and_point_programs(
            t.final_density.clone(),
            t.erosion.clone(),
            t.depth.clone(),
            t.barrier.clone(),
            t.floodedness.clone(),
            t.spread.clone(),
            t.lava.clone(),
            t.prelim.clone(),
            t.prelim_program.clone(),
            t.point_programs.clone(),
            t.positional,
            self.sea_level,
            self.min_y,
            self.height,
            cx,
            cz,
            self.slot_count,
            t.cell_width,
            t.cell_height,
            Arc::clone(preliminary),
            xz_products,
        )
    }

    /// Stage 1: `fillFromNoise` — shape + the **real** aquifer,
    /// replacing the sea-level approximation this generator used before.
    /// Returns the dense field and its solid-top heights in one pass.
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
    ) -> (Vec<BlockKind>, [i32; 256]) {
        let (packed, heights) = self.fill_stage_packed_with_sampler(
            aquifer,
            base_x,
            base_z,
            beard,
            None,
        );
        let field = packed.blocks.into_iter().map(unpack_block_kind).collect();
        (field, heights)
    }

    pub(super) fn fill_stage_packed_with_sampler(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &crate::structure::beardifier::Beardifier,
        region_sampler: Option<&NoiseChunkRegionSampler>,
    ) -> (PackedShapeField, [i32; 256]) {
        self.fill_stage_packed_with_region(
            aquifer,
            base_x,
            base_z,
            beard,
            region_sampler,
            None,
        )
    }

    pub(super) fn fill_stage_packed_with_region(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &crate::structure::beardifier::Beardifier,
        region_sampler: Option<&NoiseChunkRegionSampler>,
        region_aquifer: Option<&mut AquiferRegionCache>,
    ) -> (PackedShapeField, [i32; 256]) {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Shape);
        crate::counters::bump_full_column_scan((16 * 16 * self.height) as u64);
        let height = self.height as usize;
        let mut field = vec![pack_block_kind(BlockKind::Air); 16 * 16 * height];
        let mut heights = [self.min_y - 1; 256];
        let use_cells = region_sampler.is_some_and(|region| {
            region.supports_final_density_cells()
                && self.min_y.rem_euclid(8) == 0
                && self.height.rem_euclid(8) == 0
        });
        if use_cells {
            self.fill_stage_cells(
                aquifer,
                base_x,
                base_z,
                beard,
                region_sampler.expect("cell path selected a region sampler"),
                &mut field,
                &mut heights,
                region_aquifer,
            );
            for height in &mut heights {
                *height = (*height).max(self.sea_level - 1);
            }
            return (PackedShapeField { blocks: field, height: self.height }, heights);
        }
        let mut densities = vec![0.0; height];
        let mut blocks = vec![BlockKind::Air; height];
        if beard.is_empty() {
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let (wx, wz) = (base_x + lx, base_z + lz);
                    match region_sampler {
                        Some(region) => {
                            region.final_density_column(wx, wz, self.min_y, &mut densities)
                        }
                        None => aquifer.final_density_column(wx, wz, self.min_y, &mut densities),
                    }
                    aquifer.block_at_density_vertical_run(
                        wx,
                        wz,
                        self.min_y,
                        &densities,
                        &mut blocks,
                    );
                    for ly in 0..self.height {
                        let wy = self.min_y + ly;
                        let block = blocks[ly as usize];
                        field[Self::idx(lx, ly, lz, self.height)] = pack_block_kind(block);
                        if block == BlockKind::Stone {
                            heights[(lz * 16 + lx) as usize] = wy;
                        }
                    }
                }
            }
        } else {
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    let (wx, wz) = (base_x + lx, base_z + lz);
                    match region_sampler {
                        Some(region) => {
                            region.final_density_column(wx, wz, self.min_y, &mut densities)
                        }
                        None => aquifer.final_density_column(wx, wz, self.min_y, &mut densities),
                    }
                    for ly in 0..self.height {
                        let wy = self.min_y + ly;
                        densities[ly as usize] += beard.compute(wx, wy, wz);
                    }
                    aquifer.block_at_density_vertical_run(
                        wx,
                        wz,
                        self.min_y,
                        &densities,
                        &mut blocks,
                    );
                    for ly in 0..self.height {
                        let wy = self.min_y + ly;
                        let block = blocks[ly as usize];
                        field[Self::idx(lx, ly, lz, self.height)] = pack_block_kind(block);
                        if block == BlockKind::Stone {
                            heights[(lz * 16 + lx) as usize] = wy;
                        }
                    }
                }
            }
        }
        for height in &mut heights {
            *height = (*height).max(self.sea_level - 1);
        }
        (PackedShapeField { blocks: field, height: self.height }, heights)
    }

    fn fill_stage_cells(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &crate::structure::beardifier::Beardifier,
        sampler: &NoiseChunkRegionSampler,
        field: &mut [u16],
        heights: &mut [i32; 256],
        mut region_aquifer: Option<&mut AquiferRegionCache>,
    ) {
        let mut densities = [0.0; 128];
        let mut vertical_densities = [0.0; 8];
        let mut vertical_blocks = [BlockKind::Air; 8];
        let empty_beard = beard.is_empty();
        for cell_z in 0..4i32 {
            for cell_x in 0..4i32 {
                // Keep one tiny recurrence state per XZ column while its
                // adjacent eight-block Y slices are consumed. The states are
                // request-local and never cross an XZ column.
                let mut vertical_run_states: [VerticalRunState; 16] =
                    std::array::from_fn(|_| aquifer.vertical_run_state());
                for cell_y in 0..(self.height / 8) {
                    let x0 = base_x + cell_x * 4;
                    let y0 = self.min_y + cell_y * 8;
                    let z0 = base_z + cell_z * 4;
                    let solid_cell = if empty_beard && sampler.supports_final_density_cells() {
                        sampler.final_density_cell_or_positive(x0, y0, z0, &mut densities)
                            || Self::cell_densities_are_positive(&densities)
                    } else {
                        sampler.final_density_cell(x0, y0, z0, &mut densities);
                        empty_beard && Self::cell_densities_are_positive(&densities)
                    };
                    if solid_cell {
                        Self::write_solid_cell(
                            cell_x,
                            cell_y,
                            cell_z,
                            y0,
                            self.height,
                            field,
                            heights,
                            &mut vertical_run_states,
                        );
                        continue;
                    }
                    for lz in 0..4i32 {
                        for lx in 0..4i32 {
                            let wx = x0 + lx;
                            let wz = z0 + lz;
                            let density_start = ((lz * 4 + lx) * 8) as usize;
                            if empty_beard {
                                vertical_densities.copy_from_slice(
                                    &densities[density_start..density_start + 8],
                                );
                            } else {
                                for ly in 0..8i32 {
                                    let wy = y0 + ly;
                                    vertical_densities[ly as usize] =
                                        densities[density_start + ly as usize]
                                            + beard.compute(wx, wy, wz);
                                }
                            }
                            match region_aquifer.as_deref_mut() {
                                Some(cache) => aquifer
                                    .block_at_density_vertical_slice_with_region_cache(
                                        wx,
                                        wz,
                                        y0,
                                        &vertical_densities,
                                        &mut vertical_blocks,
                                        &mut vertical_run_states[(lz * 4 + lx) as usize],
                                        cache,
                                    ),
                                None => aquifer.block_at_density_vertical_slice(
                                    wx,
                                    wz,
                                    y0,
                                    &vertical_densities,
                                    &mut vertical_blocks,
                                    &mut vertical_run_states[(lz * 4 + lx) as usize],
                                ),
                            }
                            for ly in 0..8i32 {
                                let wy = y0 + ly;
                                let block = vertical_blocks[ly as usize];
                                field[Self::idx(
                                    cell_x * 4 + lx,
                                    cell_y * 8 + ly,
                                    cell_z * 4 + lz,
                                    self.height,
                                )] = pack_block_kind(block);
                                if block == BlockKind::Stone {
                                    heights[(cell_z * 4 + lz) as usize * 16
                                        + (cell_x * 4 + lx) as usize] = wy;
                                }
                            }
                        }
                    }
                }
            }
        }
        }

    #[inline]
    fn cell_densities_are_positive(densities: &[f64; 128]) -> bool {
        densities.iter().all(|density| *density > 0.0)
    }

    #[inline]
    fn write_solid_cell(
        cell_x: i32,
        cell_y: i32,
        cell_z: i32,
        y0: i32,
        height: i32,
        field: &mut [u16],
        heights: &mut [i32; 256],
        vertical_run_states: &mut [VerticalRunState; 16],
    ) {
        let stone = pack_block_kind(BlockKind::Stone);
        for lz in 0..4i32 {
            for lx in 0..4i32 {
                let column = ((cell_z * 4 + lz) * 16 + cell_x * 4 + lx) as usize;
                heights[column] = y0 + 7;
                vertical_run_states[(lz * 4 + lx) as usize] = VerticalRunState::default();
                for ly in 0..8i32 {
                    field[Self::idx(
                        cell_x * 4 + lx,
                        cell_y * 8 + ly,
                        cell_z * 4 + lz,
                        height,
                    )] = stone;
                }
            }
        }
    }

    /// The live `OCEAN_FLOOR_WG` heightmap ores probe after surface, carving,
    /// and structure placement. The result is one above the topmost
    /// motion-blocking block, or `min_y` for an empty column.
    ///
    /// This is intentionally separate from the fill-derived solid-top height:
    /// that earlier field-derived value is the surface-rule input and must remain
    /// stable while carving mutates the materialised grid.
    pub(super) fn ore_heights_from_world(&self, world: &crate::dense_grid::DenseBlockGrid) -> [i32; 256] {
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        assert_eq!(min_y, self.min_y, "pre-ore world min_y drifted from its generator");
        assert_eq!(size_y, self.height, "pre-ore world height drifted from its generator");
        assert_eq!(size_x, 16, "pre-ore world must cover one chunk in x");
        assert_eq!(size_z, 16, "pre-ore world must cover one chunk in z");
        Self::ocean_floor_wg_heights(world, min_x, min_y, min_z, size_y)
    }

    fn ore_heights_from_ocean_floor_state(
        &self,
        world: &crate::dense_grid::DenseBlockGrid,
        state: OceanFloorState,
    ) -> [i32; 256] {
        let (_, min_y, _, size_x, size_y, size_z) = world.bounds();
        assert_eq!(min_y, state.min_y, "ocean-floor baseline min_y drifted");
        assert_eq!(min_y, self.min_y, "pre-ore world min_y drifted from its generator");
        assert_eq!(size_y, self.height, "pre-ore world height drifted from its generator");
        assert_eq!(size_x, 16, "pre-ore world must cover one chunk in x");
        assert_eq!(size_z, 16, "pre-ore world must cover one chunk in z");

        Self::ocean_floor_wg_heights_from_state(world, state)
    }

    fn ocean_floor_wg_heights_from_state(
        world: &crate::dense_grid::DenseBlockGrid,
        state: OceanFloorState,
    ) -> [i32; 256] {
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        debug_assert_eq!(min_y, state.min_y);
        debug_assert_eq!(size_x, 16);
        debug_assert_eq!(size_z, 16);
        let mut heights = state.heights;
        if state.tracked {
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let column = lz * 16 + lx;
                    if crate::carver::touched_column(&state.touched, lx as i32, lz as i32) {
                        heights[column] = state.tracked_height(column);
                    }
                }
            }
            return heights;
        }
        let mut scanned_cells = 0u64;
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                if !crate::carver::touched_column(&state.touched, lx, lz) {
                    continue;
                }
                let (height, cells) = Self::ocean_floor_wg_height(
                    world,
                    min_x + lx,
                    min_y,
                    min_z + lz,
                    size_y,
                );
                heights[(lz * 16 + lx) as usize] = height;
                scanned_cells += cells;
            }
        }
        if scanned_cells != 0 {
            crate::counters::bump_full_column_scan(scanned_cells);
        }
        heights
    }

    fn ocean_floor_wg_height(
        world: &crate::dense_grid::DenseBlockGrid,
        x: i32,
        min_y: i32,
        z: i32,
        height: i32,
    ) -> (i32, u64) {
        let mut scanned_cells = 0u64;
        for y in (min_y..min_y + height).rev() {
            scanned_cells += 1;
            if world.get_base_facts(x, y, z).is_ocean_floor() {
                return (y + 1, scanned_cells);
            }
        }
        (min_y, scanned_cells)
    }

    fn ocean_floor_wg_heights(
        world: &crate::dense_grid::DenseBlockGrid,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        height: i32,
    ) -> [i32; 256] {
        let mut heights = [min_y; 256];
        let mut scanned_cells = 0u64;
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let (column_height, cells) =
                    Self::ocean_floor_wg_height(world, min_x + lx, min_y, min_z + lz, height);
                heights[(lz * 16 + lx) as usize] = column_height;
                scanned_cells += cells;
            }
        }
        crate::counters::bump_full_column_scan(scanned_cells);
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
    /// just been pointed at. The cause was entirely representational: state
    /// names crossed the pre-surface and rule-evaluation boundaries, and
    /// `biome_at` cloned a biome name per column (0.35%). Nothing about the
    /// *scan* changed here — see
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
    pub(super) fn surface_stage_with_preliminary_cache(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        base_x: i32,
        base_z: i32,
        cursor: Option<&mut BiomeSearchCursor>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> SurfaceDiff {
        self.surface_stage_with_prepared_and_preliminary_cache(
            field,
            heights,
            base_x,
            base_z,
            cursor,
            None,
            preliminary,
        )
    }

    fn surface_stage_with_prepared_and_preliminary_cache(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        base_x: i32,
        base_z: i32,
        cursor: Option<&mut BiomeSearchCursor>,
        prepared: Option<Arc<crate::biome::PreparedClimateGrid>>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> SurfaceDiff {
        self.surface_stage_with_prepared_and_preliminary_cache_using(
            &|lx, y, lz| match field[Self::idx(lx, y - self.min_y, lz, self.height)] {
                BlockKind::Stone => self.default_block_pre,
                BlockKind::Water => self.default_fluid_pre,
                BlockKind::Lava => self.default_lava_pre,
                BlockKind::Air => PreState::AIR,
            },
            heights,
            base_x,
            base_z,
            cursor,
            prepared,
            preliminary,
        )
    }

    fn surface_stage_packed_with_preliminary_cache(
        &self,
        field: &PackedShapeField,
        heights: &[i32; 256],
        base_x: i32,
        base_z: i32,
        cursor: Option<&mut BiomeSearchCursor>,
        prepared: Option<Arc<crate::biome::PreparedClimateGrid>>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> SurfaceDiff {
        self.surface_stage_with_prepared_and_preliminary_cache_using(
            &|lx, y, lz| match field.blocks[Self::idx(lx, y - self.min_y, lz, field.height)] {
                1 => self.default_block_pre,
                2 => self.default_fluid_pre,
                3 => self.default_lava_pre,
                0 => PreState::AIR,
                other => panic!("invalid packed fill block kind: {other}"),
            },
            heights,
            base_x,
            base_z,
            cursor,
            prepared,
            preliminary,
        )
    }

    fn surface_stage_with_prepared_and_preliminary_cache_using(
        &self,
        pre: &dyn Fn(i32, i32, i32) -> PreState,
        heights: &[i32; 256],
        base_x: i32,
        base_z: i32,
        cursor: Option<&mut BiomeSearchCursor>,
        prepared: Option<Arc<crate::biome::PreparedClimateGrid>>,
        preliminary: &Arc<crate::aquifer::PreliminarySurfaceCache>,
    ) -> SurfaceDiff {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Surface);

        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        let cursor_state = cursor.as_ref().map(|value| **value);
        let surface_biomes = RefCell::new(self.surface_biome_context(
            base_x,
            base_z,
            cursor_state,
            prepared,
        ));
        let biome_at = |lx: i32, y: i32, lz: i32| -> (&str, bool) {
            let name = surface_biomes
                .borrow_mut()
                .at_block(base_x + lx, y, base_z + lz);
            let cold = match &self.dynamic_biome {
                Some(dynamic) => crate::biome::cold_enough_to_snow(&dynamic.temperatures, name),
                None => self.fallback_cold_enough_to_snow,
            };
            (name, cold)
        };

        let column_biome_at = |lx: i32, y: i32, lz: i32| {
            surface_biomes
                .borrow_mut()
                .touch_block(base_x + lx, y, base_z + lz);
        };
        let result = self.surface.build_surface_reusing_with_preliminary_cache(
            SurfaceDiff::default(),
            &pre,
            &heightmap,
            &biome_at,
            &column_biome_at,
            base_x,
            base_z,
            preliminary,
        );
        let surface_biomes = surface_biomes.into_inner();
        if let (Some(cursor), Some(updated)) = (cursor, surface_biomes.into_cursor()) {
            *cursor = updated;
        }
        result
    }

    /// Materialises the full `16×height×16` post-surface column into a
    /// dense, world-anchored grid
    /// ([`crate::dense_grid::DenseBlockGrid`], not a `HashMap`) — the shape
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
        crate::counters::bump_full_column_conversion((16 * 16 * self.height) as u64);
        // The sparse diff is already grouped by column and descending Y. Keep a
        // cursor into the active column so materialization performs no map
        // probes, coordinate conversion, or sort before the palette walk.
        let mut surface_column = &[][..];
        let mut surface_column_x = -1;
        let mut surface_column_z = -1;
        let mut next_surface_change = 0;
        // Build the bounded vein product before the palette walk; its cursor
        // follows this closure's z, x, y order.
        let mut vein_batch = self.veins.as_ref().map(|programs| {
            programs
                .for_chunk(self.slot_count, base_x, base_z, self.min_y, self.height)
                .prepare_batch(&field, base_x, base_z, self.min_y, self.height)
        });
        let world = crate::dense_grid::DenseBlockGrid::from_ordered_state_fn(
            base_x,
            self.min_y,
            base_z,
            16,
            self.height,
            16,
            StateId::AIR,
            |x, y, z| {
                let lx = x - base_x;
                let ly = y - self.min_y;
                let lz = z - base_z;
                let index = Self::idx(lx, ly, lz, self.height);
                let base = match field[index] {
                    BlockKind::Stone => self.default_block_pre.state,
                    BlockKind::Water => self.default_fluid_pre.state,
                    BlockKind::Lava => self.default_lava_pre.state,
                    BlockKind::Air => StateId::AIR,
                };
                let vein_state = if base == self.default_block_pre.state {
                    vein_batch
                        .as_mut()
                        .and_then(|batch| batch.state_at_index(index))
                } else {
                    None
                };
                if lx != surface_column_x || lz != surface_column_z {
                    surface_column = surface_diff.column_slice(lx, lz);
                    surface_column_x = lx;
                    surface_column_z = lz;
                    next_surface_change = surface_column.len();
                }
                let surface_state = (next_surface_change != 0
                    && surface_column[next_surface_change - 1].0 == y)
                    .then(|| {
                        next_surface_change -= 1;
                        let state = surface_column[next_surface_change].1;
                        state
                    });
                vein_state.or(surface_state).unwrap_or(base)
            },
        );
        assert!(
            vein_batch
                .as_ref()
                .is_none_or(super::veins::VeinBatch::is_consumed),
            "ordered materialisation must consume every vein candidate"
        );
        world
    }

    /// Materialises the packed fill carrier in place. The callback receives
    /// the original fill code before it is overwritten with the local palette
    /// index, so the fill allocation becomes the dense-grid allocation.
    fn materialize_world_packed(
        &self,
        field: PackedShapeField,
        surface_diff: SurfaceDiff,
        base_x: i32,
        base_z: i32,
    ) -> (crate::dense_grid::DenseBlockGrid, OceanFloorState) {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Materialize);
        crate::counters::bump_full_column_conversion((16 * 16 * self.height) as u64);
        let PackedShapeField { blocks, height } = field;
        debug_assert_eq!(height, self.height);
        let mut ocean_floor = OceanFloorState::new(self.min_y);
        ocean_floor.configure(base_x, base_z, self.height);
        let mut surface_column = &[][..];
        let mut surface_column_x = -1;
        let mut surface_column_z = -1;
        let mut next_surface_change = 0;
        let mut vein_batch = self.veins.as_ref().map(|programs| {
            programs
                .for_chunk(self.slot_count, base_x, base_z, self.min_y, self.height)
                .prepare_batch_packed(&blocks, base_x, base_z, self.min_y, self.height)
        });
        let world = crate::dense_grid::DenseBlockGrid::from_ordered_packed_state_fn(
            base_x,
            self.min_y,
            base_z,
            16,
            self.height,
            16,
            StateId::AIR,
            blocks,
            |x, y, z, index, packed| {
                let lx = x - base_x;
                let lz = z - base_z;
                let base = match packed {
                    1 => self.default_block_pre.state,
                    2 => self.default_fluid_pre.state,
                    3 => self.default_lava_pre.state,
                    0 => StateId::AIR,
                    other => panic!("invalid packed fill block kind: {other}"),
                };
                let vein_state = if base == self.default_block_pre.state {
                    vein_batch
                        .as_mut()
                        .and_then(|batch| batch.state_at_index(index))
                } else {
                    None
                };
                if lx != surface_column_x || lz != surface_column_z {
                    surface_column = surface_diff.column_slice(lx, lz);
                    surface_column_x = lx;
                    surface_column_z = lz;
                    next_surface_change = surface_column.len();
                }
                let surface_state = (next_surface_change != 0
                    && surface_column[next_surface_change - 1].0 == y)
                    .then(|| {
                        next_surface_change -= 1;
                        surface_column[next_surface_change].1
                    });
                let state = vein_state.or(surface_state).unwrap_or(base);
                let facts = base_facts(state);
                ocean_floor.observe(lx, y, lz, facts);
                state
            },
        );
        assert!(
            vein_batch
                .as_ref()
                .is_none_or(super::veins::VeinBatch::is_consumed),
            "ordered materialisation must consume every vein candidate"
        );
        (world, ocean_floor)
    }

    pub(super) fn finish_pre_ore_stage_from_state_carrier(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        heights: [i32; 256],
        biome_quarts: [(BiomeRef, bool); 16],
        biome_cells: Arc<crate::overworld::biome_cells::BiomeCells>,
        carrier: PackedStateCarrier,
    ) -> PreOreResult {
        let base_x = cx * 16;
        let base_z = cz * 16;
        let mut schedule = crate::stage_schedule::OVERWORLD.cursor();
        schedule.enter(crate::stage_schedule::ColumnStage::StructureStarts);
        schedule.enter(crate::stage_schedule::ColumnStage::StructureReferences);
        schedule.enter(crate::stage_schedule::ColumnStage::StructureInfluence);
        schedule.enter(crate::stage_schedule::ColumnStage::Fill);
        schedule.enter(crate::stage_schedule::ColumnStage::Biomes);
        schedule.enter(crate::stage_schedule::ColumnStage::Surface);
        schedule.enter(crate::stage_schedule::ColumnStage::Materialize);
        let (world, ocean_floor) = carrier.into_world();
        let ocean_floor = std::rc::Rc::new(std::cell::RefCell::new(ocean_floor));
        let mutation_observer: super::BlockMutationObserverHandle = std::rc::Rc::new({
            let ocean_floor = std::rc::Rc::clone(&ocean_floor);
            move |x, y, z, old, new| ocean_floor.borrow_mut().observe_mutation(x, y, z, old, new)
        });
        schedule.enter(crate::stage_schedule::ColumnStage::Carvers);
        let world = {
            let mutation_observer = std::rc::Rc::clone(&mutation_observer);
            self.carve_stage_with_touched_and_observer(
                cx,
                cz,
                aquifer,
                &heights,
                &biome_quarts,
                base_x,
                base_z,
                world,
                None,
                None,
                Some(mutation_observer),
            )
        };
        schedule.enter(crate::stage_schedule::ColumnStage::StructurePlacement);
        let world = {
            let mutation_observer = std::rc::Rc::clone(&mutation_observer);
            self.structure_place_stage_with_touched_and_observer(
                cx,
                cz,
                world,
                None,
                Some(mutation_observer),
            )
        };
        drop(mutation_observer);
        let ocean_floor = match std::rc::Rc::try_unwrap(ocean_floor) {
            Ok(state) => state.into_inner(),
            Err(_) => panic!("ocean-floor observer remained live after pre-ore stages"),
        };
        let ore_heights = self.ore_heights_from_ocean_floor_state(&world, ocean_floor);
        schedule.finish_prefix(crate::stage_schedule::OVERWORLD.shaped_boundary_index());
        (Arc::new(world), ore_heights, biome_quarts, biome_cells)
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
        biome_quarts: &[(BiomeRef, bool); 16],
        base_x: i32,
        base_z: i32,
        world: crate::dense_grid::DenseBlockGrid,
        cursor: Option<&mut BiomeSearchCursor>,
    ) -> crate::dense_grid::DenseBlockGrid {
        self.carve_stage_with_touched(
            cx,
            cz,
            aquifer,
            heights,
            biome_quarts,
            base_x,
            base_z,
            world,
            cursor,
            None,
        )
    }

    pub(super) fn carve_stage_with_touched(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        heights: &[i32; 256],
        biome_quarts: &[(BiomeRef, bool); 16],
        base_x: i32,
        base_z: i32,
        world: crate::dense_grid::DenseBlockGrid,
        cursor: Option<&mut BiomeSearchCursor>,
        touched: Option<&mut TouchedMask>,
    ) -> crate::dense_grid::DenseBlockGrid {
        self.carve_stage_with_touched_and_observer(
            cx,
            cz,
            aquifer,
            heights,
            biome_quarts,
            base_x,
            base_z,
            world,
            cursor,
            touched,
            None,
        )
    }

    pub(super) fn carve_stage_with_touched_and_observer(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        heights: &[i32; 256],
        biome_quarts: &[(BiomeRef, bool); 16],
        base_x: i32,
        base_z: i32,
        world: crate::dense_grid::DenseBlockGrid,
        mut cursor: Option<&mut BiomeSearchCursor>,
        touched: Option<&mut TouchedMask>,
        mutation_observer: Option<super::BlockMutationObserverHandle>,
    ) -> crate::dense_grid::DenseBlockGrid {
        let _stage = crate::counters::StageGuard::enter(crate::counters::Stage::Carve);
        let mut owned_cursor = self
            .dynamic_biome
            .as_ref()
            .map(|dynamic| dynamic.table.search_cursor());
        let heightmap_fn = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        let top_material = |x: i32, y: i32, z: i32, under_fluid: bool| -> Option<StateId> {
            let lx = x - base_x;
            let lz = z - base_z;
            if !(0..16).contains(&lx) || !(0..16).contains(&lz) {
                return None;
            }
            let (biome, cold) = biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize];
            self.surface.top_material_typed(
                x,
                y,
                z,
                under_fluid,
                &heightmap_fn,
                biome,
                cold,
            )
        };
        let mut carvers_for_source = |source_x: i32, source_z: i32| -> &[CarverConfig] {
            let biome = if let Some(cursor) = cursor.as_deref_mut() {
                self.carver_biome_for_source(source_x, source_z, cursor)
            } else if let Some(cursor) = owned_cursor.as_mut() {
                self.carver_biome_for_source(source_x, source_z, cursor)
            } else {
                return self.carvers_by_biome.get_name(&self.fallback_biome);
            };
            self.carvers_by_biome.get(biome)
        };

        let mut grid = CarveGrid::from_dense(world);
        let mut observer = TraceObserver {
            enabled: std::env::var_os("LODESTONE_CARVER_TRACE").is_some(),
            center_x: cx,
            center_z: cz,
        };
        crate::carver::apply_carvers_with_touched_and_observer(
            self.seed,
            cx,
            cz,
            self.min_y,
            self.height,
            &mut carvers_for_source,
            &mut grid,
            aquifer,
            &self.carver_replaceable,
            &top_material,
            &mut observer,
            touched,
            mutation_observer,
        );
        grid.into_dense()
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

struct TraceObserver {
    enabled: bool,
    center_x: i32,
    center_z: i32,
}

impl CarveObserver for TraceObserver {
    fn after_carver<R: RandomSource>(
        &mut self,
        source_x: i32,
        source_z: i32,
        index: usize,
        started: bool,
        random: &mut R,
    ) {
        if self.enabled {
            eprintln!(
                "carver-trace center=({}, {}) source=({}, {}) offset=({}, {}) index={} started={} probe={}",
                self.center_x,
                self.center_z,
                source_x,
                source_z,
                source_x - self.center_x,
                source_z - self.center_z,
                index,
                started,
                random.next_long(),
            );
        }
    }
}

fn pre_ore_target_union(targets: &[(i32, i32)], radius: i32) -> Vec<(i32, i32)> {
    let side = radius.saturating_mul(2).saturating_add(1) as usize;
    let mut union = Vec::with_capacity(targets.len().saturating_mul(side.saturating_mul(side)));
    let mut seen = FastMap::default();
    for &(cx, cz) in targets {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let position = (cx + dx, cz + dz);
                if seen.insert(position, ()).is_none() {
                    union.push(position);
                }
            }
        }
    }
    union
}

fn canonical_pre_ore_positions(
    positions: impl IntoIterator<Item = (i32, i32)>,
) -> Vec<(i32, i32)> {
    let mut canonical = Vec::new();
    let mut seen = FastMap::default();
    for position in positions {
        if seen.insert(position, ()).is_none() {
            canonical.push(position);
        }
    }
    canonical
}

fn split_pre_ore_regions(
    positions: Vec<(i32, i32)>,
    max_side: i32,
    tile_side: i32,
) -> Vec<Vec<(i32, i32)>> {
    debug_assert!(max_side > 0 && tile_side > 0 && tile_side <= max_side);
    let (mut low_x, mut low_z) = positions[0];
    let (mut max_x, mut max_z) = positions[0];
    for &(x, z) in &positions[1..] {
        low_x = low_x.min(x);
        low_z = low_z.min(z);
        max_x = max_x.max(x);
        max_z = max_z.max(z);
    }
    let width = max_x as i64 - low_x as i64 + 1;
    let depth = max_z as i64 - low_z as i64 + 1;
    if width <= i64::from(max_side) && depth <= i64::from(max_side) {
        return vec![positions];
    }

    let mut tile_indices = FastMap::default();
    let mut regions = Vec::new();
    for position @ (x, z) in positions {
        let tile = (x.div_euclid(tile_side), z.div_euclid(tile_side));
        let index = match tile_indices.get(&tile).copied() {
            Some(index) => index,
            None => {
                let index = regions.len();
                tile_indices.insert(tile, index);
                regions.push(Vec::new());
                index
            }
        };
        regions[index].push(position);
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::{OverworldGenerator, base_facts, pre_ore_target_union, split_pre_ore_regions};
    use crate::feature::vegetation::{blocks_motion, is_air, is_fluid};
    use crate::dense_grid::DenseBlockGrid;
    use crate::feature::{
        BlockPos, ORE_READ_MAX, ORE_READ_MIN, OreConfig, OreInput, RegionHeights, place_ore_feature,
        region_view::RegionView,
    };
    use crate::rng::{RandomSource, WorldgenRandom, XoroshiroRandomSource};

    #[cfg(feature = "gen-counters")]
    mod pre_ore_region_counter_test {
        use std::path::{Path, PathBuf};

        use serde_json::Value;

        use crate::counters::Stage;
        use crate::overworld::output::GeneratedColumn;
        use super::OverworldGenerator;
        use crate::density::{NoiseParams, Resolver};

        struct FsResolver {
            root: PathBuf,
        }

        impl FsResolver {
            fn read(&self, kind: &str, id: &str) -> Value {
                let name = id.strip_prefix("minecraft:").unwrap_or(id);
                let path = self.root.join(kind).join(format!("{name}.json"));
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
                serde_json::from_str(&text)
                    .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
            }
        }

        impl Resolver for FsResolver {
            fn density_function(&self, id: &str) -> Value {
                self.read("density_function", id)
            }

            fn noise(&self, id: &str) -> NoiseParams {
                let value = self.read("noise", id);
                NoiseParams {
                    first_octave: value["firstOctave"].as_i64().expect("firstOctave") as i32,
                    amplitudes: value["amplitudes"]
                        .as_array()
                        .expect("amplitudes")
                        .iter()
                        .map(|amplitude| amplitude.as_f64().expect("amplitude"))
                        .collect(),
                }
            }
        }

        fn generator() -> OverworldGenerator {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
            let resolver = FsResolver { root: root.clone() };
            let settings: Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
                    .expect("read overworld settings"),
            )
            .expect("parse overworld settings");
            OverworldGenerator::new(42, &settings, &resolver, "minecraft:plains", false)
        }

        fn per_target_control(generator: &OverworldGenerator, targets: &[(i32, i32)]) -> usize {
            let preliminary = generator
                .preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
            targets
                .iter()
                .map(|&(cx, cz)| {
                    let positions = (-2..=2)
                        .flat_map(|dz| (-2..=2).map(move |dx| (cx + dx, cz + dz)))
                        .collect();
                    generator.prepare_pre_ore_region(positions, None, &preliminary)
                })
                .sum()
        }

        fn prefix_digest(generator: &OverworldGenerator, targets: &[(i32, i32)]) -> u64 {
            let mut digest = 0xcbf2_9ce4_8422_2325u64;
            for &(cx, cz) in targets {
                let pre = generator.pre_ore_stage(cx, cz);
                let world = &pre.0;
                let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
                for y in min_y..min_y + size_y {
                    for z in min_z..min_z + size_z {
                        for x in min_x..min_x + size_x {
                            for byte in world.get(x, y, z).as_bytes() {
                                digest = (digest ^ u64::from(*byte))
                                    .wrapping_mul(0x1000_0000_01b3);
                            }
                            digest = (digest ^ 0xff).wrapping_mul(0x1000_0000_01b3);
                        }
                    }
                }
                for &height in &pre.1 {
                    for byte in height.to_le_bytes() {
                        digest = (digest ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3);
                    }
                }
                for &(biome, cold) in &pre.2 {
                    let biome = biome.builtin_or_none().expect("fixture biome is built-in") as u8;
                    digest = (digest ^ u64::from(biome)).wrapping_mul(0x1000_0000_01b3);
                    digest = (digest ^ u64::from(u8::from(cold))).wrapping_mul(0x1000_0000_01b3);
                }
                for qy in 0..pre.3.y_quarts() {
                    for qz in 0..4 {
                        for qx in 0..4 {
                            for byte in pre.3.at_quart(qx, qy, qz).as_bytes() {
                                digest = (digest ^ u64::from(*byte))
                                    .wrapping_mul(0x1000_0000_01b3);
                            }
                        }
                    }
                }
            }
            digest
        }

        fn shaped_digest(columns: &[GeneratedColumn]) -> u64 {
            let mut digest = 0xcbf2_9ce4_8422_2325u64;
            for column in columns {
                for y in column.min_y()..column.min_y() + column.height() {
                    for z in 0..16 {
                        for x in 0..16 {
                            for byte in column.block_state_id(x, y, z).raw().to_le_bytes() {
                                digest = (digest ^ u64::from(byte))
                                    .wrapping_mul(0x1000_0000_01b3);
                            }
                            digest = (digest ^ 0xff).wrapping_mul(0x1000_0000_01b3);
                        }
                    }
                }
            }
            digest
        }

        #[test]
        fn mixed_pre_ore_region_executes_only_missing_products() {
            let positions = [(-2, -3), (-1, -3), (0, -3), (1, -3)];

            let cold = generator();
            let preliminary = cold
                .preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
            crate::counters::reset();
            assert_eq!(
                cold.prepare_pre_ore_region(positions.to_vec(), None, &preliminary),
                positions.len()
            );
            assert_eq!(
                crate::counters::snapshot().stage_entered[Stage::Shape as usize],
                positions.len() as u64,
                "cold control must execute every prefix body"
            );
            let expected = prefix_digest(&cold, &positions);

            let mixed = generator();
            let preliminary = mixed
                .preliminary_cache(crate::aquifer::PRELIMINARY_CACHE_BATCH_CAPACITY);
            for &(cx, cz) in &positions[..2] {
                let _ = mixed.pre_ore_stage(cx, cz);
            }
            crate::counters::reset();
            assert_eq!(
                mixed.prepare_pre_ore_region(positions.to_vec(), None, &preliminary),
                positions.len()
            );
            assert_eq!(
                crate::counters::snapshot().stage_entered[Stage::Shape as usize],
                2,
                "mixed region must execute only its two missing prefix bodies"
            );
            assert_eq!(prefix_digest(&mixed, &positions), expected);

            crate::counters::reset();
            assert_eq!(
                mixed.prepare_pre_ore_region(positions.to_vec(), None, &preliminary),
                positions.len()
            );
            assert_eq!(
                crate::counters::snapshot().stage_entered[Stage::Shape as usize],
                0,
                "warm control must execute no prefix bodies"
            );
        }

        #[test]
        fn three_by_three_union_shares_seven_by_seven_prefix_corners_and_bytes() {
            let targets = [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (0, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ];

            let control = generator();
            crate::counters::reset();
            assert_eq!(per_target_control(&control, &targets), 225);
            let control_counters = crate::counters::snapshot();
            let control_digest = prefix_digest(&control, &targets);

            let shared = generator();
            crate::counters::reset();
            assert_eq!(shared.prepare_pre_ore_targets_with_radius(&targets, 2), 49);
            let shared_counters = crate::counters::snapshot();
            let shared_digest = prefix_digest(&shared, &targets);

            assert_eq!(shared_counters.pre_ore_computed, 49);
            assert!(
                shared_counters.corner_evals < control_counters.corner_evals,
                "shared 7x7 region evaluated {} corners; per-target control evaluated {}",
                shared_counters.corner_evals,
                control_counters.corner_evals,
            );
            assert_eq!(shared_digest, control_digest);
        }

        #[test]
        fn leased_prefix_warms_before_shaped_columns_and_radius_one_is_incomplete() {
            let targets = [(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0),
                (-1, 1), (0, 1), (1, 1)];

            let cold = generator();
            crate::counters::reset();
            let cold_columns = {
                let lease = cold.lease_batch(&targets);
                targets
                    .iter()
                    .map(|&(cx, cz)| lease.column_shaped(cx, cz))
                    .collect::<Vec<_>>()
            };
            let cold_digest = shaped_digest(&cold_columns);

            let shared = generator();
            crate::counters::reset();
            let shared_columns = {
                let lease = shared.lease_batch(&targets);
                assert_eq!(lease.prepare_pre_ore_targets_with_radius(&targets, 2), 49);
                targets
                    .iter()
                    .map(|&(cx, cz)| lease.column_shaped(cx, cz))
                    .collect::<Vec<_>>()
            };
            let shared_counters = crate::counters::snapshot();
            assert_eq!(shared_counters.pre_ore_computed, 49);
            assert_eq!(shaped_digest(&shared_columns), cold_digest);

            let insufficient = generator();
            crate::counters::reset();
            {
                let lease = insufficient.lease_batch(&targets);
                assert_eq!(lease.prepare_pre_ore_targets_with_radius(&targets, 1), 25);
                for &(cx, cz) in &targets {
                    let _ = lease.column_shaped(cx, cz);
                }
            }
            let before_missing = crate::counters::snapshot().pre_ore_computed;
            insufficient.pre_ore_stage(-3, -3);
            let after_missing = crate::counters::snapshot().pre_ore_computed;
            assert_eq!(before_missing, 25);
            assert_eq!(after_missing, before_missing + 1);
        }
    }

    #[test]
    fn pre_ore_target_union_deduplicates_in_first_seen_order() {
        let union = pre_ore_target_union(&[(0, 0), (1, 0)], 1);
        assert_eq!(union.len(), 15);
        assert_eq!(union[0], (-1, -1));
        assert_eq!(union[9], (1, -1));
        assert_eq!(union.last(), Some(&(2, 1)));
    }

    #[test]
    fn seven_by_seven_union_stays_one_region_and_large_union_tiles() {
        let seven = pre_ore_target_union(
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
            2,
        );
        let regions = split_pre_ore_regions(seven, 8, 4);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].len(), 49);

        let large = pre_ore_target_union(&[(0, 0), (8, 0)], 2);
        let regions = split_pre_ore_regions(large, 8, 4);
        assert!(regions.len() > 1);
        assert!(regions.iter().all(|region| !region.is_empty()));
    }

    #[test]
    fn positive_cell_fast_path_matches_packed_layout_and_height_lanes() {
        let densities = [1.0; 128];
        assert!(OverworldGenerator::cell_densities_are_positive(&densities));

        let height = 24;
        let mut field = vec![0; 16 * 16 * height as usize];
        let mut heights = [-77; 256];
        let mut states = std::array::from_fn(|_| crate::aquifer::VerticalRunState::default());
        OverworldGenerator::write_solid_cell(
            1,
            1,
            2,
            -56,
            height,
            &mut field,
            &mut heights,
            &mut states,
        );

        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let selected_column = (4..8).contains(&lx) && (8..12).contains(&lz);
                let column = (lz * 16 + lx) as usize;
                assert_eq!(heights[column], if selected_column { -49 } else { -77 });
                for ly in 0..height {
                    let selected = selected_column && (8..16).contains(&ly);
                    assert_eq!(
                        field[OverworldGenerator::idx(lx, ly, lz, height)],
                        if selected { 1 } else { 0 },
                        "packed cell mismatch at ({lx}, {ly}, {lz})",
                    );
                }
            }
        }
    }

    #[test]
    fn one_nonpositive_cell_density_rejects_the_solid_fast_path() {
        let mut densities = [1.0; 128];
        densities[127] = 0.0;
        assert!(!OverworldGenerator::cell_densities_are_positive(&densities));
        densities[127] = -0.0;
        assert!(!OverworldGenerator::cell_densities_are_positive(&densities));
        densities[127] = f64::NAN;
        assert!(!OverworldGenerator::cell_densities_are_positive(&densities));
    }

    fn heightmap_with(value: i32) -> RegionHeights {
        let mut heights = RegionHeights::unset();
        for lz in ORE_READ_MIN..ORE_READ_MAX {
            for lx in ORE_READ_MIN..ORE_READ_MAX {
                heights.set(lx, lz, value);
            }
        }
        heights
    }

    fn next_long_after_ore_probe(heights: &RegionHeights) -> i64 {
        let grid = DenseBlockGrid::new(-16, 0, -16, 48, 16, 48, "minecraft:air");
        let mut view = RegionView::over_region_grid(&grid, 0, 16);
        let input = OreInput {
            chunk_x: 0,
            chunk_z: 0,
            center_x: 0,
            center_z: 0,
            min_y: 0,
            height: 16,
            min_gen_y: 0,
            gen_depth: 16,
            read_min: ORE_READ_MIN,
            read_max: ORE_READ_MAX,
            ocean_floor_wg: heights,
            in_tag: &|_, _| false,
            biome_allows: None,
        };
        let config = OreConfig {
            size: 1,
            discard_chance_on_air_exposure: 0.0,
            targets: Vec::new(),
        };
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(42));
        // `y_start` is 11 for this size-one blob. A heightmap value of 11
        // reaches `do_place`, while 10 returns immediately after its three
        // setup draws.
        place_ore_feature(&mut random, BlockPos { x: 2, y: 14, z: 2 }, &config, &input, &mut view);
        random.next_long()
    }

    fn string_ocean_floor_wg_heights(world: &DenseBlockGrid) -> [i32; 256] {
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        assert_eq!((size_x, size_z), (16, 16));
        let mut heights = [min_y; 256];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for y in (min_y..min_y + size_y).rev() {
                    let state = world.get(min_x + lx, y, min_z + lz);
                    let base = state.split('[').next().unwrap_or(state.as_str());
                    if !is_air(base) && !is_fluid(base) && blocks_motion(base) {
                        heights[(lz * 16 + lx) as usize] = y + 1;
                        break;
                    }
                }
            }
        }
        heights
    }

    fn block_digest(world: &DenseBlockGrid) -> u64 {
        let (min_x, min_y, min_z, size_x, size_y, size_z) = world.bounds();
        let mut digest = 0xcbf2_9ce4_8422_2325u64;
        for y in min_y..min_y + size_y {
            for z in min_z..min_z + size_z {
                for x in min_x..min_x + size_x {
                    for byte in world.get(x, y, z).as_bytes() {
                        digest = (digest ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3);
                    }
                    digest = (digest ^ 0xff).wrapping_mul(0x1000_0000_01b3);
                }
            }
        }
        digest
    }

    #[test]
    fn numeric_ocean_floor_scan_matches_string_oracle_for_builtin_and_extension_states() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                world.set(x, 0, z, "minecraft:stone");
                world.set(x, 1, z, "minecraft:water[level=0]");
                world.set(x, 2, z, "minecraft:seagrass");
                world.set(x, 3, z, "mod:soft_plant[variant=thin]");
                world.set(x, 4, z, "minecraft:cave_air");
                world.set(x, 5, z, "minecraft:lava[level=0]");
                world.set(x, 6, z, "minecraft:oak_leaves[distance=7,persistent=false]");
            }
        }

        let numeric = OverworldGenerator::ocean_floor_wg_heights(&world, 0, 0, 0, 16);
        let oracle = string_ocean_floor_wg_heights(&world);
        assert_eq!(numeric, oracle);
        assert!(numeric.iter().all(|&height| height == 7));
    }

    #[test]
    fn post_mutation_ocean_floor_scan_only_visits_touched_columns() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..=5 {
                    world.set(x, y, z, "minecraft:stone");
                }
            }
        }
        let mut state = super::OceanFloorState::new(0);
        state.heights = [6; 256];
        world.set(3, 5, 4, "minecraft:air");
        crate::carver::mark_touched_column(&mut state.touched, 0, 0, 3, 4);

        #[cfg(feature = "gen-counters")]
        crate::counters::reset();
        let incremental = OverworldGenerator::ocean_floor_wg_heights_from_state(&world, state);
        #[cfg(feature = "gen-counters")]
        let counters = crate::counters::snapshot();
        let full = OverworldGenerator::ocean_floor_wg_heights(&world, 0, 0, 0, 16);

        assert_eq!(incremental, full);
        assert_eq!(incremental[(4 * 16 + 3) as usize], 5);
        #[cfg(feature = "gen-counters")]
        {
            assert_eq!(counters.full_column_scans, 1);
            assert_eq!(counters.full_column_scan_cells, 12);
        }
    }

    #[test]
    fn tracked_ocean_floor_updates_the_exact_height_without_a_recount() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for y in 0..=5 {
            world.set(3, y, 4, "minecraft:stone");
        }
        let mut state = super::OceanFloorState::new(0);
        state.configure(0, 0, 16);
        for y in 0..=5 {
            state.observe(3, y, 4, base_facts(lodestone_data::block::Block::Stone.default_state()));
        }
        {
            state.observe_mutation(
                3,
                5,
                4,
                base_facts(lodestone_data::block::Block::Stone.default_state()),
                lodestone_data::block::Block::Air.default_state(),
            );
        }
        world.set(3, 5, 4, "minecraft:air");

        #[cfg(feature = "gen-counters")]
        crate::counters::reset();
        let heights = OverworldGenerator::ocean_floor_wg_heights_from_state(&world, state);
        assert_eq!(heights[(4 * 16 + 3) as usize], 5);
        #[cfg(feature = "gen-counters")]
        {
            let counters = crate::counters::snapshot();
            assert_eq!(counters.full_column_scans, 0);
            assert_eq!(counters.full_column_scan_cells, 0);
        }
    }

    #[test]
    fn post_mutation_heights_preserve_the_final_block_digest_control() {
        let mut baseline = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..=5 {
                    baseline.set(x, y, z, "minecraft:stone");
                }
            }
        }
        let mut state = super::OceanFloorState::new(0);
        state.heights = OverworldGenerator::ocean_floor_wg_heights(&baseline, 0, 0, 0, 16);
        let mut incremental_world = baseline.clone();
        let mut full_world = baseline;
        for &(x, y, z) in &[(3, 5, 4), (11, 2, 9)] {
            incremental_world.set(x, y, z, "minecraft:air");
            full_world.set(x, y, z, "minecraft:air");
            crate::carver::mark_touched_column(&mut state.touched, 0, 0, x, z);
        }

        let incremental = OverworldGenerator::ocean_floor_wg_heights_from_state(
            &incremental_world,
            state,
        );
        let full = OverworldGenerator::ocean_floor_wg_heights(&full_world, 0, 0, 0, 16);
        assert_eq!(incremental, full);
        assert_eq!(block_digest(&incremental_world), block_digest(&full_world));
    }

    #[test]
    fn no_post_mutation_ocean_floor_scan_runs_without_touches() {
        let world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        let state = super::OceanFloorState::new(0);
        #[cfg(feature = "gen-counters")]
        crate::counters::reset();
        let incremental = OverworldGenerator::ocean_floor_wg_heights_from_state(&world, state);
        #[cfg(feature = "gen-counters")]
        let counters = crate::counters::snapshot();

        assert!(incremental.iter().all(|&height| height == 0));
        #[cfg(feature = "gen-counters")]
        {
            assert_eq!(counters.full_column_scans, 0);
            assert_eq!(counters.full_column_scan_cells, 0);
        }
    }

    #[test]
    fn omitting_a_touched_column_is_rejected_by_the_full_recount_control() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..=5 {
                    world.set(x, y, z, "minecraft:stone");
                }
            }
        }
        let mut state = super::OceanFloorState::new(0);
        state.heights = [6; 256];
        world.set(1, 5, 1, "minecraft:air");
        world.set(2, 5, 2, "minecraft:air");
        crate::carver::mark_touched_column(&mut state.touched, 0, 0, 1, 1);

        let incremental = OverworldGenerator::ocean_floor_wg_heights_from_state(&world, state);
        let full = OverworldGenerator::ocean_floor_wg_heights(&world, 0, 0, 0, 16);
        assert_ne!(incremental, full, "the omitted touched bit must be observable");
        assert_eq!(incremental[(1 * 16 + 1) as usize], full[(1 * 16 + 1) as usize]);
        assert_ne!(incremental[(2 * 16 + 2) as usize], full[(2 * 16 + 2) as usize]);
    }

    #[test]
    #[ignore = "diagnostic timing: run explicitly in release mode"]
    fn measure_numeric_ocean_floor_scan_against_string_oracle() {
        use std::hint::black_box;
        use lodestone_time::Instant;

        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 384, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..64 {
                    world.set(x, y, z, "minecraft:stone");
                }
                world.set(x, 64, z, "minecraft:water[level=0]");
                world.set(x, 65, z, "minecraft:seagrass");
                world.set(x, 66, z, "mod:soft_plant[variant=thin]");
            }
        }
        let rounds = 2_000;
        let start_numeric = Instant::now();
        let mut numeric_digest = 0i64;
        for _ in 0..rounds {
            numeric_digest += i64::from(OverworldGenerator::ocean_floor_wg_heights(&world, 0, 0, 0, 384)[0]);
        }
        let numeric_elapsed = start_numeric.elapsed();
        let start_string = Instant::now();
        let mut string_digest = 0i64;
        for _ in 0..rounds {
            string_digest += i64::from(string_ocean_floor_wg_heights(&world)[0]);
        }
        let string_elapsed = start_string.elapsed();
        assert_eq!(black_box(numeric_digest), black_box(string_digest));
        eprintln!(
            "ocean_floor_scan rounds={rounds} numeric={numeric_elapsed:?} string={string_elapsed:?} digest={numeric_digest}"
        );
    }

    #[test]
    fn carved_ocean_floor_height_culls_the_ore_probe() {
        let mut pre_carve = DenseBlockGrid::new(0, 0, 0, 16, 16, 16, "minecraft:air");
        for z in 0..16 {
            for x in 0..16 {
                for y in 0..=10 {
                    pre_carve.set(x, y, z, "minecraft:stone");
                }
            }
        }
        let mut post_carve = pre_carve.clone();
        // This is the relevant 5x5 probe floor for the selected blob, carved
        // down one block. The unchanged pre-carve field would report 11 here.
        for z in 0..=4 {
            for x in 0..=4 {
                post_carve.set(x, 10, z, "minecraft:cave_air");
            }
        }

        let pre_heights = OverworldGenerator::ocean_floor_wg_heights(&pre_carve, 0, 0, 0, 16);
        let post_heights = OverworldGenerator::ocean_floor_wg_heights(&post_carve, 0, 0, 0, 16);
        for z in 0..=4 {
            for x in 0..=4 {
                let i = (z * 16 + x) as usize;
                assert_eq!(pre_heights[i], 11, "pre-carve OCEAN_FLOOR_WG at ({x}, {z})");
                assert_eq!(post_heights[i], 10, "carve must lower OCEAN_FLOOR_WG at ({x}, {z})");
            }
        }

        let before = next_long_after_ore_probe(&heightmap_with(11));
        let after = next_long_after_ore_probe(&heightmap_with(10));
        let mut reaches_blob = WorldgenRandom::new(XoroshiroRandomSource::new(42));
        let _ = reaches_blob.next_float();
        let _ = reaches_blob.next_int_bounded(3);
        let _ = reaches_blob.next_int_bounded(3);
        let _ = reaches_blob.next_double();
        assert_eq!(before, reaches_blob.next_long(), "height 11 must reach the blob draw");
        let mut culled = WorldgenRandom::new(XoroshiroRandomSource::new(42));
        let _ = culled.next_float();
        let _ = culled.next_int_bounded(3);
        let _ = culled.next_int_bounded(3);
        assert_eq!(after, culled.next_long(), "carved height 10 must cull before the blob draw");
        assert_ne!(before, after, "the control must distinguish proceed from cull");
    }
}
