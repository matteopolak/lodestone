//! Structure starts and references for the 26.3 Overworld.
//!
//! Starts come from the shared [`StructureRegistry`]; what is 26.3-specific is the
//! [`StartContext`] they sample: base column heights and fluid kinds from the 26.3 noise engine
//! and biomes from the 26.3 climate tree, so a jigsaw start's ground-hugging Y matches the real
//! server rather than the 26.2 sampler.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lodestone_worldgen_core::engine::release26_3::aquifer::Fluid;
use lodestone_worldgen_core::engine::release26_3::climate::ClimateCursor;
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::Substance;

use super::{Terrain263, Write};
use crate::structure::BlockKind;
use crate::dense_grid::DenseBlockGrid;
use crate::density::Resolver;
use crate::feature::vegetation::VegTags;
use crate::rng::{WorldgenRandom, XoroshiroRandomSource};
use lodestone_data::block_states::StateId;
use crate::structure::chunk::{BEARD_REACH, PORTAL_TERRAIN_REACH, REFS_RADIUS, StructureRefs};
use crate::structure::{CodedLoot, HeightmapKind, PieceRefinement, StartContext, StructureRegistry, StructureStart};

/// The structure registry for one seed plus the start and reference caches built on it.
/// References kept before the cache restarts; one entry is a handful of `Arc`s.
const REFS_CAPACITY: usize = 8192;

pub(super) struct Structures263 {
    registry: StructureRegistry,
    veg_tags: VegTags,
    refs: Mutex<HashMap<(i32, i32), Arc<StructureRefs>>>,
    starts: Mutex<HashMap<(i32, i32), Arc<Vec<Arc<StructureStart>>>>>,
}

impl Structures263 {
    pub(super) fn new(seed: i64, resolver: &dyn Resolver, possible_biomes: &HashSet<String>) -> Option<Self> {
        let registry = StructureRegistry::new_for_biomes(seed, resolver, Some(possible_biomes));
        (!registry.is_empty()).then(|| Self {
            registry,
            veg_tags: crate::feature::vegetation::build_veg_tags(resolver),
            refs: Mutex::new(HashMap::new()),
            starts: Mutex::new(HashMap::new()),
        })
    }
}

/// Base-column and biome queries over one [`Terrain263`].
pub(super) struct StartCtx<'a> {
    terrain: &'a Terrain263,
    ctx: RefCell<Ctx>,
    climate_ctx: RefCell<Ctx>,
    cursor: RefCell<ClimateCursor>,
    columns: RefCell<HashMap<(i32, i32), Arc<Vec<Substance>>>>,
}

impl<'a> StartCtx<'a> {
    pub(super) fn new(terrain: &'a Terrain263) -> Self {
        Self {
            terrain,
            ctx: RefCell::new(Ctx::new(&terrain.generator.program)),
            climate_ctx: RefCell::new(Ctx::uncached()),
            cursor: RefCell::new(ClimateCursor::default()),
            columns: RefCell::new(HashMap::new()),
        }
    }

    fn column(&self, x: i32, z: i32) -> Arc<Vec<Substance>> {
        if let Some(hit) = self.columns.borrow().get(&(x, z)) {
            return Arc::clone(hit);
        }
        let built = Arc::new(self.terrain.generator.fill_column(x, z, &mut self.ctx.borrow_mut()));
        let mut columns = self.columns.borrow_mut();
        if columns.len() > 4096 {
            columns.clear();
        }
        columns.insert((x, z), Arc::clone(&built));
        built
    }

    fn kind(substance: Substance) -> BlockKind {
        match substance {
            Substance::Default => BlockKind::Stone,
            Substance::Fluid(Fluid::Air) => BlockKind::Air,
            Substance::Fluid(Fluid::Water) => BlockKind::Water,
            Substance::Fluid(Fluid::Lava) => BlockKind::Lava,
        }
    }
}

impl StartContext for StartCtx<'_> {
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        let min_y = self.terrain.generator.min_y;
        let column = self.column(x, z);
        let occupied = |s: &Substance| match heightmap {
            HeightmapKind::WorldSurfaceWg => !matches!(s, Substance::Fluid(Fluid::Air)),
            HeightmapKind::OceanFloorWg => matches!(s, Substance::Default),
        };
        column.iter().rposition(occupied).map_or(min_y - 1, |i| min_y + i as i32)
    }

    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        let g = &self.terrain.generator;
        let id = g.biome_at_quart(&self.terrain.source, &mut self.cursor.borrow_mut(), qx, qy, qz, &mut self.climate_ctx.borrow_mut());
        g.biomes.info(id).name.clone()
    }

    fn sea_level(&self) -> i32 {
        self.terrain.generator.sea_level
    }

    fn min_y(&self) -> i32 {
        self.terrain.generator.min_y
    }

    fn dimension_height(&self) -> i32 {
        self.terrain.generator.height
    }

    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        let index = y - self.terrain.generator.min_y;
        if index < 0 {
            return BlockKind::Stone;
        }
        self.column(x, z).get(index as usize).copied().map_or(BlockKind::Air, Self::kind)
    }
}

impl Terrain263 {
    /// Every structure start whose origin chunk is `(cx, cz)`, in structure-set order. Empty when
    /// the generator has no structure registry.
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Arc<Vec<Arc<StructureStart>>> {
        let Some(structures) = &self.structures else {
            return Arc::default();
        };
        if let Some(hit) = structures.starts.lock().expect("starts lock poisoned").get(&(cx, cz)) {
            return Arc::clone(hit);
        }
        let ctx = StartCtx::new(self);
        self.starts_with(structures, cx, cz, None, &ctx)
    }

    fn starts_with(&self, structures: &Structures263, cx: i32, cz: i32, sets: Option<&[usize]>, ctx: &StartCtx<'_>) -> Arc<Vec<Arc<StructureStart>>> {
        if let Some(hit) = structures.starts.lock().expect("starts lock poisoned").get(&(cx, cz)) {
            return Arc::clone(hit);
        }
        let computed: Vec<Arc<StructureStart>> = match sets {
            Some(sets) => structures.registry.starts_at_sets(sets, cx, cz, ctx),
            None => structures.registry.starts_at(cx, cz, ctx),
        }
        .into_iter()
        .map(Arc::new)
        .collect();
        let computed = Arc::new(computed);
        let mut cache = structures.starts.lock().expect("starts lock poisoned");
        Arc::clone(cache.entry((cx, cz)).or_insert(computed))
    }

    /// The starts that can touch chunk `(cx, cz)`: every start within [`REFS_RADIUS`] chunks
    /// whose adjusted box comes within the beardifier's reach of it.
    pub fn structure_refs(&self, cx: i32, cz: i32) -> Arc<StructureRefs> {
        let Some(structures) = &self.structures else {
            return Arc::default();
        };
        if let Some(hit) = structures.refs.lock().expect("refs lock poisoned").get(&(cx, cz)) {
            return Arc::clone(hit);
        }
        let built = Arc::new(self.build_refs(structures, cx, cz));
        let mut cache = structures.refs.lock().expect("refs lock poisoned");
        if cache.len() >= REFS_CAPACITY {
            cache.clear();
        }
        Arc::clone(cache.entry((cx, cz)).or_insert(built))
    }

    fn build_refs(&self, structures: &Structures263, cx: i32, cz: i32) -> StructureRefs {
        let mut refs = StructureRefs::default();
        let ctx = StartCtx::new(self);
        let candidates = structures.registry.origin_candidates_by_set_in(cx - REFS_RADIUS, cx + REFS_RADIUS, cz - REFS_RADIUS, cz + REFS_RADIUS, &ctx);
        let mut by_origin: std::collections::BTreeMap<(i32, i32), Vec<usize>> = std::collections::BTreeMap::new();
        for ((sx, sz), set) in candidates {
            by_origin.entry((sx, sz)).or_default().push(set);
        }
        for ((sx, sz), mut sets) in by_origin {
            sets.sort_unstable();
            sets.dedup();
            for start in self.starts_with(structures, sx, sz, Some(&sets), &ctx).iter() {
                let near = start.adjusted_bounding_box().is_close_to_chunk(cx, cz, BEARD_REACH)
                    || (start.structure.contains("ruined_portal")
                        && start.pieces.iter().any(|piece| {
                            matches!(piece.refine.as_ref(), Some(PieceRefinement::RuinedPortalTerrain { .. }))
                                && piece.bounding_box.is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH)
                        }));
                if near {
                    refs.entries.push((sx, sz, Arc::clone(start)));
                }
            }
        }
        refs
    }

    /// The base-terrain view structure placement samples: column heights, fluid kinds and
    /// biomes of this generator before surface rules and carvers.
    #[must_use]
    pub fn start_context(&self) -> impl StartContext + '_ {
        StartCtx::new(self)
    }

    /// Concentric-ring (stronghold) origins for `set_id`.
    pub fn ring_origins(&self, set_id: &str) -> Vec<(i32, i32)> {
        let Some(structures) = &self.structures else {
            return Vec::new();
        };
        structures.registry.ring_origins(set_id, &StartCtx::new(self))
    }

    /// The beard term the structures near `(cx, cz)` imply for its terrain fill, or `None` when
    /// no adaptation-bearing start reaches it.
    pub(super) fn beardifier_for(&self, cx: i32, cz: i32) -> Option<Box<dyn lodestone_worldgen_core::engine::release26_3::sampler::BeardifierSource>> {
        self.structures.as_ref()?;
        let refs = self.structure_refs(cx, cz);
        let starts = refs.entries.iter().map(|(_, _, start)| start).filter(|start| {
            start.pieces_complete
                && start.terrain_adaptation != crate::structure::TerrainAdjustment::None
                && start.adjusted_bounding_box().is_close_to_chunk(cx, cz, BEARD_REACH)
        });
        let beard = crate::structure::beardifier::Beardifier::for_chunk(cx, cz, starts.map(std::convert::AsRef::as_ref));
        (!beard.is_empty()).then(|| Box::new(beard.to_release26_3()) as Box<_>)
    }

    /// Whether any structure start reaches chunk `(cx, cz)` in generation step `step`, so a
    /// caller can skip building the chunk's block view when none does.
    #[must_use]
    pub fn has_structures_in_step(&self, cx: i32, cz: i32, step: i32) -> bool {
        let Some(structures) = &self.structures else {
            return false;
        };
        self.structure_refs(cx, cz).entries.iter().any(|(_, _, start)| {
            start.pieces_complete && structures.registry.feature_placement_key(&start.structure).is_some_and(|(s, _)| s == step)
        })
    }

    /// Writes the pieces of every structure start that reaches chunk `(cx, cz)` and generates
    /// in `step`, in registry order, into that chunk's columns only; a piece straddling the
    /// border writes its own half here and the neighbour's half when the neighbour decorates.
    /// `read` supplies the chunk's current canonical blocks. Returns the positions that changed.
    /// Places every structure piece of `step` that reaches chunk `(cx, cz)`, returning the
    /// changed blocks and the loot containers the placement itself seeded (a fortress's chests,
    /// whose seeds are drawn while its pieces are placed).
    pub fn place_structures(&self, cx: i32, cz: i32, step: i32, read: &mut dyn FnMut(i32, i32, i32) -> StateId) -> (Vec<Write>, Vec<CodedLoot>) {
        let Some(structures) = &self.structures else {
            return (Vec::new(), Vec::new());
        };
        let registry = &structures.registry;
        let refs = self.structure_refs(cx, cz);
        let mut entries: Vec<&(i32, i32, Arc<StructureStart>)> = refs
            .entries
            .iter()
            .filter(|(_, _, start)| {
                start.pieces_complete && registry.feature_placement_key(&start.structure).is_some_and(|(s, _)| s == step)
            })
            .collect();
        if entries.is_empty() {
            return (Vec::new(), Vec::new());
        }
        entries.sort_by_key(|(_, _, start)| registry.feature_placement_key(&start.structure).unwrap_or((i32::MAX, usize::MAX)));

        let seed = registry.seed();
        let (bx, bz) = (cx * 16, cz * 16);
        let mut world = DenseBlockGrid::from_canonical_states(bx, self.min_y, bz, 16, self.height, 16, |x, y, z| read(x, y, z));
        world.begin_change_capture();
        let sampler = StartCtx::new(self);
        structures.veg_tags.bind();
        let solid_render = |state: StateId| structures.veg_tags.simple_block_support.solid_render.test_id(state);
        let stream = |structure: &str| {
            let (step, index) = registry.feature_placement_key(structure).expect("a placed structure has a key");
            let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
            let decoration_seed = random.set_decoration_seed(seed, bx, bz);
            random.set_feature_seed(decoration_seed, index as i32, step);
            random
        };
        let mut shared_streams: HashMap<&str, WorldgenRandom<XoroshiroRandomSource>> = HashMap::new();
        let mut portal_streams: HashMap<&str, WorldgenRandom<XoroshiroRandomSource>> = HashMap::new();
        let mut loot = Vec::new();
        for (_, _, start) in entries {
            let intersects = start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15);
            let is_mineshaft = registry
                .structure(&start.structure)
                .is_some_and(|definition| matches!(definition.kind, crate::structure::StructureKind::Mineshaft { .. }));
            if is_mineshaft && intersects {
                let random = shared_streams.entry(start.structure.as_str()).or_insert_with(|| stream(&start.structure));
                if let Some(blocks) = registry.mineshaft_blocks_for_chunk(start, cx, cz, &sampler, &world, random) {
                    for block in blocks {
                        world.set_id(block.pos[0], block.pos[1], block.pos[2], block.state);
                    }
                    continue;
                }
            }
            if intersects {
                let mut fortress_random = stream(&start.structure);
                if let Some(mut chests) = registry.place_fortress_for_chunk(start, cx, cz, &mut world, &mut fortress_random, &solid_render) {
                    loot.append(&mut chests);
                    continue;
                }
            }
            let reference = crate::structure::jigsaw::reference_position(&start.pieces);
            for piece in &start.pieces {
                let portal_terrain_reaches = matches!(piece.refine.as_ref(), Some(PieceRefinement::RuinedPortalTerrain { .. }))
                    && piece.bounding_box.is_close_to_chunk(cx, cz, PORTAL_TERRAIN_REACH);
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) && !portal_terrain_reaches {
                    continue;
                }
                if let Some(blocks) = &piece.blocks {
                    crate::structure::chunk::place_coded_blocks(&mut world, blocks, &solid_render);
                }
                if let Some(placement) = &piece.placement {
                    let origin = crate::structure::template::PlaceOrigin { position: placement.position, reference, seed };
                    placement.template.place(origin, &placement.settings, &mut world);
                    for extra in &piece.extra_placements {
                        let origin = crate::structure::template::PlaceOrigin { position: extra.position, reference, seed };
                        extra.template.place(origin, &extra.settings, &mut world);
                    }
                }
                match piece.refine.as_ref() {
                    Some(PieceRefinement::FeaturePlacements { placements }) => {
                        let random = shared_streams.entry(start.structure.as_str()).or_insert_with(|| stream(&start.structure));
                        crate::structure::feature_placement::place_feature_pool_elements(random, placements, &mut world, &structures.veg_tags);
                    }
                    Some(PieceRefinement::StrongholdBlocks { writes }) => {
                        crate::structure::stronghold::place_post_surface_blocks(&mut world, writes);
                    }
                    Some(PieceRefinement::BuriedTreasureChest) => {
                        crate::structure::chunk::place_buried_treasure_chest(&mut world, piece.bounding_box.min);
                    }
                    Some(PieceRefinement::RuinedPortalTerrain { placement, cold, overgrown, vines, features_cannot_replace }) => {
                        let random = portal_streams.entry(start.structure.as_str()).or_insert_with(|| stream(&start.structure));
                        crate::structure::chunk::place_ruined_portal_terrain(
                            &mut world,
                            piece.bounding_box,
                            random,
                            *placement,
                            *cold,
                            *overgrown,
                            *vines,
                            features_cannot_replace,
                        );
                    }
                    Some(PieceRefinement::FortressPlacement { .. } | PieceRefinement::NetherFossilDriedGhast { .. }) | None => {}
                }
            }
        }
        let writes = world.finish_change_capture().into_iter().map(|change| (change.position.0, change.position.1, change.position.2, change.state)).collect();
        (writes, loot)
    }
}
