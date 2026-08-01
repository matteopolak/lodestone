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
//! # Scope, honestly
//!
//! This ticks AI over terrain. It does **not** stream the resulting positions to
//! a connected client: that needs a version crate's client-bound `add_entity` /
//! `move_entity` *encoders*, which this version-free crate may not implement
//! without naming a protocol number — the same reported encoder seam
//! `serve_connection` already documents. So this is the server-authoritative
//! simulation half; the wire half is a separate seam. Keeping them separate is
//! the point: a wrong-because-half-built loop is worse than an honest boundary.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;

use lodestone_data::{block_states, collision_shapes, path_types};
use lodestone_entity::ai::goals::{RandomLookAroundGoal, RandomStrollGoal};
use lodestone_entity::ai::{Goal, GoalSelector, MobController, NavigatingMob};
use lodestone_entity::attribute::default_attributes;
use lodestone_entity::explosion::Aabb as ExplosionAabb;
use lodestone_entity::pathfinding::{Aabb, MobShape, PathType, PathWorld};
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

/// The server-side mob simulation: owns the live mobs and advances them.
///
/// The [`ChunkWorld`] is borrowed (the mobs path over it), so the caller holds
/// the world and hands it here. Drive the sim with [`tick`](MobSim::tick) once
/// per game tick, or [`tick_for`](MobSim::tick_for) to run many.
#[derive(Debug)]
pub struct MobSim<'w> {
    world: &'w ChunkWorld,
    mobs: Vec<SimMob<'w>>,
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
            next_id: 1,
            tick_count: 0,
        }
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
        let id = self.next_id;
        self.next_id += 1;
        let entity_type =
            ResourceKey::from_str("minecraft:zombie").expect("static key is valid");
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
