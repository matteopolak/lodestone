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
use std::sync::{Arc, Mutex};

use lodestone_data::{block_states, collision_shapes, entity_dimensions, entity_types, path_types};
// `collide` and `CollisionView` are the item pass's swept resolve against the real
// per-state shape census (see `ItemCollision`); `Vec3d` is the physics crate's own
// vector, which `Vec3` (this crate's) is converted to at that seam rather than
// through the whole module.
use lodestone_physics::{CollisionView, EntityDimensions, Vec3d, collision::collide};
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
use lodestone_model::{BlockPos, Difficulty, Identifier, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::chunk::{AIR, ChunkColumn, ChunkSource};
use crate::gravity_tick::FallingBlockEffect;
use crate::protocol::{EntitySnapshot, MetadataField};
use crate::mob_spawn::{
    DespawnOutcome, MobCategory, SpawnCandidateSource, SpawnRng, SpawnState, check_despawn,
};
use crate::server::EntitySource;

mod block_ids;

// Re-exported so `crate::mobs::block_state_id`/`block_state_id_or_default` keep
// resolving for every existing caller outside this module — `block_breaking.rs`,
// `block_drops.rs`, `boat.rs`, `spawn_egg.rs`, `random_tick.rs`, `effects.rs` and
// `piston.rs` all name them through that exact path, and none of those files are
// this split's to touch.
pub(crate) use block_ids::{block_state_id, block_state_id_or_default};

mod world;

// Re-exported so `crate::mobs::ChunkWorld` keeps resolving: `lib.rs`'s own
// `pub use mobs::{..., ChunkWorld, ...}`, plus `tick_area.rs`, `integrated.rs`
// and `tick.rs`, all name it through that exact path.
pub use world::ChunkWorld;

mod golem;

// Re-exported for path stability: unlike `ChunkWorld` above, no external
// caller names `crate::mobs::GolemConstruction`/`GolemSpecies` today (checked
// crate-wide), but both are `pub`, so keeping `mobs::GolemConstruction` and
// `mobs::GolemSpecies` resolving costs one line and avoids relying on that
// staying true.
pub use golem::{GolemConstruction, GolemSpecies};

// Villager professions and workstation claiming (issues #243, #245). `pub`
// (not re-exported at the top level) so `crate::server` can reach
// `crate::mobs::villager::trades::offers_up_to` when it builds a
// `MERCHANT_OFFERS` packet from an `InteractOutcome::OpenTrade`.
pub mod villager;

// No re-export: every item in `species` was already private in `mobs.rs`
// before this split (nothing outside this module ever named them), so
// `pub(super)` here — visible within `mobs` and its descendants — is a
// superset of that, not a narrowing, and there is no external path to keep
// stable.
mod species;

// No re-export: `ProjectileHit`/`projectile_damage_type`/`first_solid_along`
// were already private, and every `impl MobSim` method here is `pub`, so its
// path (`MobSim::spawn_projectile`, etc.) does not depend on which file the
// `impl` block textually lives in.
mod projectiles;

// No re-export: every `impl MobSim` method here was already `pub`, so its
// path is unaffected by the file move. `merge_neighbouring_items` is
// `pub(super)` in `items.rs` because `tick_with_terrain` below calls it via
// `self.merge_neighbouring_items()` — a method call, so no `items::` prefix
// is needed at that call site either.
mod items;

// No re-export: every `impl MobSim` method was already `pub`. `tick_orbs` is
// `pub(super)` for the same reason `merge_neighbouring_items` is (called from
// `tick_with_terrain` below), and `ORB_BEHAVIOR_SEED` is `pub(super)` because
// `MobSim::new` reads it directly as `orbs::ORB_BEHAVIOR_SEED`.
mod orbs;

// No re-export: every method here was already `pub`.
mod falling_blocks;

// No re-export: every `impl MobSim` method here was already `pub`;
// `VehicleCollision` stays private, used only within this file's own
// `tick_vehicles`.
mod vehicles;

// No re-export: every `impl MobSim` method in each is already `pub`. See
// `mobs::dragon`/`mobs::end_crystal`'s own module docs for the pure
// `crate::dragon` state machine each drives.
mod dragon;
mod end_crystal;

// See `mobs::wither`/`mobs::wither_pattern`'s own module docs: the wither
// boss's `crate::wither` state driven with real inputs, plus the summon-
// structure block-pattern matcher (`golem.rs`'s own approach, duplicated for
// a different cell alphabet rather than widened, per that module's own
// closed-`GolemCell`-enum shape).
mod wither;
mod wither_pattern;

// `pub(crate)`, unlike every sidecar module above: `crate::block_drops`,
// `crate::random_tick`, `crate::tick` and `crate::server` all need
// `tnt::is_tnt_block`/`TICK_TNT_PRIME`/`DEFAULT_FUSE_TIME` to recognise and
// schedule TNT ignition from outside this crate's mob simulation, which none
// of the boat/falling-block/orb sidecars need. Every `impl MobSim` method
// here was already `pub`; `TntCollision` stays private, used only within this
// file's own `tick_tnt`. `TNT_LAUNCH_SEED` is `pub(super)` for the same
// reason `ORB_BEHAVIOR_SEED` is — `MobSim::new` reads it directly as
// `tnt::TNT_LAUNCH_SEED`.
pub(crate) mod tnt;

// `pub(crate)`, the same shape `tnt` is above: `crate::redstone_dispenser`,
// `crate::item_use` and `crate::server` all need
// `minecart::{MinecartKind, is_rail_block, rail_shape, placement_position}`
// to recognise a rail and derive a spawn position from outside this crate's
// mob simulation. Every `impl MobSim` method here was already `pub`;
// `MinecartCollision` stays private, used only within this file's own
// `tick_minecarts`.
pub(crate) mod minecart;

// No re-export: `LiveBolt` is `pub(super)`, visible within `mobs` and its
// descendants, and every `impl MobSim` method here is either `pub` already or
// `pub(super)` because only `crate::tick` (through `MobHandle::with`) and this
// module's own driver plumbing call it.
mod lightning;

// Issue #257. No re-export: every `impl MobSim` method is `pub` already
// (`cast_fishing_bobber`, `retrieve_fishing_bobber`, …), `FishHookState`/
// `FishingBobber` stay `pub(super)`, and `FISHING_ROLL_SEED` is read directly
// as `fishing::FISHING_ROLL_SEED` by `MobSim::new`, the same shape
// `orbs::ORB_BEHAVIOR_SEED` uses.
mod fishing;

// Issue #241 (the raid half — patrols already exist, see `mobs::mod`'s own
// module doc and `docs/pillager-patrols.md`). No re-export: every `impl
// MobSim` method is `pub` already; `RAID_ROLL_SEED` is read the same way
// `fishing::FISHING_ROLL_SEED` is.
mod raid;

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

/// The per-tick velocity decay a grounded mob's horizontal motion is
/// subjected to on ordinary, unmodified-friction terrain: standard block
/// friction combined with the constant air-drag factor every entity carries
/// regardless of the block underfoot. See `docs/mob-species-spawning.md` for
/// the exact vanilla methods this reproduces.
const AI_GROUND_FRICTION: f64 = 0.6 * 0.91;

/// Converts a requested ground speed — a goal's `speedModifier` multiplied
/// onto the mob's `movement_speed` attribute, the unit every roster goal in
/// this crate already hands to [`NavigatingMob`](lodestone_entity::ai::navigating_mob::NavigatingMob)'s
/// `move_to` — into the sustained blocks-per-tick rate an AI-driven mob
/// actually converges on.
///
/// Vanilla's AI movement controller does not drive a mob at full input
/// magnitude the way a player's WASD does: the forward input it feeds into
/// the entity's own travel step is numerically the *same* value as the
/// per-tick speed scale applied to that input, so the two multiply — the
/// per-tick thrust actually added to the mob's velocity is the *square* of
/// the requested speed, not the value itself. That thrust then accumulates
/// against [`AI_GROUND_FRICTION`] every tick until it converges on this
/// steady cruising speed. See `docs/mob-species-spawning.md` for the exact
/// methods this reproduces and the live-oracle measurement it was checked
/// against (a real zombie's measured mean pursuit speed against its
/// predicted value).
fn ai_ground_speed(requested_speed: f64) -> f64 {
    (requested_speed * requested_speed) / (1.0 - AI_GROUND_FRICTION)
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
///
/// **Deliberately takes no `is_baby`.** Checked against every species this
/// sim spawns babies for: `Zombie.createAttributes()` and every breedable
/// `Animal`'s attribute builder set `max_health`/`attack_damage`/`armor`
/// identically regardless of age — only the hitbox
/// ([`species_shape`]/[`baby_dimensions`]) and, for the zombie family, the
/// movement speed ([`baby_speed_multiplier`]) actually differ. Threading a
/// parameter through that would change nothing for any modeled species is
/// the "vacuous species" this repo's own evidence section warns about;
/// re-check this comment before adding one, rather than assuming it is
/// missing.
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

/// Vanilla `LivingEntity.getAgeScale()`'s generic fallback: half size while a
/// baby, full size otherwise
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/LivingEntity.java:555-557`,
/// `getDefaultDimensions` folds it as `type.getDimensions().scale(getAgeScale())`
/// at `LivingEntity.java:3733`). Used only for a species with no entry in
/// [`baby_dimensions`] — vanilla itself does not treat this as "the" baby
/// rule, most breedable animals and the whole zombie family override
/// `getDefaultDimensions` with their own literal box instead of taking this
/// default, which is why it is the fallback and not the primary path.
const DEFAULT_BABY_AGE_SCALE: f32 = 0.5;

/// A species' own `BABY_DIMENSIONS` constant (`width`, `height`), pre-`SCALE`-
/// attribute — vanilla declares one per species rather than deriving it from
/// [`DEFAULT_BABY_AGE_SCALE`], and the two disagree: a baby zombie is
/// `0.49×0.98`
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/monster/zombie/Zombie.java:90`),
/// not `0.6×1.95 * 0.5 = 0.3×0.975`. Scoped to the species this sim actually
/// grows babies for — [`crate::ai::roster::passive`]'s breedable animals, the
/// wolf ([`crate::ai::roster::neutral`]) and the zombie family, which spawns
/// naturally as a baby without ever being bred; every other species falls
/// back to [`DEFAULT_BABY_AGE_SCALE`], which is `LivingEntity`'s own real
/// default rather than an approximation invented for the gap.
fn baby_dimensions(entity_type: &ResourceKey) -> Option<(f32, f32)> {
    Some(match entity_type.path() {
        // `Zombie.java:90`; `Husk`, `ZombifiedPiglin`, `Drowned` and
        // `ZombieVillager` each redeclare the identical literal for their own
        // `BABY_DIMENSIONS`.
        "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => (0.49, 0.98),
        // `animal/cow/AbstractCow.java:33` — shared by `Cow` and `MushroomCow`.
        "cow" | "mooshroom" => (0.45, 0.7),
        // `animal/sheep/Sheep.java:60`.
        "sheep" => (0.45, 0.65),
        // `animal/pig/Pig.java:70`.
        "pig" => (0.45, 0.45),
        // `animal/chicken/Chicken.java:59`.
        "chicken" => (0.3, 0.4),
        // `animal/rabbit/Rabbit.java:82`.
        "rabbit" => (0.24, 0.4),
        // `animal/wolf/Wolf.java:103`.
        "wolf" => (0.3, 0.425),
        _ => return None,
    })
}

/// The baby-only movement-speed multiplier vanilla applies as a transient
/// `MOVEMENT_SPEED` `AttributeModifier` with `ADD_MULTIPLIED_BASE`, so the
/// final speed is `base * (1.0 + amount)`. Only the zombie family carries one
/// (`Zombie.java:73-74`, amount `0.5`, applied in `ageUp`/`onSyncedDataUpdated`
/// rather than at spawn, but the net effect for a mob whose age never crosses
/// back is the same as always having it while a baby). Every breedable
/// `AgeableMob` this sim spawns — cow, sheep, pig, chicken, rabbit, wolf — has
/// **no** baby speed modifier at all, confirmed by reading each one's own
/// `registerGoals`/attribute setup: only the hitbox shrinks for them. `1.0`
/// (no change) for anything not listed.
fn baby_speed_multiplier(entity_type: &ResourceKey) -> f64 {
    match entity_type.path() {
        "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => 1.5,
        _ => 1.0,
    }
}

/// Resolves a species' body from the real 26.2 dimension census, folded with
/// its `attrs`' `SCALE`/`STEP_HEIGHT` — see [`SimMob::spawn_species`]'s own doc
/// comment for why this duplicates (rather than calls)
/// [`crate::resolve_mob_shape`]'s fold: that function takes a
/// `&dyn VersionAdapter` for a version-aware caller, but `MobSim` already
/// reads `lodestone_data` directly for its path/collision census, so there is
/// no adapter to thread through here.
///
/// `is_baby` selects [`baby_dimensions`]'s per-species literal, falling back
/// to [`DEFAULT_BABY_AGE_SCALE`] against the census base — never against the
/// `SCALE` attribute, which is applied once, uniformly, after either
/// selection (matching vanilla's separate `getDefaultDimensions().scale(getScale())`
/// fold at `LivingEntity.java:3729`).
fn species_shape(entity_type: &ResourceKey, attrs: &AttributeMap, is_baby: bool) -> MobShape {
    let scale = attr(attrs, "scale") as f32;
    let step_height = attr(attrs, "step_height") as f32;
    let base = entity_types::entity_type_id_parts(entity_type.namespace(), entity_type.path())
        .and_then(entity_dimensions::base_dimensions);
    let (width, height) = if is_baby {
        baby_dimensions(entity_type).unwrap_or_else(|| {
            let (w, h) = base.map_or((0.6, 1.95), |d| (d.width, d.height));
            (w * DEFAULT_BABY_AGE_SCALE, h * DEFAULT_BABY_AGE_SCALE)
        })
    } else {
        base.map_or((0.6, 1.95), |d| (d.width, d.height))
    };
    let mut shape = MobShape::land(width * scale, height * scale);
    shape.max_up_step = step_height;
    shape
}

/// Vanilla `Leashable.LEASH_TOO_FAR_DIST`: past this distance the lead snaps
/// (`Leashable.java:30`).
const LEASH_TOO_FAR_DIST: f64 = 12.0;

/// Vanilla `Leashable.LEASH_ELASTIC_DIST`: past this distance (minus both
/// entities' bounding-box widths, a nuance this port does not carry — see
/// [`MobSim::tick_leashes`]'s own doc comment) a pull force applies
/// (`Leashable.java:31`).
const LEASH_ELASTIC_DIST: f64 = 6.0;

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

/// `LongDistancePatrolGoal.findPatrolCompanions`'s search box,
/// `getBoundingBox().inflate(16.0)` (`monster/PatrollingMonster.java:203`).
/// Isotropic in vanilla (one `inflate` argument covers all three axes), unlike
/// [`FOLLOW_PARENT_RANGE`]'s horizontal/vertical split.
const PATROL_COMPANION_RANGE: f64 = 16.0;

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
    /// The player's normalised view direction — vanilla
    /// `Entity.calculateViewVector(xRot, yRot)`, what `LivingEntity.isLookingAtMe`
    /// calls as `target.getViewVector(1.0F)`. Feeds the enderman's gaze test
    /// (issue #458, primitive 2): [`lodestone_entity::ai::mob::is_in_view_cone`]
    /// takes this directly as its `look` argument. `Vec3::new(0.0, 0.0, 1.0)`
    /// (looking due "south", vanilla's own zero-rotation direction) is the
    /// honest default for a producer that has not resolved a real angle yet.
    pub view_direction: Vec3,
}

/// **Who** a connected player is, as the mob simulation needs to know it.
///
/// # Why both, and what each one is for
///
/// Ownership is keyed on the **uuid** and nothing else. Vanilla stores a tamed
/// animal's owner in `TamableAnimal.DATA_OWNERUUID_ID`, whose serializer is
/// `EntityDataSerializers.OPTIONAL_LIVING_ENTITY_REFERENCE`; that resolves to
/// `EntityReference.streamCodec()`, which is `UUIDUtil.STREAM_CODEC` — sixteen
/// raw bytes. The NBT form (`EntityReference`'s `store`/`read` with
/// `UUIDUtil.CODEC`) is the same uuid. So the uuid is what both the wire and the
/// save file demand, and it is also the only identity that *survives*: a runtime
/// entity id is reassigned on every reconnect, and this server derives an
/// offline-mode uuid from the username, so a pet's owner is still the same
/// person tomorrow.
///
/// The **entity id** is carried alongside because the rest of this sim's
/// identity vocabulary is `i32` entity ids — [`SimMob::attack_target_id`],
/// [`SimMob::owner_id`], [`EntitySnapshot`]'s ids — and a mob that wants to
/// exclude its owner from a target, or a snapshot that wants to name the owning
/// entity, cannot say so in uuids. Vanilla makes exactly this split:
/// `EntityReference` stores the uuid and *caches* the resolved live entity.
///
/// So: **the uuid is the identity; the entity id is the handle.** Storing only
/// the entity id would make ownership evaporate on reconnect; storing only the
/// uuid would make it unnameable to anything else in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerIdentity {
    /// The player's account uuid — what ownership is keyed on. In offline mode
    /// this is derived from the username, so it is stable across sessions.
    pub uuid: Uuid,
    /// The player's runtime entity id, valid for this session only.
    pub entity_id: i32,
}

/// One connected player as the mob simulation sees them: who they are, plus what
/// a mob can sense about them.
///
/// # Why this is a second type rather than two more fields on [`PlayerPerception`]
///
/// Because the two answer different questions and only one of them is
/// *perception*. A mob senses a position and a held item; it does not sense a
/// uuid. Ownership is a relation between a mob and a **person**, resolved by the
/// host, and putting the person inside the sense data would say that a mob can
/// see who you are.
///
/// The practical consequence is the useful one: `From<PlayerPerception>` yields a
/// view with **no** identity, which is exactly the honest state for a producer
/// that has not been taught to supply one. An unidentified player can still be
/// looked at and tempted; they simply cannot own anything, and no mob will ever
/// resolve them as an owner. That is a correct neutral default rather than a
/// wrong one — contrast keying ownership on a nil uuid, which would make every
/// unidentified player the owner of every pet tamed by any other.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceivedPlayer {
    /// Who this player is, or `None` for a producer that supplies no identity.
    pub identity: Option<PlayerIdentity>,
    /// What a mob can sense about them.
    pub perception: PlayerPerception,
}

impl From<PlayerPerception> for PerceivedPlayer {
    fn from(perception: PlayerPerception) -> Self {
        Self {
            identity: None,
            perception,
        }
    }
}

/// What [`MobSim::interact`] did — vanilla's `InteractionResult`, narrowed to
/// the outcomes this crate can actually produce.
///
/// Richer than a `bool` because the caller has to do different things with each:
/// a tame attempt consumes the item whether it succeeded or not, a sit toggle
/// consumes nothing (`InteractionResult.SUCCESS.withoutItem()`), and a `Pass`
/// must fall through to whatever else a right-click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractOutcome {
    /// Nothing on this mob responded. Vanilla's `InteractionResult.PASS`.
    Pass,
    /// A tame roll succeeded; the mob is now owned by the actor.
    Tamed,
    /// A tame roll failed. The item is still consumed — that is what makes taming
    /// cost bones rather than patience.
    TameFailed,
    /// A tame pet's owner toggled its sitting order. Consumes no item.
    SitToggled {
        /// The order's new value.
        sitting: bool,
    },
    /// The mob entered love mode; a partner in love within range will now breed
    /// with it.
    InLove,
    /// A hurt tame pet was healed. The arm that runs *instead* of breeding.
    Fed,
    /// A horse family member's `Temper` rose. Carries the new value, because the
    /// tame probability is a function of it and a caller that cannot read it
    /// cannot tell "fed" from "nearly tame".
    TemperRaised {
        /// `AbstractHorse.getTemper()` after the gain.
        temper: i32,
    },
    /// A professioned villager's trade screen should open — vanilla
    /// `Villager.mobInteract`, the full override that replaces the taming
    /// chain entirely rather than falling through it (issue #245). Consumes
    /// no item, the same as [`SitToggled`](Self::SitToggled): opening a menu
    /// is not a `usePlayerItem` call in vanilla either.
    OpenTrade {
        /// The villager's current profession — never `None`/`Nitwit`, which
        /// have no trades and never reach this arm (see
        /// [`MobSim::interact`]'s villager short-circuit).
        profession: villager::Profession,
        /// `VillagerData.level`, `1..=5` — how many levels of trades to
        /// accumulate (see `villager::trades::offers_up_to`).
        level: i32,
    },
}

impl InteractOutcome {
    /// Whether the interaction consumed one of the held item.
    ///
    /// `SitToggled`/`OpenTrade` are the exceptions, and `SitToggled`'s is
    /// vanilla's: `InteractionResult.SUCCESS.withoutItem()`. A pet you sit
    /// down does not eat whatever you happened to be holding, and opening a
    /// trade screen is not a `usePlayerItem` call either.
    #[must_use]
    pub fn consumes_item(self) -> bool {
        !matches!(
            self,
            Self::Pass | Self::SitToggled { .. } | Self::OpenTrade { .. }
        )
    }

    /// The particle type vanilla's matching `broadcastEntityEvent` would make the
    /// client spawn, or `None` for an outcome with no visual.
    ///
    /// `6` → `SMOKE`, `7` → `HEART` (`TamableAnimal.spawnTamingParticles`), `18`
    /// → `HEART` (`Animal.setInLove`, seven hearts, same burst).
    #[must_use]
    fn particle(self) -> Option<&'static str> {
        match self {
            Self::Tamed | Self::InLove => Some("minecraft:heart"),
            Self::TameFailed => Some("minecraft:smoke"),
            Self::Pass
            | Self::SitToggled { .. }
            | Self::Fed
            | Self::TemperRaised { .. }
            | Self::OpenTrade { .. } => None,
        }
    }
}

/// One `spawnTamingParticles`-shaped burst, as a `LEVEL_PARTICLES` packet.
///
/// Vanilla spawns seven particles client-side at `getRandomX(1.0)`,
/// `getRandomY() + 0.5`, `getRandomZ(1.0)` with a `nextGaussian() * 0.02`
/// per-axis velocity. `ClientboundLevelParticlesPacket` carries the count and a
/// per-axis spread, so the same burst is expressed as one packet: seven
/// particles, spread half a block horizontally (vanilla's `getRandomX(1.0)` is
/// ±width/2 about the centre, and 1.0 is a rough stand-in for the mob's width),
/// centred half a block above the mob's feet.
fn taming_particles(particle: &str, pos: Vec3) -> crate::effects::WorldEffect {
    crate::effects::WorldEffect::Particles {
        particle: particle.to_owned(),
        pos: Vec3::new(pos.x, pos.y + 0.5, pos.z),
        offset: lodestone_model::Vec3f::new(0.5, 0.5, 0.5),
        // Vanilla's per-particle velocity is `nextGaussian() * 0.02`, so the
        // burst barely drifts. `max_speed` is the packet's own scale for that.
        max_speed: 0.02,
        count: 7,
        long_distance: false,
    }
}

/// Who owns a tamed mob.
///
/// Two variants because the two are genuinely different relations rather than one
/// with a wider key. A player owner is a **uuid** — vanilla's
/// `TamableAnimal.DATA_OWNERUUID_ID`, which is a uuid on the wire and in NBT
/// alike, and the only identity that survives a reconnect. A mob owner is a
/// runtime **entity id**, because nothing persists it and there is no uuid to
/// resolve; that flavour predates taming and exists for the ownership questions
/// the neutral roster asks (`HurtByTargetGoal.alertOthers`' same-owner filter).
///
/// Collapsing them into one `i32` is what made ownership unable to name a player,
/// and collapsing them into one `Uuid` would require inventing uuids for mobs
/// that the wire would then never carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobOwner {
    /// Another live [`SimMob`], by runtime entity id.
    Mob(i32),
    /// A player, by account uuid.
    Player(Uuid),
}

/// What a lead is tied to — vanilla `Leashable.LeashData.leashHolder`, which
/// is any `Entity` (a player, another leashable mob, or a
/// `LeashFenceKnotEntity`). This sim has no non-living decoration-entity
/// concept ([`SimMob`] assumes health, an `AttributeMap` and a goal
/// selector, none of which a knot has), so a fence anchor is a bare
/// [`BlockPos`] rather than a spawned entity — see [`MobSim::try_leash_to_fence`]'s
/// own doc comment for what that costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeashHolder {
    /// A player, by account uuid — resolved to a live position through
    /// [`MobSim::players`]' `identity`.
    Player(Uuid),
    /// Another live [`SimMob`], by runtime entity id.
    Mob(i32),
    /// A fence post — vanilla's `LeashFenceKnotEntity`'s world position,
    /// without the entity itself.
    Fence(BlockPos),
}

/// The result of [`MobSim::try_leash`] — mirrors vanilla `Entity.interact`'s
/// two leash-specific branches (`InteractionResult::SUCCESS`/`.withoutItem()`/
/// `PASS`) closely enough that a caller can derive its own packet response
/// from it without re-deriving the branching itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeashOutcome {
    /// The mob is now leashed to the given holder. The caller must consume
    /// one `minecraft:lead` from the placer's hand (vanilla `itemStack.shrink(1)`).
    Attached,
    /// The mob was leashed to the interacting player and is now free. `true`
    /// means a `minecraft:lead` item was spawned at the mob's position
    /// (vanilla `dropLeash`); `false` means none was (vanilla `removeLeash`,
    /// the creative-mode/infinite-materials arm — the caller supplies which
    /// via `try_leash`'s own `creative` parameter, this sim having no
    /// game-mode state of its own).
    Detached { dropped_lead: bool },
    /// Neither arm applied — not leashable, out of range, or the holder
    /// requested is not a fresh attach for an already-player-held mob
    /// (vanilla's `!(leashable.getLeashHolder() instanceof Player)` guard).
    Refused,
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

/// `LivingEntity.PLAYER_HURT_EXPERIENCE_TIME` — how long after a player's hit a mob's
/// death still counts as a player kill, in ticks.
const PLAYER_HURT_EXPERIENCE_TIME: u64 = 100;

/// `Mob.getAmbientSoundInterval`'s default (`80`) — the forced gap
/// [`roll_ambient_sound`] enforces after an idle vocalisation fires (and
/// after a hurt sound, via [`MobSim::note_vocalisation`]) before the
/// per-tick chance of firing again starts climbing from zero.
const AMBIENT_SOUND_INTERVAL: i32 = 80;

/// The flat knockback power `LivingEntity.dealDefaultKnockback` applies to
/// **every** damaging hit, regardless of the attacker's own
/// `minecraft:attack_knockback` attribute — `LivingEntity.knockback(0.4F, xd,
/// zd, source, damage)`. This is separate from,
/// and applied before, any attacker-specific bonus (sprint attack,
/// enchantments) — see [`MobSim::attack`]'s own doc comment for why the two
/// are chained as two `knockback_impulse` calls rather than summed into one.
const MELEE_DEFAULT_KNOCKBACK_POWER: f64 = 0.4;

/// Wraps a bare species path back into a [`ResourceKey`] so
/// [`mob_experience_reward`] can consult [`is_hostile_species`], which takes one.
///
/// A parse rather than a second copy of that function's species list, for the reason
/// that list's own doc gives: a duplicated hostility table is one more thing to go
/// stale. An unparseable path answers "not hostile", which lands on the documented
/// `0` fallback.
fn hostile_probe(path: &str) -> ResourceKey {
    format!("minecraft:{path}")
        .parse()
        .unwrap_or_else(|_| item_entity_type())
}

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

/// Whether `species` propagates a grudge to nearby same-species mobs when it
/// is newly hurt, and if so the alert box's half-extents and whether the
/// alerted mob's owner must match the victim's.
///
/// `ZombifiedPiglin.alertOthers` (box `(FOLLOW_RANGE, 10.0, FOLLOW_RANGE)` =
/// **±35 XZ, ±10 Y**, no owner filter — piglins are not `TamableAnimal`) and
/// `HurtByTargetGoal.alertOthers` on the wolf's own `.setAlertOthers()`
/// registration (box `(FOLLOW_RANGE, 10.0, FOLLOW_RANGE)` = **±16 XZ, ±10
/// Y**, plus `tamable.getOwner() == other.getOwner()` since `Wolf` is a
/// `TamableAnimal` — see `docs/plans/mob-ai-roster.md` §4's `FOLLOW_RANGE`
/// citations and `roster::neutral`'s module doc for both derivations).
///
/// Line of sight is not modelled (disclosed the same way the enderman stare's
/// carved-pumpkin disguise is: the perception seam has no ray query), and
/// the piglin's own `ALERT_INTERVAL` throttle is not reproduced as a separate
/// timer — [`MobSim::attack`]'s "only on a *new* grudge" gate already
/// prevents an already-angry piglin from re-alerting every subsequent hit,
/// which is a stricter (never more frequent than vanilla) approximation of
/// the same effect.
fn alert_species(species_path: &str) -> Option<(f64, f64, bool)> {
    match species_path {
        "zombified_piglin" => Some((35.0, 10.0, false)),
        "wolf" => Some((16.0, 10.0, true)),
        _ => None,
    }
}

/// One draw of `Bee.customServerAiStep`'s self-destruct roll, evaluated once
/// every 5th tick since the sting connected (`elapsed % 5 == 0`, checked by
/// the caller): `nextInt(clamp(1200 - timeSinceSting, 1, 1200)) == 0`.
///
/// The clamp is what bounds it at both ends — at `elapsed == 1200` the
/// divisor is `1` and the roll is unconditional, and `1200` is itself a
/// multiple of `5` so that tick is never skipped; **a stung bee is certainly
/// alive one tick after the sting** (elapsed `1`, not a multiple of 5, so the
/// caller never calls this at all) **and certainly dead by 1200 ticks after
/// it**. See `roster::neutral::BEE`'s own doc comment for the citation this
/// was derived from.
///
/// Deterministic and independent of the mob's own [`MobController`] RNG
/// stream, for the same reason [`roll_ambient_sound`] avoids it: this can
/// fire on a cadence unrelated to that stream's own consumers, and drawing
/// from the shared stream here would shift every subsequent AI draw. Not a
/// faithful `java.util.Random` reproduction — a multiplicative hash of
/// `(tick_count, id, elapsed)`, the same style already used for the ambient
/// sound pitch/timing rolls in this module.
fn bee_sting_death_roll(tick_count: u64, id: i32, elapsed: u64) -> bool {
    if elapsed >= 1200 {
        return true;
    }
    let denom = (1200 - elapsed).max(1);
    let mix = tick_count
        .wrapping_mul(2_654_435_761)
        .wrapping_add(id as u64)
        .wrapping_mul(40_503)
        .wrapping_add(elapsed.wrapping_mul(2_246_822_519));
    mix % denom == 0
}

/// One tick of vanilla `Mob.baseTick`'s idle-vocalisation roll:
/// `if (isAlive() && random.nextInt(1000) < ambientSoundTime++) {
/// resetAmbientSoundTime(); playAmbientSound(); }`. `ambientSoundTime` starts
/// at `0` and climbs by one every tick that does not fire, so the per-tick
/// chance of firing ramps from `0` toward certainty rather than being a flat
/// roll; firing resets it to `-`[`AMBIENT_SOUND_INTERVAL`], enforcing a hard
/// cooldown before the ramp restarts (mirrored on a hurt sound too, in
/// [`MobSim::note_vocalisation`]).
///
/// **Must not draw from the mob's own [`MobController`] RNG stream.** That
/// stream is also what every AI goal draws from (wander targets, look
/// timers, …), and this roll runs unconditionally every tick for every live
/// mob — consuming from it here would shift every subsequent AI draw by
/// however many calls this makes, exactly the failure mode
/// [`MobSim::note_vocalisation`]'s own doc already flags for the same
/// reason: "consuming from a shared generator here would shift every other
/// draw." Mixed from `tick_count` and the mob's id instead, the same
/// approach that function already uses.
fn roll_ambient_sound(m: &mut SimMob<'_>, tick_count: u64) -> Option<crate::effects::WorldEffect> {
    let id = m.id as u64;
    // A small multiplicative hash of (tick_count, id) — not vanilla's RNG,
    // deterministic and cheap, and critically independent of the mob's own
    // `next_i32`/`next_f32` stream. `% 1000` matches `random.nextInt(1000)`'s
    // range.
    let mix = tick_count
        .wrapping_mul(2_654_435_761)
        .wrapping_add(id)
        .wrapping_mul(40_503);
    let roll = (mix % 1000) as i32;
    let fired = roll < m.ambient_sound_time;
    m.ambient_sound_time += 1;
    if !fired {
        return None;
    }
    m.ambient_sound_time = -AMBIENT_SOUND_INTERVAL;
    // Vanilla `LivingEntity.getVoicePitch`: `(rand - rand) * 0.2 + centre`,
    // centre `1.5` for a baby and `1.0` for an adult — unlike the hurt/death
    // pitch, which vanilla draws from a *different*, baby-blind formula (see
    // `MobSim::note_vocalisation`'s own doc for why that one differs). Same
    // tick_count/id phase approach `note_vocalisation` already uses for its
    // own pitch, so both land in `[centre - 0.1, centre + 0.1]`.
    let phase = (tick_count.wrapping_mul(31).wrapping_add(id)) % 21;
    let centre = if m.is_baby() { 1.5 } else { 1.0 };
    let pitch = centre - 0.1 + phase as f32 * 0.01;
    crate::effects::mob_ambient_sound(
        m.entity_type.to_string().as_str(),
        m.position(),
        m.category == MobCategory::Monster,
        pitch,
        tick_count as i64,
    )
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
    /// The `minecraft:max_health` attribute value resolved at spawn — the ceiling
    /// [`heal`](SimMob::heal) clamps to.
    ///
    /// Recorded rather than re-resolved because it is what decides the *order* of
    /// a tame pet's interaction arms: `Wolf.mobInteract` feeds a hurt pet
    /// (`isFood(stack) && getHealth() < getMaxHealth()`) and only falls through to
    /// the breeding and sit arms when it cannot. Without the ceiling that
    /// condition is unanswerable and the arms silently reorder.
    max_health: f32,
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
    /// Issue #233: `Bee.hasStung`, expressed as the [`MobSim::tick_count`] the
    /// sting connected rather than a bare flag, since the self-destruct roll
    /// (`Bee.customServerAiStep`) needs `timeSinceSting` and a decrementing
    /// counter would drift against a stepped tick loop for the same reason
    /// [`Anger::end_time`] is an absolute deadline rather than a countdown.
    ///
    /// `None` for a bee that has never stung. Set the instant a bee's own
    /// attack connects ([`MobSim::tick`]'s first mob loop) and never cleared
    /// — a stung bee is always on a path to death, matching vanilla's
    /// `hasStung` having no reset.
    stung_at: Option<u64>,
    /// The [`MobSim::tick_count`] at which this mob stops counting as
    /// player-killed — vanilla's `LivingEntity.lastHurtByPlayerMemoryTime`,
    /// expressed as an absolute deadline for [`Anger`]'s reason.
    ///
    /// **This is the gate on XP dropping at all.** `dropExperience` requires
    /// `lastHurtByPlayerMemoryTime > 0`, so a mob that starves, drowns, burns, falls
    /// or is killed by another mob drops **no** experience — only a kill a player had
    /// a hand in within [`PLAYER_HURT_EXPERIENCE_TIME`] ticks does. Awarding
    /// unconditionally would turn any mob farm into an XP farm and is the plausible
    /// simplification to avoid.
    ///
    /// `None` for a mob no player has ever hit.
    hurt_by_player_until: Option<u64>,
    /// Raw melee damage this mob's own attacks deal (`ATTACK_DAMAGE`
    /// attribute), applied to whatever [`attack_target_id`](SimMob::attack_target_id)
    /// names when a `MeleeAttackGoal` connects.
    attack_damage: f32,
    /// The invulnerability-frame gate for hits landing on *this* mob
    /// (`damage::HurtCooldown`), ticked once per sim tick regardless of
    /// whether anything hit this tick.
    hurt_cooldown: HurtCooldown,
    /// Vanilla `Mob.ambientSoundTime`: an increasing-probability countdown for
    /// this mob's idle vocalisation (cow moo, zombie groan, …), ticked once
    /// per sim tick regardless of whether a goal moved this mob. Starts at
    /// `0`, matching Java's field default — see
    /// [`MobSim::roll_ambient_sound`] for the roll this drives.
    ambient_sound_time: i32,
    /// The id of another live [`SimMob`] this mob's melee attacks should
    /// damage, set alongside [`set_attack_target`](SimMob::set_attack_target)'s
    /// `Vec3` (which only drives movement — the goal/navigation seam has no
    /// entity identity, just positions).
    attack_target_id: Option<i32>,
    /// Who owns this mob, if anyone — the ownership relation. Vanilla stores a
    /// tamed animal's owner as a **player** uuid
    /// (`TamableAnimal.DATA_OWNERUUID_ID`), and that is now expressible:
    /// [`PerceivedPlayer`] carries a [`PlayerIdentity`] at the perception seam. The
    /// mob-to-mob flavour is kept because the enderman/wolf-pack work needs it
    /// and nothing about a uuid replaces it.
    ///
    /// The seam carries the resolved *position*
    /// ([`MobController::owner_position`]); the identity lives here, because
    /// only a census can hold it.
    ///
    /// `None` for a wild mob.
    owner: Option<MobOwner>,
    /// Whether this mob is *tame at all*, independent of whether its owner is
    /// currently resolvable — vanilla `TamableAnimal.isTame()`, the `0x04` bit
    /// of `DATA_FLAGS_ID`.
    ///
    /// Not derived from [`owner`](Self::owner) being `Some`, and this is the
    /// distinction that matters: a tamed pet whose owner has logged out keeps
    /// its `owner` (the uuid is durable) but has **no resolvable position**, and
    /// a mob-owned pet has an owner that is not a player at all. Both are tame.
    /// Deriving tameness from a *resolved* owner would un-tame every pet the
    /// moment its owner left the player list, and goals read this.
    tame: bool,
    /// Vanilla `TamableAnimal.orderedToSit` — the sitting **intent** an owner's
    /// right-click toggles, which is what `SitWhenOrderedToGoal` reads. NBT
    /// round-trips it as `Sitting`.
    ///
    /// Kept here rather than only on the [`NavigatingMob`] because it is
    /// persisted state that outlives any goal, and because the interaction that
    /// toggles it is a host event, not a goal.
    ordered_to_sit: bool,
    /// `AbstractHorse.Temper` — how close a horse family member is to accepting
    /// a rider, `0..=getMaxTemper()`. Raised by feeding
    /// ([`horse_temper_gain`]); read by the tame roll
    /// ([`MobSim::attempt_horse_tame`]).
    ///
    /// `0` for every species outside the horse family, where nothing reads it.
    temper: i32,
    /// `minecraft:knockback_resistance` attribute value (`0.0..=1.0`),
    /// `lodestone_physics::knockback::knockback_impulse`'s own
    /// `knockback_resistance` parameter for a hit landing on *this* mob. See
    /// [`combat_defaults`]'s doc comment for why this is not folded into
    /// [`Defenses`].
    knockback_resistance: f64,
    /// What a lead currently ties this mob to, if anything — vanilla
    /// `Leashable.LeashData`. `None` is vanilla's `getLeashData() == null`;
    /// there is no separate "has data but no holder" state modelled, since
    /// nothing in this sim needs the delayed-load half `restoreLeashFromSave`
    /// exists for (persistence is a different crate's concern).
    leash_holder: Option<LeashHolder>,
    /// `MushroomCow.lastLightningBoltUUID` — which bolt (by this sim's own
    /// lightning-bolt entity id, not a real UUID; see `mobs/lightning.rs`'s
    /// module doc for why) last toggled this mob's variant, so a bolt whose
    /// `hit_entities` fires across several ticks cannot flip the same mob
    /// twice. `None` means never struck. Read by no species today except the
    /// mooshroom guard itself — see that module's doc for why nothing yet
    /// consumes the toggle this guards.
    last_lightning_bolt: Option<i32>,
    /// This villager's profession (issue #243) — `VillagerProfession.NONE`
    /// for every non-villager species, and for a villager that has not
    /// claimed a workstation yet. Only meaningful when
    /// [`entity_type`](Self::entity_type) is `minecraft:villager`.
    profession: villager::Profession,
    /// The workstation block position [`profession`](Self::profession) was
    /// claimed from, if any — `None` for an unemployed villager or a
    /// non-villager. Cleared alongside `profession` reverting to `None` when
    /// [`MobSim::tick_villager_professions`] finds the claim gone.
    workstation: Option<BlockPos>,
    /// `VillagerData.level`, `1..=5`. `1` for every non-villager and every
    /// freshly spawned villager (`VillagerData`'s own field default).
    villager_level: i32,
    /// Accumulated trading xp toward [`villager::max_xp_for_level`]'s next
    /// threshold. `Villager.villagerXp`.
    villager_xp: i32,
    /// Ticks until this mob's next job search, decremented in
    /// [`MobSim::tick_villager_professions`]. Throttles the bounded terrain
    /// scan [`villager::find_and_claim_workstation`] runs — see that
    /// function's own doc for why the scan itself is not free.
    job_search_cooldown: i32,
    /// This mob's live status effects — `LivingEntity.activeEffects`. Populated
    /// by a splash/lingering potion's impact
    /// ([`MobSim::resolve_projectile_impacts`] via
    /// `crate::mobs::projectiles::resolve_potion_splash`); nothing yet ticks it
    /// periodically for a mob the way `crate::server`'s vitals tick does for a
    /// player, so a poison/wither/regeneration effect landed here does not yet
    /// deal its own periodic damage or heal — see [`crate::mob_effects`]'s
    /// module doc for the splash side of this gap.
    effects: crate::mob_effects::ActiveEffects,
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
    ///
    /// **Also re-derives the hitbox and movement step when this crosses the
    /// baby/adult boundary** — vanilla `AgeableMob.setAge` unconditionally
    /// calls `this.refreshDimensions()`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/AgeableMob.java:189`),
    /// and until this only [`spawn_species`](Self) ever computed
    /// [`species_shape`]: a mob bred into babyhood, or one growing up, kept
    /// its spawn-time adult box and adult step speed forever. Gated on the
    /// boundary rather than run on every call, since a baby's per-tick
    /// countdown from [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to `0` would otherwise re-resolve both twenty-four thousand times for
    /// no observable change.
    pub fn set_age(&mut self, age: i32) -> &mut Self {
        let was_baby = self.mob.is_baby();
        self.mob.set_age(age);
        let is_baby = self.mob.is_baby();
        if is_baby != was_baby {
            let attrs = default_attributes(&self.entity_type).unwrap_or_else(AttributeMap::new);
            let shape = species_shape(&self.entity_type, &attrs, is_baby);
            self.mob.set_shape(shape);
            let base_speed = attr(&attrs, "movement_speed");
            let multiplier = if is_baby {
                baby_speed_multiplier(&self.entity_type)
            } else {
                1.0
            };
            self.mob.set_step_per_tick(ai_ground_speed(base_speed * multiplier));
        }
        self
    }

    /// This mob's current per-tick movement step — reflects
    /// [`baby_speed_multiplier`] once [`set_age`](Self::set_age) has crossed
    /// the baby/adult boundary.
    #[must_use]
    pub fn step_per_tick(&self) -> f64 {
        self.mob.step_per_tick()
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

    /// Who owns this mob, if anyone. `None` for a wild (untamed) mob.
    #[must_use]
    pub fn owner(&self) -> Option<MobOwner> {
        self.owner
    }

    /// The **mob** id of this mob's owner, if it is owned by a mob. `None` both
    /// for a wild mob and for one owned by a *player* — read
    /// [`owner`](Self::owner) when the difference matters.
    #[must_use]
    pub fn owner_id(&self) -> Option<i32> {
        match self.owner {
            Some(MobOwner::Mob(id)) => Some(id),
            _ => None,
        }
    }

    /// The uuid of this mob's owner, if it is owned by a player.
    #[must_use]
    pub fn owner_uuid(&self) -> Option<Uuid> {
        match self.owner {
            Some(MobOwner::Player(uuid)) => Some(uuid),
            _ => None,
        }
    }

    /// Sets this mob's owner id (the mob-to-mob flavour of
    /// [`set_owner`](Self::set_owner)).
    pub fn set_owner_id(&mut self, owner_id: Option<i32>) -> &mut Self {
        self.set_owner(owner_id.map(MobOwner::Mob))
    }

    /// Sets this mob's owner — vanilla `TamableAnimal.setOwner`.
    ///
    /// **Does not set the tame flag**, and the asymmetry is vanilla's:
    /// `setOwnerReference` sets `DATA_OWNERUUID_ID` and *then* calls
    /// `setTame(true, false)`, two separate pieces of state
    /// (`TamableAnimal.setOwnerReference`). [`tame`](Self::tame) is the call that
    /// does both, and is what a taming interaction should use.
    pub fn set_owner(&mut self, owner: Option<MobOwner>) -> &mut Self {
        self.owner = owner;
        self
    }

    /// What a lead currently ties this mob to, if anything.
    #[must_use]
    pub fn leash_holder(&self) -> Option<LeashHolder> {
        self.leash_holder
    }

    /// Whether a lead is currently attached — vanilla `Leashable.isLeashed()`,
    /// which additionally requires `leashHolder != null`; this sim has no
    /// "has leash data but no resolved holder" state (see the field's own
    /// doc comment), so `Some` and "leashed" coincide exactly.
    #[must_use]
    pub fn is_leashed(&self) -> bool {
        self.leash_holder.is_some()
    }

    /// Directly sets the leash holder, bypassing [`MobSim::try_leash`]'s
    /// distance/species gating — for a host that has already decided (e.g.
    /// restoring a save, or [`MobSim::try_leash_to_fence`]'s re-parent of an
    /// already-leashed mob onto a fresh knot).
    pub fn set_leash_holder(&mut self, holder: Option<LeashHolder>) -> &mut Self {
        self.leash_holder = holder;
        self
    }

    /// Whether this mob is tame — vanilla `TamableAnimal.isTame()`.
    ///
    /// Distinct from [`owner_position`](Self::owner_position) being `Some`: a
    /// tamed pet whose owner is offline is still tame.
    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.tame
    }

    /// Tames this mob to `owner` — vanilla `TamableAnimal.tame(player)`, which is
    /// `setTame(true, true)` plus `setOwner(player)`.
    pub fn tame(&mut self, owner: MobOwner) -> &mut Self {
        self.owner = Some(owner);
        self.tame = true;
        self.mob.set_tame(true);
        self
    }

    /// Whether the owner has told this mob to sit — vanilla
    /// `TamableAnimal.isOrderedToSit()`, the persisted intent.
    #[must_use]
    pub fn is_ordered_to_sit(&self) -> bool {
        self.ordered_to_sit
    }

    /// Sets the sitting order — vanilla `TamableAnimal.setOrderedToSit`.
    ///
    /// Pushes straight through to the [`NavigatingMob`] as well as recording it
    /// here, so `SitWhenOrderedToGoal` sees the order on the *same* tick rather
    /// than one tick late. Every other perception input is refreshed by
    /// [`MobSim::feed_perception`], but an order given by an interaction arrives
    /// between ticks and a one-tick lag is visible as a pet that ignores the
    /// first click.
    pub fn set_ordered_to_sit(&mut self, ordered_to_sit: bool) -> &mut Self {
        self.ordered_to_sit = ordered_to_sit;
        self.mob.set_ordered_to_sit(ordered_to_sit);
        self
    }

    /// Whether this mob is currently in the sitting **pose** —
    /// `SitWhenOrderedToGoal`'s observable output, which is what the `0x01`
    /// `DATA_FLAGS_ID` bit carries. Read this to answer "did the goal run",
    /// and [`is_ordered_to_sit`](Self::is_ordered_to_sit) to answer "was it
    /// told to".
    #[must_use]
    pub fn is_in_sitting_pose(&self) -> bool {
        self.mob.is_in_sitting_pose()
    }

    /// Whether this mob is part of an active pillager patrol — vanilla
    /// `PatrollingMonster.isPatrolling()`. Kept only on the [`NavigatingMob`]
    /// (unlike [`tame`](Self::tame)/[`owner`](Self::owner)): nothing outside the
    /// AI seam and [`MobSim`]'s own patrol census reads it, so there is no
    /// second host-side record to keep in sync.
    #[must_use]
    pub fn is_patrolling(&self) -> bool {
        self.mob.is_patrolling()
    }

    /// Whether this mob leads its patrol — vanilla
    /// `PatrollingMonster.isPatrolLeader()`.
    #[must_use]
    pub fn is_patrol_leader(&self) -> bool {
        self.mob.is_patrol_leader()
    }

    /// This mob's own current long-distance patrol waypoint — vanilla
    /// `PatrollingMonster.getPatrolTarget()`.
    #[must_use]
    pub fn patrol_target(&self) -> Option<Vec3> {
        self.mob.patrol_target()
    }

    /// Marks this mob as patrolling (or not) — vanilla
    /// `PatrollingMonster.setPatrolling`.
    pub fn set_patrolling(&mut self, patrolling: bool) -> &mut Self {
        self.mob.set_patrolling(patrolling);
        self
    }

    /// Marks this mob as its patrol's leader (or not) — vanilla
    /// `PatrollingMonster.setPatrolLeader`. Does not also set
    /// [`patrolling`](Self::set_patrolling); see [`NavigatingMob::set_patrol_leader`]'s
    /// own doc comment for why the two are separate calls here.
    pub fn set_patrol_leader(&mut self, leader: bool) -> &mut Self {
        self.mob.set_patrol_leader(leader);
        self
    }

    /// Sets this mob's own long-distance patrol waypoint — vanilla
    /// `PatrollingMonster.setPatrolTarget`/`findPatrolTarget`.
    pub fn set_patrol_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_target(target);
        self
    }

    /// Feeds a non-leader the patrol group's shared waypoint, as
    /// [`MobSim`]'s own per-tick census resolves it. See
    /// [`MobController::patrol_group_target`]'s own doc comment for why this
    /// exists.
    pub fn set_patrol_group_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_group_target(target);
        self
    }

    /// `AbstractHorse.getTemper()` — how close this horse is to accepting a
    /// rider. Always `0` outside the horse family.
    #[must_use]
    pub fn temper(&self) -> i32 {
        self.temper
    }

    /// `AbstractHorse.setTemper`, clamped to `0..=max` by the caller. Exists so a
    /// gate can stage a horse at a chosen temper instead of feeding it 34 times.
    pub fn set_temper(&mut self, temper: i32) -> &mut Self {
        self.temper = temper;
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

    /// The `minecraft:max_health` attribute resolved at spawn.
    #[must_use]
    pub fn max_health(&self) -> f32 {
        self.max_health
    }

    /// `LivingEntity.heal(amount)`: raises health toward
    /// [`max_health`](Self::max_health), never past it.
    pub fn heal(&mut self, amount: f32) -> &mut Self {
        self.health = (self.health + amount).min(self.max_health);
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

    /// This mob's live status effects — see [`Self::apply_effect`] to add one.
    #[must_use]
    pub fn effects(&self) -> &crate::mob_effects::ActiveEffects {
        &self.effects
    }

    /// Applies one status effect through vanilla's own stacking rule
    /// (`LivingEntity.addEffect` → [`crate::mob_effects::EffectInstance::update`]
    /// — see that type's own doc for the "remembered, not ignored or replaced"
    /// table). Returns whether the active instance changed, matching
    /// [`crate::mob_effects::ActiveEffects::apply`]'s own return.
    pub fn apply_effect(&mut self, effect_id: &str, duration: i32, amplifier: u32) -> bool {
        self.effects.apply(effect_id, duration, amplifier)
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

    /// This villager's profession — `villager::Profession::None` for every
    /// non-villager and every villager that has not claimed a workstation.
    #[must_use]
    pub fn profession(&self) -> villager::Profession {
        self.profession
    }

    /// The workstation this villager claimed, if any.
    #[must_use]
    pub fn workstation(&self) -> Option<BlockPos> {
        self.workstation
    }

    /// `VillagerData.level`, `1..=5`.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_level
    }

    /// Accumulated trading xp toward the next level.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.villager_xp
    }

    /// Assigns (or clears, with `villager::Profession::None`) this mob's
    /// profession and workstation together — the two always change in
    /// lockstep (see [`MobSim::tick_villager_professions`]'s doc for why a
    /// claim and a profession are never set independently).
    pub(crate) fn set_profession(
        &mut self,
        profession: villager::Profession,
        workstation: Option<BlockPos>,
    ) {
        self.profession = profession;
        self.workstation = workstation;
    }

    /// Lowers the mob into a version-free [`EntitySnapshot`] for the encode seam.
    /// This is the whole identity/motion surface a [`ServerProtocol`] needs to
    /// build spawn/move/remove packets; the server holds the previous snapshot
    /// per connection so the protocol can stay stateless.
    ///
    /// `metadata` is the per-species entity-metadata field list —
    /// general across mobs (see [`MetadataField`]'s own doc comment), not a
    /// creeper-only mechanism, even though a creeper was the only producer
    /// for a long time. [`crate::server::EntityStreamer::sync`] diffs this exactly like
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
    ///
    /// `MetadataField::Baby` is the same shape as `CreeperSwellDir`, not as
    /// `CreeperIgnited`: a mob **grows up**, so absence cannot safely mean
    /// "still a baby" the way it can mean "still not ignited". It is pushed
    /// unconditionally for every species eligible for it (see the species
    /// switch below), carrying the current `is_baby()` value whether that is
    /// `true` or `false`, so the adult transition reaches the client the same
    /// way the arrival as a baby did.
    #[must_use]
    pub fn snapshot(&self) -> EntitySnapshot {
        let mut metadata = Vec::new();
        if self.entity_type.path() == "creeper" {
            metadata.push(MetadataField::CreeperSwellDir(self.swell_dir()));
            if self.is_ignited() {
                metadata.push(MetadataField::CreeperIgnited(true));
            }
        }
        // Index 18's byte, **whose layout depends on the class** — see
        // `MetadataField::TamableFlags`. The species switch has to be here, in the
        // producer, because nothing downstream can recover it: four different `BYTE`
        // fields share index 18 (`TamableAnimal.DATA_FLAGS_ID`,
        // `AbstractHorse.DATA_ID_FLAGS`, `Sheep.DATA_WOOL_ID`,
        // `Shulker.DATA_COLOR_ID`) and no `entity_census` column separates them, so
        // an encoder handed a single shared "tamed" variant would have to guess.
        //
        // Emitted only for a tame mob: a wild one's byte is all-zero, which is the
        // client's own default, and `EntityStreamer::sync` skips an empty metadata
        // list entirely — so a wild mob costs no extra packet.
        //
        // Species with no arm here stream nothing, which is the honest state rather
        // than a gap to fill speculatively: a tame llama or fox needs its own flag
        // layout read off the dump first.
        if self.tame {
            match self.entity_type.path() {
                "wolf" | "cat" | "parrot" | "ocelot" => {
                    metadata.push(MetadataField::TamableFlags {
                        tame: true,
                        sitting: self.is_in_sitting_pose(),
                    });
                }
                "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => {
                    metadata.push(MetadataField::HorseFlags { tame: true });
                }
                _ => {}
            }
        }
        // Index 16's boolean, shared by `AgeableMob.DATA_BABY_ID`,
        // `Zombie.DATA_BABY_ID` and `Zoglin.DATA_BABY_ID`
        // (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
        // — but also by `Creeper.DATA_SWELL_DIR`, an `INT`, which is why the
        // species switch has to live here rather than in a shared "is baby"
        // encoder: a `MetadataField::Baby` emitted for a creeper would write
        // a boolean where the swell direction belongs. Scoped to exactly the
        // species this sim tracks age for — [`baby_dimensions`] and
        // [`baby_speed_multiplier`]'s own species lists — which are also the
        // only species mechanically confirmed (via `.cache/mc/26.2/src/`) to
        // descend from `AgeableMob` or `Zombie`, the two classes whose own
        // `DATA_BABY_ID` resolves to this index.
        //
        // Pushed unconditionally rather than only while `is_baby()` is true:
        // see this method's own doc comment for why the grown-up transition
        // needs the same treatment as the arrival.
        match self.entity_type.path() {
            "cow" | "mooshroom" | "sheep" | "pig" | "chicken" | "rabbit" | "wolf" | "zombie"
            | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => {
                metadata.push(MetadataField::Baby(self.is_baby()));
            }
            _ => {}
        }
        // `Villager.DATA_VILLAGER_DATA`, index 19 (issue #243) — the field a
        // client's `VillagerRenderer`/`VillagerProfessionLayer` actually reads
        // to pick a texture. Pushed unconditionally for every villager, at
        // whatever `profession`/`villager_level` currently are (including
        // `None`/`1`), for the same reason `Baby` above is pushed
        // unconditionally: a profession transition needs to reach the client
        // the same way the initial value did, not only while it is
        // "interesting". `kind` is always `minecraft:plains` — see
        // `crate::mobs::villager`'s module doc for why biome-derived type is
        // out of scope.
        if self.entity_type.path() == "villager" {
            metadata.push(MetadataField::VillagerData {
                kind: ResourceKey::from_str("minecraft:plains").expect("static key is valid"),
                profession: ResourceKey::from_str(&format!("minecraft:{}", self.profession.path()))
                    .expect("every Profession::path() is a valid identifier path"),
                level: self.villager_level,
            });
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
            // No mob overrides `getAddEntityPacket`'s data argument.
            object_data: 0,
            // Resolved by `MobSim::snapshots`, not here: `leash_holder` names a
            // player by uuid, and only `MobSim` (through `self.players`) can turn
            // that into the wire entity id `EntitySnapshot::leash_link` carries.
            // `SimMob` alone has no player list to resolve against.
            leash_link: None,
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
    /// The entity id that launched it, if known.
    ///
    /// Load-bearing for the impact pass, not bookkeeping: a projectile is spawned
    /// at its shooter's eye, *inside* the shooter's own bounding box, so without
    /// this a skeleton's first arrow strikes the skeleton. Vanilla's own guard is
    /// two-part — `Projectile.canHitEntity` refuses the owner until
    /// `checkLeftOwner` has seen the projectile clear it, and
    /// `ProjectileUtil.computeMargin` keeps the hitbox at zero inflation for the
    /// first two ticks — and this is the first half.
    owner: Option<i32>,
    /// The thrown stack's raw `minecraft:potion` network id
    /// (`lodestone_model::item::ItemComponents::potion`), for a splash or
    /// lingering potion only — `None` for every other throwable, and `None` for
    /// a potion whose stack carried no resolved `minecraft:potion_contents`
    /// (a bare/uncomponented stack). [`MobSim::resolve_projectile_impacts`]
    /// reads this to decide what [`crate::mob_effects::potion_splash_effects`]
    /// applies on impact.
    potion: Option<i32>,
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

/// One live `ExperienceOrb`.
///
/// # `value` and `count` are different numbers and both are player-visible
///
/// `value` is `DATA_VALUE`: the points **one** absorption pays out, and the only
/// field on the wire. `count` is `ExperienceOrb.count`, how many orbs this single
/// entity stands for — `merge` adds the absorbed orb's count and `playerTouch`
/// decrements it, discarding the entity at zero. So a merged orb is one entity, one
/// texture frame, and several separate absorptions of `value` points each.
///
/// Reading `count` as "the points this orb is worth" is the plausible wrong model: it
/// makes a merged pile pay out `value` once instead of `count` times, so a big drop
/// silently loses most of its XP while every orb still looks right on screen.
///
/// # Why `ItemMotion` carries the position and *not* the tick
///
/// [`ItemMotion`] is used purely as the position/velocity/`on_ground` triple, because
/// [`settle_entity`] already resolves that triple against real block shapes.
/// [`ItemMotion::tick`] is **not** called for an orb: an item's gravity is 0.04 and
/// its landing bounce is `velocity.y *= -0.5`, while `ExperienceOrb.getDefaultGravity`
/// is `0.03` and its bounce is `-fallSpeed * 0.4` off the *pre-move* fall speed. See
/// [`MobSim::tick_orbs`], which transcribes `ExperienceOrb.tick` in its own order.
#[derive(Debug, Clone)]
struct OrbState {
    uuid: Uuid,
    /// `ExperienceOrb.DATA_VALUE` — points per absorption.
    value: i32,
    /// `ExperienceOrb.count` — absorptions remaining before the entity is discarded.
    count: i32,
    /// `ExperienceOrb.age`, in ticks. Discarded at [`ORB_LIFETIME`], and reset to `0`
    /// by a merge so a pile does not expire on its oldest member's clock.
    age: i32,
    motion: ItemMotion,
}

/// Wire identity plus motion for one live `FallingBlockEntity` — the
/// falling-block analogue of [`ItemState`].
///
/// The `state` string is the block the entity is *imitating*
/// (`FallingBlockEntity.blockState`) and is what goes back into the world on
/// landing. It also resolves the `ADD_ENTITY` object-data field —
/// `FallingBlockEntity.getAddEntityPacket` passes
/// `Block.getId(this.getBlockState())` — which is the **only** channel a client
/// learns what a falling block looks like: `defineSynchedData` registers
/// `DATA_START_POS` and nothing else, so the state is never in an entity-metadata
/// packet. A falling block streamed with object data `0` draws as whatever state
/// id `0` happens to be, silently, exactly as an item entity with no reported
/// stack drew nothing.
#[derive(Debug, Clone)]
struct TrackedFallingBlock {
    uuid: Uuid,
    /// The imitated block state, e.g. `minecraft:sand`.
    state: String,
    motion: crate::gravity_tick::FallingBlockMotion,
    /// Where the fall ends, resolved once by
    /// `crate::gravity_tick::find_landing_y` against the live world at spawn
    /// time. See [`FallingBlockMotion::step`](crate::gravity_tick::FallingBlockMotion::step)
    /// for why this is captured rather than re-read each tick.
    landing_y: i32,
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
    /// Live `ExperienceOrb`s, keyed by network entity id.
    ///
    /// A plain map for [`falling_blocks`](Self::falling_blocks)' reason rather than a
    /// registry in `lodestone-entity`: an orb's lifecycle is an age counter and a
    /// merge rule, and **the merge rule is keyed on the network entity id**
    /// (`(orb.getId() - id) % 40 == 0`), which a version-free registry that does not
    /// own ids structurally cannot express.
    orbs: HashMap<i32, OrbState>,
    /// The `nextInt(40)` draw `ExperienceOrb.tryMergeToExisting` makes per spawned
    /// denomination, on its own stream so awarding XP cannot shift which roll a mob
    /// spawn or a block drop sees.
    orb_rng: SpawnRng,
    /// Live `FallingBlockEntity`s, keyed by network entity id — the falling
    /// sand/gravel a `crate::gravity_tick::TICK_GRAVITY` scheduled tick created.
    ///
    /// A plain map here rather than a registry in `lodestone-entity` beside
    /// [`ItemEntityRegistry`]/[`ProjectileRegistry`], because a falling block has
    /// no lifecycle to model separately from its motion: it exists for the
    /// duration of one fall, carries no age or merge rules, and its only
    /// version-free part ([`FallingBlockMotion`]) already lives in
    /// `crate::gravity_tick` next to the `FallingBlock` port that creates it.
    falling_blocks: HashMap<i32, TrackedFallingBlock>,
    next_id: i32,
    tick_count: u64,
    /// Cells the last tick's item-settling pass asked [`ItemCollision`] for —
    /// see [`items_settled_probe_count`](Self::items_settled_probe_count).
    item_probe_count: u64,
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
    /// Players struck by a hostile mob's melee attack this tick, awaiting the
    /// driver's `PlayerVitals::apply_damage` call (issue #625) — the same
    /// handoff shape as [`pending_detonations`](Self::pending_detonations)
    /// above and for the same reason: this sim owns no connection and cannot
    /// reach a player's authoritative health itself. Drained by
    /// [`take_player_hits`](Self::take_player_hits). See [`PlayerHit`]'s own
    /// doc comment for how a target position resolves to a player identity.
    pending_player_hits: Vec<PlayerHit>,
    /// Hurt and death sounds awaiting the driver (issue #530), the same handoff
    /// shape as the two above and for the same reason: this sim owns no
    /// connection. Drained by [`take_vocalisations`](Self::take_vocalisations).
    ///
    /// Before this, `apply_damage` damaged and killed mobs with **no audible
    /// result at all** — the `ServerProtocol` trait had no sound encoder, so a
    /// player could beat a cow to death in silence.
    pending_vocalisations: Vec<crate::effects::WorldEffect>,
    /// Idle ambient vocalisations awaiting the driver — the same handoff shape
    /// as [`pending_vocalisations`](Self::pending_vocalisations) and for the
    /// same reason, but rolled every tick per mob
    /// ([`roll_ambient_sound`]) rather than recorded at a damage funnel.
    /// Drained by [`take_ambient_sounds`](Self::take_ambient_sounds).
    ///
    /// Before this, `MobSim` had no periodic ambient-sound producer at all —
    /// hurt and death were the only mob sounds a client could ever hear, so
    /// ordinary exploration (no combat) was silent but for footsteps.
    pending_ambient_sounds: Vec<crate::effects::WorldEffect>,
    /// Per-entity animation cues awaiting the driver — the *visible* half of the
    /// same hits [`pending_vocalisations`](Self::pending_vocalisations) makes
    /// audible, and recorded at the same funnels for the same reason (this sim
    /// owns no connection). Drained by
    /// [`take_entity_animations`](Self::take_entity_animations).
    ///
    /// Two packets, not one, because vanilla uses two: the hurt flash is
    /// `ClientboundHurtAnimationPacket` and the fall-over is
    /// `ClientboundEntityEventPacket` byte 3
    /// (`LivingEntity.die`'s `broadcastEntityEvent`). Before this a mob could be
    /// beaten to death and simply *vanish* — no flash, no tip-over — because
    /// `ServerProtocol` had no encoder for either.
    pending_animations: Vec<MobAnimation>,
    /// Every connected player's perception-relevant state, refreshed by a
    /// driver through [`set_players`](Self::set_players) and consumed by
    /// [`tick`](Self::tick) to feed each mob's `nearest_player`/`temptation`.
    ///
    /// This crate had **no player-position feed at all** before issue #441 —
    /// see [`set_players`](Self::set_players) for why that made two of the
    /// eight perception methods unreachable, and which one line closes it.
    players: Vec<PerceivedPlayer>,
    /// The `nextInt(3)` / `nextInt(10)` / `nextInt(maxTemper)` draws the taming
    /// mechanisms make, on their own stream so a tame attempt cannot shift which
    /// roll a mob spawn, a despawn pass or an XP award sees — the same isolation
    /// [`orb_rng`](Self::orb_rng) exists for.
    ///
    /// Injectable through [`set_tame_rng`](Self::set_tame_rng), which is how a
    /// gate drives a tame roll to both sides of its threshold instead of
    /// asserting that taming "sometimes" happens.
    tame_rng: SpawnRng,
    /// The `random.nextInt(7) + 1` draw
    /// `Animal.finalizeSpawnChildFromBreeding` makes for the experience orb a
    /// successful mating pops, on its own stream for [`tame_rng`](Self::tame_rng)'s
    /// reason: a breeding event must not shift which roll a tame attempt sees.
    breed_rng: SpawnRng,
    /// The `mob_drops` game rule, mirrored in by
    /// [`set_mob_drops`](Self::set_mob_drops). `true` by default, which is vanilla's
    /// own default and the behaviour before the rule was readable.
    mob_drops: bool,
    /// Live rideable **vehicles** — every `AbstractBoat` a player has placed,
    /// keyed by network entity id.
    ///
    /// A registry of its own rather than a [`SimMob`], and that is the whole
    /// design: a boat has no attributes, no goals and no AI, so
    /// [`spawn_species`](Self::spawn_species) would give it a mob's component set
    /// and produce a boat that *wanders*. It also has to stop being
    /// server-driven the instant a player sits in it —
    /// `Entity.isClientAuthoritative()` delegates to the controlling passenger
    /// and `Player.isClientAuthoritative()` is `true` — which is a property no
    /// mob has.
    ///
    /// A plain map for the reason [`falling_blocks`](Self::falling_blocks) is
    /// one: there is no version-free lifecycle to model beyond the motion, and
    /// the motion is [`lodestone_physics::vehicle`]'s, shared with the client so
    /// a boat we *watch* and a boat we *ride* cannot disagree about a slab.
    vehicles: HashMap<i32, TrackedVehicle>,
    /// Live `PrimedTnt`, keyed by network entity id — see [`TrackedTnt`] for
    /// why this is a plain map beside [`vehicles`](Self::vehicles) rather than
    /// a [`SimMob`].
    tnt: HashMap<i32, TrackedTnt>,
    /// Live minecarts — every `AbstractMinecart` subclass, keyed by network
    /// entity id. See [`TrackedMinecart`] for the shape and `mobs::minecart`'s
    /// own module doc for the physics.
    minecarts: HashMap<i32, TrackedMinecart>,
    /// The `random.nextDouble()` draw a fresh `PrimedTnt`'s launch direction
    /// makes (`PrimedTnt`'s three-argument constructor), on its own stream for
    /// [`orb_rng`](Self::orb_rng)'s reason: priming TNT must not shift which
    /// roll a mob spawn, a block drop or anything else sees.
    tnt_rng: SpawnRng,
    /// Vanilla `PatrolSpawner.nextTick` — ticks remaining before the next
    /// patrol-spawn attempt, decremented once per
    /// [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle) call
    /// regardless of whether it does anything, exactly as vanilla's
    /// `CustomSpawner.tick` decrements its own countdown every world tick.
    patrol_next_tick: i32,
    /// The `random.nextInt(…)` draws [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)
    /// makes, on its own stream for the same isolation reason
    /// [`tame_rng`](Self::tame_rng) is separate from every other roll: a
    /// patrol-spawn attempt must not shift which roll a mob spawn, a despawn
    /// pass or a tame attempt sees.
    patrol_rng: SpawnRng,
    /// Vanilla `WanderingTraderSpawner.tickDelay` — ticks remaining before
    /// the next 1200-tick poll, decremented once per
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// call regardless of outcome, exactly as `patrol_next_tick` is.
    trader_tick_delay: i32,
    /// Vanilla's saved-data `WanderingTraderData.spawnDelay` — the
    /// 24000-tick delay nested inside the 1200-tick poll. This crate has no
    /// save/load for it (see the doc comment on
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)),
    /// so it resets with every fresh `MobSim` rather than surviving a
    /// restart.
    trader_spawn_delay: i32,
    /// Vanilla's saved-data `WanderingTraderData.spawnChance` — climbs 25→75
    /// by 25 each time the outer roll is attempted and misses, and resets to
    /// 25 on an actual spawn.
    trader_spawn_chance: i32,
    /// The `random.nextInt(…)` draws
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// makes, on its own stream for the same isolation reason
    /// [`patrol_rng`](Self::patrol_rng) is separate from every other roll.
    trader_rng: SpawnRng,
    /// Live `LightningBolt` sidecars (issue #269), keyed by network entity id —
    /// the same shape [`orbs`]'s [`OrbState`] map establishes: no
    /// [`NavigatingMob`]/[`GoalSelector`] body, because a bolt has no box and
    /// no AI. See `mobs/lightning.rs`'s module doc.
    lightning_bolts: HashMap<i32, lightning::LiveBolt>,
    /// Fire-ignition attempts a live bolt's [`lightning::tick_bolt`] made this
    /// tick, awaiting the driver's world mutation — the same handoff shape as
    /// [`pending_grazes`](Self::pending_grazes) and for the identical reason:
    /// `world: &'w ChunkWorld` is an immutable pathfinding snapshot, not the
    /// live `ChunkStore`, so this sim cannot place the fire itself. Drained by
    /// [`take_lightning_fires`](Self::take_lightning_fires).
    pending_lightning_fires: Vec<BlockPos>,
    /// Every projectile-vs-block impact this tick's
    /// [`resolve_projectile_impacts`](Self::resolve_projectile_impacts) found,
    /// awaiting the driver — see [`ProjectileBlockHit`]'s own doc for why this
    /// sim cannot resolve a target block's power write itself. Drained by
    /// [`take_projectile_block_hits`](Self::take_projectile_block_hits).
    pending_projectile_block_hits: Vec<ProjectileBlockHit>,
    /// The live workstation claim ledger [`tick_villager_professions`](Self::tick_villager_professions)
    /// reads and writes (issue #243). See [`villager::WorkstationClaims`]'s
    /// own doc for why this reuses `crate::poi_storage::PoiRecord` rather
    /// than a parallel claim table, and for what is deliberately not built
    /// (no on-disk persistence, no block-event hook).
    ///
    /// Native-only, same as [`villager::WorkstationClaims`] itself — see
    /// that type's own doc for why (it reuses `crate::poi_storage`, which is
    /// gated the same way, and this crate compiles for `wasm32-unknown-unknown`).
    #[cfg(not(target_arch = "wasm32"))]
    workstation_claims: villager::WorkstationClaims,
    /// Live ender dragons, keyed by network entity id — see [`TrackedDragon`]
    /// and `mobs::dragon`'s own module doc for the phase/heal state each one
    /// drives and exactly what is a real port vs. a simplification.
    dragons: HashMap<i32, TrackedDragon>,
    /// Live end crystals, keyed by network entity id — see [`TrackedCrystal`]
    /// and `mobs::end_crystal`'s own module doc.
    crystals: HashMap<i32, TrackedCrystal>,
    /// The dragon's own phase-transition/crystal-rescan RNG rolls
    /// (`random.nextInt(crystals+3)`, `.nextInt(10)`, ...), on its own stream
    /// for [`tnt_rng`](Self::tnt_rng)'s reason: a dragon tick must not shift
    /// which roll a mob spawn, a block drop, or anything else sees.
    dragon_rng: SpawnRng,
    /// Live fishing bobbers (issue #257), keyed by network entity id — see
    /// [`fishing::FishingBobber`] and `mobs::fishing`'s own module doc.
    fishing_bobbers: HashMap<i32, fishing::FishingBobber>,
    /// The bobber cast/bob/bite/loot-roll RNG stream, on its own stream for
    /// [`dragon_rng`](Self::dragon_rng)'s reason.
    fishing_rng: SpawnRng,
    /// Live raids (issue #241), keyed by this sim's own raid id (not a
    /// network entity id — a raid has no entity of its own; see
    /// [`raid::Raid`] and `mobs::raid`'s own module doc).
    raids: HashMap<i32, raid::Raid>,
    /// The next id [`raid::MobSim::start_raid`] assigns — a separate counter
    /// from [`next_id`](Self::next_id) because a raid id is never a network
    /// entity id and must never collide with one being reused after a raid
    /// despawns its raiders.
    next_raid_id: i32,
    /// The wave-spawn-position/spawn-count RNG stream, on its own stream for
    /// [`dragon_rng`](Self::dragon_rng)'s reason.
    raid_rng: SpawnRng,
    /// Live withers, keyed by network entity id — see [`TrackedWither`] and
    /// `mobs::wither`'s own module doc for the emergence/heal/skull-fire
    /// state each one drives.
    withers: HashMap<i32, TrackedWither>,
    /// The wither's own dangerous-skull roll, on its own stream for the same
    /// reason [`dragon_rng`](Self::dragon_rng) is.
    wither_rng: SpawnRng,
}

/// One live `AbstractBoat` — wire identity, motion, and who is aboard.
///
/// # Why the rider is here and not on the connection
///
/// `MobSim::tick` is the only thing that advances a boat, and it must **not**
/// advance a ridden one: the rider's client owns that boat's position and reports
/// it through `MoveVehicle`. So the "is anyone aboard" bit has to be readable from
/// inside the tick, which means it lives on the vehicle. A per-connection flag
/// would leave the tick fighting the client, which is the specific failure mode
/// (*"a boat that fights the player"*) this shape exists to prevent.
#[derive(Debug, Clone)]
struct TrackedVehicle {
    uuid: Uuid,
    /// The entity type, e.g. `minecraft:oak_boat`. Carried rather than derived:
    /// the twenty boat types differ only in their texture, and the client resolves
    /// the model from this key alone.
    entity_type: ResourceKey,
    motion: lodestone_physics::EntityMotion,
    /// The hull's yaw in degrees — `setYRot`, written by the placing player and
    /// then by `controlBoat` on whichever side is authoritative.
    yaw: f32,
    /// `AbstractBoat`'s between-tick state, so the server's float pass and the
    /// client's are literally the same code over the same fields.
    boat: lodestone_physics::vehicle::BoatState,
    /// The **player entity id** of the controlling passenger, or `None` for an
    /// empty boat. `Some` suspends the server-side tick entirely.
    rider: Option<i32>,
}

/// One live `PrimedTnt` — wire identity, motion and the fuse countdown.
///
/// A plain map for [`falling_blocks`](Self::falling_blocks)'s reason: no
/// lifecycle beyond the motion and a counter, so a `SimMob`'s species/goal
/// machinery would be pure overhead for an entity with no AI and no box that
/// matters (it is not selector-visible and nothing paths around it).
///
/// The block state it imitates (`PrimedTnt.DATA_BLOCK_STATE_ID`) is **not**
/// carried here: this crate's only producers (`TntBlock::prime`'s several call
/// sites) always construct vanilla's own `DEFAULT_BLOCK_STATE`
/// (`Blocks.TNT.defaultBlockState()`) — nothing here ever calls
/// `PrimedTnt.setBlockState` with anything else — so a per-entity field would
/// carry one value forever. See `mobs::tnt`'s module doc for the rest of what
/// is deliberately simplified.
#[derive(Debug, Clone)]
struct TrackedTnt {
    uuid: Uuid,
    motion: lodestone_physics::EntityMotion,
    /// `PrimedTnt.DATA_FUSE_ID` — ticks remaining before detonation, counting
    /// down from [`tnt::DEFAULT_FUSE_TIME`]. Detonates the tick this reaches
    /// `0`, matching `if (fuse <= 0)` in `PrimedTnt.tick`.
    fuse: i32,
}

/// One live ender dragon — wire identity, position/yaw, health, the
/// [`crate::dragon::phase::PhaseManager`] driving its phase, and the
/// [`crate::dragon::crystal::NearestCrystal`] tracker its heal reads. See
/// `mobs::dragon`'s own module doc for the per-tick behaviour and exactly
/// which parts are a real vanilla port vs. a named simplification (flight is
/// a simplified orbit, not vanilla's node-graph pathfinding).
#[derive(Debug, Clone)]
struct TrackedDragon {
    uuid: Uuid,
    position: Vec3,
    /// Body yaw, in degrees — driven by the simplified orbit
    /// (`mobs::dragon::tick_one_dragon`), not a real look-at-target
    /// computation.
    yaw: f32,
    health: f32,
    max_health: f32,
    phase: crate::dragon::phase::PhaseManager,
    nearest_crystal: crate::dragon::crystal::NearestCrystal,
    /// `EnderDragon.getFightOrigin()` — the arena centre this dragon orbits
    /// and measures egg/portal distances from.
    fight_origin: Vec3,
    /// The simplified orbit's current angle, in radians — this module's own
    /// state, not a vanilla field (see `mobs::dragon`'s module doc).
    orbit_angle: f64,
}

/// One live wither — wire identity, position, health, the invulnerable
/// "emerging" countdown and skull-fire cooldown. See `mobs::wither`'s own
/// module doc for the per-tick behaviour and exactly which parts are a real
/// vanilla port vs. a named simplification (no movement, one firing
/// schedule standing in for vanilla's three independent heads).
#[derive(Debug, Clone)]
struct TrackedWither {
    uuid: Uuid,
    position: Vec3,
    yaw: f32,
    health: f32,
    max_health: f32,
    /// `WitherBoss.DATA_ID_INV` — `crate::wither::INVULNERABLE_TICKS`
    /// counting down to `0`; `0` means the wither is in its active phase.
    invulnerable_ticks: i32,
    /// `Entity.tickCount` — this wither's own age, read by
    /// `crate::wither::should_heal_while_invulnerable`/`_active`.
    age: i64,
    /// Ticks until the next skull may fire — see `mobs::wither`'s module doc
    /// for why this is one schedule rather than vanilla's three per-head
    /// timers.
    next_skull_tick: i32,
}

/// One live end crystal — wire identity and a fixed position. See
/// `mobs::end_crystal`'s own module doc for why this tracks nothing else
/// (no pillar to stand on, no cage, no beam-target metadata yet).
#[derive(Debug, Clone, Copy)]
struct TrackedCrystal {
    uuid: Uuid,
    position: Vec3,
}

/// One live `AbstractMinecart` — wire identity, kind, rail-following motion,
/// riding, and the per-kind extras (a container's slots, a furnace's fuel and
/// push, a TNT cart's fuse). See `mobs::minecart`'s own module doc for the
/// physics this drives and everything deliberately simplified.
///
/// A plain map for the same reason [`TrackedVehicle`]/[`TrackedTnt`] are:
/// no `SimMob` goal machinery, because a minecart has no AI beyond
/// rail-following.
#[derive(Debug, Clone)]
struct TrackedMinecart {
    uuid: Uuid,
    kind: minecart::MinecartKind,
    motion: lodestone_physics::EntityMotion,
    /// `AbstractMinecart`'s own `yRot`, computed from the direction of travel
    /// each tick `OldMinecartBehavior.tick` moves it — never set by a
    /// placer, unlike a boat's.
    yaw: f32,
    /// `Entity.yRotO` — the previous tick's yaw, read by the flip-detection
    /// comparison alone.
    yaw_o: f32,
    /// `AbstractMinecart.flipped` — `FlippedRotation`: when the travel
    /// direction reverses near a dead stop, the sprite's *heading* flips
    /// 180° instead of visibly spinning through it.
    flipped: bool,
    /// The **player entity id** riding this cart, or `None`. Only
    /// [`minecart::MinecartKind::is_rideable`] kinds are ever `Some`.
    rider: Option<i32>,
    /// A container kind's own inventory (`MinecartKind::container_size`
    /// slots; empty for every non-container kind). See `mobs::minecart`'s
    /// own module doc for why nothing yet opens a menu against this.
    slots: Vec<Option<lodestone_model::ItemStack>>,
    /// `MinecartFurnace.fuel` — ticks of burn time remaining.
    fuel: i32,
    /// `MinecartFurnace.push` — the constant self-propulsion vector while
    /// fuelled (`y` always `0.0`).
    push: lodestone_physics::Vec3d,
    /// `MinecartTNT.fuse` — `-1` unprimed, counts down to `0` (detonate).
    fuse: i32,
}

/// One per-entity animation cue a hit produced, for
/// [`take_entity_animations`](MobSim::take_entity_animations) to hand a driver.
///
/// Two variants because vanilla sends two different packets, and the split is
/// not cosmetic: the hurt flash is `ClientboundHurtAnimationPacket` (a VarInt id
/// and a `float`) while the death tip-over is `ClientboundEntityEventPacket` (a
/// fixed-width `int` id and a status byte). A driver cannot collapse them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MobAnimation {
    /// The mob flashed red — `ClientboundHurtAnimationPacket`.
    ///
    /// No yaw is carried because vanilla's is a constant for anything that is not
    /// a player: `LivingEntity.getHurtDir` returns `0.0F` and only
    /// `ServerPlayer` overrides it, so a mob's hurt animation is always the pure
    /// roll. Adding a field here would invite a producer to invent one.
    Hurt {
        /// The mob's entity id.
        entity_id: i32,
    },
    /// The mob died — `ClientboundEntityEventPacket` with
    /// [`crate::protocol::entity_event::DEATH`], which is what starts the
    /// client's `deathTime` counter and tips the body onto its side.
    Died {
        /// The mob's entity id.
        entity_id: i32,
    },
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

/// One player struck by a hostile mob's melee attack this tick, for
/// [`take_player_hits`](MobSim::take_player_hits) to hand a driver.
///
/// `SimMob::attack_target_id` names only "another live `SimMob`" by its own
/// doc comment, so it structurally cannot carry a player: the goal seam
/// (`NearestAttackableTargetGoal`/`MeleeAttackGoal`) targets and attacks a
/// bare `Vec3`, never an identity. This is resolved by matching that target
/// position against `self.players`' [`feed_perception`]-fed positions in the
/// same tick's [`tick`](MobSim::tick) — safe because nothing mutates a
/// player's fed position between the feed at the top of the tick and the
/// goal ticks that consume it. A grudge-target attack (the anger-gated
/// `NearestAttackableTargetGoal` row) can miss this match: its target is a
/// position remembered from whenever the grudge was set, not refreshed to
/// the player's current position, so a moved player will not match. That is
/// a disclosed gap, not a silent one — ordinary hostile-melee (zombie,
/// skeleton, …) always targets the live `nearest_player` feed and matches
/// every time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerHit {
    /// Who was hit.
    pub identity: PlayerIdentity,
    /// Raw `ATTACK_DAMAGE`, unreduced — the driver runs it through the real
    /// armour/i-frame pipeline via `PlayerVitals::apply_damage`, the same
    /// split [`SimMob::apply_damage`] draws for a mob victim.
    pub raw_damage: f32,
    /// The attacking mob's position, for the driver's hurt-direction/knockback
    /// calculation (`crate::vitals::HurtDirection::from_source`).
    pub attacker_pos: Vec3,
}

/// One projectile-vs-block impact [`MobSim::resolve_projectile_impacts`] found
/// (issue #322), for [`take_projectile_block_hits`](MobSim::take_projectile_block_hits)
/// to hand a driver — the same handoff shape as
/// [`pending_grazes`](MobSim::pending_grazes)/[`pending_lightning_fires`](MobSim::pending_lightning_fires)
/// and for the identical reason: `MobSim::world` is an immutable pathfinding
/// snapshot, not the live `ChunkStore`, so this sim can neither read the real
/// current block state (to check it is actually still a `minecraft:target`)
/// nor write a new one, and has no `ScheduledTickQueue` to consult for
/// `redstone_target::apply_hit`'s `has_pending_decay` guard. This is deliberately
/// data about *every* block a projectile stopped against, not just a target —
/// the driver is what already knows which block is there and dispatches
/// accordingly, the same division `pending_lightning_fires` draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectileBlockHit {
    /// The struck cell.
    pub pos: BlockPos,
    /// Which face axis the hit entered through —
    /// `crate::redstone_target::redstone_strength`'s own `hit_axis` parameter.
    pub axis: crate::redstone_target::HitAxis,
    /// The hit point's fractional position within the cell, each in `[0.0,
    /// 1.0]` — `Mth.frac(hitLocation.{x,y,z})`.
    pub frac: Vec3,
    /// Whether the projectile was an arrow (`redstone_target::activation_duration`'s
    /// 20-vs-8-tick split) — `AbstractArrow`, not `SpectralArrow`/`Trident`
    /// carrying their own subclass distinctions this sim does not model; see
    /// [`resolve_projectile_impacts`](MobSim::resolve_projectile_impacts) for
    /// exactly which registry paths set this.
    pub is_arrow: bool,
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
            orbs: HashMap::new(),
            orb_rng: SpawnRng::new(orbs::ORB_BEHAVIOR_SEED),
            falling_blocks: HashMap::new(),
            next_id: 1,
            tick_count: 0,
            item_probe_count: 0,
            pending_detonations: Vec::new(),
            pending_grazes: Vec::new(),
            pending_player_hits: Vec::new(),
            pending_vocalisations: Vec::new(),
            pending_ambient_sounds: Vec::new(),
            pending_animations: Vec::new(),
            players: Vec::new(),
            tame_rng: SpawnRng::new(TAME_ROLL_SEED),
            breed_rng: SpawnRng::new(BREED_XP_SEED),
            mob_drops: true,
            vehicles: HashMap::new(),
            tnt: HashMap::new(),
            minecarts: HashMap::new(),
            tnt_rng: SpawnRng::new(tnt::TNT_LAUNCH_SEED),
            // Vanilla's own field default (`private int nextTick;`, never
            // explicitly initialised, so Java's `0`) — the very first call
            // sees `nextTick <= 0` and may attempt a patrol on tick one,
            // subject to every other gate still applying.
            patrol_next_tick: 0,
            patrol_rng: SpawnRng::new(PATROL_SPAWN_SEED),
            // Vanilla's own field default (`private int tickDelay = 1200;`)
            // — the constructor sets it explicitly, unlike `nextTick`, so
            // the first call does not roll before tick 1200.
            trader_tick_delay: WANDERING_TRADER_TICK_DELAY,
            trader_spawn_delay: WANDERING_TRADER_SPAWN_DELAY,
            trader_spawn_chance: WANDERING_TRADER_MIN_SPAWN_CHANCE,
            trader_rng: SpawnRng::new(WANDERING_TRADER_SPAWN_SEED),
            lightning_bolts: HashMap::new(),
            pending_lightning_fires: Vec::new(),
            pending_projectile_block_hits: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            workstation_claims: villager::WorkstationClaims::new(),
            dragons: HashMap::new(),
            withers: HashMap::new(),
            wither_rng: SpawnRng::new(wither::WITHER_SKULL_SEED),
            crystals: HashMap::new(),
            dragon_rng: SpawnRng::new(dragon::DRAGON_PHASE_SEED),
            fishing_bobbers: HashMap::new(),
            fishing_rng: SpawnRng::new(fishing::FISHING_ROLL_SEED),
            raids: HashMap::new(),
            next_raid_id: 1,
            raid_rng: SpawnRng::new(raid::RAID_ROLL_SEED),
        }
    }

    /// Replaces the RNG the taming mechanisms draw from — the injection point a
    /// tame-chance gate needs.
    ///
    /// A tame *chance* cannot be gated by observing that taming sometimes
    /// happens; that measures only that the code runs. Seed this with a stream
    /// whose first draw is known and the outcome becomes a prediction. The draw
    /// order and count are part of the specification, so a gate that reseeds
    /// between attempts is also asserting how many draws each mechanism makes.
    pub fn set_tame_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.tame_rng = rng;
        self
    }

    /// Replaces the RNG [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)
    /// draws from — the injection point a patrol-spawn gate needs, for the same
    /// reason [`set_tame_rng`](Self::set_tame_rng) exists.
    pub fn set_patrol_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.patrol_rng = rng;
        self
    }

    /// Replaces the RNG
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// draws from — the injection point a trader-spawn gate needs, for the
    /// same reason [`set_patrol_rng`](Self::set_patrol_rng) exists.
    pub fn set_trader_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.trader_rng = rng;
        self
    }

    /// Overwrites [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)'s
    /// two nested countdowns directly — the injection point a gate needs to
    /// stage a sim past the 1200-tick poll and the 24000-tick delay without
    /// calling the cycle that many times. `0` for either drives the *next*
    /// call straight to the roll, matching vanilla's own `<= 0` checks.
    pub fn set_trader_timers(&mut self, tick_delay: i32, spawn_delay: i32) -> &mut Self {
        self.trader_tick_delay = tick_delay;
        self.trader_spawn_delay = spawn_delay;
        self
    }

    /// Overwrites [`tick_count`](Self::tick_count) directly — the injection
    /// point a gate needs to stage the sim past
    /// [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)'s timeline
    /// gate without actually ticking 120,000 times. Mirrors
    /// [`SimMob::set_temper`]'s reason: staging state a real playthrough
    /// would only reach by repetition.
    pub fn set_tick_count(&mut self, tick_count: u64) -> &mut Self {
        self.tick_count = tick_count;
        self
    }

    /// How many cells the **last** tick's item-settling pass asked the collision
    /// oracle about.
    ///
    /// This is the cost of routing items through swept collision, in the one unit
    /// that survives being read on a machine with four other builds running. It
    /// scales with item count and with how fast each item is moving (a faster item
    /// sweeps a longer box), so it is also the number that says whether a floor
    /// covered in drops can eat a tick — the question a per-item measurement
    /// structurally cannot answer.
    #[must_use]
    pub fn items_settled_probe_count(&self) -> u64 {
        self.item_probe_count
    }

    /// This villager's accumulated trading xp — `crate::server`'s own
    /// consumer for the `MERCHANT_OFFERS` packet's `villager_xp` field,
    /// alongside the `profession`/`level` an [`InteractOutcome::OpenTrade`]
    /// already carries. `0` for a non-villager or an unknown id — a
    /// harmless default rather than a panic, the same convention every
    /// other by-id accessor in this file uses.
    #[must_use]
    pub fn villager_xp(&self, mob_id: i32) -> i32 {
        self.mobs
            .iter()
            .find(|m| m.id == mob_id)
            .map_or(0, |m| m.villager_xp)
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
    ///
    /// # Why the parameter is generic
    ///
    /// It accepts anything that converts into a [`PerceivedPlayer`], which in
    /// practice means a `Vec<PerceivedPlayer>` **or** a bare
    /// `Vec<PlayerPerception>`. Both shapes are wanted at once and neither is
    /// transitional sugar for the other: taming needs the identity, so the real
    /// producer supplies views, while every gate that only cares where a mob
    /// looks is clearer without a uuid it does not use. A `PlayerPerception`
    /// converts to a view with **no identity**, which is the honest state for a
    /// producer that has none — see [`PerceivedPlayer`].
    pub fn set_players<I, P>(&mut self, players: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PerceivedPlayer>,
    {
        self.players = players.into_iter().map(Into::into).collect();
        self
    }

    /// The players mob perception currently sees, with their identities.
    #[must_use]
    pub fn players(&self) -> &[PerceivedPlayer] {
        &self.players
    }

    /// The position of the player with this identity's uuid, if they are in the
    /// current player list — the resolution vanilla's
    /// `EntityReference.get(…, ServerLevel, …)` performs for
    /// `TamableAnimal.getOwner()`.
    ///
    /// Keyed on the **uuid**, never on the entity id, for the reason
    /// [`PlayerIdentity`] gives: the entity id is reassigned per session, so a
    /// pet whose owner reconnects would resolve to whoever inherited that id.
    #[must_use]
    fn player_position(&self, uuid: Uuid) -> Option<Vec3> {
        self.players
            .iter()
            .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
            .map(|v| v.perception.position)
    }

    /// Resolves a [`LeashHolder`] to the wire entity id
    /// [`ServerProtocol::encode_set_entity_link`] needs as its target — vanilla's
    /// own `ClientboundSetEntityLinkPacket(entity, holder)` takes a live `Entity`,
    /// and this is "which id" for each of the three holder shapes this sim
    /// tracks. Only `MobSim` can answer it — a bare `LeashHolder::Player` carries
    /// a uuid, not a session-scoped entity id, and resolving that needs
    /// `self.players` — which is why it is not a method on [`SimMob`] itself.
    ///
    /// [`LeashHolder::Player`] resolves through the same uuid-keyed lookup
    /// [`player_position`](Self::player_position) uses, for the identical reason
    /// given there: entity ids are reassigned per session, so keying on the uuid
    /// is what keeps a reconnecting owner's leash pointed at the right client.
    ///
    /// [`LeashHolder::Mob`] is already a wire id (`SimMob::id`), so this returns
    /// it verbatim — no lookup needed.
    ///
    /// [`LeashHolder::Fence`] returns `None`: this sim never spawns a
    /// `LeashFenceKnotEntity` (see that variant's own doc comment for why), so
    /// there is no entity id on the wire to link to yet. A mob leashed to a fence
    /// is tracked correctly server-side and draws no rope until a knot entity
    /// exists — a disclosed gap, not a silent one.
    #[must_use]
    fn resolve_leash_target(&self, holder: LeashHolder) -> Option<i32> {
        match holder {
            LeashHolder::Player(uuid) => self
                .players
                .iter()
                .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
                .map(|v| v.identity.expect("just matched").entity_id),
            LeashHolder::Mob(id) => Some(id),
            LeashHolder::Fence(_) => None,
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

    /// The id the next [`spawn`](Self::spawn) call will assign.
    ///
    /// The read side of [`set_next_id`](Self::set_next_id), and it answers one
    /// question nothing else can: **has this sim been reseeded yet?**
    /// [`MobHandle::reseed`] replaces the whole sim and then calls
    /// `set_next_id(1000)`, while [`MobSim::new`] starts at `1` — so a caller that
    /// must not touch a sim about to be thrown away (a saved-entity restore, a
    /// `/summon` racing world open) can tell the difference. Without it, that
    /// caller has to guess, and guessing wrong is silent: the work lands in the
    /// sim that is discarded a moment later.
    #[must_use]
    pub fn next_id(&self) -> i32 {
        self.next_id
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
            max_health,
            defenses,
            anger: None,
            stung_at: None,
            hurt_by_player_until: None,
            attack_damage,
            hurt_cooldown: HurtCooldown::default(),
            ambient_sound_time: 0,
            attack_target_id: None,
            owner: None,
            tame: false,
            ordered_to_sit: false,
            temper: 0,
            knockback_resistance,
            leash_holder: None,
            last_lightning_bolt: None,
            profession: villager::Profession::None,
            workstation: None,
            villager_level: 1,
            villager_xp: 0,
            job_search_cooldown: 0,
            effects: crate::mob_effects::ActiveEffects::new(),
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
    /// * **Speed**: the type's `movement_speed` attribute value feeds
    ///   [`SpeciesContext`](lodestone_entity::ai::roster::SpeciesContext) as-is
    ///   (every roster goal multiplies it by vanilla's own `speedModifier`
    ///   constants, exactly as vanilla's own move-control does before it ever
    ///   reaches motion), but the actual kinematic-follower rate handed to
    ///   [`spawn_with_type`] is [`ai_ground_speed`] of that attribute — see its
    ///   own doc for why a bare attribute value is not the mob's real
    ///   blocks/tick and `docs/mob-species-spawning.md` for the vanilla
    ///   methods and live-oracle measurement behind the conversion.
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
        // Always spawns adult-shaped; a caller wanting a baby applies
        // `set_age(BABY_START_AGE)` afterward, which re-derives the shape
        // through the same function (see `SimMob::set_age`'s own doc).
        let shape = species_shape(&entity_type, &attrs, false);
        let base_speed = attr(&attrs, "movement_speed");
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
        let hostile = species::is_hostile_species(&entity_type);

        // Built *before* `entity_type` is moved into the spawn, so the species
        // path is borrowed rather than cloned. `SpeciesContext` wants the raw
        // attribute — every roster goal supplies its own `speedModifier`
        // multiplier on top, matching vanilla's own move-control order — so it
        // is *not* `ai_ground_speed`-converted here; the conversion happens
        // once, below, for the kinematic follower's own rate.
        let goals = roster::goals_for(entity_type.path(), &SpeciesContext::new(base_speed));

        let mob = self.spawn_with_type(
            pos,
            shape,
            ai_ground_speed(base_speed),
            visited_budget,
            entity_type,
        );
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

    /// Given a just-placed carved pumpkin or jack o'lantern at `pumpkin_pos`,
    /// checks whether it completes a valid snow- or iron-golem block pattern
    /// and, if so, spawns the golem — vanilla
    /// `CarvedPumpkinBlock.trySpawnGolem`
    /// (`.cache/mc/26.2/src/net/minecraft/world/level/block/CarvedPumpkinBlock.java`).
    ///
    /// Tries the snow golem pattern first and returns on a match, exactly as
    /// vanilla's early `return` does — a pumpkin that happens to complete
    /// both (impossible for these two shapes, but the order is part of the
    /// port) only ever produces the snow golem.
    ///
    /// **A pure detection query, not a world mutation.** `MobSim` holds only
    /// a read-only [`PathWorld`] and has no block-*write* authority, so
    /// `block_at` is the caller's own world oracle (the
    /// [`tick_with_terrain`](Self::tick_with_terrain) idiom) and
    /// [`GolemConstruction::consumed`] is a report, not an action — the
    /// caller (the block-placement owner) is the one that actually clears
    /// those cells, exactly as this issue's own scope says: "given this
    /// placement, does a valid pattern exist, and if so spawn the golem".
    pub fn try_construct_golem(
        &mut self,
        block_at: &dyn Fn(i32, i32, i32) -> String,
        pumpkin_pos: (i32, i32, i32),
    ) -> Option<GolemConstruction> {
        if let Some(found) = golem::find_golem_pattern(block_at, golem::SNOW_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(0, 2, 0)` — the bottom snow block's cell.
            let feet = found.translate(0, 2, 0);
            let consumed = found.consumed(golem::SNOW_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:snow_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            return Some(GolemConstruction {
                species: GolemSpecies::Snow,
                id,
                consumed,
            });
        }
        if let Some(found) = golem::find_golem_pattern(block_at, golem::IRON_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(1, 2, 0)` — the bottom-centre iron block's cell.
            let feet = found.translate(1, 2, 0);
            let consumed = found.consumed(golem::IRON_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:iron_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            // vanilla additionally calls `setPlayerCreated(true)`
            // (`IronGolem.java:79`), which suppresses this golem attacking
            // the player who angered it and is checked on NBT save/load.
            // This sim has no such per-golem flag and no player-directed
            // hostility model for a neutral mob to suppress — a disclosed
            // gap, not a silent omission: a player-built iron golem here
            // behaves identically to a village-spawned one.
            return Some(GolemConstruction {
                species: GolemSpecies::Iron,
                id,
                consumed,
            });
        }
        None
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
    /// One tick, settling dropped items against this sim's own terrain snapshot.
    ///
    /// **Production should call [`tick_with_terrain`](Self::tick_with_terrain)
    /// instead**, and the difference is a real gameplay bug rather than a
    /// preference: the snapshot only covers the 7×7 `mob_area` columns taken when
    /// the world opened, so items dropped anywhere else fall straight through the
    /// ground. See [`settle_item`]. This entry point stays for hermetic callers,
    /// whose fixture world *is* the whole world they care about.
    pub fn tick(&mut self) {
        let world = self.world;
        self.tick_with_terrain(&|x, y, z| world.block_state(x, y, z).to_owned());
    }

    /// Ticks between one unemployed villager's job searches — throttles
    /// [`villager::find_and_claim_workstation`]'s bounded terrain scan (see
    /// that function's own doc for the cost it is bounding). 100 ticks is a
    /// scope choice, not a transcribed vanilla constant: nothing in this
    /// codebase ports `AssignProfessionFromJobSite`'s own interval.
    #[cfg(not(target_arch = "wasm32"))]
    const JOB_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-profession pass (issue #243): throttled job search for
    /// unemployed villagers, and re-verification for employed ones.
    ///
    /// Re-verification, not an event hook, is how "losing the block loses
    /// the job" is implemented — see [`villager`]'s own module doc for why,
    /// and for the one-tick lag that trade-off buys. A villager whose
    /// workstation position no longer resolves to the profession it was
    /// claimed under (destroyed, or replaced with a different workstation
    /// type) releases its ticket and goes back to unemployed on the very
    /// next call.
    ///
    /// Native-only (issue #243's wasm scope note) — see
    /// [`villager::WorkstationClaims`]'s own doc. A villager spawned in a
    /// `wasm32` (browser singleplayer) world keeps whatever profession it
    /// already had and simply never claims a new one.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_professions(&mut self) {
        let world = self.world;
        let claims = &mut self.workstation_claims;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.workstation {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::poi_type_for_block(villager::bare_block_id(state))
                    .and_then(villager::profession_for_poi_type)
                    == Some(mob.profession);
                if !still_valid {
                    claims.remove(pos);
                    mob.set_profession(villager::Profession::None, None);
                }
                continue;
            }
            // A profession with no job site (`Nitwit`) has nothing to search
            // for; only `None` (truly unemployed) runs the search below.
            if mob.profession != villager::Profession::None {
                continue;
            }
            if mob.job_search_cooldown > 0 {
                mob.job_search_cooldown -= 1;
                continue;
            }
            mob.job_search_cooldown = Self::JOB_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some((pos, profession)) =
                villager::find_and_claim_workstation(origin, world, claims)
            {
                mob.set_profession(profession, Some(pos));
            }
        }
    }

    /// One tick, settling dropped items against a caller-supplied solidity
    /// oracle — the live world, when the caller has one.
    ///
    /// Only the item-settling pass consults `block_state`; everything else still
    /// reads the snapshot, because mob pathfinding genuinely wants a view that
    /// does not change underneath a search in progress. Items are the opposite
    /// case: an item has to land on the block that is there *this* tick.
    ///
    /// **The oracle is a block-state *name*, not a solid/air boolean.** It used to be
    /// the latter, and one bit per cell cannot express the shape an item actually
    /// rests on: a bottom slab, soul sand and a patch of grass all answered "solid"
    /// and all settled the item at the top of the cell. See [`ItemCollision`] for the
    /// measured table and [`settle_item`] for the sweep that consumes it.
    pub fn tick_with_terrain(&mut self, block_state: &dyn Fn(i32, i32, i32) -> String) {
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
                .map(|p| dist_sqr(p.perception.position, pos))
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

        // Issue #625: the third element used to be discarded entirely — only
        // `take_new_attacks().is_empty()` was read, so the actual attacked
        // position (needed to resolve *which* player, if any, was on the
        // receiving end) never left this loop. See `PlayerHit`'s own doc
        // comment for what the resolution pass below does with it.
        let mut hits: Vec<(Option<i32>, Vec3, f32, Vec3)> = Vec::new();
        let mut detonations: Vec<(i32, Vec3)> = Vec::new();
        let mut bred: Vec<(i32, Vec3, ResourceKey)> = Vec::new();
        // Issue #456: accumulated into a local and moved into
        // `self.pending_grazes` after the loop, not pushed directly — `self` is
        // mutably borrowed by `&mut self.mobs` for the whole loop, exactly as it
        // is for `hits`/`detonations`/`bred`.
        let mut grazes: Vec<(BlockPos, EatenBlock)> = Vec::new();
        let mut launches: Vec<(i32, ProjectileLaunch)> = Vec::new();
        // Issue #458, primitive 4: self-inflicted damage requests, drained per
        // mob and resolved below — see the resolution pass after `hits`.
        let mut self_damage: Vec<(i32, f32)> = Vec::new();
        // Idle ambient vocalisations rolled this tick — accumulated into a
        // local for the same reason `grazes`/`bred` are: `self.mobs` is
        // mutably borrowed for the whole loop.
        let mut ambient_sounds: Vec<crate::effects::WorldEffect> = Vec::new();
        let tick_count = self.tick_count;
        for m in &mut self.mobs {
            // Vanilla ages `invulnerableTime`/`hurtTime` every tick regardless
            // of whether the mob was hit this tick.
            m.hurt_cooldown.tick();
            m.mob.tick(&mut m.goals);
            // Vanilla `Mob.baseTick`'s ambient-sound roll runs every tick a
            // mob is alive, independent of any goal — see
            // `roll_ambient_sound`'s own doc.
            if m.health > 0.0 {
                if let Some(effect) = roll_ambient_sound(m, tick_count) {
                    ambient_sounds.push(effect);
                }
            }
            let new_attacks = m.mob.take_new_attacks();
            // Issue #233: `Bee.doHurtTarget` — the sting connects the instant
            // this mob's own attack fires (this codebase has no separate
            // "swing missed" outcome, matching the fidelity every other
            // consumer of `take_new_attacks` already assumes). `stung_at` is
            // sticky (only the *first* sting matters) and clearing `anger`
            // here is vanilla's `stopBeingAngry()`, called from the same
            // method — see `SimMob::stung_at`'s own doc comment for why this
            // is what stops the bee re-acquiring a target without a second
            // `!hasStung()` guard on the goal itself.
            if !new_attacks.is_empty() && m.stung_at.is_none() && m.entity_type.path() == "bee" {
                m.stung_at = Some(tick_count);
                m.anger = None;
            }
            for target_pos in new_attacks {
                // Carry the attacker's own position too, so the victim can
                // retaliate: vanilla's `hurt` sets `lastHurtByMob` from the
                // damage source's attacker (`LivingEntity.java:1358`), which is
                // what `HurtByTargetGoal` reads. Before #441 this tuple was
                // `(target, damage)` only, so a mob struck by another mob had
                // no way to learn who hit it and `HurtByTargetGoal` could never
                // fire even once the perception seam existed.
                hits.push((m.attack_target_id, target_pos, m.attack_damage, m.position()));
            }
            // Issue #233: `Bee.customServerAiStep`'s self-destruct roll — see
            // `bee_sting_death_roll`'s own doc comment for the exact formula
            // and its two derived bounds (certainly alive at sting+1,
            // certainly dead by sting+1200).
            if let Some(stung_at) = m.stung_at {
                let elapsed = tick_count.saturating_sub(stung_at);
                if elapsed > 0 && elapsed % 5 == 0 && bee_sting_death_roll(tick_count, m.id, elapsed)
                {
                    // A fixed, large amount rather than `m.health`: this is a
                    // lethal roll, not a graded hit, and `apply_damage`'s
                    // reductions (armour, absorption) must not be able to
                    // leave a "certainly dead" tick non-lethal.
                    m.mob.damage_self(10_000.0);
                }
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
            // Paired with the launching mob's id so the impact pass can exclude
            // it: a projectile is created inside its shooter's own bounding box,
            // so without an owner a skeleton's arrow hits the skeleton.
            launches.extend(m.mob.take_new_launches().into_iter().map(|l| (m.id, l)));
            for amount in m.mob.take_self_damage() {
                self_damage.push((m.id, amount));
            }
        }
        self.push_entities();
        self.pending_grazes.extend(grazes);
        self.pending_ambient_sounds.extend(ambient_sounds);
        for (shooter, launch) in launches {
            use lodestone_entity::ai::roster::ranged::{integrates_as_arrow, projectile_entity_type};
            let projectile = if integrates_as_arrow(launch.kind) {
                Projectile::arrow(launch.origin, launch.velocity)
            } else {
                Projectile::throwable(launch.origin, launch.velocity)
            };
            let key = ResourceKey::from_str(&format!("minecraft:{}", projectile_entity_type(launch.kind)))
                .expect("static projectile key");
            self.spawn_projectile_from(key, projectile, Some(shooter));
        }
        for (target_id, target_pos, raw_damage, attacker_pos) in hits {
            if let Some(target_id) = target_id
                && let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id)
            {
                let applied = target.apply_damage(raw_damage, DamageFlags::default());
                target.mob.note_hurt(Some(attacker_pos));
                self.note_vocalisation(target_id, applied);
                continue;
            }
            // Issue #625: `attack_target_id` names only another `SimMob`
            // (see `PlayerHit`'s own doc comment), which is why every
            // hostile-melee attack against a player fell on the floor here
            // before this arm existed — `target_id` is `None` for the
            // `NearestAttackableTargetGoal`/`MeleeAttackGoal` path that
            // targets `nearest_player`, and nothing resolved `target_pos`
            // against anything. Matched by position rather than by id
            // because the goal seam carries no player identity at all — see
            // `PlayerHit`'s doc comment for the exact-match reasoning and its
            // one disclosed miss (a stale grudge target).
            if let Some(identity) = self
                .players
                .iter()
                .find(|p| dist_sqr(p.perception.position, target_pos) < 1e-6)
                .and_then(|p| p.identity)
            {
                self.pending_player_hits.push(PlayerHit {
                    identity,
                    raw_damage,
                    attacker_pos,
                });
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
        // Issue #260: the impact pass runs **before** the motion tick, matching
        // `AbstractArrow.tick`'s own order — it clips the segment it is about to
        // travel and only moves if nothing was hit. Resolving after the move would
        // put every impact a tick late and let an arrow settle on the far side of
        // a wall. Before this, `spawn_projectile`'s own doc comment said hit
        // detection was "explicit follow-up": a skeleton's arrows flew for their
        // whole lifetime through anything in the way and hurt nothing.
        self.resolve_projectile_impacts();
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
        let view = ItemCollision {
            block_state,
            probe_count: std::cell::Cell::new(0),
        };
        for (&id, state) in &mut self.item_state {
            let before = state.motion.position;
            state.motion.tick();
            settle_item(&view, &mut state.motion, before);
            if state.motion.position.y < f64::from(world.min_y) - VOID_DESPAWN_DEPTH {
                fell_out_of_the_world.push(id);
            }
        }
        // **The cost of the sweep, as a counter rather than a duration.** Swept
        // collision against real shapes is strictly more work per item than one
        // boolean lookup was, and the number of items in one tick is unbounded — so
        // the thing worth measuring is not per-item cost but how much of one tick a
        // floor covered in drops can consume. A counter is what a gate can assert
        // and what survives being read on a loaded machine; see
        // `items_settled_probe_count`.
        self.item_probe_count = view.probe_count.get();
        // `Entity.checkBelowWorld`'s discard, and not merely tidiness: an item
        // that escapes the world (a column the snapshot does not cover, so
        // `is_solid` is false everywhere) would otherwise keep being ticked and
        // streamed for its full 6000-tick life at ever-increasing depth.
        for id in fell_out_of_the_world {
            self.item_state.remove(&id);
            self.items.remove(id);
        }
        self.merge_neighbouring_items();
        // Experience orbs, on the same live-terrain oracle the items above use and for
        // the same reason: an orb settled against the sim's static `ChunkWorld`
        // snapshot would fall through any block the player has placed and rest on any
        // block they have mined. `tick_orbs` reads `tick_count` for its merge phase, so
        // it runs before the increment below.
        self.tick_orbs(&view);
        self.tick_leashes();
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_professions();
        // Issue #257: fishing bobbers. Reads `self.world` (the static
        // per-tick terrain snapshot, not the live `view` oracle the item/orb
        // passes just above use) — see `fishing::MobSim::tick_fishing_bobbers`'s
        // own doc for why a bobber's whole interesting life is spent sitting
        // in open water, where the two oracles agree.
        self.tick_fishing_bobbers();
        // Issue #241: raids. Wave spawning and victory/defeat need no live
        // terrain oracle either — see `raid::MobSim::tick_raids`'s own doc.
        self.tick_raids();

        self.tick_count += 1;
    }

    /// Shoves apart entities whose bodies overlap — vanilla `Entity::push`,
    /// invoked once per tick for every pushable neighbour by
    /// `LivingEntity::pushEntities` (`this.level().getPushableEntities(...)`
    /// then `this.doPush(entity)` per neighbour), called near the end of
    /// `LivingEntity::baseTick`, after that tick's own movement has already
    /// been applied — the same ordering `tick_with_terrain` gives this call,
    /// right after the per-mob loop that runs `m.mob.tick(...)`.
    ///
    /// # The formula lives in `lodestone-physics`, not here
    ///
    /// [`push_impulse`] delegates to [`lodestone_physics::pair_push_vector`],
    /// which already carries the full citation of `Entity::push(Entity)` —
    /// see `docs/entity-push.md` for the derivation, including the
    /// genuinely-non-obvious `sqrt(max(|dx|,|dz|))` Chebyshev normaliser
    /// (not `sqrt(dx²+dz²)`) and the widened `0.01f`/`0.05f` literals. That
    /// module is otherwise **unwired** — `docs/entity-push.md`'s own "Wiring"
    /// section says nothing in `lodestone-shell`, `lodestone-ecs` or
    /// `lodestone-client` calls it yet, because its documented use case is
    /// the *client-authoritative local player* feeling a push from nearby
    /// entities, which is a different half of vanilla's symmetric rule from
    /// this one: a *server-authoritative mob* being shoved by a player or
    /// another mob. This call site is the first production consumer.
    ///
    /// # What this port narrows, disclosed
    ///
    /// * **Overlap is a horizontal-distance-under-combined-half-width test**,
    ///   not vanilla's real AABB intersection
    ///   (`Level::getEntities`/`getPushableEntities`), which also accounts for
    ///   height overlap. Two mobs stacked exactly on top of one another with
    ///   no horizontal offset therefore push in this port and would not
    ///   collide in vanilla (their Y ranges might not overlap) — an edge case
    ///   this seam's `PathWorld`/`SimMob` do not carry enough geometry to
    ///   resolve exactly.
    /// * **Applied once per pair per tick**, not vanilla's twice (each side's
    ///   own `pushEntities()` call invokes `doPush` against the other, so a
    ///   living pair receives the impulse from *both* directions every tick).
    ///   The formula itself is unchanged; this halves the net closing-speed
    ///   reduction relative to vanilla's double application, a scope cut
    ///   rather than a transcription error.
    /// * **Player recoil is not applied.** A player's own position/velocity
    ///   in this codebase is client-authoritative (the client sends
    ///   `move_player_pos`; the server does not own a player's velocity the
    ///   way it owns a mob's), so shoving a player back needs a clientbound
    ///   self-velocity packet the client applies to its own physics —
    ///   `crates/protocol/**` and `crates/lodestone-shell/**`, both outside
    ///   this crate. This pass pushes the **mob** away from an intersecting
    ///   player (the reported "I can't push pigs" symptom: the pig now moves
    ///   out of the way), but the player itself is not nudged.
    /// * **`isPushable()`/vehicle/passenger exclusions are not modelled** —
    ///   every [`SimMob`] is treated as pushable, matching vanilla's default
    ///   for a plain `LivingEntity` with nothing riding it.
    /// * **Mount cramming damage is not modelled** — vanilla's
    ///   `maxEntityCramming` gamerule check in the same method.
    fn push_entities(&mut self) {
        let n = self.mobs.len();
        if n == 0 {
            return;
        }
        let positions: Vec<Vec3> = self.mobs.iter().map(SimMob::position).collect();
        let widths: Vec<f64> = self
            .mobs
            .iter()
            .map(|m| f64::from(m.shape().width))
            .collect();
        let mut impulses = vec![Vec3::default(); n];

        // Mob-mob pairs.
        for i in 0..n {
            for j in (i + 1)..n {
                let touch = (widths[i] + widths[j]) / 2.0;
                if let Some((a, b)) = push_impulse(positions[i], positions[j], touch) {
                    impulses[i].x += a.x;
                    impulses[i].z += a.z;
                    impulses[j].x += b.x;
                    impulses[j].z += b.z;
                }
            }
        }

        // Player-mob pairs: only the mob side is pushed — see this method's
        // own doc comment for why player recoil needs a different seam.
        const PLAYER_WIDTH: f64 = 0.6; // `Player::createLivingAttributes` has no override; the default `Entity` bounding box is 0.6 wide.
        for i in 0..n {
            for p in &self.players {
                let touch = (widths[i] + PLAYER_WIDTH) / 2.0;
                if let Some((mob_impulse, _)) = push_impulse(positions[i], p.perception.position, touch) {
                    impulses[i].x += mob_impulse.x;
                    impulses[i].z += mob_impulse.z;
                }
            }
        }

        for (mob, impulse) in self.mobs.iter_mut().zip(impulses) {
            if impulse.x != 0.0 || impulse.z != 0.0 {
                mob.apply_knockback(impulse);
            }
        }
    }

    /// Per-tick leash physics: pull leashed mobs toward their holder, and
    /// snap (dropping a lead item) past [`LEASH_TOO_FAR_DIST`] — vanilla
    /// `Leashable.tickLeash`.
    ///
    /// **Simplified, and disclosed rather than silent.** Real vanilla
    /// computes a spring/torque interaction across up to four
    /// attachment-point pairs and applies angular momentum to yaw
    /// (`Leashable.checkElasticInteractions`/`computeElasticInteraction`).
    /// This applies one straight-line impulse toward the holder's position
    /// instead, through [`SimMob::apply_knockback`] — the same "hand
    /// velocity application to the physics owner rather than growing a
    /// second model here" seam `explosion.rs`/`damage.rs` already use for
    /// combat knockback. Three things this does not carry:
    ///
    /// - No yaw torque (vanilla's `angularMomentum`/`entity.setYRot`).
    /// - **No per-entity bounding-box subtraction from the elastic
    ///   threshold** — vanilla's actual pull distance is
    ///   `LEASH_ELASTIC_DIST - holder.getBbWidth() - entity.getBbWidth()`;
    ///   this uses the flat [`LEASH_ELASTIC_DIST`] constant, so a very wide
    ///   mob starts pulling slightly later than vanilla would.
    /// - **A holder that cannot be resolved this tick silently drops the
    ///   leash with no item spawned** — vanilla's `!canInteractWithLevel()`
    ///   branch, narrowed to its `ENTITY_DROPS`-off arm (`removeLeash`)
    ///   only. A disconnected player or a removed leash-holder mob loses the
    ///   leashed mob's attachment rather than the mob dropping a lead for a
    ///   holder that is not really gone (a reconnecting player, in
    ///   particular) — the safer of the two wrong answers, but still a
    ///   simplification worth naming.
    fn tick_leashes(&mut self) {
        let mut snapped: Vec<(i32, Vec3)> = Vec::new();
        let mut pulled: Vec<(i32, Vec3)> = Vec::new();
        let mut orphaned: Vec<i32> = Vec::new();

        for i in 0..self.mobs.len() {
            let Some(holder) = self.mobs[i].leash_holder else {
                continue;
            };
            let mob_pos = self.mobs[i].position();
            let holder_pos = match holder {
                LeashHolder::Player(uuid) => self
                    .players
                    .iter()
                    .find(|p| p.identity.as_ref().map(|i| i.uuid) == Some(uuid))
                    .map(|p| p.perception.position),
                LeashHolder::Mob(id) => self.mobs.iter().find(|m| m.id == id).map(SimMob::position),
                LeashHolder::Fence(pos) => Some(Vec3::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                )),
            };
            let Some(holder_pos) = holder_pos else {
                orphaned.push(self.mobs[i].id);
                continue;
            };
            let distance = dist_sqr(mob_pos, holder_pos).sqrt();
            if distance > LEASH_TOO_FAR_DIST {
                snapped.push((self.mobs[i].id, mob_pos));
            } else if distance > LEASH_ELASTIC_DIST {
                let excess = distance - LEASH_ELASTIC_DIST;
                let dir = Vec3::new(
                    (holder_pos.x - mob_pos.x) / distance,
                    (holder_pos.y - mob_pos.y) / distance,
                    (holder_pos.z - mob_pos.z) / distance,
                );
                // Capped, so a mob yanked from far away is pulled steadily
                // rather than teleported in one tick — vanilla's own spring
                // is likewise bounded (`SPRING_DAMPENING`).
                let pull = excess.min(1.0) * 0.3;
                pulled.push((
                    self.mobs[i].id,
                    Vec3::new(dir.x * pull, dir.y * pull, dir.z * pull),
                ));
            }
        }

        for id in orphaned {
            if let Some(mob) = self.get_mut(id) {
                mob.set_leash_holder(None);
            }
        }
        for (id, impulse) in pulled {
            if let Some(mob) = self.get_mut(id) {
                mob.apply_knockback(impulse);
            }
        }
        for (id, pos) in snapped {
            if let Some(mob) = self.get_mut(id) {
                mob.set_leash_holder(None);
            }
            self.spawn_item(
                "minecraft:lead".parse().expect("valid key"),
                pos,
                Vec3::new(0.0, 0.0, 0.0),
                lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                    1,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
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
        let mut patrol_group = vec![None; n];
        let mut stared_at = vec![false; n];

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
            nearest_player[i] =
                nearest_by(&self.players, pos, |p| p.perception.position, |_| true, None);

            // --- temptation -----------------------------------------------
            // The range *is* on the mob here (`Attributes.TEMPT_RANGE`), so it
            // belongs in the feed. See `TEMPT_RANGE`.
            //
            // The item test is per-species (`tempt_food`), which is why
            // `PlayerPerception` carries the held item rather than a boolean:
            // the same wheat that tempts a cow does nothing to a chicken.
            let foods = species::tempt_food(&species);
            if !foods.is_empty() {
                temptation[i] = nearest_by(
                    &self.players,
                    pos,
                    |p| p.perception.position,
                    |p| {
                        p.perception
                            .held_item
                            .as_ref()
                            .is_some_and(|item| foods.contains(&item.path()))
                    },
                    Some((TEMPT_RANGE, TEMPT_RANGE)),
                );
            }

            // --- avoid threat ---------------------------------------------
            let avoided = species::avoided_species(&species);
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
            // The owner *identity* is a census fact (`SimMob::owner`); only the
            // resolved position can cross the seam
            // (`MobController::owner_position`), so this is resolved here
            // exactly like partner/parent.
            //
            // Both flavours resolve, and the player one is what taming produces:
            // vanilla's owner is a uuid (`TamableAnimal.DATA_OWNERUUID_ID`) and
            // `getOwner()` resolves it against the level every time it is asked,
            // which is what `player_position` does here. A tamed pet whose owner
            // is not in the list resolves to `None` — offline, or in another
            // dimension, which are the same two cases vanilla's
            // `owner.level() != this.level()` covers — and `None` is the correct
            // answer rather than a stale last-known position: a pet must not
            // path toward where you were an hour ago.
            //
            // `is_tame` is fed *unconditionally* below rather than derived from
            // this, because a mob is tame whether or not its owner is resolvable.
            match me.owner {
                Some(MobOwner::Mob(oid)) => {
                    owner[i] = nearest_by(
                        &self.mobs,
                        pos,
                        SimMob::position,
                        |other| other.id == oid,
                        None,
                    );
                }
                Some(MobOwner::Player(uuid)) => {
                    owner[i] = self.player_position(uuid);
                }
                None => {}
            }

            // --- patrol group target ---------------------------------------
            // Issue #241a. A leader never reads this — it computes its own
            // fresh target from `LongDistancePatrolGoal` itself; only a
            // non-leading, still-patrolling member needs the host's census.
            // See `nearest_patrol_leader_target`'s own doc comment for why
            // this cannot reuse `nearest_by`.
            if me.is_patrolling() && !me.is_patrol_leader() {
                patrol_group[i] = nearest_patrol_leader_target(&self.mobs, pos, me.id);
            }

            // --- gaze (issue #458, primitive 2) -----------------------------
            // `MobController::is_being_stared_at` is host-fed: the geometry is
            // `lodestone_entity::ai::mob::is_in_view_cone`, vanilla's exact
            // `dot > 1.0 - coneSize / dist`. Line of sight is the same
            // disclosed gap `find_nearest_target` already carries — no world
            // raycast at this seam, erring permissive. The carved-pumpkin
            // disguise check (`PLAYER_NOT_WEARING_DISGUISE_ITEM`) is not
            // modelled either: `PlayerPerception` has no armour-slot data yet.
            //
            // `0.025` is the enderman's own `coneSize`
            // (`EnderMan.java:210`); this feed is per-mob, not per-species, so
            // every mob gets the same tolerance today — the only consumer is
            // `EndermanFreezeWhenLookedAt`, so this is not yet observably
            // wrong, but a second gaze-gated species with a different
            // `coneSize` would need this to become species-aware.
            let mob_eye = Vec3::new(pos.x, pos.y + f64::from(me.shape().height) * 0.85, pos.z);
            stared_at[i] = self.players.iter().any(|p| {
                let player_eye = Vec3::new(
                    p.perception.position.x,
                    p.perception.position.y + PLAYER_EYE_HEIGHT,
                    p.perception.position.z,
                );
                lodestone_entity::ai::mob::is_in_view_cone(
                    player_eye,
                    p.perception.view_direction,
                    mob_eye,
                    0.025,
                    true,
                )
            });
        }

        for (i, m) in self.mobs.iter_mut().enumerate() {
            // Not folded into the chain below: `set_tame`/`set_ordered_to_sit`
            // read `m`'s own record while the chain holds `m.mob` mutably.
            let (tame, ordered_to_sit) = (m.tame, m.ordered_to_sit);
            m.mob.set_tame(tame).set_ordered_to_sit(ordered_to_sit);
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
                .set_owner(owner[i])
                .set_patrol_group_target(patrol_group[i])
                .set_stared_at(stared_at[i]);
        }
    }

    /// A player right-clicked a mob with (or without) an item — vanilla
    /// `Mob.interact` → `mobInteract`, the single producer for taming, sitting,
    /// feeding and breeding.
    ///
    /// # The dispatch order is the specification
    ///
    /// Vanilla's `mobInteract` overrides are nested `if` chains that end in
    /// `super.mobInteract`, so *which arm wins* is as much a part of the port as
    /// the constants are. Two orderings that both "tame a wolf" differ
    /// observably: feeding a hurt tame wolf meat must heal it, **not** put it in
    /// love, and only once it is at full health does the same item breed it
    /// (`Wolf.mobInteract`'s first arm, then `super` reaching
    /// `Animal.mobInteract`). This method's arms are in that order and each one
    /// names the vanilla method it comes from.
    ///
    /// # What is deliberately not here
    ///
    /// Collar dyeing, wolf body armour and its repair, the parrot's poisonous
    /// cookie, and mounting a tame horse. Each needs an item model this crate
    /// does not have (dye components, equipment slots, damage values) or a
    /// passenger model that does not exist.
    ///
    /// Returns [`InteractOutcome::Pass`] when nothing responded, which is the
    /// caller's signal to fall through to whatever it does with an unconsumed
    /// right-click.
    /// Attaches or detaches a lead between `mob_id` and the player `holder` —
    /// vanilla `Entity.interact`'s two leash-specific branches (excluding
    /// its sneak-multi-attach branch; see this method's own "not
    /// implemented" note).
    ///
    /// - If `mob_id` is already leashed to `holder`, detaches it (vanilla's
    ///   `leashable.getLeashHolder() == player` arm) and reports whether a
    ///   `minecraft:lead` item should be spawned (`creative` mirrors
    ///   `player.hasInfiniteMaterials()`, which this sim has no game-mode
    ///   state of its own to answer).
    /// - Else, if `holding_lead` and the mob is not already held by a
    ///   *player* (vanilla's `!(leashable.getLeashHolder() instanceof Player)`
    ///   guard — one player cannot steal another's leashed mob just by
    ///   holding a lead), attaches it to `holder`, dropping any existing
    ///   non-player leash first exactly as vanilla's `dropLeash()` does
    ///   before `setLeashedTo`.
    /// - Otherwise refuses: not leashable, no lead in hand, or out of
    ///   [`LEASH_TOO_FAR_DIST`] (`canHaveALeashAttachedTo`'s own
    ///   `leashSnapDistance` check).
    ///
    /// **Not implemented**: vanilla's sneak-right-click branch, which
    /// re-parents *every* mob already leashed to `holder` onto whatever
    /// entity was clicked, in one interaction. This only ever moves the one
    /// `mob_id` named — a real gap for a player leashing several animals to
    /// one another, not merely an unlikely input.
    pub fn try_leash(
        &mut self,
        mob_id: i32,
        holder: Uuid,
        holding_lead: bool,
        creative: bool,
    ) -> LeashOutcome {
        let Some(mob) = self.get(mob_id) else {
            return LeashOutcome::Refused;
        };
        if mob.leash_holder() == Some(LeashHolder::Player(holder)) {
            let pos = mob.position();
            self.get_mut(mob_id)
                .expect("just found")
                .set_leash_holder(None);
            let dropped_lead = !creative;
            if dropped_lead {
                // Spawned here, not left to the caller, for the same reason
                // `tick_leashes`' snap branch spawns its own item: one place
                // decides "a lead item now exists in the world", so a future
                // second call site cannot forget it or double it.
                self.spawn_item(
                    "minecraft:lead".parse().expect("valid key"),
                    pos,
                    Vec3::new(0.0, 0.0, 0.0),
                    lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                        1,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
            return LeashOutcome::Detached { dropped_lead };
        }
        if !holding_lead || matches!(mob.leash_holder(), Some(LeashHolder::Player(_))) {
            return LeashOutcome::Refused;
        }
        if !species::is_leashable_species(mob.entity_type()) {
            return LeashOutcome::Refused;
        }
        let mob_pos = mob.position();
        let Some(holder_pos) = self
            .players
            .iter()
            .find(|p| p.identity.as_ref().map(|i| i.uuid) == Some(holder))
            .map(|p| p.perception.position)
        else {
            return LeashOutcome::Refused;
        };
        if dist_sqr(mob_pos, holder_pos).sqrt() > LEASH_TOO_FAR_DIST {
            return LeashOutcome::Refused;
        }
        self.get_mut(mob_id)
            .expect("just found")
            .set_leash_holder(Some(LeashHolder::Player(holder)));
        LeashOutcome::Attached
    }

    /// Right-clicking a fence while holding a lead: re-parents every mob
    /// currently leashed to `holder` (the player) onto a knot at `fence_pos`
    /// — vanilla `LeadItem.bindPlayerMobs`. Unlike vanilla this never spawns
    /// a `LeashFenceKnotEntity`; see [`LeashHolder::Fence`]'s own doc
    /// comment for why, and for what that costs a real client (no visible
    /// knot to render or right-click).
    ///
    /// **Simplified from vanilla's own scan**: `bindPlayerMobs` only
    /// re-parents mobs within a 32-block radius of `fence_pos`; this moves
    /// every mob leashed to `holder` regardless of distance from the fence.
    /// The two coincide in practice — a leashed mob is already capped at
    /// [`LEASH_TOO_FAR_DIST`] (12 blocks) from `holder`, and a player using
    /// this interaction is, by construction, standing at the fence — but a
    /// contrived setup (holder far from the fence, mob far from holder in
    /// the other direction) could observe the difference.
    ///
    /// Returns the ids re-leashed; empty means no mob was leashed to
    /// `holder` at all, matching vanilla's `InteractionResult.PASS`.
    pub fn try_leash_to_fence(&mut self, holder: Uuid, fence_pos: BlockPos) -> Vec<i32> {
        let mut moved = Vec::new();
        for mob in &mut self.mobs {
            if mob.leash_holder == Some(LeashHolder::Player(holder)) {
                mob.leash_holder = Some(LeashHolder::Fence(fence_pos));
                moved.push(mob.id);
            }
        }
        moved
    }

    /// Spawns a wandering trader at `pos` with 1–2 leashed llama escorts —
    /// the entity-spawn half of vanilla `WanderingTraderSpawner.spawn`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/npc/wanderingtrader/WanderingTraderSpawner.java:75-120`).
    /// Returns the trader's id and every llama actually spawned.
    ///
    /// **This is only the "given a spawn position, create the entity group"
    /// half.** `WanderingTraderSpawner` itself is a `CustomSpawner` driven by
    /// the world tick with its own 1200-tick poll, a 24000-tick base delay,
    /// a climbing 25→75% chance, a player-anchored 48-block search for a
    /// `PoiTypes.MEETING` point (falling back to the player), and a
    /// `BiomeTags.WITHOUT_WANDERING_TRADER_SPAWNS` exclusion — none of which
    /// exists in this crate. That whole cycle belongs beside
    /// [`crate::mob_spawn`]'s existing per-species natural-spawn cap/timer
    /// engine, a file outside this pass's ownership; see this session's
    /// broker note (wandering trader spawn cycle) for the exact shape a
    /// caller there needs.
    ///
    /// **Simplified escort placement.** Vanilla's `tryToSpawnLlamaFor`
    /// searches up to 10 candidate positions within 4 blocks and can fail to
    /// find space, so "2 attempts" does not guarantee 2 llamas. This always
    /// places both at fixed offsets (`+2, 0, 0` and `-2, 0, 0` from the
    /// trader) with no space check — this sim has no per-cell obstruction
    /// query at the `MobSim` level the way vanilla's `BlockGetter` does, and
    /// two llamas beside an already-chosen valid trader spawn are the common
    /// case in practice.
    ///
    /// **Wares are not generated.** `WanderingTrader.updateTrades` builds
    /// its offer list from `TradeSets.WANDERING_TRADER_{BUYING,UNCOMMON,COMMON}`
    /// — this crate has no merchant-offer/trade-table model at all yet (see
    /// the villager-trading work this is deliberately distinct from). A
    /// spawned trader here has no wares and cannot be traded with.
    pub fn spawn_wandering_trader(&mut self, pos: Vec3) -> (i32, Vec<i32>) {
        let trader_id = self
            .spawn_species("minecraft:wandering_trader".parse().expect("valid key"), pos)
            .id();
        let mut llamas = Vec::new();
        for dx in [2.0, -2.0] {
            let llama_id = self
                .spawn_species(
                    "minecraft:trader_llama".parse().expect("valid key"),
                    Vec3::new(pos.x + dx, pos.y, pos.z),
                )
                .id();
            self.get_mut(llama_id)
                .expect("just spawned")
                .set_leash_holder(Some(LeashHolder::Mob(trader_id)));
            llamas.push(llama_id);
        }
        (trader_id, llamas)
    }

    pub fn interact(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        held_item: Option<&ResourceKey>,
    ) -> InteractOutcome {
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };
        let species = mob.entity_type().path().to_owned();
        let pos = mob.position();
        let item = held_item.map(|k| k.path().to_owned());
        let item = item.as_deref();

        // `Villager.mobInteract` is a full override, not an `Animal.mobInteract`
        // fall-through: a villager is never tameable, so this has to be a
        // short-circuit ahead of the `tame_mechanism` dispatch below rather
        // than another arm inside it (issue #245).
        if species == "villager" {
            let profession = mob.profession;
            let level = mob.villager_level;
            let has_offers = !villager::trades::offers_up_to(profession, level).is_empty();
            let outcome = if matches!(
                profession,
                villager::Profession::None | villager::Profession::Nitwit
            ) || !has_offers
            {
                // No job, or a job this crate has not ported real trades for
                // yet (`villager::trades`' own doc names which professions
                // those are) — an honest `Pass` rather than an empty screen.
                InteractOutcome::Pass
            } else {
                InteractOutcome::OpenTrade { profession, level }
            };
            return outcome;
        }

        let outcome = match species::tame_mechanism(&species) {
            Some(species::TameMechanism::Temper { max_temper }) => {
                self.interact_horse(mob_id, actor, item, max_temper)
            }
            Some(mechanism) => self.interact_tamable(mob_id, actor, item, &species, mechanism),
            // Every other species goes straight to `Animal.mobInteract`.
            None => self.interact_animal(mob_id, item, &species),
        };

        // Vanilla's particles are `broadcastEntityEvent(this, (byte)6|7|18)`,
        // which the *client* expands into a burst
        // (`TamableAnimal.spawnTamingParticles`, `Animal.handleEntityEvent`).
        // This server has no `ENTITY_EVENT` encoder, so the burst is published
        // directly as a `LEVEL_PARTICLES` packet with the same particle type,
        // count and Gaussian spread the client would have produced. A disclosed
        // substitution, not an approximation of the visual: seven HEART or SMOKE
        // particles at `getRandomY() + 0.5` either way.
        if let Some(particle) = outcome.particle() {
            self.pending_vocalisations
                .push(taming_particles(particle, pos));
        }
        outcome
    }

    /// `Wolf`/`Cat`/`Parrot.mobInteract` — the `TamableAnimal` chain.
    fn interact_tamable(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        species: &str,
        mechanism: species::TameMechanism,
    ) -> InteractOutcome {
        let species::TameMechanism::FoodRoll {
            items,
            one_in,
            sit_on_success,
        } = mechanism
        else {
            return InteractOutcome::Pass;
        };
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };

        if mob.is_tame() {
            // `isOwnedBy(player)` — a tame animal ignores everyone but its owner.
            // Vanilla's cat wraps its whole body in this check and the wolf
            // repeats it per arm; the effect is the same.
            if mob.owner_uuid() != Some(actor.uuid) {
                return InteractOutcome::Pass;
            }
            // `Wolf.mobInteract`'s first arm: `isFood(stack) && getHealth() <
            // getMaxHealth()` → `feed`. **Before** the breeding arm, which is
            // reached only through `super.mobInteract`.
            let is_food = item.is_some_and(|i| species::breeding_food(species).contains(&i));
            if is_food && mob.health() < mob.max_health() {
                let heal = species::tame_feed_heal(species);
                let mob = self.get_mut(mob_id).expect("checked above");
                mob.heal(heal);
                return InteractOutcome::Fed;
            }
            // `super.mobInteract` → `Animal.mobInteract`'s love arm.
            if is_food && self.try_set_in_love(mob_id) {
                return InteractOutcome::InLove;
            }
            // `if (!interactionResult.consumesAction() && isOwnedBy(player))` →
            // `setOrderedToSit(!isOrderedToSit())`. The *last* arm, so anything
            // above it suppresses the toggle — which is why an owner feeding a
            // hurt pet does not also sit it down.
            let mob = self.get_mut(mob_id).expect("checked above");
            let sitting = !mob.is_ordered_to_sit();
            mob.set_ordered_to_sit(sitting);
            return InteractOutcome::SitToggled { sitting };
        }

        // Untamed. The taming item is checked first and it is **not** the food
        // tag for the wolf: `Items.BONE`.
        if item.is_some_and(|i| items.contains(&i)) {
            // `Wolf.mobInteract`'s `!this.isAngry()` guard. The cat and the
            // parrot have no such gate, and `anger` is `None` for them anyway,
            // so this is one condition rather than a per-species branch.
            if self.get(mob_id).is_some_and(|m| m.anger.is_some()) {
                return InteractOutcome::Pass;
            }
            // `tryToTame`: one `nextInt(one_in)` draw, success on exactly `0`.
            let success = self.tame_rng.next_int(one_in) == 0;
            let mob = self.get_mut(mob_id).expect("checked above");
            if success {
                mob.tame(MobOwner::Player(actor.uuid));
                // `navigation.stop(); setTarget(null);` then, for the wolf and
                // the cat only, `setOrderedToSit(true)`.
                mob.set_attack_target(None);
                mob.set_attack_target_id(None);
                if sit_on_success {
                    mob.set_ordered_to_sit(true);
                }
                return InteractOutcome::Tamed;
            }
            return InteractOutcome::TameFailed;
        }

        // Still `super.mobInteract` → `Animal.mobInteract`: an **untamed** wolf
        // fed meat really does fall in love in vanilla, because the bone check
        // above did not match and the chain continues.
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// `AbstractHorse.mobInteract` → `handleEating`.
    ///
    /// The horse family's whole mechanism is a persisted counter, so this arm
    /// makes **no** tame roll: feeding raises `Temper` and nothing else. The roll
    /// lives in [`attempt_horse_tame`](Self::attempt_horse_tame), which
    /// `RunAroundLikeCrazyGoal` drives while a player is riding.
    fn interact_horse(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(item) = item else {
            // An empty-handed right-click on an untamed horse is
            // `doPlayerRide` — vanilla's only route to the tame roll. See
            // `attempt_horse_tame`'s doc for the one disclosed deviation.
            return if self.get(mob_id).is_some_and(|m| !m.is_tame()) {
                self.attempt_horse_tame(mob_id, actor, max_temper)
            } else {
                InteractOutcome::Pass
            };
        };

        // `handleEating`'s arms in order: heal, ageUp, love, temper. Love is
        // gated on `isTamed() && getAge() == 0 && !isInLove()` and only the two
        // gold items reach it.
        let mut used = false;
        if species::horse_breeding_items(item)
            && self.get(mob_id).is_some_and(SimMob::is_tame)
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }

        let gain = species::horse_temper_gain(item);
        let mob = match self.get_mut(mob_id) {
            Some(mob) => mob,
            None => return InteractOutcome::Pass,
        };
        // `if (temper > 0 && (itemUsed || !isTamed()) && getTemper() <
        // getMaxTemper())`. `hay_block` has `temper == 0` and so raises nothing,
        // however much of it you feed — the trap `horse_temper_gain` documents.
        if gain > 0 && mob.temper() < max_temper {
            let raised = (mob.temper() + gain).clamp(0, max_temper);
            mob.set_temper(raised);
            used = true;
        }
        if used {
            let temper = mob.temper();
            InteractOutcome::TemperRaised { temper }
        } else {
            InteractOutcome::Pass
        }
    }

    /// `Animal.mobInteract` for a species with no taming at all — the cow, sheep,
    /// pig, chicken and rabbit route, and the only thing feeding them does.
    fn interact_animal(
        &mut self,
        mob_id: i32,
        item: Option<&str>,
        species: &str,
    ) -> InteractOutcome {
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// `Animal.mobInteract`'s love arm as a single testable condition:
    /// `getAge() == 0 && canFallInLove()`, then `setInLove`.
    ///
    /// **`age == 0` exactly**, not `!is_baby()`. The two differ on a parent
    /// inside its post-breeding cooldown, whose age is a positive countdown: it
    /// is not a baby and it still cannot fall in love. Reading `!is_baby()` here
    /// would let a pair breed every 60 ticks forever.
    fn try_set_in_love(&mut self, mob_id: i32) -> bool {
        let Some(mob) = self.get_mut(mob_id) else {
            return false;
        };
        if mob.age() != 0 || mob.is_in_love() {
            return false;
        }
        mob.set_in_love();
        true
    }

    /// `RunAroundLikeCrazyGoal.tick`'s tame roll for the horse family:
    /// `random.nextInt(getMaxTemper()) < getTemper()`, and on failure
    /// `modifyTemper(5)` plus `makeMad()`.
    ///
    /// # The one disclosed deviation, and why it is not silent
    ///
    /// Vanilla reaches this roll from a **goal** that runs while a player is a
    /// passenger, gated on its own `random.nextInt(adjustedTickDelay(50)) == 0`
    /// — so a rider gets roughly one attempt every 25 ticks until the horse
    /// yields. This server has no passenger model at all, so there is nothing to
    /// stay mounted on and no goal to tick. The attempt is therefore made **once
    /// per mount attempt** (one empty-handed right-click), and the 1-in-50 outer
    /// gate is not drawn.
    ///
    /// What is *not* changed is the part that makes the horse a different
    /// mechanism from the wolf: the roll is still `nextInt(maxTemper) < temper`,
    /// still fails at temper `0` with certainty, and still adds 5 temper per
    /// failure — so a horse still has to be fed or ridden repeatedly, and the
    /// number of attempts it takes is vanilla's. Only the *pacing* differs.
    pub fn attempt_horse_tame(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(mob) = self.get(mob_id) else {
            return InteractOutcome::Pass;
        };
        if mob.is_tame() || max_temper <= 0 {
            return InteractOutcome::Pass;
        }
        let temper = mob.temper();
        let success = self.tame_rng.next_int(max_temper) < temper;
        let mob = self.get_mut(mob_id).expect("checked above");
        if success {
            // `tameWithName`: `setOwner` + `setTamed(true)`. Note it does **not**
            // order the horse to sit — horses have no sitting pose at all.
            mob.tame(MobOwner::Player(actor.uuid));
            InteractOutcome::Tamed
        } else {
            let raised = (temper + 5).clamp(0, max_temper);
            mob.set_temper(raised);
            InteractOutcome::TameFailed
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

            // `finalizeSpawnChildFromBreeding`'s last statement:
            // `if (gameRules.get(MOB_DROPS)) addFreshEntity(new ExperienceOrb(…,
            // random.nextInt(7) + 1))`.
            //
            // **Constructed, not awarded**, and the distinction is visible:
            // `ExperienceOrb.award` splits an amount into denominations and tries
            // `tryMergeToExisting` first, whereas breeding builds one orb with
            // one value directly. Routing this through `award_experience` would
            // let a second mating in the same spot silently fold into the first
            // orb, so `spawn_orb` is the right call even though the values are
            // small enough that denomination splitting would be a no-op.
            //
            // The gate is the `mob_drops` rule, exactly as for a mob's death
            // reward — breeding on a `doMobLoot false` server pops nothing.
            if self.mob_drops {
                let value = self.breed_rng.next_int(7) + 1;
                self.spawn_orb(value, breeder_pos, Vec3::new(0.0, 0.0, 0.0));
            }
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

    /// Drains every idle ambient vocalisation [`tick`](Self::tick) has rolled
    /// since the last call — [`take_vocalisations`](Self::take_vocalisations)'
    /// periodic sibling. Drained for the same reason: a slow consumer must not
    /// replay the same moo twice.
    pub fn take_ambient_sounds(&mut self) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut self.pending_ambient_sounds)
    }

    /// Drains every per-entity animation cue recorded since the last call — the
    /// visible sibling of [`take_vocalisations`](Self::take_vocalisations), and
    /// drained rather than read for the same reason: a slow consumer must not
    /// flash the same hit twice.
    pub fn take_entity_animations(&mut self) -> Vec<MobAnimation> {
        std::mem::take(&mut self.pending_animations)
    }

    /// Records the hurt or death sound **and animation** for a hit that landed on
    /// mob `id` — vanilla's `LivingEntity.hurt`/`die` playing
    /// `getHurtSound()`/`getDeathSound()`, plus the `broadcastDamageEvent` /
    /// `broadcastEntityEvent(this, (byte)3)` those two methods send alongside.
    ///
    /// Called from every funnel that applies damage rather than from
    /// [`SimMob::apply_damage`] itself, because the queue lives on the sim and
    /// `apply_damage` holds only the one mob. `applied <= 0.0` (a hit fully
    /// swallowed by i-frames or absorption) is silent *and* invisible, matching
    /// vanilla's own `hurtServer` returning before either broadcast — the guard is
    /// `tookFullDamage` there and the same `applied > 0.0` here.
    ///
    /// **Must be called before the end-of-tick `retain`**, or a killing blow
    /// finds no mob to read the species and position from and dies silently.
    ///
    /// # Why the sound and the animation share one entry point
    ///
    /// They share a *cause*. Vanilla emits both from inside `hurtServer`/`die`
    /// under the same guard, so splitting them into two recorders here would give
    /// two chances for one damage funnel to be taught about one of them and not
    /// the other — which is exactly how the animation came to be missing while
    /// every funnel already had the sound.
    fn note_vocalisation(&mut self, id: i32, applied: f32) {
        if applied <= 0.0 {
            return;
        }
        let Some(mob) = self.mobs.iter_mut().find(|m| m.id == id) else {
            return;
        };
        // Vanilla `Mob.playHurtSound` calls `resetAmbientSoundTime()` before
        // playing the hurt sound itself, so a mob that just yelped in pain
        // does not also roll an idle vocalisation on the very next tick.
        mob.ambient_sound_time = -AMBIENT_SOUND_INTERVAL;
        // Hurt *and* death on a killing blow, in that order, because vanilla sends
        // both: `hurtServer` broadcasts the damage event and only then calls `die`,
        // which broadcasts byte 3. The client needs the flash to have started for
        // the tip-over to look like a death rather than a teleport.
        self.pending_animations
            .push(MobAnimation::Hurt { entity_id: id });
        if mob.health <= 0.0 {
            self.pending_animations
                .push(MobAnimation::Died { entity_id: id });
        }
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

    /// Drains every player hit by a hostile mob's melee attack since the last
    /// call (issue #625) — the player-facing twin of
    /// [`take_detonations`](Self::take_detonations)'s handoff shape, for the
    /// identical reason: this sim owns no connection, so the driver
    /// (`crate::server::serve_play`'s `vitals_tick` arm) is what turns each
    /// entry into a real `PlayerVitals::apply_damage` call and a `SET_HEALTH`/
    /// hurt-animation packet. See [`PlayerHit`]'s own doc comment for how a
    /// target position resolves to an identity, and its disclosed gap for a
    /// grudge target.
    pub fn take_player_hits(&mut self) -> Vec<PlayerHit> {
        std::mem::take(&mut self.pending_player_hits)
    }

    /// Drains every fire-ignition attempt a live lightning bolt made this
    /// tick — [`pending_lightning_fires`](Self::pending_lightning_fires)'s own
    /// doc explains why this sim cannot place the fire itself. The driver
    /// (`crate::tick::run_tick_loop_with_weather`) is expected to test each
    /// position with `crate::fire::can_survive` against the *live* world and
    /// write `crate::fire::state_for_placement` only where the cell is air and
    /// survives — this drain hands over candidates, not verified placements,
    /// exactly matching `LightningBolt.spawnFire`'s own "air and canSurvive"
    /// gate at the call site rather than here.
    pub fn take_lightning_fires(&mut self) -> Vec<BlockPos> {
        std::mem::take(&mut self.pending_lightning_fires)
    }

    /// Drains every projectile-vs-block impact recorded since the last call —
    /// see [`ProjectileBlockHit`]'s own doc for why this sim hands the write
    /// to a driver rather than resolving it here. The driver is expected to
    /// read the *live* block at each `pos`, check it is really still
    /// `redstone_target::TARGET`, consult its own `ScheduledTickQueue` for
    /// `has_pending_decay`, and call `redstone_target::apply_hit`.
    pub fn take_projectile_block_hits(&mut self) -> Vec<ProjectileBlockHit> {
        std::mem::take(&mut self.pending_projectile_block_hits)
    }

    /// Every live mob or connected player's position, floored to a
    /// [`BlockPos`] — `findLightningTargetAround`'s
    /// `getEntitiesOfClass(LivingEntity.class, searchBounds, ...)` census,
    /// pre-culling deferred to the caller (`lightning::find_lightning_target_around`
    /// filters to its own search box internally, matching vanilla's own
    /// `getEntitiesOfClass` shape).
    #[must_use]
    pub fn living_entity_positions(&self) -> Vec<BlockPos> {
        self.mobs
            .iter()
            .map(|m| lightning::floor_block_pos(m.position()))
            .chain(
                self.players
                    .iter()
                    .map(|p| lightning::floor_block_pos(p.perception.position)),
            )
            .collect()
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
    /// ([`SimMob::apply_damage`]) and, whenever the hit actually landed, the
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
    /// # Two knockback contributions, chained like vanilla's two calls
    ///
    /// Vanilla applies knockback to a melee target in two independent
    /// `LivingEntity.knockback` calls, not one: `LivingEntity.hurtServer`
    /// calls `dealDefaultKnockback` — a flat `0.4F` applied to **every**
    /// damaging hit regardless of the attacker's own attributes — and,
    /// separately, `Player.attack` calls `causeExtraKnockback` with
    /// `getKnockback(...) + (sprintAttack ? 0.5F : 0.0F)`, which for a
    /// bare-handed, non-enchanted attacker is `0.0` unless sprinting.
    /// `knockback_power` here is that
    /// *second*, caller-supplied bonus (`crate::server::apply_attack`'s
    /// `SPRINT_ATTACK_KNOCKBACK_POWER`, `0.0` for a non-sprinting hit); the
    /// mandatory first `0.4` was previously missing entirely, so a plain
    /// (non-sprinting) punch applied **no knockback at all** — the literal
    /// reported symptom ("mobs dont take knockback if i punch them"), not a
    /// magnitude shortfall.
    ///
    /// Both calls are chained through the same `knockback_impulse` primitive
    /// — the second call's input velocity is the first call's output — which
    /// reproduces vanilla's halving-then-subtracting twice (a single call
    /// with the *summed* power is not equivalent: it halves the pre-hit
    /// velocity only once).
    ///
    /// # Direction
    ///
    /// Both calls use the same horizontal direction: the vector from the
    /// target's position to the attacker's, i.e. `dx = attacker_pos.x -
    /// target_pos.x` — vanilla's own `dealDefaultKnockback`
    /// (`source.getSourcePosition().x() - this.getX()`).
    /// `knockback_impulse` then subtracts that
    /// direction's contribution from velocity, so the target moves *away*
    /// from the attacker. This also substitutes for `causeExtraKnockback`'s
    /// real attacker-facing formula
    /// (`lodestone_physics::knockback::attack_direction`) for the bonus half,
    /// since nothing server-side tracks player rotation yet — a materially
    /// smaller divergence than it sounds, because a melee attack requires the
    /// crosshair to already be on the target, so facing and attacker→target
    /// are nearly always the same vector in practice.
    ///
    /// A previous version of this method computed `dx`/`dz` as
    /// `target_pos - attacker_pos` — backwards, pointing from the attacker
    /// *to* the target rather than from the target *to* the attacker —
    /// which pulled a hit mob toward its attacker instead of pushing it
    /// away.
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
        let (health, velocity, damage_dealt, pack_alert) = {
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
            let was_already_angry = mob.anger.is_some();
            let end_time = now + grudge_ticks(&mut mob.mob);
            mob.anger = Some(Anger {
                end_time,
                target: attacker_pos,
            });
            // Issue #233: zombified-piglin group aggro and wolf pack aggro
            // (`ZombifiedPiglin.alertOthers` /
            // `HurtByTargetGoal(this).setAlertOthers()` — see
            // `alert_species`'s own doc for the box/owner-filter citations).
            // Gated on the grudge being *new*: vanilla fires `alertOthers`
            // from `HurtByTargetGoal.start()`/`customServerAiStep`, i.e. once
            // per acquisition, not once per hit — an already-angry victim
            // re-hit mid-grudge must not re-wake its whole pack every tick.
            let pack_alert = if was_already_angry {
                None
            } else {
                alert_species(mob.entity_type.path()).map(|(box_xz, box_y, need_owner_match)| {
                    (
                        mob.entity_type.clone(),
                        mob.position(),
                        mob.owner_uuid(),
                        need_owner_match,
                        box_xz,
                        box_y,
                    )
                })
            };
            // `LivingEntity.setLastHurtByPlayer`: this is the *player* attack path, so
            // the kill counts as a player kill for the next 100 ticks. Vanilla's
            // `dropExperience` reads exactly this, which is why a mob killed by
            // anything else drops no XP — see `hurt_by_player_until`.
            mob.hurt_by_player_until = Some(now + PLAYER_HURT_EXPERIENCE_TIME);
            if damage_dealt > 0.0 && mob.health() > 0.0 {
                let target_pos = mob.position();
                // Vector from the target to the attacker — see this method's
                // own doc comment for why this is the correct sign (and why
                // the previous `target_pos - attacker_pos` pulled the mob
                // toward its attacker instead of pushing it away).
                let dx = attacker_pos.x - target_pos.x;
                let dz = attacker_pos.z - target_pos.z;
                let v = mob.velocity();
                let jitter = || (1.0, 0.0);
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
                //
                // First call: vanilla's mandatory `dealDefaultKnockback`,
                // applied to every damaging hit regardless of the attacker's
                // own knockback attribute.
                let after_default = lodestone_physics::knockback::knockback_impulse(
                    lodestone_physics::geometry::Vec3d { x: v.x, y: v.y, z: v.z },
                    true, // always the grounded branch — see this method's own doc comment.
                    MELEE_DEFAULT_KNOCKBACK_POWER,
                    dx,
                    dz,
                    mob.knockback_resistance(),
                    jitter,
                );
                // Second call: the attacker-specific bonus (sprint attack,
                // future enchantments), chained onto the first call's result
                // exactly as vanilla's second, independent `knockback` call
                // chains onto the first's mutated `deltaMovement`.
                let new_velocity = if knockback_power > 0.0 {
                    lodestone_physics::knockback::knockback_impulse(
                        after_default,
                        true,
                        knockback_power,
                        dx,
                        dz,
                        mob.knockback_resistance(),
                        jitter,
                    )
                } else {
                    after_default
                };
                mob.apply_knockback(Vec3::new(new_velocity.x, new_velocity.y, new_velocity.z));
            }
            (mob.health(), mob.velocity(), damage_dealt, pack_alert)
        };
        // Issue #233: resolve the group-alert census now that the borrow on
        // `target_id`'s own `SimMob` above has ended. Every same-species mob
        // in the box, not already holding a live grudge (vanilla's
        // `getTarget() == null` guard — a pack member already fighting
        // something is not redirected), gets the *same* deadline and target
        // as the victim; matching the victim's own `end_time` rather than
        // drawing a fresh one is a disclosed simplification (vanilla redraws
        // `PERSISTENT_ANGER_TIME` per alerted mob), not a rule this repo has
        // any evidence against — the alerted mobs still expire independently
        // of the victim once ticked past `end_time`.
        if let Some((species_key, victim_pos, victim_owner, need_owner_match, box_xz, box_y)) =
            pack_alert
        {
            for other in &mut self.mobs {
                if other.id == target_id || other.entity_type != species_key {
                    continue;
                }
                if need_owner_match && other.owner_uuid() != victim_owner {
                    continue;
                }
                if other.anger.is_some() {
                    continue;
                }
                let p = other.position();
                if (p.x - victim_pos.x).abs() > box_xz
                    || (p.z - victim_pos.z).abs() > box_xz
                    || (p.y - victim_pos.y).abs() > box_y
                {
                    continue;
                }
                other.anger = Some(Anger {
                    end_time: now + grudge_ticks(&mut other.mob),
                    target: attacker_pos,
                });
            }
        }
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

    /// Every mob and dropped item in this sim, as the records
    /// [`crate::entity_storage`] persists (issue #303).
    ///
    /// # Why this is not [`snapshots`](Self::snapshots)
    ///
    /// [`EntitySnapshot`] is the *wire* view: it carries an `id` (a per-session
    /// entity id that means nothing across a restart), no health, and no item
    /// lifecycle. A save built from it would come back as full-health mobs and
    /// dropped items that never despawn. This is the disk view, and the two
    /// deliberately do not share a type.
    ///
    /// **Projectiles are excluded.** An arrow in flight has no persisted
    /// identity in this sim (`ProjectileMeta` carries a uuid and a type but the
    /// registry holds no owner, no pickup state and no damage), so writing one
    /// would persist an object we could not faithfully restore. Vanilla does
    /// save them; that is a follow-up, and it is named in `docs/entity-persistence.md`
    /// rather than left to be discovered as a missing mob.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn saved_entities(&self) -> Vec<crate::entity_storage::SavedEntity> {
        let mut out: Vec<crate::entity_storage::SavedEntity> = self
            .mobs
            .iter()
            .map(|mob| crate::entity_storage::SavedEntity {
                id: mob.entity_type.clone(),
                uuid: mob.uuid,
                pos: mob.position(),
                motion: mob.velocity(),
                rotation: mob.rotation(),
                health: Some(mob.health),
                item: None,
                age: None,
                pickup_delay: None,
                extra: Vec::new(),
            })
            .collect();
        for (&id, state) in &self.item_state {
            let lifecycle = self.items.get(id).copied().unwrap_or_default();
            out.push(crate::entity_storage::SavedEntity {
                id: item_entity_type(),
                uuid: state.uuid,
                pos: state.motion.position,
                motion: state.motion.velocity,
                rotation: Rotation::new(0.0, 0.0),
                health: None,
                item: Some((state.item.clone(), lifecycle.count)),
                age: Some(lifecycle.age),
                pickup_delay: Some(lifecycle.pickup_delay),
                extra: Vec::new(),
            });
        }
        out
    }

    /// Puts saved records back into the sim, returning how many were restored.
    ///
    /// A record whose `id` is `minecraft:item` becomes a tracked dropped item;
    /// anything else becomes a mob through [`spawn_species`](Self::spawn_species),
    /// so a restored cow gets the same shape, attributes and A\* budget a freshly
    /// spawned one does — the alternative (a bare position) would restore mobs
    /// that cannot path.
    ///
    /// **The stored uuid is reinstated, not regenerated**, because
    /// [`crate::entity_storage::EntityStorage::save`] clears stale records by
    /// uuid identity: a fresh uuid on load would make the next save unable to
    /// recognise its own entity, and the mob would be duplicated on every
    /// restart.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore_saved(&mut self, entities: &[crate::entity_storage::SavedEntity]) -> usize {
        let mut restored = 0usize;
        for saved in entities {
            if saved.id == item_entity_type() {
                let Some((item, count)) = saved.item.clone() else {
                    // An `Item`-less item entity is vanilla's own "empty stack"
                    // case, which it discards on load too.
                    continue;
                };
                let id = self.spawn_item(
                    item,
                    saved.pos,
                    saved.motion,
                    ItemLifecycle {
                        age: saved.age.unwrap_or(0),
                        pickup_delay: saved.pickup_delay.unwrap_or(0),
                        count: count.max(1),
                        ..ItemLifecycle::default()
                    },
                );
                if let Some(state) = self.item_state.get_mut(&id) {
                    state.uuid = saved.uuid;
                }
                restored += 1;
                continue;
            }
            // Checked **before** spawning, not after: a stored `0.0` is a mob
            // that died in the tick the process was killed, and spawning it to
            // then skip it would leave a zero-health corpse in the sim that
            // nothing sweeps, because the death pass only runs on damage.
            if saved.health.is_some_and(|health| health <= 0.0) {
                continue;
            }
            let mob = self.spawn_species(saved.id.clone(), saved.pos);
            mob.uuid = saved.uuid;
            if let Some(health) = saved.health {
                mob.set_health(health);
            }
            restored += 1;
        }
        restored
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
    /// [`SpawnCandidateSource`] is asked for the group it would spawn there; each
    /// member becomes a real mob through [`spawn_species`](Self::spawn_species),
    /// so it arrives with the species' own body, attributes and vanilla goal set
    /// rather than a placeholder. Nothing here decides *which* mob or *where* —
    /// that is the source's registry/terrain-dependent job.
    ///
    /// The **category is the spawn list's**, not
    /// [`spawn_species`](Self::spawn_species)' hostile/friendly guess: vanilla's
    /// category is a property of the `EntityType` registration, and the biome
    /// list's key is exactly that. It is overridden after the spawn for the same
    /// reason the cap is keyed by it.
    ///
    /// A group is truncated the moment its category reaches the cap, so the cap
    /// can never be exceeded even though the source drew a whole cluster.
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
                for candidate in source.cluster(category, cx, cz) {
                    if !state.can_spawn(category) {
                        break;
                    }
                    let mob = self.spawn_species(candidate.entity_type, candidate.pos);
                    mob.set_category(category)
                        .set_persistent(category.is_persistent());
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

    /// Runs one patrol-spawn tick — the vanilla `PatrolSpawner` port
    /// (`level/levelgen/PatrolSpawner.java`, 92 lines). Meant to be called
    /// once per server tick, mirroring vanilla's `CustomSpawner.tick`: the
    /// internal countdown is decremented every call regardless of whether
    /// anything ends up spawning, so calling this less often than once a
    /// tick would make patrols rarer than vanilla rather than merely
    /// checked less often.
    ///
    /// `world` is the terrain a spawn candidate is checked against, and it
    /// must be a *live, player-following* snapshot — not this sim's own
    /// static `self.world` — because a patrol spawns near a **player**, and
    /// `self.world` is a fixed footprint around wherever `mob_area` was when
    /// the world opened. Feeding `self.world` here would reproduce the exact
    /// bug natural spawning already had and fixed: patrols would work near
    /// spawn and stop working entirely once a player walked away from it.
    /// The caller should hand in the same snapshot `crate::natural_spawn`
    /// already builds each cycle.
    ///
    /// `spawn_patrols` is the game rule of the same name; `is_bright_outside`
    /// is vanilla's `ServerLevel.isBrightOutside()` — day and not thundering
    /// — collapsed to a caller-supplied bool because no weather state crosses
    /// this seam yet.
    ///
    /// Returns the number of pillagers actually spawned this call (`0` on
    /// almost every call — vanilla's own interval is roughly once every
    /// 12000–13200 ticks per world, i.e. every 10–11 minutes).
    ///
    /// # Disclosed, not modelled
    ///
    /// `docs/pillager-patrols.md` has the full account; the summary:
    ///
    /// * **No spectator filter and no village-proximity check.** Neither a
    ///   spectator flag nor a POI/village census exists on this seam.
    /// * **No block-light check** (`checkPatrollingMonsterSpawnRules`'s
    ///   `getBrightness(BLOCK, pos) > 8 ? false : …`). [`ChunkWorld`] carries
    ///   block *identity*, not light — the same limit `natural_spawn`'s
    ///   caller-supplied light cache exists to work around for the mobs that
    ///   need it, which this method does not have access to.
    /// * **`isValidEmptySpawnBlock` is approximated** as "two blocks of open
    ///   air above the surface", with no fluid-state check.
    /// * [`patrol_group_size`] approximates `getCurrentDifficultyAt(pos)
    ///   .getEffectiveDifficulty()`, a continuous formula this crate has no
    ///   moon-phase or accumulated regional-difficulty state to compute, with
    ///   a per-[`Difficulty`]-enum constant.
    pub fn run_patrol_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_patrols: bool,
        is_bright_outside: bool,
        difficulty: Difficulty,
    ) -> usize {
        self.patrol_next_tick -= 1;
        if self.patrol_next_tick > 0 {
            return 0;
        }
        // Vanilla re-arms the countdown *before* any of the gates below can
        // reject the attempt (`this.nextTick = this.nextTick + 12000 +
        // random.nextInt(1200)`, unconditionally, immediately after the `<=
        // 0` check) — so a rejected attempt still waits a full interval
        // before the next one, rather than retrying every tick.
        self.patrol_next_tick += 12_000 + self.patrol_rng.next_int(1_200);
        if !spawn_patrols || !is_bright_outside {
            return 0;
        }
        if self.tick_count < PATROL_TIMELINE_GATE {
            return 0;
        }
        if self.patrol_rng.next_int(5) != 0 {
            return 0;
        }
        if self.players.is_empty() {
            return 0;
        }
        let player_pos = self.players
            [self.patrol_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;

        let sign_x = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let sign_z = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let dx = f64::from(24 + self.patrol_rng.next_int(24)) * sign_x;
        let dz = f64::from(24 + self.patrol_rng.next_int(24)) * sign_z;
        let mut spawn_x = (player_pos.x + dx).floor() as i32;
        let mut spawn_z = (player_pos.z + dz).floor() as i32;

        let group_size = patrol_group_size(difficulty);
        let pillager: ResourceKey = "minecraft:pillager".parse().expect("valid key");
        let mut spawned = 0;
        for i in 0..group_size {
            // `NaturalSpawner.isValidEmptySpawnBlock` + this method's own
            // "not modelled" note: a surface exists and there are two open
            // cells above it. `None`/`false` both mean "no valid cell here".
            let spawn_ok = surface_y(world, spawn_x, spawn_z).filter(|&surface| {
                !world.is_solid(spawn_x, surface + 1, spawn_z)
                    && !world.is_solid(spawn_x, surface + 2, spawn_z)
            });
            if let Some(surface) = spawn_ok {
                let pos = Vec3::new(
                    f64::from(spawn_x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(spawn_z) + 0.5,
                );
                let mob = self.spawn_species(pillager.clone(), pos);
                let id = mob.id;
                mob.set_category(MobCategory::Monster);
                self.get_mut(id)
                    .expect("just spawned")
                    .set_patrolling(true);
                if i == 0 {
                    // `findPatrolTarget`: `-500 + nextInt(1000)` on both
                    // axes, offset from the mob's *own* spawn position.
                    let tx = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let tz = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let leader = self.get_mut(id).expect("just spawned");
                    leader.set_patrol_leader(true);
                    leader.set_patrol_target(Some(Vec3::new(pos.x + tx, pos.y, pos.z + tz)));
                }
                spawned += 1;
            } else if i == 0 {
                // The leader's own spawn attempt failed — vanilla abandons
                // the whole group rather than trying a different member
                // first (`PatrolSpawner.java:44-47`'s `break`).
                break;
            }
            spawn_x += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
            spawn_z += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
        }
        // A follower spawned this same call has no group target until
        // `feed_perception` next runs its patrol census — a one-tick startup
        // lag, not a correctness gap: `LongDistancePatrolGoal::can_use`
        // requires `patrol_target().is_some()`, so it simply does not fire
        // until then.
        spawned
    }

    /// Issue #240: the wandering trader's own spawn cycle — vanilla
    /// `WanderingTraderSpawner.tick`/`spawn`
    /// (`.cache/mc/26.2/src/net/minecraft/world/entity/npc/wanderingtrader/WanderingTraderSpawner.java`).
    /// Same shape as [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle):
    /// a live, player-following `world` snapshot supplied by the caller
    /// (never `self.world`, which is a stale snapshot — see that method's
    /// own doc comment for why), a `spawn_wandering_traders` game-rule flag
    /// the caller reads, and every counter this cycle owns living on `self`
    /// so the driver need only call this once per tick.
    ///
    /// **No persistence**: vanilla stores `tickDelay`/`spawnDelay`/
    /// `spawnChance` in a `WanderingTraderData` saved-data file, so the
    /// cycle survives a server restart. This crate has no save/load for
    /// `MobSim` at all, so every field here resets with a fresh `MobSim` —
    /// a disclosed gap, not a silent one.
    ///
    /// **Simplified from `WanderingTraderSpawner.spawn`**, matching the
    /// gaps `docs/wandering-trader.md` already discloses for
    /// [`spawn_wandering_trader`](Self::spawn_wandering_trader) itself:
    /// no `PoiTypes.MEETING` search (always searches around a random
    /// player's own position), no `BiomeTags.WITHOUT_WANDERING_TRADER_SPAWNS`
    /// exclusion, no `hasEnoughSpace` collision check, and no
    /// `setDespawnDelay`/`setWanderTarget`/`setHomeTo` afterwards (this sim
    /// has no despawn-delay or home-position fields to set them on).
    ///
    /// Returns the trader's entity id on a successful spawn.
    pub fn run_wandering_trader_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_wandering_traders: bool,
    ) -> Option<i32> {
        self.trader_tick_delay -= 1;
        if self.trader_tick_delay > 0 {
            return None;
        }
        self.trader_tick_delay = WANDERING_TRADER_TICK_DELAY;
        if !spawn_wandering_traders {
            return None;
        }
        self.trader_spawn_delay -= WANDERING_TRADER_TICK_DELAY;
        if self.trader_spawn_delay > 0 {
            return None;
        }
        self.trader_spawn_delay = WANDERING_TRADER_SPAWN_DELAY;
        let chance = self.trader_spawn_chance;
        self.trader_spawn_chance = (self.trader_spawn_chance
            + WANDERING_TRADER_SPAWN_CHANCE_INCREASE)
            .min(WANDERING_TRADER_MAX_SPAWN_CHANCE);
        // `random.nextInt(100) <= chanceToSpawn` is the entry condition;
        // missing it draws nothing further and leaves the climbed chance
        // in place for next time.
        if self.trader_rng.next_int(100) > chance {
            return None;
        }
        if self.players.is_empty() {
            // vanilla `spawn`'s `getRandomPlayer() == null` arm returns
            // `true` — a "success" for chance-reset purposes — without
            // drawing further or spawning anything.
            self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
            return None;
        }
        // `spawn`'s own extra one-in-ten gate, drawn only once a player
        // exists — vanilla's short-circuit on `player == null` above skips
        // this draw entirely, which is why the empty-players check has to
        // come first rather than being folded into a single `if`.
        if self.trader_rng.next_int(10) != 0 {
            return None;
        }
        let player_pos = self.players
            [self.trader_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;
        let reference = (player_pos.x.floor() as i32, player_pos.z.floor() as i32);
        // `findSpawnPositionNear`: up to 10 candidates within a 48-block
        // radius, first one with a real surface wins. No `isSpawnPositionOk`
        // check beyond "a column exists here" — see the gaps disclosed
        // above.
        let mut spawn_pos = None;
        for _ in 0..10 {
            let x = reference.0 + self.trader_rng.next_int(96) - 48;
            let z = reference.1 + self.trader_rng.next_int(96) - 48;
            if let Some(surface) = surface_y(world, x, z) {
                spawn_pos = Some(Vec3::new(
                    f64::from(x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(z) + 0.5,
                ));
                break;
            }
        }
        let pos = spawn_pos?;
        let (trader_id, _llamas) = self.spawn_wandering_trader(pos);
        self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
        Some(trader_id)
    }

    /// Whether a death rolls its loot table — the `mob_drops` game rule, handed in
    /// by `crate::tick::run_tick_loop` once a tick (this type is version-free and
    /// holds no world-state handle). Defaults to vanilla's own default, `true`, so a
    /// sim nobody sets it on behaves exactly as before the rule existed.
    pub fn set_mob_drops(&mut self, allowed: bool) {
        self.mob_drops = allowed;
    }

    /// Discards every mob Peaceful forbids — vanilla's `Mob.checkDespawn` guard,
    /// `difficulty == PEACEFUL && !getType().isAllowedInPeaceful()`. Returns how
    /// many were removed.
    ///
    /// Rolls **no** loot: vanilla's peaceful sweep is `discard()`, not a death, so a
    /// player switching to Peaceful does not get a floor covered in rotten flesh.
    ///
    /// **The predicate is the per-type `notInPeaceful` flag
    /// ([`crate::mob_spawn::allowed_in_peaceful`]), not
    /// [`is_hostile_species`].** The two disagree in both directions and the
    /// disagreement is visible: `is_hostile_species` is a 22-name list serving the
    /// *category* question, so it kept a slime, magma cube, silverfish, phantom,
    /// vex, ravager, hoglin or warden alive on Peaceful — and slimes really do
    /// spawn here, because `crate::natural_spawn` models slime chunks. In the other
    /// direction the flag keeps a shulker and a piglin, which vanilla also keeps
    /// and which a `MobCategory.MONSTER` test would delete.
    pub fn remove_monsters(&mut self) -> usize {
        let before = self.mobs.len();
        self.mobs
            .retain(|m| crate::mob_spawn::allowed_in_peaceful(m.entity_type.path()));
        before - self.mobs.len()
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
        let now = self.tick_count;
        // `drops_experience` is `LivingEntity.dropExperience`'s own guard, read here
        // while the mob still exists: a player's hit within the last
        // `PLAYER_HURT_EXPERIENCE_TIME` ticks, and not a baby
        // (`shouldDropExperience()` is `!isBaby()`).
        let dead: Vec<(ResourceKey, Vec3, bool)> = self
            .mobs
            .iter()
            .filter(|m| m.health <= 0.0)
            .map(|m| {
                let by_player = m.hurt_by_player_until.is_some_and(|until| now < until);
                (
                    m.entity_type.clone(),
                    m.position(),
                    by_player && !m.is_baby(),
                )
            })
            .collect();
        if dead.is_empty() {
            return;
        }
        self.mobs.retain(|m| m.health > 0.0);
        for (entity_type, position, drops_experience) in dead {
            self.drop_death_loot(&entity_type, position);
            // Vanilla's `die` calls `dropAllDeathLoot` then `dropExperience`, in that
            // order, so the orbs land after the items.
            if drops_experience {
                self.drop_death_experience(&entity_type, position);
            }
        }
    }

    /// `LivingEntity.dropExperience`: pops this species' reward as orbs at `position`.
    ///
    /// The caller has already applied vanilla's two eligibility tests (see
    /// [`reap_dead`](Self::reap_dead)); this applies the third,
    /// `level.getGameRules().get(GameRules.MOB_DROPS)`, which is the same rule
    /// [`drop_death_loot`](Self::drop_death_loot) honours — so `/gamerule mobDrops
    /// false` suppresses XP as well as items, exactly as vanilla does.
    ///
    /// The reward roll rides [`orb_rng`](Self::orb_rng) rather than a position-seeded
    /// stream: unlike a loot roll, an animal's `1 + nextInt(3)` has no reason to be
    /// reproducible from the death site, and putting it on the orb stream keeps every
    /// orb-related draw in one sequence.
    fn drop_death_experience(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let reward = species::mob_experience_reward(entity_type, &mut self.orb_rng);
        if reward <= 0 {
            return;
        }
        self.award_experience(position, Vec3::new(0.0, 0.0, 0.0), reward);
    }

    /// Rolls `entity_type`'s death loot table and spawns the result at
    /// `position`. See [`reap_dead`](Self::reap_dead) for the vanilla chain.
    ///
    /// Seeded from the tick count and the position, so a death is deterministic
    /// for a given world state without threading a connection's RNG into the sim.
    fn drop_death_loot(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
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
        let mut out: Vec<EntitySnapshot> = self
            .mobs
            .iter()
            .map(|m| {
                let mut snap = m.snapshot();
                // Only `MobSim` can resolve a `LeashHolder` to a wire entity id
                // (a player holder needs `self.players`) — see
                // `resolve_leash_target`'s own doc for the three shapes.
                snap.leash_link = m.leash_holder().and_then(|holder| self.resolve_leash_target(holder));
                snap
            })
            .collect();
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
                    // `AbstractArrow` and friends leave `getAddEntityPacket`'s
                    // data at `0`; only `getAddEntityPacket` overrides carry one,
                    // and no projectile this sim spawns has one.
                    object_data: 0,
                    // A projectile is never a `Leashable`.
                    leash_link: None,
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
                // `ItemEntity` does not override `getAddEntityPacket`; the stack
                // travels as metadata (above), not as object data.
                object_data: 0,
                // A dropped item is never a `Leashable`.
                leash_link: None,
            });
        }
        // `ExperienceOrb`. Iterated in **sorted** id order, like the falling blocks
        // below and unlike the two loops above: an orb's whole visible behaviour is a
        // multi-tick drift toward the player, so a `HashMap` order would reshuffle
        // which of two orbs `EntityStreamer::sync` updates first every tick.
        let mut orb_ids: Vec<i32> = self.orbs.keys().copied().collect();
        orb_ids.sort_unstable();
        for id in orb_ids {
            let Some(orb) = self.orbs.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: orb.uuid,
                entity_type: orb_entity_type(),
                position: orb.motion.position,
                // `ExperienceOrb`'s constructor sets a random `yRot`, which nothing
                // reads: `ExperienceOrbRenderer` billboards the sprite at the camera.
                // Sending a rotation would be sending a value with no consumer.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: orb.motion.velocity,
                // **The field that decides which of the eleven sprite frames draws.**
                // `ExperienceOrb.getIcon` buckets `getValue()` — not `count`, and not
                // linearly — so an orb whose value never reaches the client draws frame
                // 0 (the smallest) whatever it is worth. `defineSynchedData` registers
                // `DATA_VALUE` and nothing else, so metadata is the only channel;
                // there is no object data on `getAddEntityPacket` to carry it.
                //
                // `count` is deliberately *not* sent: vanilla does not synchronise it,
                // and a client that knew it would still draw one sprite.
                metadata: vec![MetadataField::ExperienceOrbValue { value: orb.value }],
                object_data: 0,
                // An experience orb is never a `Leashable`.
                leash_link: None,
            });
        }
        // `FallingBlockEntity`. The **only** producer of a non-zero
        // `object_data` in this crate: `getAddEntityPacket` passes
        // `Block.getId(this.getBlockState())`, and that field is the sole channel
        // by which a client learns which block is falling (see
        // `TrackedFallingBlock`'s own doc for why metadata cannot carry it).
        //
        // Iterated in **sorted** id order, unlike the two loops above. A falling
        // block's whole point is a smooth multi-tick animation, and
        // `EntityStreamer::sync` walks this list in order to emit spawns and
        // updates; a `HashMap` order would reshuffle which of two simultaneous
        // falls is updated first from tick to tick for no reason.
        let mut falling_ids: Vec<i32> = self.falling_blocks.keys().copied().collect();
        falling_ids.sort_unstable();
        for id in falling_ids {
            let Some(tracked) = self.falling_blocks.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: tracked.uuid,
                entity_type: falling_block_entity_type(),
                position: tracked.motion.position,
                // `FallingBlockEntity` never rotates: `fall` sets no `yRot`/`xRot`
                // and nothing writes them afterwards. A falling block that visibly
                // spun would be a *more* interesting animation and a wrong one.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, tracked.motion.velocity_y, 0.0),
                // `defineSynchedData` registers `DATA_START_POS` alone, and that
                // accessor's value is the entity's own spawn cell — which the
                // client recovers from the `ADD_ENTITY` position in
                // `recreateFromPacket`. So there is genuinely nothing to send.
                metadata: Vec::new(),
                // `unwrap_or(0)` rather than skipping the entity: an unresolvable
                // state is a data-table gap, and streaming the entity with a wrong
                // texture is a visible bug a reader can chase, while silently
                // dropping it reproduces the original teleport with no trace. The
                // three states `crate::gravity_tick::is_gravity_block` accepts all
                // resolve.
                object_data: block_states::state_id(&tracked.state).unwrap_or(0) as i32,
                // A falling block is never a `Leashable`.
                leash_link: None,
            });
        }
        // Vehicles — the boats. Sorted ids, like the two loops above and for the
        // same reason: a boat's whole point is a smooth multi-tick glide, so a
        // `HashMap` order would reshuffle which of two boats `EntityStreamer::sync`
        // updates first every tick.
        let mut vehicle_ids: Vec<i32> = self.vehicles.keys().copied().collect();
        vehicle_ids.sort_unstable();
        for id in vehicle_ids {
            let Some(vehicle) = self.vehicles.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: vehicle.uuid,
                entity_type: vehicle.entity_type.clone(),
                position: Vec3::new(
                    vehicle.motion.position.x,
                    vehicle.motion.position.y,
                    vehicle.motion.position.z,
                ),
                // **The yaw is the point.** A boat's hull is the only thing that
                // shows which way it faces, and `BoatItem.use` sets it from the
                // placing player — a boat streamed at yaw 0 always points south
                // however you placed it. The pitch stays 0: `AbstractBoat` never
                // writes `xRot`.
                rotation: Rotation::new(vehicle.yaw, 0.0),
                // `AbstractBoat` is not a `LivingEntity`, so there is no separate
                // head rotation to send; `ClientboundRotateHeadPacket` is only sent
                // for entities that have one.
                head_yaw: 0.0,
                velocity: Vec3::new(
                    vehicle.motion.velocity.x,
                    vehicle.motion.velocity.y,
                    vehicle.motion.velocity.z,
                ),
                // `AbstractBoat.defineSynchedData` registers `DATA_ID_PADDLE_LEFT`,
                // `DATA_ID_PADDLE_RIGHT` and `DATA_ID_BUBBLE_TIME`, on top of
                // `VehicleEntity`'s hurt/hurtdir/damage triple. **None is sent
                // here, and the omission is deliberate rather than pending.**
                //
                // The paddle pair is the only one with a visible consequence, and
                // its wire index is shared: index 18 has 37 claimants in the
                // committed `EntityDataIndexOracle` dump, four of them `BYTE`, so a
                // producer must know the species and no census column separates
                // them. Since the rider's own client animates the paddles from its
                // *local* `ClientAction::PaddleBoat` simulation, sending them buys
                // nothing in singleplayer — the case that exists today — and a
                // wrongly-keyed field draws another entity's state. When a second
                // player needs to see someone else rowing, add a `MetadataField`
                // variant per accessor and check the guard against the dump, exactly
                // as `MetadataField::Item` and `ExperienceOrbValue` did.
                metadata: Vec::new(),
                // `AbstractBoat` does not override `getAddEntityPacket`.
                object_data: 0,
                // A boat is never a `Leashable`.
                leash_link: None,
            });
        }
        // Primed TNT. Sorted ids, for the same reason every other sidecar loop
        // in this method is: a stable per-tick update order for
        // `EntityStreamer::sync`.
        let mut tnt_ids: Vec<i32> = self.tnt.keys().copied().collect();
        tnt_ids.sort_unstable();
        for id in tnt_ids {
            let Some(t) = self.tnt.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: t.uuid,
                entity_type: tnt::tnt_entity_type(),
                position: Vec3::new(t.motion.position.x, t.motion.position.y, t.motion.position.z),
                // `PrimedTnt` never rotates — `Entity`'s base `yRot`/`xRot`
                // stay `0.0` for the whole of its short life.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(t.motion.velocity.x, t.motion.velocity.y, t.motion.velocity.z),
                // `PrimedTnt.DATA_FUSE_ID` — see `MetadataField::TntFuse`'s own
                // doc for why this is index 8's fifth `INT` claimant and must be
                // class-guarded on decode.
                metadata: vec![MetadataField::TntFuse(t.fuse)],
                // `PrimedTnt` does not override `getAddEntityPacket`.
                object_data: 0,
                // Never a `Leashable`.
                leash_link: None,
            });
        }
        // Minecarts. Sorted ids, for the same reason every other sidecar loop
        // in this method is.
        let mut minecart_ids: Vec<i32> = self.minecarts.keys().copied().collect();
        minecart_ids.sort_unstable();
        for id in minecart_ids {
            let Some(cart) = self.minecarts.get(&id) else {
                continue;
            };
            // `MinecartFurnace.DATA_ID_FUEL` — index 13, shared with
            // `MinecartCommandBlock.DATA_ID_COMMAND_NAME` (a `STRING`) under a
            // different serializer; this is the only producer of a
            // `MinecartFuel` field and it only ever fires from the furnace
            // loop, so the two can never collide the way `MetadataField::Item`'s
            // own doc describes for index 8.
            let metadata = if cart.kind.is_furnace() {
                vec![MetadataField::MinecartFuel(cart.fuel > 0)]
            } else {
                Vec::new()
            };
            out.push(EntitySnapshot {
                id,
                uuid: cart.uuid,
                entity_type: cart.kind.entity_type(),
                position: Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z),
                rotation: Rotation::new(cart.yaw, 0.0),
                // `AbstractMinecart` is not a `LivingEntity`; no separate head
                // rotation packet is ever sent for one.
                head_yaw: 0.0,
                velocity: Vec3::new(cart.motion.velocity.x, cart.motion.velocity.y, cart.motion.velocity.z),
                metadata,
                // `AbstractMinecart` does not override `getAddEntityPacket`.
                object_data: 0,
                // Never a `Leashable`.
                leash_link: None,
            });
        }
        // `LightningBolt` (issue #269). Sorted ids for the same reason the two
        // loops above are: a bolt is short-lived but real entities, and a
        // `HashMap` order would reshuffle which of two simultaneous strikes
        // `EntityStreamer::sync` updates first.
        //
        // **Empty metadata is correct, not an omission**: `LightningBolt`
        // overrides `defineSynchedData` with an empty body — it registers no
        // accessor at all (`.cache/mc/26.2/src/net/minecraft/world/entity/LightningBolt.java`),
        // so there is nothing to send.
        let mut bolt_ids: Vec<i32> = self.lightning_bolts.keys().copied().collect();
        bolt_ids.sort_unstable();
        for id in bolt_ids {
            let Some(bolt) = self.lightning_bolts.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: bolt.uuid,
                entity_type: lightning::lightning_bolt_entity_type(),
                position: bolt.pos,
                // A bolt never rotates or moves once struck.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                metadata: Vec::new(),
                // `LightningBolt` does not override `getAddEntityPacket`.
                object_data: 0,
                // Never a `Leashable`.
                leash_link: None,
            });
        }
        self.push_dragon_snapshots(&mut out);
        self.push_end_crystal_snapshots(&mut out);
        self.push_wither_snapshots(&mut out);
        // Issue #257: live fishing bobbers.
        self.fishing_bobber_snapshots(&mut out);
        // Issue #241: live raiders spawned by an active raid stream through
        // the ordinary mob loop at the top of this function — `raid.rs`
        // spawns them with `spawn_species`, exactly as a patrol does — so
        // there is nothing to append here.
        out
    }
}

/// `EntityTypes.FALLING_BLOCK`'s registry key — the falling-block twin of
/// [`item_entity_type`], and parsed per call for the same reason that one is: a
/// falling block is a rare, short-lived entity, and the parse is cheaper than the
/// `OnceLock` clone it would replace.
fn falling_block_entity_type() -> ResourceKey {
    crate::gravity_tick::FALLING_BLOCK_ENTITY_TYPE
        .parse()
        .expect("`minecraft:falling_block` is a valid resource key")
}

/// How far below the world's floor an item may sink before it is discarded —
/// vanilla's `Entity.checkBelowWorld` threshold (`Entity.java`'s
/// `this.getY() < (double)(this.level().getMinY() - 64)`).
const VOID_DESPAWN_DEPTH: f64 = 64.0;

/// The dropped item's hitbox — vanilla `ItemEntity`'s `EntityType.ITEM`
/// dimensions, `0.25 × 0.25`, with **no** auto-step.
///
/// `step_height` is `0.0` rather than the `0.6` an ordinary mob resolves from its
/// `STEP_HEIGHT` attribute: `ItemEntity` never overrides `maxUpStep()`, and
/// `Entity`'s base returns `0.0`. Getting this wrong would let a dropped item climb
/// a slab it slid into, which is the sort of thing that looks like a physics
/// improvement in a screenshot.
const ITEM_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.25, 0.25, 0.0);

/// A [`CollisionView`] over a caller-supplied block-state oracle, serving the real
/// per-block-state collision shapes.
///
/// # Why this exists rather than a `Fn(i32, i32, i32) -> bool`
///
/// The item pass used to take exactly that: one boolean per cell, `!is_air_or_fluid`,
/// with an item resolved as a **point** and its rest height hardcoded to `by + 1`.
/// Read out of `lodestone_data`'s generated shape table rather than predicted, that
/// is wrong for most of the blocks a player actually drops things onto:
///
/// | block state | true collision top | the boolean's answer |
/// |---|---|---|
/// | `short_grass`, `tall_grass`, `snow[layers=1]` | **0.0** — no collision at all | solid, so the item rests a full block *above* the ground |
/// | `oak_slab[type=bottom]` | 0.5 | 1.0 |
/// | `enchanting_table` | 0.75 | 1.0 |
/// | `soul_sand`, `mud`, `chest` | 0.875 | 1.0 |
/// | `dirt_path` | 0.9375 | 1.0 |
/// | `oak_fence` | **1.5** — uncapped | 1.0, i.e. *too low* |
///
/// The grass row is the one with the visible symptom, and it is the common case:
/// almost any grassy surface has a plant on it, so almost every dropped item floated.
/// The fence row is worth keeping because it fails in the opposite direction, so a
/// gate that only ever checked "not too high" would miss it.
///
/// # Cost, stated because it is strictly more work per item
///
/// The boolean was one map lookup per cell. This is a `String` from the oracle, a
/// name→id lookup, and an O(1) rodata index — then `collide` sweeps the cells the
/// item's expanded box spans rather than probing one column. `probe_count` is
/// incremented per cell so the cost is a **counter** a gate can assert on rather
/// than a duration, and `items_settled_probe_count` exposes it.
struct ItemCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
    probe_count: std::cell::Cell<u64>,
}

impl CollisionView for ItemCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        self.probe_count.set(self.probe_count.get() + 1);
        let name = (self.block_state)(x, y, z);
        // **`block_state_id` then `block_states::state_id`, and emphatically NOT
        // `block_state_id_or_default`.** That helper resolves a bare name to the
        // block's *lowest* state id, and its own doc comment says in as many words
        // that it "is not a substitute for `block_state_id` where the properties
        // matter (collision shapes, path types)". It was used here for one iteration
        // and a bare `minecraft:oak_slab` resolved to a full cube — the lowest id is
        // not `type=bottom` — so the item rested at the top of the cell and the fix
        // reproduced the very bug it removes. `block_states::state_id` consults
        // `span.default`, which is vanilla's real `defaultBlockState()`.
        //
        // The exact-string map is tried first because it is O(1) and because a
        // `ChunkSource` normally hands back a full canonical state; `state_id` scans
        // the block's own span, which is short but not free.
        let Some(id) = block_state_id(&name).or_else(|| block_states::state_id(&name)) else {
            return;
        };
        let Some(shape) = collision_shapes::collision_boxes(id) else {
            return;
        };
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(lodestone_physics::Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }
}

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
/// Settles one item against a solidity oracle.
///
/// # Why this takes a closure and not the sim's own `ChunkWorld`
///
/// It used to read [`ChunkWorld::is_solid`] directly, and that is why dropped
/// items phased through the ground everywhere except a small square around
/// spawn. The sim's `ChunkWorld` is a **static snapshot** of `mob_area` — 7×7
/// columns, taken once by `MobHandle::reseed` when the world opens (see that
/// method's own doc, which names widening it as a deliberate scope cut). Outside
/// those columns `is_solid` is `false` for *every* cell, because the column is
/// simply absent, so an item fell forever and was discarded at `min_y - 64`.
/// Inside them it answered from unedited worldgen terrain, so a block the player
/// had placed did not stop an item and one they had mined still did.
///
/// A snapshot cannot be the oracle for this: settling has to see the world as it
/// is *now*, at whatever coordinates the player is actually standing. The tick
/// loop is the one place that holds the live `ChunkSource`, so it supplies the
/// answer and the sim asks — see [`MobSim::tick_with_terrain`]. `tick` keeps the
/// snapshot as its oracle so hermetic callers are unchanged.
fn settle_item(view: &dyn CollisionView, motion: &mut ItemMotion, before: Vec3) {
    settle_entity(view, ITEM_DIMENSIONS, motion, before);
}

/// [`settle_item`] with the hitbox as a parameter, so an experience orb
/// (`0.5 × 0.5`) resolves against the same swept collision an item (`0.25 × 0.25`)
/// does.
///
/// Split out rather than copied: the *geometry* differs between the two entities and
/// nothing else does, and a second copy of the restitution rules is how one of them
/// ends up with the pre-swept-collision point test again. `motion` is the
/// position/velocity/`on_ground` triple only — the caller has already applied whatever
/// per-entity gravity and drag its own `tick` uses.
fn settle_entity(
    view: &dyn CollisionView,
    dimensions: EntityDimensions,
    motion: &mut ItemMotion,
    before: Vec3,
) {
    // The movement the caller's own tick just applied by translating outright. It is
    // recovered rather than recomputed so this cannot drift from that arithmetic
    // (gravity, then translate, then drag, then the landing bounce).
    let attempted = Vec3d::new(
        motion.position.x - before.x,
        motion.position.y - before.y,
        motion.position.z - before.z,
    );

    // **The ordering is deliberately identical to what it replaced**: gravity and
    // drag still happen inside `ItemMotion::tick`, before the collision, and this
    // still runs after. Vanilla's `ItemEntity.tick` collides *between* them, so its
    // friction reads the post-move `onGround`. Matching that is a separate change to
    // a crate outside this one; keeping the order fixed here means the only thing
    // this commit alters is the **geometry**, which is what makes the existing
    // settling gates still meaningful rather than merely still green.
    let bb = dimensions.bounding_box(Vec3d::new(before.x, before.y, before.z));
    let resolved = collide(view, attempted, bb, motion.on_ground, dimensions.step_height);

    motion.position = Vec3::new(
        before.x + resolved.x,
        before.y + resolved.y,
        before.z + resolved.z,
    );

    // `Entity.move`'s `restituteMovementAfterCollisions`: zero each component the
    // sweep could not fully apply. Horizontal is included now — the old point test
    // could not see a wall at all, and its doc comment argued that was safe because
    // an item's horizontal velocity decays before it can cross one. That argument
    // holds for *slow* items and not for a thrown one, and it is free to get right
    // here because `collide` resolves all three axes in one call.
    if (resolved.x - attempted.x).abs() > f64::EPSILON {
        motion.velocity.x = 0.0;
    }
    if (resolved.z - attempted.z).abs() > f64::EPSILON {
        motion.velocity.z = 0.0;
    }
    if (resolved.y - attempted.y).abs() > f64::EPSILON {
        motion.velocity.y = 0.0;
    }

    // Vanilla's own rule (`Entity.setOnGroundWithMovement`): grounded when the sweep
    // ate downward movement. This replaces a point probe one epsilon below the
    // bottom face, which is why `ITEM_SUPPORT_EPSILON` is gone: there is no longer a
    // boundary-straddling floor() to defend against, and an item resting on a slab
    // has no block boundary under its feet to probe in the first place.
    motion.on_ground = attempted.y < 0.0 && (resolved.y - attempted.y).abs() > f64::EPSILON;
}

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

/// Default seed for [`MobSim`]'s tame-roll stream. Arbitrary and fixed, exactly
/// like [`ORB_BEHAVIOR_SEED`] — what matters is that it is a *separate* stream,
/// so a tame attempt cannot shift which roll a spawn or a despawn pass sees.
/// Replace it per test with [`MobSim::set_tame_rng`].
const TAME_ROLL_SEED: u64 = 0x5441_4d45_5f52_4f4c;

/// Default seed for the breeding experience-orb stream. See
/// [`TAME_ROLL_SEED`] for why it is separate.
const BREED_XP_SEED: u64 = 0x4252_4545_445f_5850;

/// Default seed for [`MobSim::patrol_rng`]. See [`TAME_ROLL_SEED`] for why it
/// is separate.
const PATROL_SPAWN_SEED: u64 = 0x5041_5452_4f4c_5f52;

/// The `early_game.json` timeline's `gameplay/can_pillager_patrol_spawn` gate,
/// transcribed as a plain tick count rather than read from a general timeline
/// engine — this crate has no `EnvironmentAttributes`/timeline reader at all,
/// and building one is out of scope for one boolean keyframe.
/// `.cache/mc/26.2/src/data/minecraft/timeline/early_game.json`'s track has
/// exactly two keyframes: `false` at tick `0`, `true` at tick `120000`, and
/// no in-between ramp, so a single threshold constant reproduces it exactly —
/// unlike [`patrol_group_size`], which approximates a genuinely continuous
/// vanilla formula.
const PATROL_TIMELINE_GATE: u64 = 120_000;

/// Vanilla `WanderingTraderSpawner`'s own constants —
/// `DEFAULT_TICK_DELAY`/`DEFAULT_SPAWN_DELAY`/`MIN_SPAWN_CHANCE`/
/// `MAX_SPAWN_CHANCE`/`SPAWN_CHANCE_INCREASE`.
const WANDERING_TRADER_TICK_DELAY: i32 = 1200;
const WANDERING_TRADER_SPAWN_DELAY: i32 = 24_000;
const WANDERING_TRADER_MIN_SPAWN_CHANCE: i32 = 25;
const WANDERING_TRADER_MAX_SPAWN_CHANCE: i32 = 75;
const WANDERING_TRADER_SPAWN_CHANCE_INCREASE: i32 = 25;

/// Default seed for [`MobSim::trader_rng`]. See [`TAME_ROLL_SEED`] for why
/// it is separate.
const WANDERING_TRADER_SPAWN_SEED: u64 = 0x5452_4144_455f_524e;

/// The entity-type key every experience orb streams as.
///
/// Named rather than numeric for [`item_entity_type`]'s reason, which that function's
/// doc records with the measured consequence: `entity_type_id(name).unwrap_or(0)` on
/// the encode side silently turns a wrong key into `minecraft:acacia_boat`.
fn orb_entity_type() -> ResourceKey {
    "minecraft:experience_orb"
        .parse()
        .expect("`minecraft:experience_orb` is a valid resource key")
}

/// Whether `a` and `b` are within `reach` on **every** axis — an AABB-overlap test
/// stated as a per-axis comparison rather than a radius.
///
/// The distinction is the one `merge_neighbouring_items` records: vanilla's merge
/// searches are box intersections, and a Euclidean radius accepts a diagonal pair the
/// box rejects.
fn within_box(a: Vec3, b: Vec3, reach: f64) -> bool {
    (a.x - b.x).abs() < reach && (a.y - b.y).abs() < reach && (a.z - b.z).abs() < reach
}

/// Vanilla `Player.getEyeHeight()` for a standing player — `EntityDimensions`'
/// `eyeHeight` for `EntityType.PLAYER`, `1.62`.
///
/// Used only for `followNearbyPlayer`'s aim point, which is *half* this above the
/// player's feet.
const PLAYER_EYE_HEIGHT: f64 = 1.62;

/// Squared horizontal+vertical distance between two positions (vanilla
/// `distanceToSqr`).
fn dist_sqr(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

/// Vanilla `Entity::push(Entity)`'s horizontal impulse pair for two
/// overlapping entities at `p_i`/`p_j` — see [`MobSim::push_entities`]'s own
/// doc comment for the overlap test this assumes has already passed.
///
/// Delegates the actual formula to
/// [`lodestone_physics::pair_push_vector`] rather than re-transcribing it: that
/// function already carries `docs/entity-push.md`'s full citation (the
/// Chebyshev-not-Euclidean normaliser, the widened `0.01f`/`0.05f` literals,
/// the `NaN`-rejecting `!(dd >= …)` form) and 150 passing tests including
/// golden traces against an independent Python oracle. Keeping one
/// implementation in the crate that already proved it, rather than a second
/// hand-rolled copy here, is what keeps the two from silently drifting apart.
///
/// Returns `None` when the pair is not within `touch` blocks horizontally
/// (this port's overlap test — see the doc comment on the caller for how it
/// narrows vanilla's real AABB intersection) or when `pair_push_vector`'s own
/// dead zone rejects a near-coincident pair. Otherwise returns
/// `(impulse_for_p_i, impulse_for_p_j)`.
fn push_impulse(p_i: Vec3, p_j: Vec3, touch: f64) -> Option<(Vec3, Vec3)> {
    let overlap_dx = p_i.x - p_j.x;
    let overlap_dz = p_i.z - p_j.z;
    if (overlap_dx * overlap_dx + overlap_dz * overlap_dz).sqrt() > touch {
        return None;
    }
    let to_physics = |v: Vec3| lodestone_physics::Vec3d { x: v.x, y: v.y, z: v.z };
    // `pair_push_vector(self, other)` returns the vector FROM `self` TOWARD
    // `other`; vanilla's own `this.push(-xa,0,-za)` / `entity.push(xa,0,za)`
    // negates it for the near side and keeps it for the far side — see that
    // function's own doc comment for why the caller, not the function, does
    // the negation.
    let v = lodestone_physics::pair_push_vector(to_physics(p_i), to_physics(p_j))?;
    Some((Vec3::new(-v.x, 0.0, -v.z), Vec3::new(v.x, 0.0, v.z)))
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

/// The [`SimMob::patrol_target`] of the nearest **other**, patrol-*leading*
/// mob within [`PATROL_COMPANION_RANGE`] blocks of `from`, if any.
///
/// A `nearest_by`-shaped query that cannot reuse [`nearest_by`] itself: the
/// distance test there is against the *same* field the function returns, but
/// here the distance test is against a candidate's **position** (vanilla's
/// `getBoundingBox().inflate(16.0)`) while the value a follower actually wants
/// back is that candidate's **patrol target** — a different field. See
/// [`MobController::patrol_group_target`](lodestone_entity::ai::MobController::patrol_group_target)
/// for why a follower needs this at all rather than running its own census.
fn nearest_patrol_leader_target(mobs: &[SimMob<'_>], from: Vec3, exclude_id: i32) -> Option<Vec3> {
    mobs.iter()
        .filter(|m| m.id != exclude_id && m.is_patrol_leader())
        .filter(|m| dist_sqr(m.position(), from) <= PATROL_COMPANION_RANGE * PATROL_COMPANION_RANGE)
        .min_by(|a, b| dist_sqr(a.position(), from).total_cmp(&dist_sqr(b.position(), from)))
        .and_then(SimMob::patrol_target)
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
pub struct LiveMobSource(
    Arc<Mutex<Vec<EntitySnapshot>>>,
    /// The dragon fight's boss bars, published the same way `.0` is — see
    /// [`publish_boss_bars`](Self::publish_boss_bars). A second field rather
    /// than folding into `.0` because a boss bar is not an entity and has no
    /// `EntitySnapshot` shape to borrow.
    Arc<Mutex<Vec<crate::protocol::BossBarSnapshot>>>,
);

impl EntitySource for LiveMobSource {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0
            .lock()
            .expect("live mob snapshot lock poisoned")
            .clone()
    }

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.1
            .lock()
            .expect("live mob boss-bar lock poisoned")
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

    /// Replaces the published boss-bar set — the [`boss_bars`](EntitySource::boss_bars)
    /// twin of [`publish`](Self::publish), called from the same tick-loop
    /// call site right after it (see `crate::tick::run_tick_loop`).
    pub(crate) fn publish_boss_bars(&self, bars: Vec<crate::protocol::BossBarSnapshot>) {
        *self.1.lock().expect("live mob boss-bar lock poisoned") = bars;
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
    pub fn reseed(&self, mut world: ChunkWorld, center_x: i32, center_z: i32, mob_count: usize) {
        // Issue #518 part 2/4: drained while `world` is still an owned local,
        // before it leaks to `'static` below. Non-empty only the first time
        // these chunks are ever generated — see `ChunkWorld`'s own field doc
        // (`pending_generation_spawns`) for why that is what keeps a fresh
        // world's `SPAWN`-stage animals from duplicating across a restart: a
        // reload of an existing world loads these same chunks from disk, which
        // never populates this list.
        let pending_generation_spawns = world.take_pending_generation_spawns();
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
            // Issue #518: place the `SPAWN` stage's proposed animals as real
            // mobs, re-validated against the real per-species placement rule
            // and this world's own light through the exact gate the
            // tick-driven cycle uses — see
            // `NaturalSpawner::validate_generation_spawns`'s doc for why this
            // reuses rather than re-implements it.
            if !pending_generation_spawns.is_empty() {
                let mut spawner = crate::natural_spawn::NaturalSpawner::new(
                    crate::worldgen_data::bundled_biome_spawners().clone(),
                    0,
                )
                .with_world_seed(crate::worldgen_data::active_world_seed());
                spawner.begin_cycle(std::sync::Arc::new(world.clone()), 0, Vec::new());
                for candidate in spawner.validate_generation_spawns(pending_generation_spawns) {
                    let mob = sim.spawn_species(candidate.entity_type, candidate.pos);
                    mob.set_category(MobCategory::Creature)
                        .set_persistent(MobCategory::Creature.is_persistent());
                }
            }
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

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.with(|sim| sim.boss_bars())
    }
}

/// The highest solid-block Y at `(x, z)` within `world`'s loaded vertical
/// range, or `None` if the whole column reads air (or is unloaded) — the
/// ground a freshly seeded mob should stand on. A linear scan from the top
/// down; called only where a mob is placed rather than every tick — at seed
/// time, and from [`MobSim::run_patrol_spawn_cycle`], which itself only
/// reaches this on the rare tick a patrol attempt actually fires — so this is
/// not a hot path either way.
fn surface_y(world: &ChunkWorld, x: i32, z: i32) -> Option<i32> {
    let top = world.min_y + world.height - 1;
    (world.min_y..=top).rev().find(|&y| world.is_solid(x, y, z))
}

/// Approximates `AbstractRaid`-free `getCurrentDifficultyAt(pos)
/// .getEffectiveDifficulty()` for [`MobSim::run_patrol_spawn_cycle`]'s group
/// size, `(int) Math.ceil(effectiveDifficulty) + 1`
/// (`level/levelgen/PatrolSpawner.java:40`).
///
/// Vanilla's effective difficulty is a continuous value accumulated per
/// region over real playtime plus the current moon phase
/// (`LocalDifficulty`), roughly `0.75` (fresh Peaceful/Easy world) up to
/// `6.75` (long-played Hard, full moon). This crate tracks neither the
/// accumulation nor the moon phase, so each [`Difficulty`] enum value stands
/// in for a fixed point roughly in the middle of its own real range —
/// disclosed in [`MobSim::run_patrol_spawn_cycle`]'s own doc comment, and
/// picked so `ceil(value) + 1` lands on a group size vanilla actually
/// produces at that difficulty rather than an edge value.
fn patrol_group_size(difficulty: Difficulty) -> i32 {
    let effective: f64 = match difficulty {
        Difficulty::Peaceful => 0.0,
        Difficulty::Easy => 1.0,
        Difficulty::Normal => 2.0,
        Difficulty::Hard => 3.0,
    };
    effective.ceil() as i32 + 1
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
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        for _ in 0..ticks {
            sim.tick();
            if sim.get(id).expect("alive").attack_target().is_some() {
                return true;
            }
        }
        false
    }

    /// `push_impulse`'s exact vanilla formula, at an off-axis input chosen so
    /// two plausible implementations diverge: Chebyshev normalisation
    /// (`sqrt(max(|dx|, |dz|))`, what `Entity::push` actually does) against
    /// the more obvious Euclidean one (`sqrt(dx² + dz²)`, what a port that
    /// "corrected" the formula would produce). `dx=0.3, dz=0.4` was chosen
    /// precisely because `max(0.3, 0.4) = 0.4 != dx² + dz² = 0.25`, so the
    /// two hypotheses give different numbers and this input can actually
    /// tell them apart — a symmetric or on-axis pair could not.
    #[test]
    fn push_impulse_matches_a_hand_computed_off_axis_example_not_the_euclidean_alternative() {
        let p_i = Vec3::new(0.0, 0.0, 0.0);
        let p_j = Vec3::new(0.3, 0.0, 0.4);

        // Hand-computed from `Entity::push`'s own arithmetic, independent of
        // `push_impulse`'s implementation:
        //   dd = max(0.3, 0.4) = 0.4; dd = sqrt(0.4) = 0.632455532...
        //   xa = 0.3 / dd = 0.474341649...; za = 0.4 / dd = 0.632455532...
        //   pow = min(1.0, 1.0 / dd) = 1.0 (1/dd ≈ 1.581 > 1)
        //   xa *= 0.05 = 0.023717082...; za *= 0.05 = 0.031622777...
        let dd = 0.4f64.sqrt();
        let expected_xa = (0.3 / dd) * 0.05;
        let expected_za = (0.4 / dd) * 0.05;

        // The wrong hypothesis, evaluated first: Euclidean normalisation
        // (dist = sqrt(0.3² + 0.4²) = 0.5) gives a different pair of numbers.
        let euclid_dist = (0.3f64 * 0.3 + 0.4 * 0.4).sqrt();
        let wrong_xa = (0.3 / euclid_dist) * 0.05;
        let wrong_za = (0.4 / euclid_dist) * 0.05;
        assert!(
            (expected_xa - wrong_xa).abs() > 1.0e-4 && (expected_za - wrong_za).abs() > 1.0e-4,
            "precondition: the chosen input must actually separate the two \
             hypotheses, got chebyshev=({expected_xa}, {expected_za}) \
             euclidean=({wrong_xa}, {wrong_za})"
        );

        let (impulse_i, impulse_j) =
            push_impulse(p_i, p_j, 1.0).expect("well within the touch threshold");
        assert!(
            (impulse_i.x - -expected_xa).abs() < 1.0e-9 && (impulse_i.z - -expected_za).abs() < 1.0e-9,
            "p_i must be pushed away from p_j by the Chebyshev-derived amount, \
             expected ({}, {}), got ({}, {})",
            -expected_xa,
            -expected_za,
            impulse_i.x,
            impulse_i.z
        );
        assert!(
            (impulse_j.x - expected_xa).abs() < 1.0e-9 && (impulse_j.z - expected_za).abs() < 1.0e-9,
            "p_j must be pushed away from p_i by the same magnitude, opposite \
             sign, expected ({expected_xa}, {expected_za}), got ({}, {})",
            impulse_j.x,
            impulse_j.z
        );
        assert!(
            (impulse_i.x - wrong_xa).abs() > 1.0e-4,
            "the Euclidean hypothesis must NOT match — if it does, the \
             normalisation silently changed to the wrong formula"
        );
    }

    /// The control an absence assertion needs: a pair separated just beyond
    /// the touch threshold must produce no impulse at all, proving the
    /// detector (the overlap test) actually fires rather than being
    /// unconditionally permissive.
    #[test]
    fn push_impulse_is_none_just_outside_the_touch_threshold() {
        let p_i = Vec3::new(0.0, 0.0, 0.0);
        let p_j = Vec3::new(0.61, 0.0, 0.0);
        assert!(
            push_impulse(p_i, p_j, 0.6).is_none(),
            "0.61 blocks apart must not touch at a 0.6 threshold"
        );
        // And the positive control, one hair inside: proves 0.6 is not simply
        // always None.
        let p_k = Vec3::new(0.59, 0.0, 0.0);
        assert!(
            push_impulse(p_i, p_k, 0.6).is_some(),
            "0.59 blocks apart must touch at a 0.6 threshold"
        );
    }

    /// Wires `push_impulse` into the real per-tick sim: two overlapping mobs
    /// must actually separate over real ticks, and a third mob spawned well
    /// outside touch range is the control that must not move at all — an
    /// absence assertion (mob-mob pushing is un-wired) needs a detector
    /// proven to fire, which the near pair provides.
    #[test]
    fn overlapping_mobs_separate_over_real_ticks_and_a_distant_one_does_not_move() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
        let near_a = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let near_b = sim.spawn_species(key.clone(), Vec3::new(0.3, 0.0, 0.0)).id();
        let far = sim.spawn_species(key, Vec3::new(50.0, 0.0, 0.0)).id();

        for _ in 0..20 {
            sim.tick();
        }

        let gap = (sim.get(near_a).expect("alive").position()
            - sim.get(near_b).expect("alive").position())
        .x
        .abs();
        assert!(
            gap > 0.3,
            "two overlapping pigs must separate over 20 ticks of pushing, \
             gap only grew to {gap}"
        );
        assert_eq!(
            sim.get(far).expect("alive").position(),
            Vec3::new(50.0, 0.0, 0.0),
            "control: a pig with nothing nearby must not be displaced by the \
             push pass at all"
        );
    }

    /// The primary reported symptom ("I can't push entities like pigs"): a
    /// player walking into a mob must shove it out of the way. Player recoil
    /// is deliberately not asserted — see `MobSim::push_entities`'s own doc
    /// comment for why that half needs a different seam.
    #[test]
    fn a_player_overlapping_a_mob_pushes_the_mob_away() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(0.2, 0.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);

        for _ in 0..10 {
            sim.tick();
        }

        let moved = sim.get(id).expect("alive").position();
        assert!(
            moved.x < 0.0,
            "a player standing at +x inside the pig's body must push the pig \
             toward -x; pig ended at x={}",
            moved.x
        );
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
    ///
    /// The attacker position is placed well outside `flat_world`'s solid `±8`
    /// platform (issue #233): once the bee's anger-gated target row and
    /// `Bee.BeeAttackGoal` both landed, a *nearby* attacker position let the
    /// bee's own `MeleeAttackGoal` close the one-block gap and "sting" the
    /// bare position within the very first tick, clearing `anger`
    /// (`stopBeingAngry()`, by design — see `roster::neutral::BEE`'s doc
    /// comment) before this helper ever got to measure the grudge's real
    /// duration. This function measures **grudge duration**, not "does the
    /// mob's own combat ever run" — an attacker outside the walkable platform
    /// (so no path exists to it, for any of the four species' plausible
    /// speeds) decouples the two.
    fn ticks_until_anger_clears(species: &str, limit: u64) -> Option<u64> {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(128.0, 0.0, 0.0);
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

    /// Primitive 2 (issue #458): the gaze feed reaches `is_being_stared_at`
    /// in production, not just in `lodestone_entity`'s own hermetic gates.
    ///
    /// **The discriminating pair**: two sims, each with one enderman at the
    /// *identical* position and one player at the *identical* position — the
    /// only difference is the player's `view_direction`. A gate that only
    /// varied position (closer/farther) could not tell a real gaze test from
    /// a distance check; this one cannot vary anything else, because nothing
    /// else differs.
    #[test]
    fn the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not() {
        let world = flat_world();
        let player_pos = Vec3::new(0.0, 0.0, 0.0);
        let enderman_pos = Vec3::new(0.0, 0.0, 10.0);

        // Resolve the real eye positions the feed itself uses, rather than
        // guessing them — `feed_perception`'s own formula for the mob eye
        // (`height * 0.85`) and `PLAYER_EYE_HEIGHT` for the player.
        let mut probe = MobSim::new(&world);
        let probe_id = probe
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        let mob_eye_height = f64::from(probe.get(probe_id).expect("spawned").shape().height) * 0.85;
        let mob_eye = Vec3::new(enderman_pos.x, enderman_pos.y + mob_eye_height, enderman_pos.z);
        let player_eye = Vec3::new(player_pos.x, player_pos.y + PLAYER_EYE_HEIGHT, player_pos.z);
        let delta = Vec3::new(mob_eye.x - player_eye.x, mob_eye.y - player_eye.y, mob_eye.z - player_eye.z);
        let dist = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
        let looking_at = Vec3::new(delta.x / dist, delta.y / dist, delta.z / dist);
        // Exactly opposite the enderman — as far outside the cone as a unit
        // vector can be (`dot == -1`), not a near-miss.
        let looking_away = Vec3::new(-looking_at.x, -looking_at.y, -looking_at.z);

        // The naive (non-distance-adjusted) hypothesis this feed's own doc
        // warns against: reading `coneSize` (0.025) as the tolerance
        // directly gives threshold `1.0 - 0.025 = 0.975`. At `looking_at`
        // (`dot == 1.0`) both the naive and the real (`1.0 - 0.025/dist`)
        // hypotheses agree — accepted either way — which is exactly why the
        // boundary case belongs to `lodestone_entity`'s own
        // `is_in_view_cone_boundary_at_the_endermans_own_cone_size` gate and
        // not here; this test's job is only "does the feed reach the goal
        // at all", which a dead-on look and a dead-opposite one already
        // settle without needing a razor's-edge input.
        assert!(dist > 1.0, "the fixture must not degenerate to zero distance: dist={dist}");

        let mut watched = MobSim::new(&world);
        let watched_id = watched
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        watched.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: looking_at,
        }]);
        watched.tick();
        assert!(
            watched.get(watched_id).expect("alive").mob.is_being_stared_at(),
            "a player looking straight at the enderman must set is_being_stared_at"
        );

        let mut unwatched = MobSim::new(&world);
        let unwatched_id = unwatched
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        unwatched.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: looking_away,
        }]);
        unwatched.tick();
        assert!(
            !unwatched.get(unwatched_id).expect("alive").mob.is_being_stared_at(),
            "a player looking directly away, from the identical position, must not"
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

/// Issue #237's residue: the age-scaled hitbox and the baby-only movement
/// modifier, which nothing exercised before `species_shape`/`SimMob::set_age`
/// gained an `is_baby` fold.
#[cfg(test)]
mod baby_shape_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    /// **A baby zombie is the real `0.49×0.98` literal, not a halved adult.**
    ///
    /// `0.6×1.95` halved is `0.3×0.975` — close enough to the true value that
    /// an assertion only checking "shrank" would pass under either
    /// hypothesis. Predicting the exact literal is what separates a real
    /// `BABY_DIMENSIONS` port from the generic `getAgeScale()` fallback.
    #[test]
    fn a_baby_zombie_is_the_exact_vanilla_literal_not_a_halved_adult() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        {
            let adult = sim.get(id).expect("spawned");
            assert_eq!(adult.shape().width, 0.6, "adult zombie width");
            assert_eq!(adult.shape().height, 1.95, "adult zombie height");
        }

        let mob = sim.get_mut(id).expect("spawned");
        mob.set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            0.49,
            "baby zombie width is the literal BABY_DIMENSIONS, not 0.6 * 0.5 = 0.3"
        );
        assert_eq!(
            baby.shape().height,
            0.98,
            "baby zombie height is the literal BABY_DIMENSIONS, not 1.95 * 0.5 = 0.975"
        );
    }

    /// Growing back up re-derives the adult shape — the boundary crossing
    /// runs in both directions, not just baby-ward.
    #[test]
    fn growing_up_restores_the_adult_shape() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        assert_eq!(sim.get(id).expect("spawned").shape().width, 0.45, "baby cow width");

        sim.get_mut(id).expect("spawned").set_age(0);
        let grown = sim.get(id).expect("still spawned");
        assert!(!grown.is_baby(), "age 0 is the cooldown-free adult reading");
        assert_eq!(grown.shape().width, 0.9, "adult cow width restored");
        assert_eq!(grown.shape().height, 1.4, "adult cow height restored");
    }

    /// **Control: a species with no `baby_dimensions` entry uses the real
    /// `LivingEntity` fallback (half size), not a made-up constant.**
    ///
    /// Skeletons never naturally have babies, but `is_baby()` only reads the
    /// age counter — nothing species-gates it — so this is the discriminating
    /// input for the fallback arm specifically: a skeleton's adult box is
    /// `0.6×1.99`, and the *wrong* hypothesis (no fallback at all, i.e. the
    /// shape not changing) would leave it at `0.6×1.99` where the fallback
    /// predicts `0.3×0.995`.
    #[test]
    fn control_a_species_with_no_baby_table_entry_uses_the_generic_age_scale() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:skeleton".parse().expect("valid key"), above_floor())
            .id();
        let adult_width = sim.get(id).expect("spawned").shape().width;
        let adult_height = sim.get(id).expect("spawned").shape().height;
        assert_eq!(adult_width, 0.6, "adult skeleton width");
        assert_eq!(adult_height, 1.99, "adult skeleton height");

        sim.get_mut(id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            adult_width * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
        assert_eq!(
            baby.shape().height,
            adult_height * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
    }

    /// **The zombie family's baby speed boost is `base * 1.5`, and a cow's
    /// stays flat** — the discriminating pair the residue's "attribute
    /// change" half asks for. `step_per_tick` now reports the AI-driven
    /// kinematic-follower rate, not the bare attribute (see
    /// `ai_ground_speed`'s own doc): predicted here from the same outside
    /// constants (vanilla's default ground friction, `0.6 * 0.91`) in a
    /// separate expression, not by calling the function under test, so a
    /// shared bug cannot cancel out. `0.23 * 1.5 = 0.345` is still the
    /// attribute-level prediction; squaring and dividing by
    /// `1 - 0.6 * 0.91` is the extra step `ai_ground_speed` adds.
    #[test]
    fn baby_zombie_speeds_up_and_baby_cow_does_not() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let friction = 1.0 - 0.6 * 0.91;
        let predicted = |attribute: f64| attribute * attribute / friction;

        let zombie_id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        let zombie_adult_speed = sim.get(zombie_id).expect("spawned").step_per_tick();
        assert!(
            (zombie_adult_speed - predicted(0.23)).abs() < 1e-9,
            "adult zombie ground speed must be movement_speed(0.23) squared over \
             (1 - 0.6*0.91), got {zombie_adult_speed}, predicted {}",
            predicted(0.23)
        );
        sim.get_mut(zombie_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let zombie_baby_speed = sim.get(zombie_id).expect("still spawned").step_per_tick();
        assert!(
            (zombie_baby_speed - predicted(0.23 * 1.5)).abs() < 1e-9,
            "baby zombie speed must be exactly ai_ground_speed(0.23 * 1.5), got \
             {zombie_baby_speed}, predicted {}",
            predicted(0.23 * 1.5)
        );
        assert!(
            zombie_baby_speed > zombie_adult_speed,
            "the baby boost must still win after the ground-speed conversion, not \
             just at the attribute level"
        );

        let cow_id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .id();
        let cow_adult_speed = sim.get(cow_id).expect("spawned").step_per_tick();
        assert!(
            (cow_adult_speed - predicted(0.2)).abs() < 1e-9,
            "adult cow ground speed must be movement_speed(0.2) squared over \
             (1 - 0.6*0.91), got {cow_adult_speed}, predicted {}",
            predicted(0.2)
        );
        sim.get_mut(cow_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let cow_baby_speed = sim.get(cow_id).expect("still spawned").step_per_tick();
        assert!(
            (cow_baby_speed - cow_adult_speed).abs() < 1e-9,
            "a cow has no SPEED_MODIFIER_BABY — baby speed must equal adult speed exactly"
        );
    }

    /// **Control proving `ai_ground_speed` is load-bearing, not decorative**:
    /// with the bare `movement_speed` attribute used directly (the pre-fix
    /// behaviour this repo's own evidence standards require a control for),
    /// a pig's per-tick movement step is `0.25` — noticeably higher than the
    /// `ai_ground_speed(0.25)` this fix now produces, which is the measured
    /// direction of the "way too fast" report. If this control ever starts
    /// failing, `ai_ground_speed` has stopped changing the value it exists to
    /// change.
    #[test]
    fn removing_the_ground_speed_conversion_reproduces_the_too_fast_bug() {
        let attribute = 0.25;
        assert!(
            ai_ground_speed(attribute) < attribute,
            "control: the converted ground speed must be lower than the bare \
             attribute value, or the subject assertions above prove nothing \
             about the conversion firing"
        );
    }

    /// A bred child inherits the correct baby shape through
    /// `resolve_breeding`'s existing `child.set_age(BABY_START_AGE)` call —
    /// no separate wiring needed, because [`SimMob::set_age`] itself now
    /// re-derives the shape. This is the island check: a shape fold that only
    /// ran for a hand-called `set_age` in a test, and never for the
    /// production breeding path, would still look finished from the unit
    /// tests above alone.
    #[test]
    fn a_bred_child_spawns_with_the_baby_shape_already_applied() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(7.0, 1.0, 8.0))
            .id();
        let _b = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(9.0, 1.0, 8.0))
            .id();

        // Exercises `resolve_breeding`'s own partner search and
        // `child.set_age(BABY_START_AGE)` call directly — the real
        // production path a `BreedGoal` completing feeds through
        // `MobSim::tick`, without re-driving sixty ticks of love-mode timing
        // just to reach it.
        sim.resolve_breeding(vec![(
            a,
            Vec3::new(8.0, 1.0, 8.0),
            "minecraft:cow".parse().expect("valid key"),
        )]);

        let child = sim
            .mobs
            .iter()
            .find(|m| m.is_baby())
            .expect("a child was spawned and is a baby");
        assert_eq!(
            child.shape().width,
            0.45,
            "the bred calf's shape must already be the baby literal, not the adult default"
        );
    }
}

/// `MetadataField::Baby`'s producer-side species switch in
/// [`SimMob::snapshot`] — the eligible species must match exactly the union
/// [`baby_dimensions`]/[`baby_speed_multiplier`] already scope "grows a
/// baby" to, and the ineligible species (index 16's other claimants) must
/// never see the field at all.
#[cfg(test)]
mod baby_metadata_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    fn baby_field(metadata: &[MetadataField]) -> Option<bool> {
        metadata.iter().find_map(|f| match f {
            MetadataField::Baby(b) => Some(*b),
            _ => None,
        })
    }

    /// **Positive arm**: every species this sim scopes ageing to
    /// (`AgeableMob`'s breedable-animal set plus the `Zombie` family — see
    /// `SimMob::snapshot`'s own comment for the mechanical derivation off
    /// `.cache/mc/26.2/src/`) must push `MetadataField::Baby(false)` as a
    /// freshly-spawned adult.
    ///
    /// **Negative control**: index 16's other real claimants — `creeper`
    /// (`Creeper.DATA_SWELL_DIR`, an `INT`, already the producer for a
    /// different variant at this same index), `ghast`
    /// (`Ghast.DATA_IS_CHARGING`) and `phantom` (`Phantom.ID_SIZE`) — must
    /// push no `Baby` field at all. These are exactly the entities a shared
    /// "is baby" encoder would corrupt: a ghast told `Baby(false)` reads to
    /// a real client as "not charging", and a phantom's size becomes `0`.
    ///
    /// Collected rather than asserted per-iteration so one run reports every
    /// wrong species, not just the first.
    #[test]
    fn eligible_species_emit_baby_and_only_those_do() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let mut wrong: Vec<String> = Vec::new();

        for species in [
            "cow",
            "mooshroom",
            "sheep",
            "pig",
            "chicken",
            "rabbit",
            "wolf",
            "zombie",
            "husk",
            "zombie_villager",
            "drowned",
            "zombified_piglin",
        ] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata) != Some(false) {
                wrong.push(format!(
                    "{species}: expected Baby(false), metadata was {metadata:?}"
                ));
            }
        }

        for species in ["creeper", "ghast", "phantom"] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata).is_some() {
                wrong.push(format!(
                    "{species}: must emit no Baby field at all, metadata was {metadata:?}"
                ));
            }
        }

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The grown-up transition.** A baby that matures must produce a
    /// snapshot whose `Baby` is `Some(false)`, not absent — an absent field
    /// leaves the client holding whatever `Baby(true)` it was sent on
    /// arrival, so the mob would stay a baby on screen forever. See
    /// `SimMob::snapshot`'s own doc comment for why this variant is pushed
    /// unconditionally rather than only while `is_baby()` is true.
    #[test]
    fn a_grown_up_baby_reports_baby_false_not_absent() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        let baby_metadata = sim.get(id).expect("spawned").snapshot().metadata;
        assert_eq!(
            baby_field(&baby_metadata),
            Some(true),
            "a freshly spawned baby zombie must report Baby(true), got {baby_metadata:?}"
        );

        sim.get_mut(id).expect("spawned").set_age(0);
        let adult_metadata = sim.get(id).expect("still spawned").snapshot().metadata;
        assert!(
            adult_metadata.iter().any(|f| matches!(f, MetadataField::Baby(_))),
            "the grown-up snapshot must still carry a Baby field, not omit it: {adult_metadata:?}"
        );
        assert_eq!(
            baby_field(&adult_metadata),
            Some(false),
            "after growing up the field must flip to Baby(false), got {adult_metadata:?}"
        );
    }
}

/// Issue #236: lead attach/detach, the fence-knot re-parent, and the
/// distance-based pull/snap physics.
#[cfg(test)]
mod leash_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    fn player_at(uuid: Uuid, pos: Vec3) -> PerceivedPlayer {
        PerceivedPlayer {
            identity: Some(PlayerIdentity { uuid, entity_id: 99 }),
            perception: PlayerPerception {
                position: pos,
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }
    }

    #[test]
    fn attaching_a_lead_to_a_leashable_mob_holds_it() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();

        let outcome = sim.try_leash(id, holder, true, false);
        assert_eq!(outcome, LeashOutcome::Attached);
        assert_eq!(
            sim.get(id).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(holder))
        );
    }

    /// **Control: a hostile species refuses a lead** — vanilla
    /// `Mob.canBeLeashed()` is `!(this instanceof Enemy)`, so this is the
    /// discriminating input against "every mob accepts a lead".
    #[test]
    fn control_a_hostile_mob_refuses_a_lead() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species(
                "minecraft:zombie".parse().expect("valid key"),
                Vec3::new(2.0, 0.0, 0.0),
            )
            .id();

        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Refused);
        assert_eq!(sim.get(id).expect("spawned").leash_holder(), None);
    }

    #[test]
    fn detaching_returns_the_lead_unless_creative() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);

        assert_eq!(
            sim.try_leash(id, holder, false, false),
            LeashOutcome::Detached { dropped_lead: true },
            "survival mode must drop a lead item"
        );
        assert_eq!(sim.get(id).expect("still spawned").leash_holder(), None);
        assert_eq!(sim.item_count(), 1, "the reported drop must be a real item, not just a flag");

        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);
        assert_eq!(
            sim.try_leash(id, holder, false, true),
            LeashOutcome::Detached {
                dropped_lead: false
            },
            "creative mode (infinite materials) must not drop a lead item"
        );
        assert_eq!(
            sim.item_count(),
            1,
            "creative detach must not spawn a second item on top of the survival one"
        );
    }

    /// One player cannot steal another's already-leashed mob just by
    /// holding a lead — vanilla's `!(leashable.getLeashHolder() instanceof
    /// Player)` guard.
    #[test]
    fn a_different_players_lead_cannot_steal_an_already_leashed_mob() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner = Uuid::new_v4();
        let thief = Uuid::new_v4();
        sim.set_players(vec![
            player_at(owner, Vec3::new(0.0, 0.0, 0.0)),
            player_at(thief, Vec3::new(1.0, 0.0, 0.0)),
        ]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        assert_eq!(sim.try_leash(id, owner, true, false), LeashOutcome::Attached);

        assert_eq!(sim.try_leash(id, thief, true, false), LeashOutcome::Refused);
        assert_eq!(
            sim.get(id).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(owner)),
            "the mob must still belong to its original holder"
        );
    }

    /// **The exact pull vector, predicted, at a discriminating distance**
    /// (10 blocks: past `LEASH_ELASTIC_DIST` (6) and short of
    /// `LEASH_TOO_FAR_DIST` (12), so both thresholds are exercised by the
    /// same fixture in opposite directions). `excess = 10 - 6 = 4`, capped
    /// to `1.0`, times the `0.3` scale this port uses — `0.3` exactly, along
    /// `+x` since the holder is due east.
    #[test]
    fn a_leashed_mob_beyond_the_elastic_distance_is_pulled_toward_its_holder() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(10.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        let mob = sim.get(id).expect("still spawned");
        assert_eq!(
            mob.position(),
            Vec3::new(0.3, 0.0, 0.0),
            "pulled 0.3 blocks toward the holder, not teleported"
        );
        assert!(mob.is_leashed(), "still leashed — this is a pull, not a snap");
    }

    /// **Control: within the elastic distance, nothing happens at all.**
    /// Discriminates against a pull formula that fires unconditionally.
    #[test]
    fn control_a_leashed_mob_within_the_elastic_distance_is_not_pulled() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(3.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        assert_eq!(
            sim.get(id).expect("still spawned").position(),
            Vec3::new(0.0, 0.0, 0.0),
            "3 blocks is inside LEASH_ELASTIC_DIST (6) — no force at all"
        );
    }

    /// Past `LEASH_TOO_FAR_DIST` (12), the lead snaps: the mob is freed and
    /// a real `minecraft:lead` item appears at its position.
    #[test]
    fn a_leashed_mob_beyond_the_snap_distance_drops_its_lead() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(13.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));
        assert_eq!(sim.item_count(), 0, "control: nothing dropped yet");

        sim.tick_leashes();

        assert!(
            !sim.get(id).expect("still spawned").is_leashed(),
            "13 blocks is past LEASH_TOO_FAR_DIST (12) — the lead must snap"
        );
        assert_eq!(sim.item_count(), 1, "a lead item must be spawned on snap");
    }

    /// A leash holder that cannot be resolved (a disconnected player) drops
    /// the leash silently, with no item — the disclosed simplification
    /// `tick_leashes`'s own doc comment names.
    #[test]
    fn control_an_unresolvable_holder_drops_the_leash_without_an_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        // No `set_players` call — `holder` cannot be resolved to a position.
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        assert!(!sim.get(id).expect("still spawned").is_leashed());
        assert_eq!(sim.item_count(), 0, "an unresolvable holder must not drop an item");
    }

    /// Right-clicking a fence with a lead re-parents every mob leashed to
    /// the player onto that fence position — vanilla `LeadItem.bindPlayerMobs`.
    #[test]
    fn leashing_to_a_fence_reparents_every_mob_the_player_was_holding() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let a = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species("minecraft:sheep".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0))
            .id();
        // A third mob leashed to someone else must not be swept up.
        let other_holder = Uuid::new_v4();
        let c = sim
            .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 2.0))
            .id();
        sim.get_mut(a).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
        sim.get_mut(b).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
        sim.get_mut(c)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(other_holder)));

        let fence_pos = BlockPos::new(5, 0, 5);
        let mut moved = sim.try_leash_to_fence(holder, fence_pos);
        moved.sort_unstable();
        let mut expected = vec![a, b];
        expected.sort_unstable();
        assert_eq!(moved, expected, "only the calling player's own leashed mobs move");

        assert_eq!(
            sim.get(a).expect("spawned").leash_holder(),
            Some(LeashHolder::Fence(fence_pos))
        );
        assert_eq!(
            sim.get(c).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(other_holder)),
            "a mob leashed to a different player must be untouched"
        );
    }
}

/// Issue #240's entity-spawn slice: the trader plus its leashed llama
/// escort. The spawn-cycle timing/POI search is out of scope here — see
/// `spawn_wandering_trader`'s own doc comment.
#[cfg(test)]
mod wandering_trader_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    #[test]
    fn spawning_a_wandering_trader_leashes_two_llamas_to_it() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);

        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(10.0, 5.0, 10.0));

        assert_eq!(llamas.len(), 2, "vanilla attempts exactly two escorts");
        let trader = sim.get(trader_id).expect("trader spawned");
        assert_eq!(trader.entity_type().path(), "wandering_trader");

        for llama_id in llamas {
            let llama = sim.get(llama_id).expect("llama spawned");
            assert_eq!(llama.entity_type().path(), "trader_llama");
            assert_eq!(
                llama.leash_holder(),
                Some(LeashHolder::Mob(trader_id)),
                "each llama must be leashed to the trader, not merely placed near it"
            );
        }
    }

    /// **The leash is real, not cosmetic** — moving the trader and ticking
    /// leashes must pull an escort that has drifted past the elastic
    /// distance, exactly as it would for a player-held leash. This is the
    /// control that separates "the llama has a `leash_holder` field set" from
    /// "the llama is actually tethered".
    #[test]
    fn the_escort_leash_actually_pulls_when_the_trader_moves_away() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(0.0, 0.0, 0.0));
        let llama_id = llamas[0];

        // Drag the trader far enough that the escort (2 blocks from spawn,
        // at x=2) is past LEASH_ELASTIC_DIST (6) from it but still short of
        // LEASH_TOO_FAR_DIST (12), so this exercises the *pull* branch —
        // distance 8, not the snap branch a farther drag would hit instead.
        // There is no teleport API, so drive it through a knockback impulse
        // large enough to land at the target position deterministically.
        let trader_pos = sim.get(trader_id).expect("spawned").position();
        let target = Vec3::new(10.0, 0.0, 0.0);
        sim.get_mut(trader_id).expect("spawned").apply_knockback(Vec3::new(
            target.x - trader_pos.x,
            target.y - trader_pos.y,
            target.z - trader_pos.z,
        ));
        assert_eq!(sim.get(trader_id).expect("spawned").position(), target);

        let llama_before = sim.get(llama_id).expect("spawned").position();
        sim.tick_leashes();
        let llama_after = sim.get(llama_id).expect("still spawned").position();
        assert_ne!(
            llama_before, llama_after,
            "the llama must move toward its holder once the trader is far enough away"
        );
    }
}

/// `MobSim`'s periodic idle-vocalisation producer (`roll_ambient_sound`,
/// wired into [`MobSim::tick`]) — closing the gap the audit behind issue #664
/// found: hurt and death were the only mob sounds a running server could ever
/// produce, so ordinary exploration with no combat was silent but for
/// footsteps.
#[cfg(test)]
mod ambient_sound_tests {
    use super::*;
    use crate::effects::WorldEffect;
    use lodestone_model::SoundCategory;

    fn flat_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    /// **The wire is real, not merely the derivation.** A hermetic call to
    /// `effects::mob_ambient_sound` proves the string derivation; it proves
    /// nothing about whether anything in `MobSim::tick` ever calls it — the
    /// exact island shape this repo's own evidence standards warn about
    /// repeatedly. Ticking a real, freshly spawned cow through the real
    /// production `tick()` loop and draining `take_ambient_sounds()` is what
    /// proves the roll actually fires and actually reaches the queue
    /// `crate::tick::run_tick_loop` drains.
    ///
    /// `ambient_sound_time` starts at `0` and this cow's RNG stream is
    /// seeded deterministically from its id, so how many ticks the first
    /// firing takes is a fixed, measured number for this exact scenario, not
    /// a guess: measured at **tick 36** for this world/spawn order. The
    /// 400-tick loop bound is over 10x that measured value, not a round
    /// number reached for on its own — it exists only so a regression that
    /// pushed the firing tick out shows up as a clean failure rather than an
    /// infinite loop.
    #[test]
    fn a_real_mob_ticked_through_the_production_loop_eventually_vocalises() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }

        assert_eq!(
            fired.len(),
            1,
            "exactly one ambient sound must have fired within the margin, got {fired:?}"
        );
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.cow.ambient");
                assert_eq!(*category, SoundCategory::Neutral);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }

    /// **Control: the roll is load-bearing, not vacuous.** A dead mob
    /// (`health <= 0.0`) must never roll — vanilla's own guard is
    /// `isAlive() && …` — so ticking a mob whose health is forced to zero
    /// for the same window above must produce nothing at all. Without this,
    /// the subject test's "it fired" is not distinguishable from "everything
    /// always fires".
    #[test]
    fn a_dead_mob_never_rolls_an_ambient_sound() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("spawned").health = 0.0;

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
        }
        assert!(
            fired.is_empty(),
            "a mob at zero health must never roll an ambient sound, got {fired:?}"
        );
    }

    /// A hostile species reports the `Hostile` sound category, matching
    /// `mob_vocalisation`'s own hurt/death split — checked with the same
    /// production-loop wiring as the subject test above, not just the
    /// hermetic derivation.
    #[test]
    fn a_hostile_mob_vocalises_on_the_hostile_category() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }
        assert_eq!(fired.len(), 1, "expected exactly one ambient sound, got {fired:?}");
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.zombie.ambient");
                assert_eq!(*category, SoundCategory::Hostile);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }
}
