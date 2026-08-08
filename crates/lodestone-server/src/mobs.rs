//! Server-side mob simulation — the *consumer* that ticks mob AI.
//!
//! `lodestone-entity` owns a complete goal scheduler, A\* pathfinder, and the
//! [`NavigatingMob`] composition that wires them together over the version-free
//! [`PathWorld`] seam. Until now nothing in a *running* world ticked any of it:
//! in vanilla multiplayer the client interpolates server-streamed positions and
//! correctly runs no AI, so the natural home for mob AI is the **server**, and
//! the server had no tick loop for it. This module is that home.
//!
//! Two pieces, deliberately kept separate rather than fused:
//!
//! * [`ChunkWorld`] adapts the server's own [`ChunkColumn`] terrain (which
//!   stores real vanilla block-state strings, not just a solid/air bit — see
//!   its own doc comment) into a [`PathWorld`]. It is the exact analogue of
//!   `lodestone-render`'s `world.rs`: this crate owns terrain *storage*,
//!   `lodestone-entity` owns the traversal reasoning, and the adapter is the
//!   single seam between them. It classifies each cell through the real
//!   26.2 per-block-state census (`lodestone_data::path_types` +
//!   `collision_shapes`) rather than a solid/air guess (issue #204) — and it
//!   stays version-free doing it, because `lodestone-data` is 26.2 *game*
//!   data (tags, collision geometry, ...) with no protocol dependency of its
//!   own (`docs/lodestone-data-crate.md`), not a `crates/protocol/*` crate.
//!   `base_path_type`/`collision_top` now distinguish water from lava from a
//!   fence from a trapdoor from a damaging block, matching whatever vanilla's
//!   `WalkNodeEvaluator.getPathTypeFromState`/`getFloorLevel` would say for
//!   the same state. `PathWorld::collides` (the coarse jump-clearance/
//!   diagonal-reach sweep) is unchanged and still reads
//!   [`ChunkColumn::is_solid`] — vanilla's own collision sweep tests real
//!   per-shape AABBs too, but that is a wider change than this issue asked
//!   for; its own doc comment below says so.
//! * [`MobSim`] owns the live mobs and advances them one tick at a time. The
//!   world outlives the sim (the mobs borrow it), which is why `ChunkWorld` is a
//!   value the caller holds and hands to [`MobSim::new`] by reference.
//!
//! # Scope, honestly — updated for issue #217
//!
//! The paragraph this replaced said streaming positions to a client needed a
//! version crate's `add_entity`/`move_entity` *encoders* that did not exist yet.
//! Those encoders shipped separately (`V770ServerProtocol::encode_add_entity`/
//! `encode_entity_update`/`encode_remove_entity` in `crates/protocol/v770`) and
//! were proven end-to-end against a real client by
//! `crates/protocol/v770/tests/entity_streaming_live.rs` — but with a
//! hand-mutated stand-in source, not a real [`MobSim`], because `MobSim` was
//! `!Send` at the time (it stores goals as `Box<dyn Goal>`, and
//! `lodestone_entity::ai::Goal` carried no `Send` bound) and
//! `IntegratedServer::open_in_memory_with_entities` spawns its serving task
//! with `tokio::spawn`, which requires the future — and everything it captures
//! — to be `Send`. `Goal: Send` landed since (`crates/lodestone-entity/src/ai/goal.rs`),
//! so that blocker is gone (see the `assert_send::<MobSim<'static>>()` const
//! check below, which now compiles).
//!
//! So the actual remaining gap, confirmed by grepping for
//! `open_in_memory_with_entities`/`MobSim::new` outside this crate's own
//! tests, was **not** a missing encoder — it was that nothing in production
//! ever constructed a [`MobSim`] or ticked it. [`LiveMobSource`] and
//! [`run_mob_tick_loop`] below close that: a background task owns a
//! [`ChunkWorld`] snapshot and a seeded [`MobSim`] for its lifetime, ticks it
//! once per server tick, and republishes snapshots into a shared
//! `EntitySource` the same [`serve_connection`](crate::serve_connection)
//! streaming pass `entity_streaming_live.rs` already exercises picks up
//! reactively on the connection's own inbound-packet cadence. See
//! [`crate::IntegratedServer::open_in_memory_with_mobs`] for the production
//! wiring and `docs/live-mob-sim.md` for the full writeup, including what is
//! deliberately still not built (natural terrain/biome-aware spawning).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use lodestone_data::{block_states, collision_shapes, entity_dimensions, entity_types, path_types};
use lodestone_entity::ai::goals::{RandomLookAroundGoal, RandomStrollGoal};
use lodestone_entity::ai::roster::{self, SpeciesContext};
use lodestone_entity::ai::navigating_mob::{
    BABY_START_AGE, DEFAULT_FOLLOW_RANGE, PARENT_AGE_AFTER_BREEDING,
};
use lodestone_entity::ai::mob::{EatenBlock, ProjectileLaunch};
use lodestone_entity::ai::{Goal, GoalSelector, MobController, NavigatingMob};
use lodestone_entity::attribute::default_attributes;
use lodestone_entity::explosion::Aabb as ExplosionAabb;
use lodestone_entity::item_entity::{ItemEntityRegistry, ItemLifecycle, ItemMotion};
use lodestone_entity::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};
use lodestone_entity::projectile::{Projectile, ProjectileRegistry, TrackedProjectile};
use lodestone_entity::{
    AttributeMap, DamageFlags, Defenses, HurtCooldown, HurtDecision, RayView, entity_damage,
    seen_percent,
};
use lodestone_model::PathType as CensusPathType;
use lodestone_model::{BlockPos, Identifier, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::chunk::{AIR, ChunkColumn, ChunkSource};
use crate::protocol::{EntitySnapshot, MetadataField};
use crate::mob_spawn::{
    DespawnOutcome, MobCategory, SpawnCandidateSource, SpawnRng, SpawnState, check_despawn,
};
use crate::server::EntitySource;

/// Translates `lodestone_model::PathType` (what the census in
/// [`lodestone_data::path_types`] is keyed by) into
/// `lodestone_entity::pathfinding::PathType` (what the A* search and malus
/// table consume). The two enums are deliberately separate crates on
/// opposite sides of the version seam — see `pathfinding/mod.rs`'s own doc
/// ("a real adapter... maps real block-state ids to `PathType`") — so this is
/// the translation layer that doc promises, not a rename. Every variant is
/// named on both sides identically; the match is exhaustive on the census
/// side so a future variant added to either enum fails to compile here
/// instead of silently falling through.
fn census_to_pathfinding_type(pt: CensusPathType) -> PathType {
    match pt {
        CensusPathType::Blocked => PathType::Blocked,
        CensusPathType::Open => PathType::Open,
        CensusPathType::Walkable => PathType::Walkable,
        CensusPathType::WalkableDoor => PathType::WalkableDoor,
        CensusPathType::Trapdoor => PathType::Trapdoor,
        CensusPathType::PowderSnow => PathType::PowderSnow,
        CensusPathType::OnTopOfPowderSnow => PathType::OnTopOfPowderSnow,
        CensusPathType::Fence => PathType::Fence,
        CensusPathType::Lava => PathType::Lava,
        CensusPathType::Water => PathType::Water,
        CensusPathType::WaterBorder => PathType::WaterBorder,
        CensusPathType::Rail => PathType::Rail,
        CensusPathType::UnpassableRail => PathType::UnpassableRail,
        CensusPathType::FireInNeighbor => PathType::FireInNeighbor,
        CensusPathType::Fire => PathType::Fire,
        CensusPathType::DamagingInNeighbor => PathType::DamagingInNeighbor,
        CensusPathType::Damaging => PathType::Damaging,
        CensusPathType::DoorOpen => PathType::DoorOpen,
        CensusPathType::DoorWoodClosed => PathType::DoorWoodClosed,
        CensusPathType::DoorIronClosed => PathType::DoorIronClosed,
        CensusPathType::Breach => PathType::Breach,
        CensusPathType::Leaves => PathType::Leaves,
        CensusPathType::StickyHoney => PathType::StickyHoney,
        CensusPathType::Cocoa => PathType::Cocoa,
        CensusPathType::DamageCautious => PathType::DamageCautious,
        CensusPathType::OnTopOfTrapdoor => PathType::OnTopOfTrapdoor,
        CensusPathType::BigMobsCloseToDanger => PathType::BigMobsCloseToDanger,
    }
}

/// Renders block-state `id`'s canonical string (`"minecraft:name"`, or
/// `"minecraft:name[k=v,k2=v2]"` with properties sorted by key) — the exact
/// format [`ChunkColumn::block_state`] stores, so the two agree without
/// either side special-casing the other. Mirrors
/// `lodestone_worldgen::surface::block_json_key`'s key format, which is where
/// that format is proven to match vanilla's own `BlockState.CODEC`
/// canonicalisation (see that function's doc comment).
fn canonical_state_string(id: u32) -> Option<String> {
    let name = block_states::block_name(id)?;
    let props = block_states::properties(id)?;
    if props.is_empty() {
        return Some(name.to_string());
    }
    let mut s = String::with_capacity(name.len() + 2);
    s.push_str(name);
    s.push('[');
    for (i, (k, v)) in props.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(k);
        s.push('=');
        s.push_str(v);
    }
    s.push(']');
    Some(s)
}

/// The reverse of [`canonical_state_string`]: every block-state id's canonical
/// string, keyed back to its id. Built once (32,366 entries) and cached for
/// the process lifetime — `lodestone-data` exposes id → name/properties but no
/// name → id lookup (nothing has ever needed one before this), and `ChunkColumn`
/// stores block states as those canonical strings, not ids, so `ChunkWorld`
/// needs this to bridge the two.
fn state_id_by_name() -> &'static HashMap<String, u32> {
    static INDEX: OnceLock<HashMap<String, u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map = HashMap::with_capacity(block_states::STATE_COUNT as usize);
        for id in 0..block_states::STATE_COUNT {
            if let Some(key) = canonical_state_string(id) {
                map.insert(key, id);
            }
        }
        map
    })
}

/// The global block-state id for a canonical state string, or `None` for a name
/// this version's census does not carry.
///
/// The one public door onto [`state_id_by_name`]'s cached index, added for
/// [`crate::block_drops`]'s correct-tool gate (issue #539): every per-block-state
/// census in `lodestone-data` — hardness, tool rules, collision — is keyed by
/// **id**, while `ChunkColumn` stores states as canonical **strings**, so
/// anything that reads one of those censuses for a block the world names has to
/// cross this bridge. Kept here rather than duplicated because building the
/// 32,366-entry map twice per process would be the only alternative.
///
/// Accepts a bare name (`"minecraft:stone"`) or one with properties
/// (`"minecraft:oak_log[axis=y]"`), since that is exactly what
/// [`ChunkColumn::block_state`] returns.
#[must_use]
pub(crate) fn block_state_id(name: &str) -> Option<u32> {
    state_id_by_name().get(name).copied()
}

/// The **lowest** block-state id belonging to a bare block name — a stand-in for
/// vanilla's `Block.defaultBlockState()` for callers that only need a
/// per-*block* census row.
///
/// Built once (1,196 entries) beside [`state_id_by_name`] and cached the same
/// way. Ids are allocated contiguously per block, so the lowest id of a block is
/// one of its states; every census that keys on a state id but whose value is a
/// property of the *block* — [`lodestone_data::hardness`] and
/// [`lodestone_data::tool`], the two [`crate::block_breaking`] reads — gives the
/// same answer for any of them. It is **not** a substitute for
/// [`block_state_id`] where the properties matter (collision shapes, path
/// types); those must resolve the exact state.
#[must_use]
fn default_state_id_by_block() -> &'static HashMap<&'static str, u32> {
    static INDEX: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut map: HashMap<&'static str, u32> =
            HashMap::with_capacity(block_states::BLOCK_COUNT as usize);
        for id in 0..block_states::STATE_COUNT {
            if let Some(name) = block_states::block_name(id) {
                map.entry(name).or_insert(id);
            }
        }
        map
    })
}

/// [`block_state_id`], falling back to the block's default state when the exact
/// state string is not in the index.
///
/// The fallback exists because a *bare* name is only in [`state_id_by_name`] for
/// a block with **no properties**: `"minecraft:stone"` resolves, and
/// `"minecraft:sugar_cane"` does not, because every sugar cane state carries
/// `age`. Anything that names a block without spelling out its properties — a
/// feature's simple state provider, a test fixture, a `/setblock`-shaped string —
/// therefore misses, and [`crate::block_breaking`] read that miss as "unknown
/// block, do not validate", which is exactly the one-shot-block bug it was
/// written to fix.
///
/// Only use this where the census being read is per-*block* rather than
/// per-state; see [`default_state_id_by_block`].
#[must_use]
pub(crate) fn block_state_id_or_default(name: &str) -> Option<u32> {
    if let Some(id) = block_state_id(name) {
        return Some(id);
    }
    let base = name.split('[').next()?;
    default_state_id_by_block().get(base).copied()
}

/// A [`PathWorld`] over the server's real per-block-state terrain.
///
/// Backed by a sparse map of [`ChunkColumn`]s keyed by chunk coordinate. Missing
/// columns and blocks outside the vertical range read as air. Each cell's
/// canonical block-state string (what [`ChunkColumn::block_state`] stores) is
/// resolved to its global block-state id and looked up in
/// [`lodestone_data::path_types`] / [`lodestone_data::collision_shapes`] — the
/// same 32,366-state census `WalkNodeEvaluator.getPathTypeFromState` produces
/// in vanilla — so water, lava, fences, doors, rails and damaging blocks
/// classify distinctly instead of collapsing to solid/air. A state that fails
/// to resolve (should not happen for anything this crate's own worldgen or
/// [`set_block`](ChunkWorld::set_block) ever writes) falls back to the old
/// solid/air guess rather than panicking mid-tick.
#[derive(Debug, Clone)]
pub struct ChunkWorld {
    columns: HashMap<(i32, i32), ChunkColumn>,
    min_y: i32,
    height: i32,
}

impl ChunkWorld {
    /// An empty world with the given vertical extent (world Y in
    /// `min_y..min_y + height`).
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        Self {
            columns: HashMap::new(),
            min_y,
            height,
        }
    }

    /// Snapshots a square region of chunk columns from a [`ChunkSource`] into an
    /// owned world the pathfinder can query.
    ///
    /// `cx_range`/`cz_range` are inclusive chunk-coordinate bounds. This is the
    /// bridge from the *streaming* terrain source the server sends to clients to
    /// the *static* view the pathfinder needs for the duration of a search.
    #[must_use]
    pub fn from_source<S: ChunkSource>(
        source: &S,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
    ) -> Self {
        let mut columns = HashMap::new();
        let mut extent: Option<(i32, i32)> = None;
        for cz in cz_range {
            for cx in cx_range.clone() {
                let col = source.column(cx, cz);
                extent = Some((col.min_y, col.height));
                columns.insert((cx, cz), col);
            }
        }
        let (min_y, height) = extent.unwrap_or((0, 1));
        Self {
            columns,
            min_y,
            height,
        }
    }

    /// [`from_source`](Self::from_source) over columns that have **already been
    /// generated** — the same snapshot, assembled from a batch someone else
    /// fetched.
    ///
    /// This exists because `from_source`'s loop is *serial* and synchronous, and
    /// issue #454's whole subject is that 49 of those calls (~45 s at the 909 ms
    /// per composed column measured in `crate::chunk_store`) ran on the thread
    /// that opens a world. `crate::integrated` now fetches the same columns
    /// through [`crate::chunk::generate_columns_offloaded`] — parallel, on the
    /// blocking pool, and through the shared [`crate::chunk_store::ChunkStore`]
    /// so the connection path's copy of each column is the *same* generation —
    /// and hands the results here.
    ///
    /// The vertical extent is taken from the last column, exactly as
    /// `from_source` does; an empty iterator yields `(0, 1)`, again matching.
    #[must_use]
    pub fn from_columns(columns: impl IntoIterator<Item = ((i32, i32), ChunkColumn)>) -> Self {
        let mut map = HashMap::new();
        let mut extent: Option<(i32, i32)> = None;
        for (coord, col) in columns {
            extent = Some((col.min_y, col.height));
            map.insert(coord, col);
        }
        let (min_y, height) = extent.unwrap_or((0, 1));
        Self {
            columns: map,
            min_y,
            height,
        }
    }

    /// Sets a single block's solidity at world coordinates, creating the owning
    /// column on demand. The natural "place a block" primitive for building
    /// arenas and, later, applying server-side edits.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let (min_y, height) = (self.min_y, self.height);
        let col = self
            .columns
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(min_y, height));
        col.set_solid(lx, y, lz, solid);
    }

    /// Sets a single block's canonical state (e.g. `"minecraft:water"`,
    /// `"minecraft:oak_fence"`, `"minecraft:oak_slab[type=bottom]"`) at world
    /// coordinates, creating the owning column on demand. The richer sibling of
    /// [`set_solid`](Self::set_solid): use this when a test or caller needs a
    /// specific census-distinguishable state (water vs. lava vs. a fence)
    /// rather than a bare solid/air bit.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let (min_y, height) = (self.min_y, self.height);
        let col = self
            .columns
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(min_y, height));
        col.set_block(lx, y, lz, name);
    }

    /// Whether the block at world coordinates is solid (neither air nor a
    /// fluid). This is [`ChunkColumn::is_solid`]'s coarse topology view, still
    /// used by [`collides`](PathWorld::collides) and as the fallback for a
    /// block state the census cannot resolve; [`base_path_type`](PathWorld::base_path_type)
    /// and [`collision_top`](PathWorld::collision_top) read the real per-state
    /// census instead.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .is_some_and(|col| col.is_solid(lx, y, lz))
    }

    /// The canonical block-state string at world coordinates, or
    /// `"minecraft:air"` for a missing column or an out-of-range Y — matching
    /// [`ChunkColumn::block_state`]'s own out-of-range behaviour.
    #[must_use]
    fn block_state(&self, x: i32, y: i32, z: i32) -> &str {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .map_or(AIR, |col| col.block_state(lx, y, lz))
    }

    /// Resolves the global block-state id at world coordinates through
    /// [`state_id_by_name`], if the state at that cell is one the 26.2 census
    /// knows about.
    #[must_use]
    fn state_id(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        state_id_by_name().get(self.block_state(x, y, z)).copied()
    }
}

impl PathWorld for ChunkWorld {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        // Real per-state classification: `WalkNodeEvaluator.getPathTypeFromState`
        // via `lodestone_data::path_types` (issue #204). Falls back to the old
        // solid/air guess only if the state string does not resolve to a known
        // 26.2 state id — not expected in practice, since every writer of this
        // world's terrain (worldgen, `set_solid`, `set_block`) emits canonical
        // vanilla state strings, but a tick must never panic on a lookup miss.
        self.state_id(x, y, z)
            .and_then(path_types::path_type)
            .map_or_else(
                || {
                    if self.is_solid(x, y, z) {
                        PathType::Blocked
                    } else {
                        PathType::Open
                    }
                },
                census_to_pathfinding_type,
            )
    }

    /// Block *identity*, which [`base_path_type`](PathWorld::base_path_type)
    /// deliberately erases — `grass_block`, `dirt` and `stone` are one
    /// `Blocked` there (issue #456).
    ///
    /// # The tag is read from the jar's own census, never hand-written
    ///
    /// `#minecraft:edible_for_sheep` is resolved through
    /// [`lodestone_data::tool::block_tag_members`], which is generated from the
    /// jar. That is not fastidiousness: the obvious hand-written
    /// `short_grass | tall_grass` guess is **wrong in both directions**. The
    /// real tag (`data/minecraft/tags/block/edible_for_sheep.json`) is
    /// `short_grass`, `short_dry_grass`, `tall_dry_grass`, `fern` — so
    /// `tall_grass` is not a member at all, and three members would have been
    /// missing. A sheep would have refused to graze ferns and dry grass, and
    /// nothing would have failed. This repo has been wrong about a hand-written
    /// tag set twice before (`pig_food`/`chicken_food`).
    ///
    /// # Two id spaces, and mixing them is silent
    ///
    /// `block_tag_members` answers in **`minecraft:block` registry ids**, while
    /// [`state_id`](ChunkWorld::state_id) yields **block-*state*** ids — a
    /// 32,366-entry space against a ~1,100-entry one. Comparing one against the
    /// other compiles, type-checks, and matches whatever unrelated blocks happen
    /// to share those small integers. `tool::block_registry_id` is the bridge, so
    /// the lookup is state → block → tag.
    ///
    /// `grass_block` stays block equality, because vanilla's test is equality
    /// rather than a tag (`ai/goal/EatBlockGoal.java:34`, `:71`).
    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        let state = self.block_state(x, y, z);
        // `block_state` yields a full state string; strip the property list so
        // `minecraft:short_grass[...]` compares as its block path.
        let path = state.split('[').next().unwrap_or(state);
        let edible_for_sheep = self
            .state_id(x, y, z)
            .and_then(lodestone_data::tool::block_registry_id)
            .is_some_and(|block| {
                lodestone_data::tool::block_tag_members("minecraft:edible_for_sheep")
                    .is_some_and(|members| members.binary_search(&block).is_ok())
            });
        BlockCues {
            edible_for_sheep,
            grass_block: path == "minecraft:grass_block",
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        // Vanilla asks exactly this at `WalkNodeEvaluator.getFloorLevel(level,
        // pos)` (.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/
        // WalkNodeEvaluator.java:219-222): `shape.isEmpty() ? 0.0 :
        // shape.max(Direction.Axis.Y)` over the block's real collision shape —
        // not a naive "one block tall" assumption. So this is the max Y of the
        // state's collision boxes (`lodestone_data::collision_shapes`), which is
        // `1.0` for a full cube, `0.5` for a slab, `1.5` for a fence/wall (the
        // reason a 0.6 step height cannot mount one), and `0.0` for an empty
        // shape (air, water, lava, cobweb). Falls back to the old full-cell
        // solid/air guess on the same not-expected-in-practice lookup miss as
        // `base_path_type` above.
        self.state_id(x, y, z)
            .and_then(collision_shapes::collision_boxes)
            .map_or_else(
                || if self.is_solid(x, y, z) { 1.0 } else { 0.0 },
                |boxes| {
                    boxes
                        .iter()
                        .fold(0.0_f64, |acc, b| acc.max(f64::from(b.max[1])))
                },
            )
    }

    fn collides(&self, aabb: Aabb) -> bool {
        // Mirror the `noCollision` sweep: any solid full-block cell overlapping
        // the box collides. The `-1e-7` on the max edges keeps a box that merely
        // *touches* a block face (shares a boundary) from counting as a collision,
        // matching vanilla's strict-overlap semantics.
        //
        // Honest scope note: this still tests coarse [`is_solid`](ChunkWorld::is_solid)
        // full-cell occupancy, not the real per-state collision boxes
        // `base_path_type`/`collision_top` now read. Vanilla's own
        // `noCollision` does test real per-shape AABBs (a fence's `1.5`-tall box
        // included), so a jump-clearance/diagonal-reach check against, say, a
        // slab is coarser here than in vanilla. Issue #204 asked for real
        // path-type classification and collision *tops*, which this closes;
        // widening `collides` itself to real per-shape sweeps is a separate,
        // larger change (every caller of this method would need auditing for
        // the same "is a box in `Aabb` allowed to graze an over-tall shape"
        // question) and is not part of this issue's scope.
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if self.is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }

    // `is_water` is no longer overridden: the trait default
    // (`base_path_type(x, y, z) == PathType::Water`) is now correct, because
    // `base_path_type` reads the real census instead of a solid/air guess that
    // could never produce `PathType::Water` in the first place.
}

impl RayView for ChunkWorld {
    /// A coarse but sound raymarch over [`is_solid`](ChunkWorld::is_solid):
    /// steps the segment at quarter-block spacing (fine enough that a
    /// full-block cell can never be skipped between samples) and reports
    /// blocked the moment any sample lands in a solid cell. This is not
    /// vanilla's exact voxel traversal (`ClipContext`), but it is a real
    /// terrain query — not the `OpenAir` stand-in [`explosion::seen_percent`]'s
    /// own tests use — which is what makes "a wall shields a mob from a blast"
    /// an observable, testable consequence rather than an assumption.
    fn is_clear(&self, from: Vec3, to: Vec3) -> bool {
        let delta = to - from;
        let dist = delta.length();
        if dist < 1e-9 {
            return true;
        }
        let steps = (dist / 0.25).ceil().max(1.0) as u32;
        for i in 0..=steps {
            let t = f64::from(i) / f64::from(steps);
            let p = from + delta.scale(t);
            if self.is_solid(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32) {
                return false;
            }
        }
        true
    }
}

/// Reads a computed attribute value from `attrs` by bare path (e.g.
/// `"max_health"`), applying the registry default when the attribute is not
/// explicitly present — mirrors [`AttributeMap::value`]'s own fallback so a
/// caller never has to special-case an absent key.
fn attr(attrs: &AttributeMap, path: &str) -> f64 {
    Identifier::from_str(&format!("minecraft:{path}"))
        .ok()
        .and_then(|id| attrs.value(&id))
        .unwrap_or(0.0)
}

/// [`attr`], but answering **`None`** when `attrs` does not actually carry the
/// attribute, instead of silently substituting the registry default.
///
/// # Why this is not the same function with a different default
///
/// [`attr`]'s `unwrap_or(0.0)` looks like the miss case and is nearly
/// unreachable: [`AttributeMap::value`] already falls back to
/// `default_def(key).default` for an absent instance, so it returns `Some` for
/// every attribute the registry knows. `attr(&AttributeMap::new(),
/// "follow_range")` is therefore **32.0**, not `0.0` — and 32.0 is the one value
/// `follow_range` must never take, because `Mob.createMobAttributes()` overrides
/// it to `16.0` for *every* mob, so no living entity in the game ever carries the
/// registry number (`ai/attributes/Attributes.java:51` vs `Mob.java:166-168`;
/// see `DEFAULT_FOLLOW_RANGE`'s own doc).
///
/// So a caller that needs "the species really declares this" cannot get it by
/// range-checking [`attr`]'s result — the wrong value is inside the plausible
/// range. It has to ask whether the instance exists, which is what this does.
/// `control_the_attribute_lookup_misses_to_the_registry_default_not_zero` pins
/// both readings so this distinction cannot quietly collapse.
fn attr_present(attrs: &AttributeMap, path: &str) -> Option<f64> {
    Identifier::from_str(&format!("minecraft:{path}"))
        .ok()
        .and_then(|id| attrs.get(&id))
        .map(lodestone_entity::attribute::AttributeInstance::value)
}

/// The health and combat-stat defaults for a mob type: `(max_health,
/// attack_damage, defenses, knockback_resistance)`.
///
/// Folds through [`default_attributes`] when `entity_type` is one of the
/// vanilla templates that module knows (the zombie family, skeleton family,
/// creeper, spider, and the common animals); for anything else it falls back
/// to an empty [`AttributeMap`], whose [`AttributeMap::value`] already resolves
/// every path to the generic `RangedAttribute` default (`max_health` 20,
/// `attack_damage` 2, no armor, no knockback resistance) — the same "unknown
/// type gets the generic default, never a guess" shape
/// [`resolve_mob_shape`](crate::resolve_mob_shape) uses for census geometry.
///
/// `knockback_resistance` (`minecraft:knockback_resistance`, registry default
/// `0.0`) is read here rather than folded into [`Defenses`] because it is a
/// *physics* property — `lodestone_physics::knockback::knockback_impulse`'s
/// own `knockback_resistance` parameter — not a damage-reduction one;
/// `Defenses` is exhaustively the damage pipeline's own fields (see
/// `lodestone_entity::damage`'s module doc, "knockback impulse... `impl-physics`
/// builds the knockback velocity from the other side").
fn combat_defaults(entity_type: &ResourceKey) -> (f32, f32, Defenses, f64) {
    let attrs = default_attributes(entity_type).unwrap_or_else(AttributeMap::new);
    let max_health = attr(&attrs, "max_health") as f32;
    let attack_damage = attr(&attrs, "attack_damage") as f32;
    let defenses = Defenses {
        armor: attr(&attrs, "armor") as f32,
        armor_toughness: attr(&attrs, "armor_toughness") as f32,
        ..Defenses::default()
    };
    let knockback_resistance = attr(&attrs, "knockback_resistance");
    (max_health, attack_damage, defenses, knockback_resistance)
}

/// Resolves a species' body from the real 26.2 dimension census, folded with
/// its `attrs`' `SCALE`/`STEP_HEIGHT` — see [`SimMob::spawn_species`]'s own doc
/// comment for why this duplicates (rather than calls)
/// [`crate::resolve_mob_shape`]'s fold: that function takes a
/// `&dyn VersionAdapter` for a version-aware caller, but `MobSim` already
/// reads `lodestone_data` directly for its path/collision census, so there is
/// no adapter to thread through here.
fn species_shape(entity_type: &ResourceKey, attrs: &AttributeMap) -> MobShape {
    let scale = attr(attrs, "scale") as f32;
    let step_height = attr(attrs, "step_height") as f32;
    let base = entity_types::entity_type_id_parts(entity_type.namespace(), entity_type.path())
        .and_then(entity_dimensions::base_dimensions);
    let mut shape = base.map_or_else(
        || MobShape::land(0.6, 1.95),
        |d| MobShape::land(d.width * scale, d.height * scale),
    );
    shape.max_up_step = step_height;
    shape
}

/// Whether `entity_type` is one of the hostile "monster" species, for the
/// purpose of picking its [`MobCategory`] and whether it resists natural
/// despawn.
///
/// **This no longer decides anything about goals.** It used to be the
/// hostile-versus-passive switch that gave a monster a `MeleeAttackGoal` and a
/// farm animal nothing, which is why it was a literal 8-name string match; that
/// job now belongs to [`lodestone_entity::ai::roster`], keyed per species and
/// cited against the jar. What is left here is spawn-category data, and it stays
/// here on purpose: `MobCategory` is one of **two** independent types by that
/// name in this workspace (this crate's, 7 variants, and
/// [`lodestone_entity::spawn::MobCategory`], 8 variants with a different
/// `check_despawn` signature), the roster deliberately takes no side in that fork,
/// and unifying them is issue #221's call.
///
/// # Where these names come from, and the heuristic that was wrong (issue #457)
///
/// Every path below was read from that species' own registration in
/// `EntityTypes.java` (`.cache/mc/26.2/src/net/minecraft/world/entity/`), which
/// is where vanilla's `MobCategory` actually lives — `EntityType.Builder.of(X::new,
/// MobCategory.MONSTER)`. The list previously held the original **eight** and its
/// doc claimed it "covers exactly the families
/// [`lodestone_entity::attribute::default_attributes`] templates as `Monster`".
/// That heuristic is **not equivalent to vanilla's category**, and reading the
/// registrations is what showed it:
///
/// * A **ghast** is `MobCategory.MONSTER` (`EntityTypes.java:473-474`) while its
///   attribute builder is a bare `Mob.createMobAttributes()` with no
///   `attack_damage` at all (`monster/Ghast.java:116-122`). Deriving the category
///   from the attribute template would have made it a persistent `Creature`.
/// * A **snow golem** is `MobCategory.MISC` (`EntityTypes.java:886`) — neither
///   `Monster` nor `Creature`. This function is a boolean, so it lands as
///   `Creature`; that is *not* vanilla's category, merely the safe direction
///   (`Misc` also never natural-despawns). Recorded here rather than papered
///   over, because it is the one species in the roster this predicate cannot
///   represent, and it is the argument for #221's category unification.
///
/// A species outside this list is still treated as a persistent `Creature`,
/// which is the safe direction: it will not be despawned out from under a
/// player. The failure mode this list has is therefore under-listing (a monster
/// that never despawns), not over-listing.
///
/// This is still a name list, and a name list still ages — see #455 for why the
/// *goal* half took the structural route instead. What keeps this one honest is
/// `every_rostered_monster_is_categorised_hostile` below, which drives the
/// roster's own species set through it rather than restating the names.
fn is_hostile_species(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        // Zombie family — `EntityTypes.java:1090, 534, 345, 1116, 1126`.
        "zombie"
            | "husk"
            | "drowned"
            | "zombie_villager"
            | "zombified_piglin"
            // Skeleton family — `:844, 931, 1058, 238, 736`.
            | "skeleton"
            | "stray"
            | "wither_skeleton"
            | "bogged"
            | "parched"
            // `:315`, `:903`, `:265`.
            | "creeper"
            | "spider"
            | "cave_spider"
            // `:513`, `:359` — both `MONSTER` despite being water-bound.
            | "guardian"
            | "elder_guardian"
            // `:473` (bare-`Mob` attributes, `MONSTER` category), `:231`, `:368`.
            | "ghast"
            | "blaze"
            | "enderman"
    )
}

/// Vanilla `Attributes.TEMPT_RANGE`'s default value
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/ai/attributes/Attributes.java:107`,
/// `register("tempt_range", new RangedAttribute(…, 10.0, 0.0, 2048.0))`), the
/// radius `TemptGoal` searches for a tempting player
/// (`ai/goal/TemptGoal.java:57` passes it into the targeting conditions).
///
/// This one lives in the *feed* rather than in the goal because vanilla keeps
/// it on the mob as an attribute; the other ranges below are per-goal-instance
/// constructor arguments and stay with the goal.
const TEMPT_RANGE: f64 = 10.0;

/// The radius every vanilla `AvoidEntityGoal` registration in the roster's
/// species uses — `6.0F` at `monster/Creeper.java:67-68` (Ocelot, Cat),
/// `monster/skeleton/AbstractSkeleton.java:79` (Wolf) and
/// `monster/spider/Spider.java:59` (Armadillo).
const AVOID_RANGE: f64 = 6.0;

/// The vertical half-extent of `AvoidEntityGoal`'s search box:
/// `getBoundingBox().inflate(maxDist, 3.0, maxDist)`
/// (`ai/goal/AvoidEntityGoal.java:72`) — note the Y extent is a flat `3.0`,
/// *not* `maxDist`, so a threat directly overhead is out of range sooner than
/// one to the side.
const AVOID_RANGE_Y: f64 = 3.0;

/// `BreedGoal`'s partner-search radius
/// (`ai/goal/BreedGoal.java:11`, `PARTNER_TARGETING = …range(8.0)…`, applied to
/// `getBoundingBox().inflate(8.0)` at `:64`).
const BREED_RANGE: f64 = 8.0;

/// How close two parents must be for `BreedGoal` to actually produce a child
/// (`ai/goal/BreedGoal.java:57`, `this.animal.distanceToSqr(this.partner) < 9.0`).
/// Reused here to identify *which* other mob was the partner when resolving a
/// [`NavigatingMob::take_bred`] event, since by then both parents' love state
/// has already been cleared by `breed()` itself.
const BREED_DISTANCE_SQR: f64 = 9.0;

/// `FollowParentGoal`'s search box, `getBoundingBox().inflate(8.0, 4.0, 8.0)`
/// (`ai/goal/FollowParentGoal.java:29`) — horizontal, then vertical.
const FOLLOW_PARENT_RANGE: f64 = 8.0;
const FOLLOW_PARENT_RANGE_Y: f64 = 4.0;

/// The species a given species flees, i.e. the `avoidClass` of each vanilla
/// `AvoidEntityGoal` registration. This is **perception data, not a goal set** —
/// it answers "is that thing a threat to me", which is what
/// [`MobController::avoid_threat`] needs; assembling the goals themselves is
/// the roster's job (plan units B1/B4), not this feed's.
///
/// Deliberately only the registrations that exist in 26.2 for species this sim
/// can currently spawn. An unknown species yields an empty slice, so
/// `AvoidEntityGoal` stays correctly inert for it rather than silently fleeing
/// everything.
fn avoided_species(species: &str) -> &'static [&'static str] {
    match species {
        // `monster/Creeper.java:67-68` — two separate goals, one per class.
        "creeper" => &["ocelot", "cat"],
        // `monster/skeleton/AbstractSkeleton.java:79`, inherited by every
        // skeleton variant.
        "skeleton" | "stray" | "wither_skeleton" | "bogged" => &["wolf"],
        // `monster/spider/Spider.java:59`. Vanilla additionally requires
        // `!armadillo.isScared()`; nothing here models an armadillo's scared
        // state, so that filter is a disclosed omission rather than a silent
        // one — it can only make a spider flee slightly more often.
        "spider" | "cave_spider" => &["armadillo"],
        _ => &[],
    }
}

/// The item paths in each species' vanilla food tag — what `TemptGoal` follows
/// a player for.
///
/// **Every entry is transcribed from the jar's own tag JSON**, not from memory,
/// which matters more than it sounds: older Minecraft versions used a *single*
/// item per species, and a from-memory list ("carrot for pig, seeds for
/// chicken") is wrong for 26.2 in two places — `pig_food` is three items and
/// `chicken_food` is six. Files, all under
/// `.cache/mc/26.2/src/data/minecraft/tags/item/`:
///
/// | tag | file | values |
/// |---|---|---|
/// | cow | `cow_food.json` | `wheat` |
/// | sheep | `sheep_food.json` | `wheat` |
/// | pig | `pig_food.json` | `carrot`, `potato`, `beetroot` |
/// | chicken | `chicken_food.json` | `wheat_seeds`, `melon_seeds`, `pumpkin_seeds`, `beetroot_seeds`, `torchflower_seeds`, `pitcher_pod` |
/// | rabbit | `rabbit_food.json` | `carrot`, `golden_carrot`, `dandelion` |
///
/// **This is an interim table and should be replaced, not extended.** Roster
/// unit B2 owns a *generated* item-tag table following the
/// `collision_shapes`/`hardness` generate-or-assert + `LODESTONE_REGEN=1`
/// pattern; the `damage_types` extraction is the closest existing precedent for
/// pulling tags out of datapack JSON. When that lands, this function's body
/// becomes a lookup into it and nothing else changes — the plumbing above and
/// below it is already in terms of a real held item.
///
/// Matched on the resource-key *path*, so a namespace other than `minecraft:`
/// would also match. Harmless today (nothing loads datapacks) and the generated
/// table will carry full keys.
fn tempt_food(species: &str) -> &'static [&'static str] {
    match species {
        // `AbstractCow` covers both, and they share `cow_food`.
        "cow" | "mooshroom" => &["wheat"],
        "sheep" => &["wheat"],
        "pig" => &["carrot", "potato", "beetroot"],
        "chicken" => &[
            "wheat_seeds",
            "melon_seeds",
            "pumpkin_seeds",
            "beetroot_seeds",
            "torchflower_seeds",
            "pitcher_pod",
        ],
        "rabbit" => &["carrot", "golden_carrot", "dandelion"],
        // Not a mistake: most species have no food tag, and an empty slice
        // keeps `TemptGoal` correctly inert for them rather than tempting them
        // with anything.
        _ => &[],
    }
}

/// What [`MobSim`] needs to know about one connected player in order to feed
/// mob perception. See [`MobSim::set_players`].
///
/// Not `Copy`, because [`held_item`](Self::held_item) owns a [`ResourceKey`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerPerception {
    /// The player's current position.
    pub position: Vec3,
    /// The item the player is currently holding, if any — straight from their
    /// `PlayerInventory`'s selected hotbar slot.
    ///
    /// The **item itself** rather than a pre-computed "is this tempting?"
    /// boolean, because the answer is per-*species*: wheat tempts a cow and a
    /// sheep, a potato tempts only a pig, and pumpkin seeds only a chicken
    /// (see [`tempt_food`]). A boolean here would have to be either wrong for
    /// some species or computed once per (player, species) pair by the caller,
    /// which is the feed's job, not the producer's.
    pub held_item: Option<ResourceKey>,
}

/// Vanilla `Creeper.DEFAULT_EXPLOSION_RADIUS`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/monster/Creeper.java:52`,
/// `private static final byte DEFAULT_EXPLOSION_RADIUS = 3;`), used flat by
/// [`MobSim::tick`]'s detonation trigger. Vanilla doubles this for a
/// lightning-charged (`isPowered`) creeper (`Creeper.java:230-234`,
/// `explosionMultiplier = isPowered() ? 2.0F : 1.0F`); `SimMob` has no
/// "powered" state anywhere in this crate (no lightning-charging is
/// implemented), so that multiplier is not modelled — a disclosed gap, not a
/// silent one.
const CREEPER_EXPLOSION_RADIUS: f32 = 3.0;

/// One live mob in the simulation: its [`NavigatingMob`] body and its own
/// [`GoalSelector`].
///
/// Configure it after spawning with [`add_goal`](SimMob::add_goal) and
/// [`set_attack_target`](SimMob::set_attack_target); observe it with
/// [`position`](SimMob::position) / [`path_searches`](SimMob::path_searches).
/// A live persistent grudge: vanilla's `NeutralMob` anger state, resolved by
/// the host (issue #458).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anger {
    /// The absolute [`MobSim::tick_count`] at which this grudge expires. The
    /// grudge is live while `tick_count < end_time`.
    end_time: u64,
    /// Where the offending entity was when the grudge was set. A position
    /// rather than an id because that is all
    /// [`MobController::angry_target`] carries; an *ownership* relation (#458's
    /// primitive 5) is what a real identity would need, and it does not exist
    /// at this seam yet.
    target: Vec3,
}

/// Vanilla's persistent-anger duration, in ticks, **inclusive at both ends**.
///
/// `NeutralMob.PERSISTENT_ANGER_TIME = TimeUtil.rangeOfSeconds(20, 39)`, which
/// is `UniformInt.of(400, 780)` — `rangeOfSeconds` multiplies by 20, so this is
/// already ticks. Identical for all four neutral species.
///
/// **Ticks, not seconds.** Sampling `[20, 39]` here would expire a grudge in
/// under two seconds; `anger_expires_inside_the_jars_tick_window` separates
/// those two hypotheses explicitly rather than asserting a grudge merely ends.
const ANGER_TICKS: (u64, u64) = (400, 780);

/// One draw from [`ANGER_TICKS`], matching vanilla's
/// `UniformInt.sample` / `Mth.randomBetweenInclusive`: `lo + nextInt(hi - lo + 1)`.
///
/// The `+ 1` is the inclusive upper bound, and dropping it is the classic
/// off-by-one that makes 780 unreachable — a difference no "does the grudge
/// expire" assertion could see.
fn grudge_ticks(mob: &mut impl MobController) -> u64 {
    let (lo, hi) = ANGER_TICKS;
    let span = i32::try_from(hi - lo + 1).expect("the anger window fits in i32");
    lo + u64::try_from(mob.next_i32(span)).unwrap_or(0)
}

#[derive(Debug)]
pub struct SimMob<'w> {
    id: i32,
    mob: NavigatingMob<'w>,
    goals: GoalSelector,
    category: MobCategory,
    /// Vanilla `Mob.noActionTime`: ticks since the mob last "did something".
    /// Advanced each [`MobSim::tick`] and consulted by the despawn gates; reset
    /// when the mob is within a player's immune radius.
    no_action_time: i32,
    /// Whether the mob is exempt from natural despawn (named, persistence-
    /// required, or a persistent category). Persistent mobs skip the gates.
    persistent: bool,
    /// Stable identity for the mob's sim-entry lifetime, encoded verbatim in the
    /// spawn packet. Assigned once at [`MobSim::spawn`].
    uuid: Uuid,
    /// Canonical entity-type key (e.g. `minecraft:zombie`). The sim spawns mobs
    /// by spawn-rule [`MobCategory`], not species, so this is a documented
    /// placeholder (defaulting to `minecraft:zombie`, matching the default
    /// `Monster` category) until species-aware spawning lands; a consumer that
    /// knows the species sets it with [`set_entity_type`](SimMob::set_entity_type).
    entity_type: ResourceKey,
    /// Current health. A hit that drives this to `0.0` removes the mob from
    /// the sim at the end of the tick that landed it (vanilla's immediate
    /// death removal).
    health: f32,
    /// Armour/resistance/absorption state `damage::apply_reductions` reads for
    /// every incoming hit; absorption is written back after each hit.
    defenses: Defenses,
    /// Vanilla's persistent-anger state, host-side (issue #458, primitive 1):
    /// the **absolute game tick** the grudge ends at, plus where the entity it
    /// is held against was when it was set.
    ///
    /// `None` means no live grudge — vanilla's `NO_ANGER_END_TIME = -1`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/NeutralMob.java:20-22`).
    ///
    /// **A deadline, not a countdown.** 26.2 stores an absolute game time and
    /// compares against it (`NeutralMob.java:112-120`); a decrementing counter
    /// drifts against a stepped tick loop. The comparison is against
    /// [`MobSim::tick_count`], which is the only clock this sim has.
    ///
    /// This lives on the host rather than on `NavigatingMob` because
    /// [`MobController::angry_target`] is deliberately an *answer*, not a
    /// query: the seam has no shared clock, so the host resolves expiry and
    /// only `Option<Vec3>` crosses. See that method's own doc comment.
    anger: Option<Anger>,
    /// Raw melee damage this mob's own attacks deal (`ATTACK_DAMAGE`
    /// attribute), applied to whatever [`attack_target_id`](SimMob::attack_target_id)
    /// names when a `MeleeAttackGoal` connects.
    attack_damage: f32,
    /// The invulnerability-frame gate for hits landing on *this* mob
    /// (`damage::HurtCooldown`), ticked once per sim tick regardless of
    /// whether anything hit this tick.
    hurt_cooldown: HurtCooldown,
    /// The id of another live [`SimMob`] this mob's melee attacks should
    /// damage, set alongside [`set_attack_target`](SimMob::set_attack_target)'s
    /// `Vec3` (which only drives movement — the goal/navigation seam has no
    /// entity identity, just positions).
    attack_target_id: Option<i32>,
    /// The id of the entity that owns this mob, if any — issue #458's
    /// primitive 5, the ownership relation. Vanilla stores a tamed animal's
    /// owner as a **player** `UUID` (`TamableAnimal.DATA_OWNERUUID_ID`,
    /// `animal/TamableAnimal.java:41`), but player identity does not exist at
    /// this seam — [`PlayerPerception`] carries only position and held item —
    /// so today this can only name another [`SimMob`]. The seam carries the
    /// resolved *position* ([`MobController::owner_position`]); the identity
    /// lives here, because only a census of entities can hold it.
    ///
    /// `None` for a wild mob — which is every mob in production today (taming
    /// is not implemented; nothing calls [`set_owner_id`](SimMob::set_owner_id)
    /// yet).
    owner_id: Option<i32>,
    /// `minecraft:knockback_resistance` attribute value (`0.0..=1.0`),
    /// `lodestone_physics::knockback::knockback_impulse`'s own
    /// `knockback_resistance` parameter for a hit landing on *this* mob. See
    /// [`combat_defaults`]'s doc comment for why this is not folded into
    /// [`Defenses`].
    knockback_resistance: f64,
}

impl<'w> SimMob<'w> {
    /// The entity id assigned at spawn.
    #[must_use]
    pub fn id(&self) -> i32 {
        self.id
    }

    /// Adds a prioritised goal (higher priority preempts lower on shared flags),
    /// returning `&mut self` so goals can be chained at spawn.
    pub fn add_goal(&mut self, priority: i32, goal: Box<dyn Goal>) -> &mut Self {
        self.goals.add(priority, goal);
        self
    }

    /// Sets the mob's current attack target (what a `MeleeAttackGoal` chases).
    pub fn set_attack_target(&mut self, target: Option<Vec3>) {
        self.mob.set_attack_target(target);
    }

    /// Puts this animal into love mode for
    /// [`LOVE_TICKS`](lodestone_entity::ai::navigating_mob::LOVE_TICKS)
    /// (vanilla `Animal::setInLove`, `animal/Animal.java:174`) — what feeding
    /// it a breeding item does. [`MobSim::tick`]'s partner search only
    /// considers mobs in this state.
    pub fn set_in_love(&mut self) -> &mut Self {
        self.mob.set_in_love();
        self
    }

    /// Whether this animal is currently in love mode.
    #[must_use]
    pub fn is_in_love(&self) -> bool {
        self.mob.is_in_love()
    }

    /// Remaining love-mode ticks (vanilla `Animal.getInLoveTime`).
    #[must_use]
    pub fn love_time(&self) -> i32 {
        self.mob.love_time()
    }

    /// The mob's age timer: negative while a baby (counting up to `0`),
    /// positive as the post-breeding parent cooldown (counting down to `0`).
    #[must_use]
    pub fn age(&self) -> i32 {
        self.mob.age()
    }

    /// Sets the age timer — e.g.
    /// [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to spawn this mob as a baby.
    pub fn set_age(&mut self, age: i32) -> &mut Self {
        self.mob.set_age(age);
        self
    }

    /// Whether this mob is a baby (`age < 0`), which is what gates
    /// `FollowParentGoal` and excludes it from breeding.
    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.mob.is_baby()
    }

    /// Whether this mob is inside its post-damage panic window
    /// ([`PANIC_DAMAGE_TICKS`](lodestone_entity::ai::navigating_mob::PANIC_DAMAGE_TICKS)).
    #[must_use]
    pub fn is_panicking(&self) -> bool {
        self.mob.is_panicking()
    }

    /// The position of whatever most recently hurt this mob, while inside the
    /// retaliation window
    /// ([`LAST_HURT_BY_TICKS`](lodestone_entity::ai::navigating_mob::LAST_HURT_BY_TICKS)).
    #[must_use]
    pub fn last_hurt_by(&self) -> Option<Vec3> {
        self.mob.last_hurt_by()
    }

    /// Whether the mob's feet cell holds water, read from the world (never
    /// injected) — what drives `FloatGoal`.
    #[must_use]
    pub fn in_water(&self) -> bool {
        self.mob.in_water()
    }

    /// Whether the mob's feet cell holds lava.
    #[must_use]
    pub fn in_lava(&self) -> bool {
        self.mob.in_lava()
    }

    /// The nearest-player position [`MobSim::tick`] last fed this mob, if any.
    /// `None` when no player is known — including when nothing has ever called
    /// [`MobSim::set_players`], which is still the case in production; see that
    /// method's doc comment.
    #[must_use]
    pub fn nearest_player(&self) -> Option<Vec3> {
        self.mob.nearest_player()
    }

    /// The tempting-entity position [`MobSim::tick`] last fed this mob.
    #[must_use]
    pub fn temptation(&self) -> Option<Vec3> {
        self.mob.temptation()
    }

    /// The threat position [`MobSim::tick`] last fed this mob, from
    /// [`avoided_species`]'s table.
    #[must_use]
    pub fn avoid_threat(&self) -> Option<Vec3> {
        self.mob.avoid_threat()
    }

    /// The nearest-adult position [`MobSim::tick`] last fed this mob, which is
    /// what `FollowParentGoal` follows. Always `None` for an adult.
    #[must_use]
    pub fn parent_candidate(&self) -> Option<Vec3> {
        self.mob.parent_position()
    }

    /// The breeding-partner position [`MobSim::tick`] last fed this mob, which
    /// is what `BreedGoal` pursues.
    #[must_use]
    pub fn partner_candidate(&self) -> Option<Vec3> {
        self.mob.love_partner_position()
    }

    /// The id of this mob's owner, if any (issue #458, primitive 5). `None`
    /// for a wild (untamed) mob.
    #[must_use]
    pub fn owner_id(&self) -> Option<i32> {
        self.owner_id
    }

    /// Sets this mob's owner id (vanilla `TamableAnimal.tame` /
    /// `setOwnerUUID`). A future tame interaction is the producer; nothing
    /// calls it in production yet.
    pub fn set_owner_id(&mut self, owner_id: Option<i32>) -> &mut Self {
        self.owner_id = owner_id;
        self
    }

    /// The position of this mob's owner as the [`MobController`] seam reports
    /// it — what [`MobSim::tick`]'s feed last resolved from
    /// [`owner_id`](Self::owner_id). `None` until the feed has run, and for a
    /// wild mob.
    #[must_use]
    pub fn owner_position(&self) -> Option<Vec3> {
        self.mob.owner_position()
    }

    /// Teleports this mob directly to `pos` (issue #458, primitive 3: instant
    /// relocation) — the host command the enderman's damage-triggered
    /// `teleport()` and gaze-triggered `teleportTowards` reduce to. Rewrites
    /// position immediately and abandons any in-progress path (vanilla
    /// `Entity.teleportTo`, `entity/Entity.java:1513-1515`).
    pub fn teleport_to(&mut self, pos: Vec3) -> &mut Self {
        self.mob.teleport_to(pos);
        self
    }

    /// Records a self-inflicted damage request (issue #458, primitive 4) — the
    /// bee's sting self-destruct (`animal/Bee.java:374-379`). Drained and
    /// applied by [`MobSim::tick`] through the normal damage pipeline.
    pub fn damage_self(&mut self, amount: f32) -> &mut Self {
        self.mob.damage_self(amount);
        self
    }

    /// The mob's current attack-target *position* (what a `MeleeAttackGoal`
    /// chases), as distinct from
    /// [`attack_target_id`](SimMob::attack_target_id)'s entity identity. This
    /// is the state `HurtByTargetGoal` writes when it retaliates.
    #[must_use]
    pub fn attack_target(&self) -> Option<Vec3> {
        self.mob.attack_target()
    }

    /// Whether a goal has this mob holding jump this tick — the observable
    /// effect of `FloatGoal`, i.e. what floating actually looks like.
    #[must_use]
    pub fn is_jumping(&self) -> bool {
        self.mob.is_jumping()
    }

    /// The last position a goal asked this mob to look at, if any — the
    /// observable effect of `LookAtPlayerGoal`. Distinct from
    /// [`head_yaw`](SimMob::head_yaw), which is the derived angle; this is the
    /// target the goal actually chose, so a test can assert *what* the mob
    /// turned toward rather than merely that some angle changed.
    #[must_use]
    pub fn facing(&self) -> Option<Vec3> {
        self.mob.facing()
    }

    /// `no_action_time` **as the goals see it**, through the
    /// [`MobController`] seam.
    ///
    /// Deliberately separate from [`no_action_time`](SimMob::no_action_time),
    /// which reads the sim's own record. The two being equal is exactly what
    /// issue #441 fixed: the sim incremented its record every tick and never
    /// pushed it across the seam, so goals read the trait default `0` forever.
    /// Keeping both readable is what lets a test assert the equality rather
    /// than assume it.
    #[must_use]
    pub fn mob_no_action_time(&self) -> i32 {
        MobController::no_action_time(&self.mob)
    }

    /// How many goals are installed on this mob. Used to assert a
    /// [`MobSim::tick`]-spawned child inherited a goal set rather than arriving
    /// inert.
    #[must_use]
    pub fn goal_count(&self) -> usize {
        self.goals.len()
    }

    /// Marks the mob ignited (vanilla `Creeper.ignite()`), forcing a
    /// creeper's swell direction to climb every tick regardless of
    /// [`SwellGoal`](lodestone_entity::ai::goals::SwellGoal)'s own proximity
    /// check. A no-op for a mob whose [`NavigatingMob`] never has anything
    /// else move its swell direction off `-1` (every non-creeper species).
    pub fn ignite(&mut self) -> &mut Self {
        self.mob.ignite();
        self
    }

    /// Whether this mob is currently ignited. See [`ignite`](Self::ignite).
    #[must_use]
    pub fn is_ignited(&self) -> bool {
        self.mob.is_ignited()
    }

    /// The current fuse counter (vanilla `Creeper.swell`), `0..=MAX_SWELL`
    /// for a creeper; permanently `0` for a species nothing ever moves off
    /// [`swell_dir`](Self::swell_dir)'s `-1` default.
    #[must_use]
    pub fn swell(&self) -> i32 {
        self.mob.swell()
    }

    /// The mob's current swell direction (vanilla `Creeper.getSwellDir`).
    #[must_use]
    pub fn swell_dir(&self) -> i32 {
        self.mob.swell_dir()
    }

    /// Sets which live mob (by id) this mob's connecting melee attacks damage.
    /// The goal/navigation seam only ever deals in positions
    /// ([`set_attack_target`](Self::set_attack_target)); this is the identity
    /// [`MobSim::tick`] needs to resolve a strike into an actual
    /// [`apply_damage`](Self::apply_damage) call on the right mob.
    pub fn set_attack_target_id(&mut self, target_id: Option<i32>) -> &mut Self {
        self.attack_target_id = target_id;
        self
    }

    /// The id of the mob this one's connecting attacks currently damage, if set.
    #[must_use]
    pub fn attack_target_id(&self) -> Option<i32> {
        self.attack_target_id
    }

    /// Current health. Reaches `0.0` (never negative) when the mob has taken
    /// lethal damage; [`MobSim::tick`] removes a mob whose health is `0.0` at
    /// the end of the tick that landed the killing blow.
    #[must_use]
    pub fn health(&self) -> f32 {
        self.health
    }

    /// Overrides current health (e.g. to stage a near-death mob in a test).
    /// Clamped to `>= 0.0`.
    pub fn set_health(&mut self, health: f32) -> &mut Self {
        self.health = health.max(0.0);
        self
    }

    /// Overrides the raw melee damage this mob's attacks deal, in place of the
    /// type's `ATTACK_DAMAGE` default resolved at spawn.
    pub fn set_attack_damage(&mut self, attack_damage: f32) -> &mut Self {
        self.attack_damage = attack_damage;
        self
    }

    /// The raw melee damage this mob's attacks currently deal.
    #[must_use]
    pub fn attack_damage(&self) -> f32 {
        self.attack_damage
    }

    /// Overrides this mob's defensive state (armour/toughness/absorption) in
    /// place of the type's defaults resolved at spawn.
    pub fn set_defenses(&mut self, defenses: Defenses) -> &mut Self {
        self.defenses = defenses;
        self
    }

    /// This mob's current defensive state.
    #[must_use]
    pub fn defenses(&self) -> &Defenses {
        &self.defenses
    }

    /// Overrides this mob's `minecraft:knockback_resistance` value in place
    /// of the type's default resolved at spawn.
    pub fn set_knockback_resistance(&mut self, knockback_resistance: f64) -> &mut Self {
        self.knockback_resistance = knockback_resistance;
        self
    }

    /// This mob's current `minecraft:knockback_resistance` value.
    #[must_use]
    pub fn knockback_resistance(&self) -> f64 {
        self.knockback_resistance
    }

    /// Applies a velocity impulse to this mob — see
    /// [`NavigatingMob::apply_knockback`] for the exact one-tick-displacement
    /// mechanic this forwards to.
    pub fn apply_knockback(&mut self, impulse: Vec3) {
        self.mob.apply_knockback(impulse);
    }

    /// Runs the full vanilla hit pipeline against this mob for one incoming
    /// hit of `raw_damage`: the invulnerability-frame gate
    /// ([`HurtCooldown::on_hurt`]), then armour/resistance/enchantment/
    /// absorption reduction ([`apply_reductions`](lodestone_entity::apply_reductions)),
    /// then subtracts the result from [`health`](Self::health) (floored at
    /// `0.0`). A hit fully inside the i-frame window and no stronger than the
    /// one that opened it is ignored entirely, exactly as vanilla drops a
    /// weaker follow-up hit.
    ///
    /// Returns the damage that actually reached health (`0.0` if the hit was
    /// ignored, if it was fully absorbed, or if the mob was already dead).
    pub fn apply_damage(&mut self, raw_damage: f32, flags: DamageFlags) -> f32 {
        if self.health <= 0.0 {
            return 0.0;
        }
        let amount = match self.hurt_cooldown.on_hurt(raw_damage, flags) {
            HurtDecision::Ignored => return 0.0,
            HurtDecision::Full { amount } | HurtDecision::Topup { amount } => amount,
        };
        let outcome = lodestone_entity::apply_reductions(amount, &self.defenses, flags);
        self.defenses.absorption = outcome.remaining_absorption;
        self.health = (self.health - outcome.to_health).max(0.0);
        // Issue #441: every hit that is not swallowed by i-frames opens the
        // panic window, because vanilla's `PanicGoal.shouldPanic` reads the
        // damage *source* rather than the attacking mob
        // (`ai/goal/PanicGoal.java:61-63`) — so fall damage and drowning panic
        // an animal exactly as a wolf bite does. The attacker half of the
        // record is added by whichever caller knows the attacker's position
        // ([`MobSim::attack`] and [`MobSim::tick`]'s melee resolution); the
        // ones that do not (an explosion, a future environmental source) leave
        // the mob panicking with nothing to retaliate against, which is the
        // correct vanilla outcome rather than a gap.
        //
        // Placed here, in the single funnel every damage path already goes
        // through, so a new damage source cannot forget it.
        self.mob.note_hurt(None);
        outcome.to_health
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.mob.position()
    }

    /// The mob's collision body — the box [`MobSim::explode`] samples for
    /// blast exposure.
    #[must_use]
    pub fn shape(&self) -> &MobShape {
        self.mob.shape()
    }

    /// How many A\* searches this mob has run — the count that proves the
    /// pathfinder is actually being driven (a stubbed `move_to` never searches).
    #[must_use]
    pub fn path_searches(&self) -> u32 {
        self.mob.path_searches()
    }

    /// Whether the mob still has a path it is following.
    #[must_use]
    pub fn has_path(&self) -> bool {
        self.mob.has_path()
    }

    /// The mob's spawn category (drives its despawn distances).
    #[must_use]
    pub fn category(&self) -> MobCategory {
        self.category
    }

    /// Sets the mob's spawn category. Used by the spawn driver so a mob's
    /// despawn behaviour matches the category it was spawned as.
    pub fn set_category(&mut self, category: MobCategory) -> &mut Self {
        self.category = category;
        self
    }

    /// The mob's current `no_action_time` age timer (ticks since it last acted).
    #[must_use]
    pub fn no_action_time(&self) -> i32 {
        self.no_action_time
    }

    /// Whether the mob is exempt from natural despawn.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Marks the mob persistent (named / persistence-required) so it never
    /// naturally despawns, mirroring vanilla `isPersistenceRequired`.
    pub fn set_persistent(&mut self, persistent: bool) -> &mut Self {
        self.persistent = persistent;
        self
    }

    /// The mob's stable UUID, encoded verbatim in the spawn packet.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// The mob's canonical entity-type key (e.g. `minecraft:zombie`). See the
    /// field docs for the placeholder caveat.
    #[must_use]
    pub fn entity_type(&self) -> &ResourceKey {
        &self.entity_type
    }

    /// Sets the mob's canonical entity-type key. Used by a species-aware spawn
    /// driver so the encoded spawn packet names the right entity.
    pub fn set_entity_type(&mut self, entity_type: ResourceKey) -> &mut Self {
        self.entity_type = entity_type;
        self
    }

    /// The mob's body rotation (degrees). Body yaw tracks the movement
    /// direction; ground mobs keep a level body, so pitch is 0.
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        Rotation::new(self.mob.body_yaw(), 0.0)
    }

    /// The mob's head yaw in degrees — toward its look target if a goal set one,
    /// otherwise the body yaw. Matches `ClientEvent::EntityHeadRotation`.
    #[must_use]
    pub fn head_yaw(&self) -> f32 {
        self.mob.head_yaw()
    }

    /// The mob's velocity in **blocks per tick** (the unit vanilla's wire packing
    /// assumes), i.e. the position delta applied on the last tick.
    #[must_use]
    pub fn velocity(&self) -> Vec3 {
        self.mob.velocity()
    }

    /// Lowers the mob into a version-free [`EntitySnapshot`] for the encode seam.
    /// This is the whole identity/motion surface a [`ServerProtocol`] needs to
    /// build spawn/move/remove packets; the server holds the previous snapshot
    /// per connection so the protocol can stay stateless.
    ///
    /// Issue #425: `metadata` is the per-species entity-metadata field list —
    /// general across mobs (see [`MetadataField`]'s own doc comment), not a
    /// creeper-only mechanism, even though a creeper is the only producer
    /// today. [`crate::server::EntityStreamer::sync`] diffs this exactly like
    /// every other field here, so a change reaches [`ServerProtocol::encode_set_entity_data`]
    /// through the same spawn/update path `position`/`rotation` already use —
    /// no second wiring for the next mob that needs a metadata field.
    ///
    /// `CreeperSwellDir` is always included for a creeper, even at its `-1`
    /// default: unlike `CreeperIgnited` (monotonic — set once, never
    /// cleared, so *absence* safely means "still false"), `swell_dir` can
    /// legitimately return to `-1` mid-episode (`SwellGoal`'s retreat case),
    /// and that transition must reach the client exactly like the climb to
    /// `1` did — a client that keeps whatever `swell_dir` it was last sent
    /// would integrate the fuse in the wrong direction forever if a
    /// retreat-to-`-1` were ever skipped as "just the default".
    #[must_use]
    pub fn snapshot(&self) -> EntitySnapshot {
        let mut metadata = Vec::new();
        if self.entity_type.path() == "creeper" {
            metadata.push(MetadataField::CreeperSwellDir(self.swell_dir()));
            if self.is_ignited() {
                metadata.push(MetadataField::CreeperIgnited(true));
            }
        }
        EntitySnapshot {
            id: self.id,
            uuid: self.uuid,
            entity_type: self.entity_type.clone(),
            position: self.position(),
            rotation: self.rotation(),
            head_yaw: self.head_yaw(),
            velocity: self.velocity(),
            metadata,
        }
    }
}

/// Wire identity for one tracked projectile.
///
/// [`ProjectileRegistry`] (issue #211) deliberately stays version-free — its
/// own doc comment says a caller's `id`/ballistic state is all it tracks — so
/// the uuid and canonical entity-type key a spawn packet needs live here,
/// exactly the split [`SimMob`] already makes between `NavigatingMob`'s
/// version-free body and this crate's wire metadata.
#[derive(Debug, Clone)]
struct ProjectileMeta {
    uuid: Uuid,
    entity_type: ResourceKey,
}

/// Wire identity plus fall dynamics for one tracked dropped item.
///
/// [`ItemEntityRegistry`] (issue #215) tracks only the age/pickup-delay/count
/// *lifecycle* — deliberately world- and wire-free, per its own doc comment.
/// The item's identity and its [`ItemMotion`] (the fall-dynamics half that,
/// before this, only ever ran client-side for rendering — see
/// `crates/lodestone-shell/src/entities.rs`'s own `ItemMotion` import) live
/// here, the server-authoritative side that issue was missing.
#[derive(Debug, Clone)]
struct ItemState {
    uuid: Uuid,
    item: ResourceKey,
    motion: ItemMotion,
}

/// The result of [`MobSim::attack`] resolving a melee hit against a live mob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackOutcome {
    /// The target's remaining health after the hit (`0.0` if it died).
    pub health: f32,
    /// Whether this hit reduced health to `0.0` and removed the mob from the
    /// sim.
    pub killed: bool,
    /// Damage that actually reached health — `0.0` if the hit was fully
    /// ignored by the invulnerability-frame gate, matching
    /// [`SimMob::apply_damage`]'s own return convention.
    pub damage_dealt: f32,
    /// The target's velocity after knockback (unchanged from its pre-hit
    /// value whenever the call's `knockback_power` was `<= 0.0`), in
    /// blocks/tick — ready to encode on the next
    /// [`snapshots`](MobSim::snapshots) call.
    pub velocity: Vec3,
}

/// The server-side mob simulation: owns the live mobs and advances them.
///
/// The [`ChunkWorld`] is borrowed (the mobs path over it), so the caller holds
/// the world and hands it here. Drive the sim with [`tick`](MobSim::tick) once
/// per game tick, or [`tick_for`](MobSim::tick_for) to run many.
///
/// Also owns a [`ProjectileRegistry`] and an [`ItemEntityRegistry`] (issues
/// #211/#215): before this, `grep -rn 'ProjectileRegistry\|ItemEntityRegistry'`
/// outside `lodestone-entity` returned nothing — both types were fully
/// implemented and unit-tested but never constructed anywhere a real server
/// tick could reach, so arrows and dropped items never advanced on this
/// project's own server. `MobSim` is the same home the server's unified tick
/// loop ([`crate::tick::run_tick_loop`], issue #284) already ticks every
/// server tick for mobs, so folding these two in here (rather
/// than a sibling `ProjectileSim`) means [`tick`](MobSim::tick) closes the gap
/// with no new task, and [`snapshots`](MobSim::snapshots) puts every entity
/// kind on the same wire path mobs already proved reaches a real client.
#[derive(Debug)]
pub struct MobSim<'w> {
    world: &'w ChunkWorld,
    mobs: Vec<SimMob<'w>>,
    projectiles: ProjectileRegistry,
    projectile_meta: HashMap<i32, ProjectileMeta>,
    items: ItemEntityRegistry,
    item_state: HashMap<i32, ItemState>,
    next_id: i32,
    tick_count: u64,
    /// Every detonation [`tick`](Self::tick) has triggered since the last
    /// [`take_detonations`](Self::take_detonations) call (issue #425).
    /// `tick` itself has no wire access — it only knows `self.world` — so
    /// this is the handoff point a driver ([`crate::tick::run_tick_loop`])
    /// drains into an [`crate::tick::ExplosionFeed`] for a connection to
    /// turn into a real `EXPLODE` packet. See that method's own doc comment
    /// for why draining, not just reading, is what keeps a detonation from
    /// being broadcast twice.
    pending_detonations: Vec<Detonation>,
    /// Grazed blocks awaiting the driver's world mutation (issue #456), as
    /// `(mob block position, which of the two blocks)`.
    ///
    /// The same handoff shape as [`pending_detonations`](Self::pending_detonations)
    /// above, and for a stronger reason: this sim holds `world: &'w ChunkWorld`
    /// **immutably**, so [`tick`](Self::tick) structurally *cannot* apply the
    /// eat. Drained by [`take_grazes`](Self::take_grazes).
    ///
    /// Position is the mob's own block position, not the eaten block's, because
    /// the two `EatenBlock` variants are relative to it: `AtFeet` is that cell,
    /// `Below` is one down. Storing the mob's cell keeps the arithmetic with the
    /// consumer that knows what each variant means.
    pending_grazes: Vec<(BlockPos, EatenBlock)>,
    /// Hurt and death sounds awaiting the driver (issue #530), the same handoff
    /// shape as the two above and for the same reason: this sim owns no
    /// connection. Drained by [`take_vocalisations`](Self::take_vocalisations).
    ///
    /// Before this, `apply_damage` damaged and killed mobs with **no audible
    /// result at all** — the `ServerProtocol` trait had no sound encoder, so a
    /// player could beat a cow to death in silence.
    pending_vocalisations: Vec<crate::effects::WorldEffect>,
    /// Every connected player's perception-relevant state, refreshed by a
    /// driver through [`set_players`](Self::set_players) and consumed by
    /// [`tick`](Self::tick) to feed each mob's `nearest_player`/`temptation`.
    ///
    /// This crate had **no player-position feed at all** before issue #441 —
    /// see [`set_players`](Self::set_players) for why that made two of the
    /// eight perception methods unreachable, and which one line closes it.
    players: Vec<PlayerPerception>,
}

/// One detonation [`MobSim::tick`] triggered this tick, for
/// [`take_detonations`](MobSim::take_detonations) to hand a driver — the
/// minimum a [`ServerProtocol::encode_explode`](crate::protocol::ServerProtocol::encode_explode)
/// call needs. This crate tracks no block-destruction model, so there is
/// nothing else (a block list, a knockback vector) to carry yet; see that
/// method's own doc comment for exactly which vanilla `ClientboundExplodePacket`
/// fields are therefore stubbed rather than modelled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detonation {
    /// The blast's centre, in world space.
    pub centre: Vec3,
    /// The blast radius (`CREEPER_EXPLOSION_RADIUS` for every producer
    /// today).
    pub radius: f32,
}

// The integrated server owns the sim behind an `Arc<Mutex<…>>` and hands it to
// a `tokio::spawn`ed connection task as an `EntitySource`, which requires
// `Send`. `MobSim` stores goals as `Box<dyn Goal>`, so this holds only because
// `Goal: Send`; pin it here so a future `!Send` goal or field fails to compile
// with a clear pointer, instead of surfacing as an opaque spawn error at the
// call site.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<MobSim<'static>>();
};

impl<'w> MobSim<'w> {
    /// Creates an empty simulation over `world`.
    #[must_use]
    pub fn new(world: &'w ChunkWorld) -> Self {
        Self {
            world,
            mobs: Vec::new(),
            projectiles: ProjectileRegistry::new(),
            projectile_meta: HashMap::new(),
            items: ItemEntityRegistry::new(),
            item_state: HashMap::new(),
            next_id: 1,
            tick_count: 0,
            pending_detonations: Vec::new(),
            pending_grazes: Vec::new(),
            pending_vocalisations: Vec::new(),
            players: Vec::new(),
        }
    }

    /// Replaces the set of players mob perception can see, for
    /// [`tick`](Self::tick) to consume.
    ///
    /// # Why this exists, and what still has to call it
    ///
    /// Before issue #441 nothing in this crate knew where a player was.
    /// `MobSim::tick` takes no arguments and `run_tick_loop`
    /// (`crate::tick`) receives no player position either — the gap
    /// [`run_mob_tick_loop`]'s own doc comment already discloses for
    /// [`despawn_pass`](Self::despawn_pass). So
    /// [`MobController::nearest_player`] and
    /// [`MobController::temptation`] had no possible source, which is half of
    /// why `LookAtPlayerGoal` and `TemptGoal` were structurally dead.
    ///
    /// The producer is **one line in `crate::server::dispatch_play_packet`'s
    /// `ServerBound::PlayerMoved` arm**, which already holds both the new
    /// position and a `MobHandle` in the same scope. That line is not in this
    /// commit — `server.rs` is another agent's file this session — so until it
    /// lands these two methods are fed only by tests, and every other one of
    /// the eight is fed from state this crate already owns. That asymmetry is
    /// recorded in `docs/mob-perception.md` rather than left for the next
    /// author to rediscover.
    pub fn set_players(&mut self, players: Vec<PlayerPerception>) -> &mut Self {
        self.players = players;
        self
    }

    /// The players mob perception currently sees.
    #[must_use]
    pub fn players(&self) -> &[PlayerPerception] {
        &self.players
    }

    /// Overrides the id the next [`spawn`](Self::spawn) call assigns (and
    /// every one after it, still incrementing by one each time).
    ///
    /// Exists for a caller that shares its mob ids' wire namespace with a
    /// real protocol's own reserved ids. `MobSim::new`'s default start (`1`)
    /// collided, in production, with `V770ServerProtocol`'s
    /// `LOCAL_PLAYER_ENTITY_ID` (also `1`, `crates/protocol/v770/src/server_protocol.rs`):
    /// a real client never spawns "itself" as a separate `ADD_ENTITY`, so the
    /// very first mob a fresh [`MobSim`] ever spawns silently failed to
    /// appear — found live by `crates/protocol/v770/tests/live_mob_sim.rs`,
    /// which consistently observed 2 of 3 seeded mobs, never 3, until
    /// `run_mob_tick_loop` started calling this. `MobSim::new`'s default is
    /// left unchanged (`1`) so every existing hermetic test keeps its
    /// already-asserted ids stable; only a caller wired to a real wire
    /// protocol needs to call this.
    pub fn set_next_id(&mut self, next_id: i32) -> &mut Self {
        self.next_id = next_id;
        self
    }

    /// Spawns a mob at `pos` with body `shape`, moving `step_per_tick` blocks per
    /// tick (derived from its movement-speed attribute) and an A\* open-set
    /// budget of `visited_budget` (vanilla `floor(followRange * 16)`).
    ///
    /// Returns a mutable handle so the caller can attach goals and a target
    /// before the first tick.
    pub fn spawn(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
    ) -> &mut SimMob<'w> {
        let entity_type = ResourceKey::from_str("minecraft:zombie").expect("static key is valid");
        self.spawn_with_type(pos, shape, step_per_tick, visited_budget, entity_type)
    }

    /// The shared body of [`spawn`](Self::spawn) and
    /// [`spawn_species`](Self::spawn_species): everything except *which*
    /// `entity_type` (and therefore which [`combat_defaults`]) the new mob
    /// gets.
    fn spawn_with_type(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
        entity_type: ResourceKey,
    ) -> &mut SimMob<'w> {
        let id = self.next_id;
        self.next_id += 1;
        let (max_health, attack_damage, defenses, knockback_resistance) =
            combat_defaults(&entity_type);
        self.mobs.push(SimMob {
            id,
            mob: NavigatingMob::new(self.world, shape, pos, step_per_tick, visited_budget, id as u64),
            goals: GoalSelector::new(),
            category: MobCategory::Monster,
            no_action_time: 0,
            persistent: false,
            uuid: Uuid::new_v4(),
            entity_type,
            health: max_health,
            defenses,
            anger: None,
            attack_damage,
            hurt_cooldown: HurtCooldown::default(),
            attack_target_id: None,
            owner_id: None,
            knockback_resistance,
        });
        self.mobs.last_mut().expect("just pushed")
    }

    /// Spawns a mob of a specific vanilla species at `pos`, resolving its body
    /// and behaviour from real per-species data instead of the universal
    /// `minecraft:zombie` placeholder [`spawn`](Self::spawn) still uses for its
    /// own, unrelated existing callers (issue #205: `SimMob::entity_type`
    /// defaulted to zombie unconditionally and every spawned mob got an empty
    /// [`GoalSelector`], so two different species were behaviourally
    /// identical).
    ///
    /// * **Shape** comes from the real 26.2 dimension census
    ///   ([`lodestone_data::entity_dimensions`], keyed by
    ///   [`lodestone_data::entity_types::entity_type_id_parts`]) folded with the
    ///   type's `SCALE`/`STEP_HEIGHT` attributes — the same maths
    ///   [`crate::resolve_mob_shape`] uses for a version-aware caller, read
    ///   directly here since `MobSim` already depends on `lodestone_data` for
    ///   its path/collision census above. Falls back to `MobShape::land(0.6,
    ///   1.95)` for a species the census does not know by name, matching that
    ///   function's own "explicit fallback, never a silent guess" contract.
    /// * **Combat stats** come from [`combat_defaults`], already species-aware.
    /// * **Speed** is the type's `movement_speed` attribute value, read
    ///   directly as blocks/tick — the same convention
    ///   [`run_spawn_cycle`](Self::run_spawn_cycle)'s candidates and
    ///   [`seed_demo_mobs`]'s hardcoded `0.23` already use for a zombie.
    /// * **Goals** come from [`lodestone_entity::ai::roster`], which resolves the
    ///   species path to the goal set vanilla's own `registerGoals()` installs,
    ///   at vanilla's own priority numbers. This function no longer knows
    ///   anything about any individual species: a species with no roster entry
    ///   gets `roster::FALLBACK` (wander and look around), which is exactly the
    ///   baseline every species used to get here.
    ///
    ///   That matters beyond tidiness. Until the roster existed, `FloatGoal`,
    ///   `PanicGoal`, `BreedGoal`, `TemptGoal` and `FollowParentGoal` were
    ///   installed **only** by tests — implemented, unit-tested, and fed real
    ///   perception by [`tick`](Self::tick), with zero production call sites. A
    ///   cow could not panic or follow food in the running game no matter what
    ///   the perception feed reported. This is where that stopped being true.
    ///
    ///   Two consequences worth knowing when reading a mob's behaviour:
    ///   priorities here are vanilla's absolute numbers, so a creeper's
    ///   `SwellGoal` is at 2 and its `MeleeAttackGoal` at 4 rather than the `-1`
    ///   and `2` of the private scale this replaced; and the old
    ///   `step_per_tick.max(0.2)` floor on melee speed is gone, because vanilla
    ///   expresses speed as a multiplier on the mob's own `movement_speed` and
    ///   every hostile species in the roster is already above that floor
    ///   (slowest is a zombie's `0.23`).
    pub fn spawn_species(&mut self, entity_type: ResourceKey, pos: Vec3) -> &mut SimMob<'w> {
        let attrs = default_attributes(&entity_type).unwrap_or_else(AttributeMap::new);
        let shape = species_shape(&entity_type, &attrs);
        let step_per_tick = attr(&attrs, "movement_speed");
        // `minecraft:follow_range`, read **once** and fed to both consumers, so
        // target acquisition and the A* budget cannot drift apart (issue #455).
        //
        // `attr_present` rather than `attr`: for a species `default_attributes`
        // has no template for, `attrs` is empty and `attr` returns the *registry*
        // default of **32.0** — not 0.0, and not a harmless approximation. 32.0
        // is the single value this attribute never legitimately holds, because
        // `Mob.createMobAttributes()` overrides it to 16.0 for every mob
        // (`Mob.java:166-168`), so nothing in the game carries the registry
        // number. Falling back explicitly to `DEFAULT_FOLLOW_RANGE` is what makes
        // an unlisted species behave like a plain vanilla mob instead of like
        // nothing at all.
        //
        // Species that raise it do so in their own `createAttributes` — the
        // zombie family 35.0 (`monster/zombie/Zombie.java:133`), blaze 48.0,
        // enderman 64.0 — and `attribute.rs::type_spec` has arms for only
        // thirteen species (issue #457). So `zombie` gets its real 35.0 here
        // while `zombie_villager`, which vanilla also puts at 35.0, gets 16.0.
        // That is a **known wrong value on a connected wire**, tracked by #457
        // and gated below so it is visible rather than assumed; the fix is more
        // `type_spec` arms, not a fallback tuned to flatter the zombie family.
        let follow_range = attr_present(&attrs, "follow_range").unwrap_or(DEFAULT_FOLLOW_RANGE);
        let visited_budget = (follow_range * 16.0).floor() as i32;
        let hostile = is_hostile_species(&entity_type);

        // Built *before* `entity_type` is moved into the spawn, so the species
        // path is borrowed rather than cloned.
        let goals = roster::goals_for(entity_type.path(), &SpeciesContext::new(step_per_tick));

        let mob = self.spawn_with_type(pos, shape, step_per_tick, visited_budget, entity_type);
        mob.set_category(if hostile {
            MobCategory::Monster
        } else {
            MobCategory::Creature
        })
        .set_persistent(!hostile);
        for (priority, goal) in goals {
            mob.add_goal(priority, goal);
        }
        // The `FOLLOW_RANGE` attribute reaches the controller, which is what
        // bounds target acquisition (#455). Without this every hosted mob used
        // the seam's `DEFAULT_FOLLOW_RANGE`, so the zombie family — the only
        // family `seed_demo_mobs` spawns — targeted at 16 blocks instead of its
        // real 35.0. A wrong *value* on a fully connected wire, which is the
        // failure mode `cargo xtask connectedness` structurally cannot see.
        //
        // Set here rather than in `feed_perception` on purpose: this is a species
        // attribute resolved once at spawn, not per-tick perception. Putting it in
        // the feed would mean re-reading `default_attributes` for every mob every
        // tick, and would invite a second source of truth for a number
        // `visited_budget` above already derives from this exact read.
        mob.mob.set_follow_range(follow_range);
        mob
    }

    /// Advances every mob one tick: run its goals (which drive A\* and path
    /// following through the [`MobController`] seam), then step the follower.
    /// Each mob's `no_action_time` ages by one tick, mirroring vanilla
    /// `serverAiStep`'s `noActionTime++`, and is first cleared for any mob
    /// vanilla's `Mob.checkDespawn` would clear it for — a persistent mob, or
    /// one within its category's immune radius of a player from
    /// [`set_players`](Self::set_players). See the body for why that reset lives
    /// here rather than only in [`despawn_pass`](MobSim::despawn_pass), which
    /// has no production caller and left the counter monotonic — permanently
    /// disabling every idle-throttled goal five seconds into a world.
    ///
    /// A `MeleeAttackGoal` that connected this tick is resolved into a real
    /// [`SimMob::apply_damage`] call against whichever mob its
    /// [`attack_target_id`](SimMob::attack_target_id) names — the goal
    /// scheduler only ever produces the *intent* to strike (a position, via
    /// [`NavigatingMob::take_new_attacks`]); this is where that intent becomes
    /// a real health change. Resolution runs in a second pass over collected
    /// events, after every mob's own AI has ticked, so an attacker damaging
    /// another mob never needs two simultaneous mutable borrows into the same
    /// `Vec`. A mob whose health reaches `0.0` is removed at the end of the
    /// tick that killed it (vanilla's immediate death removal).
    pub fn tick(&mut self) {
        // Issue #441 (plan unit A2): feed every mob's perception inputs before
        // its goals run. Without this pass `NavigatingMob` reports the trait
        // defaults for `nearest_player`/`temptation`/`avoid_threat`/
        // `no_action_time`, and `partner_candidate`/`parent_candidate` stay
        // `None` forever — which made eight of the thirteen implemented goals
        // structurally incapable of firing in production. Ordering is
        // load-bearing: it must run *before* `m.mob.tick(&mut m.goals)` below,
        // because that call is what evaluates `can_use`.
        //
        // `no_action_time` ages *before* the feed, not after the goals, because
        // that is vanilla's own order: `Mob.serverAiStep()` opens with
        // `this.noActionTime++` and only then ticks the selectors
        // (`.cache/mc/26.2/src/net/minecraft/world/entity/Mob.java:715-717`), so
        // a goal sees the already-incremented value. Getting this backwards
        // costs exactly one tick of idle time — small, invisible to any
        // `cargo check`, and caught here only because
        // `no_action_time_crosses_the_seam_instead_of_staying_on_the_sim_record`
        // asserts the two readings are *equal* rather than merely both climbing.
        //
        // The reset half of vanilla `Mob.checkDespawn` runs *before* that
        // increment, because that is where `ServerLevel` puts it: it calls
        // `entity.checkDespawn()` every tick immediately before `entity.tick()`
        // (`.cache/mc/26.2/src/net/minecraft/server/level/ServerLevel.java:426-431`),
        // and `checkDespawn` is the **only** thing in vanilla that ever clears
        // `noActionTime` (`Mob.java:704-711`). So a mob standing next to a
        // player reads `1` here, never `2`.
        //
        // # Why this loop exists at all (the bug it fixes)
        //
        // Until now the increment above had no counterpart anywhere in
        // production. [`despawn_pass`](Self::despawn_pass) owns the same reset,
        // and it has **zero production callers** — `crate::tick::run_tick_loop`
        // never calls it, because it is handed no player position (a gap that
        // function's own doc comment discloses). So `no_action_time` was
        // monotonic for the whole life of a world, and crossed
        // `RandomStrollGoal`'s idle throttle of `100`
        // (`ai/goal/RandomStrollGoal.java:43`, our `goals.rs`'s
        // `no_action_time() >= 100` early return) after five seconds — after
        // which **no mob could ever stroll again**, which is why demo mobs
        // reached a connected client and then stood still forever
        // (`crates/protocol/v770/tests/live_mob_sim.rs`).
        //
        // It was total rather than intermittent because the throttle closed
        // before the goal's own `1/120` roll could succeed even once. **That
        // second half is now stale and is kept only as the record of why this
        // reset exists.** It read: *"every `NavigatingMob` shares one hardcoded
        // RNG seed (`SplitMix64(0x1234_5678_9ABC_DEF0)`, and `with_seed` has no
        // caller outside a test), and for that one stream the first draw where
        // `next_u64() % 120 == 0` is draw 130 — past the wall at 100 … The
        // shared seed is a separate defect in a crate this module does not
        // own."*
        //
        // That defect was fixed: issue #463 (`3b65cbf`) seeds each
        // `NavigatingMob` from its own id (`spawn_with_type` passes
        // `id as u64`), so the first hit is per-mob — draw 9 for id 1, 48 for
        // id 2, 147 for id 3. The consequence is that the *symptom* is no longer
        // uniform: a low-id mob now strolls before the throttle would have
        // closed, and only a mob whose first hit lands past 100 shows it at all.
        // Two gates in `tests/` had premises built on the old shared stream and
        // failed when the seed changed; `tests/mob_idle_throttle.rs` now selects
        // its subject's id deliberately, and its module doc carries the table.
        //
        // None of that changes what this reset is for: with it, a mob near a
        // player never reaches the throttle regardless of which stream it draws.
        //
        // Reusing [`check_despawn`] rather than restating its 32-block immune
        // radius: this call site wants only its `reset_timer` verdict, so it
        // passes `rng_hit_800: false` and **ignores `discard` entirely** —
        // removing a mob needs an RNG draw and is still `despawn_pass`'s job.
        // With `rng_hit_800` false the only `discard` arm left is gate A
        // (beyond `despawn_distance`), which never wants a reset either, so
        // dropping the field here cannot mask one.
        for m in &mut self.mobs {
            let pos = m.position();
            let nearest = self
                .players
                .iter()
                .map(|p| dist_sqr(p.position, pos))
                .min_by(f64::total_cmp);
            // Player proximity is the **only** reset condition here, and
            // deliberately *not* vanilla's other one.
            //
            // `Mob.checkDespawn`'s `else` branch does clear the timer every tick
            // for a mob that requires persistence (`Mob.java:710-711`), keyed on
            // `isPersistenceRequired() || requiresCustomPersistence()`. Keying
            // this off `SimMob::persistent` would look like a faithful port and
            // would not be one, because that flag carries a **wider** meaning
            // here than vanilla's: `spawn_species` sets it from `!hostile`, so
            // every passive animal is `persistent` in this crate. Vanilla animals
            // are not `isPersistenceRequired` — they opt out of distance
            // despawning through `Animal.removeWhenFarAway() == false`
            // (`animal/Animal.java:128`), which `checkDespawn` consults for
            // *discarding* and never for the timer. Only a name-tagged or
            // summoned mob takes vanilla's `else` branch.
            //
            // Including it therefore would not have been "more vanilla": it would
            // have given every cow, pig and sheep in the world a permanently open
            // idle throttle regardless of whether any player was near. Measured,
            // not reasoned — the first draft did include it, and
            // `tests/mob_sim.rs`'s
            // `no_action_time_crosses_the_seam_instead_of_staying_on_the_sim_record`
            // failed its own precondition, because its cow's counter could no
            // longer climb past 100 at all. `despawn_pass` treats `persistent` the
            // same way (an early `return true`, with no reset), so the two agree.
            //
            // Modelling vanilla's real persistence branch needs a flag that means
            // `isPersistenceRequired` and nothing else; that is a separate change
            // to what `spawn_species` records, not something to smuggle in here.
            let reset = nearest.is_some_and(|dist_sqr| {
                crate::mob_spawn::check_despawn(m.category, dist_sqr, m.no_action_time, false, true)
                    .reset_timer
            });
            if reset {
                m.no_action_time = 0;
            }
            m.no_action_time = m.no_action_time.saturating_add(1);
        }
        self.feed_perception();

        let mut hits: Vec<(Option<i32>, f32, Vec3)> = Vec::new();
        let mut detonations: Vec<(i32, Vec3)> = Vec::new();
        let mut bred: Vec<(i32, Vec3, ResourceKey)> = Vec::new();
        // Issue #456: accumulated into a local and moved into
        // `self.pending_grazes` after the loop, not pushed directly — `self` is
        // mutably borrowed by `&mut self.mobs` for the whole loop, exactly as it
        // is for `hits`/`detonations`/`bred`.
        let mut grazes: Vec<(BlockPos, EatenBlock)> = Vec::new();
        let mut launches: Vec<ProjectileLaunch> = Vec::new();
        // Issue #458, primitive 4: self-inflicted damage requests, drained per
        // mob and resolved below — see the resolution pass after `hits`.
        let mut self_damage: Vec<(i32, f32)> = Vec::new();
        for m in &mut self.mobs {
            // Vanilla ages `invulnerableTime`/`hurtTime` every tick regardless
            // of whether the mob was hit this tick.
            m.hurt_cooldown.tick();
            m.mob.tick(&mut m.goals);
            if !m.mob.take_new_attacks().is_empty() {
                // Carry the attacker's own position too, so the victim can
                // retaliate: vanilla's `hurt` sets `lastHurtByMob` from the
                // damage source's attacker (`LivingEntity.java:1358`), which is
                // what `HurtByTargetGoal` reads. Before #441 this tuple was
                // `(target, damage)` only, so a mob struck by another mob had
                // no way to learn who hit it and `HurtByTargetGoal` could never
                // fire even once the perception seam existed.
                hits.push((m.attack_target_id, m.attack_damage, m.position()));
            }
            if m.mob.take_detonated() {
                detonations.push((m.id, m.position()));
            }
            // Drain the "a `BreedGoal` connected this tick" flag. `breed()`
            // itself only records the *event* — the seam has no notion of the
            // partner's identity or of creating an entity — so resolving it
            // into a real child is this driver's job, and the step commit
            // `7bf2873` explicitly deferred to here.
            if m.mob.take_bred() {
                bred.push((m.id, m.position(), m.entity_type().clone()));
            }
            // Issue #456. The goal records *that* a block was eaten and which of
            // the two positions it was; it cannot mutate the world, because this
            // sim borrows `world: &'w ChunkWorld` immutably. So this takes the
            // same route `pending_detonations` does — accumulate here, and let
            // `crate::tick::run_tick_loop` (which owns mutable chunk access)
            // apply it. `docs/plans/…`/#238's plan says "a `MobSim::tick` drain";
            // that is not achievable as written, and this is why.
            for what in m.mob.take_new_eaten() {
                grazes.push((m.mob.block_position(), what));
            }
            launches.extend(m.mob.take_new_launches());
            for amount in m.mob.take_self_damage() {
                self_damage.push((m.id, amount));
            }
        }
        self.pending_grazes.extend(grazes);
        for launch in launches {
            use lodestone_entity::ai::roster::ranged::{integrates_as_arrow, projectile_entity_type};
            let projectile = if integrates_as_arrow(launch.kind) {
                Projectile::arrow(launch.origin, launch.velocity)
            } else {
                Projectile::throwable(launch.origin, launch.velocity)
            };
            let key = ResourceKey::from_str(&format!("minecraft:{}", projectile_entity_type(launch.kind)))
                .expect("static projectile key");
            self.spawn_projectile(key, projectile);
        }
        for (target_id, raw_damage, attacker_pos) in hits {
            if let Some(target_id) = target_id
                && let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id)
            {
                let applied = target.apply_damage(raw_damage, DamageFlags::default());
                target.mob.note_hurt(Some(attacker_pos));
                self.note_vocalisation(target_id, applied);
            }
        }
        // Issue #458, primitive 4: self-inflicted damage — the bee's sting
        // self-destruct (`animal/Bee.java:374-379`). `damage_self` only
        // records the intent; health lives here, so it is applied through the
        // same pipeline a melee hit uses (i-frames and armour reductions
        // included, matching vanilla's `hurtServer`). Resolved before the
        // retain below, so a mob that kills itself leaves the sim in the same
        // tick, exactly as a fatal melee hit does.
        for (id, amount) in self_damage {
            if let Some(m) = self.get_mut(id) {
                let applied = m.apply_damage(amount, DamageFlags::default());
                self.note_vocalisation(id, applied);
            }
        }
        self.reap_dead();
        self.resolve_breeding(bred);

        // Issue #213: `explode`'s exposure/damage maths was already correct
        // and already unit-tested, but had zero production callers anywhere
        // — a creeper's own fuse reaching `MAX_SWELL`
        // (`NavigatingMob::take_detonated`, driven by `SwellGoal`/`ignite`)
        // is the first one. Vanilla's `explodeCreeper`
        // (`Creeper.java:230-239`) unconditionally discards the creeper
        // alongside the blast (`this.dead = true; ...; this.discard();`), so
        // the explicit retain below does not rely on the creeper taking
        // lethal self-damage from its own blast — a wall could shield it
        // from its own explosion exactly as it shields any other mob, and
        // vanilla's `discard()` has no such exception.
        for (id, pos) in detonations {
            self.explode(pos, CREEPER_EXPLOSION_RADIUS, DamageFlags::default());
            self.mobs.retain(|m| m.id != id);
            // Issue #425: before this, nothing recorded that a detonation
            // happened at all beyond the damage `explode` itself just
            // applied — a connected client had no way to learn "an
            // explosion happened here" (no particle, no sound), because
            // `tick` discarded this entirely. See `take_detonations`'s own
            // doc comment for the drain side.
            self.pending_detonations.push(Detonation {
                centre: pos,
                radius: CREEPER_EXPLOSION_RADIUS,
            });
        }

        // Issues #211/#215: `ProjectileRegistry`/`ItemEntityRegistry` existed
        // and were unit-tested but nothing called their `tick` from a real
        // per-tick driver. `MobSim::tick` is that driver in production (see
        // `run_mob_tick_loop` below), so advancing both here is what actually
        // closes the island, not a hermetic test calling `tick` on the
        // registry directly.
        self.projectiles.tick();
        for despawned_item_id in self.items.tick() {
            self.item_state.remove(&despawned_item_id);
        }
        // Issue #533: **items land.** `ItemMotion::tick` is the entity's own
        // motion — gravity, translate, drag — and its doc comment has always said
        // "block collision that would zero a component is the world crate's job
        // and is expressed here through `on_ground`". Nothing ever did that job:
        // `on_ground` was set `false` by `ItemMotion::new` and never written
        // again, so every dropped item accelerated downward forever, fell through
        // the terrain, and streamed to the client until its 6000-tick despawn.
        //
        // That is also why merging never happened. `merge_neighbouring_items`
        // requires `|dy| < ITEM_MERGE_REACH_Y` (0.25), and two stacks dropped even
        // one tick apart fall at permanently different speeds — so the vertical
        // test could never pass for anything but two items spawned on the same
        // tick. Settling them onto a surface is what makes the merge reachable,
        // which is why #533's two halves are one fix.
        let world = self.world;
        let mut fell_out_of_the_world: Vec<i32> = Vec::new();
        for (&id, state) in &mut self.item_state {
            state.motion.tick();
            settle_item(world, &mut state.motion);
            if state.motion.position.y < f64::from(world.min_y) - VOID_DESPAWN_DEPTH {
                fell_out_of_the_world.push(id);
            }
        }
        // `Entity.checkBelowWorld`'s discard, and not merely tidiness: an item
        // that escapes the world (a column the snapshot does not cover, so
        // `is_solid` is false everywhere) would otherwise keep being ticked and
        // streamed for its full 6000-tick life at ever-increasing depth.
        for id in fell_out_of_the_world {
            self.item_state.remove(&id);
            self.items.remove(id);
        }
        self.merge_neighbouring_items();

        self.tick_count += 1;
    }

    /// Populates every mob's [`MobController`] perception inputs from this
    /// sim's own census plus [`set_players`](Self::set_players)' player list.
    ///
    /// Two passes, and the split is a borrow-checker necessity rather than a
    /// style choice: deciding mob `i`'s threat/partner/parent means reading
    /// every *other* mob, so the decisions are computed under shared borrows
    /// first and applied under a mutable one second. It is the same shape
    /// [`tick`](Self::tick) already uses for melee resolution.
    ///
    /// Nothing here is species-*goal* knowledge — that is the roster's job.
    /// The only species table it consults is [`avoided_species`], which answers
    /// "is that a threat to me", a perception question.
    fn feed_perception(&mut self) {
        let n = self.mobs.len();
        let mut nearest_player = vec![None; n];
        let mut temptation = vec![None; n];
        let mut threat = vec![None; n];
        let mut partner = vec![None; n];
        let mut parent = vec![None; n];
        let mut owner = vec![None; n];

        // --- persistent anger (issue #458, primitive 1) --------------------
        //
        // Resolved here, in the feed, for the same reason every other
        // pre-computed answer is: `MobController::angry_target` hands the goal
        // an `Option<Vec3>`, never a query, because the seam has no shared game
        // clock to compare an absolute deadline against. So the host does the
        // comparison and only the answer crosses.
        //
        // `now >= end_time` clears the grudge outright rather than merely
        // reporting `None`, mirroring vanilla's `stopBeingAngry` — a grudge
        // that expired must not come back if the clock is ever read again.
        let now = self.tick_count;
        for me in &mut self.mobs {
            if me.anger.is_some_and(|a| now >= a.end_time) {
                me.anger = None;
            }
            let target = me.anger.map(|a| a.target);
            me.mob.set_angry_target(target);
        }

        for i in 0..n {
            let me = &self.mobs[i];
            let pos = me.position();
            let species = me.entity_type().path().to_owned();

            // --- nearest player -------------------------------------------
            // Fed with **no range cut**, deliberately: vanilla's range for this
            // lives in the *goal*'s targeting conditions (`LookAtPlayerGoal`
            // takes a `lookDistance`, 6.0F or 8.0F per species —
            // `ai/goal/LookAtPlayerGoal.java:44-46`), not on the mob, and our
            // `LookAtPlayerGoal::can_use` applies exactly that cut itself
            // (`goals.rs`). Cutting here as well would silently take the
            // minimum of two ranges and make the goal's own parameter a lie.
            nearest_player[i] = nearest_by(&self.players, pos, |p| p.position, |_| true, None);

            // --- temptation -----------------------------------------------
            // The range *is* on the mob here (`Attributes.TEMPT_RANGE`), so it
            // belongs in the feed. See `TEMPT_RANGE`.
            //
            // The item test is per-species (`tempt_food`), which is why
            // `PlayerPerception` carries the held item rather than a boolean:
            // the same wheat that tempts a cow does nothing to a chicken.
            let foods = tempt_food(&species);
            if !foods.is_empty() {
                temptation[i] = nearest_by(
                    &self.players,
                    pos,
                    |p| p.position,
                    |p| {
                        p.held_item
                            .as_ref()
                            .is_some_and(|item| foods.contains(&item.path()))
                    },
                    Some((TEMPT_RANGE, TEMPT_RANGE)),
                );
            }

            // --- avoid threat ---------------------------------------------
            let avoided = avoided_species(&species);
            if !avoided.is_empty() {
                threat[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id != me.id && avoided.contains(&other.entity_type().path()),
                    Some((AVOID_RANGE, AVOID_RANGE_Y)),
                );
            }

            // --- breeding partner -----------------------------------------
            // Vanilla `Animal.canMate` (`animal/Animal.java:202-206`): the
            // partner must be the *same class* and both must be in love. A
            // baby cannot breed (`Animal.canFallInLove` gates on age), and
            // `BreedGoal.canContinueToUse` additionally requires the partner
            // not be panicking (`ai/goal/BreedGoal.java:43`) — enforced here
            // too, since feeding a panicking partner would start the goal only
            // for it to abort on the next tick.
            if me.is_in_love() && !me.is_baby() {
                partner[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && other.is_in_love()
                            && !other.is_baby()
                            && !other.is_panicking()
                    },
                    Some((BREED_RANGE, BREED_RANGE)),
                );
            }

            // --- parent ---------------------------------------------------
            // `ai/goal/FollowParentGoal.java:23` (`getAge() >= 0` → no goal)
            // and `:34` (candidate must itself have `getAge() >= 0`, i.e. be an
            // adult), searched over `inflate(8.0, 4.0, 8.0)` at `:29`.
            if me.is_baby() {
                parent[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && !other.is_baby()
                    },
                    Some((FOLLOW_PARENT_RANGE, FOLLOW_PARENT_RANGE_Y)),
                );
            }

            // --- owner ----------------------------------------------------
            // Issue #458, primitive 5. The owner *identity* is a census fact
            // (`SimMob::owner_id`); only the resolved position can cross the
            // seam (`MobController::owner_position`), so this is resolved here
            // exactly like partner/parent. Vanilla's owner is a player
            // (`TamableAnimal.DATA_OWNERUUID_ID`), and player identity does not
            // exist at this seam — `PlayerPerception` carries only position and
            // held item — so today this can only resolve mob-to-mob ownership.
            // Player ownership stays blocked until `PlayerPerception` grows an
            // identity; until then a tamed-by-player mob reads `None`, which is
            // the correct neutral default rather than a wrong feed.
            if let Some(oid) = me.owner_id {
                owner[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id == oid,
                    None,
                );
            }
        }

        for (i, m) in self.mobs.iter_mut().enumerate() {
            m.mob
                .set_nearest_player(nearest_player[i])
                .set_temptation(temptation[i])
                .set_avoid_threat(threat[i])
                // The sim has incremented this every tick since long before
                // #441, but only on its own record — it never crossed the
                // `MobController` seam, so `RandomStrollGoal`'s idle
                // suppression read the trait default `0` and never fired.
                .set_no_action_time(m.no_action_time)
                .set_love_partner_candidate(partner[i])
                .set_parent_candidate(parent[i])
                .set_owner(owner[i]);
        }
    }

    /// Turns each drained [`NavigatingMob::take_bred`] event into a real child
    /// mob and applies vanilla's post-breeding cooldown to **both** parents.
    ///
    /// Vanilla `Animal.finalizeSpawnChildFromBreeding`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/animal/Animal.java:225-228`)
    /// does three things: `setAge(PARENT_AGE_AFTER_BREEDING)` on both parents,
    /// `resetLove()` on both, and spawns the child. `NavigatingMob::breed` can
    /// only do the love reset on the mob that ran the goal — it has no notion
    /// of the partner or of creating an entity — so the other two are here.
    ///
    /// Identifying the partner is the interesting part: by the time this runs,
    /// `breed()` has already cleared the breeder's love state, so "the other
    /// mob still in love" is not a usable key. It uses proximity instead —
    /// vanilla only breeds when the pair is within
    /// [`BREED_DISTANCE_SQR`](BREED_DISTANCE_SQR) (`ai/goal/BreedGoal.java:57`),
    /// so the nearest same-species adult inside that radius *is* the partner.
    fn resolve_breeding(&mut self, bred: Vec<(i32, Vec3, ResourceKey)>) {
        if bred.is_empty() {
            return;
        }
        // A mob already consumed as someone else's partner must not breed
        // again this tick. Both animals of a pair can legitimately reach
        // `loveTime >= 60` on the same tick — each holds the other as its
        // partner candidate — and without this guard one mating produces two
        // children, doubling the population every time.
        let mut consumed: std::collections::HashSet<i32> = std::collections::HashSet::new();
        for (breeder_id, breeder_pos, species) in bred {
            if consumed.contains(&breeder_id) {
                continue;
            }
            let partner_id = self
                .mobs
                .iter()
                .filter(|m| {
                    m.id != breeder_id
                        && m.entity_type().path() == species.path()
                        && !m.is_baby()
                        && !consumed.contains(&m.id)
                        && dist_sqr(m.position(), breeder_pos) < BREED_DISTANCE_SQR
                })
                .min_by(|a, b| {
                    dist_sqr(a.position(), breeder_pos)
                        .total_cmp(&dist_sqr(b.position(), breeder_pos))
                })
                .map(SimMob::id);

            consumed.insert(breeder_id);
            for id in [Some(breeder_id), partner_id].into_iter().flatten() {
                consumed.insert(id);
                if let Some(m) = self.get_mut(id) {
                    m.set_age(PARENT_AGE_AFTER_BREEDING);
                    m.mob.reset_love();
                }
            }

            // The child spawns through `spawn_species`, not `spawn_with_type`,
            // so it inherits the same goal set and category any other mob of
            // its species gets — a child that could not act would be a fresh
            // island of exactly the kind this issue exists to close.
            let child = self.spawn_species(species, breeder_pos);
            child.set_age(BABY_START_AGE);
        }
    }

    /// Runs [`tick`](MobSim::tick) `n` times.
    pub fn tick_for(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// Drains and returns every [`Detonation`] [`tick`](Self::tick) has
    /// triggered since the last call (issue #425) — the handoff
    /// [`crate::tick::run_tick_loop`] uses to publish onto an
    /// [`crate::tick::ExplosionFeed`] every server tick, mirroring how
    /// [`items`](Self::item_count)' own despawn ids are drained rather than
    /// merely read. Draining (not just reading) is what keeps a detonation
    /// from being broadcast twice if a caller is slow to call this before
    /// the next [`tick`](Self::tick) runs.
    pub fn take_detonations(&mut self) -> Vec<Detonation> {
        std::mem::take(&mut self.pending_detonations)
    }

    /// Drains every hurt/death sound recorded since the last call (issue #530).
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not play the same hit twice.
    pub fn take_vocalisations(&mut self) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut self.pending_vocalisations)
    }

    /// Records the hurt or death sound for a hit that landed on mob `id`
    /// (issue #530) — vanilla's `LivingEntity.hurt`/`die` playing
    /// `getHurtSound()`/`getDeathSound()`.
    ///
    /// Called from every funnel that applies damage rather than from
    /// [`SimMob::apply_damage`] itself, because the queue lives on the sim and
    /// `apply_damage` holds only the one mob. `applied <= 0.0` (a hit fully
    /// swallowed by i-frames or absorption) is silent, matching vanilla's own
    /// `hurtServer` returning before the sound.
    ///
    /// **Must be called before the end-of-tick `retain`**, or a killing blow
    /// finds no mob to read the species and position from and dies silently.
    fn note_vocalisation(&mut self, id: i32, applied: f32) {
        if applied <= 0.0 {
            return;
        }
        let Some(mob) = self.mobs.iter().find(|m| m.id == id) else {
            return;
        };
        // Vanilla draws pitch from the level RNG; this sim's only clock is
        // `tick_count`, and consuming from a shared generator here would shift
        // every other draw. Mixed with the id so two mobs hit in one tick differ.
        let phase = (self.tick_count.wrapping_mul(31).wrapping_add(id as u64)) % 21;
        let pitch = 0.9 + phase as f32 * 0.01;
        let effect = crate::effects::mob_vocalisation(
            mob.entity_type.to_string().as_str(),
            mob.position(),
            mob.health <= 0.0,
            mob.category == MobCategory::Monster,
            pitch,
            self.tick_count as i64,
        );
        if let Some(effect) = effect {
            self.pending_vocalisations.push(effect);
        }
    }

    /// Drains every graze [`tick`](Self::tick) has recorded since the last call
    /// (issue #456), as `(mob block position, which block)`.
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not apply the same eat twice — and it exists
    /// at all because this sim cannot apply it itself: `world: &'w ChunkWorld` is
    /// an immutable borrow.
    ///
    /// # What the consumer owes vanilla
    ///
    /// Per `ai/goal/EatBlockGoal.java:59-80`, with `mobGriefing` on:
    ///
    /// * [`EatenBlock::AtFeet`] → destroy the block at that cell, **no drops**
    ///   (`destroyBlock(pos, false)`).
    /// * [`EatenBlock::Below`] → set the cell one down to `minecraft:dirt`, plus
    ///   level event `2001` for the break particles.
    ///
    /// And the part worth not re-deriving: vanilla calls `mob.ate()` **even when
    /// `mobGriefing` suppresses the block change**, so the wool-regrowth effect
    /// and the world mutation are separable — the gamerule check belongs on the
    /// consumer, never in the goal.
    ///
    /// Nothing drains this yet, which is the honest state: #238's remaining half
    /// is `Sheep.ate()`'s wool regrowth (`setSheared(false)` + `ageUp(60)`), which
    /// is entity metadata on the wire.
    pub fn take_grazes(&mut self) -> Vec<(BlockPos, EatenBlock)> {
        std::mem::take(&mut self.pending_grazes)
    }

    /// The number of ticks advanced so far.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Applies an explosion centred at `centre` with blast `radius` (TNT is
    /// `4.0`) to every live mob, through the real ray-sampled exposure model
    /// (`explosion::seen_percent`, sampled against the sim's own
    /// [`ChunkWorld`] via its [`RayView`] impl) and damage formula
    /// (`explosion::entity_damage`), landing through the same
    /// [`SimMob::apply_damage`] pipeline a melee hit uses. Before this,
    /// `explosion.rs` had no consumer anywhere in the tree — its exposure grid
    /// and damage formula were exercised only by their own hermetic unit
    /// tests, with no path from "an explosion happened" to a health value
    /// anywhere changing.
    ///
    /// `flags` lets the caller pick which reduction stages the blast bypasses;
    /// a plain `DamageFlags::default()` runs armour/absorption normally.
    ///
    /// Returns `(id, damage_dealt)` for every mob that took nonzero damage,
    /// and removes any mob the blast killed. A mob whose exposure is fully
    /// blocked (every sampled ray hits terrain before the centre) takes no
    /// damage and is absent from the result — a wall genuinely shields it,
    /// this is not a distance cutoff.
    pub fn explode(&mut self, centre: Vec3, radius: f32, flags: DamageFlags) -> Vec<(i32, f32)> {
        let mut dealt = Vec::new();
        for m in &mut self.mobs {
            let shape = m.shape();
            let box_ = ExplosionAabb::from_size(
                m.position(),
                f64::from(shape.width),
                f64::from(shape.height),
            );
            let box_center = Vec3::new(
                (box_.min.x + box_.max.x) / 2.0,
                (box_.min.y + box_.max.y) / 2.0,
                (box_.min.z + box_.max.z) / 2.0,
            );
            let exposure = seen_percent(centre, box_, self.world);
            if exposure <= 0.0 {
                continue;
            }
            let distance = (box_center - centre).length();
            let raw = entity_damage(radius, distance, exposure);
            if raw <= 0.0 {
                continue;
            }
            let applied = m.apply_damage(raw, flags);
            if applied > 0.0 {
                dealt.push((m.id, applied));
            }
        }
        // Issue #530, after the loop rather than inside it: `note_vocalisation`
        // needs `&mut self` while the loop holds `&mut self.mobs`, and it must
        // still precede the retain below so a mob the blast killed is read for
        // its death sound before it leaves.
        for &(id, applied) in &dealt {
            self.note_vocalisation(id, applied);
        }
        self.reap_dead();
        dealt
    }

    /// Resolves a melee attack against a live mob: runs the damage pipeline
    /// ([`SimMob::apply_damage`]) and, if `knockback_power` is positive, the
    /// knockback impulse
    /// ([`lodestone_physics::knockback::knockback_impulse`]), writing both
    /// straight into the target's own state so the very next
    /// [`snapshots`](Self::snapshots) call — and therefore the next entity
    /// packet any connection tracking this mob receives — carries the
    /// result. This is issue #12's actual missing hop: `SimMob::apply_damage`
    /// already existed and was already correct, reached only by AI-driven
    /// `MeleeAttackGoal` hits and explosions; this is the first path a
    /// *player's* attack can reach it through.
    ///
    /// `attacker_pos` supplies the knockback *direction* only (the horizontal
    /// vector from attacker to target) — see `crate::server::apply_attack`'s
    /// own doc comment for why this substitutes for
    /// `lodestone_physics::knockback::attack_direction`'s real
    /// attacker-facing formula (nothing server-side tracks player rotation
    /// yet) and for why that is a materially smaller divergence than it
    /// sounds: a melee attack requires the crosshair to already be on the
    /// target, so facing and attacker→target are nearly always the same
    /// vector in practice.
    ///
    /// A mob's own [`NavigatingMob`] follower has no ground-contact state
    /// (see that struct's own doc comment: "kinematic... not the physics
    /// integrator" — it always snaps to its waypoint's floor), so this always
    /// takes `knockback_impulse`'s grounded branch (the `0.4`-capped vertical
    /// hop), matching the common case of a hit landing on a walking mob.
    ///
    /// Returns `None` if `target_id` names no live mob. Returns `Some` for
    /// every resolved hit, including a fully-ignored one (still inside
    /// i-frames — see [`AttackOutcome::damage_dealt`]) so a caller can always
    /// tell "no such mob" from "hit landed on nothing new" without a second
    /// lookup. A killing blow removes the mob from the sim immediately
    /// (vanilla's own immediate death removal — the same behaviour
    /// [`tick`](Self::tick)'s own end-of-tick retain already gives an
    /// AI-driven kill), not deferred to the next [`tick`](Self::tick).
    pub fn attack(
        &mut self,
        target_id: i32,
        attacker_pos: Vec3,
        raw_damage: f32,
        flags: DamageFlags,
        knockback_power: f64,
    ) -> Option<AttackOutcome> {
        // Read before the mutable borrow below: the grudge deadline is
        // absolute, so it needs the clock as of this tick.
        let now = self.tick_count;
        let (health, velocity, damage_dealt) = {
            let mob = self.get_mut(target_id)?;
            let damage_dealt = mob.apply_damage(raw_damage, flags);
            // Issue #441: the retaliation half of the damage record. This is
            // the *player's* attack path (`crate::server::apply_attack` is its
            // only production caller), so this one line is what makes a mob hit
            // by a player actually turn on them through `HurtByTargetGoal` —
            // and it needs no new plumbing, because `attacker_pos` was already
            // a parameter here for knockback direction.
            mob.mob.note_hurt(Some(attacker_pos));
            // Issue #458, primitive 1. Vanilla's `NeutralMob.setLastHurtByMob`
            // starts a persistent grudge alongside the retaliation record, so
            // the two begin at the same instant and by the same event.
            //
            // Started for **every** mob, with no species list. That is #455's
            // structural route deliberately reused: only a species whose
            // jar-cited roster registers an anger-gated target row can ever
            // *read* `angry_target`, so an always-hostile zombie carrying an
            // unread grudge is inert, whereas a name list here would be one
            // more `is_hostile_species` waiting to go stale.
            let end_time = now + grudge_ticks(&mut mob.mob);
            mob.anger = Some(Anger {
                end_time,
                target: attacker_pos,
            });
            if knockback_power > 0.0 && mob.health() > 0.0 {
                let target_pos = mob.position();
                let dx = target_pos.x - attacker_pos.x;
                let dz = target_pos.z - attacker_pos.z;
                let v = mob.velocity();
                let new_velocity = lodestone_physics::knockback::knockback_impulse(
                    lodestone_physics::geometry::Vec3d { x: v.x, y: v.y, z: v.z },
                    true, // always the grounded branch — see this method's own doc comment.
                    knockback_power,
                    dx,
                    dz,
                    mob.knockback_resistance(),
                    // A degenerate (attacker and target share an exact
                    // horizontal position) direction is possible here, unlike
                    // `attack_direction`'s facing-derived one — see
                    // `knockback_impulse`'s own doc comment. A fixed,
                    // deterministic non-degenerate fallback (rather than a
                    // threaded RNG this call site has no source for) is
                    // sufficient: it only ever fires on that one pathological
                    // input, and `knockback_impulse`'s own test
                    // (`knockback_loops_the_jitter_until_a_non_degenerate_direction_lands`)
                    // already proves a single non-degenerate draw is enough
                    // to terminate the loop.
                    || (1.0, 0.0),
                );
                mob.apply_knockback(Vec3::new(new_velocity.x, new_velocity.y, new_velocity.z));
            }
            (mob.health(), mob.velocity(), damage_dealt)
        };
        // Issue #530: before the removal below, so a killing blow is read for
        // its death sound rather than finding no mob.
        self.note_vocalisation(target_id, damage_dealt);
        let killed = health <= 0.0;
        if killed {
            // Through `reap_dead`, not a bare retain: a melee kill must drop the
            // same loot an explosion kill does. Health is already `0.0` here, so
            // the shared reaper picks exactly this mob out.
            self.reap_dead();
        }
        Some(AttackOutcome {
            health,
            killed,
            damage_dealt,
            velocity,
        })
    }

    /// The number of live mobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mobs.len()
    }

    /// Whether the simulation has no mobs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }

    /// A mob by id, if present.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&SimMob<'w>> {
        self.mobs.iter().find(|m| m.id == id)
    }

    /// A mob by id, mutably, if present.
    pub fn get_mut(&mut self, id: i32) -> Option<&mut SimMob<'w>> {
        self.mobs.iter_mut().find(|m| m.id == id)
    }

    /// The world this sim's mobs path over. Exposed so a caller holding only
    /// a `&mut MobSim` (e.g. [`MobHandle::with`]) can still reach terrain —
    /// see [`seed_demo_mobs`]'s use of this to resolve spawn-surface Y
    /// without a second, separately-threaded `&ChunkWorld` parameter.
    #[must_use]
    pub(crate) fn world(&self) -> &'w ChunkWorld {
        self.world
    }

    /// The position of the mob with `id`, if present.
    #[must_use]
    pub fn position(&self, id: i32) -> Option<Vec3> {
        self.get(id).map(SimMob::position)
    }

    /// Iterates the live mobs.
    pub fn iter(&self) -> impl Iterator<Item = &SimMob<'w>> {
        self.mobs.iter()
    }

    /// Runs one despawn check over every non-persistent mob, given the nearest
    /// player's position (vanilla `getNearestPlayer(-1.0)`), removing mobs the
    /// two distance gates discard and resetting the age timer of any within the
    /// immune radius.
    ///
    /// `nearest_player` is `None` when no player is loaded, in which case vanilla
    /// runs no despawn logic at all — the mobs are simply kept. The `1/800`
    /// gate-B roll is drawn per candidate mob from `rng`, matching vanilla's
    /// per-mob `random.nextInt(800)`.
    ///
    /// Returns the number of mobs discarded.
    pub fn despawn_pass(&mut self, nearest_player: Option<Vec3>, rng: &mut SpawnRng) -> usize {
        let Some(player) = nearest_player else {
            return 0;
        };
        let before = self.mobs.len();
        self.mobs.retain_mut(|m| {
            if m.persistent {
                return true;
            }
            let dist_sqr = dist_sqr(m.mob.position(), player);
            let rng_hit_800 = rng.next_int(800) == 0;
            let outcome = check_despawn(m.category, dist_sqr, m.no_action_time, rng_hit_800, true);
            match outcome {
                DespawnOutcome { discard: true, .. } => false,
                DespawnOutcome {
                    reset_timer: true, ..
                } => {
                    m.no_action_time = 0;
                    true
                }
                _ => true,
            }
        });
        before - self.mobs.len()
    }

    /// Runs one natural-spawn cycle over `chunks`, respecting the per-category
    /// global caps in `state`.
    ///
    /// For each chunk and each spawnable category still under its cap, the
    /// [`SpawnCandidateSource`] is asked for a candidate; if it supplies one the
    /// mob is spawned, tagged with its category, and counted so the cap is
    /// honoured for the rest of the cycle. Nothing here decides *which* mob or
    /// *where* — that is the source's version/terrain-dependent job.
    ///
    /// Every naturally spawned mob gets a baseline goal set
    /// ([`RandomStrollGoal`] + [`RandomLookAroundGoal`]) so it actually moves
    /// and looks around instead of standing frozen at its spawn point — before
    /// this, a mob produced by this cycle had an empty [`GoalSelector`] and
    /// [`tick`](MobSim::tick) on it was a provable no-op. Combat goals are not
    /// added here: they need a target (a player or another mob) this
    /// version-free crate has no notion of naming yet, so a caller that does
    /// (species-aware spawning) adds them via [`SimMob::add_goal`] on the
    /// returned handle's id.
    ///
    /// Returns the number of mobs spawned.
    pub fn run_spawn_cycle(
        &mut self,
        state: &mut SpawnState,
        source: &mut dyn SpawnCandidateSource,
        chunks: &[(i32, i32)],
    ) -> usize {
        let mut spawned = 0;
        for &(cx, cz) in chunks {
            for category in MobCategory::SPAWNING {
                if !state.can_spawn(category) {
                    continue;
                }
                if let Some(candidate) = source.candidate(category, cx, cz) {
                    let speed = candidate.step_per_tick;
                    let mob = self.spawn(
                        candidate.pos,
                        candidate.shape,
                        candidate.step_per_tick,
                        candidate.visited_budget,
                    );
                    mob.set_category(category)
                        .set_persistent(category.is_persistent())
                        .add_goal(0, Box::new(RandomStrollGoal::new(speed)))
                        .add_goal(1, Box::new(RandomLookAroundGoal::new()));
                    state.record(category);
                    spawned += 1;
                }
            }
        }
        spawned
    }

    /// Builds a [`SpawnState`] for `spawnable_chunks` from a census of the mobs
    /// currently alive, exactly as vanilla rebuilds `SpawnState` each cycle.
    #[must_use]
    pub fn census(&self, spawnable_chunks: i32) -> SpawnState {
        let mut state = SpawnState::new(spawnable_chunks);
        for m in &self.mobs {
            state.record(m.category);
        }
        state
    }

    /// Registers a ballistic projectile (arrow, snowball, ender pearl, …) at
    /// its current [`Projectile::position`]/[`Projectile::velocity`] so
    /// [`tick`](Self::tick) advances it every server tick and
    /// [`snapshots`](Self::snapshots) puts it on the wire — the "spawned on
    /// launch" half of issue #211. `entity_type` is the wire identity (e.g.
    /// `minecraft:arrow`); the ballistic family/constants are whatever
    /// `Projectile::arrow`/`::throwable`/`::snowball`/… the caller already
    /// picked.
    ///
    /// Returns the assigned entity id. Hit detection against terrain/entities
    /// and the resulting damage/area-effect are **not** done here — that
    /// needs world/entity data this crate does not thread through yet and is
    /// explicit follow-up (the impact half of #211), the same scope cut
    /// `ProjectileRegistry`'s own doc comment already names. Call
    /// [`remove_projectile`](Self::remove_projectile) once an impact pass
    /// exists.
    pub fn spawn_projectile(&mut self, entity_type: ResourceKey, projectile: Projectile) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.projectiles.spawn(id, projectile);
        self.projectile_meta.insert(
            id,
            ProjectileMeta {
                uuid: Uuid::new_v4(),
                entity_type,
            },
        );
        id
    }

    /// Removes a tracked projectile (impact or manual despawn), returning its
    /// last ballistic state if it was tracked.
    pub fn remove_projectile(&mut self, id: i32) -> Option<TrackedProjectile> {
        self.projectile_meta.remove(&id);
        self.projectiles.remove(id)
    }

    /// The number of tracked projectiles.
    #[must_use]
    pub fn projectile_count(&self) -> usize {
        self.projectiles.len()
    }

    /// The current position of a tracked projectile, if any.
    #[must_use]
    pub fn projectile_position(&self, id: i32) -> Option<Vec3> {
        self.projectiles.get(id).map(|p| p.position)
    }

    /// Removes every mob at or below zero health, rolling its death loot table
    /// on the way out (issue #272 — the mob half of #337's loot chain).
    ///
    /// **This is the crate's only mob-removal-by-death path, deliberately.**
    /// Before it, four separate `self.mobs.retain(|m| m.health > 0.0)` sites
    /// dropped a dead mob on the floor, and adding loot to one of them would have
    /// meant a cow killed by a melee hit dropping leather while a cow killed by a
    /// creeper dropped nothing — the same defect in three places. Every removal
    /// now funnels through here, so a new death cause gets drops for free.
    ///
    /// Vanilla's chain is `LivingEntity.die` → `dropAllDeathLoot` →
    /// `dropFromLootTable` → `Entity.spawnAtLocation`: the table is
    /// `entities/<type>` ([`crate::block_drops::mob_loot_table_id`]) and each
    /// stack becomes an item entity at the mob's own position with the
    /// `ItemEntity` constructor's velocity — **not** `popResource`'s jittered
    /// cell position, which is a block's drop.
    ///
    /// Rolls in the **empty** loot context, so `killed_by_player` is `false` and
    /// `enchanted_count_increase` (looting) contributes nothing: rare drops gated
    /// on a player kill do not appear. That is honest rather than approximated —
    /// the context has no attacker field to fill (see [`crate::loot`]).
    fn reap_dead(&mut self) {
        let dead: Vec<(ResourceKey, Vec3)> = self
            .mobs
            .iter()
            .filter(|m| m.health <= 0.0)
            .map(|m| (m.entity_type.clone(), m.position()))
            .collect();
        if dead.is_empty() {
            return;
        }
        self.mobs.retain(|m| m.health > 0.0);
        for (entity_type, position) in dead {
            self.drop_death_loot(&entity_type, position);
        }
    }

    /// Rolls `entity_type`'s death loot table and spawns the result at
    /// `position`. See [`reap_dead`](Self::reap_dead) for the vanilla chain.
    ///
    /// Seeded from the tick count and the position, so a death is deterministic
    /// for a given world state without threading a connection's RNG into the sim.
    fn drop_death_loot(&mut self, entity_type: &ResourceKey, position: Vec3) {
        let Some(table) = crate::block_drops::mob_loot_table_id(entity_type) else {
            return;
        };
        let tables = crate::block_drops::bundled_tables();
        if tables.get(&table).is_none() {
            return;
        }
        let mut rng = SpawnRng::new(
            (self.tick_count as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (position.x.to_bits() ^ position.z.to_bits().rotate_left(31)),
        );
        let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
        for stack in rolled {
            if stack.count == 0 {
                continue;
            }
            let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            self.spawn_item(
                stack.item.clone(),
                position,
                velocity,
                ItemLifecycle::newly_dropped(
                    count,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
    }

    /// Registers a dropped item entity at `position` with fall velocity
    /// `velocity` and lifecycle `lifecycle` (typically
    /// [`ItemLifecycle::newly_dropped`]) so [`tick`](Self::tick) advances its
    /// age/pickup-delay every server tick (and removes it on despawn) — the
    /// missing driver issue #215 found: `ItemEntityRegistry`'s lifecycle had
    /// no production consumer, only the client-side fall dynamics
    /// (`ItemMotion`) reached anything, and purely for rendering.
    ///
    /// Returns the assigned entity id. Deciding *pickup* on player-overlap and
    /// merging adjacent stacks (via [`ItemEntityRegistry::merge`]) are the
    /// caller's job once it has player positions to test against — this
    /// closes the "nothing ticks the lifecycle at all" island, not the full
    /// pickup feature.
    pub fn spawn_item(
        &mut self,
        item: ResourceKey,
        position: Vec3,
        velocity: Vec3,
        lifecycle: ItemLifecycle,
    ) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.spawn(id, lifecycle);
        self.item_state.insert(
            id,
            ItemState {
                uuid: Uuid::new_v4(),
                item,
                motion: ItemMotion::new(position, velocity),
            },
        );
        id
    }

    /// Removes a tracked dropped item (pickup or manual despawn).
    ///
    /// Returns whether an item was actually tracked under `id`.
    pub fn remove_item(&mut self, id: i32) -> bool {
        self.item_state.remove(&id);
        self.items.remove(id).is_some()
    }

    /// The number of tracked dropped items.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.item_state.len()
    }

    /// The current position of a tracked dropped item, if any.
    #[must_use]
    pub fn item_position(&self, id: i32) -> Option<Vec3> {
        self.item_state.get(&id).map(|s| s.motion.position)
    }

    /// Every tracked dropped item as `(item id, count)`, in arbitrary order —
    /// the pair a caller needs to ask "what did that death drop".
    #[must_use]
    pub fn dropped_items(&self) -> Vec<(String, u8)> {
        self.item_state
            .iter()
            .map(|(id, state)| {
                let count = self.items.get(*id).map_or(0, |lifecycle| lifecycle.count);
                (state.item.to_string(), count)
            })
            .collect()
    }

    /// The current age/pickup-delay/count lifecycle of a tracked dropped
    /// item, if any.
    #[must_use]
    pub fn item_lifecycle(&self, id: i32) -> Option<&ItemLifecycle> {
        self.items.get(id)
    }

    /// Shrinks a tracked dropped item to `count`, for a **partial** pickup.
    ///
    /// Vanilla's `ItemEntity.playerTouch` hands the entity's own `ItemStack` to
    /// `Inventory.add`, which shrinks it in place; the entity is discarded only
    /// when the stack ends up empty. So a player with one free slot walking over
    /// a stack of 40 when 30 fit banks 30 and leaves an entity holding 10 —
    /// *not* nothing, and not the whole 40.
    ///
    /// Returns whether an item was tracked under `id`. A `count` of `0` is left
    /// to the caller to turn into a [`remove_item`](Self::remove_item); this
    /// setter does not implicitly delete, so "shrink to zero" cannot silently
    /// leak a zero-count entity that streams forever.
    ///
    /// Implemented as a remove-and-respawn **at the same id** rather than a
    /// mutating setter on [`ItemEntityRegistry`], which exposes none: that type
    /// lives in `lodestone-entity` and re-registering preserves the network id,
    /// so a client mid-`ADD_ENTITY` does not see the stack become a different
    /// entity. `age` and `pickup_delay` are carried over deliberately — a
    /// partial pickup must not reset the despawn clock, or a stack a full player
    /// keeps brushing past would live forever.
    pub fn set_item_count(&mut self, id: i32, count: u8) -> bool {
        let Some(tracked) = self.items.remove(id) else {
            return false;
        };
        self.items.spawn(
            id,
            ItemLifecycle {
                count,
                ..tracked.lifecycle
            },
        );
        true
    }

    /// Merges dropped items that have drifted together — `ItemEntity.tick`'s
    /// `mergeWithNeighbours()` (`ItemEntity.java`), the other consumer
    /// [`ItemEntityRegistry::merge`] was missing.
    ///
    /// Vanilla's search box is `getBoundingBox().inflate(0.5, 0.0, 0.5)`, and the
    /// **`0.0` vertical inflation is the load-bearing part**: two stacks side by
    /// side merge, two stacks a block apart vertically never do, however close
    /// they are horizontally. Since both boxes are the item's own 0.25 cube that
    /// works out to a horizontal reach of `0.125 + 0.5 + 0.125 = 0.75` and a
    /// vertical overlap of `|dy| < 0.25`. Using one isotropic radius here would
    /// silently merge a drop with one sitting on the block below it.
    ///
    /// Mergeability itself is [`ItemLifecycle::is_mergable`] (vanilla's
    /// `isMergable`: not the never-pickup sentinel, not infinite-age, under the
    /// despawn age, and not already a full stack) plus the same-item test, which
    /// lives here because [`ItemEntityRegistry`] is deliberately identity-free.
    fn merge_neighbouring_items(&mut self) {
        // Snapshot to a sorted id list first: merging mutates both registries,
        // and iteration order over a `HashMap` would otherwise make which of
        // three touching stacks absorbs the others vary run to run.
        let mut ids: Vec<i32> = self.item_state.keys().copied().collect();
        ids.sort_unstable();
        for i in 0..ids.len() {
            let to_id = ids[i];
            for j in (i + 1)..ids.len() {
                let from_id = ids[j];
                let (Some(to), Some(from)) =
                    (self.item_state.get(&to_id), self.item_state.get(&from_id))
                else {
                    continue;
                };
                if to.item != from.item {
                    continue;
                }
                let mergable = |id: i32| {
                    self.items
                        .get(id)
                        .is_some_and(ItemLifecycle::is_mergable)
                };
                if !mergable(to_id) || !mergable(from_id) {
                    continue;
                }
                let a = to.motion.position;
                let b = from.motion.position;
                if (a.x - b.x).abs() >= ITEM_MERGE_REACH_XZ
                    || (a.z - b.z).abs() >= ITEM_MERGE_REACH_XZ
                    || (a.y - b.y).abs() >= ITEM_MERGE_REACH_Y
                {
                    continue;
                }
                if self.items.merge(to_id, from_id) && self.items.get(from_id).is_none() {
                    // The source was fully absorbed, so its wire identity must go
                    // too — otherwise `snapshots()` keeps streaming a stack the
                    // lifecycle registry has already forgotten, and the client
                    // sees a permanent ghost item that never despawns.
                    self.item_state.remove(&from_id);
                }
            }
        }
    }

    /// Every dropped item a player standing at `player_feet` may collect right
    /// now, as `(entity id, item, count)` — issue #337's pickup half.
    ///
    /// Two filters, and both are vanilla:
    ///
    /// * [`crate::block_drops::is_within_pickup_range`] is `Player.aiStep`'s
    ///   inflated-AABB intersection, not a radius (see its own doc comment).
    /// * [`ItemLifecycle::can_be_picked_up`] is `ItemEntity.playerTouch`'s
    ///   `this.pickupDelay == 0` guard. A freshly popped block drop carries
    ///   [`crate::block_drops::DEFAULT_PICKUP_DELAY`] (10 ticks), so an item
    ///   is **not** collectable on the tick it spawns — a pickup gate that
    ///   asserts immediately reads that as a broken feature. Advance the tick
    ///   clock first.
    ///
    /// Read-only: the caller decides what it can actually fit and then calls
    /// [`remove_item`](Self::remove_item) for the ones it took. Splitting the
    /// query from the removal is what lets a connection roll back cleanly when
    /// its inventory is full — vanilla's `playerTouch` likewise only removes
    /// the entity once `getInventory().add(...)` succeeded.
    #[must_use]
    pub fn items_within_pickup_range(&self, player_feet: Vec3) -> Vec<(i32, ResourceKey, u8)> {
        let mut collectable: Vec<(i32, ResourceKey, u8)> = self
            .item_state
            .iter()
            .filter(|(id, state)| {
                crate::block_drops::is_within_pickup_range(player_feet, state.motion.position)
                    && self
                        .items
                        .get(**id)
                        .is_some_and(ItemLifecycle::can_be_picked_up)
            })
            .map(|(&id, state)| {
                let count = self.items.get(id).map_or(1, |lifecycle| lifecycle.count);
                (id, state.item.clone(), count)
            })
            .collect();
        // `item_state` is a `HashMap`, so its iteration order is unspecified and
        // varies run to run. Sorting by id makes a multi-item pickup deterministic
        // — without this, which of two overlapping drops lands in the selected
        // hotbar slot first is a coin flip, and a test asserting slot contents
        // would be intermittently red for reasons that look nothing like the
        // cause.
        collectable.sort_by_key(|&(id, _, _)| id);
        collectable
    }

    /// Every live entity this sim owns — mobs, projectiles, dropped items —
    /// lowered to the wire-facing [`EntitySnapshot`] the encode seam needs.
    ///
    /// This is the merged sibling of iterating [`iter`](Self::iter) alone:
    /// [`crate::tick::run_tick_loop`] (previously [`run_mob_tick_loop`])
    /// publishes this (not just the mobs) to [`LiveMobSource`], which is what
    /// actually gets a spawned projectile or
    /// dropped item onto the same `add_entity`/`move_entity`/`remove_entity`
    /// wire path mobs already proved reaches a real client
    /// (`entity_streaming_live.rs`) — without this, ticking the registries
    /// above would still be a closed loop that reaches zero pixels.
    #[must_use]
    pub fn snapshots(&self) -> Vec<EntitySnapshot> {
        let mut out: Vec<EntitySnapshot> = self.mobs.iter().map(SimMob::snapshot).collect();
        for t in self.projectiles.iter() {
            if let Some(meta) = self.projectile_meta.get(&t.id) {
                out.push(EntitySnapshot {
                    id: t.id,
                    uuid: meta.uuid,
                    entity_type: meta.entity_type.clone(),
                    position: t.projectile.position,
                    rotation: Rotation::new(0.0, 0.0),
                    head_yaw: 0.0,
                    velocity: t.projectile.velocity,
                    metadata: Vec::new(),
                });
            }
        }
        for (&id, state) in &self.item_state {
            out.push(EntitySnapshot {
                id,
                // **`minecraft:item`, not the item's own key.** This field is an
                // *entity* type and used to be set to `state.item` — so a
                // dropped `minecraft:bone_meal` streamed with entity type
                // `minecraft:bone_meal`, which is not in the entity-type
                // registry at all. `v770`'s `encode_add_entity_body` resolves it
                // with `entity_type_id(name).unwrap_or(0)`, and network entity
                // type `0` is `minecraft:acacia_boat` — so every dropped item
                // this server has ever spawned arrived at the client as a boat,
                // with no error logged anywhere. Every wire in
                // `cargo xtask connectedness` reads green for this path; the
                // value travelling it was wrong, which is the failure mode
                // CLAUDE.md records for `SET_TIME` (#323).
                //
                // The item's *identity* belongs in `metadata` instead, as
                // `ItemEntity.DATA_ITEM` (index 8, an `ITEM_STACK` serializer) —
                // see this field's note below.
                uuid: state.uuid,
                entity_type: item_entity_type(),
                position: state.motion.position,
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: state.motion.velocity,
                // **The field that makes a drop draw at all** (issue #537). A
                // client draws nothing for an item entity whose stack it has
                // not been told: vanilla's `ItemEntityRenderer.submit` returns
                // early on `state.item.isEmpty()`, and this project's own
                // client does the same (`EntityInterpolator::set_item_stack`).
                // So until this was filled a block drop spawned, streamed as a
                // real item entity, fell, merged and could be picked up — the
                // pickup being *visible*, since the inventory slot updates —
                // while drawing zero pixels. Every link in the chain was green.
                //
                // This is the **only** place in the tree that constructs a
                // `MetadataField::Item`, and that is load-bearing rather than
                // incidental: `ItemEntity.DATA_ITEM`'s wire index (8) is shared
                // with nineteen other fields on other classes, so the encoder
                // in `crates/protocol/v770/src/server_protocol.rs` relies on
                // every `Item` field belonging to a `minecraft:item` entity by
                // construction. This loop iterates `item_state`, so it does.
                // Never push one from the mob or projectile loops above.
                //
                // The count is the *entity's* stack size and lives on the
                // lifecycle, not on `ItemState` — the same
                // `map_or(1, |l| l.count)` read `merge_neighbouring_items` uses
                // above, with the same default for the (unreachable in
                // practice) case of state without a lifecycle.
                metadata: vec![MetadataField::Item {
                    item: state.item.clone(),
                    count: self.items.get(id).map_or(1, |lifecycle| lifecycle.count),
                }],
            });
        }
        out
    }
}

/// How far below the world's floor an item may sink before it is discarded —
/// vanilla's `Entity.checkBelowWorld` threshold (`Entity.java`'s
/// `this.getY() < (double)(this.level().getMinY() - 64)`).
const VOID_DESPAWN_DEPTH: f64 = 64.0;

/// The vertical epsilon used to ask "is the cell directly beneath this item's
/// bottom face solid".
///
/// A resting item's bottom face sits *exactly* on a block boundary, so
/// `position.y.floor()` is the cell **above** the supporting block and testing it
/// would always answer air. Subtracting a small epsilon first is what makes the
/// support test look at the block the item is standing on. It has to be smaller
/// than any real movement and larger than f64 noise; vanilla's own equivalent is
/// the `1.0E-7` deflation in `ItemEntity.tick`'s `noCollision` check.
const ITEM_SUPPORT_EPSILON: f64 = 1.0e-7;

/// Resolves one item's collision with the terrain after [`ItemMotion::tick`] has
/// already moved it, and records whether it is resting (issue #533).
///
/// This is the "world crate's job" [`ItemMotion::tick`]'s doc comment always
/// deferred and nothing ever did.
///
/// # What it models, and what it does not
///
/// Vertical only. Vanilla resolves the item's full `0.25 × 0.25 × 0.25` AABB
/// against every intersecting shape in `Entity.move`; this pushes the item out of
/// a solid cell it has sunk into, zeroes a downward velocity when that happens,
/// and sets `on_ground` from the cell beneath. Horizontal collision is left out
/// deliberately rather than by oversight: a dropped item's horizontal velocity is
/// a fraction of a block per tick and decays by `ITEM_AIR_DRAG` every tick, so it
/// cannot cross a wall in the time it takes to stop — whereas gravity is
/// unbounded, which is why the vertical case was the one with a visible symptom.
/// The single-column test also means an item is treated as a point at its own
/// centre rather than a cube, so it can settle in a cell whose neighbour is where
/// vanilla's wider box would have caught it. Both are visible as an item resting
/// slightly off-centre in a corner, never as an item falling through the floor.
///
/// Per-block friction is likewise not looked up: `block_friction` keeps
/// [`lodestone_entity::item_entity::DEFAULT_BLOCK_FRICTION`], so an item slides on
/// ice exactly as it does on stone. Vanilla reads
/// `getBlockPosBelowThatAffectsMyMovement().getBlock().getFriction()`; wiring that
/// needs a per-block friction census this crate does not carry.
fn settle_item(world: &ChunkWorld, motion: &mut ItemMotion) {
    let bx = motion.position.x.floor() as i32;
    let bz = motion.position.z.floor() as i32;

    // Sunk into a solid cell this tick: lift the bottom face onto its top.
    let by = motion.position.y.floor() as i32;
    if world.is_solid(bx, by, bz) {
        motion.position.y = f64::from(by + 1);
        if motion.velocity.y < 0.0 {
            // `Entity.move`'s collision resolution zeroes the delta component it
            // could not apply. Note this is also why the `-0.5` bounce in
            // `ItemMotion::tick` does not fire for a landed item in vanilla
            // either: by the time that branch runs, `y` is already `0.0`.
            motion.velocity.y = 0.0;
        }
    }

    let supporting_y = (motion.position.y - ITEM_SUPPORT_EPSILON).floor() as i32;
    motion.on_ground = world.is_solid(bx, supporting_y, bz);
}

/// Horizontal reach of `mergeWithNeighbours`' search: the item's own half-width
/// on both boxes plus vanilla's `inflate(0.5, …, 0.5)`.
const ITEM_MERGE_REACH_XZ: f64 = 0.125 + 0.5 + 0.125;

/// Vertical reach of the same search. Vanilla inflates y by **`0.0`**, so this is
/// nothing but the two 0.25-tall boxes overlapping — see
/// [`MobSim::merge_neighbouring_items`].
const ITEM_MERGE_REACH_Y: f64 = 0.25;

/// The entity-type key every dropped item streams as.
///
/// `minecraft:item` is the entity type; the *stack* is metadata. Naming the key
/// rather than the numeric id keeps this crate version-free, exactly as
/// `crate::players`' `player_entity_type` does for `minecraft:player` — and for
/// the same reason: `entity_type_id(name).unwrap_or(0)` on the encode side turns
/// a wrong key into `minecraft:acacia_boat` with no error, so the key is worth
/// stating once in one place.
fn item_entity_type() -> ResourceKey {
    "minecraft:item"
        .parse()
        .expect("`minecraft:item` is a valid resource key")
}

/// Squared horizontal+vertical distance between two positions (vanilla
/// `distanceToSqr`).
fn dist_sqr(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

/// The position of the nearest `accept`ed item to `from`, optionally restricted
/// to an axis-aligned box of `(horizontal, vertical)` half-extents.
///
/// This is vanilla's two-step shape, kept as two steps on purpose: every
/// perception search in `ai/goal/` filters by a *box* (`getEntitiesOfClass(…,
/// getBoundingBox().inflate(dx, dy, dz))`) and only then picks the nearest by
/// squared distance (`getNearestEntity`). Collapsing it into a single radius
/// test would be wrong in the corners — most visibly for
/// [`AVOID_RANGE_Y`](AVOID_RANGE_Y), where vanilla's vertical extent is a flat
/// `3.0` regardless of the horizontal one.
fn nearest_by<T>(
    items: &[T],
    from: Vec3,
    position: impl Fn(&T) -> Vec3,
    accept: impl Fn(&T) -> bool,
    range: Option<(f64, f64)>,
) -> Option<Vec3> {
    items
        .iter()
        .filter(|item| accept(item))
        .map(|item| position(item))
        .filter(|pos| match range {
            None => true,
            Some((horizontal, vertical)) => {
                (pos.x - from.x).abs() <= horizontal
                    && (pos.z - from.z).abs() <= horizontal
                    && (pos.y - from.y).abs() <= vertical
            }
        })
        .min_by(|a, b| dist_sqr(*a, from).total_cmp(&dist_sqr(*b, from)))
}

// NOTE: this module owns `ChunkWorld` + `MobSim`; the acceptance gate lives in
// `tests/mob_sim.rs` so it drives them through the crate's *public* API — the
// same discipline the rest of the project uses (a consumer that is only a
// `#[cfg(test)]` fake proves nothing about the public seam).

/// A live [`EntitySource`] fed by a background-ticked [`MobSim`] (issue
/// #217). [`IntegratedServer::open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs)
/// constructs one alongside [`crate::tick::run_tick_loop`] (issue #284; this
/// used to be [`run_mob_tick_loop`] before the mob and block-entity tick
/// loops were unified into one), the task that owns the sim and republishes
/// its snapshots here every tick.
///
/// Deliberately the same shape as `entity_streaming_live.rs`'s own test-only
/// `SharedSnapshotSource` (an `Arc<Mutex<Vec<EntitySnapshot>>>` behind
/// [`EntitySource`]) — that test already proved the read side of this shape
/// reaches a real client; this type is the production version, now fed by a
/// real simulation instead of a hand-mutated `Vec`.
#[derive(Debug, Clone, Default)]
pub struct LiveMobSource(Arc<Mutex<Vec<EntitySnapshot>>>);

impl EntitySource for LiveMobSource {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0
            .lock()
            .expect("live mob snapshot lock poisoned")
            .clone()
    }
}

impl LiveMobSource {
    /// Replaces the published snapshot set. Called once per tick — in
    /// production by [`crate::tick::run_tick_loop`] (issue #284; previously
    /// [`run_mob_tick_loop`], before the two background tick loops were
    /// unified into one), and directly by `run_mob_tick_loop`'s own test. The
    /// next `snapshots()` call from any connection (there may be several,
    /// e.g. open-to-LAN) sees the new set. `pub(crate)`, not private: the
    /// unified loop lives in a sibling module (`tick.rs`) and needs to call
    /// this directly rather than through a second wrapper.
    pub(crate) fn publish(&self, snapshots: Vec<EntitySnapshot>) {
        *self.0.lock().expect("live mob snapshot lock poisoned") = snapshots;
    }
}

/// A shared, mutation-capable handle onto one live [`MobSim`] — the
/// counterpart [`crate::BlockEntityHandle`] already established for block
/// entities, and the exact piece issue #12's own combat census named as
/// missing: *"there is no way to reach a live mob's health from a
/// connection's own task... `MobSim` is ticked entirely inside its own
/// background task and is never wrapped in a shared, lockable handle."*
/// [`LiveMobSource`] is deliberately read-only (a snapshot cache for
/// streaming, fed *by* the tick loop); this is the mutation-capable sibling a
/// connection needs to actually damage/knock back a mob a player attacked —
/// see `crate::server::apply_attack`, its one production caller.
///
/// # Why `MobSim<'static>`, and the leak that produces it
///
/// [`MobSim`] borrows its [`ChunkWorld`] (`MobSim<'w>`), but a handle shared
/// with a separately-`tokio::spawn`ed connection task must be `'static` (that
/// is what `tokio::spawn` requires of everything it captures). [`new`](Self::new)
/// resolves this with [`Box::leak`]: the `ChunkWorld` a caller hands in is
/// leaked once, for the process's remaining lifetime, rather than borrowed
/// for one task's own stack frame the way [`run_mob_tick_loop`]'s previous
/// (pre-handle) implementation did.
///
/// This is a **deliberate, bounded** leak, not an oversight.
/// `run_mob_tick_loop`'s own doc comment already discloses that its
/// `ChunkWorld` snapshot is static for the sim's whole lifetime — a fixed
/// area around the mob-spawn center, never widened after the initial load.
/// Leaking it only changes *whose* lifetime "static" is measured against:
/// "static for this one task" becomes "static for the process" — the same
/// bytes, held slightly longer, for the one [`MobSim`] a running
/// [`crate::IntegratedServer`] ever constructs per call to
/// [`open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs).
/// A caller that constructs many short-lived handles (e.g. one per test) does
/// leak once per handle — acceptable for a bounded terrain snapshot in a
/// process that exits shortly after, the same trade-off `MobSim`'s own
/// `assert_send::<MobSim<'static>>()` const-check already anticipated by
/// name.
#[derive(Debug, Clone)]
pub struct MobHandle(Arc<Mutex<MobSim<'static>>>);

impl Default for MobHandle {
    /// A handle over an empty, mobless sim backed by a tiny leaked
    /// [`ChunkWorld`] — the "nothing ticks it, but it is real and safe to
    /// attack against" default [`crate::BlockEntityHandle::default`] already
    /// establishes for connections built without a live mob population
    /// (`IntegratedServer::open_in_memory`/`open_in_memory_with_entities`/`bind`).
    /// An `Attack` packet against any entity id here simply finds no mob
    /// ([`MobSim::attack`] returns `None`) — a harmless no-op, never a panic.
    fn default() -> Self {
        Self::new(ChunkWorld::new(-64, 384))
    }
}

impl MobHandle {
    /// Builds a handle over a fresh, empty [`MobSim`] backed by a leaked copy
    /// of `world` — see the struct's own doc comment for why leaking is the
    /// deliberate choice here.
    #[must_use]
    pub fn new(world: ChunkWorld) -> Self {
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        Self(Arc::new(Mutex::new(MobSim::new(world))))
    }

    /// Builds a handle already seeded with [`seed_demo_mobs`]'s baseline
    /// population, snapshotting `world_source` the same way the previous
    /// (pre-handle) `run_mob_tick_loop` did at the top of its own future —
    /// see that function's doc comment for the `cx_range`/`cz_range`/
    /// `mob_center` scope notes, unchanged by this refactor.
    #[must_use]
    pub fn seeded<S: ChunkSource>(
        world_source: &S,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
        center_x: i32,
        center_z: i32,
        mob_count: usize,
    ) -> Self {
        let handle = Self::default();
        handle.reseed(
            ChunkWorld::from_source(world_source, cx_range, cz_range),
            center_x,
            center_z,
            mob_count,
        );
        handle
    }

    /// Replaces this handle's terrain snapshot **and** its population with a
    /// fresh [`MobSim`] over `world`, seeded exactly as
    /// [`seeded`](Self::seeded) would have.
    ///
    /// # Why this exists (issue #454)
    ///
    /// `seeded` did the whole job inside
    /// [`crate::IntegratedServer::open_in_memory_with_mobs`]'s body, *before any
    /// task spawned* — so the 49-column `ChunkWorld::from_source` snapshot it
    /// needs was on the critical path of opening a world, at ~909 ms per
    /// composed column. Vanilla does not block world-open on mob population, and
    /// neither does this crate any more: the constructor now builds a
    /// [`Default`] handle (empty, mobless, safe to attack against — see that
    /// impl's own doc comment) and a background task calls this once the terrain
    /// it needs has been fetched off-thread.
    ///
    /// # What is deliberately thrown away
    ///
    /// Everything: the old `MobSim` is dropped, not merged. That is correct for
    /// the one caller — a handle that has only ever been `Default` has no
    /// population to lose, and `set_next_id(1000)` must be re-applied to the new
    /// sim anyway. It is **not** a general "load more terrain" primitive; a mob
    /// spawned in the window before the first reseed would vanish. Widening the
    /// snapshot as the player walks (this module's long-standing documented
    /// scope cut) needs a sim that can *extend* its world, not replace it.
    ///
    /// Takes `&self`, like every other accessor here, because the sim lives
    /// behind the handle's own `Mutex` — so this is safe to call from a
    /// background task while the connection task holds a clone.
    pub fn reseed(&self, world: ChunkWorld, center_x: i32, center_z: i32, mob_count: usize) {
        // Leaked for the same reason `new` leaks: `MobSim` borrows its world for
        // `'static`. See the struct's own doc comment — one bounded snapshot per
        // reseed, and production reseeds exactly once per world.
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        self.with(|sim| {
            *sim = MobSim::new(world);
            // See `MobSim::set_next_id`'s own doc comment: id `1` collides
            // with `LOCAL_PLAYER_ENTITY_ID` on the wire.
            sim.set_next_id(1000);
            // Exactly `mob_count`, including zero — see [`seed_demo_mobs`].
            seed_demo_mobs(sim, center_x, center_z, mob_count);
        });
    }

    /// Runs `f` against the locked sim, returning its result — the same
    /// funnel-every-access shape [`crate::BlockEntityHandle::with`]
    /// established, for the identical "no caller can forget to handle a
    /// poisoned lock inconsistently" reason.
    pub fn with<R>(&self, f: impl FnOnce(&mut MobSim<'static>) -> R) -> R {
        let mut guard = self.0.lock().expect("mob sim lock poisoned");
        f(&mut guard)
    }
}

impl EntitySource for MobHandle {
    /// A `MobHandle` is a legitimate [`EntitySource`] all on its own — no
    /// separate [`LiveMobSource`] cache required — for any caller that mutates
    /// the sim directly and does not also need a background tick loop
    /// ([`crate::tick::run_tick_loop`], issue #284) republishing it on a
    /// timer. Production (`IntegratedServer::open_in_memory_with_mobs`) still layers
    /// [`LiveMobSource`] on top so the tick loop's own AI motion reaches the
    /// wire on its own cadence; a test that only cares about a hand-placed,
    /// unticked mob (e.g. an attack test) can use the handle directly instead.
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.with(|sim| sim.snapshots())
    }
}

/// The highest solid-block Y at `(x, z)` within `world`'s loaded vertical
/// range, or `None` if the whole column reads air (or is unloaded) — the
/// ground a freshly seeded mob should stand on. A linear scan from the top
/// down; only ever called at seed time (a handful of calls, not per-tick), so
/// this is not a hot path.
fn surface_y(world: &ChunkWorld, x: i32, z: i32) -> Option<i32> {
    let top = world.min_y + world.height - 1;
    (world.min_y..=top).rev().find(|&y| world.is_solid(x, y, z))
}

/// Seeds `count` zombies in a ring of radius 6 blocks around `(center_x,
/// center_z)`, each placed on the real terrain surface (skipped if the column
/// has no solid ground within `world`'s loaded range) with a baseline
/// wander/look goal set — the same defaults [`MobSim::run_spawn_cycle`] gives
/// a naturally-spawned mob.
///
/// This is **not** vanilla natural spawning: there is no light-level,
/// biome, or pack-size logic here, because no terrain/biome-aware
/// [`SpawnCandidateSource`] implementation exists in production yet (the
/// trait exists; every current impl is a test mock — see `mob_spawn.rs`).
/// Building that is a separate, considerably larger feature. This exists
/// purely so issue #217's actual subject — computed AI motion reaching the
/// wire — has a population to move; a caller that wants real spawning wires
/// [`MobSim::run_spawn_cycle`] in its place once a real source exists.
fn seed_demo_mobs(sim: &mut MobSim<'_>, center_x: i32, center_z: i32, count: usize) {
    let world = sim.world();
    // `count`, **not** `count.max(1)`. The floor was here until singleplayer
    // needed to be mob-free: it made a request for zero demo mobs silently
    // produce one zombie, so "turn the demo population off" was not expressible
    // at all. Vanilla does not seed a demo population; a caller asking for none
    // must get none.
    for i in 0..count {
        let species = DEMO_SPECIES[i % DEMO_SPECIES.len()];
        let key = ResourceKey::from_str(&format!("minecraft:{species}"))
            .expect("DEMO_SPECIES entries are valid paths");
        let angle = (i as f64) * std::f64::consts::TAU / (count.max(1) as f64);
        let x = center_x + (angle.cos() * 6.0).round() as i32;
        let z = center_z + (angle.sin() * 6.0).round() as i32;
        let Some(y) = surface_y(world, x, z) else {
            continue;
        };
        let pos = Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5);
        // Through `spawn_species`, not `spawn` + `set_entity_type` + two
        // hardcoded goals. This is the **only** production path that creates a
        // mob a connected client can see, so it is also the only place the
        // per-species roster can reach pixels: routed this way, a demo zombie
        // gets vanilla's real zombie set — `HurtByTargetGoal`,
        // `NearestAttackableTargetGoal`, `MeleeAttackGoal`, `LookAtPlayerGoal` —
        // instead of wandering obliviously past the player.
        //
        // The shape, speed and A* budget were hardcoded here as `0.6 × 1.95`,
        // `0.23` and `400`; `spawn_species` derives the first two from the same
        // dimension census and `movement_speed` attribute and gets the same
        // numbers, and the third from `follow_range * 16` = `560`, which is
        // vanilla's own figure rather than this call site's guess.
        sim.spawn_species(key, pos);
    }
}

/// The species [`seed_demo_mobs`] cycles through, in order (issue #457).
///
/// # What this is for
///
/// Until #457 this list was one hardcoded `minecraft:zombie`, and
/// [`seed_demo_mobs`] is the **only** production path that creates a
/// client-visible mob. So every roster family except `hostile_melee` — five
/// jar-cited goal tables covering 26 further species — reached **zero pixels**
/// no matter how correct it was, and no crate's own test suite could say so,
/// because each of them is a closed loop around a table nothing instantiates.
/// Widening this list is what makes those tables observable to a connected
/// client, and it is the minimum that does: it is deliberately **not** spawn
/// eggs (#224) and not a spawner block.
///
/// # Order is load-bearing, twice
///
/// The seeder cycles this list, so with production's `mob_count` of 6
/// (`lodestone-shell/src/net.rs`) a player sees exactly the **first six**
/// entries. Those six are therefore one per roster family plus one, so that a
/// default singleplayer world exercises every family rather than six variations
/// on a monster:
///
/// | # | species | family |
/// |---|---|---|
/// | 0 | `zombie` | `hostile_melee` |
/// | 1 | `cow` | `passive` |
/// | 2 | `wolf` | `neutral` |
/// | 3 | `blaze` | `ranged` |
/// | 4 | `guardian` | `specialist` |
/// | 5 | `creeper` | `hostile_melee` (its `SwellGoal` is the most visible) |
///
/// `zombie` is first for a second, narrower reason: `MobSim::set_next_id(1000)`
/// plus spawn order makes entity id 1000 deterministic, and
/// `crates/protocol/v770/tests/live_mob_sim.rs` relies on that. Keeping the
/// zombie at index 0 leaves the *first* demo mob exactly what it has always
/// been.
///
/// # Gotcha when adding to this list
///
/// Every entry must be a species some roster family claims, or it silently
/// spawns with `roster::FALLBACK` (wander and look) — visible, but proving
/// nothing about any goal table. `demo_species_are_all_rostered_and_span_every_family`
/// fails rather than letting that through. An entry also needs a
/// `type_spec` arm in `lodestone_entity::attribute`, or it runs at the 0.7
/// registry default; that is pinned separately by
/// `every_rostered_species_has_a_type_spec_arm`.
///
/// This is still a demo ring on flat ground, not natural spawning — a guardian
/// on land is a real consequence and an accepted one, since the alternative is
/// that `specialist.rs` stays unobservable.
pub const DEMO_SPECIES: &[&str] = &[
    "zombie",
    "cow",
    "wolf",
    "blaze",
    "guardian",
    "creeper",
    // Beyond production's count of 6, but reached by any caller asking for
    // more, and each one another family's table on screen.
    "skeleton",
    "spider",
    "sheep",
    "chicken",
    "enderman",
    "snow_golem",
];

/// Native tick-loop driver for issue #217: ticks the live [`MobSim`] behind
/// `handle` once every [`MOB_TICK_INTERVAL`], forever, republishing snapshots
/// to `out` after every tick.
///
/// # Superseded as of issue #284 — no longer what production spawns
///
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] used to spawn this
/// function directly, side-by-side with
/// [`crate::block_entities::run_block_entity_tick_loop`]. As of #284 it spawns
/// [`crate::tick::run_tick_loop`] instead, which ticks both the mob sim and
/// every block entity from **one** loop body instrumented with MSPT/TPS/
/// overrun accounting (issue #285) — see that module's own doc comment for
/// why one loop replaced two. This function still exists, still does exactly
/// what its doc says, and is still exercised by its own test below; it is
/// simply no longer the production driver. Kept rather than deleted because
/// its test is a real, direct regression gate on `MobSim::tick` +
/// `LiveMobSource::publish` composing correctly in isolation from block
/// entities.
///
/// # Issue #12 update: `handle` is now shared, not owned
///
/// This function used to build its own `ChunkWorld`/`MobSim` locally (borrowed
/// for its own stack frame) — the reason a connection could never reach a
/// live mob's health, per this module's own combat-census history. It now
/// takes a pre-built [`MobHandle`] instead ([`MobHandle::seeded`] is the
/// direct replacement for what this function used to do at its own top,
/// including the `set_next_id(1000)`/[`seed_demo_mobs`] seeding), so the
/// exact same [`MobSim`] this loop ticks is also the one
/// `crate::server::apply_attack` mutates through a clone of the same handle.
/// Ticking without a shared handle would still be a closed loop — the same
/// "computed but never reaches the wire" island issue #217 originally closed
/// for AI motion, this time for combat.
///
/// # Scope cuts, explicit, unchanged by the above
///
/// * The `ChunkWorld` snapshot [`MobHandle::seeded`] loads is **static** for
///   the handle's whole lifetime — nothing re-queries the original
///   `world_source` after the initial load, so a mob does not path across a
///   chunk boundary outside the area it was seeded with. Widening this to
///   grow with the player's view is future work; a fixed area around spawn is
///   an honest, working scope cut rather than a silent limitation.
/// * No natural spawning — see [`seed_demo_mobs`]'s own doc comment.
/// * No despawn pass (`MobSim::despawn_pass` needs a player position this
///   task has no way to learn; it is not plumbed through
///   [`EntitySource`], which is deliberately read-only and one-directional).
///   A long singleplayer session therefore keeps the same fixed demo
///   population forever rather than vanilla's natural cap-driven churn — an
///   explicit, documented cut, not an oversight.
///
/// Native only: uses `tokio::time::interval`, which (like
/// `server.rs`'s `serve_play`/`KEEP_ALIVE_INTERVAL` family — see that
/// module's own doc comment) is not available on `wasm32`. A `wasm32` session
/// therefore gets no live mob sim yet, exactly the same kind of documented gap
/// `PlayerVitals` already has on that target.
#[cfg(not(target_arch = "wasm32"))]
// No caller left outside this file's own `#[cfg(test)]` module since #284
// (see the "Superseded" section above) — the lib target genuinely has none,
// so plain `dead_code` would fire there even though the function is real and
// still tested.
#[allow(dead_code)]
pub(crate) async fn run_mob_tick_loop(handle: MobHandle, out: LiveMobSource) {
    // `snapshots()`, not `sim.iter().map(SimMob::snapshot)`: the latter only
    // ever lowered mobs, so a projectile or dropped item registered on this
    // `sim` (issues #211/#215) would tick correctly and still never reach
    // `LiveMobSource` — ticking without publishing is still an island, just
    // one hop further along.
    out.publish(handle.with(|sim| sim.snapshots()));

    // 50ms, matching vanilla's 20 TPS and this crate's own `VITALS_TICK_INTERVAL`
    // (`server.rs`) — kept as a local constant rather than sharing that one
    // because it is `server.rs`-private and the two are allowed to drift
    // independently (mob AI has no reason to share a literal with drowning
    // damage beyond both wanting "one vanilla tick").
    const MOB_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
    let mut tick = tokio::time::interval(MOB_TICK_INTERVAL);
    loop {
        tick.tick().await;
        handle.with(MobSim::tick);
        out.publish(handle.with(|sim| sim.snapshots()));
    }
}

/// Issue #455's host half: the `follow_range` attribute reaching the controller
/// that bounds target acquisition, and the miss case that made it wrong.
#[cfg(test)]
mod follow_range_tests {
    // Also home to the death-loot gate (issue #272), which reuses this module's
    // `flat_world` rather than growing a second copy of it.
    use super::*;

    /// A floor wide enough for a mob at the origin and a player out past 36
    /// blocks, so nothing here depends on a mob standing in the void.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=48 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Spawns `species` at the origin through the **production** path
    /// ([`MobSim::spawn_species`], what `seed_demo_mobs` calls), feeds one player
    /// `distance` blocks away on +X, and reports whether the mob ever acquires a
    /// target within `ticks`.
    ///
    /// `attack_target()` is the observable, not `can_use`:
    /// `NearestAttackableTargetGoal` throttles its own search, so this ticks a
    /// generous bound and checks after each — a fixed single tick would measure
    /// the throttle rather than the range.
    fn acquires_at(species: &str, distance: f64, ticks: usize) -> bool {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(distance, 0.0, 0.0),
            held_item: None,
        }]);
        for _ in 0..ticks {
            sim.tick();
            if sim.get(id).expect("alive").attack_target().is_some() {
                return true;
            }
        }
        false
    }

    /// A killed mob drops its loot table's items (issue #272).
    ///
    /// The expected values come from vanilla's own `entities/cow.json`, not from
    /// our roller: two pools of `rolls: 1`, leather `uniform 0..2` and beef
    /// `uniform 1..3`. So a kill always yields at least the beef, both item ids
    /// are from that file, and — the part a wrong pool loop gets wrong — the beef
    /// count is never zero while the leather stack may be absent entirely.
    #[test]
    fn a_killed_cow_drops_its_loot_table() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(1.0);

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the cow is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            !dropped.is_empty(),
            "a cow's death must drop something — entities/cow.json guarantees the beef pool"
        );
        for (item, count) in &dropped {
            assert!(
                matches!(item.as_str(), "minecraft:leather" | "minecraft:beef"),
                "cow.json names only leather and beef, got {item}"
            );
            assert!(*count > 0, "a zero-count stack must be filtered, got {item}");
        }
        assert!(
            dropped.iter().any(|(item, _)| item == "minecraft:beef"),
            "the beef pool is `rolls: 1` with `uniform 1..3`, so it is never absent: {dropped:?}"
        );
    }

    const TICKS: usize = 80;

    /// **The control, and the reason the obvious fix is wrong.**
    ///
    /// The miss case for `follow_range` is **32.0**, not `0.0`. `attr`'s
    /// `unwrap_or(0.0)` reads like the fallback and is unreachable for any
    /// attribute the registry knows, because `AttributeMap::value` already
    /// substitutes `default_def(key).default` for an absent instance.
    ///
    /// This matters because it decides what the fix can be. A guard of the shape
    /// `if r > 0.0 { r } else { DEFAULT }` is **dead code** — it never fires, and
    /// an unlisted species keeps the registry's 32.0, which is precisely the one
    /// number `follow_range` never legitimately holds (`Mob.createMobAttributes()`
    /// overrides it to 16.0 for every mob). The wrong value sits *inside* the
    /// plausible range, so only instance presence can detect the miss.
    ///
    /// Predicted from `attribute.rs:341` (`"follow_range" => d(32.0, …)`) and
    /// `AttributeMap::value`'s `else` branch, then measured. If this ever reads
    /// 0.0, `attr` changed and the `attr_present` split is redundant.
    #[test]
    fn control_the_attribute_lookup_misses_to_the_registry_default_not_zero() {
        // **Structurally** unlistable, not merely unlisted (#457).
        //
        // This precondition used to name `minecraft:zombie_villager`, with its
        // own instruction to "pick another unlisted species or this control is
        // vacuous" if that species ever gained an arm. It did — and picking
        // another real species only defers the same breakage to the next batch
        // of arms, which is not a fix but a rescheduling.
        //
        // So the precondition is now pinned to a property no future commit can
        // take away: `default_attributes` returns `None` for **any** id outside
        // the `minecraft` namespace, before it ever consults `type_spec`. That
        // keeps the miss case reachable permanently, at the cost of the claim
        // that it is reachable from a *real species* — see
        // `an_unlisted_species_still_falls_back_at_the_spawn_path` below, which
        // is where that half now lives.
        let unlisted = Identifier::from_str("modded:not_a_vanilla_mob").expect("valid id");
        assert!(
            default_attributes(&unlisted).is_none(),
            "default_attributes must answer None outside the minecraft namespace, \
             or the miss case below is not reachable at all"
        );

        let empty = AttributeMap::new();
        assert_eq!(
            attr(&empty, "follow_range"),
            32.0,
            "the miss case is the registry default, so a `> 0.0` guard can never fire"
        );
        assert_eq!(
            attr_present(&empty, "follow_range"),
            None,
            "instance presence is the only reading that can see the miss"
        );

        // And the listed case really does carry the jar's number, so the split
        // above is not simply discarding every attribute.
        let zombie = default_attributes(&Identifier::from_str("minecraft:zombie").unwrap())
            .expect("zombie has a type_spec arm");
        assert_eq!(
            attr_present(&zombie, "follow_range"),
            Some(35.0),
            "Zombie.java:133 sets FOLLOW_RANGE to 35.0"
        );
    }

    /// **The gate.** A zombie must acquire at its real 35.0, which requires
    /// separating 35 from *both* wrong candidates rather than merely showing that
    /// targeting works at all.
    ///
    /// | distance | expected | what it rules out |
    /// |---|---|---|
    /// | 20 | acquires | `DEFAULT_FOLLOW_RANGE` 16.0 (the pre-fix value) |
    /// | 34 | acquires | the registry's 32.0 as well |
    /// | 36 | **no** | an unbounded feed, and blaze/enderman's 48/64 |
    ///
    /// Asserting only "a zombie acquires a nearby player" passes at 16, at 32 and
    /// at 35 alike, which is the magnitude-species vacuous test: right subject,
    /// predicate too weak to distinguish the hypotheses.
    #[test]
    fn a_zombie_acquires_at_its_real_follow_range_not_16_or_32() {
        assert!(
            acquires_at("zombie", 20.0, TICKS),
            "a zombie must acquire a player at 20 blocks; failing here means the \
             controller is still on DEFAULT_FOLLOW_RANGE (16.0) and #455's host \
             half never landed"
        );
        assert!(
            acquires_at("zombie", 34.0, TICKS),
            "a zombie must acquire at 34 blocks — inside its real 35.0 but outside \
             both 16.0 and the registry's 32.0, so this is the assertion that pins \
             the value rather than merely the wiring"
        );
        assert!(
            !acquires_at("zombie", 36.0, TICKS),
            "a zombie must NOT acquire at 36 blocks: the cut is real and bounded at \
             35.0, not merely large. Without this the gate above passes for any \
             range >= 34, including an unbounded feed"
        );
    }

    /// The **unlisted-species** half, retired at the acquisition layer and
    /// re-established at the spawn layer (#457).
    ///
    /// # Why the previous test was retired rather than repointed
    ///
    /// `an_unlisted_species_falls_back_to_the_mob_default_not_the_registry_default`
    /// drove `zombie_villager` — a species with the full `ZOMBIE` goal table
    /// (so a real `NearestAttackableTargetGoal`) and no `type_spec` arm — and
    /// asserted it acquired at 15 and not at 17. Its own doc said that when
    /// #457 landed it would start failing at 17, and that the failure was "the
    /// signal to retire it, not to widen it". It did, and it is.
    ///
    /// The obvious salvage — repoint it at some *other* species that is both
    /// unlisted and owns a modelled target goal — **has no candidate, and
    /// cannot acquire one.** Every species any roster family claims now has a
    /// `type_spec` arm, and `attribute.rs`'s
    /// `every_rostered_species_has_a_type_spec_arm` fails if that stops being
    /// true. A species *outside* the roster gets `roster::FALLBACK`, which is
    /// wander-and-look and contains no target goal at all. So "unlisted
    /// attributes" and "modelled target goal" are now mutually exclusive by
    /// construction, and no rescheduling of this test survives the next commit.
    ///
    /// # What survives, and where
    ///
    /// The property itself is still live and still production-reachable:
    /// [`MobSim::spawn_species`] reads `attr_present(…).unwrap_or(DEFAULT_FOLLOW_RANGE)`
    /// for **any** key, so an id with no template still has to land on
    /// `Mob.createMobAttributes()`' 16.0 rather than the registry's 32.0. Only
    /// the *observable* had to move: from "does it acquire a player at 17
    /// blocks" to the range the spawn path actually installed on the
    /// controller. That is a strictly narrower claim — it no longer proves the
    /// number reaches targeting — and saying so is the point.
    ///
    /// 16 against 32 is still the whole distinction, and both are asserted, so
    /// this cannot pass by reading some third number.
    #[test]
    fn an_unlisted_species_still_falls_back_at_the_spawn_path() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        // Outside the `minecraft` namespace, so `default_attributes` answers
        // `None` structurally — see the control above.
        let key = ResourceKey::from_str("modded:not_a_vanilla_mob").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let got = MobController::follow_range(&sim.get(id).expect("alive").mob);
        assert_eq!(
            got, DEFAULT_FOLLOW_RANGE,
            "an unlisted species must fall back to Mob.createMobAttributes' 16.0"
        );
        assert_ne!(
            got, 32.0,
            "32.0 is the registry default and the one value follow_range never \
             legitimately holds — reading it here means `attr`'s registry \
             fallback is reaching the controller, the exact defect #455's \
             brokered patch would have left in place"
        );

        // Control: a *listed* species must read its own jar value through the
        // same accessor, so the assertions above are a property of the fallback
        // and not of `follow_range` always answering 16.
        let zombie = ResourceKey::from_str("minecraft:zombie").expect("valid key");
        let zid = sim.spawn_species(zombie, Vec3::new(2.0, 0.0, 0.0)).id();
        assert_eq!(
            MobController::follow_range(&sim.get(zid).expect("alive").mob),
            35.0,
            "Zombie.java:133 — if this also reads 16.0 the accessor is not \
             observing what spawn_species installed"
        );
    }
}

/// Issue #458, primitive 1: the host-resolved persistent-anger deadline.
#[cfg(test)]
mod anger_tests {
    use super::*;

    /// The jar's grudge window, in ticks, stated **independently of
    /// [`ANGER_TICKS`]**.
    ///
    /// `NeutralMob.PERSISTENT_ANGER_TIME = TimeUtil.rangeOfSeconds(20, 39)`,
    /// and `rangeOfSeconds` multiplies by 20, giving `UniformInt.of(400, 780)`.
    ///
    /// **These literals are load-bearing and must not be replaced by a read of
    /// `ANGER_TICKS`.** The first version of this module did exactly that, and
    /// the control proved it vacuous: setting `ANGER_TICKS` to `(20, 39)` — the
    /// seconds-as-ticks misreading these tests exist to exclude — left every
    /// assertion **passing**, because the expectation moved with the subject.
    /// That is `decode(encode(x)) == x` wearing a jar citation.
    const JAR_LO: u64 = 400;
    const JAR_HI: u64 = 780;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Spawns one **real** mob through the production path and hits it once,
    /// then reports the tick offset at which `angry_target` first reads `None`.
    ///
    /// Drives `MobSim` + `NavigatingMob`, never `ScriptMob` and never
    /// `roster::probe`'s double: both override the perception methods wholesale,
    /// which is exactly how #441's and #455's islands stayed hidden.
    fn ticks_until_anger_clears(species: &str, limit: u64) -> Option<u64> {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(1.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("the mob must still be alive to hold a grudge");

        // One tick to run the feed, then poll.
        for elapsed in 0..limit {
            sim.tick();
            if sim.get(id).expect("alive").mob.angry_target().is_none() {
                return Some(elapsed);
            }
        }
        None
    }

    /// **The gate.** A grudge must expire inside the jar's `[400, 780]` tick
    /// window — and the assertion has to separate that from the
    /// seconds-as-ticks reading of `rangeOfSeconds(20, 39)`, which would expire
    /// it in `[20, 39]` ticks.
    ///
    /// Predicting only "it eventually expires" is satisfied by both hypotheses
    /// and by an off-by-one on the inclusive upper bound, which is the
    /// magnitude species of vacuous test. Both bounds are asserted, and the
    /// wrong hypothesis is named in the failure message rather than left
    /// implicit.
    #[test]
    fn anger_expires_inside_the_jars_tick_window() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        // Generous headroom over `hi`, so "never expired" is distinguishable
        // from "expired late" rather than both timing out.
        let limit = hi * 2;

        for species in ["wolf", "bee", "enderman", "zombified_piglin"] {
            let elapsed = ticks_until_anger_clears(species, limit).unwrap_or_else(|| {
                panic!("{species}'s grudge never expired within {limit} ticks")
            });
            assert!(
                elapsed >= lo,
                "{species}'s grudge expired after {elapsed} ticks, before the jar's \
                 minimum of {lo}. A value in [20, 39] means rangeOfSeconds(20, 39) \
                 was read as seconds; it already returns ticks"
            );
            assert!(
                elapsed <= hi,
                "{species}'s grudge lasted {elapsed} ticks, past the jar's maximum \
                 of {hi}"
            );
        }
    }

    /// The grudge must be **live** immediately after the hit, and must name the
    /// attacker's position — not merely be non-`None` at some later point.
    ///
    /// Control for the test above: without this, a mob whose anger was never
    /// set at all would "expire" at tick 0 and only the lower-bound assertion
    /// would catch it, for the wrong reason.
    #[test]
    fn a_hit_starts_a_grudge_naming_the_attacker() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            None,
            "an unprovoked neutral mob must hold no grudge — if this is Some, \
             every neutral species is hostile on sight"
        );

        let attacker = Vec3::new(3.0, 0.0, 4.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the grudge must name where the attacker was"
        );
    }

    /// The deadline is **absolute**, so a grudge refreshed by a second hit
    /// extends from the *new* tick rather than from the first.
    ///
    /// This is the assertion a decrementing counter passes only by accident:
    /// it pins that the stored value is compared against `tick_count` rather
    /// than decremented, by advancing the clock a long way between two hits and
    /// requiring the grudge to outlive the first deadline's worst case.
    #[test]
    fn a_second_hit_extends_the_deadline_from_the_new_tick() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(1.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        // Advance well past the first grudge's *minimum* but not its maximum,
        // then hit again.
        for _ in 0..lo {
            sim.tick();
        }
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");

        // The refreshed grudge must still be live `lo` ticks later, which the
        // first grudge could not guarantee: its worst case was `hi`, and we are
        // now at `lo + lo = 800 > hi`.
        for _ in 0..lo {
            sim.tick();
        }
        assert!(
            lo + lo > hi,
            "this test's arithmetic assumes 2*{lo} exceeds {hi}; if the window \
             changed, the schedule below no longer proves anything"
        );
        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the second hit must extend the deadline from the tick it landed on; \
             a grudge that has already expired here means the deadline was not \
             recomputed against the current clock"
        );
    }
}

/// Issue #458, primitives 3-5 (instant relocation / self-damage / ownership):
/// the `MobSim` host half of the four seam primitives that landed in
/// `lodestone-entity`. The gaze feed is the one documented gap — see
/// [`PlayerPerception`]'s lack of a view vector.
#[cfg(test)]
mod primitives_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Primitive 3: a host teleport command rewrites position immediately and
    /// survives the next tick — an instant relocation, not a fast walk.
    #[test]
    fn teleport_to_moves_the_mob_instantly_and_survives_a_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:enderman").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let target = Vec3::new(30.0, 0.0, 30.0);
        sim.get_mut(id).expect("alive").teleport_to(target);
        assert_eq!(
            sim.position(id),
            Some(target),
            "teleport must move the mob to exactly the target"
        );

        sim.tick();
        assert_eq!(
            sim.position(id),
            Some(target),
            "a tick after teleport must not undo it"
        );
    }

    /// Primitive 4: a `damage_self` request is drained by [`MobSim::tick`] and
    /// resolved into real health change — a bee that damages itself for its
    /// full health is gone at the end of the same tick, matching vanilla's
    /// immediate death removal.
    #[test]
    fn damage_self_is_resolved_into_a_real_self_kill() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:bee").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        let health = sim.get(id).expect("alive").health();

        sim.get_mut(id).expect("alive").damage_self(health);
        assert_eq!(
            sim.get(id).expect("alive").health(),
            health,
            "the request alone must not change health — only the tick drain resolves it"
        );
        sim.tick();
        assert!(
            sim.get(id).is_none(),
            "a mob that damaged itself for its full health must be removed by \
             the end of the tick"
        );
    }

    /// Primitive 5: an owner id set on the host resolves to an owner *position*
    /// across the seam each tick.
    #[test]
    fn owner_id_resolves_to_an_owner_position_across_the_seam() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let wolf = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let owner_id = sim.spawn_species(wolf.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let pet_id = sim.spawn_species(wolf, Vec3::new(3.0, 0.0, 3.0)).id();
        sim.get_mut(pet_id).expect("alive").set_owner_id(Some(owner_id));

        assert_eq!(
            sim.get(pet_id).expect("alive").owner_position(),
            None,
            "before the first tick the seam has not resolved the owner"
        );

        sim.tick();
        let owner_pos = sim.get(owner_id).expect("alive").position();
        assert_eq!(
            sim.get(pet_id).expect("alive").owner_position(),
            Some(owner_pos),
            "the feed must resolve the owner id to the owner's current position"
        );
    }
}

/// Issue #456's host half: block-identity cues read from the jar's own tag
/// census, and the graze handoff out of an immutably-borrowed world.
#[cfg(test)]
mod block_cues_tests {
    use super::*;

    /// The jar's real `#minecraft:edible_for_sheep` membership
    /// (`data/minecraft/tags/block/edible_for_sheep.json`), transcribed here
    /// **only as the expectation**. The implementation does not contain this
    /// list — it resolves the tag through `lodestone_data::tool`, which is
    /// generated from the jar — so this is an independent statement of the answer
    /// rather than a restatement of the code under test.
    const JAR_EDIBLE: &[&str] = &[
        "minecraft:short_grass",
        "minecraft:short_dry_grass",
        "minecraft:tall_dry_grass",
        "minecraft:fern",
    ];

    /// A single cell of `block` with air around it, at a fixed position.
    fn world_of(block: &str) -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, block);
        world
    }

    /// **The gate that a hand-written tag list fails.**
    ///
    /// Every member of the jar's tag must classify as edible. Three of the four
    /// would have been missed by the obvious `short_grass | tall_grass` guess:
    /// `short_dry_grass`, `tall_dry_grass` and `fern`. A sheep would have refused
    /// to graze a fern, and no test in the tree would have said so.
    #[test]
    fn every_jar_tag_member_classifies_as_edible_for_sheep() {
        for block in JAR_EDIBLE {
            let world = world_of(block);
            assert!(
                world.block_cues(0, 0, 0).edible_for_sheep,
                "{block} is in #minecraft:edible_for_sheep and must classify as edible — \
                 a hand-written list missing it is exactly how this stays silently wrong"
            );
        }
    }

    /// **The other half of the same mistake: the guess's false positive.**
    ///
    /// `tall_grass` is *not* in `#minecraft:edible_for_sheep` — the jar tag has
    /// four entries and that is not one of them. It is the block most likely to be
    /// added by anyone writing the list from memory, and asserting only the
    /// positives above would let it through.
    #[test]
    fn tall_grass_is_not_edible_for_sheep_despite_looking_like_it_should_be() {
        let world = world_of("minecraft:tall_grass");
        assert!(
            !world.block_cues(0, 0, 0).edible_for_sheep,
            "minecraft:tall_grass is absent from the jar's edible_for_sheep tag; \
             classifying it as edible means the tag is being guessed, not read"
        );
    }

    /// `grass_block` is the *equality* cue, not a tag member — vanilla tests it
    /// with block equality (`ai/goal/EatBlockGoal.java:34`, `:71`). So it must set
    /// `grass_block` and must **not** set `edible_for_sheep`: a sheep standing on
    /// grass eats the block below, a sheep standing in short grass eats the block
    /// at its feet, and conflating the two would make either mechanism fire in the
    /// wrong place.
    #[test]
    fn grass_block_is_the_equality_cue_and_not_a_tag_member() {
        let cues = world_of("minecraft:grass_block").block_cues(0, 0, 0);
        assert!(cues.grass_block, "grass_block must set its own cue");
        assert!(
            !cues.edible_for_sheep,
            "grass_block is not in the edible tag — the two cues are independent"
        );
    }

    /// The negative control. Ordinary blocks and air must set neither cue,
    /// otherwise the positives above are satisfied by a classifier that says yes
    /// to everything.
    #[test]
    fn control_ordinary_blocks_set_no_cue_at_all() {
        for block in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log"] {
            let cues = world_of(block).block_cues(0, 0, 0);
            assert!(
                !cues.edible_for_sheep && !cues.grass_block,
                "{block} must set no cue; a classifier that says yes to everything \
                 passes every positive assertion above"
            );
        }
    }

    /// Property strings must not defeat the lookup: `block_state` yields a full
    /// state string, so a cue keyed on the raw string would miss any block with
    /// properties. `tall_dry_grass` is a real tag member *and* carries a
    /// `half`/`facing`-style property list in some states, which is why this is a
    /// distinct case rather than a restatement of the first test.
    #[test]
    fn a_state_with_properties_still_classifies() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, "minecraft:short_grass");
        assert!(world.block_cues(0, 0, 0).edible_for_sheep);
        // The `grass_block` arm goes through the same property strip.
        world.set_block(0, 1, 0, "minecraft:grass_block[snowy=false]");
        assert!(
            world.block_cues(0, 1, 0).grass_block,
            "a state with a property list must still match the equality cue — \
             `block_state` returns the full string, properties included"
        );
    }

    /// **The handoff gate.** A grazing mob's eat must survive `MobSim::tick` and
    /// come out of [`MobSim::take_grazes`].
    ///
    /// The goal is installed directly rather than through the roster, because
    /// `roster/passive.rs`'s sheep row is still `Registration::missing` — that flip
    /// is #456's other brokered patch and is not this file. So this gate is about
    /// the *handoff* (`take_new_eaten` → `pending_grazes` → `take_grazes`), which
    /// is the half that lives here, and it will keep passing unchanged once the
    /// roster row lands.
    ///
    /// It is deliberately **not** an assertion about the eat interval. That is
    /// `lodestone-entity`'s `block_perception.rs` gate, which distinguishes 444
    /// predicted eats from 286 — and which also recorded that a rate measured in a
    /// mutating world measures grass scarcity instead. Nothing drains the world
    /// here, so supply is infinite and the tick budget only has to make "at least
    /// one eat" overwhelmingly likely: at the halved 1-in-500 adult interval,
    /// 20,000 ticks puts the probability of zero at about e^-40.
    #[test]
    fn a_grazing_mob_hands_its_eat_to_the_driver() {
        let mut world = ChunkWorld::new(-64, 384);
        // Grass to stand on, short grass to stand in — so both cues are live and
        // whichever arm fires, the handoff is exercised.
        //
        // Wide enough that `RandomStrollGoal` cannot walk the sheep off it in
        // 20,000 ticks. That is not padding: at 5×5 the sheep reached the edge and
        // grazed at (-2, 0, -2), and outside the patch there is no floor at all,
        // so a narrower world tests falling rather than grazing.
        for x in -24..=24 {
            for z in -24..=24 {
                world.set_block(x, -1, z, "minecraft:grass_block");
                world.set_block(x, 0, z, "minecraft:short_grass");
            }
        }

        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                ResourceKey::from_str("minecraft:sheep").expect("valid key"),
                Vec3::new(0.5, 0.0, 0.5),
            )
            .id();
        sim.get_mut(id).expect("just spawned").add_goal(
            5,
            Box::new(lodestone_entity::ai::goals::EatBlockGoal::new()),
        );

        assert!(
            sim.take_grazes().is_empty(),
            "precondition: nothing is pending before any tick, so the assertion \
             below cannot be satisfied by a stale entry"
        );

        let mut grazes = Vec::new();
        for _ in 0..20_000 {
            sim.tick();
            grazes.extend(sim.take_grazes());
            if !grazes.is_empty() {
                break;
            }
        }

        assert!(
            !grazes.is_empty(),
            "a sheep standing in short grass on a grass block must record an eat \
             that reaches take_grazes; empty means the handoff is broken and #238 \
             can never mutate the world"
        );
        // The recorded position must be the *mob's* cell, not the eaten block's —
        // the consumer resolves `AtFeet` as that cell and `Below` as one down, so
        // reporting the eaten cell would make the `Below` arm write dirt a block
        // too low.
        //
        // **`y` is the whole assertion.** `x`/`z` are identical for both
        // candidates, so they carry no information about which one this is; only
        // the height distinguishes the mob's feet (`0`) from the grass block it
        // stands on (`-1`). An earlier draft of this pinned the full triple to
        // `(0, 0, 0)` and failed at `(-2, 0, -2)` — `RandomStrollGoal` had walked
        // the sheep two blocks before it grazed, so that assertion was testing a
        // false premise (that the mob holds still) rather than the handoff.
        let (pos, _what) = grazes[0];
        assert_eq!(
            pos.y, 0,
            "the handoff must carry the mob's own feet cell (y=0), not the grass \
             block below it (y=-1) — the EatenBlock variants are relative to the mob"
        );
        assert!(
            (-24..=24).contains(&pos.x) && (-24..=24).contains(&pos.z),
            "the graze must be recorded somewhere on the prepared patch, got \
             ({}, {}) — off-patch means the sheep grazed a cell with no grass",
            pos.x,
            pos.z
        );

        // Draining really drains: a second read must not re-report the same eat,
        // or a slow consumer would apply it twice.
        assert!(
            sim.take_grazes().is_empty(),
            "take_grazes must drain, not merely read"
        );
    }
}

/// Gates on [`is_hostile_species`] (issue #457), which stood at the original
/// eight names long after the roster grew to twenty-seven species.
#[cfg(test)]
mod hostility_category_tests {
    use super::*;
    use lodestone_entity::ai::roster;

    /// Every species any roster family claims, paired with the `MobCategory`
    /// vanilla registers it under in `EntityTypes.java`.
    ///
    /// **This is an independent statement of the answer, not a restatement of
    /// the code under test**: the values were read from the jar's
    /// `EntityType.Builder.of(X::new, MobCategory.…)` registrations, which is a
    /// different file and a different mechanism from the `matches!` under test.
    /// `true` here means `MONSTER`.
    ///
    /// Note the two rows a "derive it from the attribute template" heuristic
    /// gets wrong, and which are the reason this table exists rather than a
    /// clever predicate: **`ghast` is `MONSTER`** despite a bare-`Mob`
    /// attribute builder with no `attack_damage`, and **`snow_golem` is
    /// `MISC`** — neither `Monster` nor `Creature`, so it is `false` here for
    /// want of a third state (see [`is_hostile_species`]'s own doc).
    const JAR_CATEGORY: &[(&str, bool)] = &[
        // hostile_melee
        ("zombie", true),
        ("husk", true),
        ("zombie_villager", true),
        ("drowned", true),
        ("creeper", true),
        ("spider", true),
        ("cave_spider", true),
        ("skeleton", true),
        ("stray", true),
        ("bogged", true),
        ("parched", true),
        ("wither_skeleton", true),
        // ranged
        ("blaze", true),
        ("snow_golem", false), // MobCategory.MISC — see above
        // passive
        ("cow", false),
        ("mooshroom", false),
        ("sheep", false),
        ("pig", false),
        ("chicken", false),
        ("rabbit", false),
        // neutral — all four are non-`MONSTER` *or* conditionally hostile;
        // enderman and zombified_piglin are registered `MONSTER`, bee and wolf
        // `CREATURE`. Hostility-on-sight is a separate axis the roster owns.
        ("enderman", true),
        ("zombified_piglin", true),
        ("bee", false),
        ("wolf", false),
        // specialist
        ("guardian", true),
        ("elder_guardian", true),
        ("ghast", true), // MobCategory.MONSTER, bare-`Mob` attributes
    ];

    fn key(path: &str) -> ResourceKey {
        ResourceKey::from_str(&format!("minecraft:{path}")).expect("valid key")
    }

    /// The coverage half: **every species the roster claims must appear in
    /// [`JAR_CATEGORY`]**, so adding a species to a family without deciding its
    /// spawn category fails here instead of silently defaulting to `Creature`.
    ///
    /// This is the assertion the old eight-name list could never have had, and
    /// it is driven from `roster::*::SPECIES` — the same lists `goals_for`
    /// dispatches on — rather than from a copy of them.
    #[test]
    fn every_rostered_species_has_a_decided_category() {
        let all: Vec<&str> = roster::hostile_melee::SPECIES
            .iter()
            .chain(roster::ranged::SPECIES)
            .chain(roster::passive::SPECIES)
            .chain(roster::neutral::SPECIES)
            .chain(roster::specialist::SPECIES)
            .copied()
            .collect();
        assert!(
            !all.is_empty(),
            "the roster exported no species, so this gate measured nothing"
        );

        let undecided: Vec<&str> = all
            .iter()
            .copied()
            .filter(|s| !JAR_CATEGORY.iter().any(|(name, _)| name == s))
            .collect();
        assert!(
            undecided.is_empty(),
            "these rostered species have no jar-cited spawn category, so they \
             silently fall through to persistent Creature (#457): {undecided:?}"
        );
    }

    /// [`DEMO_SPECIES`]'s two invariants (issue #457): every entry is claimed
    /// by a roster family, and the first six span all five families.
    ///
    /// The first half is what stops a typo or a plausible-but-unrostered name
    /// (`"villager"`, `"bat"`) from spawning a mob that renders fine and
    /// exercises nothing — `roster::registrations_for` answers `FALLBACK` for
    /// an unclaimed species rather than failing, so nothing else would notice.
    ///
    /// The second half is the one that matters for the issue: seeding six
    /// mobs of six *different monsters* would still leave four families at zero
    /// pixels, which is the defect, not the fix.
    #[test]
    fn demo_species_are_all_rostered_and_span_every_family() {
        use lodestone_entity::ai::roster;

        assert!(
            !DEMO_SPECIES.is_empty(),
            "an empty list would make both checks below vacuous"
        );

        let unclaimed: Vec<&str> = DEMO_SPECIES
            .iter()
            .copied()
            .filter(|s| roster::is_fallback(roster::registrations_for(s)))
            .collect();
        assert!(
            unclaimed.is_empty(),
            "these DEMO_SPECIES entries are claimed by no roster family, so they \
             spawn with FALLBACK goals and demonstrate nothing: {unclaimed:?}"
        );

        // `mob_count` in `lodestone-shell/src/net.rs`. Stated here as the
        // expectation this list is ordered against; if production changes it,
        // the ordering argument in `DEMO_SPECIES`' doc needs revisiting.
        const PRODUCTION_COUNT: usize = 6;
        let families: [(&str, &[&str]); 5] = [
            ("hostile_melee", roster::hostile_melee::SPECIES),
            ("ranged", roster::ranged::SPECIES),
            ("passive", roster::passive::SPECIES),
            ("neutral", roster::neutral::SPECIES),
            ("specialist", roster::specialist::SPECIES),
        ];
        let first_six = &DEMO_SPECIES[..PRODUCTION_COUNT.min(DEMO_SPECIES.len())];
        let unreached: Vec<&str> = families
            .iter()
            .filter(|(_, members)| !first_six.iter().any(|s| members.contains(s)))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            unreached.is_empty(),
            "a default singleplayer world seeds {PRODUCTION_COUNT} mobs, and these \
             roster families are not among them — so their goal tables still reach \
             zero pixels, which is exactly the #457 defect: {unreached:?}"
        );
    }

    /// The seeder really produces those species — not merely that the constant
    /// lists them.
    ///
    /// Drives [`seed_demo_mobs`] itself (the function `MobHandle::reseed` calls
    /// in production) rather than restating the loop, and reads the entity types
    /// back off the resulting sim's snapshots. The assertion that matters is
    /// **`> 1` distinct types**: a seeder that still hardcoded one species would
    /// produce exactly one and pass any "mobs exist" check.
    #[test]
    fn the_seeder_spawns_more_than_one_species() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -12..=12 {
            for z in -12..=12 {
                world.set_solid(x, -1, z, true);
            }
        }
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);
        seed_demo_mobs(&mut sim, 0, 0, 6);

        let types: Vec<String> = sim
            .snapshots()
            .iter()
            .map(|s| s.entity_type.path().to_string())
            .collect();
        assert_eq!(types.len(), 6, "six requested mobs must all reach the sim");

        let mut distinct: Vec<&str> = types.iter().map(String::as_str).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 1,
            "the seeder produced only {distinct:?} — a single-species ring is the \
             #457 defect, and 'mobs were spawned' passes for it"
        );
        assert_eq!(
            types[0], "zombie",
            "the first demo mob must stay a zombie: entity id 1000 is \
             deterministic and live_mob_sim.rs depends on it"
        );
        for want in ["cow", "wolf", "blaze", "guardian", "creeper"] {
            assert!(
                types.iter().any(|t| t == want),
                "a default world must contain a {want}; got {types:?}"
            );
        }
    }

    /// The value half: the predicate must agree with the jar for every row.
    #[test]
    fn hostility_matches_the_jar_registration_for_every_rostered_species() {
        let mut wrong = Vec::new();
        for &(path, want) in JAR_CATEGORY {
            let got = is_hostile_species(&key(path));
            if got != want {
                wrong.push(format!("{path}: want {want}, got {got}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "is_hostile_species disagrees with EntityTypes.java: {wrong:?}"
        );

        // Control: the predicate is capable of answering `false`, so the
        // agreement above is not "everything is hostile". A species with no
        // roster entry and no reason to be a monster must still be `false`.
        assert!(
            !is_hostile_species(&key("armadillo")),
            "an unlisted species must fall through to Creature — if this is \
             true, the predicate has stopped discriminating and the whole \
             table above passes vacuously"
        );
    }

    /// The category the predicate feeds must actually reach the spawned mob:
    /// [`MobSim::spawn_species`] is the production path, and a predicate whose
    /// answer never lands on a `SimMob` is the island shape this repo keeps
    /// paying for.
    #[test]
    fn the_decided_category_reaches_a_spawned_mob() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=4 {
            for z in -4..=4 {
                world.set_solid(x, -1, z, true);
            }
        }
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);

        let pos = Vec3::new(0.5, 0.0, 0.5);
        let ghast = sim.spawn_species(key("ghast"), pos).category();
        let wolf = sim.spawn_species(key("wolf"), pos).category();

        assert_eq!(
            ghast,
            MobCategory::Monster,
            "a ghast is MobCategory.MONSTER (EntityTypes.java:473); if this is \
             Creature the widened list is not reaching spawn_species"
        );
        assert_eq!(
            wolf,
            MobCategory::Creature,
            "a wolf is MobCategory.CREATURE (EntityTypes.java:1073)"
        );
    }
}

