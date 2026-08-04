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
use lodestone_entity::ai::goals::{MeleeAttackGoal, RandomLookAroundGoal, RandomStrollGoal};
use lodestone_entity::ai::{Goal, GoalSelector, MobController, NavigatingMob};
use lodestone_entity::attribute::default_attributes;
use lodestone_entity::explosion::Aabb as ExplosionAabb;
use lodestone_entity::item_entity::{ItemEntityRegistry, ItemLifecycle, ItemMotion};
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
use lodestone_entity::projectile::{Projectile, ProjectileRegistry, TrackedProjectile};
use lodestone_entity::{
    AttributeMap, DamageFlags, Defenses, HurtCooldown, HurtDecision, RayView, entity_damage,
    seen_percent,
};
use lodestone_model::PathType as CensusPathType;
use lodestone_model::{Identifier, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::chunk::{AIR, ChunkColumn, ChunkSource};
use crate::protocol::EntitySnapshot;
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

/// The health and combat-stat defaults for a mob type: `(max_health,
/// attack_damage, defenses)`.
///
/// Folds through [`default_attributes`] when `entity_type` is one of the
/// vanilla templates that module knows (the zombie family, skeleton family,
/// creeper, spider, and the common animals); for anything else it falls back
/// to an empty [`AttributeMap`], whose [`AttributeMap::value`] already resolves
/// every path to the generic `RangedAttribute` default (`max_health` 20,
/// `attack_damage` 2, no armor) — the same "unknown type gets the generic
/// default, never a guess" shape [`resolve_mob_shape`](crate::resolve_mob_shape)
/// uses for census geometry.
fn combat_defaults(entity_type: &ResourceKey) -> (f32, f32, Defenses) {
    let attrs = default_attributes(entity_type).unwrap_or_else(AttributeMap::new);
    let max_health = attr(&attrs, "max_health") as f32;
    let attack_damage = attr(&attrs, "attack_damage") as f32;
    let defenses = Defenses {
        armor: attr(&attrs, "armor") as f32,
        armor_toughness: attr(&attrs, "armor_toughness") as f32,
        ..Defenses::default()
    };
    (max_health, attack_damage, defenses)
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

/// Whether `entity_type` is one of the hostile "monster" species that should
/// get a target-seeking [`MeleeAttackGoal`] by default, versus a passive
/// species that only wanders and looks around.
///
/// A coarse species→behaviour split, not a full roster: it covers exactly the
/// families [`lodestone_entity::attribute::default_attributes`]'s own
/// hand-verified template table names (`type_spec`, that module's private
/// function) as `Monster`-templated. Per-species roster issues refine
/// individual species' actual goal sets on top of this baseline; this only
/// has to make two different species behave *observably* differently instead
/// of both getting an empty [`GoalSelector`].
fn is_hostile_species(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        "zombie" | "husk" | "skeleton" | "stray" | "wither_skeleton" | "bogged" | "creeper" | "spider"
    )
}

/// One live mob in the simulation: its [`NavigatingMob`] body and its own
/// [`GoalSelector`].
///
/// Configure it after spawning with [`add_goal`](SimMob::add_goal) and
/// [`set_attack_target`](SimMob::set_attack_target); observe it with
/// [`position`](SimMob::position) / [`path_searches`](SimMob::path_searches).
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
    #[must_use]
    pub fn snapshot(&self) -> EntitySnapshot {
        EntitySnapshot {
            id: self.id,
            uuid: self.uuid,
            entity_type: self.entity_type.clone(),
            position: self.position(),
            rotation: self.rotation(),
            head_yaw: self.head_yaw(),
            velocity: self.velocity(),
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
/// project's own server. `MobSim` is the same home `run_mob_tick_loop` already
/// ticks every server tick for mobs, so folding these two in here (rather
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
        }
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
        let (max_health, attack_damage, defenses) = combat_defaults(&entity_type);
        self.mobs.push(SimMob {
            id,
            mob: NavigatingMob::new(self.world, shape, pos, step_per_tick, visited_budget),
            goals: GoalSelector::new(),
            category: MobCategory::Monster,
            no_action_time: 0,
            persistent: false,
            uuid: Uuid::new_v4(),
            entity_type,
            health: max_health,
            defenses,
            attack_damage,
            hurt_cooldown: HurtCooldown::default(),
            attack_target_id: None,
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
    /// * **Goals**: every species gets the wander/look baseline
    ///   [`run_spawn_cycle`](Self::run_spawn_cycle) already gives a naturally
    ///   spawned mob. A hostile species ([`is_hostile_species`]) additionally
    ///   gets a [`MeleeAttackGoal`], so it can actually connect once something
    ///   gives it an [`attack_target`](SimMob::set_attack_target) — a passive
    ///   species never does, structurally, regardless of what target it is
    ///   given. This is deliberately *not* the full per-species roster
    ///   (`NearestAttackableTargetGoal`'s own population search is a separate,
    ///   larger feature, the same explicit scope cut this file already makes
    ///   for breeding candidates); it is the baseline that makes two species
    ///   behave observably differently instead of both getting nothing.
    pub fn spawn_species(&mut self, entity_type: ResourceKey, pos: Vec3) -> &mut SimMob<'w> {
        let attrs = default_attributes(&entity_type).unwrap_or_else(AttributeMap::new);
        let shape = species_shape(&entity_type, &attrs);
        let step_per_tick = attr(&attrs, "movement_speed");
        let visited_budget = (attr(&attrs, "follow_range") * 16.0).floor() as i32;
        let hostile = is_hostile_species(&entity_type);

        let mob = self.spawn_with_type(pos, shape, step_per_tick, visited_budget, entity_type);
        mob.set_category(if hostile {
            MobCategory::Monster
        } else {
            MobCategory::Creature
        })
        .set_persistent(!hostile)
        .add_goal(0, Box::new(RandomStrollGoal::new(step_per_tick)))
        .add_goal(1, Box::new(RandomLookAroundGoal::new()));
        if hostile {
            mob.add_goal(2, Box::new(MeleeAttackGoal::new(step_per_tick.max(0.2), 2.0)));
        }
        mob
    }

    /// Advances every mob one tick: run its goals (which drive A\* and path
    /// following through the [`MobController`] seam), then step the follower.
    /// Each mob's `no_action_time` ages by one tick, mirroring vanilla
    /// `serverAiStep`'s `noActionTime++`; [`despawn_pass`](MobSim::despawn_pass)
    /// consumes and resets it.
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
        let mut hits: Vec<(Option<i32>, f32)> = Vec::new();
        for m in &mut self.mobs {
            // Vanilla ages `invulnerableTime`/`hurtTime` every tick regardless
            // of whether the mob was hit this tick.
            m.hurt_cooldown.tick();
            m.mob.tick(&mut m.goals);
            m.no_action_time = m.no_action_time.saturating_add(1);
            if !m.mob.take_new_attacks().is_empty() {
                hits.push((m.attack_target_id, m.attack_damage));
            }
        }
        for (target_id, raw_damage) in hits {
            if let Some(target_id) = target_id
                && let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id)
            {
                target.apply_damage(raw_damage, DamageFlags::default());
            }
        }
        self.mobs.retain(|m| m.health > 0.0);

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
        for state in self.item_state.values_mut() {
            state.motion.tick();
        }

        self.tick_count += 1;
    }

    /// Runs [`tick`](MobSim::tick) `n` times.
    pub fn tick_for(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
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
        self.mobs.retain(|m| m.health > 0.0);
        dealt
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

    /// The current age/pickup-delay/count lifecycle of a tracked dropped
    /// item, if any.
    #[must_use]
    pub fn item_lifecycle(&self, id: i32) -> Option<&ItemLifecycle> {
        self.items.get(id)
    }

    /// Every live entity this sim owns — mobs, projectiles, dropped items —
    /// lowered to the wire-facing [`EntitySnapshot`] the encode seam needs.
    ///
    /// This is the merged sibling of iterating [`iter`](Self::iter) alone:
    /// [`run_mob_tick_loop`] publishes this (not just the mobs) to
    /// [`LiveMobSource`], which is what actually gets a spawned projectile or
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
                });
            }
        }
        for (&id, state) in &self.item_state {
            out.push(EntitySnapshot {
                id,
                uuid: state.uuid,
                entity_type: state.item.clone(),
                position: state.motion.position,
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: state.motion.velocity,
            });
        }
        out
    }
}

/// Squared horizontal+vertical distance between two positions (vanilla
/// `distanceToSqr`).
fn dist_sqr(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

// NOTE: this module owns `ChunkWorld` + `MobSim`; the acceptance gate lives in
// `tests/mob_sim.rs` so it drives them through the crate's *public* API — the
// same discipline the rest of the project uses (a consumer that is only a
// `#[cfg(test)]` fake proves nothing about the public seam).

/// A live [`EntitySource`] fed by a background-ticked [`MobSim`] (issue
/// #217). [`IntegratedServer::open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs)
/// constructs one alongside [`run_mob_tick_loop`], the task that owns the sim
/// and republishes its snapshots here every tick.
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
    /// Replaces the published snapshot set. Called once per tick by
    /// [`run_mob_tick_loop`]; the next `snapshots()` call from any connection
    /// (there may be several, e.g. open-to-LAN) sees the new set.
    fn publish(&self, snapshots: Vec<EntitySnapshot>) {
        *self.0.lock().expect("live mob snapshot lock poisoned") = snapshots;
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
fn seed_demo_mobs(sim: &mut MobSim<'_>, world: &ChunkWorld, center_x: i32, center_z: i32, count: usize) {
    let zombie = ResourceKey::from_str("minecraft:zombie").expect("static key is valid");
    for i in 0..count.max(1) {
        let angle = (i as f64) * std::f64::consts::TAU / (count.max(1) as f64);
        let x = center_x + (angle.cos() * 6.0).round() as i32;
        let z = center_z + (angle.sin() * 6.0).round() as i32;
        let Some(y) = surface_y(world, x, z) else {
            continue;
        };
        let pos = Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5);
        let mob = sim.spawn(pos, MobShape::land(0.6, 1.95), 0.23, 400);
        mob.set_entity_type(zombie.clone())
            .add_goal(0, Box::new(RandomStrollGoal::new(0.23)))
            .add_goal(1, Box::new(RandomLookAroundGoal::new()));
    }
}

/// Native tick-loop driver for issue #217: owns a [`ChunkWorld`] snapshot and
/// a seeded [`MobSim`] for the lifetime of the returned future, ticking it
/// once every [`MOB_TICK_INTERVAL`] and republishing snapshots to `out` after
/// every tick.
///
/// `world_source` supplies the terrain the mobs path over. It is intended to
/// be a **second, independent instance** of whatever [`ChunkSource`] the
/// paired connection is also streaming terrain from — not the same shared
/// value — because this future needs to *own* a `'static` snapshot for its
/// whole lifetime (`ChunkWorld::from_source` copies the requested columns out
/// once, up front) while the connection's own `source` is moved into a
/// different, independently-spawned task. Every `ChunkSource` this crate
/// ships (`OverworldChunkSource`, `WorldgenChunkSource`) is a pure function of
/// its construction parameters/seed, so two instances built the same way
/// produce identical terrain — this is two handles onto the same
/// deterministic world, not two different worlds.
///
/// # Scope cuts, explicit
///
/// * The `ChunkWorld` snapshot loaded here is **static** for the task's whole
///   lifetime — nothing re-queries `world_source` after the initial load, so a
///   mob does not path across a chunk boundary that was not included in
///   `cx_range`/`cz_range`. Widening this to grow with the player's view is
///   future work; for a first live driver a fixed area around spawn is an
///   honest, working scope cut rather than a silent limitation.
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
pub(crate) async fn run_mob_tick_loop<S: ChunkSource>(
    world_source: S,
    cx_range: std::ops::RangeInclusive<i32>,
    cz_range: std::ops::RangeInclusive<i32>,
    center_x: i32,
    center_z: i32,
    mob_count: usize,
    out: LiveMobSource,
) {
    let world = ChunkWorld::from_source(&world_source, cx_range, cz_range);
    let mut sim = MobSim::new(&world);
    // See `set_next_id`'s own doc comment: id `1` is `LOCAL_PLAYER_ENTITY_ID`
    // on the wire (v770's, and plausibly any future protocol's own "self" id
    // — starting well clear of the low reserved range is the safe default
    // regardless), so mob ids start at 1000 rather than `MobSim::new`'s
    // hermetic-test-facing default of `1`.
    sim.set_next_id(1000);
    seed_demo_mobs(&mut sim, &world, center_x, center_z, mob_count);
    // `snapshots()`, not `sim.iter().map(SimMob::snapshot)`: the latter only
    // ever lowered mobs, so a projectile or dropped item registered on this
    // `sim` (issues #211/#215) would tick correctly and still never reach
    // `LiveMobSource` — ticking without publishing is still an island, just
    // one hop further along.
    out.publish(sim.snapshots());

    // 50ms, matching vanilla's 20 TPS and this crate's own `VITALS_TICK_INTERVAL`
    // (`server.rs`) — kept as a local constant rather than sharing that one
    // because it is `server.rs`-private and the two are allowed to drift
    // independently (mob AI has no reason to share a literal with drowning
    // damage beyond both wanting "one vanilla tick").
    const MOB_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
    let mut tick = tokio::time::interval(MOB_TICK_INTERVAL);
    loop {
        tick.tick().await;
        sim.tick();
        out.publish(sim.snapshots());
    }
}
