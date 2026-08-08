//! Structure **placement and starts** — issue #514's S1, the engine that decides
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
//! legible silence, never a silent skip. Four sets are *closed* — every structure
//! they can place has a real generator, so for those the start set is exactly
//! vanilla's:
//!
//! | set | structures | oracle starts (seed −195764831) |
//! |---|---|---|
//! | `shipwrecks` | shipwreck, shipwreck_beached | 11 |
//! | `ocean_ruins` | ocean_ruin_cold, ocean_ruin_warm | 16 |
//! | `buried_treasures` | buried_treasure | 2 |
//! | `ocean_monuments` | monument | 2 |
//!
//! Everything else waits on S2 (templates: ruined_portal), S4 (jigsaw: villages,
//! trail_ruins, trial_chambers, ancient_city, pillager_outpost) or S5 (coded
//! pieces: mineshaft, stronghold, desert_pyramid, …).
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

pub mod placement;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};
use serde_json::Value;

use crate::density::Resolver;
use placement::{Placement, PlacementKind};

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
/// a structure. Carried by S1 so S3 has it; **nothing here evaluates it**.
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
    OceanRuin,
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
    /// A structure `type` whose generator has not landed. Carries the type id so
    /// the ledger can name it.
    Unsupported(String),
}

/// What a start predicate produced, before the caller applies the biome filter.
struct GenerationStub {
    /// The position the biome check is made at.
    position: [i32; 3],
    /// The pieces, or `None` when the generator does not exist.
    pieces: Option<Vec<StructurePiece>>,
}

impl StructureKind {
    fn parse(value: &Value, resolver: &dyn Resolver) -> Self {
        match value["type"].as_str().unwrap_or_default() {
            "minecraft:shipwreck" => Self::Shipwreck {
                beached: value["is_beached"].as_bool().unwrap_or(false),
            },
            "minecraft:ocean_ruin" => Self::OceanRuin,
            "minecraft:buried_treasure" => Self::BuriedTreasure,
            "minecraft:ocean_monument" => Self::OceanMonument {
                surrounding: resolve_biome_set(
                    resolver,
                    &Value::String(
                        "#minecraft:required_ocean_monument_surrounding".to_string(),
                    ),
                ),
            },
            other => Self::Unsupported(other.to_string()),
        }
    }

    /// `Structure.findGenerationPoint` — the generation point plus, for the
    /// structures whose generators exist, the piece list.
    ///
    /// Returns `None` where vanilla returns `Optional.empty()` (no start at all,
    /// before any biome check). **Draws no RNG for the currently implemented
    /// kinds**, matching vanilla: all four are `onTopOfChunkCenter`-shaped, whose
    /// piece generation is the lazy `Either.left` arm.
    fn find_generation_point(
        &self,
        cx: i32,
        cz: i32,
        ctx: &dyn StartContext,
    ) -> Option<GenerationStub> {
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
                Some(GenerationStub {
                    position: [middle_x, y, middle_z],
                    pieces: None,
                })
            }
            Self::OceanRuin => {
                let y =
                    ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::OceanFloorWg);
                Some(GenerationStub {
                    position: [middle_x, y, middle_z],
                    pieces: None,
                })
            }
            Self::BuriedTreasure => {
                let y =
                    ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::OceanFloorWg);
                // The piece's own position is `getBlockX(9)`, not the chunk
                // middle the biome check uses, and its Y is the literal 90 from
                // `BuriedTreasureStructure.generatePieces`. Vanilla's persisted
                // box is the *post-placement* one (`postProcess` reassigns
                // `boundingBox` after walking down to bedrock-ish stone), so a
                // freshly generated start and a reloaded one legitimately differ
                // in Y — see `docs/structures.md`.
                let px = cx * 16 + 9;
                let pz = cz * 16 + 9;
                Some(GenerationStub {
                    position: [middle_x, y, middle_z],
                    pieces: Some(vec![StructurePiece {
                        id: "minecraft:btp".to_string(),
                        bounding_box: BoundingBox::of_block(px, 90, pz),
                        orientation: None,
                        gen_depth: 0,
                        template: None,
                    }]),
                })
            }
            Self::OceanMonument { surrounding } => {
                let ox = cx * 16 + 9;
                let oz = cz * 16 + 9;
                if !biomes_within_all_in(ctx, ox, ctx.sea_level(), oz, 29, surrounding) {
                    return None;
                }
                let y =
                    ctx.first_occupied_height(middle_x, middle_z, HeightmapKind::OceanFloorWg);
                Some(GenerationStub {
                    position: [middle_x, y, middle_z],
                    pieces: None,
                })
            }
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
                let _ = middle_x;
                Some(GenerationStub {
                    position: [middle_x, ctx.sea_level(), middle_z],
                    pieces: None,
                })
            }
        }
    }

    /// Whether a start of this kind, having passed its biome check, is valid.
    /// `Unknown` for the kinds whose generators have not landed.
    fn validity(&self, pieces: &Option<Vec<StructurePiece>>) -> Validity {
        match self {
            Self::Unsupported(_) => Validity::Unknown,
            // The three template-driven kinds always add at least one piece, so
            // biome-valid implies start-valid — but their piece *lists* need S2's
            // template engine, so they report `Unknown` too until it lands.
            Self::Shipwreck { .. } | Self::OceanRuin | Self::OceanMonument { .. } => {
                match pieces {
                    Some(p) if !p.is_empty() => Validity::Valid,
                    _ => Validity::Unknown,
                }
            }
            Self::BuriedTreasure => match pieces {
                Some(p) if !p.is_empty() => Validity::Valid,
                _ => Validity::Invalid,
            },
        }
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
    unsupported: BTreeMap<String, String>,
}

impl StructureRegistry {
    /// Builds the registry for `seed` from `resolver`'s structure documents.
    ///
    /// An empty [`Resolver::structure_set_ids`] yields an empty registry that
    /// places nothing, which is what every fixture resolver in this workspace
    /// gets and why none of them had to change.
    #[must_use]
    pub fn new(seed: i64, resolver: &dyn Resolver) -> Self {
        let ids = resolver.structure_set_ids();
        let mut sets: Vec<StructureSetDef> = Vec::with_capacity(ids.len());
        let mut structures: HashMap<String, StructureDef> = HashMap::new();
        let mut unsupported: BTreeMap<String, String> = BTreeMap::new();

        for set_id in ids {
            let document = resolver.structure_set(&set_id);
            if document.is_null() {
                unsupported.insert(set_id.clone(), "structure set document missing".into());
                continue;
            }
            let placement = Placement::parse(&document["placement"]);
            if let PlacementKind::Unsupported(kind) = &placement.kind {
                unsupported.insert(set_id.clone(), format!("placement type '{kind}'"));
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
                let kind = StructureKind::parse(&doc, resolver);
                if let StructureKind::Unsupported(type_id) = &kind {
                    unsupported.insert(
                        structure_id.clone(),
                        format!("no piece generator for type '{type_id}'"),
                    );
                }
                let biomes = resolve_biome_set(resolver, &doc["biomes"]);
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

        Self {
            seed,
            sets,
            set_index,
            structures,
            unsupported,
        }
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
        let stub = def.kind.find_generation_point(cx, cz, ctx)?;
        // `Structure.isValidBiome`: the biome at the *stub position*, quart-wise,
        // including Y. Using y = 0 (or the surface) instead is the "y = 0 trap"
        // `crate::biome` already documents for carvers.
        let biome = ctx.biome_at_quart(
            stub.position[0] >> 2,
            stub.position[1] >> 2,
            stub.position[2] >> 2,
        );
        if !def.biomes.contains(&biome) {
            return None;
        }
        match def.kind.validity(&stub.pieces) {
            Validity::Invalid => None,
            validity => {
                let complete = validity == Validity::Valid;
                let pieces = stub.pieces.unwrap_or_default();
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
                        min: [cx * 16, stub.position[1], cz * 16],
                        max: [cx * 16 + 15, stub.position[1], cz * 16 + 15],
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
}
