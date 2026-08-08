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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::structure::{HeightmapKind, StartContext, StructureStart};

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

    /// The starts S3's beardifier will evaluate: adaptation-bearing, piece-complete,
    /// and in reach. **Empty today for every chunk**, because no
    /// adaptation-bearing structure has a piece generator yet (all of them are
    /// jigsaw, plus stronghold; the three S2 kinds are all `terrain_adaptation:
    /// none`) — which is precisely why S3 has no structure-positive gate available
    /// at its own landing.
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

/// Chebyshev chunk radius `structure_refs` reads `structure_starts` over —
/// vanilla's `ChunkGenerator.createReferences`' hardcoded `int range = 8`, i.e.
/// a 17×17 neighbourhood.
pub const REFS_RADIUS: i32 = 8;

/// The reach `Beardifier.forStructuresInChunk` keeps a start at
/// (`isCloseToChunk(chunkPos, 12)`), and the amount
/// `Structure.adjustBoundingBox` inflates an adaptation-bearing box by. Same
/// number twice in vanilla, and it is the same number for the same reason.
pub const BEARD_REACH: i32 = 12;

/// [`StartContext`] over freshly sampled noise columns.
///
/// Holds no terrain product and reads no store slot, which is what keeps
/// `structure_starts` above `pre_ore` instead of circular. The per-chunk
/// [`AquiferSystem`] cache exists because building one is the expensive part and
/// a structure predicate asks about several columns of the same chunk.
struct StartSampler<'a> {
    generator: &'a OverworldGenerator,
    /// `(cx, cz)` → that chunk's aquifer. `RefCell` because [`StartContext`]
    /// takes `&self` (it is called from a `&dyn` behind the registry) and this is
    /// single-threaded per stage invocation.
    aquifers: RefCell<HashMap<(i32, i32), Arc<AquiferSystem>>>,
}

impl StartSampler<'_> {
    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        let built = Arc::new(self.generator.build_aquifer(cx, cz));
        self.aquifers
            .borrow_mut()
            .insert((cx, cz), Arc::clone(&built));
        built
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
        for ly in (0..generator.height()).rev() {
            let y = min_y + ly;
            let kind = aquifer.block_at(x, y, z);
            let matched = match heightmap {
                HeightmapKind::WorldSurfaceWg => kind != BlockKind::Air,
                HeightmapKind::OceanFloorWg => kind == BlockKind::Stone,
            };
            if matched {
                return y;
            }
        }
        min_y - 1
    }

    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        self.generator.biome_at_quart(qx, qy, qz)
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level()
    }
}

impl OverworldGenerator {
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
                let Some(registry) = &self.structures else {
                    return Vec::new();
                };
                let sampler = StartSampler {
                    generator: self,
                    aquifers: RefCell::new(HashMap::new()),
                };
                registry
                    .starts_at(cx, cz, &sampler)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            })
    }

    /// Stage 0b: this chunk's structure references — `createReferences`' 17×17
    /// walk, keeping the starts whose adjusted box comes within
    /// [`BEARD_REACH`] blocks of this chunk.
    ///
    /// Vanilla's `createReferences` uses a plain 16×16 chunk-box intersection;
    /// the extra 12 blocks are the beardifier's own reach
    /// (`Beardifier.forStructuresInChunk`'s `isCloseToChunk(chunkPos, 12)`), and
    /// keeping one product for both consumers is why this is the wider of the
    /// two. [`StructureRefs::packed_by_structure`] re-narrows to the exact
    /// chunk-box test for the persistence view, so the NBT is vanilla's and the
    /// beardifier's input is the beardifier's.
    pub(super) fn structure_refs_stage(&self, cx: i32, cz: i32) -> Arc<StructureRefs> {
        self.store.entry((cx, cz)).structure_refs.get_or_compute(drop, || {
            if self.structures.is_none() {
                return StructureRefs::default();
            }
            let mut entries = Vec::new();
            for sx in (cx - REFS_RADIUS)..=(cx + REFS_RADIUS) {
                for sz in (cz - REFS_RADIUS)..=(cz + REFS_RADIUS) {
                    for start in self.structure_starts_stage(sx, sz).iter() {
                        if start
                            .adjusted_bounding_box()
                            .is_close_to_chunk(cx, cz, BEARD_REACH)
                        {
                            entries.push((sx, sz, Arc::clone(start)));
                        }
                    }
                }
            }
            StructureRefs { entries }
        })
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
        let _view = self.store.open_view((cx, cz), super::COLUMN_CLOSURE_RADIUS);
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
        let _view = self.store.open_view((cx, cz), super::COLUMN_CLOSURE_RADIUS);
        self.structure_starts_stage(cx, cz).to_vec()
    }

    /// This chunk's `structures.References`, ready for the NBT writer: structure
    /// id → packed origin-chunk keys, narrowed to vanilla's own 16×16
    /// intersection test.
    #[must_use]
    pub fn structure_references(&self, cx: i32, cz: i32) -> std::collections::BTreeMap<String, Vec<i64>> {
        let _view = self.store.open_view((cx, cz), super::COLUMN_CLOSURE_RADIUS);
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

    /// Stage 4b (issue #514's S2): writes every template-driven piece that
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
    /// `placeSettings.setBoundingBox(chunkBB)` for free. That is only sound
    /// because every piece's position is fixed at *start* time and every
    /// processor draw is position-seeded; see
    /// [`StructureKind::generate_pieces`](crate::structure::StructureKind).
    pub(super) fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        mut world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        if self.structures.is_none() {
            return world;
        }
        let (bx, bz) = (cx * 16, cz * 16);
        for (_, _, start) in &self.structure_refs_stage(cx, cz).entries {
            if !start.pieces_complete {
                continue;
            }
            for piece in &start.pieces {
                let Some(placement) = &piece.placement else {
                    continue;
                };
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) {
                    continue;
                }
                placement
                    .template
                    .place(placement.position, &placement.settings, &mut world);
            }
        }
        world
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
