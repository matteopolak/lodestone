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
use crate::interner::{BaseStateFacts, StateId, StateInterner};
use crate::overworld::BiomeCells;
use lodestone_data::biomes::{BiomeRef, BuiltinBiome};

use self::census::bump as census_bump;

const HEIGHT_CACHE_UNSET: i32 = i32::MIN;
/// Compact representation for a vertically invariant biome source. Name-based
/// compatibility inputs are converted at construction; retained grids never
/// carry a parallel string array.
#[derive(Debug)]
enum FlatBiomeSources {
    Ids {
        cells: [Option<Arc<[BiomeRef; 16]>>; WIDE_SLOTS],
    },
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
    /// Unit 3 (`docs/plans/worldgen-rewrite.md`) changed the value type from
    /// `String` to [`StateId`]. That single change is where **884,736 of the
    /// 905,459 heap allocations per warm column** went: the seeding loop
    /// (`OverworldGenerator::stitch_veg_region`) copies `48 × 384 × 48` cells
    /// out of the post-ore dense grids into this map, and with a `String` value
    /// every one of those copies allocated. With ids, seeding is a `u16` move.
    ///
    /// The vegetation *engine* around this store is still string-based (that is
    /// Unit 8); its `&str` accessors below are shims over the id path, so the
    /// per-placement cost is unchanged while the per-*cell* cost is gone.
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
    /// Top-level placed-feature id to eligible biome ids. This is deliberately
    /// keyed by the placed feature, rather than its configured body: a selector
    /// branch can share a body while carrying a different placement contract.
    feature_biomes: Arc<HashMap<String, HashSet<String>>>,
    /// Resolves this grid's [`StateId`]s. Shared with the generator's dense
    /// grids, which is what lets `stitch_veg_region` move ids across without a
    /// string round-trip — ids from a different interner are meaningless here
    /// (see [`StateId`]).
    interner: Arc<StateInterner>,
    /// Positions actually written by `set_if_in_bounds`, **local** (see
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
    /// The ids of `minecraft:{air,cave_air,void_air}` in [`Self::interner`],
    /// resolved once here so [`Self::height_world_surface`]'s per-cell air test is
    /// three integer compares rather than an interner read guard. See
    /// [`Self::is_air_id`] for why an id comparison is exact for air. Unit 8.
    air_ids: [StateId; 3],
    /// Memoised heightmap results, indexed by the local `(x, z)` column.
    ///
    /// Height queries are frequent during vegetation placement and their old
    /// implementation walked the complete vertical span on every call. The
    /// four caches memoise the result for each local column. They are
    /// interior-mutable because the public height accessors intentionally stay
    /// shared (`&self`), while writes invalidate only the column they touch.
    /// A single four-lane cell keeps the caches compact (one allocation and
    /// 16 bytes per column). [`HEIGHT_CACHE_UNSET`] is outside the generated build range
    /// and represents an uncomputed lane.
    height_cache: Vec<Cell<[i32; 4]>>,
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
        Self::with_footprint_interned(
            Arc::new(StateInterner::new()),
            min_y,
            height,
            origin_x,
            origin_z,
            local_lo,
            local_hi,
        )
    }

    /// [`VegGrid::with_footprint`] against a **shared** interner — the form
    /// production must use, so ids seeded from the generator's dense grids mean
    /// the same thing here. The string-taking constructors above build a private
    /// interner, which is correct for a self-contained unit test or parity
    /// fixture and wrong for anything that exchanges ids with another grid.
    #[must_use]
    pub fn with_footprint_interned(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
    ) -> Self {
        let air_ids = [
            interner.id_of("minecraft:air"),
            interner.id_of("minecraft:cave_air"),
            interner.id_of("minecraft:void_air"),
        ];
        let local_width = usize::try_from(local_hi - local_lo)
            .expect("VegGrid footprint must have a non-negative width");
        let column_count = local_width * local_width;
        Self {
            blocks: Overlay::with_bounds(local_lo, local_hi, min_y, height),
            seeded_baseline: None,
            sources: std::array::from_fn(|_| None),
            biome_sources: None,
            flat_biome_sources: None,
            biome_zoom_seed: None,
            feature_biomes: Arc::new(HashMap::new()),
            interner,
            dirty: WriteLog::default(),
            origin_x,
            origin_z,
            min_y,
            height,
            local_lo,
            local_hi,
            air_ids,
            height_cache: std::iter::repeat_with(|| Cell::new([HEIGHT_CACHE_UNSET; 4]))
                .take(column_count)
                .collect(),
            local_width,
            block_entities: Vec::new(),
        }
    }

    /// [`VegGrid::with_footprint_interned`] over the read neighbourhood's **own**
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
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
    ) -> Self {
        let mut grid = Self::with_footprint_interned(
            interner, min_y, height, origin_x, origin_z, local_lo, local_hi,
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
        interner: Arc<StateInterner>,
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
            interner,
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
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: Arc<HashMap<String, HashSet<String>>>,
    ) -> Self {
        Self::with_sources_and_biomes_shared_impl(
            interner,
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
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: Arc<HashMap<String, HashSet<String>>>,
        biome_zoom_seed: i64,
    ) -> Self {
        Self::with_sources_and_biomes_shared_impl(
            interner,
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

    /// Shared-map form for a vertically invariant biome source. Candidate
    /// selection still uses the normal seed-derived nearby-corner zoom; only
    /// the final quart lookup reads a compact 4x4 typed slice instead of a
    /// vertically repeated [`BiomeCells`] allocation. The name-taking form is
    /// a configuration/test adapter; names are parsed before the grid retains
    /// the source.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_flat_biomes_shared_zoomed(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<[String; 16]>>,
        feature_biomes: Arc<HashMap<String, HashSet<String>>>,
        biome_zoom_seed: i64,
    ) -> Self {
        let mut grid = Self::with_sources(
            interner, min_y, height, origin_x, origin_z, local_lo, local_hi, source_at,
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
        grid.feature_biomes = feature_biomes;
        grid
    }

    /// Shared-map form for a vertically invariant biome source whose cells are
    /// constructor-assigned ids. Keeping ids in the producer's immutable cache
    /// avoids retaining sixteen owned biome strings per cached Nether column;
    /// names are resolved only while evaluating the placement membership query.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_sources_and_flat_biome_ids_shared_zoomed(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<[BiomeRef; 16]>>,
        feature_biomes: Arc<HashMap<String, HashSet<String>>>,
        biome_zoom_seed: i64,
    ) -> Self {
        let mut grid = Self::with_sources(
            interner, min_y, height, origin_x, origin_z, local_lo, local_hi, source_at,
        );
        let mut biomes = std::array::from_fn(|_| None);
        for dx in -WIDE_RADIUS..=WIDE_RADIUS {
            for dz in -WIDE_RADIUS..=WIDE_RADIUS {
                biomes[wide_slot_of_offset(dx, dz)] = biome_at(dx, dz);
            }
        }
        grid.flat_biome_sources = Some(FlatBiomeSources::Ids { cells: biomes });
        grid.biome_zoom_seed = Some(biome_zoom_seed);
        grid.feature_biomes = feature_biomes;
        grid
    }

    #[allow(clippy::too_many_arguments)]
    fn with_sources_and_biomes_shared_impl(
        interner: Arc<StateInterner>,
        min_y: i32,
        height: i32,
        origin_x: i32,
        origin_z: i32,
        local_lo: i32,
        local_hi: i32,
        source_at: impl Fn(i32, i32) -> Option<Arc<DenseBlockGrid>>,
        biome_at: impl Fn(i32, i32) -> Option<Arc<BiomeCells>>,
        feature_biomes: Arc<HashMap<String, HashSet<String>>>,
        biome_zoom_seed: Option<i64>,
    ) -> Self {
        let mut grid = Self::with_sources(
            interner, min_y, height, origin_x, origin_z, local_lo, local_hi, source_at,
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
        grid.feature_biomes = feature_biomes;
        grid
    }

    /// Whether the biome at this exact candidate location lists `feature_id`.
    /// A grid without biome sources is a compact unit fixture and deliberately
    /// preserves the historical unconstrained behaviour. A production grid
    /// rejects an inline/unidentified feature, missing sources, and out-of-range
    /// cells instead of turning an unknown membership into a permissive answer.
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
                 -> Option<&str> {
                    let dx = source_chunk_x - centre_chunk_x;
                    let dz = source_chunk_z - centre_chunk_z;
                    let slot = wide_source_slot(dx * 16, dz * 16)?;
                    let cells = match flat {
                        FlatBiomeSources::Ids { cells } => cells,
                    };
                    let cells = cells[slot].as_deref()?;
                    cells[qz * 4 + qx]
                        .builtin_or_none()
                        .map(|biome| biome.name())
                };
                let biome = crate::overworld::zoomed_biome_flat(zoom_seed, x, y, z, source_at);
                return biome.is_some_and(|biome| {
                    self.feature_biomes
                        .get(feature_id)
                        .is_some_and(|biomes| biomes.contains(biome))
                });
            }
            return true;
        };
        let biome = if let Some(zoom_seed) = self.biome_zoom_seed {
            let centre_chunk_x = self.origin_x.div_euclid(16);
            let centre_chunk_z = self.origin_z.div_euclid(16);
            crate::overworld::zoomed_biome(
                zoom_seed,
                x,
                y,
                z,
                |source_chunk_x, source_chunk_z| {
                    let dx = source_chunk_x - centre_chunk_x;
                    let dz = source_chunk_z - centre_chunk_z;
                    wide_source_slot(dx * 16, dz * 16)
                        .and_then(|slot| sources[slot].as_deref())
                },
            )
        } else {
            let lx = x - self.origin_x;
            let lz = z - self.origin_z;
            let Some(slot) = wide_source_slot(lx, lz) else {
                return false;
            };
            let Some(cells) = sources[slot].as_deref() else {
                return false;
            };
            let qx = lx.rem_euclid(16).div_euclid(4) as usize;
            let qz = lz.rem_euclid(16).div_euclid(4) as usize;
            let qy = (y - cells.min_y()).div_euclid(4);
            if qy < 0 || qy >= cells.y_quarts() as i32 {
                return false;
            }
            Some(cells.at_quart(qx, qy as usize, qz))
        };
        let allowed = biome.is_some_and(|biome| {
            self.feature_biomes
                .get(feature_id)
                .is_some_and(|biomes| biomes.contains(biome))
        });
        allowed
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
        wide_source_slot(lx, lz).and_then(|slot| self.sources[slot].as_deref())
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
            grid.get_id(self.origin_x + lx, y, self.origin_z + lz)
        })
    }

    /// This grid's interner, for a caller that needs to resolve or mint ids
    /// against it.
    #[must_use]
    pub fn interner(&self) -> &Arc<StateInterner> {
        &self.interner
    }

    /// Exclusive upper Y bound of the generated source field, distinct from
    /// this grid's receiving window. Nether decoration keeps a widened window
    /// so upper-half writes remain visible to later features, while some
    /// feature bounds are relative to the source generator's depth instead.
    #[must_use]
    pub(super) fn generation_top(&self) -> i32 {
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

    /// Positions written by `set_if_in_bounds` since construction, in write
    /// order, **as absolute world coordinates**, each paired with the state
    /// currently at that position (i.e. the *final* state if the same cell
    /// was written more than once, not an intermediate one) — what a caller
    /// should fold back into a wider grid, with no further translation
    /// needed.
    pub fn dirty_cells(&self) -> impl Iterator<Item = (i32, i32, i32, &str)> {
        self.dirty_cell_ids()
            .map(|(x, y, z, id)| (x, y, z, self.interner.name_of(id)))
    }

    /// [`Self::dirty_cells`] without resolving the states to strings — the
    /// allocation-free form, for a caller folding these writes back into
    /// another id-carrying grid.
    pub fn dirty_cell_ids(&self) -> impl Iterator<Item = (i32, i32, i32, StateId)> {
        self.dirty.iter().map(|&(lx, y, lz)| {
            (
                self.origin_x + lx,
                y,
                self.origin_z + lz,
                self.blocks.get_in_bounds(&(lx, y, lz)).unwrap_or(StateId::AIR),
            )
        })
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

    /// Seeds one column position (absolute world coordinates) from the
    /// post-ore composed grid. Callers fill every `(x, y, z)` in this
    /// chunk's own `16 × height × 16` footprint before running vegetal
    /// decoration.
    /// String-taking shim over [`Self::seed_id`], for parity fixtures and unit
    /// tests. **Not** for the production seeding loop — that is the 884,736
    /// allocations (see the `blocks` field doc).
    pub fn seed(&mut self, x: i32, y: i32, z: i32, state: String) {
        let id = self.interner.id_of(&state);
        self.seed_id(x, y, z, id);
    }

    /// Seeds one column position (absolute world coordinates) from the post-ore
    /// composed grid, by interned id — the zero-allocation seeding path.
    /// Source-less fixtures retain a second sparse copy for the immutable WG
    /// heightmap; source-backed production grids continue to read that lane
    /// from their source snapshot.
    pub fn seed_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            self.blocks.insert_in_bounds((lx, y, lz), state);
            self.invalidate_height_caches(lx, lz);
            if self.seeded_baseline.is_some() || self.sources.iter().all(Option::is_none) {
                // Source-less fixtures store their immutable baseline in a
                // separate sparse snapshot. Seeding after a prior probe must
                // invalidate the WG lane; ordinary decoration writes
                // intentionally do not, because that lane is immutable for
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
                self.height_cache[cache_index].get_mut()[1] = HEIGHT_CACHE_UNSET;
            }
        }
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
        match self.blocks.get_in_bounds(&(lx, y, lz)) {
            Some(id) => id,
            None => self.source_id(lx, y, lz),
        }
    }

    /// Typed block facts for one live cell. Source grids already cache these
    /// facts in their palettes; only a decoration overlay cell needs the
    /// interner lookup. Keeping this split makes heightmap scans exact without
    /// putting a lock on every source-terrain cell.
    #[inline]
    fn get_local_facts(&self, lx: i32, y: i32, lz: i32) -> BaseStateFacts {
        if y < self.min_y || y >= self.min_y + self.height {
            return BaseStateFacts::air();
        }
        match self.blocks.get_in_bounds(&(lx, y, lz)) {
            Some(id) => self.interner.base_facts(id),
            None => self
                .source_grid(lx, lz)
                .map_or_else(BaseStateFacts::air, |source| {
                    source.get_base_facts(self.origin_x + lx, y, self.origin_z + lz)
                }),
        }
    }

    fn get_local(&self, lx: i32, y: i32, lz: i32) -> &str {
        self.interner.name_of(self.get_local_id(lx, y, lz))
    }

    /// Reads always succeed (clamped into bounds) — a read past the local
    /// footprint approximates the nearest in-bounds column rather than
    /// panicking or returning a sentinel the caller has to special-case.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        let (lx, lz) = self.to_local_clamped(x, z);
        self.get_local(lx, y, lz)
    }

    /// [`Self::get`] without resolving to a string — the allocation-free and
    /// lock-free read path.
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
    pub fn set_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: String) -> bool {
        let id = self.interner.id_of(&state);
        self.set_id_if_in_bounds(x, y, z, id)
    }

    /// [`Self::set_if_in_bounds`] by interned id — the allocation-free write
    /// path. Identical bounds behaviour, including the census bumps, so which
    /// form a caller uses cannot change a placement outcome.
    pub fn set_id_if_in_bounds(&mut self, x: i32, y: i32, z: i32, state: StateId) -> bool {
        let (lx, lz) = self.to_local_exact(x, z);
        if self.in_bounds_local(lx, lz) && y >= self.min_y && y < self.min_y + self.height {
            census_bump(|c| c.writes += 1);
            self.blocks.insert_in_bounds((lx, y, lz), state);
            self.invalidate_height_caches(lx, lz);
            self.dirty.push((lx, y, lz));
            true
        } else {
            census_bump(|c| c.writes_rejected += 1);
            false
        }
    }

    /// `Heightmap.Types.WORLD_SURFACE` — topmost non-air, scanned live against
    /// the current (possibly already-modified-this-step) grid. `x`/`z` are
    /// absolute world coordinates. Returns `min_y` (not `min_y - 1`) for an
    /// all-air column, matching the reference `y + 1` convention with `y`
    /// floored at one below the lowest placeable block.
    #[must_use]
    pub fn height_world_surface(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        if let Some(height) = self.cached_height(lx, lz, 0) {
            return height;
        }
        let source = self.source_grid(lx, lz);
        for y in (self.min_y..self.min_y + self.height).rev() {
            let id = match self.blocks.get_in_bounds(&(lx, y, lz)) {
                Some(id) => id,
                None => self.source_id_from_grid(source, lx, y, lz),
            };
            if !self.is_air_id(id) {
                let height = y + 1;
                self.cache_height(lx, lz, 0, height);
                return height;
            }
        }
        self.cache_height(lx, lz, 0, self.min_y);
        self.min_y
    }

    /// `Heightmap.Types.WORLD_SURFACE_WG` — topmost non-air in the immutable
    /// terrain snapshot, before decoration writes. The reference world keeps
    /// this world-generation heightmap unchanged while FEATURES writes update
    /// the final heightmaps, so an earlier grass/tree write must not move a
    /// later `WORLD_SURFACE_WG` probe. `x`/`z` are absolute world coordinates.
    #[must_use]
    pub fn height_world_surface_wg(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        if let Some(height) = self.cached_height(lx, lz, 1) {
            return height;
        }
        let source = self.source_grid(lx, lz);
        for y in (self.min_y..self.min_y + self.height).rev() {
            // Production grids have a source chunk, whose immutable terrain
            // must win over this pass's overlay. Compact parity/unit fixtures
            // have no source and seed that same baseline into a sparse
            // snapshot, so they need the equivalent fallback here rather
            // than reading an all-air synthetic source or the live overlay.
            let id = source.map_or_else(
                || {
                    self.seeded_baseline
                        .as_ref()
                        .and_then(|baseline| baseline.get_in_bounds(&(lx, y, lz)))
                        .unwrap_or(StateId::AIR)
                },
                |source| source.get_id(self.origin_x + lx, y, self.origin_z + lz),
            );
            if !self.is_air_id(id) {
                let height = y + 1;
                self.cache_height(lx, lz, 1, height);
                return height;
            }
        }
        self.cache_height(lx, lz, 1, self.min_y);
        self.min_y
    }

    /// `Heightmap.Types.MOTION_BLOCKING` — the first free row above the
    /// highest motion-blocking block or fluid. Decorative plants and vines do
    /// not raise this heightmap even though they are non-air, while fluids do.
    #[must_use]
    pub fn height_motion_blocking(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        if let Some(height) = self.cached_height(lx, lz, 2) {
            return height;
        }
        for y in (self.min_y..self.min_y + self.height).rev() {
            if self.get_local_facts(lx, y, lz).is_motion_blocking() {
                let height = y + 1;
                self.cache_height(lx, lz, 2, height);
                return height;
            }
        }
        self.cache_height(lx, lz, 2, self.min_y);
        self.min_y
    }

    /// Whether `id` is one of the three air states.
    ///
    /// # Why this can be an id comparison and the fluid test below cannot
    ///
    /// **Air carries no block-state properties**, so for an air state
    /// `base_id(name) == name` and "base is one of three names" is exactly "id is
    /// one of three ids" — the three resolved in [`Self::with_footprint_interned`].
    /// `crate::feature::vegetation::config::is_air` is still the definition; this
    /// is that definition pushed through the interner once per grid instead of
    /// once per cell. Adding a property-carrying state to `is_air` would silently
    /// break this, which is why that function's doc says not to.
    ///
    /// A fluid, by contrast, really does carry properties here —
    /// `crate::carver` writes `minecraft:water[level=0]` — so the heightmap
    /// scans use the typed facts cached with each source palette entry rather
    /// than reducing a state to its base-name string.
    pub(super) fn is_air_id(&self, id: StateId) -> bool {
        self.air_ids.contains(&id)
    }

    /// `Heightmap.Types.OCEAN_FLOOR`/`OCEAN_FLOOR_WG` — topmost **motion-blocking**
    /// block, plus one. `x`/`z` are absolute world coordinates.
    ///
    /// It used to be "topmost non-air, non-fluid", which is not the same predicate
    /// and produced stacked, floating seagrass: seagrass is neither air nor fluid, so
    /// an already-placed plant counted as the floor and the next placement on that
    /// column started on top of it. The scan consumes the generated per-state
    /// blocks-motion table through [`BaseStateFacts`]. This matters for feature
    /// writes such as leaves: their empty collision shape makes them
    /// non-motion-blocking even though they are non-air, so a tree canopy cannot
    /// raise the disk's ocean-floor placement height.
    #[must_use]
    pub fn height_ocean_floor(&self, x: i32, z: i32) -> i32 {
        let (lx, lz) = self.to_local_clamped(x, z);
        if let Some(height) = self.cached_height(lx, lz, 3) {
            return height;
        }
        for y in (self.min_y..self.min_y + self.height).rev() {
            if self.get_local_facts(lx, y, lz).is_ocean_floor() {
                let height = y + 1;
                self.cache_height(lx, lz, 3, height);
                return height;
            }
        }
        self.cache_height(lx, lz, 3, self.min_y);
        self.min_y
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;

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
        /// [`super::VegGrid::set_if_in_bounds`]).
        pub writes_rejected: usize,
    }

    thread_local! {
        static CENSUS: RefCell<VegCensus> = RefCell::new(VegCensus::default());
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
    }

    /// This thread's census so far.
    #[must_use]
    pub fn snapshot() -> VegCensus {
        CENSUS.with(|c| c.borrow().clone())
    }

    pub(in crate::feature::vegetation) fn bump(f: impl FnOnce(&mut VegCensus)) {
        CENSUS.with(|c| f(&mut c.borrow_mut()));
    }
}

#[cfg(test)]
mod heightmap_tests {
    use std::sync::Arc;

    use super::VegGrid;
    use crate::dense_grid::DenseBlockGrid;
    use crate::interner::StateInterner;

    /// The P07 witness is on the east edge of source `(-26,-25)`: the source
    /// writes its first grass at `(-401,122,-387)` before the later candidate
    /// reaches `(-400,121,-385)` in target `(-25,-25)`. Keep the two columns
    /// distinct here so a live-overlay regression cannot masquerade as a
    /// source-terrain read.
    fn p07_source_grid() -> (VegGrid, u16, u16, u16) {
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let grass = interner.id_of("minecraft:grass_block");
        let short_grass = interner.id_of("minecraft:short_grass");

        let mut west = DenseBlockGrid::with_interner(
            Arc::clone(&interner), -416, 0, -400, 16, 128, 16, air,
        );
        west.set_id(-401, 121, -387, grass);

        let mut centre = DenseBlockGrid::with_interner(
            Arc::clone(&interner), -400, 0, -400, 16, 128, 16, air,
        );
        centre.set_id(-400, 120, -385, grass);

        let west = Arc::new(west);
        let centre = Arc::new(centre);
        let grid = VegGrid::with_sources(
            Arc::clone(&interner),
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
        (grid, air.raw(), grass.raw(), short_grass.raw())
    }

    #[test]
    fn p07_west_source_world_surface_wg_ignores_its_first_grass_write() {
        let (mut grid, air, grass, short_grass) = p07_source_grid();
        let source_probe: (i32, i32) = (-401, -387);
        let witness: (i32, i32, i32) = (-400, 121, -385);

        assert_eq!(source_probe.0.div_euclid(16), -26);
        assert_eq!(source_probe.1.div_euclid(16), -25);
        assert_eq!((witness.0.div_euclid(16), witness.2.div_euclid(16)), (-25, -25));
        assert_eq!(grid.get_id(witness.0, witness.1, witness.2).raw(), air);
        assert_eq!(grid.get_id(witness.0, 120, witness.2).raw(), grass);
        assert_eq!(grid.height_world_surface_wg(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface_wg(source_probe.0, source_probe.1), 122);

        assert!(grid.set_id_if_in_bounds(-401, 122, -387, grid.interner().id_of("minecraft:short_grass")));

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
        assert_eq!(grid.get_id(witness.0, witness.1, witness.2).raw(), air);
        assert_eq!(grid.height_world_surface_wg(witness.0, witness.2), 121);
        assert_eq!(grid.height_world_surface(witness.0, witness.2), 121);
        assert_eq!(short_grass, grid.interner().id_of("minecraft:short_grass").raw());
    }

    #[test]
    fn source_less_seeded_baseline_drives_world_surface_wg_without_live_overlay() {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let dirt = grid.interner().id_of("minecraft:dirt");

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

        // Seeding a changed baseline after a cached probe must invalidate the
        // fixture's WG lane without making ordinary overlay writes do so.
        grid.seed_id(0, 10, 0, dirt);
        assert_eq!(grid.height_world_surface_wg(0, 0), 11);
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
            grid.interner().id_of("minecraft:short_grass"),
        );
        assert_eq!(grid.height_world_surface(source_probe.0, source_probe.1), 123);
        assert_eq!(grid.get_id(-401, 122, -387).raw(), short_grass);
    }

    #[test]
    fn all_heightmap_caches_invalidate_after_overlay_writes() {
        let mut grid = VegGrid::with_footprint(0, 16, 0, 0, 0, 1);
        let air = grid.interner().id_of("minecraft:air");
        let grass = grid.interner().id_of("minecraft:grass_block");
        let water = grid.interner().id_of("minecraft:water[level=0]");
        let short_grass = grid.interner().id_of("minecraft:short_grass");

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
    fn ocean_floor_ignores_leaves_but_mutation_to_stone_raises_it() {
        let mut grid = VegGrid::with_footprint(0, 80, 0, 0, 0, 1);
        let dirt = grid.interner().id_of("minecraft:dirt");
        let water = grid.interner().id_of("minecraft:water[level=0]");
        let leaves = grid
            .interner()
            .id_of("minecraft:dark_oak_leaves[distance=3,persistent=false,waterlogged=false]");
        let stone = grid.interner().id_of("minecraft:stone");
        assert!(grid.set_id_if_in_bounds(0, 60, 0, dirt));
        assert!(grid.set_id_if_in_bounds(0, 61, 0, water));
        assert!(grid.set_id_if_in_bounds(0, 62, 0, water));
        assert!(grid.set_id_if_in_bounds(0, 67, 0, leaves));
        assert!(grid.set_id_if_in_bounds(0, 68, 0, leaves));

        assert_eq!(grid.height_ocean_floor(0, 0), 61);
        assert_eq!(grid.height_motion_blocking(0, 0), 63);

        // Mutation control: replacing the canopy's highest cell with a solid
        // block must raise both motion-based heightmaps to that cell.
        assert!(grid.set_id_if_in_bounds(0, 68, 0, stone));
        assert_eq!(grid.height_ocean_floor(0, 0), 69);
        assert_eq!(grid.height_motion_blocking(0, 0), 69);
    }
}
