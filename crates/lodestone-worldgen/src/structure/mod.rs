//! Structure **placement and starts** — this change's S1, the engine that decides
//! *which chunk gets which structure* for a seed.
//!
//! # What it is
//!
//! A [`StructureRegistry`], built once per generator from a
//! [`Resolver`](crate::density::Resolver)'s `worldgen/structure_set/*.json` and
//! `worldgen/structure/*.json` documents, that answers one question per chunk:
//! [`StructureRegistry::starts_at`] → the [`StructureStart`]s whose origin is
//! that chunk. It is the port of vanilla's
//! `ChunkGenerator.createStructures` + `ChunkGeneratorStructureState` +
//! `StructurePlacement` triple, and it is a pure function of
//! `(seed, chunk, climate)` — no chunk data, no block work, embarrassingly
//! parallel, which is what lets `overworld::store` memoise it in a stage slot
//! *above* the terrain stages.
//!
//! # How it works
//!
//! ```text
//! for each structure set, in vanilla's bootstrap order:
//!     placement.is_placement_chunk(seed, cx, cz)?      <- the jittered grid
//!     placement.passes_frequency(seed, cx, cz)?        <- 2 of 20 sets only
//!     no excluded neighbour placement in range?        <- 1 of 20 sets only
//!     select a structure from the set's weighted entries
//!         (one `setLargeFeatureSeed` stream, retried on an invalid start)
//!     start predicate: sample the structure's own generation point,
//!         then check the biome there against the structure's `biomes` tag
//! ```
//!
//! The per-structure predicate is [`StructureKind::start`], and the world data it
//! needs — a `WORLD_SURFACE_WG`/`OCEAN_FLOOR_WG` column height and a biome at a
//! quart — comes in through [`StartContext`] so this module stays free of the
//! terrain pipeline it must run *before*.
//!
//! # What is implemented, and the ledger that says so
//!
//! Placement is complete for both placement types. **Start generation is not
//! complete for all 34 structures and is not pretending to be**: a structure
//! whose piece generator has not landed yet
//! ([`StructureKind::Unsupported`]) still gets a start when its placement and
//! biome say so, but with [`StructureStart::pieces_complete`] `false` and an
//! empty piece list, and its id is named in
//! [`StructureRegistry::unsupported`]. That is the `collect_unsupported` pattern
//! this crate already uses for features (`feature/vegetation/config.rs`):
//! legible silence, never a silent skip.
//!
//! **This paragraph used to stop at S1 and went stale for four landings in a
//! row** — jigsaw (S4), the coded pieces (S5), mineshaft (S7) and ruined_portal
//! all shipped after it was written, and nothing here said so until this
//! sentence. Read the ledger (`StructureRegistry::unsupported`) or the oracle
//! test (`tests/structure_placement_oracle.rs`) before citing a structure as
//! unimplemented; both are re-verified in CI, this paragraph is not. Seven sets
//! are *closed* today — every structure they can place has a real generator, so
//! for those the start set is exactly vanilla's:
//!
//! | set | structures | oracle starts (seed −195764831) |
//! |---|---|---|
//! | `shipwrecks` | shipwreck, shipwreck_beached | 11 |
//! | `ocean_ruins` | ocean_ruin_cold, ocean_ruin_warm | 16 |
//! | `buried_treasures` | buried_treasure | 2 |
//! | `ocean_monuments` | monument | 2 |
//! | `mineshafts` | mineshaft, mineshaft_mesa | 46 |
//! | `ruined_portals` (overworld ids only) | ruined_portal, `_desert`, `_jungle`, `_mountain`, `_ocean`, `_swamp` | 9 |
//! | `igloos` | igloo | — |
//!
//! **Also landed, with a real piece generator, but not counted as a closed set**
//! because a jigsaw structure's own *pool graph* can still refuse an individual
//! start (a missing pool alias, an unsupported processor): villages, ancient
//! city, pillager outpost, trail ruins, trial chambers and — in the Nether —
//! bastion remnant. See [`jigsaw`] for the assembly, `docs/worldgen-jigsaw.md`
//! for what a [`jigsaw::JigsawConfig`] refuses to model, and
//! `tests/structure_jigsaw.rs` for the oracle's own coverage of each.
//!
//! **`stronghold` now has a real piece generator** — [`stronghold`], the whole
//! `StrongholdPieces` tree ending in a portal room every generated stronghold
//! is guaranteed to contain. The oracle world at `.cache/mc/survival`
//! contains no stronghold to verify piece assembly against (only ring
//! placement, [`placement`]'s `ConcentricRingsStructurePlacement`, predates
//! this), so its correctness rests on the decompiled record plus the
//! self-consistency gates in `stronghold`'s own test module.
//!
//! **`monument` now has a real piece generator** — [`monument`], the fixed
//! 58×23×58 building plus its `RoomDefinition` grid graph. The oracle world at
//! `.cache/mc/survival` records only the two monument starts' chunk positions
//! (`structures.starts` carries no piece layout — see `monument`'s own module
//! doc), so its correctness rests on the decompiled record plus
//! self-consistency, the same evidentiary footing as `stronghold`. A handful
//! of room-interior decorations are deliberately left out — see `monument`'s
//! own deviations list for exactly which and why.
//!
//! **What genuinely remains**: `ruined_portal_nether` (the Nether-side setup
//! of the *same* `type` id `ruined_portals` overworld already covers —
//! refused wholesale, see [`StructureKind::parse`]). `ruined_portal`'s own
//! frame is real but its `spreadNetherrack` terrain skirt is not —
//! `coded:ruined_portal_terrain_skirt` on the ledger.
//!
//! **S2 landed the template engine** ([`template`], [`processor`]): shipwreck,
//! ocean ruin, igloo and (S8) ruined_portal build real piece lists out of the
//! bundled `.nbt` templates and write blocks — see
//! [`docs/worldgen-structure-templates.md`] for the whole path, including which
//! vanilla behaviours are deliberately absent.
//!
//! [`docs/worldgen-structure-templates.md`]: ../../../../docs/worldgen-structure-templates.md
//!
//! # How to change it
//!
//! * **To add a structure**, add a [`StructureKind`] variant, parse it in
//!   [`StructureKind::parse`], and implement its arm of [`StructureKind::start`].
//!   The arm's job is to produce the *generation point* and the piece list;
//!   the biome filter is applied by the caller ([`StructureRegistry::try_start`])
//!   because vanilla applies it uniformly in `findValidGenerationPoint`.
//! * **The RNG stream is per-chunk and shared across a structure's own draws**:
//!   `Structure.GenerationContext` seeds one `WorldgenRandom` with
//!   `setLargeFeatureSeed(seed, cx, cz)` and *every* draw the structure makes
//!   comes out of it, in order. Mineshaft's discarded leading `nextDouble()` is
//!   the canonical trap — it exists only to shift the stream.
//! * **Piece generation is lazy in vanilla and must stay lazy here.**
//!   `Structure.GenerationStub` holds `Either<Consumer<Builder>, Builder>`: the
//!   `Consumer` arm is only run by `getPiecesBuilder()`, *after*
//!   `findValidGenerationPoint`'s biome filter. So a structure that fails its
//!   biome check consumes **no** RNG beyond the generation point. Eagerly
//!   generating pieces to "see if it works" would change every subsequent
//!   structure at that seed. `Either.right` structures (mineshaft, jigsaw)
//!   generate eagerly *by definition* — that is why their start position depends
//!   on their own pieces.
//!
//! # Configuration
//!
//! None. Everything is data:
//! [`Resolver::structure_set_ids`](crate::density::Resolver::structure_set_ids),
//! [`structure_set`](crate::density::Resolver::structure_set),
//! [`structure`](crate::density::Resolver::structure) and
//! [`biome_tag`](crate::density::Resolver::biome_tag). A resolver that supplies
//! none of them (every fixture resolver in this workspace) gets an empty
//! registry and no structures — the crate's standing "no data supplied"
//! convention.
//!
//! # Dependencies
//!
//! [`placement`] for the placement predicates,
//! [`lodestone_worldgen_core::rng`] for the seed derivations, and nothing else.
//! `crate::overworld` supplies the [`StartContext`].

pub mod beardifier;
pub mod coded;
pub mod jigsaw;
pub mod mineshaft;
pub mod monument;
pub mod placement;
pub mod pool;
pub mod processor;
pub mod stronghold;
pub mod template;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lodestone_worldgen_core::rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource, WorldgenRandom,
};
use serde_json::Value;

use crate::aquifer::BlockKind;
use crate::density::Resolver;
use jigsaw::{JigsawConfig, JigsawStub};
use placement::{Placement, PlacementKind};
use pool::PoolStore;
use processor::{PosTest, Processor, ProcessorRule, RuleTest};
use template::{BlockState, Mirror, PlaceSettings, Rotation, StructureTemplate};

/// The concrete random every structure's per-chunk stream is —
/// `WorldgenRandom` over a legacy LCG, seeded by
/// `setLargeFeatureSeed(seed, cx, cz)`.
type StructureRandom = WorldgenRandom<LegacyRandomSource>;

/// `Structure.GenerationContext`'s random for one `(structure, chunk)`.
fn structure_random(seed: i64, cx: i32, cz: i32) -> StructureRandom {
    let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
    random.set_large_feature_seed(seed, cx, cz);
    random
}

/// Vanilla's structure-set registration order, read from
/// `.cache/mc/26.2/src/net/minecraft/data/worldgen/StructureSets.java`'s
/// `bootstrap` body.
///
/// `ChunkGenerator.createStructures` walks `possibleStructureSets()` in registry
/// order, so this is the order two sets competing for one chunk resolve in.
/// In 26.2 no two sets can place the same *structure*, so the order is almost
/// inert — but "almost" is not "is", and a sorted-by-name order would be a
/// silent, seed-dependent divergence rather than an error. Unknown ids (a
/// datapack's own sets) sort after these, by name.
const BOOTSTRAP_ORDER: &[&str] = &[
    "minecraft:villages",
    "minecraft:desert_pyramids",
    "minecraft:igloos",
    "minecraft:jungle_temples",
    "minecraft:swamp_huts",
    "minecraft:pillager_outposts",
    "minecraft:ancient_cities",
    "minecraft:ocean_monuments",
    "minecraft:woodland_mansions",
    "minecraft:buried_treasures",
    "minecraft:mineshafts",
    "minecraft:ruined_portals",
    "minecraft:shipwrecks",
    "minecraft:ocean_ruins",
    "minecraft:nether_complexes",
    "minecraft:nether_fossils",
    "minecraft:end_cities",
    "minecraft:strongholds",
    "minecraft:trail_ruins",
    "minecraft:trial_chambers",
];

/// An inclusive block-space AABB — vanilla's `BoundingBox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundingBox {
    /// Inclusive minimum corner, `[x, y, z]`.
    pub min: [i32; 3],
    /// Inclusive maximum corner, `[x, y, z]`.
    pub max: [i32; 3],
}

impl BoundingBox {
    /// A single-block box, `new BoundingBox(pos)`.
    #[must_use]
    pub fn of_block(x: i32, y: i32, z: i32) -> Self {
        Self {
            min: [x, y, z],
            max: [x, y, z],
        }
    }

    /// The union of `self` and `other`.
    #[must_use]
    pub fn encapsulate(self, other: Self) -> Self {
        Self {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    /// `inflatedBy(n)` — grows every face by `n`.
    #[must_use]
    pub fn inflated_by(self, n: i32) -> Self {
        Self {
            min: [self.min[0] - n, self.min[1] - n, self.min[2] - n],
            max: [self.max[0] + n, self.max[1] + n, self.max[2] + n],
        }
    }

    /// `BoundingBox.fromCorners(a, b)` — the box spanning two corners in any
    /// order.
    #[must_use]
    pub fn from_corners(a: [i32; 3], b: [i32; 3]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        }
    }

    /// `intersects(other)` — overlap on all three axes.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.max[0] >= other.min[0]
            && self.min[0] <= other.max[0]
            && self.max[2] >= other.min[2]
            && self.min[2] <= other.max[2]
            && self.max[1] >= other.min[1]
            && self.min[1] <= other.max[1]
    }

    /// `intersects(minX, minZ, maxX, maxZ)` — the horizontal-only test both
    /// `createReferences` and the beardifier's `isCloseToChunk` are built on.
    #[must_use]
    pub fn intersects_xz(self, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> bool {
        self.max[0] >= min_x && self.min[0] <= max_x && self.max[2] >= min_z && self.min[2] <= max_z
    }

    /// `StructurePiece.isCloseToChunk(chunkPos, distance)` — whether this box
    /// comes within `distance` blocks of chunk `(cx, cz)`.
    #[must_use]
    pub fn is_close_to_chunk(self, cx: i32, cz: i32, distance: i32) -> bool {
        let (bx, bz) = (cx * 16, cz * 16);
        self.intersects_xz(
            bx - distance,
            bz - distance,
            bx + 15 + distance,
            bz + 15 + distance,
        )
    }
}

/// `TerrainAdjustment` — how (and whether) the beardifier reshapes terrain under
/// a structure.
///
/// Carried by S1 and **evaluated since S3** by [`beardifier::Beardifier::compute`],
/// which is the only reader: nothing in this file branches on it. Every variant
/// below names its structures, and every one of those structures is still on the
/// ledger (jigsaw is S4, `stronghold`/`nether_fossil` are S5), so no *real* start
/// carries a non-`None` value in a generated world yet — see
/// `docs/worldgen-beardifier.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainAdjustment {
    /// `none` — 23 of the 34 bundled structures.
    None,
    /// `beard_thin` — the 5 villages, `pillager_outpost`, `nether_fossil`.
    BeardThin,
    /// `beard_box` — `ancient_city`.
    BeardBox,
    /// `bury` — `stronghold`, `trail_ruins`.
    Bury,
    /// `encapsulate` — `trial_chambers`.
    Encapsulate,
}

impl TerrainAdjustment {
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("beard_thin") => Self::BeardThin,
            Some("beard_box") => Self::BeardBox,
            Some("bury") => Self::Bury,
            Some("encapsulate") => Self::Encapsulate,
            _ => Self::None,
        }
    }
}

/// The two heightmaps a structure's generation point can be sampled against.
///
/// Named `_WG` after vanilla because these are the *worldgen* heightmaps, read
/// from a freshly sampled noise column rather than from a generated chunk — which
/// is exactly what lets `structure_starts` run before terrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightmapKind {
    /// `WORLD_SURFACE_WG` — first non-air from the top (water counts).
    WorldSurfaceWg,
    /// `OCEAN_FLOOR_WG` — first motion-blocking block (water does **not** count).
    OceanFloorWg,
}

/// The world data a start predicate needs, supplied by the generator.
///
/// Deliberately tiny and deliberately *not* the terrain pipeline: implementing
/// this with anything that reads a generated chunk would invert the stage order
/// structures exist to respect (starts precede noise).
pub trait StartContext {
    /// `ChunkGenerator.getFirstOccupiedHeight(x, z, heightmap)` — the Y of the
    /// topmost block satisfying `heightmap`, i.e. vanilla's `getBaseHeight - 1`.
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32;
    /// `BiomeSource.getNoiseBiome(qx, qy, qz)` — the biome id at a quart cell.
    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String;
    /// The dimension's sea level.
    fn sea_level(&self) -> i32;
    /// `LevelHeightAccessor.getMinY()`. Defaulted to the overworld's so that no
    /// existing implementor had to change; only a `VerticalAnchor` other than
    /// `absolute` and a jigsaw's dimension padding read it.
    fn min_y(&self) -> i32 {
        -64
    }
    /// `LevelHeightAccessor.getHeight()`, i.e. `getMaxY() = min_y + height - 1`.
    fn dimension_height(&self) -> i32 {
        384
    }
    /// Whether the pre-surface column at `(x, y, z)` is
    /// `StructurePiece.isReplaceableByStructures` — air or fluid.
    ///
    /// The **coded**-piece equivalent of [`Self::first_occupied_height`]: a
    /// `fillColumnDown` walks downward from a local Y until it hits something
    /// unreplaceable, so it needs the column's contents and not just its top. What
    /// it reads is the raw `_WG` shape, so surface rules and carvers are not
    /// visible — a cave that vanilla's FEATURES-time `postProcess` would fill under
    /// a pyramid stays open here, the same class of deviation S2 took for template
    /// piece Y.
    ///
    /// Defaulted to agree with [`Self::block_kind_at`], so an implementor that
    /// supplies the four-way kind gets this for free and the two can never
    /// disagree. The effect of *both* defaults together is "solid everywhere": a
    /// stilt or a foundation column is one block long, never runaway.
    fn is_replaceable_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.block_kind_at(x, y, z) != BlockKind::Stone
    }
    /// The pre-surface **kind** of the block at `(x, y, z)` — the four-way answer
    /// `AquiferSystem::block_at` already computes for the fill.
    ///
    /// A strict refinement of [`Self::is_replaceable_at`], which cannot separate air
    /// from water from lava. Three transcriptions need the distinction and none of
    /// them can be written without it:
    ///
    /// * `MineShaftPiece.isInInvalidLocation` walks the shell of a piece's box
    ///   looking for `state.liquid()`, and a mineshaft that treated air as liquid
    ///   would refuse every piece it generated;
    /// * `MineShaftCorridor.fillPillarDownOrChainUp`'s downward probe treats a
    ///   liquid column as empty but stops at lava (`!belowState.is(LAVA)`);
    /// * `RuinedPortalPiece.canBlockBeReplacedByNetherrackOrMagma` tests for lava
    ///   and obsidian.
    ///
    /// # What it deliberately cannot tell you
    ///
    /// This is the raw `_WG` shape, so **every solid block is one
    /// [`BlockKind::Stone`]**. Surface rules (sand, grass, snow), ore blobs
    /// (granite/diorite/andesite) and carvers all run *after* the eager start pass,
    /// so a walk that terminates on "the first block that is not sand" would
    /// terminate on its first iteration. That is not a hypothetical: it is why
    /// `buried_treasure`'s chest is still ledgered rather than placed, and a caller
    /// that needs a *material* rather than a shape has to say so instead of reaching
    /// for this.
    ///
    /// Defaulted to [`BlockKind::Stone`] so no existing implementor had to change.
    fn block_kind_at(&self, _x: i32, _y: i32, _z: i32) -> BlockKind {
        BlockKind::Stone
    }
}

/// Everything a template-driven piece needs to write itself into a chunk: the
/// decoded template, the world position of its origin, and its place settings.
///
/// Held behind an `Arc` on the piece because a start is cloned into every chunk
/// that references it (`structure_refs`), and the template is the largest thing
/// in the engine per-chunk graph.
#[derive(Debug, Clone)]
pub struct PiecePlacement {
    /// The decoded template.
    pub template: Arc<StructureTemplate>,
    /// `templatePosition` — the world position template-relative `(0,0,0)` lands
    /// at, **after** every height adjustment (see
    /// [`StructureKind::generate_pieces`] for why that is resolved eagerly here
    /// and lazily in vanilla).
    pub position: [i32; 3],
    /// Rotation, mirror, pivot and the processor chain.
    pub settings: PlaceSettings,
}

/// One block a **coded** piece generator emits, resolved at start time.
///
/// The seam that makes a coded `postProcess` work in a per-chunk memoised
/// pipeline. Vanilla's coded generators write into the world as they walk, reading
/// heights and existing blocks at arbitrary positions and freely crossing chunk
/// borders from whichever chunk got there first; we cannot, so a coded piece
/// resolves its whole block list once, against [`StartContext`], and
/// `structure_place_stage` clips it. The cost is the list's memory (a desert
/// pyramid is ~7k entries) and the gain is that two chunks placing two halves of
/// one pyramid cannot disagree.
#[derive(Debug, Clone)]
pub struct CodedBlock {
    /// Absolute world position.
    pub pos: [i32; 3],
    /// The canonical block state string, ready for
    /// [`DenseBlockGrid::set`](crate::dense_grid::DenseBlockGrid::set).
    pub state: String,
}

/// A loot table a **coded** piece attached to a container it placed —
/// `StructurePiece.createChest`/`createDispenser`'s
/// `blockEntity.setLootTable(table, random.nextLong())`.
///
/// # Why this is a side list rather than a field on [`CodedBlock`]
///
/// A desert pyramid's block list is ~7k entries and exactly **four** of them are
/// chests, so an `Option<String>` per block would cost ~170 KiB per piece to carry
/// four values. It is also a different *kind* of fact: `state` goes to the block
/// field, this goes to whatever builds block entities — today
/// `lodestone_server::structure_loot`, which reads a **template** piece's raw
/// `structure_block` DATA markers and has no equivalent source for a coded piece,
/// since a coded piece has no template to re-read. This list is that source.
///
/// The `seed` is vanilla's `random.nextLong()`, drawn from the same stream the
/// piece's other draws come from and in source order, so it is part of the
/// stream-position specification whether or not anything rolls with it yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedLoot {
    /// Absolute world position of the container block.
    pub pos: [i32; 3],
    /// The loot table id, e.g. `minecraft:chests/jungle_temple`.
    pub table: String,
    /// `random.nextLong()` — vanilla's per-container roll seed.
    pub seed: i64,
}

/// One piece of a structure start — the unit vanilla persists under
/// `structures.starts.<id>.Children`.
#[derive(Debug, Clone)]
pub struct StructurePiece {
    /// The `StructurePieceType` id, e.g. `minecraft:btp`, `minecraft:shipwreck`.
    pub id: String,
    /// `BB`.
    pub bounding_box: BoundingBox,
    /// `O` — the piece's 2D orientation, `None` serialising as `-1`.
    pub orientation: Option<i32>,
    /// `GD` — generation depth.
    pub gen_depth: i32,
    /// `Template`, for template-driven pieces (S2). `None` for coded pieces.
    pub template: Option<String>,
    /// How to place it, for a template-driven piece. `None` for a coded piece,
    /// which therefore reaches no blocks yet (`minecraft:btp` is the only one).
    pub placement: Option<Arc<PiecePlacement>>,
    /// The *second and later* templates of a `list_pool_element`, which places
    /// several templates at one position (`pillager_outpost/towers`).
    ///
    /// A separate field rather than a `Vec` in [`Self::placement`]'s place because
    /// one `StructurePiece` really is one piece here — it carries one bounding
    /// box, one junction list and one beard — and splitting it into siblings would
    /// hand the beardifier a duplicate rigid box per sub-template.
    pub extra_placements: Vec<Arc<PiecePlacement>>,
    /// A **coded** piece's pre-resolved block list, or `None` for a template piece.
    ///
    /// `Option<Arc<…>>` rather than a bare `Vec` for the same reason
    /// [`Self::placement`] is: a start is cloned into every chunk that references
    /// it, and a pyramid's 7k blocks must be a refcount bump rather than a copy.
    pub blocks: Option<Arc<Vec<CodedBlock>>>,
    /// The containers a **coded** piece attached a loot table to, in the order
    /// vanilla's `postProcess` created them. Empty for every template piece (whose
    /// loot lives in the template's own bytes) and for a coded piece with no
    /// container. See [`CodedLoot`].
    pub loot: Vec<CodedLoot>,
    /// The `PoolElementStructurePiece`-only facts the beardifier reads, or `None`
    /// for a coded piece — which is vanilla's own `else` branch
    /// (`Beardifier.java:75`: rigid box, `groundLevelDelta` 0, no junctions), not
    /// an absence of behaviour. See [`beardifier::PieceBeard`].
    pub beard: Option<beardifier::PieceBeard>,
    /// A **placement-time** refinement — work that reads the real per-chunk
    /// [`DenseBlockGrid`](crate::dense_grid::DenseBlockGrid) as
    /// [`crate::overworld::OverworldGenerator::structure_place_stage`] writes it,
    /// rather than [`Self::blocks`]'s eager start-time list. `None` for every
    /// piece above; [`PieceRefinement::BuriedTreasureChest`] is the first and
    /// only user — see its own doc for why buried treasure's chest needs this
    /// and no other coded piece does.
    pub refine: Option<PieceRefinement>,
}

/// A piece kind whose blocks cannot be decided at start time and are instead
/// resolved when [`crate::overworld::OverworldGenerator::structure_place_stage`]
/// places the piece — the one point in this engine's pipeline where a
/// structure's own chunk has already been through surface rules and carvers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceRefinement {
    /// `BuriedTreasurePieces.postProcess`'s walk-down-to-stone-then-place-chest.
    ///
    /// Every other coded piece in this crate resolves its blocks eagerly, from
    /// [`StartContext`]'s pre-surface `_WG` shape (see `coded:buried_treasure_chest`'s
    /// predecessor row on the ledger, now removed). Buried treasure cannot: its
    /// termination condition is "the block below is sandstone, stone, andesite,
    /// granite or diorite" — a **material** distinction that, pre-surface, does
    /// not exist yet (every solid cell is one undifferentiated
    /// [`crate::aquifer::BlockKind::Stone`]). So this variant defers the whole
    /// walk to placement time, where the piece's own origin chunk's real
    /// [`DenseBlockGrid`](crate::dense_grid::DenseBlockGrid) — sand, sandstone
    /// and all — already exists, because `structure_place_stage` runs at the
    /// **end** of `pre_ore_stage`, after this chunk's own surface and carve
    /// passes. The piece's start position is unchanged (`chunkBlockX(9)`,
    /// literal Y 90, `chunkBlockZ(9)`, matching vanilla's own pre-`postProcess`
    /// box) — only the chest's *placement* is deferred, not its start.
    BuriedTreasureChest,
}

/// The decoded templates one registry can place, keyed by template id
/// (`minecraft:shipwreck/with_mast`).
///
/// Loaded **eagerly**, once per generator, for exactly the templates the
/// supported [`StructureKind`]s can name (71 of the bundled 1212): a start
/// predicate runs inside the chunk pipeline where there is no `&dyn Resolver` to
/// reach and no obvious place to put a lock, and 71 gunzips at construction is
/// cheaper than the machinery to avoid them.
#[derive(Debug, Default)]
pub struct TemplateStore {
    templates: HashMap<String, Arc<StructureTemplate>>,
}

impl TemplateStore {
    /// One decoded template, or `None` when it was not bundled or did not parse
    /// (in which case its structure is on the ledger).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<StructureTemplate>> {
        self.templates.get(id)
    }

    /// How many templates are loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// True when nothing is loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Loads every id in `ids` that is not already present, returning the ids
    /// that could not be loaded paired with why.
    fn load(&mut self, resolver: &dyn Resolver, ids: &[&str]) -> Vec<(String, String)> {
        let mut failures = Vec::new();
        for id in ids {
            if self.templates.contains_key(*id) {
                continue;
            }
            let Some(bytes) = resolver.structure_template(id) else {
                failures.push(((*id).to_string(), "template not bundled".to_string()));
                continue;
            };
            match StructureTemplate::parse(&bytes) {
                Ok(template) => {
                    self.templates.insert((*id).to_string(), Arc::new(template));
                }
                Err(why) => failures.push(((*id).to_string(), why)),
            }
        }
        failures
    }
}

/// Whether a candidate structure's start could be decided at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Validity {
    /// Pieces generated; vanilla's `isValid()` answer is known and true.
    Valid,
    /// Vanilla would have produced `INVALID_START` — try the next weighted option.
    Invalid,
    /// The generator does not exist yet, so validity is unknowable. Treated as
    /// `Valid` for placement purposes (see [`StructureStart::pieces_complete`]).
    Unknown,
}

/// One structure start: the record vanilla writes into its origin chunk's
/// `structures.starts` compound.
#[derive(Debug, Clone)]
pub struct StructureStart {
    /// The structure id, e.g. `minecraft:shipwreck` — NBT `id`.
    pub structure: String,
    /// NBT `ChunkX`.
    pub chunk_x: i32,
    /// NBT `ChunkZ`.
    pub chunk_z: i32,
    /// NBT `references`. Always 0 at generation time; vanilla increments it only
    /// when a *nearby* chunk claims the start, which our `refs` stage does not
    /// mutate (it is a pure recomputation, so a persisted 0 is not a divergence
    /// for a freshly generated chunk).
    pub references: i32,
    /// The union of the pieces' boxes, or the origin chunk's column when the
    /// piece list is not yet computable.
    pub bounding_box: BoundingBox,
    /// The pieces, `Children` in NBT. Empty when
    /// [`Self::pieces_complete`] is false.
    pub pieces: Vec<StructurePiece>,
    /// `terrain_adaptation`, for S3's beardifier.
    pub terrain_adaptation: TerrainAdjustment,
    /// **False when this engine cannot yet generate this structure's pieces.**
    ///
    /// The start is still real — its chunk and structure id are exactly vanilla's
    /// — but its box is a placeholder and its `Children` list is empty. A
    /// persistence consumer must not write an incomplete start into a save: a
    /// start with no children reloads as `INVALID` in vanilla, which is worse
    /// than absent. The `overworld` stage keeps both so the placement gate can
    /// compare ids while the NBT writer filters.
    pub pieces_complete: bool,
}

impl StructureStart {
    /// `Structure.adjustBoundingBox` — the box the beardifier and
    /// `createReferences` see, inflated by 12 for any adaptation-bearing
    /// structure.
    #[must_use]
    pub fn adjusted_bounding_box(&self) -> BoundingBox {
        if self.terrain_adaptation == TerrainAdjustment::None {
            self.bounding_box
        } else {
            self.bounding_box.inflated_by(12)
        }
    }
}

/// `ShipwreckPieces.STRUCTURE_LOCATION_BEACHED`.
const SHIPWRECK_BEACHED: &[&str] = &[
    "minecraft:shipwreck/with_mast",
    "minecraft:shipwreck/sideways_full",
    "minecraft:shipwreck/sideways_fronthalf",
    "minecraft:shipwreck/sideways_backhalf",
    "minecraft:shipwreck/rightsideup_full",
    "minecraft:shipwreck/rightsideup_fronthalf",
    "minecraft:shipwreck/rightsideup_backhalf",
    "minecraft:shipwreck/with_mast_degraded",
    "minecraft:shipwreck/rightsideup_full_degraded",
    "minecraft:shipwreck/rightsideup_fronthalf_degraded",
    "minecraft:shipwreck/rightsideup_backhalf_degraded",
];

/// `ShipwreckPieces.STRUCTURE_LOCATION_OCEAN`.
const SHIPWRECK_OCEAN: &[&str] = &[
    "minecraft:shipwreck/with_mast",
    "minecraft:shipwreck/upsidedown_full",
    "minecraft:shipwreck/upsidedown_fronthalf",
    "minecraft:shipwreck/upsidedown_backhalf",
    "minecraft:shipwreck/sideways_full",
    "minecraft:shipwreck/sideways_fronthalf",
    "minecraft:shipwreck/sideways_backhalf",
    "minecraft:shipwreck/rightsideup_full",
    "minecraft:shipwreck/rightsideup_fronthalf",
    "minecraft:shipwreck/rightsideup_backhalf",
    "minecraft:shipwreck/with_mast_degraded",
    "minecraft:shipwreck/upsidedown_full_degraded",
    "minecraft:shipwreck/upsidedown_fronthalf_degraded",
    "minecraft:shipwreck/upsidedown_backhalf_degraded",
    "minecraft:shipwreck/sideways_full_degraded",
    "minecraft:shipwreck/sideways_fronthalf_degraded",
    "minecraft:shipwreck/sideways_backhalf_degraded",
    "minecraft:shipwreck/rightsideup_full_degraded",
    "minecraft:shipwreck/rightsideup_fronthalf_degraded",
    "minecraft:shipwreck/rightsideup_backhalf_degraded",
];

/// `ShipwreckPieces.PIVOT`.
const SHIPWRECK_PIVOT: [i32; 3] = [4, 0, 15];

/// The four `OceanRuinPieces` template families, `[small, big]` each. The **index
/// into `bricks`/`cracked`/`mossy` is shared** for a cold ruin (one `nextInt`
/// picks the same slot in all three), so these must stay index-aligned.
const OCEAN_RUIN_WARM: [&[&str]; 2] = [
    &[
        "minecraft:underwater_ruin/warm_1",
        "minecraft:underwater_ruin/warm_2",
        "minecraft:underwater_ruin/warm_3",
        "minecraft:underwater_ruin/warm_4",
        "minecraft:underwater_ruin/warm_5",
        "minecraft:underwater_ruin/warm_6",
        "minecraft:underwater_ruin/warm_7",
        "minecraft:underwater_ruin/warm_8",
    ],
    &[
        "minecraft:underwater_ruin/big_warm_4",
        "minecraft:underwater_ruin/big_warm_5",
        "minecraft:underwater_ruin/big_warm_6",
        "minecraft:underwater_ruin/big_warm_7",
    ],
];

const OCEAN_RUIN_BRICK: [&[&str]; 2] = [
    &[
        "minecraft:underwater_ruin/brick_1",
        "minecraft:underwater_ruin/brick_2",
        "minecraft:underwater_ruin/brick_3",
        "minecraft:underwater_ruin/brick_4",
        "minecraft:underwater_ruin/brick_5",
        "minecraft:underwater_ruin/brick_6",
        "minecraft:underwater_ruin/brick_7",
        "minecraft:underwater_ruin/brick_8",
    ],
    &[
        "minecraft:underwater_ruin/big_brick_1",
        "minecraft:underwater_ruin/big_brick_2",
        "minecraft:underwater_ruin/big_brick_3",
        "minecraft:underwater_ruin/big_brick_8",
    ],
];

const OCEAN_RUIN_CRACKED: [&[&str]; 2] = [
    &[
        "minecraft:underwater_ruin/cracked_1",
        "minecraft:underwater_ruin/cracked_2",
        "minecraft:underwater_ruin/cracked_3",
        "minecraft:underwater_ruin/cracked_4",
        "minecraft:underwater_ruin/cracked_5",
        "minecraft:underwater_ruin/cracked_6",
        "minecraft:underwater_ruin/cracked_7",
        "minecraft:underwater_ruin/cracked_8",
    ],
    &[
        "minecraft:underwater_ruin/big_cracked_1",
        "minecraft:underwater_ruin/big_cracked_2",
        "minecraft:underwater_ruin/big_cracked_3",
        "minecraft:underwater_ruin/big_cracked_8",
    ],
];

const OCEAN_RUIN_MOSSY: [&[&str]; 2] = [
    &[
        "minecraft:underwater_ruin/mossy_1",
        "minecraft:underwater_ruin/mossy_2",
        "minecraft:underwater_ruin/mossy_3",
        "minecraft:underwater_ruin/mossy_4",
        "minecraft:underwater_ruin/mossy_5",
        "minecraft:underwater_ruin/mossy_6",
        "minecraft:underwater_ruin/mossy_7",
        "minecraft:underwater_ruin/mossy_8",
    ],
    &[
        "minecraft:underwater_ruin/big_mossy_1",
        "minecraft:underwater_ruin/big_mossy_2",
        "minecraft:underwater_ruin/big_mossy_3",
        "minecraft:underwater_ruin/big_mossy_8",
    ],
];

/// `NetherFossilPieces.FOSSILS`, in declaration order — which is the order the
/// single `nextInt(14)` indexes, so it must not be sorted.
const NETHER_FOSSILS: &[&str] = &[
    "minecraft:nether_fossils/fossil_1",
    "minecraft:nether_fossils/fossil_2",
    "minecraft:nether_fossils/fossil_3",
    "minecraft:nether_fossils/fossil_4",
    "minecraft:nether_fossils/fossil_5",
    "minecraft:nether_fossils/fossil_6",
    "minecraft:nether_fossils/fossil_7",
    "minecraft:nether_fossils/fossil_8",
    "minecraft:nether_fossils/fossil_9",
    "minecraft:nether_fossils/fossil_10",
    "minecraft:nether_fossils/fossil_11",
    "minecraft:nether_fossils/fossil_12",
    "minecraft:nether_fossils/fossil_13",
    "minecraft:nether_fossils/fossil_14",
];

/// `RuinedPortalStructure.STRUCTURE_LOCATION_PORTALS`.
const RUINED_PORTAL_TEMPLATES: [&str; 10] = [
    "minecraft:ruined_portal/portal_1",
    "minecraft:ruined_portal/portal_2",
    "minecraft:ruined_portal/portal_3",
    "minecraft:ruined_portal/portal_4",
    "minecraft:ruined_portal/portal_5",
    "minecraft:ruined_portal/portal_6",
    "minecraft:ruined_portal/portal_7",
    "minecraft:ruined_portal/portal_8",
    "minecraft:ruined_portal/portal_9",
    "minecraft:ruined_portal/portal_10",
];

/// `RuinedPortalStructure.STRUCTURE_LOCATION_GIANT_PORTALS` — the 5%-weighted
/// alternative to [`RUINED_PORTAL_TEMPLATES`].
const RUINED_PORTAL_GIANT_TEMPLATES: [&str; 3] = [
    "minecraft:ruined_portal/giant_portal_1",
    "minecraft:ruined_portal/giant_portal_2",
    "minecraft:ruined_portal/giant_portal_3",
];

/// `IglooPieces`' three templates, with their `PIVOTS` and `OFFSETS`.
const IGLOO_PARTS: [(&str, [i32; 3], [i32; 3]); 3] = [
    ("minecraft:igloo/top", [3, 5, 5], [0, 0, 0]),
    ("minecraft:igloo/middle", [1, 3, 1], [2, -3, 4]),
    ("minecraft:igloo/bottom", [3, 6, 7], [0, -3, -2]),
];

/// The Y every template-driven piece is *first* positioned at, before its own
/// height adjustment — vanilla's `IglooPieces.GENERATION_HEIGHT`, and the same
/// literal 90 in `ShipwreckStructure`, `OceanRuinStructure` and
/// `BuriedTreasureStructure`.
const GENERATION_HEIGHT: i32 = 90;

/// `OceanRuinStructure.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OceanRuinTemperature {
    /// `warm` — sandstone ruins.
    Warm,
    /// `cold` — the three-layer brick/cracked/mossy stack.
    Cold,
}

/// A structure's own generation behaviour.
///
/// Only the variants whose piece generators have landed carry configuration;
/// every other structure `type` in the bundle becomes
/// [`Unsupported`](Self::Unsupported) and is named in the registry's ledger.
#[derive(Debug, Clone)]
pub enum StructureKind {
    /// `minecraft:shipwreck`. Start is `onTopOfChunkCenter` on
    /// `WORLD_SURFACE_WG` when beached, `OCEAN_FLOOR_WG` otherwise, and is
    /// unconditionally valid once the biome passes.
    Shipwreck {
        /// `is_beached`.
        beached: bool,
    },
    /// `minecraft:ocean_ruin`. `onTopOfChunkCenter` on `OCEAN_FLOOR_WG`,
    /// unconditionally valid once the biome passes.
    OceanRuin {
        /// `biome_temp`.
        temperature: OceanRuinTemperature,
        /// `large_probability`.
        large_probability: f32,
        /// `cluster_probability`.
        cluster_probability: f32,
    },
    /// `minecraft:igloo` — one template piece, plus a ladder shaft and basement
    /// half the time.
    Igloo,
    /// `minecraft:buried_treasure` — one coded single-block piece at
    /// `(chunkBlockX(9), 90, chunkBlockZ(9))`.
    BuriedTreasure,
    /// `minecraft:ocean_monument` — needs every biome within 29 blocks of
    /// `(blockX(9), seaLevel, blockZ(9))` to carry
    /// `#minecraft:required_ocean_monument_surrounding`, then
    /// `onTopOfChunkCenter` on `OCEAN_FLOOR_WG`.
    OceanMonument {
        /// The resolved `required_ocean_monument_surrounding` biome set.
        surrounding: HashSet<String>,
    },
    /// `minecraft:jigsaw` — the five villages and `pillager_outpost`
    /// (structure placement's S4). See [`jigsaw`] for the assembly and for what a
    /// [`JigsawConfig`] refuses to model.
    Jigsaw(Box<JigsawConfig>),
    /// `minecraft:swamp_hut` — one coded 7x7x9 piece. Its only
    /// RNG draw is the piece's orientation.
    SwampHut,
    /// `minecraft:desert_pyramid` — one coded 21x15x21 piece plus a cellar and the
    /// `afterPlace` suspicious-sand pass.
    DesertPyramid,
    /// `minecraft:jungle_temple` — one coded 12x10x15 piece, two tripwire traps and
    /// a piston puzzle. Same `SinglePieceStructure` footprint refusal as
    /// [`Self::DesertPyramid`], with a 12x15 footprint instead of 21x21.
    JunglePyramid,
    /// `minecraft:nether_fossil` — one of 14 bone templates, dropped onto the first
    /// solid surface below a sampled height, plus a coin-flip dried ghast.
    ///
    /// The cheapest structure in the corpus by a wide margin (178 Java lines) and the
    /// only remaining `beard_thin` one, so it is the first structure whose own
    /// terrain flattening this engine can observe outside a jigsaw.
    NetherFossil {
        /// The `height` field — `uniform` between `absolute: 32` and `below_top: 2`
        /// in the bundled document. One draw.
        height: jigsaw::HeightProvider,
    },
    /// `minecraft:mineshaft` and `minecraft:mineshaft_mesa` — the first kind whose
    /// pieces are generated **before** the biome filter, because its own generation
    /// point is `moveBelowSeaLevel`'s answer and that is a function of the finished
    /// tree. See [`mineshaft`].
    Mineshaft {
        /// `mineshaft_type`.
        wood: mineshaft::Wood,
        /// The resolved `#minecraft:mineshaft_blocking` biome set —
        /// `isInInvalidLocation`'s first veto, and the reason a deep-dark mineshaft
        /// is a start with no blocks rather than no start.
        blocking: HashSet<String>,
    },
    /// `minecraft:ruined_portal` — the six overworld ids (`ruined_portal` and its
    /// `_desert`/`_jungle`/`_mountain`/`_ocean`/`_swamp` siblings), one weighted
    /// list of [`RuinedPortalSetup`]s each. `ruined_portal_nether` carries this
    /// same `type` id but is refused at parse time (see
    /// [`StructureKind::parse`]) — nothing here can place a Nether-side portal.
    RuinedPortal {
        /// The document's `setups`, in file order — `Setup::weight` is read
        /// relative to their sum, not normalised at parse time, exactly as
        /// vanilla's own weighted pick does.
        setups: Vec<RuinedPortalSetup>,
        /// `#minecraft:features_cannot_replace`, resolved once at parse time
        /// (the piece generator has no `&dyn Resolver` in reach) — every
        /// setup's `ProtectedBlockProcessor` reads the same fixed tag.
        features_cannot_replace: Arc<HashSet<String>>,
    },
    /// `minecraft:stronghold` — the recursive `StrongholdPieces` tree. No
    /// fields: unlike every other kind here its start predicate reads no
    /// biome and no column height, so the document holds nothing this
    /// variant needs to carry. See [`stronghold`].
    Stronghold,
    /// A structure `type` whose generator has not landed. Carries the type id so
    /// the ledger can name it.
    Unsupported(String),
}

/// `RuinedPortalPiece.VerticalPlacement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalPlacement {
    /// `on_land_surface`.
    OnLandSurface,
    /// `partly_buried`.
    PartlyBuried,
    /// `on_ocean_floor`.
    OnOceanFloor,
    /// `in_mountain`.
    InMountain,
    /// `underground`.
    Underground,
    /// `in_nether` — parsed for completeness (so an unrecognised placement
    /// string is a real parse failure and not this one by accident), but no
    /// bundled overworld setup ever names it; the one document that does
    /// (`ruined_portal_nether`) is refused wholesale before a
    /// [`RuinedPortalSetup`] carrying this variant is ever built.
    InNether,
}

impl VerticalPlacement {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "on_land_surface" => Self::OnLandSurface,
            "partly_buried" => Self::PartlyBuried,
            "on_ocean_floor" => Self::OnOceanFloor,
            "in_mountain" => Self::InMountain,
            "underground" => Self::Underground,
            "in_nether" => Self::InNether,
            _ => return None,
        })
    }
}

/// `RuinedPortalStructure.Setup` — one weighted entry of a `ruined_portal*.json`
/// document's `setups` list.
#[derive(Debug, Clone)]
pub struct RuinedPortalSetup {
    placement: VerticalPlacement,
    air_pocket_probability: f32,
    mossiness: f32,
    overgrown: bool,
    vines: bool,
    can_be_cold: bool,
    replace_with_blackstone: bool,
    weight: f32,
}

/// `RuinedPortalPiece.Properties` — resolved once, in `findGenerationPoint`,
/// and carried unchanged into the piece.
#[derive(Debug, Clone, Copy)]
struct RuinedPortalProperties {
    cold: bool,
    mossiness: f32,
    air_pocket: bool,
    overgrown: bool,
    vines: bool,
    replace_with_blackstone: bool,
}

/// Everything `findGenerationPoint` decides before the biome filter — the whole
/// `Structure.GenerationStub`'s payload for this kind, since (unlike jigsaw or
/// nether_fossil) nothing further is drawn once the biome check passes. See
/// [`Stub::RuinedPortal`].
struct RuinedPortalStub {
    position: [i32; 3],
    template_id: &'static str,
    rotation: Rotation,
    mirror: Mirror,
    pivot: [i32; 3],
    placement: VerticalPlacement,
    properties: RuinedPortalProperties,
    features_cannot_replace: Arc<HashSet<String>>,
}

/// `Structure.GenerationStub` — the generation point, plus whatever a kind needs
/// to carry across the biome filter.
///
/// Almost every kind needs nothing: its `findGenerationPoint` draws no RNG, so
/// `generate_pieces` can seed a fresh stream and be exactly vanilla. **Jigsaw is
/// the exception**: its centre rotation and centre element are drawn *before* the
/// biome check and the whole BFS continues from the same stream after it, so the
/// half-consumed random has to travel through.
#[allow(missing_debug_implementations)]
enum Stub {
    /// A position and nothing else.
    Plain([i32; 3]),
    /// A jigsaw centre piece and its live RNG stream.
    Jigsaw(Box<JigsawStub<StructureRandom>>),
    /// A position and a **half-used** random, for a kind whose
    /// `findGenerationPoint` draws before the biome check and whose piece consumer
    /// captures the same stream.
    ///
    /// Jigsaw's own variant is separate because it also carries a placed centre
    /// piece; this one is the plain case, and `nether_fossil` is its first user.
    Continued([i32; 3], Box<StructureRandom>),
    /// **The whole finished piece list**, plus the position it implies.
    ///
    /// Vanilla's `Either.right(builder)` arm: a mineshaft's stub *is* its builder,
    /// so there is nothing left for `generate_pieces` to do but hand the list on.
    /// The cost of the inversion is exactly vanilla's — a mineshaft candidate that
    /// then fails its biome check has already spent its whole stream, and any other
    /// order would be a different world.
    Eager([i32; 3], Vec<StructurePiece>),
    /// A ruined portal's fully-decided construction data. Its own variant rather
    /// than reusing [`Self::Continued`]: nothing draws after the biome check for
    /// this kind (`findGenerationPoint` decides template, rotation, mirror, Y and
    /// every `Properties` field before returning), so there is no live random
    /// stream left to carry.
    RuinedPortal(Box<RuinedPortalStub>),
}

impl Stub {
    /// The point `Structure.isValidBiome` samples.
    fn position(&self) -> [i32; 3] {
        match self {
            Self::Plain(position)
            | Self::Eager(position, _)
            | Self::Continued(position, _) => *position,
            Self::Jigsaw(stub) => stub.position,
            Self::RuinedPortal(stub) => stub.position,
        }
    }
}

impl StructureKind {
    fn parse(value: &Value, resolver: &dyn Resolver) -> Self {
        match value["type"].as_str().unwrap_or_default() {
            "minecraft:shipwreck" => Self::Shipwreck {
                beached: value["is_beached"].as_bool().unwrap_or(false),
            },
            "minecraft:ocean_ruin" => Self::OceanRuin {
                temperature: match value["biome_temp"].as_str() {
                    Some("cold") => OceanRuinTemperature::Cold,
                    _ => OceanRuinTemperature::Warm,
                },
                large_probability: value["large_probability"].as_f64().unwrap_or(0.0) as f32,
                cluster_probability: value["cluster_probability"].as_f64().unwrap_or(0.0) as f32,
            },
            "minecraft:igloo" => Self::Igloo,
            "minecraft:swamp_hut" => Self::SwampHut,
            "minecraft:desert_pyramid" => Self::DesertPyramid,
            "minecraft:jungle_temple" => Self::JunglePyramid,
            "minecraft:nether_fossil" => match jigsaw::HeightProvider::parse(&value["height"]) {
                Some(height) => Self::NetherFossil { height },
                // A `height` shape nobody bundles would silently place every fossil
                // at y=0, so it is refused and named instead.
                None => Self::Unsupported(
                    "minecraft:nether_fossil — unsupported `height` provider".to_string(),
                ),
            },
            "minecraft:mineshaft" => Self::Mineshaft {
                wood: match value["mineshaft_type"].as_str() {
                    Some("mesa") => mineshaft::Wood::Mesa,
                    _ => mineshaft::Wood::Normal,
                },
                blocking: resolve_biome_set(
                    resolver,
                    &Value::String("#minecraft:mineshaft_blocking".to_string()),
                ),
            },
            "minecraft:stronghold" => Self::Stronghold,
            "minecraft:buried_treasure" => Self::BuriedTreasure,
            "minecraft:ocean_monument" => Self::OceanMonument {
                surrounding: resolve_biome_set(
                    resolver,
                    &Value::String(
                        "#minecraft:required_ocean_monument_surrounding".to_string(),
                    ),
                ),
            },
            "minecraft:jigsaw" => match JigsawConfig::parse(value) {
                Ok(config) => Self::Jigsaw(Box::new(config)),
                // The reason travels to the ledger through the type id, which is
                // what `StructureRegistry::new` records — a jigsaw structure whose
                // config we refuse must say *why*, not just "jigsaw".
                Err(why) => Self::Unsupported(format!("minecraft:jigsaw — {why}")),
            },
            "minecraft:ruined_portal" => {
                let setups = parse_ruined_portal_setups(value);
                // `ruined_portal_nether` carries this exact `type` id and its one
                // setup is `placement: in_nether` — refused wholesale rather than
                // silently mis-Y'd, because `findSuitableY`'s `IN_NETHER` branch,
                // the Nether's own sea level and its beard/step wiring are all
                // unverified here (see the module doc's "out of scope" note).
                if setups.is_empty()
                    || setups.iter().any(|s| s.placement == VerticalPlacement::InNether)
                {
                    Self::Unsupported(
                        "minecraft:ruined_portal — in_nether placement is out of scope".to_string(),
                    )
                } else {
                    let mut features_cannot_replace = HashSet::new();
                    let mut seen = HashSet::new();
                    crate::compose::resolve_block_tag(
                        resolver,
                        "minecraft:features_cannot_replace",
                        &mut features_cannot_replace,
                        &mut seen,
                    );
                    Self::RuinedPortal {
                        setups,
                        features_cannot_replace: Arc::new(features_cannot_replace),
                    }
                }
            }
            other => Self::Unsupported(other.to_string()),
        }
    }

    /// Every template this kind can name, for eager loading into a
    /// [`TemplateStore`]. Empty for a kind that places no template.
    fn template_ids(&self) -> Vec<&'static str> {
        match self {
            Self::Shipwreck { beached } => {
                if *beached {
                    SHIPWRECK_BEACHED.to_vec()
                } else {
                    SHIPWRECK_OCEAN.to_vec()
                }
            }
            Self::OceanRuin { temperature, .. } => {
                let families: &[[&[&str]; 2]] = match temperature {
                    OceanRuinTemperature::Warm => &[OCEAN_RUIN_WARM],
                    OceanRuinTemperature::Cold => {
                        &[OCEAN_RUIN_BRICK, OCEAN_RUIN_CRACKED, OCEAN_RUIN_MOSSY]
                    }
                };
                families
                    .iter()
                    .flat_map(|family| family.iter().flat_map(|list| list.iter().copied()))
                    .collect()
            }
            Self::Igloo => IGLOO_PARTS.iter().map(|(id, _, _)| *id).collect(),
            Self::NetherFossil { .. } => NETHER_FOSSILS.to_vec(),
            // Every one of the 13 portal templates is a candidate at every
            // start (the pick is a single `nextInt` over the whole family), so
            // all 13 are loaded eagerly regardless of which setups this id
            // actually carries.
            Self::RuinedPortal { .. } => RUINED_PORTAL_TEMPLATES
                .iter()
                .chain(RUINED_PORTAL_GIANT_TEMPLATES.iter())
                .copied()
                .collect(),
            // A jigsaw structure's templates are named by its *pools*, and there
            // are hundreds of them — `PoolStore::load` pulls each one in as it
            // parses the element that names it, so there is no static list here.
            Self::Jigsaw(_)
            | Self::BuriedTreasure
            | Self::OceanMonument { .. }
            | Self::SwampHut
            | Self::DesertPyramid
            | Self::JunglePyramid
            | Self::Mineshaft { .. }
            | Self::Stronghold
            | Self::Unsupported(_) => Vec::new(),
        }
    }

    /// `Structure.findGenerationPoint`'s *position* — the point the biome filter
    /// is applied at. Draws no RNG for any kind here (all of them are
    /// `onTopOfChunkCenter`-shaped), which is what lets the piece list be built
    /// afterwards, in [`Self::generate_pieces`], exactly as vanilla's lazy
    /// `GenerationStub` does.
    ///
    /// Returns `None` where vanilla returns `Optional.empty()` — no start at all,
    /// before any biome check.
    fn find_stub(
        &self,
        cx: i32,
        cz: i32,
        seed: i64,
        ctx: &dyn StartContext,
        pools: &PoolStore,
        templates: &TemplateStore,
    ) -> Option<Stub> {
        if let Self::RuinedPortal { setups, features_cannot_replace } = self {
            // The third kind whose generation point costs RNG, and — unlike
            // jigsaw and nether_fossil — the *last* one: every draw
            // `RuinedPortalStructure.findGenerationPoint` makes (setup, air
            // pocket, template, rotation, mirror, Y) happens here, so nothing is
            // left to draw once the biome check passes. See [`RuinedPortalStub`].
            let mut random = structure_random(seed, cx, cz);
            let setup = pick_ruined_portal_setup(setups, &mut random);
            let air_pocket = ruined_portal_sample(&mut random, setup.air_pocket_probability);
            let giant = random.next_float() < 0.05;
            let template_id = if giant {
                RUINED_PORTAL_GIANT_TEMPLATES[random.next_int_bounded(3).clamp(0, 2) as usize]
            } else {
                RUINED_PORTAL_TEMPLATES[random.next_int_bounded(10).clamp(0, 9) as usize]
            };
            let template = templates.get(template_id)?;
            let rotation = Rotation::random(&mut random);
            let mirror = if random.next_float() < 0.5 { Mirror::None } else { Mirror::FrontBack };
            let size = template.size();
            let pivot = [size[0] / 2, 0, size[2] / 2];
            let (base_x, base_z) = (cx * 16, cz * 16);
            let box_settings = PlaceSettings {
                rotation,
                mirror,
                pivot,
                processors: Vec::new(),
                waterlogging: true,
            };
            // `template.getBoundingBox(basePosition, rotation, pivot, mirror)` at
            // the chunk's own Y=0 — the box's Y span does not depend on the
            // translation, only its X/Z corners do, and those are what
            // `findSuitableY` samples columns under.
            let box_ = template.bounding_box([base_x, 0, base_z], &box_settings);
            // `BoundingBox.getCenter()`: `min + (max - min + 1) / 2`, not the
            // arithmetic mean — see the type's own doc.
            let center_x = box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2;
            let center_z = box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2;
            let heightmap = if setup.placement == VerticalPlacement::OnOceanFloor {
                HeightmapKind::OceanFloorWg
            } else {
                HeightmapKind::WorldSurfaceWg
            };
            // `chunkGenerator.getBaseHeight(...) - 1`: `first_occupied_height` is
            // already `getBaseHeight - 1`, so no further offset here — the same
            // convention `find_generation_point`'s other arms already use.
            let surface_y = ctx.first_occupied_height(center_x, center_z, heightmap);
            let y_span = size[1];
            let projected_y = ruined_portal_find_suitable_y(
                &mut random,
                setup.placement,
                air_pocket,
                surface_y,
                y_span,
                box_,
                ctx,
            );
            let origin = [base_x, projected_y, base_z];
            let cold = setup.can_be_cold
                && is_cold_biome(&ctx.biome_at_quart(origin[0] >> 2, origin[1] >> 2, origin[2] >> 2));
            return Some(Stub::RuinedPortal(Box::new(RuinedPortalStub {
                position: origin,
                template_id,
                rotation,
                mirror,
                pivot,
                placement: setup.placement,
                properties: RuinedPortalProperties {
                    cold,
                    mossiness: setup.mossiness,
                    air_pocket,
                    overgrown: setup.overgrown,
                    vines: setup.vines,
                    replace_with_blackstone: setup.replace_with_blackstone,
                },
                features_cannot_replace: Arc::clone(features_cannot_replace),
            })));
        }
        if let Self::Jigsaw(config) = self {
            // The one kind whose generation point costs RNG. See [`Stub`].
            return jigsaw::begin(
                config,
                pools,
                cx,
                cz,
                ctx.min_y(),
                ctx.dimension_height(),
                seed,
                ctx,
                structure_random(seed, cx, cz),
            )
            .map(|stub| Stub::Jigsaw(Box::new(stub)));
        }
        if let Self::NetherFossil { height } = self {
            // `NetherFossilStructure.findGenerationPoint`: two `nextInt(16)`s, the
            // `height` sample, then a **draw-free** downward walk. The random then
            // travels on, because `addPieces` captures it.
            let mut random = structure_random(seed, cx, cz);
            let x = cx * 16 + random.next_int_bounded(16);
            let z = cz * 16 + random.next_int_bounded(16);
            let mut y = height.sample(&mut random, ctx.min_y(), ctx.dimension_height());
            let sea_level = ctx.sea_level();
            // The walk reads `column.getBlock(y)` for air and `getBlock(--y)` for
            // "soul sand, **or** face-sturdy". Pre-surface those two are one test:
            // soul sand is a *surface-rule* product, so every solid block here is
            // `BlockKind::Stone` and every `Stone` is face-sturdy. The disjunction is
            // therefore exactly satisfied by the shape — the one place in this engine
            // where the four-way kind loses nothing.
            while y > sea_level {
                let current_is_air = ctx.block_kind_at(x, y, z) == BlockKind::Air;
                y -= 1;
                if current_is_air && ctx.block_kind_at(x, y, z) == BlockKind::Stone {
                    break;
                }
            }
            if y <= sea_level {
                return None;
            }
            return Some(Stub::Continued([x, y, z], Box::new(random)));
        }
        if let Self::Mineshaft { wood, blocking } = self {
            // The other kind whose generation point costs RNG, and the only one
            // whose generation point costs *pieces*.
            let mut random = structure_random(seed, cx, cz);
            let (pieces, position) =
                mineshaft::generate(cx, cz, ctx, *wood, blocking, &mut random);
            return Some(Stub::Eager(position, pieces));
        }
        self.find_generation_point(cx, cz, ctx).map(Stub::Plain)
    }

    /// The draw-free half of `findGenerationPoint`, for every kind except
    /// [`Self::Jigsaw`].
    fn find_generation_point(&self, cx: i32, cz: i32, ctx: &dyn StartContext) -> Option<[i32; 3]> {
        // `ChunkPos.getMiddleBlockX/Z` — `getBlockX(8)`.
        let middle_x = cx * 16 + 8;
        let middle_z = cz * 16 + 8;
        match self {
            Self::Shipwreck { beached } => {
                let heightmap = if *beached {
                    HeightmapKind::WorldSurfaceWg
                } else {
                    HeightmapKind::OceanFloorWg
                };
                let y = ctx.first_occupied_height(middle_x, middle_z, heightmap);
                Some([middle_x, y, middle_z])
            }
            Self::OceanRuin { .. } | Self::BuriedTreasure => {
                let y = ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::OceanFloorWg);
                Some([middle_x, y, middle_z])
            }
            Self::Igloo | Self::SwampHut => {
                let y =
                    ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::WorldSurfaceWg);
                Some([middle_x, y, middle_z])
            }
            Self::DesertPyramid | Self::JunglePyramid => {
                // `SinglePieceStructure.findGenerationPoint`: refuse outright when
                // the *lowest* of the four corner heights of the (width x depth)
                // footprint is below sea level, then `onTopOfChunkCenter`. The
                // corners are sampled at `(minX, minZ)` + `(0|w, 0|d)` — from the
                // chunk's **min** corner, not its middle, and against
                // `WORLD_SURFACE_WG` even though the structure sits on land.
                //
                // The footprint is the *structure*'s `(width, depth)` pair, which
                // for the jungle temple is `(12, 15)` and not the pyramid's
                // `(21, 21)`: sharing this arm without parameterising it would
                // silently refuse jungle temples on any 12x15-flat-but-21x21-broken
                // site.
                let (width, depth) = if matches!(self, Self::JunglePyramid) {
                    (coded::JUNGLE_WIDTH, coded::JUNGLE_DEPTH)
                } else {
                    (coded::PYRAMID_WIDTH, coded::PYRAMID_DEPTH)
                };
                let (min_x, min_z) = (cx * 16, cz * 16);
                let lowest = [
                    (min_x, min_z),
                    (min_x, min_z + depth),
                    (min_x + width, min_z),
                    (min_x + width, min_z + depth),
                ]
                .into_iter()
                .map(|(x, z)| ctx.first_occupied_height(x, z, HeightmapKind::WorldSurfaceWg))
                .min()
                .unwrap_or(i32::MIN);
                if lowest < ctx.sea_level() {
                    return None;
                }
                let y =
                    ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::WorldSurfaceWg);
                Some([middle_x, y, middle_z])
            }
            Self::OceanMonument { surrounding } => {
                let ox = cx * 16 + 9;
                let oz = cz * 16 + 9;
                if !biomes_within_all_in(ctx, ox, ctx.sea_level(), oz, 29, surrounding) {
                    return None;
                }
                let y = ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::OceanFloorWg);
                Some([middle_x, y, middle_z])
            }
            // `context.chunkPos().getWorldPosition()` — the chunk's own
            // north-west corner at Y=0, unmoved by any RNG.
            // `StrongholdStructure.findGenerationPoint` does not sample a
            // column or a biome the way every other kind here does; the real
            // work (`generatePieces`) is entirely inside the lazy consumer,
            // which is why [`Self::Stronghold`] needs no [`Stub`] arm of its
            // own — `generate_pieces` below does the whole job.
            Self::Stronghold => Some([cx * 16, 0, cz * 16]),
            // Handled by `find_stub` before this function is reached.
            Self::Jigsaw(_)
            | Self::Mineshaft { .. }
            | Self::NetherFossil { .. }
            | Self::RuinedPortal { .. } => None,
            Self::Unsupported(_) => {
                // No generator, so no honest generation point — and therefore no
                // honest biome-check Y either. Sea level is used deliberately
                // rather than a sampled column height: the answer is **advisory**
                // (the structure is on the ledger and its start carries
                // `pieces_complete: false`), and sampling a real column here would
                // buy nothing but would make the placement sweep build an
                // `AquiferSystem` on the ~25% of chunks `nether_fossils`' spacing-2
                // grid nominates. Where the real generation point sits in a
                // different biome than sea level does, this start is a false
                // positive — named, not hidden.
                Some([middle_x, ctx.sea_level(), middle_z])
            }
        }
    }

    /// `Structure.GenerationStub`'s piece generator — run **after** the biome
    /// filter, exactly as vanilla runs `getPiecesBuilder()` only for a start whose
    /// biome check passed. `None` means "this engine has no generator", which is
    /// what [`StructureStart::pieces_complete`] reports as `false`.
    ///
    /// # Height adjustment happens here, not at placement time
    ///
    /// Vanilla positions every template piece at Y=90 and fixes it up inside
    /// `postProcess`, mutating the *shared* `StructureStart` the first time any
    /// chunk places it. That is unavailable to us: our chunks are generated
    /// independently and memoised, so a piece whose Y depended on which chunk got
    /// there first would shear a shipwreck along a chunk border. We instead
    /// resolve it once, here, against the same `_WG` noise columns vanilla's own
    /// too-big-to-fit branch uses (`Structure.getLowestY` /
    /// `getMeanFirstOccupiedHeight`). Two consequences, both deliberate:
    ///
    /// * the heights come from a fresh noise column rather than from the placed
    ///   chunk's stored `_WG` heightmap, so a surface rule that raises a column
    ///   (snow on a beach) is not seen — sub-block, and the only alternative is a
    ///   stage cycle;
    /// * the beached shipwreck's `random.nextInt(3)` sink comes out of the
    ///   structure's own per-chunk stream instead of the decoration stream. That
    ///   stream is per-structure-per-chunk and nothing else reads it after this
    ///   call, so no other structure's draws move.
    fn generate_pieces(
        &self,
        stub: Stub,
        cx: i32,
        cz: i32,
        seed: i64,
        ctx: &dyn StartContext,
        templates: &TemplateStore,
        pools: &PoolStore,
    ) -> Option<Vec<StructurePiece>> {
        if let Self::Jigsaw(config) = self {
            // Taken **by value**: `finish` consumes the half-used random, and a
            // copy of it would restart the stream and build a different village.
            let Stub::Jigsaw(stub) = stub else {
                return None;
            };
            return Some(jigsaw::finish(*stub, config, pools, ctx));
        }
        // The eager arm is checked on the *stub*, not on the kind, because the stub
        // is what carries the answer — and matching on the kind first would leave a
        // future eager kind silently returning `None`.
        if let Stub::Eager(_, pieces) = stub {
            return Some(pieces);
        }
        if let Self::NetherFossil { .. } = self {
            // Taken by value for the same reason jigsaw's is: `addPieces` continues
            // the stream `findGenerationPoint` left half-used, and a copy would
            // restart it and pick a different fossil.
            let Stub::Continued(position, mut random) = stub else {
                return None;
            };
            return Some(nether_fossil_pieces(
                position,
                seed,
                ctx,
                templates,
                &mut *random,
            ));
        }
        if let Self::RuinedPortal { .. } = self {
            let Stub::RuinedPortal(stub) = stub else {
                return None;
            };
            return Some(ruined_portal_piece(*stub, templates));
        }
        match self {
            Self::Shipwreck { beached } => {
                let mut random = structure_random(seed, cx, cz);
                Some(shipwreck_pieces(*beached, cx, cz, ctx, templates, &mut random))
            }
            Self::OceanRuin {
                temperature,
                large_probability,
                cluster_probability,
            } => {
                let mut random = structure_random(seed, cx, cz);
                Some(ocean_ruin_pieces(
                    *temperature,
                    *large_probability,
                    *cluster_probability,
                    cx,
                    cz,
                    ctx,
                    templates,
                    &mut random,
                ))
            }
            Self::Igloo => {
                let mut random = structure_random(seed, cx, cz);
                Some(igloo_pieces(cx, cz, ctx, templates, &mut random))
            }
            Self::SwampHut => {
                let mut random = structure_random(seed, cx, cz);
                Some(coded::swamp_hut_pieces(cx, cz, ctx, &mut random))
            }
            Self::DesertPyramid => {
                let mut random = structure_random(seed, cx, cz);
                Some(coded::desert_pyramid_pieces(cx, cz, ctx, &mut random))
            }
            Self::JunglePyramid => {
                let mut random = structure_random(seed, cx, cz);
                Some(coded::jungle_pyramid_pieces(cx, cz, ctx, &mut random))
            }
            Self::BuriedTreasure => {
                // The piece's own position is `getBlockX(9)`, not the chunk
                // middle the biome check uses, and its Y is the literal 90 from
                // `BuriedTreasureStructure.generatePieces`. Vanilla's persisted
                // box is the *post-placement* one (`postProcess` reassigns
                // `boundingBox` after walking down to bedrock-ish stone), so a
                // freshly generated start and a reloaded one legitimately differ
                // in Y — see `docs/structures.md`.
                let px = cx * 16 + 9;
                let pz = cz * 16 + 9;
                Some(vec![StructurePiece {
                    id: "minecraft:btp".to_string(),
                    bounding_box: BoundingBox::of_block(px, GENERATION_HEIGHT, pz),
                    orientation: None,
                    gen_depth: 0,
                    template: None,
                    placement: None,
                    extra_placements: Vec::new(),
                    // `BuriedTreasurePiece.postProcess` walks down to stone and
                    // places one chest plus up to five *neighbour* blocks, and the
                    // walk's terminating condition is a **material** distinction
                    // (sandstone/stone/andesite/granite/diorite) `StartContext`'s
                    // pre-surface shape cannot make. Deferred to placement time
                    // instead of resolved here — see [`PieceRefinement::BuriedTreasureChest`].
                    blocks: None,
                    loot: Vec::new(),
                    beard: None,
                    refine: Some(PieceRefinement::BuriedTreasureChest),
                }])
            }
            Self::Stronghold => Some(stronghold::generate(cx, cz, seed, ctx)),
            Self::OceanMonument { .. } => Some(monument::generate(cx, cz, seed, ctx)),
            Self::Unsupported(_) => None,
            // Handled above, before the match, because they consume `stub`.
            Self::Jigsaw(_)
            | Self::Mineshaft { .. }
            | Self::NetherFossil { .. }
            | Self::RuinedPortal { .. } => None,
        }
    }

    /// Whether a start of this kind, having passed its biome check, is valid.
    /// `Unknown` for the kinds whose generators have not landed.
    fn validity(&self, pieces: &Option<Vec<StructurePiece>>) -> Validity {
        match self {
            Self::Unsupported(_) => Validity::Unknown,
            // Every template-driven kind adds at least one piece, so biome-valid
            // implies start-valid — but an empty list means a template failed to
            // load, which is `Unknown` (named on the ledger), not `Invalid`.
            // `RuinedPortalStructure.findGenerationPoint` always returns
            // `Optional.of(...)`, exactly like the three above — but its own
            // template lookup already ran inside `find_stub` (it needs the
            // template's size before the biome check, to find a suitable Y), so
            // an empty list here can only mean that lookup failed on a
            // template this store nonetheless reports as loaded, which is the
            // same "ledgered, not invalid" shape as the others.
            Self::Shipwreck { .. }
            | Self::OceanRuin { .. }
            | Self::Igloo
            | Self::NetherFossil { .. }
            | Self::RuinedPortal { .. } => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Unknown,
            },
            // A coded piece needs no template, so an empty list here means its
            // ground-height rule found no column — vanilla's own `false` from
            // `updateAverageGroundHeight`, which produces a start with no blocks
            // rather than no start. `Invalid` is the honest answer.
            Self::SwampHut | Self::DesertPyramid | Self::JunglePyramid => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Invalid,
            },
            // A jigsaw start always has at least its centre piece — `addPieces`
            // returns `Optional.empty()` rather than an empty builder when the
            // centre cannot be placed, and that path is `find_stub` returning
            // `None`, before the biome check. So an empty list here means a pool
            // failed to load, which is ledgered rather than treated as invalid.
            Self::Jigsaw(_) => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Unknown,
            },
            Self::BuriedTreasure => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Invalid,
            },
            // A mineshaft's builder always holds at least its room, and vanilla
            // wraps it in `Optional.of` unconditionally — there is no invalid
            // mineshaft. An empty list here would mean the room itself failed to
            // build, which cannot happen, so `Unknown` (ledgered) is the honest
            // answer rather than `Invalid`.
            Self::Mineshaft { .. } => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Unknown,
            },
            // `MonumentBuilding`'s constructor is unconditional — there is no
            // failure path that produces an empty `childPieces`, so this is
            // the same shape as `Mineshaft`/`Stronghold`: `Unknown` (ledgered)
            // rather than `Invalid` if the generator itself somehow ran and
            // produced nothing.
            Self::OceanMonument { .. } => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Unknown,
            },
            // `stronghold::generate`'s retry loop never returns until a
            // portal room has been placed, so — like a mineshaft — there is
            // no invalid stronghold. `Unknown` rather than `Invalid` for the
            // same reason: an empty list here would mean the generator
            // itself never ran, which the ledger should name.
            Self::Stronghold => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Unknown,
            },
        }
    }
}

/// `level.getHeight(heightmap, x, z)` — the first **free** Y, one above the
/// topmost matching block.
///
/// [`StartContext::first_occupied_height`] is vanilla's `getFirstOccupiedHeight`
/// (`getBaseHeight - 1`), so the two differ by exactly one. Every piece generator
/// below transcribes `postProcess` code that calls `getHeight`, so mixing the two
/// up would sink every template one block into the ground.
pub(crate) fn free_height(ctx: &dyn StartContext, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
    ctx.first_occupied_height(x, z, heightmap) + 1
}

/// `RuinedPortalStructure.findGenerationPoint`'s weighted setup pick: **one**
/// `nextFloat()` regardless of how many setups there are (not one per
/// candidate), matching `RuinedPortalStructure`'s own loop — a single-setup id
/// (`_desert`, `_jungle`, `_ocean`, `_swamp`) draws nothing here at all.
fn pick_ruined_portal_setup<'a>(
    setups: &'a [RuinedPortalSetup],
    random: &mut StructureRandom,
) -> &'a RuinedPortalSetup {
    if setups.len() <= 1 {
        return &setups[0];
    }
    let total: f32 = setups.iter().map(|s| s.weight).sum();
    let mut pick = random.next_float();
    for setup in setups {
        pick -= setup.weight / total;
        if pick < 0.0 {
            return setup;
        }
    }
    // Vanilla throws `IllegalStateException` here; both bundled multi-setup
    // ids (`ruined_portal`, `ruined_portal_mountain`) weight their two setups
    // 0.5/0.5, so the loop above always returns before falling off the end.
    setups.last().unwrap_or(&setups[0])
}

/// `RuinedPortalStructure.sample` — a probability of exactly `0.0` or `1.0`
/// costs **no** draw; anything in between is one `nextFloat()`. Getting this
/// wrong would shift every draw after it for the two setups whose
/// `air_pocket_probability` is not one of the two extremes
/// (`ruined_portal_mountain`'s `on_land_surface` arm is `0.5`).
fn ruined_portal_sample(random: &mut StructureRandom, probability: f32) -> bool {
    if probability == 0.0 {
        false
    } else if probability == 1.0 {
        true
    } else {
        random.next_float() < probability
    }
}

/// The vanilla biomes whose *base* temperature is below the `0.15`
/// `warmEnoughToRain` threshold — `Biome::coldEnoughToSnow`'s answer wherever
/// the height-adjustment term is inert (`pos.y <= seaLevel + 17`, true for
/// every ruined-portal placement except a rare `in_mountain` one). Approximates
/// `coldEnoughToSnow` by biome id rather than by the real per-biome
/// `ClimateSettings`/noise model, which this crate has no access to through
/// [`StartContext`]. The only observable effect of a wrong answer is which of
/// two lava-replacement rules a `cold`-eligible setup's portal gets
/// (netherrack unconditionally, or magma 20% of the time) — a decayed-block
/// cosmetic, not the structure's position, orientation or piece list.
const COLD_ENOUGH_TO_SNOW_BIOMES: &[&str] = &[
    "minecraft:snowy_plains",
    "minecraft:snowy_taiga",
    "minecraft:snowy_slopes",
    "minecraft:snowy_beach",
    "minecraft:ice_spikes",
    "minecraft:frozen_peaks",
    "minecraft:jagged_peaks",
    "minecraft:grove",
    "minecraft:frozen_river",
    "minecraft:frozen_ocean",
    "minecraft:deep_frozen_ocean",
];

fn is_cold_biome(id: &str) -> bool {
    COLD_ENOUGH_TO_SNOW_BIOMES.contains(&id)
}

/// `RuinedPortalStructure.findSuitableY` — for `ON_LAND_SURFACE`/`ON_OCEAN_FLOOR`
/// this is `surfaceYAtCenter` outright; every other overworld placement seeds
/// `newY` from one draw, then walks it down from there until at least three of
/// the box's four bottom corners sit on ground the placement's own heightmap
/// calls opaque, or the walk runs out at `minY + 15`.
fn ruined_portal_find_suitable_y(
    random: &mut StructureRandom,
    placement: VerticalPlacement,
    air_pocket: bool,
    surface_y_at_center: i32,
    y_span: i32,
    box_: BoundingBox,
    ctx: &dyn StartContext,
) -> i32 {
    let min_y = ctx.min_y() + 15;
    let mut y = match placement {
        VerticalPlacement::InNether => {
            // Unreachable through this engine's registry — `StructureKind::parse`
            // refuses every `in_nether` setup — but transcribed so the match is
            // total and the branch reads the same as vanilla's.
            if air_pocket {
                next_int_between(random, 32, 100)
            } else if random.next_float() < 0.5 {
                next_int_between(random, 27, 29)
            } else {
                next_int_between(random, 29, 100)
            }
        }
        VerticalPlacement::InMountain => {
            random_within_interval(random, 70, surface_y_at_center - y_span)
        }
        VerticalPlacement::Underground => {
            random_within_interval(random, min_y, surface_y_at_center - y_span)
        }
        VerticalPlacement::PartlyBuried => {
            surface_y_at_center - y_span + next_int_between(random, 2, 8)
        }
        VerticalPlacement::OnLandSurface | VerticalPlacement::OnOceanFloor => surface_y_at_center,
    };
    let corners = [
        [box_.min[0], box_.min[2]],
        [box_.max[0], box_.min[2]],
        [box_.min[0], box_.max[2]],
        [box_.max[0], box_.max[2]],
    ];
    let is_opaque = |x: i32, at_y: i32, z: i32| {
        let kind = ctx.block_kind_at(x, at_y, z);
        if placement == VerticalPlacement::OnOceanFloor {
            kind == BlockKind::Stone
        } else {
            kind != BlockKind::Air
        }
    };
    while y > min_y {
        let solid = corners.iter().filter(|c| is_opaque(c[0], y, c[1])).count();
        if solid >= 3 {
            return y;
        }
        y -= 1;
    }
    y
}

/// `Mth.randomBetweenInclusive(random, minPreferred, max)` when
/// `minPreferred < max`, else `max` — `getRandomWithinInterval`.
fn random_within_interval(random: &mut StructureRandom, min_preferred: i32, max: i32) -> i32 {
    if min_preferred < max {
        next_int_between(random, min_preferred, max)
    } else {
        max
    }
}

/// `Util.getRandom(array, random)`.
fn pick<'a, R: RandomSource>(list: &[&'a str], random: &mut R) -> &'a str {
    let index = random.next_int_bounded(i32::try_from(list.len()).unwrap_or(1));
    list[usize::try_from(index).unwrap_or(0).min(list.len() - 1)]
}

/// `Mth.nextInt(random, min, max)` — inclusive both ends.
fn next_int_between<R: RandomSource>(random: &mut R, min: i32, max: i32) -> i32 {
    random.next_int_bounded(max - min + 1) + min
}

/// `ShipwreckStructure.generatePieces` + `ShipwreckPiece.postProcess`'s height
/// adjustment.
fn shipwreck_pieces<R: RandomSource>(
    beached: bool,
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    random: &mut R,
) -> Vec<StructurePiece> {
    let rotation = Rotation::random(random);
    let list = if beached { SHIPWRECK_BEACHED } else { SHIPWRECK_OCEAN };
    let name = pick(list, random);
    let Some(template) = templates.get(name) else {
        return Vec::new();
    };
    let settings = PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: SHIPWRECK_PIVOT,
        processors: vec![Processor::structure_and_air()],
        waterlogging: true,
    };
    let size = template.size();
    let heightmap = if beached {
        HeightmapKind::WorldSurfaceWg
    } else {
        HeightmapKind::OceanFloorWg
    };
    // Vanilla scans the **unrotated** footprint from `templatePosition`, which is
    // the chunk's min corner — not the rotated bounding box. Transcribed as-is.
    let (base_x, base_z) = (cx * 16, cz * 16);
    let mut sum = 0i32;
    let mut lowest = i32::MAX;
    let area = size[0] * size[2];
    for x in base_x..(base_x + size[0].max(1)) {
        for z in base_z..(base_z + size[2].max(1)) {
            let h = free_height(ctx, x, z, heightmap);
            sum += h;
            lowest = lowest.min(h);
        }
    }
    let y = if area == 0 {
        free_height(ctx, base_x, base_z, heightmap)
    } else if beached {
        // `calculateBeachedPosition`.
        lowest - size[1] / 2 - random.next_int_bounded(3)
    } else {
        sum / area
    };
    let position = [base_x, y, base_z];
    vec![template_piece("minecraft:shipwreck", name, template, position, settings)]
}

/// `NetherFossilPieces.addPieces` plus `NetherFossilPiece.placeDriedGhast`.
///
/// # Two draws, then a fork
///
/// `Rotation.getRandom` **then** `Util.getRandom(FOSSILS, random)` — the rotation is
/// a local assigned before the constructor call, so it is drawn first even though it
/// is the last argument. Swapping them picks a different fossil at every seed.
///
/// The dried ghast draws from a **positional fork of the world seed** at the fossil
/// box centre, not from the structure's stream, so it costs the stream nothing and is
/// a pure function of `(seed, box)`.
///
/// # Why the ghast is a coded block on a template piece
///
/// Vanilla's air test runs against the world *after* the template placed, and it is
/// the one read this engine cannot make at start time. It does not have to:
/// `structure_place_stage` writes `blocks` **before** `placement`, so the ghast is
/// laid down and then overwritten wherever the template has a block of its own —
/// which is exactly the set of positions vanilla's `isAir()` would have rejected.
/// What is left to test here is the *terrain* being air, which
/// [`StartContext::block_kind_at`] answers.
fn nether_fossil_pieces<R: RandomSource>(
    position: [i32; 3],
    seed: i64,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    random: &mut R,
) -> Vec<StructurePiece> {
    let rotation = Rotation::random(random);
    let name = pick(NETHER_FOSSILS, random);
    let Some(template) = templates.get(name) else {
        return Vec::new();
    };
    let settings = PlaceSettings {
        rotation,
        mirror: Mirror::None,
        // `TemplateStructurePiece`'s settings set no pivot, so it is `BlockPos.ZERO`.
        pivot: [0, 0, 0],
        processors: vec![Processor::structure_and_air()],
        waterlogging: true,
    };
    let mut piece = template_piece("minecraft:nefos", name, template, position, settings);
    let box_ = piece.bounding_box;
    // `RandomSource.createThreadLocalInstance(level.getSeed()).forkPositional().at(
    //  fossilBB.getCenter())`, and `getCenter` is `min + (max - min + 1) / 2`.
    let centre = [
        box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2,
        box_.min[1] + (box_.max[1] - box_.min[1] + 1) / 2,
        box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2,
    ];
    let mut ghast = LegacyRandomSource::new(seed)
        .fork_positional()
        .at(centre[0], centre[1], centre[2]);
    if ghast.next_float() < 0.5 {
        let x = box_.min[0] + ghast.next_int_bounded(box_.max[0] - box_.min[0] + 1);
        let y = box_.min[1];
        let z = box_.min[2] + ghast.next_int_bounded(box_.max[2] - box_.min[2] + 1);
        // The `nextInt`s are spent whether or not the position turns out to be air,
        // so the terrain test comes after them.
        if ctx.block_kind_at(x, y, z) == BlockKind::Air {
            // `DRIED_GHAST.defaultBlockState().rotate(Rotation.getRandom(positional))`
            // — a *third* draw from the same fork, and it is the block's own rotation
            // rather than the piece's.
            let facing_rotation = Rotation::random(&mut ghast);
            let state = template::BlockState::parse(
                "minecraft:dried_ghast[facing=north,hydration=0,waterlogged=false]",
            )
            .rotate(facing_rotation);
            piece.blocks = Some(Arc::new(vec![CodedBlock {
                pos: [x, y, z],
                state: state.canonical(),
            }]));
        }
    }
    vec![piece]
}

/// `RuinedPortalStructure.findGenerationPoint`'s `builder -> { ... }` closure —
/// the one piece this kind ever builds, from data [`Self::find_stub`] already
/// decided. Nothing here draws random.
///
/// # What this places, and what it deliberately does not
///
/// The template placement itself is complete: every `RuleProcessor` rule
/// (`gold_block` → 30% air, the lava swap, the non-cold `netherrack` → 7%
/// magma), [`Processor::BlockAge`]'s decay, [`Processor::ProtectedBlocks`],
/// [`Processor::LavaSubmerged`] and (when a setup asks for it)
/// [`Processor::BlackstoneReplace`] all run, in `makeSettings`'s own order.
///
/// **`RuinedPortalPiece.postProcess`'s *second* half is not ported**:
/// `spreadNetherrack` (the netherrack skirt a portal sits in), `plusNetherrackDripColumnsBelowPortal`
/// and the vine/leaf overgrowth pass. Those read the world *as the template just
/// wrote it* at positions well outside the piece's own box — this engine's other
/// coded-piece deviations (see `coded:decoration_random` on the ledger) already
/// establish the pattern for porting a postProcess pass onto an eager start-time
/// builder, but doing it faithfully needs the template's own placed cells
/// available for a box-interior overlap test, which no piece here computes until
/// placement time. Ledgered as `coded:ruined_portal_terrain_skirt` rather than
/// guessed at. The frame's position, orientation, materials and decay are real;
/// only the environmental blending around it is missing.
fn ruined_portal_piece(stub: RuinedPortalStub, templates: &TemplateStore) -> Vec<StructurePiece> {
    let Some(template) = templates.get(stub.template_id) else {
        return Vec::new();
    };
    let ignore = if stub.properties.air_pocket {
        Processor::structure_block()
    } else {
        Processor::structure_and_air()
    };
    let mut rules = vec![
        rule_random_replace("minecraft:gold_block", 0.3, "minecraft:air"),
        ruined_portal_lava_rule(stub.placement, stub.properties.cold),
    ];
    if !stub.properties.cold {
        rules.push(rule_random_replace("minecraft:netherrack", 0.07, "minecraft:magma_block"));
    }
    let mut processors = vec![
        ignore,
        Processor::Rule(rules),
        Processor::BlockAge { mossiness: stub.properties.mossiness },
        Processor::ProtectedBlocks(Arc::clone(&stub.features_cannot_replace)),
        Processor::LavaSubmerged,
    ];
    if stub.properties.replace_with_blackstone {
        processors.push(Processor::BlackstoneReplace);
    }
    let settings = PlaceSettings {
        rotation: stub.rotation,
        mirror: stub.mirror,
        pivot: stub.pivot,
        processors,
        waterlogging: true,
    };
    vec![template_piece(
        "minecraft:ruined_portal",
        stub.template_id,
        template,
        stub.position,
        settings,
    )]
}

/// `getBlockReplaceRule(source, probability, target)` —
/// `RandomBlockMatchTest(source, probability)` against `AlwaysTrue`.
fn rule_random_replace(source: &str, probability: f32, target: &str) -> ProcessorRule {
    ProcessorRule {
        input: RuleTest::RandomBlockMatch(source.to_string(), probability),
        location: RuleTest::AlwaysTrue,
        position: PosTest::AlwaysTrue,
        output: BlockState::of(target),
    }
}

/// `getBlockReplaceRule(source, target)` — the unconditional two-argument
/// overload, `BlockMatchTest` against `AlwaysTrue`.
fn rule_replace(source: &str, target: &str) -> ProcessorRule {
    ProcessorRule {
        input: RuleTest::BlockMatch(source.to_string()),
        location: RuleTest::AlwaysTrue,
        position: PosTest::AlwaysTrue,
        output: BlockState::of(target),
    }
}

/// `RuinedPortalPiece.getLavaProcessorRule` — `on_ocean_floor` unconditionally
/// swaps lava for magma; every other placement swaps to netherrack when the
/// setup came out cold, or rolls a 20% magma chance when it did not.
fn ruined_portal_lava_rule(placement: VerticalPlacement, cold: bool) -> ProcessorRule {
    if placement == VerticalPlacement::OnOceanFloor {
        rule_replace("minecraft:lava", "minecraft:magma_block")
    } else if cold {
        rule_replace("minecraft:lava", "minecraft:netherrack")
    } else {
        rule_random_replace("minecraft:lava", 0.2, "minecraft:magma_block")
    }
}

/// `OceanRuinStructure.generatePieces` → `OceanRuinPieces.addPieces`.
#[allow(clippy::too_many_arguments)]
fn ocean_ruin_pieces<R: RandomSource>(
    temperature: OceanRuinTemperature,
    large_probability: f32,
    cluster_probability: f32,
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    random: &mut R,
) -> Vec<StructurePiece> {
    let origin = [cx * 16, GENERATION_HEIGHT, cz * 16];
    let rotation = Rotation::random(random);
    let mut pieces = Vec::new();
    let is_large = random.next_float() <= large_probability;
    let base_integrity = if is_large { 0.9 } else { 0.8 };
    ocean_ruin_add_piece(
        temperature,
        origin,
        rotation,
        is_large,
        base_integrity,
        ctx,
        templates,
        random,
        &mut pieces,
    );
    if is_large && random.next_float() <= cluster_probability {
        // `addClusterRuins`.
        let parent_corner = {
            let c = template::transform([15, 0, 15], Mirror::None, rotation, [0, 0, 0]);
            [c[0] + origin[0], origin[1], c[2] + origin[2]]
        };
        let parent_box = BoundingBox::from_corners(origin, parent_corner);
        let bottom_left = [
            origin[0].min(parent_corner[0]),
            origin[1],
            origin[2].min(parent_corner[2]),
        ];
        let mut candidates = cluster_positions(bottom_left, random);
        let count = next_int_between(random, 4, 8);
        for _ in 0..count {
            if candidates.is_empty() {
                continue;
            }
            let index = random.next_int_bounded(i32::try_from(candidates.len()).unwrap_or(1));
            let pos = candidates.remove(usize::try_from(index).unwrap_or(0));
            let next_rotation = Rotation::random(random);
            let corner = {
                let c = template::transform([5, 0, 6], Mirror::None, next_rotation, [0, 0, 0]);
                [c[0] + pos[0], pos[1], c[2] + pos[2]]
            };
            if BoundingBox::from_corners(pos, corner).intersects(parent_box) {
                continue;
            }
            ocean_ruin_add_piece(
                temperature,
                pos,
                next_rotation,
                false,
                0.8,
                ctx,
                templates,
                random,
                &mut pieces,
            );
        }
    }
    pieces
}

/// `OceanRuinPieces.allPositions` — eight offsets, sixteen draws, in this order.
fn cluster_positions<R: RandomSource>(origin: [i32; 3], random: &mut R) -> Vec<[i32; 3]> {
    // `(x base, x range, z base, z range)` per candidate, in vanilla's order. Java
    // evaluates the x argument before the z argument, so each row is two draws in
    // that order and the row order is the draw order.
    const CANDIDATES: [(i32, (i32, i32), i32, (i32, i32)); 8] = [
        (-16, (1, 8), 16, (1, 7)),
        (-16, (1, 8), 0, (1, 7)),
        (-16, (1, 8), -16, (4, 8)),
        (0, (1, 7), 16, (1, 7)),
        (0, (1, 7), -16, (4, 6)),
        (16, (1, 7), 16, (3, 8)),
        (16, (1, 7), 0, (1, 7)),
        (16, (1, 7), -16, (4, 8)),
    ];
    CANDIDATES
        .iter()
        .map(|&(x_base, (x_min, x_max), z_base, (z_min, z_max))| {
            let dx = x_base + next_int_between(random, x_min, x_max);
            let dz = z_base + next_int_between(random, z_min, z_max);
            [origin[0] + dx, origin[1], origin[2] + dz]
        })
        .collect()
}

/// `OceanRuinPieces.addPiece` — one warm piece, or the cold three-layer stack
/// (which shares one template index across brick/cracked/mossy).
#[allow(clippy::too_many_arguments)]
fn ocean_ruin_add_piece<R: RandomSource>(
    temperature: OceanRuinTemperature,
    position: [i32; 3],
    rotation: Rotation,
    is_large: bool,
    base_integrity: f32,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    random: &mut R,
    out: &mut Vec<StructurePiece>,
) {
    let slot = usize::from(is_large);
    match temperature {
        OceanRuinTemperature::Warm => {
            let name = pick(OCEAN_RUIN_WARM[slot], random);
            ocean_ruin_push(
                name,
                temperature,
                position,
                rotation,
                base_integrity,
                ctx,
                templates,
                out,
            );
        }
        OceanRuinTemperature::Cold => {
            let bricks = OCEAN_RUIN_BRICK[slot];
            let index = usize::try_from(random.next_int_bounded(i32::try_from(bricks.len()).unwrap_or(1)))
                .unwrap_or(0);
            for (family, integrity) in [
                (OCEAN_RUIN_BRICK[slot], base_integrity),
                (OCEAN_RUIN_CRACKED[slot], 0.7),
                (OCEAN_RUIN_MOSSY[slot], 0.5),
            ] {
                let Some(name) = family.get(index) else { continue };
                ocean_ruin_push(
                    name,
                    temperature,
                    position,
                    rotation,
                    integrity,
                    ctx,
                    templates,
                    out,
                );
            }
        }
    }
}

/// `OceanRuinPieces.archyRuleProcessor(candidate, replacement, lootTable)` — the
/// coded `CappedProcessor` that turns exactly five of a ruin's sand or gravel
/// blocks into suspicious ones.
///
/// A *coded* processor, not one of the 40 `processor_list` documents, which is why
/// it is spelled out here rather than resolved from data. `ConstantInt.of(5)`
/// draws nothing; the five positions come from the shuffled index walk over the
/// piece's already-rotted block list.
fn ocean_ruin_archaeology(temperature: OceanRuinTemperature) -> Processor {
    let (candidate, replacement) = match temperature {
        OceanRuinTemperature::Warm => ("minecraft:sand", "minecraft:suspicious_sand[dusted=0]"),
        OceanRuinTemperature::Cold => ("minecraft:gravel", "minecraft:suspicious_gravel[dusted=0]"),
    };
    Processor::Capped {
        delegate: Box::new(Processor::Rule(vec![processor::ProcessorRule {
            input: processor::RuleTest::BlockMatch(candidate.to_string()),
            location: processor::RuleTest::AlwaysTrue,
            position: processor::PosTest::AlwaysTrue,
            // `replacementBlock.defaultBlockState()` — `BrushableBlock`'s `dusted`
            // defaults to 0, and the property has to be spelled out because the
            // state field's canonical form carries every property.
            output: template::BlockState::parse(replacement),
        }])),
        limit: 5,
    }
}

fn ocean_ruin_push(
    name: &str,
    temperature: OceanRuinTemperature,
    position: [i32; 3],
    rotation: Rotation,
    integrity: f32,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    out: &mut Vec<StructurePiece>,
) {
    let Some(template) = templates.get(name) else {
        return;
    };
    let settings = PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: [0, 0, 0],
        // `BlockRotProcessor` first, then `STRUCTURE_AND_AIR`, then the capped
        // archaeology rule: a rotted-away block is dropped before the ignore
        // list ever sees it, and the capped walk runs over what survives both.
        // Its order matters twice over — being last is what makes its index walk
        // range over the *rotted* list, so a ruin with integrity 0.5 has half as
        // many candidate positions as one with 1.0.
        processors: vec![
            Processor::BlockRot {
                rottable: None,
                integrity,
            },
            Processor::structure_and_air(),
            ocean_ruin_archaeology(temperature),
        ],
        waterlogging: true,
    };
    let position = ocean_ruin_position(template, position, &settings, ctx);
    out.push(template_piece("minecraft:orp", name, template, position, settings));
}

/// `OceanRuinPiece.postProcess`'s two-step height fix: sit on the ocean floor,
/// then sink to the floor's own minimum when the footprint is mostly overhanging.
fn ocean_ruin_position(
    template: &StructureTemplate,
    position: [i32; 3],
    settings: &PlaceSettings,
    ctx: &dyn StartContext,
) -> [i32; 3] {
    let floor = free_height(ctx, position[0], position[2], HeightmapKind::OceanFloorWg);
    let size = template.size();
    let corner = template::transform([size[0] - 1, 0, size[2] - 1], Mirror::None, settings.rotation, [0, 0, 0]);
    let corner = [corner[0] + position[0], floor, corner[2] + position[2]];
    let (x0, x1) = (position[0].min(corner[0]), position[0].max(corner[0]));
    let (z0, z1) = (position[2].min(corner[2]), position[2].max(corner[2]));
    // `getHeight`: for each column, walk down from `floor - 1` while the block is
    // air, water or ice. Against a `_WG` column that is exactly
    // `min(floor - 1, first_occupied(OCEAN_FLOOR_WG))` — there is no ice in a
    // pre-surface column, and everything above the ocean floor is water or air.
    let top = floor - 1;
    let mut min_floor = 512;
    let mut overhanging = 0;
    for x in x0..=x1 {
        for z in z0..=z1 {
            let column = ctx
                .first_occupied_height(x, z, HeightmapKind::OceanFloorWg)
                .min(top);
            min_floor = min_floor.min(column);
            if column < top - 2 {
                overhanging += 1;
            }
        }
    }
    let width = (position[0] - corner[0]).abs();
    let y = if top - min_floor > 2 && overhanging > width - 2 {
        min_floor + 1
    } else {
        floor
    };
    [position[0], y, position[2]]
}

/// `IglooStructure.generatePieces` → `IglooPieces.addPieces`, with
/// `IglooPiece.postProcess`'s entrance-column height fix folded in.
fn igloo_pieces<R: RandomSource>(
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    templates: &TemplateStore,
    random: &mut R,
) -> Vec<StructurePiece> {
    let start = [cx * 16, GENERATION_HEIGHT, cz * 16];
    let rotation = Rotation::random(random);
    let mut out = Vec::new();
    let push = |part: usize, depth: i32, out: &mut Vec<StructurePiece>| {
        let (name, pivot, offset) = IGLOO_PARTS[part];
        let Some(template) = templates.get(name) else {
            return;
        };
        let settings = PlaceSettings {
            rotation,
            mirror: Mirror::None,
            pivot,
            processors: vec![Processor::structure_block()],
            // `LiquidSettings.IGNORE_WATERLOGGING`.
            waterlogging: false,
        };
        let position = [
            start[0] + offset[0],
            start[1] + offset[1] - depth,
            start[2] + offset[2],
        ];
        // The entrance column: the same world column for all three parts, by
        // construction (each part's offset cancels against its own probe).
        let entrance = template::transform([3 - offset[0], 0, -offset[2]], Mirror::None, rotation, pivot);
        let surface = free_height(
            ctx,
            position[0] + entrance[0],
            position[2] + entrance[2],
            HeightmapKind::WorldSurfaceWg,
        );
        let position = [
            position[0],
            position[1] + surface - GENERATION_HEIGHT - 1,
            position[2],
        ];
        out.push(template_piece("minecraft:iglu", name, template, position, settings));
    };
    if random.next_double() < 0.5 {
        let depth = random.next_int_bounded(8) + 4;
        push(2, depth * 3, &mut out);
        for i in 0..(depth - 1) {
            push(1, i * 3, &mut out);
        }
    }
    push(0, 0, &mut out);
    out
}

/// Builds the piece record for a template placed at `position`.
///
/// `orientation` is `Some(2)` because `TemplateStructurePiece`'s constructor calls
/// `setOrientation(Direction.NORTH)` and `Direction.NORTH.get2DDataValue()` is 2 —
/// the piece's *rotation* lives in its place settings, not in `O`.
fn template_piece(
    id: &str,
    name: &str,
    template: &Arc<StructureTemplate>,
    position: [i32; 3],
    settings: PlaceSettings,
) -> StructurePiece {
    StructurePiece {
        id: id.to_string(),
        bounding_box: template.bounding_box(position, &settings),
        orientation: Some(2),
        gen_depth: 0,
        template: Some(name.to_string()),
        placement: Some(Arc::new(PiecePlacement {
            template: Arc::clone(template),
            position,
            settings,
        })),
        // Only a `list_pool_element` (S4) needs more than one.
        extra_placements: Vec::new(),
        blocks: None,
        // A template piece's loot lives in the template's own bytes, which
        // `lodestone_server::structure_loot` re-reads; this list is the coded-piece
        // channel only.
        loot: Vec::new(),
        // Not a jigsaw piece. Every kind `template_piece` serves is
        // `terrain_adaptation: none`, so this is doubly inert today — but it is
        // the field S4's pool pieces fill, and leaving it out of the constructor
        // would make that a wider change than it needs to be.
        beard: None,
        refine: None,
    }
}

/// `getBiomesWithin(x, y, z, r)` ⊆ `allowed`, without materialising the set.
fn biomes_within_all_in(
    ctx: &dyn StartContext,
    x: i32,
    y: i32,
    z: i32,
    r: i32,
    allowed: &HashSet<String>,
) -> bool {
    // `QuartPos.fromBlock` is `>> 2`, an arithmetic shift — not a divide, which
    // would round the negative half of the world toward zero.
    let (x0, y0, z0) = ((x - r) >> 2, (y - r) >> 2, (z - r) >> 2);
    let (x1, y1, z1) = ((x + r) >> 2, (y + r) >> 2, (z + r) >> 2);
    for qz in z0..=z1 {
        for qx in x0..=x1 {
            for qy in y0..=y1 {
                if !allowed.contains(&ctx.biome_at_quart(qx, qy, qz)) {
                    return false;
                }
            }
        }
    }
    true
}

/// One parsed structure document.
#[derive(Debug, Clone)]
pub struct StructureDef {
    /// The structure id.
    pub id: String,
    /// Its generation behaviour.
    pub kind: StructureKind,
    /// The resolved closure of its `biomes` holder-set.
    pub biomes: HashSet<String>,
    /// `terrain_adaptation`.
    pub terrain_adaptation: TerrainAdjustment,
    /// `step` (`surface_structures`, `underground_structures`, …). Carried for
    /// the eventual placement pass; nothing here reads it.
    pub step: String,
}

/// One parsed structure set.
#[derive(Debug, Clone)]
pub struct StructureSetDef {
    /// The set id, e.g. `minecraft:shipwrecks`.
    pub id: String,
    /// Its placement.
    pub placement: Placement,
    /// `(structure id, weight)` in document order — the order
    /// `createStructures`' weighted walk consumes.
    pub entries: Vec<(String, i32)>,
}

/// The structure engine for one seed.
#[allow(missing_debug_implementations)]
pub struct StructureRegistry {
    seed: i64,
    sets: Vec<StructureSetDef>,
    set_index: HashMap<String, usize>,
    structures: HashMap<String, StructureDef>,
    templates: TemplateStore,
    pools: PoolStore,
    unsupported: BTreeMap<String, String>,
}

impl StructureRegistry {
    /// Builds the registry for `seed` from `resolver`'s structure documents, with
    /// **no dimension filter** — every structure set the resolver serves is kept.
    ///
    /// An empty [`Resolver::structure_set_ids`] yields an empty registry that
    /// places nothing, which is what every fixture resolver in this workspace
    /// gets and why none of them had to change.
    ///
    /// See [`Self::new_for_biomes`] for the filtered form and for why the
    /// Overworld deliberately stays on this one.
    #[must_use]
    pub fn new(seed: i64, resolver: &dyn Resolver) -> Self {
        Self::new_for_biomes(seed, resolver, None)
    }

    /// [`Self::new`] plus vanilla's `hasBiomesForStructureSet` filter: a set is
    /// kept only when at least one of its structures' resolved `biomes` closures
    /// intersects `possible_biomes`, the dimension's own
    /// `BiomeSource.possibleBiomes()`.
    ///
    /// # Why this exists, and why it is not merely an optimisation
    ///
    /// `ChunkGeneratorStructureState.createForNormal` filters the *whole
    /// structure-set registry* through `hasBiomesForStructureSet` before
    /// `createStructures` ever walks it
    /// (`ChunkGeneratorStructureState.java:52-67`), and `possibleStructureSets()`
    /// is what that walk iterates. So a Nether generator handed the full 20-set
    /// bundle is not "vanilla plus some wasted predicates": it is a generator
    /// whose `structure_starts` can report an Overworld set's id, and whose
    /// [`Self::unsupported`] ledger names blockers for structures that dimension
    /// could never have placed. Both of those are answers to a question nobody
    /// asked, which is how a ledger row becomes noise.
    ///
    /// **It cannot change which chunk gets which structure, in either
    /// direction**, and that is worth stating because it looks like it could.
    /// [`Self::starts_at`] re-seeds its weighted walk per set
    /// (`set_large_feature_seed(seed, cx, cz)`), so dropping a set shifts no other
    /// set's stream; and a dropped set's own structures would have been rejected
    /// by [`Self::try_start`]'s biome filter anyway. What it does change is cost —
    /// a filtered Nether registry loads `bastion`'s pools and nothing else,
    /// instead of every village, `ancient_city` and `trial_chambers` pool graph.
    ///
    /// **`None` means "no filter", and that is what the Overworld passes.** Its
    /// `possibleBiomes()` is the whole 7,594-row parameter table's biome set, so
    /// filtering there would drop exactly the Nether and End sets and change
    /// nothing else — but it would also change [`Self::unsupported`]'s keys for a
    /// generator whose gates pin them, for no behavioural gain. The asymmetry is
    /// deliberate and is the reason this is a second constructor rather than a
    /// changed signature.
    #[must_use]
    pub fn new_for_biomes(
        seed: i64,
        resolver: &dyn Resolver,
        possible_biomes: Option<&HashSet<String>>,
    ) -> Self {
        let ids = resolver.structure_set_ids();
        let mut sets: Vec<StructureSetDef> = Vec::with_capacity(ids.len());
        let mut structures: HashMap<String, StructureDef> = HashMap::new();
        let mut templates = TemplateStore::default();
        let mut pools = PoolStore::default();
        let mut unsupported: BTreeMap<String, String> = BTreeMap::new();
        // One resolved `biomes` closure per structure id, shared between the
        // filter probe below and the parse loop that follows it. Without this the
        // filtered path resolves every candidate's biome-tag closure twice.
        let mut biome_sets: HashMap<String, HashSet<String>> = HashMap::new();

        for set_id in ids {
            let document = resolver.structure_set(&set_id);
            if document.is_null() {
                unsupported.insert(set_id.clone(), "structure set document missing".into());
                continue;
            }
            let entries: Vec<(String, i32)> = document["structures"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| {
                            Some((
                                e["structure"].as_str()?.to_string(),
                                e["weight"].as_i64().unwrap_or(1) as i32,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();

            // `hasBiomesForStructureSet`, before `Placement::parse` and before any
            // pool or template is touched — a set this dimension cannot place must
            // cost nothing and must leave no ledger row, exactly as vanilla drops
            // it from `possibleStructureSets` before `generatePositions` runs.
            if let Some(possible) = possible_biomes {
                let mut reachable = false;
                for (structure_id, _) in &entries {
                    let doc = resolver.structure(structure_id);
                    if doc.is_null() {
                        continue;
                    }
                    let biomes = biome_sets
                        .entry(structure_id.clone())
                        .or_insert_with(|| resolve_biome_set(resolver, &doc["biomes"]));
                    if biomes.iter().any(|b| possible.contains(b)) {
                        reachable = true;
                        break;
                    }
                }
                if !reachable {
                    continue;
                }
            }

            let placement = Placement::parse(&document["placement"]);
            if let PlacementKind::Unsupported(kind) = &placement.kind {
                unsupported.insert(set_id.clone(), format!("placement type '{kind}'"));
            }

            for (structure_id, _) in &entries {
                if structures.contains_key(structure_id) {
                    continue;
                }
                let doc = resolver.structure(structure_id);
                if doc.is_null() {
                    unsupported
                        .insert(structure_id.clone(), "structure document missing".into());
                    continue;
                }
                let mut kind = StructureKind::parse(&doc, resolver);
                // A jigsaw structure's whole pool graph is pulled in here, once,
                // for the same reason the template store is eager: a start
                // predicate runs inside the chunk pipeline with no `&dyn Resolver`
                // in reach. A pool (or one of its templates, or one of its
                // processors) that will not load demotes the structure — the ledger
                // says which one and why, instead of a village of nothing.
                if let StructureKind::Jigsaw(config) = &kind {
                    let aliases = pool::AliasedPools {
                        names: config.alias_names(),
                        targets: config.alias_targets(),
                    };
                    if let Err(why) =
                        pools.load(resolver, &mut templates, &config.start_pool, &aliases)
                    {
                        unsupported.insert(structure_id.clone(), why);
                        kind = StructureKind::Unsupported("minecraft:jigsaw".to_string());
                    }
                }
                // A template-driven kind whose templates are not all loadable
                // cannot place anything, so it is demoted rather than left to
                // produce empty piece lists at generation time.
                let failures = templates.load(resolver, &kind.template_ids());
                if !failures.is_empty() {
                    let (name, why) = &failures[0];
                    unsupported.insert(
                        structure_id.clone(),
                        format!("template '{name}' unusable ({why}), {} in total", failures.len()),
                    );
                    kind = StructureKind::Unsupported(
                        doc["type"].as_str().unwrap_or("unknown").to_string(),
                    );
                } else if let StructureKind::Unsupported(type_id) = &kind {
                    // `or_insert`, not `insert`: a demotion above this point
                    // already recorded *which* pool, template or processor was the
                    // blocker, and overwriting that with the generic "no piece
                    // generator for type 'minecraft:jigsaw'" is how a precise
                    // ledger entry becomes a useless one.
                    let reason = match type_id.split_once(" — ") {
                        Some((type_id, why)) => {
                            format!("no generator for type '{type_id}': {why}")
                        }
                        None => format!("no piece generator for type '{type_id}'"),
                    };
                    unsupported.entry(structure_id.clone()).or_insert(reason);
                }
                let biomes = biome_sets
                    .remove(structure_id)
                    .unwrap_or_else(|| resolve_biome_set(resolver, &doc["biomes"]));
                structures.insert(
                    structure_id.clone(),
                    StructureDef {
                        id: structure_id.clone(),
                        kind,
                        biomes,
                        terrain_adaptation: TerrainAdjustment::parse(&doc["terrain_adaptation"]),
                        step: doc["step"].as_str().unwrap_or("surface_structures").to_string(),
                    },
                );
            }

            sets.push(StructureSetDef {
                id: set_id,
                placement,
                entries,
            });
        }

        sets.sort_by_key(|s| {
            BOOTSTRAP_ORDER
                .iter()
                .position(|k| *k == s.id)
                .unwrap_or(BOOTSTRAP_ORDER.len())
        });
        let set_index = sets
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();

        // Gaps in the template engine itself, rather than in a structure's
        // generator: recorded once, keyed so they cannot be mistaken for a
        // structure id (the placement oracle asserts implemented structures are
        // *absent* from this map).
        if !templates.is_empty() {
            // **These two rows overstated the gap for five phases and were corrected
            // in S6 by measurement.** Both said "needs block entities and loot tables
            // in worldgen"; both machineries already exist. `lodestone-worldgen` has a
            // block-entity layer (`overworld::block_entities`) and
            // `lodestone-server` has a loot roller plus `structure_loot`, which
            // re-reads a template piece's raw bytes, finds its DATA markers, rolls and
            // attaches a filled container. So the *marker* path is closed and the real
            // gaps are narrower and different. Recording the correction here rather
            // than deleting the rows, because a row that describes the wrong gap is
            // worse than no row: it makes the right one invisible.
            unsupported.insert(
                "block_entity:append_loot".into(),
                "a `capped` archaeology rule places its `suspicious_sand`/\
                 `suspicious_gravel` block (ocean ruins, trail ruins, desert pyramid) \
                 and its `append_loot` table is bundled \
                 (`assets/loot_table/archaeology/`), but **nothing in the game brushes**: \
                 there is no `brushable_block` block entity and no brush interaction, so \
                 the blocker is gameplay-side and not in worldgen at all"
                    .into(),
            );
            unsupported.insert(
                "template:block_entity_nbt".into(),
                "**132 bundled templates** carry a chest/barrel/dispenser/decorated pot \
                 whose `LootTable` lives in that *block's own* `nbt` compound (village \
                 62, bastion 26, trial_chambers 19, ruined_portal 13, ancient_city 10, \
                 pillager_outpost 2) — a different mechanism from a `structure_block` \
                 DATA marker, and the one `lodestone_server::structure_loot` does not \
                 read. The blocks are placed; the containers are empty. Replaced \
                 `template:data_markers`, which named the three structures \
                 (shipwreck, igloo, ocean ruin) whose markers **do** get rolled"
                    .into(),
            );
            // `template:mirrored_shape` is **gone**, and its absence is the record:
            // it said a rail `shape` was not remapped under a mirror, which was true
            // for as long as nothing placed a rail. A mineshaft corridor places one
            // and its EAST/WEST orientations carry a real `CLOCKWISE_90` rotation, so
            // `BlockState::{rotate, mirror}` grew `BaseRailBlock`'s two tables and the
            // gap closed. Deleting a row whose gap has closed is the point of having
            // rows; a stale one hides the real remainder.
            // S5's own gaps. Each is a *deviation* rather than an absence, which is
            // exactly the kind of thing that disappears from the record if it is not
            // written down in the same place as the absences.
            unsupported.insert(
                "coded:average_ground_height".into(),
                "`swamp_hut`'s Y averages the heightmap over the **whole** piece box, \
                 not over its intersection with the decorating chunk: vanilla's own \
                 answer is chunk-order dependent, so there is no single value to \
                 reproduce — see docs/worldgen-structure-coded.md"
                    .into(),
            );
            unsupported.insert(
                "coded:region_random".into(),
                "`desert_pyramid`'s cellar variant and collapsed-roof rolls come from \
                 `level.getRandom()` in vanilla — the decorating region's stream, so \
                 chunk-order dependent. Position-seeded here, like every processor draw"
                    .into(),
            );
            unsupported.insert(
                "coded:pyramid_roof_seed".into(),
                "`randomCollapsedRoofPos` and `afterPlace`'s shuffle fork the **world** \
                 seed positionally, and the piece generator is three layers below the \
                 start predicate that holds it: a fixed fork seed is used, so those two \
                 picks are position-dependent but seed-independent"
                    .into(),
            );
            unsupported.insert(
                "coded:worldgen_entities".into(),
                "`swamp_hut`'s witch and cat are not spawned, a mineshaft corridor's \
                 chest **minecart** is not spawned (its rail is placed and its loot \
                 table plus vanilla's `nextLong()` roll seed travel on \
                 `StructurePiece::loot`, so only the entity is missing), a spider \
                 corridor's `spawner` block is placed with no `SpawnData`, the \
                 stronghold portal room's silverfish `spawner` is the same gap, and \
                 the ocean monument's penthouse and design-0 wing room each skip an \
                 elder guardian `spawnElder` call — nothing in worldgen can spawn an \
                 entity or build a spawner's payload yet"
                    .into(),
            );
            unsupported.insert(
                "monument:postprocess_random_unseeded".into(),
                "vanilla's `postProcess` random (`ChunkGenerator.applyBiomeDecoration`) \
                 is seeded from `RandomSupport.generateUniqueSeed()` — real entropy, \
                 unrelated to the world seed — so `OceanMonumentSimpleRoom`'s \
                 `centerPillar` coin flip and `OceanMonumentSimpleTopRoom`'s sponge \
                 scatter have no vanilla answer a fixed seed could reproduce. \
                 `structure::monument` continues the same seeded `structure_random` \
                 stream construction already used for the room graph, rather than \
                 inventing an unseeded one, so the engine stays a pure function of \
                 `(seed, chunk)` — the same shape as `coded:decoration_random`"
                    .into(),
            );
            unsupported.insert(
                "stronghold:skip_air_shell".into(),
                "every `StrongholdPieces` shell call passes `skipAir = true`, meaning \
                 vanilla only overwrites a block that is not already air — read from \
                 the real terrain a stronghold is dug into, since `postProcess` runs \
                 after NOISE/SURFACE in vanilla's own pipeline. `stronghold::generate` \
                 resolves every piece's blocks eagerly at start time, before any \
                 terrain exists to read, so the predicate has nothing to consult and \
                 every write is unconditional — the same shape as `coded:region_random`"
                    .into(),
            );
            unsupported.insert(
                "mineshaft:post_process_scope".into(),
                "vanilla runs a mineshaft piece's `postProcess` once **per decorating \
                 chunk** and clips every read and write to that chunk, so \
                 `isInInvalidLocation`'s liquid survey, `hasSturdyNeighbours` and every \
                 `getBlock` see only part of the piece and a corridor spanning two \
                 chunks draws its cobwebs twice from two unrelated streams. Resolved \
                 eagerly here, once, over the whole box — a deviation with no single \
                 vanilla answer to reproduce, the same class as \
                 `coded:average_ground_height`"
                    .into(),
            );
            unsupported.insert(
                "mineshaft:pre_surface_world_reads".into(),
                "six mineshaft helpers branch on what the world already holds \
                 (`canBeReplaced`, `isSupportingBox`, `placeSupportPillar`, \
                 `setPlanksBlock`, `placeDoubleLowerOrUpperSupport`, \
                 `fillPillarDownOrChainUp`). They read the eager overlay plus \
                 `StartContext::block_kind_at`, which is the raw `_WG` shape: **every \
                 solid block is one `Stone`**, so a surface rule's sand or an ore blob's \
                 granite is invisible and a carver's cave is not. `isFaceSturdy` is a \
                 table over the eight states a mineshaft writes rather than a solidity \
                 model"
                    .into(),
            );
            // S6's corrected form of the S5 row. The chest **blocks** are placed now
            // and their tables travel on [`StructurePiece::loot`] with vanilla's own
            // `nextLong()` seed; what is missing is a *consumer*, on the server side of
            // the seam, next to `structure_loot`'s existing DATA-marker pass.
            unsupported.insert(
                "coded:chests".into(),
                "a coded piece's containers (`desert_pyramid` ×4, `jungle_temple` \
                 2 chests + 2 dispensers) place their **block** and carry their loot \
                 table and roll seed on `StructurePiece::loot`, but nothing reads that \
                 list yet: `lodestone_server::structure_loot` resolves loot from a \
                 template's raw bytes and a coded piece has no template. So a coded \
                 chest opens empty"
                    .into(),
            );
            unsupported.insert(
                "coded:chest_reorient".into(),
                "`StructurePiece.reorient` picks a chest's `facing` from the \
                 render-solidity of its four horizontal neighbours *in the world as \
                 written so far*; `StartContext` has no block-state read and this crate \
                 has no solidity table, so a coded chest keeps `facing=north`. Cosmetic, \
                 and the only coded-piece property that is knowingly not vanilla's"
                    .into(),
            );
            unsupported.insert(
                "coded:decoration_random".into(),
                "`postProcess`'s `random` is the **decorating chunk's** feature stream, \
                 so vanilla's own answer is chunk-order dependent — `jungle_temple`'s \
                 ~5,600 `MossStoneSelector` draws and every `createChest`/\
                 `createDispenser` seed. They come out of the structure's own per-chunk \
                 stream here, in vanilla's order and count, which makes the piece a pure \
                 function of `(seed, chunk)`"
                    .into(),
            );
            unsupported.insert(
                "coded:ruined_portal_terrain_skirt".into(),
                "`RuinedPortalPiece.postProcess`'s *second* half is not ported: \
                 `spreadNetherrack`, `addNetherrackDripColumnsBelowPortal` and the \
                 vine/leaf overgrowth pass. The template placement itself (frame \
                 position, orientation, decay, mossiness, the blackstone swap) is \
                 real; what is missing is the netherrack the portal would otherwise \
                 sit in and spill down from, plus vine/leaf growth on its own \
                 blocks. A faithful port needs the template's own placed cells \
                 available for `canBlockBeReplacedByNetherrackOrMagma`'s \
                 box-interior overlap test, which no piece here computes before \
                 placement time — see `ruined_portal_piece`'s own doc"
                    .into(),
            );
            unsupported.insert(
                "coded:buried_treasure_chest".into(),
                "`buried_treasure` places a start and a bounding box but **zero blocks**. \
                 `BuriedTreasurePieces.postProcess` walks a cursor down from the ocean \
                 floor until the block *below* it is \
                 sandstone/stone/andesite/granite/diorite, then writes five neighbours \
                 and one chest. Not merely a missing `BlockKind` read: the sand it \
                 burrows through is a **surface-rule** product and the granite/diorite/\
                 andesite are **ore-blob** products, so the answer only exists at a stage \
                 the eager start pass sits above. In the pre-surface `_WG` column every \
                 solid block is `BlockKind::Stone`, so the walk would stop on its first \
                 iteration and put the chest on top of the beach instead of under it — \
                 which is why it places nothing rather than something wrong"
                    .into(),
            );
        }
        // Reachability, not mechanism — and the one class of row that looks like no
        // row is needed, because every *other* instrument says these are fine.
        //
        // **The composition half of this row is closed.** `NetherGenerator` now runs
        // starts / refs / beardifier / place, so `bastion_remnant` writes real blocks
        // into a real Nether column. What is left is one dimension-shaped gap and
        // three per-structure ones, and the row is kept (rather than deleted) because
        // the per-structure rows cannot say the dimension-level thing: a reader asking
        // "can I walk into a bastion" needs to know that no chunk source serves this
        // dimension yet.
        if !structures.is_empty() {
            unsupported.insert(
                "dimension:nether_structures".into(),
                "`NetherGenerator` composes a structure stage now, so \
                 `bastion_remnant` assembles **and places blocks** in a generated \
                 Nether column, and so does `nether_fossil` — the only structure \
                 whose `beard_thin` terrain flattening is now observable outside a \
                 jigsaw. Two gaps remain. (1) `fortress` and `ruined_portal_nether` \
                 have no piece generator — each has its own row here — so at their \
                 placement cells the Nether gets an advisory start with \
                 `pieces_complete: false` and zero blocks, which is also what \
                 stops the weighted `nether_complexes` walk from handing every \
                 fortress cell to a bastion. (2) Nothing *serves* the dimension: \
                 `lodestone-server`'s `EmbeddedResolver` hardcodes the Overworld \
                 documents and `OverworldChunkSource` is the only chunk source, so a \
                 portal trip still does not land in this terrain"
                    .into(),
            );
        }
        // S4's own gaps, recorded once. Keyed so they cannot be mistaken for a
        // structure id, exactly as the S2 rows above are.
        if !pools.is_empty() {
            unsupported.insert(
                "pool:feature_pool_element".into(),
                "a `feature_pool_element` participates in the joint graph and the \
                 free-space accumulator (so the village around it is vanilla's) but \
                 places no blocks: its `placed_feature` needs the feature driver to \
                 accept a structure-supplied origin"
                    .into(),
            );
            unsupported.insert(
                "nbt:jigsaw_pool_element".into(),
                "a persisted jigsaw child carries `Template` instead of vanilla's \
                 `pool_element` compound, so a save this engine writes would reload \
                 as an invalid start in a real client"
                    .into(),
            );
            for dangling in pools.dangling_templates() {
                unsupported.insert(
                    format!("dangling:{dangling}"),
                    "referenced by a loaded template pool but not resolvable, and not \
                     shipped by vanilla either: replaced with an empty template, which \
                     is what `StructureTemplateManager.getOrCreate` does — the element \
                     stays in its pool and places nothing"
                        .into(),
                );
            }
            unsupported.insert(
                "jigsaw:step_order".into(),
                "every structure this engine places is written at the end of `pre_ore`, \
                 which is vanilla's `surface_structures` slot: an `underground_structures` \
                 structure is therefore placed slightly late and an \
                 `underground_decoration` one (ancient_city) early enough that ores can \
                 overwrite it"
                    .into(),
            );
            unsupported.insert(
                "jigsaw:gravity_reads_a_pre_beard_column".into(),
                "a `terrain_matching` element's GravityProcessor reads a fresh `_WG` \
                 noise column resolved at start time, not the decorating chunk's \
                 post-beard heightmap: chunk-independent by construction, which is \
                 what stops a street shearing at a chunk border"
                    .into(),
            );
        }

        Self {
            seed,
            sets,
            set_index,
            structures,
            templates,
            pools,
            unsupported,
        }
    }

    /// The world seed this registry was built for.
    ///
    /// Read by the placement stage, not by anything here: `CappedProcessor` forks
    /// the **world** seed positionally, so the value has to reach
    /// [`template::PlaceOrigin`] from somewhere and this is the only object on that
    /// path that already holds it.
    #[must_use]
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// The loaded jigsaw template pools.
    #[must_use]
    pub fn pools(&self) -> &PoolStore {
        &self.pools
    }

    /// The decoded templates this registry loaded.
    #[must_use]
    pub fn templates(&self) -> &TemplateStore {
        &self.templates
    }

    /// True when this registry places nothing at all (no structure-set data).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The parsed sets, in vanilla's bootstrap order.
    #[must_use]
    pub fn sets(&self) -> &[StructureSetDef] {
        &self.sets
    }

    /// One structure's parsed document.
    #[must_use]
    pub fn structure(&self, id: &str) -> Option<&StructureDef> {
        self.structures.get(id)
    }

    /// **The ledger**: every set, structure or placement type this registry
    /// parsed but cannot fully generate, mapped to why.
    ///
    /// Read this instead of assuming coverage. It is the difference between "this
    /// engine places 34 structures" and "this engine places 4 and names the other
    /// 30", and only the second sentence is true today.
    #[must_use]
    pub fn unsupported(&self) -> &BTreeMap<String, String> {
        &self.unsupported
    }

    /// `ChunkGeneratorStructureState.hasStructureChunkInRange` for an exclusion
    /// zone's `other_set`.
    ///
    /// **One level deep only.** Vanilla's version recurses through the other
    /// set's own placement (including *its* exclusion zone); in 26.2 the single
    /// exclusion zone (`pillager_outposts` → `villages`) points at a set with no
    /// zone of its own, so one level is exact. A datapack chaining two zones
    /// would be silently under-excluded here, which is why this is written down.
    fn has_placement_in_range(&self, other_set: &str, cx: i32, cz: i32, range: i32) -> bool {
        let Some(&index) = self.set_index.get(other_set) else {
            return false;
        };
        let other = &self.sets[index];
        for x in (cx - range)..=(cx + range) {
            for z in (cz - range)..=(cz + range) {
                if other.placement.is_placement_chunk(self.seed, x, z)
                    && other.placement.passes_frequency(self.seed, x, z)
                {
                    return true;
                }
            }
        }
        false
    }

    /// `StructurePlacement.isStructureChunk` — placement, frequency and
    /// exclusion, in vanilla's order (each gate can draw RNG, so the order is
    /// the specification).
    #[must_use]
    pub fn is_structure_chunk(&self, set: &StructureSetDef, cx: i32, cz: i32) -> bool {
        if !set.placement.is_placement_chunk(self.seed, cx, cz) {
            return false;
        }
        if !set.placement.passes_frequency(self.seed, cx, cz) {
            return false;
        }
        match &set.placement.exclusion_zone {
            None => true,
            Some(zone) => {
                !self.has_placement_in_range(&zone.other_set, cx, cz, zone.chunk_count)
            }
        }
    }

    /// `ChunkGenerator.createStructures` for one chunk: every start whose origin
    /// is `(cx, cz)`.
    ///
    /// Pure in `(seed, cx, cz, ctx)`. Starts are returned in structure-set
    /// (bootstrap) order.
    pub fn starts_at(&self, cx: i32, cz: i32, ctx: &dyn StartContext) -> Vec<StructureStart> {
        let mut out = Vec::new();
        for set in &self.sets {
            if !self.is_structure_chunk(set, cx, cz) {
                continue;
            }
            if set.entries.len() == 1 {
                if let Some(start) = self.try_start(&set.entries[0].0, cx, cz, ctx) {
                    out.push(start);
                }
                continue;
            }
            // The weighted walk. One legacy stream seeded per chunk (not per
            // set), re-drawn after each rejected option with the rejected
            // option's weight removed — so an early rejection shifts every
            // later draw. `nextInt(total)` is drawn *before* the linear scan,
            // once per attempt.
            let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
            random.set_large_feature_seed(self.seed, cx, cz);
            let mut options = set.entries.clone();
            let mut total: i32 = options.iter().map(|(_, w)| *w).sum();
            while !options.is_empty() {
                let mut choice = random.next_int_bounded(total);
                let mut index = 0usize;
                for (_, weight) in &options {
                    choice -= *weight;
                    if choice < 0 {
                        break;
                    }
                    index += 1;
                }
                // Vanilla indexes with the loop counter, which lands on
                // `options.size()` only if the weights do not sum to `total` —
                // impossible here, but clamped rather than panicking.
                let index = index.min(options.len() - 1);
                if let Some(start) = self.try_start(&options[index].0, cx, cz, ctx) {
                    out.push(start);
                    break;
                }
                total -= options[index].1;
                options.remove(index);
                if total <= 0 {
                    break;
                }
            }
        }
        out
    }

    /// `ChunkGenerator.tryGenerateStructure` + `Structure.generate`: the
    /// generation point, then the biome filter, then validity.
    fn try_start(
        &self,
        structure_id: &str,
        cx: i32,
        cz: i32,
        ctx: &dyn StartContext,
    ) -> Option<StructureStart> {
        let def = self.structures.get(structure_id)?;
        let stub = def.kind.find_stub(cx, cz, self.seed, ctx, &self.pools, &self.templates)?;
        let position = stub.position();
        // `Structure.isValidBiome`: the biome at the *stub position*, quart-wise,
        // including Y. Using y = 0 (or the surface) instead is the "y = 0 trap"
        // `crate::biome` already documents for carvers.
        let biome = ctx.biome_at_quart(position[0] >> 2, position[1] >> 2, position[2] >> 2);
        if !def.biomes.contains(&biome) {
            return None;
        }
        // Only now — vanilla's `GenerationStub` runs its piece consumer inside
        // `getPiecesBuilder()`, after `findValidGenerationPoint`'s biome filter, so
        // a biome-rejected candidate must consume no RNG and sample no columns.
        let generated = def.kind.generate_pieces(
            stub,
            cx,
            cz,
            self.seed,
            ctx,
            &self.templates,
            &self.pools,
        );
        match def.kind.validity(&generated) {
            Validity::Invalid => None,
            validity => {
                let complete = validity == Validity::Valid;
                let pieces = generated.unwrap_or_default();
                let bounding_box = pieces
                    .iter()
                    .map(|p| p.bounding_box)
                    .reduce(BoundingBox::encapsulate)
                    .unwrap_or(BoundingBox {
                        // Placeholder for an incomplete start: the origin
                        // chunk's own column. Never persisted (see
                        // `pieces_complete`) and never inflated into a
                        // beardifier box, because S3 filters on
                        // `pieces_complete` too.
                        min: [cx * 16, position[1], cz * 16],
                        max: [cx * 16 + 15, position[1], cz * 16 + 15],
                    });
                Some(StructureStart {
                    structure: def.id.clone(),
                    chunk_x: cx,
                    chunk_z: cz,
                    references: 0,
                    bounding_box,
                    pieces,
                    terrain_adaptation: def.terrain_adaptation,
                    pieces_complete: complete,
                })
            }
        }
    }
}

/// `RuinedPortalStructure.Setup.CODEC` — the `setups` array of a
/// `ruined_portal*.json` document. A malformed entry (a missing field, an
/// unrecognised `placement` string) is dropped rather than defaulted, so a
/// typo in the data reads as "one fewer setup" rather than a silently wrong
/// weight or placement.
fn parse_ruined_portal_setups(value: &Value) -> Vec<RuinedPortalSetup> {
    value["setups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(RuinedPortalSetup {
                placement: VerticalPlacement::parse(s["placement"].as_str()?)?,
                air_pocket_probability: s["air_pocket_probability"].as_f64()? as f32,
                mossiness: s["mossiness"].as_f64()? as f32,
                overgrown: s["overgrown"].as_bool()?,
                vines: s["vines"].as_bool()?,
                can_be_cold: s["can_be_cold"].as_bool()?,
                replace_with_blackstone: s["replace_with_blackstone"].as_bool()?,
                weight: s["weight"].as_f64()? as f32,
            })
        })
        .collect()
}

/// Resolves a `biomes`-shaped holder-set — a `"#tag"` reference, a bare id, or a
/// list of either — into the flat set of biome ids it denotes.
///
/// Recursive, because vanilla biome tags nest (`#has_structure/shipwreck`
/// includes `#is_ocean`, which includes `#is_deep_ocean`). Cycle-guarded by a
/// visited set, matching `crate::compose::resolve_block_tag`.
fn resolve_biome_set(resolver: &dyn Resolver, value: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut seen = BTreeSet::new();
    collect(resolver, value, &mut out, &mut seen);
    out
}

fn collect(
    resolver: &dyn Resolver,
    value: &Value,
    out: &mut HashSet<String>,
    seen: &mut BTreeSet<String>,
) {
    match value {
        Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => {
                if !seen.insert(tag.to_string()) {
                    return;
                }
                let document = resolver.biome_tag(tag);
                if let Some(values) = document["values"].as_array() {
                    for v in values {
                        // Tag entries may be `{"id": "...", "required": false}`.
                        match v {
                            Value::Object(o) => {
                                if let Some(id) = o.get("id") {
                                    collect(resolver, id, out, seen);
                                }
                            }
                            other => collect(resolver, other, out, seen),
                        }
                    }
                }
            }
            None => {
                out.insert(entry.clone());
            }
        },
        Value::Array(entries) => {
            for entry in entries {
                collect(resolver, entry, out, seen);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolver with no structure data places nothing and names nothing —
    /// the convention every fixture resolver in this workspace relies on.
    #[test]
    fn no_data_yields_an_inert_registry() {
        struct Empty;
        impl Resolver for Empty {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }
            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                unreachable!()
            }
        }
        let registry = StructureRegistry::new(42, &Empty);
        assert!(registry.is_empty());
        assert!(registry.unsupported().is_empty());
        struct NoWorld;
        impl StartContext for NoWorld {
            fn first_occupied_height(&self, _x: i32, _z: i32, _h: HeightmapKind) -> i32 {
                63
            }
            fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
                "minecraft:plains".into()
            }
            fn sea_level(&self) -> i32 {
                63
            }
        }
        assert!(registry.starts_at(0, 0, &NoWorld).is_empty());
    }

    /// `is_close_to_chunk` is the beardifier's reach test; a box one block
    /// outside the inflated window must not match, or S3's `refs` neighbourhood
    /// silently widens.
    #[test]
    fn close_to_chunk_is_exact_at_the_boundary() {
        let box_at = |x: i32| BoundingBox {
            min: [x, 0, 0],
            max: [x, 0, 0],
        };
        // Chunk 0 spans blocks 0..=15; with distance 12 the window is -12..=27.
        assert!(box_at(-12).is_close_to_chunk(0, 0, 12));
        assert!(!box_at(-13).is_close_to_chunk(0, 0, 12));
        assert!(box_at(27).is_close_to_chunk(0, 0, 12));
        assert!(!box_at(28).is_close_to_chunk(0, 0, 12));
    }

    /// A flat column at a fixed solid height, for [`ruined_portal_find_suitable_y`]
    /// — every `(x, z)` reads the same [`BlockKind`] up to `surface`, air above.
    struct FlatColumn {
        surface: i32,
    }
    impl StartContext for FlatColumn {
        fn first_occupied_height(&self, _x: i32, _z: i32, _h: HeightmapKind) -> i32 {
            self.surface
        }
        fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
            "minecraft:plains".into()
        }
        fn sea_level(&self) -> i32 {
            63
        }
        fn min_y(&self) -> i32 {
            -64
        }
        fn block_kind_at(&self, _x: i32, y: i32, _z: i32) -> BlockKind {
            if y <= self.surface { BlockKind::Stone } else { BlockKind::Air }
        }
    }

    /// `RuinedPortalStructure.sample` — the two extremes draw nothing and the
    /// probability decides deterministically; a middle value draws exactly one
    /// `nextFloat()` and gives a real mixture, not all-or-nothing.
    #[test]
    fn ruined_portal_sample_extremes_are_draw_free() {
        let mut random = structure_random(1, 0, 0);
        assert!(!ruined_portal_sample(&mut random, 0.0));
        assert!(ruined_portal_sample(&mut random, 1.0));
        // Both extremes drew nothing, so the stream is exactly where it started.
        let mut untouched = structure_random(1, 0, 0);
        assert_eq!(random.next_int_bounded(1_000_000), untouched.next_int_bounded(1_000_000));

        let mut trues = 0;
        for seed in 0..200i64 {
            let mut r = structure_random(seed, 3, -5);
            if ruined_portal_sample(&mut r, 0.5) {
                trues += 1;
            }
        }
        assert!((40..160).contains(&trues), "0.5 sampled true {trues}/200 times");
    }

    /// `findSuitableY`'s `ON_LAND_SURFACE`/`ON_OCEAN_FLOOR` arms draw nothing and
    /// are exactly `surfaceYAtCenter` — no search needed because the flat
    /// column is already solid there.
    #[test]
    fn find_suitable_y_at_surface_placements_is_exactly_the_sampled_surface() {
        let ctx = FlatColumn { surface: 80 };
        let box_ = BoundingBox { min: [0, 0, 0], max: [7, 0, 7] };
        for placement in [VerticalPlacement::OnLandSurface, VerticalPlacement::OnOceanFloor] {
            let mut random = structure_random(7, 2, -3);
            let untouched = structure_random(7, 2, -3);
            let y = ruined_portal_find_suitable_y(&mut random, placement, false, 80, 5, box_, &ctx);
            assert_eq!(y, 80, "{placement:?} did not land on the surface");
            // And no draw happened getting there.
            let mut a = random;
            let mut b = untouched;
            assert_eq!(a.next_int_bounded(1_000_000), b.next_int_bounded(1_000_000));
        }
    }

    /// `in_mountain`/`underground`/`partly_buried` seed `newY` from a draw before
    /// the search loop ever runs; over a column that is solid **everywhere** the
    /// loop's first check always succeeds, so the returned value is exactly that
    /// seed — predicted here from the same arithmetic the function itself uses
    /// (`random_within_interval`/`next_int_between`), on an independently-seeded
    /// but identically-positioned stream, rather than assumed to land on a round
    /// number.
    #[test]
    fn find_suitable_y_below_ground_placements_return_the_seeded_y_unmodified() {
        struct AlwaysSolid;
        impl StartContext for AlwaysSolid {
            fn first_occupied_height(&self, _x: i32, _z: i32, _h: HeightmapKind) -> i32 {
                80
            }
            fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
                "minecraft:plains".into()
            }
            fn sea_level(&self) -> i32 {
                63
            }
            fn min_y(&self) -> i32 {
                -64
            }
            fn block_kind_at(&self, _x: i32, _y: i32, _z: i32) -> BlockKind {
                BlockKind::Stone
            }
        }
        let ctx = AlwaysSolid;
        let box_ = BoundingBox { min: [0, 0, 0], max: [7, 0, 7] };
        let surface = 80;
        let y_span = 5;

        let mut expect_in_mountain = structure_random(11, -4, 6);
        let expected_in_mountain = random_within_interval(&mut expect_in_mountain, 70, surface - y_span);
        let mut random = structure_random(11, -4, 6);
        let y = ruined_portal_find_suitable_y(
            &mut random,
            VerticalPlacement::InMountain,
            false,
            surface,
            y_span,
            box_,
            &ctx,
        );
        assert_eq!(y, expected_in_mountain);

        let mut expect_underground = structure_random(11, -4, 6);
        let expected_underground =
            random_within_interval(&mut expect_underground, ctx.min_y() + 15, surface - y_span);
        let mut random = structure_random(11, -4, 6);
        let y = ruined_portal_find_suitable_y(
            &mut random,
            VerticalPlacement::Underground,
            false,
            surface,
            y_span,
            box_,
            &ctx,
        );
        assert_eq!(y, expected_underground);

        let mut expect_buried = structure_random(11, -4, 6);
        let expected_buried = surface - y_span + next_int_between(&mut expect_buried, 2, 8);
        let mut random = structure_random(11, -4, 6);
        let y = ruined_portal_find_suitable_y(
            &mut random,
            VerticalPlacement::PartlyBuried,
            false,
            surface,
            y_span,
            box_,
            &ctx,
        );
        assert_eq!(y, expected_buried);
    }

    /// The weighted setup pick draws exactly one `nextFloat()` regardless of
    /// list length, and both entries of a 50/50 split are reachable.
    #[test]
    fn pick_ruined_portal_setup_is_one_draw_and_reaches_both_entries() {
        let setups = vec![
            RuinedPortalSetup {
                placement: VerticalPlacement::Underground,
                air_pocket_probability: 1.0,
                mossiness: 0.2,
                overgrown: false,
                vines: false,
                can_be_cold: true,
                replace_with_blackstone: false,
                weight: 0.5,
            },
            RuinedPortalSetup {
                placement: VerticalPlacement::OnLandSurface,
                air_pocket_probability: 0.5,
                mossiness: 0.2,
                overgrown: false,
                vines: false,
                can_be_cold: true,
                replace_with_blackstone: false,
                weight: 0.5,
            },
        ];
        let mut seen_underground = false;
        let mut seen_surface = false;
        for seed in 0..200i64 {
            let mut random = structure_random(seed, 4, 9);
            let picked = pick_ruined_portal_setup(&setups, &mut random);
            match picked.placement {
                VerticalPlacement::Underground => seen_underground = true,
                VerticalPlacement::OnLandSurface => seen_surface = true,
                other => panic!("picked a placement not in the list: {other:?}"),
            }
        }
        assert!(seen_underground && seen_surface, "the 50/50 split favoured one entry only");
    }

    /// `getLavaProcessorRule`: `on_ocean_floor` always swaps to magma; elsewhere
    /// a cold setup swaps unconditionally to netherrack and a warm one only
    /// rolls magma sometimes — the three branches of vanilla's own `if`.
    #[test]
    fn ruined_portal_lava_rule_picks_the_right_branch() {
        let ocean = ruined_portal_lava_rule(VerticalPlacement::OnOceanFloor, false);
        assert!(matches!(ocean.input, RuleTest::BlockMatch(ref s) if s == "minecraft:lava"));
        assert_eq!(ocean.output.name, "minecraft:magma_block");

        let cold = ruined_portal_lava_rule(VerticalPlacement::Underground, true);
        assert!(matches!(cold.input, RuleTest::BlockMatch(ref s) if s == "minecraft:lava"));
        assert_eq!(cold.output.name, "minecraft:netherrack");

        let warm = ruined_portal_lava_rule(VerticalPlacement::Underground, false);
        assert!(matches!(warm.input, RuleTest::RandomBlockMatch(ref s, p) if s == "minecraft:lava" && p == 0.2));
        assert_eq!(warm.output.name, "minecraft:magma_block");
    }

    /// `makeSettings`'s processor chain, assembled from a hand-built stub: the
    /// ignore processor tracks `air_pocket`, the non-cold netherrack→magma rule
    /// is present only when `!cold`, and `BlackstoneReplace` is appended only
    /// when the setup asked for it — four independent on/off facts, not one.
    #[test]
    fn ruined_portal_piece_assembles_the_documented_processor_chain() {
        struct OnePortalTemplate;
        impl Resolver for OnePortalTemplate {
            fn density_function(&self, _id: &str) -> Value {
                Value::Null
            }
            fn noise(&self, _id: &str) -> crate::density::NoiseParams {
                unreachable!()
            }
            fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
                (id == "minecraft:ruined_portal/portal_1").then(|| {
                    std::fs::read(
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../lodestone-server/assets/structure/ruined_portal/portal_1.nbt"),
                    )
                    .expect("bundled portal_1.nbt")
                })
            }
        }
        let mut templates = TemplateStore::default();
        let failures = templates.load(&OnePortalTemplate, &["minecraft:ruined_portal/portal_1"]);
        assert!(failures.is_empty(), "{failures:?}");

        let make = |cold: bool, blackstone: bool| RuinedPortalStub {
            position: [0, 70, 0],
            template_id: "minecraft:ruined_portal/portal_1",
            rotation: Rotation::None,
            mirror: Mirror::None,
            pivot: [0, 0, 0],
            placement: VerticalPlacement::Underground,
            properties: RuinedPortalProperties {
                cold,
                mossiness: 0.2,
                air_pocket: true,
                overgrown: false,
                vines: false,
                replace_with_blackstone: blackstone,
            },
            features_cannot_replace: Arc::new(HashSet::new()),
        };

        let warm = ruined_portal_piece(make(false, false), &templates);
        let settings = &warm[0].placement.as_ref().expect("template piece").settings;
        assert_eq!(settings.processors.len(), 5, "no blackstone processor expected");
        assert!(
            matches!(&settings.processors[0], Processor::BlockIgnore(v) if v.len() == 1 && v[0] == "minecraft:structure_block")
        );
        let Processor::Rule(rules) = &settings.processors[1] else {
            panic!("second processor must be the rule chain");
        };
        assert_eq!(rules.len(), 3, "gold + lava + non-cold netherrack rule");

        let cold = ruined_portal_piece(make(true, false), &templates);
        let Processor::Rule(rules) = &cold[0].placement.as_ref().unwrap().settings.processors[1] else {
            panic!("second processor must be the rule chain");
        };
        assert_eq!(rules.len(), 2, "cold setups skip the non-cold netherrack rule");

        let blackstone = ruined_portal_piece(make(false, true), &templates);
        let bs_settings = &blackstone[0].placement.as_ref().unwrap().settings;
        assert_eq!(bs_settings.processors.len(), 6, "blackstone processor appended");
        assert!(matches!(bs_settings.processors[5], Processor::BlackstoneReplace));
    }
}
