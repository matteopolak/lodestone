//! [`VegGrid`] — vegetal decoration's read/write surface over one column's 3×3
//! neighbourhood — and the [`census`] counters that make a silent no-op step visible.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::dense_grid::DenseBlockGrid;
use crate::feature::region_view::{
    Overlay, WIDE_RADIUS, WIDE_SLOTS, WriteLog, wide_slot_of_offset, wide_source_slot,
};
use crate::dense_grid::BaseStateFacts;
use crate::compose::FeatureBiomePlan;
use crate::feature::FeatureMembershipId;
use lodestone_data::block_states::StateId;
use crate::overworld::BiomeCells;
use lodestone_data::biomes::{BiomeRef, BuiltinBiome};

use self::census::bump as census_bump;

const HEIGHT_CACHE_UNSET: i32 = i32::MIN;

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
        is_fluid: lodestone_data::snow_support::has_fluid_state(state),
        blocks_motion: lodestone_data::block_solidity::blocks_motion(state),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeightLaneMask(u8);

impl HeightLaneMask {
    const LIVE_SURFACE: Self = Self(1 << 0);
    const WORLD_SURFACE_WG: Self = Self(1 << 1);
    const MOTION_BLOCKING: Self = Self(1 << 2);
    const OCEAN_FLOOR: Self = Self(1 << 3);
    const OCEAN_FLOOR_WG: Self = Self(1 << 4);
    const LIVE: Self = Self(
        Self::LIVE_SURFACE.0 | Self::MOTION_BLOCKING.0 | Self::OCEAN_FLOOR.0,
    );
    const WG: Self = Self(Self::WORLD_SURFACE_WG.0 | Self::OCEAN_FLOOR_WG.0);

    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn for_lane(lane: usize) -> Self {
        Self(1 << lane)
    }
}

/// Compact representation for a vertically invariant biome source. Name-based
/// compatibility inputs are converted at construction; retained grids never
/// carry a parallel string array.
#[derive(Debug)]
enum FlatBiomeSources {
    Ids {
        cells: [Option<Arc<[BiomeRef; 16]>>; WIDE_SLOTS],
    },
}

#[derive(Debug)]
struct DynamicSources {
    min_chunk_x: i32,
    min_chunk_z: i32,
    width: usize,
    depth: usize,
    blocks: Vec<Option<Arc<DenseBlockGrid>>>,
    biomes: Vec<Option<Arc<BiomeCells>>>,
}

/// Constructor adapter kept for compact legacy fixtures. Production passes
/// the immutable typed plan; the old string map is converted only at this
/// boundary and never retained by [`VegGrid`].
pub trait FeatureBiomeInput {
    fn into_feature_biome_plan(self) -> Arc<FeatureBiomePlan>;
}

impl FeatureBiomeInput for Arc<FeatureBiomePlan> {
    fn into_feature_biome_plan(self) -> Arc<FeatureBiomePlan> {
        self
    }
}

impl FeatureBiomeInput for Arc<HashMap<String, HashSet<String>>> {
    fn into_feature_biome_plan(self) -> Arc<FeatureBiomePlan> {
        Arc::new(FeatureBiomePlan::from_legacy(&self))
    }
}

impl DynamicSources {
    fn index(&self, chunk_x: i32, chunk_z: i32) -> Option<usize> {
        let x = usize::try_from(chunk_x - self.min_chunk_x).ok()?;
        let z = usize::try_from(chunk_z - self.min_chunk_z).ok()?;
        (x < self.width && z < self.depth).then_some(z * self.width + x)
    }
}

/// The mutable block field vegetal decoration reads and writes. Defaults to
/// chunk-local (`0..16` × `0..16`, absolute `y`) via [`VegGrid::new`] — see
/// module doc's "Scope" section for why single-chunk was this module's
/// original footprint. [`VegGrid::with_footprint`] widens the local bound to
/// an arbitrary `[lo, hi)` on both axes (the real vanilla 3×3
/// per-source write-radius driver uses [`crate::feature::REGION_MIN`]/
/// [`crate::feature::REGION_MAX`], the exact constants
/// [`crate::feature::OreInput::region_local`] already established for the
/// ore engine's own 3×3 driver — reused here rather than duplicated, per
/// CLAUDE.md's instruction to follow that precedent) with `origin_x`/
/// `origin_z` fixed at the **centre** chunk's own absolute origin, so every
/// one of the 9 sources' absolute-coordinate writes translates through the
/// same origin and lands (or is dropped) relative to the centre — exactly
/// [`crate::feature::OreInput`]'s `chunk_x`/`chunk_z` (varies per source) vs
/// `center_x`/`center_z` (fixed) split, applied to this module's own grid
/// type instead of introducing a second region-grid mechanism.
#[derive(Debug)]
pub struct VegGrid {
    /// Keyed by **local** `(0..16, y, 0..16)` — every public accessor takes
    /// **absolute world** coordinates (matching every `BlockPos` this
    /// engine's placement modifiers compute — noise sampling, the decoration
    /// seed, `RandomOffset`'s scatter, all of it is absolute-coordinate
    /// arithmetic, not local) and converts via `origin_x`/`origin_z`
    /// internally. Getting this translation wrong is exactly the bug this
    /// comment exists to prevent from recurring: an earlier version of this
    /// struct stored *and exposed* local coordinates, silently accepting the
    /// engine's absolute-coordinate `BlockPos`es and comparing them against
    /// a `0..16` bound that was almost always false — every placement
    /// attempt for any chunk other than `(0, 0)` failed `in_bounds`/`get`'s
    /// implicit "must already be local" assumption, so vegetation composed,
    /// ran, and reached zero blocks in every real served chunk. Caught by a
    /// sweep gate measuring **zero** grass/flowers/logs/leaves over a plains
    /// neighbourhood — this module's own hermetic unit tests never caught it
    /// because every one of them happened to place at `origin = BlockPos { x: 8,
    /// ... z: 8 }`, which is coincidentally already "local" (chunk (0,0)'s own
    /// footprint), the exact island CLAUDE.md's rule 1 describes: a unit
    /// test can be green while the real integration seam is broken.
    ///
    /// **That gate was then deleted, and this comment kept naming it** — it read
    /// `lodestone_server::worldgen_data::tests::diagnostic_vegetation_counts_over_plains_sweep`
    /// up to `074b5e9`, by which point no such test existed anywhere in the tree
    ///. So for an unknown span the repo held a written record of a
    /// regression with nothing watching for its return, and the reference read as
    /// coverage on inspection. The live gate is now
    /// `lodestone_server::worldgen_data::tests::vegetation_reaches_real_blocks_over_a_production_sweep`,
    /// with `plains_grass_patch_attempt_count_matches_the_placement_json` carrying
    /// the predicted magnitude — but treat *this sentence* as a claim like any
    /// other and grep for the name before trusting it.
    /// The value type is the canonical [`StateId`]; callers keep text only at
    /// configuration boundaries and use ids for every placement operation.
    ///
    /// **Unit 7 then deleted the seeding loop itself, and this map with it became
    /// a sparse *overlay*.** Unit 3 made a seeded cell a `u16` move rather than a
    /// `String` allocation, but there were still 884,736 of them per column —
    /// nine already-computed post-ore chunks copied, cell by cell, into a
    /// `HashMap` that then held 884,736 live entries. Production now supplies the
    /// nine grids as [`VegGrid::sources`] and this map holds **only what
    /// decoration wrote** (a few thousand cells), with a miss falling through to
    /// the source chunk that owns the column. [`Self::seed_id`] still writes
    /// fixture cells here (and mirrors their baseline into the separate WG
    /// snapshot), which keeps every parity fixture — naturally one hand-written
    /// sparse map with no source grids at all — on the identical read path.
    blocks: Overlay,
    overlay_absolute: bool,
    /// Baseline cells supplied by source-less fixtures. Production grids read
    /// their immutable terrain through `sources`; compact fixtures instead
    /// seed this sparse snapshot before decoration so the world-surface WG
    /// lane cannot mistake the absent source for an all-air column or observe
    /// a later live-overlay write.
    seeded_baseline: Option<Overlay>,
    /// The **25** source chunks of `centre ± `[`WIDE_RADIUS`] a read falls through
    /// to, indexed by [`wide_source_slot`] over this grid's **local** coordinates.
    /// Empty for every fixture/unit-test constructor (a miss then answers air,
    /// exactly as an unseeded cell always did); populated by [`Self::with_sources`]
    /// in production.
    ///
    /// **It is 25 and not 9 because of a measured seam defect, not for margin.**
    /// The 3×3 driver decorates nine sources and every one of them can write into
    /// the centre, so each source's placements must be a function of that source
    /// alone — the chunks either side of a seam recompute the same tree, and if
    /// their two computations disagree the served world keeps one half. A source at
    /// offset `(-1, 0)` reads up to [`crate::feature::VEG_PADDING`] blocks past its
    /// own west edge, i.e. into chunk offset `-2`. With a nine-slot table those
    /// columns answered **air**, and *where that boundary fell depended on which
    /// column was the centre*, so the source's own pass diverged between the two
    /// drives. See [`WIDE_RADIUS`] for the measurement (94 truncated seam rows over
    /// the 66 bundled biomes, 50 of them removed by this) and
    /// `tests/vegetation_seam_consistency.rs` for the live gate.
    ///
    /// The inner 3×3 carries **post-ore** terrain; the 16 rim chunks carry
    /// **pre-ore** terrain — see [`Self::with_sources`] for why that is exact for
    /// heightmaps and what it approximates.
    ///
    /// Read-only, and that is a rule rather than an accident: these are `Arc`
    /// snapshots shared with every other in-flight column that has the same
    /// neighbour, and the rewrite plan's parallel model requires each chunk's grid
    /// to have exactly one writer — its own serve task. Writes therefore go to
    /// `blocks` and the caller folds them into the one grid it owns.
    ///
    /// Note the source table is now **wider** than the footprint, which is the
    /// reverse of how this read before the 5×5 landed. `local_lo`/`local_hi` span
    /// `REGION_MIN - VEG_PADDING .. REGION_MAX + VEG_PADDING` = `[-24, 40)`, and the
    /// 5×5 covers `[-32, 48)` — so the padding ring **does** have a source now and
    /// answers real terrain rather than air. That is the whole point: the ring is
    /// exactly what a source at the edge of the decorated 3×3 reads into, and
    /// answering air there is what made its pass depend on the centre. Canopy
    /// spilling into the pad is still writable and still readable back, unchanged;
    /// only what an *unwritten* pad cell reads has changed.
    sources: [Option<Arc<DenseBlockGrid>>; WIDE_SLOTS],
    /// Request-scoped rectangular source layout. Unlike `sources`, this covers
    /// a whole multi-target write union and is addressed by absolute chunk.
    dynamic_sources: Option<DynamicSources>,
    /// The matching biome cells for [`Self::sources`]. `None` keeps compact
    /// feature fixtures independent of a biome source; production fills all
    /// slots so the biome placement modifier can query the candidate's 3-D cell.
    biome_sources: Option<[Option<Arc<BiomeCells>>; WIDE_SLOTS]>,
    /// Vertically invariant 4x4 biome slices, used by dimensions whose
    /// climate has no Y component. This avoids materialising repeated 3-D
    /// biome containers solely for the placement membership predicate.
    flat_biome_sources: Option<FlatBiomeSources>,
    /// Seed for the Overworld's three-dimensional nearby-corner biome lookup.
    /// `None` is retained by the dimension-neutral constructor used by the
    /// Nether, whose biome source supplies its own lookup in the decoration
    /// driver. Overworld grids opt in through
    /// [`Self::with_sources_and_biomes_shared_zoomed`].
    biome_zoom_seed: Option<i64>,
    /// Optional source-generation ceiling when resident grids are padded to a
    /// dimension window larger than the terrain generator's own height.
    generation_top_override: Option<i32>,
    /// Top-level placed-feature id to eligible biome ids. This is deliberately
    /// keyed by the placed feature, rather than its configured body: a selector
    /// branch can share a body while carrying a different placement contract.
    feature_biomes: Arc<FeatureBiomePlan>,
    /// Positions actually written by `set_id_if_in_bounds`, **local** (see
    /// `blocks`' doc), in write order — a `Vec`, not a re-iterated
    /// `HashMap`, specifically so a caller folding this back into a dense
    /// grid (`OverworldGenerator`'s vegetation stage) has a *deterministic*
    /// order to replay, the same discipline `docs/worldgen-parity.md`'s
    /// "Performance" section describes fixing for `surface_diff` (point
    /// lookups inside a fixed loop, never a raw `HashMap` iteration) — here
    /// achieved even more directly, since insertion order into a `Vec`
    /// carries no ambiguity to begin with. Lets the fold-back touch only the
    /// (typically small) written subset instead of rewriting all
    /// `16 × height × 16` cells.
    dirty: WriteLog,
    /// Ore writes are held separately until their entry finishes so the
    /// established `(x, z, y)` transfer order remains observable without a
    /// second world medium.
    ore_writes: Vec<(i32, i32, i32)>,
    ore_entry_active: bool,
    structure_mutation_capture: Option<Vec<(i32, i32, i32, StateId)>>,
    epoch_target_prepared: bool,
    origin_x: i32,
    origin_z: i32,
pub(super)     min_y: i32,
pub(super)     height: i32,
    /// Local-coordinate bound `[local_lo, local_hi)` on both `lx` and `lz` —
    /// `(0, 16)` for the single-chunk case ([`VegGrid::new`]), widened to
    /// [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`] for the
    /// 3×3 driver ([`VegGrid::with_footprint`],
    /// [`apply_vegetal_decoration_step_3x3_per_source`]).
    local_lo: i32,
    local_hi: i32,
    /// The ids of `minecraft:{air,cave_air,void_air}`,
    /// stored once here so [`Self::height_world_surface`]'s per-cell air test is
    /// three integer compares rather than a registry read. See
    /// [`Self::is_air_id`] for why an id comparison is exact for air. Unit 8.
    air_ids: [StateId; 3],
    /// Memoised heightmap results, indexed by the local `(x, z)` column.
    ///
    /// Height queries are frequent during vegetation placement. A miss walks
    /// the vertical span once for the compatible live lanes, or separately for
    /// the immutable WG lanes; each cache memoises its result for the local
    /// column. They are interior-mutable because the public height accessors
    /// intentionally stay shared (`&self`), while writes invalidate only the
    /// column they touch.
    /// A single five-lane cell keeps the caches compact (one allocation and
    /// 20 bytes per column). [`HEIGHT_CACHE_UNSET`] is outside the generated build range
    /// and represents an uncomputed lane.
    height_cache: Vec<Cell<[i32; 5]>>,
    local_width: usize,
    /// Block entities decoration produced, with **absolute** world
    /// positions, in write order. Alongside [`Self::dirty`] for the same reason
    /// that is a `Vec` and not a map: insertion order carries no ambiguity, and a
    /// generated column has at most a handful.
    ///
    /// Every constructor leaves this empty and only [`Self::push_block_entity`]
    /// ever grows it, so a fixture that never decorates a beehive sees exactly the
    /// behaviour from before block-entity decoration existed.
    block_entities: Vec<crate::overworld::block_entities::GeneratedBlockEntity>,
}

impl VegGrid {
    /// `origin_x`/`origin_z` are the chunk's own **absolute** block origin
    /// (`chunk_x * 16`, `chunk_z * 16`) — every other method on this type
    /// takes absolute world coordinates and translates through these.
    /// Single-chunk footprint (`0..16` on both axes) — see
    /// [`VegGrid::with_footprint`] for the 3×3 driver's widened case.
    #[must_use]
    pub fn new(min_y: i32, height: i32, origin_x: i32, origin_z: i32) -> Self {
        Self::with_footprint(min_y, height, origin_x, origin_z, 0, 16)
    }

    /// Like [`VegGrid::new`], but with an explicit local-coordinate bound
    /// `[local_lo, local_hi)` on both `lx` and `lz` instead of the hardcoded
    /// `0..16` — the real vanilla 3×3 `blockStateWriteRadius(1)` driver
    /// passes [`crate::feature::REGION_MIN`]/[`crate::feature::REGION_MAX`]
    /// here with `origin_x`/`origin_z` fixed at the **centre** chunk's own
    /// origin (see this struct's own doc comment).
    #[must_use]
    pub fn with_footprint(min_y: i32, height: i32, origin_x: i32, origin_z: i32, local_lo: i32, local_hi: i32) -> Self {
        Self::with_footprint_canonical(min_y, height, origin_x, origin_z, local_lo, local_hi)
    }

    /// Canonical constructor used by all sources and fixtures.
    #[must_use]
    pub fn with_footprint_canonical(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
    ) -> Self {
        let air_ids = [
            lodestone_data::block::Block::Air.default_state(),
            lodestone_data::block::Block::CaveAir.default_state(),
            lodestone_data::block::Block::VoidAir.default_state(),
        ];
        let local_width = usize::try_from(local_hi - local_lo)
            .expect("VegGrid footprint must have a non-negative width");
        let column_count = local_width * local_width;
        Self {
            blocks: Overlay::with_bounds(local_lo, local_hi, min_y, height),
            overlay_absolute: false,
            seeded_baseline: None,
            sources: std::array::from_fn(|_| None),
            dynamic_sources: None,
            biome_sources: None,
            flat_biome_sources: None,
            biome_zoom_seed: None,
            generation_top_override: None,
            feature_biomes: Arc::new(FeatureBiomePlan::default()),
            dirty: WriteLog::default(),
            ore_writes: Vec::new(),
            ore_entry_active: false,
            structure_mutation_capture: None,
            epoch_target_prepared: false,
            origin_x,
            origin_z,
            min_y,
            height,
            local_lo,
            local_hi,
            air_ids,
            height_cache: std::iter::repeat_with(|| Cell::new([HEIGHT_CACHE_UNSET; 5]))
                .take(column_count)
                .collect(),
            local_width,
            block_entities: Vec::new(),
        }
    }

    /// [`VegGrid::with_footprint_canonical`] over the read neighbourhood's **own**
    /// grids instead of a seeded copy of them — the production form since Unit 7
    /// of `docs/plans/worldgen-rewrite.md`.
    ///
    /// `source_at(dx, dz)` is called once per chunk offset in
    /// `[-`[`WIDE_RADIUS`]`, `[`WIDE_RADIUS`]`]²` — **25 offsets, not 9** — and
    /// returns that chunk's terrain (absolute-coordinate addressed), or `None` to
    /// make that chunk read as air, which is exactly what the
    /// `LODESTONE_VEG_SINGLE_SOURCE_DEBUG` path wants for everything but the centre.
    ///
    /// # Why 25, and what the rim is allowed to be
    ///
    /// See the [`Self::sources`] field doc for the defect. The consequence for a
    /// caller is a split:
    ///
    /// * the **inner 3×3** are the chunks the driver decorates, and they must carry
    ///   that chunk's **post-ore** world — decoration reads and writes against it,
    ///   and it is what vanilla's FEATURES stage sees;
    /// * the **16 rim chunks** are read-only context. `crate::overworld` supplies
    ///   **pre-ore** terrain there, because a column's pre-ore closure is already
    ///   the 5×5 (`overworld::COLUMN_CLOSURE_RADIUS`) and every one of those 25
    ///   results is already memoised for every served column — so the rim costs
    ///   *no* extra pipeline work, where post-ore terrain would widen the closure
    ///   to 7×7. That is **exact for both heightmaps**: ore placement *replaces*
    ///   blocks rather than adding or removing them, so the topmost non-air `y` is
    ///   identical either way. It approximates only a state *identity* check landing
    ///   on a cell an ore blob replaced, ≥16 blocks from the chunk being served.
    ///
    /// The slots are filled through [`wide_source_slot`], the same function every
    /// read routes with, so the fill convention and the lookup convention cannot
    /// drift apart. Getting that wrong is the recorded `VegGrid` failure mode — see
    /// this type's own doc comment.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
    ) -> Self {
        let mut grid = Self::with_footprint_canonical(
            min_y, height, origin_x, origin_z, local_lo, local_hi,
        );
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                let slot = wide_source_slot(dx * 16, dz * 16)
                    .expect("a 5x5 offset's own origin column is inside the read region");
                debug_assert_eq!(slot, wide_slot_of_offset(dx, dz));
                grid.sources[slot] = source_at(dx, dz);
            }
        }
        grid
    }

    /// [`Self::with_sources`] plus the 3-D biome data and placed-feature
    /// membership required by the `biome` placement modifier. The terrain and
    /// biome closures use the same 5×5 offset convention, so a candidate always
    /// reads the biome column that owns its terrain position.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_biomes(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: HashMap<String, HashSet<String>>,
    ) -> Self {
        Self::with_sources_and_biomes_shared(
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
            source_at,
            biome_at,
            Arc::new(feature_biomes),
        )
    }

    /// Shared-map form used by production generators. The feature admission
    /// map is immutable for a generator lifetime; cloning its `Arc` avoids
    /// rebuilding every feature and biome string for each served column.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_biomes_shared(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: impl FeatureBiomeInput,
    ) -> Self {
        Self::with_sources_and_biomes_shared_impl(
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
            source_at,
            biome_at,
            feature_biomes,
            None,
        )
    }

    /// Shared-map Overworld form. Its biome placement modifier resolves the
    /// candidate through the same seed-derived three-dimensional zoom used by
    /// the world biome accessor, rather than reading the candidate's containing
    /// quart directly.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_biomes_shared_zoomed(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: impl FeatureBiomeInput,
        biome_zoom_seed: i64,
    ) -> Self {
        Self::with_sources_and_biomes_shared_impl(
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
            source_at,
            biome_at,
            feature_biomes,
            Some(biome_zoom_seed),
        )
    }

    /// Builds one request-scoped grid over an arbitrary rectangular source
    /// layout. The callback coordinates are absolute chunk coordinates; the
    /// grid itself remains absolute-coordinate addressed, so adjacent source
    /// bodies share one overlay without translating their vegetation writes.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_dynamic_sources_and_biomes_shared_zoomed(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        min_chunk_x: i32,
        min_chunk_z: i32,
        width: usize,
        depth: usize,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: impl FeatureBiomeInput,
        biome_zoom_seed: i64,
    ) -> Self {
        assert!(width > 0 && depth > 0, "dynamic source layout must be non-empty");
        let mut grid = Self::with_footprint_canonical(
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
        );
        let mut blocks = Vec::with_capacity(width * depth);
        let mut biomes = Vec::with_capacity(width * depth);
        for chunk_z in min_chunk_z..min_chunk_z + i32::try_from(depth).expect("source depth") {
            for chunk_x in min_chunk_x..min_chunk_x + i32::try_from(width).expect("source width") {
                blocks.push(source_at(chunk_x, chunk_z));
                biomes.push(biome_at(chunk_x, chunk_z));
            }
        }
        grid.dynamic_sources = Some(DynamicSources {
            min_chunk_x,
            min_chunk_z,
            width,
            depth,
            blocks,
            biomes,
        });
        grid.biome_zoom_seed = Some(biome_zoom_seed);
        grid.feature_biomes = feature_biomes.into_feature_biome_plan();
        grid
    }

    pub(crate) fn install_epoch_overlay(&mut self, overlay: Overlay) {
        self.blocks = overlay;
        self.overlay_absolute = true;
    }

    pub(crate) fn begin_epoch_target(&mut self, chunk_x: i32, chunk_z: i32) {
        debug_assert!(self.overlay_absolute);
        self.origin_x = chunk_x * 16;
        self.origin_z = chunk_z * 16;
        self.dirty.clear();
        self.ore_writes.clear();
        self.ore_entry_active = false;
        self.structure_mutation_capture = None;
        for cache in &self.height_cache {
            cache.set([HEIGHT_CACHE_UNSET; 5]);
        }
        self.epoch_target_prepared = true;
    }

    pub(crate) fn begin_epoch_target_if_needed(&mut self, chunk_x: i32, chunk_z: i32) {
        if self.epoch_target_prepared {
            self.epoch_target_prepared = false;
        } else {
            self.begin_epoch_target(chunk_x, chunk_z);
        }
    }

    /// Shared-map form for a vertically invariant biome source. Candidate
    /// selection still uses the normal seed-derived nearby-corner zoom; only
    /// the final quart lookup reads a compact 4x4 typed slice instead of a
    /// vertically repeated [`BiomeCells`] allocation. The name-taking form is
    /// a configuration/test adapter; names are parsed before the grid retains
    /// the source.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_flat_biomes_shared_zoomed(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<[String; 16]>>,
        feature_biomes: impl FeatureBiomeInput,
        biome_zoom_seed: i64,
    ) -> Self {
        let mut grid = Self::with_sources(
            min_y, height, origin_x, origin_z, local_lo, local_hi, source_at,
        );
        let mut biomes = std::array::from_fn(|_| None);
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                biomes[wide_slot_of_offset(dx, dz)] = biome_at(dx, dz).map(|names| {
                    Arc::new(std::array::from_fn(|index| {
                        let biome = BuiltinBiome::parse(&names[index]).unwrap_or_else(|error| {
                            panic!("flat biome source is not a generated built-in: {error}")
                        });
                        BiomeRef::builtin(biome)
                    }))
                });
            }
        }
        grid.flat_biome_sources = Some(FlatBiomeSources::Ids { cells: biomes });
        grid.biome_zoom_seed = Some(biome_zoom_seed);
        grid.feature_biomes = feature_biomes.into_feature_biome_plan();
        grid
    }

    /// Shared-map form for a vertically invariant biome source whose cells are
    /// constructor-assigned ids. Keeping ids in the producer's immutable cache
    /// avoids retaining sixteen owned biome strings per cached Nether column;
    /// names are resolved only while evaluating the placement membership query.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_flat_biome_ids_shared_zoomed(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<[BiomeRef; 16]>>,
        feature_biomes: impl FeatureBiomeInput,
        biome_zoom_seed: i64,
    ) -> Self {
        let mut grid = Self::with_sources(
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
            source_at,
        );
        let mut biomes = std::array::from_fn(|_| None);
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                biomes[wide_slot_of_offset(dx, dz)] = biome_at(dx, dz);
            }
        }
        grid.flat_biome_sources = Some(FlatBiomeSources::Ids { cells: biomes });
        grid.biome_zoom_seed = Some(biome_zoom_seed);
        grid.feature_biomes = feature_biomes.into_feature_biome_plan();
        grid
    }

    #[allow(clippy::too_many_arguments)]
    fn with_sources_and_biomes_shared_impl(
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: impl FeatureBiomeInput,
        biome_zoom_seed: Option<i64>,
    ) -> Self {
        let mut grid = Self::with_sources(
            min_y, height, origin_x, origin_z, local_lo, local_hi, source_at,
        );
        let mut biomes = std::array::from_fn(|_| None);
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                let slot = wide_slot_of_offset(dx, dz);
                biomes[slot] = biome_at(dx, dz);
            }
        }
        grid.biome_sources = Some(biomes);
        grid.biome_zoom_seed = biome_zoom_seed;
        grid.feature_biomes = feature_biomes.into_feature_biome_plan();
        grid
    }

    /// Whether the typed biome at this exact candidate location lists a
    /// compiled feature token.
    /// A grid without biome sources is a compact unit fixture and deliberately
    /// preserves the historical unconstrained behaviour. A production grid
    /// rejects an inline/unidentified feature, missing sources, and out-of-range
    /// cells instead of turning an unknown membership into a permissive answer.
    #[must_use]
    pub fn biome_allows_membership(
        &self,
        membership: FeatureMembershipId,
        x: i32,
        y: i32,
        z: i32,
    ) -> bool {
        if let Some(dynamic) = &self.dynamic_sources {
            let Some(zoom_seed) = self.biome_zoom_seed else {
                return false;
            };
            let biome = crate::overworld::zoomed_biome_ref(
                zoom_seed,
                x,
                y,
                z,
                |source_chunk_x, source_chunk_z| {
                    dynamic.index(source_chunk_x, source_chunk_z).and_then(|index| {
                        dynamic.biomes[index].as_deref()
                    })
                },
            );
            return biome.is_some_and(|biome| self.feature_biomes.allows(membership, biome));
        }
        let Some(sources) = &self.biome_sources else {
            if let Some(flat) = &self.flat_biome_sources {
                let Some(zoom_seed) = self.biome_zoom_seed else {
                    return false;
                };
                let centre_chunk_x = self.origin_x.div_euclid(16);
                let centre_chunk_z = self.origin_z.div_euclid(16);
                let source_at = |source_chunk_x: i32,
                                 source_chunk_z: i32,
                                 qx: usize,
                                 qz: usize|
                 -> Option<BiomeRef> {
                    let dx = source_chunk_x - centre_chunk_x;
                    let dz = source_chunk_z - centre_chunk_z;
                    let slot = wide_source_slot(dx * 16, dz * 16)?;
                    census::record_source_slot(slot);
                    let cells = match flat {
                        FlatBiomeSources::Ids { cells } => cells,
                    };
                    let cells = cells[slot].as_deref()?;
                    Some(cells[qz * 4 + qx])
                };
                let biome = crate::overworld::zoomed_biome_flat(zoom_seed, x, y, z, source_at);
                return biome.is_some_and(|biome| self.feature_biomes.allows(membership, biome));
            }
            return true;
        };
        let biome = if let Some(zoom_seed) = self.biome_zoom_seed {
            let centre_chunk_x = self.origin_x.div_euclid(16);
            let centre_chunk_z = self.origin_z.div_euclid(16);
            crate::overworld::zoomed_biome_ref(
                zoom_seed,
                x,
                y,
                z,
                |source_chunk_x, source_chunk_z| {
                    let dx = source_chunk_x - centre_chunk_x;
                    let dz = source_chunk_z - centre_chunk_z;
                    wide_source_slot(dx * 16, dz * 16).and_then(|slot| {
                        census::record_source_slot(slot);
                        sources[slot].as_deref()
                    })
                },
            )
        } else {
            let lx = x - self.origin_x;
            let lz = z - self.origin_z;
            let Some(slot) = wide_source_slot(lx, lz) else {
                return false;
            };
            census::record_source_slot(slot);
            let Some(cells) = sources[slot].as_deref() else {
                return false;
            };
            let qx = lx.rem_euclid(16).div_euclid(4) as usize;
            let qz = lz.rem_euclid(16).div_euclid(4) as usize;
            let qy = (y - cells.min_y()).div_euclid(4);
            if qy < 0 || qy >= cells.y_quarts() as i32 {
                return false;
            }
            Some(cells.at_quart_ref(qx, qy as usize, qz))
        };
        let allowed = biome.is_some_and(|biome| self.feature_biomes.allows(membership, biome));
        allowed
    }

    /// Compatibility boundary for inline/test placed records. Production
    /// records are bound to [`Self::biome_allows_membership`] once at catalog
    /// construction and do not enter this name-to-token lookup.
    #[must_use]
    pub fn biome_allows_placed_feature(
        &self,
        feature_id: Option<&str>,
        x: i32,
        y: i32,
        z: i32,
    ) -> bool {
        let Some(feature_id) = feature_id else {
            return self.biome_sources.is_none() && self.flat_biome_sources.is_none();
        };
        self.feature_biomes
            .token_for(feature_id)
            .is_some_and(|membership| self.biome_allows_membership(membership, x, y, z))
    }

    /// The state one of the 25 source chunks holds at **local** `(lx, y, lz)`,
    /// or [`StateId::AIR`] when no source owns that column (outside
    /// `centre ± `[`WIDE_RADIUS`], a fixture with no sources, or a `None` slot).
    fn source_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        self.source_id_from_grid(self.source_grid(lx, lz), lx, y, lz)
    }

    /// Resolves the source grid for one horizontal column. Keeping this
    /// separate from [`Self::source_id`] lets a vertical heightmap scan route
    /// once and reuse the same source for every Y instead of repeating the
    /// chunk-band calculation for each cell.
    #[inline]
    fn source_grid(&self, lx: i32, lz: i32) -> Option<&DenseBlockGrid> {
        if let Some(dynamic) = &self.dynamic_sources {
            let chunk_x = (self.origin_x + lx).div_euclid(16);
            let chunk_z = (self.origin_z + lz).div_euclid(16);
            return dynamic
                .index(chunk_x, chunk_z)
                .and_then(|index| dynamic.blocks[index].as_deref());
        }
        wide_source_slot(lx, lz).and_then(|slot| {
            census::record_source_slot(slot);
            self.sources[slot].as_deref()
        })
    }

    #[inline]
    fn source_id_from_grid(
        &self,
        source: Option<&DenseBlockGrid>,
        lx: i32,
        y: i32,
        lz: i32,
    ) -> StateId {
        source.map_or(StateId::AIR, |grid| {
            #[cfg(feature = "gen-counters")]
            census::record_source_read(grid, self.origin_x + lx, y, self.origin_z + lz, 1);
            grid.get_id(self.origin_x + lx, y, self.origin_z + lz)
        })
    }

    /// Exclusive upper Y bound of the generated source field, distinct from
    /// this grid's receiving window. Nether decoration keeps a widened window
    /// so upper-half writes remain visible to later features, while some
    /// feature bounds are relative to the source generator's depth instead.
    #[must_use]
    pub(super) fn generation_top(&self) -> i32 {
        if let Some(top) = self.generation_top_override {
            return top;
        }
        let centre = wide_slot_of_offset(0, 0);
        self.sources[centre]
            .as_ref()
            .map_or(self.min_y + self.height, |source| {
                let (_, min_y, _, _, height, _) = source.bounds();
                min_y + height
            })
    }

    /// Records a block entity decoration produced, at an **absolute**
    /// world position. Unbounded by the grid's footprint on purpose — the caller
    /// filters to the served chunk, exactly as it does for [`Self::dirty_cells`].
    pub fn push_block_entity(
        &mut self,
        entity: crate::overworld::block_entities::GeneratedBlockEntity,
    ) {
        self.block_entities.push(entity);
    }

    /// Takes the recorded block entities, leaving the list empty. Draining rather
    /// than borrowing because the one consumer moves them into the served column.
    pub fn take_block_entities(
        &mut self,
    ) -> Vec<crate::overworld::block_entities::GeneratedBlockEntity> {
        std::mem::take(&mut self.block_entities)
    }

    /// Positions written by `set_id_if_in_bounds` since construction, in write
    /// order, **as absolute world coordinates**, each paired with the state
    /// currently at that position (i.e. the *final* state if the same cell
    /// was written more than once, not an intermediate one) — what a caller
    /// should fold back into a wider grid, with no further translation
    /// needed.
    /// Positions written by `set_id_if_in_bounds`, with their local state ids.
    pub fn dirty_cells(&self) -> impl Iterator<Item = (i32, i32, i32, StateId)> {
        self.dirty.iter().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks
                    .get_in_bounds(&self.overlay_key(lx, y, lz))
                    .unwrap_or(StateId::AIR),
            )
        })
    }

    pub(crate) fn dirty_cell_ids_reverse(
        &self,
    ) -> impl Iterator<Item = (i32, i32, i32, StateId)> {
        self.dirty.iter_rev().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks
                    .get_in_bounds(&self.overlay_key(lx, y, lz))
                    .unwrap_or(StateId::AIR),
            )
        })
    }

    pub(crate) fn overlay_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        let (lx, lz) = self.to_local_exact(x, z);
        if !self.in_bounds_local(lx, lz)
            || !(self.min_y..self.min_y + self.height).contains(&y)
        {
            return None;
        }
        self.blocks.get_in_bounds(&self.overlay_key(lx, y, lz))
    }

    pub(crate) fn overlay_len(&self) -> usize {
        self.blocks.len()
    }

    pub(crate) fn begin_structure_mutation_capture(&mut self) {
        self.structure_mutation_capture = Some(Vec::new());
    }

    pub(crate) fn take_structure_mutation_capture(
        &mut self,
    ) -> Option<Vec<(i32, i32, i32, StateId)>> {
        self.structure_mutation_capture.take()
    }

    /// Keep feature height guards tied to the terrain source when resident
    /// grids include padded rows above the generated field.
    pub(crate) fn set_generation_top(&mut self, top: i32) {
        self.generation_top_override = Some(top);
    }

    /// The number of writes recorded so far — a caller (currently only
    /// [`place_tree`]) that brackets a `dirty_len()` call before and after a
    /// span of writes and then reads `dirty_cells().skip(before)` gets
    /// exactly the absolute-coordinate positions written in that span, in
    /// order. Used to compute one tree's own `trunks ∪ foliage ∪
    /// decorations` bounding box for [`update_leaf_distances`] — see that
    /// function's own doc comment for why the bound matters.
    #[must_use]
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    /// Current writes for one absolute chunk, in the dense grid's stable
    /// `(y, z, x)` fold order. Repeated writes collapse to their final state.
    pub fn writes_for_chunk_in_scan_order(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Vec<(i32, i32, i32, StateId)> {
        self.writes_for_chunks_in_scan_order(&[(chunk_x, chunk_z)])
            .pop()
            .expect("one requested chunk projection")
    }

    /// Projects all requested chunks from the write log in one pass. The
    /// per-chunk vectors are sorted only after routing so each dense fold keeps
    /// its established y,z,x order without rescanning the shared log.
    pub fn writes_for_chunks_in_scan_order(
        &self,
        chunks: &[(i32, i32)],
    ) -> Vec<Vec<(i32, i32, i32, StateId)>> {
        let mut chunk_indices = HashMap::with_capacity(chunks.len());
        for (index, &chunk) in chunks.iter().enumerate() {
            chunk_indices.insert(chunk, index);
        }
        let mut writes = (0..chunks.len()).map(|_| Vec::new()).collect::<Vec<_>>();
        for write in self.dirty_cells() {
            let chunk = (write.0.div_euclid(16), write.2.div_euclid(16));
            if let Some(&index) = chunk_indices.get(&chunk) {
                writes[index].push(write);
            }
        }
        for writes in &mut writes {
            writes.sort_unstable_by_key(|&(x, y, z, _)| (y, z, x));
            let mut unique = 0usize;
            for read in 0..writes.len() {
                if unique > 0
                    && writes[unique - 1].0 == writes[read].0
                    && writes[unique - 1].1 == writes[read].1
                    && writes[unique - 1].2 == writes[read].2
                {
                    writes[unique - 1] = writes[read];
                } else {
                    writes[unique] = writes[read];
                    unique += 1;
                }
            }
            writes.truncate(unique);
        }
        writes
    }

    /// Conservative request accounting for the grid's mutable region and
    /// write log. Source `Arc`s are borrowed and therefore excluded.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.blocks.len() * std::mem::size_of::<(i32, i32, i32, StateId)>()
            + self.dirty_len() * std::mem::size_of::<(i32, i32, i32)>()
            + self.height_cache.capacity() * std::mem::size_of::<Cell<[i32; 5]>>()
            + self
                .dynamic_sources
                .as_ref()
                .map_or(0, |sources| {
                    sources.blocks.capacity() * std::mem::size_of::<Option<Arc<DenseBlockGrid>>>()
                        + sources.biomes.capacity() * std::mem::size_of::<Option<Arc<BiomeCells>>>()
                })
    }

    fn in_bounds_local(&self, lx: i32, lz: i32) -> bool {
        (self.local_lo..self.local_hi).contains(&lx) && (self.local_lo..self.local_hi).contains(&lz)
    }

    #[inline]
    fn height_cache_index(&self, lx: i32, lz: i32) -> usize {
        debug_assert!(self.in_bounds_local(lx, lz));
        ((lz - self.local_lo) as usize) * self.local_width + (lx - self.local_lo) as usize
    }

    #[inline]
    fn invalidate_height_caches(&self, lx: i32, lz: i32) {
        let index = self.height_cache_index(lx, lz);
        let mut cache = self.height_cache[index].get();
        cache[0] = HEIGHT_CACHE_UNSET;
        cache[2] = HEIGHT_CACHE_UNSET;
        cache[3] = HEIGHT_CACHE_UNSET;
        self.height_cache[index].set(cache);
    }

    #[inline]
    fn cached_height(&self, lx: i32, lz: i32, lane: usize) -> Option<i32> {
        let value = self.height_cache[self.height_cache_index(lx, lz)].get()[lane];
        (value != HEIGHT_CACHE_UNSET).then_some(value)
    }

    #[inline]
    fn cache_height(&self, lx: i32, lz: i32, lane: usize, value: i32) {
        debug_assert_ne!(value, HEIGHT_CACHE_UNSET);
        let index = self.height_cache_index(lx, lz);
        let mut cache = self.height_cache[index].get();
        cache[lane] = value;
        self.height_cache[index].set(cache);
    }

    #[inline]
    fn uncached_height_lanes(&self, lx: i32, lz: i32, lanes: HeightLaneMask) -> HeightLaneMask {
        let cache = self.height_cache[self.height_cache_index(lx, lz)].get();
        let mut uncached = HeightLaneMask(0);
        for (lane, value) in cache.into_iter().enumerate() {
            if value == HEIGHT_CACHE_UNSET {
                uncached = HeightLaneMask(uncached.0 | (1 << lane));
            }
        }
        HeightLaneMask(lanes.0 & uncached.0)
    }

    /// Absolute world `(x, z)` -> local `[local_lo, local_hi)`, **clamped**
    /// into range — used only by read paths, which must always answer
    /// something.
    fn to_local_clamped(&self, x: i32, z: i32) -> (i32, i32) {
        (
            (x - self.origin_x).clamp(self.local_lo, self.local_hi - 1),
            (z - self.origin_z).clamp(self.local_lo, self.local_hi - 1),
        )
    }

    /// Absolute world `(x, z)` -> local, **unclamped** — used only by the
    /// write path, which must know whether the position genuinely falls
    /// inside this chunk's own footprint rather than silently relocating a
    /// write to the nearest edge.
    fn to_local_exact(&self, x: i32, z: i32) -> (i32, i32) {
        (x - self.origin_x, z - self.origin_z)
    }

    #[inline]
    fn overlay_key(&self, lx: i32, y: i32, lz: i32) -> (i32, i32, i32) {
        if self.overlay_absolute {
            (self.origin_x + lx, y, self.origin_z + lz)
        } else {
            (lx, y, lz)
        }
    }

    /// Seeds one column position (absolute world coordinates) from the
    /// post-ore composed grid. Callers fill every `(x, y, z)` in this
    /// chunk's own `16 × height × 16` footprint before running vegetal
    /// decoration.
    /// Seeds one column position (absolute world coordinates) from the post-ore
    /// composed grid, by interned id — the zero-allocation seeding path.
    /// Source-less fixtures retain a second sparse copy for the immutable WG
    /// heightmaps; source-backed production grids continue to read those lanes
    /// from their source snapshot.
    pub fn seed_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            let key = self.overlay_key(lx, y, lz);
            self.blocks.insert_in_bounds(key, state);
            self.invalidate_height_caches(lx, lz);
            if self.seeded_baseline.is_some() || self.sources.iter().all(Option::is_none) {
                // Source-less fixtures store their immutable baseline in a
                // separate sparse snapshot. Seeding after a prior probe must
                // invalidate the WG lanes; ordinary decoration writes
                // intentionally do not, because those lanes are immutable for
                // the pass.
                let local_lo = self.local_lo;
                let local_hi = self.local_hi;
                let min_y = self.min_y;
                let height = self.height;
                let baseline = self.seeded_baseline.get_or_insert_with(|| {
                    Overlay::with_bounds(local_lo, local_hi, min_y, height)
                });
                baseline.insert_in_bounds((lx, y, lz), state);
                let cache_index = self.height_cache_index(lx, lz);
                let cache = self.height_cache[cache_index].get_mut();
                cache[1] = HEIGHT_CACHE_UNSET;
                cache[4] = HEIGHT_CACHE_UNSET;
            }
        }
    }

    /// Seed one absolute cell into an epoch overlay whose bounds span more
    /// than the currently selected target. Unlike [`Self::seed_id`], this
    /// route does not interpret the coordinates through the current target's
    /// local footprint; the direct overlay is already globally addressed.
    /// This is used for lifecycle revisions so a write consumed while one
    /// target is active remains visible when a later target selects the same
    /// shared grid.
    pub(crate) fn seed_epoch_absolute_id(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: StateId,
    ) -> bool {
        if !self.overlay_absolute
            || !(self.min_y..self.min_y + self.height).contains(&y)
            || !self.blocks.insert_if_in_bounds((x, y, z), state)
        {
            return false;
        }
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) {
            self.invalidate_height_caches(lx, lz);
        }
        true
    }

    /// This pass's own write if there is one, else the source chunk that owns the
    /// column, else air.
    ///
    /// Overlay-first is load-bearing, not an optimisation: vanilla's heightmaps
    /// update as decoration places blocks, so a tree placed earlier in the step
    /// must be visible to a later `height_world_surface` probe in the same step.
    /// A post-pass merge of the writes would answer stale and is parity-unsafe.
    fn get_local_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        if y < self.min_y || y >= self.min_y + self.height {
            return StateId::AIR;
        }
        match self.blocks.get_in_bounds(&self.overlay_key(lx, y, lz)) {
            Some(id) => id,
            None => self.source_id(lx, y, lz),
        }
    }

    #[inline]
    fn live_id_and_facts(
        &self,
        source: Option<&DenseBlockGrid>,
        lx: i32,
        y: i32,
        lz: i32,
    ) -> (StateId, BaseStateFacts) {
        if y < self.min_y || y >= self.min_y + self.height {
            return (StateId::AIR, BaseStateFacts::air());
        }
        if let Some(id) = self.blocks.get_in_bounds(&self.overlay_key(lx, y, lz)) {
            return (id, base_facts(id));
        }
        source.map_or((StateId::AIR, BaseStateFacts::air()), |source| {
            #[cfg(feature = "gen-counters")]
            census::record_source_read(source, self.origin_x + lx, y, self.origin_z + lz, 2);
            (
                source.get_id(self.origin_x + lx, y, self.origin_z + lz),
                source.get_base_facts(self.origin_x + lx, y, self.origin_z + lz),
            )
        })
    }

    #[inline]
    fn worldgen_id(
        &self,
        source: Option<&DenseBlockGrid>,
        lx: i32,
        y: i32,
        lz: i32,
    ) -> StateId {
        source.map_or_else(
            || {
                self.seeded_baseline
                    .as_ref()
                    .and_then(|baseline| baseline.get_in_bounds(&(lx, y, lz)))
                    .unwrap_or(StateId::AIR)
            },
            |source| {
                #[cfg(feature = "gen-counters")]
                census::record_source_read(source, self.origin_x + lx, y, self.origin_z + lz, 1);
                source.get_id(self.origin_x + lx, y, self.origin_z + lz)
            },
        )
    }

    fn scan_height_lanes(
        &self,
        lx: i32,
        lz: i32,
        requested: HeightLaneMask,
        primary: HeightLaneMask,
    ) {
        let mut pending = self.uncached_height_lanes(lx, lz, requested);
        if pending.is_empty() {
            return;
        }
        census_bump(|c| c.height_scans += 1);
        let source = self.source_grid(lx, lz);
        for y in (self.min_y..self.min_y + self.height).rev() {
            census_bump(|c| c.height_scan_cells += 1);
            if pending.contains(primary) {
                census_bump(|c| c.height_primary_cells += 1);
            } else if !pending.is_empty() {
                census_bump(|c| c.height_companion_tail_cells += 1);
            }
            if !pending.intersection(HeightLaneMask::LIVE).is_empty() {
                let (id, facts) = self.live_id_and_facts(source, lx, y, lz);
                if pending.contains(HeightLaneMask::LIVE_SURFACE) && !self.is_air_id(id) {
                    self.cache_height(lx, lz, 0, y + 1);
                    pending = pending.without(HeightLaneMask::LIVE_SURFACE);
                }
                if pending.contains(HeightLaneMask::MOTION_BLOCKING) && facts.is_motion_blocking() {
                    self.cache_height(lx, lz, 2, y + 1);
                    pending = pending.without(HeightLaneMask::MOTION_BLOCKING);
                }
                if pending.contains(HeightLaneMask::OCEAN_FLOOR) && facts.is_ocean_floor() {
                    self.cache_height(lx, lz, 3, y + 1);
                    pending = pending.without(HeightLaneMask::OCEAN_FLOOR);
                }
            }
            if !pending.intersection(HeightLaneMask::WG).is_empty() {
                let id = self.worldgen_id(source, lx, y, lz);
                if pending.contains(HeightLaneMask::WORLD_SURFACE_WG) && !self.is_air_id(id) {
                    self.cache_height(lx, lz, 1, y + 1);
                    pending = pending.without(HeightLaneMask::WORLD_SURFACE_WG);
                }
                if pending.contains(HeightLaneMask::OCEAN_FLOOR_WG)
                    && base_facts(id).is_ocean_floor()
                {
                    self.cache_height(lx, lz, 4, y + 1);
                    pending = pending.without(HeightLaneMask::OCEAN_FLOOR_WG);
                }
            }
            if pending.is_empty() {
                break;
            }
        }
        for lane in 0..5 {
            if pending.contains(HeightLaneMask::for_lane(lane)) {
                self.cache_height(lx, lz, lane, self.min_y);
            }
        }
    }

    #[inline]
    fn height_for_lane(&self, lx: i32, lz: i32, lane: usize) -> i32 {
        if self.cached_height(lx, lz, lane).is_none() {
            let (group, primary) = match lane {
                0 => (HeightLaneMask::LIVE_SURFACE, HeightLaneMask::LIVE_SURFACE),
                1 => (
                    HeightLaneMask::WORLD_SURFACE_WG,
                    HeightLaneMask::WORLD_SURFACE_WG,
                ),
                2 => (
                    HeightLaneMask::LIVE_SURFACE.union(HeightLaneMask::MOTION_BLOCKING),
                    HeightLaneMask::MOTION_BLOCKING,
                ),
                3 => (HeightLaneMask::LIVE, HeightLaneMask::OCEAN_FLOOR),
                4 => (
                    HeightLaneMask::OCEAN_FLOOR_WG,
                    HeightLaneMask::OCEAN_FLOOR_WG,
                ),
                _ => unreachable!("height cache lane out of range"),
            };
            self.scan_height_lanes(lx, lz, group, primary);
        }
        self.height_cache[self.height_cache_index(lx, lz)].get()[lane]
    }

    /// Reads always succeed (clamped into bounds) — a read past the local
    /// footprint approximates the nearest in-bounds column rather than
    /// panicking or returning a sentinel the caller has to special-case.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> StateId {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local_id(lx, y, lz)
    }

    /// Interned read state. Kept as an explicit id-named alias for call sites
    /// that document their numeric hot path.
    #[must_use]
    pub fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local_id(lx, y, lz)
    }

    /// Writes past the local footprint (`0..16` for [`VegGrid::new`], wider
    /// for [`VegGrid::with_footprint`]) or outside the vertical build range
    /// are dropped, not clamped — see module doc's "Scope" section; a write
    /// past whatever footprint this grid covers would fabricate a block on
    /// the wrong column. Returns whether the write actually landed.
    /// Writes by local interned id. Identical bounds behaviour, including the
    /// census bumps, is shared by every producer.
    pub fn set_id_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: StateId) -> bool {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            census_bump(|c| c.writes += 1);
            let key = self.overlay_key(lx, y, lz);
            let previous = self.blocks.get_in_bounds(&key);
            self.blocks.insert_in_bounds(key, state);
            self.invalidate_height_caches(lx, lz);
            if self.ore_entry_active {
                if previous != Some(state) {
                    self.ore_writes.push((lx, y, lz));
                }
            } else {
                self.dirty.push((lx, y, lz));
            }
            if let Some(capture) = &mut self.structure_mutation_capture {
                capture.push((x, y, z, state));
            }
            true
        } else {
            census_bump(|c| c.writes_rejected += 1);
            false
        }
    }

    pub fn set_canonical_if_in_bounds(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) -> bool {
        self.set_id_if_in_bounds(x, y, z, state)
    }

    /// `Heightmap.Types.WORLD_SURFACE` — topmost non-air, scanned live against
    /// the current (possibly already-modified-this-step) grid. `x`/`z` are
    /// absolute world coordinates. Returns `min_y` (not `min_y - 1`) for an
    /// all-air column, matching the reference `y + 1` convention with `y`
    /// floored at one below the lowest placeable block.
    #[must_use]
    pub fn height_world_surface(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.height_for_lane(lx, lz, 0)
    }

    /// `Heightmap.Types.WORLD_SURFACE_WG` — topmost non-air in the immutable
    /// terrain snapshot, before decoration writes. The reference world keeps
    /// this world-generation heightmap unchanged while FEATURES writes update
    /// the final heightmaps, so an earlier grass/tree write must not move a
    /// later `WORLD_SURFACE_WG` probe. `x`/`z` are absolute world coordinates.
    #[must_use]
    pub fn height_world_surface_wg(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.height_for_lane(lx, lz, 1)
    }

    /// `Heightmap.Types.MOTION_BLOCKING` — the first free row above the
    /// highest motion-blocking block or fluid. Decorative plants and vines do
    /// not raise this heightmap even though they are non-air, while fluids do.
    #[must_use]
    pub fn height_motion_blocking(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.height_for_lane(lx, lz, 2)
    }

    /// Scans the live overlay using the resolver's predicate, without priming other heightmaps.
    #[inline]
    pub(crate) fn top_layer_motion_blocking_first_free_if_extent<F>(
        &self,
        x: i32,
        z: i32,
        min_y: i32,
        height: i32,
        motion_blocking: F,
    ) -> Option<i32>
    where
        F: Fn(StateId) -> bool,
    {
        if min_y != self.min_y || height != self.height {
            return None;
        }
        let (lx, lz) = self.to_local_exact(x, z);
        if !self.in_bounds_local(lx, lz) {
            return None;
        }
        let source = self.source_grid(lx, lz);
        for y in (min_y..min_y + height).rev() {
            let state = self
                .blocks
                .get_in_bounds(&self.overlay_key(lx, y, lz))
                .unwrap_or_else(|| self.source_id_from_grid(source, lx, y, lz));
            if motion_blocking(state) {
                return Some(y + 1);
            }
        }
        Some(min_y)
    }

    /// Whether `id` is one of the three air states.
    ///
    /// # Why this can be an id comparison and the fluid test below cannot
    ///
    /// **Air carries no block-state properties**, so for an air state
    /// `base_id(name) == name` and "base is one of three names" is exactly "id is
    /// one of three ids" — the three resolved in [`Self::with_footprint_canonical`].
    /// `crate::feature::vegetation::config::is_air` is still the definition; this
    /// is that definition pushed through the canonical table once per grid instead
    /// of once per cell. Adding a property-carrying state to `is_air` would silently
    /// break this, which is why that function's doc says not to.
    ///
    /// A fluid, by contrast, really does carry properties here —
    /// `crate::carver` writes `minecraft:water[level=0]` — so the heightmap
    /// scans use the typed facts cached with each source palette entry rather
    /// than reducing a state to its base-name string.
    pub(super) fn is_air_id(&self, id: StateId) -> bool {
        self.air_ids.contains(&id)
    }

    /// Ocean-floor height: topmost **motion-blocking** block, plus one,
    /// including decoration writes. `x`/`z` are absolute world coordinates.
    ///
    /// The scan consumes the generated per-state blocks-motion table through
    /// [`BaseStateFacts`], so fluids are excluded while blocks such as leaves
    /// retain the reference predicate's own blocks-motion result.
    #[must_use]
    pub fn height_ocean_floor(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.height_for_lane(lx, lz, 3)
    }

    /// Ocean-floor world-generation height: topmost **motion-blocking** block,
    /// plus one, in the immutable pre-decoration terrain snapshot.
    #[must_use]
    pub fn height_ocean_floor_wg(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.height_for_lane(lx, lz, 4)
    }

}

impl super::super::OreWorldAccess for VegGrid {
    #[inline]
    fn ore_get_id(&self, lx: i32, y: i32, lz: i32) -> StateId {
        let (lx, lz) = self.to_local_clamped(self.origin_x + lx, self.origin_z + lz);
        let overlay = self
            .blocks
            .get_in_bounds(&self.overlay_key(lx, y, lz))
            .is_some();
        let id = self.get_local_id(lx, y, lz);
        if overlay {
            super::super::ore_probe::bump_region_read_overlay(1);
        }
        id
    }

    #[inline]
    fn ore_set_id(&mut self, lx: i32, y: i32, lz: i32, state: StateId) -> bool {
        if !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lx)
            || !(crate::feature::REGION_MIN..crate::feature::REGION_MAX).contains(&lz)
        {
            return false;
        }
        self.set_id_if_in_bounds(self.origin_x + lx, y, self.origin_z + lz, state)
    }

    #[inline]
    fn ore_entry_begin(&mut self) {
        debug_assert!(!self.ore_entry_active);
        self.ore_entry_active = true;
        self.ore_writes.clear();
    }

    fn ore_entry_end(&mut self) {
        debug_assert!(self.ore_entry_active);
        self.ore_writes
            .sort_unstable_by_key(|&(lx, y, lz)| (lx, lz, y));
        for index in 0..self.ore_writes.len() {
            self.dirty.push(self.ore_writes[index]);
        }
        self.ore_writes.clear();
        self.ore_entry_active = false;
    }
}

/// Runs the whole `VEGETAL_DECORATION` step for one chunk against its own
/// grid — single-source only, see module doc's "Scope" section.
/// `features` is `(raw step index, resolved PlacedRef)`, matching
/// the decoration catalog's global raw-position convention so `setFeatureSeed`'s
/// index is the source's position in the global step order, not a filtered count.
/// Per-thread census of what the vegetal-decoration placer actually *did* —
/// the "make absence loud" half of that convention.
///
/// # Why this exists, and why the existing gate was not enough
///
/// This module's blanket rule is "an unmodelled feature/trunk/foliage/provider
/// kind degrades to a silent no-op, never a panic" (see the module doc). That
/// rule is right — a datapack naming a feature we don't implement must still
/// produce a world — but on its own it makes *every* quantity of vegetation,
/// including zero, look identical from the outside. This was found
/// against exactly that shape, and the previous instance of the same shape
/// (the absolute-vs-local `VegGrid` coordinate bug recorded in
/// [`VegGrid`]'s own doc comment) reached **zero** blocks in every served
/// chunk with the whole suite green.
///
/// [`collect_unsupported`] plus `lodestone_server::worldgen_data`'s
/// `KNOWN_VEGETATION_GAPS` already make absence loud at **resolve** time: they
/// answer "does this biome's declared step name a placer we don't implement?"
/// They structurally cannot answer "did the placer that *is* implemented reach
/// a block?", because they never run it. This census answers the second
/// question — the one that separates a fully-connected wire carrying real
/// blocks from a fully-connected wire carrying nothing.
///
/// # Thread-local, not global
///
/// `OverworldGenerator` is shared across threads by
/// `lodestone_server::chunk::generate_columns_parallel`, and `cargo test` runs
/// test binaries multi-threaded. A process-global counter would make any gate
/// built on it read another test's work, which is the *duration* species of
/// vacuous test (a counter accumulating past the gate's own lifetime). Each
/// thread sees only its own placements, so a gate resets, generates, and reads
/// back on one thread and measures exactly what it caused.
pub mod census {
    #[cfg(feature = "gen-counters")]
    use crate::dense_grid::DenseBlockGrid;
    #[cfg(feature = "gen-counters")]
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const MAX_SOURCE_OWNERS: usize = 256;
    const XZ_LANE_WORDS: usize = 4;
    const SOURCE_CELL_WORDS: usize = 12;

    /// Reads attributed to one absolute source chunk. The masks cover its
    /// 16×16 horizontal lanes and 4×48×4 cells of size 4×8×4, indexed from
    /// that source grid's minimum Y.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SourceOwnerReads {
        occupied: bool,
        pub chunk_x: i32,
        pub chunk_z: i32,
        pub source_min_y: i32,
        pub reads: u64,
        pub height_reads: u64,
        pub lane_mask: [u64; XZ_LANE_WORDS],
        pub height_lane_mask: [u64; XZ_LANE_WORDS],
        pub cell_mask: [u64; SOURCE_CELL_WORDS],
        pub cell_reads_outside_window: u64,
    }

    impl SourceOwnerReads {
        #[must_use]
        pub fn unique_xz_lanes(&self) -> u32 {
            self.lane_mask.iter().map(|word| word.count_ones()).sum()
        }

        #[must_use]
        pub fn unique_height_xz_lanes(&self) -> u32 {
            self.height_lane_mask
                .iter()
                .map(|word| word.count_ones())
                .sum()
        }

        #[must_use]
        pub fn unique_4x8x4_cells(&self) -> u32 {
            self.cell_mask.iter().map(|word| word.count_ones()).sum()
        }
    }

    /// Bounded source-read census. Empty entries are ignored; `owner_overflow`
    /// counts reads omitted after the fixed owner table fills.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SourceReadSnapshot {
        owners: [SourceOwnerReads; MAX_SOURCE_OWNERS],
        pub owner_count: usize,
        pub owner_overflow: u64,
    }

    impl Default for SourceReadSnapshot {
        fn default() -> Self {
            Self {
                owners: [SourceOwnerReads::default(); MAX_SOURCE_OWNERS],
                owner_count: 0,
                owner_overflow: 0,
            }
        }
    }

    impl SourceReadSnapshot {
        #[must_use]
        pub fn owners(&self) -> impl Iterator<Item = &SourceOwnerReads> {
            self.owners.iter().filter(|owner| owner.occupied)
        }

        #[must_use]
        pub fn is_complete(&self) -> bool {
            self.owner_overflow == 0
                && self
                    .owners()
                    .all(|owner| owner.cell_reads_outside_window == 0)
        }
    }

    /// Terminal-dispatch and write tallies for one thread's placements.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct VegCensus {
        /// [`super::ConfiguredFeature::SimpleBlock`] terminal dispatches — one
        /// per position that survived the whole placement pipeline.
        pub simple_block: usize,
        /// [`super::ConfiguredFeature::Tree`] terminal dispatches.
        pub tree: usize,
        /// [`super::ConfiguredFeature::BlockColumn`] terminal dispatches.
        pub block_column: usize,
        /// [`super::ConfiguredFeature::RandomSelector`] traversals (not
        /// terminals — each recurses into a branch).
        pub random_selector: usize,
        /// [`super::ConfiguredFeature::SimpleRandomSelector`] traversals.
        pub simple_random_selector: usize,
        /// Dispatches into any of the 24 feature types added beyond
        /// the original seven, terminals and traversals together. One shared
        /// counter rather than 24 fields — the question this census answers is
        /// "did decoration reach a modelled body", and `unsupported` already
        /// names the ones it did not.
        pub other_feature: usize,
        /// Unmodelled terminal dispatches, **keyed by the reason string**
        /// [`super::ConfiguredFeature::Unsupported`] carries. This is the loud
        /// part: a new unimplemented feature type shows up here as a named,
        /// counted row instead of as a slightly emptier world.
        pub unsupported: BTreeMap<String, usize>,
        /// `SimpleBlock` dispatches dropped because the state provider
        /// produced nothing.
        pub simple_block_no_state: usize,
        /// `SimpleBlock` dispatches dropped because the block below is not in
        /// `#minecraft:supports_vegetation` (`VegetationBlock.canSurvive`).
        /// Legitimately the majority — `random_offset` scatters positions off
        /// the heightmap column — so this is a diagnostic, not a defect count.
        pub simple_block_unsupported_ground: usize,
        /// Positions handed to a [`super::VegPlacement::BlockPredicateFilter`].
        ///
        /// This is the **last exactly-predictable boundary** in a vanilla
        /// vegetal-decoration pipeline, and the reason it is counted separately
        /// from everything else here. Every 26.2 overworld vegetation
        /// `placed_feature` ends in at least one filter whose outcome depends on
        /// terrain (measured: of 262 bundled placed features, the only three
        /// with no filter at all are `end_spike`, `freeze_top_layer` and
        /// `void_start_platform`), so no *terminal* count can be predicted from
        /// the JSON alone. Everything upstream of the filter can:
        /// `count`/`noise_threshold_count` multiply by a JSON constant,
        /// `in_square`/`biome`/`random_offset` are each exactly
        /// position-preserving, and `heightmap` yields exactly one position for
        /// any column that is not entirely air. So for a single-source run of a
        /// single placed feature this number is a product of JSON constants —
        /// which is what lets a gate *predict* it instead of asserting a sign.
        /// See `lodestone_server::worldgen_data`'s
        /// `plains_grass_patch_attempt_count_matches_the_placement_json`.
        pub block_predicate_filter_in: usize,
        /// Positions that passed a [`super::VegPlacement::BlockPredicateFilter`].
        pub block_predicate_filter_out: usize,
        /// Grid writes that landed.
        pub writes: usize,
        /// Grid writes dropped as outside the grid's own footprint (spill into
        /// a chunk this grid does not cover — expected, see
        /// [`super::VegGrid::set_id_if_in_bounds`]).
        pub writes_rejected: usize,
        /// Downward height-cache walks started.
        pub height_scans: usize,
        /// Vertical cells visited by height-cache walks.
        pub height_scan_cells: u64,
        /// Cells visited while the requested lane remained unresolved.
        pub height_primary_cells: u64,
        /// Cells visited after the requested lane resolved for companions.
        pub height_companion_tail_cells: u64,
    }

    thread_local! {
        static CENSUS: RefCell<VegCensus> = RefCell::new(VegCensus::default());
        #[cfg(feature = "gen-counters")]
        static SOURCE_SLOT_MASK: Cell<u32> = const { Cell::new(0) };
        #[cfg(feature = "gen-counters")]
        static SOURCE_READS: RefCell<SourceReadSnapshot> = RefCell::new(SourceReadSnapshot::default());
    }

    /// Whether an unmodelled terminal dispatch should panic instead of being
    /// counted — `LODESTONE_VEG_STRICT=1`. Read once per process.
    ///
    /// Off by default on purpose: the module's degrade-don't-crash rule is what
    /// lets a trimmed datapack generate at all, and 26.2's own vanilla data
    /// reaches unmodelled types in nearly every biome (`multiface_growth` alone
    /// is in 55 of them), so strict mode is a *debugging* switch for "which
    /// type am I missing here", not a mode anything ships in.
    #[must_use]
    pub fn strict() -> bool {
        static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *STRICT.get_or_init(|| {
            std::env::var("LODESTONE_VEG_STRICT").is_ok_and(|v| v != "0" && !v.is_empty())
        })
    }

    /// Zeroes this thread's census. Call immediately before the generation a
    /// gate intends to measure.
    pub fn reset() {
        CENSUS.with(|c| *c.borrow_mut() = VegCensus::default());
        reset_source_slots();
        reset_source_reads();
    }

    /// This thread's census so far.
    #[must_use]
    pub fn snapshot() -> VegCensus {
        CENSUS.with(|c| c.borrow().clone())
    }

    #[cfg(feature = "gen-counters")]
    pub(super) fn record_source_read(
        source: &DenseBlockGrid,
        x: i32,
        y: i32,
        z: i32,
        read_count: u64,
    ) {
        let chunk_x = x.div_euclid(16);
        let chunk_z = z.div_euclid(16);
        let lane = z.rem_euclid(16) as usize * 16 + x.rem_euclid(16) as usize;
        let source_min_y = source.bounds().1;
        let y_cell = (y - source_min_y).div_euclid(8);
        SOURCE_READS.with(|reads| {
            let mut snapshot = reads.borrow_mut();
            let Some(index) = source_owner_slot(&snapshot, chunk_x, chunk_z) else {
                snapshot.owner_overflow += 1;
                return;
            };
            if !snapshot.owners[index].occupied {
                if snapshot.owner_count == MAX_SOURCE_OWNERS {
                    snapshot.owner_overflow += 1;
                    return;
                }
                snapshot.owner_count += 1;
                let owner = &mut snapshot.owners[index];
                owner.occupied = true;
                owner.chunk_x = chunk_x;
                owner.chunk_z = chunk_z;
                owner.source_min_y = source_min_y;
            }
            let owner = &mut snapshot.owners[index];
            owner.source_min_y = source_min_y;
            owner.reads += read_count;
            owner.lane_mask[lane / 64] |= 1u64 << (lane % 64);
            if (0..48).contains(&y_cell) {
                let cell = y_cell as usize * 16
                    + z.rem_euclid(16) as usize / 4 * 4
                    + x.rem_euclid(16) as usize / 4;
                owner.cell_mask[cell / 64] |= 1u64 << (cell % 64);
            } else {
                owner.cell_reads_outside_window += 1;
            }
        });
    }

    #[cfg(feature = "gen-counters")]
    pub(crate) fn record_height_read(chunk_x: i32, chunk_z: i32, local_x: usize, local_z: usize) {
        let lane = local_z * 16 + local_x;
        SOURCE_READS.with(|reads| {
            let mut snapshot = reads.borrow_mut();
            let Some(index) = source_owner_slot(&snapshot, chunk_x, chunk_z) else {
                snapshot.owner_overflow += 1;
                return;
            };
            if !snapshot.owners[index].occupied {
                snapshot.owner_count += 1;
                let owner = &mut snapshot.owners[index];
                owner.occupied = true;
                owner.chunk_x = chunk_x;
                owner.chunk_z = chunk_z;
            }
            let owner = &mut snapshot.owners[index];
            owner.height_reads += 1;
            owner.height_lane_mask[lane / 64] |= 1u64 << (lane % 64);
        });
    }

    #[cfg(feature = "gen-counters")]
    fn source_owner_slot(
        snapshot: &SourceReadSnapshot,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Option<usize> {
        let key = (u64::from(chunk_x as u32) << 32) | u64::from(chunk_z as u32);
        let mut hash = key;
        hash ^= hash >> 30;
        hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 27;
        hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
        hash ^= hash >> 31;
        let start = hash as usize % MAX_SOURCE_OWNERS;
        (0..MAX_SOURCE_OWNERS)
            .map(|offset| (start + offset) % MAX_SOURCE_OWNERS)
            .find(|&index| {
                let owner = &snapshot.owners[index];
                !owner.occupied || (owner.chunk_x == chunk_x && owner.chunk_z == chunk_z)
            })
    }

    #[cfg(feature = "gen-counters")]
    pub fn reset_source_reads() {
        SOURCE_READS.with(|reads| *reads.borrow_mut() = SourceReadSnapshot::default());
    }

    #[cfg(not(feature = "gen-counters"))]
    pub fn reset_source_reads() {}

    #[cfg(feature = "gen-counters")]
    #[must_use]
    pub fn source_read_snapshot() -> SourceReadSnapshot {
        SOURCE_READS.with(|reads| *reads.borrow())
    }

    #[cfg(not(feature = "gen-counters"))]
    #[must_use]
    pub fn source_read_snapshot() -> SourceReadSnapshot {
        SourceReadSnapshot::default()
    }

    pub(in crate::feature::vegetation) fn bump(f: impl FnOnce(&mut VegCensus)) {
        CENSUS.with(|c| f(&mut c.borrow_mut()));
    }

    #[cfg(feature = "gen-counters")]
    pub(in crate::feature::vegetation) fn record_source_slot(slot: usize) {
        SOURCE_SLOT_MASK.with(|mask| mask.set(mask.get() | (1u32 << slot)));
    }

    #[cfg(not(feature = "gen-counters"))]
    pub(in crate::feature::vegetation) fn record_source_slot(_slot: usize) {}

    #[cfg(feature = "gen-counters")]
    pub fn reset_source_slots() {
        SOURCE_SLOT_MASK.with(|mask| mask.set(0));
    }

    #[cfg(not(feature = "gen-counters"))]
    pub fn reset_source_slots() {}

    #[cfg(feature = "gen-counters")]
    #[must_use]
    pub fn source_slot_mask() -> u32 {
        SOURCE_SLOT_MASK.with(Cell::get)
    }

    #[cfg(not(feature = "gen-counters"))]
    #[must_use]
    pub fn source_slot_mask() -> u32 {
        0
    }
}

#[cfg(test)]
mod heightmap_tests {
    use std::sync::Arc;

    use lodestone_data::block_states::StateId;

    use super::census;
    use super::VegGrid;
    use crate::dense_grid::DenseBlockGrid;
    #[cfg(feature = "gen-counters")]
    use crate::feature::region_view::wide_slot_of_offset;

    fn state(spec: &str) -> StateId {
        StateId::from_state_str(spec).expect("test state is in the generated table")
    }

    /// The P07 witness is on the east edge of source `(-26,-25)`: the source
    /// writes its first grass at `(-401,122,-387)` before the later candidate
    /// reaches `(-400,121,-385)` in target `(-25,-25)`. Keep the two columns
    /// distinct here so a live-overlay regression cannot masquerade as a
    /// source-terrain read.
    fn p07_source_grid() -> (VegGrid, StateId, StateId, StateId) {
        let air = state("minecraft:air");
        let grass = state("minecraft:grass_block");
        let short_grass = state("minecraft:short_grass");

        let mut west = DenseBlockGrid::with_default(-416, 0, -400, 16, 128, 16, air);
        west.set_id(-401, 121, -387, grass);

        let mut centre = DenseBlockGrid::with_default(-400, 0, -400, 16, 128, 16, air);
        centre.set_id(-400, 120, -385, grass);

        let west = Arc::new(west);
        let centre = Arc::new(centre);
        let grid = VegGrid::with_sources(
            0,
            128,
            -400,
            -400,
            -16,
            32,
            move |dx, dz| {
                if (dx, dz) == (-1, 0) {
                    Some(Arc::clone(&west))
                } else {
                    Some(Arc::clone(&centre))
                }
            },
        );
        (grid, air, grass, short_grass)
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn source_slot_census_detects_a_radius_two_read() {
        let (grid, _, _, _) = p07_source_grid();
        census::reset();
        let _ = grid.source_id(32, 120, 0);
        let far_slot = wide_slot_of_offset(2, 0);
        assert_ne!(census::source_slot_mask() & (1u32 << far_slot), 0);
    }

    #[cfg(feature = "gen-counters")]
    #[test]
    fn source_read_census_excludes_overlay_reads() {
        let (mut grid, _, grass, short_grass) = p07_source_grid();
        census::reset_source_reads();
        assert_eq!(grid.get_id(-401, 121, -387), grass);
        assert_eq!(grid.get_id(-401, 121, -387), grass);
        let snapshot = census::source_read_snapshot();
        assert!(snapshot.is_complete());
        let owner = snapshot.owners().next().expect("west source was read");
        assert_eq!((owner.chunk_x, owner.chunk_z), (-26, -25));
        assert_eq!(snapshot.owner_count, 1);
        assert_eq!(owner.reads, 2);
        assert_eq!(owner.unique_xz_lanes(), 1);
        assert_eq!(owner.unique_4x8x4_cells(), 1);

        assert!(grid.set_id_if_in_bounds(-401, 121, -387, short_grass));
        assert_eq!(grid.get_id(-401, 121, -387), short_grass);
        assert_eq!(census::source_read_snapshot(), snapshot);
    }

    #[test]
    fn p07_west_source_world_surface_wg_ignores_its_first_grass_write() {
        let (mut grid, air, grass, short_grass) = p07_source_grid();
        let source_probe: (i32, i32) = (-401, -387);
        let witness: (i32, i32, i32) = (-400, 121, -385);

        assert_eq!(source_probe.0.div_euclid(16), -26);
        assert_eq!(source_probe.1.div_euclid(16), -25);
        assert_eq!((witness.0.div_euclid(16), witness.2.div_euclid(16)), (-25, -25));
        assert_eq!(grid.get_id(witness.0, witness.1, witness.2), air);
        assert_eq!(grid.get_id(witness.0, 120, witness.2), grass);
        assert_eq!(grid.height_world_surface_wg(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface_wg(source_probe.0, source_probe.1), 122);

        assert!(grid.set_id_if_in_bounds(-401, 122, -387, state("minecraft:short_grass")));

        assert_eq!(
            grid.height_world_surface_wg(source_probe.0, source_probe.1),
            122,
            "P07 source (-26,-25)'s immutable WG height must ignore its first grass write"
        );
        assert_eq!(
            grid.height_world_surface(source_probe.0, source_probe.1),
            123,
            "the live final surface must see that same write"
        );
        assert_eq!(grid.get_id(witness.0, witness.1, witness.2), air);
        assert_eq!(grid.height_world_surface_wg(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface(witness.0, witness.2), 121);
        assert_eq!(short_grass, state("minecraft:short_grass"));
    }

    #[test]
    fn source_less_seeded_baseline_drives_frozen_heightmaps_without_live_overlay() {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let dirt = state("minecraft:dirt");

        // Compact external fixtures have no source grids; seed their immutable
        // terrain before decoration. The expected height is the occupied row
        // plus one, independent of the implementation's cache or scan.
        grid.seed_id(0, 4, 0, dirt);

        // Wrong-rule control: the live heightmap sees a later decoration write,
        // while WORLD_SURFACE_WG must retain the pre-decoration baseline even
        // when its first probe occurs after that write.
        assert!(grid.set_id_if_in_bounds(0, 8, 0, dirt));
        assert_eq!(grid.height_world_surface(0, 0), 9);
        assert_eq!(grid.height_world_surface_wg(0, 0), 5);
        assert_eq!(grid.height_ocean_floor_wg(0, 0), 5);

        // Seeding a changed baseline after a cached probe must invalidate the
        // fixture's WG lanes without making ordinary overlay writes do so.
        grid.seed_id(0, 10, 0, dirt);
        assert_eq!(grid.height_world_surface_wg(0, 0), 11);
        assert_eq!(grid.height_ocean_floor_wg(0, 0), 11);
    }

    #[test]
    fn source_backed_heightmaps_keep_waterlogged_state_facts() {
        let air = state("minecraft:air");
        let grass = state("minecraft:grass_block");
        let waterlogged_coral = state("minecraft:brain_coral_fan[waterlogged=true]");
        let waterlogged_stairs = state(
            "minecraft:stone_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
        );
        assert!(lodestone_data::snow_support::has_fluid_state(waterlogged_coral));
        assert!(!lodestone_data::block_solidity::blocks_motion(waterlogged_coral));
        assert!(lodestone_data::snow_support::has_fluid_state(waterlogged_stairs));
        assert!(lodestone_data::block_solidity::blocks_motion(waterlogged_stairs));

        let mut source = DenseBlockGrid::with_default(0, 0, 0, 16, 16, 16, air);
        source.set_id(0, 1, 0, grass);
        source.set_id(0, 5, 0, waterlogged_coral);
        source.set_id(1, 1, 0, grass);
        source.set_id(1, 7, 0, waterlogged_stairs);
        let source = Arc::new(source);
        let grid = VegGrid::with_sources(0, 16, 0, 0, 0, 16, move |dx, dz| {
            ((dx, dz) == (0, 0)).then(|| Arc::clone(&source))
        });

        assert_eq!(grid.height_motion_blocking(0, 0), 6);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);
        assert_eq!(grid.height_motion_blocking(1, 0), 8);
        assert_eq!(grid.height_ocean_floor(1, 0), 8);
    }

    #[test]
    fn world_surface_heightmap_control_sees_overlay_writes() {
        let (mut grid, _air, _grass, short_grass) = p07_source_grid();
        let source_probe: (i32, i32) = (-401, -387);
        assert_eq!(grid.height_world_surface(source_probe.0, source_probe.1), 122);
        grid.set_id_if_in_bounds(
            source_probe.0,
            122,
            source_probe.1,
            state("minecraft:short_grass"),
        );
        assert_eq!(grid.height_world_surface(source_probe.0, source_probe.1), 123);
        assert_eq!(grid.get_id(-401, 122, -387), short_grass);
    }

    #[test]
    fn all_heightmap_caches_invalidate_after_overlay_writes() {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let air = state("minecraft:air");
        let grass = state("minecraft:grass_block");
        let water = state("minecraft:water[level=0]");
        let waterlogged_coral = state("minecraft:brain_coral_fan[waterlogged=true]");
        let waterlogged_stairs = state(
            "minecraft:stone_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
        );
        let short_grass = state("minecraft:short_grass");

        // Prime all four caches with an all-air answer before any writes.
        assert_eq!(grid.height_world_surface(0, 0), 0);
        assert_eq!(grid.height_world_surface_wg(0, 0), 0);
        assert_eq!(grid.height_motion_blocking(0, 0), 0);
        assert_eq!(grid.height_ocean_floor(0, 0), 0);

        assert!(grid.set_id_if_in_bounds(0, 1, 0, grass));
        assert_eq!(grid.height_world_surface(0, 0), 2);
        assert_eq!(grid.height_world_surface_wg(0, 0), 0);
        assert_eq!(grid.height_motion_blocking(0, 0), 2);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);

        // Fluids count for motion blocking but not for the ocean-floor
        // heightmap, and the cached results must observe that new state.
        assert!(grid.set_id_if_in_bounds(0, 3, 0, water));
        assert_eq!(grid.height_world_surface(0, 0), 4);
        assert_eq!(grid.height_motion_blocking(0, 0), 4);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);

        assert!(lodestone_data::snow_support::has_fluid_state(waterlogged_coral));
        assert!(!lodestone_data::block_solidity::blocks_motion(waterlogged_coral));
        assert!(grid.set_id_if_in_bounds(0, 5, 0, waterlogged_coral));
        assert_eq!(grid.height_motion_blocking(0, 0), 6);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);

        // The solid predicate, not the presence of fluid, defines Ocean Floor.
        assert!(lodestone_data::snow_support::has_fluid_state(waterlogged_stairs));
        assert!(lodestone_data::block_solidity::blocks_motion(waterlogged_stairs));
        assert!(grid.set_id_if_in_bounds(0, 7, 0, waterlogged_stairs));
        assert_eq!(grid.height_motion_blocking(0, 0), 8);
        assert_eq!(grid.height_ocean_floor(0, 0), 8);
        assert!(grid.set_id_if_in_bounds(0, 7, 0, air));
        assert_eq!(grid.height_motion_blocking(0, 0), 6);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);

        // A plant is part of WORLD_SURFACE but does not raise the two
        // motion-based heightmaps.
        assert!(grid.set_id_if_in_bounds(0, 5, 0, short_grass));
        assert_eq!(grid.height_world_surface(0, 0), 6);
        assert_eq!(grid.height_motion_blocking(0, 0), 4);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);

        // Replacing the highest writes with air must invalidate the same
        // column too; otherwise a stale top would survive an edit.
        assert!(grid.set_id_if_in_bounds(0, 5, 0, air));
        assert_eq!(grid.height_world_surface(0, 0), 4);
        assert!(grid.set_id_if_in_bounds(0, 3, 0, air));
        assert_eq!(grid.height_motion_blocking(0, 0), 2);
        assert_eq!(grid.height_ocean_floor(0, 0), 2);
    }

    #[test]
    fn ocean_floor_counts_leaves_and_mutation_to_stone_keeps_it_raised() {
        let mut grid = VegGrid::with_footprint(0, 80, 0, 0, 0, 1);
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water[level=0]");
        let leaves = state("minecraft:dark_oak_leaves[distance=3,persistent=false,waterlogged=false]");
        let short_grass = state("minecraft:short_grass");
        let stone = state("minecraft:stone");
        assert!(grid.set_id_if_in_bounds(0, 60, 0, dirt));
        assert!(grid.set_id_if_in_bounds(0, 61, 0, water));
        assert!(grid.set_id_if_in_bounds(0, 62, 0, water));
        assert!(grid.set_id_if_in_bounds(0, 67, 0, leaves));
        assert!(grid.set_id_if_in_bounds(0, 68, 0, leaves));

        assert_eq!(grid.height_ocean_floor(0, 0), 69);
        assert_eq!(grid.height_motion_blocking(0, 0), 69);

        assert!(grid.set_id_if_in_bounds(0, 67, 0, short_grass));
        assert!(grid.set_id_if_in_bounds(0, 68, 0, short_grass));
        assert_eq!(grid.height_ocean_floor(0, 0), 61);
        assert_eq!(grid.height_motion_blocking(0, 0), 63);

        assert!(grid.set_id_if_in_bounds(0, 68, 0, stone));
        assert_eq!(grid.height_ocean_floor(0, 0), 69);
        assert_eq!(grid.height_motion_blocking(0, 0), 69);
    }

    fn feature_rich_grid() -> VegGrid {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let dirt = state("minecraft:dirt");
        let water = state("minecraft:water[level=0]");
        let leaves = state("minecraft:dark_oak_leaves[distance=3,persistent=false,waterlogged=false]");
        let short_grass = state("minecraft:short_grass");
        grid.seed_id(0, 2, 0, dirt);
        grid.seed_id(0, 4, 0, leaves);
        assert!(grid.set_id_if_in_bounds(0, 6, 0, water));
        assert!(grid.set_id_if_in_bounds(0, 8, 0, short_grass));
        grid
    }

    fn surface_only_grid() -> VegGrid {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let short_grass = state("minecraft:short_grass");
        assert!(grid.set_id_if_in_bounds(0, 8, 0, short_grass));
        grid
    }

    #[test]
    fn live_height_walk_fills_compatible_lanes_once() {
        let grid = feature_rich_grid();
        census::reset();

        assert_eq!(grid.height_ocean_floor(0, 0), 5);
        assert_eq!(grid.height_world_surface(0, 0), 9);
        assert_eq!(grid.height_motion_blocking(0, 0), 7);

        let snapshot = census::snapshot();
        assert_eq!(snapshot.height_scans, 1);
        assert_eq!(snapshot.height_scan_cells, 12);
        assert_eq!(snapshot.height_primary_cells, 12);
        assert_eq!(snapshot.height_companion_tail_cells, 0);
        assert!(
            snapshot.height_scan_cells < 8 + 10 + 12,
            "fused live walk did not beat three independent scans: {snapshot:?}"
        );
    }

    #[test]
    fn surface_only_walk_has_no_companion_tail() {
        let grid = surface_only_grid();
        census::reset();

        assert_eq!(grid.height_world_surface(0, 0), 9);

        let snapshot = census::snapshot();
        assert_eq!(snapshot.height_scans, 1);
        assert_eq!(snapshot.height_scan_cells, 8);
        assert_eq!(snapshot.height_primary_cells, 8);
        assert_eq!(snapshot.height_companion_tail_cells, 0);
    }

    #[test]
    fn immutable_wg_walk_stays_separate_when_live_height_differs() {
        let grid = feature_rich_grid();
        census::reset();

        assert_eq!(grid.height_world_surface(0, 0), 9);
        assert_eq!(grid.height_world_surface_wg(0, 0), 5);

        let snapshot = census::snapshot();
        assert_eq!(snapshot.height_scans, 2);
        assert_eq!(snapshot.height_scan_cells, 8 + 12);
        assert_eq!(snapshot.height_primary_cells, 8 + 12);
        assert_eq!(snapshot.height_companion_tail_cells, 0);
    }

    #[test]
    fn removing_the_live_top_block_forces_a_fresh_live_walk() {
        let mut grid = feature_rich_grid();
        let air = state("minecraft:air");
        assert_eq!(grid.height_ocean_floor(0, 0), 5);
        assert_eq!(grid.height_world_surface_wg(0, 0), 5);

        census::reset();
        assert!(grid.set_id_if_in_bounds(0, 8, 0, air));
        assert_eq!(grid.height_ocean_floor(0, 0), 5);
        assert_eq!(grid.height_world_surface(0, 0), 7);
        assert_eq!(grid.height_motion_blocking(0, 0), 7);

        let snapshot = census::snapshot();
        assert_eq!(snapshot.height_scans, 1);
        assert_eq!(snapshot.height_scan_cells, 12);
        assert_eq!(snapshot.height_primary_cells, 12);
        assert_eq!(snapshot.height_companion_tail_cells, 0);
        assert_eq!(grid.height_world_surface_wg(0, 0), 5);
    }
}
